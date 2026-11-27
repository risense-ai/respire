import assert from "node:assert/strict";
import {isDeepStrictEqual} from "node:util";
import {createHash} from "node:crypto";
import {open, readdir, realpath} from "node:fs/promises";
import {join, resolve} from "node:path";
import {pathToFileURL} from "node:url";

const triples = [
  "x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl", "aarch64-unknown-linux-gnu",
  "aarch64-unknown-linux-musl", "x86_64-apple-darwin", "aarch64-apple-darwin",
  "x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc",
];
// GitHub returned HTTP 404 for the approved Microsoft ORT source gitlink lookups.
// Both Runtime musl targets are excluded; CLI retains independent native builds.
const runtimeExclusions = new Set(["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"]);
const runtimeTriples = triples.filter(triple => !runtimeExclusions.has(triple));
const npmPackages = ["rsrs-cli", ...[
  "linux-x64-gnu", "linux-x64", "linux-arm64-gnu", "linux-arm64", "macos-x64", "macos-arm64",
  "win-x64", "win-arm64",
].map(name => "@rsrsai/" + name)];
const desktopUpdates = Object.entries({windows: "exe", linux: "tar.gz", darwin: "zip"})
  .flatMap(([platform, extension]) => ["amd64", "arm64"].map(arch => "respire-" + platform + "-" + arch + "." + extension));
const desktopChecksums = [...desktopUpdates, "respire-windows-amd64-setup.exe",
  "respire-windows-arm64-setup.exe", "respire-darwin-arm64.dmg"];
const hex = /^[0-9a-f]{64}$/;
const commit = /^[0-9a-f]{40}$/;

// Accept only the stable/dev version forms that the shipping updaters know.
// Outer monorepo tags differ; embedded artifact metadata keeps its legacy tag.
export function parseComponentTag(tag) {
  const match = typeof tag === "string" && /^(runtime|cli|client2)-v((0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-dev\.(0|[1-9][0-9]*))?)$/.exec(tag);
  assert(match && match[0] === tag && match.slice(3).filter(Boolean).every(n => Number.isSafeInteger(Number(n))),
    "Expected an exact component tag with supported stable or dev SemVer");
  const [, component, version] = match;
  return Object.freeze({component, version, tag, channel: match[6] === undefined ? "stable" : "dev",
    monorepo: "risense-ai/respire", legacy: "risense-ai/respire-" + component, legacyTag: "v" + version});
}

function executable(prefix, triple) {
  return prefix + "-" + triple + (triple.endsWith("-windows-msvc") ? ".exe" : "");
}

export function expectedReleaseAssets(tag) {
  const {component, version} = parseComponentTag(tag);
  const github = component === "runtime"
    ? runtimeTriples.flatMap(triple => [executable("rsrs-runtime", triple), "runtime-build-" + triple + ".json",
      "rsrs-runtime-" + triple + ".tar.gz"])
    : component === "cli"
      ? triples.flatMap(triple => [executable("rsrs", triple), "cli-build-" + triple + ".json"])
      : [...desktopChecksums, "SHA256SUMS", "client-updates.json", "release-notes.md", "LICENSE.txt", "THIRD_PARTY_NOTICES.txt"];
  const npm = Object.fromEntries((component === "cli" ? npmPackages : [])
    .map(pkg => [pkg, pkg.replace(/^@/, "").replace("/", "-") + "-" + version + ".tgz"]));
  return {github: github.sort(), npm};
}

// Called with the in-memory result of byte verification in the trusted checkout.
export function localFanoutPlan(receipt) {
  const plan = parseComponentTag(receipt.tag);
  assert.equal(receipt.mode, "dry-run");
  assert.equal(receipt.monorepo, plan.monorepo);
  assert.equal(receipt.legacy, plan.legacy);
  assert.equal(receipt.legacyTag, plan.legacyTag);
  const expected = expectedReleaseAssets(plan.tag);
  assert.deepEqual(Object.keys(receipt.assets).sort(), expected.github);
  assert.deepEqual(Object.keys(receipt.npm).sort(), Object.values(expected.npm).sort());
  function asset(name, inventory) {
    const {size, sha256} = inventory[name];
    assert(Number.isSafeInteger(size) && size > 0 && typeof sha256 === "string" &&
      sha256.length === 64 && hex.test(sha256), "Invalid verified asset");
    return {name, size, sha256};
  }
  const assets = expected.github.map(name => asset(name, receipt.assets));
  return {
    mode: "verification-only",
    github: [
      {repository: plan.monorepo, tag: plan.tag, assets},
      {repository: plan.legacy, tag: plan.legacyTag, assets},
    ],
    npm: Object.entries(expected.npm).map(([packageName, filename]) => ({
      package: packageName, version: plan.version, tarball: asset(filename, receipt.npm),
    })),
  };
}

