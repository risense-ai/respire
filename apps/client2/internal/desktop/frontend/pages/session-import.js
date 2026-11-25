import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { $, all, h, api, number, execute, action, confirm, notify, summary, panel, rows } from "../ui/core.js";
import { heading } from "./overview.js";

import { renderImportPreview, renderImportResult } from "./session-import-results.js";

// Session Import: a guided, non-writing preview over the CLI's extract
// contract, then one confirmed write of the previewed result set. The CLI
// re-scans at execution time, so the page never claims a locked snapshot;
// source Harness stores stay read-only and imports are ordinary Respire
// summary memories (one per session, not full transcripts).
const SINCE_CHOICES = [
  ["", i18nMessages.m12edbc42122f1ade],
  ["today", i18nMessages.md7c9f16c18d1a488],
  ["yesterday", i18nMessages.mcda06428f11437d9],
  ["7d", i18nMessages.m2261b06712a3c505],
  ["30d", i18nMessages.mf729bb3d3f7ef33c],
];
const LIMIT_DEFAULT = 200,
  LIMIT_MAX = 2000;

const DISCLOSURE =
  i18nMessages.maefb9c29838e3517;

function sessionImportMarkup() {
  return (
    `<p class="hint si-disclosure">${h(DISCLOSURE)}</p>` +
    panel(
      i18nMessages.me8b22c4002883395,
      `${i18nMessages.m3159f7cef65486f2}`,
    ) +
    panel(
      i18nMessages.mc7d14a7c11ea3a4c,
      `${i18nMessages.mb71013f82865c41f}${SINCE_CHOICES.map(
        ([value, text]) => `<option value="${h(value)}">${h(text)}</option>`,
      ).join("")}${i18nMessages.m062c2228fec97ff3}${LIMIT_MAX}${i18nMessages.me8783c7e74c952da}${LIMIT_MAX}" value="${LIMIT_DEFAULT}${i18nMessages.m6686701a1afa1b69}`,
    ) +
    panel(
      i18nMessages.m04406bccab1c4b0a,
      `${i18nMessages.m820c37631eef8e11}`,
    )
  );
}

export function sessionImport(root, ctx) {
  root.innerHTML =
    heading(
      i18nMessages.m3e75b03e259494c9,
      i18nMessages.m59a86243becca1a4,
    ) + sessionImportMarkup();
  return sessionImportController(root, ctx);
}

// The onboarding wizard mounts the same regions without the page heading.
export function wizardImportStep(root, ctx) {
  root.innerHTML = sessionImportMarkup();
  return sessionImportController(root, ctx);
}

