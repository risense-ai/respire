import { messages as i18nMessages } from "./ui/i18n/messages.js";
// Tray quick panel: mirrors the overview page in a small flyout. Renders
// Chinese first; ui/i18n.js translates to English when the app language is en.
import {
  $,
  h,
  number,
  date,
  bytes,
  api,
  execute,
  notify,
  panel,
  pairs,
  metric,
} from "./ui/core.js";
import { applyTypography } from "./ui/typography.js";
import { applyTranslations, setLanguage } from "./ui/i18n.js";
applyTypography();
// App surface, not a browser page: no native context menu.
document.addEventListener("contextmenu", (event) => event.preventDefault());

const phases = {
  idle: i18nMessages.mdae661d17c2dd8f0,
  running: i18nMessages.m01679253dade46b3,
  ok: i18nMessages.mf28461bb49c85647,
  err: i18nMessages.m336e8460fd284629,
  scheduled: i18nMessages.m2fcab8f67ceeae3e,
  ready: i18nMessages.mb073db9c0bd6f2ed,
  failed: i18nMessages.m28384d7afd2e4fa6,
  loading: i18nMessages.md04fcbda737fc0c6,
  unavailable: i18nMessages.m460b3574e4bdb8ca,
};
const phase = (value) => phases[value] || value || "—";

const state = { desktop: null, status: null, syncing: false };

const systemTheme = matchMedia("(prefers-color-scheme: dark)");
function appearance() {
  const prefs = state.desktop?.preferences || {};
  const theme =
    prefs.theme || localStorage.getItem("respire-theme") || "system";
  const resolved =
    theme === "system" ? (systemTheme.matches ? "dark" : "light") : theme;
  document.documentElement.dataset.theme = resolved;
  $("#brand-icon").src =
    resolved === "dark" ? "brand/respire.svg" : "brand/respire-light.svg";
}
systemTheme.addEventListener("change", () => {
  appearance();
  render();
});

function render() {
  const s = state.status?.summary || {},
    sync = s.sync_live || {},
    index = s.retrieval_index || {},
    inference = state.desktop?.health?.inference || {},
    op = s.model_operation || {},
    session = s.session || {};
  const connected = Boolean(state.desktop?.connected);
  $("#qp-dot").classList.toggle("online", connected);
  $("#qp-connection-label").textContent = connected
    ? `Runtime ${state.desktop.health?.bin || "—"}`
    : i18nMessages.mcef1537a814bdc68;
  $("#qp-body").innerHTML =
    `<div class="qp-metrics">${metric(i18nMessages.m1dcb520e9408dad5, number(s.local_alive), `${i18nMessages.mc665408f30f661f0}${number(s.local_total)}${i18nMessages.m32aca0663f5c881e}`)}${metric(i18nMessages.me25115d1d25e3421, number(sync.remote_alive), s.remote_configured ? i18nMessages.m2616c6d1ebc7614a : i18nMessages.m9183a87f3a6c526e)}${metric(i18nMessages.me09cbaccf1dd8bf8, number(sync.pending), `${number(sync.conflicts)}${i18nMessages.m0cc6b0935f0e15b9}`)}${metric(i18nMessages.m11676e8ace776256, phase(index.state), index.error || "")}</div>` +    panel(
      i18nMessages.mdcfe516d1114e14f,
      pairs([
        [i18nMessages.m6320b4a8722a851f, phase(sync.phase)],
        [i18nMessages.m6e9adb42a28942eb, s.autosync === undefined ? "—" : s.autosync ? i18nMessages.m8a4ef3e48e4e8a5a : i18nMessages.m6744b4c6a9aa0038],
        [i18nMessages.mf2be833267a9befd, `${number(sync.pulled)} / ${number(sync.pushed)}`],
        [
          i18nMessages.m81e60238bae4e54b,
          s.sync_scheduler?.last_success_unix
            ? date(s.sync_scheduler.last_success_unix * 1000)
            : i18nMessages.m25280b57b647e36e,
        ],
        [i18nMessages.m061246bd31e084a7, session.user || i18nMessages.m54392fa24b359913],
        [i18nMessages.m02739d1bae8bd996, s.server_addr],
      ]),
    ) +
    panel(
      i18nMessages.m211a95d339f1f408,
      pairs([
        [i18nMessages.m8168e2d3bbb44b32, phase(inference.phase)],
        [i18nMessages.m07ae2c46b85c3d54, number(inference.queued)],
        [
          i18nMessages.md5a7af1c407f935f,
          index.running ? i18nMessages.m0a53c8de45a1ed4a : index.scheduled ? i18nMessages.m4f021f59121c6827 : phase(index.state),
        ],
        [i18nMessages.m281dccfa29c6c9fb, op.active ? `${phase(op.phase)} · ${op.item || ""}` : i18nMessages.mac3f9d75f986bef6],
      ]) +
        (op.active
          ? `<div class="panel-body">${`<div class="progress-label"><span>${h(op.item || op.phase)}</span><span>${h(bytes(op.done))} / ${h(bytes(op.total))}</span></div><progress ${op.total ? `value="${h(op.done)}" max="${h(op.total)}"` : ""}></progress>`}</div>`
          : ""),
    );
  $("#qp-sync").disabled = state.syncing || !connected;
  $("#qp-sync").textContent = state.syncing ? i18nMessages.m4e2238b74ec75987 : i18nMessages.m36e7a43a1c674e50;
  applyTranslations();
  fit();
}

