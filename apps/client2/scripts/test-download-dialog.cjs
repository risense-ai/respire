// Run the production dialog and API helpers with deterministic browser boundaries.
const { test } = require("node:test");
const assert = require("node:assert/strict");
const { i18nMessages, readFrontend } = require("./frontend-source.cjs");

const core = readFrontend("ui/core.js").replace(/^export /gm, "");
const modal = readFrontend("pages/model-download-modal.js")
  .replace(/^import .*;\r?\n/gm, "")
  .replace(/^export /gm, "");
const build = new Function(
  "document", "fetch", "EventSource", "performance", "setTimeout", "clearTimeout", "i18nMessages",
  core + "\n" + modal + "\nreturn { openDownloadModal, activeDownloadSession, reopenActiveDownloadModal };",
);
const flush = () => new Promise(resolve => setImmediate(resolve));
const ok = result => Response.json({ ok: true, result });
const failure = status => Response.json({ ok: false, error: `status ${status}` }, { status });
const busy = { id: "fixture", busy: true };
const completed = { id: "fixture", busy: false, stage: i18nMessages.mc0b3fbff51ccc40b };
const initial = () => ok({ id: "fixture" });
const button = name => `[data-dl="${name}"]`;

function fixture({ starts = initial, snapshot = () => ok(busy), sse = false } = {}) {
  const timers = new Map(), nodes = new Map(), handlers = new Map();
  const requests = [], streams = [];
  let now = 0, sequence = 0;
  const dialog = {
    open: false,
    showModal() { this.open = true; },
    close() { this.open = false; handlers.get("close")(); },
    addEventListener(name, handler) { handlers.set(name, handler); },
    querySelector(selector) {
      if (!nodes.has(selector)) nodes.set(selector, {});
      return nodes.get(selector);
    },
  };
  const { openDownloadModal, activeDownloadSession, reopenActiveDownloadModal } = build(
    { createElement: () => dialog, body: { append() {} }, dispatchEvent() {} },
    async (url, opts) => {
      requests.push({ url, opts });
      if (url === "/api/model-download") return starts();
	  if (url === "/api/runtime-operation?id=fixture" && opts?.method === "DELETE") return ok({ state: "cancelled" });
      assert.equal(url, "/api/task?id=fixture");
      return snapshot(opts);
    },
    !sse ? undefined : class {
      constructor(url) {
        assert.ok(["/api/task-events?id=fixture", "/api/runtime-operation-events?id=fixture"].includes(url));
		this.handlers = new Map();
        streams.push(this);
      }
      addEventListener(name, handler) { this.handlers.set(name, handler); }
      runtimeFrame(event, state = event) {
        this.handlers.get(event)({ data: JSON.stringify({ event, data: {
          operation_id: "fixture", kind: "model_download", state, progress_percent: 40,
        } }) });
      }
      close() { this.closed = true; }
      frame(task) { this.onmessage({ data: JSON.stringify(task) }); }
    },
    { now: () => now },
    (fn, delay) => timers.set(++sequence, { fn, delay }) && sequence,
    id => timers.delete(id),
    i18nMessages,
  );
  let finished = 0;
  const { done } = openDownloadModal({ model: "m3", variant: "quantized", label: "Test", onDone: () => finished++ });
  return {
    done, dialog, timers, requests, streams,
    activeDownloadSession, reopenActiveDownloadModal,
    finished: () => finished,
    progressText: () => nodes.get("[data-dl-bytes]").textContent,
    error: () => nodes.get("[data-dl-error]"),
    click(name) {
      const target = nodes.get(button(name));
      target.dataset = { dl: name };
      handlers.get("click")({ target: { closest: () => target } });
    },
    advance(ms) { now += ms; },
    async runTimer(delay) {
      assert.equal(timers.size, 1);
      const [[id, entry]] = timers;
      assert.equal(entry.delay, delay);
      now += delay;
      timers.delete(id);
      entry.fn();
      await flush();
    },
  };
}

