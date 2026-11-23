#!/usr/bin/env node
// Respire Monorepo architecture and language guardrail checker (zero dependencies).
// Verifies:
// 1. No root Cargo.toml exists (forbids merging apps/core, apps/runtime, apps/cli into one Cargo workspace);
// 2. Any Cargo.toml under apps/core forbids I/O crates (rusqlite, sqlx, reqwest, ureq, axum, tiny_http);
// 3. Any Cargo.toml under apps/cli forbids ../runtime, ../core, and server/storage crates;
// 4. apps/client2/go.mod forbids go-sqlite3 or any sqlite3 driver;
// 5. contracts/sync.lock.json exists and matches contracts/runtime-api/openapi.yaml SHA-256;
// 6. .github/workflows/*.yml count <= 5;
// 7. Zero CJK characters outside dedicated UI i18n files/fixtures.

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const root = process.cwd();
const errors = [];

const CJK_REGEX = /[\u3000-\u303F\u3400-\u4DBF\u4E00-\u9FFF\uF900-\uFAFF\uFF00-\uFFEF]/;
const I18N_EXEMPT_REGEXES = [
  /(^|\/)i18n(\/|\.js$|\.ts$|\.json$)/,
  /(^|\/)locales\//,
  /_zh(-CN|-TW)?\.(json|js|ts)$/,
  /\.zh(-CN|-TW)?\.(json|js|ts)$/,
  /^test-cases\/fixtures\/i18n\//,
];
const TEXT_EXTENSIONS = new Set([
  ".md",
  ".yml",
  ".yaml",
  ".mjs",
  ".js",
  ".ts",
  ".json",
  ".html",
  ".css",
  ".sh",
  ".ps1",
  ".txt",
  ".toml",
  ".rs",
  ".go",
]);

function walkFiles(dir, predicate, acc = []) {
  if (!fs.existsSync(dir)) return acc;
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (
      entry.name === ".git" ||
      entry.name === "node_modules" ||
      entry.name === "target" ||
      entry.name === ".worktrees" ||
      entry.name === "vendor"
    ) {
      continue;
    }
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      walkFiles(full, predicate, acc);
    } else if (entry.isFile() && predicate(entry.name, full)) {
      acc.push(full);
    }
  }
  return acc;
}

// 1. Forbid root Cargo.toml
if (fs.existsSync(path.join(root, "Cargo.toml"))) {
  errors.push("Root Cargo.toml is forbidden: apps/core, apps/runtime, and apps/cli MUST keep isolated build manifests");
}

// 2. Forbid I/O crates inside apps/core
const forbiddenCorePatterns = [
  { label: "SQLite / SQL crates", regex: /\b(rusqlite|sqlx)\b/ },
  { label: "HTTP / network crates", regex: /\b(reqwest|ureq|axum|tiny_http)\b/ },
];

for (const cargoToml of walkFiles(path.join(root, "apps", "core"), (name) => name === "Cargo.toml")) {
  const text = fs.readFileSync(cargoToml, "utf8");
  const rel = path.relative(root, cargoToml).replace(/\\/g, "/");
  for (const rule of forbiddenCorePatterns) {
    if (rule.regex.test(text)) {
      errors.push(`${rel} violates Core pure-memory boundary: ${rule.label}`);
    }
  }
}

// 3. Forbid server/cross-app dependencies inside apps/cli
const forbiddenCliPatterns = [
  { label: "cross-app path dependency (runtime/core)", regex: /\.\.\/(runtime|core)\b|\bapps\/(runtime|core)\b/ },
  { label: "respire_storage", regex: /\brespire_storage\b/ },
  { label: "respire_core_sdk", regex: /\brespire_core_sdk\b/ },
  { label: "respire_app / respire_app_domain", regex: /\brespire_app(_domain)?\b/ },
  { label: "axum / tiny_http server crates", regex: /\b(axum|tiny_http)\b/ },
];

