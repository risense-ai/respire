import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { execute, raw, summary, notify } from "../ui/core.js";

let lastUndo = null;
let running = false;
const stamp = async () => {
  const status = summary(await execute("status"));
  return raw([status.data_dir, summary(await execute("memory_revision"))]);
};
export async function recordUndo({ kind, id, before, importance, action }) {
  if (
    (kind === "create" && (importance !== "important" || action !== "created")) ||
    (kind === "edit" && (before?.importance !== "important" || importance !== "important"))
  ) {
    lastUndo = null;
    notify(i18nMessages.m25b23530e29d6d0d);
    return;
  }
  try {
    lastUndo = { kind, id, before, revision: await stamp() };
  } catch (error) {
    lastUndo = null;
    notify(
      `${i18nMessages.m111790a436bcd102}${error.message}${i18nMessages.m114ed002336870fa}`,
      true,
    );
  }
}
export async function undoLast() {
  if (running) throw new Error(i18nMessages.m7a56af26e8d4bd4f);
  const item = lastUndo;
  if (!item) throw new Error(i18nMessages.m76371501e9bf8c75);
  running = true;
  try {
    if ((await stamp()) !== item.revision)
      throw new Error(
        i18nMessages.mfc99985fa226cf73,
      );
    lastUndo = null;
    // Consume before submitting: an unknown write outcome must never be replayed.
    if (item.kind === "create") await execute("memory_delete", { id: item.id });
    else if (item.kind === "delete")
      await execute("memory_restore", { id: item.id });
    else if (item.kind === "move")
      await execute("organize_resort_apply", {
        spec: raw({
          ops: [{ id: item.id, parent: item.before.parent_id || "" }],
        }),
      });
    else {
      const old = item.before;
      await execute("memory_update", {
        id: item.id,
        title: old.title,
        content: old.content,
        kind: old.kind.toLowerCase(),
        tags: (old.tags || []).join(","),
        importance: old.importance,
      });
    }
    // Revision checks are not an atomic CAS; do not revalidate older operations
    // after a write that could have raced another client.
    return item.kind === "create" ? "" : item.id;
  } finally {
    running = false;
  }
}
