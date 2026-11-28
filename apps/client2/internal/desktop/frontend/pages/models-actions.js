import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { $, execute, action, confirm, notify, resultView, waitForOperation } from "../ui/core.js";
import { cancellableModelID, liveModelOperation, syncModelCancel } from "./models-view.js";
import { reopenActiveDownloadModal } from "./model-kit.js";

export async function followRebuild(job, root, ctx, isCurrent, onSettled) {
  try {
    const result = await waitForOperation(job);
    if (isCurrent()) $("[data-result]", root).innerHTML = resultView(i18nMessages.m4347dbc9b646e415, result);
  } catch (error) {
    if (isCurrent()) notify(error.message, true);
  } finally {
    onSettled();
    if (isCurrent()) await ctx.refresh();
  }
}

export function bindModelActions(view, { root, ctx, refreshProfileState, runIndexedAction }) {
  root.addEventListener("click", (event) => {
    const button = event.target.closest("[data-model]");
    if (!button) return;
    const operation = button.dataset.model;
    if (operation === "reopen-download") return action(button, reopenActiveDownloadModal);
    if (operation === "cancel") {
      const id = cancellableModelID(liveModelOperation(view, ctx.status));
      if (!id || view.cancelPending) return;
      return action(button, async () => {
        view.cancelPending = id;
        syncModelCancel(root, view, liveModelOperation(view, ctx.status));
        try {
          const result = await execute("model_cancel", { id });
          if (root.isConnected)
            $("[data-result]", root).innerHTML = resultView(i18nMessages.m4347dbc9b646e415, result);
        } finally {
          view.cancelPending = null;
          await refreshProfileState();
        }
      }).finally(() => syncModelCancel(root, view, liveModelOperation(view, ctx.status)));
    }
    if (operation !== "index" && operation !== "rebuild") return;
    return action(button, async () => {
      if (operation === "rebuild" && !await confirm(
        i18nMessages.m57644898280f0862, i18nMessages.me4976021b8157aca, i18nMessages.m7da653631fe83d12,
      )) return;
      const result = await runIndexedAction(operation === "index" ? "index_prepare" : "index_rebuild");
      if (root.isConnected) $("[data-result]", root).innerHTML = resultView(i18nMessages.m4347dbc9b646e415, result);
      notify(i18nMessages.m18df5c941d940c3d);
      await ctx.refresh();
    });
  });
}
