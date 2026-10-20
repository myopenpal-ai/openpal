import { readFileSync } from "node:fs";
import { expect, test, type Locator, type Page } from "@playwright/test";
import { freshInstallResponses } from "./support/fixtures";
import { persistSettings } from "./support/persistedSettings";
import { mockTauriIpc } from "./support/tauriMock";

/**
 * The Theme setting in a real browser. jsdom computes no CSS, so the unit tests
 * can read `<html data-theme>` and the stylesheet's text but never see the
 * cascade pick a palette; this is the tier that does. It drives Chromium (and
 * WebKit locally, the stand-in for WKWebView) against the Vite dev server with
 * the Tauri IPC mocked, so what it cannot see is a real window: the OS-drawn
 * frame, how a webview paints its own select popups, scrollbars and
 * placeholders, a real OS appearance change reaching WKWebView or WebView2, and
 * the production bundle's render-blocking stylesheet. The `color-scheme`
 * assertions are the proxy for "native controls stay legible": they prove the
 * pinned scheme reaches the controls, not how an engine then draws them.
 */

type Scheme = "light" | "dark";
type Theme = "system" | Scheme;
type Language = "en" | "fr";

// The palettes are read out of global.css rather than copied here, as
// contrast.test.ts does: this spec proves WHICH palette is in force, so
// re-tuning a colour must not need an edit to it. The plain :root block is the
// light palette; the dark one is read from the pinned-Dark block (the media
// query repeats the same tokens, which contrast.test.ts keeps identical).
const globalCss = readFileSync(
  new URL("../../src/styles/global.css", import.meta.url),
  "utf-8",
).replace(/\/\*[\s\S]*?\*\//g, "");

function paletteFrom(block: RegExp) {
  const body = globalCss.match(block)?.[1];
  if (!body) throw new Error(`global.css has no block matching ${block}`);
  const colour = (token: string) => {
    const hex = body.match(new RegExp(`(?<![\\w-])--${token}:\\s*#([0-9a-fA-F]{6})\\s*;`))?.[1];
    if (!hex) throw new Error(`--${token} is not a plain hex colour in ${block}`);
    const [r, g, b] = [0, 2, 4].map((at) => parseInt(hex.slice(at, at + 2), 16));
    return { hex: `#${hex.toLowerCase()}`, rgb: `rgb(${r}, ${g}, ${b})` };
  };
  return { paper: colour("paper"), raised: colour("paper-raised"), ink: colour("ink") };
}

const PALETTE = {
  light: paletteFrom(/^:root\s*\{([^}]*)\}/m),
  dark: paletteFrom(/:root\[data-theme="dark"\]\s*\{([^}]*)\}/),
};

/** The words the bilingual test looks for; the English tests spell theirs out. */
const WORDS = {
  en: { more: "More", settings: "Settings", appearance: "Appearance", theme: "Theme" },
  fr: { more: "Plus", settings: "Réglages", appearance: "Apparence", theme: "Thème" },
} as const;

type Invoke = { cmd: string; args?: Record<string, unknown> };

/** What the page saw the first time a screen of the app appeared in the DOM. */
type FirstFrame = {
  scene: "shell" | "onboarding";
  theme: string | null;
  background: string;
};

interface BootOptions {
  /** The OS appearance, emulated before the first navigation. */
  os?: Scheme;
  /** Saved settings, over a fresh install's. `{ theme: undefined }` leaves the
   * key out altogether, as a row saved before the setting existed. */
  settings?: Record<string, unknown>;
  /** Canned command responses, over the fixture's. */
  responses?: Record<string, unknown>;
  /** A fresh install (onboarding) rather than an onboarded one (the shell). */
  fresh?: boolean;
  /** Let `page.reload()` act as a relaunch: saved settings are read back. */
  persist?: boolean;
  /** Commands that are sent and never answered. */
  hang?: string[];
}

/** Everything up to the first navigation: the OS appearance, the mocked
 * backend, and two recorders — the commands the page sends, and what
 * `<html>` looks like the first time the shell or onboarding is in the DOM. */
