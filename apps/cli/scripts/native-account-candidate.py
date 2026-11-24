#!/usr/bin/env python3
"""Run the unchanged Runtime account regression against two exact source commits.

The default retains the account case; --suite full runs every original business case.
The fixed diagnostic-failures suite isolates four known failures and is not release acceptance.
Only sanitized identity and result JSON is publishable. Product output stays in
private runner-temporary logs, including on failures. This is not release proof.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import subprocess
import sys
import tarfile
import time
import tomllib

from native_account_cases import (
    TARGET,
    CASE,
    MODULE_COUNTS,
    NATIVE_STOP_CASE,
    G1_BUSINESS_CASES,
    BASELINE_CASES,
    STABLE_DIARY_CASES,
    STABLE_ASSETS,
    DIAGNOSTIC_CASES,
)

def selected_cases(listing, suite, require_native_stop=False):
    require(not require_native_stop or suite == "full", "native_stop_requires_full_suite")
    names = [line[:-6] for line in listing.splitlines() if line.endswith(": test")]
    require(names.count(CASE) == 1, "original_account_case_missing")
    if suite == "account":
        return [CASE]
    if suite == "g1-business":
        require(all(names.count(case) == 1 for case in G1_BUSINESS_CASES),
                "g1_business_case_inventory_mismatch")
        return list(G1_BUSINESS_CASES)
    if suite == "stable-diary-remote":
        require(len(STABLE_DIARY_CASES) == 10
                and all(names.count(case) == 1 for case in STABLE_DIARY_CASES),
                "stable_diary_case_inventory_mismatch")
        return list(STABLE_DIARY_CASES)
    if suite == "diagnostic-failures":
        require(all(names.count(case) == 1 for case in DIAGNOSTIC_CASES), "diagnostic_case_inventory_mismatch")
        return list(DIAGNOSTIC_CASES)
    if suite in ("diagnostic-native-stop", "g1-owned-stop"):
        require(names.count(NATIVE_STOP_CASE) == 1, "native_stop_business_case_missing")
        return [NATIVE_STOP_CASE]
    require(suite == "full", "unknown_business_suite")
    expected = set(BASELINE_CASES)
    if NATIVE_STOP_CASE in names:
        expected.add(NATIVE_STOP_CASE)
    require(len(names) == len(set(names)) == sum(MODULE_COUNTS.values()) + int(NATIVE_STOP_CASE in expected)
            and set(names) == expected, "business_case_inventory_mismatch")
    require(not require_native_stop or NATIVE_STOP_CASE in expected, "native_stop_business_case_missing")
    return names

def stable_legacy_probe_checks(captured):
    # This marker proves a harness probe's legacy projection, not its sender
    # being the old CLI. The separately verified binary executes business calls.
    marker = b"stable-diary-legacy-probe "
    expected = {"probe_without_scope": True, "legacy_projection_verified": True,
                "same_actor_before_after": True, "candidate_process_verified": True}
    checks = []
    for line in captured.splitlines():
        if marker not in line:
            continue
        try:
            value = json.loads(line.split(marker, 1)[1])
        except (ValueError, UnicodeDecodeError):
            return None
        if (not isinstance(value, dict) or set(value) != set(expected)
                or any(value[key] is not True for key in expected) or len(checks) >= 128):
            return None
        checks.append(expected.copy())
    return checks


def compatibility_host_checks(captured):
    checks = []
    marker = b"client2-account-host "
    for line in captured.splitlines():
        if marker not in line:
            continue
        try:
            value = json.loads(line.split(marker, 1)[1])
        except (ValueError, UnicodeDecodeError):
            return None
        if (not isinstance(value, dict) or set(value) != {"stage", "passed"}
                or value["stage"] not in ("space-profile", "import-target", "restart-target")
                or type(value["passed"]) is not bool):
            return None
        checks.append(value)
    return checks


from native_account_diagnostics import safe_failure_diagnostics


from native_account_release_fixture import (
    MODEL_HASHES, CLIENT2_OWNED_SOURCE_SHA, OWNED_PROBE_FILES,
    require, digest, write_json, verify_source, source_version, official_cli_version,
    stable_business_identity, safe_file, package_runtime, frozen_runtime_package,
    private_env, verify_receipt, cleanup_processes, standalone_stop_checks,
    validate_owned_stop_options, build_owned_stop_probe, owned_stop_checks,
)


def private_command(command, env, cwd, output, timeout):
    with subprocess.Popen([str(item) for item in command], env=env, cwd=cwd,
                          stdout=output, stderr=subprocess.STDOUT, start_new_session=True) as child:
        try:
            status = child.wait(timeout=timeout)
        except BaseException:
            # Do not leave the owned D-Bus/Secret Service group after interruption.
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.wait()
            raise
        require(status == 0, "private_command_failed")


def run(args):
    require(sys.platform == "linux" and platform.machine() == "x86_64"
            and os.environ.get("GITHUB_ACTIONS") == "true"
            and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted", "native_hosted_linux_required")
    root = args.root.resolve()
    temp = Path(os.environ["RUNNER_TEMP"]).resolve(strict=True)
    require(root.is_relative_to(temp) and root != temp and not root.exists(), "fresh_owned_root_required")
    os.umask(0o077)
    root.mkdir(mode=0o700)
    diagnostic = args.suite in ("diagnostic-failures", "diagnostic-native-stop")
    stable_remote = args.suite == "stable-diary-remote"
    stable_full = args.legacy_stable_contracts
    report = {"schema_version": 1,
              "kind": "unpublished-source-diagnostic" if diagnostic else "unpublished-source-fixture",
              "case": ("native-stop-diagnostic" if args.suite == "diagnostic-native-stop" else
                       "targeted-failure-diagnostic" if diagnostic else
                       "stable-diary-remote-subset" if stable_remote else
                       "g1-owned-stop" if args.suite == "g1-owned-stop" else
                       "g1-business-subset" if args.suite == "g1-business" else
                       CASE if args.suite == "account" else "full-business-suite"), "suite": args.suite,
              "client2_host_prototype": args.compatibility_host,
              "required_native_stop": args.require_native_stop,
              "passed": False, "release_acceptance": False, "test_executed": False,
              "cli_source_sha": args.cli_sha, "runtime_source_sha": args.runtime_sha,
              "run_id": os.environ.get("GITHUB_RUN_ID"), "run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT"),
              "target": TARGET, "stage": "source_identity"}
    validate_owned_stop_options(args)
    env = private_env(root)
    for name in ("home", "data", "config", "cache", "runtime", "tmp", "installation"):
        (root / name).mkdir(mode=0o700)
    cli, runtime, harness = args.cli.resolve(), args.runtime.resolve(), args.harness.resolve()
    hashes = {digest(cli), digest(runtime), digest(harness)}
    log = root / "private.log"
    cases = []
    try:
        verify_source(args.cli_source, args.cli_sha)
        verify_source(args.runtime_source, args.runtime_sha)
        if stable_remote or stable_full:
            diary_source = args.runtime_source / "crates/commands/examples/cli_business_tests/diary_identity.rs"
            fixture = diary_source.read_bytes()
            require(all(marker in fixture for marker in (
                b"RSRS_TEST_STABLE_DIARY_SUITE", b"RSRS_TEST_STABLE_CLI_BIN",
                b"fn business_command(", b"fn verify_business_owner(", b"fn business_output(",
                b"fn verify_legacy_projection(", b"stable-diary-legacy-probe ")),
                "stable_business_fixture_support_missing")
            report["diary_source_sha256"] = digest(diary_source)
            stable_cli = args.stable_business_cli.resolve(strict=True)
            require(stable_cli not in (cli, runtime, harness), "stable_business_binary_not_separate")
            report["stable_business_cli"] = stable_business_identity(
                args.stable_business_cli, args.stable_business_cli_metadata,
                args.stable_business_runtime_archive, env, full_contracts=stable_full)
            hashes.add(report["stable_business_cli"]["binary_sha256"])
            if stable_remote:
                env.update(RSRS_TEST_STABLE_DIARY_SUITE="stable-diary-remote",
                           RSRS_TEST_STABLE_CLI_BIN=str(stable_cli))
            if stable_full:
                report["legacy_contracts"] = {"passed":False,
                    "scope":"existing_stable_contracts_in_full31_not_all_product_features",
                    "current_full31_is_separate":True,"modern_only":[NATIVE_STOP_CASE,
                        "runtime.account-result","runtime.account-rollback","scoped_http_fingerprint_matrix"],
                    "required_cases":list(BASELINE_CASES),"executed_cases":[],"missing_cases":list(BASELINE_CASES)}
        version = source_version(args.runtime_source, "runtime")
        cli_source_version = source_version(args.cli_source, "cli")
        cli_version = (official_cli_version(args.official_cli_metadata, cli, args.cli_sha, env)
                       if args.official_cli_metadata else cli_source_version)
        report.update(cli_source_version=cli_source_version,
                      cli_version_origin="official-release-metadata" if args.official_cli_metadata else "source-manifest")
        if args.official_cli_metadata:
            report["official_cli_metadata_sha256"] = digest(args.official_cli_metadata)
        pin = json.loads((args.cli_source / "test-support/runtime-test-fixture.json").read_text())["required"]
        require(version != pin, "candidate_must_not_impersonate_official_pin")
        require((args.cli_source / "sdk/core-sdk.lock.json").read_bytes()
                == (args.runtime_source / "sdk/core-sdk.lock.json").read_bytes()
                == (args.runtime_source / "crates/core-sdk/core-sdk.lock.json").read_bytes(),
                "sdk_lock_mismatch")
        report.update(runtime_version=version, cli_version=cli_version,
            cli_binary_sha256=digest(cli), runtime_binary_sha256=digest(runtime),
            harness_sha256=digest(harness), sdk_lock_sha256=digest(args.runtime_source / "sdk/core-sdk.lock.json"),
            account_source_sha256=digest(args.runtime_source / "crates/commands/examples/cli_business_tests/account.rs"))
        report["stage"] = "candidate_archive"
        if args.suite in ("g1-business", "g1-owned-stop"):
            archive, metadata_path = frozen_runtime_package(args, runtime, version)
        else:
            archive, metadata_path = package_runtime(runtime, runtime.parent, root / "archive",
                version, args.runtime_sha, args.cli_sha, digest(cli), cli_version)
        with log.open("wb") as output:
            def invoke(command, timeout=900):
                return private_command(command, env, args.cli_source, output, timeout)
            if args.owned_stop_mode:
                probe, probe_identity = build_owned_stop_probe(args, root, output)
                report["owned_stop"] = {"source_binding": probe_identity, "passed": False}
                hashes.add(probe_identity["probe_binary_sha256"])
                env.update(RSRS_TEST_NATIVE_OWNED_STOP=args.owned_stop_mode, RSRS_CLIENT2_PROBE_BIN=str(probe))
            for binary, expected in ((cli, cli_version), (runtime, version)):
                actual = subprocess.check_output([str(binary), "--version"], env=env, timeout=30)
                require(expected.encode() in actual.split(), "compiled_version_mismatch")
            report["stage"] = "offline_install"
            invoke([cli, "--json", "runtime", "install", "--file", archive, "--version", version])
            installed = verify_receipt(root / "installation", metadata_path)
            report.update(runtime_archive_sha256=digest(archive), runtime_metadata_sha256=digest(metadata_path),
                          offline_installer_verified=True)
            report["stage"] = "real_model"
            invoke([cli, "--json", "model", "install-m3"], timeout=1800)
            sys.path.insert(0, str(args.cli_source / "scripts"))
            from dev_model_fixture import share_cpu_container
            share_cpu_container(root, env, root / "model")
            for name, expected in MODEL_HASHES.items():
                require(digest(safe_file(root / "model", name)) == expected, "real_model_digest_mismatch")
            report["model_sha256"] = MODEL_HASHES
            env.update(RSRS_CLI_BIN=str(cli), RSRS_CLI_SOURCE_DIR=str(args.cli_source.resolve()),
                RSRS_CLI_SOURCE_SHA=args.cli_sha, RSRS_RUNTIME_BIN=str(installed),
                RSRS_RUNTIME_SOURCE_SHA=args.runtime_sha, RSRS_RUNTIME_CANDIDATE_METADATA=str(metadata_path),
                RSRS_CORE_TEST_MODEL_DIR=str(root / "model"), RSRS_CORE_TEST_MODE="1")
            if args.compatibility_host:
                env["RSRS_TEST_EXPECT_CLIENT2_HOST"] = "1"
            listing = subprocess.check_output([str(harness), "--list"], env=env, timeout=30, stderr=output)
            listing_text = listing.decode()
            report["harness_case_count"] = sum(line.endswith(": test") for line in listing_text.splitlines())
            report["native_stop_present"] = f"{NATIVE_STOP_CASE}: test" in listing_text.splitlines()
            cases = selected_cases(listing_text, args.suite, args.require_native_stop)
            report["required_cases"] = cases
            report["selected_case_count"] = len(cases)
            report["selected_native_stop"] = NATIVE_STOP_CASE in cases
            test_args = ([*cases, "--exact", "--test-threads=1"]
                         if args.suite in ("account", "g1-business", "g1-owned-stop", "diagnostic-failures", "diagnostic-native-stop", "stable-diary-remote") else ["--test-threads=1"])
            if args.compatibility_host or diagnostic or stable_remote or args.suite in ("g1-business", "g1-owned-stop"):
                test_args.append("--show-output")
            command = [sys.executable, "-B", args.cli_source / "scripts/dev-secret-service-keyring-selftest.py",
                "--root", root / "keyring", "--", harness, *test_args]
            if args.owned_stop_mode == "registered":
                command.insert(3, "--hosted-user-bus")
            else:
                command = ["dbus-run-session", "--", "env", "RSRS_SS_FIXTURE_BUS=private", *command]
            report["stage"] = "private_diagnostic_test" if diagnostic else "private_account_test"
            report["test_requested"] = True
            invoke(command, timeout=1000 if args.suite == "account" else 2700)
            if stable_full:
                # Separate output and private keyring prevent the second pass from
                # satisfying any original current31 result or credential assertion.
                legacy_cases = list(BASELINE_CASES)
                legacy_env = dict(env, RSRS_TEST_LEGACY_STABLE_CONTRACTS="stable-contracts-v1",
                    RSRS_TEST_STABLE_DIARY_SUITE="stable-diary-remote",
                    RSRS_TEST_STABLE_CLI_BIN=str(stable_cli))
                legacy_log = root / "legacy-private.log"
                legacy_command = ["dbus-run-session", "--", "env", "RSRS_SS_FIXTURE_BUS=private",
                    sys.executable, "-B", args.cli_source / "scripts/dev-secret-service-keyring-selftest.py",
                    "--root", root / "legacy-keyring", "--", harness, *legacy_cases,
                    "--exact", "--test-threads=1", "--show-output"]
                with legacy_log.open("wb") as private_output:
                    private_command(legacy_command, legacy_env, args.cli_source, private_output, 2700)
                legacy_keyring = json.loads((root / "legacy-keyring/business-keyring-selftest.json").read_text())
                require(legacy_keyring.get("passed") is True and legacy_keyring.get("business_passed") is True,
                        "legacy_private_business_fixture_failed")
                legacy_output = legacy_log.read_bytes()
                roles = {"diary_identity":"candidate_runtime", "memory_revision":"old_embedded",
                         "capture_main":"host_local", "account":"old_embedded",
                         "snapshot_lifecycle":"old_embedded"}
                receipts = []
                for line in legacy_output.splitlines():
                    if b"legacy-stable-contract " not in line:
                        continue
                    value = json.loads(line.split(b"legacy-stable-contract ", 1)[1])
                    require(isinstance(value, dict) and set(value) == {"case","sender","execution","passed"}
                            and value["case"] in legacy_cases and value["sender"] == "old_cli"
                            and value["execution"] == roles[value["case"].split("::")[0]]
                            and value["passed"] is True and len(receipts) < 30,
                            "legacy_contract_receipt_invalid")
                    receipts.append(value)
                executed = [value["case"] for value in receipts]
                require(len(executed) == len(set(executed)) == len(legacy_cases)
                        and set(executed) == set(legacy_cases)
                        and all(f"test {case} ... ok".encode() in legacy_output.splitlines() for case in legacy_cases),
                        "legacy_contract_case_result_missing")
                api_features = {"snapshot.create", "snapshot.list", "witness.verify", "feedback.mark", "recall.empty"}
                observed_apis = []
                for line in legacy_output.splitlines():
                    if b"legacy-stable-api " not in line:
                        continue
                    value = json.loads(line.split(b"legacy-stable-api ", 1)[1])
                    require(isinstance(value, dict) and set(value) == {"feature","sender","execution","passed"}
                            and value["feature"] in api_features and value["sender"] == "old_cli"
                            and value["execution"] == "candidate_runtime" and value["passed"] is True
                            and len(observed_apis) < 64, "legacy_remote_api_receipt_invalid")
                    observed_apis.append(value)
                require({value["feature"] for value in observed_apis} == api_features,
                        "legacy_remote_api_contract_missing")
                report["legacy_contracts"]["recovery_input_origin"] = "harness_original_schema_with_old_snapshot_bytes"
                report["legacy_contracts"]["recovery_interruption_by_old_binary"] = False
                report["legacy_contracts"]["candidate_api_receipts"] = observed_apis
                report["legacy_contracts"]["modern_substeps_not_claimed_as_old"] = [
                    "snapshot_staged_store_fault_injection", "queued_direct_rpc_resume",
                    "scoped_idempotency_matrix", "account_awaiting_input_result_and_rollback"]
                missing = [case for case in BASELINE_CASES if case not in executed]
                report["legacy_contracts"].update(executed_cases=executed, missing_cases=missing,
                    receipts=receipts, keyring=legacy_keyring, passed=not missing)
                require(not missing, "legacy_contract_case_missing")
        report["stage"] = "result_verification"
        keyring = json.loads((root / "keyring/business-keyring-selftest.json").read_text())
        report["keyring"] = keyring
        require(keyring.get("passed") is True and keyring.get("business_passed") is True,
                "private_business_fixture_failed")
        require(digest(cli) == report["cli_binary_sha256"] and digest(runtime) == report["runtime_binary_sha256"]
                and digest(harness) == report["harness_sha256"], "executed_binaries_changed")
        if stable_remote or stable_full:
            require(stable_business_identity(args.stable_business_cli, args.stable_business_cli_metadata,
                    args.stable_business_runtime_archive, full_contracts=stable_full) == report["stable_business_cli"],
                    "stable_business_assets_changed")
        captured = log.read_bytes()
        require(f"test result: ok. {len(cases)} passed; 0 failed;".encode() in captured,
                "required_business_cases_did_not_pass")
        require(all(f"test {case} ... ok".encode() in captured.splitlines() for case in cases),
                "required_business_case_result_missing")
        if stable_remote:
            probes = stable_legacy_probe_checks(captured)
            require(probes is not None and len(probes) >= len(cases),
                    "stable_legacy_projection_not_verified")
        if args.compatibility_host and args.suite != "diagnostic-native-stop":
            checks = compatibility_host_checks(captured)
            expected = {"import-target", "restart-target"}
            if args.suite in ("full", "diagnostic-failures"):
                expected.add("space-profile")
            require(checks is not None and checks and all(check["passed"] for check in checks)
                    and expected.issubset({check["stage"] for check in checks}),
                    "legacy_host_transitions_not_verified")
        if args.suite == "g1-business":
            report["standalone_stop"] = standalone_stop_checks(captured, report["runtime_binary_sha256"])
        if args.owned_stop_mode:
            require(digest(probe) == probe_identity["probe_binary_sha256"], "owned_stop_probe_binary_changed")
            report["owned_stop"]["observations"] = owned_stop_checks(captured, args.owned_stop_mode,
                probe_identity["probe_binary_sha256"], report["runtime_binary_sha256"], cli_sha256=digest(cli))
            report["owned_stop"].update(passed=True, runtime_archive_sha256=report["runtime_archive_sha256"],
                runtime_metadata_sha256=report["runtime_metadata_sha256"], runtime_source_sha=args.runtime_sha,
                cli_source_sha=args.cli_sha, harness_sha256=report["harness_sha256"])
        report["stage"] = "complete"
        report["passed"] = True
    except Exception as error:
        # No raw exception text or command output: auth replies can contain secrets.
        report["failure_type"] = type(error).__name__
        if isinstance(error, RuntimeError) and re.fullmatch(r"[a-z_]+", str(error)):
            report["failure_code"] = str(error)
    finally:
        try:
            forced_processes = []
            cleaned = cleanup_processes(root, hashes, forced_processes)
            time.sleep(0.1)
            require(cleanup_processes(root, hashes) == 0, "owned_process_cleanup_incomplete")
            roles = {report.get("cli_binary_sha256"): "current_cli",
                     report.get("runtime_binary_sha256"): "candidate_runtime",
                     report.get("harness_sha256"): "harness",
                     report.get("owned_stop", {}).get("source_binding", {}).get("probe_binary_sha256"): "original_client2_probe",
                     report.get("stable_business_cli", {}).get("binary_sha256"): "official_stable_cli"}
            for process in forced_processes:
                process["image_role"] = roles.get(process["image_sha256"], "run_bound_unknown")
            report["owned_process_cleanup"] = {"passed": True, "forced": cleaned,
                                                "forced_processes": forced_processes}
            if cleaned:
                report["passed"] = False
        except Exception:
            report["owned_process_cleanup"] = {"passed": False}
            report["passed"] = False
        if (root / "keyring/business-keyring-selftest.json").is_file():
            report["keyring"] = json.loads((root / "keyring/business-keyring-selftest.json").read_text())
        if log.is_file():
            captured = log.read_bytes()
            lines = captured.splitlines()
            if stable_full:
                legacy_keyring_path = root / "legacy-keyring/business-keyring-selftest.json"
                if legacy_keyring_path.is_file():
                    report["legacy_contracts"]["keyring"] = json.loads(legacy_keyring_path.read_text())
                report["legacy_contracts"]["current_full31_passed"] = (
                    len(cases) == 31 and all(f"test {case} ... ok".encode() in lines for case in cases)
                    and b"test result: ok. 31 passed; 0 failed;" in captured)
                legacy_log = root / "legacy-private.log"
                if legacy_log.is_file():
                    private = legacy_log.read_bytes()
                    passed = [case for case in BASELINE_CASES if f"test {case} ... ok".encode() in private.splitlines()]
                    failed = [case for case in BASELINE_CASES if case not in passed]
                    report["legacy_contracts"].update(passed_cases=passed, missing_or_failed_cases=failed,
                        failure_diagnostics=safe_failure_diagnostics(private, failed))
            if stable_remote:
                report["stable_legacy_probe_checks"] = stable_legacy_probe_checks(captured)
                report["stable_legacy_probe_scope"] = "harness_probe_only_not_old_binary_sender"
            if args.compatibility_host:
                report["compatibility_host_checks"] = compatibility_host_checks(captured)
            report["passed_cases"] = [case for case in cases if f"test {case} ... ok".encode() in lines]
            report["missing_or_failed_cases"] = [case for case in cases if case not in report["passed_cases"]]
            report["failure_diagnostics"] = safe_failure_diagnostics(captured, report["missing_or_failed_cases"])
            report["test_executed"] = bool(cases) and all(
                (f"test {case} ... ".encode() in captured) for case in cases)
        report["passed"] = report["passed"] and report["test_executed"]
        write_json(root / "result.json", report)
    print(json.dumps({"case": report["case"], "passed": report["passed"], "test_executed": report["test_executed"]}))
    return 0 if report["passed"] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("root", "cli-source", "runtime-source", "cli", "runtime", "harness"):
        parser.add_argument("--" + name, required=True, type=Path)
    parser.add_argument("--cli-sha", required=True)
    parser.add_argument("--runtime-sha", required=True)
    parser.add_argument("--suite", choices=("account", "full", "g1-business", "g1-owned-stop", "diagnostic-failures", "diagnostic-native-stop", "stable-diary-remote"), default="account")
    parser.add_argument("--runtime-archive", type=Path)
    parser.add_argument("--runtime-metadata", type=Path)
    parser.add_argument("--owned-stop-mode", choices=("registered", "client2-owned"))
    parser.add_argument("--client2-source", type=Path)
    parser.add_argument("--legacy-stable-contracts", action="store_true")
    parser.add_argument("--compatibility-host", action="store_true")
    parser.add_argument("--require-native-stop", action="store_true")
    parser.add_argument("--official-cli-metadata", type=Path,
                        help="Verified fixed official dev.6 GNU sidecar; preserves its release version")
    for name in ("stable-business-cli", "stable-business-cli-metadata", "stable-business-runtime-archive"):
        parser.add_argument("--" + name, type=Path)
    def interrupted(_number, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    args = parser.parse_args()
    if args.suite in ("g1-business", "g1-owned-stop"):
        if (not args.runtime_archive or not args.runtime_metadata or args.compatibility_host
                or args.official_cli_metadata or args.require_native_stop or args.legacy_stable_contracts):
            parser.error("g1-business requires frozen Runtime archive/metadata and current CLI without compatibility/native-stop/stable-contracts")
    elif args.runtime_archive or args.runtime_metadata:
        parser.error("frozen Runtime inputs require --suite g1-business or g1-owned-stop")
    if args.require_native_stop and args.suite != "full":
        parser.error("--require-native-stop requires --suite full")
    stable_paths = (args.stable_business_cli, args.stable_business_cli_metadata,
                    args.stable_business_runtime_archive)
    if args.legacy_stable_contracts:
        if (args.suite != "full" or not all(stable_paths) or args.official_cli_metadata
                or args.compatibility_host or not args.require_native_stop):
            parser.error("legacy stable contracts require full31, three fixed stable assets and the current manager")
    elif args.suite == "stable-diary-remote":
        if (not all(stable_paths) or args.official_cli_metadata or args.compatibility_host
                or args.require_native_stop):
            parser.error("stable-diary-remote requires three stable assets and the current manager without compatibility/native-stop")
    elif any(stable_paths):
        parser.error("stable business assets require --suite stable-diary-remote")
    validate_owned_stop_options(args)
    return run(args)


if __name__ == "__main__":
    sys.exit(main())
