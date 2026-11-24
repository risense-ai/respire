#!/usr/bin/env node

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { isAbsolute, join, posix, resolve, win32 } from "node:path";
import process from "node:process";
import { isDeepStrictEqual } from "node:util";

import { boundPaths, fail, fixturePath, manifest, negative, positive, readJSON, root } from "./runtime-control-fixture-io.mjs";
for (const name of [...positive, ...negative]) {
  const normalized = name.replaceAll("\\", "/");
  if (!boundPaths.has(normalized)) fail(`fixture is not digest-bound: ${name}`);
  const fixture = readJSON(fixturePath(name));
  if (!fixture.fixture_type) fail(`${name}: missing fixture_type`);
  if (fixture.expected !== "accept" && fixture.expected !== "reject") {
    fail(`${name}: invalid expected state`);
  }
  if (fixture.expected === "reject" && typeof fixture.error_code !== "string") {
    fail(`${name}: rejection lacks error_code`);
  }
  if (fixture.fixture_type !== "release_metadata" && !fixture.payload) {
    fail(`${name}: missing payload`);
  }
}

const releaseV2 = readJSON(fixturePath(positive[0]));
const releaseV1 = readJSON(fixturePath(positive[1]));
if (releaseV2.payload.schema_version !== 2) fail("first positive fixture must be schema 2");
if (releaseV1.payload.schema_version !== 1) fail("second positive fixture must be schema 1");
if (releaseV2.payload.archive_sha256 === releaseV2.payload.binary_sha256) {
  fail("canonical fixture must not imply archive and binary hashes are interchangeable");
}
if (releaseV1.payload.sha256 !== releaseV1.payload.archive_sha256) {
  fail("legacy mapping must bind sha256 to archive_sha256");
}
if (releaseV1.payload.triple !== releaseV1.payload.target_triple) {
  fail("legacy mapping must bind triple to target_triple");
}

