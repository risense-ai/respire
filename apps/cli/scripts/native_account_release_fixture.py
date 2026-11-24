"""Prepare and verify the native account candidate release and owned stop probe."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import tarfile
import tomllib
from native_account_cases import TARGET
from native_account_cases import STABLE_ASSETS


MODEL_HASHES = {
    "tokenizer.json": "6710678b12670bc442b99edc952c4d996ae309a7020c1fa0096dd245c2faf790",
    "onnx/model_quantized.onnx": "0826f8c1ab9edf1801db86c61919d4d108e8bfc0b809ec823ad366882ff0b77d",
    "onnx/model_quantized.cpu-v1.ort": "7ff152e912bc234f5c90e4012f99897a7b2ce180f016db2321304057756b64ff",
}


def require(value, code):
    if not value:
        raise RuntimeError(code)


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def verify_source(source, sha):
    require(re.fullmatch(r"[a-f0-9]{40}", sha), "exact_source_sha_required")
    head = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
    require(head == sha, "source_head_mismatch")
    subprocess.run(["git", "-C", str(source), "diff", "--exit-code", "HEAD", "--"],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def source_version(source, package):
    version = tomllib.loads((source / package / "Cargo.toml").read_text())["package"]["version"]
    require(re.fullmatch(r"\d+\.\d+\.\d+-dev\.\d+", version), "candidate_dev_version_required")
    return version


def official_cli_version(metadata_path, cli, source_sha, env):
    # Official publication applies an ephemeral version without editing the tagged source.
    require(source_sha == "425f397db2a64347d3d27fa93db630fad6b9b520",
            "official_cli_source_mismatch")
    metadata_path = Path(metadata_path)
    require(metadata_path.name == "cli-build-x86_64-unknown-linux-gnu.json"
            and not metadata_path.is_symlink() and metadata_path.is_file()
            and metadata_path.stat().st_size == 427
            and digest(metadata_path) == "243a679da8b9436b7a6e5e09a5e4f113ad09e4d64e9166f50e6a10105c45619b",
            "official_cli_metadata_digest_mismatch")
    metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
    expected = {"schema_version": 2, "artifact_layout": "cli-frontend-v2",
                "git_sha": source_sha, "version": "1.0.21-dev.6", "target": TARGET,
                "binary_file": "rsrs-x86_64-unknown-linux-gnu", "binary_size": 14133456,
                "binary_sha256": "f82eef533b906dd5ed5e96ebe17321571084ff138c29acdc92f55ebb8a8ecf2e"}
    require(all(metadata.get(key) == value for key, value in expected.items()),
            "official_cli_metadata_identity_mismatch")
    require(cli.stat().st_size == metadata["binary_size"]
            and digest(cli) == metadata["binary_sha256"], "official_cli_binary_mismatch")
    version = metadata["version"]
    actual = subprocess.check_output([str(cli), "--version"], env=env,
                                     stderr=subprocess.PIPE, timeout=30)
    require(version.encode() in actual.split(), "official_cli_version_mismatch")
    return version


def stable_business_identity(cli, metadata_path, archive, env=None, *, full_contracts=False):
    # Fixed schema-1 release only. It never replaces the candidate's manager CLI.
    paths = (Path(cli), Path(metadata_path), Path(archive))
    require(len({path.parent.resolve() for path in paths}) == 1,
            "stable_business_assets_must_be_adjacent")
    for path, name in zip(paths, STABLE_ASSETS):
        size, expected_hash = STABLE_ASSETS[name]
        require(path.name == name and not path.is_symlink() and path.is_file()
                and path.stat().st_size == size and digest(path) == expected_hash,
                "stable_business_asset_mismatch")
    cli, metadata_path, archive = paths
    metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
    expected = {"schema_version": 1, "git_sha": "26ead1ecf5c1a41e6d5e518aeb375e896f6c4c9e",
                "version": "1.0.20", "target": TARGET, "binary_file": cli.name,
                "runtime_file": archive.name, "binary_sha256": STABLE_ASSETS[cli.name][1],
                "runtime_sha256": STABLE_ASSETS[archive.name][1]}
    require(metadata == expected, "stable_business_metadata_identity_mismatch")
    with tarfile.open(archive, "r:gz") as bundle:
        names = set()
        for member in bundle.getmembers():
            name = member.name.rstrip("/")
            require(name and not name.startswith("/")
                    and not any(part in ("", ".", "..") for part in name.split("/"))
                    and not any(character in name for character in "\\:\0")
                    and (member.isfile() or member.isdir())
                    and (name == "core-runtime.json" or name == "core-notices"
                         or name.startswith("core-notices/"))
                    and name not in names, "stable_business_archive_member_rejected")
            names.add(name)
        require("core-runtime.json" in names, "stable_business_core_metadata_missing")
        core_stream = bundle.extractfile("core-runtime.json")
        require(core_stream is not None, "stable_business_core_metadata_missing")
        with core_stream:
            core_bytes = core_stream.read()
            core = json.loads(core_bytes)
        if full_contracts:
            require(safe_file(cli.parent, "core-runtime.json").read_bytes() == core_bytes,
                    "stable_staged_core_metadata_changed")
            require(isinstance(core.get("files"), list), "stable_staged_core_inventory_missing")
            for row in core["files"]:
                require(isinstance(row, dict) and isinstance(row.get("path"), str)
                        and row["path"].startswith("core-notices/")
                        and re.fullmatch(r"[0-9a-f]{64}", row.get("sha256", "")),
                        "stable_staged_notice_identity_invalid")
                require(digest(safe_file(cli.parent, row["path"])) == row["sha256"],
                        "stable_staged_notice_changed")
        require(core.get("schema_version") == 1 and core.get("target") == TARGET,
                "stable_business_core_identity_mismatch")
    if env is not None:
        actual = subprocess.check_output([str(cli), "--version"], env=env,
                                         stderr=subprocess.PIPE, timeout=30)
        require(b"1.0.20" in actual.split(), "stable_business_version_mismatch")
    return {"version": "1.0.20", "source_sha": expected["git_sha"], "target": TARGET,
            "binary_sha256": digest(cli), "metadata_sha256": digest(metadata_path),
            "runtime_archive_sha256": digest(archive),
            "scope": ("fixed_existing_stable_contracts_in_full31_not_all_product_features" if full_contracts
                      else "ten_remote_diary_cases_not_all_stable_features"),
            "concurrent_http_checks": "candidate_runtime_only"}


def safe_file(root, relative):
    require(isinstance(relative, str) and relative and not relative.startswith("/")
            and not any(part in ("", ".", "..") for part in relative.split("/"))
            and not any(character in relative for character in "\\:\0"), "unsafe_payload_path")
    path = root
    for part in relative.split("/"):
        path = path / part
        require(not path.is_symlink(), "linked_payload_rejected")
    require(path.is_file(), "payload_file_missing")
    return path


def package_runtime(runtime, staged, destination, version, runtime_sha, cli_sha, cli_hash, cli_version):
    destination.mkdir(mode=0o700)
    payload = destination / "payload"
    payload.mkdir(mode=0o700)
    binary_name = f"rsrs-runtime-{TARGET}"
    core_path = safe_file(staged, "core-runtime.json")
    core = json.loads(core_path.read_text())
    require(core.get("schema_version") == 1 and core.get("target") == TARGET,
            "core_target_mismatch")
    shutil.copy2(runtime, payload / binary_name)
    shutil.copy2(core_path, payload / "core-runtime.json")
    paths = {binary_name, "core-runtime.json"}
    for entry in core["files"]:
        relative = entry["path"]
        require(relative not in paths, "duplicate_payload_path")
        source = safe_file(staged, relative)
        require(source.stat().st_size == entry["size"] and digest(source) == entry["sha256"],
                "staged_core_digest_mismatch")
        paths.add(relative)
        target = payload / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
    inventory = [{"path": name, "size": (payload / name).stat().st_size,
                  "sha256": digest(payload / name)} for name in sorted(paths)]
    archive = destination / f"rsrs-runtime-{TARGET}.tar.gz"
    with tarfile.open(archive, "w:gz", format=tarfile.USTAR_FORMAT) as stream:
        for name in sorted(paths):
            stream.add(payload / name, arcname=name, recursive=False)
    metadata = {
        "schema_version": 2, "runtime_version": version, "tag": "v" + version,
        "channel": "dev", "target_triple": TARGET, "archive_file": archive.name,
        "archive_sha256": digest(archive), "binary_file": binary_name,
        "binary_sha256": digest(runtime), "source_commit": runtime_sha,
        "compat": {"min_cli": cli_version, "max_cli": "*"},
        "protocol_range": {"minimum": 1, "maximum": 1},
        "capabilities": {"business_executor": "1", "mcp_server": "1"},
        "files": inventory,
        "candidate": {"kind": "unpublished-source-fixture", "cli_source_sha": cli_sha,
                      "cli_binary_sha256": cli_hash},
    }
    metadata_path = destination / f"runtime-build-{TARGET}.json"
    write_json(metadata_path, metadata)
    return archive, metadata_path


def frozen_runtime_package(args, runtime, version):
    # Keep exact compressed candidate bytes. The existing installer/receipt
    # checks validate archive members and every installed payload byte.
    archive = args.runtime_archive.resolve(strict=True)
    metadata_path = args.runtime_metadata.resolve(strict=True)
    require(archive.is_file() and metadata_path.is_file()
            and not args.runtime_archive.is_symlink() and not args.runtime_metadata.is_symlink(),
            "frozen_runtime_files_required")
    metadata = json.loads(metadata_path.read_text())
    require(metadata.get("schema_version") == 2 and metadata.get("target_triple") == TARGET
            and metadata.get("runtime_version") == version
            and metadata.get("source_commit") == args.runtime_sha
            and metadata.get("archive_file") == archive.name
            and metadata.get("archive_sha256") == digest(archive)
            and metadata.get("binary_sha256") == digest(runtime)
            and metadata.get("candidate") == {
                "kind": "unpublished-source-fixture", "cli_source_sha": args.cli_sha,
                "cli_binary_sha256": digest(args.cli.resolve(strict=True))},
            "frozen_runtime_identity_mismatch")
    return archive, metadata_path


def private_env(root):
    env = {key: os.environ[key] for key in ("PATH", "GITHUB_ACTIONS", "RUNNER_ENVIRONMENT",
           "RUNNER_TEMP") if key in os.environ}
    env.update(LANG="C.UTF-8", HOME=str(root / "home"), USERPROFILE=str(root / "home"),
        XDG_DATA_HOME=str(root / "data"), XDG_CONFIG_HOME=str(root / "config"),
        XDG_CACHE_HOME=str(root / "cache"), XDG_RUNTIME_DIR=str(root / "runtime"),
        TMPDIR=str(root / "tmp"), RSRS_DATA_DIR=str(root / "installation"),
        RSRS_M3_DIR=str(root / "model"), RSRS_ENGINE="cpu", RSRS_NO_AUTOSYNC="1",
        RSRS_UPDATE_CHECK="0", RUST_MIN_STACK="16777216", RUST_TEST_THREADS="1")
    return env


def verify_receipt(root, metadata_path):
    metadata = json.loads(metadata_path.read_text())
    selected = json.loads((root / "bin/runtime.json").read_text())
    runtime = root / "bin/runtime" / metadata["runtime_version"] / "rsrs-runtime"
    require(selected.get("version") == metadata["runtime_version"]
            and selected.get("sha256") == metadata["binary_sha256"]
            and selected.get("release", {}).get("metadata_json") == metadata_path.read_text()
            and selected["release"]["metadata_sha256"] == digest(metadata_path)
            and digest(runtime) == metadata["binary_sha256"], "offline_receipt_mismatch")
    for entry in metadata["files"]:
        name = "rsrs-runtime" if entry["path"] == metadata["binary_file"] else entry["path"]
        if (name.startswith("core-notices/") and not any(part in ("", ".", "..") for part in name.split("/"))
                and not any(char in name for char in "\\:\0")
                and not (runtime.parent / name).exists() and not (runtime.parent / name).is_symlink()):
            continue
        path = safe_file(runtime.parent, name)
        require(path.stat().st_size == entry["size"] and digest(path) == entry["sha256"],
                "installed_inventory_mismatch")
    return runtime


def cleanup_processes(root, hashes, observations=None):
    # Detached Runtime children do not necessarily remain in the test process group.
    # A unique inherited TMPDIR plus executable bytes binds only this run's children.
    marker = f"TMPDIR={root / 'tmp'}".encode()
    cleaned = 0
    for proc in Path("/proc").iterdir():
        if not proc.name.isdecimal():
            continue
        fd = None
        try:
            fd = os.pidfd_open(int(proc.name))
            if marker not in (proc / "environ").read_bytes().split(b"\0"):
                continue
            image_sha256 = digest(proc / "exe")
            if image_sha256 not in hashes:
                continue
            signal.pidfd_send_signal(fd, signal.SIGKILL)
            cleaned += 1
            if observations is not None:
                # Only existing run-bound executable hashes and a numeric PID.
                # Never retain environ, command lines, paths or exception text.
                observations.append({"pid": int(proc.name), "image_sha256": image_sha256})
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
        finally:
            if fd is not None:
                os.close(fd)
    return cleaned


def standalone_stop_checks(captured, runtime_sha256):
    def records(marker):
        return [json.loads(line.split(marker, 1)[1]) for line in captured.splitlines() if marker in line]
    stops = records(b"NATIVE_STOP_RAW ")
    writes = records(b"NATIVE_ACCEPTED_WRITE_RAW ")
    require(len(stops) == 2 and all(type(row.get("forced")) is bool for row in stops)
            and {row["forced"] for row in stops} == {False, True} and len(writes) == 1,
            "standalone_stop_case_inventory_mismatch")
    observations = []
    for row in [*stops, *writes]:
        forced = row.get("forced") is True
        write = row is writes[0]
        health, receipt = row["runtime_identity"], row["final_receipt"]
        require(row.get("schema") == 1 and row.get("evidence_class") == "native-raw"
                and row.get("gate") == "not-evaluated" and row.get("startup_mode") == "standalone"
                and health["image_sha256"] == runtime_sha256
                and receipt["image_sha256"] == runtime_sha256
                and all(receipt[key] == health[key] for key in
                        ("installation_id", "instance_id", "account_id", "generation"))
                and receipt["operation_id"] == row["accepted"]["operation_id"]
                and row["accepted"]["state"] == "accepted"
                and row["accepted"].get("deadline_ms") is None
                and row["accepted"].get("timeout_state") is None,
                "standalone_stop_identity_mismatch")
        before, after = row["business_before_stop_ms"], row["business_held_after_stop_ms"]
        require(type(before) is int and before > 4000 and type(after) is int and after > 0
                and (forced or after > 4000) and row["provider_requests"] == 1
                and row["same_key_result"]["code"] == "request_in_progress",
                "standalone_stop_held_business_not_proven")
        require(receipt["state"] == ("forced_timeout" if forced else "drained"),
                "standalone_stop_final_state_mismatch")
        if forced:
            require(row.get("passed") is True and row["provider_http_status"] == 401
                    and any(item["operation_id"] == row["business_result"]["operation_id"]
                            and item["result"] == "unknown" for item in receipt["unfinished"])
                    and row["held_health"]["inference"]["retired_worker"]["worker_reaped"] is True
                    and row["force_requests"]
                    and all(item["timeout_state"] == "forced_timeout"
                            and item["operation_id"] == receipt["operation_id"]
                            for item in row["force_requests"]),
                    "standalone_force_outcome_not_proven")
        else:
            require(receipt["unfinished"] == [], "standalone_normal_stop_has_unfinished_work")
            if write:
                require(row["provider_http_status"] == 200 and row["encrypted_write_committed"] is True
                        and row["plaintext_parent_matches_encrypted_payload"] is True,
                        "standalone_accepted_write_not_committed")
            else:
                require(row.get("passed") is True and row["provider_http_status"] == 401
                        and row["force_requests"] == [], "standalone_normal_stop_was_forced")
        if not write:
            require(row["stale_scope_result"]["code"] == "scope_mismatch"
                    and row["restarted_receipt"]["state"] == "drained",
                    "standalone_stop_restart_not_verified")
        observations.append({"case": "accepted-write" if write else "explicit-force" if forced else "normal",
            "startup_mode": "standalone", "business_before_stop_ms": before,
            "business_held_after_stop_ms": after, "final_state": receipt["state"],
            "provider_requests": row["provider_requests"], "encrypted_write_committed": write,
            "forced_operation_remains_unknown": forced})
    return {"passed": True, "observations": observations}


CLIENT2_OWNED_SOURCE_SHA = "6ffa187685734873721ab51f3a54d8b2c29cd269"
OWNED_PROBE_FILES = ("diagnostics_darwin.go", "diagnostics_other.go", "host_operations.go", "main.go")


def validate_owned_stop_options(args):
    frozen = args.suite == "g1-owned-stop"
    if args.owned_stop_mode is None:
        require(not frozen and args.client2_source is None, "owned_stop_source_requires_mode")
        return
    require(args.owned_stop_mode in ("registered", "client2-owned"), "unknown_owned_stop_mode")
    require(args.suite in ("diagnostic-native-stop", "g1-owned-stop") and args.client2_source is not None
            and not args.official_cli_metadata and not args.legacy_stable_contracts
            and not any((args.stable_business_cli, args.stable_business_cli_metadata,
                         args.stable_business_runtime_archive))
            and not args.require_native_stop, "owned_stop_requires_current_candidate_diagnostic")
    require(bool(args.runtime_archive) == frozen and bool(args.runtime_metadata) == frozen,
            "owned_stop_frozen_input_mismatch")
    require(re.fullmatch(r"[a-f0-9]{40}", args.cli_sha)
            and re.fullmatch(r"[a-f0-9]{40}", args.runtime_sha), "owned_stop_workflow_source_mismatch")
    if frozen:
        require(os.environ.get("GITHUB_REPOSITORY") == "risense-ai/respire-runtime"
                and args.runtime_sha == os.environ.get("GITHUB_SHA"), "owned_stop_workflow_source_mismatch")
    else:
        require(args.cli_sha == os.environ.get("GITHUB_SHA"), "owned_stop_workflow_source_mismatch")


def build_owned_stop_probe(args, root, output, *, cache_root=None):
    # Never accept an external executable: derive the probe from both exact Git sources.
    validate_owned_stop_options(args)
    source = args.client2_source.resolve(strict=True)
    verify_source(source, CLIENT2_OWNED_SOURCE_SHA)
    stage = root / "owned-stop-probe"
    stage.mkdir(mode=0o700)
    archive = stage / "client2-source.tar"
    with archive.open("wb") as stream:
        subprocess.run(["git", "-C", str(source), "archive", "--format=tar", CLIENT2_OWNED_SOURCE_SHA],
                       stdout=stream, stderr=output, check=True, timeout=60)
    extracted = stage / "client2"
    extracted.mkdir()
    with tarfile.open(archive) as stream:
        members = stream.getmembers()
        require(len(members) < 10000 and sum(row.size for row in members) < 256 * 1024 * 1024
                and all((row.isfile() or row.isdir()) and not Path(row.name).is_absolute()
                        and ".." not in Path(row.name).parts and "\\" not in row.name
                        for row in members), "owned_stop_source_archive_rejected")
        stream.extractall(extracted, filter="data")
    probe = stage / "probe"
    probe.mkdir()
    fixture_hashes = {}
    for name in OWNED_PROBE_FILES:
        tracked = "scripts/fixtures/client2-v0256-probe/" + name
        content = subprocess.check_output(["git", "-C", str(args.cli_source), "show", args.cli_sha + ":" + tracked])
        require(content == (args.cli_source / tracked).read_bytes(), "owned_stop_probe_fixture_changed")
        (probe / name).write_bytes(content)
        fixture_hashes[tracked] = hashlib.sha256(content).hexdigest()
    (probe / "go.mod").write_text("module respire-client2/regression\n\ngo 1.26.0\n\n"
        "require respire-client2 v0.0.0\nreplace respire-client2 => " + json.dumps(str(extracted)) + "\n")
    (probe / "go.sum").write_bytes((extracted / "go.sum").read_bytes())
    build_env = {name: os.environ[name] for name in ("PATH", "SystemRoot", "WINDIR") if name in os.environ}
    cache_root = stage if cache_root is None else cache_root
    build_env.update(GOPATH=str(cache_root / "go-path"), GOCACHE=str(cache_root / "go-cache"),
                     GOMODCACHE=str(cache_root / "go-mod"), GOTOOLCHAIN="local", GOENV="off", GOWORK="off")
    binary = probe / "probe"
    subprocess.run(["go", "build", "-buildvcs=false", "-mod=mod", "-o", str(binary), "."],
                   cwd=probe, env=build_env, stdout=output, stderr=output, check=True, timeout=600)
    require(binary.is_file() and not binary.is_symlink(), "owned_stop_probe_binary_missing")
    identity = {"client2_source_sha": CLIENT2_OWNED_SOURCE_SHA,
        "client2_source_archive_sha256": digest(archive), "probe_fixture_cli_sha": args.cli_sha,
        "probe_fixture_sha256": fixture_hashes, "generated_go_mod_sha256": digest(probe / "go.mod"),
        "source_go_sum_sha256": digest(extracted / "go.sum"), "resolved_go_sum_sha256": digest(probe / "go.sum"),
        "probe_binary_sha256": digest(binary), "mode": args.owned_stop_mode,
        "workflow_source_sha": os.environ.get("GITHUB_SHA"), "run_id": os.environ.get("GITHUB_RUN_ID"),
        "run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT")}
    return binary, identity


def owned_stop_checks(captured, mode, probe_sha256, runtime_sha256, *, cli_sha256=None):
    require(mode in {"registered", "client2-owned"}, "unknown_owned_stop_mode")
    marker = b"NATIVE_REGISTERED_STOP_RAW " if mode == "registered" else b"NATIVE_CLIENT2_STOP_RAW "
    rows = [json.loads(line.split(marker, 1)[1]) for line in captured.splitlines() if marker in line]
    require(len(rows) == 3 and {row["case"] for row in rows} == {"normal", "caller-timeout", "explicit-force"},
            "owned_stop_case_inventory_mismatch")
    expected_mode = "cli-supervised" if mode == "registered" else "client2-owned"
    for row in rows:
        require(row.get("startup_mode") == expected_mode and row.get("passed") is True
                and row.get("probe_sha256") == probe_sha256 and row.get("business_sender") == "modern-harness"
                and row.get("provider_http_status") == 200 and row.get("provider_requests") == 1
                and row.get("credential_cleanup_verified") is True
                and row.get("runtime_compatibility_timeout_claimed") is False
                and row["runtime_identity"]["image_sha256"] == runtime_sha256,
                "owned_stop_real_consumer_evidence_mismatch")
        if mode == "client2-owned":
            require(row.get("actor_lane") == "client2-cli-runtime-core" and isinstance(row.get("cli_control"), dict),
                    "owned_stop_actor_lane_mismatch")
            cli_control, runtime = row["cli_control"], row["runtime_identity"]
            require(isinstance(cli_sha256, str) and re.fullmatch(r"[0-9a-f]{64}", cli_sha256)
                    and cli_control.get("sha256") == cli_sha256
                    and all(isinstance(cli_control.get(field), str) and cli_control[field]
                            for field in ("path", "version"))
                    and type(runtime.get("pid")) is int and runtime["pid"] > 0
                    and all(isinstance(runtime.get(field), str) and runtime[field]
                            for field in ("instance_id", "process_start_id")),
                    "owned_stop_actor_binding_mismatch")
        expected_sender = ("modern-cli" if row["case"] == "normal" else "original-go-client2-managed") if mode == "registered" else "original-go-client2"
        require(row.get("stop_sender") == expected_sender, "owned_stop_sender_mismatch")
        timeout = row.get("caller_timeout_ms")
        require(timeout is None if row["case"] == "normal" else
                type(timeout) is int and 35000 <= timeout <= 60000,
                "owned_stop_caller_timeout_invalid")
        forced = row["case"] == "explicit-force"
        require(row.get("encrypted_write_committed") is (not forced)
                and row.get("forced_operation_remains_unknown") is forced,
                "owned_stop_write_outcome_mismatch")
    # Only source-defined scalar observations leave the private business log.
    fields = ("case", "startup_mode", "business_sender", "stop_sender",
        "caller_timeout_ms", "provider_http_status", "provider_requests", "encrypted_write_committed",
        "forced_operation_remains_unknown", "credential_cleanup_verified")
    if mode == "client2-owned":
        fields += ("actor_lane",)
    return [{key: row.get(key) for key in fields} for row in rows]
