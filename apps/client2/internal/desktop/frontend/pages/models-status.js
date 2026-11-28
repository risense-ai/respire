import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { bytes, h, number } from "../ui/core.js";
import { CURATED_MODELS, MODEL_CARDS, variantFilename } from "./model-kit.js";

export function servingModel(active, inventory) {
  const model = CURATED_MODELS.find(({ id }) => id === active?.model) ||
    MODEL_CARDS.find(({ id }) => id === active?.model);
  const variant = inventory?.variants?.find(({ name }) => name === active?.variant);
  return { title: model?.title || active?.model, dims: model?.dims, context: model?.context,
    filename: variantFilename(variant || { name: active?.variant }), size: variant?.size,
    installed: variant && Boolean(variant.installed && inventory.tokenizer_installed) };
}

export function rpcLatency(timings) {
  const samples = (Array.isArray(timings) ? timings : []).map((timing) => {
    if (!Array.isArray(timing)) return timing;
    const [queued, execution] = timing;
    return Number.isFinite(queued) && Number.isFinite(execution) ? queued + execution : NaN;
  }).filter((value) => Number.isFinite(value) && value >= 0).map((value) => value / 1000);
  const latest = samples.at(-1);
  const average = samples.length ? samples.reduce((sum, value) => sum + value, 0) / samples.length : undefined;
  const scale = Math.max(1, ...samples);
  const y = (value) => (23 - value / scale * 22).toFixed(2);
  const points = samples.length > 1
    ? samples.map((value, index) => `${(1 + index * 98 / (samples.length - 1)).toFixed(2)},${y(value)}`).join(" ")
    : `1,${y(latest ?? 0)} 99,${y(latest ?? 0)}`;
  return { latest, average, points };
}

export function statusCells(entries) {
  return entries.map(([label, value]) => `<div><dt>${h(label)}</dt><dd>${h(value)}</dd></div>`).join("");
}

export function statusProgress(label, done, total, { percent, format = number } = {}) {
  const measured = Number.isFinite(done) && total > 0;
  const ratio = measured ? done / total * 100 : percent;
  const bounded = Number.isFinite(ratio) ? Math.round(Math.max(0, Math.min(100, ratio))) : null;
  const count = Number.isFinite(done) || Number.isFinite(total) ? `${format(done)} / ${format(total)}` : "";
  const display = count && bounded !== null ? `${count} (${bounded}%)` : count || (bounded === null ? "" : `${bounded}%`);
  const bar = measured ? `value="${h(done)}" max="${h(total)}"` : bounded === null ? "" : `value="${bounded}" max="100"`;
  return `<div class="progress-label"><span>${h(label)}</span><span>${h(display)}</span></div>
    <progress aria-label="${h(label)}" ${bar}></progress>`;
}

export function renderLiveInference({ active, model, health, status, shadow, buildingShadow, servingIndex }) {
  const inference = health?.inference || {};
  const foreground = inference.foreground;
  const background = inference.background;
  const activeCount = (value) => typeof value === "boolean" ? Number(value) : value;
  const foregroundActive = activeCount(foreground?.active ?? inference.active);
  const latency = rpcLatency(health?.rpc?.recent_timing_micros);
  const lane = (active, capacity) => `${number(active)} / ${number(capacity)}`;
  const ms = (value) => value === undefined ? "—" : `${value.toFixed(1)} ms`;
  const activity = buildingShadow ? i18nMessages.modelBuildingShadow : foregroundActive > 0 ? i18nMessages.modelForegroundActive
    : active ? i18nMessages.modelOnlineIdle : "—";
  return `<section class="panel model-status-card" data-models-live-card>
    <header class="panel-heading"><h2>${h(i18nMessages.modelLiveInference)}</h2></header>
    <div class="model-status-body">
      <div class="model-status-identity"><strong>${h(model.title || "—")}</strong>
        ${active?.variant ? `<span class="model-badge">${h(active.variant)}</span>` : ""}
        ${active?.engine ? `<span class="model-badge">${h(active.engine.toUpperCase())}</span>` : ""}
        <span class="model-badge ${active ? "current" : ""}"><span>${h(activity)}</span>${!buildingShadow && foregroundActive > 0 ? ` (${number(foregroundActive)})` : ""}</span>
      </div>
      ${model.dims ? `<p>${number(model.dims)} <span>${h(i18nMessages.modelDims)}</span> · ${number(model.context)} <span>${h(i18nMessages.modelContext)}</span></p>` : ""}
      ${model.filename ? `<p><code>${h(model.filename)}</code> · ${h(bytes(model.size))}</p>` : ""}
      <dl class="model-status-cells">${statusCells([
        [i18nMessages.modelForegroundLane, lane(foregroundActive, foreground?.capacity)],
        [i18nMessages.modelBackgroundLane, lane(activeCount(background?.active), background?.capacity)],
        [i18nMessages.modelQueueLane, number(inference.queued)],
        [i18nMessages.modelWorkerPid, number(inference.worker_pid)],
        [i18nMessages.modelLatencyLatest, ms(latency.latest)],
        [i18nMessages.modelLatencyAvg, ms(latency.average)],
      ])}</dl>
      <svg class="model-latency-sparkline" viewBox="0 0 100 24" preserveAspectRatio="none" aria-hidden="true">
        <polyline points="${latency.points}" vector-effect="non-scaling-stroke"/>
      </svg>
    </div>
    <footer class="model-status-footer">
      ${statusProgress(i18nMessages.modelLocalIndex, status.indexed_count, status.local_alive,
        { percent: status.indexed_count === 0 && status.local_alive === 0 ? 0 : undefined })}
      ${shadow && servingIndex ? `<p>${h(i18nMessages.modelIncumbentReady)}</p>` : ""}
      ${buildingShadow ? statusProgress(i18nMessages.modelPipelineShadow, shadow.done, shadow.total) : ""}
    </footer>
  </section>`;
}
