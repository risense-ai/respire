import assert from "node:assert/strict";
import {once} from "node:events";
import {realpath, readFile, stat} from "node:fs/promises";
import {createRequire} from "node:module";
import {isAbsolute, join, relative, sep} from "node:path";
import {server} from "../../prototypes/mock-api/server.mjs";
import {cancellationServer} from "./cancellation.mjs";
import {measureLayout, screens, validateCatalogs, validateScreens, viewports} from "./layout.mjs";

const {expect, test} = createRequire(import.meta.url)("@playwright/test");
validateScreens();
const compiled = process.env.RESPIRE_E2E_CLIENT2_FIXTURE;
let build;
let manifest;
if (compiled) {
  assert(isAbsolute(compiled), "Client2 fixture directory must be absolute");
  build = await realpath(compiled);
  manifest = JSON.parse(await readFile(join(build, "acceptance.json"), "utf8"));
  assert(Array.isArray(manifest.requiredKeys) && manifest.requiredKeys.length, "Expected required translation keys");
  validateCatalogs(manifest.catalogs.en, manifest.catalogs.zh, manifest.requiredKeys);
  assert.deepEqual(Object.keys(manifest.screens).sort(), Object.keys(screens.en).sort());
  for (const [name, entry] of Object.entries(manifest.screens)) {
    assert(typeof entry.path === "string" && /^\/(?!\/)[/A-Za-z0-9_-]*$/.test(entry.path), "Invalid screen path: " + name);
    for (const locale of ["en", "zh"]) {
      assert(typeof entry[locale] === "string" && entry[locale].trim(), "Missing heading: " + name + " " + locale);
    }
    assert.notEqual(entry.en, entry.zh, "Expected localized headings: " + name);
    for (const locale of ["en", "zh"]) {
      assert(entry.ready?.[locale]?.selector && entry.ready[locale].text?.trim(),
        "Supply application-specific loaded UI selector and text: " + name + " " + locale);
    }
  }
  assert(manifest.errorSelector?.trim(), "Supply the application error notice selector");
}
const mode = compiled ? "COMPILED CLIENT2 WEBUI + MOCK RUNTIME" : "MOCK RENDER";
const compact = {width: 800, height: 600};
const loaded = {
  wizard: ["#results", "Ready to create a local profile."],
  overview: ["#results details", null], workspace: ["#results details", null],
  recall: ["#results details", null], quickpanel: ["#results details", null],
  diary: ["#results small", "Saved journal entry"],
  organize: ["#results", "Preview the proposed moves"],
  models: ["#active-model", "granite-311m-r2"], storage: ["#results .row .pill", "Active"],
  "session-import": ["#results input:checked", null], stats: ["#results meter", null],
  accounts: ["#spaces .row", null], grants: ["#results", "Enable the synthetic owner session"],
  "space-members": ["#results .row", null], injection: ["#results", ".github/copilot-instructions.md"],
  settings: ["#results", "Current mode:"]
};

async function assertLoaded(page, name, locale) {
  const errorSelector = compiled ? manifest.errorSelector : "#notice[data-error]";
  const [selector, text] = compiled
    ? [manifest.screens[name].ready[locale].selector, manifest.screens[name].ready[locale].text]
    : loaded[name];
  await page.waitForFunction(({selector, text, errorSelector}) =>
    document.querySelector(errorSelector) || [...document.querySelectorAll(selector)].some(node =>
      node.getClientRects().length && (!text || node.textContent.includes(text))),
  {selector, text, errorSelector});
  await expect(page.locator(errorSelector), "Application initialization must not report an error").toHaveCount(0);
  const content = page.locator(selector);
  await expect(text ? content.filter({hasText: text}).first() : content.first()).toBeVisible();
}

let app;
let base;
async function listen(app) {
  await once(app.listen(0, "127.0.0.1"), "listening");
  return "http://127.0.0.1:" + app.address().port;
}

