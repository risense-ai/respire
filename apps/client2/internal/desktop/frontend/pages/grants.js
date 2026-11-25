import { messages as i18nMessages } from "../ui/i18n/messages.js";
import {
  $,
  h,
  raw,
  execute,
  action,
  confirm,
  dialog,
  input,
  panel,
  showText,
  notify,
  rows,
} from "../ui/core.js";
import { heading } from "./overview.js";
import { t } from "../ui/i18n.js";
import { entry } from "../memory/model.js";

export async function inspectGrants(memory) {
  const ancestors = new Set(), seen = new Set([memory.id]);
  let parent = memory.parent_id;
  while (parent && !seen.has(parent)) {
    if (seen.size >= 256) throw Error(i18nMessages.m5f3dead7154e598b);
    seen.add(parent);
    ancestors.add(parent);
    parent = entry(await execute("memory_show", { id: parent })).parent_id;
  }
  const grants = rows(await execute("grant_list"), "grants");
  const relevant = grants.filter((grant) => !grant.revoked &&
    (grant.root_id === memory.id || ancestors.has(grant.root_id)));
  showText(i18nMessages.m6a868b79f55a3165, raw({
    node: memory.id,
    note: i18nMessages.mec98dc1d49c6752c,
    grants: relevant.map((grant) => ({ ...grant,
      relationship: grant.root_id === memory.id ? i18nMessages.m2e2fa8914be8c2cc : i18nMessages.m2fdd805e6345608e })),
  }));
}

export function grants(root, ctx) {
  let loading = false,
    attempted = false;
  root.innerHTML =
    heading(
      i18nMessages.m9a3851e9d2cb4763,
      i18nMessages.mb8e2f676c3923c0e,
      i18nMessages.m1c6327bbfae91bc3,
    ) + "<div data-grants></div>";
  async function load() {
    if (loading || !ctx.desktop?.connected) return;
    loading = true;
    attempted = true;
    try {
      const result = await execute("grant_list");
      if (!root.isConnected) return;
      const records = rows(result, "grants");
      $("[data-grants]", root).innerHTML = panel(
        i18nMessages.m5060cae0de5fb260,
        records.length
          ? `<ul class="clean">${records.map((item) => `<li class="row"><div class="row-copy"><strong>${h(item.label || item.id)}</strong><small>${h(t(i18nMessages.grantSubtreeLabel))} ${h(item.root_id || item.root)} · ${h(t(item.revoked ? i18nMessages.mfef96e34f77b742b : i18nMessages.m3b5ec3533b0e4485))}</small></div>${item.revoked ? "" : `<button class="danger" data-revoke="${h(item.id)}${i18nMessages.m36e4ca8fc6d13508}`}</li>`).join("")}</ul>`
          : i18nMessages.m686cf93ed312b399,
      );
    } catch (e) {
      if (root.isConnected) $("[data-grants]", root).textContent = e.message;
    } finally {
      loading = false;
    }
  }
  $("[data-create]", root).onclick = () =>
    dialog({
      title: i18nMessages.me8a0f6ed03f1a63a,
      body:
        input("root", i18nMessages.mc0f89e1f11d71cdc, "", { required: true }) +
        input("label", i18nMessages.m9543f9eef7c29904, "", { required: true }) +
        i18nMessages.m2c8f265b96b2c000,
      onSubmit: async (values) => {
        const result = await execute("grant_create", values);
        showText(i18nMessages.mf82c7601b57bd6c4, raw(result.details || result.summary));
        await load();
      },
    });
  root.onclick = (event) => {
    const button = event.target.closest("[data-revoke]");
    if (button)
      action(button, async () => {
        if (
          !(await confirm(
            i18nMessages.m94cd3fc74631f293,
            i18nMessages.me1ab9b20e07da08f,
            i18nMessages.m926a50b98ece2667,
          ))
        )
          return;
        await execute("grant_revoke", { id: button.dataset.revoke });
        notify(i18nMessages.m7e1d24e7fd16dcc3);
        await load();
      });
  };
  load();
  return {
    refresh: load,
    update() {
      if (!attempted) load();
    },
  };
}
