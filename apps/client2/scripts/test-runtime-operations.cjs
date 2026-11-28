const { test } = require("node:test");
const assert = require("node:assert/strict");
const { readFrontend, i18nMessages } = require("./frontend-source.cjs");

function fixture({ replies = [] } = {}) {
  const streams = [], events = [], requests = [], timers = [];
  class EventSource {
    constructor(url) { this.url = url; this.handlers = new Map(); streams.push(this); }
    addEventListener(name, handler) { this.handlers.set(name, handler); }
    close() { this.closed = true; }
    frame(name, state = name, id = "op") {
      this.handlers.get(name)({ data: JSON.stringify({ event: name, data: {
        operation_id: id, state, progress_percent: 40, message: "Working",
        ...(name === "failed" ? { error: { detail: "Download checksum failed" } } : {}),
      } }) });
    }
  }
  const core = readFrontend("ui/core.js").replace(/^export /gm, "");
  const helpers = new Function("document", "CustomEvent", "EventSource", "fetch", "setTimeout",
    core + "\nreturn {watchRuntimeOperation, cancelRuntimeOperation, observeRuntimeSwitch, currentRuntimeSwitch};")(
    { dispatchEvent(event) { events.push(event); } },
    class { constructor(type, init) { this.type = type; this.detail = init.detail; } }, EventSource,
    async (url, options) => { requests.push({ url, options }); return Response.json(replies.shift()); },
    (fn, ms) => { timers.push(ms); queueMicrotask(fn); },
  );
  return { ...helpers, streams, events, requests, timers };
}

test("Runtime SSE delivers progress and completion without state or snapshot polling", async () => {
  const f = fixture(), progress = [];
  const watch = f.watchRuntimeOperation({ operation_id: "op", stream: true }, data => progress.push(data));
  const [source] = f.streams;
  assert.equal(source.url, "/api/runtime-operation-events?id=op");
  source.frame("progress", "running");
  assert.equal(progress[0].progress_percent, 40);
  source.frame("completed");
  assert.equal((await watch.done).state, "completed");
  assert.equal(source.closed, true);
  assert.equal(f.requests.length, 0);
  assert.deepEqual(f.timers, []);
  assert.deepEqual(f.events.map(event => event.type), ["respire-runtime-progress", "respire-runtime-progress", "respire-operation"]);
});

for (const terminal of ["failed", "cancelled"]) {
  test(`Runtime ${terminal} is exposed without inventing success`, async () => {
    const f = fixture();
    const watch = f.watchRuntimeOperation({ operation_id: "op", stream: true });
    f.streams[0].frame(terminal);
    await assert.rejects(watch.done, terminal === "failed" ? /Download checksum failed/ : { cancelled: true });
    assert.equal(f.streams[0].closed, true);
    assert.equal(f.requests.length, 0);
  });
}

test("SSE transport failure is explicit and never starts a hidden poller", async () => {
  const f = fixture();
  const watch = f.watchRuntimeOperation({ operation_id: "op", stream: true });
  f.streams[0].onerror();
  await assert.rejects(watch.done, /stream disconnected/);
  assert.equal(f.streams[0].closed, true);
  assert.deepEqual(f.timers, []);
  assert.equal(f.requests.length, 0);
});

test("wrong SSE operation identity is rejected", async () => {
  const f = fixture();
  const watch = f.watchRuntimeOperation({ operation_id: "op", stream: true });
  f.streams[0].frame("completed", "completed", "other");
  await assert.rejects(watch.done, /Invalid Runtime operation/);
});

test("snapshot-only Runtime uses operation snapshots", async () => {
  const f = fixture({ replies: [
    { ok: true, result: { operation_id: "op", state: "running" } },
    { ok: true, result: { operation_id: "op", state: "completed" } },
  ] });
  assert.equal((await f.watchRuntimeOperation({ operation_id: "op", stream: false }).done).state, "completed");
  assert.equal(f.streams.length, 0);
  assert.deepEqual(f.requests.map(request => request.url), ["/api/runtime-operation?id=op", "/api/runtime-operation?id=op"]);
  assert.deepEqual(f.timers, [1000]);
});

