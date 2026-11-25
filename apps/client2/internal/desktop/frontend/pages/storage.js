import { messages as i18nMessages } from "../ui/i18n/messages.js";
import {
  $,
  api,
  input,
  panel,
  execute,
  action,
  confirm,
  notify,
  summary,
} from "../ui/core.js";

export function storagePanel(root, ctx) {
  const section = document.createElement("section");
  section.className = "section-gap";
  section.innerHTML = panel(
    i18nMessages.m862803f9d4aa26db,
    `<form class="panel-body stack">${input("dir", i18nMessages.m95ad5bf35f521bc6, ctx.status?.summary?.data_dir || "", { required: true })}${i18nMessages.me62cb4ad5a14ebef}`,
  );
  root.append(section);
  $("[data-folder]", section).onclick = (event) =>
    action(event.currentTarget, async () => {
      const dir = await api("file-dialog", { kind: "directory" });
      if (dir) $("[name=dir]", section).value = dir;
    });
  $("form", section).onsubmit = (event) => {
    event.preventDefault();
    action($("[type=submit]", section), async () => {
      const dir = $("[name=dir]", section).value.trim();
      if (
        !(await confirm(
          i18nMessages.m9948eb831dca086b,
          `${i18nMessages.mb4187f649e2e2320}${dir}${i18nMessages.m475352278ec102c5}`,
          i18nMessages.meff514b8b83f29ff,
        ))
      )
        return;
      const result = await execute("data_dir_set", { dir });
      await ctx.refresh();
      const config = summary(result);
      if (config.runtime_applied === false) {
        notify(i18nMessages.m01f3ade049e86104);
        return;
      }
      const expected = config.data_dir,
        actual = ctx.status?.summary?.data_dir;
      const normalize = (s) =>
        navigator.userAgent.includes("Windows")
          ? String(s).replaceAll("\\", "/").replace(/\/+$/, "").toLowerCase()
          : String(s).replace(/\/+$/, "");
      if (!actual || !expected || normalize(actual) !== normalize(expected))
        throw Error(
          i18nMessages.m8030188fbc066f56,
        );
      notify(i18nMessages.mdc475b216b20869b);
    });
  };
}
