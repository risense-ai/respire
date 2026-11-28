const { test } = require("node:test");
const assert = require("node:assert/strict");
const { posix } = require("node:path");
const { i18nMessages, readFrontend } = require("./frontend-source.cjs");

const small = "granite-97m-r2", large = "granite-311m-r2";
const variants = { [small]: ["quint8_avx2", "onnx/model_quint8_avx2.onnx"], [large]: ["quint8_avx2", "onnx/model_quint8_avx2.onnx"], m3: ["q4f16", "onnx/model_q4f16.onnx"] };
const catalogClick = "[data-catalog-model], [data-catalog-variant], [data-catalog-engine], [data-catalog-action]";
const taskContent = "[data-model-task-content]";
const reopenButton = '[data-model="reopen-download"]';
const cancelButton = '[data-model="cancel"]';
const flush = () => new Promise(resolve => setImmediate(resolve));

class Element {
  nodes = new Map();
  handlers = new Map();
  attributes = new Map();
  children = [];
  dataset = {};
  open = false;
  isConnected = true;
  classList = { toggle() {}, add() {}, remove() {} };
  querySelector(selector) {
    if (!this.nodes.has(selector)) this.nodes.set(selector, new Element());
    return this.nodes.get(selector);
  }
  querySelectorAll(selector) { return this.lists?.[selector] || []; }
  set textContent(value) { this.text = value; this.children = []; }
  get textContent() { return this.children.length ? this.children.map(node => node.textContent).join("") : this.text; }
  replaceChildren(...nodes) { this.children = nodes; }
  setAttribute(name, value) { this.attributes.set(name, value); }
  addEventListener(name, handler) {
    const previous = this.handlers.get(name);
    this.handlers.set(name, event => { previous?.(event); return handler(event); });
  }
  emit(name, target = this) { return this.handlers.get(name)({ target, currentTarget: target, preventDefault() {} }); }
  closest() { return null; }
  focus() {}
  cloneNode() { return new Element(); }
  remove() { this.isConnected = false; }
  showModal() { this.open = true; }
  close() { this.open = false; this.emit("close"); }
}

function frontend(globals) {
  const cache = new Map(Object.entries({
    "pages/overview.js": { heading: () => "", phase: value => value || "" },
    "pages/recall.js": { recallChecks: () => () => {} },
    "ui/i18n.js": { t: value => value },
  }));
  function load(file) {
    if (cache.has(file)) return cache.get(file);
    const imports = {}, exports = [];
    const loadRelative = path => load(posix.normalize(posix.join(posix.dirname(file), path)));
    const source = readFrontend(file)
      .replace(/^import \{([^}]+)\} from "([^"]+)";\r?\n/gm, (_, names, path) => {
        const dependency = loadRelative(path);
        for (const name of names.split(",").map(name => name.trim()).filter(Boolean)) {
          assert.ok(Object.hasOwn(dependency, name), `Missing ${name} from ${path}`);
          imports[name] = dependency[name];
        }
        return "";
      })
      .replace(/^export \{([^}]+)\} from "([^"]+)";\r?\n/gm, (_, names, path) => {
        const dependency = loadRelative(path);
        for (const name of names.split(",").map(name => name.trim())) {
          imports[name] = dependency[name];
          exports.push(name);
        }
        return "";
      })
      .replace(/^export (?=(?:async )?(?:function|const|class) ([$\w]+))/gm, (_, name) => {
        exports.push(name);
        return "";
      });
    const scope = { i18nMessages, ...globals, ...imports };
    const module = new Function(...Object.keys(scope), `${source}\nreturn {${exports.join(",")}};`)(...Object.values(scope));
    cache.set(file, module);
    return module;
  }
  return load;
}

