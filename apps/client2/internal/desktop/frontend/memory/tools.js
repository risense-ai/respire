import { messages as i18nMessages } from "../ui/i18n/messages.js";
import {
  $,
  raw,
  execute,
  action,
  confirm,
  showText,
  notify,
  dialog,
  input,
} from "../ui/core.js";
import { recordUndo } from "./undo.js";
import { inspectGrants } from "../pages/grants.js";

export function memoryTools(reader, memory, { changed }) {
  const area = document.createElement("div");
  area.className = "panel-body actions";
  area.innerHTML =
    i18nMessages.m2f3b29b102de909c;
  reader.append(area);
  area.onclick = (event) => {
    const button = event.target.closest("[data-tool]");
    if (!button) return;
    action(button, async () => {
      const kind = button.dataset.tool;
      if (kind === "access") return inspectGrants(memory);
      if (kind === "purge") {
        dialog({
          title: i18nMessages.m34b310fd2a7e66d9,
          body:
            i18nMessages.m309b3544d196f405 +
            input("id", i18nMessages.m57bead98c19e1f04, "", { required: true }),
          submit: i18nMessages.m947399b3ab715ec1,
          onSubmit: async (value) => {
            if (value.id.trim() !== memory.id)
              throw Error(i18nMessages.mc7a528b93466cbed);
            await execute("memory_purge", { id: memory.id });
            notify(i18nMessages.md8a725b4347f222f);
            await changed();
          },
        });
        return;
      }
      if (kind === "promote") {
        if (
          !(await confirm(
            i18nMessages.mf6291ac94a1ef0e4,
            i18nMessages.m30b590e08d19fbcf,
            i18nMessages.m8c51a6bcd64b3071,
          ))
        )
          return;
        await execute("memory_promote", { id: memory.id });
        await recordUndo({ kind: "move", id: memory.id, before: memory });
        await changed();
        return;
      }
      const name = {
        book: "book_material",
        portrait: "portrait_material",
        material: "memory_material",
      }[kind];
      const result = await execute(
        name,
        kind === "portrait" ? { limit: 40 } : { root: memory.id },
      );
      showText(
        { book: i18nMessages.mcce00744cbb2a23a, portrait: i18nMessages.mbca8e1c450e6079c, material: i18nMessages.ma1294dfdaa790f7a }[
          kind
        ],
        raw(result.details || result.summary),
      );
    });
  };
}
