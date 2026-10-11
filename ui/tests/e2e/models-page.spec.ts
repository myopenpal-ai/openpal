import { expect, test, type Page } from "@playwright/test";
import { mockTauriIpc } from "./support/tauriMock";
import { freshInstallResponses } from "./support/fixtures";

/**
 * The Models page's compact list exists because the table it replaced could
 * not fit: thirteen `white-space: nowrap` columns inside a content column
 * that is 746px wide in the shipped window and 546px at its minimum. jsdom
 * has no layout engine, so the unit tests can assert the structure but not
 * the thing the user actually complained about. These run in a real browser
 * and measure it.
 *
 * The window sizes below are not arbitrary — `crates/app/tauri.conf.json`
 * ships 1100x740 with `minWidth: 900`, so 900 is the narrowest the page can
 * ever be asked to render, and the widths in library.css are chosen against
 * exactly these numbers.
 */

function spec(over: Record<string, unknown> & { id: string; name: string }) {
  return {
    family: "Test",
    description: "A model used by the layout regression test.",
    strengths: [],
    url: "https://example.com/model.gguf",
    fileName: "model.gguf",
    sizeBytes: 4_590_807_392,
    quant: "Q4_K_M",
    sha256: null,
    paramsB: 8,
    moe: false,
    contextMax: 131072,
    qualityScore: 50,
    license: "Apache 2.0",
    extraArgs: [],
    hidden: false,
    curated: false,
    source: "catalog",
    localPath: null,
    vision: false,
    taskScores: {},
    releaseDate: "2025-08-01",
    publisher: "Google",
    ...over,
  };
}

/** Deliberately the worst realistic case for width: the longest fit label,
 * a long publisher, an MoE parameter string, a flag badge, and a model the
 * user added themselves (which is what makes the toolbar render). */
const MODELS = [
  {
    spec: spec({
      id: "gemma-4-e2b",
      name: "Gemma 4 E2B",
      curated: true,
      qualityScore: 30,
      vision: true,
    }),
    status: { type: "notInstalled" },
    fit: "perfect",
    why: "Fits easily in your memory.",
    isRecommended: true,
    favorite: false,
  },
  {
    spec: spec({ id: "gemma-4-e4b", name: "Gemma 4 E4B", curated: true, qualityScore: 68 }),
    status: { type: "installed", data: { path: "/m/a.gguf", sizeBytes: 4_590_807_392 } },
    fit: "perfect",
    why: "Fits easily in your memory.",
    isRecommended: false,
    favorite: true,
  },
  {
    spec: spec({
      id: "gpt-oss-20b",
      name: "GPT-OSS 20B",
      publisher: "OpenAI",
      curated: true,
      moe: true,
      paramsB: 21,
      activeParamsB: 4,
      quant: "MXFP4",
      sizeBytes: 11_800_000_000,
      qualityScore: 72,
    }),
    status: { type: "notInstalled" },
    fit: "diskSlow",
    why: "Bigger than your memory.",
    isRecommended: false,
    favorite: false,
  },
  {
    spec: spec({
      id: "qwen-hf",
      name: "Qwen3.6 27B Instruct",
      publisher: "Alibaba (Qwen team)",
      source: "huggingFace",
      groupId: "qwen/Qwen3.6-27B",
      license: "Apache 2.0",
      qualityScore: 60,
    }),
    status: { type: "notInstalled" },
    fit: "tight",
    why: "A bit snug.",
    isRecommended: false,
    favorite: false,
  },
  {
    spec: spec({
      id: "qwen-hf-q8",
      name: "Qwen3.6 27B Instruct Q8",
      publisher: "Alibaba (Qwen team)",
      source: "huggingFace",
      groupId: "qwen/Qwen3.6-27B",
      quant: "Q8_0",
      sizeBytes: 9_000_000_000,
      qualityScore: 61,
    }),
    status: { type: "notInstalled" },
    fit: "tight",
    why: "A bit snug.",
    isRecommended: false,
    favorite: false,
  },
];

