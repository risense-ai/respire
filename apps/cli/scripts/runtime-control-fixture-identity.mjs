import { fail, fixturePath, readJSON } from "./runtime-control-fixture-io.mjs";

const identity = readJSON(fixturePath("fixtures/positive/health-identity.json"));
for (const key of [
  "installation_root",
  "installation_id",
  "instance_id",
  "pid",
  "process_start_id",
  "image_path",
  "image_sha256",
  "runtime_version",
  "account_id",
  "generation",
  "protocol_range",
  "capabilities",
  "store_ready",
  "state",
]) {
  if (!(key in identity.payload)) fail(`health identity lacks ${key}`);
}

// Fixture-only parsing and admission checks; no Runtime is started.
function scopeFailure(request, current) {
  const scope = request.scope;
  if (scope === undefined || scope === null) return "scope_required";
  if (typeof scope !== "object" || Array.isArray(scope) ||
      Object.keys(scope).sort().join(",") !== "account_id,generation,instance_id" ||
      typeof scope.account_id !== "string" || typeof scope.instance_id !== "string" ||
      !Number.isInteger(scope.generation) || scope.generation < 0) return "rpc_body_invalid";
  if (!scope.account_id.trim() || !scope.instance_id.trim() || scope.generation === 0 ||
      scope.account_id !== current.account_id || scope.instance_id !== current.instance_id ||
      scope.generation !== current.generation) return "scope_mismatch";
  return null;
}

const currentScope = Object.fromEntries(["account_id", "instance_id", "generation"]
  .map(key => [key, identity.payload[key]]));
const controlSchema = readJSON(fixturePath("schema/control-contract.schema.json"));

// Evaluate the standard schema keywords used by assets evidence; no native probe runs here.

export { controlSchema, currentScope, identity, scopeFailure };