import { controlSchema, currentScope, identity, scopeFailure } from "./runtime-control-fixture-identity.mjs";
import { assetsFailure } from "./runtime-control-fixture-assets.mjs";
if (!controlSchema.properties.fixture_type.enum.includes("assets_status") ||
    !controlSchema.allOf.some(rule => rule.if?.properties?.fixture_type?.const === "assets_status" &&
      rule.then?.properties?.payload?.$ref === "#/$defs/assetsStatus")) {
  fail("assets status schema is not bound to fixtures");
}
for (const name of [...positive, ...negative]) {
  const fixture = readJSON(fixturePath(name));
  if (fixture.fixture_type !== "assets_status") continue;
  const expected = fixture.expected === "accept" ? null : fixture.error_code;
  const actual = assetsFailure(fixture.payload);
  if (actual !== expected) fail(`${name}: assets rejection ${actual}, expected ${expected}`);
}
const loadedAssets = readJSON(fixturePath("fixtures/positive/assets-loaded.json")).payload;
// Every required success field must remain bound, including each nested identity.
for (const prefix of [[], ["runtime"], ["sdk_pin"], ["modules", 0]]) {
  let original = loadedAssets.backend_load.evidence;
  for (const key of prefix) original = original[key];
  for (const key of Object.keys(original)) {
    const changed = structuredClone(loadedAssets);
    let target = changed.backend_load.evidence;
    for (const part of prefix) target = target[part];
    delete target[key];
    if (!assetsFailure(changed)) fail(`missing probe evidence accepted: ${[...prefix, key].join(".")}`);
  }
}
for (const [selected, engine, provider, cpu_node_fallback] of [
  ["Gpu: DmlExecutionProvider", "gpu", "DmlExecutionProvider", "allowed"],
  ["Npu: OpenVINOExecutionProvider", "npu", "OpenVINOExecutionProvider", "disabled"],
  ["Host", "npu", "OpenVINOExecutionProvider", "host_managed"],
]) {
  const changed = structuredClone(loadedAssets);
  Object.assign(changed.backend_load.evidence, {selected, engine, provider, cpu_node_fallback});
  if (assetsFailure(changed)) fail(`valid selection rejected: ${selected}`);
  changed.backend_load.evidence.cpu_node_fallback = "not_applicable";
  if (!assetsFailure(changed)) fail(`fallback policy mismatch accepted: ${selected}`);
}
const scopeSchema = controlSchema.$defs?.runtimeRequestScope;
if (scopeSchema?.type !== "object" || scopeSchema.additionalProperties !== false ||
    [...(scopeSchema.required ?? [])].sort().join(",") !== "account_id,generation,instance_id" ||
    Object.keys(scopeSchema.properties ?? {}).sort().join(",") !== "account_id,generation,instance_id" ||
    scopeSchema.properties?.generation?.type !== "integer" ||
    scopeSchema.properties?.generation?.minimum !== 1 ||
    scopeSchema.properties?.account_id?.type !== "string" ||
    scopeSchema.properties?.account_id?.minLength !== 1 ||
    scopeSchema.properties?.account_id?.pattern !== "\\S" ||
    scopeSchema.properties?.instance_id?.type !== "string" ||
    scopeSchema.properties?.instance_id?.minLength !== 1 ||
    scopeSchema.properties?.instance_id?.pattern !== "\\S" ||
    controlSchema.$defs?.businessRpcRequest?.properties?.scope?.$ref !== "#/$defs/runtimeRequestScope" ||
    !controlSchema.$defs.businessRpcRequest.required?.includes("scope") ||
    !controlSchema.allOf?.some(rule =>
      rule.if?.properties?.fixture_type?.const === "business_rpc_request" &&
      rule.then?.properties?.payload?.$ref === "#/$defs/businessRpcRequest") ||
    !controlSchema.allOf?.some(rule =>
      rule.if?.properties?.fixture_type?.const === "operation_result" &&
      rule.then?.properties?.payload?.required?.includes("scope") &&
      rule.then.properties.payload.properties?.scope?.$ref === "#/$defs/runtimeRequestScope")) {
  fail("scope schema does not bind strict identity fields");
}
for (const name of [...positive, ...negative]) {
  const fixture = readJSON(fixturePath(name));
  if (fixture.fixture_type === "business_rpc_request") {
    const actual = scopeFailure(fixture.payload, currentScope);
    const expected = fixture.expected === "accept" ? null : fixture.error_code;
    if (actual !== expected) fail(`${name}: scope rejection ${actual}, expected ${expected}`);
    if (fixture.payload.v !== 1 || !fixture.payload.id || !fixture.payload.method) {
      fail(`${name}: invalid request envelope`);
    }
  }
  if (fixture.fixture_type === "operation_result" && scopeFailure(fixture.payload, currentScope)) {
    fail(`${name}: operation receipt lost its scope binding`);
  }
}
const submission = readJSON(fixturePath("fixtures/positive/business-rpc-scoped.json")).payload;
const resultQuery = readJSON(fixturePath("fixtures/positive/runtime-result-scoped.json")).payload;
if (resultQuery.id === submission.id || resultQuery.method !== "runtime.result" ||
    resultQuery.args[0] !== submission.operation_id) fail("result query lost transport/operation identity");

