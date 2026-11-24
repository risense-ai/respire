"""Contain and reap the owned Runtime children of nonloopback rejection probes."""
import ctypes
import multiprocessing
import signal
import os
from pathlib import Path
import subprocess
import sys
import time
from runtime_smoke_failures import Failure
from runtime_smoke_failures import require


def rejection_children():
    # This is the private subreaper's direct/adopted children, not a PID sweep.
    pid = os.getpid()
    return [int(value) for value in Path(f'/proc/{pid}/task/{pid}/children').read_text().split()]


def preflight_rejection_pidfd():
    require(hasattr(os, 'pidfd_open') and hasattr(signal, 'pidfd_send_signal'),
            'nonloopback_containment_unavailable')
    fd = os.pidfd_open(os.getpid())
    try:
        signal.pidfd_send_signal(fd, 0)
    finally:
        os.close(fd)


def kill_rejection_child(pid):
    # This single-threaded supervisor never reaps between identity verification
    # and signaling. A direct/adopted child's PID cannot be reused until reaped.
    try:
        before = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[19]
        fd = None
        degraded = False
        try:
            try:
                fd = os.pidfd_open(pid)
            except OSError:
                degraded = True
            after = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[19]
            require(before == after and pid in rejection_children(), 'nonloopback_cleanup_identity_failed')
            if fd is None:
                os.kill(pid, signal.SIGKILL)
            else:
                try:
                    signal.pidfd_send_signal(fd, signal.SIGKILL)
                except OSError:
                    # Still our unreaped child, with no intervening waitpid.
                    os.kill(pid, signal.SIGKILL)
                    degraded = True
            return degraded
        finally:
            if fd is not None:
                os.close(fd)
    except (ProcessLookupError, FileNotFoundError):
        return False


def reap_rejection_children(deadline):
    leaked = False
    failed = False
    while True:
        children = rejection_children()
        if not children:
            require(not failed, 'nonloopback_cleanup_failed')
            return leaked
        leaked = True
        for pid in children:
            try:
                failed = kill_rejection_child(pid) is True or failed
            except Exception:
                # One child failure must not strand the other owned children.
                failed = True
        while True:
            try:
                pid, _ = os.waitpid(-1, os.WNOHANG)
            except ChildProcessError:
                break
            except OSError:
                failed = True
                break
            if pid == 0:
                break
        require(time.monotonic() < deadline, 'nonloopback_cleanup_timeout')
        time.sleep(0.01)


def rejection_supervisor(connection, command, env, root, timeout):
    # A dedicated subreaper retains even setsid() descendants after CLI exit.
    # The parent fixture and unrelated children are outside this boundary.
    response = {'error': 'nonloopback_execution_failed'}
    process = None
    def interrupted(signum, frame):
        raise OSError('supervisor interrupted')
    signal.signal(signal.SIGTERM, interrupted)
    try:
        libc = ctypes.CDLL(None, use_errno=True)
        require(libc.prctl(36, 1, 0, 0, 0) == 0, 'nonloopback_containment_unavailable')
        preflight_rejection_pidfd()
        process = subprocess.Popen(command, env=env, cwd=root, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            # A fast normal exit may already be a zombie, with no /proc exe.
            if process.poll() is None:
                try:
                    image = Path(f'/proc/{process.pid}/exe').resolve(strict=True)
                except FileNotFoundError:
                    require(process.poll() is not None, 'nonloopback_process_image_mismatch')
                else:
                    require(image == Path(command[0]).resolve(strict=True),
                            'nonloopback_process_image_mismatch')
            stdout, stderr = process.communicate(timeout=timeout)
            response = {'returncode': process.returncode, 'stdout': stdout, 'stderr': stderr}
        except (OSError, subprocess.TimeoutExpired, Failure):
            response = {'error': 'nonloopback_execution_failed'}
    except Exception:
        response = {'error': 'nonloopback_execution_failed'}
    finally:
        # Do not let another parent cancellation interrupt descendant cleanup.
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        try:
            leaked = reap_rejection_children(time.monotonic() + 5)
            if leaked:
                response = {'error': 'nonloopback_descendant_cleanup_required'}
            if process is not None:
                # waitpid above can already have reaped the direct child.
                process.wait(timeout=1)
                process.stdout.close()
                process.stderr.close()
        except Exception:
            response = {'error': 'nonloopback_cleanup_failed'}
        try:
            connection.send(response)
        finally:
            connection.close()


def contained_rejection_run(command, env, root, timeout=90):
    require(sys.platform.startswith('linux'), 'nonloopback_containment_unavailable')
    context = multiprocessing.get_context('fork')
    reader, writer = context.Pipe(duplex=False)
    supervisor = context.Process(target=rejection_supervisor,
                                 args=(writer, command, env, root, timeout))
    started = False
    try:
        supervisor.start()
        started = True
        writer.close()
        cancelled = not reader.poll(timeout + 10)
        if cancelled:
            supervisor.terminate()  # SIGTERM requests its finally cleanup, not SIGKILL.
            require(reader.poll(10), 'nonloopback_cleanup_unconfirmed')
        response = reader.recv()
        supervisor.join(5)
        require(not supervisor.is_alive() and supervisor.exitcode == 0,
                'nonloopback_cleanup_unconfirmed')
        require(not cancelled, 'nonloopback_execution_failed')
        require('error' not in response, response.get('error', 'nonloopback_execution_failed'))
        return subprocess.CompletedProcess([], response['returncode'], response['stdout'], response['stderr'])
    except (EOFError, OSError):
        raise Failure('nonloopback_execution_failed') from None
    finally:
        reader.close()
        writer.close()
        if started and supervisor.is_alive():
            supervisor.terminate()
            supervisor.join(10)
