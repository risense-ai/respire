#!/usr/bin/env node
// CARD-012: check actual Core Cargo edges, not lockfile membership.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const TARGETS = [
  "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu",
  "x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc",
  "x86_64-apple-darwin", "aarch64-apple-darwin",
];
const BAD = /^(?:rusqlite|libsqlite3-sys|sqlite(?:3)?|sqlx(?:-.+)?|reqwest|ureq(?:-proto)?|axum(?:-.+)?|tiny[-_]http|hyper(?:-.+)?|curl(?:-sys)?|isahc|surf|actix-web|tokio-postgres|postgres|keyring|secret-service)$/;
const SOURCE = "registry+https://github.com/rust-lang/crates.io-index";
const PINS = new Map([
  ["ort", ["2.0.0-rc.13", "4336a1e2b38848325241c72889086886004e589b7c74f335e60a8e8db5138a0b"]],
  ["ort-sys", ["2.0.0-rc.13", "cf211e3776eea6aec988552fa118dd746d70e1b1e5e244058d1c98015f3e5872"]],
  ["ureq", ["3.4.2", "9a7ac20be9b7726e0bbdbf974c059676d9acb1cd414961f570a4e8231cacd7fc"]],
  ["ureq-proto", ["0.6.4", "f86fd172ccca569e458f61b6bdd6220965a9ef36e672a6852953b51a0e1583be"]],
]);
const UREQ_FEATURES = "_tls,native-tls,native-tls-no-default,native-tls-webpki-roots,socks-proxy";
const APPROVED_CORE = new Set(["respire_core", "respire_core_ffi"]);
const PKG = /^((?:\|   |    )*)(\|-- |\x60-- )?PKG=([A-Za-z0-9_-]+) v([^ ()|]+)(?: \((.*)\))?\|FEAT=([A-Za-z0-9_,./?+-]*)(?: (\(\*\)))?$/;
const SECTION = /^((?:\|   |    )*)\[(build|dev)-dependencies\]$/;
const MAX = 16 * 1024 * 1024;

function cargo(args, root) {
  const result = spawnSync("cargo", args, {
    cwd: root, encoding: "utf8", maxBuffer: MAX, timeout: 120_000, windowsHide: true,
  });
  if (result.error) throw result.error;
  assert.ok(result.status === 0, "Cargo could not resolve the locked dependency graph"
    + (args.includes("--offline") ? " (--offline requires the exact dependency cache)" : ""));
  return result.stdout;
}

function lockEntries(text) {
  const entries = new Map();
  for (const section of text.split(/^\[\[package\]\]\r?$/m).slice(1)) {
    const keys = {};
    for (const match of section.matchAll(/^(name|version|source|checksum) = "([^"]+)"\r?$/gm)) {
      assert.ok(!(match[1] in keys), "unexpected duplicate field in Cargo.lock");
      keys[match[1]] = match[2];
    }
    const key = keys.name + "@" + keys.version + "@" + keys.source;
    assert.ok(!entries.has(key), "duplicate package in Cargo.lock");
    entries.set(key, keys.checksum);
  }
  return entries;
}

export function parseCargoTree(output) {
  const stack = [], rows = [];
  for (const line of output.split(/\r?\n/).filter(Boolean)) {
    const section = SECTION.exec(line);
    if (section) {
      const depth = section[1].length / 4;
      assert.ok(stack[depth], "dependency section without parent");
      stack.length = depth + 1;
      stack[depth].section = section[2];
      continue;
    }
    const entry = PKG.exec(line);
    assert.ok(entry && (!entry[2] || entry[2] === "|-- " || entry[2] === String.fromCharCode(96) + "-- ")
      && !entry[7], "unrecognized or deduplicated Cargo tree row");
    const depth = entry[1].length / 4 + Number(Boolean(entry[2]));
    assert.ok(entry[2] || depth === 0, "indented Cargo root");
    assert.ok(depth <= stack.length, "Cargo tree missing parent");
    stack.length = depth;
    const parent = stack.at(-1);
    const row = { name: entry[3], version: entry[4], sourceDetail: entry[5],
      features: entry[6].split(",").filter(Boolean).sort().join(","),
      kind: parent?.section ?? "normal", parent: parent?.row };
    stack.push({row,section:null});
    rows.push(row);
  }
  assert.ok(rows.length, "empty active Cargo tree");
  return rows;
}

function isRegistryPin(row, lock) {
  const info = PINS.get(row.name);
  return info && row.version === info[0] && row.sourceDetail === undefined
    && lock.get(row.name + "@" + row.version + "@" + SOURCE) === info[1];
}

function isReviewedOrtChain(row, lock) {
  if (!row || row.name !== "ort-sys" || row.kind !== "normal" || !isRegistryPin(row, lock)
    || !row.features.split(",").includes("download-binaries")) return false;
  let parent = row.parent;
  if (parent?.name === "ort") {
    if (parent.kind !== "normal" || !isRegistryPin(parent, lock)
      || parent.features.split(",").includes("fetch-models")) return false;
    parent = parent.parent;
  }
  if (parent?.name !== "respire_core") return false;
  if (parent.parent?.name === "respire_core_ffi" && parent.kind === "normal") parent = parent.parent;
  return APPROVED_CORE.has(parent.name) && !parent.parent;
}