async function close(app) {
  if (!app?.listening) return;
  await new Promise((resolve, reject) => {
    app.close(error => error ? reject(error) : resolve());
    app.closeAllConnections();
  });
}

test.beforeAll(async () => {
  app = server();
  base = await listen(app);
});
test.afterAll(async () => close(app));

async function routeBuild(route, url) {
  const path = route.request().isNavigationRequest() ? "/index.html" : decodeURIComponent(url.pathname);
  const candidate = await realpath(join(build, path));
  const rel = relative(build, candidate);
  assert(rel && rel !== ".." && !rel.startsWith(".." + sep) && !isAbsolute(rel), "Asset must be in build directory");
  assert((await stat(candidate)).isFile(), "Expected a built asset");
  await route.fulfill({path: candidate});
}

async function open(page, name, locale, seed, origin = base, failInit = false) {
  const errors = [];
  page.on("console", message => { if (message.type() === "error") errors.push(message.text()); });
  page.on("pageerror", error => errors.push(error.message));
  await page.route("**/*", async route => {
    const url = new URL(route.request().url());
    if (url.origin !== origin) {
      errors.push("Unexpected network origin: " + url.origin);
      await route.abort();
    } else if (failInit && url.pathname === "/v1/health") {
      await route.fulfill({json: {capabilities: []}});
    } else if (compiled && !url.pathname.startsWith("/v1/") && !url.pathname.startsWith("/preview/")) {
      await routeBuild(route, url);
    } else {
      // Recall initializes by searching; provide valid synthetic input before health releases setup.
      if (!compiled && name === "recall" && url.pathname === "/v1/health") {
        await page.locator("#input").fill("field");
      }
      await route.continue();
    }
  });
  const viewport = name === "quickpanel" ? viewports.quickpanel : viewports.desktop;
  await page.setViewportSize(viewport);
  const entry = manifest?.screens[name];
  const path = entry?.path ?? "/pages/" + name + ".html";
  const query = new URLSearchParams({locale});
  if (seed || name === "models") query.set("scenario", seed ?? "rich-1k");
  await page.goto(origin + path + "?" + query);
  const main = page.getByRole("main");
  const heading = main.getByRole("heading", {level: 1});
  await expect(main).toBeVisible();
  if (!compiled) {
    await page.waitForFunction(() => typeof screenReady !== "undefined");
    if (name !== "models" || seed !== "downloading-sse") await page.evaluate(() => screenReady);
    await expect(page.locator("#notice[data-error]")).toHaveCount(0);
    if (name === "quickpanel") {
      await page.locator("#input").fill("field");
      await page.locator("#search").click();
    }
  }
  await assertLoaded(page, name, locale);
  if (!compiled) {
    await expect(heading).toHaveText(screens.en[name][0]);
    await expect(page.locator("nav a[aria-current=page]")).toHaveText(screens.en[name][1]);
    await page.evaluate(({locale, name, catalog}) => {
      document.documentElement.lang = locale;
      document.querySelector("main h1").textContent = catalog[name][0];
      for (const link of document.querySelectorAll("nav a")) {
        const id = new URL(link.href).pathname.split("/").at(-1).replace(/\.html$/, "");
        link.textContent = catalog[id][1];
      }
    }, {locale, name, catalog: screens[locale]});
    await expect(page.locator("nav a")).toHaveCount(16);
    await expect(page.locator("nav a[aria-current=page]")).toHaveText(screens[locale][name][1]);
    for (const [screen, [, label]] of Object.entries(screens[locale])) {
      await expect(page.locator('nav a[href*="/pages/' + screen + '.html"]')).toHaveText(label);
    }
  }
  await expect(page.locator("html")).toHaveAttribute("lang", new RegExp("^" + locale + "(?:-|$)", "i"));
  await expect(heading).toHaveText(entry?.[locale] ?? screens[locale][name][0]);
  await page.evaluate(() => document.fonts.ready.then(() => {}));
  return errors;
}

