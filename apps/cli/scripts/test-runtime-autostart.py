#!/usr/bin/env python3
"""Actual native lifecycle in a disposable home; optional owned OS registration.

The CLI and an extracted independent Runtime bundle are inputs. Only disposable
CLI-created credentials are used; no user data or inherited Respire configuration.
Real sign-in service adapters are exercised only with --register on an ephemeral hosted
runner; a foreground-only run does not claim registration acceptance.
"""
import argparse
import json
import os
from pathlib import Path
import re
import socket
import runpy
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request
import uuid

from native_fixture_process import same_existing_path
from runtime_autostart_registration import verify_registration


def backend_health(port):
    # These callers verify the native Runtime's OS identity, not the legacy host view.
    request = urllib.request.Request(f"http://127.0.0.1:{port}/api/health",
                                     headers={"X-Respire-Health": "runtime-v1"})
    return urllib.request.urlopen(request, timeout=5)


def foreground_health_ready(health, root, runtime, expected_ready, owner):
    assert health["server"] == "respire" and health["managed_id"] is None, "unexpected foreground owner"
    assert same_existing_path(health["exe"], runtime), "foreground Runtime image differs"
    assert same_existing_path(health["installation_root"], root), "foreground installation differs"
    assert same_existing_path(health["data_dir"], root), "foreground profile differs"
    assert isinstance(health["instance_id"], str) and health["instance_id"], "foreground instance missing"
    identity = tuple(health[key] for key in
                     ("pid", "instance_id", "process_start_id", "image_sha256"))
    assert owner is None or identity == owner, "foreground owner changed during startup"
    return identity, health.get("startup_complete") is True and health.get("store_ready") is expected_ready


