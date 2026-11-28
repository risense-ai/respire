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
const modelSelect = "[data-mirror] [name=model]";
const downloadDialog = 'dialog:has([data-dl="background"])';
let browser;

async function openModels(t) {
  const fixture = await startModelsFixture();
  let context;
  const errors = [];
  t.after(async () => {
    try { await context?.close(); } finally { await fixture.close(); }
    assert.deepEqual([...errors, ...fixture.errors.map(String)], []);
  });
  context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  const page = await context.newPage();
  page.setDefaultTimeout(7000);
  page.on("pageerror", error => errors.push(error.stack || error.message));
  page.on("console", message => {
    if (message.type() === "error") errors.push(message.text());
  });
  await page.goto(`${fixture.url}#models`);
  await page.locator(".status-card.ready").waitFor();
  return { page, fixture };
}

async function text(page, selector, value, timeout = 7000) {
  await page.waitForFunction(({ selector, value }) => document.querySelector(selector)?.textContent.includes(value),
    { selector, value }, { timeout });
}
async function showModel(page, model) {
  await advanced(page);
  await page.locator(modelSelect).selectOption(model);
  await text(page, "[data-variants]", granite[model].path);
}
async function confirm(page) {
  await page.locator('dialog[open] button[type="submit"]').click();
}
async function advanced(page) {
  if (!await page.locator("[data-advanced-panel]").isVisible())
    await page.locator("[data-advanced-open] [data-action=toggle-advanced]").click();
}
async function startRowDownload(page, model) {
  await showModel(page, model);
  await page.locator(`[data-model=download][data-variant=${granite[model].name}]`).click();
  await confirm(page);
  await page.locator(`${downloadDialog}[open] [data-dl=background]`).waitFor();
}
async function assertRowProgress(page, model, percent) {
  const variant = granite[model].name;
  await text(page, `[data-status=${variant}]`, `${percent}%`);
  await page.locator(`[data-model=download][data-variant=${variant}]:disabled`).waitFor();
  assert.ok(!(await page.locator("[data-variants]").innerText()).includes("model_quantized.onnx"));
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

  test("model mirror preferences persist and notices can be dismissed or cleared", async t => {
    const { page } = await openModels(t);
    await advanced(page);
    const select = page.locator("[data-mirror] [name=mirror]");
    const custom = page.locator("[name=mirror-custom]");
    const save = page.locator("[data-mirror] [type=submit]");
    const dismiss = page.locator("[data-notice-dismiss]");
    const mirrorState = "[data-mirror-state]";
    await select.selectOption("https://hf-mirror.com");
    await save.click();
    await text(page, mirrorState, "https://hf-mirror.com");
    await page.reload();
    await text(page, mirrorState, "https://hf-mirror.com");
    assert.equal(await select.inputValue(), "https://hf-mirror.com");
    await advanced(page);
    await select.selectOption("custom");
    await custom.fill("https://mirror.example.org");
    await save.click();
    await text(page, mirrorState, "https://mirror.example.org");
    await page.reload();
    await text(page, mirrorState, "https://mirror.example.org");
    assert.equal(await select.inputValue(), "custom");
    assert.equal(await custom.inputValue(), "https://mirror.example.org");
    await advanced(page);
    await custom.fill("ftp://bad");
    await save.click();
    await text(page, "#notice.error", "Model mirror must use HTTP or HTTPS");
    await dismiss.click();
    await page.locator("#notice").waitFor({ state: "hidden" });
    assert.equal(await dismiss.count(), 0);
    await save.click();
    await text(page, "#notice.error", "Model mirror must use HTTP or HTTPS");
    let release;
    const held = new Promise(resolve => { release = resolve; });
    await page.route("**/api/preferences", async route => {
      await held;
      await route.continue();
    }, { times: 1 });
    try {
      await select.selectOption("auto");
      await Promise.all([page.waitForRequest("**/api/preferences"), save.click()]);
      assert.equal(await page.locator("#notice").isVisible(), false);
    } finally { release(); }
    await text(page, mirrorState, "auto");
  });

  test("Host and Runtime Granite downloads update the matching live row", async t => {
    const { page, fixture } = await openModels(t);
    await startRowDownload(page, small);
    await text(page, `${downloadDialog} [data-dl-bytes]`, "25.0 KiB / 100.0 KiB");
    await background(page);
    await text(page, taskProgress, "tokenizer.json");
    await assertRowProgress(page, small, 25);
    fixture.progressDownload(small, 65);
    await text(page, taskProgress, "65.0 KiB / 100.0 KiB");
    await assertRowProgress(page, small, 65);
    await page.locator(reopen).click();
    await page.locator(`${downloadDialog}[open] [data-dl=pause]`).waitFor();
    await text(page, `${downloadDialog} [data-dl-bytes]`, "65.0 KiB / 100.0 KiB");
    fixture.progressDownload(small, 100, "completed");
    await page.locator(`${downloadDialog} [data-dl=dismiss]`).click();
    await page.locator("[data-apply][data-variant=standard][data-engine=cpu]").waitFor();

    await startRowDownload(page, large);
    await text(page, `${downloadDialog} [data-dl-bytes]`, "40%");
    assert.equal(await page.locator(`${downloadDialog} [data-dl=pause]`).isVisible(), false);
    await text(page, taskProgress, "40%");
    await assertRowProgress(page, large, 40);
    await background(page);
    fixture.progressDownload(large, 70);
    await text(page, taskProgress, "70%");
    await assertRowProgress(page, large, 70);
    fixture.progressDownload(large, 100, "completed");
    await page.locator(reopen).waitFor({ state: "hidden" });
    await page.locator("[data-apply][data-variant=quantized][data-engine=cpu]").waitFor();
    assert.deepEqual(fixture.switches, []);
  });

  test("backgrounded primary download switches on completion and preserves selection on cancellation", async t => {
    const { page, fixture } = await openModels(t);
    await showModel(page, large);
    await page.locator("[data-mode-apply]").click();
    await background(page);
    await text(page, taskProgress, "40%");
    assert.deepEqual(fixture.switches, []);
    fixture.progressDownload(large, 100, "completed");
    await page.locator(".status-card.ready").waitFor();
    assert.deepEqual(fixture.selection, { model: large, engine: "cpu", variant: "quantized" });

    await showModel(page, small);
    await page.locator("[data-mode-apply]").click();
    await background(page);
    await page.locator(reopen).click();
    await page.locator(`${downloadDialog} [data-dl=cancel]`).click();
    await page.locator(downloadDialog).waitFor({ state: "hidden" });
    await page.locator(".status-card.empty").waitFor();
    assert.equal(fixture.switches.length, 1);
    assert.equal(fixture.selection.model, large);
    assert.equal([...fixture.downloads.values()].find(entry => entry.model === small).state, "cancelled");
  });

  test("reindex updates within five seconds and can be cancelled after navigation", async t => {
    const { page, fixture } = await openModels(t);
    assert.deepEqual(fixture.health.capabilities, ["openapi.v1", "sse.operations.v1"]);
    await advanced(page);
    await page.locator("[data-model=rebuild]").click();
    await confirm(page);
    await page.locator(`${taskProgress} progress[value="1"][max="20"]`).waitFor();
    fixture.progressIndex(9);
    await text(page, "[data-inference-status]", "9 / 20", 4800);
    await page.locator(`${taskProgress} progress[value="9"][max="20"]`).waitFor();
    await page.locator("nav [data-page=overview]").click();
    await page.waitForURL("**/#overview");
    await page.locator("[data-overview-summary]").waitFor();
    await Promise.all([
      page.waitForResponse(response => new URL(response.url()).pathname === "/api/operations" && response.ok()),
      page.locator("nav [data-page=models]").click(),
    ]);
    await page.waitForURL("**/#models");
    await page.locator(`${taskProgress} progress[value="9"][max="20"]`).waitFor();
    await page.locator('[data-live] [data-model="cancel"]').click();
    await text(page, "#notice.error", "Index rebuild cancelled");
    await page.locator(`${taskProgress} progress`).waitFor({ state: "hidden" });
    assert.deepEqual(fixture.cancellations, [fixture.rebuild.id]);
    assert.equal(fixture.rebuild.state, "failed");
  });
}
