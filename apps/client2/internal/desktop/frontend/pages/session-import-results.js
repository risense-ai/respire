import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { $, all, h, number, summary, metric, rows } from "../ui/core.js";

const sessionLine = (session) => {
  const parts = [];
  if (session.updated_at || session.created_at)
    parts.push(session.updated_at || session.created_at);
  if (session.cwd) parts.push(session.cwd);
  if (session.model || session.provider)
    parts.push(session.model || session.provider);
  return parts.join(" · ");
};

export function renderImportPreview(root, preview, previewBusy) {
  const note = $("[data-preview-note]", root);
  if (note)
    note.textContent = previewBusy
      ? i18nMessages.m515c22d6e9710a7d
      : preview?.stale
        ? preview.note || i18nMessages.m561d90de65290694
        : "";
  const area = $("[data-preview-result]", root);
  if (!area || !root.isConnected) return;
  if (!preview) {
    area.innerHTML = "";
    return;
  }
  if (preview.error) {
    area.innerHTML = `<div class="empty">${h(
      preview.error,
    )}${i18nMessages.mb6f55b7abe9d3c88}`;
    return;
  }
  const s = summary(preview.envelope);
  const sessions = preview.envelope?.details?.sessions || [];
  const pending = s.pending ?? 0;
  area.innerHTML =
    `<div class="metrics">${metric(i18nMessages.m5b6f38c5b9f2e75e, number(pending))}${metric(
      i18nMessages.mccfd39d180096647,
      number(s.skipped_imported ?? 0),
    )}${metric(i18nMessages.m2d63b2740037559e, number(s.deferred_by_limit ?? 0))}${metric(
      i18nMessages.me17ba95288e54994,
      number(s.scanned ?? 0),
    )}</div>` +
    (Array.isArray(s.by_platform) && s.by_platform.length
      ? `${i18nMessages.m763fb08de7174d4f}${h(
          s.by_platform.map((item) => `${item.id} ${item.count}`).join(" · "),
        )}</p>`
      : "") +
    (pending === 0
      ? i18nMessages.mbcf734a2e4fa8458
      : `<div class="si-sessions">${sessions
          .map(
            (session) =>
              `<div class="si-session"><div class="row-copy"><strong>${h(
                session.platform,
              )}</strong><small>${h(sessionLine(session))}</small><p>${h(
                String(session.first_user_message || "").slice(0, 160),
              )}${i18nMessages.m1faf838d1d5816d4}${h(
                `${session.platform}:${session.id}`,
              )}</pre></details></div>`,
          )
          .join("")}</div>`);
}

// Per-Harness receipt line for the aggregated result list.
const entryLine = (entry) => {
  if (entry.state === "running") return i18nMessages.mf6dc1b1641600dd0;
  if (entry.state === "unconfirmed") return i18nMessages.mc3c8784b917e95fb;
  const s = summary(entry.envelope);
  const imported = s.imported ?? 0;
  if (entry.state === "failed") return entry.error || i18nMessages.m01aaebc96af187a9;
  return imported > 0 ? `${i18nMessages.m2a9010137d7ba142}${number(imported)}${i18nMessages.m6fb2840d568a340f}` : i18nMessages.m605d3a09deed9c00;
};

export function renderImportResult(root, preview, previewBusy, applyBusy, applyResult, hasUncertainRequests) {
  const button = $("[data-apply]", root);
  if (!button || !root.isConnected) return;
  // A zero-pending preview disables import: "nothing matches" is reported
  // by the preview, never submitted as a write (spec).
  const ready = Boolean(
    preview &&
      !preview.stale &&
      !preview.error &&
      !previewBusy &&
      preview.envelope?.summary?.pending > 0,
  );
  const unresolved = applyResult?.state === "unconfirmed";
  button.disabled = applyBusy || (unresolved ? !hasUncertainRequests() : !ready);
  button.textContent = applyBusy
    ? i18nMessages.mf6dc1b1641600dd0
    : unresolved
      ? i18nMessages.m3107a8f862e45a45
      : ready
      ? `${i18nMessages.mec603f7ca4ee8b35}${number(preview.envelope.summary.pending)}${i18nMessages.ma2c90b09d74c0cfb}`
      : i18nMessages.mb41a062364aa1707;
  // The submission freezes the preview model; filter editing stays locked
  // until the apply settles.
  all("[data-since],[data-cwd],[data-model],[data-limit]", root).forEach(
    (input) => (input.disabled = applyBusy || unresolved),
  );
  const previewButton = $("[data-preview]", root);
  if (previewButton) previewButton.disabled = applyBusy || unresolved;
  const memories = $("[data-open-memories]", root);
  const operations = $("[data-open-operations]", root);
  const showNav = Boolean(applyResult);
  memories.hidden = !showNav;
  operations.hidden = !showNav;
  const area = $("[data-apply-result]", root);
  if (!area) return;
  if (!applyResult || applyResult.state === "running") {
    area.innerHTML = applyResult?.state === "running"
      ? i18nMessages.m81021491d1d974f8
      : "";
    return;
  }
  if (applyResult.state === "unconfirmed") {
    const entries = (applyResult.results || [])
      .map(
        (entry) =>
          `<div class="row"><span class="dot ${
            entry.state === "done" ? "online" : ""
          }"></span><strong>${h(entry.name)}</strong><small>${h(
            entryLine(entry),
          )}</small></div>`,
      )
      .join("");
    area.innerHTML =
      `<div class="empty">${h(
        i18nMessages.mc8e33ed8510f2410,
      )}</div>` +
      (entries ? `<div class="stack">${entries}</div>` : "") +
      i18nMessages.m18b7527e5b2d39a6;
    return;
  }
  const results = applyResult.results || [];
  const doneEntries = results.filter((entry) => entry.state === "done");
  const failedEntries = results.filter((entry) => entry.state === "failed");
  const totals = doneEntries.reduce(
    (acc, entry) => {
      const s = summary(entry.envelope);
      acc.imported += s.imported ?? 0;
      acc.scanned += s.scanned ?? 0;
      acc.deferred += s.deferred_by_limit ?? 0;
      return acc;
    },
    { imported: 0, scanned: 0, deferred: 0 },
  );
  const rows = results
    .map(
      (entry) =>
        `<div class="row"><span class="dot ${
          entry.state === "done" ? "online" : ""
        }"></span><strong>${h(entry.name)}</strong><small>${h(
          entryLine(entry),
        )}</small></div>`,
    )
    .join("");
  area.innerHTML =
    `<div class="metrics">${metric(i18nMessages.mf62f58be54a10962, number(totals.imported))}${metric(
      i18nMessages.me17ba95288e54994,
      number(totals.scanned),
    )}${metric(i18nMessages.m2d63b2740037559e, number(totals.deferred))}</div>` +
    (results.length > 1 ? `<div class="stack">${rows}</div>` : "") +
    (failedEntries.length
      ? i18nMessages.m0fb385724e4ff98a
      : totals.imported === 0
        ? i18nMessages.m289124956a040ea3
        : i18nMessages.m6ce6493c509894d4);
}
