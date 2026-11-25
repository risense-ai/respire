import { messages as i18nMessages } from "../ui/i18n/messages.js";
import {
  $,
  all,
  h,
  raw,
  api,
  execute,
  action,
  confirm,
  notify,
  number,
  date,
  bytes,
  summary,
  panel,
  pairs,
  resultView,
  select,
} from "../ui/core.js";
import { heading } from "./overview.js";
import { typographyPanel } from "../ui/typography.js";
import { storagePanel } from "./storage.js";
import { proxyRows } from "../ui/proxy.js";
import { currentLanguage } from "../ui/i18n.js";
import { recoveryMaterialDialog, continueAuthentication } from "./authentication.js";

function setting(label, description, control) {
  return `<div class="setting-row"><div><strong>${h(label)}</strong><p>${h(description)}</p></div>${control}</div>`;
}
export function settings(root, ctx) {
  let initialized = false,
    agentAttempted = false,
    savedWorkspace,
    savedDiary;
  root.innerHTML =
    heading(i18nMessages.mdf3d58c7d84b85f2, i18nMessages.m1a1ca5761657ed17) +
    panel(
      i18nMessages.m86a63f23a076b11e,
      setting(
        i18nMessages.m788db1cfec2a3db5,
        i18nMessages.m236fdfb8b6db3d84,
        `${i18nMessages.me24cfa06d6d99885}`,
      ) +
        setting(
          i18nMessages.mfaa2f2ed67357847,
          i18nMessages.m10e696d534030dd1,
          `${i18nMessages.mf955d1db67ab37c6}`,
        ) +
        setting(
          i18nMessages.m9f6fee1aba17a565,
          i18nMessages.mf8df890cf9e92109,
          i18nMessages.m9db37385555efad4,
        ),
    ) +
    panel(
      i18nMessages.m56410fc65314dfb5,
      setting(
        i18nMessages.mc96aebea8f89badd,
        i18nMessages.md856dc96a4b10180,
        i18nMessages.m63dc784dc4534205,
      ) +
        setting(
          i18nMessages.m50fe04340ab6371e,
          i18nMessages.m67f4d47eaf567f60,
          i18nMessages.m7877984c14fd1a8f,
        ),
    ) +
    panel(
      i18nMessages.m97b31b5d63f57e51,
      setting(
        i18nMessages.m5e84ea61e8386af7,
        i18nMessages.m2730e94e472c575a,
        i18nMessages.m1105c0d59ee97fd1,
      ) + `<div class="panel-body" data-proxy></div>`,
    ) +
    panel(
      i18nMessages.mbe10054ab72d8092,
      `<form data-runtime class="panel-body stack"><div class="form-grid">${select(
        "workspace",
        i18nMessages.m966cfe3f0c0fef25,
        [
          ["normal", i18nMessages.m296de0e31f8c22d9],
          ["readonly", i18nMessages.m3b5ec3533b0e4485],
          ["off", i18nMessages.m8d12fc0d4eb26021],
        ],
        "normal",
      )}${select(
        "diary",
        i18nMessages.m156570ebf57f4733,
        [
          ["concise", i18nMessages.m4c1a7dfcf0876247],
          ["verbose", i18nMessages.m41afd9e8b973783f],
        ],
        "concise",
      )}${i18nMessages.mf34040eb1db0f811}`,
    ) +
    panel(
      i18nMessages.m11e17ed647028b14,
      `${i18nMessages.m2ed94d580e2de2ca}`,
    ) +
    panel(
      i18nMessages.m67249ea987b10faf,
      `${i18nMessages.m400dc23342f81b2c}`,
    ) +
    panel(
      i18nMessages.m52d25a9e30ba94f1,
      `${i18nMessages.m08170f4034fdb980}`,
    );
  root.addEventListener("change", (event) => {
    const control = event.target;
    if (control.dataset.pref)
      action(control, async () => {
        if (control.dataset.pref === "language") {
          await ctx.changeLanguage(control.value);
          return;
        }
        await ctx.savePreferences({
          [control.dataset.pref]:
            control.type === "checkbox" ? control.checked : control.value,
        });
        if (control.dataset.pref === "proxyMode") await ctx.refresh();
        notify(i18nMessages.mbd03c0aad701a8ca);
      });
    if (control.hasAttribute("data-autostart"))
      action(control, async () => {
        try {
          await api("autostart", { enabled: control.checked });
          notify(i18nMessages.me42a689a9f762502);
        } catch (error) {
          control.checked = !control.checked;
          throw error;
        }
      });
  });
  root.addEventListener("click", (event) => {
    const button = event.target.closest("button");
    if (!button) return;
    if (button.hasAttribute("data-wizard-reopen")) {
      ctx.openWizard?.();
      return;
    }
    if (button.dataset.language)
      action(button, async () => {
        await api("cli-language", { language: button.dataset.language });
        notify(i18nMessages.ma43ada4536138fb9);
      });
    if (button.dataset.file)
      action(button, async () => {
        const kind = button.dataset.file,
          path = await api("file-dialog", { kind });
        if (!path) return;
        const names = {
          export: "memory_export",
          import: "memory_import",
          backup: "memory_backup",
          "share-import": "memory_share_preview",
        };
        if (
          kind === "import" &&
          !(await confirm(
            i18nMessages.m6313d49468e9ddd2,
            `${i18nMessages.m8f3251cb3f7a35d9}`,
            i18nMessages.m576d81bb0631b165,
          ))
        )
          return;
        const result = await execute(names[kind], { path });
        if (kind === "share-import") {
          const choice = await confirm(
            i18nMessages.m8c8630357402f4d1,
            raw(summary(result)),
            i18nMessages.m4a8d6841b4023edf,
          );
          if (choice) await execute("memory_share_import", { path });
        }
        notify(i18nMessages.md86a3270cf8d5075);
        await ctx.refresh();
      });
    if (button.dataset.prefFile)
      action(button, async () => {
        const kind = button.dataset.prefFile,
          path = await api("file-dialog", { kind });
        if (!path) return;
        if (
          kind === "preferences-import" &&
          !(await confirm(i18nMessages.m1a80d4a192698b02, i18nMessages.m4a54f1625b995d75, i18nMessages.m576d81bb0631b165))
        )
          return;
        await api(kind === "preferences-import" ? "preferences-import" : "preferences-export", { path });
        notify(kind === "preferences-import" ? i18nMessages.mec7ffe953ccdc02a : i18nMessages.m022ada6c95af2255);
        await ctx.refresh();
      });
  });
  $("[data-runtime]", root).addEventListener("submit", (event) => {
    event.preventDefault();
    action($("[type=submit]", event.target), async () => {
      const values = Object.fromEntries(new FormData(event.target));
      let workspace;
      if (values.workspace !== savedWorkspace) {
        workspace = summary(await execute("agent_set", {
          setting: "workspace_mode=" + values.workspace,
        }));
        savedWorkspace = workspace.workspace_mode;
        $("[name=workspace]", event.target).value = savedWorkspace;
      }
      if (values.diary !== savedDiary) {
        await execute("agent_set", { setting: "diary_mode=" + values.diary });
        savedDiary = values.diary;
        if (!workspace) notify(i18nMessages.m382a23dcffd35e79);
      }
      if (workspace) {
        if (!workspace.runtime_applied) notify(i18nMessages.m0767c6924b7b8802);
        else if (workspace.workspace_mode !== "off" && !workspace.store_ready)
          notify(i18nMessages.m9be4ddc458b6e714);
        else notify(i18nMessages.m9d2bb51e8d033934);
      }
      await ctx.refresh();
    });
  });
  function update() {
    const state = ctx.desktop;
    if (!state) return;
    if (!initialized) {
      all("[data-pref]", root).forEach((node) => {
        const value = state.preferences[node.dataset.pref];
        if (node.type === "checkbox") node.checked = Boolean(value);
        else node.value = value;
      });
      $("[data-autostart]", root).checked = state.autostart;
      initialized = true;
    }
    if (!agentAttempted) {
      agentAttempted = true;
      execute("agent_config")
        .then((envelope) => {
          if (!root.isConnected) return;
          const value = summary(envelope);
          savedWorkspace = value.workspace_mode || "normal";
          $("[name=workspace]", root).value = savedWorkspace;
          savedDiary = value.diary_mode || "concise";
          $("[name=diary]", root).value = savedDiary;
        })
        .catch((error) => notify(error.message, true));
    }
    // The release-channel select lives on the updates page now; the guard
    // keeps update() from aborting here (it silently emptied the paths and
    // about rows below).
    const channel = $("[data-pref=runtimeChannel]", root);
    if (channel) channel.disabled = state.task?.busy;
    $("[data-paths]", root).innerHTML = pairs([
      [i18nMessages.m210e6097e83a266a, state.root],
      [i18nMessages.m38fe89326eb745c7, ctx.status?.summary?.data_dir],
      [i18nMessages.m2634666b5faad646, state.endpoint],
    ]);
    $("[data-about-version]", root).innerHTML = pairs([
      [i18nMessages.m837bc95767211631, state.version || "—"],
      [i18nMessages.m910887705c7a66d2, state.platform || "—"],
    ]);
    const proxy = state.proxy || {};
    $("[data-proxy]", root).innerHTML = pairs(proxyRows(proxy));
  }
  typographyPanel(root);
  storagePanel(root, ctx);
  update();
  return {
    update,
    refresh() {
      initialized = false;
      agentAttempted = false;
      update();
    },
  };
}

