import assert from "node:assert/strict";
import {constants} from "node:fs";
import {copyFile, lstat, mkdir, mkdtemp, readFile, readdir, realpath, rm} from "node:fs/promises";
import {dirname, isAbsolute, join, resolve} from "node:path";
import {pathToFileURL} from "node:url";
import {expectedReleaseAssets, verifyLocalRelease} from "./component-release-plan.mjs";

const commitPattern = /^[a-f0-9]{40}$/;
const metadataMax = 2 << 20;
const archiveMax = 2 ** 31;

async function directory(path) {
  assert(typeof path === "string" && isAbsolute(path), "Expected an absolute build directory");
  const canonical = await realpath(path);
  assert.equal(canonical, resolve(path), "Build directory must not be redirected");
  assert((await lstat(canonical)).isDirectory(), "Expected a build directory");
  return canonical;
}

async function regularFile(path, limit) {
  const stat = await lstat(path);
  assert(stat.isFile() && !stat.isSymbolicLink() && stat.size > 0 && stat.size <= limit,
    "Build input must be a nonempty bounded regular file");
}

async function copyExclusive(source, destination) {
  await regularFile(source, archiveMax);
  await copyFile(source, destination, constants.COPYFILE_EXCL);
  await regularFile(destination, archiveMax);
}

export async function assembleLocalRelease({tag, source, batches, outputParent}) {
  const expected = expectedReleaseAssets(tag);
  assert(typeof source === "string" && source.length === 40 && commitPattern.test(source), "Expected exact source commit SHA");
  assert(Array.isArray(batches) && batches.length > 0 && batches.length <= 32, "Invalid batch count");
  const parent = await directory(outputParent);
  const expectedGitHub = new Set(expected.github);
  const expectedNpm = new Set(Object.values(expected.npm));
  const githubFiles = new Map();
  const npmFiles = new Map();
  const usedDirectories = new Set();
  // Detach verified native producer outputs from mutable working trees and
  // admit only allowlisted filenames associated with the exact invocation.
  for (const batch of batches) {
    assert(batch && Object.keys(batch).sort().join(",") === "directory,kind,source,tag" &&
      batch.source === source && batch.tag === tag, "Batch does not bind exact source and component tag");
    assert(batch.kind === "github" || batch.kind === "npm", "Unknown build batch kind");
    const root = await directory(batch.directory);
    assert(!usedDirectories.has(root), "Build batch directory repeated");
    usedDirectories.add(root);
    const expectedNames = batch.kind === "github" ? expectedGitHub : expectedNpm;
    const collected = batch.kind === "github" ? githubFiles : npmFiles;
    const entries = await readdir(root, {withFileTypes: true});
    assert(entries.length > 0 && entries.length <= expectedNames.size, "Invalid build batch file count");
    for (const entry of entries) {
      assert(entry.isFile() && !entry.isSymbolicLink() && expectedNames.has(entry.name) && !collected.has(entry.name),
        "Unexpected, repeated or nonregular build asset: " + entry.name);
      collected.set(entry.name, join(root, entry.name));
    }
  }
  assert.equal(githubFiles.size, expectedGitHub.size, "Missing GitHub release assets");
  assert.equal(npmFiles.size, expectedNpm.size, "Missing npm release packs");

  const root = await mkdtemp(join(parent, "component-release-dry-run-"));
  try {
    const monorepoDir = join(root, "monorepo");
    const legacyDir = join(root, "legacy");
    const npmDir = npmFiles.size ? join(root, "npm") : undefined;
    for (const dir of [monorepoDir, legacyDir, npmDir].filter(Boolean)) await mkdir(dir);
    // Mirror the exact staged output, never repackage or re-fetch it.
    for (const [name, from] of githubFiles) {
      const staged = join(monorepoDir, name);
      await copyExclusive(from, staged);
      await copyExclusive(staged, join(legacyDir, name));
    }
    for (const [name, from] of npmFiles) await copyExclusive(from, join(npmDir, name));
    const receipt = await verifyLocalRelease({tag, source, monorepoDir, legacyDir, npmDir});
    return {root, receipt};
  } catch (error) {
    assert.equal(dirname(root), parent, "Refusing cleanup outside owned build parent");
    assert.equal(await realpath(root), root, "Refusing cleanup of redirected build root");
    await rm(root, {recursive: true});
    throw error;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  assert(args.length === 4, "Usage: assemble-component-release.mjs <tag> <source-sha> <batch-plan.json> <output-parent>");
  const [tag, source, planPath, outputParent] = args;
  assert(isAbsolute(planPath) && await realpath(planPath) === resolve(planPath), "Expected an absolute unredirected batch plan");
  await regularFile(planPath, metadataMax);
  const batches = JSON.parse(await readFile(planPath, "utf8"));
  console.log(JSON.stringify(await assembleLocalRelease({tag, source, batches, outputParent})));
}
