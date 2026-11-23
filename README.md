# Respire

Open-source Monorepo hosting Respire's four product components and generated contract SDKs:

- **`apps/core/`** — Pure in-memory retrieval, embedding, and clustering kernel (open-source, C/Rust ABI, zero filesystem/DB/network I/O)
- **`apps/runtime/`** — Authoritative control & data plane daemon (Rust: SQLite persistence, model catalog, managed downloads, OpenAPI 3.1 + SSE server, frozen legacy JSON-RPC shim)
- **`apps/cli/`** — Ultra-thin CLI, TUI, `rsrs mcp` stdio bridge, and Runtime daemon launcher/installer (Rust)
- **`apps/client2/`** — Desktop shell, system tray, OS proxy/keyring bridge, and WebUI (Go + Wails)
- **`packages/`** — Auto-generated typed client SDKs (`runtime-sdk-rs`, `runtime-sdk-go`, `runtime-sdk-ts` — never hand-edited)
- **`contracts/`** — Read-only contract snapshot synced from [risense-ai/respire-resources](https://github.com/risense-ai/respire-resources) and pinned by SHA-256

---

## Core Engineering Rules

1. **English-Only Code & Docs**: All docs, source files, comments, scripts, PRs, and commits MUST be written in English. Non-English UI translations are allowed ONLY inside dedicated UI i18n paths (`**/i18n/**`, `**/locales/**`, etc.).
2. **Isolated Build Manifests**: `apps/core`, `apps/runtime`, `apps/cli`, and `apps/client2` each own separate dependency manifests and lockfiles. Never add a root `Cargo.toml`.
3. **Path-Filtered CI (`ci-gate`)**: Modifying `apps/client2/**` skips `core-ci`, `runtime-ci`, and `cli-ci` in 0s; modifying `apps/core/**` tests `core-ci` + `runtime-ci` without blocking `cli-ci` or `client2-ci`.
4. **Independent Component Releases**: Components ship independently on separate schedules with component-prefixed tags: `runtime-vX.Y.Z`, `cli-vX.Y.Z`, `client2-vX.Y.Z`.
5. **Spec-First**: Every change requires an approved OpenSpec Change and Milestone Issue in `risense-ai/respire-resources`.

---

## Entry Points for AI Agents & Contributors

- **[AGENTS.md](AGENTS.md)** — Repository rules inheriting `risense-ai/respire-resources/AGENTS.md`
- **[Cold-Start Agent Execution Guide](https://github.com/risense-ai/respire-resources/blob/main/openspec/AGENT_EXECUTION_GUIDE.md)** — Mandatory `respire-process.mjs next` loop
- **[OpenSpec Master Roadmap](https://github.com/risense-ai/respire-resources/blob/main/openspec/ROADMAP.md)** — Six-stage transformation DAG

## Local Guardrails

```sh
node scripts/check-guardrails.mjs
```

## License

Published under the [Respire Noncommercial License 1.0](LICENSE). Commercial use requires a separate written license — see [COMMERCIAL-LICENSE.md](COMMERCIAL-LICENSE.md). All third-party dependency licenses and attribution notices remain preserved.
