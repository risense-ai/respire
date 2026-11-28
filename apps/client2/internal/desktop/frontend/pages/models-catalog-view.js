import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { $, bytes, h, number, notify } from "../ui/core.js";
import { CURATED_MODELS, DEFAULT_MODEL, MIRROR_OPTIONS, engineAvailable, marksActiveVariant, variantFilename } from "./model-kit.js";

const engines = ["cpu", "gpu", "npu"];
const backends = {
  windows: { gpu: "DirectML", npu: "ORT" },
  darwin: { gpu: "CoreML", npu: "ANE" },
  linux: { gpu: "CUDA / ROCm", npu: "OpenVINO" },
};

export const curatedModel = (id) => CURATED_MODELS.find((model) => model.id === id);
export const downloadedVariant = (inventory, variant) => Boolean(inventory?.tokenizer_installed && variant?.installed);

export function catalogMarkup() {
  return `<section class="panel model-catalog">
    <div class="panel-heading model-catalog-head">
      <h2>${h(i18nMessages.modelCatalog)}</h2>
      <form data-mirror class="model-mirror">
        <label for="model-mirror">${h(i18nMessages.modelMirror)}</label>
        <select id="model-mirror" name="mirror">${MIRROR_OPTIONS.map(([value, label]) =>
          `<option value="${h(value)}">${h(label)}</option>`).join("")}</select>
        <div data-mirror-custom hidden>
          <input type="url" name="mirror-custom" aria-label="${h(i18nMessages.modelMirror)}" placeholder="https://mirror.example.com">
          <button type="submit">${h(i18nMessages.modelMirrorSave)}</button>
        </div>
      </form>
    </div>
    <div class="panel-body stack">
      <div data-legacy-model role="status" hidden></div>
      <div data-catalog-models class="model-card-grid"></div>
    </div>
  </section>
  <section class="panel model-catalog">
    <div class="panel-heading"><h2>${h(i18nMessages.modelQuantization)}</h2></div>
    <div class="panel-body stack">
      <div data-variants class="model-card-grid model-variant-grid"></div>
      <div data-model-dock class="model-action-dock"></div>
    </div>
  </section>`;
}

export function bindDownloadMirror(root, ctx) {
  const form = $("[data-mirror]", root);
  const preset = $("[name=mirror]", form);
  const custom = $("[data-mirror-custom]", form);
  const input = $("[name=mirror-custom]", form);
  const button = $("[type=submit]", form);
  let saving = false;
  const value = () => ctx.desktop?.preferences?.modelMirror || "auto";
  function sync() {
    const saved = value();
    const known = MIRROR_OPTIONS.some(([option]) => option === saved && option !== "custom");
    preset.value = known ? saved : "custom";
    input.value = known ? "" : saved;
    custom.hidden = known;
  }
  async function save(mirror) {
    saving = preset.disabled = input.disabled = button.disabled = true;
    try {
      await ctx.savePreferences({ modelMirror: mirror });
      sync();
      notify(i18nMessages.m9b49b31f2416becd);
    } catch (error) {
      if (preset.value !== "custom") sync();
      notify(error.message, true);
    } finally {
      saving = preset.disabled = input.disabled = button.disabled = false;
    }
  }
  preset.addEventListener("change", () => {
    if (saving) return;
    custom.hidden = preset.value !== "custom";
    if (custom.hidden) return save(preset.value);
    input.focus();
  });
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    if (saving || preset.value !== "custom") return;
    const mirror = input.value.trim();
    if (!URL.canParse(mirror)) return notify(i18nMessages.modelMirrorInvalid, true);
    const url = new URL(mirror);
    if (!["http:", "https:"].includes(url.protocol) || url.username || url.password)
      return notify(i18nMessages.modelMirrorInvalid, true);
    return save(mirror);
  });
  return { sync, value };
}

const badge = (text, active = false) => `<span class="model-badge${active ? " current" : ""}">${h(text)}</span>`;
const pressed = (value) => `aria-pressed="${value}"`;
const disabled = (value) => value ? " disabled" : "";
const count = (value, label) => `${h(number(value))} <span>${h(label)}</span>`;

