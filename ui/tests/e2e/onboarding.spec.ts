import { expect, test, type Page } from "@playwright/test";
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

  // The look of the app comes next, and nothing in it has to be chosen.
  await expect(page.getByRole("heading", { name: "How should OpenPal look?" })).toBeVisible();
  await page.getByRole("button", { name: "Continue" }).click();

  // ...then whether to be told about models and answers, which is optional too.
  await expect(page.getByRole("heading", { name: "Want a heads-up?" })).toBeVisible();
  await page.getByRole("button", { name: "Skip this step" }).click();

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

/** The words the appearance step is reached and read by, in each language, and
 * the browser locale that starts a fresh install in it: the first-run language
 * nudge goes by the browser's locale, so a saved `language: "fr"` alone would be
 * overwritten with English. */
const WORDS = {
  en: {
    label: "English",
    locale: "en-US",
    start: "Let's set things up",
    heading: "How should OpenPal look?",
    system: "Use system default",
    systemHint: "Follows your device's light or dark mode",
    light: "Light",
    dark: "Dark",
  },
  fr: {
    label: "French",
    locale: "fr-FR",
    start: "Configurons tout ça",
    heading: "Quelle apparence souhaitez-vous pour OpenPal ?",
    system: "Réglage du système",
    systemHint: "Suit le mode clair ou sombre de votre appareil",
    light: "Clair",
    dark: "Sombre",
  },
} as const;

/** Opens the app (its mock already in place), then goes from the welcome past
 * the name step onto the appearance step. The name is typed and submitted rather
 * than skipped: the keyboard has no pointer to aim at a button that is still
 * rising into place (see `clickQuiet` in onboarding-skip.spec.ts). */
async function toAppearance(page: Page, words: (typeof WORDS)[keyof typeof WORDS] = WORDS.en) {
  await page.goto("/");
  await page.getByRole("button", { name: words.start }).click();
  await page.getByRole("textbox").fill("Ada");
  await page.keyboard.press("Enter");
  await expect(page.getByRole("heading", { name: words.heading })).toBeVisible();
}

for (const [install, hasTheme] of [
  ["a fresh install", true],
  ["an install saved before the setting existed", false],
] as const) {
  test(`the appearance step offers System, Light and Dark, with System chosen (${install})`, async ({
    page,
  }) => {
    const responses = freshInstallResponses({ update_settings: null });
    // An upgrade: settings written before the setting existed have no `theme`
    // at all, and must read as System too.
    if (!hasTheme) delete (responses.get_settings as Record<string, unknown>).theme;
    await mockTauriIpc(page, responses);

    await toAppearance(page);

    // The question names the group, and the line under it describes it.
    const group = page.getByRole("radiogroup", { name: "How should OpenPal look?" });
    await expect(group).toHaveAccessibleDescription(
      "Pick a theme. You can change it anytime in Settings.",
    );
    const options = group.getByRole("radio");
    await expect(options).toHaveCount(3);
    for (const [i, name] of ["Use system default", "Light", "Dark"].entries()) {
      await expect(options.nth(i)).toHaveAccessibleName(name);
    }
    // Nothing has to be chosen: System already is, so Continue is all it takes.
    await expect(options.nth(0)).toBeChecked();
    await expect(options.nth(1)).not.toBeChecked();
    await expect(options.nth(2)).not.toBeChecked();
    await expect(options.nth(0)).toHaveAccessibleDescription("Follows your device's light or dark mode");
    await expect(page.getByRole("button", { name: "Continue" })).toBeEnabled();
  });
}

test("the appearance step's options work from the keyboard alone", async ({ page }) => {
  await mockTauriIpc(page, freshInstallResponses({ update_settings: null }));
  await toAppearance(page);

  // Native radios, so the arrow keys move through the group and choose as they
  // go. Only a real browser can tell that nothing hides them from the keyboard.
  const group = page.getByRole("radiogroup", { name: "How should OpenPal look?" });
  const html = page.locator("html");
  await group.getByRole("radio", { name: "Use system default" }).focus();

  await page.keyboard.press("ArrowRight");
  await expect(group.getByRole("radio", { name: "Light" })).toBeFocused();
  await expect(group.getByRole("radio", { name: "Light" })).toBeChecked();
  await expect(html).toHaveAttribute("data-theme", "light");

  await page.keyboard.press("ArrowRight");
  await expect(group.getByRole("radio", { name: "Dark" })).toBeFocused();
  await expect(group.getByRole("radio", { name: "Dark" })).toBeChecked();
  await expect(html).toHaveAttribute("data-theme", "dark");
});