test("cancellation sends DELETE through the desktop boundary", async () => {
  const f = fixture({ replies: [{ ok: true, result: { state: "cancelled" } }] });
  await f.cancelRuntimeOperation("operation/a");
  assert.deepEqual(f.requests, [{ url: "/api/runtime-operation?id=operation%2Fa", options: {
    method: "DELETE", headers: { "X-Respire-Client": "1" },
  } }]);
});

test("profile switch waits on Runtime SSE instead of configuring or polling", async () => {
  const f = fixture();
  const source = readFrontend("pages/model-kit.js");
  const body = source.slice(source.indexOf("export async function submitProfileSwitch("), source.indexOf("// profileStateSettled"));
  const calls = [];
  const submit = new Function("api", "observeRuntimeSwitch", body.replace(/^export /gm, "") + "\nreturn submitProfileSwitch;")(
    async (path, body) => { calls.push({ path, body }); return { operation_id: "op", stream: true }; }, f.observeRuntimeSwitch,
  );
  const pending = submit("m3", "cpu", "quantized");
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(calls.length, 1);
  assert.equal(calls[0].path, "model-switch");
  f.streams[0].frame("completed");
  assert.equal((await pending).settled, true);
  assert.deepEqual(f.timers, []);
});

test("legacy profile switch returns its receipt for existing progress fallback", async () => {
  const source = readFrontend("pages/model-kit.js");
  const body = source.slice(source.indexOf("export async function submitProfileSwitch("), source.indexOf("// profileStateSettled"));
  const submit = new Function("api", "watchRuntimeOperation", body.replace(/^export /gm, "") + "\nreturn submitProfileSwitch;")(
    async () => ({}), () => { throw Error("legacy Runtime must not use operation SSE"); },
  );
  assert.deepEqual(await submit("model", "gpu", "fp16"), {});
});

test("a remounted model page reuses the pending stream and its terminal receipt", async () => {
  const f = fixture();
  const receipt = { operation_id: "op", stream: true };
  const original = f.observeRuntimeSwitch(receipt, { model: "m3", engine: "cpu", variant: "quantized" });
  f.streams[0].frame("progress", "running");
  assert.equal(f.currentRuntimeSwitch().progress.progress_percent, 40);
  const resumed = f.observeRuntimeSwitch(receipt, { model: "m3" });
  assert.equal(original, resumed);
  assert.equal(f.streams.length, 1);
  f.streams[0].frame("completed");
  assert.equal((await resumed).state, "completed");
  assert.equal(f.currentRuntimeSwitch(), null);
});

function loadFunctions(file, dependencies, exports) {
  const source = readFrontend(file).replace(/^(?:import [\s\S]*?|export \{[^\n]*\} from [^\n]*);\r?\n/gm, "").replace(/^export /gm, "");
  return new Function(...Object.keys(dependencies), `${source}\nreturn {${exports}};`)(...Object.values(dependencies));
}

function modelControls(f) {
  const { $, h, bytes, number } = loadFunctions("ui/core.js", {}, "$, h, bytes, number");
  const { readProfileState } = loadFunctions("pages/model-kit.js", { i18nMessages }, "readProfileState");
  const { activeDownloadSession } = loadFunctions("pages/model-download-modal.js", {}, "activeDownloadSession");
  return loadFunctions("pages/models-view.js", { i18nMessages, $, h, bytes, number, readProfileState,
    activeDownloadSession, currentRuntimeSwitch: f.currentRuntimeSwitch, phase: value => value },
  "liveModelOperation, modelOperationProgress, syncModelCancel, cancellableModelID, nextSwitchStallState, switchStep");
}

test("latency uses Runtime queue and execution timings with a flat empty baseline", () => {
  const { rpcLatency } = loadFunctions("pages/models-status.js", {}, "rpcLatency");
  assert.deepEqual(rpcLatency([]), { latest: undefined, average: undefined, points: "1,23.00 99,23.00" });
  const timings = rpcLatency([[250, 750], null, [1000, 2000]]);
  assert.equal(timings.latest, 3);
  assert.equal(timings.average, 2);
  assert.equal(timings.points, "1.00,15.67 99.00,1.00");
});

