import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { $, h, raw, number, summary, panel, pairs, input, select, execute, notify, resultView, api } from "../ui/core.js";
import { heading, phase } from "./overview.js";
import { recallChecks } from "./recall.js";
import {
  MIRROR_OPTIONS,
  MODEL_OPTIONS,
  graniteVariant,
  MODEL_MODES,
  isProfileModel,
  modeForVariant,
  performanceAvailable,
  detectBestEngine,
  fetchProfileState,
  readProfileState,
  activeModelScope,
  profileStateSettled,
  applyEngineChoice,
  activeDownloadSession,
  openDownloadModal,
} from "./model-kit.js";

import { createModelsView, downloadTaskProgress, indexTaskProgress, liveModelOperation, modelCancelButton, modelOperationProgress, nextSwitchStallState, PAGE_STATES } from "./models-view.js";
import { bindModelActions, followRebuild } from "./models-actions.js";
import { bindModelSettings, FUSION_MODES, FUSION_NUMERIC } from "./models-settings.js";

export function models(root, ctx) {
  const view = {
    engineState: { engine: "cpu", model: "quantized" },
    runtimeActive: undefined,
    runtimeActiveScope: null,
    legacyEngineState: null,
    get inventory() { return ctx.modelInventory.get(this.modelID); },
    sawMissingBaseline: false,
    baselinePrepared: false,
    modelID: localStorage.getItem("respire-model") || "m3",
    profileState: null,
    switchInFlight: false,
    switchingVariant: "",
    switchTimer: null,
    runtimeProgress: null,
    selectedMode: localStorage.getItem("respire-model-mode") || "balanced",
    stateOverride: null,
    advancedExpanded: false,
    lastVerifiedAt: null,
    pageKey: "",
    switchTicks: 0,
    switchProgressKey: "",
    profileSwitchPending: false,
    lastProbe: null,
    rebuildJobID: null,
    indexInFlight: false,
  };
  let agent = {},
    backend = {},
    loaded = false,
    loading = false,
    fusion = { runtime: null, draft: null, checked: false, loadFailed: false, requestId: null };
  root.innerHTML =
    heading(i18nMessages.mc243416c93c07db7, i18nMessages.m21212371a7632e3b) +
    `<div data-live></div>` +
    panel(
      i18nMessages.m37c1e84c75e34529,
      `${i18nMessages.ma09aa3187840096f}${select(
        "model",
        i18nMessages.mc98e118e0a43f078,
        MODEL_OPTIONS,
        "m3",
      )}${select(
        "mirror",
        i18nMessages.m012f0f4b569f1b90,
        MIRROR_OPTIONS,
        "auto",
      )}<div data-mirror-custom hidden>${input(
        "mirror-custom",
        i18nMessages.mad70940de1eb29fc,
        "https://",
        { placeholder: i18nMessages.m257fe10d0ecb3248 },
      )}${i18nMessages.m6941234208ff9cd8}`,
    ) +
    panel(
      i18nMessages.m53ebb8d00517a375,
      `<form data-recall class="panel-body stack">${select(
        "mode",
        i18nMessages.m62526c9ea059f75d,
        [
          ["fast", i18nMessages.m14391b39f3fb9682],
          ["quality", i18nMessages.mc7915d37c4e567a3],
        ],
        "fast",
      )}${i18nMessages.mf951f4d56310d8a5}`,
    ) +
    panel(
      i18nMessages.me2eeeb0f8c4000a6,
      `${i18nMessages.m96dcffba614d075b}${select(
        "fusion",
        i18nMessages.m44de59339437df88,
        FUSION_MODES,
        "legacy",
      )}${i18nMessages.m0b574560b24d3c97}${FUSION_NUMERIC.map(
        ([key, label, value]) =>
          input(key, label, value, { type: "number", step: "any" }),
      ).join(
        "",
      )}${i18nMessages.mc2aeb76c2f3e85c5}`,
    ) +
    panel(
      i18nMessages.m5446a708c510b87c,
      `<form data-api class="panel-body stack"><div class="form-grid">${input("base", i18nMessages.md94c3fb876621695, "https://api.deepseek.com/v1", { required: true })}${input("model", i18nMessages.mc98e118e0a43f078, "deepseek-flash", { required: true })}</div>${input("key", i18nMessages.m6f390ff0a6915223, "", { type: "password" })}${i18nMessages.ma276baf02d7ae01d}`,
    ) +
    `<div data-result class="section-gap"></div>`;
  $("[data-model=providers]", root)?.remove();
  $("[data-engine-hint]", root).textContent = "";
  const live = $("[data-live]", root);
  live.innerHTML = `<div class="grid"><div data-inference-status></div>${panel(
    i18nMessages.m281dccfa29c6c9fb,
    `<div class="panel-body"><div data-model-task-progress><div data-model-task-content></div><div class="actions"><button type="button" data-model="reopen-download" hidden>${h(i18nMessages.mDownloadShowProgress)}</button>${modelCancelButton(root)}</div></div><p class="form-error section-gap" data-model-task-error hidden></p>${i18nMessages.m31f6729bd220085e}</pre></details>`,
  )}</div>`;
  let profileRefreshing = false;
  let disposed = false;
  function currentMirror() {
    const value = $("[data-mirror] [name=mirror]", root).value;
    return value === "custom"
      ? $("[name=mirror-custom]", root).value.trim()
      : value;
  }
  const {
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
  } = createModelsView(view, { root, ctx, live });
  function handleModeSelect(modeId) {
    if (!MODEL_MODES.some((mode) => mode.id === modeId)) return;
    view.selectedMode = modeId;
    localStorage.setItem("respire-model-mode", modeId);
    view.stateOverride = null;
    const mode = currentMode();
    if (
      mode.requiresGPU &&
      !performanceAvailable(ctx.desktop?.platform)
    )
      notify(i18nMessages.m73e343ed81463a71, true);
    renderDynamic(true);
  }
  async function handlePrimaryAction() {
    const mode = currentMode();
    const model = view.modelID;
    const scope = activeModelScope(ctx.desktop?.health);
    const engine =
      mode.engine === "auto"
        ? detectBestEngine(ctx.desktop?.platform)
        : mode.engine;
    const cpuBaseline = model === "m3" && engine === "cpu" && mode.variant === "quantized";
    const mirror = currentMirror();
    let inventory = view.inventory;
    const ready = () => inventory?.tokenizer_installed &&
      inventory.variants.some((variant) => variant.name === mode.variant && variant.installed) &&
      (!cpuBaseline || inventory.cpu_container_ready);
    const downloads = [];
    if (!cpuBaseline && !isProfileModel(model) && !inventory?.tokenizer_installed) downloads.push("tokenizer");
    if (!modeVariant(mode)?.installed || (isProfileModel(model) && !inventory?.tokenizer_installed) ||
        (cpuBaseline && !ready())) downloads.push(cpuBaseline ? "baseline" : mode.variant);
    for (const [index, variant] of downloads.entries()) {
      const modal = openDownloadModal({ model, variant, label: mode.title, getMirror: () => mirror });
      if (!await modal.done || scope !== activeModelScope(ctx.desktop?.health)) {
        await ctx.refresh();
        return;
      }
      if (index + 1 < downloads.length && modal.element.open) modal.element.close();
      inventory = await ctx.modelInventory.load(model);
    }
    if (ready()) await applyEngineAfterDownload(mode, engine, model);
    else if (!disposed) update();
  }
  function acceptRuntimeActive(details, scope) {
    if (details?.activeKnown && scope && scope === activeModelScope(ctx.desktop?.health)) {
      view.runtimeActive = details.active;
      view.runtimeActiveScope = scope;
    }
  }
  async function applyEngineAfterDownload(mode, engine, model) {
    const scope = activeModelScope(ctx.desktop?.health);
    try {
      view.switchInFlight = true;
      view.stateOverride = null;
      if (!disposed) update();
      const result = await applyEngineChoice({
        model,
        engine,
        variant: mode.variant,
      });
      acceptRuntimeActive(result.details, scope);
      if (!isProfileModel(model) && scope === activeModelScope(ctx.desktop?.health) &&
          view.runtimeActive === null) {
        view.legacyEngineState = { engine: result.engine, model: result.variant };
      }
      if (result.fallback || result.failed) {
        // Reflect what the runtime actually fell back to; the mode tag and
        // the status card must not advertise the failed selection.
        if (isProfileModel(view.modelID)) {
          const active = result.details?.active;
          if (active) view.engineState = { engine: active.engine, model: active.variant };
        } else {
          view.engineState = { engine: result.engine, model: result.variant };
        }
        view.stateOverride = PAGE_STATES.FALLBACK;
        notify(result.warning || i18nMessages.m25d9f10996f6661b, true);
      } else {
        view.engineState = { engine: result.engine, model: result.variant };
        view.lastVerifiedAt = new Date();
        notify(`${i18nMessages.m4ffb3ce1d19caaab}${mode.title}${i18nMessages.m20a8e6cac8b15e1d}`);
      }
      if (result.envelope && root.isConnected)
        $("[data-result]", root).innerHTML = resultView(
          i18nMessages.m382c41a1a6270019,
          result.envelope,
        );
    } catch (error) {
      view.stateOverride = PAGE_STATES.ERROR;
      notify(error.message, true);
    } finally {
      view.switchInFlight = false;
      if (!disposed) {
        renderDynamic(true);
        renderVariants();
      }
      await ctx.refresh();
    }
  }
  function update() {
    const s = ctx.status?.summary || {},
      health = ctx.desktop?.health || {},
      inference = health.inference || {},
      index = s.retrieval_index || {},
      op = liveModelOperation(view, ctx.status);
    $("[data-inference-status]", live).innerHTML = panel(
      i18nMessages.me46b0272bd641090,
      pairs([
        [i18nMessages.m8168e2d3bbb44b32, phase(inference.phase)],
        [
          i18nMessages.m33ceb11047a0786a,
          inference.worker_active === undefined
            ? "—"
            : inference.worker_active
              ? i18nMessages.mab27f80d046f3d7f
              : i18nMessages.me6fc5eb8c3b83c01,
        ],
        [
          i18nMessages.m01ee528ff519b884,
          `${number(inference.foreground?.active ?? inference.active)} / ${number(inference.queued)}`,
        ],
        [i18nMessages.m11676e8ace776256, phase(index.state)],
        [
          i18nMessages.m70dee7f8303c7d74,
          index.running ? i18nMessages.m0a53c8de45a1ed4a : index.scheduled ? i18nMessages.m2fcab8f67ceeae3e : i18nMessages.mdae661d17c2dd8f0,
        ],
        [i18nMessages.mIndexCounts, `${number(s.indexed_count)} / ${number(s.local_alive)}`],
      ]),
    );
    const task = ctx.desktop?.task;
    const session = activeDownloadSession();
    const progress = op.active ? modelOperationProgress(op)
      : modelTaskBusy(task) ? downloadTaskProgress(task)
      : index.running || index.scheduled || view.indexInFlight
        ? indexTaskProgress(index.scheduled && !index.running ? i18nMessages.m2fcab8f67ceeae3e : i18nMessages.m0a53c8de45a1ed4a, s)
        : `<p>${h(i18nMessages.m707bd8db0c65b214)}</p>`;
    $("[data-model-task-content]", live).innerHTML = progress;
    $('[data-model="reopen-download"]', live).hidden = !session;
    const error = $("[data-model-task-error]", live);
    error.hidden = !index.error;
    error.textContent = index.error || "";
    $("details pre", live).textContent = raw({ inference, index, model: op });
    syncProfilePolling(op);
    updateVariantsProgress();
    renderDynamic();
    renderEngineHint();
    // Ask Runtime to verify and index after all CPU baseline files arrive.
    if (view.inventory && view.modelID === "m3") {
      const baselineReady = view.inventory.tokenizer_installed && view.inventory.cpu_container_ready &&
        view.inventory.variants.some((variant) => variant.name === "quantized" && variant.installed);
      if (!baselineReady) view.sawMissingBaseline = true;
      else if (view.sawMissingBaseline && !view.baselinePrepared && !view.switchInFlight) {
        view.baselinePrepared = true;
        const mirror = currentMirror();
        runIndexedAction("model_prepare", {
          mirror: mirror && mirror !== "auto" ? mirror : "auto",
        })
          .then(() => notify(i18nMessages.m17ba200c46586b47))
          .catch((error) => notify(error.message, true));
      }
    }
  }
  async function load() {
    if (loading || !ctx.desktop?.connected) return;
    loading = true;
    loaded = true;
    const scope = activeModelScope(ctx.desktop?.health);
    try {
      const savedModel = localStorage.getItem("respire-model");
      if (savedModel && MODEL_OPTIONS.some(([id]) => id === savedModel))
        view.modelID = savedModel;
      const mirror = ctx.desktop.preferences.modelMirror || "auto";
      const mirrorSel = $("[data-mirror] [name=mirror]", root);
      const customMirror = ![...mirrorSel.options].some((option) => option.value === mirror);
      mirrorSel.value = customMirror ? "custom" : mirror;
      $("[data-mirror-custom]", root).hidden = !customMirror;
      $("[name=mirror-custom]", root).value = customMirror ? mirror : "";
      $("[data-mirror-state]", root).textContent = i18nMessages.m1f45e225299def52 + mirror;
      const jobs = await api("operations");
      const activeRebuild = jobs.find((job) => job.name === "index_rebuild" && job.state === "running");
      if (activeRebuild && !disposed && view.rebuildJobID !== activeRebuild.id) {
        view.rebuildJobID = activeRebuild.id;
        view.indexInFlight = true;
        syncProfilePolling(liveModelOperation(view, ctx.status));
        followRebuild(activeRebuild, root, ctx, () => !disposed && scope === activeModelScope(ctx.desktop?.health), () => {
          if (view.rebuildJobID !== activeRebuild.id) return;
          view.rebuildJobID = null;
          view.indexInFlight = false;
          if (!disposed) update();
        }).catch((error) => notify(error.message, true));
      }
      fusion.requestId = jobs.find((job) => job.name === "agent_set" &&
        job.state === "unconfirmed" && (!job.transaction || job.transaction.kind === "core_config"))?.id || null;
      const values = await Promise.allSettled([
        execute("agent_config"),
        execute("model_engine"),
        execute("classification_config", { backend: "ds" }),
        ctx.modelInventory.load(view.modelID),
        fetchProfileState(),
      ]);
      if (!root.isConnected) return;
      if (values[0].status === "fulfilled") {
        agent = summary(values[0].value);
        fusion.loadFailed = false;
        adoptFusion(agent.recall_fusion_runtime);
        $("[data-recall] [name=mode]", root).value =
          agent.recall_mode || "fast";
      }
      if (values[1].status === "fulfilled") {
        const state = summary(values[1].value);
        view.engineState = {
          engine: state.engine || "cpu",
          model:
            state.model ||
            (state.engine === "gpu" || state.engine === "npu"
              ? "fp16"
              : "quantized"),
        };
        view.legacyEngineState = view.engineState;
      }
      if (values[2].status === "fulfilled") backend = summary(values[2].value);
      if (values[3].status === "fulfilled") {
        const inventory = values[3].value;
        if (inventory.model && MODEL_OPTIONS.some(([id]) => id === inventory.model)) {
          view.modelID = inventory.model;
        }
      }
      if (values[4].status === "fulfilled") {
        view.profileState = values[4].value;
        // The runtime's catalog carries what is actually active; the selector
        // keeps an explicit local choice when one exists.
        const { active, activeKnown, switchState } = readProfileState(view.profileState);
        acceptRuntimeActive({ active, activeKnown }, scope);
        const activeModel = active?.model || "";
        if (activeModel && !view.switchInFlight) {
          const known = MODEL_OPTIONS.some(([id]) => id === activeModel);
          if (!savedModel && known) view.modelID = activeModel;
          if (view.modelID === activeModel) {
            if (view.modelID !== (view.inventory?.model || "")) {
              await ctx.modelInventory.load(view.modelID);
            }
            view.engineState = {
              engine: active.engine || "cpu",
              model: active.variant || graniteVariant(active.model) || "quantized",
            };
          }
        }
        // A switch left pending by a previous page session keeps being
        // watched so its outcome still reaches the notice.
        if (!view.switchInFlight && switchState.state === "pending") {
          view.switchInFlight = true;
          view.profileSwitchPending = true;
          view.switchingVariant = "";
          view.switchTicks = 0;
          view.switchProgressKey = "";
          notify(i18nMessages.m6119b43db7c76cb2);
        }
      } else {
        view.profileState = null;
      }
      // Existing installations map back to a mode card; a stored preference
      // always wins over the mapping.
      const savedMode = localStorage.getItem("respire-model-mode");
      view.selectedMode =
        savedMode && MODEL_MODES.some((mode) => mode.id === savedMode)
          ? savedMode
          : modeForVariant(view.engineState.model);
      if (values[0].status === "rejected") {
        fusion.loadFailed = true;
        renderFusion();
      }
      $("[data-mirror] [name=model]", root).value = view.modelID;
      $("[data-api] [name=base]", root).value =
        agent.recall_api_base ||
        backend.api_base ||
        "https://api.deepseek.com/v1";
      $("[data-api] [name=model]", root).value =
        agent.recall_model || backend.model || "deepseek-flash";
      $("[data-key-state]", root).textContent =
        values[2].status === "rejected"
          ? i18nMessages.m12903930a68716f9
          : backend.has_key
            ? i18nMessages.m9e0f15cc43fbafbb
            : i18nMessages.mdcf6e6f07cd5a433;
      for (const [index, result] of values.entries()) {
        if (result.status === "rejected" && index !== 1 && index !== 3 && index !== 4)
          notify(result.reason.message, true);
      }
      renderVariants();
      renderDynamic(true);
      update();
    } catch (error) {
      notify(error.message, true);
    } finally {
      loading = false;
    }
  }
  // Model switcher: persist the choice and reload the weight table for it.
  $("[data-mirror] [name=model]", root).addEventListener("change", (event) => {
    const next = event.target.value;
    if (!MODEL_OPTIONS.some(([id]) => id === next) || next === view.modelID) return;
    view.modelID = next;
    localStorage.setItem("respire-model", view.modelID);
    const node = $("[data-variants]", root);
    if (node) node.innerHTML = i18nMessages.m6cb3cc3ccc61b15a;
    (async () => {
      try {
        await ctx.modelInventory.load(view.modelID);
        await refreshProfileState();
        renderVariants();
      } catch (error) {
        notify(error.message, true);
        renderVariants();
      }
    })();
  });
  function syncProfilePolling(op) {
    const polling = view.profileSwitchPending || (!disposed && (view.switchInFlight || view.indexInFlight || op.active));
    if (polling && !view.switchTimer) {
      view.switchTimer = setInterval(async () => {
        try {
          if (view.indexInFlight) await ctx.refresh();
          await refreshProfileState();
        } catch (error) {
          notify(error.message, true);
        }
      }, 3000);
    } else if (!polling && view.switchTimer) {
      clearInterval(view.switchTimer);
      view.switchTimer = null;
    }
  }
  async function runIndexedAction(name, args) {
    view.indexInFlight = true;
    syncProfilePolling(liveModelOperation(view, ctx.status));
    try {
      return await execute(name, args);
    } finally {
      view.indexInFlight = Boolean(view.rebuildJobID);
      await refreshProfileState();
    }
  }
  async function refreshProfileState() {
    if (profileRefreshing) return;
    profileRefreshing = true;
    const scope = activeModelScope(ctx.desktop?.health);
    const snapshot = ctx.status?.summary?.model_operation;
    try {
      const state = await fetchProfileState();
      if (state && scope === activeModelScope(ctx.desktop?.health)) {
        view.profileState = state;
        view.runtimeProgress = readProfileState(state).operation;
        if (typeof view.runtimeProgress.active === "boolean") view.profileStatus = snapshot;
      }
    } finally {
      profileRefreshing = false;
    }
    if (scope !== activeModelScope(ctx.desktop?.health)) return;
    const { active, activeKnown, switchState, operation } = readProfileState(view.profileState);
    acceptRuntimeActive({ active, activeKnown }, scope);
    if (active?.model === view.modelID) {
      view.engineState = {
        engine: active.engine || "cpu",
        model: active.variant || graniteVariant(active.model) || view.switchingVariant || "quantized",
      };
    }
    if (view.profileSwitchPending) {
      const verdict = profileStateSettled(view.profileState);
      // Allow 30 minutes without progress, matching the Runtime's idle timeout.
      const { key, ticks, expired } = nextSwitchStallState(view.switchProgressKey, view.switchTicks, operation);
      view.switchProgressKey = key;
      view.switchTicks = ticks;
      if (verdict.settled || expired) {
        view.switchInFlight = false;
        view.profileSwitchPending = false;
        view.switchingVariant = "";
        if (!verdict.failed && !expired) view.lastVerifiedAt = new Date();
        view.switchTicks = 0;
        view.switchProgressKey = "";
        notify(
          expired
            ? i18nMessages.m1fdbb18d383794e2
            : verdict.failed
              ? `${i18nMessages.m92cab07534b96c14}${switchState.error ? `${i18nMessages.mf114611cd61000bf}${switchState.error}` : ""}${i18nMessages.m652f974167819ea3}`
              : `${i18nMessages.m74ee41c4e16d47f2}${view.modelID} · ${view.engineState.model}${i18nMessages.md7fe6fd5e14d390c}`,
          expired || verdict.failed,
        );
        if (root.isConnected) {
          await ctx.refresh();
        }
      }
    }
    if (!disposed) update();
    else syncProfilePolling({});
  }
  async function refreshM3Inventory() {
    if (!root.isConnected || view.modelID !== "m3") return true;
    try {
      const inventory = await ctx.modelInventory.load("m3");
      if (root.isConnected && view.modelID === "m3" && inventory.model === "m3") {
        renderVariants();
        renderDynamic(true);
      }
      return true;
    } catch (error) {
      if (root.isConnected) notify(error.message, true);
      return false;
    }
  }
  bindModelActions(view, {
    root,
    renderAdvancedVisibility,
    handleModeSelect,
    handlePrimaryAction,
    refreshProfileState,
    runIndexedAction,
    update,
    renderVariants,
    ctx,
    currentMirror,
    load,
    refreshM3Inventory,
    acceptRuntimeActive,
  });
  const { adoptFusion, renderFusion } = bindModelSettings({ root, ctx, fusion, currentMirror, load });
  update();
  const dispose = recallChecks(root);
  if (ctx.desktop?.connected) load();
  return {
    dispose() {
      disposed = true;
      syncProfilePolling({});
      dispose();
    },
    update() {
      update();
      if (!loaded && ctx.desktop?.connected) load();
    },
    refresh: load,
  };
}
