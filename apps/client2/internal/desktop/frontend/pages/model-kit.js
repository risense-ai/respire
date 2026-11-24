import { messages as i18nMessages } from "../ui/i18n/messages.js";
import {
  $,
  h,
  bytes,
  summary,
  execute,
  action,
  notify,
  api,
} from "../ui/core.js";

// Shared model business rules for the models page and the onboarding wizard:
// catalog metadata, device availability, the download dialog, and engine
// application. Both surfaces must use these; neither copies its own version.

export const MIRROR_OPTIONS = [
  ["auto", i18nMessages.md8539b76b38011d1],
  ["https://hf-mirror.com", "hf-mirror.com"],
  ["https://hf-mirror.net", "hf-mirror.net"],
  ["https://huggingface.co", "huggingface.co"],
  ["custom", i18nMessages.m3cf919d6fc3b7fe4],
];

export const MODEL_OPTIONS = [
  ["m3", i18nMessages.m32ff0afa36013d05],
  ["bge-base-zh-v1.5", i18nMessages.mb25f222e3bd3ef66],
  ["bge-m3-distill-8l", i18nMessages.m8e77e6012f356980],
  ["granite-311m-r2", i18nMessages.mbdb1553b745a333f],
  ["granite-97m-r2", i18nMessages.m8dab78f66fc2dec9],
];

export const MODEL_HINTS = {
  m3: i18nMessages.m026d9f185ee9ad4a,
  "bge-base-zh-v1.5":
    i18nMessages.mf1244ec89a9292f8,
  "bge-m3-distill-8l":
    i18nMessages.mffd16b9de5fa61f3,
  "granite-311m-r2": i18nMessages.m1df80e9abdb662d5,
  "granite-97m-r2": i18nMessages.m7840829177780380,
};

// Wizard card copy: positioning only; download sizes come from the live
// inventory so numbers never go stale.
export const MODEL_CARDS = [
  {
    id: "m3",
    title: "BGE-M3",
    detail: i18nMessages.mfc2587ac4b0b7e43,
  },
  {
    id: "bge-base-zh-v1.5",
    title: "BGE-base-zh-v1.5",
    detail: i18nMessages.m9317beae11e2c758,
  },
  {
    id: "bge-m3-distill-8l",
    title: "BGE-M3-Distill-8L",
    detail: i18nMessages.m894e2b97d07f2591,
  },
  { id: "granite-311m-r2", title: "Granite 311M R2", detail: i18nMessages.m951a04d65044cc37 },
  { id: "granite-97m-r2", title: "Granite 97M R2", detail: i18nMessages.m8bf18719af6ce02b },
];

export const graniteVariant = (model) =>
  ({ "granite-311m-r2": "quantized", "granite-97m-r2": "standard" })[model];

// Weight per device: CPU stays quantized; GPU/NPU prefer fp16.
export const DEVICE_VARIANTS = { cpu: "quantized", gpu: "fp16", npu: "fp16" };

// Scenario modes for the models page: three intent cards instead of the raw
// variant/engine matrix. Subtitles are static fallbacks; the page overrides
// them with live inventory sizes so numbers never go stale.
export const MODEL_MODES = [
  {
    id: "lite",
    title: i18nMessages.ma711f2f1e55164f2,
    subtitle: "~540 MB",
    description: i18nMessages.m5fc5a35765ed7cc0,
    variant: "quantized",
    engine: "cpu",
    requiresGPU: false,
  },
  {
    id: "balanced",
    title: i18nMessages.m2588b5b9bacc1df6,
    subtitle: "~540 MB",
    description: i18nMessages.me7627beca34d848e,
    variant: "quantized",
    engine: "cpu",
    requiresGPU: false,
    recommended: true,
  },
  {
    id: "performance",
    title: i18nMessages.m4fbe6bf6f691659f,
    subtitle: "~1 GB",
    description: i18nMessages.m9905ca942c82a151,
    variant: "fp16",
    engine: "auto",
    requiresGPU: true,
  },
];

// Map an active weight back to the mode card that represents it; used when
// an existing installation has no stored mode preference yet.
export function modeForVariant(variant) {
  return variant === "fp16" ? "performance" : "balanced";
}

export function performanceAvailable(platform) {
  return engineAvailable(platform, "gpu") || engineAvailable(platform, "npu");
}

export function detectBestEngine(platform) {
  if (engineAvailable(platform, "npu")) return "npu";
  if (engineAvailable(platform, "gpu")) return "gpu";
  return "cpu";
}

export const DEVICES = [
  {
    id: "cpu",
    title: "CPU",
    detail: i18nMessages.me20e7deca37a99cb,
  },
  {
    id: "gpu",
    title: "GPU",
    detail: i18nMessages.mc935637ded3b5ec6,
  },
  {
    id: "npu",
    title: "NPU",
    detail: i18nMessages.mc3b16af08c93ecb1,
  },
];

