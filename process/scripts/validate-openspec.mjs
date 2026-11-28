#!/usr/bin/env node
// Zero-dependency OpenSpec, OpenAPI 3.1, relative link, line-cap, and Zero-CJK validator.

import fs from "node:fs";
import path from "node:path";

const root = process.cwd();
const errors = [];
const governanceDirs = ["openspec", "contracts", "prototypes", "test-cases", "process"];
const governanceDocs = ["AGENTS.md", "LESSONS.md", "README.md"];

const CJK_REGEX = /[\u3000-\u303F\u3400-\u4DBF\u4E00-\u9FFF\uF900-\uFAFF\uFF00-\uFFEF]/;
const I18N_EXEMPT_REGEXES = [
  /(^|\/)i18n(\/|\.js$|\.ts$|\.json$)/,
  /(^|\/)locales\//,
  /_zh(-CN|-TW)?\.(json|js|ts)$/,
  /\.zh(-CN|-TW)?\.(json|js|ts)$/,
  /^test-cases\/fixtures\/i18n\//,
];
const TEXT_EXTENSIONS = new Set([
  ".md", ".yml", ".yaml", ".mjs", ".js", ".ts", ".json",
  ".html", ".css", ".sh", ".ps1", ".txt", ".toml", ".rs", ".go",
]);

function checkExists(relPath) {
  const full = path.join(root, relPath);
  if (!fs.existsSync(full)) {
    errors.push(`Missing required file: ${relPath}`);
    return null;
  }
  return full;
}

function walkFiles(dir, predicate, acc = [], skipArchive = false) {
  if (!fs.existsSync(dir)) return acc;
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === ".git" || entry.name === "node_modules" || entry.name === ".worktrees") continue;
    if (skipArchive && entry.name === "archive") continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      walkFiles(full, predicate, acc, skipArchive);
    } else if (entry.isFile() && predicate(entry.name, full)) {
      acc.push(full);
    }
  }
  return acc;
}

function isExemptI18nPath(relUnix) {
  return I18N_EXEMPT_REGEXES.some((re) => re.test(relUnix));
}

function governanceFiles(predicate, skipArchive = false) {
  const docs = governanceDocs.filter(predicate).map((name) => path.join(root, name))
    .filter((file) => fs.existsSync(file));
  return governanceDirs.flatMap((dir) => walkFiles(path.join(root, dir), predicate, [], skipArchive))
    .concat(docs);
}

function stripCodeBlocksAndSpans(mdText) {
  const fences = /^ {0,3}((`|~)\2{2,})(?!\2)[^\r\n]*\r?\n[\s\S]*?(?:^ {0,3}\1\2*[ \t]*\r?$|(?![\s\S]))/gm;
  return mdText.replace(fences, "\n").replace(/(?<!`)(`+)(?!`)[\s\S]*?(?<!`)\1(?!`)/g, " ");
}

// 1. Core governance files
checkExists("AGENTS.md");
checkExists("LESSONS.md");
checkExists("README.md");
checkExists("openspec/config.yaml");
checkExists("openspec/ROADMAP.md");

// 2. OpenSpec Changes validation
const changesDir = path.join(root, "openspec", "changes");
let changeCount = 0;

if (!fs.existsSync(changesDir)) {
  errors.push("Missing directory: openspec/changes");
} else {
  const entries = fs
    .readdirSync(changesDir, { withFileTypes: true })
    .filter((d) => d.isDirectory() && d.name !== "archive");

  if (entries.length === 0) errors.push("No active Change directories found under openspec/changes");

  for (const dir of entries) {
    changeCount += 1;
    const changeRel = path.join("openspec", "changes", dir.name);
    for (const reqFile of ["proposal.md", "design.md", "tasks.md"]) {
      checkExists(path.join(changeRel, reqFile));
    }

    const specsRoot = path.join(root, changeRel, "specs");
    const specFiles = [];
    if (fs.existsSync(specsRoot)) {
      for (const domain of fs.readdirSync(specsRoot, { withFileTypes: true })) {
        if (domain.isDirectory()) {
          const candidate = path.join(specsRoot, domain.name, "spec.md");
          if (fs.existsSync(candidate)) specFiles.push(candidate);
        }
      }
    }

    if (specFiles.length === 0) {
      errors.push(`${changeRel} is missing at least one specs/<domain>/spec.md`);
      continue;
    }

    for (const sf of specFiles) {
      const rel = path.relative(root, sf);
      const text = fs.readFileSync(sf, "utf8");
      if (!/^##\s+(ADDED|MODIFIED|REMOVED)\s+Requirements/m.test(text)) {
        errors.push(`${rel} is missing a Delta heading (## ADDED/MODIFIED/REMOVED Requirements)`);
      }
      if (!/\b(MUST|SHALL)\b/.test(text)) {
        errors.push(`${rel} is missing RFC 2119 requirement keyword (MUST or SHALL)`);
      }
      if (!/^####\s+Scenario:/m.test(text)) {
        errors.push(`${rel} is missing at least one #### Scenario: block`);
      }
      if (!/\*\*GIVEN\*\*|\bGIVEN\b/.test(text) || !/\*\*WHEN\*\*|\bWHEN\b/.test(text) || !/\*\*THEN\*\*|\bTHEN\b/.test(text)) {
        errors.push(`${rel} scenario blocks must include GIVEN / WHEN / THEN`);
      }
    }
  }
}

