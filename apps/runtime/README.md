# apps/runtime

本目录承载 `respire-runtime`（权威控制面与数据面守护进程），**在 OpenSpec `003` 阶段从 `risense-ai/respire-runtime` 保留历史导入**。

- 必须保留独立 `Cargo.toml` 与 `Cargo.lock`；
- 唯一对外契约参见根目录 `contracts/runtime-api/openapi.yaml`；
- 详细合入与瘦身步骤见 `../respire-resources/openspec/changes/003-monorepo-merge-and-release-bridge/` 与 `004-ci-guardrails-and-code-slimming/`。