function fixture({ model = large, managed = true, installed = false, active = null, rebuild } = {}) {
  const document = new Element(), dialogs = [], streams = [], requests = [], switches = [], patches = [], intervals = new Map();
  const timers = new Map(), clipboard = [];
  const [variant, filename] = variants[model] || ["quantized", "onnx/model_quantized.onnx"];
  const engineState = { model: "quantized", engine: "cpu" };
  const storage = new Map([["respire-model", model]]);
  const ctx = { desktop: { connected: true, platform: "windows", preferences: { modelMirror: "auto" }, task: {},
    health: { instance_id: "fixture", account_id: "account", generation: 1, inference: { phase: "idle" }, capabilities: ["sse.operations.v1"] } },
  status: { summary: {} },
  async savePreferences(patch) { patches.push(patch); Object.assign(this.desktop.preferences, patch); },
  async refresh() { this.refreshes = (this.refreshes || 0) + 1; page?.update(); } };
  let snapshot = { id: "download", busy: true, stage: `Downloading ${filename}`, done: 4096, total: 10240 };
  let runtime = { operation_id: "download", kind: "model_download", message: "Downloading model assets", state: "running", progress_percent: 40 };
  let job = rebuild;
  let page, sequence = 0, probeRuns = 0;
  const globals = {
    document, ctx, CSS: { escape: value => value },
    FormData: class { *[Symbol.iterator]() {} },
    navigator: { clipboard: { async writeText(text) { clipboard.push(text); } } },
    localStorage: { getItem: key => storage.get(key), setItem: (key, value) => storage.set(key, value) },
    CustomEvent: class { constructor(type, { detail }) { Object.assign(this, { type, detail }); } },
    setInterval: (callback, ms) => { intervals.set(++sequence, { callback, ms }); return sequence; },
    clearInterval: id => intervals.delete(id),
    setTimeout: (callback, ms) => { timers.set(++sequence, { callback, ms }); return sequence; },
    clearTimeout: id => timers.delete(id),
    EventSource: class {
      constructor(url) { this.url = url; this.handlers = new Map(); streams.push(this); }
      addEventListener(name, handler) { this.handlers.set(name, handler); }
      close() { this.closed = true; }
      frame(event, data) { this.handlers.get(event)({ data: JSON.stringify({ event, data }) }); }
    },
    fetch: async (url, options) => {
      const command = JSON.parse(options?.body || "null");
      const path = new URL(url, "http://fixture").pathname;
      requests.push({ path, url, command, method: options?.method });
      let result;
      switch (path) {
        case "/api/operations": result = job ? [job] : []; break;
        case "/api/operation": result = job; break;
        case "/api/operation-ack": result = true; break;
        case "/api/model-variants": {
          const id = new URL(url, "http://fixture").searchParams.get("model");
          const names = id.startsWith("granite-") ? ["quint8_avx2", "standard"] :
            ["quantized", "fp16", "int8", "uint8", "q4", "q4f16", "bnb4", "fp32"];
          result = { model: id, tokenizer_installed: id === model && installed,
            variants: names.map(name => ({ name, path: `onnx/${["standard", "fp32"].includes(name) ? "model" : `model_${name}`}.onnx`,
              installed: id === model && installed && name === variant, size: 10240 })) };
          break;
        }
        case "/api/model-profile-state": result = { catalog: { summary: { active } }, progress: { summary: {
          operation: ctx.status.summary.model_operation || { active: false }, switch: { state: "ready" },
        } } }; break;
        case "/api/model-download":
          assert.equal(command.model, model);
          assert.ok([variant, "quantized"].includes(command.variant));
          result = { id: "download", runtime: managed, stream: managed };
          break;
        case "/api/model-probe":
          result = { ok: ++probeRuns === 1, os: "windows", arch: "amd64", client_version: "fixture-client",
            diagnostics: { runtime_version: "fixture-runtime", weights_path: filename, inference_ms: 7, dimensions: 768,
              vector_norm: 1, execution_provider: "CPUExecutionProvider", error: probeRuns > 1 ? "Provider error" : null } };
          break;
        case "/api/model-delete": installed = false; result = { status: "deleted" }; break;
        case "/api/open-model-dir": result = "Opened"; break;
        case "/api/model-download-cancel": break;
        case "/api/task": result = snapshot; break;
        case "/api/runtime-operation": result = runtime; break;
        case "/api/model-switch":
          switches.push(command);
          active = command;
          result = { operation_id: "switch", stream: true };
          break;
        case "/api/execute": {
          if (command.name === "model_cancel") {
            assert.equal(command.args.id, job.id);
            job = { ...job, state: "failed", error: "Rebuild cancelled" };
            ctx.status = { summary: { indexed_count: 4, local_alive: 10 } };
          }
          result = { ...command, state: "done", envelope: { summary: command.name === "model_engine" ? engineState : {} } };
          break;
        }
        default: assert.fail(`Unexpected endpoint ${url}`);
      }
      return Response.json({ ok: true, result });
    },
  };
  document.createElement = tag => {
    const node = new Element();
    if (tag === "dialog") dialogs.push(node);
    return node;
  };
  document.body = { append() {} };
  document.dispatchEvent = event => document.handlers.get(event.type)?.(event);
  const load = frontend(globals);
  const client = readFrontend("client.js");
  const listener = client.slice(client.indexOf('document.addEventListener("respire-runtime-progress"'), client.indexOf("let translationScheduled"));
  const poll = client.slice(client.indexOf("export function pollIntervalMs"), client.indexOf("let pollTimer"));
  const pollIntervalMs = new Function("document", "ctx", "view", `${listener}\n${poll.replace("export ", "")}\nreturn pollIntervalMs;`)(
    document, ctx, { update: () => page?.update() },
  );
  const core = load("ui/core.js"), kit = load("pages/model-kit.js");
  ctx.modelInventory = kit.createModelInventory(() => ctx.desktop);
  let root, live, cancel;
  return {
    ctx, dialogs, streams, requests, switches, patches, document, core, kit, intervals, pollIntervalMs, load, clipboard,
    get root() { return root; }, get live() { return live; }, get cancel() { return cancel; },
    mount() {
      page?.dispose();
      root = new Element();
      live = root.querySelector("[data-live]");
      cancel = live.querySelector(cancelButton);
      cancel.dataset.model = "cancel";
      root.lists = { [cancelButton]: [cancel] };
      page = load("pages/models.js").models(root, ctx);
      return flush();
    },
    click(selector, node = root.querySelector(selector)) {
      root.emit("click", { closest: candidate => candidate === selector ? node : null });
      return flush();
    },
    pick(dataset) { return this.click(catalogClick, { dataset }); },
    dialogClick(name) {
      const dialog = dialogs.at(-1), button = dialog.querySelector(`[data-dl="${name}"]`);
      button.dataset.dl = name;
      dialog.emit("click", { closest: () => button });
      return flush();
    },
    task(state = "running", file = filename) {
      if (state === "completed") installed = true;
      snapshot = { ...snapshot, busy: state === "running", stage: state === "completed" ? i18nMessages.mc0b3fbff51ccc40b : `Downloading ${file}` };
      ctx.desktop.task = snapshot;
      streams.find(stream => stream.url.startsWith("/api/task-events"))?.onmessage({ data: JSON.stringify(snapshot) });
      page?.update();
    },
    frame(state = "running", deliver = true) {
      if (state === "completed") installed = true;
      runtime = { ...runtime, state };
      if (deliver) streams[0].frame(state === "running" ? "progress" : state, runtime);
    },
    async tick(ms) {
      const match = [...timers].find(([, entry]) => entry.ms === ms);
      assert.ok(match, `No ${ms} ms timer`);
      const [id, { callback }] = match;
      timers.delete(id);
      callback();
      await flush();
    },
    completeSwitch() {
      streams.find(stream => stream.url.endsWith("=switch")).frame("completed",
        { operation_id: "switch", kind: "model_switch", state: "completed", progress_percent: 100 });
      return flush();
    },
  };
}