const updateNames = {
  idle: i18nMessages.m8f31a776d53b824a,
  checking: i18nMessages.md85ff08f9141848c,
  "up-to-date": i18nMessages.mbc310480b4ce9236,
  available: i18nMessages.m208628087b89e85c,
  downloading: i18nMessages.me2fc571a4cf00ceb,
  verifying: i18nMessages.m784f9b54ae2ba27b,
  installing: i18nMessages.mb53be09930f4e02d,
  ready: i18nMessages.m1a30745b69a190a4,
  error: i18nMessages.mec99e5c45d648fd6,
};
export function updates(root, ctx) {
  let latest = null;
  function update() {
    const state = ctx.desktop || {},
      u = state.clientUpdate || {},
      task = state.task || {},
      p = state.preferences || {};
    const channelRow = (pref) =>
      `${i18nMessages.mcb1faa45161fd7ca}${pref}"><option value="stable">Stable</option><option value="dev">Dev</option></select></div>`;
    // The gh-proxy release mirror is a China-only convenience.
    const mirrorButton = (target) =>
      currentLanguage() === "zh"
        ? `<button data-open-releases="${h(target)}${i18nMessages.m6db88eefd5afef8f}`
        : "";
    root.innerHTML =
      heading(
        i18nMessages.m18b7eb9a19d1092c,
        i18nMessages.m58c03fb91e774f29,
      ) +
      `<div class="grid">${panel(
        i18nMessages.mc574f97e4f5d7c04,
        `${i18nMessages.m8514ebb252075638}` +
        `${i18nMessages.m9ff50f045b87f60e}` +
        `${i18nMessages.m56aa40c8507660a8}` +
        channelRow("clientChannel") +
        pairs([
          [i18nMessages.m837bc95767211631, state.version],
          [i18nMessages.m6320b4a8722a851f, updateNames[u.state] || u.state || "—"],
          [i18nMessages.m2c4a7f77e6ffede6, u.latest || "—"],
          [i18nMessages.mde0e6877cfcba3c6, u.source || "—"],
        ]) +
          `${i18nMessages.m1b5627a1afaf9dbb}${u.state === "ready" ? "disabled" : ""}>${u.error ? i18nMessages.m87dd9db3387c472b : i18nMessages.ma78b542de2824eb2}</button><button data-update="client-restart" ${u.state !== "ready" ? "disabled" : ""}${i18nMessages.m923d39dbdbdf3627}${u.cancellable ? `${i18nMessages.m21ae538897c283fe}` : ""}</div><div class="panel-body upd-actions upd-links"><button data-open-releases="client">GitHub Releases</button>${mirrorButton("client-mirror")}</div>${u.error ? `<div class="panel-body form-error">${h(u.error)}</div>` : ""}${u.notes ? `${i18nMessages.m24d0107830848728}${h(u.notes)}</pre></details>` : ""}`,
      )}
      ${panel(
        "CLI / Runtime",
        `${i18nMessages.m26fd974874b15693}` +
        `${i18nMessages.m2d8224f87d8132f4}` +
        `${i18nMessages.m63d577f21e55b9bf}` +
        channelRow("runtimeChannel") +
        pairs([
          [i18nMessages.m7e7936979442884c, state.health?.bin || i18nMessages.m3d52574ce1500561],
          [i18nMessages.ma8b6c39dcabfc14b, state.installed?.version || (state.externalRuntime ? `${state.externalRuntime.version}${i18nMessages.mc7d325a79d41fb1a}` : i18nMessages.m156219e305f6cebc)],
          [i18nMessages.m2c4a7f77e6ffede6, latest?.version || i18nMessages.mcdbc0434f568fc9e],
          [i18nMessages.mde0e6877cfcba3c6, latest?.channel || "—"],
        ]) +
          `${i18nMessages.m3ea346554149d615}${task.busy ? "disabled" : ""}${i18nMessages.m91cd5717ac029e69}${task.busy || !state.installed?.version || state.installed.version === (state.health?.bin || "") ? "disabled" : ""}${i18nMessages.m8b10605c11d78b75}${mirrorButton("cli-mirror")}</div>`,
      )}</div>` +
      panel(
        i18nMessages.ma0e76ee2479a3bd7,
        `<div class="panel-body"><p>${h(task.stage || i18nMessages.mb073db9c0bd6f2ed)}</p>${task.total ? `<div class="progress-label"><span>${h(bytes(task.done))} / ${h(bytes(task.total))}</span></div><progress value="${h(task.done)}" max="${h(task.total)}"></progress>` : ""}${task.error ? `<p class="form-error section-gap">${h(task.error)}</p>` : ""}</div>`,
      ) +
      `${i18nMessages.m040d2e4c3995968e}`;
    all("[data-pref]", root).forEach((node) => {
      const value = p[node.dataset.pref];
      if (node.type === "checkbox") node.checked = Boolean(value);
      else if (node.dataset.pref.endsWith("Channel")) node.value = value || "stable";
      else node.value = value || "6h";
    });
  }
  root.addEventListener("change", (event) => {
    const control = event.target;
    if (!control.dataset.pref) return;
    action(control, async () => {
      await ctx.savePreferences({
        [control.dataset.pref]:
          control.type === "checkbox" ? control.checked : control.value,
      });
      notify(i18nMessages.mbd03c0aad701a8ca);
    });
  });
  root.addEventListener("click", (event) => {
    const button = event.target.closest("button");
    if (!button) return;
    action(button, async () => {
      if (button.dataset.openReleases) {
        await api("open-releases", { target: button.dataset.openReleases });
      } else if (button.hasAttribute("data-cli-check")) {
        latest = await api("runtime-release");
        update();
      } else if (button.dataset.update) {
        await api(button.dataset.update === "client-cancel" ? "client-cancel" : button.dataset.update, {});
        await ctx.refresh();
      }
    });
  });
  update();
  return { update, refresh: update };
}

