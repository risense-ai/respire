const { test } = require("node:test");
const assert = require("node:assert/strict");
const { readFrontend } = require("./frontend-source.cjs");

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
