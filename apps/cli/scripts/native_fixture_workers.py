"""Process capture, overlap sampling and isolated worker inspection."""
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import threading
import time
import traceback


def process_start_id(pid):
    if sys.platform.startswith('linux'):
        raw = pathlib.Path('/proc', str(pid), 'stat').read_text()
        fields = raw[raw.rindex(')') + 2:].split()
        if fields[0] == 'Z':
            raise ProcessLookupError('Runtime owner is a zombie')
        return 'linux-starttime-' + fields[19]
    if sys.platform == 'win32':
        import ctypes
        from ctypes import wintypes
        kernel = ctypes.WinDLL('kernel32', use_last_error=True)
        kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel.OpenProcess.restype = wintypes.HANDLE
        kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
        kernel.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        handle = kernel.OpenProcess(0x1000, False, pid)
        if not handle:
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            times = [wintypes.FILETIME() for _ in range(4)]
            if not kernel.GetProcessTimes(handle, *(ctypes.byref(value) for value in times)):
                raise ctypes.WinError(ctypes.get_last_error())
            exit_code = wintypes.DWORD()
            if not kernel.GetExitCodeProcess(handle, ctypes.byref(exit_code)):
                raise ctypes.WinError(ctypes.get_last_error())
            if exit_code.value != 259:  # STILL_ACTIVE
                raise ProcessLookupError('Runtime owner exited')
            return 'windows-filetime-' + str((times[0].dwHighDateTime << 32) | times[0].dwLowDateTime)
        finally:
            kernel.CloseHandle(handle)
    if sys.platform == 'darwin':
        value = captured_run(['ps', '-p', str(pid), '-o', 'lstart='],
                             env={**os.environ, 'LC_ALL': 'C'}, timeout=5)
        if value.returncode or not value.stdout.strip():
            raise ProcessLookupError('Runtime birth unavailable')
        return 'darwin-lstart-' + value.stdout.decode().strip()
    raise RuntimeError('authoritative Runtime birth unsupported on this platform')


class FixtureCancelled(Exception):
    """An owned fixture command was explicitly stopped and reaped."""


def captured_run(command, *, env, timeout, cancel=None):
    # Regular files avoid communicate() waiting forever for inherited pipe EOF
    # after Windows subprocess.run() kills a timed-out direct child.
    started_unix = time.time()
    started_monotonic = time.monotonic()
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        process = subprocess.Popen(command, env=env, stdout=stdout, stderr=stderr)
        try:
            if cancel is None:
                process.wait(timeout=timeout)
            else:
                deadline = started_monotonic + timeout
                while process.poll() is None:
                    if cancel.is_set():
                        raise FixtureCancelled('owned foreground probe stopped')
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise subprocess.TimeoutExpired(command, timeout)
                    try:
                        process.wait(timeout=min(.05, remaining))
                    except subprocess.TimeoutExpired:
                        pass
            wait_completed_monotonic = time.monotonic()
        except BaseException:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
            raise
        stdout.seek(0)
        stderr.seek(0)
        result = subprocess.CompletedProcess(command, process.returncode, stdout.read(), stderr.read())
        result.fixture_timing = {'client_pid': process.pid, 'started_unix': started_unix,
            'started_monotonic': started_monotonic, 'wait_completed_monotonic': wait_completed_monotonic}
        return result


class ForegroundOverlapProbe:
    """Keep at most two real foreground calls in flight during a bounded window.

    The caller must supply an operation honoring its remaining timeout and the
    cancellation event. Only an
    actual same-snapshot pair of active inference lanes completes the challenge;
    queued work or activity in two different snapshots is insufficient.
    """
    def __init__(self, invoke, timeout=120):
        self.invoke = invoke
        self.started = time.monotonic()
        self.deadline = self.started + timeout
        self.observed = threading.Event()
        self.stopped = threading.Event()
        self.cancelled = threading.Event()
        self.calls = []
        self.errors = []
        self.threads = [threading.Thread(target=self._run, name=f'foreground-overlap-{i}', daemon=True)
                        for i in range(2)]

    def start(self):
        for thread in self.threads:
            thread.start()

    def _run(self):
        try:
            while not self.stopped.is_set() and not self.observed.is_set():
                remaining = self.deadline - time.monotonic()
                if remaining <= 0:
                    return
                self.calls.append(self.invoke(remaining, self.cancelled))
        except FixtureCancelled as error:
            if not self.cancelled.is_set():
                self.errors.append(error)
        except BaseException as error:
            self.errors.append(error)

    def observe(self, inference):
        # A late first overlapping heartbeat must not turn an expired challenge
        # into success (HTTP or boundary inspection can cross the deadline).
        self.check()
        if inference.get('foreground', {}).get('active') and inference.get('background', {}).get('active'):
            self.observed.set()

    def check(self):
        if self.errors:
            raise RuntimeError('foreground overlap probe failed: ' + str(self.errors[0])) from self.errors[0]
        if not self.observed.is_set() and time.monotonic() >= self.deadline:
            raise TimeoutError('concurrent foreground/background activity was not observed before the overlap deadline')

    def close(self, cancel_pending=False):
        self.stopped.set()
        if cancel_pending:
            self.cancelled.set()
        # In-flight captured_run calls honor cancellation and the shared deadline,
        # reaping their child in either case. On success, drain submitted calls
        # so their errors and completion are checked before later index checks.
        for thread in self.threads:
            thread.join(timeout=max(0, self.deadline - time.monotonic()) + 6)
        if any(thread.is_alive() for thread in self.threads):
            raise RuntimeError('foreground overlap probe did not stop')
        if self.errors:
            raise RuntimeError('foreground overlap probe failed: ' + str(self.errors[0])) from self.errors[0]


