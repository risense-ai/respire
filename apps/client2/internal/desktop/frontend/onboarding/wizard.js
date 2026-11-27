import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { $, h, confirm, notify } from "../ui/core.js";

import { accountSteps } from "./wizard-account.js";
import { modelSteps } from "./wizard-model.js";
import { agentSteps } from "./wizard-agents.js";

// First-run onboarding wizard: a resumable step machine over real runtime
// state. Steps advance only on real receipts (auth envelopes, session
// reads, task state, profile state); step markers in preferences are just
// the resume hint. Recovery material lives in memory only and never
// reaches preferences, storage or logs.

const STEP_ORDER = [
  "language",
  "sync",
  "account",
  "recovery",
  "model",
  "device",
  "confirm",
  "execute",
  "agents",
  "import",
];

const STEP_TITLES = {
  language: i18nMessages.m9f6fee1aba17a565,
  sync: i18nMessages.m5f30fb70507cbc2c,
  account: i18nMessages.m061246bd31e084a7,
  recovery: i18nMessages.mbd1b5e718803f832,
  model: i18nMessages.mc98e118e0a43f078,
  device: i18nMessages.m2ba569c9b4c8c68b,
  confirm: i18nMessages.m36f33adaf0942634,
  execute: i18nMessages.mc0b3fbff51ccc40b,
  agents: i18nMessages.m09225fee469e33eb,
  import: i18nMessages.mbc3a954af6871f0e,
};



