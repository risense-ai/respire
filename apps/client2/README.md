# apps/client2

Hosts `respire-client2` (Wails desktop shell, system tray, OS keyring/proxy adapter, and WebUI), imported with preserved history from `risense-ai/respire-client2` during OpenSpec `003` (`CARD-008`).

- Owns an independent `go.mod` and `go.sum`;
- MUST NEVER import `go-sqlite3` or any SQLite driver;
- Consumes `packages/runtime-sdk-go` and SSE progress streams exclusively.
