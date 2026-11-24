"""Exercise hosted OS registration, recovery, and managed profile switches."""
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import time
import urllib.request

from runtime_autostart_upgrade import verify_previous_runtime


def verify_registration(args, base, binary, bundle_helpers, command, copy_managed_installation,
                        env, health, home, keys, managed_fixture, off_env, off_root, port, root,
                        runtime_name, stage_runtime, backend_health, seed_local_credentials,
                        read_restart_stages, kernel, ctypes):
    def collect_managed_profiles():
        # A service manager deliberately does not inherit test-runner
        # profiling variables. Collect the real instrumented native
        # child's default profiles from this disposable working root.
        pattern = os.environ.get("LLVM_PROFILE_FILE")
        if not pattern:
            return
        import shutil
        import uuid
        destination = Path(pattern).parent
        assert destination.is_dir(), "coverage profile destination missing"
        for profile in root.rglob("*.profraw"):
            assert not profile.is_symlink() and profile.stat().st_size <= 64 * 1024 * 1024
            shutil.copyfile(profile, destination / f"autostart-managed-{uuid.uuid4().hex}.profraw")

    from contextlib import contextmanager, nullcontext

    @contextmanager
    def owned_windows_pair(expected_root):
        # Kernel-backed process handles bind the exact instances; a
        # stale PID or a Task Scheduler engine must never be killed.
        import ctypes
        from ctypes import wintypes
        assert os.name == "nt"
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/api/health",timeout=5) as response:
            current=json.load(response)
        expected=json.loads((expected_root/"runtime/autostart/registration.json").read_text())
        assert Path(current["installation_root"]).resolve()==expected_root.resolve() and current["managed_id"]==expected["id"]
        kernel=ctypes.WinDLL("kernel32",use_last_error=True)
        kernel.OpenProcess.argtypes=[wintypes.DWORD,wintypes.BOOL,wintypes.DWORD];kernel.OpenProcess.restype=wintypes.HANDLE
        kernel.GetProcessId.argtypes=[wintypes.HANDLE];kernel.GetProcessId.restype=wintypes.DWORD
        kernel.QueryFullProcessImageNameW.argtypes=[wintypes.HANDLE,wintypes.DWORD,wintypes.LPWSTR,ctypes.POINTER(wintypes.DWORD)];kernel.QueryFullProcessImageNameW.restype=wintypes.BOOL
        kernel.GetProcessTimes.argtypes=[wintypes.HANDLE]+[ctypes.POINTER(wintypes.FILETIME)]*4;kernel.GetProcessTimes.restype=wintypes.BOOL
        kernel.TerminateProcess.argtypes=[wintypes.HANDLE,wintypes.UINT];kernel.TerminateProcess.restype=wintypes.BOOL
        kernel.WaitForSingleObject.argtypes=[wintypes.HANDLE,wintypes.DWORD];kernel.WaitForSingleObject.restype=wintypes.DWORD
        kernel.CloseHandle.argtypes=[wintypes.HANDLE];kernel.CloseHandle.restype=wintypes.BOOL
        handles=[]
        def checked_process(pid, executable):
            assert pid not in (0,os.getpid())
            handle=kernel.OpenProcess(0x101001,False,pid) # terminate + limited query + synchronize
            assert handle, "cannot open the owned native fixture process"
            handles.append(handle)
            image=ctypes.create_unicode_buffer(32768);size=wintypes.DWORD(len(image))
            assert kernel.GetProcessId(handle)==pid and kernel.QueryFullProcessImageNameW(handle,0,image,ctypes.byref(size))
            assert os.path.samefile(image.value,executable), "fixture process has a foreign executable"
            values=[wintypes.FILETIME() for _ in range(4)]
            assert kernel.GetProcessTimes(handle,*[ctypes.byref(value) for value in values])
            assert kernel.WaitForSingleObject(handle,0)==258, "fixture process already exited"
            return handle,(values[0].dwHighDateTime<<32)|values[0].dwLowDateTime
        try:
            selection=json.loads((expected_root/"bin/runtime.json").read_text())
            runtime_image=expected_root/"bin/runtime"/selection["version"]/runtime_name
            child,child_birth=checked_process(current["pid"], runtime_image)
            class ProcessEntry(ctypes.Structure):
                _fields_=[("dwSize",wintypes.DWORD),("cntUsage",wintypes.DWORD),
                    ("th32ProcessID",wintypes.DWORD),("th32DefaultHeapID",ctypes.c_size_t),
                    ("th32ModuleID",wintypes.DWORD),("cntThreads",wintypes.DWORD),
                    ("th32ParentProcessID",wintypes.DWORD),("pcPriClassBase",wintypes.LONG),
                    ("dwFlags",wintypes.DWORD),("szExeFile",wintypes.WCHAR*260)]
            kernel.CreateToolhelp32Snapshot.argtypes=[wintypes.DWORD,wintypes.DWORD]
            kernel.CreateToolhelp32Snapshot.restype=wintypes.HANDLE
            for name in ("Process32FirstW","Process32NextW"):
                method=getattr(kernel,name)
                method.argtypes=[wintypes.HANDLE,ctypes.POINTER(ProcessEntry)]
                method.restype=wintypes.BOOL
            snapshot=kernel.CreateToolhelp32Snapshot(2,0)
            assert snapshot not in (None,ctypes.c_void_p(-1).value), "process snapshot unavailable"
            parent_pid=None
            try:
                entry=ProcessEntry();entry.dwSize=ctypes.sizeof(entry)
                available=kernel.Process32FirstW(snapshot,ctypes.byref(entry))
                for _ in range(65536):
                    if not available:
                        break
                    if entry.th32ProcessID==current["pid"]:
                        parent_pid=entry.th32ParentProcessID
                        break
                    available=kernel.Process32NextW(snapshot,ctypes.byref(entry))
            finally:
                kernel.CloseHandle(snapshot)
            assert parent_pid is not None, "owned child absent from process snapshot"
            assert parent_pid != current["pid"]
            parent,parent_birth=checked_process(parent_pid,expected["executable"])
            assert parent_birth <= child_birth, "supervisor PID was reused after the owned child started"
            yield {"kernel":kernel,"parent":parent,"child":child,"parent_pid":parent_pid,"child_pid":current["pid"],"parent_birth":parent_birth,"child_birth":child_birth,"data_dir":current["data_dir"],"store_ready":current.get("store_ready",False)}
        finally:
            for handle in handles:
                kernel.CloseHandle(handle)

    def require_pair_exited(pair):
        assert pair["kernel"].WaitForSingleObject(pair["child"],15000)==0, "owned runtime child survived its lifecycle owner"
        assert pair["kernel"].WaitForSingleObject(pair["parent"],15000)==0, "owned supervisor remained alive after stop"

    def require_replacement_pair(old, expected_profile):
        require_pair_exited(old)
        with owned_windows_pair(root) as fresh:
            assert (fresh["child_pid"],fresh["child_birth"])!=(old["child_pid"],old["child_birth"])
            assert (fresh["parent_pid"],fresh["parent_birth"])!=(old["parent_pid"],old["parent_birth"])
            assert Path(fresh["data_dir"]).resolve()==expected_profile.resolve() and fresh["store_ready"]

    def diagnose_crash_restart():
        # Only this hosted fixture's fixed-field lifecycle state and
        # numeric scheduler policy/result. Never export XML or errors.
        directory=root/"runtime/autostart"
        counts={}
        for event_file in [directory/"events.jsonl",directory/"events.previous.jsonl"]:
            if event_file.is_file() and event_file.stat().st_size <= 65536:
                for line in event_file.read_text().splitlines():
                    event=json.loads(line).get("event")
                    if event in {"starting","paused","crash_limit","exit_ok","exit_error"}:
                        counts[event]=counts.get(event,0)+1
        print("isolated crash lifecycle diagnostic",json.dumps({"events":counts,"maintenance":(directory/"maintenance.json").exists()}))
        if os.name != "nt":
            return
        import base64
        import re
        identity=json.loads((directory/"registration.json").read_text())["id"]
        assert re.fullmatch("[0-9a-f]{24}",identity)
        script="""$ErrorActionPreference='Stop';[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);
$s=[Activator]::CreateInstance([Type]::GetTypeFromProgID('Schedule.Service'));$s.Connect();$t=$s.GetFolder('\\').GetTask('Respire-runtime-IDENTITY');$settings=$t.Definition.Settings;
[Console]::Out.WriteLine('state='+[int]$t.State);[Console]::Out.WriteLine('last_result='+[long]$t.LastTaskResult);
[Console]::Out.WriteLine('restart_count='+[int]$settings.RestartCount);$seconds=-1;try{$seconds=[int][Xml.XmlConvert]::ToTimeSpan($settings.RestartInterval).TotalSeconds}catch{};[Console]::Out.WriteLine('restart_seconds='+$seconds);
[Console]::Out.WriteLine('multiple_instances='+[int]$settings.MultipleInstances);[Console]::Out.WriteLine('enabled='+[int][bool]$t.Enabled);
[Console]::Out.WriteLine('instances='+[int]$t.GetInstances(0).Count);
""".replace("IDENTITY",identity)
        executable=Path(env["SystemRoot"])/"System32/WindowsPowerShell/v1.0/powershell.exe"
        try:
            result=subprocess.run([str(executable),"-NoProfile","-NonInteractive","-EncodedCommand",base64.b64encode(script.encode("utf-16le")).decode("ascii")],env=env,capture_output=True,text=True,timeout=20)
            fields={}
            for line in result.stdout.splitlines():
                match=re.fullmatch("(state|last_result|restart_count|restart_seconds|multiple_instances|enabled|instances)=(-?[0-9]{1,20})",line)
                if match:
                    fields[match[1]]=int(match[2])
            print("isolated scheduler crash diagnostic",json.dumps({"query_exit":result.returncode,"fields":fields}))
        except (OSError,subprocess.TimeoutExpired):
            print("isolated scheduler crash diagnostic",json.dumps({"available":False}))

    def diagnose_registration():
        import hashlib
        import platform
        if os.name != "posix":
            return
        identity = hashlib.sha256(f"respire-runtime-v1\0{root.resolve()}\0{os.getuid()}".encode()).hexdigest()[:24]
        system = platform.system()
        if system == "Linux":
            name = f"respire-runtime-{identity}.service"
            output = subprocess.run(["/usr/bin/systemctl", "--user", "show", name,
                "--property=LoadState,FragmentPath,DropInPaths,UnitFileState"], env=env,
                capture_output=True, text=True, timeout=15)
            print("isolated systemd path diagnostic", json.dumps({"expected":str(home.resolve()/".config/systemd/user"/name),"properties":output.stdout[:8192]}))
        elif system == "Darwin":
            label = f"ai.risense.respire.runtime.{identity}"
            output = subprocess.run(["/bin/launchctl", "print-disabled", f"gui/{os.getuid()}"],
                env=env, capture_output=True, text=True, timeout=15)
            matching = [line.strip() for line in output.stdout.splitlines() if line.strip().startswith(f'"{label}" => ')]
            print("isolated launchd override diagnostic",json.dumps({"label":label,"entries":[line[:160] for line in matching]}))

    def run(*argv):
        if argv[:1] == ("config",):
            # Host maintenance must use the exact registered controller image.
            registered_cli = Path(json.loads(config_path.read_text())["executable"])
            cli_name = "rsrs.exe" if os.name == "nt" else "rsrs"
            assert os.path.samefile(registered_cli, Path(env["RSRS_BIN_DIR"]) / cli_name)
            assert bundle_helpers["digest"](registered_cli) == bundle_helpers["digest"](Path(binary))
            result = subprocess.run([str(registered_cli), "--json", *argv], env=env,
                                    capture_output=True, text=True, timeout=45)
        else:
            result = command(*argv)
        if result.returncode != 0:
            diagnose_registration()
        assert result.returncode == 0, f"{argv}: {result.stderr} {result.stdout}"
        value = json.loads(result.stdout)
        assert value["status"] == "ok", value
        return value["summary"]

    try:
        if args.previous_binary:
            verify_previous_runtime(args, base, binary, bundle_helpers, copy_managed_installation,
                                    env, health, managed_fixture, owned_windows_pair, port,
                                    require_pair_exited, runtime_name, stage_runtime)

        # An invalid bin directory is now refused by read-only admission,
        # before maintenance. Keep that negative case, but do not pretend
        # it exercises the later persisted-failure recovery path.
        blocked_root=base/"blocked-installation"
        blocked=base/"blocked-native-directory"
        blocked.write_text("synthetic file, not a directory")
        rejected=subprocess.run([binary,"--json","runtime","autostart","enable"],
            env=dict(env,RSRS_DATA_DIR=str(blocked_root),RSRS_BIN_DIR=str(blocked)),
            capture_output=True,text=True,timeout=45)
        assert rejected.returncode!=0 and not blocked_root.exists(), "invalid bin path created lifecycle state"
        assert blocked.read_text()=="synthetic file, not a directory"

        # Reach the actual identity check AFTER admission/maintenance:
        # an independently owned, healthy off-mode Runtime occupies this
        # port, while a different verified installation tries to enable.
        # Neither a synthetic receipt nor a forged health response is used.
        failure_root=base/"failed-installation"
        failure_runtime=copy_managed_installation(managed_fixture / "bin",failure_root)
        failure_bin=failure_root/"bin"
        shutil.copy2(binary,failure_bin/("rsrs.exe" if os.name=="nt" else "rsrs"))
        failure_env=dict(env,RSRS_DATA_DIR=str(failure_root),RSRS_BIN_DIR=str(failure_bin),RSRS_RUNTIME_BIN=str(failure_runtime))
        owner_attempted=False
        owner_before=None
        try:
            owner_attempted=True
            owner_start=subprocess.run([binary,"--json","runtime","start"],env=off_env,capture_output=True,text=True,timeout=45)
            assert owner_start.returncode==0, f"identity fixture startup failed: {owner_start.stdout} {owner_start.stderr}"
            with backend_health(port) as response:
                owner_before=json.load(response)
            assert Path(owner_before["installation_root"]).resolve()==off_root.resolve()
            assert Path(owner_before["data_dir"]).resolve()==off_root.resolve()
            assert os.path.samefile(owner_before["exe"],off_env["RSRS_RUNTIME_BIN"])
            owner_endpoint=off_root/"runtime/endpoint.json"
            owner_endpoint_before=owner_endpoint.read_bytes()
            failed=subprocess.run([binary,"--json","runtime","autostart","enable"],env=failure_env,capture_output=True,text=True,timeout=45)
            assert failed.returncode!=0
            assert "runtime identity/profile does not match" in " ".join(json.loads(failed.stdout)["errors"]), failed.stdout
            assert (failure_root/"runtime/autostart/maintenance.json").is_file(), "identity failure did not reach persisted maintenance"
            assert owner_endpoint.read_bytes()==owner_endpoint_before
            with backend_health(port) as response:
                owner_after=json.load(response)
            for key in ("pid","instance_id","process_start_id","image_sha256","account_id","generation"):
                assert owner_after[key]==owner_before[key], f"wrong-installation enable replaced owner: {key}"
        finally:
            primary_error=sys.exc_info()[1]
            try:
                # Even a failed/timed-out start may have created its child.
                # Only the original installation may drain this owner.
                # Never kill by port or manually clear a maintenance fence.
                if owner_attempted:
                    stopped=subprocess.run([binary,"--json","runtime","stop"],env=off_env,capture_output=True,text=True,timeout=45)
                    assert stopped.returncode==0, f"identity owner cleanup failed: {stopped.stdout} {stopped.stderr}"
                    assert not (off_root/"runtime/endpoint.json").exists()
                    if owner_before is not None:
                        # Stop=0 alone could mean "already stopped". Require
                        # the original instance's actual final drain receipt.
                        receipt=json.loads(stopped.stdout)["summary"]
                        for key in ("pid","generation","installation_id","instance_id","process_start_id","image_path","image_sha256","runtime_version","account_id"):
                            assert receipt[key]==owner_before[key], f"cleanup receipt owner mismatch: {key}"
                        assert receipt["schema_version"]==1 and receipt["protocol_version"]==owner_before["v"]
                        assert receipt["operation_id"] and receipt["state"]=="drained" and receipt["unfinished"]==[]
                        receipt_file=off_root/"runtime"/f"stop-{owner_before['instance_id']}.json"
                        assert not receipt_file.is_symlink() and receipt_file.stat().st_size<=1024*1024
                        assert json.loads(receipt_file.read_text())==receipt
                        # A reused live PID conservatively fails/retains the
                        # directory. Never infer OS exit from a closed port.
                        deadline=time.monotonic()+15
                        while True:
                            if os.name=="nt":
                                handle=kernel.OpenProcess(0x100000,False,owner_before["pid"])
                                if handle:
                                    try:
                                        exited=kernel.WaitForSingleObject(handle,0)==0
                                    finally:
                                        kernel.CloseHandle(handle)
                                else:
                                    assert ctypes.get_last_error()==87, "could not verify fixture process exit"
                                    exited=True
                            else:
                                state=subprocess.run(["ps","-p",str(owner_before["pid"]),"-o","stat="],env=off_env,capture_output=True,text=True,timeout=5)
                                assert state.returncode in (0,1) and not state.stderr.strip(), "could not verify fixture process exit"
                                exited=(state.returncode==1 and not state.stdout.strip()) or state.stdout.strip().startswith("Z")
                            if exited:
                                break
                            assert time.monotonic()<deadline, "inspected fixture PID still exists after drain"
                            time.sleep(0.1)
                cleaned=subprocess.run([binary,"--json","runtime","autostart","remove","--stop"],env=failure_env,capture_output=True,text=True,timeout=45)
                assert cleaned.returncode == 0 and not (failure_root/"runtime/autostart/maintenance.json").exists(), f"explicit failed-install cleanup did not recover: {cleaned.stdout} {cleaned.stderr}"
            except Exception as cleanup_error:
                if primary_error is None:
                    raise
                print(f"identity fixture cleanup also failed: {cleanup_error}",file=sys.stderr)
        print("PASS: verified cross-installation owner refused without replacement; persisted fence cleared only by explicit recovery after owner drain")
        if sys.platform.startswith("linux"):
            env["RSRS_RUNTIME_BIN"] = str(copy_managed_installation(managed_fixture / "bin", root))
            seed_local_credentials(binary, env, keys, root)
        # Unbound receipts belong to the installation, not just the session file.
        secondary = root / "accounts/secondary-profile"
        selected = command("account", "use", "secondary-profile")
        assert selected.returncode == 0, "secondary profile selection failed"
        seed_local_credentials(binary, env, keys, secondary)
        assert (secondary / "rsrs.db").is_file(), "secondary fixture has no initialized store"
        selected = command("account", "use", "main")
        assert selected.returncode == 0, "main profile selection failed"
        primary_session = json.loads((root / "session.json").read_text(encoding="utf-8"))
        secondary_session = json.loads((secondary / "session.json").read_text(encoding="utf-8"))
        assert secondary_session["keyring_account"] != primary_session["keyring_account"], "profile fixtures share credentials"
        enabled = run("runtime", "autostart", "enable")
        assert enabled["enabled"] is True and enabled["registered"] is True, enabled
        assert enabled["runtime"]["running"] is False, "enable without --now unexpectedly starts runtime"
        run("runtime", "autostart", "enable")  # idempotent, same installation
        run("runtime", "start")
        up = run("runtime", "status")
        assert up["runtime"]["running"] is True and up["runtime"]["store_ready"] is True, up
        first_pid = up["runtime"]["pid"]
        config_path = root / "runtime/autostart/registration.json"
        assert os.path.samefile(json.loads(config_path.read_text())["executable"], binary), "registration changed the CLI image used for key custody"
        # Observe the managed child's captured opt-out through the
        # existing client-only RPC, with the caller deliberately set
        # to the opposite value. No sync or network write is requested.
        status_reply=subprocess.run([binary,"--json","--client-only","status"],
            env=dict(env,RSRS_NO_AUTOSYNC="0"),capture_output=True,text=True,timeout=45)
        assert status_reply.returncode == 0, f"managed status query failed: {status_reply.stdout} {status_reply.stderr}"
        runtime_status=json.loads(status_reply.stdout)
        assert runtime_status["command"] == "status" and runtime_status["summary"]["autosync"] is False, "managed runtime lost explicit no-autosync opt-out"
        print("PASS: managed child retains no-autosync opt-out independently of caller environment")
        unchanged_config = config_path.read_bytes()
        with socket.socket() as other_socket:
            other_socket.bind(("127.0.0.1",0))
            other_port = other_socket.getsockname()[1]
        wrong_endpoint = subprocess.run([binary,"--json","runtime","stop"],
            env=dict(env,RSRS_RPC_PORT=str(other_port)),capture_output=True,text=True,timeout=45)
        assert wrong_endpoint.returncode != 0 and config_path.read_bytes() == unchanged_config
        assert run("runtime","status")["runtime"]["pid"] == first_pid, "wrong-port stop changed the owned process"
        if os.name == "posix" and os.uname().sysname == "Linux":
            assert Path(f"/proc/{first_pid}/cwd").resolve() == root.resolve(), "supervised process has the wrong actual working directory"
        # Deliberate SIGKILL/TerminateProcess does not flush LLVM
        # counters; run this fault in the dedicated uninstrumented
        # three-platform job, while coverage keeps graceful profiles.
        if not os.environ.get("LLVM_PROFILE_FILE"):
            # Kill only the fixture's just-verified managed owner. Observe
            # HTTP readiness directly afterwards: invoking runtime start or
            # another auto-starting CLI command would hide a broken OS
            # restart policy. No user registration/data is involved.
            import signal
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/api/health",timeout=5) as response:
                owned_health=json.load(response)
            assert owned_health["pid"] == first_pid and Path(owned_health["installation_root"]).resolve() == root.resolve()
            assert owned_health["managed_id"] == json.loads(config_path.read_text())["id"]
            failed_owner=owned_windows_pair(root) if os.name=="nt" else nullcontext(None)
            with failed_owner as pair:
                if pair is not None:
                    assert pair["child_pid"]==first_pid
                    supervisor_before=(pair["parent_pid"],pair["parent_birth"])
                    assert pair["kernel"].TerminateProcess(pair["child"],1), "failed to inject owned child failure"
                    assert pair["kernel"].WaitForSingleObject(pair["child"],15000)==0
                else:
                    os.kill(first_pid,signal.SIGKILL)
                restart_deadline=time.monotonic()+140
                recovered_health=None
                candidate=None
                while time.monotonic() < restart_deadline:
                    try:
                        with urllib.request.urlopen(f"http://127.0.0.1:{port}/api/health",timeout=1) as response:
                            candidate=json.load(response)
                        if candidate["pid"] != first_pid and candidate.get("store_ready"):
                            recovered_health=candidate
                            break
                    except (OSError,ValueError):
                        pass
                    time.sleep(0.25)
                if recovered_health is None:
                    if candidate is not None:
                        print("isolated crash health diagnostic",json.dumps({
                            "replacement":candidate.get("pid") != first_pid,
                            "managed_owner":candidate.get("managed_id") == owned_health["managed_id"],
                            "startup_complete":candidate.get("startup_complete"),
                            "store_ready":candidate.get("store_ready"),
                            "credentials_required":candidate.get("credential_state") == "required",
                        }))
                    diagnose_crash_restart()
                assert recovered_health is not None, "OS supervisor did not restart its failed native owner"
                assert recovered_health["managed_id"] == owned_health["managed_id"] and recovered_health["bin"] == health["bin"]
                assert Path(recovered_health["installation_root"]).resolve() == root.resolve()
                first_pid=recovered_health["pid"]
                print("PASS: actual abnormal process exit followed by supervised store-ready restart, without a CLI start request")
                if pair is not None:
                    with owned_windows_pair(root) as fresh:
                        assert fresh["child_pid"]==recovered_health["pid"] and fresh["child_birth"]!=pair["child_birth"]
                        assert (fresh["parent_pid"],fresh["parent_birth"])==supervisor_before
                        assert fresh["store_ready"] and Path(fresh["data_dir"]).resolve()==root.resolve()
            if os.name == "nt":
                with owned_windows_pair(root) as pair:
                    assert (pair["parent_pid"],pair["parent_birth"])==supervisor_before, "child recovery unexpectedly replaced its supervisor"
                    assert pair["kernel"].TerminateProcess(pair["parent"],1), "failed to inject owned supervisor failure"
                    require_pair_exited(pair)
                # This proves job-bound cleanup, not automatic parent
                # recovery. Explicit host restart establishes a new pair.
                run("runtime","restart")
                with owned_windows_pair(root) as pair:
                    assert pair["parent_birth"]!=supervisor_before[1]
                    first_pid=pair["child_pid"]
                print("PASS: actual supervisor kill removes its owned child; explicit restart verifies a new store-ready pair without an orphan")
        # Keep the exited Windows process objects referenced until the
        # replacement exists. Otherwise a legitimate numeric PID reuse
        # can invalidate a PID-only comparison after handles are closed.
        stopped_owner=owned_windows_pair(root) if os.name=="nt" else nullcontext(None)
        with stopped_owner as pair:
            if pair is not None:
                first_pid=pair["child_pid"]
            run("runtime", "stop")
            if pair is not None:
                require_pair_exited(pair)
            time.sleep(1)
            stopped = run("runtime", "status")
            assert stopped["runtime"]["running"] is False, "manual stop was automatically resurrected"
            run("runtime", "restart")
            restarted = run("runtime", "status")
            assert restarted["runtime"]["running"] and restarted["runtime"]["pid"] != first_pid, restarted
            if pair is not None:
                with owned_windows_pair(root) as replacement:
                    assert replacement["child_pid"]==restarted["runtime"]["pid"]
                    assert replacement["child_birth"]!=pair["child_birth"] and replacement["parent_birth"]!=pair["parent_birth"], "restart reused the old process instances"
        previous_pid = restarted["runtime"]["pid"]
        for target in (secondary, root):
            switching_owner = owned_windows_pair(root) if os.name == "nt" else nullcontext(None)
            with switching_owner as pair:
                run("config", "--data-dir", str(target))
                # Unbound config selection stays idle; only explicit start unlocks its store.
                selected = run("runtime", "status")
                assert selected["runtime"]["running"] is False, "unbound config switch unexpectedly started Runtime"
                assert not (root / "runtime/autostart/maintenance.json").exists(), "config switch left pending maintenance"
                run("runtime", "start")
                if pair is not None:
                    require_replacement_pair(pair, target)
            switched = run("runtime", "status")
            assert Path(switched["runtime"]["data_dir"]).resolve() == target.resolve(), switched
            assert switched["runtime"]["store_ready"] is True, "selected profile did not unlock its store"
            assert switched["runtime"]["pid"] != previous_pid
            previous_pid = switched["runtime"]["pid"]
        print("PASS: independent unbound profile switch and return stay idle until explicit start; verified new store-ready owners")

        # Persisted interrupted-controller fixtures are exercised by the
        # actual CLI and OS supervisor. This is process recovery from
        # recorded states, not a physical power-loss experiment.
        import uuid
        registration = root / "runtime/autostart/registration.json"
        marker = registration.with_name("maintenance.json")
        for phase in ("Paused", "Starting", "Verified"):
            if phase != "Starting":
                run("runtime", "stop")
            config = json.loads(registration.read_text())
            if phase == "Starting":
                with urllib.request.urlopen(f"http://127.0.0.1:{port}/api/health", timeout=5) as response:
                    maintenance_health=json.load(response)
                assert Path(maintenance_health["installation_root"]).resolve() == root.resolve()
                assert maintenance_health["managed_id"] == config["id"]
                maintenance_scope={key: maintenance_health[key] for key in ("account_id", "instance_id", "generation")}
                assert maintenance_health["v"] == 1 and all(maintenance_scope.values())
            stopped_flag = config["manually_stopped"]
            config["maintenance"] = True
            registration.write_text(json.dumps(config))
            value = {"schema":1,"id":config["id"],"token":str(uuid.uuid4()),"phase":phase,
                "expected_profile":str(root.resolve()),"expected_version":restarted["runtime"]["version"],
                "owner_pid":os.getpid(),"owner_birth":"synthetic-expired-controller"}
            with os.fdopen(os.open(marker,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600),"w") as pending:
                json.dump(value,pending)
                pending.flush()
                os.fsync(pending.fileno())
            database = root / "rsrs.db"
            before = database.read_bytes()
            modified = database.stat().st_mtime_ns
            denied_start = command("runtime", "start")
            assert denied_start.returncode != 0 and marker.exists()
            assert json.loads(registration.read_text())["manually_stopped"] == stopped_flag
            denied_read = command("--direct", "memory-revision")
            assert denied_read.returncode != 0 and marker.exists()
            if phase == "Starting":
                for method in ("model.prepare", "index.prepare"):
                    request = urllib.request.Request(f"http://127.0.0.1:{port}/api/rpc",
                        data=json.dumps({"v":1,"id":str(uuid.uuid4()),"method":method,"args":[],"scope":maintenance_scope}).encode(),
                        headers={"Content-Type":"application/json"})
                    with urllib.request.urlopen(request,timeout=5) as response:
                        reply=json.load(response)
                    assert reply["ok"] is False and reply["code"] == "runtime_maintenance", reply
            assert database.read_bytes() == before and database.stat().st_mtime_ns == modified
            run("runtime", "restart")
            if phase == "Starting":
                assert read_restart_stages()[-3:] == ["restart_probe","restart_managed","restart_ready"], "live managed recovery did not follow the verified coordinator"
            recovered = run("runtime", "status")
            assert recovered["runtime"]["running"] and recovered["runtime"]["store_ready"]
            assert not marker.exists() and not json.loads(registration.read_text())["maintenance"]
            print(f"PASS: native persisted {phase} recovery; direct reads/preparation fenced; manual-stop intent retained until explicit restart")

        # Managed off-mode uses process/profile readiness, not a store
        # that this mode intentionally never opens.
        run("runtime", "stop")
        client = root / "client.json"
        previous_client = client.read_bytes() if client.exists() else None
        settings = json.loads(previous_client) if previous_client else {}
        database = root / "rsrs.db"
        before_database = database.read_bytes()
        before_modified = database.stat().st_mtime_ns
        settings["service_mode"] = "off"
        client.write_text(json.dumps(settings))
        run("runtime", "restart")
        off_status = run("runtime", "status")
        assert off_status["runtime"]["running"] and off_status["runtime"]["store_ready"] is False, off_status
        assert database.read_bytes() == before_database and database.stat().st_mtime_ns == before_modified
        run("runtime", "stop")
        if previous_client is None:
            client.unlink()
        else:
            client.write_bytes(previous_client)
        run("runtime", "restart")
        assert run("runtime", "status")["runtime"]["store_ready"] is True
        run("runtime", "autostart", "disable", "--stop")
        disabled = run("runtime", "status")
        assert disabled["enabled"] is False and disabled["runtime"]["running"] is False, disabled
        run("runtime", "autostart", "enable", "--now")
        assert run("runtime", "status")["runtime"]["running"] is True
        preserved = root / "synthetic-preserved-file"
        preserved.write_text("must survive uninstall")
        run("runtime", "autostart", "remove", "--stop")
        removed = run("runtime", "status")
        assert not removed["registered"] and not removed["enabled"] and not removed["runtime"]["running"], removed
        assert preserved.read_text() == "must survive uninstall"
        print("PASS: actual current-user register/readback/start/stop/restart/disable/reenable/remove; data retained")
    finally:
        # The CLI revalidates exact owner/action/files before cleanup;
        # never delete unknown OS tasks/units merely to make a test pass.
        primary_error=sys.exc_info()[1]
        try:
            cleanup = command("runtime", "autostart", "remove", "--stop")
            assert cleanup.returncode == 0, f"Owned registration cleanup failed: {cleanup.stderr} {cleanup.stdout}"
            collect_managed_profiles()
        except Exception as cleanup_error:
            if primary_error is None:
                raise
            print(f"owned registration cleanup also failed: {cleanup_error}",file=sys.stderr)