async function boot(
  page: Page,
  {
    os = "light",
    settings = {},
    responses = {},
    fresh = false,
    persist = false,
    hang = [],
  }: BootOptions = {},
) {
  // Before the page loads, so the app's very first paint already sees it.
  await page.emulateMedia({ colorScheme: os });
  const base = freshInstallResponses();
  await mockTauriIpc(page, {
    ...base,
    get_settings: {
      ...(base.get_settings as Record<string, unknown>),
      onboardingCompleted: !fresh,
      userName: fresh ? null : "Ada",
      ...settings,
    },
    update_settings: null,
    // The sidebar asks for the app version; unanswered, every page load logs an
    // unhandled rejection from the dev server.
    "plugin:app|version": "0.1.0",
    ...responses,
  });
  if (persist) await persistSettings(page);
  // Added after the mock's own init script, so it wraps that `invoke` (the way
  // onboarding-skip.spec.ts records what a fresh install sends).
  await page.addInitScript((hung: string[]) => {
    const w = window as unknown as {
      __invokes: Invoke[];
      __TAURI_INTERNALS__: {
        invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
      };
    };
    const internals = w.__TAURI_INTERNALS__;
    const answered = internals.invoke;
    w.__invokes = [];
    internals.invoke = (cmd, args) => {
      w.__invokes.push({ cmd, args });
      return hung.includes(cmd) ? new Promise(() => {}) : answered(cmd, args);
    };
  }, hang);
  // A MutationObserver's callback runs as a microtask right after the DOM change
  // that triggered it, before the browser can paint. So what it reads is what
  // the first frame of that screen is drawn with — and an attribute that only
  // lands later (a passive effect, a second state update) is still missing.
  await page.addInitScript(() => {
    const w = window as unknown as { __firstFrame: FirstFrame | null };
    w.__firstFrame = null;
    const watcher = new MutationObserver(() => {
      const screen = document.querySelector(".shell, .onboarding");
      if (!screen) return;
      w.__firstFrame = {
        scene: screen.classList.contains("shell") ? "shell" : "onboarding",
        theme: document.documentElement.getAttribute("data-theme"),
        background: getComputedStyle(document.body).backgroundColor,
      };
      watcher.disconnect();
    });
    watcher.observe(document, { childList: true, subtree: true });
  });
  await page.goto("/");
}

const invokes = (page: Page) =>
  page.evaluate(() => (window as unknown as { __invokes: Invoke[] }).__invokes);

const firstFrame = (page: Page) =>
  page.evaluate(() => (window as unknown as { __firstFrame: FirstFrame | null }).__firstFrame);

/** The theme in the last `update_settings` the page sent, i.e. what the
 * backend would hold. */
async function savedTheme(page: Page) {
  const saves = (await invokes(page)).filter((call) => call.cmd === "update_settings");
  return (saves.at(-1)?.args?.settings as { theme?: string } | undefined)?.theme;
}

const moreButton = (page: Page, language: Language = "en") =>
  page.getByRole("button", { name: WORDS[language].more, exact: true });

/** The app shell of an onboarded install; the sidebar's More menu is its mark. */
async function openShell(page: Page, options: BootOptions & { language?: Language } = {}) {
  const { language = "en", ...rest } = options;
  await boot(page, { ...rest, settings: { language, ...rest.settings } });
  await expect(moreButton(page, language)).toBeVisible();
}

/** Opens Settings from the sidebar's More menu; it lands on General. */
async function openSettings(page: Page, language: Language = "en") {
  const words = WORDS[language];
  await moreButton(page, language).click();
  await page.getByRole("button", { name: words.settings, exact: true }).click();
  const dialog = page.getByRole("dialog", { name: words.settings });
  await expect(dialog).toBeVisible();
  return dialog;
}

/** Settings → General's Theme radiogroup, and one of its options. */
const themeGroup = (page: Page, language: Language = "en") =>
  page.getByRole("dialog").getByRole("radiogroup", { name: WORDS[language].theme });
