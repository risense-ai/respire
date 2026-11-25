import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { t } from "../ui/i18n.js";
import {
  $,
  h,
  execute,
  action,
  notify,
  panel,
  raw,
  input,
  dialog,
  showText,
  rows,
  number,
} from "../ui/core.js";
import { heading } from "../pages/overview.js";
import { entry, list } from "./model.js";

const pad = (value) => String(value).padStart(2, "0");
export const localCalendarDay = (date) =>
  `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;

function validDay(value) {
  if (typeof value !== "string" || !/^\d{4}-\d{2}-\d{2}$/.test(value)) return false;
  const utc = new Date(value + "T00:00:00Z");
  return !Number.isNaN(utc.getTime()) && utc.toISOString().slice(0, 10) === value;
}

function localTimestamp(value) {
  if (typeof value !== "string" || !/^\d{4}-\d{2}-\d{2}T.+(?:Z|[+-]\d{2}:\d{2})$/.test(value)) return null;
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? null : date;
}

function entryDay(item) {
  if (validDay(item.day)) return item.day;
  const titleDay = String(item.title || "").match(/\d{4}-\d{2}-\d{2}/)?.[0];
  if (validDay(titleDay)) return titleDay;
  const timestamp = localTimestamp(item.created_at);
  return timestamp ? localCalendarDay(timestamp) : String(item.created_at || "").slice(0, 10);
}

function entryTime(item) {
  const timestamp = localTimestamp(item.created_at);
  if (!timestamp) return String(item.created_at || "").slice(11, 16);
  return `${pad(timestamp.getHours())}:${pad(timestamp.getMinutes())}`;
}
export function diary(root, ctx) {
  const now = new Date();
  let month = `${now.getFullYear()}-${pad(now.getMonth() + 1)}`,
    items = [],
    loading = false;
  root.innerHTML =
    heading(i18nMessages.mbea4df8bd3d493f7, i18nMessages.mdc3ad8c792737ec8) +
    `${i18nMessages.m7db7bbf75e1f986c}${month}${i18nMessages.mf4d6d28744006586}`;
  function render() {
    const [year, m] = month.split("-").map(Number),
      first = new Date(year, m - 1, 1),
      lead = (first.getDay() + 6) % 7,
      total = new Date(year, m, 0).getDate();
    const groups = new Map();
    for (const item of items) {
      const day = entryDay(item);
      if (!groups.has(day)) groups.set(day, []);
      groups.get(day).push(item);
    }
    $("[data-calendar]", root).innerHTML =
      [i18nMessages.m51a75f4634dfa859, i18nMessages.m084b42f6e95e4e65, i18nMessages.ma4c3313deb180960, i18nMessages.m754a9d5828d502f1, i18nMessages.mc9b87f516a38ea36, i18nMessages.mde07b5383874b2a3, i18nMessages.m85217f7aff778414]
        .map((day) => `<span class="calendar-weekday">${day}</span>`)
        .join("") +
      Array.from(
        { length: lead },
        () => '<span class="calendar-blank"></span>',
      ).join("") +
      Array.from({ length: total }, (_, i) => {
        const day = `${month}-${pad(i + 1)}`,
          count = groups.get(day)?.length || 0;
        return `<button class="calendar-day ${count ? "has-entries" : ""} ${day === localCalendarDay(now) ? "today" : ""}" data-day="${day}"><span>${i + 1}</span>${count ? `<small>${h(t(count === 1 ? i18nMessages.diaryEntryCountOne : i18nMessages.diaryEntryCountMany).replace("{count}", number(count)))}</small>` : ""}</button>`;
      }).join("");
    function select(day) {
      $("[data-day-title]", root).textContent = day;
      const entries = groups.get(day) || [];
      $("[data-entries]", root).className = "";
      $("[data-entries]", root).innerHTML = entries.length
        ? `<ol class="timeline">${entries
            .map((item) => {
              const time = entryTime(item);
              return `<li class="timeline-item"><span class="timeline-dot" aria-hidden="true"></span><div class="timeline-body"><button data-entry="${h(item.id)}">${h(item.title)}</button>${time ? `<small>${h(time)}</small>` : ""}</div></li>`;
            })
            .join("")}</ol>`
        : i18nMessages.m3e9f5615f968cbf2;
    }
    $("[data-calendar]", root).onclick = (event) => {
      const button = event.target.closest("[data-day]");
      if (button) select(button.dataset.day);
    };
    if (month === localCalendarDay(now).slice(0, 7)) select(localCalendarDay(now));
  }
  async function load() {
    if (loading) return;
    loading = true;
    try {
      const [year, m] = month.split("-").map(Number),
        last = new Date(year, m, 0).getDate();
      const result = await execute("diary_list", {
        from: month + "-01",
        to: `${month}-${pad(last)}`,
      });
      if (!root.isConnected) return;
      items = list(result);
      render();
    } catch (error) {
      if (root.isConnected)
        $("[data-entries]", root).innerHTML =
          `<div class="empty danger-text">${h(error.message)}</div>`;
    } finally {
      loading = false;
    }
  }
  root.addEventListener("click", (event) => {
    const button = event.target.closest("button");
    if (!button) return;
    if (
      button.hasAttribute("data-prev") ||
      button.hasAttribute("data-next") ||
      button.hasAttribute("data-today")
    ) {
      if (button.hasAttribute("data-today")) month = dayOf(now).slice(0, 7);
      else {
        const [year, m] = month.split("-").map(Number);
        const next = new Date(
          year,
          m - 1 + (button.hasAttribute("data-next") ? 1 : -1),
          1,
        );
        month = `${next.getFullYear()}-${pad(next.getMonth() + 1)}`;
      }
      $("[name=month]", root).value = month;
      load();
    }
    if (button.dataset.entry) {
      // Persistent selection highlight, matching the memory tree rows.
      root
        .querySelectorAll(".timeline-body button.active")
        .forEach((el) => el.classList.remove("active"));
      button.classList.add("active");
      action(button, async () => {
        const memory = entry(
          await execute("memory_show", { id: button.dataset.entry }),
        );
        showText(memory.title, memory.content || "");
      });
    }
    if (button.hasAttribute("data-search"))
      dialog({
        title: i18nMessages.m8d2331f8248c6942,
        body:
          input("query", i18nMessages.mf7153322f4df0d31, "", { required: true }) +
          "<div data-hits></div>",
        submit: i18nMessages.m44ce7ae909bbb28b,
        wide: true,
        onSubmit: async (values, element) => {
          const result = await execute("diary_search", {
            query: values.query,
            limit: 20,
            max_chars: 8000,
            mode: "hybrid",
          });
          const hits = rows(result, "fragments");
          $("[data-hits]", element).innerHTML = hits.length
            ? hits
                .map(
                  (hit) =>
                    `<div class="suggestion-card"><div class="row-copy"><strong>${h(hit.title || hit.source_id)}</strong><p>${h(hit.text || hit.content || hit.snippet || "")}</p><small>${h(hit.source_id)} / ${h(hit.fragment_id)} / ${h(hit.source_version)}</small></div><button type="button" data-source="${h(hit.source_id)}" data-fragment="${h(hit.fragment_id)}${i18nMessages.me8b59ef6b5785d56}`,
                )
                .join("")
            : `<pre class="code">${h(raw(result))}</pre>`;
          $("[data-hits]", element).onclick = (event) => {
            const control = event.target.closest("[data-fragment]");
            if (control)
              action(control, async () => {
                const fragment = await execute("diary_fragment", {
                  source_id: control.dataset.source,
                  fragment_id: control.dataset.fragment,
                  neighbors: 1,
                  max_chars: 8000,
                });
                showText(i18nMessages.m9395e1a9990a936d, raw(fragment.details || fragment.summary));
              });
          };
          return false;
        },
      });
  });
  $("[name=month]", root).addEventListener("change", (event) => {
    if (event.target.value) {
      month = event.target.value;
      load();
    }
  });
  render();
  if (ctx.desktop?.connected) load();
  let attempted = Boolean(ctx.desktop?.connected);
  return {
    refresh: load,
    update() {
      if (!attempted && ctx.desktop?.connected) {
        attempted = true;
        load();
      }
    },
  };
}