function buildAcquisition(row, lock) {
  if (!isRegistryPin(row, lock)) return false;
  if (row.name === "ureq") {
    return row.kind === "build" && row.features === UREQ_FEATURES
      && isReviewedOrtChain(row.parent, lock);
  }
  if (row.name === "ureq-proto") {
    return row.kind === "normal" && row.features === "client" && row.parent?.name === "ureq"
      && buildAcquisition(row.parent, lock);
  }
  return false;
}

export function checkActiveEdges(rows, lock, target) {
  const diagnostics = [];
  let allowed = 0;
  for (const row of rows) {
    if (row.name === "ort" && row.features.split(",").includes("fetch-models")) {
      diagnostics.push(target + ": ort enables prohibited runtime fetch-models");
    }
    if (!BAD.test(row.name)) continue;
    if (buildAcquisition(row, lock)) { allowed++; continue; }
    const via = [], context = new Set();
    for (let node=row;node;node=node.parent) {
      via.unshift(node.name + "@" + node.version);
      if (node !== row && node.kind !== "normal") context.add(node.kind);
    }
    diagnostics.push(target + ": forbidden " + row.kind + " dependency " + via.join(" -> ")
      + (context.size ? " (via " + [...context].join("/") + ")" : ""));
  }
  return { diagnostics: Array.from(new Set(diagnostics)), allowed };
}

export function checkDeclaredDependencies(metadata) {
  assert.ok(Array.isArray(metadata.workspace_members) && metadata.workspace_members.length > 0
    && Array.isArray(metadata.packages), "Cargo metadata did not describe the workspace");
  const packages = new Map(metadata.packages.map(p=>[p.id,p]));
  const reports = [];
  for (const id of metadata.workspace_members) {
    const member = packages.get(id);
    assert.ok(member && Array.isArray(member.dependencies), "workspace dependency metadata is incomplete");
    for (const dep of member.dependencies) {
      assert.equal(typeof dep.name, "string", "Cargo dependency lacks a package name");
      if (BAD.test(dep.name) || (typeof dep.rename === "string" && BAD.test(dep.rename))) {
        reports.push(member.name + ": forbidden declared " + (dep.kind ?? "normal")
          + " dependency " + dep.name + (dep.rename ? " (alias " + dep.rename + ")" : ""));
      }
    }
  }
  return reports;
}

export function checkCoreDependencies({root, manifest, targets=TARGETS, offline=false, defaultOnly=false}) {
  assert.ok(Array.isArray(targets) && targets.length > 0 && targets.every(t=>TARGETS.includes(t)),
    "missing or unsupported target matrix");
  const common = ["--locked", "--manifest-path", manifest, ...(offline ? ["--offline"] : [])];
  const metadata = JSON.parse(cargo(["metadata", "--format-version", "1", "--no-deps", ...common], root));
  const reports = checkDeclaredDependencies(metadata);
  if (reports.length) return { reports, targets: 0, allowed: 0 };
  const members = new Set(metadata.packages.filter(p=>metadata.workspace_members.includes(p.id)).map(p=>p.name));
  const lock = lockEntries(fs.readFileSync(path.join(path.dirname(manifest), "Cargo.lock"), "utf8"));
  let allowed = 0;
  for (const target of targets) {
    const flags = ["tree", ...common, "--workspace", "--target", target, ...(defaultOnly ? [] : ["--all-features"]),
      "--edges", "normal,build,dev", "--no-dedupe", "--charset", "ascii", "--color", "never", "--format", "PKG={p}|FEAT={f}"];
    let rows;
    try { rows = parseCargoTree(cargo(flags, root)); }
    catch (error) { throw new Error(target + ": " + error.message); }
    const roots = rows.filter(row=>!row.parent);
    assert.ok(roots.length === members.size && new Set(roots.map(row=>row.name)).size === members.size
      && roots.every(row=>members.has(row.name)), "missing active graph for a workspace member on " + target);
    const graph = checkActiveEdges(rows, lock, target);
    reports.push(...graph.diagnostics);
    allowed += graph.allowed;
  }
  return { reports, targets: targets.length, allowed };
}

function main(args) {
  assert.ok(args.length <= 1 && (args.length === 0 || args[0] === "--offline"),
    "Usage: node scripts/check-core-dependencies.mjs [--offline]");
  const root = process.cwd();
  const result = checkCoreDependencies({root, manifest: path.join(root,"apps/core/Cargo.toml"), offline: args[0] === "--offline"});
  if (result.reports.length) {
    console.error(result.reports.join("\n"));
    process.exitCode = 1;
    return;
  }
  console.log("Core dependency guard passed: " + result.targets
    + " target graphs; " + result.allowed + " reviewed build-path occurrences.");
}

if (process.argv[1] && fileURLToPath(import.meta.url) === path.resolve(process.argv[1])) {
  try { main(process.argv.slice(2)); }
  catch (error) { console.error("Core dependency guard failed: " + error.message); process.exitCode = 1; }
}