async function openModelsPage(page: Page, language: "en" | "fr") {
  const base = freshInstallResponses();
  await mockTauriIpc(page, {
    ...base,
    get_settings: {
      ...(base.get_settings as Record<string, unknown>),
      onboardingCompleted: true,
      userName: "Ada",
      language,
    },
    get_models: MODELS,
  });
  await page.goto("/");
  // The Models page lives behind the sidebar's overflow menu, and these
  // labels are themselves translated. It opens as a page of the More dialog.
  const models = language === "fr" ? "Modèles" : "Models";
  await page.getByRole("button", { name: language === "fr" ? "Plus" : "More", exact: true }).click();
  await page.getByRole("button", { name: models }).click();
  // Scoped to the dialog: the chat stays mounted underneath, heading and all.
  const dialog = page.getByRole("dialog", { name: models });
  await expect(dialog.getByRole("heading", { level: 1 })).toBeVisible();
}

/** An explainer popover — told apart from the More dialog the page sits in,
 * which is a `dialog` too. */
function infoPopover(page: Page) {
  return page.getByRole("dialog").and(page.locator(".info-pop"));
}

/** What a horizontal scrollbar would actually be caused by. `.library-scroll`
 * is the scroll container; the document is the backstop. */
async function overflow(page: Page) {
  return page.evaluate(() => {
    const scroller = document.querySelector<HTMLElement>(".library-scroll")!;
    const col = document.querySelector<HTMLElement>(".library-col")!;
    const rows = [...document.querySelectorAll<HTMLElement>(".model-row")];
    const pad = getComputedStyle(col);
    return {
      document: document.documentElement.scrollWidth - document.documentElement.clientWidth,
      scroller: scroller.scrollWidth - scroller.clientWidth,
      worstRow: Math.max(0, ...rows.map((r) => r.scrollWidth - r.clientWidth)),
      // The content box, not `clientWidth` — that includes the column's own
      // 36px of side padding, which is not space a row can use.
      contentWidth:
        col.clientWidth - parseFloat(pad.paddingLeft) - parseFloat(pad.paddingRight),
      rowCount: rows.length,
    };
  });
}

for (const language of ["en", "fr"] as const) {
  test(`the models list never scrolls sideways, at any window size (${language})`, async ({
    page,
  }) => {
    await openModelsPage(page, language);

    // The default shipped window.
    await page.setViewportSize({ width: 1100, height: 740 });
    let m = await overflow(page);
    expect(m.rowCount).toBeGreaterThan(0);
    expect(m.document, "document overflows at the default window").toBeLessThanOrEqual(0);
    expect(m.scroller, "the models list overflows at the default window").toBeLessThanOrEqual(0);
    expect(m.worstRow, "a row overflows at the default window").toBeLessThanOrEqual(0);
    // Guards the width budget documented at the top of library.css: 788px is
    // the `max-width` cap, reached only above a 1142px window, so the default
    // window must come in under it.
    expect(m.contentWidth).toBeLessThanOrEqual(788);
    expect(m.contentWidth).toBeGreaterThan(600);

    // The narrowest the window can ever be (tauri.conf.json minWidth).
    await page.setViewportSize({ width: 900, height: 740 });
    m = await overflow(page);
    expect(m.document, "document overflows at the minimum window").toBeLessThanOrEqual(0);
    expect(m.scroller, "the models list overflows at the minimum window").toBeLessThanOrEqual(0);
    expect(m.worstRow, "a row overflows at the minimum window").toBeLessThanOrEqual(0);

    // And everywhere in between, which is where the breakpoints live.
    for (const width of [1000, 960, 920]) {
      await page.setViewportSize({ width, height: 740 });
      m = await overflow(page);
      expect(m.scroller, `the models list overflows at ${width}px`).toBeLessThanOrEqual(0);
      expect(m.worstRow, `a row overflows at ${width}px`).toBeLessThanOrEqual(0);
    }
  });
}

test("the table is gone and every model is one row", async ({ page }) => {
  await openModelsPage(page, "en");
  expect(await page.locator("table").count()).toBe(0);
  // Four top-level rows: three curated picks plus one group for the two
  // quantizations sharing a Hugging Face repo.
  await expect(page.locator(".model-list > .model-row")).toHaveCount(4);
});

test("a multi-quantization group collapses into one row and expands on demand", async ({
  page,
}) => {
  await openModelsPage(page, "en");
  const group = page.locator(".model-row-group");
  await expect(group).toHaveCount(1);
  await expect(group).toContainText("2 quantizations");

  // Nothing in the group is installed or downloading, so it starts collapsed.
  await expect(page.locator(".model-row-quants")).toHaveCount(0);
  await page.getByRole("button", { name: /show the 2 quantizations/i }).click();
  await expect(page.locator(".model-row-quants .model-row")).toHaveCount(2);
});