test("switch phases take precedence over SSE percentages", () => {
  const { switchStep } = modelControls(fixture());
  assert.deepEqual(["switch-verify", "switch-load", "switch-index", "switch-diary"].map(phase =>
    switchStep({ phase, progress_percent: 90 })), [0, 1, 2, 3]);
  assert.deepEqual([5, 15, 20, 80].map(progress_percent => switchStep({ phase: "running", progress_percent })), [0, 1, 2, 3]);
});

test("advancing switch progress keeps the poll alive beyond 600 ticks", () => {
  const { nextSwitchStallState } = modelControls(fixture());
  let state = { key: "", ticks: 0 };
  const op = { id: "switch", active: true, phase: "switch-index", done: 0 };
  for (let tick = 0; tick < 1200; tick += 1) {
    if (tick === 600) op.done += 1;
    state = nextSwitchStallState(state.key, state.ticks, op);
    assert.equal(state.expired, false);
  }
  op.phase = "switch-diary";
  state = nextSwitchStallState(state.key, state.ticks, op);
  assert.equal(state.ticks, 0);
  assert.equal(state.expired, false);
});

test("unchanged switch progress expires at idle tick 601", () => {
  const { nextSwitchStallState } = modelControls(fixture());
  const op = { id: "switch", active: true, phase: "switch-index", done: 4, elapsed: 0 };
  let state = nextSwitchStallState("", 0, op);
  for (let tick = 1; tick <= 600; tick += 1) {
    op.elapsed += 3;
    state = nextSwitchStallState(state.key, state.ticks, op);
    assert.equal(state.expired, false);
  }
  state = nextSwitchStallState(state.key, state.ticks, op);
  assert.deepEqual(state, { key: "switch|switch-index|4||", ticks: 601, expired: true });
});

test("model cancel button stays hidden until an active cancellable operation appears", () => {
  const { syncModelCancel } = modelControls(fixture());
  const button = { hidden: false, disabled: false };
  const root = { querySelector: () => button };
  syncModelCancel(root, {}, { active: false });
  assert.equal(button.hidden, true);
  assert.equal(button.disabled, true);
  syncModelCancel(root, {}, { active: true, id: "op-1", cancellable: true });
  assert.equal(button.hidden, false);
  assert.equal(button.disabled, false);
});

test("fresh switch profile renders indexed items and elapsed time over a stale status snapshot", () => {
  const { liveModelOperation, modelOperationProgress } = modelControls(fixture());
  const snapshot = { id: "switch", active: true, phase: "switch-load", done: 0 };
  const operation = { id: "switch", active: true, phase: "switch-index", item: "granite", done: 24, total: 80, elapsed: 7, elapsed_secs: 99 };
  const view = { profileState: { progress: { summary: { operation } } }, profileStatus: snapshot };
  const status = { summary: { model_operation: snapshot } };
  const html = modelOperationProgress(liveModelOperation(view, status));
  assert.match(html, /granite.*24 \/ 80 \(30%\).*<span>7 /);
  assert.doesNotMatch(html, /[KM]iB|99/);
  assert.match(modelOperationProgress({ ...operation, total: null }), /<span>24<\/span>/);
  assert.match(modelOperationProgress({ phase: "verify", done: 1024, total: 2048 }), /1\.0 KiB \/ 2\.0 KiB/);
  view.profileState.progress.summary.operation = { active: false };
  assert.deepEqual(liveModelOperation(view, status), {});
});

test("overview uses the shared progress formatter for active model operations", () => {
  const source = readFrontend("pages/overview.js");
  assert.match(source, /import \{ modelOperationProgress, runtimeDownloadOperation \} from "\.\/models-view\.js"/);
  assert.match(source, /op\.active \? modelOperationProgress\(op\) : ""/);
  assert.doesNotMatch(source, /bytes\(op\.done\)/);
});

