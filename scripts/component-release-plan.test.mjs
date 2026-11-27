import assert from "node:assert/strict";
import {createHash} from "node:crypto";
import {cp, mkdir, mkdtemp, readFile, realpath, rm, writeFile} from "node:fs/promises";
import {tmpdir} from "node:os";
import path, {join} from "node:path";
import {test} from "node:test";
import {runInNewContext} from "node:vm";
import {expectedReleaseAssets, localFanoutPlan, parseComponentTag, verifyLocalRelease} from "./component-release-plan.mjs";

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

test("publish jobs require successful prerequisites and a non-cancelled run", async () => {
  const workflow = await readFile(new URL("../.github/workflows/release.yml", import.meta.url), "utf8");
  for (const [component, job] of [["runtime", "runtime-assembly"], ["cli", "npm-dry-run"], ["client2", "desktop-assembly"]]) {
    const condition = new RegExp(`^  publish-${component}:\\r?\\n(?:    [^\\r\\n]*\\r?\\n)*?    if: \\$\\{\\{ (.+?) \\}\\}$`, "m")
      .exec(workflow)?.[1];
    assert.ok(condition, `Missing publish-${component} condition`);
    assert.ok(!condition.includes("||"), `publish-${component} must use conjunctions`);
    const clauses = condition.split(" && ");
    for (const clause of ["!cancelled()", "needs.tag-plan.result == 'success'", `needs.${job}.result == 'success'`]) {
      assert.ok(clauses.includes(clause), `publish-${component} must require ${clause}`);
    }
  }
});

test("CLI musl builds mount workspace dependencies and cache under the CLI directory", async () => {
  const cliScripts = new URL("../apps/cli/scripts/", import.meta.url);
  const host = await readFile(new URL("build-linux-musl.sh", cliScripts), "utf8");
  assert.match(host, /-v "\$root:\/workspace\/apps\/cli"/);
  assert.match(host, /-v "\$repo_root\/packages\/runtime-sdk-rs:\/workspace\/packages\/runtime-sdk-rs:ro"/);
  assert.match(host, /-v "\$repo_root\/contracts\/runtime-api:\/workspace\/contracts\/runtime-api:ro"/);
  const container = await readFile(new URL("build-linux-musl-container.sh", cliScripts), "utf8");
  assert.match(container, /^export CARGO_HOME="\/workspace\/apps\/cli\/target\/musl-cache\/\$TARGET\/cargo"$/m);
});

