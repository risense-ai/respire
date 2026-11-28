import { messages as i18nMessages } from "../ui/i18n/messages.js";
import {
  summary,
  execute,
  api,
  observeRuntimeSwitch,
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

// Weight per device: CPU stays quantized; GPU prefers fp16.
export const DEVICE_VARIANTS = { cpu: "quantized", gpu: "fp16" };

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
  return engineAvailable(platform, "gpu");
}

export function detectBestEngine(platform) {
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
];

export const isProfileModel = (model) => model !== "m3";

export function variantFilename(variant) {
  return (variant?.path && String(variant.path).split(/[/\\]/).pop()) ||
    (variant?.name ? `model_${variant.name}.onnx` : "");
}

function matchesFilename(item, filename) {
  if (!filename) return false;
  const escaped = filename.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(?:^|[^a-zA-Z0-9_.-])${escaped}(?:$|[^a-zA-Z0-9_.-])`).test(item || "");
}

export function taskMatchesVariant(task, variant) {
  return Boolean(task?.busy) && matchesFilename(task.stage, variantFilename(variant));
}

export function variantFromItem(item, inventory) {
  return inventory?.variants?.find((entry) => matchesFilename(item, variantFilename(entry)))?.name || "";
}

// GPU still goes through the runtime's inference probe when applied.
export function engineAvailable(platform, engine) {
  if (engine === "cpu") return true;
  if (engine === "gpu") return /^(windows|darwin)/.test(platform || "");
  return false;
}

export function createModelInventory(getDesktop) {
  const entries = new Map();
  let scope;
  function currentScope() {
    const desktop = getDesktop();
    const next = desktop?.connected ? activeModelScope(desktop.health) ?? desktop.health?.data_dir ?? "connected" : null;
    if (scope !== next) {
      entries.clear();
      scope = next;
    }
    return next;
  }
  return {
    get(model) {
      currentScope();
      return entries.get(model)?.value ?? null;
    },
    load(model) {
      const owner = currentScope();
      const entry = entries.get(model) || {};
      if (entry.pending) return entry.pending;
      entries.set(model, entry);
      entry.pending = api("model-variants?model=" + encodeURIComponent(model))
        .then((value) => {
          if (currentScope() !== owner || entries.get(model) !== entry)
            throw new Error("Runtime changed while reading model inventory");
          entry.value = value;
          return value;
        }).finally(() => { entry.pending = null; });
      return entry.pending;
    },
  };
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
    activeKnown: Object.hasOwn(catalog, "active"),
  };
}

const canonicalModelID = (id) => (id === "bge-m3" ? "m3" : id);

export const activeModelScope = (health) =>
  health?.instance_id && health.account_id && health.generation > 0
    ? [health.instance_id, health.account_id, health.generation].join("/")
    : null;

export function activeVariantSelection(active, legacy) {
  // Only the authoritative null selection establishes implicit legacy M3.
  if (active === null && legacy?.model && legacy?.engine) {
    return { model: "m3", variant: legacy.model, engine: legacy.engine };
  }
  if (!active?.model) return null;
  const model = canonicalModelID(active.model);
  return { model, variant: active.variant || graniteVariant(model), engine: active.engine };
}

export function marksActiveVariant(active, model, variant, engine) {
  return Boolean(active?.variant && active.model === canonicalModelID(model) &&
    active.variant === variant && (engine === undefined || active.engine === engine));
}

export function managedM3ApplySelection(result) {
  // The existing managed switch receipt identifies the activated pair.
  // An ordinary legacy inference probe does not establish a profile change.
  const receipt = result?.probe;
  const selection = receipt?.result?.selection;
  if (result?.fallback || receipt?.state !== "ready" || receipt.active_changed !== true ||
      canonicalModelID(selection?.model) !== "m3" || !selection.variant || !selection.engine ||
      selection.variant !== result.model || selection.engine !== result.engine) return undefined;
  return selection;
}

export async function applyLegacyEngine(engine, variant) {
  const receipt = await api("model-switch", { model: "m3", engine, variant });
  if (receipt?.operation_id) {
    const operation = await observeRuntimeSwitch(receipt, { model: "m3", engine, variant });
    return { envelope: { status: "ok", summary: operation }, result: {
      engine, model: variant, fallback: false,
      probe: { state: "ready", active_changed: true, result: { selection: { model: "m3", engine, variant } } },
    } };
  }
  const envelope = await execute("model_engine_apply", {
    engine,
    model: variant,
  });
  return { envelope, result: summary(envelope) };
}

export async function submitProfileSwitch(model, engine, variant, onProgress) {
  const receipt = await api("model-switch", { model, engine, variant });
  if (receipt?.operation_id) {
    await observeRuntimeSwitch(receipt, { model, engine, variant }, onProgress);
    return { ...receipt, settled: true };
  }
  return receipt;
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

export { activeDownloadSession, openDownloadModal, reopenActiveDownloadModal } from "./model-download-modal.js";

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
    const receipt = await submitProfileSwitch(model, engine, variant);
    const verdict = receipt?.settled ? { settled: true, failed: false, details: readProfileState(await fetchProfileState()) } : await waitForProfileSwitch();
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
  const active = managedM3ApplySelection(result);
  return {
    details: { activeKnown: active !== undefined, active },
    engine: result.engine,
    variant: result.model,
    fallback: Boolean(result.fallback),
    failed: false,
    warning: fallbackMessage(engine, result),
    envelope,
  };
}
