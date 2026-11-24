import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { $, all, h, execute, action, notify, input, dialog, confirm, showText, rows } from "../ui/core.js";
import { t } from "../ui/i18n.js";
import { entry } from "./model.js";
import { recordUndo } from "./undo.js";

export function bindWorkspaceEvents(view, {
  renderTree,
  tree,
  checked,
  root,
  choose,
  edit,
  undo,
  load,
  search,
  reader,
  ctx,
  setFocusMode,
}) {
  // Anchor for Shift range selection: last row chosen with a plain click.
  let selectionAnchor = null;
  // Double-click folding: the first click re-renders the tree, so the
  // browser never synthesizes a dblclick across the replaced node — detect
  // two clicks on the same row ourselves instead.
  let lastFoldClick = { id: "", at: 0 };
  const isDoubleTap = (id) => {
    const now = Date.now();
    const double = lastFoldClick.id === id && now - lastFoldClick.at < 500;
    lastFoldClick = { id, at: now };
    return double;
  };
  const foldToggle = (row) => {
    const toggle = row.querySelector("[data-expand]");
    if (!toggle || toggle.disabled) return;
    const id = toggle.dataset.expand;
    if (view.expanded.has(id)) view.expanded.delete(id);
    else view.expanded.add(id);
    renderTree();
  };
  // Select a whole row: plain click resets to just this row, Ctrl/Cmd toggles
  // it in the multi-select set, Shift selects the inclusive visible range
  // between the anchor and this row.
  function selectRow(row, { ctrl = false, shift = false } = {}) {
    const id = row?.dataset.id;
    if (!id) return;
    if (shift && selectionAnchor) {
      const rows = all(".memory-row[data-id], .memory-card[data-id]", tree);
      const from = rows.findIndex((n) => n.dataset.id === selectionAnchor);
      const to = rows.findIndex((n) => n.dataset.id === id);
      if (from >= 0 && to >= 0) {
        if (!ctrl) checked.clear();
        const [a, b] = from < to ? [from, to] : [to, from];
        for (let i = a; i <= b; i++) checked.add(rows[i].dataset.id);
        selectionAnchor = id;
        renderTree();
        return;
      }
    }
    if (ctrl) {
      if (checked.has(id)) checked.delete(id);
      else checked.add(id);
    } else {
      checked.clear();
      checked.add(id);
    }
    selectionAnchor = id;
    renderTree();
  }
  root.addEventListener("click", async (event) => {
    const button = event.target.closest("button");
    const card = event.target.closest("[data-card]");
    if (card) {
      if (button) {
        // Buttons inside the card (none today) keep dedicated handling.
        choose(card.dataset.card);
        return;
      }
      // Ctrl/Cmd or Shift on a card multi/range-selects; a plain card click
      // still opens the memory in the reader.
      if (event.ctrlKey || event.metaKey || event.shiftKey) {
        selectRow(card, { ctrl: event.ctrlKey || event.metaKey, shift: event.shiftKey });
        return;
      }
      selectRow(card, { ctrl: false, shift: false });
      choose(card.dataset.card);
      return;
    }
    const row = event.target.closest(".memory-row[data-id]");
    if (row) {
      if (button && button.classList.contains("memory-title")) {
        if (event.ctrlKey || event.metaKey || event.shiftKey) {
          selectRow(row, { ctrl: event.ctrlKey || event.metaKey, shift: event.shiftKey });
          return;
        }
        // A plain title click selects the row and opens it in the reader;
        // a double-click on an expandable row folds it instead of reopening.
        selectRow(row, { ctrl: false, shift: false });
        if (isDoubleTap(row.dataset.id)) {
          foldToggle(row);
          return;
        }
        if (event.detail <= 1) choose(row.dataset.id);
        return;
      }
      if (!button) {
        // Clicking anywhere else on the row selects the whole row, and a
        // double-click folds or unfolds an expandable row.
        selectRow(row, { ctrl: event.ctrlKey || event.metaKey, shift: event.shiftKey });
        if (isDoubleTap(row.dataset.id)) foldToggle(row);
        return;
      }
      // Other buttons (tree expand toggles) fall through to their handlers.
    }
    if (!button) return;
    if (button.dataset.view) {
      view.mode = button.dataset.view;
      view.listPage = 1;
      renderTree();
      return;
    }
    if (button.dataset.pageSet) {
      view.listPage += Number(button.dataset.pageSet);
      renderTree();
      return;
    }
    if (button.dataset.select) {
      choose(button.dataset.select);
      return;
    }
    if (button.dataset.expand) {
      const id = button.dataset.expand;
      if (view.expanded.has(id)) view.expanded.delete(id);
      else view.expanded.add(id);
      renderTree();
      return;
    }
    if (button.hasAttribute("data-render-more")) {
      view.shown += 200;
      renderTree();
      return;
    }
    if (button.hasAttribute("data-new")) {
      edit({ parent: view.selected });
      return;
    }
    if (button.hasAttribute("data-undo")) {
      await action(button, undo);
      return;
    }
    if (button.hasAttribute("data-more")) {
      view.limit = Math.min(100000, view.limit * 2);
      await load();
      return;
    }
    if (button.hasAttribute("data-clear-search")) {
      view.filter = "";
      view.searchCaption = "";
      $("[data-search]", root).value = "";
      button.hidden = true;
      renderTree();
      return;
    }
    if (button.hasAttribute("data-search-submit")) {
      await action(button, search);
      return;
    }
    if (button.hasAttribute("data-merge")) {
      await action(button, async () => {
        const chosen = [];
        for (const id of checked)
          chosen.push(entry(await execute("memory_show", { id })));
        edit({
          memory: {
            ...chosen[0],
            content: chosen
              .map((item) => `## ${item.title}\n\n${item.content}`)
              .join("\n\n"),
          },
          merge: chosen.map((item) => item.id),
        });
      });
      return;
    }
    const memory = view.selectedEntry;
    if (!memory) return;
    if (button.hasAttribute("data-edit")) edit({ memory });
    if (button.hasAttribute("data-child")) edit({ parent: memory.id });
    if (button.hasAttribute("data-copy"))
      await action(button, async () => {
        await navigator.clipboard.writeText(memory.content || "");
        notify(i18nMessages.m3400ddb2de4c4734);
      });
    if (button.hasAttribute("data-move"))
      dialog({
        title: i18nMessages.m866509cb5f6c4f7e,
        body: input("parent", i18nMessages.m5ec562e460e2e6ed, memory.parent_id || "", {
          required: true,
        }),
        onSubmit: async (value) => {
          await execute("memory_attach", {
            id: memory.id,
            parent: value.parent.trim(),
          });
          await recordUndo({ kind: "move", id: memory.id, before: memory });
          notify(i18nMessages.md0dcf9188900e96c);
          await load();
        },
      });
    if (button.hasAttribute("data-share"))
      await action(button, async () => {
        const result = await execute("memory_share", { root: memory.id });
        const text = result.details?.payload;
        if (!text) throw new Error(i18nMessages.m8e7f971e2ad729e8);
        showText(i18nMessages.m5d2306f08373168d, text);
      });
    if (button.hasAttribute("data-chain"))
      await action(button, async () => {
        const result = await execute("memory_chain", {
          id: memory.id,
          depth: 3,
        });
        const detail = result.details || {};
        const sequence = [
          ...(detail.ancestors || []),
          detail.entry,
          ...(detail.descendants || []),
        ]
          .filter(Boolean)
          .map((item) => item.entry || item);
        showText(
          i18nMessages.m0a6db930231e13f7,
          sequence
            .map((item) => `${item.title || item.id}\n\n${item.content || ""}`)
            .join("\n\n────────\n\n"),
        );
      });
    if (button.hasAttribute("data-delete"))
      await action(button, async () => {
        if (
          !(await confirm(
            i18nMessages.m34b094b2e6408aec,
            `${i18nMessages.mfbc9178f86b779b7}${memory.title}${i18nMessages.m9444660dfa344579}`,
            i18nMessages.m2f9daa828907b93f,
          ))
        )
          return;
        await execute("memory_delete", { id: memory.id });
        await recordUndo({ kind: "delete", id: memory.id, before: memory });
        view.selected = "";
        view.selectedEntry = null;
        view.selectionVersion++;
        checked.delete(memory.id);
        reader.innerHTML = `${i18nMessages.m2663823feee03ae9}${h(memory.id)}${i18nMessages.mc1946bfd65cf837b}`;
        $("[data-restore]", reader).addEventListener("click", (event) =>
          action(event.currentTarget, async () => {
            await execute("memory_restore", { id: memory.id });
            view.selected = memory.id;
            await load();
            await ctx.refresh();
          }),
        );
        await load();
        await ctx.refresh();
      });
  });
  root.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      const technical = $(".technical.fullscreen", reader);
      if (technical) {
        technical.classList.remove("fullscreen");
        const techButton = $("[data-tech-full]", technical);
        if (techButton) techButton.textContent = t(i18nMessages.mf716596a20b085b0);
      }
      if (root.classList.contains("focus-mode")) setFocusMode(false);
      return;
    }
    if (event.key !== "Enter" && event.key !== " ") return;
    const card = event.target.closest?.("[data-card]");
    if (card) {
      event.preventDefault();
      choose(card.dataset.card);
    }
  });
}