const themeOption = (page: Page, name: string) =>
  themeGroup(page).getByRole("radio", { name, exact: true });

/** Exactly one option is selected, and it is `picked`. */
async function expectPicked(page: Page, picked: "Use system default" | "Light" | "Dark") {
  for (const name of ["Use system default", "Light", "Dark"]) {
    if (name === picked) await expect(themeOption(page, name)).toBeChecked();
    else await expect(themeOption(page, name)).not.toBeChecked();
  }
}

/** Clicks an option and reports what <html> and the body are at when the next
 * frame is about to be drawn. The click and the frame request happen in one
 * task, so no frame can come between them: what comes back is what the user
 * sees first after choosing. */
function pickAndSeeNextFrame(option: Locator) {
  return option.evaluate(
    (input: HTMLInputElement) =>
      new Promise<{ theme: string | null; background: string }>((resolve) => {
        input.click();
        requestAnimationFrame(() =>
          resolve({
            theme: document.documentElement.getAttribute("data-theme"),
            background: getComputedStyle(document.body).backgroundColor,
          }),
        );
      }),
  );
}

/** Which palette is in force: the root's `--paper` token, and the colour the
 * body actually paints. */
async function expectPalette(page: Page, scheme: Scheme) {
  const { paper } = PALETTE[scheme];
  await expect
    .poll(() =>
      page.evaluate(() =>
        getComputedStyle(document.documentElement).getPropertyValue("--paper").trim().toLowerCase(),
      ),
    )
    .toBe(paper.hex);
  await expect(page.locator("body")).toHaveCSS("background-color", paper.rgb);
}

/** What the app wrote on <html>: System writes nothing. */
async function expectDataTheme(page: Page, theme: Theme) {
  const html = page.locator("html");
  if (theme === "system") await expect(html).not.toHaveAttribute("data-theme");
  else await expect(html).toHaveAttribute("data-theme", theme);
}

/** Flips the emulated OS appearance and returns once the page has seen it and
 * drawn a frame. That is what lets a palette that must NOT change be asserted
 * without waiting on a timer. */
async function flipOs(page: Page, os: Scheme) {
  await page.emulateMedia({ colorScheme: os });
  await expect
    .poll(() => page.evaluate(() => matchMedia("(prefers-color-scheme: dark)").matches))
    .toBe(os === "dark");
  await page.evaluate(
    () =>
      new Promise<void>((done) => requestAnimationFrame(() => requestAnimationFrame(() => done()))),
  );
}

// ---------------------------------------------------------------------------
// 1. Which palette each (OS, theme) pair paints
// ---------------------------------------------------------------------------

// The truth table of global.css, written out rather than computed so a bug in
// the rule can't hide in the same logic here. Only a pinned theme pins
// color-scheme; System leaves it to <meta name="color-scheme">, as before the
// setting existed.
const MATRIX: { os: Scheme; theme: Theme; paints: Scheme; title: string }[] = [
  {
    os: "light",
    theme: "system",
    paints: "light",
    title: "System on a light OS paints the light palette and writes no data-theme",
  },
  {
    os: "dark",
    theme: "system",
    paints: "dark",
    title: "System on a dark OS paints the dark palette and writes no data-theme",
  },
  {
    os: "light",
    theme: "light",
    paints: "light",
    title: "Light on a light OS paints the light palette with color-scheme pinned to light",
  },
  {
    os: "dark",
    theme: "light",
    paints: "light",
    title: "Light on a dark OS still paints the light palette, with color-scheme pinned to light",
  },
  {
    os: "light",
    theme: "dark",
    paints: "dark",
    title: "Dark on a light OS still paints the dark palette, with color-scheme pinned to dark",
  },
  {
    os: "dark",
    theme: "dark",
    paints: "dark",
    title: "Dark on a dark OS paints the dark palette with color-scheme pinned to dark",
  },
];

