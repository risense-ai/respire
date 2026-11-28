# AGENTS.md — Respire Monorepo Constitution

These rules apply throughout `risense-ai/respire`. Local rules may add stricter checks or build instructions.

## 1. English and UI Localization

Write specifications, docs, source, comments, scripts, workflows, commits, PRs, and Issues in English. Localized UI strings are permitted only in dedicated i18n files and fixtures, including `**/i18n/**`, `**/i18n.js`, `**/locales/**`, `**/*_zh*.json`, `**/*.zh-CN.*`, and `**/test-cases/fixtures/i18n/**`.

## 2. Spec Approval, Contracts, and License

- Propose each feature, cross-module refactor, interface change, and UI redesign in `openspec/changes/<change-id>/` with `proposal.md`, `design.md`, `tasks.md`, and `specs/<domain>/spec.md`. The Maintainer must approve the Change and label its Issue `spec:approved` before anyone branches for implementation or edits product code.
- Run `node process/scripts/respire-process.mjs next` from the repository root to select an approved card. The [execution guide](openspec/AGENT_EXECUTION_GUIDE.md) describes dispatch and handoff.
- [Runtime OpenAPI](contracts/runtime-api/openapi.yaml) is canonical for Runtime HTTP and SSE. [Core ABI](contracts/core-abi/README.md) defines the Core FFI boundary. Generate SDKs from the contract and prohibit hand-written duplicate DTOs across components.
- For Change `008` and later, obtain `spec:approved` on the `openspec/` design and specs before implementation. Land new OpenAPI endpoints, generated `packages/runtime-sdk-*`, and their Runtime and Client2 implementation atomically in one PR. Keep [v1-reviewed-route-gaps.json](apps/runtime/scripts/v1-reviewed-route-gaps.json) at `[]`.
- The public source uses [Respire Noncommercial License 1.0](LICENSE). Follow the [commercial licensing terms](COMMERCIAL-LICENSE.md) and preserve third-party licenses and notices. Keep the legacy `risense-ai/respire-core` archive private.

## 3. Architecture and Build Isolation

| Component | Responsibility and constraints |
| --- | --- |
| `apps/core` | Pure in-memory inference, retrieval, ranking, classification, and FFI. Accept buffers and callbacks from the host. Never import production filesystem, directory, SQLite, keyring, HTTP, or network I/O crates. |
| `apps/runtime` | Sole authoritative control and data plane. Own SQLite, model management, OpenAPI and SSE, legacy RPC, and account and space transactions. Never add a secondary Clap parser, TTY prompts, IDE injection, or unpreemptable background write locks. |
| `apps/cli` | Thin CLI, TUI, MCP stdio bridge, OS autostart, and Runtime installer and watchdog. Never import `apps/runtime`, `apps/core`, `storage`, `core-sdk`, `app`, `axum`, or `tiny_http`. Never stop Runtime or write `Paused` for downloads, configuration, or uninstall. |
| `apps/client2` | Wails desktop shell, tray, OS proxy and keyring bridge, WebUI, and OpenAPI and SSE client. Never import SQLite drivers in `go.mod`, add Go-side account, space, or model state machines, or add `cli.exec` argument tables. |

- Keep each app's build manifest independent. Never create a root Cargo workspace joining Core, Runtime, and CLI.
- Use `packages/runtime-sdk-*` as the sole shared Runtime-to-client code boundary. Never hand-edit generated SDKs.

## 4. Independent Verification

- A read-only Verifier in a fresh session or process provides final verification. Implementers must never sign off on their own changes.
- Support verification claims with command output, test logs, HTTP responses, or visual evidence. Mark checks `UNTESTED` with the reason when coverage is unavailable. Never fabricate results or weaken assertions.
- Keep specs, workflows, and scripts tool-agnostic. Omit vendor-specific agent names and private session prompts.

## 5. Database, Vault, and Feature Compatibility

- Only Runtime may open SQLite databases. CLI and Client2 must never access `.db` files directly.
- Preserve existing columns, keys, ciphertext and nonce layouts, and sync triggers in authoritative tables (`memories`, `memory_chunks`, `meta`, `sync_*`, `review_history`, `access_grants`, `query_log`, `query_feedback`, `skill_runs`, `entry_audit`, `recall_pairs`, `core_artifacts`). Extend schema through idempotent additive columns with defaults or new tables. Rebuild only derived caches (`retrieval_*`, `diary_*`).
- Before migrating a legacy database, create an atomic `<db>.bak-pre-v<N>-<ts>` backup via SQLite Online Backup. Run migration and `PRAGMA quick_check` in a single transaction. Roll back on failure.
- Inspect columns and triggers when bridging unversioned 1.x databases (`user_version = 0`). Advance `PRAGMA user_version` monotonically, preserve shipped migrations, and reject higher versions with an explicit upgrade error.
- Runtime storage PRs must pass lossless upgrade, recall, `sync_outbox` preservation, and idempotent restart checks against sanitized 1.x fixtures in `test-cases/fixtures/db/`.
- Preserve `~/.rsrs/`, `RSRS_DATA_DIR`, `accounts/<name>/`, `session.json`, `client.json`, diary caches, and `lock.db` for existing profiles.
- Preserve login and decryption for Vault v1 (legacy derivation plus Account Secret), v2 (PBKDF2-SHA256 at 210k), v3 (PBKDF2 plus Secret Key), and v4 (18-byte `A3-...` entropy with HKDF). Preserve `onememory:kek:v4`, `rsrs:kek:v4`, `onememory:kek:v1`, `rsrs:kek:v1`, `rsrs:data:v1`, `rsrs:embedding:v1`, `onememory:auth-salt:v1`, and `rsrs:auth-salt:v1`. Retain PBKDF2-SHA256 auth hashing at 100k, AES-256-GCM 12-byte nonces, unmarked legacy ciphertexts and `rsrs:v1:` envelopes, keyring service `rsrs` with `keyring_account` aliases, `--super`, and `rsrs keys-export`. Never write plaintext credentials to `session.json`.
- Remove only dead or duplicated code during slimming and move business state machines into Runtime. Preserve all user features, including CLI, TUI, MCP, IDE injection, all 16 Client2 views, accounts, spaces, grants, diary, organize, session import, sync and conflict recovery, and model install, switch, online uninstall, and CPU or GPU selection.

## 6. Capabilities, Plugins, and Windows

- Branch solely on `capabilities` from `GET /v1/health` or its legacy health fallback. Never compare Runtime version strings.
- Keep existing `rsrs` flags and JSON outputs, MCP stdio tools, and the frozen Runtime `/rpc` shim under 200 lines compatible during transition to OpenAPI 3.1 and discovery.
- Use local OpenAPI HTTP or `rsrs mcp` stdio for new and migrated integrations. Never transport large payloads through Windows CLI arguments.

## 7. Worktrees, Limits, and Local Gates

- Work in `../.worktrees/<card-id>/respire` on a feature branch. Keep clean `main` clones for dispatch and worktree creation.
- Limit non-archive OpenSpec Markdown to 400 lines and new or refactored Rust, Go, JS, and TS files to 600 lines. Reference sections, paths, or symbols instead of source line-number anchors.
- Never add `.unwrap()` or `.expect()` to non-test Rust code.
- Use Conventional Commits, attach the matching Milestone, and include one leading `Fixes <owner/repo>#<issue>` directive and concrete verification evidence when the PR completes a card.
- Before opening a PR, run both checks from the repository root and run the relevant component tests.

```sh
node process/scripts/validate-openspec.mjs
node scripts/check-guardrails.mjs
```
