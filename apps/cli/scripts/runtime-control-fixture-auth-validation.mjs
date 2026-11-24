import { isDeepStrictEqual } from "node:util";
import { createHash } from "node:crypto";
import { same } from "./runtime-control-fixture-equality.mjs";
import { configFixtureFailure, fixedWriteModelFailure, hostSchema, hostSchemaValid } from "./runtime-control-fixture-host-schema.mjs";
import { canonical, sortKeys } from "./runtime-control-fixture-space-validation.mjs";
import { identity } from "./runtime-control-fixture-identity.mjs";
import { assetsFailure } from "./runtime-control-fixture-assets.mjs";

function keyMigrationFailure(payload, params, kind) {
  const request = payload.request, result = payload.result, binding = params.binding;
  const same = isDeepStrictEqual;
  const hash = value => createHash("sha256").update(JSON.stringify(value)).digest("hex");
  if (kind === "keyResult" && params.operation.kind === "fivekeys") {
    const context = payload.key_context, write = payload.write_context;
    if (!context || !hostSchemaValid(context, hostSchema.$defs.keyContext) ||
        !write || !hostSchemaValid(write, hostSchema.$defs.writeContext) ||
        write.installation_root !== binding.installation_root || !same(write.source_identity, params.source_identity) ||
        !same(write.operation, params.operation) || write.operation_id !== request.operation_id ||
        write.idempotency_key !== request.idempotency_key || binding.profile !== context.launch_profile ||
        !same(params.identity, context.launch_identity)) return "write_context_mismatch";
    const canonical = value => Array.isArray(value) ? value.map(canonical) : value && typeof value === "object"
      ? Object.fromEntries(Object.keys(value).sort().map(key => [key, canonical(value[key])])) : value;
    const fingerprint = hash(canonical({schema_version: 1, method: "control.key", root: write.installation_root,
      profile: write.profile, source_identity: write.source_identity, operation: write.operation}));
    if (write.fingerprint !== fingerprint || result.fingerprint !== fingerprint) return "host_fingerprint_mismatch";
    if (!same(result.operation, params.operation) || result.source_profile !== write.profile ||
        !same(result.identity, context.selected_identity) ||
        (binding.expected_account_id ?? null) !== (params.identity.kind === "bound" ? params.identity.account_id : null)) return "write_identity_mismatch";
    const applied = result.verification_required === true || result.outcome === "committed";
    if (result.super_password != null || result.required_inputs.some(field => !["password", "super_password", "account_secret", "totp"].includes(field))) return "private_secret_exposure";
    if (applied) {
      const account = createHash("sha256").update(params.operation.user).digest("hex");
      if (result.target_profile !== context.target_profile || context.selected_profile !== context.target_profile ||
          !same(result.identity, context.target_identity) || result.identity.kind !== "bound" ||
          result.account_id !== account || result.identity.account_id !== account ||
          result.session_sha256 !== result.identity.session_sha256 || result.required_inputs.length ||
          (result.verification_required === true ? result.outcome !== "pending" : result.outcome !== "committed") ||
          (result.outcome === "committed" && !context.verified) ||
          ![write.profile, context.target_profile].includes(binding.profile)) return "write_identity_mismatch";
    } else if (result.account_id != null || result.session_sha256 != null ||
        !same(result.identity, params.source_identity) || context.selected_profile !== write.profile ||
        (result.outcome === "awaiting_input") !== (result.required_inputs.length > 0)) return "write_identity_mismatch";
    if (request.method === "control.key.complete" && (!applied || result.outcome !== "committed" || result.verification_required === true || !context.verified ||
        binding.profile !== context.target_profile || !same(params.identity, context.target_identity) || params.input != null)) return "key_completion_mismatch";
    if (context.input_state === "initial" && request.method === "control.key.fivekeys" && params.input?.password == null &&
        !(result.outcome === "awaiting_input" && result.required_inputs.includes("password"))) return "key_input_mismatch";
    if (context.input_state === "continuation" && (payload.original_operation_id !== request.operation_id ||
        !params.input || typeof params.input.totp !== "string")) return "key_input_mismatch";
  } else if (kind === "migrationResult") {
    const original = params.original, context = payload.migration_context;
    if (!context || !hostSchemaValid(context, hostSchema.$defs.migrationContext)) return "migration_context_mismatch";
    if (original.operation_id !== request.operation_id || original.key !== request.idempotency_key || original.key !== original.operation_id) return "host_operation_mismatch";
    if (original.installation_root !== binding.installation_root || original.profile !== binding.profile ||
        !same(original.identity, params.identity) || !same(result.identity, original.identity) ||
        (binding.expected_account_id ?? null) !== (original.identity.kind === "bound" ? original.identity.account_id : null) ||
        original.source_id !== context.source_id || original.account !== context.account) return "migration_context_mismatch";
    const identity = original.identity.kind === "bound" ? {kind: "bound", account_id: original.identity.account_id, session_sha256: original.identity.session_sha256}
      : original.identity.kind === "unbound" ? {kind: "unbound", operation_id: original.identity.operation_id, session_sha256: original.identity.session_sha256} : {kind: "absent"};
    const fingerprint = hash([1, "control.migration.profile", original.installation_root, original.profile, identity, original.source_id, original.account]);
    if (original.fingerprint !== fingerprint || result.fingerprint !== fingerprint || context.fingerprint_preimage !== JSON.stringify([1,
        "control.migration.profile", original.installation_root, original.profile, identity, original.source_id, original.account])) return "host_fingerprint_mismatch";
    const destination = original.installation_root + "/accounts/" + original.account, marker = context.marker;
    if (result.outcome === "completed") {
      if (context.publication !== "bound" || !marker || !same(marker.control, original) || marker.schema !== 2 ||
          marker.complete !== true || marker.namespace_migrated !== true || marker.original_preserved !== true ||
          marker.source_identity !== original.source_id || marker.source !== context.source || marker.destination !== destination ||
          !same(marker.control_result, result.details) || !same(result.details, {state: "migrated", account: original.account,
            dir: destination, source: context.source, original_preserved: true})) return "migration_marker_mismatch";
    } else if (result.outcome === "already_migrated") {
      if (request.method !== "control.migration.profile" || context.publication !== "legacy_unbound" || !marker ||
          marker.control != null || marker.complete !== true || marker.source_identity !== original.source_id ||
          result.details.state !== "already_migrated" || result.details.dir !== marker.destination) return "migration_marker_mismatch";
    } else if (result.details !== null || context.publication === "bound") return "migration_marker_mismatch";
  } else return null;
  const rule = kind === "migrationResult" ? "migrationEnvelope" : "keyEnvelope", envelope = payload.private_response;
  const status = result.outcome === "unknown" ? "fail" : ["pending", "awaiting_input"].includes(result.outcome) ? "pending" : "ok";
  if (!envelope || !hostSchemaValid(envelope, hostSchema.$defs[rule]) || envelope.status !== status ||
      !same(envelope.summary, result)) return "host_result_invalid";
  for (const [name, status, value] of [["control_ready", "ok", "true"], ["store_ready", "skip", "false"]]) {
    const items = envelope.items.filter(item => item.name === name);
    if (items.length !== 1 || items[0].status !== status || items[0].value !== value) return "host_result_invalid";
  }
  if (payload.progress != null || payload.ledger != null) return "private_secret_exposure";
  return null;
}

