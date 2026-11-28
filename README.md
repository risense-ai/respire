# Respire

Respire's monorepo contains the product components, their canonical contracts, and the engineering workflow.

- **`apps/core/`** — Pure in-memory retrieval, embedding, and clustering kernel (open-source, C/Rust ABI, zero filesystem/DB/network I/O)
- **`apps/runtime/`** — Authoritative control & data plane daemon (Rust: SQLite persistence, model catalog, managed downloads, OpenAPI 3.1 + SSE server, frozen legacy JSON-RPC shim)
- **`apps/cli/`** — Ultra-thin CLI, TUI, `rsrs mcp` stdio bridge, and Runtime daemon launcher/installer (Rust)
- **`apps/client2/`** — Desktop shell, system tray, OS proxy/keyring bridge, and WebUI (Go + Wails)
- **[packages/](packages/README.md)** — Generated Runtime SDKs for Rust, Go, and TypeScript
- **[contracts/](contracts/README.md)** — Canonical Runtime OpenAPI and Core ABI contracts
- **[openspec/](openspec/ROADMAP.md)** — Approved specs, Change proposals, and the roadmap
- **[prototypes/](prototypes/)** — HTML prototypes and a deterministic mock API
- **[test-cases/](test-cases/README.md)** — Black-box acceptance scenarios and fixtures
- **[process/](process/README.md)** — Governance roles, templates, and orchestration scripts
- **[scripts/](scripts/)** — Monorepo validation and build tooling
- **[AGENTS.md](AGENTS.md)** — Repository and architecture rules
- **[LESSONS.md](LESSONS.md)** — Lessons and recurring regression patterns

---

## Core Engineering Rules

1. **English-Only Code & Docs**: All docs, source files, comments, scripts, PRs, and commits MUST be written in English. Non-English UI translations are allowed ONLY inside dedicated UI i18n paths (`**/i18n/**`, `**/locales/**`, etc.).
2. **Isolated Build Manifests**: `apps/core`, `apps/runtime`, `apps/cli`, and `apps/client2` each own separate dependency manifests and lockfiles. Never add a root `Cargo.toml`.
3. **Path-Filtered CI (`ci-gate`)**: Modifying `apps/client2/**` skips `core-ci`, `runtime-ci`, and `cli-ci` in 0s; modifying `apps/core/**` tests `core-ci` + `runtime-ci` without blocking `cli-ci` or `client2-ci`.
4. **Independent Component Releases**: Components ship independently on separate schedules with component-prefixed tags: `runtime-vX.Y.Z`, `cli-vX.Y.Z`, `client2-vX.Y.Z`.
5. **Spec-First**: Obtain approval for the OpenSpec Change and Milestone Issue in `risense-ai/respire` before implementation.

---

## Contributor Entry Points

- **[Execution guide](openspec/AGENT_EXECUTION_GUIDE.md)** — Card dispatch, implementation, and independent verification
- **[Feature lifecycle](openspec/FEATURE_LIFECYCLE.md)** — Spec approval, implementation, and release in the monorepo

## Local Validation and Dispatch

```sh
node scripts/check-guardrails.mjs
node process/scripts/validate-openspec.mjs
node process/scripts/respire-process.mjs preflight
node process/scripts/respire-process.mjs next
```

## License

Published under the [Respire Noncommercial License 1.0](LICENSE). Commercial use requires a separate written license — see [COMMERCIAL-LICENSE.md](COMMERCIAL-LICENSE.md). All third-party dependency licenses and attribution notices remain preserved.