// Mirror CLI response bindings without starting Runtime or weakening its checker.
import { same } from "./runtime-control-fixture-equality.mjs";
function responseFailure({request, response, cached_response: cached}) {
  if (response.v !== 1 || response.id !== request.id ||
      response.code === "protocol_version_mismatch") return "response_header_mismatch";
  if (!Number.isInteger(response.exit) || response.exit < 0 ||
      typeof response.ok !== "boolean" || response.ok !== (response.exit === 0)) {
    return "response_status_mismatch";
  }
  for (const [key, error] of [["operation_id", "response_operation_mismatch"],
    ["idempotency_key", "response_key_mismatch"]]) {
    if (key in response && response[key] !== request[key]) return error;
  }
  if (["scope_required", "scope_mismatch"].includes(response.code)) {
    if (response.ok || response.exit <= 0 ||
        (response.envelope && response.envelope.status !== "fail")) return "response_status_mismatch";
    if (response.scope != null &&
        scopeFailure({scope: response.scope}, response.scope) === "rpc_body_invalid") {
      return "response_scope_mismatch";
    }
    return null;
  }
  const receipt = request.method === "runtime.result" ? response.envelope?.summary : response;
  if (receipt?.operation_id !== request.operation_id) return "response_operation_mismatch";
  if (scopeFailure(receipt ?? {}, request.scope)) return "response_scope_mismatch";
  if ("idempotency_key" in receipt && receipt.idempotency_key !== request.idempotency_key) {
    return "response_key_mismatch";
  }
  if (request.method === "runtime.result") {
    if (receipt.request_id !== request.id) return "response_result_request_mismatch";
    if ("scope" in response && !same(response.scope, receipt.scope)) return "response_scope_mismatch";
  }
  if (cached && !same({...response, id: cached.id}, cached)) return "response_replay_mismatch";
  return null;
}
const responseSchema = controlSchema.$defs?.rpcResponse;
if (!controlSchema.properties.fixture_type.enum.includes("business_rpc_response") ||
    !controlSchema.allOf.some(rule =>
      rule.if?.properties?.fixture_type?.const === "business_rpc_response" &&
      rule.then?.properties?.payload?.$ref === "#/$defs/businessRpcResponseCase") ||
    !["v", "id", "ok", "exit"].every(key => responseSchema?.required.includes(key)) ||
    responseSchema.properties.scope?.$ref !== "#/$defs/runtimeRequestScope" ||
    responseSchema.properties.v?.const !== 1 ||
    !["request", "response"].every(key =>
      controlSchema.$defs.businessRpcResponseCase.required.includes(key))) {
  fail("response schema lost transport or scope binding");
}
for (const name of [...positive, ...negative]) {
  const fixture = readJSON(fixturePath(name));
  if (fixture.fixture_type !== "business_rpc_response") continue;
  const expected = fixture.expected === "accept" ? null : fixture.error_code;
  const actual = responseFailure(fixture.payload);
  if (actual !== expected) fail(`${name}: response rejection ${actual}, expected ${expected}`);
  const request = fixture.payload.request;
  if (scopeFailure(request, currentScope) || request.operation_id !== submission.operation_id ||
      request.idempotency_key !== submission.idempotency_key ||
      (request.method === "runtime.result" && request.args[0] !== submission.operation_id)) {
    fail(`${name}: response fixture lost original submission context`);
  }
}
for (const [scope, expected] of [
  [null, "scope_required"], [[], "rpc_body_invalid"],
  [{...currentScope, unexpected: true}, "rpc_body_invalid"],
  [{...currentScope, generation: "7"}, "rpc_body_invalid"],
  [{...currentScope, generation: -1}, "rpc_body_invalid"],
  [{...currentScope, generation: 0}, "scope_mismatch"],
  [{...currentScope, instance_id: " "}, "scope_mismatch"],
]) {
  if (scopeFailure({scope}, currentScope) !== expected) fail("scope mutation was not rejected");
}
if (scopeFailure({...currentScope}, currentScope) !== "scope_required") {
  fail("flattened identity must not replace top-level scope");
}
for (const key of Object.keys(currentScope)) {
  const incomplete = {...currentScope};
  delete incomplete[key];
  if (scopeFailure({scope: incomplete}, currentScope) !== "rpc_body_invalid") {
    fail(`missing scope member accepted: ${key}`);
  }
}