for (const { os, theme, paints, title } of MATRIX) {
  test(title, async ({ page }) => {
    await openShell(page, { os, settings: { theme } });

    await expectDataTheme(page, theme);
    await expectPalette(page, paints);
    // The scheme UA-drawn controls inherit is the theme's, not the OS's. How an
    // engine then paints a select popup or a scrollbar needs a real window.
    const html = page.locator("html");
    if (theme === "system") await expect(html).not.toHaveCSS("color-scheme", /^(light|dark)$/);
    else await expect(html).toHaveCSS("color-scheme", theme);
  });
}

test("a settings row saved before the Theme existed reads as System: it follows the OS and the control says so", async ({
  page,
}) => {
  await openShell(page, { os: "dark", settings: { theme: undefined } });
  // The key really is missing from what the backend answers, not just null.
  const answered = await page.evaluate(() => {
    const w = window as unknown as {
      __TAURI_INTERNALS__: { invoke: (cmd: string) => Promise<object> };
    };
    return w.__TAURI_INTERNALS__.invoke("get_settings");
  });
  expect("theme" in answered).toBe(false);

  await expectDataTheme(page, "system");
  await expectPalette(page, "dark");
  await openSettings(page);
  await expectPicked(page, "Use system default");
});

// ---------------------------------------------------------------------------
// 2. The OS changing while the app is open
// ---------------------------------------------------------------------------

test("System follows the OS when it flips while the app is open, with no reload", async ({
  page,
}) => {
  await openShell(page, { os: "light", settings: { theme: "system" } });
  await expectPalette(page, "light");
  // The same document throughout: a reload would pass every check below too.
  const loadedAt = await page.evaluate(() => performance.timeOrigin);

  await flipOs(page, "dark");
  await expectPalette(page, "dark");
  await expectDataTheme(page, "system"); // the engine did it, not a script

  await flipOs(page, "light");
  await expectPalette(page, "light");
  expect(await page.evaluate(() => performance.timeOrigin)).toBe(loadedAt);
});

// The two tests below assert that nothing changes, which a check made too soon
// would pass for the wrong reason. flipOs waits for the page to have seen the
// flip and drawn a frame, and the test above is the positive control: the same
// flip does repaint a System page.
test("a pinned Light ignores the OS switching to dark while the app is open", async ({ page }) => {
  await openShell(page, { os: "light", settings: { theme: "light" } });
  await expectPalette(page, "light");

  await flipOs(page, "dark");
  await expectPalette(page, "light");
  await expectDataTheme(page, "light");
});

test("a pinned Dark ignores the OS switching to light while the app is open", async ({ page }) => {
  await openShell(page, { os: "dark", settings: { theme: "dark" } });
  await expectPalette(page, "dark");

  await flipOs(page, "light");
  await expectPalette(page, "dark");
  await expectDataTheme(page, "dark");
});

// ---------------------------------------------------------------------------
// 3. The control in Settings → General
// ---------------------------------------------------------------------------

test("Settings → General has an Appearance group whose Theme control offers System, Light and Dark, with System selected", async ({
  page,
}) => {
  await openShell(page);
  const dialog = await openSettings(page);

  await expect(dialog.getByRole("heading", { level: 1, name: "General" })).toBeVisible();
  const appearance = dialog.getByRole("region", { name: "Appearance" });
  await expect(appearance.getByRole("heading", { level: 2, name: "Appearance" })).toBeVisible();
  const theme = appearance.getByRole("radiogroup", { name: "Theme" });
  await expect(theme).toHaveAccessibleDescription(
    "Choose \"Use system default\" to follow your device's light or dark mode.",
  );
  // What a screen reader announces: the group's name, each option's name, and
  // which one is selected.
  await expect(theme).toMatchAriaSnapshot(`
    - radiogroup "Theme":
      - radio "Use system default" [checked]
      - radio "Light"
      - radio "Dark"
  `);
});

