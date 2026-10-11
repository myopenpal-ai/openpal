import { expect, test } from "@playwright/test";
import { mockTauriIpc } from "./support/tauriMock";
import { freshInstallResponses } from "./support/fixtures";

/** A not-installed catalog model, per the `MODEL` fixture in
 *  onboarding-skip.spec.ts — just enough spec for the recommend scene. */
const makeModel = (id: string, name: string) => ({
  spec: {
    id,
    name,
    family: "Qwen",
    description: "A recommendation test model.",
    strengths: [],
    url: `https://example.com/${id}.gguf`,
    fileName: `${id}.gguf`,
    sizeBytes: 2_393_165_824,
    quant: "Q4_K_M",
    sha256: null,
    paramsB: 4,
    moe: false,
    contextMax: 32768,
    qualityScore: 50,
    license: "Apache 2.0",
    extraArgs: [],
    hidden: false,
    curated: false,
    source: "catalog",
    localPath: null,
    vision: false,
    taskScores: {},
    releaseDate: "2025-01-01",
    publisher: "Qwen",
  },
  status: { type: "notInstalled" },
  fit: "good",
  why: "Fits your Mac well.",
  isRecommended: false,
  favorite: false,
});

test("a fresh install boots into the onboarding welcome screen", async ({ page }) => {
  await mockTauriIpc(page, freshInstallResponses());

  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") consoleErrors.push(msg.text());
  });

  await page.goto("/");

  await expect(page.getByRole("button", { name: "Let's set things up" })).toBeVisible();
  await expect(page.getByText("OpenPal", { exact: true })).toBeVisible();

  expect(consoleErrors).toEqual([]);
});

test("onboarding saves a name before choosing a model", async ({ page }) => {
  await mockTauriIpc(page, freshInstallResponses({ update_settings: null }));

  await page.goto("/");

  await page.getByRole("button", { name: "Let's set things up" }).click();
  await expect(page.getByRole("heading", { name: "What should we call you?" })).toBeVisible();

  await page.getByLabel("Your name").fill("Ada");
  await page.getByRole("button", { name: "Continue" }).click();

  await expect(page.getByRole("heading", { name: "First, let's see what your Mac can do." })).toBeVisible();

  // Regression guard for the scan stat cards: the Graphics value ("Ready for
  // AI ✓") is the widest of the four, and used to overflow its fixed ~126px
  // grid column. No `.ob-stat` card may scroll horizontally — i.e. no value
  // may be wider than its box (values wrap within the card instead).
  await expect(async () => {
    const overflowed = await page.$$eval(".ob-stat", (cards) =>
      cards.some((c) => c.scrollWidth > c.clientWidth + 0.5),
    );
    expect(overflowed).toBe(false);
  }).toPass();
});

test("'Pick a different one' lists the recommended model first and scrolls instead of clipping", async ({
  page,
}) => {
  // Shorter than the expanded content, so its overflow actually bites.
  await page.setViewportSize({ width: 1280, height: 620 });

  const primary = makeModel("qwen3-4b", "Qwen 3 4B");
  const alts = Array.from({ length: 6 }, (_, i) =>
    makeModel(`alt-${i + 1}`, `Alt Model ${i + 1}B`),
  );
  const base = freshInstallResponses();
  await mockTauriIpc(page, {
    ...base,
    update_settings: null,
    get_models: [primary, ...alts],
    get_system_report: {
      ...(base.get_system_report as Record<string, unknown>),
      recommendation: {
        primaryModelId: "qwen3-4b",
        headline: "Qwen 3 4B is the best fit for your Mac.",
        reasons: ["Fits easily in your memory."],
        ranked: [
          { modelId: "qwen3-4b", fit: "perfect", why: "Fits easily in your memory." },
          ...alts.map((m) => ({ modelId: m.spec.id, fit: m.fit, why: m.why })),
        ],
      },
    },
  });

  await page.goto("/");
  await page.getByRole("button", { name: "Let's set things up" }).click();
  await page.getByLabel("Your name").fill("Ada");
  await page.getByRole("button", { name: "Continue" }).click();
  await page.getByRole("button", { name: "Show my match" }).click();
  await expect(page.getByRole("heading", { name: "Qwen 3 4B" })).toBeVisible();

  await page.getByRole("button", { name: "Pick a different one" }).click();
  // The card is swapped for the list: the recommended model is its first row,
  // tagged, and every row is the same size — nothing is pushed down.
  await expect(page.locator(".ob-alt-row")).toHaveCount(alts.length + 1);
  await expect(page.getByRole("heading", { name: "Qwen 3 4B" })).toHaveCount(0);
  await expect(page.locator(".ob-alt-row").first()).toContainText("Qwen 3 4B");
  await expect(page.locator(".ob-alt-row").first()).toContainText("Recommended");
  const heights = await page
    .locator(".ob-alt-row")
    .evaluateAll((els) => els.map((el) => Math.round(el.getBoundingClientRect().height)));
  expect(new Set(heights).size).toBe(1);

  const scene = page.locator(".ob-scene");
  const vp = page.viewportSize()!;

  // Regression guard: expanding the alternatives must never outgrow the
  // window. The recommend scene is capped at the window height and becomes the
  // scroll container, instead of overflowing .onboarding's overflow: hidden —
  // whose flex-centering used to clip the top of the scene and leave the
  // bottom of the list plus the skip controls unreachable.
  await expect(async () => {
    const state = await scene.evaluate((el) => {
      const cs = getComputedStyle(el);
      return {
        clientH: el.clientHeight,
        innerH: window.innerHeight,
        scrollable: cs.overflowY === "auto" || cs.overflowY === "scroll",
      };
    });
    expect(state.clientH).toBeLessThanOrEqual(state.innerH + 0.5);
    expect(state.scrollable).toBe(true);
  }).toPass();

  // At the top of the scroll, the scene's own first content (the eyebrow above
  // the list) is on screen — it used to be shoved up and clipped.
  const eyebrow = page.locator(".ob-eyebrow");
  await scene.evaluate((el) => el.scrollTo(0, 0));
  await expect(async () => {
    const box = await eyebrow.boundingBox();
    expect(box!.y).toBeGreaterThanOrEqual(-0.5);
    expect(box!.y + box!.height).toBeLessThanOrEqual(vp.height + 0.5);
  }).toPass();

  // Scrolled to the bottom, the whole alternatives list and the skip controls
  // are reachable — nothing is left cut off below the window.
  await scene.evaluate((el) => el.scrollTo(0, el.scrollHeight));
  for (const sel of [".ob-alts", ".ob-skips"]) {
    await expect(async () => {
      const boxes = await page
        .locator(sel)
        .evaluateAll((els) => els.map((el) => el.getBoundingClientRect()));
      expect(boxes.length).toBeGreaterThan(0);
      for (const r of boxes) {
        expect(r.top).toBeGreaterThanOrEqual(-0.5);
        expect(r.bottom).toBeLessThanOrEqual(vp.height + 0.5);
      }
    }).toPass();
  }

  // And the very last alternative is revealable through the list's own scroll.
  const lastAlt = page.locator(".ob-alt-row").last();
  await lastAlt.scrollIntoViewIfNeeded();
  await expect(async () => {
    const box = await lastAlt.boundingBox();
    expect(box!.y).toBeGreaterThanOrEqual(-0.5);
    expect(box!.y + box!.height).toBeLessThanOrEqual(vp.height + 0.5);
  }).toPass();
});
