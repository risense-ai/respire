import { messages as i18nMessages } from "../ui/i18n/messages.js";
import {
  $,
  h,
  raw,
  rows,
  api,
  execute,
  action,
  notify,
  panel,
  input,
  dialog,
  confirm,
  showText,
  resultView,
} from "../ui/core.js";
import { heading } from "./overview.js";
import { invitationDialog, membersDialog } from "./space-members.js";
import { authenticationPanel, executeAuthentication, recoveryMaterialDialog } from "./authentication.js";

// Routes replace the page, but an account action still belongs to the same app.
const accountActionStates = new WeakMap();

export function accounts(root, ctx) {
  let loading = false,
    attempted = false;
  root.innerHTML =
    heading(
      i18nMessages.m811fcc60bd966d24,
      i18nMessages.m57ea6657ee621869,
    ) +
    "<div data-session></div>" +
    `<div class="grid section-gap">${panel(i18nMessages.ma7c0e7f91ed1f125, i18nMessages.m0c5e66006bc11e9a)}${panel(i18nMessages.m3347e052c0f17434, i18nMessages.m685c9cfffccca9fe)}</div>` +
    panel(
      i18nMessages.mcd8487b1e51593f6,
      `<form data-server class="panel-body stack">${input("addr", i18nMessages.md3716cc5a2f5a810, "https://api.rsrs.rs", { required: true })}${i18nMessages.m54ff0dfc19c24571}`,
    ) +
    panel(
      i18nMessages.mf2b2be0655ff3504,
      i18nMessages.m5393e30edb9c0c36,
    ) +
    '<div data-result class="section-gap"></div>';
  let actionState = accountActionStates.get(ctx);
  if (!actionState) {
    actionState = { pending: new Set(), update: null };
    accountActionStates.set(ctx, actionState);
  }
  const sessionRoot = $("[data-session]", root);
  sessionRoot.innerHTML = panel(
    i18nMessages.mfebfae60a4295c23,
    '<div class="identity-grid">' +
      [
        [i18nMessages.m061246bd31e084a7, "user"],
        [i18nMessages.m301fe1eff85781c2, "token"],
        [i18nMessages.m1c7d92560b920d14, "keys"],
        [i18nMessages.mbdb0c9ab5464a2f4, "dir"],
      ]
        .map(
          ([label, field]) =>
            `<div class="identity-cell"><small>${h(label)}</small><strong data-session-field="${field}"></strong></div>`,
        )
        .join("") +
      "</div>" +
      `${i18nMessages.mb9c67a8667d12b22}`,
  );
  function update() {
    const s = ctx.status?.summary || {},
      session = s.session || {},
      hasToken = session.has_token || ctx.desktop?.localSessionHasToken === true;
    const values = {
      user: session.user || i18nMessages.m54392fa24b359913,
      token: hasToken ? i18nMessages.m0a01eee1cd371a58 : i18nMessages.mc1f65837307903eb,
      keys: session.has_local_keys ? i18nMessages.m1bd91a7d0c53c936 : i18nMessages.m07564e6524ba73ad,
      dir: s.data_dir ?? "—",
    };
    for (const [field, value] of Object.entries(values)) {
      const target = $(`[data-session-field="${field}"]`, sessionRoot);
      if (target.textContent !== String(value)) target.textContent = value;
    }
    const disabled = {
      login: false,
      dashboard: false,
      keygen: Boolean(session.has_local_keys),
      keys: !session.has_local_keys,
      logout: !hasToken,
      sync: false,
      conflicts: false,
      migration: false,
      migrate: false,
    };
    for (const [kind, unavailable] of Object.entries(disabled)) {
      const button = $(`[data-account="${kind}"]`, root);
      button.disabled = unavailable || actionState.pending.has(kind);
      if (kind === "logout") button.title = ctx.desktop?.localSessionError || "";
    }
  }
  async function accountAction(button, fn) {
    const kind = button.dataset.account;
    if (button.disabled || actionState.pending.has(kind)) return;
    actionState.pending.add(kind);
    try {
      await action(button, async () => {
        actionState.update?.();
        return fn();
      });
    } finally {
      actionState.pending.delete(kind);
      // The initiating page may be gone; restore the current page's controls.
      actionState.update?.();
    }
  }
  async function load() {
    if (loading) return;
    loading = true;
    attempted = true;
    const results = await Promise.allSettled([
      execute("account_list"),
      execute("space_list"),
    ]);
    if (!root.isConnected) {
      loading = false;
      return;
    }
    const [profileResult, spaceResult] = results;
    if (profileResult.status === "fulfilled") {
      const profiles = rows(profileResult.value, "accounts");
      $("[data-accounts]", root).className = "";
      $("[data-accounts]", root).innerHTML = profiles.length
        ? `<ul class="clean">${profiles.map((item) => `<li class="row"><div class="row-copy"><strong>${h(item.name)}</strong><small>${h(item.user || item.dir || "")}</small></div>${item.current ? i18nMessages.m2d52f59d92a3504a : `<button data-use="${h(item.name)}${i18nMessages.m4b4e132078ac38ad}`}</li>`).join("")}</ul>`
        : i18nMessages.m7a8d8a2984c19381;
    } else
      $("[data-accounts]", root).textContent = profileResult.reason.message;
    if (spaceResult.status === "fulfilled") {
      const spaces = rows(spaceResult.value, "spaces");
      $("[data-spaces]", root).innerHTML = spaces.length
        ? `<ul class="clean">${spaces.map((item) => `<li class="row"><div class="row-copy"><strong>${h(item.name)}</strong><small>${h(item.role || item.kind || "")}</small></div>${item.current ? i18nMessages.mcaf22de6f8c631b3 : `<button data-space-use="${h(item.id ? "v1:" + item.id : item.name)}${i18nMessages.m48ab9f10de3bb930}`}${item.is_main || item.current ? "" : `<button class="danger" data-space-remove="${h(item.id ? "v1:" + item.id : item.name)}${i18nMessages.mf2e5d6dbb59fd2c8}`}</li>`).join("")}</ul>`
        : i18nMessages.m28b00c1b72e7fbc1;
    } else $("[data-spaces]", root).textContent = spaceResult.reason.message;
    $("[name=addr]", root).value = ctx.desktop?.serverAddress || "";
    $("[name=autosync]", root).checked = ctx.status?.summary?.autosync === true;
    if (ctx.desktop?.serverAddressError) notify(ctx.desktop.serverAddressError, true);
    loading = false;
  }
  const commands = {
    sync: "sync",
    conflicts: "sync_conflicts",
    migration: "migration_list",
  };
  root.addEventListener("click", (event) => {
    const button = event.target.closest("button");
    if (!button) return;
    if (button.dataset.use)
      action(button, async () => {
        if (
          !(await confirm(
            i18nMessages.m83fc369772fbeb5e,
            `${i18nMessages.mb4187f649e2e2320}${button.dataset.use}${i18nMessages.m34e97e96b68b52cc}`,
            i18nMessages.meff514b8b83f29ff,
          ))
        )
          return;
        await execute("account_use", { name: button.dataset.use });
        await ctx.refresh();
        await load();
      });
    if (button.dataset.spaceUse)
      action(button, async () => {
        await execute("space_use", { name: button.dataset.spaceUse });
        await ctx.refresh();
        await load();
      });
    if (button.dataset.spaceRemove)
      action(button, async () => {
        if (
          !(await confirm(
            i18nMessages.m74c9530b39883b85,
            `${i18nMessages.m9fad3beb447115a7}${button.dataset.spaceRemove}${i18nMessages.m94565a16c97bcaac}`,
            i18nMessages.m74c9530b39883b85,
          ))
        )
          return;
        await execute("space_remove", { name: button.dataset.spaceRemove });
        await load();
      });
    if (button.dataset.space) {
      const kind = button.dataset.space;
      if (kind === "create" || kind === "join")
        dialog({
          title: kind === "create" ? i18nMessages.m9afb05b80ec154a8 : i18nMessages.m21b1a844ef589335,
          body: input("value", kind === "create" ? i18nMessages.m39d0533d0b209c81 : i18nMessages.mc8b56dca7aed559b, "", {
            required: true,
          }),
          onSubmit: async (value) => {
            await execute(
              kind === "create" ? "space_create" : "space_join",
              kind === "create" ? { name: value.value } : { code: value.value },
            );
            await load();
            await ctx.refresh();
          },
        });
      else
        action(button, async () => {
          if (kind === "members") await membersDialog();
          else invitationDialog();
        });
    }
    if (button.dataset.account)
      accountAction(button, async () => {
        const kind = button.dataset.account;
        if (commands[kind]) {
          const result = await execute(commands[kind]);
          const target =
            kind === "migration" ? "[data-migration]" : "[data-result]";
          $(target, root).innerHTML = resultView(button.textContent, result);
          if (kind === "sync") await ctx.refresh();
          return;
        }
        if (kind === "dashboard") {
          await api("open-dashboard", {});
          return;
        }
        if (kind === "login") {
          notify(i18nMessages.m26d75de37bbed271);
          const result = await executeAuthentication("login");
          if (!recoveryMaterialDialog(result)) notify(i18nMessages.m389103d033d82ba2);
          await ctx.refresh();
          await load();
          return;
        }
        if (kind === "keygen") {
          if (
            !(await confirm(
              i18nMessages.mbe955f267dff1aed,
              i18nMessages.m7a133aae23eb719a,
              i18nMessages.m696325d06ddf10e4,
            ))
          )
            return;
          const result = await execute("keygen");
          if (!recoveryMaterialDialog(result)) notify(i18nMessages.md02a06e0523f7c4b);
          await ctx.refresh();
          await load();
          return;
        }
        if (kind === "keys") {
          const out = await api("file-dialog", { kind: "keys" });
          if (!out) return;
          if (
            !(await confirm(
              i18nMessages.mb9112558e7159f17,
              i18nMessages.mcf90c6146da244e9,
              i18nMessages.m76420433f22d1c80,
            ))
          )
            return;
          await execute("keys_export", { out });
          notify(i18nMessages.ma60acd1ebe1bdae0);
          return;
        }
        if (kind === "logout") {
          if (
            !(await confirm(
              i18nMessages.mcb2ee9707e992d59,
              i18nMessages.m8ccd30facc109584,
              i18nMessages.m498e1d59b4d787ee,
            ))
          )
            return;
          await execute("logout");
          await ctx.refresh();
          await load();
          return;
        }
        if (kind === "migrate")
          dialog({
            title: i18nMessages.m779f5c8d6c7d5780,
            body:
              input("source", i18nMessages.mabf331d3213483e0, "", { required: true }) +
              input("account", i18nMessages.mc8c573bc91b458ea, "", { required: true }),
            submit: i18nMessages.mc175e224c89a493e,
            onSubmit: async (values) => {
              const result = await execute("migration_apply", values);
              $("[data-migration]", root).innerHTML = resultView(
                i18nMessages.m40a75b17969e76e1,
                result,
              );
              await load();
            },
          });
      });
  });
  $("[data-server]", root).addEventListener("submit", (event) => {
    event.preventDefault();
    action($("[type=submit]", event.target), async () => {
      const value = Object.fromEntries(new FormData(event.target));
      await execute("server_set", { addr: value.addr.trim() });
      await execute("autosync_set", { enabled: Boolean(value.autosync) });
      notify(i18nMessages.m203fe077b567bad5);
      await ctx.refresh();
    });
  });
  authenticationPanel(root, ctx, load);
  actionState.update = update;
  update();
  if (ctx.desktop?.connected) load();
  return {
    update() {
      update();
      if (!attempted && ctx.desktop?.connected) load();
    },
    refresh: load,
    dispose() {
      if (actionState.update === update) actionState.update = null;
    },
  };
}
