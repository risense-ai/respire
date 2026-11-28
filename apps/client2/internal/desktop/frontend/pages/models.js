import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { $, action, api, confirm, execute, h, input, notify, panel, raw, select, summary } from "../ui/core.js";
import { t } from "../ui/i18n.js";
import { heading } from "./overview.js";
import { recallChecks } from "./recall.js";
import {
  activeDownloadSession, activeModelScope, activeVariantSelection, CURATED_MODELS, DEFAULT_MODEL,
  defaultVariantForModel, engineAvailable, fetchProfileState, marksActiveVariant, openDownloadModal,
  profileStateSettled, readProfileState, submitProfileSwitch,
} from "./model-kit.js";
import { bindDownloadMirror, catalogMarkup, curatedModel, downloadedVariant, renderCatalog } from "./models-catalog-view.js";
import { openModelProbeModal } from "./model-probe-modal.js";
import { liveModelOperation, modelActivityTimeline, modelTaskBusy, modelTaskState, nextSwitchStallState, syncModelCancel } from "./models-view.js";
import { renderLiveInference, servingModel } from "./models-status.js";
import { bindModelActions, followRebuild } from "./models-actions.js";
import { bindModelSettings, FUSION_MODES, FUSION_NUMERIC } from "./models-settings.js";

const activityByContext = new WeakMap();