export function diagnostics(root, ctx) {
  root.innerHTML =
    heading(
      i18nMessages.m40ff6300f9817deb,
      i18nMessages.me9cb2cad7c39d234,
      `${i18nMessages.m4933731ec4712d4b}`,
    ) +
    `<div data-result></div>` +
    panel(
      i18nMessages.m82479cb6ca73042d,
      `${i18nMessages.m3758ff4ad78f5824}`,
    );
  function update() {
    if (!ctx.desktop) return;
    $("[data-connection]", root).innerHTML = pairs([
      ["RPC", ctx.desktop.endpoint],
      [i18nMessages.m7603b8958dba6bc5, ctx.desktop.root],
      [i18nMessages.ma5574109f0208e89, ctx.desktop.connected ? i18nMessages.m5be0323e8adcaeae : i18nMessages.m3d52574ce1500561],
      [i18nMessages.m0bc1fb72ae1be5c5, ctx.statusError || i18nMessages.m484d55613910eb8c],
    ]);
    $("[data-health]", root).textContent = raw(ctx.desktop.health || {});
  }
  $("[data-doctor]", root).addEventListener("click", (event) =>
    action(event.currentTarget, async () => {
      let value;
      try {
        value = await execute("doctor");
      } catch (error) {
        if (!error.envelope) throw error;
        value = error.envelope;
      }
      if (root.isConnected)
        $("[data-result]", root).innerHTML = resultView(i18nMessages.m7c5569c38a3c03db, value);
    }),
  );
  $("[data-diagnostic-export]", root).addEventListener("click", (event) =>
    action(event.currentTarget, async () => {
      const path = await api("file-dialog", { kind: "diagnostics-export" });
      if (!path) return;
      await api("diagnostics-export", { path });
      notify(i18nMessages.m770bd0f9d7a9d22a);
    }),
  );
  update();
  return { update, refresh: update };
}

