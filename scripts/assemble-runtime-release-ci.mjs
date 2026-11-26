import assert from "node:assert/strict";
import {createHash} from "node:crypto";
import {execFileSync} from "node:child_process";
import {lstat, open, readFile, readdir, realpath} from "node:fs/promises";
import {dirname, isAbsolute, join, relative, resolve, sep} from "node:path";
import {pathToFileURL} from "node:url";
import {finalizeComponent} from "./finalize-component-receipt.mjs";
import {workflowIdentity} from "./assemble-component-ci.mjs";
import {expectedReleaseAssets, localFanoutPlan, parseComponentTag} from "./component-release-plan.mjs";

const profiles = {
  "windows-x64": ["x86_64-pc-windows-msvc", "Windows", "x64"],
  "windows-arm64": ["aarch64-pc-windows-msvc", "Windows", "arm64"],
  "linux-gnu-x64": ["x86_64-unknown-linux-gnu", "Linux", "x64", "adjacent-dynamic"],
  "linux-gnu-arm64": ["aarch64-unknown-linux-gnu", "Linux", "arm64", "adjacent-dynamic"],
  "linux-musl-x64": ["x86_64-unknown-linux-musl", "Linux", "x64", "static"],
  "linux-musl-arm64": ["aarch64-unknown-linux-musl", "Linux", "arm64", "static"],
  "macos-intel": ["x86_64-apple-darwin", "macOS", "x64", "adjacent-dynamic"],
  "macos-arm64": ["aarch64-apple-darwin", "macOS", "arm64"],
};
const cases = {
  "runtime.host-control-no-wall-clock-deadline": ["delayed-input-survives-and-closed-input-is-rejected"],
  "runtime.client-json-bad-config": ["scoped-structured-invalid-config-envelope",
    "invalid-client-config-bytes-preserved", "inference-settings-preserved-on-rejection",
    "scoped-recovery-preserves-inference-settings"],
  "runtime.remember-result": ["recover-original-receipt-and-read-back-without-replay"],
  "runtime.stop-clean-drained": ["standalone-stop-accepted", "standalone-normal-exit",
    "standalone-identity-bound-final-drained-receipt", "standalone-listener-closed-after-exit"],
  "runtime.mcp-initialize-tools-list": ["initialize", "tools/list", "tools/call:memory_status", "same-process-after-call"],
};
const coverage = Object.entries(cases).flatMap(([name, items])=>items.map(item=>name + ":" + item)).sort();
const hash = /^[a-f0-9]{64}$(?![\s\S])/;

async function directory(path) {
  assert(isAbsolute(path), "Expected an absolute directory");
  const exact = await realpath(path);
  assert.equal(exact, resolve(path), "Directory redirects outside reviewed input");
  assert((await lstat(exact)).isDirectory(), "Expected a direct directory");
  return exact;
}

function below(parent, child) {
  const name = relative(parent, child);
  return name !== "" && name !== ".." && !name.startsWith(".." + sep) && !isAbsolute(name);
}

function namesFor(target) {
  return [`rsrs-runtime-${target}${target.endsWith("windows-msvc") ? ".exe" : ""}`,
    `rsrs-runtime-${target}.tar.gz`, `runtime-build-${target}.json`].sort();
}

async function exactNames(path, expected) {
  assert.deepEqual((await readdir(path)).sort(), [...expected].sort(), "Incomplete or unexpected native input set");
}

function smokeEvidence(report, lane, tag, provenance, names) {
  const [target, os, arch, loader] = profiles[lane];
  assert.equal(report.schema, 1, "Unsupported native receipt schema");
  assert.equal(report.mode, "dry-run", "Native receipt must be dry-run only");
  for (const [key, value] of Object.entries({...provenance, target, tag})) {
    assert.equal(report[key], value, "Native receipt identity mismatch: " + key);
  }
  // On musl the Rust process is built for the musl target on a native GNU host.
  const rustHost = target.replace(/-musl$/, "-gnu");
  assert.deepEqual(report.native_identity,
    {runner_os: os, runner_arch: arch.toUpperCase(), node_arch: arch, rust_host: rustHost},
    "Native host identity differs");
  if (loader) assert.equal(report.native_loader, loader, "Native loader policy differs");
  else if (os === "macOS") assert(["static", "adjacent-dynamic"].includes(report.native_loader));
  else assert.equal(report.native_loader, undefined, "Unexpected Windows loader receipt");
  if (os === "Linux" || lane === "macos-intel") {
    assert(hash.test(report.provider_archive_sha256 ?? ""), "Missing reviewed provider archive digest");
    if (lane.startsWith("linux-gnu")) assert.equal(report.provider_archive_sha256,
      "769d1d3ea8ab6cd69f737c9dd4d4462aa4ad0ccfa106eaf506efc40d7bead5db",
      "GNU provider differs from prepare-gnu-ort.sh");
  }
  assert.deepEqual(Object.keys(report.artifact_sha256 ?? {}).sort(), names, "Native receipt asset set differs");
  assert(Object.values(report.artifact_sha256).every(value=>typeof value === "string" && hash.test(value)),
    "Native receipt has invalid artifact digest");
  const entries = report.packaged_native_smoke?.case_results;
  assert.equal(report.packaged_native_smoke?.passed, true, "Native packaged smoke failed");
  assert(Array.isArray(entries) && entries.length === coverage.length, "Incomplete native smoke cases");
  assert(entries.every(entry=>entry?.status === "passed"), "Native packaged smoke case failed");
  assert.deepEqual(entries.map(entry=>entry.test_id + ":" + entry.name).sort(), coverage,
    "Native packaged smoke scope differs");
  return report.artifact_sha256;
}

