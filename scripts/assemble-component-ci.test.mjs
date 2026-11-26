import {fileURLToPath} from "node:url";
import assert from "node:assert/strict";
import {execFileSync, spawnSync} from "node:child_process";
import {access, copyFile, mkdir, mkdtemp, realpath, rm, truncate, writeFile} from "node:fs/promises";
import {tmpdir} from "node:os";
import {join} from "node:path";
import test from "node:test";
import {workflowIdentity} from "./assemble-component-ci.mjs";

const repo = "risense-ai/respire";
const workflow = repo + "/.github/workflows/runtime-release-dry-run.yml@";
const initial = {
  GITHUB_ACTIONS: "true", RUNNER_ENVIRONMENT: "github-hosted", GITHUB_REPOSITORY: repo,
  GITHUB_SHA: "a".repeat(40), GITHUB_WORKFLOW_SHA: "b".repeat(40),
  GITHUB_RUN_ID: "234", GITHUB_RUN_ATTEMPT: "2",
};
const repository = {full_name: repo};
const main = {...initial, GITHUB_EVENT_NAME: "workflow_dispatch", GITHUB_REF: "refs/heads/main",
  GITHUB_WORKFLOW_REF: workflow + "refs/heads/main"};
const pull = {...initial, GITHUB_EVENT_NAME: "pull_request", GITHUB_REF: "refs/pull/33/merge",
  GITHUB_WORKFLOW_REF: workflow + "refs/pull/33/merge"};
const event = {
  repository, number: 33,
  pull_request: {head: {repo: repository}, base: {repo: repository, ref: "main"},
    labels: [{name: "native-release-dry-run"}]},
};

test("records exact main and opted-in same-repo PR provenance", () => {
  assert.deepEqual(workflowIdentity(main, {repository}), {
    repository: repo, source: initial.GITHUB_SHA, event: "workflow_dispatch",
    workflow_ref: main.GITHUB_WORKFLOW_REF, workflow_sha: initial.GITHUB_WORKFLOW_SHA,
    run_id: "234", run_attempt: "2",
  });
  assert.equal(workflowIdentity(pull, event).workflow_ref, pull.GITHUB_WORKFLOW_REF);
});

test("rejects mismatched checkout metadata, forks and unapproved refs", () => {
  for (const override of [
    {RUNNER_ENVIRONMENT: "self-hosted"}, {GITHUB_REPOSITORY: "fork/respire"},
    {GITHUB_SHA: "latest"}, {GITHUB_WORKFLOW_SHA: ""}, {GITHUB_RUN_ID: "0"},
    {GITHUB_RUN_ATTEMPT: "2a"}, {GITHUB_WORKFLOW_REF: "other"},
    {GITHUB_EVENT_NAME: "pull_request_target"},
  ]) assert.throws(() => workflowIdentity({...pull, ...override}, event));
  assert.throws(() => workflowIdentity({...main, GITHUB_REF: "refs/tags/runtime-v1.0.0",
    GITHUB_WORKFLOW_REF: workflow + "refs/tags/runtime-v1.0.0"}, {repository}), /trusted main/);
  for (const override of [{head: {repo: {full_name: "fork/respire"}}},
    {base: {repo: repository, ref: "next"}}, {labels: []}]) {
    assert.throws(() => workflowIdentity(pull, {...event, pull_request: {...event.pull_request, ...override}}));
  }
  assert.throws(() => workflowIdentity(pull, {...event, number: 34}), /integration ref/);
});

test("the real CLI rejects a mismatched checked-out SHA and never creates output", async t => {
  const directory = await mkdtemp(join(tmpdir(), "component-ci-test-"));
  t.after(() => rm(directory, {recursive: true}));
  const root = await realpath(directory);
  const scripts = join(root, "scripts");
  await mkdir(scripts);
  const names = ["assemble-component-ci.mjs", "assemble-component-release.mjs", "component-release-plan.mjs", "finalize-component-receipt.mjs"];
  for (const name of names) await copyFile(new URL("./" + name, import.meta.url), join(scripts, name));
  const git = (...args) => execFileSync("git", args, {cwd: root, encoding: "utf8"}).trim();
  git("init", "--quiet");
  git("add", "scripts");
  git("-c", "user.name=Test Fixture", "-c", "user.email=fixture@example.invalid", "-c",
    "commit.gpgsign=false", "commit", "--quiet", "-m", "fixture");
  const temporary = join(root, "temp");
  await mkdir(temporary);
  const payload = join(temporary, "empty-payload");
  await mkdir(payload);
  const output = join(temporary, "assembly");
  const eventFile = join(temporary, "event.json");
  await writeFile(eventFile, JSON.stringify({repository}));
  const env = {...process.env, ...main, GITHUB_EVENT_PATH: eventFile,
    GITHUB_WORKSPACE: root, RUNNER_TEMP: temporary};
  const invoke = (environment, input) => spawnSync(process.execPath,
    [join(scripts, names[0]), "runtime-v1.2.3", input, "-", output],
    {cwd: root, env: environment, encoding: "utf8"});
  const wrong = invoke(env, payload);
  assert.equal(wrong.status, 1);
  assert.equal(wrong.stderr.trim(), "COMPONENT_RECEIPT_FAILED");
  const exact = {...env, GITHUB_SHA: git("rev-parse", "HEAD")};
  const unrelated = invoke(exact, scripts);
  assert.equal(unrelated.status, 1);
  assert.equal(unrelated.stderr.trim(), "COMPONENT_RECEIPT_FAILED");
  await assert.rejects(access(output), {code: "ENOENT"});
  const missing = invoke(exact, payload);
  assert.equal(missing.status, 1);
  assert.match(missing.stderr, /COMPONENT_RECEIPT_FAILED/);
});

test("both executables emit only fixed errors for malformed and oversized event files", async t => {
  const root = await mkdtemp(join(tmpdir(), "event-redaction-"));
  t.after(() => rm(root, {recursive: true, force: true}));
  const file = join(root, "UNTRUSTED_PATH");
  const output = join(root, "output");
  for (const oversized of [false, true]) {
    await writeFile(file, "UNTRUSTED_EVENT\x1b[31m");
    if (oversized) await truncate(file, 25 * 1024 * 1024 + 1);
    for (const [script, code, third] of [
      ["assemble-component-ci.mjs", "COMPONENT_RECEIPT_FAILED", "-"],
      ["assemble-runtime-release-ci.mjs", "RUNTIME_NATIVE_RECEIPT_FAILED", root],
    ]) {
      const path = new URL(script, import.meta.url);
      const result = spawnSync(process.execPath, [fileURLToPath(path), "runtime-v1.0.0", root, third, output],
        {env: {...process.env, ...main, GITHUB_EVENT_PATH: file}, encoding: "utf8"});
      assert.equal(result.status, 1);
      assert.equal(result.stderr.trim(), code);
      assert.equal(result.stdout, "");
      await assert.rejects(access(output), {code: "ENOENT"});
    }
  }
});