export function operations(root) {
  root.innerHTML =
    heading(
      i18nMessages.mb2db0d03bb7d08ca,
      i18nMessages.m815f0f40c131dbf5,
    ) +
    `${i18nMessages.m4e0e68794e5c20fa}`;
  async function refresh() {
    try {
      const jobs = await api("operations");
      if (!root.isConnected) return;
      $("[data-jobs]", root).innerHTML = panel(
        i18nMessages.m7047b8672ca4bb1a,
        jobs.length
          ? `${i18nMessages.mebe15b345c32d5b0}${jobs.map((job) => `<tr><td>${h(job.name)}</td><td>${h({ running: i18nMessages.m5026a63b58cdf048, done: i18nMessages.mf28461bb49c85647, failed: i18nMessages.m28384d7afd2e4fa6, unconfirmed: i18nMessages.me6bedaccc2650c55 }[job.state] || job.state)}${job.error ? `<p class="form-error">${h(job.error)}</p>` : ""}</td><td>${h(date(job.started))}</td><td><code>${h(job.id)}</code></td><td>${job.state === "unconfirmed" ? `<button data-recover="${h(job.id)}${i18nMessages.m2759bbe5aa97c027}` : ""}</td></tr>`).join("")}</tbody></table></div>`
          : i18nMessages.m67a12d4e96959ae0,
      );
      all("tbody tr", root).forEach((row, index) => {
        const job = jobs[index];
        if (job.state === "running") return;
        if (job.materialPending && job.state === "done") {
          const recover = document.createElement("button");
          recover.textContent = i18nMessages.m809cb6eded0057fd;
          recover.dataset.recover = job.id;
          row.lastElementChild.append(recover);
        }
        if (job.input) {
          const resume = document.createElement("button");
          resume.textContent = i18nMessages.m9b6f5ef5f6712555;
          resume.dataset.continue = job.id;
          row.lastElementChild.append(resume);
        }
        const control = document.createElement("button");
        control.textContent = i18nMessages.me96cff0f0005ac2c;
        control.dataset.receipt = job.id;
        row.lastElementChild.append(control);
      });
    } catch (error) {
      notify(error.message, true);
    }
  }
  root.addEventListener("click", (event) => {
    const receipt = event.target.closest("[data-receipt]");
    if (receipt?.dataset.receipt)
      action(receipt, async () => {
        const job = await api(
          "operation?id=" + encodeURIComponent(receipt.dataset.receipt),
        );
        $("div[data-receipt]", root).innerHTML = job.envelope
          ? resultView(i18nMessages.m62ecca751abbb39e, job.envelope)
          : panel(
              i18nMessages.m62ecca751abbb39e,
              i18nMessages.mfb012fd60786e993,
            );
      });
    const resume = event.target.closest("[data-continue]");
    if (resume)
      action(resume, async () => {
        try {
          const envelope = await continueAuthentication(resume.dataset.continue);
          recoveryMaterialDialog(envelope);
          notify(i18nMessages.m2d8df94a2df7a793);
        } finally {
          await refresh();
        }
      });
    const button = event.target.closest("[data-recover]");
    if (button)
      action(button, async () => {
        const job = await api("operation-recover", {
          id: button.dataset.recover,
        });
        if (job.state === "done") recoveryMaterialDialog(job.envelope, { requestId: job.materialPending ? job.id : undefined });
        notify(
          job.state === "done" ? i18nMessages.m8364e0b874b872c4 : job.error || i18nMessages.mac2148b35f2d4e58,
          job.state !== "done",
        );
        await refresh();
      });
  });
  refresh();
  return { refresh, update: refresh };
}
