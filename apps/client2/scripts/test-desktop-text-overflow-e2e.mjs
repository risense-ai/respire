import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { startModelsFixture } from "./models-e2e-fixture.mjs";
import { findChromium, loadPlaywright } from "./test-models-e2e.mjs";

const longToken = "UnbreakableDesktopText".repeat(4);
const success = (details = []) => ({ envelope: { status: "ok", summary: {}, details } });
const memory = { id: "long-title", title: longToken, content: "Fixture memory", kind: "context" };

let browser;
before(async () => {
  const { chromium } = loadPlaywright();
  const executablePath = findChromium(chromium);
  assert.ok(executablePath, "Set PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH to a Chromium browser");
  browser = await chromium.launch({ executablePath, headless: true });
});
after(() => browser?.close());

async function assertBounds(page, view, checks = [], narrow = false) {
  const failures = await page.evaluate(async ({ checks, longToken, narrow }) => {
    await document.fonts.ready;
    const failures = [];
    const check = (condition, message) => { if (!condition) failures.push(message); };
    const fits = (node, label, tolerance = 0) => check(
      node.scrollWidth <= node.clientWidth + tolerance,
      `${label}: scrollWidth ${node.scrollWidth} exceeds clientWidth ${node.clientWidth}`,
    );
    const visible = selector => [...document.querySelectorAll(selector)].filter(node => node.getClientRects().length);
    for (const { text, card, inject, memoryTitle, elementOnly, ellipsis } of checks) {
      const nodes = visible(text);
      check(nodes.length > 0, `Missing visible ${text}`);
      for (const node of nodes) {
        if (inject) node.prepend(`${longToken} `);
        const container = node.closest(card);
        check(container, `${text} has no ${card} ancestor`);
        if (!container) continue;
        const label = `${text} in ${card}`;
        fits(container, label, 1);
        const bounds = container.getBoundingClientRect();
        const inside = rect => rect.left >= bounds.left - 1 && rect.right <= bounds.right + 1;
        check(inside(node.getBoundingClientRect()), `${label}: element crosses card edge`);
        if (elementOnly) continue;
        const style = getComputedStyle(node);
        if (memoryTitle && narrow) {
          check(style.whiteSpace === "normal" && style.overflowWrap === "anywhere",
            `${label}: expected normal/anywhere, got ${style.whiteSpace}/${style.overflowWrap}`);
        }
        if (ellipsis || (memoryTitle && !narrow)) {
          check(style.textOverflow === "ellipsis" && style.overflowX === "hidden",
            `${label}: expected contained ellipsis`);
          continue;
        }
        const range = document.createRange();
        range.selectNodeContents(node);
        check([...range.getClientRects()].every(inside), `${label}: text crosses card edge`);
      }
    }
    fits(document.documentElement, "document.documentElement");
    fits(document.querySelector(".view-scroll"), ".view-scroll");
    return [...new Set(failures)];
  }, { checks, longToken, narrow });
  assert.deepEqual(failures, [], `${view} has horizontal overflow`);
}

async function assertModeContract(page) {
  const failures = await page.evaluate(longToken => {
    function hasModeRules(rules) {
      return [...rules].some(rule => rule.selectorText === ".mode-card" ||
        (rule.cssRules && hasModeRules(rule.cssRules)) ||
        (rule.styleSheet && hasModeRules(rule.styleSheet.cssRules)));
    }
    let selector = document.querySelector(".mode-selector");
    let probe;
    if (!selector) {
      if (![...document.styleSheets].some(sheet => hasModeRules(sheet.cssRules))) return [];
      probe = document.createElement("div");
      probe.className = "mode-selector";
      probe.setAttribute("data-mode-selector", "");
      probe.innerHTML = `<button class="mode-card">${longToken}</button>`.repeat(3);
      document.querySelector("[data-catalog-models]").parentElement.append(probe);
      selector = probe;
    }
    try {
      const cards = [...selector.querySelectorAll(".mode-card")];
      const failures = [];
      if (getComputedStyle(selector).display !== "grid" || !cards.length) failures.push("Missing mode grid or cards");
      for (const card of cards) {
        const { minWidth, whiteSpace } = getComputedStyle(card);
        if (minWidth !== "0px" || whiteSpace !== "normal" || card.scrollWidth > card.clientWidth + 1) {
          failures.push(`Mode card: ${minWidth}, ${whiteSpace}, ${card.scrollWidth}/${card.clientWidth}`);
        }
      }
      return failures;
    } finally {
      probe?.remove();
    }
  }, longToken);
  assert.deepEqual(failures, [], "Models mode-card CSS contract");
}

