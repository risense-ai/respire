import { messages as i18nMessages } from "./i18n/messages.js";
export const $ = (selector, root = document) => root.querySelector(selector);
export const all = (selector, root = document) => [
  ...root.querySelectorAll(selector),
];
export const h = (value) =>
  String(value ?? "").replace(
    /[&<>"']/g,
    (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[
        c
      ],
  );
export const number = (value) =>
  typeof value === "number" ? value.toLocaleString() : "—";
export const date = (value) => (value ? new Date(value).toLocaleString() : "—");
export const bytes = (value) =>
  typeof value === "number"
    ? value < 1048576
      ? `${(value / 1024).toFixed(1)} KiB`
      : `${(value / 1048576).toFixed(1)} MiB`
    : "—";
export const summary = (envelope) => envelope?.summary || {};
export function rows(envelope, key) {
  if (Array.isArray(envelope?.details)) return envelope.details;
  if (key && Array.isArray(envelope?.details?.[key]))
    return envelope.details[key];
  if (key && Array.isArray(envelope?.summary?.[key]))
    return envelope.summary[key];
  return [];
}
export const raw = (value) => JSON.stringify(value, null, 2);
export function notify(message, error = false) {
  const node = typeof document === "undefined" ? null : document.querySelector?.("#notice");
  if (!node) return;
  node.classList.toggle("error", Boolean(message && error));
  node.hidden = !message;
  if (!message || !document.createElement) {
    node.textContent = message || "";
    return;
  }
  if (node.firstElementChild) {
    node.firstElementChild.textContent = message;
    return;
  }
  const text = document.createElement("span");
  text.textContent = message;
  const dismiss = document.createElement("button");
  dismiss.type = "button";
  dismiss.className = "notice-dismiss";
  dismiss.setAttribute("data-notice-dismiss", "");
  dismiss.setAttribute("aria-label", i18nMessages.mNoticeDismiss);
  dismiss.textContent = "×";
  dismiss.addEventListener("click", () => notify(""));
  node.replaceChildren(text, dismiss);
}
export class ApiTransportError extends TypeError {}

export async function api(path, body, signal) {
  const response = await fetch(
    "/api/" + path,
    body === undefined
      ? { signal }
      : {
          method: "POST",
          headers: {
            "Content-Type": "application/json",
            "X-Respire-Client": "1",
          },
          body: JSON.stringify(body),
        },
  ).catch((error) => {
    // Only a failure to fetch is retryable, not an error decoding the body.
    if (error instanceof TypeError && error.status == null) {
      throw new ApiTransportError(error.message, { cause: error });
    }
    throw error;
  });
  const data = await response.json();
  if (!response.ok || !data.ok)
    throw Object.assign(new Error(data.error || `${i18nMessages.md8804b190e701500}${response.status})`), { status: response.status });
  return data.result;
}
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
export function watchRuntimeOperation(receipt, onProgress) {
  const id = receipt.operation_id || receipt.id;
  let source, closed = false, rejectDone;
  const close = () => { closed = true; source?.close(); };
  const done = new Promise((resolve, reject) => {
    rejectDone = reject;
    const fail = (error) => { close(); reject(error); };
    const receive = (event) => {
      const data = event.data;
      if (closed) return;
      if (data?.operation_id !== id || !["progress", "completed", "failed", "cancelled"].includes(event.event))
        return fail(new Error("Invalid Runtime operation event"));
      onProgress?.(data);
      document.dispatchEvent(new CustomEvent("respire-runtime-progress", { detail: data }));
      if (event.event === "progress") return;
      close();
      document.dispatchEvent(new CustomEvent("respire-operation", { detail: data }));
      if (event.event === "completed") resolve(data);
      else reject(Object.assign(new Error(data.error?.detail || data.error?.title || data.message || `Runtime operation ${event.event}`),
        { cancelled: event.event === "cancelled" }));
    };
    if (receipt.stream) {
      try {
        source = new EventSource("/api/runtime-operation-events?id=" + encodeURIComponent(id));
        for (const type of ["progress", "completed", "failed", "cancelled"])
          source.addEventListener(type, (event) => {
            try { receive(JSON.parse(event.data)); } catch (error) { fail(error); }
          });
        source.addEventListener("transport-error", (event) => {
          try { fail(new Error(JSON.parse(event.data).error)); } catch (error) { fail(error); }
        });
        source.onerror = () => fail(new Error("Runtime operation stream disconnected"));
      } catch (error) { fail(error); }
    } else {
      (async () => {
        while (!closed) {
          const data = await api("runtime-operation?id=" + encodeURIComponent(id));
          receive({ event: ["pending", "running"].includes(data.state) ? "progress" : data.state, data });
          if (!closed) await pause(1000);
        }
      })().catch(fail);
    }
  });
  return { done, close() { close(); rejectDone(new Error("Runtime operation observation closed")); } };
}

export async function cancelRuntimeOperation(id) {
  const response = await fetch("/api/runtime-operation?id=" + encodeURIComponent(id), {
    method: "DELETE", headers: { "X-Respire-Client": "1" },
  });
  const reply = await response.json();
  if (!response.ok || !reply.ok) throw new Error(reply.error || "Runtime cancellation rejected");
  return reply.result;
}

let observedSwitch = null;
export const currentRuntimeSwitch = () => observedSwitch;
export function observeRuntimeSwitch(receipt, selection, onProgress) {
  if (observedSwitch?.receipt.operation_id === receipt.operation_id) return observedSwitch.done;
  const observation = { receipt, ...selection, progress: null };
  observedSwitch = observation;
  const watcher = watchRuntimeOperation(receipt, (data) => {
    observation.progress = data;
    onProgress?.(data);
  });
  observation.done = watcher.done.finally(() => {
    if (observedSwitch === observation) observedSwitch = null;
  });
  return observation.done;
}

const pending = new Map();
export const isPending = (name) => [...pending.values()].some((entry) => entry.name === name);
export async function execute(
  name,
  args = {},
  { secret = "", onProgress } = {},
) {
  const id = "desktop-" + crypto.randomUUID();
  let job;
  try {
    job = await api("execute", { name, id, args, confirmed: true, secret });
  } catch (error) {
    // The server may have accepted the write before its response was lost.
    // Keep the original id available for reconciliation; never resubmit it.
    error.requestId = id;
    throw error;
  }
  return waitForOperation(job, onProgress);
}

export function waitForOperation(job, onProgress) {
  let entry = pending.get(job.id);
  if (entry) {
    if (onProgress) entry.listeners.add(onProgress);
    return entry.done;
  }
  const listeners = new Set(onProgress ? [onProgress] : []);
  entry = { name: job.name, listeners };
  pending.set(job.id, entry);
  entry.done = observeDesktopOperation(job, listeners);
  return entry.done;
}

async function observeDesktopOperation(job, listeners) {
  const { id, name } = job;
  try {
    let rounds = 0;
    while (job.state === "running") {
      for (const listener of listeners) listener(job);
      await pause(rounds++ < 5 ? 120 : 500);
      job = await api("operation?id=" + encodeURIComponent(id));
    }
    if (job.state !== "done") {
      const error = new Error(
        job.error ||
          `${i18nMessages.mebf2c23fd61f499e}${id}${i18nMessages.mf3b1fc50ed227f1e}`,
      );
      error.requestId = id;
      error.envelope = job.envelope;
      error.operation = job;
      throw error;
    }
    return job.materialPending
      ? { ...job.envelope, recoveryRequestId: id }
      : job.envelope;
  } catch (error) {
    if (job.state === "running") {
      error.message = `${i18nMessages.mebf2c23fd61f499e}${id}${i18nMessages.mbe0540704b885e28}${error.message}`;
      error.requestId = id;
    }
    throw error;
  } finally {
    pending.delete(id);
    if (job.state === "done" || job.state === "failed")
      api("operation-ack", { id }).catch((error) =>
        notify(error.message, true),
      );
    document.dispatchEvent(
      new CustomEvent("respire-operation", {
        detail: { name, id, state: job.state },
      }),
    );
  }
}
export async function action(button, fn) {
  if (button.disabled) return;
  button.disabled = true;
  try {
    if (typeof document !== "undefined" && document.querySelector?.("#notice")) notify("");
    return await fn();
  } catch (error) {
    notify(error.message, true);
  } finally {
    button.disabled = false;
  }
}
export function panel(title, body, extra = "") {
  return `<section class="panel"><div class="panel-heading"><h2>${h(title)}</h2>${extra}</div>${body}</section>`;
}
export function pairs(items) {
  return `<dl class="facts">${items.map(([label, value]) => `<div><dt>${h(label)}</dt><dd>${h(value ?? "—")}</dd></div>`).join("")}</dl>`;
}
export function metric(label, value, note = "") {
  return `<div class="metric"><span>${h(label)}</span><strong>${h(value)}</strong><small>${h(note)}</small></div>`;
}
export function input(name, label, value = "", options = {}) {
  return `<label>${h(label)}<input name="${h(name)}" value="${h(value)}" type="${h(options.type || "text")}" ${options.required ? "required" : ""} ${options.placeholder ? `placeholder="${h(options.placeholder)}"` : ""} ${options.step ? `step="${h(options.step)}"` : ""} autocomplete="${options.type === "password" ? "new-password" : "off"}"></label>`;
}
export function select(name, label, items, value) {
  return `<label>${h(label)}<select name="${h(name)}">${items
    .map((item) => {
      const [key, text] = Array.isArray(item) ? item : [item, item];
      return `<option value="${h(key)}" ${key === value ? "selected" : ""}>${h(text)}</option>`;
    })
    .join("")}</select></label>`;
}
export function dialog({
  title,
  body,
  submit = i18nMessages.ma3030bf8f16dc63c,
  onSubmit,
  wide = false,
}) {
  const element = document.createElement("dialog");
  element.className = wide ? "dialog wide" : "dialog";
  element.innerHTML = `<form><div class="dialog-heading"><h2>${h(title)}${i18nMessages.m09e4d676aee89ad5}${body}${i18nMessages.m5a00f852051a297a}${h(submit)}</button></footer></form>`;
  document.body.append(element);
  let busy = false;
  all("[data-close]", element).forEach((button) =>
    button.addEventListener("click", () => {
      if (!busy) element.close();
    }),
  );
  element.addEventListener("cancel", (event) => {
    if (busy) event.preventDefault();
  });
  element.addEventListener("close", () => element.remove(), { once: true });
  $("form", element).addEventListener("submit", async (event) => {
    event.preventDefault();
    if (busy) return;
    busy = true;
    const button = $("[type=submit]", element);
    button.disabled = true;
    const errorNode = $(".form-error", element);
    errorNode.hidden = true;
    try {
      const value = await onSubmit(
        Object.fromEntries(new FormData(event.target)),
        element,
      );
      if (value !== false) element.close();
    } catch (error) {
      errorNode.textContent = error.message;
      errorNode.hidden = false;
    } finally {
      busy = false;
      button.disabled = false;
    }
  });
  element.showModal();
  return element;
}
export async function confirm(title, message, label = i18nMessages.m68a35e2242686dcc) {
  return new Promise((resolve) => {
    let accepted = false;
    const element = dialog({
      title,
      body: `<p>${h(message)}</p>`,
      submit: label,
      onSubmit: () => {
        accepted = true;
        resolve(true);
      },
    });
    element.addEventListener(
      "close",
      () => {
        if (!accepted) resolve(false);
      },
      { once: true },
    );
  });
}
export function showText(title, text) {
  const element = dialog({
    title,
    body: `<textarea class="reading-text" aria-label="${h(title)}" readonly rows="16">${h(text)}</textarea>`,
    submit: i18nMessages.m63d90d977348ab1f,
    onSubmit: async () => {
      await navigator.clipboard.writeText(text);
      notify(i18nMessages.m8f6f8d979c981ced);
      return false;
    },
    wide: true,
  });
  return element;
}
export function resultView(title, envelope) {
  const details = envelope?.details;
  const body =
    Array.isArray(envelope?.items) && envelope.items.length
      ? `<div class="result-lines">${envelope.items.map((item) => `<div><span>${h(item.name)}</span><strong class="${item.status === "fail" ? "danger-text" : ""}">${h(typeof item.value === "object" ? raw(item.value) : item.value)}</strong></div>`).join("")}</div>`
      : pairs(
          Object.entries(summary(envelope)).filter(
            ([, value]) => value === null || typeof value !== "object",
          ),
        );
  return panel(
    title,
    (Array.isArray(envelope?.details?.warnings) &&
    envelope.details.warnings.length
      ? `<div class="panel-body warning-text">${envelope.details.warnings.map(h).join("<br>")}</div>`
      : "") +
      (Array.isArray(envelope?.errors) && envelope.errors.length
        ? `<div class="panel-body form-error" role="alert">${envelope.errors.map(h).join("<br>")}</div>`
        : "") +
      body +
      (details && typeof details === "object"
        ? `${i18nMessages.mad29d313edac4a8b}${h(raw(details))}</pre></details>`
        : ""),
  );
}
