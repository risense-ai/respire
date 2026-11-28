import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { h, bytes, number, currentRuntimeSwitch } from "../ui/core.js";
import { phase } from "./overview.js";
import { readProfileState, activeDownloadSession } from "./model-kit.js";

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
      item: progress.message, progress_percent: progress.progress_percent };
  }
}

export function liveModelOperation(view, status) {
  const download = runtimeDownloadOperation();
  if (download) return download;
  const profile = readProfileState(view.profileState).operation;
  const snapshot = status?.summary?.model_operation;
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

export function downloadTaskProgress(task) {
  const total = task.total > 0;
  const count = total ? `${bytes(task.done)} / ${bytes(task.total)}` : task.done > 0 ? bytes(task.done) : "";
  return `<div class="progress-label"><span>${h(task.stage)}</span><span>${h(count)}</span></div><progress ${total ? `value="${h(task.done)}" max="${h(task.total)}"` : ""}></progress>`;
}

export function indexTaskProgress(state, status) {
  const count = `${number(status.indexed_count)} / ${number(status.local_alive)}`;
  const measured = typeof status.indexed_count === "number" && status.local_alive > 0;
  return `<div class="progress-label"><span>${h(state)}</span><span>${h(count)}</span></div><progress ${measured ? `value="${h(status.indexed_count)}" max="${h(status.local_alive)}"` : ""}></progress>`;
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