def inspect_worker(request, phase):
    import psutil
    phase('children')
    host = psutil.Process(request['host_pid'])
    if request.get('runtime_binary'):
        assert pathlib.Path(host.exe()).resolve() == pathlib.Path(request['runtime_binary']).resolve()
        assert process_start_id(host.pid) == request['process_start_id']
    else:
        assert '--runtime-internal' in host.cmdline(), 'inspection must target the fixture runtime'
    children = [child for child in host.children(recursive=True)
                if not (sys.platform == 'win32' and child.name().lower() == 'conhost.exe')]
    assert len(children) == 1, 'expected exactly one supervised Core worker'
    worker = children[0]
    assert worker.pid == request['worker_pid'] and worker.ppid() == host.pid
    assert '--core-worker-internal' in worker.cmdline()
    assert pathlib.Path(worker.exe()).resolve() == pathlib.Path(host.exe()).resolve()
    phase('open_files')
    assert not any(pathlib.Path(file.path).name in ('rsrs.db', 'lock.db') for file in worker.open_files())
    phase('environ')
    assert not {'GH_TOKEN', 'GITHUB_TOKEN', 'OPENAI_API_KEY', 'RSRS_SUPER'} & worker.environ().keys()
    phase('identity')
    return [worker.pid, worker.create_time()]


class WorkerInspector:
    def __init__(self, directory, timeout=5):
        self.directory = pathlib.Path(directory)
        self.directory.mkdir(parents=True, exist_ok=False)
        self.timeout = timeout
        self.sequence = 0
        self.log = (self.directory / 'inspector.log').open('wb')
        self.process = subprocess.Popen([sys.executable, '-u', __file__, str(self.directory)],
                                        stdin=subprocess.PIPE, stdout=self.log, stderr=self.log)

    def inspect(self, request):
        """Check one identity within the deadline; every helper error fails closed."""
        self.sequence += 1
        request = {**request, 'sequence': self.sequence}
        response = self.directory / ('response-' + str(self.sequence) + '.json')
        try:
            self.process.stdin.write((json.dumps(request) + '\n').encode())
            self.process.stdin.flush()
        except OSError as error:
            failure = RuntimeError('worker inspector request pipe failed; see ' +
                                   str(self.directory / 'inspector.log'))
            try:
                self.record_failure('worker inspector request pipe failed: ' + str(error))
            except OSError as recording_error:
                failure.add_note('failure record could not be written: ' + str(recording_error))
            raise failure from error
        deadline = time.monotonic() + self.timeout
        while True:
            if self.process.poll() is not None:
                self.record_failure('worker inspector exited')
                raise RuntimeError('worker inspector exited; see ' + str(self.directory / 'inspector.log'))
            if time.monotonic() >= deadline:
                self.record_failure('worker boundary inspection exceeded deadline')
                self.close()
                raise TimeoutError('worker boundary inspection exceeded ' + str(self.timeout) +
                                   's; see ' + str(self.directory / 'inspector.log'))
            try:
                result = json.loads(response.read_text(encoding='utf-8'))
                break
            except (FileNotFoundError, PermissionError):
                # Windows may briefly deny access while a just-renamed file is
                # being closed or scanned. This shares the inspection deadline.
                time.sleep(.005)
        if not result['passed']:
            self.record_failure(result.get('error', 'worker boundary assertion failed'))
            raise AssertionError(result.get('error'))
        return tuple(result['identity'])

    def record_failure(self, reason):
        # Persist before kill/wait: a cleanup failure cannot erase the cause.
        (self.directory / 'inspection-failure.json').write_text(json.dumps({
            'passed': False, 'sequence': self.sequence, 'helper_pid': self.process.pid,
            'deadline_seconds': self.timeout, 'error': reason,
        }, indent=2), encoding='utf-8')

    def close(self):
        """Reap the helper and close streams without masking a broken-pipe cause."""
        try:
            if self.process.poll() is None:
                self.process.kill()
            self.process.wait(timeout=5)
        finally:
            try:
                # A dead helper can leave buffered request bytes behind. The
                # helper has already been reaped; do not let its failed close
                # flush hide the inspection failure during cleanup.
                try:
                    self.process.stdin.close()
                except OSError:
                    if self.process.poll() is None:
                        raise
            finally:
                self.log.close()


def serve(directory):
    """Publish boundary results; the parent enforces the inspection deadline."""
    import faulthandler
    faulthandler.enable()
    directory = pathlib.Path(directory)
    import psutil
    print(json.dumps({'inspector_pid': os.getpid(), 'psutil': psutil.__version__}), flush=True)
    # Keep fatal-crash diagnostics, but do not schedule asynchronous stack dumps:
    # they can race live frames on Windows (CPython #116008). The controller's
    # inspection deadline still kills/reaps a hung helper; phase logs locate it.
    for line in sys.stdin:
        request = json.loads(line)
        sequence = request['sequence']
        def phase(name):
            print(json.dumps({'sequence': sequence, 'phase': name, 'monotonic': time.monotonic()}), flush=True)
        try:
            identity = inspect_worker(request, phase)
            result = {'passed': True, 'identity': identity}
        except Exception:
            result = {'passed': False, 'error': traceback.format_exc()}
        phase('response')
        temporary = directory / 'response.tmp'
        temporary.write_text(json.dumps(result), encoding='utf-8')
        os.replace(temporary, directory / ('response-' + str(sequence) + '.json'))
        phase('responded')


if __name__ == '__main__':
    serve(sys.argv[1])
