import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { $, bytes, summary, execute, action, confirm, notify, resultView, api } from "../ui/core.js";
import {
  MODEL_OPTIONS,
  isProfileModel,
  applyLegacyEngine,
  submitProfileSwitch,
  fallbackMessage,
  openDownloadModal,
} from "./model-kit.js";

export function bindModelActions(view, {
  root,
  renderAdvancedVisibility,
  handleModeSelect,
  handlePrimaryAction,
  refreshProfileState,
  renderVariants,
  renderDynamic,
  ctx,
  currentMirror,
  load,
}) {
  root.addEventListener("click", (event) => {
    const toggle = event.target.closest("[data-action=toggle-advanced]");
    if (toggle) {
      view.advancedExpanded = !view.advancedExpanded;
      renderAdvancedVisibility();
      return;
    }
    const modeCard = event.target.closest("[data-mode-select]");
    if (modeCard) {
      handleModeSelect(modeCard.dataset.modeSelect);
      return;
    }
    const modeApply = event.target.closest("[data-mode-apply]");
    if (modeApply) {
      action(modeApply, () => handlePrimaryAction());
      return;
    }
    const apply = event.target.closest("[data-apply]");
    if (apply) {
      action(apply, async () => {
        const engine = apply.dataset.engine;
        const variant = apply.dataset.variant;
        if (isProfileModel(view.modelID)) {
          view.switchingVariant = variant;
          view.switchInFlight = true;
          view.switchTicks = 0;
          if (!view.switchTimer)
            view.switchTimer = setInterval(() => refreshProfileState(), 3000);
          try {
            await submitProfileSwitch(view.modelID, engine, variant);
            notify(`${i18nMessages.m0e3d526f29331bd7}${view.modelID} · ${engine.toUpperCase()} · ${variant}${i18nMessages.m1df4614471fdde44}`);
          } catch (error) {
            view.switchInFlight = false;
            view.switchingVariant = "";
            clearInterval(view.switchTimer);
            view.switchTimer = null;
            throw error;
          }
          await refreshProfileState();
          return;
        }
        // Legacy m3: one host operation saves the selection, reloads the
        // model and probes it; on failure the host restores the previous
        // selection and the receipt carries the cause.
        view.switchingVariant = variant;
        view.switchInFlight = true;
        renderVariants();
        notify(`${i18nMessages.mf756893b5a1a7aa8}${engine.toUpperCase()} · ${variant}${i18nMessages.m79ecb4c2993c2d33}`);
        try {
          const { envelope, result } = await applyLegacyEngine(engine, variant);
          view.engineState = { engine: result.engine, model: result.model };
          if (!result.fallback) view.lastVerifiedAt = new Date();
          const probe = result.probe || {};
          if (typeof probe.elapsed_ms === "number" && probe.selected)
            view.lastProbe = { selected: probe.selected, elapsed_ms: probe.elapsed_ms };
          if (result.fallback)
            notify(fallbackMessage(engine, result), true);
          else
            notify(
              `${i18nMessages.m543b8d1935f06493}${engine.toUpperCase()} · ${result.model}${i18nMessages.m58c610d7a9c8ba72}${probe.selected ? `${i18nMessages.mda047c04c9b5e27e}${probe.selected} · ${probe.elapsed_ms}${i18nMessages.m4cf435bfd8907c1c}` : ""}${i18nMessages.meb0d5884569b398e}`,
            );
          if (root.isConnected)
            $("[data-result]", root).innerHTML = resultView(i18nMessages.m382c41a1a6270019, envelope);
        } finally {
          view.switchInFlight = false;
          view.switchingVariant = "";
          if (root.isConnected) {
            renderVariants();
            renderDynamic(true);
          }
          await ctx.refresh();
        }
      });
      return;
    }
    const reveal = event.target.closest("[data-open-dir]");
    if (reveal) {
      action(reveal, async () => {
        notify(await api("open-model-dir", { path: reveal.dataset.openDir }));
      });
      return;
    }
    const button = event.target.closest("[data-model]");
    if (!button) return;
    action(button, async () => {
      const operation = button.dataset.model;
      let result;
      const mirror = currentMirror();
      if (operation === "prepare") {
        if (
          !(await confirm(
            i18nMessages.m569629fad59bbf11,
            i18nMessages.m96fba8bc01d4ae3c,
            i18nMessages.m541961415c3d4b09,
          ))
        )
          return;
        view.sawMissingBaseline = true;
        openDownloadModal({
          model: view.modelID,
          variant: "baseline",
          label: i18nMessages.m801867119ecff472,
          getMirror: currentMirror,
          onDone: () => load(),
        });
        await ctx.refresh();
      } else if (operation === "download") {
        const variant = button.dataset.variant;
        const size = view.inventory?.variants?.find(
          (item) => item.name === variant,
        )?.size;
        const label =
          MODEL_OPTIONS.find(([id]) => id === view.modelID)?.[1] || view.modelID;
        if (
          !(await confirm(
            i18nMessages.m67d6e756c50437db,
            `${i18nMessages.m7e3b2989a9f4819f}${label}${i18nMessages.m00c911729bb78544}${variant}${i18nMessages.m0820ce7967d9e8e3}${bytes(size)}${i18nMessages.md258db85a8e44ba0}${view.inventory?.model_dir || i18nMessages.m95acb8ed97c94e61}${i18nMessages.md1b83389357335d2}`,
            i18nMessages.m541961415c3d4b09,
          ))
        )
          return;
        openDownloadModal({
          model: view.modelID,
          variant,
          label,
          getMirror: currentMirror,
          onDone: () => load(),
        });
        await ctx.refresh();
      } else if (operation === "cancel") {
        const id = ctx.status?.summary?.model_operation?.id;
        if (!id) throw new Error(i18nMessages.m0eda705f43ee11f7);
        result = await execute("model_cancel", { id });
      } else if (operation === "uninstall") {
        if (
          !(await confirm(
            i18nMessages.mb10b98c9247f55b0,
            i18nMessages.m347c983277bfa7d9,
            i18nMessages.m06bc14b60f3598a3,
          ))
        )
          return;
        result = await execute("model_uninstall");
      } else if (operation === "probe") {
        // Real inference on the active settings: the receipt names the
        // execution provider the runtime actually selected.
        result = await execute("model_probe");
        const probe = summary(result) || {};
        if (probe.selected && typeof probe.elapsed_ms === "number")
          view.lastProbe = { selected: probe.selected, elapsed_ms: probe.elapsed_ms };
        notify(
          `${i18nMessages.md7104a3e99ee9fd9}${probe.selected || i18nMessages.m46cbcb7c5a65bff9} · ${probe.elapsed_ms ?? "—"}ms · cosine ${typeof probe.cosine === "number" ? probe.cosine.toFixed(4) : "—"}`,
        );
      } else if (operation === "providers")
        result = await execute("model_install_engines");
      else if (operation === "index") result = await execute("index_prepare");
      else if (operation === "rebuild") {
        if (
          !(await confirm(
            i18nMessages.m57644898280f0862,
            i18nMessages.me4976021b8157aca,
            i18nMessages.m7da653631fe83d12,
          ))
        )
          return;
        result = await execute("index_rebuild");
      }
      if (result && root.isConnected)
        $("[data-result]", root).innerHTML = resultView(i18nMessages.m4347dbc9b646e415, result);
      if (operation !== "probe")
        notify(i18nMessages.m18df5c941d940c3d);
      await ctx.refresh();
    });
  });
}