export const isProfileModel = (model) => model !== "m3";

export function variantFromItem(item) {
  const match = /model_([a-z0-9]+)\.onnx/.exec(item || "");
  return match ? match[1] : "";
}

// OS-level engine entry availability. This is not hardware detection: NPU and
// GPU still go through the runtime's real inference probe when applied.
export function engineAvailable(platform, engine) {
  if (engine === "cpu") return true;
  if (engine === "gpu") return !String(platform || "").startsWith("linux");
  if (engine === "npu") return String(platform || "").startsWith("windows");
  return false;
}

export async function fetchInventory(model) {
  return api("model-variants?model=" + encodeURIComponent(model));
}

export async function fetchProfileState() {
  try {
    return await api("model-profile-state");
  } catch {
    return null;
  }
}

// The runtime reports a switch in progress.summary: `switch` carries the
// terminal state (pending / ready / failed + error), `operation` the live
// phase, and catalog.active the running selection. Read that shape in one
// place so the models page, the wizard and the overview never diverge.
export function readProfileState(state) {
  const progressEnvelope = state?.progress || {};
  const progress = progressEnvelope.details || progressEnvelope.summary || {};
  const catalogEnvelope = state?.catalog || {};
  const catalog = catalogEnvelope.details || catalogEnvelope.summary || {};
  return {
    switchState: progress.switch || {},
    operation: progress.operation || {},
    active: catalog.active || null,
  };
}

export async function applyLegacyEngine(engine, variant) {
  const envelope = await execute("model_engine_apply", {
    engine,
    model: variant,
  });
  return { envelope, result: summary(envelope) };
}

export async function submitProfileSwitch(model, engine, variant) {
  await api("model-configure", { model, variant });
  await api("model-switch", { model, engine, variant });
}

// profileStateSettled reports whether a switch task reached a terminal
// state, and whether that state is success. Only switch.state is authoritative:
// pending → ready | failed; the live operation phase never settles.
export function profileStateSettled(state) {
  const { switchState } = readProfileState(state);
  const text = String(switchState.state || "");
  if (/failed/i.test(text)) return { settled: true, failed: true };
  if (/ready/i.test(text)) return { settled: true, failed: false };
  return { settled: false, failed: false };
}

