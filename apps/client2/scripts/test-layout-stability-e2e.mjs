import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { startModelsFixture } from "./models-e2e-fixture.mjs";
import { findChromium, loadPlaywright } from "./test-models-e2e.mjs";

let browser;
before(async () => {
  const { chromium } = loadPlaywright();
  const executablePath = findChromium(chromium);
  assert.ok(executablePath, "Set PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH to a Chromium browser");
  browser = await chromium.launch({ executablePath, headless: true });
});
after(() => browser?.close());

const longText = "A long status message with details. ".repeat(50);
const success = (details = [], summary = {}) => ({ envelope: { status: "ok", summary, details } });
const memory = { id: "test-memory", title: "Fixture memory", content: longText, kind: "context" };

async function geometry(page, selectors, relative) {
  return page.evaluate(async ({ selectors, relative }) => {
    await document.fonts.ready;
    await new Promise(resolve => requestAnimationFrame(resolve));
    return Object.fromEntries(selectors.map(selector => {
      const node = document.querySelector(selector);
      if (!node || !node.getClientRects().length) throw new Error(`Missing visible slot: ${selector}`);
      const { top, height } = node.getBoundingClientRect();
      let scrollOffset = 0;
      if (relative) {
        for (let parent = node.parentElement; parent; parent = parent.parentElement) scrollOffset += parent.scrollTop;
      }
      return [selector, [top + scrollOffset, height]];
    }));
  }, { selectors, relative });
}

async function stable(page, selectors, transition, relative = true) {
  const before = await geometry(page, selectors, relative);
  await transition();
  const after = await geometry(page, selectors, relative);
  assert.deepEqual(after, before, `Vertical geometry on ${new URL(page.url()).hash}`);
}

async function contains(page, selector, text) {
  await page.waitForFunction(({ selector, text }) => document.querySelector(selector)?.textContent.includes(text), { selector, text });
}

async function boundedToast(page) {
  const toast = page.locator('#notice[role="status"]');
  await toast.waitFor();
  assert.equal(await toast.evaluate(node => getComputedStyle(node).position), "fixed");
  const box = await toast.boundingBox();
  const { width, height } = page.viewportSize();
  assert.ok(box.x >= 0 && box.y >= 0 && box.x + box.width <= width && box.y + box.height <= height);
  return box;
}

async function assertToast(page) {
  const box = await boundedToast(page);
  for (const header of await page.locator(".topbar .top-actions, .view-page > .page-heading, [data-overview-summary] > .page-heading, #view > .tabs").all()) {
    const bounds = await header.boundingBox();
    assert.ok(box.y >= bounds.y + bounds.height, `Toast must clear the workspace header controls on ${new URL(page.url()).hash}`);
  }
  await page.locator("#theme-dropdown .dropdown-toggle").click();
  await page.locator("#theme-dropdown .dropdown-menu").waitFor();
  await page.keyboard.press("Escape");
}