for (const name of Object.keys(screens.en)) {
  test(mode + " " + name + " bilingual layout", async ({page}, info) => {
    const errors = await open(page, name, info.project.name);
    const initial = await page.evaluate(measureLayout);
    expect(initial.issues).toEqual([]);
    if (name !== "quickpanel" || !compiled) {
      if (name === "quickpanel") {
        await page.setViewportSize(viewports.desktop);
        expect((await page.evaluate(measureLayout)).issues).toEqual([]);
      }
      await page.setViewportSize(viewports.desktopSmall);
      const small = await page.evaluate(measureLayout);
      expect(small.issues).toEqual([]);
      if (name !== "quickpanel") expect(small.fontSize).toBeLessThan(initial.fontSize);
    }
    if (!compiled) {
      for (const viewport of [viewports.desktop, viewports.desktopSmall, compact]) {
        await page.setViewportSize(viewport);
        expect((await page.evaluate(measureLayout)).issues).toEqual([]);
        const shifts = await page.evaluate(() => {
          const main = document.querySelector("main");
          return [...main.querySelectorAll("#results, #audit, #suggestions, #spaces")].map(list => {
            const card = list.closest("article");
            const geometry = () => [list.offsetHeight, card.offsetHeight, main.offsetHeight, main.scrollHeight];
            const children = [...list.childNodes];
            list.replaceChildren();
            const empty = geometry();
            list.append(...Array.from({length: 40}, () => {
              const row = document.createElement("div");
              row.className = "row";
              row.textContent = "Layout fixture entry";
              return row;
            }));
            const shift = geometry().map((height, index) => height - empty[index]);
            list.replaceChildren(...children);
            return {id: list.id, shift};
          });
        });
        for (const {id, shift} of shifts) expect(shift, `${name} #${id} at ${viewport.width}x${viewport.height}`).toEqual([0, 0, 0, 0]);
      }
      if (name === "overview") {
        await expect(page.locator("#index-state")).toHaveText("Ready");
        await expect(page.locator("#index-coverage")).toHaveText("1000 / 1000 indexed (100%)");
        await expect(page.getByRole("progressbar", {name: "Index coverage"})).toHaveJSProperty("value", 100);
        await expect(page.locator("#model-status")).toHaveText("granite-311m-r2 \u00b7 quint8_avx2 \u00b7 cpu");
      }
      if (name === "accounts") {
        const geometry = () => page.evaluate(() => {
          const spaces = document.querySelector("#spaces");
          const main = document.querySelector("main");
          return [spaces.closest("article").offsetHeight, main.offsetHeight, main.scrollHeight];
        });
        const initialGeometry = await geometry();
        const count = await page.locator("#spaces .row").count();
        for (let index = 0; index < 7; index++) {
          await page.locator("#space-name").fill(`Layout space ${index}`);
          await page.locator("#create").click();
          await expect(page.locator("#spaces .row")).toHaveCount(count + index + 1);
        }
        expect(await geometry()).toEqual(initialGeometry);
        expect(await page.locator("#spaces").evaluate(node => node.scrollHeight > node.clientHeight)).toBe(true);

        await page.locator("#sync").click();
        const notice = page.locator("#notice");
        await expect(notice).toHaveText(/Synthetic sync completed/);
        const toast = await notice.boundingBox();
        expect(toast.y).toBeLessThan(30);
        expect(compact.width - toast.x - toast.width).toBeLessThan(40);
        expect(await geometry()).toEqual(initialGeometry);
        await page.getByRole("button", {name: "Dismiss notice"}).click();
        await expect(page.locator("#notice-dismiss")).toHaveAttribute("data-notice-dismiss", "");
        await expect(notice).toBeHidden();
        expect(await geometry()).toEqual(initialGeometry);
      }
    }
    expect(errors).toEqual([]);
    await expect(page.locator(compiled ? manifest.errorSelector : "#notice[data-error]")).toHaveCount(0);
  });
}

