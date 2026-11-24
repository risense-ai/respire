# Client2 source provenance and migration status

## Source

- Repository: `https://github.com/risense-ai/respire-client2`
- Local read-only source: `D:/sources/repos-new/gh/1memory/respire-client2`
- Commit: `87ced0aa1f43cd3d8fe9c973758edfbac3fbec96`
- Import date: 2026-11-24
- Destination: `apps/client2/` in the CARD-008 worktree
- Specification: local OpenSpec `003-monorepo-merge-and-release-bridge`
- Claim authority: the Coordinator's supplied successful CARD-008 claim receipt;
  no remote process or claim command was repeated by this worker.

The input was a Git archive of the exact commit, not the legacy working tree.
Local uncommitted source changes are therefore not part of this import. The
source repository was neither modified nor built. This is a source snapshot;
Git history has not been merged into the destination.

Included source roots: `.gitattributes`, `.gitignore`, `build`, `cmd`,
`exit_other.go`, `exit_windows.go`, `go.mod`, `go.sum`, `internal`, `main.go`,
and `scripts`. The original first-party LICENSE and README were replaced by
the monorepo license and component-specific English documentation.

Excluded roots: `.github`, `.worktrees`, source agent instructions, historical
docs/screenshots, and the translated README. No contracts, dependency trees,
release binaries, or build outputs were imported. Third-party font assets,
font licenses, and `internal/desktop/frontend/notices.txt` were preserved.

## Preserved behavior and blocking exceptions

This snapshot is **not an accepted M3 migration** and must not be merged as a
guardrail-compliant client. No compatibility shim or success placeholder was
introduced.

1. **Model deletion fence:** `internal/host/model_uninstall_selection.go` uses
   the source SQLite read-only/no-follow inspection of saved model selection.
   It retains database identity and sidecar validation, installation/profile
   locks, configuration rechecks, path ancestry and reparse-point checks,
   directory identity checks, staged removal, and post-removal checks.
   `model_uninstall.go` retains verified Runtime ownership, stop and restart
   checks. Removing the SQLite import alone would destroy this safety chain.
   The `go-sqlite3` dependency is a known monorepo boundary violation retained
   under the explicit instruction to preserve proven original safety behavior.
   A Runtime-owned equivalent must be verified before replacing it. The
   original online-entry workflow stops and restores its verified Runtime;
   uninterrupted online uninstall has not been established.
2. **Runtime host boundary:** Client2 still calls `/api/health` and
   `/api/rpc`, including `cli.exec` argv dispatch, and contains host-side
   account, space, and model workflows. The checked-in Runtime exposes v1
   health, authentication, and selected owner routes. Its v1 health response
   cannot supply the process ownership and account generation used by the
   existing client lifecycle and receipt checks. Client2 uses the Go SDK's
   generated health call for dashboard discovery and checks the legacy details
   against the v1 instance. A legacy-only listener can still serve the dashboard
   when discovery returns HTTP 404. SDK generation and missing Runtime routes
   are outside this worktree's Client2 write scope.

Runtime image/version comparisons in the imported safety and installation
code identify the selected artifact. No Client2-to-Runtime version equality
requirement was added. Each operation still needs its own capability and a
Runtime route that supplies the data and guarantees the desktop needs.

## Runtime authority integration

Client2 uses an in-process Go interface for host authority. The current verified
private child protocol has no implementation of this interface. A missing
authority fails before ownership is granted. Release acceptance of the
dependent actions requires the Runtime worker integration.

| Operation | Required Runtime behavior |
| --- | --- |
| Ownership acquisition and release | Authenticate the verified image, parent and installation using the inherited private channel. Acknowledge acquisition only after taking the same exclusion as CLI takeover at `runtime/takeover/lock.db` or the Runtime/library exclusion for the scoped directory. Derive and validate the root, runtime directory and eligible source/target profiles on the Runtime side. Preserve bounded waiting for takeover and immediate busy errors for library requests. Retain valid ownership through Close, including across cancellation of the acquisition context. A child that can die and release a lock while the host keeps writing is insufficient. Use transferable ownership or execute the guarded writes in Runtime. |
| Offline account maintenance | Under takeover, require a closed listener and acquire the installation and selected library exclusion before creating the existing operation record, active pointer and paused marker. Keep these locks through publication, then release the directory locks so the existing account worker can acquire its own ownership. Keep takeover through handoff and recovery. Bind to the same canonical profile, issuer, operation, owner and marker bytes. The current account workers expect the marker to exist before launch. |
| Online BGE-M3 uninstall | Keep takeover across verified stop, removal and restart. The private removal operation must bind the same root, profile, address and target, and check the config digests under exclusion. Take the stopped-Runtime installation, library and inference fences. Coordinate with all supported startup, selection and download paths that can use the assets. Veto implicit legacy BGE-M3, explicit BGE-M3 and another selection with an asset under the target. Check aliases and file identity. Validate the tree, stage it beside the target, and delete and verify under the same exclusion. Return removed, absent, active or an explicit error. Client2 retains its pre-stop identity checks and post-restart account checks. |