export function renderCatalog(root, ctx, view, active, busy) {
  const { modelID, variantID, engine, inventory } = view;
  const model = curatedModel(modelID);
  const legacy = $("[data-legacy-model]", root);
  legacy.hidden = active?.model !== "bge-m3-distill-8l";
  if (!legacy.hidden) setMarkup(legacy, `${badge(i18nMessages.modelLegacyActive, true)} <span>${h(active.model)} · ${h(active.variant || "—")} · ${h(active.engine?.toUpperCase() || "—")}</span>`);

  setMarkup($("[data-catalog-models]", root), CURATED_MODELS.map((entry) => {
    const files = ctx.modelInventory.get(entry.id);
    const downloaded = files?.variants.filter((variant) => downloadedVariant(files, variant)).length;
    return `<button type="button" class="model-choice" data-catalog-model="${entry.id}" ${pressed(entry.id === modelID)}>
      <strong>${h(entry.title)}</strong>
      <span class="model-card-badges">${active?.model === entry.id ? badge(i18nMessages.modelActive, true) : ""}${entry.id === DEFAULT_MODEL ? badge(i18nMessages.modelRecommendedDefault) : ""}</span>
      <span>${count(entry.dims, i18nMessages.modelDims)} · ${count(entry.context, i18nMessages.modelContext)}</span>
      <span>${h(entry.tag)}</span>
      <span>${count(entry.variantCount, i18nMessages.modelVariants)} · ${downloaded == null ? "—" : h(number(downloaded))} <span>${h(i18nMessages.modelDownloaded)}</span></span>
    </button>`;
  }).join(""));

  setMarkup($("[data-variants]", root), inventory ? inventory.variants.map((variant) => {
    const filename = variantFilename(variant);
    const current = marksActiveVariant(active, modelID, variant.name);
    const status = current ? i18nMessages.modelActive : downloadedVariant(inventory, variant)
      ? i18nMessages.modelDownloaded : i18nMessages.modelNotDownloaded;
    return `<button type="button" class="model-choice model-variant" data-catalog-variant="${h(variant.name)}" ${pressed(variant.name === variantID)}>
      <strong>${h(variant.name)}</strong>${badge(status, current)}
      <code title="${h(filename)}">${h(filename)}</code><span>${h(bytes(variant.size))}</span>
    </button>`;
  }).join("") : `<p class="hint" role="status">${h(view.inventoryErrors.has(modelID) ? i18nMessages.modelInventoryFailed : i18nMessages.modelInventoryLoading)}</p>`);

  const variant = inventory?.variants.find(({ name }) => name === variantID);
  const installed = downloadedVariant(inventory, variant);
  const activeVariant = marksActiveVariant(active, modelID, variantID);
  const activeCombo = marksActiveVariant(active, modelID, variantID, engine);
  const platform = ctx.desktop?.platform;
  const tags = backends[platform?.split("-")[0]];
  const engineDisabled = !engineAvailable(platform, engine);
  const actionDisabled = busy || !ctx.desktop?.connected || !variant;
  const button = (action, label, unavailable, className = "") =>
    `<button type="button" class="${className}" data-catalog-action="${action}"${disabled(unavailable)}>${label}</button>`;
  const probe = button("probe", h(i18nMessages.modelProbe), actionDisabled || engineDisabled);
  const known = view.activeKnown();
  const actions = activeCombo
    ? button("enabled", h(i18nMessages.modelEnabled), true) + probe + button("open", h(i18nMessages.modelOpenFolder), !variant?.path)
    : installed
      ? button("enable", h(i18nMessages.modelEnable), actionDisabled || engineDisabled || !known, "primary") + probe +
        button("delete", h(i18nMessages.modelDelete), actionDisabled || activeVariant || !known, "danger")
      : button("download", `<span>${h(i18nMessages.modelDownload)}</span>${variant ? ` (${h(bytes(variant.size))})` : ""}`, actionDisabled, "primary");
  const combo = `${model.title} · ${variantID} · ${engine.toUpperCase()}`;
  setMarkup($("[data-model-dock]", root), `<strong class="model-combo" title="${h(combo)}">${h(combo)}</strong>
    <div class="model-engine-picker" role="group" aria-label="${h(i18nMessages.modelEngine)}">${engines.map((id) =>
      `<button type="button" data-catalog-engine="${id}" ${pressed(id === engine)}${disabled(!engineAvailable(platform, id))}>${id.toUpperCase()} <small>${id === "cpu" ? "ORT" : tags?.[id] || "—"}</small></button>`).join("")}</div>
    <div class="model-dock-actions">${actions}</div>`);
}

const renderedMarkup = new WeakMap();
function setMarkup(node, markup) {
  if (renderedMarkup.get(node) === markup) return;
  const focused = document.activeElement;
  const restore = focused && node.contains(focused) &&
    ["data-catalog-model", "data-catalog-variant", "data-catalog-engine", "data-catalog-action"]
      .find((name) => focused.hasAttribute(name));
  renderedMarkup.set(node, markup);
  node.innerHTML = markup;
  if (restore) node.querySelector(`[${restore}="${CSS.escape(focused.getAttribute(restore))}"]`)?.focus({ preventScroll: true });
}
