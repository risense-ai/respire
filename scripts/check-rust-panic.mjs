#!/usr/bin/env node
// Compare syntax sites from exact Git objects; never execute application code or hooks.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { isAbsolute, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const OID = /^[a-f0-9]{40}$/;
const UTF8 = new TextDecoder("utf-8", { fatal: true });
const MAX = 16 * 1024 * 1024;

function command(cwd, executable, args, input, label) {
  const result = spawnSync(executable, args, { cwd, input, maxBuffer: MAX, windowsHide: true, timeout: 20_000 });
  assert.ok(!result.error && result.status === 0 && !result.signal, label);
  return result.stdout;
}

function git(root, ...args) {
  return command(root, "git", ["--literal-pathspecs", ...args], undefined, "Git could not verify " + args[0]);
}

function pathList(bytes) {
  const raw = UTF8.decode(bytes);
  assert.ok(!raw || raw.endsWith("\0"), "invalid Git path list");
  return raw.split("\0").slice(0, -1).filter(path => /\.rs$/i.test(path));
}

function fileBlob(root, commit, path) {
  const raw = UTF8.decode(git(root, "ls-tree", "-z", "--full-tree", commit, "--", path));
  if (raw === "") return null;
  const entry = /^([0-7]{6}) blob ([0-9a-f]{40})\t([^\0]+)\0$/.exec(raw);
  assert.ok(entry && entry[3] === path && ["100644", "100755"].includes(entry[1]),
    "expected tracked regular Rust source: " + JSON.stringify(path));
  return git(root, "cat-file", "blob", entry[2]);
}

function scan(root, parser, bytes, path) {
  if (bytes === null) return [];
  const out = UTF8.decode(command(root, parser, [], bytes, "Rust source analysis failed: " + JSON.stringify(path)));
  if (!out) return [];
  assert.ok(out.endsWith("\n"), "incomplete parser response");
  return out.replace(/\r\n/g, "\n").split("\n").slice(0, -1).map(record => {
    const match = /^([1-9][0-9]*)\t([0-9]+)\t(unwrap|expect)\t([0-9a-f]{16})$/.exec(record);
    assert.ok(match, "malformed parser response");
    return { line: Number(match[1]), column: Number(match[2]), method: match[3], shape: match[4] };
  });
}

export function diffHunks(diff) {
  return diff.split("\n").filter(line => line.startsWith("@@")).map(line => {
    const m = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/.exec(line);
    assert.ok(m, "cannot project Git diff line positions");
    const oldCount = Number(m[2] ?? 1), newCount = Number(m[4] ?? 1);
    // A zero-length side points to the preceding line.
    return { oldLine: Number(m[1]) + Number(oldCount === 0), oldCount,
      newLine: Number(m[3]) + Number(newCount === 0), newCount };
  });
}

function projection(line, hunks) {
  let added = 0;
  for (const h of hunks) {
    if (line < h.newLine) break;
    if (line < h.newLine + h.newCount) return { hunk: h };
    added += h.newCount - h.oldCount;
  }
  return { oldLine: line - added };
}

export function newSites(old, current, hunks) {
  const unmatched = new Set(current);
  const used = new Set();
  // Match unchanged identifier positions first. Requiring the old AST to have a
  // site prevents a newly added call or removal of an enclosing test gate from
  // slipping through just because the method identifier's line did not move.
  for (const site of current) {
    const {oldLine} = projection(site.line, hunks);
    if (oldLine === undefined) continue;
    const prior = old.find(x => !used.has(x) && x.line === oldLine
      && x.column === site.column && x.method === site.method && x.shape === site.shape);
    if (prior) { used.add(prior); unmatched.delete(site); }
  }
  // Changed source lines retain prior one-to-one calls only within that same
  // hunk and with the same syntax shape. A moved or duplicated site gets no
  // exemption from its name alone, the overall tally, or another file.
  for (const site of unmatched) {
    const {hunk} = projection(site.line, hunks);
    if (!hunk) continue;
    const prior = old.find(x => !used.has(x) && x.line >= hunk.oldLine
      && x.line < hunk.oldLine + hunk.oldCount && x.method === site.method && x.shape === site.shape);
    if (prior) { used.add(prior); unmatched.delete(site); }
  }
  return [...unmatched];
}

export function checkRustPanic(root, base, head, parser) {
  assert.match(base, OID, "trusted base full SHA required");
  assert.match(head, OID, "checked head full SHA required");
  assert.ok(isAbsolute(parser), "supply absolute path to built syntax checker");
  for (const oid of new Set([base, head])) {
    assert.equal(UTF8.decode(git(root, "cat-file", "-t", oid)).trim(), "commit", "expected commit object");
  }
  git(root, "merge-base", "--is-ancestor", base, head);
  const changed = pathList(git(root, "diff", "--name-only", "-z", "--diff-filter=AMT",
    "--no-renames", "--no-ext-diff", "--no-textconv", base, head, "--"));
  const violations = [];
  for (const path of changed) {
    const previous = fileBlob(root, base, path), current = fileBlob(root, head, path);
    assert.ok(current !== null, "current Rust source missing: " + JSON.stringify(path));
    const hunks = diffHunks(UTF8.decode(git(root, "diff", "--no-renames", "--no-ext-diff",
      "--no-textconv", "--text", "--diff-algorithm=myers", "--no-indent-heuristic",
      "--unified=0", base, head, "--", path)));
    const additions = newSites(scan(root, parser, previous, path), scan(root, parser, current, path), hunks);
    for (const site of additions) violations.push(path + ":" + site.line + ":" + (site.column + 1)
      + ": new production " + site.method + " site");
  }
  return { checked: changed.length, violations };
}

function main(args) {
  assert.ok(args.length === 6 && args[0] === "--base" && args[2] === "--head" && args[4] === "--parser",
    "Usage: node scripts/check-rust-panic.mjs --base <trusted SHA> --head <checked SHA> --parser <absolute executable>");
  const {checked, violations} = checkRustPanic(process.cwd(), args[1], args[3], args[5]);
  if (violations.length) {
    for (const violation of violations) console.error(violation);
    process.exitCode = 1;
  } else console.log("Production Rust panic-site guard passed for " + checked + " changed Rust files.");
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  try { main(process.argv.slice(2)); }
  catch (error) { console.error("Rust panic-site guard failed: " + error.message); process.exitCode = 1; }
}