async function digestFile(root, name) {
  const file = await open(join(root, name), "r");
  try {
    const stat = await file.stat();
    assert(stat.isFile() && stat.size > 0 && stat.size <= 2 ** 31, "Invalid local release file size: " + name);
    let size = 0;
    const hash = createHash("sha256");
    // Keep smaller metadata bytes for parsing the exact content we hashed.
    const isMetadata = /(?:\.json|SHA256SUMS)$/.test(name);
    if (isMetadata && stat.size > 2 ** 21) throw new Error("RELEASE_METADATA_SIZE");
    const chunks = isMetadata ? [] : null;
    for await (const bytes of file.createReadStream({autoClose: false})) {
      size += bytes.length;
      assert(size <= stat.size, "Release file grew during inspection: " + name);
      hash.update(bytes);
      if (chunks) chunks.push(bytes);
    }
    assert.equal(size, stat.size, "Release file shrank during inspection: " + name);
    return {size, sha256: hash.digest("hex"), text: chunks ? new TextDecoder("utf-8", {fatal: true}).decode(Buffer.concat(chunks)) : undefined};
  } finally {
    await file.close();
  }
}

async function examine(directory, expected) {
  assert.equal(await realpath(directory), resolve(directory), "Release directory must not redirect elsewhere");
  const entries = await readdir(directory, {withFileTypes: true});
  assert(entries.every(entry => entry.isFile() && !entry.isSymbolicLink()), "Release directory must contain only regular files");
  assert(isDeepStrictEqual(entries.map(entry => entry.name).sort(), [...expected].sort()), "Incomplete or unexpected local release asset set");
  const assets = {};
  // Limit memory/FD pressure across the eight native targets.
  for (const name of expected) assets[name] = await digestFile(directory, name);
  return assets;
}

function metadata(assets, name) {
  assert(assets[name]?.text, "Missing or oversized release metadata: " + name);
  try {
    return JSON.parse(assets[name].text);
  } catch {
    throw new Error("RELEASE_METADATA_JSON");
  }
}

function checkNative(plan, sha, assets) {
  for (const target of plan.component === "runtime" ? runtimeTriples : triples) {
    if (plan.component === "runtime") {
      const archive = "rsrs-runtime-" + target + ".tar.gz";
      const binary = executable("rsrs-runtime", target);
      const info = metadata(assets, "runtime-build-" + target + ".json");
      assert(info.schema_version === 2 && info.runtime_version === plan.version && info.tag === plan.legacyTag &&
        info.channel === plan.channel && info.target_triple === target && info.source_commit === sha &&
        info.archive_file === archive && info.archive_sha256 === assets[archive].sha256 &&
        info.binary_file === binary && info.binary_sha256 === assets[binary].sha256,
      "Runtime metadata identity/checksum mismatch: " + target);
    } else {
      const binary = executable("rsrs", target);
      const info = metadata(assets, "cli-build-" + target + ".json");
      assert(info.schema_version === 2 && info.artifact_layout === "cli-frontend-v2" && info.version === plan.version &&
        info.target === target && info.git_sha === sha && info.binary_file === binary &&
        info.binary_size === assets[binary].size && info.binary_sha256 === assets[binary].sha256,
      "CLI metadata identity/checksum mismatch: " + target);
    }
  }
}

function checkDesktop(plan, assets) {
  const sums = assets.SHA256SUMS.text?.trim().split(/\r?\n/) ?? [];
  const names = new Set();
  for (const line of sums) {
    const pair = /^([0-9a-f]{64}) [ *]([a-z0-9.-]+)$/.exec(line);
    assert(pair && desktopChecksums.includes(pair[2]) && !names.has(pair[2]) && assets[pair[2]].sha256 === pair[1],
      "Invalid/duplicate Client2 SHA256SUMS entry");
    names.add(pair[2]);
  }
  assert.equal(names.size, desktopChecksums.length, "Missing Client2 checksum");
  const manifest = metadata(assets, "client-updates.json");
  const channels = plan.channel === "stable" ? ["dev", "stable"] : ["dev"];
  assert(manifest.schemaVersion === 1, "Unknown Client2 update manifest schema");
  assert(isDeepStrictEqual(Object.keys(manifest).sort(), ["schemaVersion", ...channels].sort()), "Unexpected Client2 update channels");
  for (const channel of channels) {
    const release = manifest[channel];
    assert(release?.version === plan.version && Array.isArray(release.artifacts) &&
      release.artifacts.length === desktopUpdates.length, "Invalid Client2 update version or artifact count");
    assert(isDeepStrictEqual(release.artifacts.map(artifact => artifact.filename).sort(), [...desktopUpdates].sort()),
      "Invalid Client2 update target set");
    for (const {filename, url, sha256, size} of release.artifacts) {
      const canonical = "https://github.com/" + plan.legacy + "/releases/download/" + plan.legacyTag + "/" + filename;
      assert(url === canonical && size === assets[filename].size &&
        sha256 === Buffer.from(assets[filename].sha256, "hex").toString("base64"), "Client2 manifest asset binding mismatch");
    }
  }
}