export function createWizard(ctx) {
  const root = document.getElementById("onboarding");
  const wizard = { state: null, busy: false, executeStarted: false };
  let pill = null;

  function active() {
    return Boolean(wizard.state) && !wizard.state.done && root.hidden === false;
  }

  function connected() {
    return Boolean(ctx.desktop?.connected);
  }

  function session() {
    return ctx.status?.summary?.session || {};
  }

  async function saveStep(step, extra = {}) {
    wizard.state.step = step;
    try {
      await ctx.savePreferences({ onboardingStep: step, ...extra });
    } catch (error) {
      notify(error.message, true);
    }
  }

  async function goto(step, extra = {}) {
    if (wizard.busy) return;
    wizard.state.error = "";
    await saveStep(step, extra);
    render();
  }

  function stepIndex(step) {
    const index = STEP_ORDER.indexOf(step);
    return index < 0 ? STEP_ORDER.length : index;
  }

  // ---------------------------------------------------------------- render

  function render() {
    root.hidden = false;
    root.className = "onboarding";
    // A re-run opened from Settings can be backed out; the first run has no
    // exit by design.
    root.innerHTML = `${i18nMessages.m1deb9e741051c3d1}${wizard.state.reRun ? i18nMessages.m19cc84e67971b1c4 : ""}<ol class="wizard-steps">${STEP_ORDER.map(
      (step, index) => {
        const current = stepIndex(wizard.state.step);
        const cls =
          index < current ? " done" : index === current ? " current" : "";
        return `<li class="wizard-step${cls}" ${index === current ? 'aria-current="step"' : ""}><span class="wizard-step-dot"></span><span class="wizard-step-label">${h(
          STEP_TITLES[step],
        )}</span></li>`;
      },
    ).join("")}</ol><div class="wizard-body"></div></div></div>`;
    const body = $(".wizard-body", root);
    const exit = $("[data-wizard-exit]", root);
    if (exit)
      exit.addEventListener("click", () => {
        if (!wizard.state || wizard.state.agentBusy) return;
        // Backing out of a re-run keeps everything already applied and
        // writes nothing; completion markers are untouched.
        destroy();
      });
    // Back buttons are shared across steps; delegate so every re-render
    // keeps them wired.
    body.addEventListener("click", (event) => {
      const back = event.target.closest("[data-back]");
      if (back && !wizard.busy) goto(back.dataset.back);
      const skip = event.target.closest("[data-skip-wizard]");
      if (skip && !wizard.busy && !wizard.state.agentBusy) skipWizard();
    });
    const render = {
      language: renderLanguage,
      sync: renderSync,
      account: renderAccount,
      recovery: renderRecovery,
      model: renderModel,
      device: renderDevice,
      confirm: renderConfirm,
      execute: renderExecute,
      agents: renderAgents,
      import: renderImport,
    }[wizard.state.step];
    (render || renderSync)(body);
  }

  function frame(title, subtitle, inner, actions = "", { skip = true } = {}) {
    // First run has no exit button, so every step carries an explicit skip
    // that ends the wizard for good; a re-run already exits from the card
    // corner without writing anything.
    const skipButton = wizard.state.reRun
      ? ""
      : `${i18nMessages.m46c2a5ac3ccb371c}`;
    return `<h1>${h(title)}</h1>${subtitle ? `<p class="wizard-sub">${h(subtitle)}</p>` : ""}${inner}<div class="wizard-actions">${skip ? skipButton : ""}${actions}</div><p class="form-error" role="alert" data-wizard-error ${wizard.state.error ? "" : "hidden"}>${h(wizard.state.error || "")}</p>`;
  }

  function card(
    label,
    detail,
    value,
    { disabled = false, badge = "", active = false } = {},
  ) {
    return `<button type="button" class="wizard-choice${active ? " active" : ""}" data-choice="${h(value)}"${disabled ? " disabled" : ""}><span class="wizard-choice-copy"><strong>${h(label)}${badge ? `<span class="wizard-badge">${h(badge)}</span>` : ""}</strong><small>${h(detail)}</small></span></button>`;
  }

  function backButton(step) {
    return `<button type="button" data-back="${h(step)}${i18nMessages.m901142c1dbad76cd}`;
  }

  async function setError(message) {
    wizard.state.error = message || "";
    const node = $("[data-wizard-error]", root);
    if (node) {
      node.textContent = message || "";
      node.hidden = !message;
    }
    if (message) notify(message, true);
  }

  // ----------------------------------------------------------------- steps

  const {
    renderLanguage,
    renderSync,
    renderAccount,
    renderRecovery,
  } = accountSteps(wizard, {
    ctx,
    frame,
    card,
    render,
    goto,
    backButton,
    saveStep,
    connected,
    pause,
    setError,
    root,
    session,
  });

  const { renderModel, renderDevice, renderConfirm, renderExecute } = modelSteps(wizard, {
    frame,
    card,
    backButton,
    render,
    goto,
    complete,
    connected,
    ctx,
    setError,
    root,
    session,
    exitToModels,
  });
  const { renderAgents, renderImport } = agentSteps(wizard, {
    goto,
    connected,
    frame,
    root,
    render,
    backButton,
    ctx,
    pause,
    complete,
  });
  async function exitToModels() {
    await complete();
    ctx.navigate("models");
  }

  // Skip ends the wizard on any first-run step: onboardingCompleted is set,
  // so neither the wizard nor the resume pill ever comes back on its own.
  // Applied settings are kept; an in-flight model task keeps running in the
  // background and stays visible on the models page.
  async function skipWizard() {
    const tail = wizard.state.executing
      ? i18nMessages.mc07b8692e5e4b78a
      : i18nMessages.m2e392b20dfc50393;
    if (
      !(await confirm(
        i18nMessages.me2605ff0f16d6115,
        `${i18nMessages.m9d4bd561ce6b0d0d}${tail}`,
        i18nMessages.mc743a37661393052,
      ))
    )
      return;
    complete();
  }

  async function complete() {
    wizard.state.done = true;
    try {
      await ctx.savePreferences({
        onboardingCompleted: true,
        onboardingStep: "done",
      });
    } catch (error) {
      notify(error.message, true);
    }
    destroy();
  }

  // ------------------------------------------------------- pause & resume

  function pause() {
    root.hidden = true;
    if (!pill) {
      pill = document.createElement("button");
      pill.type = "button";
      pill.className = "wizard-resume-pill";
      pill.textContent = i18nMessages.mebc3707754fd0961;
      pill.addEventListener("click", () => show());
      document.body.append(pill);
    }
    pill.hidden = false;
  }

  function show({ reopen = false } = {}) {
    if (reopen || !wizard.state) start(reopen);
    if (!wizard.state || wizard.state.done) return;
    if (pill) pill.hidden = true;
    // Re-read live state so a resume judges by reality, not by markers.
    resumeFromState();
    render();
  }

  // Runtime-state recovery: trust receipts over step markers.
  function resumeFromState() {
    if (!wizard.state) return;
    const step = wizard.state.step;
    if (step === "account" || step === "recovery") {
      const token = session().has_token;
      if (token && wizard.state.material) wizard.state.step = "recovery";
      else if (token || (step === "recovery" && !wizard.state.material))
        wizard.state.step = "model";
    }
  }

  function start(reopen = false) {
    const prefs = ctx.desktop?.preferences || {};
    const completed = Boolean(prefs.onboardingCompleted);
    wizard.state = {
      step: prefs.onboardingStep || "language",
      syncChoice: prefs.onboardingSyncChoice || "",
      language: "",
      accountMode: "register",
      accountPhase: "auth",
      autoSync: true,
      material: null,
      model: "m3",
      device: "cpu",
      variant: "",
      variantTouched: false,
      stages: {},
      agentScope: "prompt",
      agentTargets: null,
      agentSelected: null,
      agentResults: null,
      agentBusy: false,
      error: "",
      done: false,
      // Re-run (Settings entry or an already-completed install): starts from
      // the top and may be exited without writing anything.
      reRun: Boolean(reopen || completed),
    };
    if (!STEP_ORDER.includes(wizard.state.step)) wizard.state.step = "language";
    // Model/device/weight choices are deliberately not persisted; after a
    // reload the user re-selects them and already-verified files are skipped
    // by the download stage.
    if (wizard.state.step === "confirm" || wizard.state.step === "execute")
      wizard.state.step = "model";
    wizard.executeStarted = false;
    // A first run skips the language card when a language is already known;
    // a re-run always starts from the top so every step is reconfigurable.
    if (!wizard.state.reRun && wizard.state.step === "language" &&
      (prefs.language || localStorage.getItem("respire-language")))
      wizard.state.step = "sync";
  }

  function destroy() {
    root.hidden = true;
    root.innerHTML = "";
    if (pill) {
      pill.remove();
      pill = null;
    }
    wizard.state = null;
    wizard.executeStarted = false;
  }

  return {
    show,
    destroy,
    isActive: active,
    isDone: () => !wizard.state || wizard.state.done,
  };
}