// Fixture contexts are inputs, not grants, production journals, or native proof.
function profileRecoveryFailure(payload, params, kind) {
  const profile = kind === "profileResult";
  if (!profile && kind !== "recoveryResult") return null;
  const context = payload[profile ? "profile_context" : "recovery_context"];
  if (!context || !hostSchemaValid(context, hostSchema.$defs[profile ? "profileContext" : "recoveryContext"])) return "profile_recovery_context_mismatch";
  const {request, result} = payload, binding = params.binding, authority = context.authority;
  const same = isDeepStrictEqual, statusRead = request.method === "control.auth.recovery_status";
  if (authority.installation_root !== binding.installation_root || authority.profile !== binding.profile || authority.launch_id !== binding.launch_id ||
      !same(authority.identity, params.identity) ||
      (binding.expected_account_id ?? null) !== (params.identity.kind === "bound" ? params.identity.account_id : null)) return "profile_recovery_binding_mismatch";
  if (authority.grant_maintenance !== (statusRead ? "none" : "Paused") || authority.owns_auth !== !statusRead ||
      authority.serving_authorized !== false || ["parent_verified", "binding_verified_before"].some(key => authority[key] !== true) ||
      (profile || statusRead) && authority.binding_verified_after !== true) return "profile_recovery_authority_mismatch";
  if (payload.history != null || payload.original_fingerprint != null || !profile && payload.original_operation_id != null ||
      payload[profile ? "recovery_context" : "profile_context"] != null) return "host_request_invalid";
  if (profile) {
    const original = params.original;
    if (original.operation_id !== request.operation_id || original.key !== request.idempotency_key || original.key !== original.operation_id) return "host_operation_mismatch";
    if (original.installation_root !== binding.installation_root || original.profile !== binding.profile ||
        !same(original.identity, params.identity) || !same(result.identity, original.identity)) return "profile_recovery_binding_mismatch";
    const identity = value => value.kind === "bound" ? {kind: "bound", account_id: value.account_id, session_sha256: value.session_sha256}
      : value.kind === "unbound" ? {kind: "unbound", operation_id: value.operation_id, session_sha256: value.session_sha256} : {kind: "absent"};
    const preimage = JSON.stringify(["control.profile.remove", original.schema_version, original.installation_root, original.profile,
      identity(original.identity), original.name, identity(original.target_identity)]);
    const fingerprint = createHash("sha256").update(preimage).digest("hex");
    if (original.fingerprint !== fingerprint || result.fingerprint !== fingerprint || context.fingerprint_preimage !== preimage) return "host_fingerprint_mismatch";
    const records = context.records;
    if (context.entry_count < records.length || records.some((entry, index) => records.slice(0, index).some(previous =>
        previous.original.operation_id === entry.original.operation_id || previous.original.key === entry.original.key))) return "profile_history_conflict";
    const previous = records.find(entry => entry.original.operation_id === original.operation_id || entry.original.key === original.key);
    let expected;
    if (previous) {
      if (!same(previous.original, original)) return "profile_history_conflict";
      if (context.delete_attempted !== false) return "profile_replay_forbidden";
      expected = previous.phase === "committed" ? "committed" : "unknown";
    } else if (request.method === "control.profile.result") {
      if (context.delete_attempted !== false) return "profile_replay_forbidden";
      expected = "unknown";
    } else {
      if (context.entry_count === 4096 || context.active === true) return "profile_admission_blocked";
      if (context.snapshot_clear !== true) return "profile_snapshot_pending";
      const target = original.installation_root + "/accounts/" + original.name;
      if (context.target_path !== target || context.target_path === original.profile || !same(context.target_identity, original.target_identity)) return "profile_target_mismatch";
      if (context.delete_attempted !== true) return "profile_receipt_mismatch";
      expected = "committed";
    }
    if (result.outcome !== expected) return "profile_receipt_mismatch";
  } else if (statusRead) {
    if (result.state !== context.journal_state || result.recovered !== false || context.recovery_attempted !== false) return "legacy_recovery_mismatch";
  } else {
    if (context.journal_state === "operation_pending") return "legacy_recovery_forbidden";
    const recovered = context.journal_state === "legacy_pending";
    if (result.state !== "clear" || result.recovered !== recovered || context.recovery_attempted !== recovered) return "legacy_recovery_mismatch";
  }
  const envelope = payload.private_response, status = profile && result.outcome === "unknown" ? "fail" : "ok";
  if (!envelope || !hostSchemaValid(envelope, hostSchema.$defs[profile ? "profileEnvelope" : "recoveryEnvelope"]) ||
      !same(envelope.summary, result) || envelope.status !== status) return "host_result_invalid";
  for (const [name, status, value] of [["control_ready", "ok", "true"], ["store_ready", "skip", "false"]]) {
    const items = envelope.items.filter(item => item.name === name);
    if (items.length !== 1 || items[0].status !== status || items[0].value !== value) return "host_result_invalid";
  }
  const response = payload.private_rpc_response, unknown = profile && result.outcome === "unknown";
  if (!response || !hostSchemaValid(response, hostSchema.$defs.profileRecoveryResponse) ||
      response.id !== request.id || (response.operation_id ?? null) !== (request.operation_id ?? null) ||
      (response.idempotency_key ?? null) !== (request.idempotency_key ?? null) || !same(response.envelope, envelope) ||
      response.ok !== !unknown || response.exit !== (unknown ? 2 : 0) ||
      response.code !== (unknown ? "profile_remove_outcome_unknown" : null)) return "host_result_invalid";
  return null;
}

