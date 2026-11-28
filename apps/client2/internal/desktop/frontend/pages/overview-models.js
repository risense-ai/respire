import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { t } from "../ui/i18n.js";
import { h, number, panel } from "../ui/core.js";
import { activeVariantSelection, CURATED_MODELS, MODEL_CARDS } from "./model-kit.js";
import { modelOperationProgress, modelTaskBusy } from "./models-view.js";
import { statusProgress } from "./models-status.js";

function servingModel(active, snapshot) {
  const legacy = snapshot.model_profile
    ? { model: snapshot.model_profile, variant: snapshot.variant ?? snapshot.model, engine: snapshot.engine }
    : null;
  const selection = activeVariantSelection(active ?? legacy, active === null ? {
    model: snapshot.variant ?? snapshot.model, engine: snapshot.engine,
  } : undefined);
  if (!selection) return null;
  const metadata = CURATED_MODELS.find(({ id }) => id === selection.model)
    ?? MODEL_CARDS.find(({ id }) => id === selection.model);
  return { ...metadata, ...selection,
    label: `${metadata?.title ?? selection.model} \u00b7 ${selection.variant}`,
  };
}

function laneCount(lane, legacyActive) {
  const active = lane?.active ?? legacyActive;
  const count = number(typeof active === "boolean" ? Number(active) : active);
  return active != null && lane?.capacity != null ? `${count} / ${number(lane.capacity)}` : count;
}

function averageLatency(timings) {
  const samples = (Array.isArray(timings) ? timings : [])
    .map((timing) => (Array.isArray(timing) ? timing[1] : NaN))
    .filter(Number.isFinite);
  if (!samples.length) return "\u2014";
  const mean = samples.reduce((total, sample) => total + sample, 0) / samples.length / 1000;
  return `${number(Math.round(mean * 100) / 100)} ms`;
}

export function overviewModelsPanel() {
  return panel(t(i18nMessages.mc243416c93c07db7),
    '<div class="overview-model-content" data-overview-model-content></div>',
    `<button class="link-btn" type="button" data-go="models">${h(t(i18nMessages.overviewModelsSettings || "Model Settings \u2192"))}</button>`);
}

export function overviewModelStatus(state, summary, active, op, operationProgress = "") {
  const health = state.health || {};
  const index = summary.retrieval_index ?? health.retrieval_index ?? {};
  const inference = health.inference || {};
  const model = state.connected ? servingModel(active, state.engine || {}) : null;
  const label = model?.label ?? "\u2014";
  const coverageTitle = t(i18nMessages.overviewIndexCoverage || "Index Coverage");
  const indexTitle = t(i18nMessages.overviewIndexStatus || "Index Status");
  const counts = `${number(summary.indexed_count)} / ${number(summary.local_alive)}`;
  const percent = summary.indexed_count == null || summary.local_alive == null ? null :
    summary.local_alive === 0 ? 0 : Math.round(100 * summary.indexed_count / summary.local_alive);
  const coverage = `${counts} (${percent == null ? "\u2014" : `${percent}%`})`;
  const failed = Boolean(index.error) || ["failed", "error"].includes(index.state);
  const paused = index.state === "paused";
  const shadow = (!paused && (index.running || index.scheduled || ["running", "scheduled"].includes(index.state))) ||
    (op.active && ["switch-index", "switch-diary", "index", "running"].includes(op.phase));
  const ready = state.connected && (index.state === "ready" ||
    ((!index.state || index.state === "idle") && percent != null));
  const status = failed ? t(i18nMessages.m0bc1fb72ae1be5c5) :
    shadow ? t(i18nMessages.overviewShadowIndexing || "Shadow Indexing") : paused ? t(i18nMessages.modelPaused || "Paused") :
      ready ? t(i18nMessages.overviewIndexReady || "Ready") : t(i18nMessages.m460b3574e4bdb8ca);
  const statusTone = failed ? "danger-text" : shadow ? "warning-text" : ready ? "good" : "";
  const metric = `<div class="metric index-kpi"><span class="row spread">${h(indexTitle)}<span class="badge ${statusTone}">${h(status)}</span></span>
    <strong>${h(coverage)}</strong><progress class="index-kpi-bar" aria-label="${h(coverageTitle)}" max="100" ${percent == null ? "" : `value="${percent}"`}></progress>
    <small>${h(label)}</small>${index.error ? `<small class="form-error">${h(index.error)}</small>` : ""}</div>`;

  const task = state.task || {};
  const taskOp = modelTaskBusy(task) ? { phase: "download", item: task.stage } : { phase: task.stage };
  const progress = operationProgress || (task.busy && modelTaskBusy(task) ?
    modelOperationProgress({ ...taskOp, done: task.done, total: task.total }) : shadow && !failed ?
      statusProgress(t(i18nMessages.modelBuildingShadow || "Building Shadow Index"), summary.indexed_count, summary.local_alive) : "");
  const activity = (inference.foreground?.active ?? inference.active)
    ? t(i18nMessages.modelForegroundActive || "Foreground Active") : t(i18nMessages.modelOnlineIdle || "Online Idle");
  const steady = `<p class="hint"><span class="${statusTone}">${h(status)}</span>${model && ready && !failed ? ` \u00b7 <span>${h(activity)}</span>` : ""}</p>`;
  const telemetry = [
    [t(i18nMessages.modelForegroundLane || "Foreground"), laneCount(inference.foreground, inference.active)],
    [t(i18nMessages.modelBackgroundLane || "Background"), laneCount(inference.background)],
    [coverageTitle, coverage],
  ];
  const details = `<div class="panel-body">
    <div class="overview-serving-model">
      <small>${h(t(i18nMessages.overviewActiveSummary || "Serving Model"))}</small>
      <div class="row spread"><strong>${h(label)}</strong>${model?.engine ? `<span class="badge">${h(model.engine.toUpperCase())}</span>` : ""}</div>
      ${model?.dims ? `<small>${h(number(model.dims))} <span>${h(t(i18nMessages.modelDims || "dims"))}</span> \u00b7 ${h(number(model.context))} <span>${h(t(i18nMessages.modelContext || "token context"))}</span></small>` : ""}
      <small title="${h(t(i18nMessages.modelLatencyAvgTitle || "Average RPC execution time"))}"><span>${h(t(i18nMessages.modelLatencyAvg || "Avg Latency"))}</span> <strong>${h(averageLatency(health.rpc?.recent_timing_micros))}</strong></small>
    </div>
    <dl class="overview-model-telemetry">${telemetry.map(([title, value]) =>
      `<div><dt>${h(title)}</dt><dd>${h(value)}</dd></div>`).join("")}</dl>
  </div><div class="model-progress overview-model-strip">${progress || steady}</div>`;
  return { metric, details };
}
