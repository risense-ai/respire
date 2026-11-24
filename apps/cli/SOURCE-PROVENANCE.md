# CLI source provenance

The selected source is `risense-ai/respire-cli`, commit
`3a28019305839f0265a918c74178d3905b461bd0` on `main`.
The import was extracted with `git archive` from committed Git objects at this
revision, not copied from mutable working files. The local read-only source is
`D:/sources/repos-new/gh/1memory/respire-cli`; its HEAD resolved to the same
full commit. No legacy build, branch switch, staging, or source edit was made.

## Import filter

OpenSpec 003, design section 2, specifies the server exclusions. The actual
import excludes:

- The embedded `runtime/` and server crates under
  `crates/{core-sdk,app,app-domain,storage}/`, plus the crypto engine module and
  its optional Core SDK dependency.
- Server modules under `crates/commands/src/`: `admission`, `bench`, `engine`,
  `lexical`, `lexical_cache`, `model_install`, `network`, `recall_select`,
  `rpc_serve`, `rpc_worker`, `runtime_http`, `skill`, `sync_flow`, and `util`,
  including their implementation subdirectories.
- `cli/src/network.rs`, the otherwise unreferenced server-only
  `crates/commands/src/recall_metadata.rs`, and the server-only lexical-cache
  integration test.
- Duplicate `contracts/`, legacy `.github/workflows/`, and `.worktrees/`.
- Legacy Core SDK pin/download/staging/cache scripts and the `sdk/` pin.
- Repository-local agent instructions, historical repository documentation,
  and obsolete workflow configuration. `test-support/runtime-test-fixture.json`
  is retained as an existing test input, not a workflow.

The command crate no longer declares a `server` feature or dependencies on
`respire_app`, `axum`, or the asynchronous server runtime. Server branches in
capture, classify, metadata, review, hooks, and output emission were removed.
The workspace manifest and lockfile were updated offline for this graph.

## Explicit retained compatibility exceptions

The approved source is newer than the exclusion list: production host-control
calls the non-engine crypto primitives for compatible Vault v1-v4 custody,
key generation, rewrapping, and export. Those primitives and their original
validation remain in `crates/crypto`; no Core engine is linked. Removing the
whole crate would break real host callers.

`model_progress.rs` is used by Runtime binary downloads and cancellation
acknowledgement. `model_granite.rs` still has a frontend installer caller.
Both files were restored from the same source commit after inspecting their
callers. Their migration is outstanding, rather than silently disabling the
commands. SQLite ownership locks and the authenticated client host HTTP
boundary also remain unchanged pending a compatible replacement.

Chinese recall-fusion UI strings were moved verbatim into
`crates/commands/src/i18n/cli_zh.rs`. The synthetic Unicode path fixture lives
under `cli/test-cases/fixtures/i18n/`. Two injected instruction documents were
moved verbatim into `docs/i18n/`; their `include_str!` callers were updated.

## Test-only source materials

Frozen legacy `contracts/runtime-control` fixtures and consumer assertions
were extracted from this same commit into ignored `target/contracts` only.
Tests require `RSRS_CONTRACT_FIXTURES` to name that directory explicitly.
No duplicate contracts were added to the product source or canonical contracts.

First-party imports use the repository's [Respire Noncommercial License
1.0](../../LICENSE). Third-party components retain their original licenses.
The upstream CLI's Linux credential dependency notices are under
`docs/dependency-notices/` at the selected revision.
First-party Cargo manifests point to the monorepo root license. Local copies
of the same root license and commercial-license notice also support existing
packaging paths. No `BINARY-LICENSE.txt` was imported.
