import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { $, summary, execute, action, notify, api } from "../ui/core.js";

// Five-axis recall fusion (docs/client2gui-recall-fusion.md): the GUI only
// edits the persistent `recall_fusion` agent setting and shows the host
// receipt; the algorithm runs in the Runtime. Names must match the CLI.
export const FUSION_MODES = [
  ["legacy", i18nMessages.m93b30cb21f374b95],
  ["attention", i18nMessages.me2eeeb0f8c4000a6],
  ["attention_anchor", i18nMessages.m86cac6c2ca87306a],
  ["auto", i18nMessages.mdb9773faa309c257],
];
export const FUSION_NUMERIC = [
  ["fusion_alpha", i18nMessages.m12b7493a7d7fd921, 0.2],
  ["fusion_beta", i18nMessages.mebc245bb195fee79, 0.25],
  ["fusion_temperature", i18nMessages.m6592c5b83e8b7950, 1],
  ["fusion_gate_recency", i18nMessages.m0f4c34612d5dc70a, 0.02],
  ["fusion_gate_recency_temporal", i18nMessages.m16a83e1ab2f22639, 1],
  ["fusion_gate_hot", i18nMessages.mae133fc798ecbd8c, 0.1],
  ["fusion_gate_tree", i18nMessages.m51437ea69aa89ebe, 0.02],
  ["fusion_temporal_alpha", i18nMessages.mcef2be911c6ed9fd, 0.15],
];
const fusionLabel = (key) =>
  FUSION_NUMERIC.find(([name]) => name === key)?.[1] || key;
const fusionModeName = (mode) =>
  FUSION_MODES.find(([id]) => id === mode)?.[1] || mode;
