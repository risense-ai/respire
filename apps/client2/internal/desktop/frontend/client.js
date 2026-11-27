import { messages as i18nMessages } from "./ui/i18n/messages.js";
import { $, all, api, execute, notify, number, rows } from "./ui/core.js";
import { overview } from "./pages/overview.js";
import { memoryWorkspace } from "./memory/workspace.js";
import { diary, localCalendarDay } from "./memory/diary.js";
import { organize } from "./pages/organize.js";
import { sessionImport } from "./pages/session-import.js";
import { models } from "./pages/models.js";
import {
  settings,
  updates,
  diagnostics,
  operations,
} from "./pages/settings.js";
import { accounts } from "./pages/accounts.js";
import { restorePendingRecoveryMaterials } from "./pages/authentication.js";
import { injection } from "./pages/injection.js";
import { applyTypography } from "./ui/typography.js";
import { grants } from "./pages/grants.js";
import {
  applyTranslations,
  currentLanguage,
  setLanguage,
  t,
} from "./ui/i18n.js";
import { createWizard } from "./onboarding/wizard.js";
applyTypography();
// The window is an app surface, not a browser page: no native context menu.
document.addEventListener("contextmenu", (event) => event.preventDefault());

const definitions = {
  overview: [i18nMessages.mfea405f9b01d1416, overview],
  memories: [i18nMessages.m7a6335b379b9a161, memoryWorkspace],
  diary: [i18nMessages.mfee683550df02f68, diary],
  organize: [i18nMessages.mff0934736802c998, organize],
  "session-import": [i18nMessages.mc565fe326df60aae, sessionImport],
  models: [i18nMessages.mc243416c93c07db7, models],
  accounts: [i18nMessages.m36d905ba1af35450, accounts],
  injection: [i18nMessages.m202a0f6e0434231a, injection],
  grants: [i18nMessages.m129727729d8606db, grants],
  settings: [i18nMessages.mdf3d58c7d84b85f2, settings],
  updates: [i18nMessages.m3055a035f0eb7a8b, updates],
  operations: [i18nMessages.mb2db0d03bb7d08ca, operations],
  diagnostics: [i18nMessages.m40ff6300f9817deb, diagnostics],
};
const settingsPages = ["settings", "updates", "operations", "diagnostics"];
let current,
  view,
  refreshing = false;