// These anchors describe fixture evidence, not a consumed launch grant.
function settingsAuthorityFailure(payload, binding, context, identity, read) {
  const a = context.authority, snapshot = payload.request.method === "control.snapshot.preflight";
  if (a.installation_root !== binding.installation_root || a.profile !== binding.profile || a.launch_id !== binding.launch_id ||
      (binding.expected_account_id ?? null) !== (snapshot ? null : identity.account_id)) return "settings_binding_mismatch";
  if (!same(a.identity, identity) || a.serving_authorized !== false ||
      ["parent_verified", "binding_verified_before", "binding_verified_after", "grant_verified", "registration_verified"].some(k => a[k] !== true) ||
      a.grant_maintenance !== (payload.maintenance_phase === "Idle" ? "none" : payload.maintenance_phase) ||
      !(read ? ["Idle", "Starting", "Verified"] : ["Paused"]).includes(payload.maintenance_phase)) return "settings_authority_mismatch";
  return null;
}

function offlineReadFailure(payload) {
  const {request, result, offline_context: c} = payload;
  if (Object.hasOwn(payload, "public_response")) return "private_secret_exposure";
  if (["history", "original_operation_id", "original_fingerprint", "config_context", "write_context", "key_context",
      "migration_context", "profile_context", "recovery_context", "metadata_context", "agent_config_context", "progress", "ledger"].some(k => Object.hasOwn(payload, k)) ||
      request.operation_id != null || request.idempotency_key != null) return "host_request_invalid";
  let params;
  try { params = JSON.parse(request.args[0]); } catch { return "host_params_invalid"; }
  if (!hostSchemaValid(params, hostSchema.$defs.offlineReadParams)) return "host_params_invalid";
  if (!c || !hostSchemaValid(c, hostSchema.$defs.offlineReadContext)) return "settings_context_mismatch";
  const authority = settingsAuthorityFailure(payload, params.binding, c, c.authority.identity, true);
  if (authority) return authority;
  const snapshot = request.method === "control.snapshot.preflight", metadata = request.method === "control.metadata.status";
  const discovery = params.legacy_snapshot_discovery === true;
  if (discovery && !snapshot) return "host_params_invalid";
  if (snapshot ? !discovery && !c.snapshot_clear : c.auth_recovery_pending) return "offline_read_blocked";
  if (c.mutated) return "offline_read_mutation";
  if (payload.finish_maintenance) return "maintenance_not_terminal";
  if (!result || !hostSchemaValid(result, hostSchema.$defs.offlineEnvelope) ||
      !same(result, payload.private_response) ||
      result.command !== (snapshot ? "snapshot-preflight" : metadata ? "metadata" : "agent-config") ||
      !hostSchemaValid(result.summary, hostSchema.$defs[snapshot ? "snapshotPreflightSummary" : metadata ? "metadataStatusSummary" : "agentReadSummary"]) ||
      (metadata ? !hostSchemaValid(result.details, hostSchema.$defs.metadataStatusDetails) ||
        result.summary.enabled !== result.details.enabled || result.summary.mode !== result.details.mode : result.details !== null)) return "host_result_invalid";
  if (snapshot && (discovery ? result.summary.allowed !== c.snapshot_clear :
      result.summary.allowed !== true || result.summary.legacy_recovery != null)) return "host_result_invalid";
  for (const [name, status, value] of [["control_ready", "ok", "true"], ["store_ready", "skip", "false"]]) {
    const items = result.items.filter(i => i.name === name);
    if (items.length !== 1 || items[0].status !== status || items[0].value !== value) return "host_result_invalid";
  }
  const r = payload.private_rpc_response;
  if (!r || r.id !== request.id || r.operation_id != null || r.idempotency_key != null ||
      !same(r.envelope, result) || r.ok !== true || r.exit !== 0 || r.code !== null || r.data_dir !== null) return "host_result_invalid";
  return null;
}