// waitForProfileSwitch polls model-profile-state until the switch settles or
// the deadline passes; it never reports success without a runtime receipt.
export async function waitForProfileSwitch({ onTick, timeoutMs = 15 * 60_000 } = {}) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const state = await fetchProfileState();
    onTick?.(state);
    const verdict = profileStateSettled(state);
    if (verdict.settled) return { ...verdict, details: readProfileState(state) };
    await new Promise((resolve) => setTimeout(resolve, 3000));
  }
  return { settled: true, failed: true, details: {}, timeout: true };
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
  const element = document.createElement("dialog");
  element.className = "dialog";
  const subject = variant === "baseline" ? i18nMessages.m93017b92d469c248 : variant;
  element.innerHTML = `${i18nMessages.ma3ef29eb6519fe60}${h(label)} · ${h(subject)}${i18nMessages.mb853f3b54b0293e1}`;
  document.body.append(element);
  element.showModal();
  const stageNode = $("[data-dl-stage]", element),
    bytesNode = $("[data-dl-bytes]", element),
    bar = $("[data-dl-bar]", element),
    errorNode = $("[data-dl-error]", element);
  const buttons = {
    pause: $('[data-dl="pause"]', element),
    resume: $('[data-dl="resume"]', element),
    cancel: $('[data-dl="cancel"]', element),
    retry: $('[data-dl="retry"]', element),
    dismiss: $('[data-dl="dismiss"]', element),
  };
  // The wizard awaits this; the models page ignores it and refreshes on close.
  let resolveDone;
  const done = new Promise((resolve) => {
    resolveDone = resolve;
  });
  function setMode(mode) {
    for (const [name, button] of Object.entries(buttons))
      button.hidden = true;
    if (mode === "downloading") buttons.pause.hidden = false, buttons.cancel.hidden = false;
    else if (mode === "paused") buttons.resume.hidden = false, buttons.cancel.hidden = false;
    else if (mode === "done") buttons.dismiss.hidden = false;
    else if (mode === "failed") buttons.retry.hidden = false, buttons.dismiss.hidden = false;
    else if (mode === "cancelled") buttons.retry.hidden = false, buttons.dismiss.hidden = false;
  }
  let timer = null;
  let startPromise = Promise.resolve();
  // While our POST is in flight a terminal task snapshot may still describe
  // the previous task, so poll() must not treat it as our result.
  let submitting = false;
  function start() {
    errorNode.hidden = true;
    setMode("downloading");
    const mirror = getMirror?.() || "auto";
    submitting = true;
    startPromise = api("model-download", {
      model,
      variant,
      ...(mirror && mirror !== "auto" ? { mirror } : {}),
    }).then(
      () => {
        submitting = false;
      },
      (error) => {
        submitting = false;
        // The bridge rejects with 409 when another task is running.
        stageNode.textContent = i18nMessages.m7e24b28fccaf9e1e;
        errorNode.textContent = error.message;
        errorNode.hidden = false;
        setMode("failed");
        throw error;
      },
    );
    return startPromise;
  }
  function applyTask(task) {
    stageNode.textContent = task.stage || "";
    bytesNode.textContent = task.total
      ? `${bytes(task.done)} / ${bytes(task.total)}`
      : "";
    if (task.total) {
      bar.value = task.done;
      bar.max = task.total;
    }
  }
  async function poll() {
    if (submitting) return;
    try {
      const state = await api("state");
      const task = state.task || {};
      applyTask(task);
      if (task.busy) return;
      // The download ended: success, paused, cancelled or failed.
      clearInterval(timer);
      timer = null;
      if (task.stage === i18nMessages.meb0c326b60ae897a) {
        setMode("paused");
        stageNode.textContent = i18nMessages.m807d77097ff13314;
      } else if (task.stage === i18nMessages.mc0b3fbff51ccc40b) {
        setMode("done");
        stageNode.textContent = i18nMessages.m1e60d41bd478c07a;
        bar.value = bar.max = task.total || 1;
        resolveDone(true);
        onDone?.();
      } else if (task.stage === i18nMessages.ma37778f17c5f3ee5) {
        setMode("cancelled");
        stageNode.textContent = i18nMessages.ma37778f17c5f3ee5;
        resolveDone(false);
      } else {
        setMode("failed");
        stageNode.textContent = i18nMessages.m8a03e35ad323e387;
        errorNode.textContent = task.error || i18nMessages.m45f5dfb2c9262c74;
        errorNode.hidden = false;
        resolveDone(false);
      }
    } catch {
      // transient state poll failure: keep the modal as-is
    }
  }
  const resumeOrRetry = (button) =>
    action(button, async () => {
      setMode("downloading");
      await start();
      if (!timer) timer = setInterval(poll, 700);
    });
  element.addEventListener("click", (event) => {
    const button = event.target.closest("[data-dl]");
    if (!button || button.hidden) return;
    const act = button.dataset.dl;
    if (act === "pause")
      action(button, async () => {
        await api("model-download-cancel", { pause: true });
      });
    else if (act === "resume" || act === "retry") resumeOrRetry(button);
    else if (act === "cancel")
      action(button, async () => {
        await api("model-download-cancel", { pause: false });
        element.close();
      });
    else if (act === "dismiss") element.close();
  });
  element.addEventListener(
    "close",
    () => {
      if (timer) {
        clearInterval(timer);
        timer = null;
      }
      // Closing the modal does not stop the download; an unresolved `done`
      // settles false so the awaiting wizard can recover from task state.
      resolveDone(false);
      onDone?.();
    },
    { once: true },
  );
  setMode("downloading");
  // Poll from the start, not after the POST resolves: the server answers as
  // soon as the task is registered, so progress streams in while it runs.
  timer = setInterval(poll, 700);
  start();
  return { element, done };
}

// notifyFallback surfaces the runtime's real post-probe engine when the
// selected device failed verification; the wizard and models page share it so
// a fallback can never be presented as success of the chosen device. The
// probe failure cause travels in result.warning — always show it.
export function fallbackMessage(engine, result) {
  if (!result?.fallback) return "";
  const reason = result.warning ? `${i18nMessages.mda047c04c9b5e27e}${result.warning}${i18nMessages.mf2e44c6c9545f08b}` : "";
  const restored = result.engine
    ? `${result.engine.toUpperCase()} · ${result.model}`
    : `${i18nMessages.m8b1a33da0e112553}${result.model}${i18nMessages.mf2e44c6c9545f08b}`;
  return `${engine.toUpperCase()}${i18nMessages.m36536183cc74a8f9}${reason}${i18nMessages.m67d420897c7ab363}${restored}${i18nMessages.m186daccba0b7642b}`;
}

export async function applyEngineChoice({ model, engine, variant }) {
  if (isProfileModel(model)) {
    await submitProfileSwitch(model, engine, variant);
    const verdict = await waitForProfileSwitch();
    return {
      engine,
      variant,
      fallback: false,
      failed: verdict.failed,
      warning: verdict.failed
        ? verdict.timeout
          ? i18nMessages.mde475b35590ce61d
          : i18nMessages.m96cf31ee716f8903
        : "",
      details: verdict.details,
    };
  }
  const { envelope, result } = await applyLegacyEngine(engine, variant);
  return {
    engine: result.engine,
    variant: result.model,
    fallback: Boolean(result.fallback),
    failed: false,
    warning: fallbackMessage(engine, result),
    envelope,
  };
}