test("the picker's own styling is in force: the radios are invisible overlays and the checked option wears the dot", async ({
  page,
  browserName,
}) => {
  await openShell(page);
  await openSettings(page);

  const input = themeOption(page, "Use system default");
  await expect(input).toHaveCSS("opacity", "0");
  await expect(input).toHaveCSS("position", "absolute");
  const dot = (name: string) =>
    themeGroup(page).locator("label", { hasText: name }).locator(".theme-option-mark");
  await expect(dot("Use system default")).toHaveCSS("background-image", /radial-gradient/);
  await expect(dot("Light")).toHaveCSS("background-image", "none");
  // Keyboard focus draws a ring on the visible face, not on the invisible input.
  // WebKit doesn't match :focus-visible on a radio reached this way (it skips
  // radios in the Tab order too), so the ring is a Chromium-only assertion.
  if (browserName !== "chromium") return;
  await themeOption(page, "Use system default").focus();
  await page.keyboard.press("ArrowRight");
  const face = themeGroup(page).locator("label", { hasText: "Light" }).locator(".theme-option-face");
  await expect(face).not.toHaveCSS("box-shadow", "none");
});

for (const saved of ["Light", "Dark"] as const) {
  test(`the Theme control shows ${saved} when ${saved} is the saved choice`, async ({ page }) => {
    await openShell(page, { settings: { theme: saved.toLowerCase() } });
    await openSettings(page);

    await expectPicked(page, saved);
  });
}

test("choosing a theme in Settings re-themes the open app at once with no Save, and System hands it back to the OS", async ({
  page,
}) => {
  await openShell(page, { os: "light" });
  await openSettings(page);

  // At once: the first frame drawn after the click is already dark, with nothing
  // to confirm or save first.
  expect(await pickAndSeeNextFrame(themeOption(page, "Dark"))).toEqual({
    theme: "dark",
    background: PALETTE.dark.paper.rgb,
  });
  await expectDataTheme(page, "dark");
  await expectPalette(page, "dark");
  await expectPicked(page, "Dark");
  await expect.poll(() => savedTheme(page)).toBe("dark");

  // System removes the attribute again, and the OS (light here) decides.
  await themeOption(page, "Use system default").click();
  await expectDataTheme(page, "system");
  await expectPalette(page, "light");
  await expectPicked(page, "Use system default");
  await expect.poll(() => savedTheme(page)).toBe("system");

  await themeOption(page, "Light").click();
  await expectDataTheme(page, "light");
  await expectPicked(page, "Light");
  await expect.poll(() => savedTheme(page)).toBe("light");

  // The options follow the store, not state of their own: Settings closed and
  // reopened shows the same pick.
  await themeOption(page, "Dark").click();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expectPalette(page, "dark");
  await openSettings(page);
  await expectPicked(page, "Dark");
});

test("a pick re-themes the app before its save has finished: the save never has to settle", async ({
  page,
}) => {
  await openShell(page, { os: "light", hang: ["update_settings"] });
  await openSettings(page);

  await themeOption(page, "Dark").click();

  await expectDataTheme(page, "dark");
  await expectPalette(page, "dark");
  await expectPicked(page, "Dark");
  // The write did go out, as the whole settings object, and is still pending.
  const saves = (await invokes(page)).filter((call) => call.cmd === "update_settings");
  expect(saves.at(-1)?.args?.settings).toMatchObject({
    theme: "dark",
    userName: "Ada",
    onboardingCompleted: true,
  });
});