test("MOCK RENDER rejects a synthetic failing initialization", async ({page}, info) => {
  test.skip(Boolean(compiled), "Synthetic initialization targets the gallery only");
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  page.on("console", message => { if (message.type() === "error") errors.push(message.text()); });
  await assert.rejects(open(page, "settings", info.project.name, undefined, base, true), /#notice\[data-error\]/);
  await expect(page.locator("#notice[data-error]")).toHaveText("Runtime API unavailable.");
  expect(errors).toEqual([]);
});

test("MOCK RENDER model progress reconciles after SSE cancellation", async ({page}, info) => {
  test.skip(Boolean(compiled), "Gallery controls belong to Mock Render");
  const controlled = cancellationServer();
  try {
    const origin = await listen(controlled);
    const errors = await open(page, "models", info.project.name, "downloading-sse", origin);
    const cancel = page.getByRole("button", {name: /cancel/i});
    const state = page.locator("#op-state");
    await expect(state).toHaveText("model download / running / 0%");
    await expect(page.locator("#op-target")).toHaveText("granite-97m-r2 / quint8_avx2 / cpu");
    await expect(page.locator("#pipeline-steps li")).toHaveText(["Prepare", "Download", "Verify", "Ready"]);
    await expect(page.locator("#pipeline-steps [aria-current=step]")).toHaveText("Prepare");
    await expect(cancel).toBeVisible();
    await expect(cancel).toBeEnabled();
    const [response] = await Promise.all([
      page.waitForResponse(response => response.request().method() === "DELETE" &&
        new URL(response.url()).pathname === "/v1/operations/op-1"),
      cancel.click()
    ]);
    expect(response.status()).toBe(202);
    expect(await response.json()).toMatchObject({operation_id: "op-1", state: "cancelled"});
    await expect(state).toHaveText("model download / cancelled / 0%");
    await expect(page.locator("#notice")).toContainText(/cancelled/i);
    await expect(cancel).toBeDisabled();
    await expect(page.locator("#notice[data-error]")).toHaveCount(0);
    expect(errors).toEqual([]);
  } finally {
    await close(controlled);
  }
});

test("MOCK RENDER model selection reconciles after SSE completion", async ({page}, info) => {
  test.skip(Boolean(compiled), "Gallery controls belong to Mock Render");
  const errors = await open(page, "models", info.project.name, "rich-1k");
  await page.setViewportSize(compact);
  await page.locator("#results").getByRole("button", {name: /BGE-M3/}).click();
  await page.locator("#variants [data-id=q4f16]").click();
  const geometry = () => page.evaluate(() => [document.querySelector(".twin-cards").offsetHeight,
    document.querySelector("main").offsetHeight, document.querySelector("main").scrollHeight]);
  const initialGeometry = await geometry();
  await page.getByRole("button", {name: "Enable", exact: true}).click();
  await expect(page.locator("#op-state")).toContainText("model switch / running");
  await expect(page.locator("#op-target")).toHaveText("m3 / q4f16 / cpu");
  await expect(page.locator("#pipeline-steps li")).toHaveText(["Verify", "Probe", "Shadow Index", "Switch"]);
  await page.waitForFunction(() => document.querySelector("#pipeline-steps [aria-current=step]")?.textContent === "Shadow Index");
  expect(await geometry()).toEqual(initialGeometry);
  await expect(page.locator("#notice")).toContainText("Operation completed.");
  await expect(page.locator("#active-model")).toHaveText("m3 \u00b7 q4f16 \u00b7 cpu");
  await expect(page.locator("#variants [data-id=q4f16]")).toContainText("Active");
  await expect(page.locator("#notice[data-error]")).toHaveCount(0);
  expect(errors).toEqual([]);
});
