import { messages as i18nMessages } from "../ui/i18n/messages.js";
import {
  $,
  h,
  rows,
  raw,
  input,
  dialog,
  execute,
  action,
  confirm,
  notify,
  showText,
} from "../ui/core.js";

export function invitationDialog() {
  dialog({
    title: i18nMessages.me925f63556925ab9,
    body:
      input("note", i18nMessages.md60dcdd586806e94) +
      i18nMessages.m32b7491278a720d1,
    submit: i18nMessages.m7aa9a5cbe70c4740,
    onSubmit: async (value) => {
      const result = await execute("space_invite", {
        note: value.note,
        readonly: Boolean(value.readonly),
      });
      showText(i18nMessages.m16b8a5a0ac9e80d2, raw(result.details || result.summary));
    },
  });
}
export async function membersDialog() {
  const element = dialog({
    title: i18nMessages.m1b1d4c2861b840fd,
    body: i18nMessages.m1f9031cfbcabffdd,
    submit: i18nMessages.m3fd47edce45b3603,
    onSubmit: () => {},
  });
  async function load() {
    const result = await execute("space_members");
    if (!element.isConnected) return;
    const members = rows(result, "members");
    $("[data-members]", element).innerHTML = members.length
      ? `<ul class="clean">${members.map((member) => `<li class="row"><div class="row-copy"><strong>${h(member.device_name || member.session_id)}</strong><small>${member.readonly ? i18nMessages.m3b5ec3533b0e4485 : i18nMessages.md5c3328266122e34} · ${h(member.issued_at || "")}</small></div><button class="danger" data-kick="${h(member.session_id)}${i18nMessages.m96ef6a644a135fe9}`).join("")}</ul>`
      : i18nMessages.m41848db1f0817cc4;
  }
  element.addEventListener("click", (event) => {
    const button = event.target.closest("[data-kick], [data-kick-all]");
    if (button)
      action(button, async () => {
        if (
          !(await confirm(
            i18nMessages.m95561c26cc92afa0,
            button.hasAttribute("data-kick-all")
              ? i18nMessages.m9a220d9fe80f9db6
              : i18nMessages.m11a902b4d6160e32,
            i18nMessages.m227e93b266876ebd,
          ))
        )
          return;
        const result = button.hasAttribute("data-kick-all")
          ? await execute("space_kick_all")
          : await execute("space_kick", { session: button.dataset.kick });
        if (result.status === "warn") showText(i18nMessages.m642fd23eca17f757, raw(result));
        else notify(i18nMessages.m05c72bb28258928f);
        await load();
      });
  });
  try {
    await load();
  } catch (e) {
    $("[data-members]", element).textContent = e.message;
  }
}
