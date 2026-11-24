import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { $, h, number, panel, metric, execute } from "../ui/core.js";

function rate(hits, recalls) {
  if (!recalls) return i18nMessages.m301eb7291c894e38;
  return `${Math.round((hits / recalls) * 100)}${i18nMessages.m9460f4ab7c37ea9c}`;
}

function localDay() {
  const now = new Date();
  const month = String(now.getMonth() + 1).padStart(2, "0");
  const day = String(now.getDate()).padStart(2, "0");
  return `${now.getFullYear()}-${month}-${day}`;
}

export function stats(root, ctx) {
  root.innerHTML =
    `${i18nMessages.m88f60133e9cba11a}` +
    `${i18nMessages.m0d12fbc4bf4fa869}`;
  async function load() {
    const body = $("[data-body]", root);
    if (!ctx.desktop?.connected) {
      body.innerHTML = `${i18nMessages.m7ebcf629491bd81d}`;
      return;
    }
    try {
      const envelope = await execute("query_log", { stats: true, limit: 1 });
      if (!root.isConnected) return;
      const details = envelope?.details || {};
      const daily = Array.isArray(details.daily) ? details.daily : [];
      const today = daily.find((row) => row.day === localDay()) || {
        day: localDay(),
        recalls: 0,
        hits: 0,
        adopted: 0,
        good: 0,
      };
      const rows = [...daily].reverse();
      body.innerHTML =
        `<div class="metrics">${metric(i18nMessages.mfc40a227e904f869, number(today.recalls), today.day)}${metric(
          i18nMessages.ma86fba1fab962dad,
          number(today.hits),
          rate(today.hits, today.recalls),
        )}${metric(i18nMessages.m1ff47789e9e07a55, number(details.total), `${i18nMessages.md4aa24a729178927}${number(details.empty_candidates)}`)}${metric(
          i18nMessages.m7ac21a786368d63f,
          number(details.with_adopted),
          `${i18nMessages.m0fc6b42f8031ea9e}${number(details.good_total)}`,
        )}</div>` +
        panel(
          i18nMessages.mc5f1c28c0d5f061d,
          rows.length
            ? `${i18nMessages.m21d8457c18e9d4ce}${rows
                .map(
                  (row) =>
                    `<tr><td>${h(row.day)}</td><td>${h(number(row.recalls))}</td><td>${h(number(row.hits))}</td><td>${h(number(row.adopted))}</td><td>${h(number(row.good))}</td></tr>`,
                )
                .join("")}</tbody></table></div>`
            : `${i18nMessages.m59e5cf8d49cfa93d}`,
        );
    } catch (error) {
      if (!root.isConnected) return;
      body.innerHTML = `<div class="panel empty">${h(error.message || i18nMessages.m6aa27f14bff16faa)}</div>`;
    }
  }
  $("[data-reload]", root).onclick = () => load();
  let connected = Boolean(ctx.desktop?.connected);
  load();
  return {
    refresh: load,
    update() {
      const next = Boolean(ctx.desktop?.connected);
      if (next === connected) return;
      connected = next;
      load();
    },
  };
}
