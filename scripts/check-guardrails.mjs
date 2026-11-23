#!/usr/bin/env node
// Respire Monorepo 架构护栏校验脚本（零外部依赖）
// 校验内容：
// 1. 禁止根目录出现 Cargo.toml（严禁合并 runtime 与 cli 为单一 Workspace）；
// 2. 若 apps/cli/Cargo.toml 存在，严禁引用 ../runtime 或服务端/存储 crate；
// 3. 若 apps/client2/go.mod 存在，严禁引入 sqlite 驱动（如 go-sqlite3）；
// 4. contracts/sync.lock.json 必须与 contracts/runtime-api/openapi.yaml 校验和一致；
// 5. .github/workflows/ 下文件数量 <= 5。

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const root = process.cwd();
const errors = [];

// 1. 禁止根 Cargo.toml
if (fs.existsSync(path.join(root, "Cargo.toml"))) {
  errors.push("禁止在 Monorepo 根目录放置 Cargo.toml，apps/runtime 与 apps/cli 必须保持物理独立构建清单");
}

// 2. 扫描 apps/cli 禁用依赖
function findFilesNamed(dir, fileName, acc = []) {
  if (!fs.existsSync(dir)) return acc;
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === "target" || entry.name === "node_modules" || entry.name === ".git") continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      findFilesNamed(full, fileName, acc);
    } else if (entry.isFile() && entry.name === fileName) {
      acc.push(full);
    }
  }
  return acc;
}

const forbiddenCliPatterns = [
  { label: "跨应用引用 runtime", regex: /\.\.\/runtime|\bapps\/runtime\b/ },
  { label: "respire_storage", regex: /\brespire_storage\b/ },
  { label: "respire_core_sdk", regex: /\brespire_core_sdk\b/ },
  { label: "respire_app / respire_app_domain", regex: /\brespire_app(_domain)?\b/ },
  { label: "axum / tiny_http 服务端框架", regex: /\b(axum|tiny_http)\b/ },
];

for (const cargoToml of findFilesNamed(path.join(root, "apps", "cli"), "Cargo.toml")) {
  const text = fs.readFileSync(cargoToml, "utf8");
  const rel = path.relative(root, cargoToml);
  for (const rule of forbiddenCliPatterns) {
    if (rule.regex.test(text)) {
      errors.push(`${rel} 触发 CLI 禁用依赖规则: ${rule.label}`);
    }
  }
}

// 3. 扫描 apps/client2 禁用 SQLite 驱动
const client2GoMod = path.join(root, "apps", "client2", "go.mod");
if (fs.existsSync(client2GoMod)) {
  const goModText = fs.readFileSync(client2GoMod, "utf8");
  if (/go-sqlite3|\bsqlite3\b/i.test(goModText)) {
    errors.push("apps/client2/go.mod 禁止引入 go-sqlite3 或任何 SQLite 驱动");
  }
}

// 4. 校验 contracts/sync.lock.json 与 openapi.yaml 校验和一致
const lockPath = path.join(root, "contracts", "sync.lock.json");
const openapiPath = path.join(root, "contracts", "runtime-api", "openapi.yaml");

if (!fs.existsSync(lockPath)) {
  errors.push("缺失契约锁文件: contracts/sync.lock.json");
} else if (!fs.existsSync(openapiPath)) {
  errors.push("缺失同步契约文件: contracts/runtime-api/openapi.yaml");
} else {
  try {
    const lock = JSON.parse(fs.readFileSync(lockPath, "utf8"));
    const openapiBytes = fs.readFileSync(openapiPath);
    const actualSha256 = crypto.createHash("sha256").update(openapiBytes).digest("hex");
    if (!lock.commit || !/^[a-f0-9]{40}$/.test(lock.commit)) {
      errors.push("contracts/sync.lock.json 缺少有效的 40 位 commit SHA");
    }
    if (lock.openapi_sha256 !== actualSha256) {
      errors.push(
        `contracts/runtime-api/openapi.yaml 校验和不匹配: lock=${lock.openapi_sha256}, actual=${actualSha256}`
      );
    }
  } catch (err) {
    errors.push(`解析 contracts/sync.lock.json 失败: ${err.message}`);
  }
}

// 5. 校验 Workflow 数量上限 <= 5
const workflowsDir = path.join(root, ".github", "workflows");
if (fs.existsSync(workflowsDir)) {
  const wfFiles = fs
    .readdirSync(workflowsDir, { withFileTypes: true })
    .filter((d) => d.isFile() && (d.name.endsWith(".yml") || d.name.endsWith(".yaml")));
  if (wfFiles.length > 5) {
    errors.push(`.github/workflows 下文件数量为 ${wfFiles.length}，超过核心流水线 5 个上限`);
  }
}

if (errors.length > 0) {
  console.error("Respire 架构护栏校验失败:");
  for (const err of errors) {
    console.error(`  - ${err}`);
  }
  process.exit(1);
}

console.log("Respire 架构护栏校验通过。");
