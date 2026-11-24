import { messages as i18nMessages } from "../ui/i18n/messages.js";
import {
  $,
  all,
  h,
  raw,
  input,
  select,
  dialog,
  execute,
  action,
  confirm,
  notify,
  summary,
  panel,
  resultView,
  showText,
} from "../ui/core.js";
import { heading } from "./overview.js";
import { report, entry } from "../memory/model.js";

export function organize(root, ctx) {
  let plan = null,
    planRevision = "",
    suggestions = [],
    backend = "ds";
  root.innerHTML =
    heading(
      i18nMessages.m13279f26b37b9f22,
      i18nMessages.mad32a102aafc5320,
    ) +
    `${i18nMessages.m74eecf56f38089b8}` +
    `${i18nMessages.m78095bdde1fa8887}` +
    panel(
      i18nMessages.m6a345e92b15041ed,
      `${i18nMessages.mdd9deb95178bb430}`,
    ) +
    panel(
      i18nMessages.mee92667a857268db,
      `<form data-ai class="panel-body stack"><div class="form-grid">${select(
        "backend",
        i18nMessages.m0dd2743cdc788409,
        [
          ["ds", i18nMessages.mce3956dabca876c6],
          ["jev", "Jev"],
        ],
        "ds",
      )}${input("limit", i18nMessages.m980458ea993bde31, "50", { type: "number", required: true })}${input("root", i18nMessages.mf42f11358e5907c9)}${i18nMessages.mf7a844056abb5620}`,
    ) +
    `<div data-ai-result class="section-gap"></div>`;
  const area = $("[data-report]", root),
    specField = $("[name=spec]", root);
  async function revision() {
    return raw(summary(await execute("memory_revision")));
  }
  function setPlan(value, stamp) {
    plan = value;
    planRevision = stamp;
    specField.value = raw(value);
    $("[data-plan-apply]", root).disabled = true;
  }
  async function inspect(kind) {
    const names = {
      suggestions: "organize_suggestions",
      duplicates: "organize_duplicates",
      audit: "organize_audit",
      plan: "organize_plan",
    };
    const stamp = await revision();
    const result = await execute(
      names[kind],
      kind === "suggestions"
        ? { top: 50 }
        : kind === "duplicates"
          ? { min: 0.8, top: 30 }
          : {},
    );
    if (!root.isConnected) return;
    const value = report(result);
    if (kind === "suggestions") {
      if (!Array.isArray(value.suggests))
        throw new Error(i18nMessages.m506261e7db1c5ca7);
      suggestions = value.suggests;
      area.innerHTML = panel(
        i18nMessages.m2bc0ee63d3cd9cfe,
        suggestions.length
          ? i18nMessages.m27dce25c8c4d732f + suggestions
              .map(
                (item, index) =>
                  `<div class="suggestion-card"><div class="row-copy"><strong>${h(item.orphan?.title || item.orphan?.id)}</strong><p>→ ${h(item.target?.title || item.target?.id)}</p><small>${h(item.target_tree?.title || "")}</small></div><button data-suggestion="${index}${i18nMessages.m098f5d621e840ed1}`,
              )
              .join("")
          : i18nMessages.ma97e3e49972317f2,
      );
    } else if (kind === "plan") {
      if (!Array.isArray(value.ops))
        throw new Error(i18nMessages.m2593eca4101ea876);
      setPlan({ ops: value.ops }, stamp);
      area.innerHTML = resultView(i18nMessages.m535add146d1e5503, result);
      notify(i18nMessages.m3649467f148f732c);
    } else
      area.innerHTML = resultView(
        kind === "duplicates" ? i18nMessages.me600f4015f2de1e6 : i18nMessages.mc863edd801d2636d,
        result,
      );
  }
  async function preview() {
    const parsed = JSON.parse(specField.value);
    if (!Array.isArray(parsed.ops)) throw new Error(i18nMessages.med352f1779fa3c88);
    const stamp = await revision();
    const result = await execute("organize_resort_preview", {
      spec: raw(parsed),
    });
    plan = parsed;
    planRevision = stamp;
    area.innerHTML = resultView(i18nMessages.ma6dacc87d2ae1a12, result);
    $("[data-plan-apply]", root).disabled = false;
    notify(i18nMessages.me5901ed29595e961);
  }
  async function ai(apply) {
    const value = Object.fromEntries(new FormData($("[data-ai]", root)));
    const args = {
      backend: value.backend,
      limit: Number(value.limit),
      causal: true,
    };
    if (value.root.trim()) args.root = value.root.trim();
    if (value.all) args.all = true;
    if (apply) {
      if (
        !(await confirm(
          i18nMessages.m5872eeefa1ca224d,
          i18nMessages.m8beb469795f95b02,
          i18nMessages.m68a35e2242686dcc,
        ))
      )
        return;
      args.auto = true;
    }
    const stamp = await revision();
    const result = await execute("organize_classify", args, {
      onProgress: (job) => {
        if (root.isConnected)
          $("[data-ai-result]", root).innerHTML = panel(
            i18nMessages.m7c37a843de2c7186,
            `${i18nMessages.mbc9865b6430bdc74}${h(job.id)}</small></div>`,
          );
      },
    });
    if (!root.isConnected) return;
    $("[data-ai-result]", root).innerHTML = resultView(
      apply ? i18nMessages.m5283c9082c03c269 : i18nMessages.m2fa75549aafb98cb,
      result,
    );
    if (!apply) {
      const value = report(result);
      if (Array.isArray(value.ops)) setPlan({ ops: value.ops }, stamp);
    }
    if (apply) await ctx.refresh();
  }
  root.addEventListener("click", (event) => {
    const button = event.target.closest("button");
    if (!button) return;
    if (button.dataset.analyze)
      action(button, () => inspect(button.dataset.analyze));
    if (button.dataset.suggestion !== undefined)
      action(button, async () => {
        const item = suggestions[Number(button.dataset.suggestion)];
        setPlan(
          { ops: [{ id: item.orphan.id, parent: item.target.id }] },
          await revision(),
        );
        await preview();
      });
    if (button.hasAttribute("data-suggestions-all"))
      action(button, async () => {
        setPlan({ ops: suggestions.map((item) => ({
          id: item.orphan.id, parent: item.target.id,
        })) }, await revision());
        await preview();
      });
    if (button.hasAttribute("data-plan-preview")) action(button, preview);
    if (button.hasAttribute("data-plan-copy"))
      action(button, async () => {
        await navigator.clipboard.writeText(specField.value);
        notify(i18nMessages.medef96106a3a651a);
      });
    if (button.hasAttribute("data-plan-apply"))
      action(button, async () => {
        if (!plan || raw(JSON.parse(specField.value)) !== raw(plan))
          throw new Error(i18nMessages.m03c5d4565d86fb6c);
        if ((await revision()) !== planRevision)
          throw new Error(i18nMessages.meba8b3ca98eac75d);
        if (
          !(await confirm(
            i18nMessages.m2d3fac1cb61acbe3,
            `${i18nMessages.m0d82373d48608890}${plan.ops.length}${i18nMessages.m52280276afdc99ec}`,
            i18nMessages.m487c5545cad9a298,
          ))
        )
          return;
        const result = await execute("organize_resort_apply", {
          spec: raw(plan),
        });
        area.innerHTML = resultView(i18nMessages.m7ace24d13fae7bc3, result);
        $("[data-plan-apply]", root).disabled = true;
        notify(i18nMessages.m83419a84e7e85857);
        await ctx.refresh();
      });
    if (button.hasAttribute("data-ai-apply")) action(button, () => ai(true));
    if (button.hasAttribute("data-provider"))
      action(button, async () => {
        const current = summary(
          await execute("classification_config", {
            backend: $("[name=backend]", root).value,
          }),
        );
        dialog({
          title: i18nMessages.m49c8025c7a18032f,
          body: `${select(
            "backend",
            i18nMessages.m9b826346239e803f,
            [
              ["ds", i18nMessages.mce3956dabca876c6],
              ["jev", "Jev"],
            ],
            current.backend || backend,
          )}${input("base", i18nMessages.mb893d28dec278644, current.api_base || "https://api.deepseek.com/v1")}${input("model", i18nMessages.me47a7f8169ee2a69, current.model || "deepseek-flash")}${input("key", i18nMessages.m6f390ff0a6915223, "", { type: "password" })}${i18nMessages.mbd95c483fdb66a2e}`,
          onSubmit: async (values) => {
            const args = { backend: values.backend };
            if (values.backend === "ds") {
              args.base = values.base;
              args.model = values.model;
            }
            await execute("classification_save", args, { secret: values.key });
            backend = values.backend;
            notify(i18nMessages.m90d94e3fd7910ca0);
            await loadProvider();
          },
        });
      });
  });
  specField.addEventListener("input", () => {
    $("[data-plan-apply]", root).disabled = true;
  });
  $("[data-ai]", root).addEventListener("submit", (event) => {
    event.preventDefault();
    action($("[type=submit]", event.target), () => ai(false));
  });
  async function loadProvider() {
    try {
      const value = summary(await execute("classification_config"));
      if (!root.isConnected) return;
      backend = value.backend || "ds";
      $("[name=backend]", root).value = backend;
      $("[data-provider-state]", root).textContent = value.has_key
        ? i18nMessages.m8f1fb304f0852ada
        : i18nMessages.m1382b2e978a8fc30;
    } catch (error) {
      if (root.isConnected)
        $("[data-provider-state]", root).textContent = error.message;
    }
  }
  if (ctx.desktop?.connected) loadProvider();
  return { refresh: () => inspect("suggestions") };
}
