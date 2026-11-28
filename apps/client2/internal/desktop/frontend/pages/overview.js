import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { t } from "../ui/i18n.js";
import {
  $,
  h,
  number,
  date,
  bytes,
  metric,
  panel,
  pairs,
  execute,
  action,
  isPending,
  notify,
} from "../ui/core.js";
import {
  activeModelScope,
  fetchProfileState,
  readProfileState,
} from "./model-kit.js";
import { overviewModelsPanel, overviewModelStatus } from "./overview-models.js";
import { modelOperationProgress, runtimeDownloadOperation } from "./models-view.js";

import { stats } from "./stats.js";

const phases = {
  idle: i18nMessages.mdae661d17c2dd8f0,
  running: i18nMessages.m01679253dade46b3,
  ok: i18nMessages.mf28461bb49c85647,
  err: i18nMessages.m336e8460fd284629,
  scheduled: i18nMessages.m2fcab8f67ceeae3e,
  ready: i18nMessages.mb073db9c0bd6f2ed,
  failed: i18nMessages.m28384d7afd2e4fa6,
  loading: i18nMessages.md04fcbda737fc0c6,
  unavailable: i18nMessages.m460b3574e4bdb8ca,
};
export const phase = (value) => phases[value] || value || "—";
export function heading(title, description, actions = "") {
  return `<div class="page-heading"><div><div class="eyebrow">RESPIRE / LOCAL WORKSPACE</div><h1>${h(title)}</h1><p>${h(description)}</p></div><div class="heading-actions">${actions}</div></div>`;
}
export function updateConnectionIndicator(indicator, connected) {
  const label = t(connected ? i18nMessages.m3053ee2f4c18204a : i18nMessages.mcef1537a814bdc68);
  if (indicator.textContent !== label) indicator.textContent = label;
  indicator.classList.toggle("good", Boolean(connected));
}
export function overview(root, ctx) {
  // Polls update status around these controls without replacing focused buttons.
  root.innerHTML =
    '<div data-overview-summary>' +
    heading(
      t(i18nMessages.me8705434968b58fd),
      i18nMessages.md4138a10d95dbb7a,
      '<span class="badge" role="status" data-runtime-connection></span>' +
        i18nMessages.mdea3a195d4b5e4d1,
    ) +
    '<div class="metrics" data-overview-metrics></div>' +
    '<div data-overview-status-error></div></div>' +
    '<section class="section-gap" data-overview-stats></section>' +
    `<div class="section-gap" data-overview-details><div class="grid">${panel(
      i18nMessages.mdcfe516d1114e14f, '<div data-overview-sync></div>',
    )}${overviewModelsPanel()}</div></div>` +
    panel(
      "Runtime",
      i18nMessages.m5fa416eee7f98137,
    ) +
    '<div data-overview-onboarding></div>';
  const connection = $("[data-runtime-connection]", root);
  const metricsRoot = $("[data-overview-metrics]", root);
  const statusErrorBanner = $("[data-overview-status-error]", root);
  const statsView = stats($("[data-overview-stats]", root), ctx);
  const detailsRoot = $("[data-overview-details]", root);
  const syncRoot = $("[data-overview-sync]", detailsRoot);
  const modelRoot = $("[data-overview-model-content]", detailsRoot);
  const runtimeStatus = $("[data-runtime-status]", root);
  const runtimeError = $("[data-runtime-error]", root);
  const startButton = $('[data-host="start"]', root);
  const stopButton = $('[data-host="stop"]', root);
  const onboarding = $("[data-overview-onboarding]", root);
  runtimeStatus.closest(".panel").classList.add("section-gap");
  // Onboarding: prompt/plugin readiness is checked once per page visit.
  let promptsReady = null;
  let profileSelection;
  let profileRequest;
  let hostPending = false;
  async function loadProfileSelection(state, operation, force = false) {
    if (!state.connected) {
      profileRequest = undefined;
      profileSelection = undefined;
      return;
    }
    const scope = activeModelScope(state.health) ?? ctx.statusScope;
    const snapshot = state.engine;
    const switching = Boolean(operation?.active);
    const key = JSON.stringify([scope, snapshot?.model_profile, snapshot?.variant, snapshot?.engine,
      operation?.id, switching]);
    if (!force && profileRequest?.key === key && !profileRequest.retry) return;
    if (profileRequest?.scope !== scope || (profileRequest?.switching && !switching)) profileSelection = undefined;
    const pending = { scope, key, switching };
    profileRequest = pending;
    const profile = await fetchProfileState();
    if (!root.isConnected || profileRequest !== pending) return;
    const { active, activeKnown } = readProfileState(profile);
    if (!activeKnown && state.health?.capabilities?.includes("model.catalog.v2")) {
      pending.retry = true;
      return;
    }
    if (activeKnown || profileSelection === undefined) profileSelection = active;
    update();
  }
  function update() {
    statsView.update();
    statusErrorBanner.innerHTML = ctx.statusError
      ? `${i18nMessages.m0bb307125afa6949}${h(ctx.statusError)}</p>`
      : "";
    const state = ctx.desktop || {},
      s = ctx.status?.summary || {},
      health = state.health || {},
      sync = s.sync_live || {},
      op = runtimeDownloadOperation() || s.model_operation || {},
      session = s.session || {};
    const nowTask = state.task || {};
    if (promptsReady === null && state.connected)
      ctx
        .api("inject-targets")
        .then((r) => {
          promptsReady = (r.targets || []).some(
            (t) => t.state === "fresh" || !!t.plugin?.source,
          );
          if (root.isConnected) update();
        })
        .catch(() => {
          promptsReady = false;
        });
    loadProfileSelection(state, s.model_operation);
    const operationProgress = op.active ? modelOperationProgress(op) : "";
    const modelStatus = overviewModelStatus(state, s, profileSelection, op, operationProgress);
    updateConnectionIndicator(connection, state.connected);
    metricsRoot.innerHTML = `${metric(i18nMessages.m1dcb520e9408dad5, number(s.local_alive), `${i18nMessages.mdf1c26055f9f39f5}${number(s.local_total)}${i18nMessages.m32aca0663f5c881e}`)}${metric(i18nMessages.me25115d1d25e3421, number(sync.remote_alive), s.remote_configured ? i18nMessages.m2616c6d1ebc7614a : i18nMessages.m9183a87f3a6c526e)}${metric(i18nMessages.me09cbaccf1dd8bf8, number(sync.pending), `${number(sync.conflicts)}${i18nMessages.m0cc6b0935f0e15b9}`)}${modelStatus.metric}`;
    modelRoot.innerHTML = modelStatus.details;
    syncRoot.innerHTML =
      pairs([
        [i18nMessages.m6320b4a8722a851f, phase(sync.phase)],
        [
          i18nMessages.m6e9adb42a28942eb,
          s.autosync === undefined ? "—" : s.autosync ? i18nMessages.m8a4ef3e48e4e8a5a : i18nMessages.m6744b4c6a9aa0038,
        ],
        [
          i18nMessages.mf2be833267a9befd,
          `${number(sync.pulled)} / ${number(sync.pushed)}`,
        ],
        [
          i18nMessages.m81e60238bae4e54b,
          s.sync_scheduler?.last_success_unix
            ? date(s.sync_scheduler.last_success_unix * 1000)
            : i18nMessages.m25280b57b647e36e,
        ],
        [i18nMessages.m061246bd31e084a7, session.user || i18nMessages.m54392fa24b359913],
        [i18nMessages.m02739d1bae8bd996, s.server_addr],
      ]) +
        `<div class="panel-body"><button data-command="sync" ${isPending("sync") ? "disabled" : ""}>${isPending("sync") ? i18nMessages.m4e2238b74ec75987 : i18nMessages.m36e7a43a1c674e50}</button>${sync.error ? `<p class="form-error section-gap">${h(sync.error)}</p>` : ""}</div>`;
    runtimeStatus.innerHTML =
      pairs([
        [i18nMessages.ma5574109f0208e89, state.connected ? i18nMessages.m5be0323e8adcaeae : i18nMessages.m3d52574ce1500561],
        [i18nMessages.m5f76b2bf82dd2c5d, health.bin || "—"],
        [
          i18nMessages.mf938903ab4abdd10,
          state.managed
            ? i18nMessages.m878870aae77f58b3
            : state.connected
              ? i18nMessages.m2bdc6bc9fc8c17c8
              : i18nMessages.m3d52574ce1500561,
        ],
        [
          i18nMessages.m9ed69551e83de61d,
          `${number(health.rpc?.executing_until_reply)} / ${number(health.rpc?.queued_or_cancelled_pending_removal)}`,
        ],
        [i18nMessages.m8a06a866ced279eb, nowTask.stage || "—"],
      ]) +
        `${nowTask.busy && nowTask.total ? `<div class="model-progress"><div class="progress-label"><span>${h(nowTask.stage)}</span><span>${h(bytes(nowTask.done))} / ${h(bytes(nowTask.total))}</span></div><progress value="${h(nowTask.done)}" max="${h(nowTask.total)}"></progress></div>` : ""}`;
    startButton.disabled = hostPending || Boolean(nowTask.busy);
    stopButton.disabled = hostPending || Boolean(nowTask.busy || !state.connected);
    runtimeError.innerHTML = nowTask.error
      ? `<div class="panel-body form-error">${h(nowTask.error)}</div>`
      : "";
    // First-run checklist: each step clears by real state, and the whole card
    // disappears once everything is done.
    const steps = [
      {
        done: state.connected,
        title: i18nMessages.m3137e284699880bc,
        desc: nowTask.busy
          ? nowTask.stage
          : state.connected
            ? i18nMessages.m789937e986026902
            : i18nMessages.mf6cf07a86e67576f,
        action: nowTask.busy
          ? null
          : { go: "updates", label: i18nMessages.mc92129b3f00e0509 },
      },
      {
        done: Boolean(session.user),
        title: i18nMessages.mfe44d969c6fb8fcf,
        desc: i18nMessages.mc7468ea210c7e511,
        action: { go: "accounts", label: i18nMessages.m6eb2f39b4387f88e },
      },
      {
        done: Boolean(session.has_local_keys),
        title: i18nMessages.m1cca9b4c3c9cd3dd,
        desc: i18nMessages.mcca07358949e550b,
        action: { go: "accounts", label: i18nMessages.m6eb2f39b4387f88e },
      },
      {
        done: Boolean(promptsReady),
        title: i18nMessages.m519aa329bdfb20db,
        desc: i18nMessages.mc216849aad09b2b0,
        action: { go: "injection", label: i18nMessages.m7bbfa3e2e75a6daf },
      },
    ];
    onboarding.innerHTML = !steps.every((step) => step.done)
      ? `${i18nMessages.mfa38e689e6c7e1ca}${steps
          .map(
            (step) =>
              `<div class="row spread"><div class="row-copy"><strong><span class="dot ${
                step.done ? "online" : ""
              }"></span>${h(step.title)}</strong><small>${h(step.desc)}</small></div>${
                step.done
                  ? i18nMessages.mf27aac25467851e3
                  : step.action
                    ? `<button data-go="${h(step.action.go)}">${h(step.action.label)}</button>`
                    : ""
              }</div>`,
          )
          .join("")}</div></div>`
      : "";
  }
  root.addEventListener("click", (event) => {
    const button = event.target.closest("button");
    if (!button) return;
    if (button.dataset.go) ctx.navigate(button.dataset.go);
    if (button.dataset.command)
      action(button, async () => {
        await execute(button.dataset.command);
        notify(i18nMessages.m678b02618ee73838);
        await ctx.refresh();
      });
    if (button.dataset.host)
      action(button, async () => {
        hostPending = true;
        update();
        try {
          await ctx.api(button.dataset.host, {});
          await ctx.refresh();
        } finally {
          hostPending = false;
        }
      }).finally(() => {
        if (root.isConnected) update();
      });
  });
  update();
  return {
    update,
    refresh() {
      return Promise.all([
        statsView.refresh(),
        loadProfileSelection(ctx.desktop || {}, ctx.status?.summary?.model_operation, true),
      ]);
    },
  };
}