test("backgrounded host download keeps its observer and can reopen until completion", async () => {
  const f = fixture({ sse: true });
  await flush();
  f.click("background");
  assert.equal(f.dialog.open, false);
  assert.equal(f.finished(), 0);
  assert.equal(f.activeDownloadSession().done, f.done);
  f.streams[0].frame({ ...busy, done: 1024, total: 2048 });
  assert.equal(f.progressText(), "1.0 KiB / 2.0 KiB");
  f.reopenActiveDownloadModal();
  assert.equal(f.dialog.open, true);
  f.click("background");
  f.streams[0].frame(completed);
  assert.equal(await f.done, true);
  assert.equal(f.finished(), 1);
  assert.equal(f.activeDownloadSession(), null);
  assert.equal(f.streams[0].closed, true);
});

test("backgrounded Runtime download shows percentage and settles on failure", async () => {
  const f = fixture({ sse: true, starts: () => ok({ id: "fixture", runtime: true, stream: true }) });
  await flush();
  f.click("background");
  f.streams[0].runtimeFrame("progress", "running");
  assert.equal(f.progressText(), "40%");
  assert.equal(f.finished(), 0);
  f.streams[0].runtimeFrame("failed");
  assert.equal(await f.done, false);
  assert.equal(f.finished(), 1);
  assert.equal(f.activeDownloadSession(), null);
});

test("managed download consumes Runtime SSE without the host watchdog or snapshots", async () => {
  const f = fixture({ sse: true, starts: () => ok({ id: "fixture", runtime: true, stream: true }) });
  await flush();
  f.streams[0].runtimeFrame("progress", "running");
  assert.equal(f.timers.size, 0);
  f.streams[0].runtimeFrame("completed");
  assert.equal(await f.done, true);
  assert.equal(f.requests.length, 1);
  assert.equal(f.streams[0].closed, true);
});

test("managed download cancellation reaches Runtime DELETE", async () => {
  const f = fixture({ sse: true, starts: () => ok({ id: "fixture", runtime: true, stream: true }) });
  await flush();
  f.click("cancel");
  await flush();
  assert.equal(await f.done, false);
  assert.equal(f.requests[1].url, "/api/runtime-operation?id=fixture");
  assert.equal(f.requests[1].opts.method, "DELETE");
  assert.equal(f.streams[0].closed, true);
});

test("closing before a managed receipt arrives never creates an observer", async () => {
  let release;
  const pending = new Promise(resolve => { release = resolve; });
  const f = fixture({ sse: true, starts: () => pending });
  f.dialog.close();
  release(ok({ id: "fixture", runtime: true, stream: true }));
  await flush();
  assert.equal(await f.done, false);
  assert.equal(f.streams.length, 0);
  assert.equal(f.timers.size, 0);
});

test("rejected start can be dismissed and retried without extra snapshots", async () => {
  const dismissed = fixture({ starts: () => failure(409) });
  await flush();
  dismissed.click("dismiss");
  assert.equal(await dismissed.done, false);
  let attempts = 0;
  const retry = fixture({ starts: () => ++attempts === 1 ? failure(409) : initial() });
  await flush();
  retry.click("retry");
  await flush();
  assert.equal(retry.requests.length, 3);
  assert.equal(retry.error().hidden, true);
  retry.dialog.close();
  assert.equal(await retry.done, false);
  assert.equal(retry.timers.size, 0);
});

test("activity watchdog falls back to polling after a valid SSE frame", async () => {
  const f = fixture({ sse: true, snapshot: () => ok(completed) });
  await flush();
  const [source] = f.streams;
  f.advance(1500);
  source.frame(busy);
  f.advance(5000);
  source.frame(busy);
  await f.runTimer(10_000);
  assert.equal(source.closed, true);
  assert.equal(await f.done, true);
  source.frame(busy);
  source.onerror();
  assert.equal(f.requests.length, 2);
  assert.equal(f.timers.size, 0);
});

