# CARD-007 implementation and verification status

This is a buildable partial migration, not an accepted M3 completion.
Independent verification and repository gates remain with the coordinator.

## Script and downstream compatibility follow-up

The continuation worked on the existing claimed CARD-007 worktree. It kept
the legacy script entry points and fixture cases. The file-size inventory
found 27 scripts and two coverage catalogs over 600 lines, excluding the
generated Cargo lockfile and ignored build output. The split and formatting
changes resolved 26 entries. These three orchestration scripts still exceed
the cap.

| Script | Lines |
| --- | ---: |
| `scripts/dev-login-interruption-smoke.py` | 1,502 |
| `scripts/test-real-m3-account-switch.py` | 1,193 |
| `scripts/test-runtime-autostart.py` | 1,166 |

Pinned Client2 `57bc070826949994c3f7ce9a64277160704bdfe5` runs
`--json runtime start` or the legacy `--json status` on its selected image.
Its `requireActive` and `requireExternalActive` checks compare health against
that image. The retained launcher regression also checks that the stable CLI
image owns the compatibility endpoint. Starting the authorized independent
Runtime cannot produce that result because the Runtime image owns the socket.
The newer Client2 source uses an explicit launch grant and verifies the
independent Runtime process. The retired CLI private flags still report a
startup failure. Support for the unchanged old client remains blocked.

The pinned Runtime provides authenticated model preparation and verification.
The CLI maintenance review still needs the committed model selection and the
active-asset fence. The Runtime dispatcher also explicitly rejects the
private secret and key export methods with `host_owned_secret`. The existing
SQLite fence, model maintenance, and Vault paths remain pending a Runtime
authority that can preserve those behaviors.

Local verification used the existing offline locked cache with the
`client2-host-prototype` feature and isolated test paths. The build check,
26 MCP tests, 17 IDE injection tests, six TUI tests, and 10 CLI contract/help
tests passed. The old compatibility transport unit group passed two cases
and failed Windows loopback listener ownership inspection. Synthetic script
checks passed for release diagnostics, coverage catalogs, selected release
and packaging cases, and the PowerShell terminal and keygen gates. The
broader scripts that require deleted legacy workflow files, the Windows
Python launcher, or unavailable native and loopback facilities did not pass
in this environment. No published launcher, real Runtime, or native GUI
acceptance was run. Verification logs use the `target/card007-*.log` prefix.

## Earlier CARD-007 continuation

- Removed the CLI's production compatibility listener. Regular CLI startup,
  including builds with `client2-host-prototype`, uses the existing authorized
  standalone Runtime launch and RPC client. The retired private Client2 launcher
  entry points report a startup failure. Old launchers that expect the CLI image
  to own the HTTP endpoint need a launcher update.
- Removed both legacy server dependencies. Existing HTTP regressions use bounded
  loopback fixtures compiled only for tests. Moved the standalone Runtime test
  pin to `test-support` and removed the nested `.github` directory.
- Split the frontend, RPC, Runtime installer, model maintenance, lifecycle and
  MCP files by responsibility and moved independent test groups into separate
  files. The remaining imported file size violations still require work.
- The checked-in Runtime does not yet expose the model selection, maintenance
  and takeover operations needed to remove the CLI's remaining SQLite access.
  The Runtime's private model plan supplies asset pins, and the CLI still needs
  a trusted source for profile provider configuration and download policy. The
  existing lock and Vault custody paths still protect legacy installations.

The sections below record the original import pass.

## Implemented

- Imported committed CLI, TUI, MCP, IDE injection, launcher, client helpers,
  tests, connectors, and packaging sources at the revision in provenance.
- Removed the embedded Runtime, Core SDK, app, domain, storage, crypto engine,
  server feature graph, and server command implementations.
- Kept independent CLI manifests and regenerated the lockfile offline.
- Preserved first-party and third-party licensing.
- Relocated existing localized content into allowed i18n paths.
- Changed legacy contract tests to require an explicit external fixture root.
- Confirmed the original launcher rejects substituting the CLI image for the
  independent Runtime. Protocol checks and image identity checks were retained;
  no product-version compatibility gate or in-process fallback was added.

## Environment and reproducible commands

All commands below ran from the assigned monorepo worktree. The cache was
copied from existing registry archives/indexes, without copying credentials.
Build outputs, extracted dependencies, contract fixtures, temporary directories,
and synthetic profile data are under ignored `apps/cli/target`.

```powershell
$env:CARGO_HOME = Join-Path $PWD 'apps/cli/target/cargo-home'
$env:CARGO_TARGET_DIR = Join-Path $PWD 'apps/cli/target'
$env:RSRS_CONTRACT_FIXTURES = Join-Path $PWD 'apps/cli/target/contracts/runtime-control'
$env:RSRS_DATA_DIR = Join-Path $PWD 'apps/cli/target/synthetic-data'
$env:TEMP = Join-Path $PWD 'apps/cli/target/tmp'
$env:TMP = $env:TEMP
```