// Size the popup window to the content so nothing ever scrolls inside.
function fit() {
  const header = $(".qp-header"),
    notice = $("#notice"),
    main = $("#qp-body"),
    footer = $(".qp-footer");
  const zoom =
    parseFloat(document.documentElement.style.zoom || "1") || 1;
  // #qp-body sits on a 1fr grid track and stretches to the window height, so
  // its scrollHeight reports the stale window size once the content shrinks
  // and the panel would never shrink back. Measure the natural content
  // height instead; the inline height is restored before anything paints.
  main.style.height = "fit-content";
  const natural = main.scrollHeight;
  main.style.height = "";
  const height = Math.ceil(
    (header.offsetHeight +
      natural +
      footer.offsetHeight +
      (notice.hidden ? 0 : notice.offsetHeight) +
      2) *
      zoom,
  );
  api("panel-size", { height }).catch(() => {});
}

async function refresh() {
  try {
    state.desktop = await api("state");
    appearance();
    state.status = state.desktop.connected ? await api("runtime-status") : null;
  } catch {
    // a failed poll keeps the last rendered values; the next poll corrects them
  }
  render();
}

$("#qp-refresh").addEventListener("click", () => refresh());
$("#qp-open").addEventListener("click", async (event) => {
  const button = event.currentTarget;
  button.disabled = true;
  try {
    await api("open-main", {});
  } catch (error) {
    notify(error.message, true);
  } finally {
    button.disabled = false;
  }
});
$("#qp-sync").addEventListener("click", async (event) => {
  const button = event.currentTarget;
  state.syncing = true;
  render();
  try {
    await execute("sync");
    notify(i18nMessages.m678b02618ee73838);
  } catch (error) {
    notify(error.message, true);
  } finally {
    state.syncing = false;
    render();
  }
  await refresh();
});
// Refresh whenever the flyout gains focus (tray click) and poll gently.
window.addEventListener("focus", () => refresh());
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) refresh();
});
setInterval(() => {
  if (!document.hidden) refresh();
}, 3000);

(async () => {
  try {
    state.desktop = await api("state");
    const preferred =
      state.desktop.preferences?.language ||
      localStorage.getItem("respire-language") ||
      "zh";
    setLanguage(preferred);
  } catch {
    setLanguage(localStorage.getItem("respire-language") || "zh");
  }
  appearance();
  await refresh();
})();