for (const [width, height, colorScheme] of [[1180, 800, "light"], [1024, 768, "light"], [800, 600, "dark"]]) {
  test(`Desktop text stays within bounds at ${width}x${height} (${colorScheme})`, { timeout: 60000 }, async t => {
    const fixture = await startModelsFixture({ scenario: {
      status: { unlocked: true, retrieval_index: { state: "error", error: longToken } },
      commands: {
        account_list: () => success(),
        space_list: () => success([{ id: "main", name: "Fixture space", current: true }]),
        capabilities: () => success(),
        memory_list: () => success([memory]),
        memory_revision: () => success(),
        diary_list: ({ from }) => success([2, 3].flatMap(count =>
          Array.from({ length: count }, (_, index) => ({
            id: `diary-${count}-${index}`, day: `${from.slice(0, 7)}-0${count}`, title: longToken,
          })))),
      },
    } });
    let context;
    const errors = [];
    t.after(async () => {
      try { await context?.close(); } finally { await fixture.close(); }
      assert.deepEqual([...errors, ...fixture.errors.map(String)], []);
    });
    context = await browser.newContext({ viewport: { width, height }, colorScheme, reducedMotion: "reduce" });
    const page = await context.newPage();
    page.on("pageerror", error => errors.push(error.message));
    page.setDefaultTimeout(7000);
    await page.goto(`${fixture.url}#overview`);
    await page.locator(`html[data-theme="${colorScheme}"]`).waitFor();

    async function navigate(route, ready) {
      await page.evaluate(route => { location.hash = route; }, route);
      return page.locator(`.view-page[data-page="${route}"]`).locator(ready).first().waitFor();
    }

    const overviewNote = '[data-overview-summary] .metric > small.form-error';
    await page.locator(overviewNote).filter({ hasText: longToken }).waitFor();
    const overviewModels = ":is([data-overview-models], [data-overview-model-content])";
    await assertBounds(page, "Overview", [
      { text: overviewNote, card: ".metric" },
      { text: `${overviewModels} .overview-serving-model strong:first-child`, card: overviewModels, inject: true },
    ]);

    const narrow = width <= 1100;
    const memoryTitle = '.memory-title[data-select="long-title"]';
    await navigate("memories", memoryTitle);
    assert.equal(await page.locator(memoryTitle).textContent(), longToken);
    await assertBounds(page, "Memories tree", [{ text: memoryTitle, card: ".memory-row", memoryTitle: true }], narrow);
    await page.locator('[data-view="list"]').click();
    await page.locator(`.memory-list ${memoryTitle}`).waitFor();
    await assertBounds(page, "Memories list", [
      { text: memoryTitle, card: ".memory-row", memoryTitle: true },
      { text: ".memory-list .memory-row", card: ".memory-list", elementOnly: true },
    ], narrow);

    const diaryCount = ".diary-grid .calendar-day small";
    await navigate("diary", diaryCount);
    assert.deepEqual((await page.locator(diaryCount).allTextContents()).map(label => label.match(/^\d+/)?.[0]), ["2", "3"]);
    const diaryCheck = { text: diaryCount, card: ".calendar-day" };
    await assertBounds(page, "Diary counts", [diaryCheck]);
    await assertBounds(page, "Diary stress text", [{ ...diaryCheck, inject: true }]);

    const catalog = ":is(.catalog-card, [data-catalog-model])";
    const variant = ":is([data-variant], [data-catalog-variant])";
    await navigate("models", variant);
    const taskError = "[data-model-task-error]";
    await page.locator(taskError).filter({ hasText: longToken }).waitFor();
    await assertModeContract(page);
    await assertBounds(page, "Models", [
      { text: `${catalog} > strong`, card: catalog, inject: true },
      { text: `${variant} > strong`, card: variant, inject: true },
      { text: `${variant} > code`, card: variant, inject: true, ellipsis: true },
      { text: "[data-models-live-card] .model-status-identity strong", card: "[data-models-live-card]", inject: true },
      { text: taskError, card: "[data-models-tasks-card]" },
    ]);

    for (const [route, ready] of [["accounts", "[data-session] > .panel"], ["settings", ".panel:has([data-proxy])"]]) {
      await navigate(route, ready);
      await assertBounds(page, route);
    }

    fixture.preferences.onboardingCompleted = false;
    fixture.preferences.onboardingStep = "model";
    await page.reload();
    await page.locator("[data-next-model]").waitFor();
    const choice = ":is(.choice-card, .wizard-choice)";
    await assertBounds(page, "Onboarding Wizard", [
      { text: ".wizard-card h1", card: ".wizard-card", inject: true },
      { text: `${choice} strong`, card: choice, inject: true },
      { text: ".wizard-step-content", card: ".wizard-step-content", elementOnly: true },
    ]);
  });
}