test("model mirror form saves built-in and trimmed custom URLs, and notices dismiss or clear", async () => {
  const f = fixture();
  await f.mount();
  const form = f.root.querySelector("[data-mirror]"), mirror = form.querySelector("[name=mirror]");
  for (const [selected, custom, expected] of [
    ["https://hf-mirror.com", "", "https://hf-mirror.com"],
    ["custom", " https://mirror.example.org ", "https://mirror.example.org"],
  ]) {
    mirror.value = selected;
    await mirror.emit("change");
    assert.equal(form.querySelector("[data-mirror-custom]").hidden, selected !== "custom");
    if (selected === "custom") {
      form.querySelector("[name=mirror-custom]").value = custom;
      await form.emit("submit");
    }
    await flush();
    assert.deepEqual(f.patches.at(-1), { modelMirror: expected });
  }
  form.querySelector("[name=mirror-custom]").value = "https://user:pass@mirror.example.org";
  await form.emit("submit");
  assert.equal(f.patches.length, 2);
  const notice = f.document.querySelector("#notice");
  f.core.notify("Dismiss me", true);
  const dismiss = notice.children.find(node => node.attributes.has("data-notice-dismiss"));
  assert.ok(dismiss);
  dismiss.emit("click");
  assert.equal(notice.hidden, true);
  assert.deepEqual(notice.children, []);
  f.core.notify("Stale");
  await f.core.action(new Element(), () => {
    assert.equal(notice.textContent, "");
    assert.equal(notice.hidden, true);
  });
});

