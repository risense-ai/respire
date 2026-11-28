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
  "granite-97m-r2": { name: "standard", path: "onnx/model.onnx" },
  "granite-311m-r2": { name: "quantized", path: "onnx/model_quint8_avx2.onnx" },
};
const baseline = { model: "m3", variant: "quantized", engine: "cpu" };

export async function startModelsFixture() {
  const preferences = { onboardingCompleted: true, language: "en", modelMirror: "auto" };
  const health = { instance_id: "fixture", account_id: "account", generation: 1,
    capabilities: ["openapi.v1", "sse.operations.v1"], inference: { phase: "idle" } };
  const installed = new Set(["m3"]);
  const jobs = new Map(), downloads = new Map(), runtime = new Map(), subscribers = new Map();
  const switches = [], requests = [], cancellations = [];
  let active = baseline, task = {}, rebuild, indexed = 1, sequence = 0;
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
  function progressDownload(model, percent, state = "running", file = granite[model].path) {
    const download = [...downloads.values()].findLast(entry => entry.model === model && entry.state === "running");
    assert.ok(download, `No running download for ${model}`);
    const { id } = download;
    download.state = state;
    if (state === "completed") installed.add(model);
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
      case "/api/state": return reply({ connected: true, version: "fixture", platform: "windows", health, preferences, task });
      case "/api/runtime-status": return reply(envelope({ indexed_count: indexed, local_alive: total,
        retrieval_index: { running: indexRunning(), state: indexRunning() ? "running" : "idle" }, model_operation: modelOperation() }));
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
        const variant = granite[model] || { name: "quantized", path: "onnx/model_quantized.onnx" };
        return reply({ model, model_dir: `models/${model}`, tokenizer_installed: installed.has(model), cpu_container_ready: true,
          variants: [{ ...variant, size: 102400, bytes: 102400, installed: installed.has(model) }] });
      }
      case "/api/model-profile-state": return reply({ catalog: { summary: { active } }, progress: { summary: { operation: modelOperation(), switch: { state: "ready" } } } });
      case "/api/model-download": {
        const { model, variant } = body;
        assert.equal(variant, granite[model]?.name);
        const download = { id: `download-${++sequence}`, model, state: "running", managed: model === "granite-311m-r2" };
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
        assert.ok(installed.has(body.model));
        switches.push(body);
        active = body;
        const operation_id = `switch-${++sequence}`;
        runtime.set(operation_id, { operation_id, kind: "model_switch", state: "completed", progress_percent: 100 });
        return reply({ operation_id, stream: true });
      }
      case "/api/runtime-operation":
        if (request.method === "DELETE") progressDownload(downloads.get(id).model, 0, "cancelled");
        return reply(runtime.get(id));
      case "/api/runtime-operation-events": return subscribe(id, runtime.get(id), response);
      case "/api/execute": return reply(execute(body));
      case "/api/operation": return reply(jobs.get(id));
      case "/api/operations": return reply([...jobs.values()].filter(job => job.state === "running"));
      case "/api/operation-ack": return reply(jobs.delete(body.id));
      case "/api/inject-targets":
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
    url: `http://127.0.0.1:${server.address().port}/`, errors, requests, switches, cancellations, downloads, installed, health,
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