def seed_local_credentials(binary, env, keys, profile):
    """Create only an owned, unbound session through the public host transaction."""
    root = Path(env["RSRS_DATA_DIR"]).resolve()
    profile = profile.resolve()
    session_path = profile / "session.json"
    journal = root / "runtime/autostart/key-control.json"
    results = journal.parent / "key-results"
    offline = journal.with_name("offline-keygen.json")
    assert not any(path.exists() for path in (session_path, journal, results, offline)), "key fixture must be fresh"
    stage = None
    alias = None

    def checked(result, action):
        if result.returncode == 0:
            return
        # CLI output may contain the Super Key; report fixed classifications only.
        try:
            value = json.loads(result.stdout)
        except (ValueError, UnicodeError, RecursionError):
            value = None
        errors = value.get("errors") if isinstance(value, dict) else None
        text = "\n".join(error for error in errors if isinstance(error, str)) if isinstance(errors, list) else ""
        text += "\n" + result.stderr
        categories = (
            ("original_operation_changed", "offline keygen original operation changed"),
            ("handoff_unsupported", "offline_keygen_handoff_unsupported"),
            ("runtime_repair_required", "repair required"),
            ("unknown_owner", "offline keygen owner is unverifiable"),
            ("live_or_unknown_owner", "offline keygen live or unknown owner exists"),
            ("native_custody", "host auth credential"),
            ("control_spawn", "cannot start owned host control Runtime"),
            ("control_response", "host control response"),
            ("control_exit", "host control Runtime exit"),
            ("account_rejected", "Runtime rejected account control"),
            ("selection_changed", "offline keygen installation, selection or image changed"),
            ("maintenance_pending", "runtime maintenance is incomplete"),
            ("source_session_invalid", "invalid auth source session"),
            ("path_missing", "(os error 2)"),
            ("directory_missing", "(os error 3)"),
            ("access_denied", "(os error 5)"),
            ("sharing_violation", "(os error 32)"),
        )
        diagnostic = {"returncode": result.returncode,
                      "category": next((name for name, marker in categories if marker in text), "unclassified"),
                      "stdout_json": isinstance(value, dict), "stderr_present": bool(result.stderr),
                      "offline_stage": stage, "material_tracked": alias is not None,
                      "session_present": session_path.exists()}
        raise AssertionError(f"synthetic {action} failed: {json.dumps(diagnostic, sort_keys=True)}")

    try:
        try:
            generated = subprocess.run([binary, "--json", "keygen"], env=env,
                                       capture_output=True, text=True, timeout=45)
        finally:
            # The offline controller checkpoints its independently generated alias
            # before native custody, including when keygen fails after the write.
            if offline.exists():
                assert not offline.is_symlink(), "key fixture journal must not be linked"
                binding = json.loads(offline.read_text(encoding="utf-8"))
                operation = uuid.UUID(binding["operation_id"])
                assert binding["schema"] == 1 and operation.version == 4 and str(operation) == binding["operation_id"], "invalid key fixture operation"
                assert os.path.samefile(binding["root"], root) and all(os.path.samefile(binding[key], profile) for key in ("source", "profile")), "foreign key fixture owner"
                stage = binding["stage"]
                assert stage in ("prepared", "sent", "committed", "starting", "verified", "finished", "handed_off",
                                 "rolled_back", "rollback_starting", "rollback_finished", "rollback_handed_off"), "invalid key fixture stage"
                material = binding.get("material")
                if material is not None:
                    alias = material.get("keyring_account")
                    assert material.get("auth_operation_id") == binding["operation_id"] and material.get("vault_version") == 4, "foreign key fixture material"
                    assert isinstance(alias, str) and re.fullmatch(r"login-[a-f0-9]{32}", alias), "invalid keygen fixture alias"
                    assert material.get("keyring_backend") == keys.credential_backend, "keygen fixture custody differs"
                    keys.entries.add(("rsrs", "super:" + alias))
        checked(generated, "CLI keygen")
        assert not session_path.is_symlink(), "key fixture session must not be linked"
        session = json.loads(session_path.read_text(encoding="utf-8"))
        assert not session.get("user") and session.get("vault_version") == 4, "keygen must create an unbound v4 session"
        assert alias is not None and session.get("keyring_account") == alias, "keygen session differs from owned material"
        assert ("rsrs", "super:" + alias) in keys.entries and session.get("keyring_backend") == keys.credential_backend, "keygen fixture custody differs"
        assert not any(key in session for key in ("pass", "super", "secret", "secret_key")), "keygen retained inline credentials"
        started = subprocess.run([binary, "--json", "runtime", "start"], env=env,
                                 capture_output=True, text=True, timeout=45)
        checked(started, "key handoff start")
        status = subprocess.run([binary, "--json", "runtime", "status"], env=env,
                                capture_output=True, text=True, timeout=45)
        checked(status, "key handoff status")
        assert json.loads(status.stdout)["summary"]["runtime"]["store_ready"] is True, "host did not unlock synthetic credentials"
    finally:
        primary_error = sys.exc_info()[1]
        try:
            stopped = subprocess.run([binary, "--json", "runtime", "stop"], env=env,
                                     capture_output=True, text=True, timeout=45)
            checked(stopped, "key fixture stop")
        except Exception as cleanup_error:
            if primary_error is None:
                raise
            primary_error.add_note("synthetic key fixture cleanup also failed; fixture retained")
            raise primary_error from cleanup_error


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--runtime-binary", type=Path, help="Independent Runtime in its extracted bundle; defaults to RSRS_RUNTIME_BIN or the CLI's sibling rsrs-runtime")
    parser.add_argument("--runtime-archive", type=Path, default=os.environ.get("RSRS_RUNTIME_ARCHIVE"), help="Release archive with adjacent trusted runtime-build-<triple>.json")
    parser.add_argument("--runtime-version", default=os.environ.get("RSRS_RUNTIME_VERSION"))
    parser.add_argument("--register", action="store_true", help="Exercise a disposable current-user OS registration on an ephemeral GitHub runner")
    parser.add_argument("--previous-binary", type=Path, help="Pinned official DEV.6 native executable for upgrade acceptance")
    parser.add_argument("--keyless-foreground-diagnostic", type=Path, help="Run only keyless foreground and write fixed safe evidence; never full acceptance")
    args = parser.parse_args()
    if args.keyless_foreground_diagnostic and args.register:
        parser.error("keyless diagnostic cannot exercise OS registration")
    if args.previous_binary and not args.register:
        parser.error("previous-version upgrade acceptance requires --register")
    if args.register and not (os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted"):
        parser.error("OS registration acceptance is limited to an ephemeral GitHub-hosted runner")
    if not (os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted"):
        parser.error("native credential lifecycle acceptance requires an ephemeral GitHub-hosted runner")
    binary = str(args.binary.resolve(strict=True))
    runtime_name = "rsrs-runtime.exe" if os.name == "nt" else "rsrs-runtime"
    selected_runtime = args.runtime_binary or os.environ.get("RSRS_RUNTIME_BIN")
    if args.runtime_archive and (selected_runtime or not args.runtime_version):
        parser.error("Runtime archive requires a version and cannot be combined with a Runtime binary")
    if args.runtime_version and not args.runtime_archive:
        parser.error("Runtime version requires a Runtime archive")
    runtime_binary = None if args.runtime_archive else Path(selected_runtime or Path(binary).with_name(runtime_name)).absolute()
    if runtime_binary is not None and not runtime_binary.is_file():
        parser.error("independent Runtime candidate missing; supply --runtime-binary/RSRS_RUNTIME_BIN or --runtime-archive with --runtime-version")
    if runtime_binary is not None and os.path.samefile(binary, runtime_binary):
        parser.error("independent Runtime required; the CLI cannot be its Runtime candidate")

    bundle_helpers = runpy.run_path(str(Path(__file__).with_name("runtime-acceptance-bundle.py")))
    prepare_bundle = bundle_helpers["stage_runtime"]
    copy_managed_installation = bundle_helpers["copy_managed_installation"]

    def stage_runtime(directory, installation_root=None):
        return prepare_bundle(binary, runtime_binary, directory,
                              args.runtime_archive if runtime_binary is None else None,
                              args.runtime_version if runtime_binary is None else None,
                              installation_root=installation_root)

    # Failed startup/cleanup can leave a detached owner or OS registration.
    # Preserve its files for explicit recovery instead of deleting live paths.
    temporary = tempfile.mkdtemp(prefix="respire-autostart-", dir=os.environ["RUNNER_TEMP"])
    keys = None
    try:
        base = Path(temporary).resolve(strict=True)
        home = base / "home"
        home.mkdir()
        root = base / ('data spaces % $ " \\' if os.name == "posix" else "data spaces % and $")
        with socket.socket() as port_socket:
            port_socket.bind(("127.0.0.1", 0))
            port = port_socket.getsockname()[1]
        env = {key: os.environ[key] for key in (
            "PATH", "LANG", "LC_ALL", "SystemRoot", "WINDIR", "TEMP", "TMP",
            "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS", "LLVM_PROFILE_FILE",
            "APPDATA", "LOCALAPPDATA", "COMPUTERNAME", "COMSPEC", "PATHEXT",
            "ProgramFiles", "ProgramFiles(x86)", "ProgramData"
        ) if key in os.environ}
        if args.register and os.name == "posix" and os.uname().sysname == "Linux":
            import stat
            runtime = Path(f"/run/user/{os.getuid()}")
            if runtime.is_dir() and runtime.stat().st_uid == os.getuid():
                bus = runtime / "bus"
                if bus.exists() and stat.S_ISSOCK(bus.stat().st_mode):
                    env.setdefault("XDG_RUNTIME_DIR", str(runtime))
                    env.setdefault("DBUS_SESSION_BUS_ADDRESS", f"unix:path={bus}")
        env.update(HOME=str(home), USERPROFILE=str(home), RSRS_DATA_DIR=str(root),
                   RSRS_BIN_DIR=str(base / "bin"), RSRS_RPC_PORT=str(port),
                   RSRS_CORE_TEST_MODE="1", RSRS_NO_AUTOSYNC="1", RSRS_UPDATE_CHECK="0")
        managed_fixture = base / "managed-installation"
        # Restart/drain and registration tests must start from the installer's
        # original release proof, not an unbound explicit Runtime override.
        runtime_binary = stage_runtime(base / "bin", managed_fixture)
        shutil.copy2(binary, base / "bin" / ("rsrs.exe" if os.name == "nt" else "rsrs"))
        binary = str(base / "bin" / ("rsrs.exe" if os.name == "nt" else "rsrs"))
        env["RSRS_RUNTIME_BIN"] = str(base / "bin" / runtime_name)
        helper = "dev-migration-secret-service.py" if sys.platform.startswith("linux") else "dev-migration-keyring.py"
        credential_helpers = runpy.run_path(str(Path(__file__).with_name(helper)))
        if sys.platform.startswith("linux"):
            os.environ.update(credential_helpers["hosted_user_bus_environment"]())
            keys = credential_helpers["create"](base / "keyring-home", hosted_user_bus=True)
            keys.configure_business_env(env)
        else:
            keys = credential_helpers["create"](base / "keyring-home")
            keys.configure_env(env)

        restart_stages={"restart_probe","restart_managed","restart_identity","restart_stop","restart_start","restart_ready"}
        def read_restart_stages():
            events=root/"runtime/autostart/events.jsonl"
            try:
                if not events.is_file() or events.is_symlink() or events.stat().st_size>65536:
                    return []
                return [value["event"] for line in events.read_text().splitlines()
                        if isinstance(value:=json.loads(line),dict) and value.get("event") in restart_stages]
            except (OSError,ValueError):
                return [] # Diagnostics must not replace the original timeout.

        def command(*argv):
            try:
                return subprocess.run([binary, "--json", *argv], env=env,
                                      capture_output=True, text=True, timeout=45)
            except subprocess.TimeoutExpired:
                # Fixed stage names only: never dump captured stderr, arguments,
                # registration, raw errors or private account content.
                print("isolated restart stage diagnostic",json.dumps({"stages":read_restart_stages()[-12:]}),flush=True)
                raise

        status = command("runtime", "autostart", "status")
        assert status.returncode == 0, status.stderr
        summary = json.loads(status.stdout)["summary"]
        assert summary["enabled"] is False and summary["registered"] is False
        assert not root.exists(), "status must not create a profile"
        denied = command("--client-only", "runtime", "autostart", "enable", "--now")
        assert denied.returncode != 0 and not root.exists()
        def foreground(expected_ready):
            with (base / ("foreground-ready.log" if expected_ready else "foreground-locked.log")).open("w+") as log:
                child = subprocess.Popen([binary, "runtime", "run"], env=env,
                                         stdout=log, stderr=log)
                foreground_stopped = False
                stop_returncode = None
                health = None
                owner = None
                try:
                    deadline = time.monotonic() + 30
                    while time.monotonic() < deadline:
                        if child.poll() is not None:
                            raise AssertionError("foreground exited before readiness")
                        try:
                            with urllib.request.urlopen(
                                f"http://127.0.0.1:{port}/api/health", timeout=1
                            ) as response:
                                candidate = json.load(response)
                            identity, ready = foreground_health_ready(candidate, root, env["RSRS_RUNTIME_BIN"], expected_ready, owner)
                            assert candidate["pid"] != child.pid, "foreground health identified the CLI instead of Runtime"
                            health, owner = candidate, identity
                            if ready:
                                break
                        except (OSError, ValueError):
                            pass
                        time.sleep(0.1)
                    assert health and health.get("startup_complete") is True and health["store_ready"] is expected_ready, "foreground readiness differs from fixture credentials"
                    if not expected_ready:
                        assert health["credential_state"] == "required", "keyless foreground must require credentials"
                        assert not (root / "session.json").exists(), "keyless startup created a session"
                        assert not (root / "rsrs.db").exists() and not (root / "onememory.db").exists(), "keyless startup opened a library"
                    # Status runs inside Runtime and reports its own environment's
                    # autosync switch without needing keys or an inference model.
                    query=command("--client-only","status")
                    status_reply=json.loads(query.stdout)
                    assert status_reply["command"] == "status"
                    if expected_ready:
                        assert query.returncode == 0, "unlocked foreground status failed"
                        assert status_reply["summary"]["autosync"] is False
                    else:
                        assert query.returncode == 2 and status_reply["status"] == "warn", "locked status must report a credential warning"
                        assert status_reply["summary"]["credential_state"] == "required"
                        assert status_reply["summary"]["store_ready"] is False
                    assert child.poll() is None, "status RPC terminated the foreground host"
                    stopped = command("runtime", "stop")
                    stop_returncode = stopped.returncode
                    assert stopped.returncode == 0, stopped.stderr
                    foreground_stopped = True
                    assert child.wait(timeout=20) == 0
                    assert not (root / "runtime/autostart/registration.json").exists()
                except Exception:
                    print("isolated foreground diagnostic", json.dumps({
                        "temporary": temporary,
                        "base": str(base),
                        "root": str(root),
                        "foreground_returncode": child.poll(),
                        "foreground_log": log.name,
                    }), file=sys.stderr, flush=True)
                    raise
                finally:
                    primary_error = sys.exc_info()[1]
                    try:
                        # No observed Runtime means there is no verified HTTP owner
                        # to clean up; retain the original foreground startup error.
                        if not foreground_stopped and health is not None:
                            with urllib.request.urlopen(f"http://127.0.0.1:{port}/api/health", timeout=5) as response:
                                cleanup_health=json.load(response)
                            assert cleanup_health["server"] == "respire" and cleanup_health["managed_id"] is None
                            assert os.path.samefile(cleanup_health["exe"], env["RSRS_RUNTIME_BIN"])
                            assert Path(cleanup_health["installation_root"]).resolve() == root.resolve()
                            assert Path(cleanup_health["data_dir"]).resolve() == root.resolve()
                            for key in ("pid", "instance_id", "process_start_id", "image_sha256", "account_id", "generation"):
                                assert cleanup_health[key] == health[key], f"foreground owner changed before cleanup: {key}"
                            # Host stop verifies native birth identity, waits for the real
                            # Runtime to exit and validates its final drained receipt.
                            cleanup=command("runtime", "stop")
                            assert cleanup.returncode == 0, f"foreground Runtime cleanup failed: {cleanup.stdout} {cleanup.stderr}"
                        if child.poll() is None:
                            assert child.wait(timeout=20) == 0, "foreground CLI did not exit after Runtime stopped"
                    except Exception as cleanup_error:
                        if primary_error is None:
                            raise
                        print(f"foreground cleanup also failed: {cleanup_error}", file=sys.stderr)
                    finally:
                        try:
                            if args.keyless_foreground_diagnostic or (not expected_ready and primary_error is not None):
                                # Export only fixed markers and primitive state, never raw logs/journals.
                                log.flush()
                                log.seek(0)
                                text = log.read()
                                markers = (
                                    "background core owner exited", "background core owner changed",
                                    "core config owner exited", "core config owner is unavailable",
                                    "core config owner changed", "core config response missing envelope",
                                    "agent_config_outcome_unknown", "core_config_rejected",
                                    "core config response binding mismatch", "core config request rejected",
                                    "core config response missing", "core config response state mismatch",
                                    "runtime_draining", "runtime_stopping", "runtime_unavailable",
                                    "core config read failed", "core config validate failed", "core config apply failed",
                                    "Connection refused", "Connection reset", "connection closed",
                                    "Broken pipe", "timed out", "cannot reap foreground Runtime",
                                    "Runtime drain thread panicked", "No such file or directory",
                                    "Permission denied", "database is locked", "database is busy",
                                )
                                journals = []
                                for path in sorted((root / "runtime/autostart/core-config-applications").glob("*.json")):
                                    assert path.is_file() and not path.is_symlink(), "invalid diagnostic journal"
                                    control = json.loads(path.read_text(encoding="utf-8"))["control"]
                                    response = control.get("response")
                                    journals.append({"apply_sent": control.get("apply_sent") is True,
                                                     "response_present": response is not None,
                                                     "response_ok": isinstance(response, dict) and response.get("ok") is True})
                                report = {"schema": 1, "diagnostic_only": True, "full_passed": False,
                                          "keyless": expected_ready is False,
                                          "stop_returncode": stop_returncode,
                                          "foreground_returncode": child.poll(),
                                          "log_characters": len(text),
                                          "error_markers": [marker for marker in markers if marker in text],
                                          "startup_journals": journals}
                                if args.keyless_foreground_diagnostic:
                                    args.keyless_foreground_diagnostic.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
                                print("isolated keyless foreground evidence", json.dumps(report), file=sys.stderr, flush=True)
                        except Exception:
                            if primary_error is None:
                                raise
                            primary_error.add_note("keyless_foreground_evidence_failed")
            return health

        foreground(False)
        if args.keyless_foreground_diagnostic:
            print("PASS: keyless foreground diagnostic only; full_passed=false")
            return
        print("PASS: fresh default-off status, client-only denial, keyless locked foreground and owned stop")
        env["RSRS_RUNTIME_BIN"] = str(copy_managed_installation(managed_fixture / "bin", root))
        seed_local_credentials(binary, env, keys, root)
        health = foreground(True)
        print("PASS: CLI-created unbound credentials, real host unlock, foreground store readiness and owned stop")
        # A fresh off-mode installation must restart without opening/creating a
        # library. Its detached child is always stopped before leaving the test.
        off_root = base / "off installation"
        off_root.mkdir()
        (off_root / "client.json").write_text(json.dumps({"service_mode":"off"}))
        off_env = dict(env, RSRS_DATA_DIR=str(off_root), RSRS_BIN_DIR=str(base/"off-bin"))
        stage_runtime(base / "off-bin")
        shutil.copy2(binary, base / "off-bin" / ("rsrs.exe" if os.name == "nt" else "rsrs"))
        off_env["RSRS_RUNTIME_BIN"] = str(base / "off-bin" / runtime_name)
        off_env["RSRS_RUNTIME_BIN"] = str(copy_managed_installation(managed_fixture / "bin", off_root))
        try:
            for argv in [("runtime","start"),("runtime","stop")]*3+[("runtime","restart")]:
                result = subprocess.run([binary,"--json",*argv],env=off_env,capture_output=True,text=True,timeout=45)
                assert result.returncode == 0, f"off mode {argv}: {result.stdout} {result.stderr}"
                assert not (off_root/"rsrs.db").exists() and not (off_root/"onememory.db").exists(), "off restart created a database"
            if os.name == "nt":
                # Reproduce the real upgrade failure without an account/model:
                # suspend only this fixture's verified unmanaged native owner.
                import ctypes
                from ctypes import wintypes
                with backend_health(port) as response:
                    frozen=json.load(response)
                assert Path(frozen["data_dir"]).resolve()==off_root.resolve()
                kernel=ctypes.WinDLL("kernel32",use_last_error=True)
                kernel.OpenProcess.argtypes=[wintypes.DWORD,wintypes.BOOL,wintypes.DWORD];kernel.OpenProcess.restype=wintypes.HANDLE
                kernel.GetProcessId.argtypes=[wintypes.HANDLE];kernel.GetProcessId.restype=wintypes.DWORD
                kernel.QueryFullProcessImageNameW.argtypes=[wintypes.HANDLE,wintypes.DWORD,wintypes.LPWSTR,ctypes.POINTER(wintypes.DWORD)];kernel.QueryFullProcessImageNameW.restype=wintypes.BOOL
                kernel.WaitForSingleObject.argtypes=[wintypes.HANDLE,wintypes.DWORD];kernel.WaitForSingleObject.restype=wintypes.DWORD
                kernel.CloseHandle.argtypes=[wintypes.HANDLE];kernel.CloseHandle.restype=wintypes.BOOL
                ntdll=ctypes.WinDLL("ntdll")
                ntdll.NtSuspendProcess.argtypes=[wintypes.HANDLE];ntdll.NtSuspendProcess.restype=wintypes.LONG
                ntdll.NtResumeProcess.argtypes=[wintypes.HANDLE];ntdll.NtResumeProcess.restype=wintypes.LONG
                owned=kernel.OpenProcess(0x101800,False,frozen["pid"]) # synchronize/query/suspend, never an arbitrary PID
                assert owned and kernel.GetProcessId(owned)==frozen["pid"]
                suspended=False
                try:
                    image=ctypes.create_unicode_buffer(32768);length=wintypes.DWORD(len(image))
                    assert kernel.QueryFullProcessImageNameW(owned,0,image,ctypes.byref(length))
                    assert os.path.samefile(image.value,off_env["RSRS_RUNTIME_BIN"])
                    assert kernel.WaitForSingleObject(owned,0)==258
                    endpoint=off_root/"runtime/endpoint.json"
                    endpoint_before=endpoint.read_bytes()
                    assert ntdll.NtSuspendProcess(owned)>=0
                    suspended=True
                    rejected=subprocess.run([binary,"--json","runtime","restart"],env=off_env,capture_output=True,text=True,timeout=45)
                    assert rejected.returncode!=0, "restart accepted an owner without health/drain proof"
                    assert "owner health is unavailable" in " ".join(json.loads(rejected.stdout)["errors"]), rejected.stdout
                    assert kernel.WaitForSingleObject(owned,0)==258, "restart terminated the suspended owner"
                    assert endpoint.read_bytes()==endpoint_before, "rejected restart changed the endpoint owner"
                    assert not (off_root/"runtime/autostart/registration.json").exists()
                    assert ntdll.NtResumeProcess(owned)>=0, "owned fixture could not be resumed"
                    suspended=False
                    with backend_health(port) as response:
                        resumed=json.load(response)
                    for key in ("pid", "instance_id", "process_start_id", "image_sha256", "account_id", "generation"):
                        assert resumed[key]==frozen[key], f"suspended owner was replaced: {key}"
                    recovered=subprocess.run([binary,"--json","runtime","restart"],env=off_env,capture_output=True,text=True,timeout=45)
                    assert recovered.returncode==0, f"resumed owner restart: {recovered.stdout} {recovered.stderr}"
                    envelope=json.loads(recovered.stdout)
                    assert envelope["command"]=="runtime.restart" and envelope["status"]=="ok", envelope
                    summary=envelope["summary"]
                    assert summary["forced"] is False and summary["stopped_pid"]==frozen["pid"] and summary["pid"]!=frozen["pid"]
                    assert kernel.WaitForSingleObject(owned,15000)==0, "the inspected process survived normal drain"
                    assert not (off_root/"rsrs.db").exists() and not (off_root/"onememory.db").exists()
                    print("PASS: suspended Windows owner refused without replacement; resumed owner normally drained/restarted, no WMI or database")
                finally:
                    if suspended and kernel.WaitForSingleObject(owned,0)==258:
                        assert ntdll.NtResumeProcess(owned)>=0, "owned failed fixture could not be resumed"
                    kernel.CloseHandle(owned)
        finally:
            primary_error=sys.exc_info()[1]
            try:
                result = subprocess.run([binary,"--json","runtime","stop"],env=off_env,capture_output=True,text=True,timeout=45)
                assert result.returncode == 0, f"off-mode child cleanup failed: {result.stdout} {result.stderr}"
            except Exception as cleanup_error:
                if primary_error is None:
                    raise
                print(f"off-mode cleanup also failed: {cleanup_error}",file=sys.stderr)
        print("PASS: fresh memory-off start/stop/restart without a database")
        if args.register:
            if os.name == "posix" and os.uname().sysname == "Linux":
                rejected_root=base/"rejected-native-path"
                rejected=subprocess.run([binary,"--json","runtime","autostart","enable"],
                    env=dict(env,RSRS_DATA_DIR=str(rejected_root),RSRS_BIN_DIR=str(base/'unsupported \" backslash\\')),
                    capture_output=True,text=True,timeout=45)
                assert rejected.returncode != 0 and not rejected_root.exists(), "unsupported executable path created state"
                print("PASS: unsupported systemd executable path refused before any lifecycle state")
                # Keep the hostile-path negative case above, then exercise the
                # real systemd lifecycle using a supported installation path.
                root = base / "registered data spaces % and $"
                assert not root.exists(), "registration fixture must start fresh"
                env["RSRS_DATA_DIR"] = str(root)

            verify_registration(args, base, binary, bundle_helpers, command, copy_managed_installation,
                                env, health, home, keys, managed_fixture, off_env, off_root, port, root,
                                runtime_name, stage_runtime, backend_health, seed_local_credentials,
                                read_restart_stages, kernel if os.name == "nt" else None,
                                ctypes if os.name == "nt" else None)

    except BaseException:
        print(f"Failed lifecycle fixture retained for explicit recovery: {temporary}",file=sys.stderr)
        raise
    finally:
        primary_error = sys.exc_info()[1]
        if keys is not None:
            try:
                cleanup = keys.cleanup()
                if cleanup.get("passed") is not True or cleanup.get("remaining_entries"):
                    diagnostic = {
                        "remaining_count": len(cleanup.get("remaining_entries", [])),
                        "provider_daemon": cleanup.get("provider_daemon"),
                    }
                    print("isolated credential cleanup diagnostic", json.dumps(diagnostic), file=sys.stderr)
                    raise RuntimeError("synthetic native credential cleanup failed; fixture retained")
            except Exception as cleanup_error:
                if primary_error is None:
                    raise
                primary_error.add_note("synthetic credential cleanup also failed; fixture retained")
                raise primary_error from cleanup_error
        if primary_error is None:
            shutil.rmtree(temporary)


if __name__ == "__main__":
    main()