test("curated catalog uses the Granite default and shows a legacy selection read-only", async () => {
  const legacy = "bge-m3-distill-8l";
  const f = fixture({ model: legacy, active: { model: legacy, variant: "quantized", engine: "cpu" } });
  await f.mount();
  const cards = f.root.querySelector("[data-catalog-models]").innerHTML;
  const grid = f.root.querySelector("[data-variants]");
  assert.deepEqual([...cards.matchAll(/data-catalog-model="([^"]+)"/g)].map(match => match[1]),
    [large, small, "m3", "bge-base-zh-v1.5"]);
  assert.match(cards, /data-catalog-model="granite-311m-r2" aria-pressed="true"/);
  assert.equal(f.kit.DEFAULT_MODEL, large);
  assert.equal(f.kit.defaultVariantForModel(small), "quint8_avx2");
  assert.equal(f.kit.defaultVariantForModel("m3"), "quantized");
  assert.ok(f.root.querySelector("[data-legacy-model]").innerHTML.includes(legacy));
  assert.equal((grid.innerHTML.match(/data-catalog-variant/g) || []).length, 2);
  await f.pick({ catalogModel: "m3" });
  assert.equal((grid.innerHTML.match(/data-catalog-variant/g) || []).length, 8);
  assert.match(grid.innerHTML, /data-catalog-variant="quantized" aria-pressed="true"/);
});

test("active weights stay protected when choosing another engine, and probe can re-test", async () => {
  const f = fixture({ installed: true, active: { model: large, variant: "quantized", engine: "cpu" } });
  await f.mount();
  const dock = f.root.querySelector("[data-model-dock]");
  assert.match(dock.innerHTML, /data-catalog-action="enabled" disabled/);
  await f.pick({ catalogAction: "open" });
  assert.deepEqual(f.requests.at(-1).command, { path: "onnx/model_quint8_avx2.onnx" });
  assert.match(dock.innerHTML, /DirectML/);
  await f.pick({ catalogEngine: "npu" });
  assert.match(dock.innerHTML, /data-catalog-engine="npu" aria-pressed="true"/);
  assert.match(dock.innerHTML, /data-catalog-action="delete" disabled/);
  await f.pick({ catalogAction: "probe" });
  const modal = f.dialogs.at(-1);
  const status = modal.querySelector("[data-probe-status]");
  const log = modal.querySelector("[data-probe-log]");
  assert.deepEqual(f.requests.at(-1).command, { model: large, variant: "quint8_avx2", engine: "npu" });
  assert.equal(status.dataset.result, "pass");
  assert.match(log.textContent, /fixture-runtime|execution_provider/);
  const issue = new URL(modal.querySelector("[data-probe-issue]").href);
  assert.equal(issue.pathname, "/risense-ai/respire/issues/new");
  assert.doesNotMatch(issue.searchParams.get("body"), /model_quint8_avx2\.onnx/);
  await modal.querySelector("[data-probe-retry]").emit("click");
  await flush();
  assert.equal(status.dataset.result, "fail");
  assert.match(log.textContent, /Provider error/);
  await modal.querySelector("[data-probe-copy]").emit("click");
  assert.deepEqual(f.clipboard, [log.textContent]);
  modal.querySelector("[data-probe-close]").emit("click");
  assert.equal(modal.isConnected, false);
  assert.deepEqual(f.switches, []);
});