export function models(root, ctx) {
  const savedModel = curatedModel(localStorage.getItem("respire-model"));
  const view = {
    modelID: savedModel?.id || DEFAULT_MODEL,
    variantID: defaultVariantForModel(savedModel?.id || DEFAULT_MODEL),
    engine: "cpu",
    inventoryErrors: new Set(),
    get inventory() { return ctx.modelInventory.get(this.modelID); },
    activeKnown: () => Boolean(scope() && view.runtimeActiveScope === scope() && view.runtimeActive !== undefined),
  };
  const fusion = { runtime: null, draft: null, checked: false, loadFailed: false, requestId: null };
  let loading = false, loadedScope, disposed = false, selectionTouched = false;
  let profileRefreshing = false, switchTimer;
  let switchProgress = { key: "", ticks: 0 };
  const scope = () => ctx.desktop?.connected ? activeModelScope(ctx.desktop.health) : null;
  const isCurrent = (owner) => !disposed && owner === scope();
  const activeSelection = () => view.activeKnown() ? activeVariantSelection(view.runtimeActive, view.legacyEngineState) : null;
  const busy = () => view.mutating || view.switchInFlight || view.indexInFlight || liveModelOperation(view, ctx.status).active ||
    activeDownloadSession() || modelTaskBusy(ctx.desktop?.task);
  let snapshot, downloadCancel;

  root.innerHTML = heading(i18nMessages.mc243416c93c07db7, "") +
    `<div data-live class="grid section-gap"><div data-inference-status></div><section class="panel model-status-card" data-models-tasks-card>
      <header class="panel-heading"><h2>${h(i18nMessages.modelTasksPipeline)}</h2>
        <button type="button" class="secondary" data-copy-snapshot>${h(i18nMessages.modelCopySnapshot)}</button>
      </header>
      <div class="model-status-body" data-model-task-progress><div data-model-task-content></div><div class="actions">
        <button type="button" data-model="reopen-download" hidden>${h(i18nMessages.mDownloadShowProgress)}</button>
        <button type="button" data-model="cancel" hidden>${h(i18nMessages.modelCancelTask)}</button>
      </div><p class="form-error" data-model-task-error hidden></p></div>
      <footer class="model-status-footer" data-recent-activity><h3>${h(i18nMessages.modelRecentActivity)}</h3><div data-activity-entries></div></footer>
    </section></div>` + catalogMarkup() + panel(
      i18nMessages.m53ebb8d00517a375,
      `<form data-recall class="panel-body stack">${select("mode", i18nMessages.m62526c9ea059f75d, [
        ["fast", i18nMessages.m14391b39f3fb9682], ["quality", i18nMessages.mc7915d37c4e567a3],
      ], "fast")}${i18nMessages.mf951f4d56310d8a5}`,
    ) + panel(
      i18nMessages.me2eeeb0f8c4000a6,
      `${i18nMessages.m96dcffba614d075b}${select("fusion", i18nMessages.m44de59339437df88, FUSION_MODES, "legacy")}${i18nMessages.m0b574560b24d3c97}${FUSION_NUMERIC.map(
        ([key, label, value]) => input(key, label, value, { type: "number", step: "any" }),
      ).join("")}${i18nMessages.mc2aeb76c2f3e85c5}`,
    ) + panel(
      i18nMessages.m5446a708c510b87c,
      `<form data-api class="panel-body stack"><div class="form-grid">${input("base", i18nMessages.md94c3fb876621695, "https://api.deepseek.com/v1", { required: true })}${input("model", i18nMessages.mc98e118e0a43f078, "deepseek-flash", { required: true })}</div>${input("key", i18nMessages.m6f390ff0a6915223, "", { type: "password" })}${i18nMessages.ma276baf02d7ae01d}`,
    ) + `<div data-result class="section-gap"></div>`;

  const live = $("[data-live]", root);
  const cancel = $('[data-model="cancel"]', live);
  const copy = $("[data-copy-snapshot]", live);
  const mirror = bindDownloadMirror(root, ctx);
  copy.addEventListener("click", () => action(copy, async () => {
    await navigator.clipboard.writeText(raw(snapshot));
    notify(t(i18nMessages.modelSnapshotCopied));
  }));
  cancel.addEventListener("click", (event) => {
    if (!downloadCancel) return;
    event.stopPropagation();
    return action(cancel, downloadCancel);
  });

  function selectModel(id) {
    const active = activeSelection();
    view.modelID = id;
    view.variantID = active?.model === id ? active.variant : defaultVariantForModel(id);
    view.engine = active?.model === id && engineAvailable(ctx.desktop?.platform, active.engine) ? active.engine : "cpu";
  }

  function update() {
    if (disposed) return;
    const status = ctx.status?.summary || {};
    const health = ctx.desktop?.connected ? ctx.desktop.health : undefined;
    const inference = health?.inference || {};
    const index = status.retrieval_index || health?.retrieval_index || {};
    const op = liveModelOperation(view, ctx.status);
    const active = activeSelection();
    const model = servingModel(active, active && ctx.modelInventory.get(active.model));
    const servingIndex = Boolean(active) && index.state !== "failed";
    const session = activeDownloadSession();
    const desktopTask = ctx.desktop?.task;
    const task = modelTaskBusy(desktopTask) && (!session || !session.id || desktopTask.id === session.id) ? desktopTask : null;
    const downloadModel = session?.model || op.downloadModel;
    const variant = downloadModel && ctx.modelInventory.get(downloadModel)?.variants.find(
      ({ name }) => name === (session?.variant || op.downloadVariant));
    const state = modelTaskState({ op, session, task, index, view, status, servingIndex, variant,
      workerReady: inference.worker_active ?? (inference.worker_pid ? true : undefined), weightsReady: model.installed,
      mirror: ctx.desktop?.preferences?.modelMirror || "auto" });
    const shadow = state.kind === "switch" ? op : null;
    $("[data-inference-status]", live).innerHTML = renderLiveInference({ active, model, health, status, shadow, servingIndex,
      buildingShadow: state.step === 2 });
    $("[data-model-task-content]", live).innerHTML = state.markup;
    const owner = scope();
    let activity = activityByContext.get(ctx);
    if (owner && owner !== activity?.owner) {
      activity = { owner, render: modelActivityTimeline() };
      activityByContext.set(ctx, activity);
    }
    $("[data-activity-entries]", live).innerHTML = owner && activity?.render(state, session?.id || task?.id || op.id)
      || `<p>${h(i18nMessages.modelNoRecentActivity)}</p>`;
    $('[data-model="reopen-download"]', live).hidden = !session;
    const error = $("[data-model-task-error]", live);
    error.hidden = !index.error;
    error.textContent = index.error || "";
    snapshot = { inference, index, model: op };
    cancel.textContent = t(state.cancelLabel || i18nMessages.modelCancelTask);
    syncModelCancel(live, view, op);
    downloadCancel = null;
    if (state.kind === "download" && (session || task)) {
      const modalCancel = session?.element.querySelector('[data-dl="cancel"]');
      if (session?.id && modalCancel && !modalCancel.hidden) downloadCancel = () => modalCancel.click();
      else if (!session && task?.id) downloadCancel = async () => {
        await api("model-download-cancel", { id: task.id, pause: false });
        await ctx.refresh();
      };
      cancel.hidden = cancel.disabled = !downloadCancel;
    }
    syncProfilePolling();
    renderCatalog(root, ctx, view, active, busy());
  }

  async function loadInventory(model, owner) {
    view.inventoryErrors.delete(model);
    try {
      return await ctx.modelInventory.load(model);
    } catch (error) {
      if (isCurrent(owner)) {
        view.inventoryErrors.add(model);
        if (model === view.modelID) notify(error.message, true);
      }
    } finally {
      if (isCurrent(owner)) update();
    }
  }

  function acceptProfileState(state, owner, snapshot) {
    view.profileState = state;
    const { active, activeKnown, operation } = readProfileState(state);
    view.runtimeActive = activeKnown ? active : undefined;
    view.runtimeActiveScope = owner;
    view.runtimeProgress = operation;
    if (typeof operation.active === "boolean") view.profileStatus = snapshot;
  }

  async function load() {
    if (loading || !ctx.desktop?.connected) return;
    const owner = scope();
    const initial = loadedScope !== owner;
    const snapshot = ctx.status?.summary?.model_operation;
    loading = true;
    loadedScope = owner;
    if (initial) {
      mirror.sync();
      view.profileState = view.runtimeProgress = null;
      view.profileSwitchPending = view.switchInFlight = false;
      view.rebuildJobID = null;
      view.indexInFlight = false;
    }
    const inventory = Promise.all(CURATED_MODELS.map(({ id }) => loadInventory(id, owner)));
    try {
      const [agentResult, engineResult, backendResult, jobsResult, state] = await Promise.all([
        ...[execute("agent_config"), execute("model_engine"), execute("classification_config", { backend: "ds" }), api("operations")]
          .map((request) => request.then((value) => ({ value }), (error) => ({ error }))),
        fetchProfileState(),
      ]);
      if (!isCurrent(owner)) return;
      const jobs = jobsResult.value || [];
      const job = jobs.find(({ name, state }) => name === "index_rebuild" && state === "running");
      if (job && view.rebuildJobID !== job.id) {
        view.rebuildJobID = job.id;
        view.indexInFlight = true;
        followRebuild(job, root, ctx, () => isCurrent(owner), () => {
          if (view.rebuildJobID !== job.id) return;
          view.rebuildJobID = null;
          view.indexInFlight = false;
          update();
        }).catch((error) => notify(error.message, true));
      }
      fusion.requestId = jobs.find((job) => job.name === "agent_set" && job.state === "unconfirmed" &&
        (!job.transaction || job.transaction.kind === "core_config"))?.id || null;
      const agent = summary(agentResult.value) || {};
      const backend = summary(backendResult.value) || {};
      if (agentResult.value) adoptFusion(agent.recall_fusion_runtime);
      fusion.loadFailed = Boolean(agentResult.error);
      renderFusion();
      $("[data-recall] [name=mode]", root).value = agent.recall_mode || "fast";
      $("[data-api] [name=base]", root).value = agent.recall_api_base || backend.api_base || "https://api.deepseek.com/v1";
      $("[data-api] [name=model]", root).value = agent.recall_model || backend.model || "deepseek-flash";
      $("[data-key-state]", root).textContent = backendResult.error ? i18nMessages.m12903930a68716f9 :
        backend.has_key ? i18nMessages.m9e0f15cc43fbafbb : i18nMessages.mdcf6e6f07cd5a433;
      for (const result of [agentResult, backendResult, jobsResult]) {
        if (result.error) notify(result.error.message, true);
      }
      view.legacyEngineState = summary(engineResult.value);
      if (state) acceptProfileState(state, owner, snapshot);
      if (initial && !selectionTouched) {
        const current = curatedModel(activeSelection()?.model);
        selectModel(savedModel?.id || current?.id || DEFAULT_MODEL);
      }
      if (!view.switchInFlight && readProfileState(state).switchState.state === "pending") {
        view.switchInFlight = view.profileSwitchPending = true;
        switchProgress = { key: "", ticks: 0 };
      }
      update();
    } finally {
      await inventory;
      loading = false;
      if (!disposed && ctx.desktop?.connected && loadedScope !== scope()) void load();
    }
  }

  function syncProfilePolling() {
    const operationActive = () => view.profileSwitchPending || view.switchInFlight || view.indexInFlight ||
      liveModelOperation(view, ctx.status).active;
    const inferenceActive = () => Boolean(ctx.desktop?.health?.inference?.foreground?.active);
    const polling = !disposed && (operationActive() || inferenceActive());
    if (polling && !switchTimer) {
      switchTimer = setInterval(async () => {
        try {
          if (view.indexInFlight || activeDownloadSession() || inferenceActive()) await ctx.refresh();
          if (operationActive()) await refreshProfileState();
          else syncProfilePolling();
        } catch (error) {
          notify(error.message, true);
        }
      }, 3000);
    } else if (!polling && switchTimer) {
      clearInterval(switchTimer);
      switchTimer = null;
    }
  }

  async function refreshProfileState() {
    if (profileRefreshing || disposed) return;
    profileRefreshing = true;
    const owner = scope();
    const snapshot = ctx.status?.summary?.model_operation;
    try {
      const state = await fetchProfileState();
      if (!isCurrent(owner)) return;
      if (state) acceptProfileState(state, owner, snapshot);
      if (view.profileSwitchPending) {
        const { switchState, operation } = readProfileState(state);
        const verdict = profileStateSettled(state);
        switchProgress = nextSwitchStallState(switchProgress.key, switchProgress.ticks, operation);
        if (verdict.settled || switchProgress.expired) {
          view.switchInFlight = view.profileSwitchPending = false;
          notify(switchProgress.expired ? i18nMessages.m1fdbb18d383794e2 : verdict.failed
            ? switchState.error || i18nMessages.m96cf31ee716f8903 : i18nMessages.modelEnabled,
          switchProgress.expired || verdict.failed);
          await ctx.refresh();
        }
      }
      update();
    } finally {
      profileRefreshing = false;
    }
  }

  async function runIndexedAction(name) {
    view.indexInFlight = true;
    update();
    try {
      return await execute(name);
    } finally {
      view.indexInFlight = Boolean(view.rebuildJobID);
      await refreshProfileState();
    }
  }

  async function dockAction(operation) {
    const { modelID: model, variantID, engine, inventory } = view;
    const variant = inventory?.variants.find(({ name }) => name === variantID);
    if (!variant || !ctx.desktop?.connected) return;
    const owner = scope();
    const active = activeSelection();
    const selection = { model, variant: variantID, engine };
    const installed = downloadedVariant(inventory, variant);
    if (operation === "open" && marksActiveVariant(active, model, variantID, engine)) {
      notify(await api("open-model-dir", { path: variant.path }));
      return;
    }
    if (busy()) return;
    if (operation === "probe" && (installed || marksActiveVariant(active, model, variantID, engine)) &&
        engineAvailable(ctx.desktop.platform, engine)) {
      openModelProbeModal(selection, ctx.desktop, variant.path);
      return;
    }
    if (operation === "download" && !installed) {
      openDownloadModal({ model, variant: variantID, label: curatedModel(model).title, getMirror: mirror.value,
        onDone: async () => {
          if (!isCurrent(owner)) return;
          view.mutating = true;
          try {
            await loadInventory(model, owner);
            if (isCurrent(owner)) await ctx.refresh();
          } catch (error) {
            if (isCurrent(owner)) notify(error.message, true);
          } finally {
            view.mutating = false;
            if (isCurrent(owner)) update();
          }
        },
      });
      update();
      return;
    }
    if (!installed || !view.activeKnown()) return;
    if (operation === "delete" && !marksActiveVariant(active, model, variantID)) {
      if (!await confirm(i18nMessages.modelDelete, i18nMessages.modelDeleteConfirm, i18nMessages.modelDelete) ||
          !isCurrent(owner) || busy() || marksActiveVariant(activeSelection(), model, variantID)) return;
      view.mutating = true;
      update();
      try {
        await api("model-delete", { model, variant: variantID });
        if (isCurrent(owner)) await loadInventory(model, owner);
      } finally {
        view.mutating = false;
        update();
      }
    } else if (operation === "enable" && !marksActiveVariant(active, model, variantID, engine) &&
        engineAvailable(ctx.desktop.platform, engine)) {
      view.switchInFlight = true;
      update();
      try {
        const receipt = await submitProfileSwitch(model, engine, variantID);
        if (isCurrent(owner)) {
          view.profileSwitchPending = !receipt?.settled;
          switchProgress = { key: "", ticks: 0 };
          if (receipt?.settled) notify(i18nMessages.modelEnabled);
        }
      } finally {
        view.switchInFlight = Boolean(view.profileSwitchPending);
        if (isCurrent(owner)) {
          await refreshProfileState();
          await ctx.refresh();
        }
      }
    }
  }

  root.addEventListener("click", (event) => {
    const button = event.target.closest("[data-catalog-model], [data-catalog-variant], [data-catalog-engine], [data-catalog-action]");
    if (!button || button.disabled) return;
    const { catalogModel, catalogVariant, catalogEngine, catalogAction } = button.dataset;
    if (catalogAction) return action(button, () => dockAction(catalogAction));
    if (catalogModel && curatedModel(catalogModel) && catalogModel !== view.modelID) {
      selectModel(catalogModel);
      localStorage.setItem("respire-model", catalogModel);
      if (!view.inventory && ctx.desktop?.connected) void loadInventory(catalogModel, scope());
    } else if (catalogVariant && view.inventory?.variants.some(({ name }) => name === catalogVariant)) {
      view.variantID = catalogVariant;
    } else if (catalogEngine && engineAvailable(ctx.desktop?.platform, catalogEngine)) {
      view.engine = catalogEngine;
    }
    selectionTouched = true;
    update();
  });

  bindModelActions(view, { root, ctx, refreshProfileState, runIndexedAction });
  const { adoptFusion, renderFusion } = bindModelSettings({ root, ctx, fusion, load });
  mirror.sync();
  update();
  const disposeRecall = recallChecks(root);
  if (ctx.desktop?.connected) void load();
  return {
    dispose() {
      disposed = true;
      syncProfilePolling();
      disposeRecall();
    },
    update() {
      update();
      if (ctx.desktop?.connected && loadedScope !== scope()) void load();
    },
    refresh: load,
  };
}
