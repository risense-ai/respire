import { isAbsolute, join, resolve } from "node:path";
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";

const root = resolve("contracts/runtime-control");

function fail(message) {
  throw new Error(message);
}

function readJSON(path) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    fail(`${path}: ${error.message}`);
  }
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function fixturePath(name) {
  if (typeof name !== "string" || name.length === 0 || isAbsolute(name)) {
    fail(`invalid fixture name: ${String(name)}`);
  }
  const normalized = name.replaceAll("\\", "/");
  if (normalized.includes("..")) {
    fail(`fixture escapes contract root: ${name}`);
  }
  return join(root, normalized);
}

const manifestPath = join(root, "manifest.json");
const manifest = readJSON(manifestPath);
if (manifest.contract !== "runtime-control") fail("wrong contract");
if (manifest.contract_version !== 1) fail("wrong contract version");
if (manifest.release_metadata_schema_version !== 2) fail("wrong release metadata schema");

const positive = manifest.fixtures?.positive;
const negative = manifest.fixtures?.negative;
if (!Array.isArray(positive) || positive.length !== 92) fail("positive fixture count");
if (!Array.isArray(negative) || negative.length !== 301) fail("negative fixture count");

const boundPaths = new Set();
for (const bound of manifest.files ?? []) {
  if (typeof bound.path !== "string" || bound.path.includes("..")) {
    fail("invalid bound path");
  }
  const path = fixturePath(bound.path);
  if (boundPaths.has(bound.path)) fail(`duplicate bound path: ${bound.path}`);
  boundPaths.add(bound.path);
  if (sha256(path) !== bound.sha256) fail(`digest drift: ${bound.path}`);
}

export { boundPaths, fail, fixturePath, manifest, negative, positive, readJSON, root };
