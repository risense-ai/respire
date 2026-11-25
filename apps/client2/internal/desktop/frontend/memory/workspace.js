import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { $, all, h, number, date, raw, execute, notify, rows, summary } from "../ui/core.js";
import { t } from "../ui/i18n.js";
import { heading } from "../pages/overview.js";
import { entry, list, inventory, kindNames } from "./model.js";
import { editor } from "./editor.js";
import { renderGraph } from "./graph.js";
import { recordUndo, undoLast } from "./undo.js";
import { memoryInteractions } from "./interactions.js";
import { memoryTools } from "./tools.js";
import { buildIndex, visibleRows, searchForceIds } from "./tree-model.js";

import { bindWorkspaceEvents } from "./workspace-events.js";

const VIEW_MODES = [
  ["tree", i18nMessages.mc0ae67d44a1fcef3],
  ["list", i18nMessages.maedd6814ff8c516c],
  ["card", i18nMessages.mfb5640f8e12e3337],
  ["graph", i18nMessages.m3b51eaa934172f3e],
];
// Rows per page for the list and card pagers, matching the console dashboard.
const PAGE_SIZE = 24;

const kindLabel = (row) =>
  kindNames[String(row.kind || "context").toLowerCase()] || row.kind;
const whenText = (row) => date(row.updated_at || row.created_at);
const tagList = (row) =>
  Array.isArray(row.tags) ? row.tags.filter(Boolean) : [];