for (const [width, height, colorScheme] of [[1180, 800, "light"], [800, 600, "dark"]]) {
  test(`Client2 keeps desktop geometry at ${width}x${height} in ${colorScheme} mode`, { timeout: 60000 }, async t => {
    const scenario = {
      state: {}, status: { unlocked: true }, operations: [],
      agents: { targets: [{ id: "codex", name: "Fixture agent", mode: "all", state: "fresh", path: longText }] },
      commands: {
        account_list: () => success(Array.from({ length: 20 }, (_, index) => ({ name: `Profile ${index}`, dir: longText }))),
        space_list: () => success([{ id: "main", name: "Fixture space", current: true }]),
        capabilities: () => success(),
        memory_list: () => success([memory]),
        memory_show: () => success({ entry: memory }),
        memory_revision: () => success([], { revision: "fixture" }),
        organize_suggestions: () => success({ suggests: [{ orphan: memory, target: { title: "Fixture parent" } }] }),
        session_import_targets: () => success([{ id: "codex", name: "Fixture source", installed: true }]),
        session_import_preview: () => success({ sessions: [{ id: "session", platform: "Fixture session" }] }, { pending: 1 }),
        grant_list: () => success([{ id: "grant", root_id: memory.id, label: "Fixture grant" }]),
        doctor: () => success([longText]),
      },
    };
    const fixture = await startModelsFixture({ scenario });
    const context = await browser.newContext({ viewport: { width, height }, colorScheme, reducedMotion: "reduce" });
    const errors = [];
    t.after(async () => {
      try { await context.close(); } finally { await fixture.close(); }
      assert.deepEqual([...errors, ...fixture.errors.map(String)], []);
    });
    const page = await context.newPage();
    page.on("pageerror", error => errors.push(error.message));
    page.setDefaultTimeout(6000);
    await page.clock.install();
    await page.goto(`${fixture.url}#overview`);

    async function navigate(route, ready) {
      await page.evaluate(route => { location.hash = route; }, route);
      await page.locator(`.view-page[data-page="${route}"]`).locator(ready).first().waitFor();
    }

    for (const [route, panel] of [
      ["overview", "[data-overview-metrics]"], ["models", "[data-catalog-models]"],
      ["accounts", "[data-session] > .panel"], ["settings", ".panel:has([data-proxy])"],
    ]) {
      await navigate(route, panel);
      const selectors = [".topbar", ".view-scroll", "#view", "#view .page-heading", panel, ".statusbar"];
      await stable(page, selectors, async () => {
        await page.evaluate(async () => (await import("/ui/core.js")).notify("Fixture notice"));
        await assertToast(page);
      }, false);
      if (route === "overview") {
        const slots = ["[data-overview-metrics]", "[data-overview-status-error]", "[data-overview-stats]", "[data-overview-details]", "[data-runtime-status]", '[data-host="start"]'];
        await stable(page, slots, async () => {
          scenario.statusError = "Fixture Runtime error";
          await page.clock.fastForward(30000);
          await contains(page, "[data-overview-status-error]", scenario.statusError);
        });
        await stable(page, slots, async () => {
          scenario.statusError = "";
          await page.clock.fastForward(30000);
          await page.waitForFunction(() => !document.querySelector("[data-overview-status-error]").textContent);
        });
      }
      if (route === "accounts") {
        await contains(page, "[data-accounts]", "Profile 19");
        await stable(page, [".topbar", ".view-scroll", "#view", ".view-page", ".page-heading", "[data-session]", "[data-accounts]", "[data-spaces]", "[data-server]"], async () => {
          scenario.status.session = { user: "Fixture profile", has_token: true, has_local_keys: true };
          scenario.status.data_dir = longText;
          await page.clock.fastForward(30000);
          await contains(page, '[data-session-field="user"]', "Fixture profile");
          assert.ok(await page.locator("[data-accounts]").evaluate(node => node.scrollHeight > node.clientHeight));
          scenario.status.session = {};
        });
      }
      await stable(page, selectors, async () => {
        await page.locator("[data-notice-dismiss]").focus();
        await page.evaluate(async text => (await import("/ui/core.js")).notify(text, true), longText);
        assert.equal(await page.locator("[data-notice-dismiss]").evaluate(node => node === document.activeElement), true);
        await assertToast(page);
        await page.locator("[data-notice-dismiss]").click();
        await page.locator("#notice").waitFor({ state: "hidden" });
      }, false);
      await stable(page, selectors, async () => {
        await page.evaluate(async () => {
          const { notify, action } = await import("/ui/core.js");
          notify("Cleared by action");
          await action(document.querySelector("#refresh"), () => {});
        });
        await page.locator("#notice").waitFor({ state: "hidden" });
      }, false);
      if (route === "overview") {
        await page.locator('[data-overview-summary] [data-go="memories"]').click();
        await page.locator('.view-page[data-page="memories"]').waitFor();
      }
    }

    if (width === 800) {
      await page.evaluate(async text => {
        const { applyTypography, normalizeTypography } = await import("/ui/typography.js");
        applyTypography(normalizeTypography({ uiScale: 2 }));
        (await import("/ui/core.js")).notify(text, true);
      }, longText);
      await boundedToast(page);
      await page.locator("[data-notice-dismiss]").click();
      await navigate("diagnostics", "[data-doctor]");
      await page.evaluate(async text => (await import("/ui/core.js")).notify(text, true), longText);
      const toast = await boundedToast(page);
      const doctor = await page.locator("[data-doctor]").boundingBox();
      assert.ok(toast.y >= doctor.y + doctor.height, `Toast starts at ${toast.y} and diagnostics control ends at ${doctor.y + doctor.height}`);
      await page.locator("[data-notice-dismiss]").click();
      await page.evaluate(async () => (await import("/ui/typography.js")).applyTypography());
      await navigate("settings", "[data-runtime]");
    }

    const settingsGeometry = [".view-scroll", "#view > .tabs", "#view .page-heading"];
    for (const [name, ready] of [["updates", "[data-update]"], ["operations", "[data-jobs] .panel"], ["diagnostics", "[data-connection]"], ["settings", "[data-runtime]"]]) {
      await stable(page, settingsGeometry, async () => {
        await page.locator("#view > .tabs button").filter({ hasText: new RegExp(`^${name}$`, "i") }).click();
        await page.locator(ready).first().waitFor();
      });
      if (name === "updates") {
        const slots = ["#view .grid > .panel", "#view .grid + .panel"];
        await stable(page, slots, async () => {
          scenario.state.clientUpdate = { state: "error", error: longText, notes: longText, cancellable: true };
          scenario.state.task = { busy: true, done: 1, total: 5, stage: "Fixture running" };
          await page.clock.fastForward(30000);
          await contains(page, "#view .grid + .panel", "Fixture running");
        });
        await stable(page, slots, async () => {
          scenario.state.task = { error: "Fixture failure" };
          await page.clock.fastForward(3000);
          await contains(page, "#view .grid + .panel", "Fixture failure");
        });
      }
      if (name === "operations") {
        await stable(page, ["[data-jobs]", "div[data-receipt]"], async () => {
          scenario.operations = Array.from({ length: 30 }, (_, index) => ({ id: `job-${index}`, name: `Fixture operation ${index}`, state: "running" }));
          await page.locator("#refresh").click();
          await contains(page, "[data-jobs]", "Fixture operation 29");
          assert.ok(await page.locator("[data-jobs]").evaluate(node => node.scrollHeight > node.clientHeight));
        });
      }
    }

    await navigate("models", '[data-catalog-model="granite-311m-r2"]');
    await page.locator('[data-model-dock] [data-catalog-action]').first().waitFor();
    assert.equal(await page.locator("[data-live] details, [data-live] pre").count(), 0);
    const twinHeights = await page.evaluate(() => [
      document.querySelector("[data-models-live-card]").getBoundingClientRect().height,
      document.querySelector("[data-models-tasks-card]").getBoundingClientRect().height,
    ]);
    assert.equal(twinHeights[0], twinHeights[1]);
    const modelsSlots = ["[data-catalog-models]", "[data-variants]", "[data-model-dock]", "[data-models-live-card]", "[data-models-tasks-card]", "[data-recall]"];
    await stable(page, modelsSlots, async () => {
      scenario.status.retrieval_index = { state: "error", error: longText };
      await page.clock.fastForward(30000);
      await contains(page, "[data-model-task-error]", longText);
    });
    await stable(page, modelsSlots, async () => {
      delete scenario.status.retrieval_index;
      await page.clock.fastForward(30000);
      await page.locator("[data-model-task-error]").waitFor({ state: "hidden" });
    });
    await stable(page, modelsSlots, async () => {
      await page.locator("[data-catalog-model=bge-base-zh-v1\\.5]").click();
      await contains(page, "[data-variants]", "model_quantized.onnx");
      await page.locator("[data-catalog-model=granite-97m-r2]").click();
      await contains(page, "[data-variants]", "model.onnx");
    });
    await stable(page, [...modelsSlots, "[data-mirror] [name=mirror]"], async () => {
      await page.locator("[data-mirror] [name=mirror]").selectOption("custom");
      await page.locator("[name=mirror-custom]").waitFor();
    });

    await navigate("memories", '[data-select="test-memory"]');
    await stable(page, [".memory-toolbar", ".memory-layout", ".memory-reader"], async () => {
      await page.locator('[data-select="test-memory"]').first().click();
      await contains(page, ".memory-reader", "Fixture memory");
      await page.locator('[data-view="list"]').click();
      await page.locator('.memory-row.list-row').waitFor();
    });
    await page.locator("[data-focus]").click();
    await stable(page, [".view-scroll", ".memory-layout"], async () => {
      await page.evaluate(async () => (await import("/ui/core.js")).notify("Focus mode notice"));
      await page.locator("#notice").waitFor();
      await page.locator("[data-notice-dismiss]").click();
    }, false);
    await navigate("diary", ".calendar-day");
    await stable(page, [".diary-grid", "[data-calendar]", ".diary-day-panel"], async () => {
      const month = await page.locator('input[type="month"]').inputValue();
      await page.locator("[data-next]").click();
      assert.notEqual(await page.locator('input[type="month"]').inputValue(), month);
      await page.locator(".calendar-day").first().click();
    });
    await navigate("organize", "[data-report]");
    await stable(page, ["[data-report]", "[data-ai]"], async () => {
      await page.locator("[data-analyze=suggestions]").click();
      await contains(page, "[data-report]", "Fixture parent");
    });
    await navigate("session-import", "input[data-source=codex]");
    await stable(page, ["[data-sources]", "[data-preview-result]", "[data-apply]", "[data-apply-result]"], async () => {
      await page.locator("[data-preview]").click();
      await contains(page, "[data-preview-result]", "Fixture session");
      const limit = page.locator("[data-limit]");
      await limit.fill("42");
      await limit.blur();
    });
    await navigate("injection", "[data-scope-tab=plugin]");
    await stable(page, ["[data-scope-note]", "[data-agents]"], async () => {
      await page.locator("[data-scope-tab=plugin]").click();
      await page.locator('[data-scope-tab=plugin][aria-selected="true"]').waitFor();
    });
    await navigate("grants", "[data-grants] .panel");
    await contains(page, "[data-grants]", "Fixture grant");

    fixture.preferences.onboardingCompleted = false;
    fixture.preferences.onboardingStep = "model";
    await page.reload();
    await page.locator("[data-next-model]").waitFor();
    const wizardSlots = [".wizard-card", ".wizard-steps", ".wizard-step-content", ".wizard-actions"];
    await stable(page, wizardSlots, async () => {
      await page.locator("[data-choice=granite-97m-r2]").click();
      await page.locator("[data-next-model]").click();
      await page.locator("[data-weights] [data-choice]").first().waitFor();
    });
    await stable(page, wizardSlots, async () => {
      await page.locator('[data-weights] [data-choice="standard"]').click();
      await page.locator("[data-device-next]").click();
      await page.locator('.wizard-step[aria-current="step"]').filter({ hasText: /confirm/i }).waitFor();
    });
  });
}
