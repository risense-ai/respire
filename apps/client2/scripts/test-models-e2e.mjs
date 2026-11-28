import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { after, before, test } from "node:test";
import { granite, startModelsFixture } from "./models-e2e-fixture.mjs";

const require = createRequire(import.meta.url);
export function resolvePlaywright(override = process.env.PLAYWRIGHT_CORE_PATH) {
  return require.resolve(override ? resolve(override) : "playwright-core");
}
export function loadPlaywright(override) {
  return require(resolvePlaywright(override));
}
export function findChromium(chromium, override = process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH) {
  if (override) {
    if (!existsSync(override)) throw new Error(`Chromium executable does not exist: ${override}`);
    return resolve(override);
  }
  return [
    "/usr/bin/google-chrome", "/usr/bin/chromium", "/usr/bin/chromium-browser",
    "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
    "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
    ...[process.env.ProgramFiles, process.env.LOCALAPPDATA].filter(Boolean).flatMap(root => [
      join(root, "Microsoft/Edge/Application/msedge.exe"), join(root, "Google/Chrome/Application/chrome.exe"),
    ]),
    ...["/Applications", join(homedir(), "Applications")].flatMap(root => [
      join(root, "Google Chrome.app/Contents/MacOS/Google Chrome"),
      join(root, "Chromium.app/Contents/MacOS/Chromium"),
      join(root, "Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
    ]),
    chromium.executablePath(),
  ].find(existsSync);
}

const small = "granite-97m-r2", large = "granite-311m-r2";
const taskProgress = "[data-model-task-progress]";
const reopen = '[data-model="reopen-download"]';
const downloadDialog = 'dialog:has([data-dl="background"])';
let browser;

async function openModels(t, options) {
  const fixture = await startModelsFixture(options);
  let context;
  const errors = [];
  t.after(async () => {
    try { await context?.close(); } finally { await fixture.close(); }
    assert.deepEqual([...errors, ...fixture.errors.map(String)], []);
  });
  context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  const page = await context.newPage();
  page.setDefaultTimeout(7000);
  await page.addInitScript(() => {
    window.__copied = [];
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText: async value => { window.__copied.push(value); } },
      configurable: true,
    });
  });
  page.on("pageerror", error => errors.push(error.stack || error.message));
  page.on("console", message => {
    if (message.type() === "error") errors.push(message.text());
  });
  await page.goto(`${fixture.url}#models`);
  await page.locator('[data-catalog-model="granite-311m-r2"]').waitFor();
  await page.locator('[data-model-dock] [data-catalog-action]').first().waitFor();
  return { page, fixture };
}

