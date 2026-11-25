import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { $, all, h, api } from "../ui/core.js";
import {
  agentResultMessage,
  agentStatus,
  targetsForScope,
} from "../ui/agent-install.js";
import { wizardImportStep } from "../pages/session-import.js";

// Two mutually exclusive install modes; one apply only ever runs the chosen
// mode, and existing prompts/plugins on the machine are always kept.
const AGENT_SCOPES = [
  {
    id: "prompt",
    title: i18nMessages.me52f6f9c2e738a24,
    detail: i18nMessages.m2a38a5459a96aa60,
    badge: i18nMessages.m1452deafc6d96546,
  },
  {
    id: "plugin",
    title: i18nMessages.m7a763afa15c4dc03,
    detail: i18nMessages.mb395b0e5ab809ef3,
  },
];

export function agentSteps(wizard, {
  goto,
  connected,
  frame,
  root,
  render,
  backButton,
  ctx,
  pause,
  complete,
}) {
  // ------------------------------------------------------- agent tools step

  function defaultAgentSelection() {
    const selected = new Set();
    // First run offers only targets this machine actually detected, for the
    // currently chosen mode.
    for (const target of targetsForScope(wizard.state.agentTargets, wizard.state.agentScope, {
      detectedOnly: true,
    }))
      selected.add(target.id);
    return selected;
  }

  async function renderAgents(body) {
    // The agents step now advances to the optional import step; completing
    // without either stays available through the import step's Finish button.
    const finishButton = `${i18nMessages.m6b1518d8f38f215f}`;
    const wireFinish = () =>
      $("[data-finish]", body).addEventListener("click", () => goto("import"));
    if (!connected()) {
      body.innerHTML = frame(
        i18nMessages.m5053d66690eedc1a,
        i18nMessages.m5994bb4654df4b4f,
        i18nMessages.m5b2c03c868d9d1f7,
        finishButton,
      );
      wireFinish();
      return;
    }
    if (!wizard.state.agentTargets) {
      body.innerHTML = frame(
        i18nMessages.m5053d66690eedc1a,
        i18nMessages.m5994bb4654df4b4f,
        i18nMessages.m8cefe3932e4191fe,
        finishButton,
      );
      wireFinish();
      try {
        const result = await api("inject-targets");
        wizard.state.agentTargets = result.targets || [];
      } catch (error) {
        wizard.state.agentTargets = [];
        wizard.state.agentResults = [
          { name: i18nMessages.mf1ab23c69724b516, ok: false, message: error.message },
        ];
      }
      if (root.isConnected && wizard.state.step === "agents") render();
      return;
    }
    const eligible = targetsForScope(wizard.state.agentTargets, wizard.state.agentScope, {
      detectedOnly: true,
    });
    if (!wizard.state.agentSelected) wizard.state.agentSelected = defaultAgentSelection();
    const results = wizard.state.agentResults || [];
    body.innerHTML = frame(
      i18nMessages.m5053d66690eedc1a,
      i18nMessages.m73d46415c4dc3a06,
      `<div class="wizard-choices">${AGENT_SCOPES.map(
        (scope) =>
          `<button type="button" class="wizard-choice${
            wizard.state.agentScope === scope.id ? " active" : ""
          }" data-agent-scope="${h(scope.id)}"${
            wizard.state.agentBusy ? " disabled" : ""
          }><span class="wizard-choice-copy"><strong>${h(scope.title)}${
            scope.badge ? `<span class="wizard-badge">${h(scope.badge)}</span>` : ""
          }</strong><small>${h(scope.detail)}</small></span></button>`,
      ).join("")}</div><div class="stack" data-agent-list>${
        eligible.length
          ? eligible
              .map(
                (target) =>
                  `<label class="wizard-agent-row"><input type="checkbox" data-agent-target="${h(
                    target.id,
                  )}"${
                    wizard.state.agentSelected.has(target.id) ? " checked" : ""
                  }${wizard.state.agentBusy ? " disabled" : ""}><span class="wizard-agent-copy"><strong>${h(
                    target.name,
                  )}</strong><small>${h(
                    agentStatus(target, wizard.state.agentScope),
                  )}</small></span></label>`,
              )
              .join("")
          : i18nMessages.mc253c080741dbb85
      }</div>${
        results.length
          ? `<div class="wizard-agent-results">${results
              .map(
                (item) =>
                  `<div class="row"><span class="dot ${
                    item.ok ? "online" : ""
                  }"></span><strong>${h(item.name)}</strong><small>${h(
                    item.message,
                  )}</small></div>`,
              )
              .join("")}</div>`
          : ""
      }`,
      (wizard.state.agentBusy
        ? ""
        : `<button type="button" data-agent-apply${
            wizard.state.agentSelected.size ? "" : " disabled"
          }${i18nMessages.m3c20e3129e9c42a1}`) +
        finishButton,
    );
    all("[data-agent-scope]", body).forEach((button) =>
      button.addEventListener("click", () => {
        if (wizard.state.agentBusy || wizard.state.agentScope === button.dataset.agentScope)
          return;
        wizard.state.agentScope = button.dataset.agentScope;
        wizard.state.agentSelected = defaultAgentSelection();
        wizard.state.agentResults = null;
        render();
      }),
    );
    all("[data-agent-target]", body).forEach((input) =>
      input.addEventListener("change", () => {
        if (input.checked) wizard.state.agentSelected.add(input.dataset.agentTarget);
        else wizard.state.agentSelected.delete(input.dataset.agentTarget);
        const apply = $("[data-agent-apply]", root);
        if (apply) apply.disabled = !wizard.state.agentSelected.size;
      }),
    );
    const apply = $("[data-agent-apply]", body);
    if (apply)
      apply.addEventListener("click", () => applyAgents());
    wireFinish();
  }

  async function applyAgents() {
    if (wizard.state.agentBusy || !wizard.state.agentSelected.size) return;
    wizard.state.agentBusy = true;
    wizard.state.agentResults = [];
    render();
    for (const id of [...wizard.state.agentSelected]) {
      const target = (wizard.state.agentTargets || []).find((item) => item.id === id);
      const name = target?.name || id;
      try {
        const result = await api("inject-set", {
          id,
          enable: true,
          scope: wizard.state.agentScope,
        });
        wizard.state.agentResults.push({
          name,
          ok: true,
          message: agentResultMessage(result),
        });
      } catch (error) {
        wizard.state.agentResults.push({ name, ok: false, message: error.message });
      }
      const area = $("[data-agent-results]", root);
      if (area)
        area.innerHTML = wizard.state.agentResults
          .map(
            (item) =>
              `<div class="row"><span class="dot ${
                item.ok ? "online" : ""
              }"></span><strong>${h(item.name)}</strong><small>${h(
                item.message,
              )}</small></div>`,
          )
          .join("");
    }
    wizard.state.agentBusy = false;
    try {
      const result = await api("inject-targets");
      wizard.state.agentTargets = result.targets || [];
    } catch {}
    wizard.state.agentSelected = defaultAgentSelection();
    render();
  }

  // Optional last step: the session-import flow mounted inside the wizard.
  // Completing or skipping never starts a write; an in-flight import keeps
  // running like any host operation and stays visible in Operations.
  function renderImport(body) {
    body.innerHTML = frame(
      i18nMessages.mbc3a954af6871f0e,
      i18nMessages.m5b5d3b7bfc6eed6c,
      '<div data-import-mount></div>',
      backButton("agents") +
        `${i18nMessages.mbf8804a669b87b74}`,
    );
    wizardImportStep($("[data-import-mount]", body), {
      ...ctx,
      get desktop() {
        return ctx.desktop;
      },
      // The mounted flow's page links must pause the wizard before routing,
      // like the updates-page escape on the account step.
      navigate: (page) => {
        pause();
        ctx.navigate(page);
      },
    });
    $("[data-finish-import]", body).addEventListener("click", () => complete());
  }
  return { renderAgents, renderImport };
}
