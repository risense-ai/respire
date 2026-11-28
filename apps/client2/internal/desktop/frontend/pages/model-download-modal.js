import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { $, h, bytes, api, action, ApiTransportError, watchRuntimeOperation, cancelRuntimeOperation } from "../ui/core.js";

const DOWNLOAD_WATCH_MS = 61 * 60_000;
const DOWNLOAD_STREAM_IDLE_MS = 10_000;
const TEMPORARY_TASK_STATUSES = new Set([408, 429, 502, 503, 504]);
const publishDownloadState = () => document.dispatchEvent(new CustomEvent("respire-operation", { detail: { name: "model_download" } }));

let activeSession = null;
export const activeDownloadSession = () => activeSession;

export function reopenActiveDownloadModal() {
  if (!activeSession || activeSession.settled) return;
  const { element } = activeSession;
  if (!element.isConnected) document.body.append(element);
  if (!element.open) element.showModal();
}

// Download progress modal: live bytes, pause / resume / cancel, result.
// Resolves `done` with true only when the task reached the completed stage.
export function openDownloadModal({
  model,
  variant,
  label,
  getMirror,
  onDone,
} = {}) {
  if (activeSession) {
    if (activeSession.model !== model || activeSession.variant !== variant)
      throw new Error(i18nMessages.mDownloadAlreadyRunning);
    reopenActiveDownloadModal();
    return activeSession;
  }
  const element = document.createElement("dialog");
  element.className = "dialog";
  const subject = variant === "baseline" ? i18nMessages.m93017b92d469c248 : variant;
  const controls = i18nMessages.mb853f3b54b0293e1.replace('<button type="button" data-dl="cancel"',
    `<button type="button" data-dl="background" hidden>${h(i18nMessages.mDownloadBackground)}</button><button type="button" data-dl="cancel"`);
  element.innerHTML = `${i18nMessages.ma3ef29eb6519fe60}${h(label)} · ${h(subject)}${controls}`;
  document.body.append(element);
  element.showModal();
  const stageNode = $("[data-dl-stage]", element),
    bytesNode = $("[data-dl-bytes]", element),
    bar = $("[data-dl-bar]", element),
    errorNode = $("[data-dl-error]", element);
  const buttons = {
    pause: $('[data-dl="pause"]', element),
    resume: $('[data-dl="resume"]', element),
    background: $('[data-dl="background"]', element),
    cancel: $('[data-dl="cancel"]', element),
    retry: $('[data-dl="retry"]', element),
    dismiss: $('[data-dl="dismiss"]', element),
  };
  let session;
  let resolveDone;
  let backgrounded = false;
  function settle(success) {
    if (session.settled) return;
    session.settled = true;
    if (activeSession === session) activeSession = null;
    if (!element.open) element.remove?.();
    resolveDone(success);
    onDone?.();
  }
  function setMode(mode) {
    session.state = mode;
    buttons.pause.hidden = mode !== "downloading" || session.runtimeDownload;
    buttons.resume.hidden = mode !== "paused";
    buttons.background.hidden = buttons.cancel.hidden = !["downloading", "paused"].includes(mode);
    buttons.retry.hidden = !["failed", "cancelled"].includes(mode);
    buttons.dismiss.hidden = !["done", "failed", "cancelled"].includes(mode);
  }
  let paused = false;
  let timer = null;
  let stream = null;
  let taskID = "";
  let request = null;
  let watchDeadline = 0;
  let pollFailures = 0;
  function stopUpdates() {
    clearTimeout(timer);
    const activeStream = stream;
    stream = null;
    activeStream?.close();
    request?.abort();
  }
  async function start() {
    if (activeSession && activeSession !== session)
      throw new Error(i18nMessages.mDownloadAlreadyRunning);
    if (!session || session.settled) {
      const done = new Promise((resolve) => { resolveDone = resolve; });
      session = { element, done, model, variant, label, runtimeDownload: false, settled: false };
      activeSession = session;
    }
    paused = false;
    taskID = "";
    pollFailures = 0;
    bar.removeAttribute?.("value");
    bytesNode.textContent = "";
    errorNode.hidden = true;
    setMode("downloading");
    try {
      const mirror = getMirror?.() || "auto";
      session.receipt = api("model-download", {
        model, variant, ...(mirror !== "auto" ? { mirror } : {}),
      });
      const receipt = await session.receipt;
      session.runtimeDownload = Boolean(receipt?.runtime);
      session.id = receipt?.id;
      if (session.settled) return;
      if (!receipt?.id) throw new Error("Missing download receipt");
      taskID = receipt.id;
      publishDownloadState();
      if (session.runtimeDownload) {
        const watcher = watchRuntimeOperation(receipt, (operation) => {
          session.progress = operation;
          stageNode.textContent = operation.message || operation.state;
          const percent = operation.progress_percent;
          bytesNode.textContent = typeof percent === "number" ? `${Math.round(percent)}%` : "";
          if (typeof percent === "number") {
            bar.max = 100;
            bar.value = percent;
          } else bar.removeAttribute?.("value");
        });
        stream = watcher;
        buttons.pause.hidden = true;
        try {
          await watcher.done;
          if (session.settled) return;
          stopUpdates();
          setMode("done");
          bar.value = bar.max = 100;
          settle(true);
        } catch (error) {
          if (session.settled) return;
          if (error.cancelled) { stopUpdates(); setMode("cancelled"); settle(false); }
          else failWatch(error.message);
        }
        return;
      }
      watchDeadline = performance.now() + DOWNLOAD_WATCH_MS;
    } catch (error) {
      if (session.settled) return;
      stageNode.textContent = i18nMessages.m7e24b28fccaf9e1e;
      failWatch(error.message);
      return;
    }
    if (session.settled) return;
    if (typeof EventSource === "undefined") return poll();
    const id = taskID;
    let source;
    try {
      source = new EventSource("/api/task-events?id=" + encodeURIComponent(id));
    } catch {
      return poll();
    }
    stream = source;
    const fallback = () => {
      if (stream !== source) return;
      stopUpdates();
      if (canWatch(id)) poll();
    };
    const armWatchdog = (idleMs) => {
      clearTimeout(timer);
      timer = setTimeout(fallback, Math.min(idleMs, remainingWatch()));
    };
    // Some native bridges buffer the first frame until EOF. Even after a frame
    // arrives, switch to snapshots if the stream ceases making progress.
    armWatchdog(2000);
    source.onmessage = (event) => {
      if (stream !== source || !canWatch(id)) return;
      let task;
      try {
        task = JSON.parse(event.data);
      } catch {
        fallback();
        return;
      }
      if (!validTask(task, id)) return fallback();
      receive(task);
      if (stream === source) armWatchdog(DOWNLOAD_STREAM_IDLE_MS);
    };
    source.onerror = fallback;
  }
  function validTask(task, id) {
    return task?.id === id && typeof task.busy === "boolean";
  }
  function receive(task) {
    stageNode.textContent = task.stage || "";
    bytesNode.textContent = task.total > 0
      ? `${bytes(task.done)} / ${bytes(task.total)}`
      : task.done > 0 ? bytes(task.done) : "";
    if (task.total > 0) {
      bar.max = task.total;
      bar.value = task.done;
    } else bar.removeAttribute?.("value");
    if (task.busy) return;
    stopUpdates();
    if (task.stage === i18nMessages.meb0c326b60ae897a) {
      paused = true;
      setMode("paused");
      stageNode.textContent = i18nMessages.m807d77097ff13314;
    } else if (task.stage === i18nMessages.mc0b3fbff51ccc40b) {
      setMode("done");
      stageNode.textContent = i18nMessages.m1e60d41bd478c07a;
      bar.value = bar.max = task.total || 1;
      settle(true);
    } else if (task.stage === i18nMessages.ma37778f17c5f3ee5) {
      setMode("cancelled");
      stageNode.textContent = i18nMessages.ma37778f17c5f3ee5;
      settle(false);
    } else {
      stageNode.textContent = i18nMessages.m8a03e35ad323e387;
      failWatch(task.error || i18nMessages.m45f5dfb2c9262c74);
    }
  }
  function failWatch(message) {
    stopUpdates();
    setMode("failed");
    errorNode.textContent = message;
    errorNode.hidden = false;
    settle(false);
  }
  function remainingWatch() {
    return Math.max(0, watchDeadline - performance.now());
  }
  function canWatch(id) {
    if (session.settled || id !== taskID) return false;
    if (remainingWatch() > 0) return true;
    failWatch(i18nMessages.m23dfe4404e8a5ebe);
    return false;
  }
  async function poll() {
    const id = taskID;
    if (!canWatch(id)) return;
    const abort = new AbortController();
    request = abort;
    const timeout = setTimeout(() => abort.abort(), Math.min(5000, remainingWatch()));
    try {
      const task = await api("task?id=" + encodeURIComponent(id), undefined, abort.signal);
      if (!canWatch(id)) return;
      if (!validTask(task, id)) {
        failWatch(i18nMessages.m23dfe4404e8a5ebe);
        return;
      }
      pollFailures = 0;
      receive(task);
      if (!task.busy) return;
    } catch (error) {
      if (!canWatch(id)) return;
      const temporary = abort.signal.aborted || error instanceof ApiTransportError
        || TEMPORARY_TASK_STATUSES.has(error.status);
      if (!temporary || ++pollFailures >= 3) {
        failWatch(error.status ? error.message : i18nMessages.m23dfe4404e8a5ebe);
        return;
      }
    } finally {
      clearTimeout(timeout);
      if (request === abort) request = null;
    }
    if (canWatch(id)) timer = setTimeout(poll, Math.min(700, remainingWatch()));
  }
  element.addEventListener("click", (event) => {
    const button = event.target.closest("[data-dl], [data-close]");
    if (!button || button.hidden) return;
    const act = button.dataset.dl;
    if ((act === "pause" || act === "cancel") && !taskID) return;
    if (act === "background") {
      backgrounded = true;
      element.close();
      publishDownloadState();
    } else if (act === "pause")
      action(button, async () => {
        await api("model-download-cancel", { id: taskID, pause: true });
      });
    else if (act === "resume" || act === "retry") action(button, start);
    else if (act === "cancel")
      action(button, async () => {
        if (session.runtimeDownload) await cancelRuntimeOperation(taskID);
        else if (!paused) await api("model-download-cancel", { id: taskID, pause: false });
        stopUpdates();
        setMode("cancelled");
        settle(false);
        element.close();
      });
    else if (act === "dismiss" || !act) element.close();
  });
  element.addEventListener(
    "close",
    () => {
      if (backgrounded) {
        backgrounded = false;
        return;
      }
      stopUpdates();
      if (!session.settled) {
        session.state = "closed";
        settle(false);
      }
      element.remove?.();
    },
  );
  start();
  return session;
}
