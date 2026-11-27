import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { $, all, h, bytes, execute, confirm, notify } from "../ui/core.js";
import { t } from "../ui/i18n.js";
import {
  MODEL_CARDS,
  MODEL_OPTIONS,
  DEVICE_VARIANTS,
  graniteVariant,
  DEVICES,
  engineAvailable,
  isProfileModel,
  openDownloadModal,
  applyEngineChoice,
} from "../pages/model-kit.js";

const EXECUTE_STAGES = [
  ["download", i18nMessages.m4673a23061656125],
  ["verify", i18nMessages.m5ae04fddcd3363e3],
  ["load", i18nMessages.m0d1dbb79b1c0fbb4],
  ["probe", i18nMessages.m040226ef08d08ad7],
  ["index", i18nMessages.m374d1132cdace3bf],
];

function modelLabel(id) {
  return MODEL_OPTIONS.find(([key]) => key === id)?.[1] || id;
}
export function modelSteps(wizard, {
  frame,
  card,
  backButton,
  render,
  goto,
  complete,
  connected,
  ctx,
  setError,
  root,
  session,
  exitToModels,
}) {
  async function ensureInventory(model) {
    return ctx.modelInventory.get(model) || ctx.modelInventory.load(model);
  }

  function renderModel(body) {
    const benefit =
      i18nMessages.m682a50ff84a77cf7;
    body.innerHTML = frame(
      i18nMessages.m1afed6a81a2512d2,
      "",
      `<p class="hint">${h(benefit)}</p><div class="wizard-choices">${MODEL_CARDS.map(
        (model, index) =>
          card(model.title, model.detail, model.id, {
            badge: index === 0 ? i18nMessages.m844b8cc8dff7c1d8 : "",
            active: wizard.state.model === model.id,
          }),
      ).join("")}</div>`,
      backButton(wizard.state.syncChoice === "cloud" ? "account" : "sync") +
        `${i18nMessages.meed1c46e80a4bae6}`,
    );
    all("[data-choice]", body).forEach((choice) =>
      choice.addEventListener("click", () => {
        if (wizard.state.model !== choice.dataset.choice) {
          wizard.state.model = choice.dataset.choice;
          if (graniteVariant(wizard.state.model)) wizard.state.device = "cpu";
          wizard.state.variantTouched = false;
          wizard.state.variant = "";
        }
        render();
      }),
    );
    $("[data-next-model]", body).addEventListener("click", () => {
      if (wizard.busy) return;
      goto("device");
    });
    $("[data-skip-model]", body).addEventListener("click", async () => {
      if (
        await confirm(
          i18nMessages.mc9f42140d9d65f5c,
          i18nMessages.mf59790f8330caffe,
          i18nMessages.m6204eca8946e477f,
        )
      )
        complete();
    });
  }

  function renderDevice(body) {
    if (!connected()) {
      body.innerHTML = frame(
        i18nMessages.mbbccf7516f5e53ae,
        "",
        i18nMessages.m8465daf7e2405980,
        backButton("model"),
      );
      return;
    }
    const platform = ctx.desktop?.platform || "";
    // OS-level entry gates from the shared kit; real capability is decided
    // by the runtime's inference probe at enable time.
    const gpuEntry = engineAvailable(platform, "gpu");
    body.innerHTML = frame(
      i18nMessages.mbbccf7516f5e53ae,
      i18nMessages.m4b4018eca61fe94e,
      `<div class="wizard-choices">${DEVICES.map((device) =>
        card(device.title, device.detail, device.id, {
          disabled:
            (Boolean(graniteVariant(wizard.state.model)) && device.id !== "cpu") ||
            (device.id === "gpu" && !gpuEntry),
          badge: device.id === "cpu" ? i18nMessages.m844b8cc8dff7c1d8 : "",
          active: wizard.state.device === device.id,
        }),
      ).join("")}</div>${i18nMessages.meefec8afa359fa58}`,
      backButton("model") +
        `${i18nMessages.m8c76acc0c2bd1487}`,
    );
    all("[data-choice]", body).forEach((choice) =>
      choice.addEventListener("click", () => {
        wizard.state.device = choice.dataset.choice;
        wizard.state.variantTouched = false;
        render();
      }),
    );
    loadWeights(body);
  }

  // Weights follow the device: CPU defaults to quantized, GPU to fp16;
  // the advanced section may override. Download estimate always reflects the
  // files actually missing on disk.
  async function loadWeights(body) {
    let inventory;
    try {
      inventory = await ensureInventory(wizard.state.model);
    } catch (error) {
      setError(error.message);
      return;
    }
    if (!root.isConnected || wizard.state.step !== "device") return;
    if (!wizard.state.variantTouched || !wizard.state.variant)
      wizard.state.variant = graniteVariant(wizard.state.model) || DEVICE_VARIANTS[wizard.state.device] || "quantized";
    const weights = $("[data-weights]", body);
    if (weights) {
      const options = inventory.variants
        .map(
          (variant) =>
            `<option value="${h(variant.name)}" ${variant.name === wizard.state.variant ? "selected" : ""}>${h(variant.name)} · ${h(bytes(variant.size))}${variant.installed ? i18nMessages.m49dc4aa5307dd88e : ""}</option>`,
        )
        .join("");
      weights.innerHTML = `${i18nMessages.m4355d91bc30250f1}${options}${i18nMessages.m4f223f8434acbff1}`;
      const select = $("select[name=variant]", weights);
      select.addEventListener("change", () => {
        wizard.state.variant = select.value;
        wizard.state.variantTouched = true;
        updateEstimate(body, inventory);
      });
    }
    const next = $("[data-device-next]", body);
    if (next) {
      next.addEventListener("click", () => goto("confirm"));
    }
    updateEstimate(body, inventory);
  }

  function pendingDownloadBytes(inventory) {
    const variant = inventory.variants.find(
      (item) => item.name === wizard.state.variant,
    );
    const weights = variant?.installed ? 0 : variant?.size || 0;
    const cpuContainer = needsCPUContainer() && !inventory.cpu_container_ready ? inventory.cpu_container_size : 0;
    return weights + cpuContainer;
  }

  function needsCPUContainer() {
    return wizard.state.model === "m3" && wizard.state.device === "cpu" && wizard.state.variant === "quantized";
  }

  function updateEstimate(body, inventory) {
    const node = $("[data-download-estimate]", body);
    if (!node) return;
    const size = pendingDownloadBytes(inventory);
    const tokenizer = inventory.tokenizer_installed ? "" : t(i18nMessages.mc55d8be2853cb329);
    node.textContent = `${t(i18nMessages.mb3716b333ab3ad48)}${size ? h(bytes(size)) : t(i18nMessages.m1a30ef5341640ba8)}${tokenizer} · ${t(i18nMessages.m0b7e2876922e4662)} ${h(inventory.model_dir)}`;
  }

  async function renderConfirm(body) {
    if (!connected()) {
      body.innerHTML = frame(
        i18nMessages.m3550e7790941f374,
        "",
        i18nMessages.m3c40e2fca2125078,
        backButton("device"),
      );
      return;
    }
    let inventory;
    try {
      inventory = await ensureInventory(wizard.state.model);
    } catch (error) {
      body.innerHTML = frame(
        i18nMessages.m3550e7790941f374,
        "",
        `<p class="form-error">${h(error.message)}</p>`,
        backButton("device"),
      );
      return;
    }
    const cloud =
      wizard.state.syncChoice === "cloud"
        ? `${t(i18nMessages.m5f30fb70507cbc2c)}${i18nMessages.mda047c04c9b5e27e}${h(session().user || "")}${i18nMessages.m68f30e86a6eb9b0a}${t(i18nMessages.m6e9adb42a28942eb)}${wizard.state.autoSync ? t(i18nMessages.m8a4ef3e48e4e8a5a) : t(i18nMessages.m3cffa9757b693b52)}`
        : t(i18nMessages.mdaa2940fee504741);
    const size = pendingDownloadBytes(inventory);
    body.innerHTML = frame(
      i18nMessages.m3550e7790941f374,
      "",
      `<dl class="facts"><div><dt>${t(i18nMessages.m45a38d424be78a56)}</dt><dd>${cloud}</dd></div><div><dt>${t(i18nMessages.mc98e118e0a43f078)}</dt><dd>${h(modelLabel(wizard.state.model))}</dd></div><div><dt>${t(i18nMessages.m2ba569c9b4c8c68b)}</dt><dd>${h(wizard.state.device.toUpperCase())}</dd></div><div><dt>${t(i18nMessages.mdb18831a04574887)}</dt><dd>${h(wizard.state.variant)}</dd></div><div><dt>${t(i18nMessages.mb3716b333ab3ad48)}</dt><dd>${size ? h(bytes(size)) : t(i18nMessages.m1a30ef5341640ba8)}</dd></div><div><dt>${t(i18nMessages.m0b7e2876922e4662)}</dt><dd>${h(inventory.model_dir)}</dd></div></dl><p class="hint">${t(i18nMessages.m149de516b461d75b)}</p>`,
      backButton("device") +
        `${i18nMessages.m86a3d337cb1e43a6}`,
    );
    $("[data-later-download]", body).addEventListener("click", () =>
      complete(),
    );
    $("[data-start-execute]", body).addEventListener("click", () => {
      wizard.executeStarted = false;
      goto("execute");
    });
  }

  function renderExecute(body) {
    const stages = wizard.state.stages || {};
    body.innerHTML = frame(
      i18nMessages.m0ec3d8f669844995,
      "",
      `<ol class="wizard-stagelist">${EXECUTE_STAGES.map(
        ([key, label]) => {
          const cls = stages[key] || "pending";
          return `<li class="wizard-stage ${cls}"><span class="wizard-stage-dot"></span>${h(label)}<span class="wizard-stage-note" data-stage-note="${h(key)}"></span></li>`;
        },
      ).join("")}</ol><div class="wizard-execute-status" data-execute-status></div><div class="actions" data-execute-actions></div>`,
    );
    if (!wizard.executeStarted && !wizard.state.executing) {
      wizard.executeStarted = true;
      runExecute("download");
    }
  }

  function setStage(stage, status, note = "") {
    if (!wizard.state) return; // wizard closed by skip mid-execute
    wizard.state.stages = wizard.state.stages || {};
    wizard.state.stages[stage] = status;
    const node = root.querySelector(
      `[data-stage-note="${CSS.escape(stage)}"]`,
    );
    const li = node?.closest(".wizard-stage");
    if (li) {
      li.classList.remove("pending", "active", "done", "failed");
      li.classList.add(status);
    }
    if (node) node.textContent = note;
  }

  function executeStatus(text, error = false) {
    const node = $("[data-execute-status]", root);
    if (node) {
      node.textContent = text;
      node.classList.toggle("form-error", error);
    }
  }

  function executeActions(buttons) {
    const area = $("[data-execute-actions]", root);
    if (!area) return;
    area.innerHTML = buttons
      .map(
        ([label, action, primary]) =>
          `<button type="button" ${primary ? 'class="primary" ' : ""}data-exec="${h(action)}">${h(label)}</button>`,
      )
      .join("");
    all("[data-exec]", area).forEach((button) =>
      button.addEventListener("click", () => {
        const action = button.dataset.exec;
        if (action === "retry-download") runExecute("download");
        else if (action === "retry-enable") runExecute("enable");
        else if (action === "retry-index") runExecute("index");
        else if (action === "exit-models") exitToModels();
      }),
    );
  }

  function failure(stage, message) {
    setStage(stage, "failed");
    executeStatus(message, true);
    const retry =
      stage === "download"
        ? [i18nMessages.m87dd9db3387c472b, "retry-download"]
        : stage === "index"
          ? [i18nMessages.mcbc4c9eed5d1331c, "retry-index"]
          : [i18nMessages.m1345ce2f67ecc4af, "retry-enable"];
    executeActions([
      [retry[0], retry[1], true],
      [i18nMessages.m5b78b263519b84df, "exit-models"],
    ]);
  }

  async function runExecute(startStage) {
    if (wizard.state.executing) return;
    wizard.state.executing = true;
    // The in-memory device/weight selection may be missing on a resumed
    // run; derive the device default so the download never uses a blank
    // variant name.
    if (!wizard.state.variant)
      wizard.state.variant = graniteVariant(wizard.state.model) || DEVICE_VARIANTS[wizard.state.device] || "quantized";
    executeActions([]);
    try {
      if (startStage === "download") {
        if (!(await runDownloadStage())) {
          if (wizard.state) wizard.state.executing = false;
          return;
        }
      }
      if (startStage === "download" || startStage === "enable") {
        if (!(await runEnableStage())) {
          if (wizard.state) wizard.state.executing = false;
          return;
        }
      }
      if (!(await runIndexStage())) {
        if (wizard.state) wizard.state.executing = false;
        return;
      }
      if (wizard.state) {
        wizard.state.executing = false;
        renderExecuteSuccess();
      }
    } catch (error) {
      if (wizard.state) wizard.state.executing = false;
      failure(startStage === "index" ? "index" : "probe", error.message);
    }
  }

  async function runDownloadStage() {
    setStage("download", "active");
    executeStatus(i18nMessages.m549fa993310649f7);
    let inventory;
    try {
      inventory = await ensureInventory(wizard.state.model);
    } catch (error) {
      failure("download", error.message);
      return false;
    }
    const variant = inventory.variants.find(
      (item) => item.name === wizard.state.variant,
    );
    const cpuBaseline = needsCPUContainer();
    const needed = !inventory.tokenizer_installed || !variant?.installed ||
      (cpuBaseline && !inventory.cpu_container_ready);
    if (!needed) {
      // Existing verified weights: skip straight to enabling.
      setStage("download", "done", i18nMessages.m6ebf6f7e220b98e0);
      setStage("verify", "done", i18nMessages.m1936eafc0c3c4a5a);
      return true;
    }
    executeStatus(i18nMessages.m6208b7f00f4adb15);
    const downloads = [cpuBaseline ? "baseline" : wizard.state.variant];
    // Legacy M3 weight downloads do not include the tokenizer.
    if (!cpuBaseline && !isProfileModel(wizard.state.model) && !inventory.tokenizer_installed) {
      if (variant?.installed) downloads.pop();
      downloads.unshift("tokenizer");
    }
    for (const [index, variant] of downloads.entries()) {
      const { element, done } = openDownloadModal({
        model: wizard.state.model,
        variant,
        label: modelLabel(wizard.state.model),
        getMirror: () => "auto",
      });
      const ok = await done;
      // Follow a download whose modal was closed without resubmitting it.
      if (!ok && !(await trackRunningDownload())) {
        failure("download", i18nMessages.m98556c16632dcfad);
        return false;
      }
      if (index + 1 < downloads.length) element.close();
    }
    try {
      inventory = await ctx.modelInventory.load(wizard.state.model);
    } catch (error) {
      failure("verify", error.message);
      return false;
    }
    const installed = inventory.variants.find(
      (item) => item.name === wizard.state.variant,
    );
    if (!inventory.tokenizer_installed || !installed?.installed ||
        (cpuBaseline && !inventory.cpu_container_ready)) {
      failure("verify", i18nMessages.m429eb757c29c7408);
      return false;
    }
    setStage("download", "done");
    setStage("verify", "done", i18nMessages.m3051a776eaa52624);
    return true;
  }

  // Reopen-resume path: a download may still be running after the wizard or
  // the modal was closed; follow the client task state to its end.
  async function trackRunningDownload() {
    for (let rounds = 0; rounds < 7200; rounds++) {
      if (!wizard.state) return false; // wizard closed by skip: stop tracking
      await ctx.refresh();
      const task = ctx.desktop?.task || {};
      if (!task.busy) return task.stage === i18nMessages.mc0b3fbff51ccc40b;
      executeStatus(
        `${task.stage || i18nMessages.me2fc571a4cf00ceb}${task.total ? ` · ${bytes(task.done)} / ${bytes(task.total)}` : ""}`,
      );
      await new Promise((resolve) => setTimeout(resolve, 1500));
    }
    return false;
  }

  async function runEnableStage() {
    setStage("load", "active");
    executeStatus(i18nMessages.mb65a65b52b02afd6);
    let outcome;
    try {
      outcome = await applyEngineChoice({
        model: wizard.state.model,
        engine: wizard.state.device,
        variant: wizard.state.variant,
      });
    } catch (error) {
      failure("load", error.message);
      return false;
    }
    if (outcome.failed) {
      failure("probe", outcome.warning || i18nMessages.m0b80573357511991);
      return false;
    }
    if (outcome.fallback) {
      // Report the engine the host restored after the failed probe: never
      // present the selected device as successful when verification fell back.
      setStage("load", "done");
      setStage("probe", "done", `${outcome.engine.toUpperCase()} · ${t(i18nMessages.m23089ecc6ad00006)}`);
      executeStatus(outcome.warning, true);
    } else {
      setStage("load", "done");
      setStage("probe", "done", `${outcome.engine.toUpperCase()} · ${t(i18nMessages.m0eb40e8903ea919d)}`);
    }
    wizard.state.enableOutcome = outcome;
    return true;
  }

  async function runIndexStage() {
    setStage("index", "active");
    executeStatus(i18nMessages.m9496197e29dc08c5);
    try {
      if (isProfileModel(wizard.state.model))
        await execute("index_prepare");
      else
        await execute("model_prepare", { mirror: "auto" });
    } catch (error) {
      failure("index", error.message);
      return false;
    }
    setStage("index", "done");
    return true;
  }

  function renderExecuteSuccess() {
    if (!wizard.state) return; // wizard closed by skip mid-execute
    const outcome = wizard.state.enableOutcome || {};
    const body = $(".wizard-body", root);
    const actualEngine = outcome.engine || "cpu";
    const note = outcome.fallback
      ? `${h(wizard.state.device.toUpperCase())} ${t(i18nMessages.m0a4138d2a7f2b55f)}${h(actualEngine.toUpperCase())} · ${h(outcome.variant)}${i18nMessages.meb0d5884569b398e}${h(outcome.warning || "")}`
      : `${t(i18nMessages.m76cf2e3cdf3e78a2)}${i18nMessages.mf114611cd61000bf}${h(modelLabel(wizard.state.model))} · ${h(actualEngine.toUpperCase())} · ${h(outcome.variant || wizard.state.variant)}${i18nMessages.meb0d5884569b398e}`;
    body.innerHTML = frame(
      i18nMessages.ma2c69b4733366246,
      "",
      `<p>${note}</p><p class="hint">${t(i18nMessages.md7a02d311272e9ad)}</p>`,
      `${i18nMessages.ma651ff6b19816454}`,
      { skip: false },
    );
    $("[data-next-agents]", body).addEventListener("click", () => goto("agents"));
    $("[data-skip-agents]", body).addEventListener("click", () => complete());
  }
  return { renderModel, renderDevice, renderConfirm, renderExecute };
}
