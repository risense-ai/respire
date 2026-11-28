import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { h, bytes, number, currentRuntimeSwitch } from "../ui/core.js";
import { phase } from "./overview.js";
import { readProfileState, activeDownloadSession, variantFilename } from "./model-kit.js";
import { statusCells, statusProgress } from "./models-status.js";

const SWITCH_PHASES = {
  queued: i18nMessages.md6f766f2adf53227,
  "switch-verify": i18nMessages.mab207f8bb66005ca,
  "switch-download": i18nMessages.m621982e6d8de510a,
  "switch-load": i18nMessages.m836ebd4436a27d05,
  "switch-index": i18nMessages.m3e4ab1850f35cc47,
  "switch-diary": i18nMessages.m3f110122999a31c4,
  index: i18nMessages.m3e4ab1850f35cc47,
  "index-load": i18nMessages.m836ebd4436a27d05,
};

export function runtimeDownloadOperation() {
  const transfer = activeDownloadSession();
  const progress = transfer?.progress;
  if (progress && ["pending", "running"].includes(progress.state)) {
    return { id: transfer.id, active: true, phase: "download", cancellable: false,
      downloadModel: transfer.model, downloadVariant: transfer.variant,
      item: progress.message, done: progress.done, total: progress.total,
      progress_percent: progress.progress_percent };
  }
}

export function liveModelOperation(view, status) {
  const download = runtimeDownloadOperation();
  const snapshot = status?.summary?.model_operation;
  if (download) {
    if (download.id && snapshot?.id === download.id && snapshot.active && snapshot.phase === "download") {
      return { ...download, done: snapshot.done ?? download.done, total: snapshot.total ?? download.total,
        item: snapshot.item || download.item };
    }
    return download;
  }
  const profile = readProfileState(view.profileState).operation;
  const observed = currentRuntimeSwitch();
  const runtime = observed ? {
    ...observed.progress,
    operation_id: observed.receipt.operation_id,
    item: observed.progress?.item || observed.model,
    state: observed.progress?.state || "pending",
  } : view.runtimeProgress;
  const runtimeID = runtime?.operation_id || runtime?.id;
  const runtimeActive = runtime && (runtime.active ?? (runtime.state
    ? ["pending", "running"].includes(runtime.state) : view.switchInFlight));
  const currentSnapshot = snapshot?.active && snapshot !== view.profileStatus ? snapshot : null;
  const id = [currentSnapshot?.id, profile.active && profile.id, runtimeID]
    .find((value) => typeof value === "string" && value.trim());
  const sameTask = (op) => op && (!op.id || op.id === id);
  const activeProfile = profile.active && sameTask(profile) ? profile : null;
  if (id && runtimeID === id && !runtimeActive && runtime?.state) return {};
  if (!activeProfile && !sameTask(currentSnapshot) && !(runtimeActive && runtimeID === id)) return {};
  const runtimeFields = runtimeID === id && runtimeActive ? runtime : null;
  const op = { ...runtimeFields, ...(sameTask(currentSnapshot) ? currentSnapshot : null), ...activeProfile };
  const runtimePhase = runtimeFields?.message || runtimeFields?.state;
  if (runtimeFields?.state && !activeProfile && !runtimeFields.phase && (!op.phase || op.phase === runtimePhase)) {
    op.phase = runtimeFields.state === "pending" ? "queued" : runtimePhase;
    op.done = runtimeFields.done;
    op.total = runtimeFields.total;
  }
  return { ...op, id, active: true };
}

export const cancellableModelID = (op) => op.active && op.cancellable !== false ? op.id : undefined;
const modelPhase = (op) => SWITCH_PHASES[op.phase] || phase(op.phase);

export function modelOperationProgress(op) {
  const total = op.total > 0 && op.done != null;
  const format = op.phase === "download" || (op.phase === "verify" && total) ? bytes : number;
  const percent = total ? `${Math.round(op.done / op.total * 100)}%` : "";
  const count = total ? `${format(op.done)} / ${format(op.total)} (${percent})`
    : op.done != null ? format(op.done) : op.progress_percent == null ? "" : `${op.progress_percent}%`;
  const elapsed = op.elapsed ?? op.elapsed_secs;
  return `<div class="progress-label"><span>${h(modelPhase(op))}${op.item ? `<span> · ${h(op.item)}</span>` : ""}</span><span>${h(count)}</span><span>${elapsed == null ? "" : h(elapsed + i18nMessages.m00fe8dda1fd70f64)}</span></div><progress ${total ? `value="${h(op.done)}" max="${h(op.total)}"` : op.progress_percent == null ? "" : `value="${h(op.progress_percent)}" max="100"`}></progress>`;
}

const PIPELINE = [i18nMessages.modelPipelineVerify, i18nMessages.modelPipelineProbe,
  i18nMessages.modelPipelineShadow, i18nMessages.modelPipelineSwitch];

export function switchStep(op) {
  if (["switch-diary", "switch-commit", "commit", "switch"].includes(op.phase)) return 3;
  if (["switch-index", "shadow-index"].includes(op.phase)) return 2;
  if (["switch-load", "switch-probe", "probe"].includes(op.phase)) return 1;
  if (SWITCH_PHASES[op.phase] || op.phase === "verify") return 0;
  const percent = op.progress_percent;
  return percent >= 80 ? 3 : percent >= 20 ? 2 : percent >= 15 ? 1 : 0;
}

