# Respire

Respire 开源产品 Monorepo，统一承载三大独立应用与契约生成包：

- **`apps/runtime/`** — 权威控制面与数据面守护进程（Rust：SQLite 持久化、模型目录与托管下载、OpenAPI 3.1 + SSE 服务、旧 RPC 薄兼容）
- **`apps/cli/`** — 超薄 CLI / TUI / `rsrs mcp` stdio 桥与 Runtime 启停安装器（Rust）
- **`apps/client2/`** — 桌面应用壳、系统托盘与 OS 胶水适配器（Go + Wails + WebUI）
- **`packages/`** — 根据唯一 OpenAPI 3.1 契约自动生成的强类型客户端 SDK（`runtime-sdk-rs` / `runtime-sdk-go` / `runtime-sdk-ts`，禁止手改）
- **`contracts/`** — 从权威中枢仓 [risense-ai/respire-resources](https://github.com/risense-ai/respire-resources) 同步并锁定 SHA-256 的只读契约快照

---

## 核心工程原则（合仓不拖累）

1. **物理构建清单隔离**：`apps/runtime`、`apps/cli`、`apps/client2` 各自保留独立的 `Cargo.toml`/`Cargo.lock` 或 `go.mod`/`go.sum`，严禁共享顶层 Cargo Workspace，严禁跨 `apps/*` 源码相互引用。
2. **路径路由 CI（改谁只跑谁）**：改动 `apps/client2/**` 只触发 `client2-ci`，`runtime-ci` 与 `cli-ci` 0 秒跳过；分支保护只要求单一聚合门禁 `ci-gate`。
3. **组件独立发版**：三大端独立演进、**永不配对版本号**，通过组件前缀 Tag 触发各自流水线：
   - `runtime-vX.Y.Z`
   - `cli-vX.Y.Z`
   - `client2-vX.Y.Z`
4. **Spec-First（规范先行）**：任何新功能、接口改动或大重构必须先在 `risense-ai/respire-resources` 通过 OpenSpec 审批。

---

## AI Agent 与开发者入口

- **[AGENTS.md](AGENTS.md)** — 本仓开发红线（继承自 `respire-resources/AGENTS.md`）
- **[冷启动执行手册](https://github.com/risense-ai/respire-resources/blob/main/openspec/AGENT_EXECUTION_GUIDE.md)** — 多 Agent 接手推进 `001`–`006` 的操作规程与派单提示词
- **[OpenSpec 总路线图](https://github.com/risense-ai/respire-resources/blob/main/openspec/ROADMAP.md)** — 全系统演进计划

## 本地护栏检查

```sh
node scripts/check-guardrails.mjs
```