export function memoryWorkspace(root, ctx) {
  const view = {
    selected: ctx.params?.get("node") || "",
    selectedEntry: null,
    expanded: new Set(),
    filter: "",
    mode: "tree",
    limit: 500,
    shown: 200,
    listPage: 1,
    selectionVersion: 0,
    searchCaption: "",
  };
  let records = [],
    renderedEntry = null,
    busy = false,
    revision = "",
    autoReloadAt = 0,
    loaded = false;
  let metadataCap = null;
  const checked = new Set();
  const graphState = {};
  root.innerHTML =
    heading(
      i18nMessages.me90bb30ab1d6dc46,
      i18nMessages.mf58e51cd064e19f1,
      `${i18nMessages.m9f2f6c61c1e2c800}`,
    ) +
    `${i18nMessages.m66060b89d8e94379}${VIEW_MODES.map(
      ([value, label]) =>
        `<button role="tab" data-view="${value}" aria-selected="${value === view.mode}" class="${value === view.mode ? "active" : ""}">${label}</button>`,
    ).join("")}${i18nMessages.m359f7c410726caa6}`;
  const tree = $(".memory-index", root),
    reader = $(".memory-reader", root),
    count = $("[data-count]", root);
  // Draggable split between the tree index and the reader; width persists in
  // preferences (0 keeps the proportional default).
  const INDEX_MIN = 185,
    INDEX_MAX = 640;
  const savedIndexWidth = Number(ctx.desktop?.preferences?.memoryIndexWidth) || 0;
  if (savedIndexWidth >= INDEX_MIN)
    document.documentElement.style.setProperty(
      "--memory-index-w",
      `${savedIndexWidth}px`,
    );
  $("#memory-resizer", root).addEventListener("mousedown", (event) => {
    if (root.classList.contains("graph-mode")) return;
    event.preventDefault();
    document.body.classList.add("resizing");
    const apply = (clientX) => {
      const layout = $(".memory-layout", root).getBoundingClientRect();
      const width = Math.round(
        Math.min(INDEX_MAX, Math.max(INDEX_MIN, clientX - layout.left)),
      );
      document.documentElement.style.setProperty(
        "--memory-index-w",
        `${width}px`,
      );
      return width;
    };
    const move = (e) => apply(e.clientX);
    const up = (e) => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      document.body.classList.remove("resizing");
      const width = apply(e.clientX);
      ctx
        .savePreferences({ memoryIndexWidth: width })
        .catch((error) => notify(error.message, true));
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  });
  const undoButton = document.createElement("button");
  undoButton.textContent = i18nMessages.m926a50b98ece2667;
  undoButton.dataset.undo = "";
  undoButton.title = i18nMessages.m852c9aa0b1ce9c14;
  $(".memory-toolbar", root).append(undoButton);
  // Focus mode: the tree and reader fill the window until toggled back.
  function setFocusMode(on) {
    root.classList.toggle("focus-mode", on);
    document.body.classList.toggle("memory-focus", on);
    const button = $("[data-focus]", root);
    button.title = on ? i18nMessages.m84759b9df5d45e50 : i18nMessages.mf716596a20b085b0;
    button.setAttribute("aria-label", button.title);
    button.setAttribute("aria-pressed", String(on));
  }
  $("[data-focus]", root).addEventListener("click", () =>
    setFocusMode(root.classList.contains("focus-mode") ? false : true),
  );
  const textMatch = (row) =>
    `${row.title || ""} ${row.id}`.toLowerCase().includes(view.filter.trim().toLowerCase());
  const paged = (values) => {
    const pages = Math.max(1, Math.ceil(values.length / PAGE_SIZE));
    view.listPage = Math.min(Math.max(1, view.listPage), pages);
    return {
      slice: values.slice((view.listPage - 1) * PAGE_SIZE, view.listPage * PAGE_SIZE),
      pages,
      page: view.listPage,
      total: values.length,
    };
  };
  const pager = ({ pages, page, total }) =>
    pages > 1
      ? `${i18nMessages.m2dea68e7914bcefc}${page <= 1 ? "disabled" : ""}${i18nMessages.mb357d3f35f3f602a}${page} / ${pages}${i18nMessages.mf5080bf79e103974}${number(total)}${i18nMessages.m8c490d8c5399060c}${page >= pages ? "disabled" : ""}${i18nMessages.me6b26c001247439b}`
      : "";
  const memoryRow = ({ row, depth = 0, kids = 0, open = false }, folder) =>
    `<div class="memory-row ${row.id === view.selected ? "active" : ""}${checked.has(row.id) ? " selected" : ""}" data-id="${h(row.id)}" data-level="${Math.min(depth, 8)}" role="treeitem" aria-selected="${checked.has(row.id) || row.id === view.selected}" ${kids ? `aria-expanded="${Boolean(open)}"` : ""}><button class="tree-toggle" data-expand="${h(row.id)}" ${kids ? "" : "disabled"}${i18nMessages.m144fe995b239315c}${kids ? (open ? "▾" : "▸") : "·"}</button><button class="memory-title" data-select="${h(row.id)}" title="${h(row.title)}">${h(row.title || row.id.slice(0, 8))}</button></div>`;
  const listRow = (row) =>
    `<div class="memory-row list-row ${row.id === view.selected ? "active" : ""}${checked.has(row.id) ? " selected" : ""}" data-id="${h(row.id)}" role="row"><span class="memory-cell cell-kind"><span class="badge">${h(kindLabel(row))}</span></span><button class="memory-title cell-title" data-select="${h(row.id)}" title="${h(row.title)}">${h(row.title || row.id.slice(0, 8))}</button><span class="memory-cell cell-project">${h(row.project || "—")}</span><span class="memory-cell cell-updated">${h(whenText(row))}</span><span class="memory-cell cell-tags">${h(tagList(row).join(", ") || "—")}</span></div>`;
  const memoryCard = (row) => {
    const tags = tagList(row);
    return `<div class="memory-card ${row.id === view.selected ? "active" : ""}${checked.has(row.id) ? " selected" : ""}" data-id="${h(row.id)}" data-card="${h(row.id)}" data-select="${h(row.id)}" data-kind="${h(String(row.kind || "context").toLowerCase())}" tabindex="0" role="button" aria-selected="${row.id === view.selected || checked.has(row.id)}"><span class="badge">${h(kindLabel(row))}</span><strong>${h(row.title || row.id.slice(0, 8))}</strong>${
      row.content ? `<p>${h(String(row.content).replace(/\s+/g, " ").slice(0, 140))}</p>` : ""
    }<small><span>${h(row.project || "—")}</span><span>${h(whenText(row))}</span>${
      tags.length ? `<span class="hit-tags">${h(tags.join(", "))}</span>` : ""
    }</small></div>`;
  };
  function renderTree(searchRows) {
    const keptScroll = tree.scrollTop;
    // The catalog filter panel is gone; keep the previous default order
    // (most recently updated first) for siblings in every view.
    const filtered = (searchRows || records)
      .slice()
      .sort((a, b) =>
        String(b.updated_at || b.created_at || "").localeCompare(
          String(a.updated_at || a.created_at || ""),
        ),
      );
    root.classList.toggle("graph-mode", view.mode === "graph");
    root.classList.toggle("wide-mode", view.mode === "list" || view.mode === "card");
    tree.classList.toggle("graph-shell", view.mode === "graph");
    all("[data-view]", root).forEach((tab) => {
      const active = tab.dataset.view === view.mode;
      tab.classList.toggle("active", active);
      tab.setAttribute("aria-selected", String(active));
    });
    if (view.mode === "graph") {
      const values = filtered.filter(textMatch);
      renderGraph(tree, values, view.selected, graphState, (id) => choose(id));
      count.textContent = searchRows
        ? `${i18nMessages.m9d8a18d58e2290a5}${values.length}${i18nMessages.m060f56edfbc0cfb9}`
        : `${i18nMessages.m73d20904db482e59}${number(records.length)} / ${number(ctx.status?.summary?.local_alive)}${i18nMessages.m7ceb84239779a93c}${checked.size}${i18nMessages.m6fb2840d568a340f}`;
      $("[data-merge]", root).disabled = checked.size < 2 || busy;
      $("[data-more]", root).disabled =
        busy ||
        records.length >= Number(ctx.status?.summary?.local_alive || Infinity);
      return;
    }
    if (view.mode === "card") {
      const values = searchRows ? filtered : filtered.filter(textMatch);
      const view = paged(values);
      tree.innerHTML = `<div class="memory-cards">${view.slice.map(memoryCard).join("") || (values.length ? "" : i18nMessages.m51372c6197a81298)}</div>${searchRows ? "" : pager(view)}`;
      finish();
      return;
    }
    if (view.mode === "list") {
      const values = searchRows ? filtered : filtered.filter(textMatch);
      const view = paged(values);
      tree.innerHTML = `${i18nMessages.m080b48cd4814eea8}${view.slice.map(listRow).join("")}</div>${searchRows ? "" : pager(view)}`;
      if (!values.length)
        tree.innerHTML = i18nMessages.m51372c6197a81298;
      finish();
      return;
    }
    // Tree view: the console's model — trivial (diary) rows group on the diary
    // page, empty leaves and pure category notes hide, search keeps matches
    // and their ancestors visible with every branch opened.
    const needle = searchRows ? "" : view.filter.trim();
    let source = filtered;
    if (needle && !searchRows) {
      const keep = searchForceIds(source, needle.toLowerCase());
      source = source.filter((row) => keep.has(row.id));
    }
    const index = buildIndex(source);
    index.forceIds = needle ? searchForceIds(source, needle.toLowerCase()) : new Set();
    const visible = searchRows
      ? filtered.map((row) => ({ row, depth: 0, kids: 0, open: false }))
      : visibleRows(index, view.expanded, Boolean(needle));
    tree.innerHTML =
      visible
        .slice(0, view.shown)
        .map((item) =>
          memoryRow(item, item.row.kind === "folder" || item.kids > 0),
        )
        .join("") +
      (visible.length > view.shown
        ? `${i18nMessages.mcd29944c73d8eb80}${Math.min(200, visible.length - view.shown)}${i18nMessages.m08428039112060ff}`
        : "");
    if (!visible.length)
      tree.innerHTML = i18nMessages.m51372c6197a81298;
    all(".memory-row", tree).forEach((node) => (node.draggable = true));
    tree.scrollTop = keptScroll;
    finish();
  }
  function finish() {
    all(".memory-row", tree).forEach((node) => (node.draggable = true));
    count.textContent =
      view.searchCaption ||
      `${i18nMessages.m73d20904db482e59}${number(records.length)} / ${number(ctx.status?.summary?.local_alive)}${i18nMessages.m7ceb84239779a93c}${checked.size}${i18nMessages.m6fb2840d568a340f}`;
    $("[data-merge]", root).disabled = checked.size < 2 || busy;
    $("[data-more]", root).disabled =
      busy ||
      records.length >= Number(ctx.status?.summary?.local_alive || Infinity);
  }
  function locked() {
    reader.innerHTML = `${i18nMessages.m312ef7a0d4574023}`;
    $("[data-accounts]", reader).addEventListener("click", () =>
      ctx.navigate("accounts"),
    );
    count.textContent = ctx.desktop?.connected
      ? i18nMessages.m1e5c888106021aea
      : i18nMessages.m98894a7442c1d837;
  }
  async function load() {
    if (busy) return;
    if (!ctx.desktop?.connected || ctx.status?.summary?.unlocked === false) {
      locked();
      return;
    }
    busy = true;
    let succeeded = false;
    count.textContent = i18nMessages.m019fb208a51f0a57;
    try {
      if (metadataCap === null) {
        try {
          metadataCap =
            summary(await execute("capabilities")).capabilities
              ?.memory_list_metadata === true;
        } catch (error) {
          if (error.envelope?.summary?.reason !== "invalid_input")
            throw error;
          metadataCap = false;
          notify(
            i18nMessages.mb5398f3abaa7620c,
          );
        }
      }
      const data = await execute("memory_list", {
        limit: view.limit,
        ...(metadataCap ? { metadata: true } : {}),
      });
      if (!root.isConnected) return;
      // The compact catalog omits bodies; the paginated catalog provides them
      // and they feed card excerpts and text matching until the next reload.
      records = list(data).map((row) =>
        metadataCap ? { ...row, content: undefined } : row,
      );
      // A paginated catalog flattens subtrees whose parent is not loaded yet:
      // buildIndex drops orphans to the top level. The metadata edition is
      // cheap, so top it up until the whole library is in memory.
      if (metadataCap) {
        const alive = Number(ctx.status?.summary?.local_alive) || 0;
        let fetched = view.limit;
        while (
          alive &&
          records.length < alive &&
          records.length >= fetched / 2 &&
          fetched < 100000
        ) {
          count.textContent = `${i18nMessages.m73d20904db482e59}${number(records.length)} / ${number(alive)}${i18nMessages.m816a7653e1650437}`;
          fetched = Math.min(fetched * 2, 100000);
          const more = await execute("memory_list", {
            limit: fetched,
            metadata: true,
          });
          if (!root.isConnected) return;
          const rows = list(more);
          if (rows.length <= records.length) break; // server has no more
          records = rows.map((row) => ({ ...row, content: undefined }));
        }
        view.limit = Math.max(view.limit, fetched);
      }
      loaded = true;
      succeeded = true;
      // A reader still showing the pre-unlock lock screen resets to the idle hint.
      if (!view.selected && !reader.querySelector(".reader-heading"))
        reader.innerHTML =
          i18nMessages.m87ed639abd0b2b1b;
      if (!view.expanded.size)
        records
          .filter((row) => !row.parent_id)
          .slice(0, 15)
          .forEach((row) => view.expanded.add(row.id));
      renderTree();
      if (view.selected) await choose(view.selected, false);
    } catch (error) {
      if (root.isConnected) {
        count.textContent = i18nMessages.m6aa27f14bff16faa;
        tree.innerHTML = `<div class="empty danger-text">${h(error.message)}</div>`;
      }
    } finally {
      busy = false;
      if (root.isConnected && succeeded) renderTree();
    }
  }
  // Body records carry literal Cause/Action/Result markers; render each
  // marker section as its own labelled block instead of one dense paragraph.
  // Body records carry literal Cause/Action/Result markers (Chinese
  // legacy) or the English Cause/Action/Result set the current CLI
  // writes; render each marker section as its own labelled block.
  const CONTENT_MARKERS =
    new RegExp(i18nMessages.m362a58a9e5ebe418,"g");
  function readingBlocks(text) {
    const source = String(text || "");
    CONTENT_MARKERS.lastIndex = 0;
    const marks = [...source.matchAll(CONTENT_MARKERS)];
    if (!marks.length) return "";
    const sections = [];
    const intro = source.slice(0, marks[0].index).trim();
    if (intro) sections.push({ label: "", body: intro });
    marks.forEach(([marker], i) => {
      const end = i + 1 < marks.length ? marks[i + 1].index : source.length;
      const body = source.slice(marks[i].index + marker.length, end).trim();
      if (body) sections.push({ label: marker.slice(1, -1), body });
    });
    return sections
      .map(
        (section) =>
          `<section class="content-block${section.label ? "" : " plain"}">${section.label ? `<h4>${h(section.label)}</h4>` : ""}<p>${h(section.body)}</p></section>`,
      )
      .join("");
  }
  function renderReader(memory) {
    const metadata = inventory(records),
      parent = metadata.byId.get(memory.parent_id),
      children = metadata.children.get(memory.id) || [];
    reader.innerHTML = `<div class="reader-heading"><div class="reader-path">${h(parent?.title || memory.parent_id || i18nMessages.md003475c60afffe8)}</div><h2>${h(memory.title)}</h2><div class="reader-meta"><span>${h(kindNames[String(memory.kind || "context").toLowerCase()] || memory.kind)}</span><span>${memory.importance === "trivial" ? i18nMessages.mfee683550df02f68 : i18nMessages.mf1beaee625c0847a}</span><span>${h(date(memory.updated_at || memory.created_at))}${i18nMessages.m9f89f8c146d7463d}${readingBlocks(memory.content) || h(memory.content || "")}${i18nMessages.mb1e8c7ae8b7d1b02}${parent ? `<button data-select="${h(parent.id)}">↑ ${h(parent.title)}</button>` : ""}${children.map((id) => `<button data-select="${h(id)}">↓ ${h(metadata.byId.get(id)?.title || id)}</button>`).join("")}${!parent && !children.length ? i18nMessages.m1fa6921d6157370c : ""}${i18nMessages.m85a10fd7b4ca4ec1}${h(raw({ ...memory, content: undefined }))}</pre></details>`;
    const technical = $(".technical", reader);
    const techButton = $("[data-tech-full]", technical);
    techButton.addEventListener("click", () => {
      const on = technical.classList.toggle("fullscreen");
      if (on) technical.open = true;
      techButton.textContent = on ? t(i18nMessages.m84759b9df5d45e50) : t(i18nMessages.mf716596a20b085b0);
    });
  }
  async function choose(id, scroll = true) {
    const byId = inventory(records).byId,
      seen = new Set();
    let ancestor = byId.get(id);
    while (ancestor?.parent_id && !seen.has(ancestor.parent_id)) {
      seen.add(ancestor.parent_id);
      view.expanded.add(ancestor.parent_id);
      ancestor = byId.get(ancestor.parent_id);
    }
    view.selected = id;
    view.selectedEntry = null;
    const version = ++view.selectionVersion;
    renderTree();
    // A background refresh of the visible record must not flash a loading
    // placeholder: keep the current content until the fresh copy differs.
    const viewing = renderedEntry?.id === id;
    const previous = renderedEntry;
    if (!viewing)
      reader.innerHTML = i18nMessages.mc0caa9014ca6c328;
    try {
      const memory = entry(await execute("memory_show", { id }));
      if (!root.isConnected || version !== view.selectionVersion) return;
      view.selectedEntry = memory;
      if (!(viewing && previous.updated_at === memory.updated_at)) {
        renderReader(memory);
        const listed = byId.get(id);
        $(".reader-meta", reader).insertAdjacentHTML(
          "beforeend",
          `${i18nMessages.m36fbf08e33779d3c}${h(listed?.recall_count ?? "—")}${i18nMessages.md4e2e4060664572d}${h(memory.project || i18nMessages.mec727a333db7fdd4)}</span><span>${h((memory.tags || []).join(" · "))}</span>`,
        );
        memoryTools(reader, memory, {
          changed: async () => {
            view.selected = "";
            view.selectedEntry = null;
            view.selectionVersion++;
            reader.innerHTML =
              i18nMessages.m39914445289df1a3;
            await load();
            await ctx.refresh();
          },
        });
        if (scroll) reader.scrollTop = 0;
      }
      renderedEntry = memory;
    } catch (error) {
      if (root.isConnected && version === view.selectionVersion)
        reader.innerHTML = `<div class="empty danger-text">${h(error.message)}</div>`;
    }
  }
  async function search() {
    const query = $("[data-search]", root).value.trim();
    if (!query) {
      view.filter = "";
      view.searchCaption = "";
      $("[data-clear-search]", root).hidden = true;
      renderTree();
      return;
    }
    count.textContent = i18nMessages.md959d76a6f0278e7;
    try {
      const result = await execute("memory_search", { query, limit: 50 });
      if (!root.isConnected) return;
      const matches = rows(result)
        .map((value) => {
          try {
            return entry(value);
          } catch {
            return null;
          }
        })
        .filter(Boolean);
      view.searchCaption = `${i18nMessages.m9d8a18d58e2290a5}${matches.length}${i18nMessages.m060f56edfbc0cfb9}`;
      renderTree(matches);
      $("[data-clear-search]", root).hidden = false;
    } catch (error) {
      notify(error.message, true);
    }
  }
  async function undo() {
    view.selected = await undoLast();
    view.selectedEntry = null;
    view.selectionVersion++;
    if (!view.selected)
      reader.innerHTML =
        i18nMessages.md3b036467d3d121a;
    await load();
    await ctx.refresh();
    notify(i18nMessages.mfef96e34f77b742b);
  }
  function edit(options = {}) {
    editor({
      ...options,
      onSaved: async (id, undo) => {
        if (undo) await recordUndo({ ...undo, id });
        if (id) view.selected = id;
        await load();
        await ctx.refresh();
      },
    });
  }
  root.addEventListener("input", (event) => {
    if (event.target.matches("[data-search]")) {
      view.filter = event.target.value;
      view.shown = 200;
      view.listPage = 1;
      view.searchCaption = "";
      renderTree();
    }
  });
  $("[data-search]", root).addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      search();
    }
  });
  bindWorkspaceEvents(view, {
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
  });
  function update() {
    const s = ctx.status?.summary;
    if (!s?.unlocked) {
      if (!loaded) locked();
      return;
    }
    if (document.querySelector("dialog[open]")) return;
    const stamp = `${s.data_dir}/${s.local_total}/${s.max_updated_at}`;
    if (stamp !== revision && !busy) {
      revision = stamp;
      load();
      return;
    }
    // Final-consistency fallback: cloud sync lag or a missed stamp must not
    // freeze the catalog — quietly re-read it every 30s like the dashboard.
    const now = Date.now();
    if (!busy && now >= autoReloadAt) {
      autoReloadAt = now + 30000;
      load();
    }
  }
  update();
  const dispose = memoryInteractions(root, {
    getRecords: () => records,
    getSelected: () => view.selected,
    select: choose,
    search: () => $("[data-search]", root).focus(),
    newMemory: (subtree) => edit({ parent: subtree ? view.selected : "" }),
    edit: () => {
      if (view.selectedEntry) edit({ memory: view.selectedEntry });
    },
    remove: () => $("[data-delete]", reader)?.click(),
    undo,
    moved: async (before) => {
      await recordUndo({ kind: "move", id: before.id, before });
      view.selected = before.id;
      await load();
      await ctx.refresh();
      notify(i18nMessages.md0dcf9188900e96c);
    },
  });
  return {
    update,
    refresh: load,
    dispose: () => {
      dispose();
      document.body.classList.remove("memory-focus");
      if (graphState.raf) cancelAnimationFrame(graphState.raf);
      graphState.raf = 0;
      graphState.dom = null;
    },
  };
}
