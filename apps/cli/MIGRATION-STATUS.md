# CARD-007 implementation and verification status

This is a buildable partial migration, not accepted M3 completion.
Independent verification and repository gates remain with the coordinator.
This follow-up starts from `7d349b61ccc594160c959713c372f866b9011c61`
(`feat(cli): import client-only CLI and wire component CI`).

## Current implementation

- Imported the committed client sources recorded in `SOURCE-PROVENANCE.md`.
  The CLI keeps an independent workspace manifest and an offline lockfile.
- Removed the embedded Runtime, Core SDK, app, domain, storage, crypto engine,
  server feature graph, and server command implementations.
- Removed the production CLI compatibility listener and both `tiny_http`
  dependencies. `client2-host-prototype` is now an empty build option.
  Retired private launcher entry points report startup failure. Ordinary CLI
  startup resolves and launches an independent Runtime, never a CLI server shim.
- Preserved licensing, Vault custody, SQLite ownership fences, and local model
  maintenance where a compatible Runtime authority is still required.
- Split imported code and scripts. Tracked CLI code, scripts, and JSON catalogs
  now have no files over 600 lines. The earlier 66-file inventory and three
  oversized orchestration-script counts described intermediate states.
- Kept frozen legacy contract consumers behind `RSRS_CONTRACT_FIXTURES` rather
  than duplicating canonical contracts. Localized materials remain in i18n paths.

## Synthetic transport and CI follow-up

Hosted support-crate verification reported only
`net_rpc::call::tests::response_after_client_deadline_reports_unknown_and_can_be_inspected_without_replay`
failing. The HTTP fixture treated the health client's empty TCP connectivity
probe as an incomplete HTTP request. Its actor exited before serving health,
so the test failed before reaching the intended dispatch deadline.

The shared test-only HTTP fixture now skips empty connections but still rejects
partial HTTP headers. A focused regression checks both behaviors. The delayed
write fixture acknowledges actor readiness, allows five seconds for fixture
coordination, and uses a separate 250 ms dispatch deadline. A channel withholds
its response until the client reports `OutcomeUnknown`. All existing operation
scope, result inspection, and no-replay assertions remain. Production health
and request timeouts and ownership checks are unchanged.

`cli-ci` retains the complete support-crate test command. It also runs
`rpc::runtime_resolution_tests::` and `autostart::lifecycle::tests::` separately
with `RUST_TEST_THREADS=1`. These fixtures use disposable profiles, synthetic
images, and the test manager adapter, not published Runtime bundles or actual
service registration. Linux-only registration and restart cases belong on the
Ubuntu runner. These groups do not establish real-Runtime product acceptance.

## Packaged startup remains unverified

The retained launcher baselines distinguish Client2 `v0.2.56` source
`57bc070826949994c3f7ce9a64277160704bdfe5`, the default `v2.0.0-dev.5` source
`be682108c555a3e5a788ad74217e088bcb25170b`, and the newer host-owned candidate.
They are distinct acceptance inputs, not interchangeable versions of one test.

Tests requiring the removed CLI-owned endpoint and retired flags cannot pass
through that endpoint. This does not prove that every install-bound standalone
Runtime path is incompatible. CLI resolution still selects an independent
image; `runtime_acceptable` checks the selected image and profile. Scoped
business RPC reuse checks installation/profile identity separately, as covered
by the existing resolution test.

The existing `scripts/test-client2-launcher-compat.py` includes a genuine
`desktop-owned` path. It installs the supplied Runtime archive into a disposable
installation, checks the selected published desktop digest, and invokes that
original desktop headlessly. `client2_launcher_process_checks.py` verifies the
independent Runtime image, digest, process birth, profile, and OS listener owner.
The harness also checks status, RPC status, and reuse of the same Runtime.
The Windows `probe-client2-launch-runtime.ps1` separately checks installation
manifests and the complete published payload inventory. Synthetic CLI transport
tests cannot substitute for either path.

Packaged startup is UNTESTED here: verified desktop/Runtime bundles, matching
unchanged Client2 source, and unrestricted localhost are unavailable. The old
source clone was unreadable in this sandbox; its present HEAD and internal
startup branches were not reverified. The next useful acceptance run is the
existing launcher harness on an isolated hosted Linux x64 runner, with an
explicit baseline, matching source and published desktop bytes, and verified
Runtime archive. Record desktop-owned results separately from retired CLI-host
failures. No old clone or Client2 worktree was changed.

## Scoped offline verification

Commands run from this worktree with the existing isolated cache and fixtures:

```powershell
$env:CARGO_HOME = Join-Path $PWD 'apps/cli/target/cargo-home'
$env:CARGO_TARGET_DIR = Join-Path $PWD 'apps/cli/target'
$env:RSRS_CONTRACT_FIXTURES = Join-Path $PWD 'apps/cli/target/contracts/runtime-control'
$env:RSRS_DATA_DIR = Join-Path $PWD 'apps/cli/target/synthetic-data'
$env:TEMP = Join-Path $PWD 'apps/cli/target/tmp'
$env:TMP = $env:TEMP
```

| Follow-up check | Result |
| --- | --- |
| `cargo test --manifest-path apps/cli/Cargo.toml --offline --locked -p respire_commands --lib --no-run` | Passed; existing warnings |
| `cargo test --manifest-path apps/cli/Cargo.toml --offline --locked -p respire --features client2-host-prototype --lib --no-run` | Passed; existing warnings |
| Support-crate command with `net_rpc::call::tests:: -- --test-threads=1` | Two argument/deadline tests passed; ambiguous-write test failed at health connection with sandbox socket error 10013 |
| Support-crate command with `loopback_http::tests:: -- --test-threads=1` | Failed at loopback connection with error 10013; fixture behavior remains unverified locally |
| Frontend command with `rpc::runtime_resolution_tests:: -- --test-threads=1` | Failed at loopback connection with error 10013 |
| Frontend command with `autostart::lifecycle::tests:: -- --test-threads=1` | Seven passed, two failed; maintenance hit socket error 10013 and cold-start failed its expected historical-session error assertion; Linux-only cases were not compiled on Windows |
| `node scripts/check-guardrails.mjs` | Passed: zero violations across 1,063 scanned files |

Logs use `apps/cli/target/card007-followup-*.log`. The coordinator must rerun
real-localhost timing and the Ubuntu groups; no local full-suite or hosted pass
is claimed. Earlier logs record historical checks, including failures before
listener removal and splitting. Those are not current results.

## Remaining acceptance gates

1. Complete real-localhost and Ubuntu verification without suppressing assertions,
   ignoring tests, or weakening production timeouts or ownership.
2. Move remaining SQLite locks, host custody, and local model maintenance to
   compatible Runtime boundaries while preserving active-asset and Vault checks.
3. Migrate frozen/private protocol consumers to the shared Runtime SDK when the
   required operations are available.
4. Run genuine install-bound packaged startup and real-Runtime product suites
   with verified inputs, then obtain independent acceptance.

The follow-up write set is limited to `apps/cli/**` and root
`.github/workflows/ci.yml`. No Git index, commit, push, GitHub, canonical contract,
OpenSpec, Runtime, or Client2 change was made. The pre-existing untracked
`.serena/` directory was retained.