export function bindModelSettings({ root, ctx, fusion, currentMirror, load }) {
  // Host configuration is not proof of the resident Runtime's loaded settings.
  // Keep editing saved values available while marking unverified state explicitly.
  function renderFusion() {
    const area = $("[data-fusion]", root);
    if (!area) return;
    const runtime = fusion.runtime;
    const editor = $("[data-fusion-form]", root);
    const state = $("[data-fusion-state]", root);
    $("[data-fusion-query]", root).hidden = !fusion.requestId;
    if (!runtime?.effective) {
      editor.hidden = true;
      $("[data-fusion-effective]", root).textContent = fusion.loadFailed
        ? i18nMessages.m7eabd5e0fac05f40
        : fusion.checked
          ? i18nMessages.m12b4eff36ebc21a1
          : i18nMessages.m8a044dd742765ffd;
      return;
    }
    editor.hidden = false;
    const overrides = Array.isArray(runtime.overrides) ? runtime.overrides : [];
    const configured = runtime.configured || {};
    const verified = runtime.runtime_verified === true;
    let text = verified
      ? `${i18nMessages.mcf92a499fc1b1c91}${fusionModeName(runtime.effective.fusion)}${i18nMessages.meb0d5884569b398e}`
      : i18nMessages.m9321751055a2de69;
    text += Object.keys(configured).length
      ? i18nMessages.m9406b1c20f69dea6
      : i18nMessages.m2eb73d03e8366cac;
    if (verified && overrides.length)
      text += `${i18nMessages.mdfd249cb57f64812}${overrides.length}${i18nMessages.m6b2f970f15df40e5}${overrides
        .map(
          (key) =>
            `${fusionLabel(key)}${i18nMessages.mf936266b58d0a768}${runtime.effective[key]}${
              configured[key] !== undefined
                ? `${i18nMessages.m20fe257efbd4bd7f}${configured[key]}${i18nMessages.mf2e44c6c9545f08b}`
                : ""
            }`,
        )
        .join(i18nMessages.mb0d3c9cfa0e1ef67)}${i18nMessages.m522b5f4e3e944216}`;
    $("[data-fusion-effective]", root).textContent = text;
    state.textContent = fusion.requestId ? i18nMessages.ma1ad337e231e90f0 : "";
    $("[type=submit]", editor).disabled = Boolean(fusion.requestId);
    const draft = fusion.draft || {};
    $("[name=fusion]", editor).value = draft.fusion || "legacy";
    for (const [key] of FUSION_NUMERIC) {
      const node = $(`[name=${key}]`, editor);
      if (node) node.value = draft[key] === undefined ? "" : String(draft[key]);
    }
  }
  // Collect + validate the draft. Only instant feedback: the host and the
  // Runtime re-validate the whole object on save.
  function fusionDraft() {
    const form = $("[data-fusion-form]", root);
    const draft = { fusion: $("[name=fusion]", form).value };
    for (const [key, label] of FUSION_NUMERIC) {
      const text = $(`[name=${key}]`, form).value.trim();
      if (!text) throw new Error(`${label}${i18nMessages.m8f1ac1c9d94fe02a}`);
      const value = Number(text);
      if (!Number.isFinite(value)) throw new Error(`${label}${i18nMessages.mb1fc54e00e2b3fa1}`);
      if (key === "fusion_temperature") {
        if (value <= 0 || value > 10)
          throw new Error(`${label}${i18nMessages.mf98f9f4485b9366b}`);
      } else if (value < 0 || value > 1) {
        throw new Error(`${label}${i18nMessages.mbff7f6bb2625dac0}`);
      }
      draft[key] = value;
    }
    if (draft.fusion_alpha + draft.fusion_beta > 1)
      throw new Error(i18nMessages.m96f7b4507c3b244b);
    return draft;
  }
  function adoptFusion(runtime) {
    fusion.checked = true;
    fusion.runtime = runtime || null;
    if (fusion.runtime?.effective)
      fusion.draft = { ...fusion.runtime.defaults, ...fusion.runtime.configured };
    renderFusion();
  }
  function fusionApplicationNotice(application) {
    if (application?.state === "applied" && application.applies === "next_query" &&
        Number.isSafeInteger(application.revision) && application.revision > 0 &&
        application.scope?.account_id && application.scope?.instance_id &&
        Number.isSafeInteger(application.scope?.generation) && application.scope.generation > 0 &&
        application.effective?.recall_fusion) {
      return { text: i18nMessages.m9f2e59f05d1d9119, warning: false };
    }
    if (application?.state === "pending" && application.reason === "runtime_not_running") {
      return { text: i18nMessages.mec966299eec655cd, warning: true };
    }
    return { text: i18nMessages.m82b0706e1ca5642b, warning: true };
  }
  $("[data-mirror]", root).addEventListener("submit", (event) => {
    event.preventDefault();
    action($("[type=submit]", event.target), async () => {
      const mirror = currentMirror();
      await ctx.savePreferences({ modelMirror: mirror || "auto" });
      $("[data-mirror-state]", root).textContent =
        i18nMessages.m1f45e225299def52 + (mirror || "auto");
      notify(i18nMessages.m9b49b31f2416becd);
    });
  });
  $("[data-mirror] [name=mirror]", root).addEventListener("change", (event) => {
    $("[data-mirror-custom]", root).hidden = event.target.value !== "custom";
  });
  $("[data-recall]", root).addEventListener("submit", (event) => {
    event.preventDefault();
    action($("[type=submit]", event.target), async () => {
      await execute("agent_set", {
        setting: "recall_mode=" + new FormData(event.target).get("mode"),
      });
      notify(i18nMessages.mc6446a2d7e4591b4);
      await ctx.refresh();
    });
  });
  $("[data-api]", root).addEventListener("submit", (event) => {
    event.preventDefault();
    action($("[type=submit]", event.target), async () => {
      const value = Object.fromEntries(new FormData(event.target));
      if (value.key) {
        const previous = summary(
          await execute("classification_config"),
        ).backend;
        await execute(
          "classification_save",
          { backend: "ds", base: value.base, model: value.model },
          { secret: value.key },
        );
        $("[name=key]", event.target).value = "";
        if (previous && previous !== "ds")
          await execute("classification_save", { backend: previous });
      }
      await execute("agent_set", { setting: "recall_api_base=" + value.base });
      await execute("agent_set", { setting: "recall_model=" + value.model });
      notify(i18nMessages.m32cb03e3ae0246fd);
      await load();
    });
  });
  // Keep an uncertain operation's identity; readback cannot replace its receipt.
  $("[data-fusion-form]", root).addEventListener("submit", async (event) => {
    event.preventDefault();
    const button = $("[type=submit]", event.target);
    if (button.disabled) return;
    await action(button, async () => {
      if (!fusion.runtime?.effective)
        throw new Error(i18nMessages.m2f80aaa4a86511b0);
      if (fusion.requestId)
        throw new Error(i18nMessages.ma1ad337e231e90f0);
      const draft = fusionDraft();
      let saved;
      try {
        saved = summary(await execute("agent_set", {
          setting: "recall_fusion=" + JSON.stringify(draft),
        }));
      } catch (error) {
        if (error.requestId && error.operation?.state !== "failed") {
          fusion.requestId = error.requestId;
          renderFusion();
          notify(i18nMessages.mf8f210923b0759dc, true);
          return;
        }
        throw error;
      }
      let readBack;
      try {
        readBack = summary(await execute("agent_config")).recall_fusion_runtime;
      } catch {
        notify(i18nMessages.mc64bb243d5a886dd, true);
        return;
      }
      if (readBack?.effective) adoptFusion(readBack);
      if (
        !readBack?.configured ||
        Object.keys(draft).some((key) => readBack.configured[key] !== draft[key])
      )
        throw new Error(i18nMessages.m173b20634d2aadce);
      const notice = fusionApplicationNotice(saved.core_application);
      notify(notice.text, notice.warning);
    });
    button.disabled = Boolean(fusion.requestId);
  });
  $("[data-fusion-query]", root).addEventListener("click", (event) => {
    action(event.currentTarget, async () => {
      if (!fusion.requestId) return;
      const job = await api("operation-recover", { id: fusion.requestId });
      if (job.state === "done" || job.state === "failed") {
        fusion.requestId = null;
        const notice = job.state === "done"
          ? fusionApplicationNotice(summary(job.envelope).core_application)
          : { text: job.error || i18nMessages.m63ef2fd690ba2266, warning: true };
        notify(notice.text, notice.warning);
      } else {
        notify(i18nMessages.m44581a48e036c420, true);
      }
      renderFusion();
    });
  });
  // Restoring defaults only rewrites the draft; nothing persists until Save.
  $("[data-fusion-reset]", root).addEventListener("click", (event) => {
    action(event.target, async () => {
      if (!fusion.runtime?.defaults)
        throw new Error(i18nMessages.m6f8e4a2802e3d105);
      fusion.draft = { ...fusion.runtime.defaults };
      renderFusion();
      $("[data-fusion-state]", root).textContent = i18nMessages.m9002976fd9299a0e;
    });
  });
  return { adoptFusion, renderFusion };
}
