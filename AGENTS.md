# AGENTS.md — `risense-ai/respire` Monorepo Guidelines

> **Inheritance**: This repository strictly inherits all global rules from [`risense-ai/respire-resources/AGENTS.md`](https://github.com/risense-ai/respire-resources/blob/main/AGENTS.md) (`../respire-resources/AGENTS.md`).

---

## Monorepo Hard Constraints

1. **English-Only Everywhere Except UI i18n**: All code, comments, commit messages, docs, scripts, and PR descriptions MUST be in English. Chinese text is permitted ONLY inside dedicated UI i18n files/fixtures (`**/i18n/**`, `**/i18n.js`, `**/locales/**`, `**/*_zh*.json`, `**/*.zh-CN.*`, `**/test-cases/fixtures/i18n/**`), enforced by CI.
2. **Spec-First & `respire-process` Dispatch**: Never start coding without an approved OpenSpec card via `node ../respire-resources/process/scripts/respire-process.mjs next`.
3. **Worktree Isolation**: Always branch inside `../.worktrees/<card-id>/respire`; never develop directly on the clean `main` clone.
4. **Build Manifest & Layer Isolation**:
   - NEVER create a root `Cargo.toml` merging `apps/core`, `apps/runtime`, and `apps/cli` into one workspace;
   - `apps/core` MUST NEVER import filesystem, SQLite, keyring, or network I/O crates;
   - `apps/cli` MUST NEVER import `apps/runtime`, `apps/core`, `storage`, `core-sdk`, `app`, `axum`, or `tiny_http`;
   - `apps/client2/go.mod` MUST NEVER import `github.com/ncruces/go-sqlite3` or any SQLite driver;
   - The ONLY shared cross-app boundary between Runtime and clients is `packages/runtime-sdk-*`.
5. **Never Hand-Edit Contracts or Generated SDKs**: Sync `contracts/` from `respire-resources` and regenerate `packages/runtime-sdk-*`.
6. **File Caps & Rust Safety**: New/refactored files **<= 600 lines**; zero new `.unwrap()` / `.expect()` in non-test Rust code.
7. **Capability Negotiation Only**: Branch strictly on `capabilities` from `/v1/health`, never on version strings.
8. **Run Guardrails Before PR**:

```sh
node scripts/check-guardrails.mjs
```