// Set once the topbar dropdowns are wired; appearance() runs earlier, so it
// only syncs the theme value when the dropdown already exists.
let themeDropdown = null;
const ctx = {
  desktop: null,
  status: null,
  statusError: "",
  api,
  execute,
  notify,
  navigate,
  refresh,
  changeLanguage,
  savePreferences,
};
const wizard = createWizard(ctx, {
  onFinish: () => route(),
});
ctx.openWizard = () => wizard.show({ reopen: true });
const systemTheme = matchMedia("(prefers-color-scheme: dark)");
function appearance() {
  const prefs = ctx.desktop?.preferences || {};
  const theme =
    prefs.theme || localStorage.getItem("respire-theme") || "system";
  const resolved =
    theme === "system" ? (systemTheme.matches ? "dark" : "light") : theme;
  document.documentElement.dataset.theme = resolved;
  $("#brand-icon").src =
    resolved === "dark" ? "brand/respire.svg" : "brand/respire-light.svg";
  themeDropdown?.set(theme);
  document.body.dataset.layout = prefs.layout || "sidebar";
  const toggle = $("#layout-toggle");
  const title = prefs.layout === "compact" ? i18nMessages.md4b5eee98dcd211f : i18nMessages.ma64997f9e1bcaafe;
  toggle.title = t(title);
  toggle.setAttribute("aria-label", t(title));
  const width = Number(prefs.sidebarWidth) || 0;
  document.documentElement.style.setProperty(
    "--sidebar-w",
    width ? `${width}px` : "",
  );
}
appearance();
systemTheme.addEventListener("change", appearance);
async function savePreferences(patch) {
  if (!ctx.desktop) throw new Error(i18nMessages.m3dfe86440d25010d);
  // Post only the patch: the bridge overlays present keys onto the stored
  // preferences, so a stale local mirror can never rewrite fields the
  // caller did not touch (this un-did checkbox toggles).
  await api("preferences", patch);
  ctx.desktop.preferences = { ...ctx.desktop.preferences, ...patch };
  if (patch.theme) localStorage.setItem("respire-theme", patch.theme);
  if (patch.language) languageDropdown.set(patch.language);
  appearance();
}
async function changeLanguage(value) {
  await savePreferences({ language: value });
  setLanguage(value);
  location.reload();
}
function navigate(page) {
  // While the onboarding wizard owns the screen, ordinary routes cannot be
  // reached; the wizard navigates through ctx.navigate only after pausing
  // itself (e.g. the updates entry), when isActive() is already false.
  if (wizard.isActive()) return;
  location.hash = page;
}
function route() {
  if (wizard.isActive()) return;
  const [page = "overview", query = ""] = (
    location.hash.slice(1) || "overview"
  ).split("?");
  ctx.params = new URLSearchParams(query);
  if (!definitions[page]) {
    navigate("overview");
    return;
  }
  current = page;
  view?.dispose?.();
  notify("");
  const root = document.createElement("section");
  root.className = "view-page";
  $("#view").replaceChildren(root);
  $("#breadcrumb").textContent = definitions[page][0];
  const inSettings = settingsPages.includes(page);
  if (inSettings) {
    const navigation = document.createElement("div");
    navigation.className = "tabs";
    navigation.setAttribute("role", "navigation");
    navigation.setAttribute("aria-label", i18nMessages.mdf3d58c7d84b85f2);
    settingsPages.forEach((key) => {
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = definitions[key][0];
      button.classList.toggle("active", key === page);
      if (key === page) button.setAttribute("aria-current", "page");
      button.addEventListener("click", () => navigate(key));
      navigation.append(button);
    });
    root.before(navigation);
  }
  all("nav [data-page]").forEach((button) => {
    const active = button.dataset.page === page ||
      (button.dataset.page === "settings" && inSettings);
    button.classList.toggle("active", active);
    if (active)
      button.setAttribute("aria-current", "page");
    else button.removeAttribute("aria-current");
  });
  view = definitions[page][1](root, ctx);
  document.querySelector(".view-scroll").scrollTop = 0;
}
// Sidebar nav counts: memories come from the status summary; diary (today)
// and organize (pending suggestions) are RPC-backed and refreshed sparingly.
const navCounts = { at: 0, running: false };
async function refreshNavCounts() {
  if (!ctx.desktop?.connected || navCounts.running) return;
  if (Date.now() - navCounts.at < 60000) return;
  navCounts.running = true;
  navCounts.at = Date.now();
  try {
    const day = localCalendarDay(new Date());
    const diaryEnv = await execute("diary_list", { from: day, to: day });
    if (!document.isConnected) return;
    $("#nav-diary-count").textContent = number(rows(diaryEnv).length);
  } catch {
    $("#nav-diary-count").textContent = "";
  }
  try {
    const suggestEnv = await execute("organize_suggestions", { top: 50 });
    if (!document.isConnected) return;
    const items = suggestEnv?.details?.suggests;
    $("#nav-organize-count").textContent = Array.isArray(items)
      ? number(items.length)
      : "";
  } catch {
    $("#nav-organize-count").textContent = "";
  }
  navCounts.running = false;
}
// Keep unchanged refresh labels' text nodes intact, including for accessibility.
// Compare the live DOM rather than cached values so external changes are repaired.
function setRefreshText(element, value) {
  const text = value == null ? "" : `${value}`;
  if (element.textContent !== text) element.textContent = text;
}
async function refresh() {
  if (refreshing) return;
  refreshing = true;
  try {
    ctx.desktop = await api("state");
    appearance();
    $("#connection-dot").classList.toggle("online", ctx.desktop.connected);
    setRefreshText($("#connection-label"), ctx.desktop.connected
      ? t(i18nMessages.m3053ee2f4c18204a)
      : t(i18nMessages.mcef1537a814bdc68));
    setRefreshText($("#client-version"), t(i18nMessages.m477581b84e505d10) + " " + ctx.desktop.version);
    setRefreshText($("#client-platform"), ctx.desktop.platform);
    if (ctx.desktop.connected) {
      try {
        ctx.status = await api("runtime-status");
        ctx.statusError = "";
      } catch (error) {
        ctx.status = null;
        ctx.statusError = error.message;
      }
    } else {
      ctx.status = null;
    ctx.statusError = ctx.desktop.connectionError || t(i18nMessages.mcef1537a814bdc68);
    }
    const s = ctx.status?.summary || {};
    setRefreshText($("#nav-memory-count"), number(s.local_alive));
    setRefreshText($("#status-summary"), ctx.desktop.connected
      ? currentLanguage() === "zh"
        ? `${number(s.local_alive)}${i18nMessages.mb744c153d9ad8e8d}${s.workspace || "normal"} · Runtime ${ctx.desktop.health?.bin || "—"}`
        : `${number(s.local_alive)} memories · ${s.workspace || "normal"} · Runtime ${ctx.desktop.health?.bin || "—"}`
      : t(i18nMessages.mcef1537a814bdc68));
    const sync = s.sync_live || {};
    setRefreshText($("#sync-summary"), ctx.statusError
      ? i18nMessages.m23dfe4404e8a5ebe
      : currentLanguage() === "zh"
        ? `${i18nMessages.me0a0d9df9e272b07}${sync.phase || "—"}${i18nMessages.mbcf32fee8dea4af8}${number(sync.pending)}${i18nMessages.m49025184d9da0006}${number(sync.conflicts)}`
        : `Sync ${sync.phase || "—"} · pending ${number(sync.pending)} · conflicts ${number(sync.conflicts)}`);
    view?.update?.();
  } catch (error) {
    notify(error.message, true);
  } finally {
    refreshing = false;
  }
  refreshNavCounts();
}
all("nav [data-page]").forEach((button) =>
  button.addEventListener("click", () => navigate(button.dataset.page)),
);
// The local-workspace card is a shortcut to the accounts page.
$("#workspace-entry").addEventListener("click", () => navigate("accounts"));
// Self-drawn dropdowns: native selects render with dated system chrome on
// some platforms, so the topbar controls draw their own listbox.
function initDropdown(dropdown, onChange) {
  const toggle = $(".dropdown-toggle", dropdown),
    menu = $(".dropdown-menu", dropdown),
    value = $(".dropdown-value", dropdown);
  const options = all("[role=option]", menu);
  const close = () => {
    menu.hidden = true;
    toggle.setAttribute("aria-expanded", "false");
  };
  const pick = (option) => {
    options.forEach((item) =>
      item.setAttribute("aria-selected", String(item === option)),
    );
    value.textContent = option.textContent;
    close();
    onChange(option.dataset.value);
  };
  toggle.addEventListener("click", () => {
    menu.hidden = !menu.hidden;
    toggle.setAttribute("aria-expanded", String(!menu.hidden));
    if (!menu.hidden)
      (
        options.find((item) => item.getAttribute("aria-selected") === "true") ||
        options[0]
      )?.focus();
  });
  menu.addEventListener("click", (event) => {
    const option = event.target.closest("[role=option]");
    if (option) pick(option);
  });
  menu.addEventListener("keydown", (event) => {
    const index = options.indexOf(document.activeElement);
    if (event.key === "Escape") {
      close();
      toggle.focus();
    } else if (event.key === "ArrowDown") {
      event.preventDefault();
      options[Math.min(options.length - 1, index + 1)]?.focus();
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      options[Math.max(0, index - 1)]?.focus();
    } else if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      const option = document.activeElement?.closest("[role=option]");
      if (option) pick(option);
    }
  });
  document.addEventListener("click", (event) => {
    if (!dropdown.contains(event.target)) close();
  });
  return {
    set(next) {
      const option = options.find((item) => item.dataset.value === next);
      if (!option) return;
      options.forEach((item) =>
        item.setAttribute("aria-selected", String(item === option)),
      );
      value.textContent = option.textContent;
    },
  };
}
themeDropdown = initDropdown($("#theme-dropdown"), async (value) => {
  try {
    await savePreferences({ theme: value });
  } catch (error) {
    notify(error.message, true);
    appearance();
  }
});
const languageDropdown = initDropdown(
  $("#language-dropdown"),
  async (value) => {
    try {
      await changeLanguage(value);
    } catch (error) {
      notify(error.message, true);
      languageDropdown.set(currentLanguage());
    }
  },
);
$("#layout-toggle").addEventListener("click", async () => {
  try {
    await savePreferences({
      layout:
        ctx.desktop.preferences.layout === "compact" ? "sidebar" : "compact",
    });
  } catch (error) {
    notify(error.message, true);
  }
});
const SIDEBAR_MIN = 170,
  SIDEBAR_MAX = 380;
