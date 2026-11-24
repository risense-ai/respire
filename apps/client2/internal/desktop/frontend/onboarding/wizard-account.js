import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { $, all, h, execute, confirm, notify } from "../ui/core.js";
import { setLanguage, t } from "../ui/i18n.js";
import {
  issuedRecoveryMaterial,
  recoveryMaterialJson,
  runAuthentication,
  validateAuthentication,
} from "../pages/authentication.js";

export function accountSteps(wizard, {
  ctx,
  frame,
  card,
  render,
  goto,
  backButton,
  saveStep,
  connected,
  pause,
  setError,
  root,
  session,
}) {
  function renderLanguage(body) {
    if (!wizard.state.language)
      wizard.state.language =
        ctx.desktop?.preferences?.language ||
        localStorage.getItem("respire-language") ||
        "";
    body.innerHTML = frame(
      i18nMessages.mb90a8ac9c488ce46,
      i18nMessages.madfb73bb6ac85606,
      `<div class="wizard-choices">${card("English", i18nMessages.mde5f284d2c17e1d2, "en", { active: wizard.state.language === "en" })}${card(i18nMessages.m72726d8818f69306, i18nMessages.m14ea21c316ef92d4, "zh", { active: wizard.state.language === "zh" })}</div>`,
      `<button type="button" class="primary" data-next-language${wizard.state.language ? "" : " disabled"}${i18nMessages.m58ecca80f9403242}`,
    );
    all("[data-choice]", body).forEach((choice) =>
      choice.addEventListener("click", () => {
        if (wizard.busy) return;
        wizard.state.language = choice.dataset.choice;
        applyLanguage(wizard.state.language);
        render();
      }),
    );
    $("[data-next-language]", body).addEventListener("click", () => {
      if (wizard.busy || !wizard.state.language) return;
      goto("sync");
    });
  }

  // Selecting a card applies the language at once so the wizard and the UI
  // behind it retranslate; Next advances without re-saving.
  function applyLanguage(value) {
    setLanguage(value);
    ctx.savePreferences({ language: value }).catch((error) => {
      notify(error.message, true);
    });
  }

  function renderSync(body) {
    body.innerHTML = frame(
      i18nMessages.mc549c8aede64ab1a,
      "",
      `<div class="wizard-choices">${card(
        i18nMessages.m8d76177c725442d8,
        i18nMessages.mdbaabd8a76fcada7,
        "cloud",
        { badge: i18nMessages.m1452deafc6d96546, active: wizard.state.syncChoice === "cloud" },
      )}${card(
        i18nMessages.m74aaaf8aef069164,
        i18nMessages.m3efe96d460458672,
        "offline",
        { active: wizard.state.syncChoice === "offline" },
      )}</div>`,
      backButton("language") +
        `<button type="button" class="primary" data-next-sync${wizard.state.syncChoice ? "" : " disabled"}${i18nMessages.m58ecca80f9403242}`,
    );
    all("[data-choice]", body).forEach((choice) =>
      choice.addEventListener("click", () => {
        if (wizard.busy) return;
        wizard.state.syncChoice = choice.dataset.choice;
        render();
      }),
    );
    $("[data-next-sync]", body).addEventListener("click", () => {
      if (wizard.busy || !wizard.state.syncChoice) return;
      chooseSync(wizard.state.syncChoice);
    });
  }

  async function chooseSync(value) {
    if (wizard.busy) return;
    wizard.busy = true;
    try {
      wizard.state.syncChoice = value;
      wizard.state.error = "";
      if (value === "cloud") {
        await saveStep("account", { onboardingSyncChoice: value });
        wizard.state.step = "account";
      } else {
        await saveStep("model", { onboardingSyncChoice: value });
        wizard.state.step = "model";
      }
      render();
    } finally {
      wizard.busy = false;
    }
  }

  function renderAccount(body) {
    if (wizard.state.accountPhase === "autosync") return renderAutosync(body);
    if (!connected()) return renderAccountOffline(body);
    const mode = wizard.state.accountMode;
    if (ctx.desktop?.serverAddressError) {
      body.innerHTML = frame(i18nMessages.m8a4d761757a32507, "", `<p class="form-error">${h(ctx.desktop.serverAddressError)}</p>`, backButton("sync"));
      return;
    }
    const server = ctx.desktop?.serverAddress || "";
    body.innerHTML = frame(
      i18nMessages.m8a4d761757a32507,
      i18nMessages.m359c66a0345e40bb,
      `<div class="wizard-tabs" role="tablist"><button type="button" role="tab" data-mode="register" aria-selected="${mode === "register"}${i18nMessages.m324a82c6a0281ef1}${mode === "login"}${i18nMessages.m7bb9ae0a3f436f07}${h(server)}${i18nMessages.md29984c8f7b0f022}${
        mode === "register"
          ? i18nMessages.m1fc5df4f7f41d799
          : i18nMessages.m6e94a8cbb215ef04
      }</form>`,
      `${i18nMessages.m3b8d4b05af04284b}${mode === "register" ? i18nMessages.m733dee6d88971ae1 : i18nMessages.m1e2df9c3075ae9e4}</button>`,
    );
    all("[data-mode]", body).forEach((tab) =>
      tab.addEventListener("click", () => {
        wizard.state.accountMode = tab.dataset.mode;
        wizard.state.error = "";
        render();
      }),
    );
    $("#wizard-auth-form", body).addEventListener("submit", (event) => {
      event.preventDefault();
      submitAuthentication(Object.fromEntries(new FormData(event.target)));
    });
  }

  // Runtime/CLI unavailable: offer the updates page or an explicit,
  // re-confirmed offline fallback. A failed auth must never be recorded as
  // a configured cloud account.
  function renderAccountOffline(body) {
    body.innerHTML = frame(
      i18nMessages.m3114f5e21228b569,
      i18nMessages.m21655a4b122a1609,
      "",
      `${i18nMessages.m5ce8893d548990e6}${backButton("sync")}`,
    );
    $("[data-open-updates]", body).addEventListener("click", () => {
      pause();
      ctx.navigate("updates");
    });
    $("[data-recheck]", body).addEventListener("click", async () => {
      await ctx.refresh();
      render();
    });
    $("[data-later-offline]", body).addEventListener("click", () => {
      deferOffline();
    });
  }

  async function deferOffline() {
    if (
      !(await confirm(
        i18nMessages.mf6aa5c6982b18cb9,
        i18nMessages.m4ad52683188edd1c,
        i18nMessages.md0f8a634ed29990f,
      ))
    )
      return;
    wizard.state.syncChoice = "offline";
    await goto("model", { onboardingSyncChoice: "offline" });
  }

  // One busy control: spinner + disabled label so an in-flight remote call
  // is visible; the button restores itself on failure without re-rendering
  // (a re-render would wipe the typed credentials).
  function setButtonBusy(button, busyText) {
    if (!button) return () => {};
    const original = button.innerHTML;
    button.disabled = true;
    button.classList.add("wizard-busy");
    button.innerHTML = `<span class="wizard-spinner" aria-hidden="true"></span>${h(busyText)}`;
    return () => {
      button.disabled = false;
      button.classList.remove("wizard-busy");
      button.innerHTML = original;
    };
  }

  async function submitAuthentication(values) {
    if (wizard.busy) return;
    wizard.busy = true;
    await setError("");
    const restore = setButtonBusy(
      $('button[form="wizard-auth-form"]', root),
      wizard.state.accountMode === "register" ? t(i18nMessages.m4429b8ba395646d9) : t(i18nMessages.md8712b0d82e63a46),
    );
    try {
      validateAuthentication(wizard.state.accountMode, values);
      const envelope = await runAuthentication(wizard.state.accountMode, values);
      // Judge by the reported session, not the step marker; a concurrent
      // refresh may still be in flight, so poll briefly for the receipt.
      for (let i = 0; i < 10 && !session().has_token; i++) {
        await new Promise((resolve) => setTimeout(resolve, 300));
        await ctx.refresh();
      }
      if (!session().has_token)
        throw Error(i18nMessages.mdb367cb6aae9f2dd);
      wizard.state.material = issuedRecoveryMaterial(envelope);
      wizard.state.accountPhase = "autosync";
      render();
    } catch (error) {
      setError(error.message);
      restore();
    } finally {
      wizard.busy = false;
    }
  }

  function renderAutosync(body) {
    const keys = session().has_local_keys;
    body.innerHTML = frame(
      i18nMessages.m6e9adb42a28942eb,
      wizard.state.accountMode === "register"
        ? i18nMessages.m2a6d81807e07ec98
        : i18nMessages.m61a06d3f73c9d406,
      `<label class="checkbox-label"><input type="checkbox" name="autosync" ${wizard.state.autoSync ? "checked" : ""}${i18nMessages.m149dfc6dd6c39041}${t(i18nMessages.m1c7d92560b920d14)}${i18nMessages.mf114611cd61000bf}${keys ? t(i18nMessages.mab27f80d046f3d7f) : t(i18nMessages.m07564e6524ba73ad)}${i18nMessages.meb0d5884569b398e}${t(i18nMessages.m3c661b3eacf3728f)}</p>`,
      `${i18nMessages.m6d68e0e09b48078f}`,
    );
    $("[data-autosync-continue]", body).addEventListener("click", (event) => {
      saveAutosync($("[name=autosync]", body).checked, event.currentTarget);
    });
  }

  async function saveAutosync(enabled, button) {
    if (wizard.busy) return;
    wizard.busy = true;
    await setError("");
    const restore = setButtonBusy(button, t(i18nMessages.m6bdb4435095e5d28));
    try {
      wizard.state.autoSync = enabled;
      await execute("autosync_set", { enabled });
      // Poll briefly: the reported autosync value races the in-flight
      // status refresh after the write.
      let reported;
      for (let i = 0; i < 10; i++) {
        await new Promise((resolve) => setTimeout(resolve, 300));
        await ctx.refresh();
        reported = ctx.status?.summary?.autosync;
        if (reported === undefined || Boolean(reported) === enabled) break;
      }
      if (reported !== undefined && Boolean(reported) !== enabled)
        throw Error(i18nMessages.m49dacf0182f2865e);
      wizard.state.accountPhase = "auth";
      if (wizard.state.material) {
        await saveStep("recovery");
        wizard.state.step = "recovery";
      } else {
        await saveStep("model");
        wizard.state.step = "model";
      }
      render();
    } catch (error) {
      setError(error.message);
      restore();
    } finally {
      wizard.busy = false;
    }
  }

  function renderRecovery(body) {
    if (!wizard.state.material) {
      // A reload clears the in-memory material; never fake this step.
      body.innerHTML = frame(
        i18nMessages.md7db191ca6e124c3,
        "",
        i18nMessages.mb9ae1e046e3040f7,
        `${i18nMessages.mfc989563bb17330c}`,
      );
      $("[data-recovery-next]", body).addEventListener("click", () =>
        goto("model"),
      );
      return;
    }
    body.innerHTML = frame(
      i18nMessages.m1ed3e12c0172265d,
      i18nMessages.m7bf11b499c198bcf,
      `${i18nMessages.m98e66644b0e88d95}${h(
        recoveryMaterialJson(wizard.state.material),
      )}${i18nMessages.med02037415897cbc}`,
      `${i18nMessages.mba1c9e4f02101bf2}`,
    );
    $("[data-copy-material]", body).addEventListener("click", async () => {
      await navigator.clipboard.writeText(
        recoveryMaterialJson(wizard.state.material),
      );
      notify(i18nMessages.m8f6f8d979c981ced);
    });
    const next = $("[data-recovery-next]", body);
    $("[data-saved-material]", body).addEventListener("change", (event) => {
      next.disabled = !event.target.checked;
    });
    next.addEventListener("click", () => {
      // The material is acknowledged: drop it from memory immediately.
      wizard.state.material = null;
      goto("model");
    });
  }
  return { renderLanguage, renderSync, renderAccount, renderRecovery };
}
