# Respire Client2

This directory contains the Wails desktop client imported for CARD-008 under
OpenSpec `003-monorepo-merge-and-release-bridge`.

The import is **not ready for integration**. See
[SOURCE-PROVENANCE.md](SOURCE-PROVENANCE.md) for the exact source revision,
preserved safety behavior, migration exceptions, and verification results.

The application retains the source desktop shell, tray, onboarding, quick
panel, memory views, diary, organize, session import, models, accounts and
spaces, injection, access grants, settings, updates, operations, and
diagnostics. Model management retains GPU/CPU selection and the original
uninstall workflow. Source preservation is not proof of compatibility with
the monorepo Runtime.

## Local checks

Run the repository guardrail command from the monorepo root:

```text
node scripts/check-guardrails.mjs
```

Client2 uses the monorepo's generated Go Runtime SDK. Generate it from the
pinned contract as described in [Runtime SDKs](../../packages/README.md) before
running Go checks or desktop build scripts. Keep `GOMODCACHE` and `GOCACHE`
outside the worktree. Once the locked dependencies are available locally,
`GOPROXY=off`, `GOSUMDB=off`, and `GOTOOLCHAIN=local` keep Go checks offline.

Run component checks from this directory:

```text
go vet ./...
go test ./...
node scripts/test-account-session-state.cjs
node scripts/test-client-refresh.cjs
node scripts/test-settings-proxy.cjs
```

The JavaScript tests execute production frontend code with synthetic DOM and
backend fixtures. They do not validate a real browser or native Wails window.
Native Runtime integration tests require separately supplied fixture artifacts.

## Licensing

First-party source uses the root Respire Noncommercial License 1.0, copied
into [LICENSE](LICENSE) for component packaging. Third-party notices and font
licenses remain in `internal/desktop/frontend/notices.txt` and
`internal/desktop/frontend/fonts/*-OFL.txt`.
