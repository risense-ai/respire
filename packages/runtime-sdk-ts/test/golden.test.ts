
import { deepEqual, equal } from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createRuntimeClient, type components } from "../src/index.js";

type Schemas = components["schemas"];
const root = new URL("../../../contracts/runtime-api/fixtures/", import.meta.url);
function fixture(name: string): string {
  return readFileSync(new URL(name, root), "utf8");
}

test("round-trip memory and catalog through the generated route types", async () => {
  const memory = fixture("memory-item.json");
  const catalog = fixture("model-catalog-response.json");
  const runtime = createRuntimeClient({
    baseUrl: "http://127.0.0.1:3930",
    fetch: async (request) => {
      const path = new URL(request.url).pathname;
      if (path !== "/v1/memories/memory-golden-001" && path !== "/v1/models/catalog") {
        throw new Error("Unexpected fixture route: " + path);
      }
      return new Response(path.startsWith("/v1/memories/") ? memory : catalog, {
        headers: { "Content-Type": "application/json" },
      });
    },
  });
  const item = await runtime.GET("/v1/memories/{memoryId}", {
    params: { path: { memoryId: "memory-golden-001" } },
  });
  const models = await runtime.GET("/v1/models/catalog");
  const memoryData: Schemas["MemoryItem"] | undefined = item.data;
  const catalogData: Schemas["ModelCatalogResponse"] | undefined = models.data;
  equal(memoryData?.parent_id, null);
  equal(catalogData?.models[0]?.engines[0]?.reason, null);
  deepEqual(JSON.parse(JSON.stringify(memoryData)), JSON.parse(memory));
  deepEqual(JSON.parse(JSON.stringify(catalogData)), JSON.parse(catalog));
});

test("round-trip error and nested SSE event JSON payload", async () => {
  const problem = fixture("problem-details.json");
  const runtime = createRuntimeClient({
    baseUrl: "http://127.0.0.1:3930",
    fetch: async () => new Response(problem, {
      status: 502, headers: { "Content-Type": "application/problem+json" },
    }),
  });
  const response = await runtime.POST("/v1/models/downloads", {
    body: { model_id: "model-golden-001" },
  });
  const error: Schemas["ProblemDetails"] | undefined = response.error;
  equal(error?.code, "model_mirror_fallback_exhausted");
  deepEqual(JSON.parse(JSON.stringify(error)), JSON.parse(problem));

  const source = fixture("operation-event.json");
  const event: Schemas["OperationEvent"] = JSON.parse(source);
  equal(event.event, "failed");
  deepEqual(event.data.error, error);
  deepEqual(JSON.parse(JSON.stringify(event)), JSON.parse(source));
});
