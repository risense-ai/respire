// CARD-012: tracked UTF-8 is subject to Zero-CJK, including SVG and locale files.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const ALLOWANCES = "scripts/zero-cjk-reviewed-data.json";
const utf8 = new TextDecoder("utf-8", { fatal: true });
// Script (not Script_Extensions): a Latin middle dot is not CJK punctuation.
// Keep dedicated punctuation/compatibility blocks and supplementary Han ranges
// explicit, independent of the runner's assigned Unicode version.
const CJK = /[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Hangul}\p{Script=Bopomofo}\u2e80-\u2fff\u3000-\u30ff\u31c0-\u31ef\u3200-\u33ff\ufe10-\ufe1f\ufe30-\ufe4f\uff00-\uffef\u{20000}-\u{2a6df}\u{2a700}-\u{2ee5f}\u{2f800}-\u{2fa1f}\u{30000}-\u{3347f}]/u;
const sha = bytes => createHash("sha256").update(bytes).digest("hex");
const PNG = Buffer.from("89504e470d0a1a0a","hex");
const binaryFixtureSha = "969084b7a76f634a34897dc351f85bb0d0a78107e4eac19a11f5ee5bdb8a364f";
const binaryFixtures = new Set([
  "apps/core/crates/memory-core/tests/fixtures/legacy_embedding.f32le",
  "apps/runtime/crates/app/src/fixtures/legacy-memory-v1/legacy_embedding.f32le",
]);
const sqliteHeader = Buffer.from("SQLite format 3\0", "ascii");
const sqliteFixtures = new Map([
  ["test-cases/fixtures/db/comprehensive-synthetic.db", "33f83225b1441f6d813792add07ea3d5ab0d0748b2ae4f14363b1725f17e2e30"],
  ["test-cases/fixtures/db/respire-1x-synthetic.db", "a081b149521d3dd516fdda50bc744249293fb30990e3b98d1ee392189fe02c9d"],
]);

function validName(name) {
  return !/[\\\x00-\x1f\x7f]/.test(name)
    && name.split("/").every(part => part !== "" && part !== "." && part !== "..");
}

export function cjkProjection(text) {
  const hits = [];
  for (const [i, line] of text.split(/\r?\n/).entries()) {
    const match = CJK.exec(line);
    if (match) hits.push({ line: i + 1, source: line, point: match[0].codePointAt(0) });
  }
  return { hits, digest: sha(JSON.stringify(hits.map(hit => hit.source))) };
}

function binary(file, bytes) {
  if (binaryFixtures.has(file)) return bytes.length === 12 && sha(bytes) === binaryFixtureSha;
  const sqliteSha = sqliteFixtures.get(file);
  if (sqliteSha) {
    return bytes.length === 270336 && bytes.subarray(0, sqliteHeader.length).equals(sqliteHeader)
      && sha(bytes) === sqliteSha;
  }
  if (file.startsWith("apps/client2/internal/desktop/frontend/fonts/") && file.endsWith(".woff2")) {
    return bytes.length >= 48 && bytes.subarray(0, 4).toString("ascii") === "wOF2"
      && bytes.readUInt32BE(8) === bytes.length;
  }
  if (file.startsWith("apps/client2/internal/desktop/") && file.endsWith(".png")) {
    return bytes.length >= 45 && bytes.subarray(0, 8).equals(PNG)
      && bytes.subarray(12, 16).toString("ascii") === "IHDR"
      && bytes.subarray(-8, -4).toString("ascii") === "IEND";
  }
  return false;
}

export function scanCjk(files, reviewed) {
  const reports = [], used = new Set();
  let assets = 0, textFiles = 0;
  for (const [file, bytes] of files) {
    if (CJK.test(file)) reports.push(file + ": CJK in tracked filename");
    let text;
    if (!sqliteFixtures.has(file)) {
      try { text = utf8.decode(bytes); } catch { /* Only typed assets may be non-UTF-8. */ }
    }
    if (text === undefined || text.includes("\0")) {
      if (!binary(file, bytes)) reports.push(file + ": unclassified binary or malformed UTF-8");
      else assets++;
      continue;
    }
    textFiles++;
    const { hits, digest } = cjkProjection(text);
    if (hits.length === 0) continue;
    const pinned = reviewed.get(file);
    if (pinned && pinned.lines === hits.length && pinned.sha256 === digest) {
      used.add(file);
    } else {
      const code = hits[0].point.toString(16).toUpperCase().padStart(4, "0");
      reports.push(file + ":" + hits[0].line + ": unreviewed CJK text (U+" + code + ")");
    }
  }
  for (const file of reviewed.keys()) {
    if (!used.has(file)) reports.push(file + ": remove or re-review stale CJK data allowance");
  }
  return { reports, textFiles, assets, reviewed: used.size };
}

function parseReview(bytes) {
  const text = utf8.decode(bytes).replace(/\r\n/g, "\n");
  const obj = JSON.parse(text);
  assert.ok(obj?.schema === 1 && Array.isArray(obj.files) && Object.keys(obj).length === 2,
    "invalid Zero-CJK review data");
  assert.equal(text, JSON.stringify(obj, null, 2) + "\n", "review data must use canonical JSON");
  const reviewed = new Map();
  for (const entry of obj.files) {
    assert.ok(entry && Object.keys(entry).length === 4 && typeof entry.path === "string" && validName(entry.path)
      && !reviewed.has(entry.path) && Number.isSafeInteger(entry.lines) && entry.lines > 0
      && typeof entry.sha256 === "string" && /^[0-9a-f]{64}$/.test(entry.sha256)
      && typeof entry.reason === "string" && /^[a-z0-9 /-]+$/i.test(entry.reason),
    "invalid Zero-CJK review entry");
    reviewed.set(entry.path, entry);
  }
  return reviewed;
}

function tracked(root) {
  const result = spawnSync("git", ["ls-files", "--stage", "-z"], {
    cwd: root, maxBuffer: 16 * 1024 * 1024, timeout: 30_000, windowsHide: true,
  });
  assert.ok(!result.error && result.status === 0, "cannot enumerate tracked files for Zero-CJK");
  const raw = utf8.decode(result.stdout);
  assert.ok(raw.endsWith("\0"), "empty or incomplete tracked file list");
  return raw.split("\0").slice(0, -1).map(record => {
    const entry = /^(100644|100755) [0-9a-f]{40} 0\t([\s\S]+)$/.exec(record);
    assert.ok(entry && validName(entry[2]), "Zero-CJK requires regular, merged tracked files");
    return entry[2];
  });
}

export function checkZeroCjk(cwd) {
  const root = fs.realpathSync.native(cwd);
  const names = tracked(root);
  assert.ok(names.includes(ALLOWANCES), "missing tracked Zero-CJK review manifest");
  const read = file => {
    const resolved = path.join(root, file);
    assert.ok(fs.lstatSync(resolved).isFile() && fs.realpathSync.native(resolved) === resolved,
      "unsafe tracked Zero-CJK path: " + file);
    return fs.readFileSync(resolved);
  };
  const reviews = parseReview(read(ALLOWANCES));
  const files = (function* () {
    for (const name of names) yield [name, read(name)];
  })();
  return scanCjk(files, reviews);
}