test("the Theme control works from the keyboard: one Tab stop, and the arrow keys change the theme", async ({
  page,
  browserName,
}) => {
  // A dark OS, so every step below changes the palette: dark, light, dark, light, dark.
  await openShell(page, { os: "dark" });
  await openSettings(page);
  const system = themeOption(page, "Use system default");
  const light = themeOption(page, "Light");
  const dark = themeOption(page, "Dark");

  if (browserName === "webkit") {
    // WebKit on macOS leaves buttons, checkboxes and radios out of the Tab order
    // unless the OS's full keyboard access is on, so Tab never arrives. Focus is
    // placed directly; what follows is the platform's own radio-group handling.
    await system.focus();
  } else {
    // A bounded walk: how many controls precede the group isn't under test.
    for (let stops = 0; stops < 40; stops++) {
      if (await system.evaluate((el) => el === document.activeElement)) break;
      await page.keyboard.press("Tab");
    }
  }
  await expect(system).toBeFocused();
  await expectPicked(page, "Use system default");

  await page.keyboard.press("ArrowRight");
  await expect(light).toBeFocused();
  await expectPicked(page, "Light");
  await expectDataTheme(page, "light");
  await expectPalette(page, "light");

  await page.keyboard.press("ArrowRight");
  await expect(dark).toBeFocused();
  await expectPicked(page, "Dark");
  await expectDataTheme(page, "dark");
  await expectPalette(page, "dark");

  await page.keyboard.press("ArrowLeft");
  await expectPicked(page, "Light");
  await expectPalette(page, "light");

  await page.keyboard.press("ArrowLeft");
  await expectPicked(page, "Use system default");
  await expectDataTheme(page, "system");
  await expectPalette(page, "dark");

  if (browserName !== "webkit") {
    // The whole group is one Tab stop, on the selected option.
    await page.keyboard.press("Tab");
    const stillInGroup = await page.evaluate(
      () => !!document.activeElement?.closest('[role="radiogroup"]'),
    );
    expect(stillInGroup).toBe(false);
    await page.keyboard.press("Shift+Tab");
    await expect(system).toBeFocused();
  }
});

// ---------------------------------------------------------------------------
// 4. Every surface re-themes
// ---------------------------------------------------------------------------

// A theme that differs from the OS's, in both directions.
const AGAINST_THE_OS: { os: Scheme; theme: Scheme }[] = [
  { os: "light", theme: "dark" },
  { os: "dark", theme: "light" },
];

for (const { os, theme } of AGAINST_THE_OS) {
  test(`${theme === "dark" ? "Dark" : "Light"} on a ${os} OS paints the shell, the More dialog and a dialog portalled out of the app root alike`, async ({
    page,
  }) => {
    await openShell(page, { os, settings: { theme } });
    const palette = PALETTE[theme];

    // The shell behind everything: the page and its greeting.
    await expect(page.locator("body")).toHaveCSS("background-color", palette.paper.rgb);
    await expect(page.getByRole("heading", { name: /Hi, Ada/ })).toHaveCSS(
      "color",
      palette.ink.rgb,
    );

    await moreButton(page).click();
    await page.getByRole("button", { name: "Skills", exact: true }).click();
    await expect(page.locator(".more-dialog")).toHaveCSS("background-color", palette.paper.rgb);

    // The skill editor renders into <body>, beside #root rather than inside it,
    // so only a theme set on <html> reaches it.
    await page.getByRole("button", { name: "New skill" }).click();
    const editor = page.getByRole("dialog", { name: "New skill" });
    await expect(editor).toBeVisible();
    const outsideRoot = await editor.evaluate(
      (el) => !document.getElementById("root")!.contains(el),
    );
    expect(outsideRoot, "the skill editor is rendered inside #root").toBe(true);
    await expect(editor).toHaveCSS("background-color", palette.raised.rgb);
    await expect(editor.getByRole("heading", { name: "New skill" })).toHaveCSS(
      "color",
      palette.ink.rgb,
    );
  });
}

for (const { os, theme } of AGAINST_THE_OS) {
  test(`native controls take the ${theme} theme's color-scheme on a ${os} OS, in Settings and in a portalled dialog`, async ({
    page,
  }) => {
    await openShell(page, { os, settings: { theme } });
    const dialog = await openSettings(page);

    // A select, a checkbox and a text field: the three kinds of control whose
    // pixels the browser draws itself.
    await expect(dialog.getByRole("combobox")).toHaveCSS("color-scheme", theme);
    await expect(dialog.getByRole("checkbox").first()).toHaveCSS("color-scheme", theme);
    await expect(dialog.getByRole("textbox", { name: "Search settings" })).toHaveCSS(
      "color-scheme",
      theme,
    );

    await dialog
      .getByRole("navigation", { name: "More" })
      .getByRole("button", { name: "Skills" })
      .click();
    await page.getByRole("button", { name: "New skill" }).click();
    const editor = page.getByRole("dialog", { name: "New skill" });
    await expect(editor.getByRole("textbox").first()).toHaveCSS("color-scheme", theme);
  });
}

