import assert from "node:assert/strict";
import {createHash} from "node:crypto";
import {mkdir, mkdtemp, readFile, readdir, realpath, rm, writeFile} from "node:fs/promises";
import {tmpdir} from "node:os";
import {join} from "node:path";
import {test} from "node:test";
import {assembleLocalRelease} from "./assemble-component-release.mjs";
import {expectedReleaseAssets} from "./component-release-plan.mjs";

const tag = "cli-v2.0.0-dev.11";
const source = "a".repeat(40);
const hash = value => createHash("sha256").update(value).digest("hex");

async function fixture(t) {
  const temp = await mkdtemp(join(await realpath(tmpdir()), "assemble release test "));
  t.after(() => rm(temp, {recursive: true, force: true}));
  const outputParent = join(temp, "output");
  const one = join(temp, "batch-one");
  const two = join(temp, "batch-two");
  const npm = join(temp, "batch-npm");
  for (const path of [outputParent, one, two, npm]) await mkdir(path);
  const expected = expectedReleaseAssets(tag);
  for (const name of expected.github) {
    if (!name.endsWith(".json")) {
      await writeFile(join(one, name), "fixture:" + name);
      continue;
    }
    const triple = name.slice("cli-build-".length, -5);
    const binary = "rsrs-" + triple + (triple.endsWith("windows-msvc") ? ".exe" : "");
    const payload = "fixture:" + binary;
    await writeFile(join(two, name), JSON.stringify({schema_version: 2, artifact_layout: "cli-frontend-v2",
      git_sha: source, version: "2.0.0-dev.11", target: triple, binary_file: binary,
      binary_size: Buffer.byteLength(payload), binary_sha256: hash(payload)}));
  }
  for (const pack of Object.values(expected.npm)) await writeFile(join(npm, pack), "fixture:" + pack);
  const batches = [
    {source, tag, directory: one, kind: "github"},
    {source, tag, directory: two, kind: "github"},
    {source, tag, directory: npm, kind: "npm"},
  ];
  return {tag, source, batches, outputParent};
}

test("assembles detached outputs and an exact independent local mirror", async t => {
  const input = await fixture(t);
  const {root, receipt} = await assembleLocalRelease(input);
  assert.equal(receipt.mode, "dry-run");
  assert.equal(receipt.source, source);
  assert.equal(Object.keys(receipt.assets).length, 16);
  assert.equal(Object.keys(receipt.npm).length, 9);
  assert.deepEqual((await readdir(root)).sort(), ["legacy", "monorepo", "npm"]);
  for (const file of Object.keys(receipt.assets)) {
    assert.deepEqual(await readFile(join(root, "legacy", file)), await readFile(join(root, "monorepo", file)));
  }
});

test("refuses wrong producer selection or an incomplete input without staging", async t => {
  const input = await fixture(t);
  const [_one, two, npm] = input.batches;
  await assert.rejects(assembleLocalRelease({...input, batches: [{..._one, source: "b".repeat(40)}, two, npm]}),
    /exact source and component tag/);
  await assert.rejects(assembleLocalRelease({...input, batches: [_one, two]}), /Missing npm/);
  assert.deepEqual(await readdir(input.outputParent), []);
});

test("fails closed on out-of-scope, repeated, or internally corrupt assets", async t => {
  const input = await fixture(t);
  const [one, two] = input.batches;
  const file = (await readdir(one.directory))[0];
  await writeFile(join(two.directory, file), "duplicate");
  await assert.rejects(assembleLocalRelease(input), /Unexpected, repeated or nonregular/);
  await rm(join(two.directory, file));
  const extra = join(one.directory, "surprise.exe");
  await writeFile(extra, "not a release artifact");
  await assert.rejects(assembleLocalRelease(input), /Unexpected, repeated or nonregular/);
  await rm(extra);
  await writeFile(join(one.directory, file), "wrong bytes for real metadata");
  await assert.rejects(assembleLocalRelease(input), /CLI metadata identity\/checksum mismatch/);
  assert.deepEqual(await readdir(input.outputParent), []);
});
