import { createHash } from "node:crypto";
import { isDeepStrictEqual } from "node:util";
import { fixturePath, manifest, readJSON } from "./runtime-control-fixture-io.mjs";
import { hostSchemaValid } from "./runtime-control-fixture-host-schema.mjs";

const spaceExtension = manifest.extensions.space;
const spaceSchema = readJSON(fixturePath(spaceExtension.schema));
const spaceValid = (value, name) => hostSchemaValid(value, spaceSchema.$defs[name], spaceSchema.$defs);
const canonical = value => JSON.stringify(sortKeys(value));
function sortKeys(value) {
  if (Array.isArray(value)) return value.map(sortKeys);
  if (value && typeof value === "object") return Object.fromEntries(Object.keys(value).sort().map(key => [key, sortKeys(value[key])]));
  return value;
}
const textHash = text => createHash("sha256").update(text, "utf8").digest("hex");
function spaceFailure(p) {
  if (p.channel !== "inherited_pipe") return "private_channel_required";
  if (Object.hasOwn(p, "public_response")) return "private_secret_exposure";
  const q = p.request;
  if (!spaceValid(q, "request")) return "space_request_invalid";
  let a;
  try { a = JSON.parse(q.args[0]); } catch { return "space_params_invalid"; }
  if (!spaceValid(a, "params")) return "space_params_invalid";
  if (a.operation.kind === "kick") {
    if (a.material != null || a.invite_code != null) return "space_params_invalid";
  } else if (!a.material || typeof a.material.super_password !== "string" || !a.material.super_password.length ||
      typeof a.material.keyring_account !== "string" || !/^login-[A-Za-z0-9-]*$/.test(a.material.keyring_account) ||
      a.material.keyring_account.length > 128 || !["native", "secret_service"].includes(a.material.keyring_backend)) {
    return "space_params_invalid";
  }
  if (q.method !== "control.space.result" && q.method !== `control.space.${a.operation.kind}`) return "space_method_mismatch";
  const r = p.result, c = p.space_context, op = a.operation;
  if (!spaceValid(r, "result")) return "space_result_invalid";
  for (const name of ["ledger", "progress"]) {
    if (Object.hasOwn(p, name) && !spaceValid(p[name], "publicState")) return "private_secret_exposure";
  }
  if (p.maintenance_phase !== "Paused") return "maintenance_phase_mismatch";
  if (!spaceValid(c, "context") || c.installation_root !== a.binding.installation_root || c.profile !== a.binding.profile ||
      !isDeepStrictEqual(c.source_identity, a.source_identity) || !isDeepStrictEqual(c.operation, op) ||
      c.operation_id !== q.operation_id || c.idempotency_key !== q.idempotency_key) return "space_context_mismatch";
  if (r.operation_id !== q.operation_id || !isDeepStrictEqual(r.operation, op)) return "space_operation_mismatch";
  const preimage = canonical({schema_version: 1, method: "control.space", root: c.installation_root,
    profile: c.profile, source_identity: a.source_identity, operation: op});
  if (c.fingerprint_preimage !== preimage || c.fingerprint !== textHash(preimage) || r.fingerprint !== c.fingerprint) return "space_fingerprint_mismatch";
  if (r.profile !== c.profile || c.selected_profile !== c.profile || !isDeepStrictEqual(r.identity, a.source_identity) ||
      !isDeepStrictEqual(a.identity, a.source_identity) || a.binding.expected_account_id !== a.source_identity.account_id) return "space_identity_mismatch";
  const outcome = {admitted: "pending", authorized: "pending", remote_pending: "unknown", remote_unknown: "unknown", committed: "committed", rolled_back: "rejected"}[c.journal_phase];
  if (r.outcome !== outcome || [p.ledger, p.progress].some(v => v && v.outcome !== r.outcome)) return "space_outcome_mismatch";
  for (const state of [p.ledger, p.progress]) {
    if (state && ["operation_id", "idempotency_key", "fingerprint"].some(k => Object.hasOwn(state, k) && state[k] !== ({...q, fingerprint: c.fingerprint})[k])) return "space_context_mismatch";
  }
  if (op.kind === "join") {
    if (r.invite_code !== null || r.session_id !== null || r.kick.length || r.remaining !== null ||
        r.target_profile !== c.target_profile || r.target_account_id !== c.target_account_id || r.target_session_sha256 !== c.target_session_sha256 ||
        r.target_profile !== `${c.installation_root}/accounts/${op.space}` ||
        r.outcome === "committed" && (c.readonly_team !== op.readonly || r.target_account_id !== textHash(op.user) || !r.target_session_sha256) ||
        r.outcome !== "committed" && (r.target_account_id !== null || r.target_session_sha256 !== null)) return "space_join_mismatch";
    if (a.invite_code !== null) {
      try {
        if (!/^1mem-invite:(?:[0-9a-f]{2})+$/.test(a.invite_code) || textHash(a.invite_code) !== op.invite_sha256) return "space_join_mismatch";
        const invite = JSON.parse(Buffer.from(a.invite_code.slice(12), "hex").toString("utf8"));
        if (["space", "addr", "user", "readonly"].some(k => invite[k] !== op[k])) return "space_join_mismatch";
      } catch { return "space_join_mismatch"; }
    }
  } else if (op.kind === "kick") {
    if (!isDeepStrictEqual(r.kick.map(k => k.session_id), c.kick_targets) ||
        !op.all && (r.kick.length !== 1 || r.kick[0].session_id !== op.session_id) ||
        op.all && r.remaining !== r.kick.filter(k => k.state !== "revoked").length ||
        r.outcome === "committed" && r.kick.some(k => ["queued", "remote_pending", "unknown"].includes(k.state)) ||
        [r.invite_code, r.session_id, r.target_profile, r.target_account_id, r.target_session_sha256].some(v => v !== null)) return "space_kick_mismatch";
    if (q.method === "control.space.result" && c.remote_posts.length || c.previous_kick.some((old, i) =>
      old.session_id !== r.kick[i]?.session_id || old.state === "revoked" && r.kick[i].state !== "revoked" ||
      ["remote_pending", "unknown"].includes(old.state) && r.kick[i].state !== "unknown")) return "space_remote_replay";
  } else {
    if ([r.target_profile, r.target_account_id, r.target_session_sha256].some(v => v !== null) || r.kick.length) return "space_result_invalid";
    if (r.outcome === "committed" || r.outcome === "pending" && r.invite_code !== null) {
      if (r.outcome === "pending" && (c.journal_phase !== "authorized" || q.method === "control.space.result")) return "space_result_invalid";
      try {
        const code = JSON.parse(Buffer.from(r.invite_code.slice(12), "hex").toString("utf8"));
        if (!r.invite_code.startsWith("1mem-invite:") || code.session_id !== r.session_id ||
            code.control_operation_id !== q.operation_id || code.control_fingerprint !== r.fingerprint || code.readonly !== op.readonly) return "space_result_invalid";
      } catch { return "space_result_invalid"; }
    } else if (r.invite_code !== null || r.session_id !== null) return "space_result_invalid";
  }
  const e = p.private_response;
  if (!spaceValid(e, "envelope") || !isDeepStrictEqual(e.summary, r) ||
      e.status !== ({pending: "pending", unknown: "fail", committed: "ok", rejected: "fail"})[r.outcome]) return "space_result_invalid";
  for (const [name, status, value] of [["control_ready", "ok", "true"], ["store_ready", "skip", "false"]]) {
    const items = e.items.filter(i => i.name === name);
    if (items.length !== 1 || items[0].status !== status || items[0].value !== value) return "space_result_invalid";
  }
  if (p.finish_maintenance && !["committed", "rejected"].includes(r.outcome)) return "maintenance_not_terminal";
  return null;
}

export { canonical, sortKeys, spaceExtension, spaceFailure, spaceSchema };
