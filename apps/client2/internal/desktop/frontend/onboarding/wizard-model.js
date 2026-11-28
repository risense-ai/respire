import { messages as i18nMessages } from "../ui/i18n/messages.js";
import { $, all, action, h, bytes, api, execute, confirm, notify, input, select } from "../ui/core.js";
import { t } from "../ui/i18n.js";
import {
  MODEL_CARDS,
  MODEL_OPTIONS,
  MIRROR_OPTIONS,
  engineAvailable,
  isProfileModel,
  variantFilename,
  activeDownloadSession,
  openDownloadModal,
  reopenActiveDownloadModal,
  applyEngineChoice,
} from "../pages/model-kit.js";

const MODEL_IDS = ["granite-311m-r2", "granite-97m-r2", "m3", "bge-base-zh-v1.5"];
const ENGINES = ["cpu", "gpu", "npu"];

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
  goto,
  complete,
  connected,
  ctx,
  setError,
  root,
  session,
  exitToModels,
  curatedModels,
  defaultModel,
  defaultVariantForModel,
}) {
  async function ensureInventory(model) {
    return ctx.modelInventory.get(model) || ctx.modelInventory.load(model);
  }

  function bindChoices(group, onSelect) {
    const choices = all("[data-choice]", group);
    choices.forEach((choice) => choice.addEventListener("click", () => {
      onSelect(choice.dataset.choice);
      choices.forEach((button) => {
        const active = button === choice;
        button.classList.toggle("active", active);
        button.setAttribute("aria-pressed", String(active));
      });
    }));
  }

  function renderModel(body) {
    const cards = curatedModels || MODEL_IDS.map((id) => MODEL_CARDS.find((model) => model.id === id));
    const mirror = ctx.desktop?.preferences?.modelMirror || "auto";
    const preset = MIRROR_OPTIONS.some(([value]) => value !== "custom" && value === mirror);
    body.innerHTML = frame(
      i18nMessages.m1afed6a81a2512d2,
      "",
      `<div class="wizard-choices wizard-model-grid">${cards.map((model) => {
        const detail = model.dims ? `${model.dims} ${t(i18nMessages.modelDims)} · ${model.context.toLocaleString()} ${t(i18nMessages.modelContext)} · ${t(model.tag)}` : "";
        return card(model.title, detail, model.id, {
          badge: model.id === defaultModel ? i18nMessages.m844b8cc8dff7c1d8 : "",
          active: wizard.state.model === model.id,
        });
      }).join("")}</div><form class="wizard-form wizard-mirror" data-wizard-mirror>
        ${select("mirror", i18nMessages.m012f0f4b569f1b90, MIRROR_OPTIONS, preset ? mirror : "custom")}
        <div data-mirror-custom ${preset ? "hidden" : ""}>${input("mirror-custom", i18nMessages.mad70940de1eb29fc, preset ? "" : mirror, { type: "url" })}</div>
        <button type="submit">${h(i18nMessages.ma3030bf8f16dc63c)}</button>
      </form>`,
      backButton(wizard.state.syncChoice === "cloud" ? "account" : "sync") +
        `${i18nMessages.meed1c46e80a4bae6}`,
    );
    bindChoices($(".wizard-model-grid", body), (id) => {
      if (wizard.state.model === id) return;
      wizard.state.model = id;
      wizard.state.variant = defaultVariantForModel?.(id) || (id.startsWith("granite-") ? "quint8_avx2" : "quantized");
    });
    const form = $("[data-wizard-mirror]", body);
    const mirrorSelect = $("[name=mirror]", form);
    const custom = $("[name=mirror-custom]", form);
    function updateMirrorField() {
      const isCustom = mirrorSelect.value === "custom";
      $("[data-mirror-custom]", form).hidden = !isCustom;
      custom.disabled = !isCustom;
      custom.required = isCustom;
    }
    updateMirrorField();
    mirrorSelect.addEventListener("change", updateMirrorField);
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      const isCustom = mirrorSelect.value === "custom";
      const value = isCustom ? custom.value.trim() : mirrorSelect.value;
      if (isCustom) {
        if (!URL.canParse(value)) return setError(t(i18nMessages.modelMirrorInvalid));
        const parsed = new URL(value);
        if (!["http:", "https:"].includes(parsed.protocol) || parsed.username || parsed.password)
          return setError(t(i18nMessages.modelMirrorInvalid));
      }
      action($("[type=submit]", form), async () => {
        wizard.busy = true;
        try {
          setError("");
          await ctx.savePreferences({ modelMirror: value });
          notify(i18nMessages.m9b49b31f2416becd);
        } catch (error) {
          if (body.isConnected) setError(error.message);
        } finally {
          wizard.busy = false;
        }
      });
    });
    $("[data-next-model]", body).addEventListener("click", () => {
      if (wizard.busy) return;
      goto("device");
    });
    $("[data-skip-model]", body).addEventListener("click", async () => {
      if (wizard.busy) return;
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
    const available = (engine) => engineAvailable(platform, engine) ||
      (!curatedModels && /^(windows|darwin|linux)\b/.test(platform));
    body.innerHTML = frame(
      i18nMessages.mbbccf7516f5e53ae,
      "",
      `<div class="wizard-choices wizard-engines">${ENGINES.map((engine) =>
        card(engine.toUpperCase(), "", engine, {
          disabled: !available(engine),
          active: wizard.state.device === engine,
        }),
      ).join("")}</div><h2 class="wizard-weight-title">${h(t(i18nMessages.modelQuantization))}</h2>
      <div class="wizard-weights" data-weights aria-busy="true"><p class="hint wizard-weight-loading">${h(t(i18nMessages.modelInventoryLoading))}</p></div>
      <p class="hint" data-download-estimate></p>`,
      backButton("model") +
        `${i18nMessages.m8c76acc0c2bd1487}`,
    );
    loadWeights(body);
  }

  async function loadWeights(body) {
    const weights = $("[data-weights]", body);
    const next = $("[data-device-next]", body);
    next.disabled = true;
    let inventory;
    bindChoices($(".wizard-engines", body), (engine) => {
      wizard.state.device = engine;
      if (inventory) updateEstimate(body, inventory);
    });
    try {
      inventory = await ensureInventory(wizard.state.model);
    } catch (error) {
      if (body.isConnected) {
        weights.textContent = "";
        weights.removeAttribute("aria-busy");
        setError(error.message);
      }
      return;
    }
    if (!body.isConnected || wizard.state?.step !== "device") return;
    weights.innerHTML = inventory.variants.map((variant) => {
      const filename = variantFilename(variant);
      const detail = `${variant.name === "fp32" ? "" : `${filename} · `}${bytes(variant.size)}`;
      return card(variant.name === "fp32" ? filename : variant.name, detail, variant.name, {
        badge: variant.installed ? i18nMessages.me2f12da7faa68217 : i18nMessages.mdccec95b65e15ff0,
        active: variant.name === wizard.state.variant,
      });
    }).join("");
    weights.removeAttribute("aria-busy");
    bindChoices(weights, (variant) => {
      wizard.state.variant = variant;
      next.disabled = false;
      updateEstimate(body, inventory);
    });
    next.disabled = !inventory.variants.some((variant) => variant.name === wizard.state.variant);
    next.addEventListener("click", () => goto("confirm"));
    if (!next.disabled) updateEstimate(body, inventory);
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
      if (!body.isConnected) return;
      body.innerHTML = frame(
        i18nMessages.m3550e7790941f374,
        "",
        `<p class="form-error">${h(error.message)}</p>`,
        backButton("device"),
      );
      return;
    }
    if (!body.isConnected) return;
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
    if (activeDownloadSession()?.model === wizard.state.model)
      executeActions([[i18nMessages.mDownloadShowProgress, "reopen-download"]]);
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
        if (action === "reopen-download") reopenActiveDownloadModal();
        else if (action === "retry-download") runExecute("download");
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
      const modal = openDownloadModal({
        model: wizard.state.model,
        variant,
        label: modelLabel(wizard.state.model),
        getMirror: () => ctx.desktop?.preferences?.modelMirror || "auto",
      });
      executeActions([[i18nMessages.mDownloadShowProgress, "reopen-download"]]);
      const ok = await modal.done;
      executeActions([]);
      if (!wizard.state) return false;
      if (!ok && !(modal.state === "closed" && await trackRunningDownload(modal))) {
        failure("download", i18nMessages.m98556c16632dcfad);
        return false;
      }
      if (!wizard.state) return false;
      if (index + 1 < downloads.length && modal.element.open) modal.element.close();
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

  async function trackRunningDownload(modal) {
    let receipt;
    try {
      receipt = await modal.receipt;
    } catch (error) {
      failure("download", error.message);
      return false;
    }
    if (!receipt?.id) return false;
    for (let rounds = 0; rounds < 7200; rounds++) {
      if (!wizard.state) return false; // wizard closed by skip: stop tracking
      await ctx.refresh();
      if (receipt.runtime) {
        const live = ctx.status?.summary?.model_operation;
        const op = await api("runtime-operation?id=" + encodeURIComponent(receipt.id));
        if (!["pending", "running"].includes(op.state)) return op.state === "completed";
        const percent = op.progress_percent;
        const stage = op.message || (live?.id === receipt.id && live.phase) || i18nMessages.me2fc571a4cf00ceb;
        executeStatus(`${stage}${typeof percent === "number" ? ` · ${Math.round(percent)}%` : ""}`);
      } else {
        const snapshot = ctx.desktop?.task;
        const task = snapshot?.id === receipt.id ? snapshot : await api("task?id=" + encodeURIComponent(receipt.id));
        if (!task.busy) return task.stage === i18nMessages.mc0b3fbff51ccc40b;
        executeStatus(`${task.stage || i18nMessages.me2fc571a4cf00ceb}${task.total > 0 ? ` · ${bytes(task.done)} / ${bytes(task.total)}` : ""}`);
      }
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
        await execute("model_prepare", { mirror: ctx.desktop?.preferences?.modelMirror || "auto" });
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
