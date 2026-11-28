import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { extname, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import frontendSource from "./frontend-source.cjs";

const { i18nMessages } = frontendSource;
const frontend = fileURLToPath(new URL("../internal/desktop/frontend", import.meta.url));
const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml", ".woff2": "font/woff2" };
export const granite = {
  "granite-97m-r2": { name: "quint8_avx2", path: "onnx/model_quint8_avx2.onnx" },
  "granite-311m-r2": { name: "quint8_avx2", path: "onnx/model_quint8_avx2.onnx" },
};
const MODEL_VARIANTS = {
  "granite-311m-r2": [["quint8_avx2", 313421909], ["standard", 1247170481]],
  "granite-97m-r2": [["quint8_avx2", 98247878], ["standard", 390004608]],
  m3: [["quantized", 569694530], ["q4f16", 699932840], ["fp16", 1133992936], ["int8", 571000000], ["uint8", 571000000], ["q4", 1248237611], ["bnb4", 1229364419], ["fp32", 607298]],
  "bge-base-zh-v1.5": [["quantized", 102868746], ["q4f16", 81549931], ["fp16", 203679052], ["int8", 102407793], ["uint8", 102407824], ["q4", 120342974], ["bnb4", 115035086], ["fp32", 406953171]],
};
const variantPath = (name) => (name === "standard" || name === "fp32" ? "onnx/model.onnx" : `onnx/model_${name}.onnx`);
const baseline = { model: "m3", variant: "quantized", engine: "cpu" };

export async function startModelsFixture({ scenario = {} } = {}) {
  const preferences = { onboardingCompleted: true, language: "en", modelMirror: "auto" };
  const health = { instance_id: "fixture", account_id: "account", generation: 1,
    capabilities: ["openapi.v1", "sse.operations.v1"],
    inference: { phase: "idle", foreground: { active: 1, capacity: 2 }, background: { active: 0, capacity: 1 }, queued: 0, worker_pid: 4242 },
    rpc: { executing_until_reply: 0, queued_or_cancelled_pending_removal: 0, recent_timing_micros: [[1200, 7000], [1400, 8000], [1100, 6500], [1500, 8700], [1300, 7600]] } };
  const installed = new Map([["m3", new Set(["quantized"])]]);
  const hasModel = (model) => (installed.get(model)?.size || 0) > 0;
  const jobs = new Map(), downloads = new Map(), runtime = new Map(), subscribers = new Map();
  const switches = [], requests = [], cancellations = [], probes = [], deletions = [];
  let active = baseline, task = {}, rebuild, indexed = 1, sequence = 0, probeFails = false;
  const total = 20;
  const indexRunning = () => rebuild?.state === "running";
  const modelOperation = () => indexRunning()
    ? { id: rebuild.id, active: true, phase: "index", done: indexed, total }
    : { active: false };

  function writeFrame(response, entry) {
    if (entry.operation_id) {
      const event = entry.state === "running" ? "progress" : entry.state;
      response.write(`event: ${event}\ndata: ${JSON.stringify({ event, data: entry })}\n\n`);
    } else response.write(`data: ${JSON.stringify(entry)}\n\n`);
  }
  function publish(id, entry) {
    for (const response of subscribers.get(id) || []) writeFrame(response, entry);
  }
  function subscribe(id, entry, response) {
    assert.ok(entry, `Unknown SSE operation ${id}`);
    response.writeHead(200, { "Content-Type": "text/event-stream", "Cache-Control": "no-cache" });
    if (!subscribers.has(id)) subscribers.set(id, new Set());
    subscribers.get(id).add(response);
    response.on("close", () => subscribers.get(id).delete(response));
    writeFrame(response, entry);
  }
  function progressDownload(model, percent, state = "running", file) {
    const download = [...downloads.values()].findLast(entry => entry.model === model && entry.state === "running");
    assert.ok(download, `No running download for ${model}`);
    const { id, variant } = download;
    file ||= variantPath(variant);
    download.state = state;
    if (state === "completed") {
      if (!installed.has(model)) installed.set(model, new Set());
      installed.get(model).add(variant);
    }
    if (download.managed) {
      const entry = { operation_id: id, kind: "model_download", state, progress_percent: percent, message: "Downloading model assets" };
      runtime.set(id, entry);
      publish(id, entry);
    } else {
      task = { id, busy: state === "running", done: percent * 1024, total: 100 * 1024,
        stage: state === "completed" ? i18nMessages.mc0b3fbff51ccc40b
          : state === "cancelled" ? i18nMessages.ma37778f17c5f3ee5 : `Downloading ${file}` };
      publish(id, task);
    }
  }
  const envelope = (summary = {}, details = []) => ({ status: "ok", summary, details });
  function execute({ id, name, args }) {
    const job = { id, name, state: "done", envelope: envelope() };
    jobs.set(id, job);
    if (Object.hasOwn(scenario.commands ?? {}, name)) {
      Object.assign(job, scenario.commands[name](args));
      return job;
    }
    switch (name) {
      case "index_rebuild":
        rebuild = job;
        job.state = "running";
        break;
      case "model_cancel":
        assert.equal(args.id, rebuild?.id);
        assert.ok(indexRunning());
        cancellations.push(args.id);
        rebuild.state = "failed";
        rebuild.error = "Index rebuild cancelled";
        break;
      case "model_engine":
        job.envelope = envelope({ engine: active.engine, model: active.variant });
        break;
      case "agent_config":
      case "classification_config":
      case "query_log":
      case "diary_list": break;
      default: throw new Error(`Unexpected command ${name}`);
    }
    return job;
  }

  async function route(request, response) {
    const url = new URL(request.url, "http://127.0.0.1");
    const path = url.pathname, id = url.searchParams.get("id");
    if (!path.startsWith("/api/")) {
      if (path === "/wails/runtime.js") {
        response.writeHead(200, { "Content-Type": "text/javascript" }).end();
        return;
      }
      if (path === "/favicon.ico") {
        response.writeHead(204).end();
        return;
      }
      const file = resolve(frontend, "." + (path === "/" ? "/workspace.html" : decodeURIComponent(path)));
      assert.ok(file.startsWith(frontend + sep));
      const contents = await readFile(file);
      response.writeHead(200, { "Content-Type": types[extname(file)] || "application/octet-stream" }).end(contents);
      return;
    }
    let body;
    if (request.method === "POST") {
      let text = "";
      for await (const chunk of request) text += chunk;
      body = JSON.parse(text);
    }
    requests.push({ path, id, method: request.method, body });
    const reply = (result) => response.writeHead(200, { "Content-Type": "application/json" })
      .end(JSON.stringify({ ok: true, result }));
    switch (path) {
      case "/api/state": return reply({ connected: true, version: "fixture", platform: "windows-amd64", health, preferences, task, ...scenario.state });
      case "/api/runtime-status":
        if (scenario.statusError) return response.writeHead(503, { "Content-Type": "application/json" })
          .end(JSON.stringify({ ok: false, error: scenario.statusError }));
        return reply(envelope({ indexed_count: indexed, local_alive: total,
          retrieval_index: { running: indexRunning(), state: indexRunning() ? "running" : "idle" }, model_operation: modelOperation(), ...scenario.status }));
      case "/api/preferences": {
        if (body?.modelMirror && !/^(?:auto$|https?:\/\/[^/]+)/.test(body.modelMirror)) {
          return response.writeHead(200, { "Content-Type": "application/json" })
            .end(JSON.stringify({ ok: false, error: "Model mirror must use HTTP or HTTPS" }));
        }
        Object.assign(preferences, body);
        return reply(preferences);
      }
      case "/api/model-variants": {
        const model = url.searchParams.get("model");
        const defs = MODEL_VARIANTS[model] || [["quantized", 102400]];
        const set = installed.get(model);
        return reply({
          model,
          model_dir: `models/${model}`,
          tokenizer_installed: hasModel(model),
          cpu_container_ready: true,
          variants: defs.map(([name, size]) => {
            const rel = variantPath(name);
            return {
              name, size, bytes: size, installed: Boolean(set?.has(name)),
              path: set?.has(name) ? `models/${model}/${rel}` : "",
            };
          }),
        });
      }
      case "/api/model-profile-state": return reply({ catalog: { summary: { active } }, progress: { summary: { operation: modelOperation(), switch: { state: "ready" } } } });
      case "/api/model-download": {
        const { model, variant } = body;
        assert.ok(MODEL_VARIANTS[model]?.some(([name]) => name === variant), `Unexpected download ${model}:${variant}`);
        const download = { id: `download-${++sequence}`, model, variant, state: "running", managed: model === "granite-311m-r2" };
        downloads.set(download.id, download);
        if (download.managed) progressDownload(model, 40);
        else progressDownload(model, 25, "running", "tokenizer.json");
        return reply({ id: download.id, runtime: download.managed, stream: download.managed });
      }
      case "/api/model-download-cancel":
        progressDownload(downloads.get(body.id).model, 0, "cancelled");
        return reply(true);
      case "/api/task": return reply(task);
      case "/api/task-events": return subscribe(id, task.id === id && task, response);
      case "/api/model-switch": {
        assert.ok(installed.get(body.model)?.has(body.variant));
        switches.push(body);
        active = body;
        const operation_id = `switch-${++sequence}`;
        runtime.set(operation_id, { operation_id, kind: "model_switch", state: "completed", progress_percent: 100 });
        return reply({ operation_id, stream: true });
      }
      case "/api/model-probe": {
        probes.push(body);
        const ep = body.engine === "gpu" ? "DirectML" : body.engine === "npu" ? "OpenVINO" : "CPUExecutionProvider";
        if (probeFails) {
          return reply({
            ok: false, ...body, client_version: "fixture", os: "windows", arch: "amd64",
            diagnostics: { ok: false, execution_provider: ep, error_code: "ep_unavailable", error: "Probe execution failed on device" },
            runtime: { exit: 1 },
          });
        }
        return reply({
          ok: true, ...body, client_version: "fixture", os: "windows", arch: "amd64",
          diagnostics: { ok: true, execution_provider: ep, dimensions: 384, vector_norm: 1.0, load_ms: 14.2, inference_ms: 5.8 },
          runtime: { exit: 0 },
        });
      }
      case "/api/model-delete": {
        if (active.model === body.model && active.variant === body.variant) {
          return response.writeHead(409, { "Content-Type": "application/json" })
            .end(JSON.stringify({ ok: false, error: "model variant is active" }));
        }
        deletions.push(body);
        installed.get(body.model)?.delete(body.variant);
        return reply({ ...body, status: "deleted" });
      }
      case "/api/runtime-operation":
        if (request.method === "DELETE") progressDownload(downloads.get(id).model, 0, "cancelled");
        return reply(runtime.get(id));
      case "/api/runtime-operation-events": return subscribe(id, runtime.get(id), response);
      case "/api/execute": return reply(execute(body));
      case "/api/operation": return reply(jobs.get(id));
      case "/api/operations": return reply(scenario.operations ?? [...jobs.values()].filter(job => job.state === "running"));
      case "/api/operation-ack": return reply(jobs.delete(body.id));
      case "/api/inject-targets": return reply(scenario.agents ?? []);
      case "/api/plugins": return reply([]);
      case "/api/stats": return reply(envelope());
      default: throw new Error(`Unexpected route ${request.method} ${path}`);
    }
  }

  const errors = [];
  const server = createServer((request, response) => {
    route(request, response).catch(error => {
      errors.push(error);
      if (!response.headersSent) response.writeHead(500, { "Content-Type": "application/json" });
      response.end(JSON.stringify({ ok: false, error: error.message }));
    });
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  return {
    url: `http://127.0.0.1:${server.address().port}/`, errors, requests, switches, cancellations, downloads,
    installed, probes, deletions, health, preferences,
    setProbeFailure(value) { probeFails = Boolean(value); },
    get selection() { return active; },
    get rebuild() { return rebuild; },
    progressDownload,
    progressIndex(count) { assert.ok(indexRunning()); indexed = count; },
    close() {
      return new Promise(resolve => {
        server.close(resolve);
        server.closeAllConnections();
      });
    },
  };
}