export function downloadTaskProgress({ task, op, session, variant, mirror }) {
  const label = session?.label || op.downloadModel;
  const reportedFile = /(?:^|[/\\\s])([\w.-]+\.(?:onnx|ort|json)(?:_data)?)$/i.exec(op.item || task?.stage || "")?.[1];
  const filename = reportedFile || variantFilename(variant || { name: session?.variant || op.downloadVariant });
  const target = [label, filename].filter(Boolean).join(" · ") || task?.stage || op.item;
  return `<p><strong>${h(target || i18nMessages.modelDownload)}</strong></p>
    ${statusProgress(i18nMessages.modelDownload, op.done ?? task?.done, op.total ?? task?.total ?? variant?.size,
      { format: bytes, percent: op.progress_percent })}
    <p><span>${h(i18nMessages.modelMirrorOrigin)}</span> · <span>${h(mirror)}</span></p>`;
}

export function modelTaskState({ op, session, task, index, view, status, workerReady, weightsReady, servingIndex, variant, mirror }) {
  if (op.phase === "download" || session || task) {
    return { kind: "download", label: i18nMessages.modelDownload, cancelLabel: i18nMessages.modelCancelDownload,
      markup: downloadTaskProgress({ task, op, session, variant, mirror }) };
  }
  if ((op.active && !/^index(?:-|$)/.test(op.phase || "")) || view.switchInFlight) {
    const step = switchStep(op);
    const pipeline = `<ol class="model-task-pipeline">${PIPELINE.map((label, position) =>
      `<li${step === position ? ' aria-current="step"' : ""}>${position + 1}. <span>${h(label)}</span></li>`).join("")}</ol>`;
    const progress = step === 2 && ["switch-index", "shadow-index"].includes(op.phase)
      ? statusProgress(PIPELINE[2], op.done, op.total) : statusProgress(PIPELINE[step], undefined, undefined, { percent: op.progress_percent });
    return { kind: "switch", step, label: PIPELINE[step], cancelLabel: i18nMessages.modelCancelSwitch,
      markup: `${pipeline}${progress}${servingIndex ? `<p>${h(i18nMessages.modelIncumbentReady)}</p>` : ""}` };
  }
  if (op.active || index.running || index.scheduled || view.indexInFlight) {
    const label = index.scheduled && !index.running ? i18nMessages.m2fcab8f67ceeae3e : i18nMessages.m0a53c8de45a1ed4a;
    return { kind: "rebuild", label, cancelLabel: i18nMessages.modelCancelRebuild,
      markup: statusProgress(label, op.done ?? status.indexed_count, op.total ?? status.local_alive) };
  }
  const readiness = (ready, unavailable) => ready === undefined ? "—" : ready ? i18nMessages.modelReady : unavailable;
  return { kind: "idle", label: i18nMessages.modelOnlineIdle, markup: `<dl class="model-status-cells model-readiness">
    ${statusCells([
      [i18nMessages.modelTileWeights, readiness(weightsReady, i18nMessages.modelNotDownloaded)],
      [i18nMessages.modelTileWorker, readiness(workerReady, i18nMessages.me6fc5eb8c3b83c01)],
      [i18nMessages.modelTileSwitchLock, view.mutating ? i18nMessages.modelLocked : servingIndex ? i18nMessages.modelUnlocked : "—"],
    ])}</dl>` };
}

export function modelActivityTimeline() {
  let previous = "idle:", previousID;
  const entries = [];
  return (task, id) => {
    const key = [task.kind, task.step].join(":");
    if ((key !== previous || (id && previousID && id !== previousID)) && (task.kind !== "idle" || entries.length)) {
      entries.unshift({ when: new Date(), label: task.label });
      entries.length = Math.min(entries.length, 3);
    }
    previous = key;
    previousID = id;
    return entries.length ? `<ol>${entries.map(({ when, label }) => `<li>
      <time datetime="${when.toISOString()}">${h(when.toLocaleTimeString())}</time><span>${h(label)}</span></li>`).join("")}</ol>`
      : `<p>${h(i18nMessages.modelNoRecentActivity)}</p>`;
  };
}

export function nextSwitchStallState(prevKey, prevTicks, op, limit = 600) {
  const key = op.active ? [op.id ?? "", op.phase ?? "", op.done ?? "", op.total ?? "", op.progress_percent ?? ""].join("|") : "";
  const ticks = key && key !== prevKey ? 0 : prevTicks + 1;
  return { key, ticks, expired: ticks > limit };
}

export function syncModelCancel(root, view, op) {
  const button = root.querySelector('[data-model="cancel"]');
  const id = cancellableModelID(op);
  button.hidden = !id;
  button.disabled = !id || id === view.cancelPending;
}

export function modelTaskBusy(task) {
  return Boolean(task?.busy) && /(?:^|[^a-zA-Z0-9_.-])(?:[a-z0-9_.-]+\.(?:onnx|ort)|tokenizer\.json)(?:$|[^a-zA-Z0-9_.-])/i.test(String(task.stage || ""));
}