// 3. OpenAPI 3.1 baseline check
const openapiPath = checkExists("contracts/runtime-api/openapi.yaml");
if (openapiPath) {
  const apiText = fs.readFileSync(openapiPath, "utf8");
  if (!/^openapi:\s*3\.1\.0\b/m.test(apiText)) errors.push("contracts/runtime-api/openapi.yaml must declare openapi: 3.1.0");
  if (!/^info:/m.test(apiText)) errors.push("contracts/runtime-api/openapi.yaml is missing info: section");
  if (!/^paths:/m.test(apiText)) errors.push("contracts/runtime-api/openapi.yaml is missing paths: section");
}

// 4. Non-archive OpenSpec Markdown <= 400 lines
const mdFiles = walkFiles(path.join(root, "openspec"), (n) => n.endsWith(".md"), [], true);
for (const md of mdFiles) {
  const lines = fs.readFileSync(md, "utf8").split(/\r?\n/).length;
  if (lines > 400) errors.push(`${path.relative(root, md)} has ${lines} lines (limit: 400)`);
}

// 5. Strict Zero-CJK scan outside UI i18n paths
const textFiles = governanceFiles((name) => TEXT_EXTENSIONS.has(path.extname(name).toLowerCase()) || name === ".gitattributes" || name === ".gitignore");
for (const file of textFiles) {
  const relUnix = path.relative(root, file).replace(/\\/g, "/");
  if (isExemptI18nPath(relUnix)) continue;
  const lines = fs.readFileSync(file, "utf8").split(/\r?\n/);
  for (let i = 0; i < lines.length; i += 1) {
    if (CJK_REGEX.test(lines[i])) {
      errors.push(`Non-i18n CJK text forbidden: ${relUnix}:${i + 1}`);
      break;
    }
  }
}

// 6. Markdown relative links in governance docs may point anywhere in the monorepo.
const allNonArchiveMd = governanceFiles((name) => /\.md$/i.test(name), true);
const linkRe = /\[[^\]\r\n]*\]\(\s*(?:<([^>\r\n]+)>|((?:[^\s()]|\([^()\r\n]*\))+))(?:[ \t]+(?:"[^"\r\n]*"|'[^'\r\n]*'|\([^()\r\n]*\)))?\s*\)/g;
for (const mdFile of allNonArchiveMd) {
  const relDoc = path.relative(root, mdFile).replace(/\\/g, "/");
  const stripped = stripCodeBlocksAndSpans(fs.readFileSync(mdFile, "utf8"));
  for (const match of stripped.matchAll(linkRe)) {
    const rawTarget = match[1] ?? match[2];
    if (/^(?:[a-z][a-z\d+.-]*:|\/\/)/i.test(rawTarget)) continue;
    const cleanTarget = rawTarget.split(/[?#]/, 1)[0];
    if (!cleanTarget) continue;
    try {
      const target = decodeURIComponent(cleanTarget);
      const resolved = target.startsWith("/") ? path.resolve(root, `.${target}`) : path.resolve(path.dirname(mdFile), target);
      if (fs.existsSync(resolved)) continue;
    } catch (error) {
      if (!(error instanceof URIError)) throw error;
    }
    errors.push(`Broken Markdown link in ${relDoc}: (${rawTarget})`);
  }
}

// 7. Governance scripts <= 600 lines
const scriptFiles = walkFiles(path.join(root, "process", "scripts"), (n) => n.endsWith(".mjs") || n.endsWith(".js"));
for (const sf of scriptFiles) {
  const lines = fs.readFileSync(sf, "utf8").split(/\r?\n/).length;
  if (lines > 600) errors.push(`${path.relative(root, sf)} has ${lines} lines (limit: 600)`);
}

if (errors.length > 0) {
  console.error("OpenSpec validation failed:");
  for (const err of errors) console.error(`  - ${err}`);
  process.exit(1);
}

console.log(
  `OpenSpec validation passed: ${changeCount} Change packages, ${mdFiles.length} OpenSpec Markdown files (<= 400 lines), ${allNonArchiveMd.length} docs with valid relative links, ${scriptFiles.length} scripts (<= 600 lines), OpenAPI 3.1 valid, and 0 non-i18n CJK violations across ${textFiles.length} scanned files.`
);