test("deleting an inactive variant requires confirmation and refreshes its download status", async () => {
  const f = fixture({ installed: true });
  await f.mount();
  await f.pick({ catalogAction: "delete" });
  const dialog = f.dialogs.at(-1);
  assert.equal(f.requests.some(({ path }) => path === "/api/model-delete"), false);
  await dialog.querySelector("form").emit("submit");
  await flush();
  const request = f.requests.find(({ path }) => path === "/api/model-delete");
  assert.deepEqual(request.command, { model: large, variant: "quint8_avx2" });
  assert.match(f.root.querySelector("[data-model-dock]").innerHTML, /data-catalog-action="download"/);
});

for (const model of [small, large]) {
  for (const managed of [false, true]) {
    test(`${model} displays task progress via ${managed ? "Runtime SSE" : "host task"}`, async () => {
      const f = fixture({ model, managed });
      const [variant, filename] = variants[model];
      await f.mount();
      assert.ok(f.root.querySelector("[data-variants]").innerHTML.includes(filename.split("/").at(-1)));
      assert.equal(f.kit.variantFromItem("onnx/model_quantized.onnx", f.ctx.modelInventory.get(model)), "");
      assert.equal(f.kit.taskMatchesVariant({ busy: true, stage: "model_quantized.onnx" }, { name: variant, path: filename }), false);
      await f.pick({ catalogAction: "download" });
      const session = f.kit.activeDownloadSession();
      assert.equal(session.variant, variant);
      const dock = f.root.querySelector("[data-model-dock]");
      if (managed) f.frame(); else f.task();
      const panel = f.live.querySelector(taskContent).innerHTML;
      assert.match(f.root.innerHTML, /data-model-task-progress><div data-model-task-content>/);
      assert.match(panel, managed ? /40%.*value="40" max="100"/ : /4\.0 KiB \/ 10\.0 KiB.*value="4096" max="10240"/);
      assert.match(dock.innerHTML, /data-catalog-action="download" disabled/);
      if (managed) f.frame("completed"); else f.task("completed");
      assert.equal(await session.done, true);
      await flush();
      assert.match(dock.innerHTML, /data-catalog-action="enable"/);
      assert.deepEqual(f.switches, []);
    });
  }
}

for (const outcome of ["completed", "cancelled", "failed"]) {
  test(`backgrounded download settles ${outcome} without switching`, async () => {
    const f = fixture();
    await f.mount();
    await f.pick({ catalogAction: "download" });
    const session = f.kit.activeDownloadSession(), [stream] = f.streams;
    f.frame();
    await f.dialogClick("background");
    await f.ctx.refresh();
    assert.equal(session.element.open, false);
    assert.equal(f.live.querySelector(reopenButton).hidden, false);
    assert.equal(f.kit.activeDownloadSession(), session);
    assert.ok(!stream.closed);
    await f.click("[data-model]", { dataset: { model: "reopen-download" } });
    assert.equal(session.element.open, true);
    assert.equal(session.element.querySelector('[data-dl="pause"]').hidden, true);
    assert.equal(session.element.querySelector("[data-dl-bytes]").textContent, "40%");
    await f.dialogClick("background");
    if (outcome === "cancelled") {
      await f.click("[data-model]", { dataset: { model: "reopen-download" } });
      await f.dialogClick("cancel");
      assert.ok(f.requests.some(request => request.method === "DELETE" && request.url === "/api/runtime-operation?id=download"));
    } else f.frame(outcome);
    assert.equal(await session.done, outcome === "completed");
    assert.equal(f.kit.activeDownloadSession(), null);
    assert.equal(stream.closed, true);
    await flush();
    assert.deepEqual(f.switches, []);
    const dock = f.root.querySelector("[data-model-dock]");
    if (outcome === "completed") {
      assert.match(dock.innerHTML, /data-catalog-action="enable"/);
      await f.pick({ catalogAction: "enable" });
      assert.deepEqual(f.switches, [{ model: large, variant: "quint8_avx2", engine: "cpu" }]);
      await f.completeSwitch();
      assert.match(dock.innerHTML, /data-catalog-action="enabled" disabled/);
    } else assert.match(dock.innerHTML, /data-catalog-action="download"/);
  });
}

