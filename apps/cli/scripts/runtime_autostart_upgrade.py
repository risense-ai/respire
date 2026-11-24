"""Verify the hosted transition from the pinned historical native owner."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import urllib.request


def verify_previous_runtime(args, base, binary, bundle_helpers, copy_managed_installation,
                            env, health, managed_fixture, owned_windows_pair, port,
                            require_pair_exited, runtime_name, stage_runtime):
    import hashlib
    previous=args.previous_binary.resolve(strict=True)
    version=subprocess.run([str(previous),"--version"],env=env,capture_output=True,text=True,timeout=15)
    assert version.returncode == 0 and "1.0.17-dev.6" in version.stdout, "unexpected previous native version"
    upgrade_root=base/"previous-version-upgrade"
    upgrade_env=dict(env,RSRS_DATA_DIR=str(upgrade_root),RSRS_BIN_DIR=str(upgrade_root/"bin"))
    stage_runtime(upgrade_root / "bin")
    upgrade_env["RSRS_RUNTIME_BIN"] = str(upgrade_root / "bin" / runtime_name)
    def upgrade_call(executable,*argv):
        call_env = dict(upgrade_env)
        if executable == previous:
            call_env.pop("RSRS_RUNTIME_BIN", None)
        return subprocess.run([str(executable),"--json",*argv],env=call_env,capture_output=True,text=True,timeout=45)
    def require_upgrade(*argv):
        result=upgrade_call(binary,*argv)
        assert result.returncode == 0, f"upgrade {argv}: {result.stdout} {result.stderr}"
        return json.loads(result.stdout)["summary"]
    stable=upgrade_root/"bin"/("rsrs.exe" if os.name=="nt" else "rsrs")
    legacy_running=False
    old_health=None
    def stop_legacy_owner():
        nonlocal legacy_running
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/api/health",timeout=5) as response:
            current=json.load(response)
        assert current["pid"]==old_health["pid"] and current["bin"]=="1.0.17-dev.6"
        assert Path(current["data_dir"]).resolve()==upgrade_root.resolve()
        assert os.path.samefile(current["exe"],stable)
        assert hashlib.sha256(stable.read_bytes()).digest()==hashlib.sha256(previous.read_bytes()).digest()
        endpoint=upgrade_root/"runtime/endpoint.json"
        assert not endpoint.is_symlink() and endpoint.stat().st_size<=65536
        endpoint_before=endpoint.read_bytes()
        recorded=json.loads(endpoint_before)
        assert recorded["pid"]==current["pid"] and recorded["bin"]==current["bin"]
        assert recorded["url"]==f"http://127.0.0.1:{port}"
        stopped=upgrade_call(previous,"--runtime-internal","--stop")
        assert stopped.returncode==0, f"legacy owner stop failed: {stopped.stdout} {stopped.stderr}"
        # DEV.6 waits for native exit/lock release but leaves its endpoint.
        assert not endpoint.is_symlink() and endpoint.read_bytes()==endpoint_before
        endpoint.unlink()
        legacy_running=False
    try:
        old=upgrade_call(previous,"status")
        assert old.returncode in (0,2), f"previous native startup failed: {old.stdout} {old.stderr}"
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/api/health",timeout=5) as response:
            old_health=json.load(response)
        assert old_health["bin"] == "1.0.17-dev.6"
        assert Path(old_health["data_dir"]).resolve()==upgrade_root.resolve()
        assert os.path.samefile(old_health["exe"],stable)
        assert hashlib.sha256(stable.read_bytes()).digest()==hashlib.sha256(previous.read_bytes()).digest()
        legacy_running=True
        # An unbound old installation is rejected before any
        # migration. Deeper owned-identity/fence rejection is
        # tested separately below with a genuinely managed fixture.
        endpoint=upgrade_root/"runtime/endpoint.json"
        endpoint_before=endpoint.read_bytes()
        denied=upgrade_call(binary,"runtime","autostart","enable")
        assert denied.returncode!=0, "legacy owner was adopted without registration identity"
        assert "legacy_runtime_migration_required" in " ".join(json.loads(denied.stdout)["errors"]), denied.stdout
        assert endpoint.read_bytes()==endpoint_before
        assert bundle_helpers["digest"](stable)==bundle_helpers["digest"](previous)
        assert not (upgrade_root/"runtime.json").exists()
        assert not (upgrade_root/"bin/runtime.json").exists()
        assert not (upgrade_root/"runtime/autostart/registration.json").exists()
        assert not (upgrade_root/"runtime/autostart/maintenance.json").exists()
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/api/health",timeout=5) as response:
            preserved=json.load(response)
        assert preserved["pid"]==old_health["pid"] and preserved["bin"]==old_health["bin"]
        stop_legacy_owner()
        require_upgrade("runtime","autostart","remove","--stop")
        # Explicit test-harness transition AFTER the verified
        # legacy stop. This is not automatic migration acceptance.
        upgrade_env["RSRS_RUNTIME_BIN"] = str(copy_managed_installation(managed_fixture / "bin", upgrade_root))
        shutil.copy2(binary,stable)
        assert bundle_helpers["digest"](stable)==bundle_helpers["digest"](Path(binary))
        upgraded=require_upgrade("runtime","autostart","enable","--now")
        assert upgraded["enabled"] and upgraded["runtime"]["running"]
        assert upgraded["runtime"]["version"] == health["bin"] and upgraded["runtime"]["pid"] != old_health["pid"]
        fingerprint=hashlib.sha256(stable.read_bytes()).digest()
        legacy=upgrade_call(previous,"status")
        # A compatible old client may remain read-only; it must
        # never replace or restart the live supervised owner.
        still=require_upgrade("runtime","status")
        assert still["runtime"]["pid"] == upgraded["runtime"]["pid"] and hashlib.sha256(stable.read_bytes()).digest() == fingerprint
        if os.name == "nt":
            with owned_windows_pair(upgrade_root) as pair:
                require_upgrade("runtime","autostart","disable","--stop")
                require_pair_exited(pair)
        else:
            require_upgrade("runtime","autostart","disable","--stop")
        # Restore only known public previous-version native
        # files while stopped. Do not run old code against the
        # now-upgraded database; the next start is the new CLI.
        for source in previous.parent.rglob("*"):
            assert not source.is_symlink(), "previous native package has a symlink"
            destination=upgrade_root/"bin"/source.relative_to(previous.parent)
            if source.is_dir():
                destination.mkdir(parents=True,exist_ok=True)
            else:
                assert source.is_file() and not destination.is_symlink()
                destination.parent.mkdir(parents=True,exist_ok=True)
                shutil.copy2(source,destination)
        # A verified Runtime receipt does not authorize replacing
        # an unknown old controller, even while disabled/stopped.
        registration=upgrade_root/"runtime/autostart/registration.json"
        disabled_before=registration.read_bytes()
        refused=upgrade_call(binary,"runtime","start")
        assert refused.returncode!=0
        assert "legacy_runtime_migration_required" in " ".join(json.loads(refused.stdout)["errors"]), refused.stdout
        assert bundle_helpers["digest"](stable)==bundle_helpers["digest"](previous)
        assert registration.read_bytes()==disabled_before
        assert not (upgrade_root/"runtime/endpoint.json").exists()
        # Deliberately restore the known controller in the stopped
        # test fixture; managed Runtime sidecars were never changed.
        shutil.copy2(binary,stable)
        assert bundle_helpers["digest"](stable)==bundle_helpers["digest"](Path(binary))
        require_upgrade("runtime","start")
        disabled_upgrade=require_upgrade("runtime","status")
        assert disabled_upgrade["enabled"] is False and disabled_upgrade["runtime"]["version"] == health["bin"]
        require_upgrade("runtime","stop")
        print("PASS: DEV.6 migration deferred without replacement; explicitly stopped/trusted fixture transition, live old-client preservation and disabled-state checks (not automatic migration)")
    finally:
        primary_error=sys.exc_info()[1]
        try:
            if legacy_running:
                stop_legacy_owner()
            stopped=upgrade_call(binary,"runtime","stop")
            removed=upgrade_call(binary,"runtime","autostart","remove","--stop")
            assert stopped.returncode == 0 and removed.returncode == 0, f"previous-version fixture cleanup failed: stop={stopped.stdout} {stopped.stderr}; remove={removed.stdout} {removed.stderr}"
        except Exception as cleanup_error:
            if primary_error is None:
                raise
            print(f"previous-version cleanup also failed: {cleanup_error}",file=sys.stderr)