test("the fit verdict is a small icon, not a block of text on every row", async ({ page }) => {
  // French is where the wording hurt most — "Parfaitement adapté" ran about
  // 145px, the widest thing on the line.
  await openModelsPage(page, "fr");
  // ~17px for the verdict glyph, ~31px when the perfect-fit bolt rides along
  // — against the ~145px "Parfaitement adapté" ran to as text.
  const box = await page.locator(".model-row .model-fit").first().boundingBox();
  expect(box!.width).toBeLessThan(40);

  // The wording is one click away rather than gone.
  await page.getByRole("button", { name: /que signifient les icônes/i }).click();
  const legend = infoPopover(page);
  await expect(legend).toContainText("Parfaitement adapté");
  await expect(legend.locator(".fit-legend li")).toHaveCount(5);
  // The bolt that marks the fastest picks is explained here too.
  await expect(legend.locator(".model-fit-bolt")).toHaveCount(1);
});

test("a disk-streamed model explains on hover that it doesn't fit in memory", async ({ page }) => {
  await openModelsPage(page, "en");
  const row = page.locator(".model-row", { hasText: "GPT-OSS 20B" });
  const title = await row.locator(".model-fit").getAttribute("title");
  expect(title).toMatch(/doesn't fit in your memory/i);
  expect(title).toMatch(/can still be used/i);
  expect(title).toMatch(/far slower/i);
});

test("an explainer opened from below the fold stays open, fully on screen", async ({ page }) => {
  await openModelsPage(page, "en");
  // A short window leaves the group row below the fold, so clicking its "i"
  // makes the browser scroll it into view first. That scroll used to reach
  // the close-on-scroll listener and shut the panel in the same breath it
  // opened — invisible to jsdom, which has no scrolling.
  await page.setViewportSize({ width: 1100, height: 520 });
  await page.getByRole("button", { name: /what is a quantization/i }).click();

  const panel = infoPopover(page);
  await expect(panel).toBeVisible();
  await expect(panel).toContainText(/smaller file size/i);
  // Painted on top, not merely present: the Models page sits in the More
  // dialog, so the panel (portalled to <body>) must layer above that too.
  const onTop = await panel.evaluate((el) => {
    const r = el.getBoundingClientRect();
    return el.contains(document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2));
  });
  expect(onTop, "the explainer is hidden behind the More dialog").toBe(true);

  // These triggers sit at the end of a row, inches from the right edge, so
  // an unclamped panel loses most of its text off-screen.
  const box = (await panel.boundingBox())!;
  const viewport = page.viewportSize()!;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.y).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(viewport.width);
  expect(box.y + box.height).toBeLessThanOrEqual(viewport.height);
});

test("Escape closes an open explainer, not the More dialog around the page", async ({ page }) => {
  await openModelsPage(page, "en");
  await page.getByRole("button", { name: /what is a quantization/i }).click();
  await expect(infoPopover(page)).toBeVisible();

  await page.keyboard.press("Escape");
  await expect(infoPopover(page)).toHaveCount(0);
  await expect(page.getByRole("dialog", { name: "Models" })).toBeVisible();

  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
});

test("a row's actions menu paints above the More dialog it opens from", async ({ page }) => {
  await openModelsPage(page, "en");
  await page.getByRole("button", { name: /more actions for/i }).first().click();

  const menu = page.locator(".model-row-menu");
  await expect(menu).toBeVisible();
  const onTop = await menu.evaluate((el) => {
    const r = el.getBoundingClientRect();
    return el.contains(document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2));
  });
  expect(onTop, "the row menu is hidden behind the More dialog").toBe(true);
});

test("the curated picker is a radiogroup whose arrow keys change the spotlight card", async ({
  page,
}) => {
  await openModelsPage(page, "en");
  const stops = page.getByRole("radio");
  await expect(stops).toHaveCount(3);

  // Gemma 4 E2B is flagged as our pick, so the dial opens on it.
  await expect(page.getByRole("heading", { level: 2, name: "Gemma 4 E2B" })).toBeVisible();
  await stops.first().focus();
  await page.keyboard.press("ArrowRight");
  await expect(page.getByRole("heading", { level: 2, name: "Gemma 4 E4B" })).toBeVisible();
});
