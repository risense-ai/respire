import { controlSchema } from "./runtime-control-fixture-identity.mjs";

function assetsSchemaValid(value, schema) {
  if (schema.$ref) return assetsSchemaValid(value,
    controlSchema.$defs[schema.$ref.replace("#/$defs/", "")]);
  if (schema.const !== undefined && value !== schema.const) return false;
  if (schema.enum && !schema.enum.includes(value)) return false;
  if (schema.type === "object" && (!value || typeof value !== "object" || Array.isArray(value))) return false;
  if (schema.type === "array" && !Array.isArray(value)) return false;
  if (schema.type === "string" && typeof value !== "string") return false;
  if (schema.type === "boolean" && typeof value !== "boolean") return false;
  if (schema.type === "integer" && !Number.isInteger(value)) return false;
  if (schema.minimum !== undefined && value < schema.minimum) return false;
  if (schema.maximum !== undefined && value > schema.maximum) return false;
  if (schema.minLength !== undefined && value.length < schema.minLength) return false;
  if (schema.pattern && !new RegExp(schema.pattern).test(value)) return false;
  if (schema.required?.some(key => !Object.hasOwn(value, key))) return false;
  if (schema.additionalProperties === false &&
      Object.keys(value).some(key => !Object.hasOwn(schema.properties, key))) return false;
  for (const [key, child] of Object.entries(schema.properties ?? {})) {
    if (Object.hasOwn(value, key) && !assetsSchemaValid(value[key], child)) return false;
  }
  if (schema.minItems !== undefined && value.length < schema.minItems) return false;
  if (schema.maxItems !== undefined && value.length > schema.maxItems) return false;
  if (schema.items && !value.every(item => assetsSchemaValid(item, schema.items))) return false;
  if (schema.uniqueItems && new Set(value.map(item => JSON.stringify(item))).size !== value.length) return false;
  if (schema.allOf?.some(child => !assetsSchemaValid(value, child))) return false;
  if (schema.not && assetsSchemaValid(value, schema.not)) return false;
  if (schema.if) {
    const branch = assetsSchemaValid(value, schema.if) ? schema.then : schema.else;
    if (branch && !assetsSchemaValid(value, branch)) return false;
  }
  return true;
}
function assetsFailure(payload) {
  if (!assetsSchemaValid(payload, controlSchema.$defs.assetsStatus)) return "assets_status_invalid";
  const evidence = payload.backend_load?.evidence;
  if (evidence && ((evidence.selected.startsWith("Npu: ") &&
      evidence.provider !== evidence.selected.slice(5)) ||
      new Set(evidence.modules.map(module => module.id)).size !== evidence.modules.length)) {
    return "assets_status_invalid";
  }
  return null;
}

export { assetsFailure };