// The Translate screen's two language pickers are the one place the app sets no
// colours on a native select, so the browser picks its background and text from
// the used color-scheme. A theme that differs from the OS's would show there
// first: a dark box on a light page, or the reverse. Writing Tools only appears
// with a plugin that advertises the capability.
const WRITING_TOOLS_PLUGIN = {
  id: "writing-tools",
  name: "Writing tools",
  description: "",
  version: "1.0.0",
  author: "",
  capabilities: ["writing-tools"],
  enabled: true,
  setup: { type: "ready" },
  service: { type: "notApplicable" },
  hasChat: false,
  experimental: false,
};

/** How light a CSS `rgb()` or `rgba()` colour is, from 0 (black) to 1 (white). */
function lightness(colour: string) {
  const [r, g, b] = colour.match(/[\d.]+/g)!.slice(0, 3).map(Number);
  return (r + g + b) / (3 * 255);
}

for (const { os, theme } of AGAINST_THE_OS) {
  test(`${theme === "dark" ? "Dark" : "Light"} on a ${os} OS draws the Translate screen's un-styled select ${theme} too, in step with the page`, async ({
    page,
  }) => {
    await openShell(page, {
      os,
      settings: { theme },
      responses: { list_plugins: [WRITING_TOOLS_PLUGIN] },
    });
    await page.getByRole("button", { name: "Translate" }).click();
    const palette = PALETTE[theme];

    // The page's own text box is painted from the tokens...
    const source = page.getByRole("textbox", { name: "Your text" });
    await expect(source).toHaveCSS("background-color", palette.raised.rgb);
    await expect(source).toHaveCSS("color", palette.ink.rgb);

    // ...and the browser-drawn select agrees with it, background and text both.
    const select = page.getByRole("combobox", { name: "Translate from" });
    await expect(select).toBeVisible();
    const { background, text } = await select.evaluate((el) => {
      const style = getComputedStyle(el);
      return { background: style.backgroundColor, text: style.color };
    });
    const dark = theme === "dark";
    expect(lightness(background) < 0.5, `select background ${background}`).toBe(dark);
    expect(lightness(text) > 0.5, `select text ${text}`).toBe(dark);
  });
}

// ---------------------------------------------------------------------------
// 5. Relaunch, and the first frame
// ---------------------------------------------------------------------------

test("a picked theme survives a relaunch: the app wears it and Settings shows it selected", async ({
  page,
}) => {
  await openShell(page, { os: "light", persist: true });
  await openSettings(page);
  await themeOption(page, "Dark").click();
  await expect.poll(() => savedTheme(page)).toBe("dark");

  await page.reload();
  await expect(moreButton(page)).toBeVisible();

  await expectDataTheme(page, "dark");
  await expectPalette(page, "dark");
  await openSettings(page);
  await expectPicked(page, "Dark");
});

test("after a relaunch the saved theme is already on <html> the first time the shell is in the DOM", async ({
  page,
}) => {
  await openShell(page, { os: "light", persist: true });
  await openSettings(page);
  await themeOption(page, "Dark").click();
  await expect.poll(() => savedTheme(page)).toBe("dark");

  await page.reload();
  await expect(moreButton(page)).toBeVisible();

  // The OS (light) is never what the shell is first drawn with.
  expect(await firstFrame(page)).toEqual({
    scene: "shell",
    theme: "dark",
    background: PALETTE.dark.paper.rgb,
  });
});

test("after a relaunch partway through onboarding, the saved theme is already on <html> the first time onboarding is in the DOM", async ({
  page,
}) => {
  await boot(page, { os: "light", fresh: true, persist: true });
  await page.getByRole("button", { name: "Let's set things up" }).click();
  await page.getByRole("button", { name: "Skip this step" }).click(); // the name step
  const looks = page.getByRole("radiogroup", { name: "How should OpenPal look?" });
  await looks.getByRole("radio", { name: "Dark", exact: true }).check();
  // The step itself re-themes at once.
  await expectDataTheme(page, "dark");
  await expectPalette(page, "dark");
  await expect.poll(() => savedTheme(page)).toBe("dark");

  await page.reload();
  await expect(page.getByRole("button", { name: "Let's set things up" })).toBeVisible();

  expect(await firstFrame(page)).toEqual({
    scene: "onboarding",
    theme: "dark",
    background: PALETTE.dark.paper.rgb,
  });
});