for (const cargoToml of walkFiles(path.join(root, "apps", "cli"), (name) => name === "Cargo.toml")) {
  const text = fs.readFileSync(cargoToml, "utf8");
  const rel = path.relative(root, cargoToml).replace(/\\/g, "/");
  for (const rule of forbiddenCliPatterns) {
    if (rule.regex.test(text)) {
      errors.push(`${rel} triggers CLI forbidden dependency rule: ${rule.label}`);
    }
  }
}

// 4. Forbid SQLite drivers inside apps/client2/go.mod
const client2GoMod = path.join(root, "apps", "client2", "go.mod");
if (fs.existsSync(client2GoMod)) {
  const goModText = fs.readFileSync(client2GoMod, "utf8");
  if (/go-sqlite3|\bsqlite3\b/i.test(goModText)) {
    errors.push("apps/client2/go.mod MUST NOT import go-sqlite3 or any SQLite driver");
  }
}

// 5. Verify contracts/sync.lock.json matches contracts/runtime-api/openapi.yaml
const lockPath = path.join(root, "contracts", "sync.lock.json");
const openapiPath = path.join(root, "contracts", "runtime-api", "openapi.yaml");

if (!fs.existsSync(lockPath)) {
  errors.push("Missing contract lock file: contracts/sync.lock.json");
} else if (!fs.existsSync(openapiPath)) {
  errors.push("Missing synced OpenAPI contract: contracts/runtime-api/openapi.yaml");
} else {
  try {
    const lock = JSON.parse(fs.readFileSync(lockPath, "utf8"));
    const openapiBytes = fs.readFileSync(openapiPath);
    const actualSha256 = crypto.createHash("sha256").update(openapiBytes).digest("hex");
    if (!lock.commit || !/^[a-f0-9]{40}$/.test(lock.commit)) {
      errors.push("contracts/sync.lock.json must contain a 40-char source commit SHA");
    }
    if (lock.openapi_sha256 !== actualSha256) {
      errors.push(
        `contracts/runtime-api/openapi.yaml checksum mismatch: lock=${lock.openapi_sha256}, actual=${actualSha256}`
      );
    }
  } catch (err) {
    errors.push(`Failed to parse contracts/sync.lock.json: ${err.message}`);
  }
}

// 6. Cap .github/workflows/*.yml count <= 5
const workflowsDir = path.join(root, ".github", "workflows");
if (fs.existsSync(workflowsDir)) {
  const wfFiles = fs
    .readdirSync(workflowsDir, { withFileTypes: true })
    .filter((d) => d.isFile() && (d.name.endsWith(".yml") || d.name.endsWith(".yaml")));
  if (wfFiles.length > 5) {
    errors.push(`.github/workflows has ${wfFiles.length} workflow files (limit: 5)`);
  }
}

// 7. Strict Zero-CJK scan outside UI i18n paths
const textFiles = walkFiles(root, (name) => {
  const ext = path.extname(name).toLowerCase();
  return TEXT_EXTENSIONS.has(ext) || name === ".gitattributes" || name === ".gitignore";
});

for (const file of textFiles) {
  const relUnix = path.relative(root, file).replace(/\\/g, "/");
  if (I18N_EXEMPT_REGEXES.some((re) => re.test(relUnix))) continue;
  const lines = fs.readFileSync(file, "utf8").split(/\r?\n/);
  for (let i = 0; i < lines.length; i += 1) {
    if (CJK_REGEX.test(lines[i])) {
      errors.push(`Non-i18n CJK text forbidden: ${relUnix}:${i + 1}`);
      break;
    }
  }
}

if (errors.length > 0) {
  console.error("Respire guardrail checks failed:");
  for (const err of errors) {
    console.error(`  - ${err}`);
  }
  process.exit(1);
}

console.log(`Respire guardrails passed (0 violations across ${textFiles.length} scanned files).`);