test("first-frame timeout switches buffered SSE to polling", async () => {
  const f = fixture({ sse: true, snapshot: () => ok(completed) });
  await flush();
  await f.runTimer(2000);
  assert.equal(await f.done, true);
});

test("overall deadline still expires on an active SSE stream", async () => {
  const f = fixture({ sse: true });
  await flush();
  f.advance(61 * 60_000 - 20);
  f.streams[0].frame(busy);
  await f.runTimer(20);
  assert.equal(await f.done, false);
  assert.equal(f.error().textContent, i18nMessages.m23dfe4404e8a5ebe);
  assert.equal(f.requests.length, 1);
});

test("busy poll cannot extend the overall deadline", async () => {
  let f;
  f = fixture({ snapshot: async () => {
    await flush();
    f.advance(61 * 60_000);
    return ok(busy);
  } });
  for (let i = 0; i < 2; i++) await flush();
  assert.equal(await f.done, false);
  assert.equal(f.error().textContent, i18nMessages.m23dfe4404e8a5ebe);
  assert.equal(f.requests.length, 2);
  assert.equal(f.timers.size, 0);
});

for (const status of [403, 404, 410, 500]) {
  test(`HTTP ${status} is surfaced without retry`, async () => {
    const f = fixture({ snapshot: () => failure(status) });
    await flush();
    assert.equal(await f.done, false);
    assert.equal(f.error().textContent, `status ${status}`);
    assert.equal(f.requests.length, 2);
    assert.equal(f.timers.size, 0);
  });
}

test("invalid JSON or a wrong task identity is surfaced without retry", async () => {
  for (const response of [new Response("{"), ok({ ...busy, id: "wrong" })]) {
    const f = fixture({ snapshot: () => response });
    await flush();
    assert.equal(await f.done, false);
    assert.equal(f.error().textContent, i18nMessages.m23dfe4404e8a5ebe);
    assert.equal(f.timers.size, 0);
  }
});

test("transient status can recover, but persistent failures stop after three attempts", async () => {
  let attempts = 0;
  const recovering = fixture({ snapshot: () => ++attempts === 1 ? failure(503) : ok(completed) });
  await flush();
  await recovering.runTimer(700);
  assert.equal(await recovering.done, true);

  const failing = fixture({ snapshot: () => failure(503) });
  await flush();
  await failing.runTimer(700);
  await failing.runTimer(700);
  assert.equal(await failing.done, false);
  assert.equal(failing.error().textContent, "status 503");
  assert.equal(failing.requests.length, 4);
  assert.equal(failing.timers.size, 0);
});

test("status-less fetch rejection can recover within the existing retry budget", async () => {
  let attempts = 0;
  const offline = () => { throw new TypeError("Failed to fetch"); };
  const recovering = fixture({ snapshot: () => ++attempts === 1 ? offline() : ok(completed) });
  await flush();
  await recovering.runTimer(700);
  assert.equal(await recovering.done, true);

  const failing = fixture({ snapshot: offline });
  await flush();
  await failing.runTimer(700);
  await failing.runTimer(700);
  assert.equal(await failing.done, false);
  assert.equal(failing.requests.length, 4);
  assert.equal(failing.timers.size, 0);
});

test("response decoding TypeError does not enter the fetch retry budget", async () => {
  const f = fixture({ snapshot: () => ({
    async json() { throw new TypeError("Unable to decode response"); },
  }) });
  await flush();
  assert.equal(await f.done, false);
  assert.equal(f.error().textContent, i18nMessages.m23dfe4404e8a5ebe);
  assert.equal(f.requests.length, 2);
  assert.equal(f.timers.size, 0);
});

test("closing the dialog aborts an in-flight snapshot", async () => {
  const f = fixture({ snapshot: ({ signal }) => new Promise((resolve, reject) => {
    signal.addEventListener("abort", () => reject(signal.reason), { once: true });
  }) });
  await flush();
  f.dialog.close();
  await flush();
  assert.equal(await f.done, false);
  assert.equal(f.requests[1].opts.signal.aborted, true);
  assert.equal(f.timers.size, 0);
});