function settingsWriteResponseFailure(payload, envelopeKind, code, profile) {
  const {request, result, private_response: e, private_rpc_response: r} = payload, unknown = result.outcome === "unknown";
  if (!e || !hostSchemaValid(e, hostSchema.$defs[envelopeKind]) || !same(e.summary, result) ||
      e.status !== (unknown ? "fail" : "ok")) return "host_result_invalid";
  for (const [name, status, value] of [["control_ready", "ok", "true"], ["store_ready", "skip", "false"]]) {
    const items = e.items.filter(i => i.name === name);
    if (items.length !== 1 || items[0].status !== status || items[0].value !== value) return "host_result_invalid";
  }
  const responseKind = envelopeKind === "agentConfigEnvelope" ? "agentConfigResponse" : "settingsResponse";
  if (!r || !hostSchemaValid(r, hostSchema.$defs[responseKind]) || r.id !== request.id ||
      r.operation_id !== request.operation_id || r.idempotency_key !== request.idempotency_key ||
      !same(r.envelope, e) || r.ok !== !unknown || r.exit !== (unknown ? 2 : 0) ||
      r.code !== (unknown ? code : null) || r.data_dir !== profile) return "host_result_invalid";
  if (payload.finish_maintenance && unknown) return "maintenance_not_terminal";
  return null;
}