async function text(page, selector, value, timeout = 7000) {
  await page.waitForFunction(({ selector, value }) => document.querySelector(selector)?.textContent.includes(value),
    { selector, value }, { timeout });
}
async function confirm(page) {
  await page.locator('dialog[open] button[type="submit"]').click();
}
async function selectModel(page, model) {
  await page.locator(`[data-catalog-model="${model}"]`).click();
  await text(page, "[data-variants]", granite[model]?.path.split("/").pop() || "model_quantized.onnx");
}
async function startDownload(page, model, variant) {
  await selectModel(page, model);
  await page.locator(`[data-catalog-variant="${variant}"]`).click();
  await page.locator('[data-catalog-action="download"]').click();
  await page.locator(`${downloadDialog}[open] [data-dl=background]`).waitFor();
}
async function background(page) {
  await page.locator(`${downloadDialog} [data-dl=background]`).click();
  await page.locator(downloadDialog).waitFor({ state: "hidden" });
  await page.locator(`${taskProgress} ${reopen}`).waitFor();
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  before(async () => {
    const { chromium } = loadPlaywright();
    const executablePath = findChromium(chromium);
    assert.ok(executablePath, "Set PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH to a Chromium browser");
    browser = await chromium.launch({ executablePath, headless: true });
  });
  after(() => browser?.close());

  test("curated mirror header persists custom URL and notices clear", async t => {
    const { page } = await openModels(t);
    const select = page.locator("[data-mirror] [name=mirror]");
    const custom = page.locator("[name=mirror-custom]");
    const save = page.locator("[data-mirror] [type=submit]");
    const dismiss = page.locator("[data-notice-dismiss]");
    await select.selectOption("https://hf-mirror.com");
    await text(page, "#notice", "Download mirror saved");
    await page.reload();
    assert.equal(await select.inputValue(), "https://hf-mirror.com");
    await select.selectOption("custom");
    await custom.fill("https://mirror.example.org");
    await save.click();
    await text(page, "#notice", "Download mirror saved");
    await page.reload();
    assert.equal(await select.inputValue(), "custom");
    assert.equal(await custom.inputValue(), "https://mirror.example.org");
    await custom.fill("ftp://bad");
    await save.click();
    await text(page, "#notice.error", "Enter a valid HTTP or HTTPS mirror URL.");
    await dismiss.click();
    await page.locator("#notice").waitFor({ state: "hidden" });
    await select.selectOption("auto");
    await text(page, "#notice", "Download mirror saved");
  });

  test("4-model matrix keeps 0px vertical jump and dock supports two-step download, enable, probe, and delete", async t => {
    const { page, fixture } = await openModels(t);
    assert.equal(await page.locator("[data-catalog-model]").count(), 4);
    assert.equal(await page.locator("select[name=model], [data-mode-selector]").count(), 0);
    assert.equal(await page.locator('[data-catalog-model="bge-m3-distill-8l"]').count(), 0);

    await selectModel(page, large);
    const twoVariantDockTop = await page.locator("[data-model-dock]").evaluate(node => node.getBoundingClientRect().top);
    await selectModel(page, "bge-base-zh-v1.5");
    assert.equal(await page.locator("[data-catalog-variant]").count(), 8);
    const eightVariantDockTop = await page.locator("[data-model-dock]").evaluate(node => node.getBoundingClientRect().top);
    assert.equal(eightVariantDockTop, twoVariantDockTop);

    await page.locator("[data-catalog-engine=cpu]").waitFor();
    await page.locator("[data-catalog-engine=gpu]").waitFor();
    await page.locator("[data-catalog-engine=npu]").waitFor();

    await startDownload(page, small, "quint8_avx2");
    await background(page);
    await text(page, taskProgress, "tokenizer.json");
    fixture.progressDownload(small, 100, "completed");
    await page.locator(`${taskProgress} ${reopen}`).waitFor({ state: "hidden" });
    assert.deepEqual(fixture.switches, []);
    await page.locator('[data-catalog-action="enable"]').waitFor();
    await page.locator('[data-catalog-action="probe"]').waitFor();
    await page.locator('[data-catalog-action="delete"]').waitFor();

    await page.locator("[data-catalog-engine=npu]").click();
    await page.locator('[data-catalog-action="probe"]').click();
    const modal = page.locator("dialog.model-probe-dialog[open]");
    await modal.waitFor();
    await page.locator('[data-probe-status][data-result="pass"]').waitFor();
    await text(page, "[data-probe-status]", "Pass");
    assert.equal(fixture.selection.model, "m3");
    await modal.locator("[data-probe-copy]").click();
    const issueHref = await modal.locator("[data-probe-issue]").getAttribute("href");
    assert.ok(issueHref.startsWith("https://github.com/risense-ai/respire/issues/new?"));
    assert.equal(await page.evaluate(() => window.__copied.length), 1);

    fixture.setProbeFailure(true);
    await modal.locator("[data-probe-retry]").click();
    await page.locator('[data-probe-status][data-result="fail"]').waitFor();
    await text(page, "[data-probe-status]", "Fail");
    await modal.locator("[data-probe-close]").click();

    await page.locator('[data-catalog-action="delete"]').click();
    await confirm(page);
    await page.locator('[data-catalog-action="download"]').waitFor();
    assert.equal(fixture.deletions.length, 1);

    await startDownload(page, large, "quint8_avx2");
    await text(page, `${downloadDialog} [data-dl-bytes]`, "40%");
    fixture.progressDownload(large, 100, "completed");
    await page.locator(`${downloadDialog} [data-dl=dismiss]`).click();
    await page.locator("[data-catalog-engine=npu]").click();
    await page.locator('[data-catalog-action="enable"]').click();
    await page.locator('[data-catalog-action="enabled"]:disabled').waitFor();
    await page.locator('[data-catalog-action="open"]').waitFor();
    assert.deepEqual(fixture.selection, { model: large, variant: "quint8_avx2", engine: "npu" });
  });

  test("reindex updates within five seconds, cancels after navigation, and onboarding defaults to granite-311m-r2", async t => {
    const { page, fixture } = await openModels(t);
    await page.locator("[data-model=rebuild]").click();
    await confirm(page);
    await page.locator(`${taskProgress} progress[value="1"][max="20"]`).waitFor();
    fixture.progressIndex(9);
    await text(page, "[data-inference-status]", "9 / 20", 4800);
    await page.locator("nav [data-page=overview]").click();
    await page.waitForURL("**/#overview");
    await page.locator("nav [data-page=models]").click();
    await page.waitForURL("**/#models");
    await page.locator('[data-live] [data-model="cancel"]').click();
    await text(page, "#notice.error", "Index rebuild cancelled");

    fixture.preferences.onboardingCompleted = false;
    fixture.preferences.onboardingStep = "model";
    await page.reload();
    await page.locator('[data-choice="granite-311m-r2"].active').waitFor();
    await page.locator("[data-next-model]").click();
    await page.locator('[data-weights] [data-choice="quint8_avx2"].active').waitFor();
    await page.locator('.wizard-engines [data-choice="cpu"].active').waitFor();
  });
}