type Box = { x: number; y: number; width: number; height: number };

/** Whether two boxes share any area (touching edges don't count). */
const overlaps = (a: Box, b: Box) =>
  a.x + a.width > b.x + 0.5 &&
  b.x + b.width > a.x + 0.5 &&
  a.y + a.height > b.y + 0.5 &&
  b.y + b.height > a.y + 0.5;

// The window the app ships in, its minimum (crates/app/tauri.conf.json) and a phone.
const WINDOWS = [
  ["the shipped window", 1100, 740],
  ["the smallest window", 900, 620],
  ["a phone", 390, 844],
] as const;

// In French as well as English: it runs longest, and its labels are the only way
// to tell Clair from Sombre.
for (const words of Object.values(WORDS)) {
  test.describe(`the appearance step in ${words.label}`, () => {
    test.use({ locale: words.locale });

    test(`names its options ${words.system}, ${words.light} and ${words.dark}, and each does what its name says`, async ({
      page,
    }) => {
      await mockTauriIpc(page, freshInstallResponses({ update_settings: null }));
      await toAppearance(page, words);

      const group = page.getByRole("radiogroup", { name: words.heading });
      const option = (name: string) => group.getByRole("radio", { name, exact: true });
      const html = page.locator("html");
      await expect(option(words.system)).toBeChecked();
      await expect(option(words.system)).toHaveAccessibleDescription(words.systemHint);

      await option(words.dark).check();
      await expect(html).toHaveAttribute("data-theme", "dark");
      await option(words.light).check();
      await expect(html).toHaveAttribute("data-theme", "light");
      // System pins nothing: the attribute is gone, and the device decides again.
      await option(words.system).check();
      await expect(html).not.toHaveAttribute("data-theme", /.*/);
    });

    for (const [size, width, height] of WINDOWS) {
      test(`fits ${size}: every option and the way on stay in view`, async ({ page }) => {
        await page.setViewportSize({ width, height });
        await mockTauriIpc(page, freshInstallResponses({ update_settings: null }));
        await toAppearance(page, words);

        // The radios only overlay the options; the faces are what is drawn. The
        // way on is Continue and the two skips.
        const options = await page.locator(".theme-option-face").all();
        const wayOn = await page.locator(".ob-appearance-actions").getByRole("button").all();
        expect(options).toHaveLength(3);
        expect(wayOn).toHaveLength(3);
        await expect(async () => {
          const boxes: Box[] = [];
          for (const el of [...options, ...wayOn]) boxes.push((await el.boundingBox())!);
          for (const box of boxes) {
            expect(box.x).toBeGreaterThanOrEqual(-0.5);
            expect(box.x + box.width).toBeLessThanOrEqual(width + 0.5);
            expect(box.y).toBeGreaterThanOrEqual(-0.5);
            expect(box.y + box.height).toBeLessThanOrEqual(height + 0.5);
          }
          // A long label or hint must not push one option over its neighbour.
          const [system, light, dark] = boxes;
          expect(overlaps(system, light), "the first two options overlap").toBe(false);
          expect(overlaps(light, dark), "the last two options overlap").toBe(false);
          expect(overlaps(system, dark), "the first and last options overlap").toBe(false);
        }).toPass();
        // No label or hint is wider than its card, and the page never scrolls sideways.
        const spilled = await page.$$eval(".theme-option-face", (faces) =>
          faces.some((face) => face.scrollWidth > face.clientWidth + 0.5),
        );
        expect(spilled).toBe(false);
        const overflow = await page.evaluate(
          () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
        );
        expect(overflow).toBeLessThanOrEqual(0);
      });
    }
  });
}

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
  await expect(page.getByRole("heading", { name: "How should OpenPal look?" })).toBeVisible();
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(page.getByRole("heading", { name: "Want a heads-up?" })).toBeVisible();
  await page.getByRole("button", { name: "Skip this step" }).click();
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