test("CLI pack inventory agrees with the current producer and actual launcher selection", async () => {
  const cli = new URL("../apps/cli/", import.meta.url);
  const manifest = JSON.parse(await readFile(new URL("npm/artifact-manifest.json", cli), "utf8"));
  const cargo = await readFile(new URL("cli/Cargo.toml", cli), "utf8");
  const lock = await readFile(new URL("Cargo.lock", cli), "utf8");
  const launcher = await readFile(new URL("npm/bin/cli.js", cli), "utf8");
  assert.equal(manifest.cliVersion, /^version\s*=\s*"([^"]+)"/m.exec(cargo)?.[1]);
  assert.equal(manifest.cliVersion, /\[\[package\]\]\r?\nname = "respire"\r?\nversion = "([^"]+)"/.exec(lock)?.[1]);
  assert.equal(manifest.npmScope, "rsrs-cli", "Current producer uses the direct launcher, not the historical alias");
  assert.equal(manifest.targets.length, 8);
  const platformNames = new Set(manifest.targets.map(target => target.pkg));
  assert.equal(platformNames.size, 8);
  const version = manifest.cliVersion;
  const expected = expectedReleaseAssets("cli-v" + version);
  const filenames = [...platformNames, manifest.npmScope].map(name =>
    [name, name.replace(/^@/, "").replace("/", "-") + "-" + version + ".tgz"]);
  assert.deepEqual(expected.npm, Object.fromEntries(filenames));
  assert.deepEqual(expected.github, manifest.targets.flatMap(({triple}) => [
    "rsrs-" + triple + (triple.endsWith("-windows-msvc") ? ".exe" : ""), "cli-build-" + triple + ".json",
  ]).sort());
  assert.equal(expected.npm["@rsrsai/cli"], undefined);

  for (const {os: [platform], cpu: [arch], libc, pkg} of manifest.targets) {
    const selected = [], errors = [];
    const stopped = new Error("stop before starting any binary");
    function require(name) {
      if (name === "../package.json") return {version};
      if (name === "child_process") return {spawnSync: () => assert.fail("Cannot run a binary in an inventory test")};
      if (name === "path") return path;
      if (name === "fs") return {existsSync: () => false, readFileSync: () => assert.fail("Unexpected package access")};
      assert.fail("Unexpected launcher dependency " + name);
    }
    require.resolve = name => { selected.push(name); throw Error("Fixture platform binary absent"); };
    assert.throws(() => runInNewContext(launcher, {
      require, __dirname: "/fixture/npm/bin",
      process: {platform, arch, env: libc ? {RSRS_LIBC: libc[0]} : {}, exit: code => {
        assert.equal(code, 1);
        throw stopped;
      }},
      console: {error: message => errors.push(message)},
    }), error => error === stopped);
    assert.deepEqual(selected, [pkg + "/bin/rsrs" + (platform === "win32" ? ".exe" : "")]);
    assert.equal(errors.length, 1);
    assert(errors[0].includes(pkg + "@" + version));
  }
});

test("CLI local release rejects misspelled Windows packs and historical aliases", async t => {
  const {input, expected} = await makeFixture(t, "cli-v2.5.0");
  const old = "rsrsai-win32-x64-2.5.0.tgz";
  await rm(join(input.npmDir, expected.npm["@rsrsai/win-x64"]));
  await writeFile(join(input.npmDir, old), "wrong package");
  await assert.rejects(verifyLocalRelease(input), /asset set/);
  await rm(join(input.npmDir, old));
  await writeFile(join(input.npmDir, expected.npm["@rsrsai/win-x64"]), "replacement fixture");
  const historical = join(input.npmDir, "rsrsai-cli-2.5.0.tgz");
  await writeFile(historical, "historical package");
  await assert.rejects(verifyLocalRelease(input), /asset set/);
  await rm(historical);
  assert.equal(Object.keys((await verifyLocalRelease(input)).npm).length, 9);
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
  for (const [tag, github, npm] of [["runtime-v2.5.0-dev.1", 18, 0], ["cli-v2.5.0", 16, 9], ["client2-v2.5.0", 14, 0]]) {
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

test("verified component plans bind both tag namespaces and exact npm pack digests", async t => {
  for (const component of ["runtime", "cli", "client2"]) {
    const tag = component + "-v2.5.0";
    const {input, expected} = await makeFixture(t, tag);
    const receipt = await verifyLocalRelease(input);
    const plan = localFanoutPlan(receipt);
    assert.deepEqual(plan.github.map(({repository, tag}) => [repository, tag]), [
      ["risense-ai/respire", tag], ["risense-ai/respire-" + component, "v2.5.0"],
    ]);
    assert.deepEqual(plan.github[0].assets, expected.github.map(name => ({name, ...receipt.assets[name]})));
    assert.deepEqual(plan.github[1].assets, plan.github[0].assets);
    assert.deepEqual(plan.npm, Object.entries(expected.npm).map(([pkg, name]) => ({
      package: pkg, version: "2.5.0", tarball: {name, ...receipt.npm[name]},
    })));
    assert.throws(() => localFanoutPlan({...receipt, legacy: "untrusted/mirror"}));
    assert.throws(() => localFanoutPlan({...receipt, assets: {}}));
    assert.throws(() => localFanoutPlan({...receipt, npm: {...receipt.npm, extra: {}}}));
  }
});