function metadataFailure(payload) {
  const {request, result, metadata_context: c} = payload;
  if (Object.hasOwn(payload, "public_response")) return "private_secret_exposure";
  if (["history", "original_operation_id", "original_fingerprint", "config_context", "write_context", "key_context",
      "migration_context", "profile_context", "recovery_context", "offline_context", "agent_config_context", "progress", "ledger"].some(k => Object.hasOwn(payload, k))) return "host_request_invalid";
  if (!request.operation_id || !request.idempotency_key) return "host_operation_required";
  let params;
  try { params = JSON.parse(request.args[0]); } catch { return "host_params_invalid"; }
  if (!hostSchemaValid(params, hostSchema.$defs.metadataParams)) return "host_params_invalid";
  const o = params.original, i = o.intent, query = request.method === "control.metadata.result";
  if (!query && request.method !== "control.metadata." + i.kind ||
      i.kind === "enable" && !(i.mode === "isolated" && i.deployment_review === null ||
        i.mode === "controlled_writers" && i.deployment_review && Object.values(i.deployment_review).every(v => v.trim()))) return "host_params_invalid";
  if (o.operation_id !== request.operation_id || o.key !== o.operation_id || o.key !== request.idempotency_key) return "host_operation_mismatch";
  if (o.installation_root !== params.binding.installation_root || o.profile !== params.binding.profile ||
      !same(o.identity, params.identity)) return "settings_binding_mismatch";
  if (!c || !hostSchemaValid(c, hostSchema.$defs.metadataContext)) return "settings_context_mismatch";
  const authority = settingsAuthorityFailure(payload, params.binding, c, params.identity, false);
  if (authority) return authority;
  const review = i.deployment_review;
  const intent = i.kind === "disable" ? {kind: "disable"} : {kind: "enable", mode: i.mode, deployment_review: review === null ? null :
    {operator: review.operator, upgraded_writers: review.upgraded_writers, disconnected_old_clients: review.disconnected_old_clients, reconciled_offline_writes: review.reconciled_offline_writes}};
  const preimage = JSON.stringify([1, "control.metadata", o.installation_root, o.profile,
    {account_id: o.identity.account_id, session_sha256: o.identity.session_sha256}, intent]);
  if (c.fingerprint_preimage !== preimage || o.fingerprint !== c.fingerprint ||
      o.fingerprint !== createHash("sha256").update(preimage).digest("hex")) return "host_fingerprint_mismatch";
  if (c.auth_recovery_pending || !c.snapshot_clear || !c.writable || c.memory_off || !c.locks_owned) return "metadata_admission_blocked";
  if (!result || !hostSchemaValid(result, hostSchema.$defs.metadataResult) || !same(result.original, o)) return "host_result_invalid";
  const before = c.receipt_before, after = c.receipt_after;
  if (before !== null) {
    if (!same(before.original, o) || before.outcome !== "committed" || !same(after, before)) return "metadata_history_conflict";
    if (c.effect_attempted) return "metadata_replay_forbidden";
    if (!same(result, before)) return "metadata_receipt_mismatch";
  } else if (query) {
    if (c.effect_attempted || after !== null) return "metadata_replay_forbidden";
    if (result.outcome !== "unknown" || result.details !== null) return "metadata_receipt_mismatch";
  } else if (!c.effect_attempted || after === null || after.outcome !== "committed" || !same(after, result)) return "metadata_receipt_mismatch";
  if (result.outcome === "committed" && (i.kind === "disable"
      ? result.details?.enabled !== false || result.details?.sync_isolation_retained !== true
      : result.details?.deployment_assurance !== "operator_attestation_only" || result.details?.old_client_access_enforced !== false ||
        !same(result.details?.deployment_review, i.deployment_review) || result.details?.receipt?.enabled !== true ||
        result.details?.receipt?.deployment?.mode !== i.mode)) return "metadata_receipt_mismatch";
  return settingsWriteResponseFailure(payload, "metadataEnvelope", "metadata_outcome_unknown", null);
}

