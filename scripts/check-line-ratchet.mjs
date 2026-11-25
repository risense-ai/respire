#!/usr/bin/env node
// CARD-012: enforce exact, non-increasing ceilings for tracked source files.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const LIMIT = 600;
const ANCHOR = "b2036da4db08141e11f3c24347ceeed92aa41015";
const MANIFEST = "scripts/line-ratchet-baseline.json";
const TREE_LIMIT = 16 * 1024 * 1024;
const BLOB_LIMIT = 128 * 1024 * 1024;
const OID = /^[0-9a-f]{40}$/;
const SOURCE = /\.(?:rs|go|js|ts)$/i;
const decoder = new TextDecoder("utf-8", { fatal: true });

function git(root, args, { input, maxBuffer = TREE_LIMIT } = {}) {
  const process = spawnSync("git", args, { cwd: root, input, maxBuffer, windowsHide: true });
  assert.ok(!process.error && process.status === 0 && !process.signal,
    "Git could not prove " + args[0] + " (objects must be fetched)");
  return process.stdout;
}

export function countSourceLines(bytes) {
  const source = decoder.decode(bytes);
  assert.ok(!source.includes("\0") && !/\r(?!\n)/.test(source), "source is not LF or CRLF text");
  if (!source) return 0;
  let count = source.endsWith("\n") ? 0 : 1;
  for (let i = 0; i < source.length; i++) if (source[i] === "\n") count++;
  return count;
}

function validSourcePath(path) {
  return SOURCE.test(path) && !/[\\\x00-\x1f\x7f]/.test(path)
    && path.split("/").every(part => part !== "" && part !== "." && part !== "..");
}

function tracked(root, commit) {
  assert.match(commit, OID, "expected a full Git commit SHA");
  const entries = new Map();
  const raw = decoder.decode(git(root, ["ls-tree", "-rz", "--full-tree", commit]));
  assert.ok(raw === "" || raw.endsWith("\0"), "incomplete Git tree listing");
  for (const line of raw.split("\0").slice(0, -1)) {
    const match = /^([0-7]{6}) (\w+) ([0-9a-f]{40})\t([\s\S]+)$/.exec(line);
    assert.ok(match, "invalid Git tree entry");
    const [, mode, kind, oid, path] = match;
    if (!SOURCE.test(path) && path !== MANIFEST) continue;
    assert.ok((validSourcePath(path) || path === MANIFEST) && kind === "blob"
      && (mode === "100644" || mode === "100755"), "tracked source or baseline is not a regular Git file: " + JSON.stringify(path));
    assert.ok(!entries.has(path), "duplicate tracked path");
    entries.set(path, oid);
  }
  return entries;
}

function inspectBlobBatch(root, entries, consume) {
  const ids = Array.from(new Set(entries.values()));
  if (!ids.length) return;
  const byOid = new Map();
  for (const [path, id] of entries) {
    if (!byOid.has(id)) byOid.set(id, []);
    byOid.get(id).push(path);
  }
  const stdout = git(root, ["cat-file", "--batch"], { input: ids.join("\n") + "\n", maxBuffer: BLOB_LIMIT });
  let cursor = 0;
  for (const id of ids) {
    const end = stdout.indexOf(10, cursor);
    assert.ok(end >= cursor, "incomplete Git blob header");
    const header = new RegExp("^" + id + " blob ([0-9]+)$").exec(stdout.subarray(cursor, end).toString("ascii"));
    assert.ok(header, "expected tracked source blob " + id);
    const size = Number(header[1]);
    const start = end + 1;
    cursor = start + size;
    assert.ok(Number.isSafeInteger(size) && stdout[cursor] === 10, "incomplete Git blob body");
    const payload = stdout.subarray(start, cursor);
    for (const path of byOid.get(id)) consume(path, payload);
    cursor++;
  }
  assert.equal(cursor, stdout.length, "unexpected extra Git blob data");
}

