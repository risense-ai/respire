import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { createHash } from "node:crypto";
import { isDeepStrictEqual } from "node:util";
import { boundPaths, fail, fixturePath, readJSON } from "./runtime-control-fixture-io.mjs";
import { canonical } from "./runtime-control-fixture-space-validation.mjs";

const contract = readFileSync(resolve("docs/runtime-control-contract.md"), "utf8");
for (const target of [
  "x86_64-unknown-linux-gnu",
  "x86_64-unknown-linux-musl",
  "aarch64-unknown-linux-gnu",
  "aarch64-unknown-linux-musl",
  "aarch64-apple-darwin",
  "x86_64-apple-darwin",
  "x86_64-pc-windows-msvc",
  "aarch64-pc-windows-msvc",
]) {
  if (!contract.includes(target)) fail(`compatibility contract omits ${target}`);
}

const hostSchema = readJSON(fixturePath("schema/private-host-control.schema.json"));
if (!boundPaths.has("schema/private-host-control.schema.json")) fail("private host schema is not digest-bound");
if (!boundPaths.has("consumers/private_host_control.rs")) fail("shared Rust consumer is not digest-bound");
for (const marker of ["private-host-control.schema.json", "awaiting_verification", "snapshot-restore-v1", "Terminal history"]) {
  if (!contract.includes(marker)) fail(`private host contract omits ${marker}`);
}

// Only the keywords used by this frozen schema are evaluated. Context fields
// declare test inputs, not proof of a consumed grant or a live process.
function hostSchemaValid(value, schema, definitions = hostSchema.$defs) {
  if (schema.$ref) return hostSchemaValid(value, definitions[schema.$ref.slice("#/$defs/".length)], definitions);
  if (schema.allOf?.some(child => !hostSchemaValid(value, child, definitions))) return false;
  if (schema.not && hostSchemaValid(value, schema.not, definitions)) return false;
  if (schema.if) {
    const branch = hostSchemaValid(value, schema.if, definitions) ? schema.then : schema.else;
    if (branch && !hostSchemaValid(value, branch, definitions)) return false;
  }
  if (schema.anyOf && !schema.anyOf.some(child => hostSchemaValid(value, child, definitions))) return false;
  if (schema.oneOf && schema.oneOf.filter(child => hostSchemaValid(value, child, definitions)).length !== 1) return false;
  if (Object.hasOwn(schema, "const") && value !== schema.const) return false;
  if (schema.enum && !schema.enum.includes(value)) return false;
  if (schema.type === "null" && value !== null) return false;
  if (schema.type === "object" && (!value || typeof value !== "object" || Array.isArray(value))) return false;
  if (schema.type === "array" && !Array.isArray(value)) return false;
  if (schema.type === "string" && typeof value !== "string") return false;
  if (schema.type === "boolean" && typeof value !== "boolean") return false;
  if (schema.type === "integer" && !Number.isSafeInteger(value)) return false;
  if (schema.minimum !== undefined && value < schema.minimum) return false;
  if (schema.maximum !== undefined && value > schema.maximum) return false;
  if (schema.minLength !== undefined && value.length < schema.minLength) return false;
  if (schema.maxLength !== undefined && value.length > schema.maxLength) return false;
  if (schema.pattern && !new RegExp(schema.pattern).test(value)) return false;
  if (schema.required?.some(key => !Object.hasOwn(value, key))) return false;
  if (schema.additionalProperties === false && Object.keys(value).some(key => !Object.hasOwn(schema.properties, key))) return false;
  for (const [key, child] of Object.entries(schema.properties ?? {})) {
    if (Object.hasOwn(value, key) && !hostSchemaValid(value[key], child, definitions)) return false;
  }
  if (schema.minItems !== undefined && value.length < schema.minItems) return false;
  if (schema.maxItems !== undefined && value.length > schema.maxItems) return false;
  if (schema.items && !value.every(item => hostSchemaValid(item, schema.items, definitions))) return false;
  if (schema.uniqueItems && new Set(value.map(item => JSON.stringify(item))).size !== value.length) return false;
  return true;
}

function configFixtureFailure(params, context) {
  if (!context || !hostSchemaValid(context, hostSchema.$defs.configContext)) return "config_context_mismatch";
  const digest = text => text == null ? null : createHash("sha256").update(text, "utf8").digest("hex");
  if ((params.original_config ?? null) !== context.original_config ||
      (params.original_revision ?? null) !== context.original_revision ||
      digest(params.original_config) !== (params.original_revision ?? null) ||
      params.candidate_revision !== context.candidate_revision ||
      context.candidate_config !== context.reviewed_candidate_config ||
      digest(context.candidate_config) !== params.candidate_revision) return "config_revision_mismatch";
  if (params.target_profile !== params.binding.profile || params.source_profile !== context.source_profile ||
      params.target_profile !== context.target_profile || !isDeepStrictEqual(params.source_identity, context.source_identity) ||
      !isDeepStrictEqual(params.identity, context.target_identity)) return "config_context_mismatch";
  let original, candidate;
  try { original = params.original_config == null ? {} : JSON.parse(params.original_config); candidate = JSON.parse(context.candidate_config); }
  catch { return "config_intent_mismatch"; }
  if (!original || typeof original !== "object" || Array.isArray(original)) return "config_intent_mismatch";
  const expected = structuredClone(original);
  for (const key of ["data_dir", "addr", "autosync", "cure_auto", "rpc_parallelism"]) {
    let value = params.intent[key];
    if (value == null) continue;
    if (key === "data_dir" || key === "addr") value = value.trim();
    if (key === "data_dir" && value === "" || key === "rpc_parallelism" && value === 0) delete expected[key];
    else expected[key] = value;
  }
  return isDeepStrictEqual(candidate, expected) ? null : "config_intent_mismatch";
}