The frozen fixture tree is obtained with a read-only archive operation against
the source revision, followed by extraction into `apps/cli/target`:

```text
git archive --format=tar --output=<assigned-cli-target>/contract-fixtures.tar 3a28019305839f0265a918c74178d3905b461bd0 contracts/runtime-control
tar -xf apps/cli/target/contract-fixtures.tar -C apps/cli/target
```

Do not use a live profile or credentials to substitute for the synthetic fixtures.

| Exact verification command | Result |
| --- | --- |
| `cargo check --manifest-path apps/cli/Cargo.toml --offline --locked` | Passed; existing unused-code warnings remain |
| `cargo test --manifest-path apps/cli/Cargo.toml --offline --locked -p respire --no-run` | Passed; all CLI test targets compiled |
| `cargo test --manifest-path apps/cli/Cargo.toml --offline --locked -p respire_protocol -p respire_crypto -p respire_logging -- --test-threads=1` | Passed: 38 tests |
| `cargo test --manifest-path apps/cli/Cargo.toml --offline --locked -p respire --lib mcp:: -- --test-threads=1` | Passed: 26 tests |
| `cargo test --manifest-path apps/cli/Cargo.toml --offline --locked -p respire --lib host_ui::inject::tests:: -- --test-threads=1` | Passed: 17 tests |
| `cargo test --manifest-path apps/cli/Cargo.toml --offline --locked -p respire --lib shell:: -- --test-threads=1` | Passed: 6 tests |
| `cargo test --manifest-path apps/cli/Cargo.toml --offline --locked -p respire --test version_flag --test runtime_help --test control_contract_fixtures -- --test-threads=1` | Passed: 10 tests |
| `cargo test --manifest-path apps/cli/Cargo.toml --offline --locked -p respire_commands capture:: -- --test-threads=1` | Passed: 3 tests |
| `cargo test --manifest-path apps/cli/Cargo.toml --offline --locked -p respire_commands i18n:: -- --test-threads=1` | Passed: 4 tests |
| `cargo test --manifest-path apps/cli/Cargo.toml --offline --locked -p respire_commands model_progress:: -- --test-threads=1` | Passed: 3 tests |
| `cargo test --manifest-path apps/cli/Cargo.toml --offline --locked -p respire_commands hooks::tests::timeout_terminates_hook_tree_and_unblocks_writer -- --exact --nocapture` | Failed: process-tree termination returned exit code 1; stdin pipe ended (Windows error 109) |
| `node scripts/check-guardrails.mjs` | Failed: two `tiny_http` dependency findings |

The 107 passing tests are separate existing tests, not a full-suite pass.
The combined command including all `respire_commands` tests was interrupted
after the hook failure and a stall following the model-asset test. It must not
be treated as passing. No failing assertion was weakened and no test was hidden
with an ignore annotation. Full real-Runtime integration tests were not run:
their pinned independent Runtime fixture was not provisioned in this worker.

Logs are in `target/{check,cli-test-build,pure-tests,mcp-tests,inject-tests,
tui-tests,cli-contract-tests,capture-tests,i18n-tests,model_progress-tests,
hook-timeout-test,unit-tests,guardrails}.log`.

## Outstanding acceptance gates

1. Replace the legacy authenticated client host HTTP boundary before removing
   production `tiny_http`. The command crate also retains `tiny_http` for real
   loopback test fixtures; the guardrail currently rejects that dev dependency.
   No dependency alias or scanner exception was introduced to hide either use.
2. Move SQLite ownership locking, host credential preparation, and local model
   maintenance to compatible Runtime boundaries. Preserve the existing active
   asset fence and Vault behavior until that replacement is verified.
3. Replace frozen legacy protocol consumption with the shared Runtime SDK where
   the required legacy/private host operations are available. The current
   import does not claim exclusive SDK consumption or full protocol migration.
4. Split oversized imported source files. The current audit reports 66 source
   files over 600 lines, including an 861-line command enum and a 618-line
   frontend dispatcher. The complete list is `target/over-limit-files.txt`.
   This hard requirement remains unsatisfied; compilation does not waive it.
5. Resolve the Windows child-process termination test and complete the stalled
   download tests with synthetic loopback fixtures, then run independent
   verification and the required real-Runtime regression suite.

No staging, commit, push, remote claim, CI edit, canonical contract edit, or
OpenSpec edit was performed. Initial untracked provenance and dependency
notices were retained. Local code-tool activation created an untracked
`.serena/` directory outside `apps/cli`; it was not staged, deleted, or committed.
That incidental tool output is a scope exception for coordinator review.