function agentConfigFailure(payload) {
  const {request, result, agent_config_context: c} = payload;
  if (Object.hasOwn(payload, "public_response")) return "private_secret_exposure";
  if (["history", "original_operation_id", "original_fingerprint", "config_context", "write_context", "key_context",
      "migration_context", "profile_context", "recovery_context", "offline_context", "metadata_context", "progress", "ledger"]
      .some(k => Object.hasOwn(payload, k))) return "host_request_invalid";
  if (!request.operation_id || !request.idempotency_key) return "host_operation_required";
  let params;
  try { params = JSON.parse(request.args[0]); } catch { return "host_params_invalid"; }
  if (!hostSchemaValid(params, hostSchema.$defs.agentConfigParams)) return "host_params_invalid";
  const r = params.review, intent = r.intent;
  if (r.operation_id !== request.operation_id || r.operation_id !== request.idempotency_key) return "host_operation_mismatch";
  if (r.installation_root !== params.binding.installation_root || r.profile !== params.binding.profile) return "settings_binding_mismatch";
  if (!c || !hostSchemaValid(c, hostSchema.$defs.agentConfigContext)) return "settings_context_mismatch";
  const authority = settingsAuthorityFailure(payload, params.binding, c, r.identity, false);
  if (authority) return authority;
  const digest = text => text === null ? null : createHash("sha256").update(text).digest("hex");
  if (r.original_revision !== c.original_revision || digest(r.original_config) !== r.original_revision) return "agent_config_revision_mismatch";
  let value, candidate, workspace = false;
  try { value = JSON.parse(intent.raw_value); } catch { value = intent.raw_value; }
  const key = intent.key.trim();
  if (!key || key === "recall_mode" && !["fast", "quality"].includes(value)) return "host_params_invalid";
  let mode;
  if (["memory_off", "readonly"].includes(key)) {
    if (typeof value !== "boolean") return "host_params_invalid";
    workspace = true; mode = value ? key === "memory_off" ? "off" : "readonly" : "normal";
  } else if (key === "workspace_mode") {
    if (!["normal", "readonly", "off"].includes(value)) return "host_params_invalid";
    workspace = true; mode = value;
  }
  try { candidate = r.original_config === null ? {} : JSON.parse(r.original_config); }
  catch { if (workspace) return "host_params_invalid"; candidate = {}; }
  if (workspace) {
    if (!candidate || typeof candidate !== "object" || Array.isArray(candidate)) return "host_params_invalid";
    candidate.service_mode = mode;
  } else if (candidate && typeof candidate === "object" && !Array.isArray(candidate)) candidate[key] = value;
  if (c.candidate_config !== JSON.stringify(sortKeys(candidate), null, 2) ||
      r.candidate_revision !== c.candidate_revision || digest(c.candidate_config) !== r.candidate_revision) return "agent_config_candidate_mismatch";
  const preimage = JSON.stringify([1, "control.agent_config.set", r.operation_id, r.installation_root, r.profile,
    {account_id: r.identity.account_id, session_sha256: r.identity.session_sha256},
    {key: intent.key, raw_value: intent.raw_value}, r.original_revision, r.candidate_revision]);
  if (r.fingerprint !== c.fingerprint || c.fingerprint_preimage !== preimage || digest(preimage) !== r.fingerprint) return "host_fingerprint_mismatch";
  if (c.auth_recovery_pending || !c.snapshot_clear || !c.locks_owned) return "agent_config_admission_blocked";
  const before = c.record_before, after = c.record_after, previous = before.receipt;
  if (!same(before.review, r) || !before.sent || !same({...after, receipt: null}, {...before, receipt: null})) return "agent_config_original_mismatch";
  if (!result || !hostSchemaValid(result, hostSchema.$defs.agentConfigReceipt) || !same(result.review, r)) return "host_result_invalid";
  if (previous !== null) {
    if (!same(previous.review, r) || !["committed", "rolled_back"].includes(previous.outcome) ||
        !same(after, before) || !same(result, previous)) return "agent_config_receipt_mismatch";
    if (c.write_attempted || c.receipt_published || c.rollback_source !== null) return "agent_config_replay_forbidden";
    if (c.actual_before !== (previous.outcome === "committed" ? c.candidate_config : r.original_config) ||
        c.actual_after !== c.actual_before) return "agent_config_cas_conflict";
  } else if (request.method === "control.agent_config.result") {
    if (c.write_attempted || c.receipt_published || c.rollback_source !== null || !same(after, before)) return "agent_config_replay_forbidden";
    if (result.outcome !== "unknown" || result.result !== null) return "agent_config_receipt_mismatch";
    if (c.actual_after !== c.actual_before) return "agent_config_cas_conflict";
  } else {
    if (c.actual_before !== r.original_config) return "agent_config_cas_conflict";
    if (!c.marker_unchanged || !c.receipt_published || !same(after.receipt, result) ||
        !["committed", "rolled_back"].includes(result.outcome)) return "agent_config_receipt_mismatch";
    if (result.outcome === "committed" ? !c.write_attempted || c.actual_after !== c.candidate_config || c.rollback_source !== null :
        c.actual_after !== r.original_config || c.rollback_source !== r.original_config && c.rollback_source !== c.candidate_config) return "agent_config_cas_conflict";
  }
  if (result.outcome !== "unknown") {
    const action = result.result, committed = result.outcome === "committed";
    if (!action || !hostSchemaValid(action, hostSchema.$defs.agentConfigAction) ||
        action.status !== (committed ? "ok" : "fail") || action.summary.action !== (committed ? "updated" : "rolled_back")) return "host_result_invalid";
    if (committed ? action.summary.key !== intent.key || !same(action.summary.value, value) || action.items.length !== 1 ||
        action.items[0].name !== intent.key || action.items[0].status !== "ok" || action.items[0].value !== intent.raw_value || action.errors.length !== 0 :
        action.errors.length === 0 || action.items.length !== 0) return "host_result_invalid";
  }
  return settingsWriteResponseFailure(payload, "agentConfigEnvelope", "agent_config_outcome_unknown", r.profile);
}