// Inputs must be separate exact same-run/attempt downloads from GitHub Actions.
// This verifies the resulting evidence and bytes, not the transport or supplier.
async function verifyRuntime({tag, provenance, evidenceRoot, payloadRoot, output}) {
  assert.equal(parseComponentTag(tag).component, "runtime", "Expected a Runtime component tag");
  assert.equal(provenance.event, "workflow_dispatch", "Runtime payload assembly is restricted to main dispatch");
  for (const key of ["run_id", "run_attempt"]) assert(/^[1-9][0-9]*$(?![\s\S])/.test(provenance[key] ?? ""));
  const lanes = Object.keys(profiles);
  const expected = expectedReleaseAssets(tag).github;
  assert.deepEqual(lanes.flatMap(lane=>namesFor(profiles[lane][0])).sort(), expected,
    "Native profile inventory differs from release plan");
  const evidence = await directory(evidenceRoot), payload = await directory(payloadRoot);
  assert(evidence !== payload && !below(evidence,payload) && !below(payload,evidence), "Native inputs overlap");
  const suffix = "-" + provenance.run_id + "-" + provenance.run_attempt;
  const receiptFolders = lanes.map(lane=>"runtime-" + lane + "-evidence" + suffix);
  const payloadFolders = lanes.map(lane=>"runtime-" + lane + "-payload" + suffix);
  await exactNames(evidence, receiptFolders);
  await exactNames(payload, payloadFolders);
  const batches = [], claims = {}, nativeReceipts = {};
  for (const [index, lane] of lanes.entries()) {
    const receipts = await directory(join(evidence, receiptFolders[index]));
    const artifacts = await directory(join(payload, payloadFolders[index]));
    const receiptName = "runtime-" + lane + ".json", names = namesFor(profiles[lane][0]);
    await exactNames(receipts, [receiptName]);
    await exactNames(artifacts, names);
    const file = join(receipts, receiptName), st = await lstat(file);
    assert(st.isFile() && st.nlink === 1 && st.size > 0 && st.size <= 64 * 1024, "Invalid native receipt file");
    const handle = await open(file, "r");
    const bytes = Buffer.alloc(st.size);
    try {
      let offset = 0;
      while (offset < bytes.length) {
        const {bytesRead} = await handle.read(bytes, offset, bytes.length - offset);
        if (!bytesRead) throw new Error("NATIVE_RECEIPT_SIZE");
        offset += bytesRead;
      }
      if ((await handle.read(Buffer.alloc(1))).bytesRead) throw new Error("NATIVE_RECEIPT_SIZE");
    } finally {
      await handle.close();
    }
    const report = JSON.parse(new TextDecoder("utf-8", {fatal: true}).decode(bytes));
    nativeReceipts[lane] = createHash("sha256").update(bytes).digest("hex");
    Object.assign(claims, smokeEvidence(report, lane, tag, provenance, names));
    batches.push({tag, source: provenance.source, kind: "github", directory: artifacts});
  }
  assert(isAbsolute(output) && resolve(output) === output, "Expected a normalized output path");
  await directory(dirname(output));
  assert([evidence,payload].every(input=>input !== output && !below(input,output) && !below(output,input)),
    "Native output overlaps source data");
  return finalizeComponent({tag, source: provenance.source, batches, output, makeResult(receipt) {
    assert.deepEqual(Object.fromEntries(Object.entries(receipt.assets).map(([name, asset])=>[name, asset.sha256])),
      claims);
    return {schema: 1, scope:"same-attempt-native-runtime-and-local-byte-parity", provenance, receipt,
      native_receipt_sha256: nativeReceipts, fanout: localFanoutPlan(receipt)};
  }});
}

export async function assembleRuntimeNative(inputs) {
  try {
    return await verifyRuntime(inputs);
  } catch {
    throw new Error("RUNTIME_NATIVE_RECEIPT_FAILED");
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  assert.equal(process.argv.length, 6,
    "Usage: assemble-runtime-release-ci.mjs <tag> <same-attempt-evidence-root> <same-attempt-payload-root> <fresh-output>");
  const [tag, evidenceRoot, payloadRoot, output] = process.argv.slice(2);
  const env = process.env;
  assert(isAbsolute(env.GITHUB_EVENT_PATH ?? ""), "Missing GitHub event path");
  const provenance = workflowIdentity(env, JSON.parse(await readFile(env.GITHUB_EVENT_PATH,"utf8")));
  const root = await directory(env.GITHUB_WORKSPACE ?? "");
  assert.equal(await directory(resolve(dirname(process.argv[1]), "..")), root, "Assembler checkout mismatch");
  const git = (...args)=>execFileSync("git",args,{cwd:root,encoding:"utf8"}).trim();
  assert.equal(git("rev-parse","HEAD"), provenance.source, "Checkout must match Actions event SHA");
  assert.equal(git("status","--porcelain","--untracked-files=no"), "", "Tracked checkout must be clean");
  const temporary = await directory(env.RUNNER_TEMP ?? "");
  assert([await directory(evidenceRoot),await directory(payloadRoot),resolve(output)].every(path=>below(temporary,path)),
    "Inputs and output must remain inside RUNNER_TEMP");
  await assembleRuntimeNative({tag, provenance, evidenceRoot, payloadRoot, output});
}