// Stage-one summaries describe existing paths, not the future split-root layout.
const rootFields = [
  "installation_root", "device_config_root", "client_config_path", "account_data_root",
  "accounts_root", "asset_root", "runtime_state_root",
];
function rootsFailure(roots) {
  if (!roots || roots.schema_version !== 1 || roots.layout !== "phase1" ||
      typeof roots.legacy_whole_isolation !== "boolean" ||
      rootFields.some(key => typeof roots[key] !== "string" || !roots[key]) ||
      !roots.sources || Array.isArray(roots.sources) ||
      Object.keys(roots.sources).sort().join(",") !== [...rootFields].sort().join(",") ||
      Object.keys(roots).some(key => ![...rootFields, "schema_version", "layout",
        "legacy_whole_isolation", "sources", "log_root"].includes(key))) return "root_layout_invalid";
  const path = /^[A-Za-z]:[\\/]/.test(roots.installation_root) ||
    roots.installation_root.startsWith("\\\\") ? win32 : posix;
  const equal = (left, right) => path.normalize(left) === path.normalize(right);
  const within = (root, child) => {
    const relative = path.relative(root, child);
    return relative === "" || (!path.isAbsolute(relative) && relative !== ".." &&
      !relative.startsWith(".." + path.sep));
  };
  if (rootFields.some(key => !path.isAbsolute(roots[key]) ||
      roots[key].split(/[\\/]/).includes(".."))) return "root_layout_invalid";
  const source = roots.legacy_whole_isolation ? "RSRS_DATA_DIR" : "default_data_dir";
  for (const key of rootFields) {
    const allowed = [source];
    if (key === "account_data_root") allowed.push("client.json:data_dir");
    if (key === "asset_root") allowed.push("RSRS_M3_DIR");
    if (!allowed.includes(roots.sources[key])) return "root_source_invalid";
  }
  const root = roots.installation_root;
  const wrongLayout = !equal(roots.device_config_root, root) ||
    !equal(roots.client_config_path, path.join(root, "client.json")) ||
    !equal(roots.accounts_root, path.join(root, "accounts")) ||
    !equal(roots.runtime_state_root, path.join(root, "runtime")) ||
    (roots.sources.asset_root !== "RSRS_M3_DIR" &&
      !equal(roots.asset_root, path.join(root, "models", "bge-m3"))) ||
    (roots.sources.account_data_root !== "client.json:data_dir" &&
      !equal(roots.account_data_root, root)) ||
    (roots.legacy_whole_isolation && !within(root, roots.account_data_root));
  if (wrongLayout) return roots.legacy_whole_isolation ?
    "legacy_root_semantic_conflict" : "root_layout_invalid";
  // Retained only for old fixture consumers; the summary itself has no shared log root.
  if (roots.log_root !== undefined && !equal(roots.log_root, path.join(root, "client2"))) {
    return "root_layout_invalid";
  }
  return null;
}
const rootSchema = controlSchema.allOf.find(rule =>
  rule.if?.properties?.fixture_type?.const === "root_layout")?.then?.properties?.payload;
if (rootSchema?.properties?.layout?.const !== "phase1" ||
    rootSchema.properties.schema_version?.const !== 1 ||
    rootSchema.additionalProperties !== false ||
    !rootFields.every(key => rootSchema.required?.includes(key) &&
      rootSchema.properties?.[key]?.type === "string" &&
      rootSchema.properties.sources?.required?.includes(key)) ||
    !rootSchema.required.includes("sources") ||
    !rootSchema.required.includes("legacy_whole_isolation") ||
    rootSchema.properties.sources?.additionalProperties !== false ||
    rootSchema.required.includes("log_root") || rootSchema.properties.summary_sha256) {
  fail("root schema must describe phase-one actual roots without a fabricated summary hash");
}
for (const name of [...positive, ...negative]) {
  const fixture = readJSON(fixturePath(name));
  if (fixture.fixture_type !== "root_layout") continue;
  const expected = fixture.expected === "accept" ? null : fixture.error_code;
  const actual = rootsFailure(fixture.payload);
  if (actual !== expected) fail(name + ": root rejection " + actual + ", expected " + expected);
}
const rootSummary = readJSON(fixturePath("fixtures/positive/root-layout.json")).payload;
const bareSummary = {...rootSummary};
delete bareSummary.log_root;
if (rootsFailure(bareSummary)) fail("actual summary must not require a synthetic log root");
for (const mutation of [
  {device_config_root: rootSummary.installation_root + "/config"},
  {client_config_path: rootSummary.installation_root + "/config/client.json"},
  {account_data_root: rootSummary.installation_root + "-sibling/accounts/a"},
  {account_data_root: rootSummary.installation_root + "/accounts/../../escape"},
  {accounts_root: rootSummary.account_data_root + "/accounts"},
  {runtime_state_root: rootSummary.account_data_root + "/runtime"},
  {asset_root: rootSummary.installation_root + "/models"},
  {sources: {...rootSummary.sources, client_config_path: "unknown"}},
  {sources: {...rootSummary.sources, account_data_root: "RSRS_DATA_DIR"}},
  {summary_sha256: "5".repeat(64)},
]) {
  if (!rootsFailure({...bareSummary, ...mutation})) fail("root mutation was not rejected");
}
for (const root of ["/tmp/isolated/.rsrs", "C:/isolated/.rsrs"]) {
  const portable = {...bareSummary, installation_root: root, device_config_root: root,
    client_config_path: root + "/client.json", account_data_root: root + "/accounts/a",
    accounts_root: root + "/accounts", asset_root: root + "/models/bge-m3",
    runtime_state_root: root + "/runtime"};
  if (rootsFailure(portable)) fail("portable root fixture rejected: " + root);
  const splitModel = {...portable, asset_root: root + "-assets/bge-m3",
    sources: {...portable.sources, asset_root: "RSRS_M3_DIR"}};
  if (rootsFailure(splitModel)) fail("explicit model source must remain visible");
  if (!rootsFailure({...splitModel, sources: portable.sources})) {
    fail("model root cannot escape legacy isolation without an explicit source");
  }
}

