import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { $, h, bytes, date } from "../ui/core.js";
import { phase } from "./overview.js";
import {
  MODEL_OPTIONS,
  graniteVariant,
  MODEL_HINTS,
  MODEL_MODES,
  isProfileModel,
  variantFromItem,
  engineAvailable,
  modeForVariant,
  performanceAvailable,
  readProfileState,
  activeVariantSelection,
  activeModelScope,
  marksActiveVariant,
} from "./model-kit.js";

// Scenario-mode states: the page speaks in "can I use it now" terms instead of
// exposing the variant/engine matrix. FALLBACK and ERROR are sticky overrides
// on top of the computed state so a failed verification stays visible until
// the user retries or picks another mode.
export const PAGE_STATES = {
  EMPTY: "empty",
  DOWNLOADING: "downloading",
  VERIFYING: "verifying",
  READY: "ready",
  FALLBACK: "fallback",
  ERROR: "error",
};

const STATE_CONFIG = {
  [PAGE_STATES.EMPTY]: {
    icon: "○",
    title: i18nMessages.ma3bdaa80bbff8919,
    description: i18nMessages.mee70fd347f09bf72,
    buttonText: i18nMessages.m74096cb2762ab834,
    buttonClass: "primary",
  },
  [PAGE_STATES.DOWNLOADING]: {
    icon: "↓",
    title: i18nMessages.m547451cc8c7dbb03,
    description: i18nMessages.m791e5397cd8aa384,
    buttonText: i18nMessages.mbcc2333dc631061e,
    buttonClass: "disabled",
  },
  [PAGE_STATES.VERIFYING]: {
    icon: "…",
    title: i18nMessages.m80652be50407f6a0,
    description: i18nMessages.mef7700c1aefb6f26,
    buttonText: i18nMessages.m7f2d0739a240dc51,
    buttonClass: "disabled",
  },
  [PAGE_STATES.READY]: {
    icon: "✓",
    title: i18nMessages.meb8a53c811579b76,
    description: i18nMessages.mb3625e570d74163e,
    buttonText: i18nMessages.mdfb802238b38fbd4,
    buttonClass: "success",
  },
  [PAGE_STATES.FALLBACK]: {
    icon: "⚠",
    title: i18nMessages.mf79d93d848c9d524,
    description: i18nMessages.m25d9f10996f6661b,
    buttonText: i18nMessages.mb8784c8dd5636ff2,
    buttonClass: "primary",
  },
  [PAGE_STATES.ERROR]: {
    icon: "✗",
    title: i18nMessages.m00ffd4502cf77867,
    description: i18nMessages.m445d5cbfb9967765,
    buttonText: i18nMessages.mb8784c8dd5636ff2,
    buttonClass: "primary",
  },
};

// Runtime model_progress phases while a profile switch runs (model_progress.rs).
const SWITCH_PHASES = {
  queued: i18nMessages.md6f766f2adf53227,
  "switch-verify": i18nMessages.mab207f8bb66005ca,
  "switch-download": i18nMessages.m621982e6d8de510a,
  "switch-load": i18nMessages.m836ebd4436a27d05,
  "switch-index": i18nMessages.m3e4ab1850f35cc47,
  "switch-diary": i18nMessages.m3f110122999a31c4,
};