// The backend builds its store, and so can read settings, only after the lock is
// opened (see boot() in state/store.ts), so these frames can't know a saved
// theme. They follow the OS, which is the default the ticket proposes.
for (const { os, theme } of AGAINST_THE_OS) {
  test(`the lock screen follows a ${os} OS even though ${theme} is the saved theme, since settings can't be read before unlock`, async ({
    page,
  }) => {
    const base = freshInstallResponses();
    await boot(page, {
      os,
      settings: { theme },
      responses: {
        get_app_info: { ...(base.get_app_info as Record<string, unknown>), needsAppUnlock: true },
      },
    });

    const pad = page.getByRole("dialog", { name: "Enter your PIN" });
    await expect(pad).toBeVisible();
    await expectDataTheme(page, "system");
    await expectPalette(page, os);
    await expect(pad).toHaveCSS("background-color", PALETTE[os].paper.rgb);
    // Why: nothing has asked for the settings yet.
    expect((await invokes(page)).map((call) => call.cmd)).not.toContain("get_settings");
  });
}

// ---------------------------------------------------------------------------
// 6. A phone-width screen
// ---------------------------------------------------------------------------

const PHONES: { language: Language; locale: string; label: string }[] = [
  { language: "en", locale: "en-US", label: "English" },
  { language: "fr", locale: "fr-FR", label: "French" },
];

for (const { language, locale, label } of PHONES) {
  test.describe(`at phone width in ${label}`, () => {
    test.use({ locale });

    test("the Appearance group fits the screen without scrolling sideways", async ({ page }) => {
      const words = WORDS[language];
      await openShell(page, { language });
      // The More menu lives in the sidebar, a drawer below the mobile breakpoint:
      // open the dialog first, then narrow the window under it.
      const dialog = await openSettings(page, language);
      await page.setViewportSize({ width: 390, height: 844 });

      const appearance = dialog.getByRole("region", { name: words.appearance });
      await appearance.scrollIntoViewIfNeeded();
      await expect(appearance).toBeInViewport();
      await expect(themeGroup(page, language)).toBeVisible();

      const fit = await appearance.evaluate((section) => {
        const frame = section.getBoundingClientRect();
        const stray = [...section.querySelectorAll<HTMLElement>("*")]
          .filter((el) => {
            const box = el.getBoundingClientRect();
            return (
              box.width > 0 &&
              (box.left < frame.left - 0.5 ||
                box.right > frame.right + 0.5 ||
                el.scrollWidth > el.clientWidth + 0.5)
            );
          })
          .map((el) => el.className || el.tagName);
        return {
          stray,
          right: frame.right,
          page: document.documentElement.scrollWidth - document.documentElement.clientWidth,
        };
      });
      // Nothing inside the group pokes out of it or is clipped by its own box...
      expect(fit.stray, "elements wider than the group").toEqual([]);
      // ...the group sits inside the screen, and the page doesn't scroll sideways.
      expect(fit.right).toBeLessThanOrEqual(390);
      expect(fit.page).toBeLessThanOrEqual(0);

      // What was measured is the language's own copy, not English under a French
      // locale: the longer French labels are the ones most likely to overflow.
      await expect(themeGroup(page, language)).toMatchAriaSnapshot(
        language === "fr"
          ? `
            - radiogroup "Thème":
              - radio "Réglage du système" [checked]
              - radio "Clair"
              - radio "Sombre"
          `
          : `
            - radiogroup "Theme":
              - radio "Use system default" [checked]
              - radio "Light"
              - radio "Dark"
          `,
      );
    });
  });
}
