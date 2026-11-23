# apps/client2

本目录承载 `respire-client2`（Wails 桌面宿主、系统托盘与 WebUI），**在 OpenSpec `003` 阶段从 `risense-ai/respire-client2` 保留历史导入**。

- 必须保留独立 `go.mod` 与 `go.sum`；
- 严禁在 `go.mod` 引入 `go-sqlite3` 或任何 SQLite 驱动；
- 唯一后端通信契约消费 `packages/runtime-sdk-go`。
