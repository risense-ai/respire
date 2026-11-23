import createClient from "openapi-fetch";
import type { paths } from "./contract.generated.js";

export type { components, operations, paths } from "./contract.generated.js";

export function createRuntimeClient(options: Parameters<typeof createClient<paths>>[0]) {
  return createClient<paths>(options);
}