function sessionImportController(root, ctx) {
  let discovery = { state: "idle", sources: [], error: "" };
  // Batch selection over the detected Harnesses (default: all). The CLI
  // takes one --id per invocation, so a subset runs one extraction per
  // selected Harness while the full selection omits --id entirely.
  let selected = new Set();
  let preview = null; // { model, envelope, error, stale }
  let previewToken = 0;
  let previewBusy = false;
  let applyBusy = false;
  let applyResult = null; // { state, results?, error?, requestId? }
  let attempted = false;

  const filterError = () => {
    const limit = Number($("[data-limit]", root).value);
    if (!Number.isInteger(limit) || limit < 1 || limit > LIMIT_MAX)
      return `${i18nMessages.m6f1d645074732c8a}${LIMIT_MAX}${i18nMessages.mf62f73ba55986683}`;
    return "";
  };

  // One canonical filter model feeds preview and apply (D2): empty optional
  // strings are omitted, never forwarded. sources records the batch
  // selection; allSelected means the CLI's own all-detected pass (no --id).
  const readModel = () => {
    const installed = detected();
    return {
      sources: [...selected],
      allSelected: installed.length > 0 && selected.size === installed.length,
      since: $("[data-since]", root).value,
      cwd: $("[data-cwd]", root).value.trim(),
      model: $("[data-model]", root).value.trim(),
      limit: Number($("[data-limit]", root).value),
    };
  };

  // Filters shared by every operation of one submission.
  const baseArgs = (model) => {
    const args = {};
    if (model.since) args.since = model.since;
    if (model.cwd) args.cwd = model.cwd;
    if (model.model) args.model = model.model;
    if (Number.isInteger(model.limit) && model.limit >= 1)
      args.limit = model.limit;
    return args;
  };
  const argsFor = (model, id) => {
    const args = baseArgs(model);
    if (id) args.id = id;
    return args;
  };

  const sourceLabel = (id) =>
    discovery.sources.find((item) => item.id === id)?.name || id;

  const selectedNames = (model) =>
    model.allSelected
      ? i18nMessages.m1c0887804d79b767
      : model.sources.map((id) => sourceLabel(id)).join(i18nMessages.mc4a40ea8fe356274);

  const modelText = (model) => {
    const parts = [selectedNames(model)];
    if (model.since)
      parts.push(
        SINCE_CHOICES.find(([value]) => value === model.since)?.[1] ||
          model.since,
      );
    if (model.cwd) parts.push(`${i18nMessages.mabba3e0c2005b4ae}${model.cwd}${i18nMessages.md93a835d0f2787bc}`);
    if (model.model) parts.push(`${i18nMessages.mae465c72ba072f4a}${model.model}${i18nMessages.md93a835d0f2787bc}`);
    parts.push(`${i18nMessages.m044a3e4b2312af22}${model.limit}`);
    return parts.join(" · ");
  };

  const detected = () => discovery.sources.filter((item) => item.installed);

  async function discover() {
    if (!ctx.desktop?.connected) {
      discovery = {
        state: "offline",
        sources: [],
        error: i18nMessages.m6cef431b3aee8ddd,
      };
      renderSources();
      return;
    }
    discovery = { state: "running", sources: discovery.sources, error: "" };
    renderSources();
    try {
      const envelope = await execute("session_import_targets", {});
      if (!root.isConnected) return;
      discovery = {
        state: "done",
        sources: rows(envelope),
        error: "",
      };
      // Keep the selection inside the freshly detected set; an empty
      // selection falls back to all detected.
      const ids = detected().map((item) => item.id);
      selected = new Set([...selected].filter((id) => ids.includes(id)));
      if (!selected.size && ids.length) selected = new Set(ids);
    } catch (error) {
      if (!root.isConnected) return;
      discovery = { state: "error", sources: [], error: error.message };
    }
    invalidatePreview(i18nMessages.m64aaf7c37cb628b5);
    renderSources();
  }

  // Any source/filter change marks the previous preview invalid; import
  // needs a fresh, matching preview (D2/D4).
  function invalidatePreview(note = i18nMessages.m07ed47cc6372ff55) {
    previewToken++;
    if (preview && !preview.stale)
      preview = { ...preview, stale: true, note };
    renderPreview();
    renderApply();
  }

  async function runPreview() {
    const error = filterError();
    if (error) {
      $("[data-preview-note]", root).textContent = error;
      notify(error, true);
      return;
    }
    if (!detected().length) {
      $("[data-preview-note]", root).textContent = i18nMessages.m757d11c3c04e5eac;
      return;
    }
    if (!selected.size) {
      $("[data-preview-note]", root).textContent = i18nMessages.mf5deb79b5c2e8a58;
      return;
    }
    const model = readModel();
    const token = ++previewToken;
    previewBusy = true;
    preview = { model, envelope: null, error: "", stale: false, note: "" };
    renderPreview();
    renderApply();
    try {
      let envelope;
      if (model.allSelected) {
        // The full selection is one CLI pass without --id.
        envelope = await execute("session_import_preview", argsFor(model, null));
        if (token !== previewToken || !root.isConnected) return;
      } else {
        // A subset cannot be expressed as one CLI call: dry-run each
        // selected Harness with the same filters and aggregate.
        envelope = {
          status: "ok",
          summary: { scanned: 0, pending: 0, skipped_imported: 0, deferred_by_limit: 0, by_platform: [] },
          details: { sessions: [] },
          items: [],
          errors: [],
        };
        for (const id of model.sources) {
          const part = await execute("session_import_preview", argsFor(model, id));
          if (token !== previewToken || !root.isConnected) return;
          const ps = summary(part);
          envelope.summary.scanned += ps.scanned ?? 0;
          envelope.summary.pending += ps.pending ?? 0;
          envelope.summary.skipped_imported += ps.skipped_imported ?? 0;
          envelope.summary.deferred_by_limit += ps.deferred_by_limit ?? 0;
          envelope.summary.by_platform.push(...(Array.isArray(ps.by_platform) ? ps.by_platform : []));
          (envelope.details.sessions ||= []).push(...(part.details?.sessions || []));
        }
      }
      preview = { model, envelope, error: "", stale: false, note: "" };
    } catch (caught) {
      if (token !== previewToken || !root.isConnected) return;
      preview = { model, envelope: null, error: caught.message, stale: false, note: "" };
    } finally {
      previewBusy = false;
    }
    renderPreview();
    renderApply();
  }

  async function queryOperation(requestId) {
    try {
      const job = await api("operation?id=" + encodeURIComponent(requestId));
      if (job.state === "done" && job.envelope)
        return { state: "done", envelope: job.envelope };
      if (["failed", "running", "unconfirmed"].includes(job.state))
        return { state: job.state, error: job.error || i18nMessages.mceaee936b63c72b0 };
      return { state: "unconfirmed", error: i18nMessages.mba35866df9410c7f };
    } catch (error) {
      return { state: "unconfirmed", error: error.message };
    }
  }

  // One extraction per selected Harness (or a single all-detected pass).
  // Each Harness reports its own receipt; one failure never blocks the
  // rest, and an uncertain receipt is reconciled against its original
  // operation instead of being resubmitted.
  async function runApply() {
    if (applyBusy) return;
    if (applyResult?.state === "unconfirmed") {
      const uncertain = (applyResult.results || []).filter(
        (entry) =>
          (entry.state === "unconfirmed" || entry.state === "running") &&
          entry.requestId,
      );
      if (!uncertain.length) return;
      applyBusy = true;
      applyResult.state = "running";
      renderApply();
      for (const entry of uncertain) {
        const job = await queryOperation(entry.requestId);
        if (job.state === "done" && job.envelope) {
          entry.state = "done";
          entry.envelope = job.envelope;
        } else {
          entry.state = job.state;
          entry.error = job.error || entry.error;
        }
      }
      applyResult.state = aggregateState(applyResult.results);
      applyBusy = false;
      await afterApplySettled(applyResult);
      renderApply();
      return;
    }
    if (!preview || preview.stale || preview.error || previewBusy) return;
    const selectedPreview = preview;
    const token = previewToken;
    const model = preview.model; // frozen at confirm time (D2)
    const pending = preview.envelope?.summary?.pending ?? 0;
    if (pending <= 0) return;
    if (
      !(await confirm(
        i18nMessages.mb41a062364aa1707,
        `${i18nMessages.ma26bfcbf7ac83a58}${h(modelText(model))}${i18nMessages.m6d134080116c1038}${number(pending)}${i18nMessages.m8dbeb3a14c112f1f}`,
        i18nMessages.m4a8d6841b4023edf,
      ))
    )
      return;
    if (applyBusy || preview !== selectedPreview || previewToken !== token) return;
    applyBusy = true;
    // Consume the preview before the first submission so a lost response
    // can never be followed by a second write of the same conditions.
    preview = null;
    previewToken++;
    const ids = model.allSelected
      ? [null]
      : model.sources;
    applyResult = {
      state: "running",
      results: ids.map((id) => ({
        id,
        name: id ? sourceLabel(id) : i18nMessages.m1c0887804d79b767,
        state: "running",
      })),
    };
    renderPreview();
    renderApply();
    for (const entry of applyResult.results) {
      try {
        const envelope = await execute(
          "session_import_apply",
          argsFor(model, entry.id),
        );
        entry.state = "done";
        entry.envelope = envelope;
      } catch (error) {
        entry.state = "unconfirmed";
        entry.error = error.message;
        entry.requestId = error.requestId;
        if (error.requestId) {
          const job = await queryOperation(error.requestId);
          if (job.state === "done" && job.envelope) {
            entry.state = "done";
            entry.envelope = job.envelope;
          } else if (job.state !== "unconfirmed" && job.state !== "running") {
            entry.state = job.state;
          }
        }
      }
      applyResult.state = aggregateState(applyResult.results);
      renderApply();
    }
    applyBusy = false;
    await afterApplySettled(applyResult);
    renderSources();
    renderPreview();
    renderApply();
  }

  // Uncertain receipts keep the whole batch on the reconcile path until
  // every operation's outcome is known.
  function aggregateState(results) {
    return results.some(
      (entry) => entry.state === "unconfirmed" || entry.state === "running",
    )
      ? "unconfirmed"
      : "done";
  }

  async function afterApplySettled(result) {
    if (result.state !== "done") return;
    try {
      await ctx.refresh();
    } catch (error) {
      notify(error.message, true);
    }
  }

  function renderSources() {
    const area = $("[data-sources]", root);
    if (!area || !root.isConnected) return;
    if (discovery.state === "running") {
      area.innerHTML = i18nMessages.mab08f8b216317288;
      return;
    }
    if (discovery.state === "offline" || discovery.state === "error") {
      area.innerHTML = `<div class="empty">${h(
        discovery.error,
      )}${i18nMessages.m691c6226ab4610cb}`;
      $("[data-rediscover]", area)?.addEventListener("click", () =>
        action($("[data-rediscover]", area), discover),
      );
      return;
    }
    const list = discovery.sources;
    if (!list.length) {
      area.innerHTML =
        i18nMessages.m79cab8ffd114ed7a;
      $("[data-rediscover]", area)?.addEventListener("click", () =>
        action($("[data-rediscover]", area), discover),
      );
      return;
    }
    const installedList = detected();
    const allChecked = installedList.length > 0 && selected.size === installedList.length;
    const body =
      (!installedList.length
        ? i18nMessages.m4ec5edcf668e67e2
        : `<label class="si-source${allChecked ? " active" : ""}"><input type="checkbox" data-source-all${
            allChecked ? " checked" : ""
          }${i18nMessages.mebd970e4dc1eeea1}${h(
            `${i18nMessages.m52c963128eab12b5}${selected.size} / ${installedList.length}${i18nMessages.m7aaf96ad52e049e5}`,
          )}</small></span></label>` +
          installedList
            .map(
              (item) =>
                `<label class="si-source${selected.has(item.id) ? " active" : ""}"><input type="checkbox" data-source="${h(
                  item.id,
                )}"${selected.has(item.id) ? " checked" : ""}><span><strong>${h(
                  item.name,
                )}${i18nMessages.m0215ee316aedfaa1}${(item.roots || []).length}${i18nMessages.mc189b41c33b65c9c}`,
            )
            .join("")) +
      list
        .filter((item) => !item.installed)
        .map(
          (item) =>
            `<label class="si-source unavailable"><input type="checkbox" disabled><span><strong>${h(
              item.name,
            )}${i18nMessages.m55bfbbeca21e49d6}`,
        )
        .join("") +
      i18nMessages.m4a7635646d0586c9;
    area.innerHTML = body;
    all('input[data-source]:not([disabled])', area).forEach((box) =>
      box.addEventListener("change", () => {
        if (box.checked) selected.add(box.dataset.source);
        else selected.delete(box.dataset.source);
        invalidatePreview();
        renderSources();
      }),
    );
    all("input[data-source-all]", area).forEach((box) =>
      box.addEventListener("change", () => {
        selected = new Set(
          box.checked ? detected().map((item) => item.id) : [],
        );
        invalidatePreview();
        renderSources();
      }),
    );
    $("[data-rediscover]", area)?.addEventListener("click", () =>
      action($("[data-rediscover]", area), discover),
    );
  }

  function renderPreview() {
    renderImportPreview(root, preview, previewBusy);
  }

  function renderApply() {
    renderImportResult(root, preview, previewBusy, applyBusy, applyResult, hasUncertainRequests);
  }

  const hasUncertainRequests = () =>
    (applyResult?.results || []).some(
      (entry) =>
        (entry.state === "unconfirmed" || entry.state === "running") &&
        entry.requestId,
    );

  root.addEventListener("click", (event) => {
    const button = event.target.closest("button");
    if (!button) return;
    if (button.hasAttribute("data-preview")) action(button, runPreview);
    if (button.hasAttribute("data-apply")) action(button, runApply);
    if (button.hasAttribute("data-open-memories")) ctx.navigate("memories");
    if (button.hasAttribute("data-open-operations")) ctx.navigate("operations");
  });
  // Editing any filter invalidates a previous preview immediately (D2).
  root.addEventListener("input", (event) => {
    if (event.target.closest("[data-since],[data-cwd],[data-model],[data-limit]"))
      invalidatePreview();
  });

  if (ctx.desktop?.connected) {
    attempted = true;
    discover();
  } else {
    // Disconnected at mount: the honest offline state with a retry, never
    // an empty-sources result.
    discovery = {
      state: "offline",
      sources: [],
      error: i18nMessages.m6cef431b3aee8ddd,
    };
    renderSources();
  }
  return {
    refresh: () => discover(),
    update() {
      if (!attempted && ctx.desktop?.connected) {
        attempted = true;
        discover();
      }
    },
  };
}