function summarize(assets) {
  return Object.fromEntries(Object.entries(assets).map(([name, {size, sha256}]) => [name, {size, sha256}]));
}

// Local byte-integrity and mirror parity only. The caller still runs native
// packaging smoke and npm tarball content checks before any promotion.
export async function verifyLocalRelease({tag, source, monorepoDir, legacyDir, npmDir}) {
  const plan = parseComponentTag(tag);
  assert(typeof source === "string" && source.length === 40 && commit.test(source), "Expected exact source commit SHA");
  assert(Boolean(npmDir) === (plan.component === "cli"), "Exactly CLI needs a local npm pack directory");
  assert((await realpath(monorepoDir)) !== (await realpath(legacyDir)), "Mirror must be a separate local directory");
  const expected = expectedReleaseAssets(tag);
  const official = await examine(monorepoDir, expected.github);
  const mirror = await examine(legacyDir, expected.github);
  assert(isDeepStrictEqual(summarize(official), summarize(mirror)), "Local legacy mirror differs byte-for-byte");
  if (plan.component === "client2") checkDesktop(plan, official);
  else checkNative(plan, source, official);
  const npm = npmDir ? summarize(await examine(npmDir, Object.values(expected.npm))) : {};
  const assets = summarize(official);
  assert([...Object.values(assets), ...Object.values(npm)].every(item => hex.test(item.sha256)), "Invalid artifact digest");
  return {mode: "dry-run", ...plan, source, assets, npm};
}

async function verifyPublication(tag, directory, env) {
  const plan = parseComponentTag(tag);
  assert.equal(env.GITHUB_EVENT_NAME, "push");
  assert.equal(env.GITHUB_REPOSITORY, plan.monorepo);
  assert.equal(env.GITHUB_REF, "refs/tags/" + tag);
  assert.equal(env.RELEASE_CHANNEL, plan.channel);
  const expected = expectedReleaseAssets(tag);
  const receiptName = "component-release-receipt.json";
  const files = await examine(directory, [receiptName, ...expected.github, ...Object.values(expected.npm)]);
  const {schema, provenance, receipt} = metadata(files, receiptName);
  assert.equal(schema, 1);
  assert.deepEqual(provenance, {
    repository: plan.monorepo, event: "push", source: env.GITHUB_SHA,
    workflow_ref: env.GITHUB_WORKFLOW_REF, workflow_sha: env.GITHUB_WORKFLOW_SHA,
    run_id: env.GITHUB_RUN_ID, run_attempt: env.GITHUB_RUN_ATTEMPT,
  });
  for (const [key, value] of Object.entries(plan)) assert.equal(receipt[key], value);
  assert.equal(receipt.source, env.GITHUB_SHA);
  const inventory = names => summarize(Object.fromEntries(names.map(name => [name, files[name]])));
  assert.deepEqual(receipt.assets, inventory(expected.github));
  assert.deepEqual(receipt.npm, inventory(Object.values(expected.npm)));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [operation, tag, source, monorepoDir, legacyDir, npmDir, ...extra] = process.argv.slice(2);
  assert(extra.length === 0, "Unexpected release plan arguments");
  if (operation === "tag") {
    assert(source === undefined, "Tag accepts exactly one argument");
    console.log(JSON.stringify(parseComponentTag(tag)));
  } else if (operation === "verify-publication") {
    assert(tag && source && monorepoDir === undefined, "Usage: component-release-plan.mjs verify-publication <tag> <asset-dir>");
    await verifyPublication(tag, source, process.env);
  } else {
    assert(operation === "dry-run" && tag && source && monorepoDir && legacyDir,
      "Usage: component-release-plan.mjs tag <tag> | dry-run <tag> <source-sha> <monorepo-dir> <legacy-dir> [npm-dir]");
    console.log(JSON.stringify(await verifyLocalRelease({tag, source, monorepoDir, legacyDir, npmDir})));
  }
}