function fixedWriteModelFailure(payload, params, kind) {
  if (!["identityWriteResult", "keyResult", "modelMaintenanceReceipt"].includes(kind)) return null;
  const result = payload.result, binding = params.binding;
  if (kind !== "modelMaintenanceReceipt") {
    const context = payload.write_context;
    if (!context || !hostSchemaValid(context, hostSchema.$defs.writeContext) ||
        context.installation_root !== binding.installation_root || context.profile !== binding.profile ||
        !isDeepStrictEqual(context.source_identity, params.source_identity) ||
        !isDeepStrictEqual(context.operation, params.operation) ||
        context.operation_id !== payload.request.operation_id || context.idempotency_key !== payload.request.idempotency_key) return "write_context_mismatch";
    const canonical = value => Array.isArray(value) ? value.map(canonical) : value && typeof value === "object"
      ? Object.fromEntries(Object.keys(value).sort().map(key => [key, canonical(value[key])])) : value;
    const fingerprint = createHash("sha256").update(JSON.stringify(canonical({schema_version: 1,
      method: kind === "keyResult" ? "control.key" : "control.identity.write", root: context.installation_root,
      profile: context.profile, source_identity: context.source_identity, operation: context.operation}))).digest("hex");
    if (result.fingerprint !== fingerprint || context.fingerprint !== fingerprint) return "host_fingerprint_mismatch";
    if (!isDeepStrictEqual(result.operation, params.operation)) return "write_context_mismatch";
    const before = params.identity, after = result.identity;
    if ((binding.expected_account_id ?? null) !== (before.kind === "bound" ? before.account_id : null)) return "write_identity_mismatch";
    const full = kind === "identityWriteResult" && params.operation.kind === "logout" && params.operation.full;
    if (after.kind === "absent" ? before.kind !== "absent" && !full :
        before.kind !== after.kind || (after.kind === "bound" ? before.account_id !== after.account_id : before.operation_id !== after.operation_id)) return "write_identity_mismatch";
    if (kind === "identityWriteResult") {
      if (result.path !== binding.profile + "/session.json" ||
          (result.session_sha256 ?? null) !== (after.session_sha256 ?? null) ||
          full && result.outcome === "committed" && after.kind !== "absent" ||
          params.operation.kind === "logout" && result.revoke != null ||
          params.operation.kind === "session_revoke" && (result.revoke != null && result.revoke.session_id.toLowerCase() !== params.operation.session_id.toLowerCase() ||
            result.outcome === "committed" && result.revoke == null)) return "write_identity_mismatch";
    } else {
      if (result.verification_required === true || result.source_profile !== binding.profile || result.target_profile != null && result.target_profile !== binding.profile ||
          result.outcome === "committed" && (result.target_profile !== binding.profile || after.kind !== "bound" ||
            result.account_id !== after.account_id || result.session_sha256 !== after.session_sha256 || !result.super_password) ||
          result.outcome !== "committed" && (result.super_password != null || result.account_id != null || result.session_sha256 != null) ||
          (result.outcome === "awaiting_input") !== (result.required_inputs.length > 0)) return "write_identity_mismatch";
    }
  } else {
    if (params.expected != null && !isDeepStrictEqual(params.expected, result.plan.review)) return "model_review_mismatch";
    if (result.plan.kind !== params.kind || result.phase !== params.phase || result.verified !== (params.phase !== "plan") ||
        result.config_applied !== (params.phase === "apply") ||
        params.kind === "reset_cpu" && (!result.plan.cpu_candidate_sha256 || result.plan.preparation != null || result.plan.catalog != null || result.plan.uninstall_directory != null) ||
        params.kind === "uninstall_m3" && (!result.plan.uninstall_directory || result.plan.cpu_candidate_sha256 != null || result.plan.preparation != null || result.plan.catalog != null) ||
        params.kind === "install_engines" && (result.plan.uninstall_directory != null || result.plan.cpu_candidate_sha256 != null ||
          (result.plan.preparation == null) !== (result.plan.catalog == null) || result.plan.catalog != null &&
          result.plan.catalog.destination !== binding.installation_root + "/engines/winml-2.4.89/Microsoft.Windows.AI.MachineLearning.dll")) return "model_receipt_mismatch";
  }
  const model = kind === "modelMaintenanceReceipt", envelope = model ? payload.public_response : payload.private_response;
  const rule = model ? "modelMaintenanceEnvelope" : kind === "keyResult" ? "keyEnvelope" : "identityWriteEnvelope";
  const status = model ? "ok" : result.outcome === "unknown" ? "fail" : ["pending", "awaiting_input"].includes(result.outcome) ? "pending" : "ok";
  if (!envelope || !hostSchemaValid(envelope, hostSchema.$defs[rule]) || envelope.status !== status ||
      !isDeepStrictEqual(model ? envelope.details : envelope.summary, result) || model &&
      !isDeepStrictEqual(envelope.summary, {kind: params.kind, phase: params.phase, verified: result.verified})) return "host_result_invalid";
  for (const [name, status, value] of [["control_ready", "ok", "true"], ["store_ready", "skip", "false"]]) {
    const matching = envelope.items.filter(item => item.name === name);
    if (matching.length !== 1 || matching[0].status !== status || matching[0].value !== value) return "host_result_invalid";
  }
  return null;
}

export { configFixtureFailure, fixedWriteModelFailure, hostSchema, hostSchemaValid };
