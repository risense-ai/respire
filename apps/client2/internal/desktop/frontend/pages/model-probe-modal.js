import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { $, action, api, h, notify, raw } from "../ui/core.js";

export function openModelProbeModal(selection, { platform, version, health, installed }, weightsPath) {
  const dialog = document.createElement("dialog");
  dialog.className = "dialog model-probe-dialog";
  dialog.setAttribute("aria-labelledby", "model-probe-title");
  dialog.innerHTML = `<div class="dialog-body stack">
    <h2 id="model-probe-title">${h(i18nMessages.modelProbe)}</h2>
    <strong data-probe-status class="model-probe-status" role="status"></strong>
    <pre data-probe-log tabindex="0"></pre>
    <div class="actions">
      <button type="button" data-probe-copy disabled>${h(i18nMessages.modelProbeCopy)}</button>
      <a data-probe-issue target="_blank" rel="noopener noreferrer" hidden>${h(i18nMessages.modelProbeIssue)}</a>
      <button type="button" data-probe-retry disabled>${h(i18nMessages.modelProbeRetry)}</button>
      <button type="button" data-probe-close>${h(i18nMessages.modelProbeClose)}</button>
    </div>
  </div>`;
  const status = $("[data-probe-status]", dialog);
  const log = $("[data-probe-log]", dialog);
  const copy = $("[data-probe-copy]", dialog);
  const issue = $("[data-probe-issue]", dialog);
  const retry = $("[data-probe-retry]", dialog);
  const [os, arch] = (platform || "").split("-");
  const environment = {
    ...selection,
    os, arch,
    client_version: version,
    runtime_version: health?.version || installed?.version,
    weights_path: weightsPath,
  };

  async function run() {
    retry.disabled = copy.disabled = issue.hidden = true;
    status.dataset.result = "running";
    status.textContent = i18nMessages.modelProbeRunning;
    log.textContent = raw(environment);
    let passed = false;
    let report;
    try {
      const result = await api("model-probe", selection);
      passed = result.ok === true;
      report = {
        ...environment, ...result.diagnostics,
        os: result.os || environment.os,
        arch: result.arch || environment.arch,
        client_version: result.client_version || environment.client_version,
        runtime: result.runtime,
      };
    } catch (error) {
      report = { ...environment, error: error.message, stack: error.stack };
    }
    if (!dialog.open) return;
    status.dataset.result = passed ? "pass" : "fail";
    status.textContent = passed ? i18nMessages.modelProbePass : i18nMessages.modelProbeFail;
    const { model, variant, engine, os, arch, client_version, runtime_version, execution_provider } = report;
    const issueFields = { model, variant, engine, os, arch, client_version, runtime_version, execution_provider };
    const query = new URLSearchParams({
      title: `Model probe ${passed ? "passed" : "failed"}: ${model} / ${variant} / ${engine}`,
      body: `### Model probe\n\n\`\`\`json\n${raw(issueFields)}\n\`\`\`\n\n### What happened?\n\n\n### Diagnostic log\n\nPaste the copied log here after reviewing it for local paths and other private details.`,
    });
    log.textContent = raw(report);
    issue.href = `https://github.com/risense-ai/respire/issues/new?${query}`;
    retry.disabled = copy.disabled = issue.hidden = false;
  }

  copy.addEventListener("click", () => action(copy, async () => {
    await navigator.clipboard.writeText(log.textContent);
    notify(i18nMessages.modelProbeCopied);
  }));
  retry.addEventListener("click", run);
  $("[data-probe-close]", dialog).addEventListener("click", () => dialog.close());
  dialog.addEventListener("close", () => dialog.remove());
  document.body.append(dialog);
  dialog.showModal();
  void run();
}
