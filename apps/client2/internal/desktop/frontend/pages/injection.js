import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { $, all, h, api, action, notify, confirm, panel } from "../ui/core.js";
import { heading } from "./overview.js";
import { hostIcons } from "../icons.js";
import { currentLanguage } from "../ui/i18n.js";
import {
  agentResultMessage,
  agentStatus,
  targetDetected,
  targetsForScope,
} from "../ui/agent-install.js";

const modes = [
  ["normal", i18nMessages.m1a6d1086ff83691e],
  ["readonly", i18nMessages.m04e2f0f162971a2e],
  ["off", i18nMessages.mae3a7c1aa11dd011],
];

const modeNames = {
  inject: i18nMessages.m4ce4c98eb27e4b65,
  block: i18nMessages.mae60964c61d97034,
  reference: i18nMessages.me0ec651f0dae7cf3,
  "ref-block": i18nMessages.mf0303be456b065c3,
  plugin: i18nMessages.me806fbbe8ec6e82d,
};
const modeHints = {
  inject: i18nMessages.m7f23d2131cd6c873,
  block: i18nMessages.m6bd817e76d131627,
  reference: i18nMessages.mdefe9f717fc20a88,
  "ref-block": i18nMessages.m859b00d2079569a0,
  plugin: i18nMessages.mb859880840aa6f33,
};

const SCOPE_LABELS = {
  prompt: i18nMessages.me52f6f9c2e738a24,
  plugin: i18nMessages.m7a763afa15c4dc03,
};

const SCOPE_NOTES = {
  prompt:
    i18nMessages.m2a38a5459a96aa60,
  plugin:
    i18nMessages.m7b7c1b07a0f3f724,
};

const SCOPE_EMPTY = {
  prompt:
    i18nMessages.mac57ed54e7202c52,
  plugin:
    i18nMessages.m9abbbc8a59e1fd08,
};

