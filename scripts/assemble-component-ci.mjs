import assert from "node:assert/strict";
import {execFileSync} from "node:child_process";
import {lstat, open, realpath} from "node:fs/promises";
import {dirname, isAbsolute, join, relative, resolve, sep} from "node:path";
import {pathToFileURL} from "node:url";
import {finalizeComponent} from "./finalize-component-receipt.mjs";
import {localFanoutPlan, parseComponentTag} from "./component-release-plan.mjs";

const repository = "risense-ai/respire";
const workflow = repository + "/.github/workflows/release.yml@";
const commitPattern = /^[0-9a-f]{40}$(?![\s\S])/;
const idPattern = /^[1-9][0-9]*$(?![\s\S])/;

// GitHub event payloads are bounded even when the file changes during reading.
export async function readWorkflowEvent(path) {
  const limit = 25 * 1024 * 1024;
  const initial = await lstat(path);
  assert(initial.isFile() && initial.size > 0 && initial.size <= limit);
  const handle = await open(path, "r");
  try {
    const bytes = Buffer.alloc(initial.size + 1);
    let offset = 0;
    while (offset < bytes.length) {
      const {bytesRead} = await handle.read(bytes, offset, bytes.length - offset);
      if (!bytesRead) break;
      offset += bytesRead;
    }
    assert.equal(offset, initial.size);
    return JSON.parse(new TextDecoder("utf-8", {fatal: true}).decode(bytes.subarray(0, offset)));
  } finally {
    await handle.close();
  }
}

export function workflowIdentity(env, event) {
  assert.equal(env.GITHUB_ACTIONS, "true", "Requires GitHub Actions");
  assert.equal(env.RUNNER_ENVIRONMENT, "github-hosted", "Requires GitHub-hosted runner");
  assert.equal(env.GITHUB_REPOSITORY, repository, "Unexpected repository");
  for (const name of ["GITHUB_SHA", "GITHUB_WORKFLOW_SHA"]) {
    assert(commitPattern.test(env[name] ?? ""), "Invalid " + name);
  }
  for (const name of ["GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT"]) {
    assert(idPattern.test(env[name] ?? ""), "Invalid " + name);
  }
  assert.equal(event.repository?.full_name, repository, "Unexpected event repository");
  assert.equal(env.GITHUB_WORKFLOW_REF, workflow + env.GITHUB_REF, "Unexpected workflow identity");
  if (env.GITHUB_EVENT_NAME === "pull_request") {
    const pr = event.pull_request;
    assert(Number.isSafeInteger(event.number) && event.number > 0, "Invalid pull request number");
    assert.equal(env.GITHUB_REF, "refs/pull/" + event.number + "/merge", "Expected PR integration ref");
    assert.equal(pr?.head?.repo?.full_name, repository, "Fork pull requests cannot assemble releases");
    assert.equal(pr?.base?.repo?.full_name, repository, "Unexpected PR base repository");
    assert.equal(pr?.base?.ref, "main", "Unexpected PR base branch");
    assert(pr?.labels?.some(label => label.name === "native-release-dry-run"),
      "Expected native release dry-run opt-in");
  } else {
    assert.equal(env.GITHUB_EVENT_NAME, "workflow_dispatch", "Unexpected dry-run event");
    assert.equal(env.GITHUB_REF, "refs/heads/main", "Workflow dispatch must use trusted main");
  }
  return {
    repository, event: env.GITHUB_EVENT_NAME, source: env.GITHUB_SHA,
    workflow_ref: env.GITHUB_WORKFLOW_REF, workflow_sha: env.GITHUB_WORKFLOW_SHA,
    run_id: env.GITHUB_RUN_ID, run_attempt: env.GITHUB_RUN_ATTEMPT,
  };
}

async function exactDirectory(path) {
  assert(isAbsolute(path), "Expected an absolute directory path");
  const canonical = await realpath(path);
  assert.equal(canonical, resolve(path), "Directory must not be redirected");
  assert((await lstat(canonical)).isDirectory(), "Expected a directory");
  return canonical;
}

function isInside(parent, child) {
  const path = relative(parent, child);
  return path !== "" && path !== ".." && !path.startsWith(".." + sep) && !isAbsolute(path);
}

async function run(args, env) {
  assert.equal(args.length, 4,
    "Usage: assemble-component-ci.mjs <component-tag> <github-payload-dir> <npm-payload-dir-or-> <fresh-output-parent>");
  const [tag, github, npm, output] = args;
  const plan = parseComponentTag(tag);
  assert.equal(npm !== "-", plan.component === "cli", "Exactly CLI needs npm packs");
  assert(isAbsolute(env.GITHUB_EVENT_PATH ?? ""), "Missing GitHub event path");
  const provenance = workflowIdentity(env, await readWorkflowEvent(env.GITHUB_EVENT_PATH));
  const workspace = await exactDirectory(env.GITHUB_WORKSPACE ?? "");
  const checkout = resolve(dirname(process.argv[1]), "..");
  assert.equal(await realpath(checkout), workspace, "Assembler is not running from the Actions checkout");
  const source = execFileSync("git", ["rev-parse", "HEAD"], {cwd: workspace, encoding: "utf8", stdio: "pipe"}).trim();
  assert.equal(source, provenance.source, "Checkout must match the Actions event SHA");
  assert.equal(execFileSync("git", ["status", "--porcelain", "--untracked-files=no"],
    {cwd: workspace, encoding: "utf8", stdio: "pipe"}).trim(), "", "Tracked checkout must be clean");

  const temp = await exactDirectory(env.RUNNER_TEMP ?? "");
  const directories = [await exactDirectory(github), ...(npm === "-" ? [] : [await exactDirectory(npm)])];
  assert(directories.every(path => isInside(temp, path)), "Payload directories must be under RUNNER_TEMP");
  assert(isAbsolute(output) && output === resolve(output), "Expected a normalized absolute output path");
  await exactDirectory(dirname(output));
  assert(isInside(temp, output), "Output must be under RUNNER_TEMP");
  assert(directories.every(path => !isInside(path, output) && !isInside(output, path) && path !== output),
    "Output must be separate from payload directories");
  const batches = directories.map((directory, index) => ({
    tag, source, kind: index === 0 ? "github" : "npm", directory,
  }));
  await finalizeComponent({tag, source, batches, output, makeResult(receipt) {
    const result = {schema: 1, scope: "local-byte-parity-only", provenance, receipt};
    if (provenance.event === "workflow_dispatch" && plan.component !== "runtime") {
      result.fanout = localFanoutPlan(receipt);
    }
    return result;
  }});
  console.log(join(output, "component-release-receipt.json"));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    await run(process.argv.slice(2), process.env);
  } catch {
    console.error("COMPONENT_RECEIPT_FAILED");
    process.exitCode = 1;
  }
}
