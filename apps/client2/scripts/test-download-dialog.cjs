// Run against the Go-exported production assets with the compiled WebUI lane's Playwright.
const { test } = require("node:test");
const assert = require("node:assert/strict");
const path = require("node:path");
const { chromium } = require("@playwright/test");

test("download dialog recovers from rejected starts and bounds busy snapshots", async () => {
  const assets = path.join(process.env.RESPIRE_E2E_WEBUI_BUNDLE, "assets");
  const browser = await chromium.launch({ executablePath: process.env.RESPIRE_E2E_BROWSER_EXECUTABLE });
  try {
    const page = await browser.newPage();
    const origin = "http://127.0.0.1:41237";
    let starts = 0;
    let snapshots = 0;
    await page.route("**/*", async route => {
      const url = new URL(route.request().url());
      assert.equal(url.origin, origin);
      if (url.pathname === "/") return route.fulfill({ contentType: "text/html", body: "<!doctype html>" });
      if (url.pathname === "/api/model-download") {
        starts++;
        if (starts <= 2) return route.fulfill({ status: 409, json: { ok: false, error: "busy" } });
        return route.fulfill({ json: { ok: true, result: { id: "fixture" } } });
      }
      if (url.pathname === "/api/task") {
        assert.equal(url.searchParams.get("id"), "fixture");
        snapshots++;
        await page.evaluate(() => { window.elapsed = 62 * 60_000; });
        return route.fulfill({ json: { ok: true, result: { id: "fixture", busy: true } } });
      }
      assert.match(url.pathname, /^\/(pages|ui)\/[a-z0-9/-]+\.js$/i);
      return route.fulfill({ path: path.join(assets, url.pathname), contentType: "text/javascript" });
    });
    await page.goto(origin);
    await page.evaluate(async () => {
      window.EventSource = undefined;
      window.elapsed = 0;
      const now = Date.now.bind(Date);
      Date.now = () => now() + window.elapsed;
      const { openDownloadModal } = await import("/pages/model-kit.js");
      window.openDownload = () => { window.dialog = openDownloadModal({ model: "m3", variant: "quantized", label: "Test" }); };
      window.openDownload();
    });
    const active = page.locator("dialog[open]");
    await active.locator('[data-dl="dismiss"]').click();
    assert.equal(await page.evaluate(() => window.dialog.done), false);
    await page.evaluate(() => window.openDownload());
    await active.locator('[data-dl="retry"]').click();
    await active.locator('[data-dl-error]:visible').waitFor();
    assert.equal(await page.evaluate(() => window.dialog.done), false);
    assert.equal(starts, 3);
    await page.waitForTimeout(800);
    assert.equal(snapshots, 1);
  } finally {
    await browser.close();
  }
});
