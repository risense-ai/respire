# apps/cli

本目录承载 `respire-cli`（超薄 CLI / TUI / `rsrs mcp` / 守护启停），**在 OpenSpec `003` 阶段从 `risense-ai/respire-cli` 过滤掉 5.4 万行遗留服务端 crate 与历史脚本后保留历史导入**。

- 必须保留独立 `Cargo.toml` 与 `Cargo.lock`；
- 严禁依赖 `apps/runtime` 内部 crate、`storage`、`core-sdk`、`app` 或 `axum`；
- 唯一通信契约消费 `packages/runtime-sdk-rs`。
