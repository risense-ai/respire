import assert from "node:assert/strict";
import {createHash} from "node:crypto";
import {cp, mkdir, mkdtemp, realpath, rm, writeFile} from "node:fs/promises";
import {tmpdir} from "node:os";
import {join} from "node:path";
import {test} from "node:test";
import {expectedReleaseAssets, parseComponentTag, verifyLocalRelease} from "./component-release-plan.mjs";

const source = "a".repeat(40);
const hash = value => createHash("sha256").update(value).digest("hex");

test("separates exact runtime, CLI, and Client2 tags", () => {
  assert.deepEqual(parseComponentTag("runtime-v2.5.0"), {
    component: "runtime", version: "2.5.0", tag: "runtime-v2.5.0", channel: "stable",
    monorepo: "risense-ai/respire", legacy: "risense-ai/respire-runtime", legacyTag: "v2.5.0",
  });
  assert.equal(parseComponentTag("cli-v1.0.0-dev.1").legacyTag, "v1.0.0-dev.1");
  assert.equal(parseComponentTag("client2-v0.1.0").component, "client2");
  for (const input of ["v2.5.0", "runtime-v02.5.0", "cli-v1.0.0-dev.01", "client2-v1.0.0-rc.1",
    "runtime-v1.0.0+build", "CORE-v1.0.0", "cli-v1.0.0\n", "runtime-v9007199254740992.1.0",
    "refs/tags/cli-v2.3.4", "runtime-v1.3.0/foo", "", null]) {
    assert.throws(() => parseComponentTag(input), /exact component tag/);
  }
});

async function makeFixture(t, tag) {
  const root = await mkdtemp(join(await realpath(tmpdir()), "release-plan-"));
  t.after(() => rm(root, {recursive: true, force: true}));
  const monorepoDir = join(root, "mono");
  const legacyDir = join(root, "legacy");
  const npmDir = tag.startsWith("cli-") ? join(root, "npm") : undefined;
  await mkdir(monorepoDir);
  const expected = expectedReleaseAssets(tag);
  const {version, legacyTag, legacy, channel, component} = parseComponentTag(tag);
  const bytes = new Map(expected.github.map(name => [name, "synthetic:" + name]));
  for (const name of expected.github) {
    if (name.startsWith("runtime-build-")) {
      const triple = name.slice(14, -5);
      const binary = "rsrs-runtime-" + triple + (triple.endsWith("windows-msvc") ? ".exe" : "");
      const archive = "rsrs-runtime-" + triple + ".tar.gz";
      bytes.set(name, JSON.stringify({schema_version: 2, runtime_version: version, tag: legacyTag, channel,
        target_triple: triple, source_commit: source, archive_file: archive, binary_file: binary,
        archive_sha256: hash(bytes.get(archive)), binary_sha256: hash(bytes.get(binary))}));
    }
    if (name.startsWith("cli-build-")) {
      const target = name.slice(10, -5);
      const binary = "rsrs-" + target + (target.endsWith("windows-msvc") ? ".exe" : "");
      bytes.set(name, JSON.stringify({schema_version: 2, artifact_layout: "cli-frontend-v2", version, git_sha: source,
        target, binary_file: binary, binary_size: Buffer.byteLength(bytes.get(binary)), binary_sha256: hash(bytes.get(binary))}));
    }
  }
  if (component === "client2") {
    const products = expected.github.filter(name => name.startsWith("respire-"));
    const updates = products.filter(name => !name.endsWith("setup.exe") && !name.endsWith(".dmg"));
    const artifacts = updates.map(filename => ({
      filename, url: "https://github.com/" + legacy + "/releases/download/" + legacyTag + "/" + filename,
      size: Buffer.byteLength(bytes.get(filename)), sha256: Buffer.from(hash(bytes.get(filename)), "hex").toString("base64"),
    }));
    const release = {version, artifacts};
    bytes.set("client-updates.json", JSON.stringify({schemaVersion: 1, dev: release,
      ...(channel === "stable" ? {stable: release} : {})}));
    bytes.set("SHA256SUMS", products.map(name => hash(bytes.get(name)) + "  " + name).join("\n") + "\n");
  }
  for (const [name, content] of bytes) await writeFile(join(monorepoDir, name), content);
  await cp(monorepoDir, legacyDir, {recursive: true});
  if (npmDir) {
    await mkdir(npmDir);
    for (const file of Object.values(expected.npm)) await writeFile(join(npmDir, file), "synthetic:" + file);
  }
  return {input: {tag, source, monorepoDir, legacyDir, npmDir}, bytes, expected};
}

test("validates all full component inventories without publication", async t => {
  for (const [tag, github, npm] of [["runtime-v2.5.0-dev.1", 24, 0], ["cli-v2.5.0", 16, 9], ["client2-v2.5.0", 14, 0]]) {
    const {input} = await makeFixture(t, tag);
    const result = await verifyLocalRelease(input);
    assert.equal(result.mode, "dry-run");
    assert.equal(Object.keys(result.assets).length, github);
    assert.equal(Object.keys(result.npm).length, npm);
  }
});

test("rejects missing assets, mixed mirror bytes, and invalid source or metadata", async t => {
  const {input, bytes, expected} = await makeFixture(t, "runtime-v2.5.0");
  await assert.rejects(verifyLocalRelease({...input, source: "a".repeat(39) + "\n"}), /source commit/);
  await assert.rejects(verifyLocalRelease({...input, legacyDir: input.monorepoDir}), /separate local directory/);
  const archive = expected.github.find(name => name.endsWith(".tar.gz"));
  await writeFile(join(input.legacyDir, archive), "changed");
  await assert.rejects(verifyLocalRelease(input), /mirror differs/);
  await writeFile(join(input.legacyDir, archive), bytes.get(archive));
  await assert.rejects(verifyLocalRelease({...input, source: "b".repeat(40)}), /metadata identity/);
  await rm(join(input.legacyDir, archive));
  await assert.rejects(verifyLocalRelease(input), /asset set/);
});

test("rejects an update manifest even if byte-identical when it points to the wrong repo", async t => {
  const {input, bytes} = await makeFixture(t, "client2-v2.5.0-dev.1");
  const name = "client-updates.json";
  const replaced = bytes.get(name).replace("risense-ai/respire-client2", "risense-ai/respire");
  for (const dir of [input.monorepoDir, input.legacyDir]) await writeFile(join(dir, name), replaced);
  await assert.rejects(verifyLocalRelease(input), /manifest asset binding mismatch/);
});