test("only active foreground work uses the three-second interval with SSE available", () => {
  const { ctx, pollIntervalMs } = fixture();
  assert.equal(pollIntervalMs(ctx, false), 30000);
  ctx.desktop.task = { busy: true };
  assert.equal(pollIntervalMs(ctx, false), 3000);
  ctx.desktop.task = { busy: false };
  for (const summary of [
    { retrieval_index: { running: true } }, { retrieval_index: { scheduled: true } }, { model_operation: { active: true } },
  ]) {
    ctx.status = { summary };
    assert.equal(pollIntervalMs(ctx, false), 3000);
    assert.equal(pollIntervalMs(ctx, true), 30000);
  }
});

test("remounted page tracks a running rebuild, renders counts and cancels its terminal job", async () => {
  const f = fixture({ rebuild: { id: "rebuild", name: "index_rebuild", state: "running" } });
  await f.mount();
  await f.mount();
  assert.deepEqual([...f.intervals.values()].map(interval => interval.ms), [3000]);
  assert.match(f.live.querySelector(taskContent).innerHTML, /<progress\s*>/);
  f.ctx.status.summary = { retrieval_index: { running: true }, indexed_count: 4, local_alive: 10,
    model_operation: { id: "rebuild", active: true, phase: "index", done: 4, total: 10 } };
  await [...f.intervals.values()][0].callback();
  assert.equal(f.ctx.refreshes, 1);
  assert.match(f.live.querySelector("[data-inference-status]").innerHTML, /4 \/ 10/);
  assert.match(f.live.querySelector(taskContent).innerHTML, /value="4" max="10"/);
  assert.equal(f.cancel.hidden, false);
  assert.equal(f.cancel.disabled, false);
  await f.click("[data-model]", f.cancel);
  await f.tick(120);
  assert.ok(f.requests.some(({ command }) => command?.name === "model_cancel"));
  assert.ok(f.requests.some(({ command, path }) => path === "/api/operation-ack" && command.id === "rebuild"));
  assert.equal(f.intervals.size, 0);
  assert.equal(f.cancel.hidden, true);
  assert.doesNotMatch(f.live.querySelector(taskContent).innerHTML, /<progress/);
  assert.match(f.document.querySelector("#notice").textContent, /Rebuild cancelled/);
});

for (const [path, outcome] of [
  ["direct", "completed"], ["background", "completed"], ["close", "completed"],
  ["background", "cancelled"], ["background", "failed"], ["close", "cancelled"], ["close", "failed"],
]) {
  test(`wizard ${path} download ${outcome}`, async () => {
    const f = fixture(), root = new Element();
    const wizard = { state: { model: large, variant: "quint8_avx2", device: "cpu" } };
    const { renderExecute } = f.load("onboarding/wizard-model.js").modelSteps(wizard, { root, ctx: f.ctx, frame: () => "" });
    renderExecute(root.querySelector(".wizard-body"));
    await flush();
    const session = f.kit.activeDownloadSession();
    assert.equal(wizard.state.stages.download, "active");
    if (path === "background") await f.dialogClick("background");
    if (path === "close") {
      session.element.close();
      assert.equal(await session.done, false);
      await flush();
      f.frame(outcome, false);
      await f.tick(1500);
      assert.equal(f.requests.filter(request => request.path === "/api/runtime-operation").length, 2);
    } else {
      f.frame(outcome);
      assert.equal(await session.done, outcome === "completed");
      await flush();
    }
    if (outcome === "completed") {
      assert.equal(wizard.state.stages.download, "done");
      assert.equal(wizard.state.stages.verify, "done");
      assert.equal(f.switches.length, 1);
      await f.completeSwitch();
      assert.equal(wizard.state.stages.index, "done");
    } else {
      assert.equal(wizard.state.stages.download, "failed");
      assert.deepEqual(f.switches, []);
    }
    assert.equal(wizard.state.executing, false);
  });
}