test("the three-second profile poll publishes fresh counts and stops at idle", async () => {
  const controls = modelControls(fixture());
  const { readProfileState, profileStateSettled } = loadFunctions("pages/model-kit.js", { i18nMessages }, "readProfileState, profileStateSettled");
  const source = readFrontend("pages/models.js");
  const acceptSrc = source.slice(source.indexOf("  function acceptProfileState("), source.indexOf("  async function load("));
  const polling = source.slice(source.indexOf("  function syncProfilePolling("), source.indexOf("  async function runIndexedAction("));
  const snapshot = { active: true, id: "switch", phase: "queued", done: 0 };
  const operation = { active: true, id: "switch", phase: "switch-index", done: 4, total: 10, elapsed: 6 };
  const ctx = { status: { summary: { model_operation: snapshot } }, refresh: async () => {} };
  const view = { modelID: "granite-311m-r2", profileSwitchPending: false, switchInFlight: false, indexInFlight: false };
  let tick, interval;
  const start = new Function("view", "ctx", "fetchProfileState", "readProfileState", "profileStateSettled",
    "setInterval", "clearInterval", "liveModelOperation", "nextSwitchStallState", "i18nMessages", "notify", "activeDownloadSession", `
    let disposed = false, profileRefreshing = false, switchTimer;
    let switchProgress = { key: "", ticks: 0 };
    const scope = () => "scope";
    const isCurrent = (owner) => !disposed && owner === scope();
    ${acceptSrc}
    ${polling}
    function update() { syncProfilePolling(); }
    return update;
  `)(view, ctx, async () => ({ progress: { summary: { operation: { ...operation }, switch: { state: "pending" } } } }),
    readProfileState, profileStateSettled,
    (fn, ms) => { tick = fn; interval = ms; return 1; }, id => { assert.equal(id, 1); tick = null; switchTimer = null; },
    controls.liveModelOperation, controls.nextSwitchStallState, i18nMessages, assert.fail, () => null);
  start();
  assert.equal(interval, 3000);
  await tick();
  assert.deepEqual(view.runtimeProgress, operation);
  assert.deepEqual(readProfileState(view.profileState).operation, operation);
  assert.equal(controls.liveModelOperation(view, ctx.status).done, 4);
  operation.active = false;
  await tick();
  assert.equal(tick, null);
});

test("model cancel button uses the active operation ID across snapshots and streams", async () => {
  const f = fixture();
  const controls = modelControls(f);
  const snapshot = { active: true, id: "status-task" };
  const profile = { active: true, id: "profile-task" };
  const button = { dataset: { model: "cancel" }, hidden: false, disabled: false };
  const root = { querySelector: () => button, addEventListener: (_, fn) => { root.click = fn; } };
  const calls = [];
  const { bindModelActions } = loadFunctions("pages/models-actions.js", {
    ...controls, i18nMessages, action: (_, run) => run(), execute: async (...args) => { calls.push(args); },
    reopenActiveDownloadModal: () => {},
  }, "bindModelActions");

  const observed = f.observeRuntimeSwitch({ operation_id: "receipt-task", stream: true }, { model: "granite-311m-r2" });
  const receiptOperation = controls.liveModelOperation({}, {});
  assert.equal(receiptOperation.id, "receipt-task");
  f.streams[0].frame("completed", "completed", "receipt-task");
  await observed;

  for (const [view, status, id] of [
    [{}, { summary: { model_operation: snapshot } }, "status-task"],
    [{ profileState: { progress: { details: { operation: profile } } } }, {}, "profile-task"],
    [{ runtimeProgress: { operation_id: "runtime-task", state: "running" } }, {}, "runtime-task"],
    [{ runtimeProgress: { ...receiptOperation, id: "receipt-task" } }, {}, "receipt-task"],
    [{ runtimeProgress: { ...profile, cancellable: false } }, {}, undefined],
    [{ runtimeProgress: { id: "finished", state: "completed" } }, {}, undefined],
  ]) {
    controls.syncModelCancel(root, view, controls.liveModelOperation(view, status));
    assert.equal(button.disabled, !id);
    assert.equal(button.hidden, !id);
    bindModelActions(view, { root, ctx: { status }, refreshProfileState: async () => {} });
    root.click({ target: { closest: selector => selector === "[data-model]" ? button : null } });
    await new Promise(resolve => setImmediate(resolve));
  }
  assert.deepEqual(calls, ["status-task", "profile-task", "runtime-task", "receipt-task"].map(id => ["model_cancel", { id }]));
  assert.match(readFrontend("pages/models.js"), /data-model-task-progress[\s\S]*data-model="cancel"/);
});