`GET /v1/models/active-assets` remains an optional pre-stop veto. The private
child presently supports model install planning and verification. Those
operations have no uninstall mutation or transferable ownership. Existing
account mutation operations acquire their own scoped locks after admission.

## Completed separable migration

- Extracted 791 Go literals into `internal/i18n/messages_*.go` without changing
  their string values. Call sites use constants, including existing tests.
- Extracted 1216 frontend/test language strings and template fragments into
  `internal/desktop/frontend/ui/i18n/messages*.js`. The existing source-execution
  test harness loads the production catalogs through `scripts/frontend-source.cjs`.
  Existing assertions remain, including original request receipt checks.
- Split the existing translation dictionary into bounded data modules and
  changed static HTML labels to their existing English translations.
- Split three long CSS files at complete rule boundaries, preserving rule
  order through relative CSS imports and preserving font-relative URLs.
- Split five long Go files at top-level declarations: injection, bootstrap
  tests, device validation, operations, and account control tests. Package
  names and function bodies remain in the original packages.
- Split memory workspace events, onboarding steps, model presentation and
  actions, and session-import results into bounded frontend modules.
- Moved the desktop's installed zh-CN instruction templates into its locale
  assets and kept the embedded bytes. Moved Chinese installer UI messages,
  diagnostic log patterns, and release announcements into locale resources.
- Isolated the existing legacy RPC envelope from the local HTTP transport.
  Request decoration and response validation share the same receipt policy.
  This keeps the account, operation, and parent operation identifiers on
  the existing wire protocol while the OpenAPI SDK and routes are pending.
- Replaced the component README with English migration and verification notes.

## Import verification history

The initial import used local Go dependencies with network access disabled.
The following results are historical and predate the host authority boundary.

- `node scripts/check-guardrails.mjs` at the monorepo root: **FAIL**, SQLite
  dependency only. The scan reported zero CJK violations.
- `go build ./...`, `go test -run '^$' ./...`, and `go vet ./...`: **PASS**.
- `go test -json ./...`: **FAIL**, exit 1. The same 27 cases failed before
  and after the RPC refactor, with 192 passed and 1 skipped, including nested
  results. Of 21 failed leaf cases, 14 reported loopback socket denial and
  3 reported Windows Credential Manager denial. The other 4 reported release
  lookup or model download assertions. Those fixtures use loopback HTTP.
  The Coordinator subsequently ran the native baseline with working loopback
  and reported 228 pass events, four skips and exit 0. That baseline precedes
  the v1 dashboard discovery and active-model veto.
- `go vet ./...` and `go test ./...` initially failed before compilation because
  the fresh isolated module cache was empty and module lookup was disabled.
- `node scripts/test-account-session-state.cjs`: **PASS**, 40 cases.
- `node scripts/test-client-refresh.cjs`: **PASS**, 14 cases.
- `node scripts/test-settings-proxy.cjs`: **PASS**, 5 cases.
- `node --check` for all 50 frontend JavaScript files: **PASS**.
- All 154 relative frontend module imports resolve, and all five existing
  entry point exports in the split modules load.
- Source size and language placement scan: **PASS**, 246 files at or below
  600 lines and zero CJK characters outside locale resources.
- PowerShell diagnostic parser check: **PASS**. Git Bash could not start
  because Windows denied creation of its MSYS namespace. Release-script
  execution and installer compilation remain unverified.
- `git diff --check`: **PASS** for tracked changes; the imported source files
  remain untracked, so this is not a whitespace review of the entire import.
- Native UI, cross-platform builds, real GPU inference, Runtime integration,
  and a live model uninstall: **UNTESTED**. No live user data was used.

No staging, commit, push, CI edit, remote claim, PR, release, or merge was
performed. Independent verification and acceptance remain with the Coordinator.

## Workspace exception

Project activation by the code-navigation tool automatically created an
untracked root `.serena/` directory outside the requested `apps/client2/**`
scope. It has not been staged or deleted. No other tracked path outside
`apps/client2/**` changed. `git diff --cached --stat` is empty.