const stop = readJSON(fixturePath("fixtures/negative/stop-ambiguous-ok.json"));
if (stop.payload.state !== "drained" || !stop.payload.unfinished?.length) {
  fail("ambiguous stop fixture no longer demonstrates the forbidden state");
}

const finalStop = readJSON(fixturePath("fixtures/positive/stop-final-drained.json")).payload;
for (const key of [
  "installation_id", "instance_id", "pid", "process_start_id", "image_path",
  "image_sha256", "runtime_version", "account_id", "generation",
]) {
  if (finalStop[key] !== identity.payload[key]) fail(`final stop identity differs: ${key}`);
}
if (finalStop.schema_version !== 1 || finalStop.protocol_version !== 1 ||
    !finalStop.operation_id || finalStop.state !== "drained" ||
    !Array.isArray(finalStop.unfinished) || finalStop.unfinished.length !== 0) {
  fail("final stop fixture must prove a traceable complete drain");
}
const incompleteStop = readJSON(fixturePath("fixtures/negative/stop-final-unfinished.json"));
if (incompleteStop.error_code !== "drain_incomplete" ||
    incompleteStop.payload.state !== "drained" || !incompleteStop.payload.unfinished?.length) {
  fail("final stop negative fixture no longer demonstrates incomplete drain");
}

function terminalStopFailure(payload) {
  if (!["drained", "forced_timeout", "compatibility_timeout", "failed", "unknown"].includes(payload.state) ||
      !Array.isArray(payload.unfinished) || payload.unfinished.some(operation =>
        typeof operation.operation_id !== "string" || !operation.operation_id ||
        !["aborted", "unknown"].includes(operation.result))) return "stop_outcome_invalid";
  return payload.state === "drained" && payload.unfinished.length ? "drain_incomplete" : null;
}
if (terminalStopFailure(finalStop)) fail("valid drained receipt rejected");
for (const state of ["forced_timeout", "compatibility_timeout"]) {
  const path = `fixtures/positive/stop-final-${state.replaceAll("_", "-")}.json`;
  const fixture = readJSON(fixturePath(path));
  if (fixture.expected !== "accept" || fixture.payload.state !== state ||
      !fixture.payload.unfinished.length || terminalStopFailure(fixture.payload)) {
    fail("timeout receipt must retain explicit unfinished outcomes");
  }
  for (const key of ["installation_id", "instance_id", "pid", "process_start_id", "image_path",
    "image_sha256", "runtime_version", "account_id", "generation"]) {
    if (fixture.payload[key] !== identity.payload[key]) fail(`timeout receipt identity differs: ${key}`);
  }
}
const fabricatedStop = readJSON(fixturePath("fixtures/negative/stop-final-fabricated-completion.json"));
if (fabricatedStop.expected !== "reject" || fabricatedStop.error_code !== "stop_outcome_invalid" ||
    terminalStopFailure(fabricatedStop.payload) !== fabricatedStop.error_code ||
    terminalStopFailure(incompleteStop.payload) !== incompleteStop.error_code) {
  fail("unsafe terminal stop outcomes must be rejected");
}

const conflict = readJSON(fixturePath("fixtures/negative/idempotency-conflict.json"));
if (conflict.error_code !== "idempotency_conflict") fail("idempotency fixture drifted");
if (!conflict.payload.idempotency_key) fail("idempotency fixture lacks key");

const route = readJSON(fixturePath("fixtures/positive/bootstrap-route.json"));
if (route.payload.operation.startsWith("auth.") && route.payload.executor !== "cli") {
  fail("identity operation must route to CLI");
}

