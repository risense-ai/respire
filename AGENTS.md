# AGENTS.md — `risense-ai/respire` Monorepo 工程准则

> **继承声明**：本仓无条件遵守 [`risense-ai/respire-resources/AGENTS.md`](https://github.com/risense-ai/respire-resources/blob/main/AGENTS.md) 全部全局宪法（本地兄弟目录 `../respire-resources/AGENTS.md`）。

---

## Monorepo 专属硬约束

1. **必须先在 `respire-resources` 看单与批准**：无已批准的 OpenSpec Change 禁止写业务代码；开工前请完整阅读 `../respire-resources/openspec/AGENT_EXECUTION_GUIDE.md`。
2. **Worktree 隔离**：禁止直接在干净主副本上开发，所有分支在 `../.worktrees/<change-id>/respire` 独立 Worktree 进行。
3. **构建物理隔离红线**：
   - 严禁在根目录创建 `Cargo.toml` 把 `apps/runtime` 与 `apps/cli` 绑进同一个 Cargo Workspace；
   - `apps/cli` 严禁引用 `apps/runtime` 内部 crate 或任何服务端/存储 crate（`storage` / `core-sdk` / `app` / `axum`）；
   - `apps/client2/go.mod` 严禁引入 `github.com/ncruces/go-sqlite3` 或任何 SQLite 驱动；
   - 各端唯一允许共享的接口层是 `packages/runtime-sdk-*`。
4. **禁止手改生成代码与锁定契约**：`contracts/runtime-api/openapi.yaml`、`contracts/sync.lock.json` 与 `packages/runtime-sdk-*` 必须由 `respire-resources` 同步及代码生成器生成，禁止人工手改。
5. **单文件体量与 Rust 安全**：新改或拆分后的代码文件 **≤ 600 行**；Rust 非测试代码禁止新增 `.unwrap()` / `.expect()`。
6. **只查 `capabilities`，禁查版本号**：`apps/cli` 与 `apps/client2` 对 Runtime 的特性判断一律走 `/v1/health`（及兼容端点）的 `capabilities` 字段。
7. **提交前必跑本地护栏**：

```sh
node scripts/check-guardrails.mjs
```