export function injection(root, ctx) {
  let targets = [],
    mode = "normal",
    scope = "prompt",
    openIds = new Set(),
    showAll = false,
    busy = false,
    attempted = false;

  root.innerHTML =
    heading(
      i18nMessages.m4f9ebfa17f6c4de8,
      i18nMessages.m765b4a5b3324e263,
    ) +
    panel(
      i18nMessages.me51da3f7bf33afbe,
      `${i18nMessages.mc8269ec6b30183e1}${modes
        .map(([value, text]) => `<option value="${value}">${text}</option>`)
        .join("")}${i18nMessages.m47e7bf906f22b97d}`,
    ) +
    panel(
      "Agent",
      `<div class="panel-body hint" data-scope-note>${h(
        SCOPE_NOTES[scope],
      )}${i18nMessages.m8ae0520495f4dc87}${h(
        SCOPE_LABELS.prompt,
      )}</button><button type="button" role="tab" data-scope-tab="plugin" aria-selected="false">${h(
        SCOPE_LABELS.plugin,
      )}${i18nMessages.mffaace0c020824cc}`,
    );

  const monogram = (name) => (String(name).trim()[0] || "·").toUpperCase();

  const iconHTML = (t) => {
    const icon = hostIcons[t.id];
    if (!icon) return h(monogram(t.name));
    if (icon.img) return `<img src="${h(icon.img)}" alt="">`;
    return icon.svg;
  };

  // Scoped actions inside the details follow the active tab: the prompt tab
  // only writes or removes prompts, the plugin tab only installs plugins.
  const detailActionsHTML = (t) => {
    const buttons = [];
    if (scope === "prompt") {
      if (t.state === "fresh")
        buttons.push(
          `<button class="danger" data-scope="${h(t.id)}${i18nMessages.mc54b66c7e4f30b7c}`,
        );
      else
        buttons.push(
          `<button class="primary" data-scope="${h(t.id)}" data-enable="true" data-only="prompt">${
            t.state === "stale" ? i18nMessages.mca9dd05e7f3de28c : i18nMessages.m92b304bb79ab9211
          }</button>`,
        );
    }
    if (scope === "plugin" && t.plugin) {
      buttons.push(
        `<button class="primary" data-scope="${h(t.id)}${i18nMessages.mcf49b9c954baf852}`,
      );
      buttons.push(
        `<button data-repo="${h(t.plugin.id)}${i18nMessages.m1969b07ab4adfe45}`,
      );
      // The gh-proxy mirror is a China-only convenience.
      if (currentLanguage() === "zh")
        buttons.push(
          `<button data-mirror="${h(t.plugin.id)}${i18nMessages.m4f8c5dc36eaf9542}`,
        );
    }
    return buttons.length ? `<div class="actions">${buttons.join("")}</div>` : "";
  };

  const statusLine = (t) => {
    const parts = [agentStatus(t, scope)];
    if (scope === "plugin" && t.plugin) {
      parts.push(t.plugin.command ? i18nMessages.mdfe7eb8847e36d8f : i18nMessages.mc52401866498fa75);
    } else if (t.path) {
      parts.push(t.path);
    }
    return parts.join(" · ");
  };

  const factsHTML = (t) => {
    const pluginSide = scope === "plugin";
    // Only plugin-only hosts carry the plugin directory in path/realPath; a
    // dual host's row shows the prompt file there, so its plugin side
    // reports the prepared state instead of a wrong path.
    const sourceValue = pluginSide
      ? t.mode === "plugin"
        ? t.realPath || t.path
        : t.plugin?.source
          ? i18nMessages.m25464bca0534f2a1
          : i18nMessages.m59c39e4128fb9877
      : t.realPath || t.path;
    const lines = [
      [pluginSide ? i18nMessages.m3f91640010a77713 : i18nMessages.mddcb43323487557c, sourceValue],
      [
        pluginSide ? i18nMessages.m9c320cc7909ff27b : i18nMessages.mfb0413772e2fc30a,
        pluginSide
          ? t.plugin?.auto
            ? i18nMessages.ma3e20470c56b47a8
            : i18nMessages.m9be68868db613778
          : `${modeNames[t.mode] || t.mode} · ${modeHints[t.mode] || ""}`,
      ],
    ];
    if (t.plugin) {
      if (t.plugin.profiles) lines.push(["Profile", t.plugin.profiles]);
      if (t.plugin.manual) lines.push([i18nMessages.m4262c45dc797bb0c, t.plugin.manual]);
    }
    return `<dl class="facts">${lines
      .map(
        ([label, value]) =>
          `<div><dt>${h(label)}</dt><dd>${h(value ?? "—")}</dd></div>`,
      )
      .join("")}</dl>`;
  };

  const rowHTML = (t) => {
    const promptSide = scope === "prompt";
    const on = promptSide ? t.state === "fresh" : !!t.plugin?.source;
    const active = promptSide ? on || t.state === "stale" : on;
    const open = openIds.has(t.id);
    return `<div class="ag-row${promptSide && t.state === "stale" ? " drifted" : ""}${
      targetDetected(t) ? "" : " ag-folded"
    }" data-row="${h(t.id)}">
<span class="ag-icon" aria-hidden="true">${iconHTML(t)}</span>
<div class="ag-who">
<div class="ag-name"><strong>${h(t.name)}</strong><span class="ag-tag">${
      modeNames[t.mode] || h(t.mode)
    }</span></div>
<div class="ag-line"><span class="ag-dot ${
      on ? "on" : active ? "stale" : ""
    }"></span><small>${h(statusLine(t))}</small></div>
</div>
<button class="ag-detail" data-detail="${h(t.id)}" aria-expanded="${
      open ? "true" : "false"
    }${i18nMessages.md36602baf054ee60}${
      open ? "\uFE3F" : "\uFE40"
    }</span></button>
<label class="ag-switch"><input type="checkbox" data-toggle="${h(
      t.id,
    )}" data-kind="${h(t.mode)}"${on ? " checked" : ""}><span class="ag-slider"></span><span class="sr-only">${
      promptSide
        ? `${i18nMessages.m803c4082258e8813}${h(t.name)}${i18nMessages.m81efe16fe35d4e6a}`
        : `${i18nMessages.m1a0b2de6f946971f}${h(t.name)}${i18nMessages.m4f49659ae853a212}`
    }</span></label>
<div class="ag-open" data-open="${h(t.id)}"${
      open ? "" : " hidden"
    }>${factsHTML(t)}${detailActionsHTML(t)}</div>
</div>`;
  };

  function render() {
    all("[data-scope-tab]", root).forEach((tab) =>
      tab.setAttribute("aria-selected", String(tab.dataset.scopeTab === scope)),
    );
    const note = $("[data-scope-note]", root);
    if (note) note.textContent = SCOPE_NOTES[scope];
    const list = $("[data-agents]", root);
    if (!targets.length) {
      list.innerHTML = i18nMessages.mde74b669696942ae;
      return;
    }
    // The tab filter always applies; undetected targets stay behind Show more.
    const supported = targetsForScope(targets, scope);
    const inUse = supported.filter((t) => targetDetected(t));
    const rest = supported.filter((t) => !inUse.includes(t));
    const shown = showAll ? supported : inUse;
    list.classList.toggle("busy", busy);
    list.innerHTML =
      (shown.length
        ? shown.map(rowHTML).join("")
        : `<div class="empty">${h(SCOPE_EMPTY[scope])}</div>`) +
      (rest.length
        ? `<div class="ag-fold"><button data-fold>${
            showAll ? i18nMessages.mafd4b783536b8297 : i18nMessages.m20771df81145b8e4
          } ${h(rest.length)}${i18nMessages.m372a59ddeece72c4}`
        : "");
  }

  let loading = false;
  async function load() {
    if (loading) return;
    loading = true;
    attempted = true;
    if (!ctx.desktop?.connected) {
      $("[data-agents]", root).innerHTML =
        i18nMessages.m3a545392696cb79f;
      return;
    }
    try {
      const result = await api("inject-targets");
      if (!root.isConnected) return;
      mode = result.mode || "normal";
      targets = result.targets || [];
      $("[data-mode]", root).value = mode;
      render();
    } catch (error) {
      if (root.isConnected) {
        $("[data-agents]", root).innerHTML = `<div class="empty">${h(
          error.message,
        )}</div>`;
      }
    } finally {
      loading = false;
    }
  }

  const resultMessage = (data) => {
    if (data.mode) {
      return `${i18nMessages.ma0df09237cbfc480}${modes.find(([key]) => key === data.mode)?.[1] || data.mode}${i18nMessages.m44a991230350086e}${data.updated ?? 0}${i18nMessages.mf86f791b48c0617e}`;
    }
    return agentResultMessage(data);
  };

  // Every apply on this page carries the active tab's scope; the combined
  // scope=all switch is no longer exposed here.
  async function setTarget(id, enable, opScope = scope) {
    const target = targets.find((t) => t.id === id);
    if (!target) return;
    if (!enable && opScope === "plugin") {
      notify(i18nMessages.m65a4f13eab0a1e80, true);
      render();
      return;
    }
    const what =
      opScope === "plugin"
        ? i18nMessages.m4fb2eff3ea192f04
        : enable
          ? `${i18nMessages.m75527e5b5cba4a88}${target.state === "stale" ? i18nMessages.mbbc370df609f593e : i18nMessages.m5c783c4679655185} ${target.name}${i18nMessages.m0f1dabb4c2c9530b}`
          : `${i18nMessages.m2d48350ef103995f}${target.name}${i18nMessages.m54b560667bb94d9a}`;
    const label =
      opScope === "plugin"
        ? i18nMessages.medc3f02843ff9f0a
        : enable
          ? `${i18nMessages.m99d63ea41429f002}${target.name}`
          : i18nMessages.mc2fe03fc40bd7a53;
    if (
      !(await confirm(
        label,
        what,
        opScope === "plugin" ? i18nMessages.me8f88f51ccb085ea : enable ? i18nMessages.mf4f0ead1116b5b62 : i18nMessages.m6135d4159e892541,
      ))
    ) {
      render();
      return;
    }
    busy = true;
    render();
    try {
      const result = await api("inject-set", { id, enable, scope: opScope });
      notify(resultMessage(result));
    } catch (error) {
      notify(error.message, true);
    }
    busy = false;
    await load();
  }

  root.addEventListener("change", (event) => {
    const input = event.target.closest("[data-toggle]");
    if (!input) return;
    if (busy) {
      input.checked = !input.checked;
      return;
    }
    setTarget(input.dataset.toggle, input.checked, scope);
  });

  root.addEventListener("click", (event) => {
    const button = event.target.closest("button");
    if (!button) return;
    if (button.dataset.scopeTab) {
      if (busy || scope === button.dataset.scopeTab) return;
      // Switching tabs keeps page state but never carries the other tab's
      // fold/results into this one's actions.
      scope = button.dataset.scopeTab;
      showAll = false;
      render();
      return;
    }
    if (button.dataset.detail) {
      const id = button.dataset.detail;
      if (openIds.has(id)) openIds.delete(id);
      else openIds.add(id);
      render();
    }
    if (button.hasAttribute("data-fold")) {
      showAll = !showAll;
      render();
    }
    if (button.hasAttribute("data-refresh")) action(button, load);
    if (button.hasAttribute("data-apply-mode"))
      action(button, async () => {
        if (
          !(await confirm(
            i18nMessages.mee98f60c4d11e39f,
            i18nMessages.mf6cc50465a1611f9,
            i18nMessages.m63c73c4730f4473e,
          ))
        )
          return;
        busy = true;
        render();
        try {
          const result = await api("inject-mode", {
            mode: $("[data-mode]", root).value,
          });
          notify(resultMessage(result));
        } finally {
          busy = false;
        }
        await load();
      });
    if (button.dataset.scope)
      setTarget(
        button.dataset.scope,
        button.dataset.enable === "true",
        button.dataset.only,
      );
    if (button.dataset.repo)
      action(button, () =>
        api("open-releases", { target: "plugin-" + button.dataset.repo }),
      );
    if (button.dataset.mirror)
      action(button, () =>
        api("open-releases", {
          target: "plugin-mirror-" + button.dataset.mirror,
        }),
      );
  });

  // Disconnected Runtime: an honest explanation, never cached or fake
  // targets; re-detect becomes available once connected again.
  if (ctx.desktop?.connected) load();
  else
    $("[data-agents]", root).innerHTML =
      i18nMessages.m3a545392696cb79f;
  return {
    refresh: load,
    update() {
      if (!attempted && ctx.desktop?.connected) load();
    },
  };
}