import { hostSchema, hostSchemaValid } from "./runtime-control-fixture-host-schema.mjs";

// Fixture contexts are evidence anchors, never capabilities or native execution.
import { privateHostFailure } from "./runtime-control-fixture-auth-validation.mjs";

const profileRecoveryExtension = manifest.extensions.profile_recovery;
if (profileRecoveryExtension.schema !== "schema/private-host-control.schema.json" ||
    profileRecoveryExtension.consumer !== "consumers/private_host_control.rs") fail("profile/recovery must reuse the shared schema and consumer");
for (const [groupName, group, legacyCount, newCount, extraCount] of [["positive", positive, 25, 67, 21], ["negative", negative, 48, 253, 132]]) {
  let checked = 0;
  const extra = profileRecoveryExtension.fixtures[groupName];
  if (!Array.isArray(extra) || extra.length !== extraCount) fail("profile/recovery fixture count drift");
  if (new Set([...group, ...extra]).size !== group.length + extra.length) fail("duplicate profile/recovery fixture");
  for (const name of [...group, ...extra]) {
    if (!boundPaths.has(name)) fail(`unbound host fixture: ${name}`);
    const fixture = readJSON(fixturePath(name));
    if (extra.includes(name) && (fixture.fixture_type !== "private_host_control" ||
        fixture.expected !== (groupName === "positive" ? "accept" : "reject"))) fail(`profile/recovery fixture kind: ${name}`);
    if (fixture.fixture_type !== "private_host_control") continue;
    if (fixture.expected === "accept" && !hostSchemaValid(fixture, hostSchema)) fail(`${name}: invalid host fixture container`);
    const expected = fixture.expected === "accept" ? null : fixture.error_code;
    const actual = privateHostFailure(fixture.payload);
    if (actual !== expected) fail(`${name}: host rejection ${actual}, expected ${expected}`);
    checked++;
  }
  if (checked !== newCount + extraCount || group.length + extraCount - checked !== legacyCount) fail("legacy/private host fixture counts drifted");
}

const settingsExtension = manifest.extensions.settings;
if (settingsExtension.schema !== "schema/private-host-control.schema.json" ||
    settingsExtension.consumer !== "consumers/private_host_control.rs") fail("settings must reuse the shared schema and consumer");
for (const [group, count] of [["positive", 10], ["negative", 35]]) {
  const paths = settingsExtension.fixtures[group];
  if (paths.length !== count || new Set(paths).size !== count) fail("settings fixture count drift");
  for (const name of paths) {
    if (!boundPaths.has(name) || [...positive, ...negative, ...profileRecoveryExtension.fixtures.positive,
        ...profileRecoveryExtension.fixtures.negative].includes(name)) fail("unbound or duplicate settings fixture");
    const f = readJSON(fixturePath(name));
    if (f.fixture_type !== "private_host_control" || f.expected !== (group === "positive" ? "accept" : "reject")) fail("settings fixture kind");
    if (group === "positive" && !hostSchemaValid(f, hostSchema)) fail("invalid settings fixture container");
    const actual = privateHostFailure(f.payload), expected = group === "positive" ? null : f.error_code;
    if (actual !== expected) fail(`${name}: settings rejection ${actual}, expected ${expected}`);
  }
}

const metadataExtension = manifest.extensions.metadata;
if (metadataExtension.schema !== "schema/private-host-control.schema.json" ||
    metadataExtension.consumer !== "consumers/private_host_control.rs") fail("metadata must reuse the shared schema and consumer");
for (const [group, count] of [["positive", 7], ["negative", 18]]) {
  const paths = metadataExtension.fixtures[group];
  if (paths.length !== count || new Set(paths).size !== count) fail("metadata fixture count drift");
  for (const name of paths) {
    if (!boundPaths.has(name) || [...positive, ...negative, ...profileRecoveryExtension.fixtures.positive,
        ...profileRecoveryExtension.fixtures.negative, ...settingsExtension.fixtures.positive,
        ...settingsExtension.fixtures.negative].includes(name)) fail("unbound or duplicate metadata fixture");
    const f = readJSON(fixturePath(name));
    if (f.fixture_type !== "private_host_control" || f.expected !== (group === "positive" ? "accept" : "reject")) fail("metadata fixture kind");
    if (group === "positive" && !hostSchemaValid(f, hostSchema)) fail("invalid metadata fixture container");
    const actual = privateHostFailure(f.payload), expected = group === "positive" ? null : f.error_code;
    if (actual !== expected) fail(`${name}: metadata rejection ${actual}, expected ${expected}`);
  }
}