function privateHostFailure(payload) {
  if (payload.channel !== "inherited_pipe") return "private_channel_required";
  if (!hostSchemaValid(payload, hostSchema.$defs.case)) return "host_request_invalid";
  const request = payload.request, method = hostSchema["x-methods"][request.method];
  if (method.params === "offlineReadParams") return offlineReadFailure(payload);
  if (method.params === "metadataParams") return metadataFailure(payload);
  if (method.params === "agentConfigParams") return agentConfigFailure(payload);
  if (["offline_context", "metadata_context", "agent_config_context"].some(k => Object.hasOwn(payload, k))) return "host_request_invalid";
  if (!["profileResult", "recoveryResult"].includes(method.result) &&
      ["profile_context", "recovery_context", "private_rpc_response"].some(key => Object.hasOwn(payload, key))) return "host_request_invalid";
  if (["secretResult", "keysExportResult", "identityWriteResult", "keyResult", "migrationResult", "profileResult", "recoveryResult"].includes(method.result) &&
      Object.hasOwn(payload, "public_response")) return "private_secret_exposure";
  if (method.mutable && (!request.operation_id || !request.idempotency_key)) return "host_operation_required";
  if (!method.mutable && (request.operation_id != null || request.idempotency_key != null)) return "host_request_invalid";
  let params;
  try { params = JSON.parse(request.args[0]); } catch { return "host_params_invalid"; }
  if (!hostSchemaValid(params, hostSchema.$defs[method.params])) return "host_params_invalid";
  if (["profileResult", "recoveryResult"].includes(method.result) &&
      ["config_context", "write_context", "key_context", "migration_context"].some(key => Object.hasOwn(payload, key))) return "host_request_invalid";
  if ((Object.hasOwn(payload, "progress") || Object.hasOwn(payload, "ledger")) &&
      method.result !== "migrationResult" && !(method.result === "keyResult" && params.operation.kind === "fivekeys")) return "host_request_invalid";
  if (method.params === "keyParams" && ((request.method === "control.key.super_reset" && params.operation.kind !== "super_reset") ||
      ["control.key.fivekeys", "control.key.complete"].includes(request.method) && params.operation.kind !== "fivekeys")) return "host_params_invalid";
  if (method.params === "identityWriteParams" && request.method !== "control.identity.write_result" &&
      (request.method === "control.identity.logout") !== (params.operation.kind === "logout")) return "host_params_invalid";
  if (method.params === "modelMaintenanceParams" && (params.phase !== "plan" && params.expected == null ||
      params.phase === "plan" && params.expected != null && params.kind !== "reset_cpu" ||
      params.phase === "apply" || params.phase === "verify" && params.kind === "reset_cpu")) return "host_params_invalid";
  if (method.params === "modelParams" && (request.method === "control.model_install_plan"
      ? params.selection_sha256 != null : params.selection_sha256 == null)) return "host_params_invalid";
  const snapshot = request.method === "control.snapshot_maintenance";
  const legacySnapshot = request.method === "control.snapshot_legacy_recovery";
  const verified = ["control.auth.account_complete", "control.key.complete"].includes(request.method) || snapshot && ["commit", "release"].includes(params.action);
  if (payload.maintenance_phase !== (request.method === "control.auth.recovery_status" ? "Idle" : verified ? "Verified" : "Paused")) return "maintenance_phase_mismatch";
  if (payload.original_operation_id && payload.original_operation_id !== request.operation_id) return "host_operation_mismatch";
  if (legacySnapshot) {
    if (params.recovery_operation_id !== request.operation_id) return "host_operation_mismatch";
    if (params.request_fingerprint !== payload.original_fingerprint) return "host_fingerprint_mismatch";
  }
  if (snapshot) {
    if (params.operation_id !== request.operation_id) return "host_operation_mismatch";
    const tuple = ["snapshot-restore-v1", params.binding.installation_root, params.binding.profile,
      params.binding.expected_account_id ?? null, params.sync_generation, params.snapshot_id];
    const fingerprint = createHash("sha256").update(JSON.stringify(tuple)).digest("hex");
    if (params.request_fingerprint !== fingerprint || payload.original_fingerprint && payload.original_fingerprint !== fingerprint) return "host_fingerprint_mismatch";
  }
  if (method.params === "configParams") {
    const failure = configFixtureFailure(params, payload.config_context);
    if (failure) return failure;
  }
  const result = payload.result;
  if (!result) return ["profileResult", "recoveryResult", "legacySnapshotRecoveryResult"].includes(method.result) ? "host_result_invalid" : payload.finish_maintenance ? "maintenance_not_terminal" : null;
  if (!hostSchemaValid(result, hostSchema.$defs[method.result])) return "host_result_invalid";
  if (method.mutable && result[legacySnapshot ? "recovery_operation_id" : "operation_id"] !== request.operation_id) return "host_operation_mismatch";
  if (legacySnapshot && (result.request_fingerprint !== params.request_fingerprint ||
      result.journal_sha256 !== params.journal_sha256)) return "host_fingerprint_mismatch";
  const profileRecoveryError = profileRecoveryFailure(payload, params, method.result);
  if (profileRecoveryError) return profileRecoveryError;
  const extension = method.result === "migrationResult" || method.result === "keyResult" && params.operation.kind === "fivekeys";
  const fixedFailure = extension ? keyMigrationFailure(payload, params, method.result) : fixedWriteModelFailure(payload, params, method.result);
  if (fixedFailure) return fixedFailure;
  if (["identityResult", "configReceipt"].includes(method.result)) {
    const envelope = payload.private_response;
    const kind = method.result === "identityResult" ? "identityEnvelope" : "configEnvelope";
    if (!envelope || !hostSchemaValid(envelope, hostSchema.$defs[kind]) ||
        !isDeepStrictEqual(method.result === "identityResult" ? envelope.details : envelope.summary, result)) return "host_result_invalid";
    for (const [name, status, value] of [["control_ready", "ok", "true"], ["store_ready", "skip", "false"]]) {
      const matching = envelope.items.filter(item => item.name === name);
      if (matching.length !== 1 || matching[0].status !== status || matching[0].value !== value) return "host_result_invalid";
    }
    if (method.result === "configReceipt") {
      for (const [receipt, parameter] of [["original_revision", "original_revision"], ["candidate_revision", "candidate_revision"],
          ["source_profile", "source_profile"], ["target_profile", "target_profile"],
          ["source_identity", "source_identity"], ["target_identity", "identity"]]) {
        if (!isDeepStrictEqual(result[receipt] ?? null, params[parameter] ?? null)) return "config_receipt_mismatch";
      }
      for (const key of ["data_dir", "addr", "autosync", "cure_auto", "rpc_parallelism"]) {
        if (!isDeepStrictEqual(result.intent[key] ?? null, params.intent[key] ?? null)) return "config_receipt_mismatch";
      }
    }
  }

  if (method.result === "modelPlan" && JSON.stringify(result.assets.assets) !== JSON.stringify(result.assets.missing)) return "host_result_invalid";
  if (["modelPlan", "modelVerify"].includes(method.result)) {
    if (method.result === "modelVerify" && result.selection_sha256 !== params.selection_sha256) return "host_selection_mismatch";
    const envelope = payload.public_response;
    const command = method.result === "modelPlan" ? "model-install-plan" : "model-install-verify";
    if (!envelope || !hostSchemaValid(envelope, hostSchema.$defs.modelEnvelope) ||
        envelope.command !== command || envelope.summary.complete !== result.assets.complete ||
        JSON.stringify(envelope.details) !== JSON.stringify(result) || assetsFailure(envelope.details.assets)) return "host_result_invalid";
    for (const [name, status, value] of [["control_ready", "ok", "true"], ["store_ready", "skip", "false"]]) {
      const matching = envelope.items.filter(item => item.name === name);
      if (matching.length !== 1 || matching[0].status !== status || matching[0].value !== value) return "host_result_invalid";
    }
  }
  if (method.result === "accountResult") {
    if (["awaiting_verification", "committed"].includes(result.outcome) &&
        (!result.target_profile || !result.account_id || !result.session_sha256)) return "host_result_invalid";
    if (result.outcome === "rolled_back" && (result.account_id != null || result.session_sha256 != null || result.super_password != null)) return "host_result_invalid";
  }
  if (snapshot) {
    if (result.request_fingerprint !== params.request_fingerprint || result.snapshot_id !== params.snapshot_id) return "host_fingerprint_mismatch";
    for (const previous of payload.history ?? []) {
      if (previous.operation_id !== request.operation_id) continue;
      if (previous.request_fingerprint !== params.request_fingerprint || previous.snapshot_id !== params.snapshot_id) return "terminal_history_conflict";
      if (["released", "rolled_back"].includes(previous.state) && result.state !== previous.state ||
          previous.state === "verified" && !["verified", "released"].includes(result.state)) return "terminal_history_conflict";
    }
  }
  const terminal = method.result === "profileResult" && result.outcome === "committed" ||
    method.result === "recoveryResult" && request.method === "control.auth.recover_legacy" && result.state === "clear" ||
    method.result === "accountResult" && ["committed", "rolled_back"].includes(result.outcome) ||
    extension && (method.result === "migrationResult" ? ["completed", "already_migrated"].includes(result.outcome) :
      ["committed", "rolled_back"].includes(result.outcome) && !result.verification_required) ||
    (snapshot || legacySnapshot) && ["released", "rolled_back"].includes(result.state);
  if (payload.finish_maintenance && !terminal) return "maintenance_not_terminal";
  return null;
}

export { privateHostFailure };