function inventory(root, commit) {
  const ceilings = new Map();
  let baseline;
  const entries = tracked(root, commit);
  inspectBlobBatch(root, entries, (path, bytes) => {
    if (path === MANIFEST) {
      baseline = parseBaseline(bytes);
    } else {
      const count = countSourceLines(bytes);
      if (count > LIMIT) ceilings.set(path, count);
    }
  });
  return { ceilings, baseline, scanned: entries.size - Number(entries.has(MANIFEST)) };
}

function wire(map) {
  return JSON.stringify({ schema: 1, ceilings: Object.fromEntries(Array.from(map).sort(
    ([a], [b]) => a < b ? -1 : a > b ? 1 : 0)) }, null, 2) + "\n";
}

export function parseBaseline(bytes) {
  const text = decoder.decode(bytes);
  const obj = JSON.parse(text);
  assert.ok(obj?.schema === 1 && obj.ceilings && typeof obj.ceilings === "object"
    && !Array.isArray(obj.ceilings), "invalid line ratchet baseline");
  const map = new Map(Object.entries(obj.ceilings));
  for (const [path, count] of map) {
    assert.ok(validSourcePath(path) && Number.isSafeInteger(count) && count > LIMIT,
      "invalid baseline path/count: " + JSON.stringify(path));
  }
  assert.equal(text, wire(map), "baseline must use canonical sorted JSON (no duplicate entries)");
  return map;
}

export function ratchetViolations({ ceilings, baseline }, prior) {
  const out = [];
  for (const [path, count] of ceilings) {
    const max = prior.get(path);
    if (max === undefined) out.push(path + ": new or renamed file has " + count + " lines (limit " + LIMIT + ")");
    else if (count > max) out.push(path + ": " + count + " lines exceeds previous ceiling " + max);
    if (baseline.get(path) !== count) out.push(path + ": record its exact " + count + "-line ceiling (or split to " + LIMIT + ")");
  }
  for (const path of baseline.keys()) {
    if (!ceilings.has(path)) out.push(path + ": remove stale line exception");
  }
  return out;
}

export function checkLineRatchet(root, base, head) {
  assert.match(base, OID, "provide the trusted base commit SHA with --base");
  assert.match(head, OID, "provide the checked commit SHA");
  for (const id of new Set([base, head])) {
    assert.equal(decoder.decode(git(root, ["cat-file", "-t", id])).trim(), "commit", "expected a commit object");
  }
  git(root, ["merge-base", "--is-ancestor", base, head]);
  const previous = inventory(root, base);
  assert.ok(previous.baseline || base === ANCHOR, "base lacks a reviewed line ratchet baseline");
  // A malformed or stale base may not be used as authority to add exceptions.
  if (previous.baseline) assert.deepEqual(previous.baseline, previous.ceilings, "base baseline does not match base source");
  const current = inventory(root, head);
  assert.ok(current.baseline, "checked commit lacks the tracked line ratchet baseline");
  return { scanned: current.scanned, exceptions: current.baseline.size,
    violations: ratchetViolations(current, previous.baseline ?? previous.ceilings) };
}

function main(args) {
  assert.ok((args.length === 2 || args.length === 4) && args[0] === "--base"
    && (args.length !== 4 || args[2] === "--head"), "Usage: node scripts/check-line-ratchet.mjs --base <trusted SHA> [--head <SHA>]");
  const root = process.cwd();
  const head = args[3] ?? decoder.decode(git(root, ["rev-parse", "--verify", "HEAD"])).trim();
  const report = checkLineRatchet(root, args[1], head);
  if (report.violations.length) {
    console.error(report.violations.join("\n"));
    process.exitCode = 1;
  } else {
    console.log("Line ratchet passed: " + report.scanned + " tracked sources; "
      + report.exceptions + " exact, non-growing ceilings.");
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  try { main(process.argv.slice(2)); }
  catch (error) { console.error("Line ratchet failed: " + error.message); process.exitCode = 1; }
}