const agentConfigExtension = manifest.extensions.agent_config;
if (agentConfigExtension.schema !== "schema/private-host-control.schema.json" ||
    agentConfigExtension.consumer !== "consumers/private_host_control.rs") fail("agent config must reuse the shared schema and consumer");
for (const [group, count] of [["positive", 6], ["negative", 14]]) {
  const paths = agentConfigExtension.fixtures[group];
  if (paths.length !== count || new Set(paths).size !== count) fail("agent config fixture count drift");
  for (const name of paths) {
    if (!boundPaths.has(name) || [...positive, ...negative, ...profileRecoveryExtension.fixtures.positive,
        ...profileRecoveryExtension.fixtures.negative, ...settingsExtension.fixtures.positive, ...settingsExtension.fixtures.negative,
        ...metadataExtension.fixtures.positive, ...metadataExtension.fixtures.negative].includes(name)) fail("unbound or duplicate agent config fixture");
    const f = readJSON(fixturePath(name));
    if (f.fixture_type !== "private_host_control" || f.expected !== (group === "positive" ? "accept" : "reject")) fail("agent config fixture kind");
    if (group === "positive" && !hostSchemaValid(f, hostSchema)) fail("invalid agent config fixture container");
    const actual = privateHostFailure(f.payload), expected = group === "positive" ? null : f.error_code;
    if (actual !== expected) fail(`${name}: agent config rejection ${actual}, expected ${expected}`);
  }
}

import { spaceExtension, spaceFailure, spaceSchema } from "./runtime-control-fixture-space-validation.mjs";
if (!boundPaths.has(spaceExtension.schema) || !boundPaths.has(spaceExtension.consumer)) fail("unbound space schema/consumer");
for (const [group, count] of [["positive", 7], ["negative", 24]]) {
  const paths = spaceExtension.fixtures[group];
  if (paths.length !== count) fail("space fixture count drift");
  for (const name of paths) {
    if (!boundPaths.has(name)) fail(`unbound space fixture: ${name}`);
    const f = readJSON(fixturePath(name));
    if (f.fixture_type !== "private_space_control" || f.expected !== (group === "positive" ? "accept" : "reject")) fail(`space fixture kind: ${name}`);
    if (group === "positive" && !hostSchemaValid(f, spaceSchema, spaceSchema.$defs)) fail(`invalid space container: ${name}`);
    const actual = spaceFailure(f.payload), expected = group === "positive" ? null : f.error_code;
    if (actual !== expected) fail(`${name}: space rejection ${actual}, expected ${expected}`);
    if (group === "positive") {
      const missing = structuredClone(f.payload), args = JSON.parse(missing.request.args[0]);
      if (args.operation.kind !== "kick") {
        delete args.material;
        missing.request.args[0] = JSON.stringify(args);
        if (spaceFailure(missing) !== "space_params_invalid") fail(`${name}: host material must be required`);
      }
      if (args.operation.kind === "invite" && f.payload.result.outcome === "committed") {
        const pending = structuredClone(f.payload);
        pending.space_context.journal_phase = "authorized";
        pending.result.outcome = "pending";
        pending.result.remaining = null;
        pending.private_response.status = "pending";
        pending.private_response.summary = structuredClone(pending.result);
        pending.finish_maintenance = false;
        for (const key of ["ledger", "progress"]) if (pending[key]) pending[key].outcome = "pending";
        if (spaceFailure(pending) !== null) fail(`${name}: private host persistence handshake rejected`);
        pending.request.method = "control.space.result";
        if (spaceFailure(pending) !== "space_result_invalid") fail(`${name}: query exposed unpersisted code`);
      }
    }
  }
}
console.log(`runtime control contract verified: ${boundPaths.size} bound files (624 prior bindings retained with schema/consumer digest updates; 6 positive / 14 negative AgentConfig cases; prior cohorts retained)`);