$("#sidebar-resizer").addEventListener("mousedown", (event) => {
  if (document.body.dataset.layout === "compact") return;
  event.preventDefault();
  document.body.classList.add("resizing");
  const apply = (clientX) =>
    document.documentElement.style.setProperty(
      "--sidebar-w",
      `${Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, clientX))}px`,
    );
  const move = (e) => apply(e.clientX);
  const up = (e) => {
    window.removeEventListener("mousemove", move);
    window.removeEventListener("mouseup", up);
    document.body.classList.remove("resizing");
    apply(e.clientX);
    const width = Math.round(
      Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, e.clientX)),
    );
    savePreferences({ sidebarWidth: width }).catch((error) =>
      notify(error.message, true),
    );
  };
  window.addEventListener("mousemove", move);
  window.addEventListener("mouseup", up);
});
$("#refresh").addEventListener("click", async () => {
  await refresh();
  await view?.refresh?.();
});
// While the wizard is active it owns the screen: hash edits from the
// keyboard or extensions are rolled back instead of re-routing.
window.addEventListener("hashchange", () => {
  if (wizard.isActive()) {
    if (location.hash.slice(1).split("?")[0] !== current)
      location.hash = current;
    return;
  }
  route();
});
document.addEventListener("respire-operation", () => refresh());
document.addEventListener("respire-runtime-progress", ({ detail: operation }) => {
  if (!ctx.status?.summary || !["model_switch", "model_download"].includes(operation.kind)) return;
  ctx.status.summary.model_operation = {
    id: operation.operation_id, active: ["pending", "running"].includes(operation.state),
    phase: operation.message || operation.state, done: operation.progress_percent, total: 100,
  };
  view?.update?.();
});
let translationScheduled = false;
new MutationObserver(() => {
  if (translationScheduled) return;
  translationScheduled = true;
  requestAnimationFrame(() => {
    translationScheduled = false;
    applyTranslations();
  });
}).observe(document.documentElement, {
  childList: true,
  subtree: true,
  characterData: true,
  attributes: true,
  attributeFilter: ["title", "placeholder", "aria-label"],
});
async function poll() {
  await refresh();
  const streaming = ctx.desktop?.health?.capabilities?.includes("sse.operations.v1");
  setTimeout(poll, streaming ? 30000 : document.hidden ? 15000 : 3000);
}
(async () => {
  try {
    ctx.desktop = await api("state");
    const preferred =
      ctx.desktop.preferences?.language ||
      localStorage.getItem("respire-language") ||
      "";
    if (preferred) {
      setLanguage(preferred);
      languageDropdown.set(preferred);
    }
  } catch {
    const preferred = localStorage.getItem("respire-language");
    if (preferred) setLanguage(preferred);
  }
  route();
  // First run: the wizard replaces the old first-language layer and covers
  // every step from language to model enablement. The auto-show waits for
  // the first status read and never opens when this machine already has a
  // local account; the entry in Settings → About still opens a full re-run.
  const firstPoll = poll();
  try {
    await restorePendingRecoveryMaterials();
  } catch (error) {
    notify(error.message, true);
  }
  if (!ctx.desktop || ctx.desktop.preferences?.onboardingCompleted) return;
  await firstPoll;
  if (ctx.status?.summary?.session?.has_token) return;
  wizard.show();
})();