export function createModelsView(view, { root, ctx, live }) {
  const activeSelection = () => view.runtimeActiveScope === activeModelScope(ctx.desktop?.health)
    ? activeVariantSelection(view.runtimeActive, view.legacyEngineState)
    : null;
  const currentVariant = (variant, engine) =>
    marksActiveVariant(activeSelection(), view.modelID, variant, engine);
  // A running download or apply disables switching.
  function engineEnabled(engine, opActive) {
    if (opActive) return false;
    return engineAvailable(ctx.desktop?.platform, engine);
  }
  function engineChips(variant, opActive) {
    return ["cpu", "gpu"]
      .map((engine) => {
        const active = currentVariant(variant.name, engine);
        return `<button class="engine-chip${active ? " active" : ""}" data-apply data-engine="${engine}" data-variant="${h(
          variant.name,
        )}"${engineEnabled(engine, opActive) ? "" : " disabled"}${i18nMessages.m663ae289c366e504}${engine.toUpperCase()} · ${
          variant.name
        }${i18nMessages.m58f47e4685c6a1e2}${engine.toUpperCase()}</button>`;
      })
      .join("");
  }
  // Live selection + availability line: what the runtime uses right now,
  // which entries this OS offers, and what a failed verification does.
  function renderEngineHint() {
    const node = $("[data-engine-hint]", root);
    if (!node) return;
    const platform = ctx.desktop?.platform || "";
    const entries = [
      `${i18nMessages.m7df3ba5a96cc5427}`,
      `GPU${engineAvailable(platform, "gpu") ? i18nMessages.m537352400c495fb8 : i18nMessages.m30b4456e2243ce0e}`,
    ];
    const active = activeSelection();
    const label = MODEL_OPTIONS.find(([id]) => id === active?.model)?.[1] || active?.model || "-";
    const probe = view.lastProbe
      ? `${i18nMessages.m0496a3bcda0575de}${view.lastProbe.selected}${i18nMessages.mda047c04c9b5e27e}${view.lastProbe.elapsed_ms}${i18nMessages.m4cf435bfd8907c1c}`
      : "";
    const snapshotNote = ctx.desktop?.engine?.note
      ? ` ${ctx.desktop.engine.note}`
      : "";
    node.textContent = `${i18nMessages.md567fd653565c629}${label} · ${active?.variant || "-"}${i18nMessages.m765ab9c51918e21e}${active?.engine?.toUpperCase() || "-"}${i18nMessages.m4ddc0667b7c58915}${probe}${i18nMessages.m97a2b38e72546427}${entries.join(" / ")}${i18nMessages.mb7a7b3a5d77b24d2}${snapshotNote}`;
  }
  // One status renderer: switching state first, then client-side parallel
  // download, runtime download/verify, installed, or missing.
  function variantStatus(variant, op, downloading, task) {
    const busy = downloading === variant.name;
    const current = currentVariant(variant.name);
    if (view.switchInFlight && view.switchingVariant === variant.name) {
      if (!isProfileModel(view.modelID))
        return `${i18nMessages.m1e9762ef816d3fbb}`;
      const operation = view.runtimeProgress ? {
        phase: view.runtimeProgress.message || view.runtimeProgress.state,
        done: view.runtimeProgress.progress_percent, total: 100,
      } : ctx.status?.summary?.model_operation?.active
        ? ctx.status.summary.model_operation : readProfileState(view.profileState).operation;
      const phase =
        SWITCH_PHASES[operation.phase] || operation.phase || i18nMessages.mdc9591e56d502b43;
      const pct = operation.total
        ? ` ${Math.round((operation.done / operation.total) * 100)}%`
        : "";
      return `${i18nMessages.mee98756ab8a33df2}${h(String(phase))}${pct}</strong>`;
    }
    const pct = op.total
      ? Math.min(100, Math.round((op.done / op.total) * 100))
      : 0;
    if (
      task?.busy &&
      String(task.stage || "").includes("model_" + variant.name + ".onnx")
    ) {
      const tPct = task.total
        ? Math.min(100, Math.round((task.done / task.total) * 100))
        : 0;
      return `${i18nMessages.m1b72aba0ad0d0039}${tPct}%</strong><progress ${task.total ? `value="${h(task.done)}" max="${h(task.total)}"` : ""}></progress>`;
    }
    if (busy)
      return `<strong>${op.phase === "verify" ? i18nMessages.m784f9b54ae2ba27b : i18nMessages.me2fc571a4cf00ceb} ${pct}%</strong><progress ${op.total ? `value="${h(op.done)}" max="${h(op.total)}"` : ""}></progress>`;
    if (variant.installed)
      return `<span class="variant-status"><strong>${current ? i18nMessages.m3bef61e2898bcd2d : i18nMessages.me2f12da7faa68217}</strong><span class="hint">${h(bytes(variant.bytes))}${i18nMessages.mde9f2b736c831935}`;
    return i18nMessages.mdccec95b65e15ff0;
  }
  function renderVariants() {
    const node = $("[data-variants]", root);
    if (!node || !view.inventory) return;
    const op = ctx.status?.summary?.model_operation || {};
    const downloading = op.active ? variantFromItem(op.item) : "";
    const hint = $("[data-model-hint]", root);
    if (hint) hint.textContent = MODEL_HINTS[view.modelID] || MODEL_HINTS.m3;
    node.innerHTML = `${i18nMessages.m96cac5f992733c0b}${view.inventory.variants
      .map((variant) => {
        const badge =
          variant.name === "quantized"
            ? i18nMessages.m0e57bc95c4e552ba
            : variant.name === "fp16"
              ? i18nMessages.m5feb166f314441c0
              : variant.name === "fp32"
                ? i18nMessages.m8ad5ce2b50a7ece5
                : "";
        const activeHere = currentVariant(variant.name);
        const ops = variant.installed
          ? `<div class="variant-ops">${engineChips(variant, Boolean(op.active) || view.switchInFlight)}</div><button data-model="download" data-variant="${h(variant.name)}"${downloading === variant.name ? " disabled" : ""}>${activeHere ? i18nMessages.mff8d2afbf63e1811 : i18nMessages.m4673a23061656125}</button>`
          : `<button data-model="download" data-variant="${h(variant.name)}"${downloading === variant.name ? " disabled" : ""}${i18nMessages.m95de3b8faabce594}`;
        return `<tr><td><span class="variant-name"><strong>${h(variant.name)}</strong>${badge ? `<span class="hint">${badge}${variant.name === "fp16" ? i18nMessages.m52b8ee376f013c8e : ""}</span>` : ""}</span></td><td>${h(bytes(variant.size))}</td><td data-status="${h(variant.name)}">${variantStatus(variant, op, downloading, ctx.desktop?.task)}</td><td><span class="path-cell"><code title="${h(variant.path)}">${h(variant.path)}</code><button class="link-btn" data-open-dir="${h(variant.path)}${i18nMessages.m57c846016182bd26}${ops}</td></tr>`;
      })
      .join("")}</tbody></table>`;
    const cancelBtn = $("[data-model=cancel]", root);
    if (cancelBtn)
      cancelBtn.disabled =
        !op.active || !ctx.status?.summary?.model_operation?.id;
    renderBaseline();
  }
  function renderBaseline() {
    const area = $("[data-baseline]", root);
    if (!area || !view.inventory) return;
    const task = ctx.desktop?.task || {};
    const baseBusy =
      task.busy &&
      new RegExp(i18nMessages.m591d866b38483e71,"").test(task.stage || "");
    if (baseBusy) {
      const pct = task.total
        ? Math.min(100, Math.round((task.done / task.total) * 100))
        : 0;
      area.innerHTML = `${i18nMessages.mbe8e9dbd7e03294b}${pct}%</strong><progress ${task.total ? `value="${h(task.done)}" max="${h(task.total)}"` : ""}></progress></div>`;
      return;
    }
    area.innerHTML = view.inventory.tokenizer_installed
      ? `${i18nMessages.md50ec785be9d042e}${h(view.inventory.model_dir)}</code></span>`
      : `${i18nMessages.m262e83b3937e2dd1}${h(bytes(quantizedBytes()))}${i18nMessages.md2eaa99b6f3a6907}`;
  }
  function quantizedBytes() {
    const quantized = (view.inventory?.variants || []).find(
      (variant) => variant.name === "quantized",
    );
    return quantized?.size || 0;
  }
  // The client-side download task only counts as a model download when its
  // stage names model files; update or index tasks must not flip the page.
  function modelTaskBusy(task) {
    return (
      Boolean(task?.busy) &&
      /model(?:_[a-z0-9_]+)?\.onnx|tokenizer\.json/.test(String(task.stage || ""))
    );
  }
  function currentMode() {
    const fixed = graniteVariant(view.modelID);
    if (fixed) return { ...MODEL_MODES[1], variant: fixed, title: "CPU" };
    return (
      MODEL_MODES.find((mode) => mode.id === view.selectedMode) || MODEL_MODES[1]
    );
  }
  function modeVariant(mode) {
    return (view.inventory?.variants || []).find(
      (item) => item.name === mode.variant,
    );
  }
  // Default state from live data: downloading, missing weights, switching,
  // or ready for the selected mode.
  function computePageState() {
    const op = ctx.status?.summary?.model_operation || {};
    const task = ctx.desktop?.task || {};
    if (op.active || modelTaskBusy(task)) return PAGE_STATES.DOWNLOADING;
    const variant = modeVariant(currentMode());
    if (!variant?.installed || !view.inventory?.tokenizer_installed) return PAGE_STATES.EMPTY;
    if (view.switchInFlight) return PAGE_STATES.VERIFYING;
    if (!currentVariant(currentMode().variant)) return PAGE_STATES.EMPTY;
    if (currentMode().requiresGPU && activeSelection()?.engine === 'cpu') return PAGE_STATES.EMPTY;
    if (!view.lastVerifiedAt && ctx.desktop?.health?.inference?.phase !== 'ready') return PAGE_STATES.EMPTY;
    return PAGE_STATES.READY;
  }
  function effectivePageState() {
    const computed = computePageState();
    // A sticky FALLBACK/ERROR survives idle recomputation (that is exactly
    // when it must stay visible); only a new download or switch — which
    // always computes as DOWNLOADING/VERIFYING — supersedes it.
    if (
      computed === PAGE_STATES.DOWNLOADING ||
      computed === PAGE_STATES.VERIFYING
    )
      view.stateOverride = null;
    return view.stateOverride || computed;
  }
  function renderStatusCard() {
    const node = $("[data-status-card]", root);
    if (!node) return;
    const state = effectivePageState();
    const config = STATE_CONFIG[state];
    const activeMode = MODEL_MODES.find(
      (mode) => mode.id === modeForVariant(view.engineState.model),
    );
    node.innerHTML = `<div class="status-card ${state}"><div class="status-icon">${config.icon}</div><div class="status-body"><strong>${h(config.title)}</strong><span class="hint">${h(config.description)}</span></div><div class="status-meta"><span class="mode-tag">${h(activeMode?.title || "—")} · ${h(view.engineState.engine.toUpperCase())}</span>${view.lastVerifiedAt ? `<span class="hint">${h(i18nMessages.m29a1264657999b1c + date(view.lastVerifiedAt.toISOString()))}</span>` : ""}</div></div>`;
  }
  function renderModeSelector() {
    const node = $("[data-mode-selector]", root);
    if (!node) return;
    const accel = performanceAvailable(ctx.desktop?.platform);
    const modes = graniteVariant(view.modelID) ? [currentMode()] : MODEL_MODES;
    node.innerHTML = modes.map((mode) => {
      const variant = modeVariant(mode);
      const subtitle = variant?.size ? "≈ " + bytes(variant.size) : mode.subtitle;
      const noAccel = mode.requiresGPU && !accel;
      return `<button type="button" class="mode-card${view.selectedMode === mode.id ? " selected" : ""}${mode.recommended ? " recommended" : ""}" data-mode-select="${mode.id}">${mode.recommended ? i18nMessages.mf24530eb2adc0403 : ""}<strong>${h(mode.title)}</strong><span class="hint">${h(subtitle)}</span><span class="mode-desc">${h(mode.description)}</span>${noAccel ? i18nMessages.m541e6fab2ed7e231 : ""}</button>`;
    }).join("");
  }
  function renderPrimaryAction() {
    const node = $("[data-primary-action]", root);
    if (!node) return;
    const config = STATE_CONFIG[effectivePageState()];
    const idle =
      effectivePageState() === PAGE_STATES.DOWNLOADING ||
      effectivePageState() === PAGE_STATES.VERIFYING ||
      effectivePageState() === PAGE_STATES.READY;
    node.innerHTML = `<div data-mode-progress hidden></div><button type="button" class="${config.buttonClass}" data-mode-apply${idle ? " disabled" : ""}>${h(config.buttonText)}</button>`;
    updateModeProgress();
  }
  function updateModeProgress() {
    const node = $("[data-mode-progress]", root);
    if (!node) return;
    const op = ctx.status?.summary?.model_operation || {};
    const task = ctx.desktop?.task || {};
    if (modelTaskBusy(task)) {
      const pct = task.total
        ? Math.min(100, Math.round((task.done / task.total) * 100))
        : 0;
      node.hidden = false;
      node.innerHTML = `<div class="progress-label"><span>${h(String(task.stage || ""))}</span><span>${pct}%</span></div><progress ${task.total ? `value="${h(task.done)}" max="${h(task.total)}"` : ""}></progress>`;
    } else if (op.active) {
      const pct = op.total
        ? Math.min(100, Math.round((op.done / op.total) * 100))
        : 0;
      node.hidden = false;
      node.innerHTML = `<div class="progress-label"><span>${h(String(op.phase || ""))} · ${h(String(op.item || ""))}</span><span>${pct}%</span></div><progress ${op.total ? `value="${h(op.done)}" max="${h(op.total)}"` : ""}></progress>`;
    } else {
      node.hidden = true;
      node.innerHTML = "";
    }
  }
  function renderAdvancedVisibility() {
    const panelNode = $("[data-advanced-panel]", root);
    const openBar = $("[data-advanced-open]", root);
    if (panelNode) panelNode.hidden = !view.advancedExpanded;
    if (openBar) openBar.hidden = view.advancedExpanded;
  }
  // The selector and the primary button rebuild only when something they
  // display changed; polling every 3 seconds must not replace a button
  // mid-click.
  function renderDynamic(force = false) {
    const variant = modeVariant(currentMode());
    const key = [
      effectivePageState(),
      view.selectedMode,
      view.engineState.engine,
      view.engineState.model,
      variant ? `${variant.installed ? 1 : 0}:${variant.size}` : "none",
      view.lastVerifiedAt ? view.lastVerifiedAt.getTime() : 0,
      performanceAvailable(ctx.desktop?.platform) ? 1 : 0,
    ].join("|");
    if (!force && key === view.pageKey) {
      updateModeProgress();
      return;
    }
    view.pageKey = key;
    renderModeSelector();
    renderPrimaryAction();
    renderStatusCard();
  }
  // Polling refreshes status cells in place; rebuilding the whole table would
  // replace buttons mid-click every 3 seconds.
  function updateVariantsProgress() {
    if (!view.inventory) return;
    const op = ctx.status?.summary?.model_operation || {};
    const task = ctx.desktop?.task || {};
    const downloading = op.active ? variantFromItem(op.item) : "";
    for (const variant of view.inventory.variants) {
      const cell = $(`[data-status="${CSS.escape(variant.name)}"]`, root);
      if (!cell) continue;
      const html = variantStatus(variant, op, downloading, task);
      if (cell.innerHTML !== html) cell.innerHTML = html;
      const button = $(
        `[data-variant="${CSS.escape(variant.name)}"]`,
        root,
      );
      const clientBusy =
        task.busy &&
        String(task.stage || "").includes("model_" + variant.name + ".onnx");
      if (button)
        button.disabled =
          downloading === variant.name || Boolean(op.active) || clientBusy;
      for (const chip of document.querySelectorAll(
        `[data-apply][data-variant="${CSS.escape(variant.name)}"]`,
      )) {
        const active = currentVariant(variant.name, chip.dataset.engine);
        chip.classList.toggle("active", active);
        chip.disabled = !engineEnabled(chip.dataset.engine, Boolean(op.active) || view.switchInFlight);
      }
    }
    renderBaseline();
  }
  return {
    renderVariants,
    renderBaseline,
    renderEngineHint,
    quantizedBytes,
    modelTaskBusy,
    currentMode,
    modeVariant,
    renderDynamic,
    renderAdvancedVisibility,
    updateVariantsProgress,
  };
}
