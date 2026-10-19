import { expect, test, type Locator, type Page } from "@playwright/test";
import { mockTauriIpc } from "./support/tauriMock";
import { freshInstallResponses } from "./support/fixtures";
import { persistSettings } from "./support/persistedSettings";

/** The app shell after onboarding finishes early: the empty chat composer
 * (rendered unconditionally by ChatView, with no model or conversation). */
const shell = (page: Page) => page.getByRole("textbox", { name: "Message" });

/** Skipping the whole setup asks first, since it leaves the app with no model. */
const skipDialog = (page: Page) => page.getByRole("alertdialog", { name: "Skip setup?" });
const confirmSkip = (page: Page) =>
  skipDialog(page).getByRole("button", { name: "Skip setup" }).click();

/** Clicks one of the quiet buttons a scene offers for leaving it, once the
 * entrance animations (`.ob-appear`, each starting after its own short delay) on
 * it and around it have finished. Playwright's check that a control is stable
 * passes while it is still waiting out that delay, so the click is aimed where the
 * button is about to leave: it rises 18px, more than half a quiet button's height,
 * and a click that arrives after that lands just below it. WebKit under load is
 * slow enough to do so. */
async function clickQuiet(button: Locator) {
  await button.evaluate((el) =>
    Promise.all(
      document
        .getAnimations()
        .filter((animation) => {
          const effect = animation.effect as KeyframeEffect | null;
          return effect?.target?.contains(el) && effect.getTiming().iterations !== Infinity;
        })
        .map((animation) => animation.finished.catch(() => undefined)),
    ),
  );
  await button.click();
}
const skipStep = (page: Page) => clickQuiet(page.getByRole("button", { name: "Skip this step" }));
const skipSetup = (page: Page) => clickQuiet(page.getByRole("button", { name: "Skip setup" }));

const appearanceHeading = (page: Page) =>
  page.getByRole("heading", { name: "How should OpenPal look?" });
const notificationsHeading = (page: Page) =>
  page.getByRole("heading", { name: "Want a heads-up?" });
const scanHeading = (page: Page) =>
  page.getByRole("heading", { name: "First, let's see what your Mac can do." });
/** One of the appearance step's three options. */
const themeOption = (page: Page, name: "System" | "Light" | "Dark") =>
  page.getByRole("radio", { name });

/** What the two palettes paint, as computed colours: `--paper` (the page),
 * `--paper-raised` (a dialog) and `--ink` (text) in global.css. */
const LIGHT = { paper: "rgb(247, 247, 248)", ink: "rgb(26, 28, 32)" };
const DARK = { paper: "rgb(16, 14, 12)", raised: "rgb(31, 27, 22)", ink: "rgb(237, 228, 216)" };

/** A bare model so the recommend scene has something to show (not installed,
 * so its button starts a download). Mirrors the models-page spec fixture. */
const MODEL = {
  spec: {
    id: "qwen3-4b",
    name: "Qwen 3 4B",
    family: "Qwen",
    description: "A recommendation test model.",
    strengths: [],
    url: "https://example.com/qwen3-4b.gguf",
    fileName: "qwen3-4b.gguf",
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
  fit: "perfect",
  why: "Fits easily in your memory.",
  isRecommended: true,
  favorite: false,
};

type Invoke = { cmd: string; args?: Record<string, unknown> };

/** Boots a fresh install that recommends `MODEL`. Every command the page sends
 * is recorded (see `invokes`), and `download_model` never settles — a download
 * genuinely in flight — since the plain request/response mock can't stream.
 * With `persist` the mocked backend also remembers what `update_settings` was
 * sent, so a reload is a relaunch (see `persistSettings`). `responses` replaces
 * the canned answer of any command, for a spec that needs the OS to say something
 * else (the notification permission, say). */
async function bootFreshInstall(
  page: Page,
  { persist = false, responses = {} }: { persist?: boolean; responses?: Record<string, unknown> } = {},
) {
  const base = freshInstallResponses();
  await mockTauriIpc(page, {
    ...base,
    update_settings: null,
    cancel_download: null,
    "plugin:app|version": "0.1.0",
    get_models: [MODEL],
    get_system_report: {
      ...(base.get_system_report as Record<string, unknown>),
      recommendation: {
        primaryModelId: "qwen3-4b",
        headline: "Qwen 3 4B is the best fit for your Mac.",
        reasons: ["Fits easily in your memory."],
        ranked: [{ modelId: "qwen3-4b", fit: "perfect", why: "Fits easily in your memory." }],
      },
    },
    ...responses,
  });
  // Before the recorder below, so the recorder is the outer wrapper and still
  // sees the `update_settings` calls this one answers itself.
  if (persist) await persistSettings(page);
  // Added after the mock's own init script, so it wraps that `invoke`.
  // `transformCallback` is the callback-id shim the Channel `startDownload`
  // opens needs (see the chat-empty spec); without it Download throws.
  await page.addInitScript(() => {
    const w = window as unknown as {
      __invokes: Invoke[];
      __TAURI_INTERNALS__: {
        invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
        transformCallback: () => number;
      };
    };
    const internals = w.__TAURI_INTERNALS__;
    const mocked = internals.invoke;
    w.__invokes = [];
    internals.transformCallback = () => 0;
    internals.invoke = (cmd, args) => {
      w.__invokes.push({ cmd, args });
      return cmd === "download_model" ? new Promise(() => {}) : mocked(cmd, args);
    };
  });
  await page.goto("/");
}

const invokes = (page: Page) =>
  page.evaluate(() => (window as unknown as { __invokes: Invoke[] }).__invokes);

/** The `update_settings` call that marks onboarding complete, if one was sent. */
const completionIndex = (calls: Invoke[]) =>
  calls.findIndex(
    (c) =>
      c.cmd === "update_settings" &&
      (c.args?.settings as { onboardingCompleted?: boolean } | undefined)?.onboardingCompleted ===
        true,
  );

/** The `theme` of every `update_settings` sent, in order. Each one carries the
 * whole `Settings` object, so even the name step's save has one. */
const savedThemes = (calls: Invoke[]) =>
  calls
    .filter((c) => c.cmd === "update_settings")
    .map((c) => (c.args?.settings as { theme?: string } | undefined)?.theme);

/** What `<html data-theme>` pins the palette to: null for System, which pins
 * nothing and leaves the choice to the device. */
const pinnedTheme = (page: Page) =>
  page.evaluate(() => document.documentElement.getAttribute("data-theme"));

/** Welcome, then past the name step, onto the appearance step. */
async function toAppearance(page: Page) {
  await page.getByRole("button", { name: "Let's set things up" }).click();
  await skipStep(page); // the name step
  await expect(appearanceHeading(page)).toBeVisible();
}

/** ...and on past the appearance step, with nothing picked, to the notifications step. */
async function toNotifications(page: Page) {
  await toAppearance(page);
  await skipStep(page); // the appearance step
  await expect(notificationsHeading(page)).toBeVisible();
}

/** ...and on past the notifications step, left off, to the hardware scan. */
async function toScan(page: Page) {
  await toNotifications(page);
  await skipStep(page); // the notifications step
  await expect(scanHeading(page)).toBeVisible();
}

type Frame = { theme: string | null; paper: string; drawn: boolean };

/** Samples what each animation frame paints, from the document's first: the
 * theme `<html>` is pinned to, the colour of the page, and whether the app itself
 * (the shell or onboarding, not just its bare titlebar) is on screen yet. It is an
 * init script, so a reload starts a new recording from the relaunched page's own
 * first frame. */
async function recordFrames(page: Page) {
  await page.addInitScript(() => {
    const frames: Frame[] = [];
    (window as unknown as { __frames: Frame[] }).__frames = frames;
    const sample = () => {
      frames.push({
        theme: document.documentElement.getAttribute("data-theme"),
        paper: document.body ? getComputedStyle(document.body).backgroundColor : "",
        drawn: document.querySelector(".shell, .onboarding") !== null,
      });
      requestAnimationFrame(sample);
    };
    requestAnimationFrame(sample);
  });
}

/** Every frame from the one that first showed the app. Before it the page is a
 * bare titlebar, drawn before settings could be read, which follows the device. */
const drawnFrames = async (page: Page) =>
  (await page.evaluate(() => (window as unknown as { __frames: Frame[] }).__frames)).filter(
    (frame) => frame.drawn,
  );

/** The app was dark from the first frame it was drawn in: the device's light theme
 * is never painted first and then switched. */
async function expectDarkFromFirstFrame(page: Page) {
  // Without this, an app that was never sampled would pass for one that was dark.
  await expect.poll(async () => (await drawnFrames(page)).length).toBeGreaterThan(0);
  const notDark = (await drawnFrames(page)).filter(
    (frame) => frame.theme !== "dark" || frame.paper !== DARK.paper,
  );
  expect(notDark, "frames drawn in another theme").toEqual([]);
}

test("welcome scene asks before skipping, and can be backed out of", async ({ page }) => {
  await bootFreshInstall(page);

  await expect(page.getByRole("button", { name: "Let's set things up" })).toBeVisible();
  // Its main button already moves on, so the only skip here is the whole setup.
  await expect(page.getByRole("button", { name: "Skip this step" })).toHaveCount(0);
  await skipSetup(page);

  await expect(skipDialog(page)).toBeVisible();
  await skipDialog(page).getByRole("button", { name: "Cancel" }).click();
  await expect(skipDialog(page)).toBeHidden();
  await expect(page.getByRole("button", { name: "Let's set things up" })).toBeVisible();

  await skipSetup(page);
  await confirmSkip(page);

  await expect(page.getByRole("heading", { name: "What can I help with?" })).toBeVisible();
  await expect(shell(page)).toBeVisible();
});

test("name scene's Skip this step moves on without asking", async ({ page }) => {
  await bootFreshInstall(page);

  await page.getByRole("button", { name: "Let's set things up" }).click();
  await expect(page.getByRole("heading", { name: "What should we call you?" })).toBeVisible();
  await skipStep(page);

  await expect(appearanceHeading(page)).toBeVisible();
  await expect(skipDialog(page)).toHaveCount(0);
});

test("scan scene: Skip this step skips the model setup, through to the final screen", async ({
  page,
}) => {
  await bootFreshInstall(page);

  await toScan(page);
  await skipStep(page); // the model setup

  // The PIN offer, then the final screen — which doesn't claim a model is awake.
  await expect(page.getByRole("heading", { name: "Add a PIN?" })).toBeVisible();
  await skipStep(page);
  await expect(page.getByRole("heading", { name: "Almost there." })).toBeVisible();
  await expect(page.getByText(/Pick a model in Models whenever you're ready/)).toBeVisible();
  await expect(page.getByText(/is awake and listening/)).toHaveCount(0);

  await page.getByRole("button", { name: "Get started" }).click();

  await expect(page.getByRole("heading", { name: "What can I help with?" })).toBeVisible();
  await expect(shell(page)).toBeVisible();
  expect((await invokes(page)).map((c) => c.cmd)).not.toContain("download_model");
});

test("scan scene: Skip setup can be backed out of, then confirmed into the app", async ({
  page,
}) => {
  await bootFreshInstall(page);

  await toScan(page);

  await skipSetup(page);
  await skipDialog(page).getByRole("button", { name: "Cancel" }).click();
  await expect(skipDialog(page)).toBeHidden();
  await expect(page.getByRole("button", { name: "Show my match" })).toBeVisible();

  await skipSetup(page);
  await confirmSkip(page);

  await expect(page.getByRole("heading", { name: "What can I help with?" })).toBeVisible();
  await expect(shell(page)).toBeVisible();
});

test("download scene: Skip this step drops the download in flight and moves on", async ({
  page,
}) => {
  await bootFreshInstall(page);

  await toScan(page);
  await page.getByRole("button", { name: "Show my match" }).click();
  await expect(page.getByRole("heading", { name: "Qwen 3 4B" })).toBeVisible();
  await page.getByRole("button", { name: /^Download/ }).click();
  await expect(page.getByRole("heading", { name: "Bringing Qwen 3 4B home…" })).toBeVisible();

  await skipStep(page);

  await expect(page.getByRole("heading", { name: "Add a PIN?" })).toBeVisible();
  await expect(page.getByText("You can add one later in Settings.")).toBeVisible();
  const calls = await invokes(page);
  expect(calls.find((c) => c.cmd === "cancel_download")?.args).toEqual({ modelId: "qwen3-4b" });
  expect(completionIndex(calls)).toBe(-1);

  // The way out says the download was stopped, and that it can be resumed.
  await skipStep(page);
  await expect(page.getByRole("heading", { name: "Almost there." })).toBeVisible();
  await expect(page.getByText("Your download was stopped.", { exact: false })).toBeVisible();
});

test("download scene: Skip setup warns, then saves the skip before dropping the download", async ({
  page,
}) => {
  await bootFreshInstall(page);

  await toScan(page);
  await page.getByRole("button", { name: "Show my match" }).click();
  await page.getByRole("button", { name: /^Download/ }).click();
  await expect(page.getByRole("heading", { name: "Bringing Qwen 3 4B home…" })).toBeVisible();

  await skipSetup(page);

  await expect(skipDialog(page)).toContainText("Your model download will stop.");
  // Backing out leaves the download running: nothing was cancelled or saved.
  await skipDialog(page).getByRole("button", { name: "Cancel" }).click();
  await expect(page.getByRole("heading", { name: "Bringing Qwen 3 4B home…" })).toBeVisible();
  let calls = await invokes(page);
  expect(calls.map((c) => c.cmd)).not.toContain("cancel_download");
  expect(completionIndex(calls)).toBe(-1);

  await skipSetup(page);
  await confirmSkip(page);

  await expect(page.getByRole("heading", { name: "What can I help with?" })).toBeVisible();
  await expect(shell(page)).toBeVisible();
  await expect.poll(async () => (await invokes(page)).some((c) => c.cmd === "cancel_download")).toBe(
    true,
  );
  calls = await invokes(page);
  expect(completionIndex(calls)).toBeGreaterThanOrEqual(0);
  expect(calls.findIndex((c) => c.cmd === "cancel_download")).toBeGreaterThan(
    completionIndex(calls),
  );
});

test("PIN offer: Skip setup asks, then goes straight into the app", async ({ page }) => {
  await bootFreshInstall(page);

  await toScan(page);
  await skipStep(page); // the model setup
  await expect(page.getByRole("heading", { name: "Add a PIN?" })).toBeVisible();

  await skipSetup(page);
  await expect(skipDialog(page)).toBeVisible();
  await confirmSkip(page);

  await expect(page.getByRole("heading", { name: "What can I help with?" })).toBeVisible();
  await expect(shell(page)).toBeVisible();
});

// The appearance step: picking a theme re-themes onboarding at once and is saved
// there and then, and leaving it with nothing picked leaves the theme to the
// device. Each test says which way the device leans — Playwright's own default
// is light whatever the host is set to — and sets it before the page opens.

/** The three ways out of the appearance step, and what each one lands on. */
const WAYS_OUT: {
  way: string;
  leave: (page: Page) => Promise<void>;
  lands: (page: Page) => Locator;
}[] = [
  {
    way: "Continue",
    leave: (page) => page.getByRole("button", { name: "Continue" }).click(),
    lands: notificationsHeading,
  },
  { way: "Skip this step", leave: skipStep, lands: notificationsHeading },
  {
    way: "Skip setup",
    leave: async (page) => {
      await skipSetup(page);
      await confirmSkip(page);
    },
    lands: shell,
  },
];

for (const { way, leave, lands } of WAYS_OUT) {
  test(`appearance scene: ${way} with nothing picked leaves the theme to the device`, async ({
    page,
  }) => {
    // A dark device, so that a theme pinned by mistake shows: as a pin on
    // <html> even when it pins the dark the device already gives, as light
    // paper when it pins light.
    await page.emulateMedia({ colorScheme: "dark" });
    await bootFreshInstall(page);
    await toAppearance(page);
    await expect(themeOption(page, "System")).toBeChecked();

    await leave(page);
    await expect(lands(page)).toBeVisible();

    expect(await pinnedTheme(page)).toBeNull();
    await expect(page.locator("body")).toHaveCSS("background-color", DARK.paper);
    // The name step's save carries the whole settings, theme included, so there
    // is always something to look at here.
    const themes = savedThemes(await invokes(page));
    expect(themes.length).toBeGreaterThan(0);
    expect(themes.filter((theme) => theme === "light" || theme === "dark")).toEqual([]);
  });
}

test("appearance scene: picking Dark re-themes the step at once, and saves the pick", async ({
  page,
}) => {
  await page.emulateMedia({ colorScheme: "light" });
  await bootFreshInstall(page);
  await toAppearance(page);
  const heading = appearanceHeading(page);
  const body = page.locator("body");
  await expect(body).toHaveCSS("background-color", LIGHT.paper);
  await expect(heading).toHaveCSS("color", LIGHT.ink);
  // Marked, so that a remount of the scene (which would replay its entrance
  // animations) can't pass for a re-theme.
  await heading.evaluate((el) => el.setAttribute("data-same-node", ""));

  await themeOption(page, "Dark").check();

  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(body).toHaveCSS("background-color", DARK.paper);
  await expect(heading).toHaveCSS("color", DARK.ink);
  await expect(heading).toBeVisible();
  await expect(heading).toHaveAttribute("data-same-node", "");
  await expect(themeOption(page, "Dark")).toBeChecked();
  await expect(themeOption(page, "System")).not.toBeChecked();
  // Saved on the pick, not on Continue — and onboarding is still open.
  await expect.poll(async () => savedThemes(await invokes(page)).at(-1)).toBe("dark");
  expect(completionIndex(await invokes(page))).toBe(-1);
});

test("appearance scene: Skip setup asks first, in the theme picked, and Cancel keeps the pick", async ({
  page,
}) => {
  await page.emulateMedia({ colorScheme: "light" });
  await bootFreshInstall(page);
  await toAppearance(page);
  await themeOption(page, "Dark").check();

  await skipSetup(page);

  // The question is portalled into <body>, outside the app's root, so it is
  // themed (and so is the page behind it) only if the theme sits on <html>.
  await expect(skipDialog(page)).toBeVisible();
  await expect(skipDialog(page)).toContainText("You can't chat until you pick a model");
  await expect(skipDialog(page)).toHaveCSS("background-color", DARK.raised);
  await expect(skipDialog(page).getByRole("heading")).toHaveCSS("color", DARK.ink);
  await expect(page.locator("body")).toHaveCSS("background-color", DARK.paper);
  expect(completionIndex(await invokes(page))).toBe(-1);

  await skipDialog(page).getByRole("button", { name: "Cancel" }).click();
  await expect(skipDialog(page)).toBeHidden();
  await expect(appearanceHeading(page)).toBeVisible();
  await expect(themeOption(page, "Dark")).toBeChecked();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  expect(completionIndex(await invokes(page))).toBe(-1);
});

test("appearance scene: a pick outlives Skip setup, and Settings shows it", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "light" });
  await bootFreshInstall(page, { persist: true });
  await toAppearance(page);
  await themeOption(page, "Dark").check();

  await skipSetup(page);
  await confirmSkip(page);

  // Finishing reads the saved settings back (`completeOnboarding`), which is
  // where a pick that only lived in memory would be lost.
  await expect(page.getByRole("heading", { name: "What can I help with?" })).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(page.locator("body")).toHaveCSS("background-color", DARK.paper);

  await page.getByRole("button", { name: "More", exact: true }).click();
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const theme = page
    .getByRole("dialog", { name: "Settings" })
    .getByRole("radiogroup", { name: "Theme" });
  await expect(theme.getByRole("radio", { name: "Dark" })).toBeChecked();
});

test("appearance scene: after a relaunch the app is dark from its first frame", async ({
  page,
}) => {
  await page.emulateMedia({ colorScheme: "light" });
  await recordFrames(page);
  await bootFreshInstall(page, { persist: true });
  await toAppearance(page);
  await themeOption(page, "Dark").check();
  await skipSetup(page);
  await confirmSkip(page);
  await expect(shell(page)).toBeVisible();

  // Quit and open it again: a reload re-runs the mocked backend, which hands
  // back what was last saved, the way the real one does.
  await page.reload();

  await expect(shell(page)).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expectDarkFromFirstFrame(page);
});

test("appearance scene: quitting mid-onboarding, the relaunched welcome is dark from its first frame", async ({
  page,
}) => {
  await page.emulateMedia({ colorScheme: "light" });
  await recordFrames(page);
  await bootFreshInstall(page, { persist: true });
  await toAppearance(page);
  await themeOption(page, "Dark").check();
  await expect.poll(async () => savedThemes(await invokes(page)).at(-1)).toBe("dark");

  await page.reload();

  // Onboarding starts over from the welcome, in the theme that was saved.
  await expect(page.getByRole("button", { name: "Let's set things up" })).toBeVisible();
  await expectDarkFromFirstFrame(page);
  await toAppearance(page);
  await expect(themeOption(page, "Dark")).toBeChecked();
});

// The notifications step: turning them on is saved there and then and asks the
// OS (on macOS, its one-time prompt); skipping it, or the whole setup, leaves
// them off. The OS side is the mocked `request_notification_permission` command
// and `notification_permission`, which Settings reads ("granted" unless a spec
// says otherwise): a real notification, and the OS really reporting it blocked, can
// only be seen in the shipped app (see docs/TESTING.md).

/** The `desktopNotifications` of every `update_settings` sent, in order. Each one
 * carries the whole `Settings`, so even the name step's save has one (false, from
 * the fresh install's settings). */
const savedNotificationFlags = (calls: Invoke[]) =>
  calls
    .filter((c) => c.cmd === "update_settings")
    .map((c) => (c.args?.settings as { desktopNotifications?: boolean } | undefined)?.desktopNotifications);

/** Opens Settings (its General page) from the app shell's More menu. */
async function openSettings(page: Page) {
  await page.getByRole("button", { name: "More", exact: true }).click();
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  return page.getByRole("dialog", { name: "Settings" });
}
const notificationsSwitch = (dialog: Locator) =>
  dialog.getByRole("checkbox", { name: "Desktop notifications" });
const BLOCKED = "Notifications are blocked for OpenPal. In your system settings, turn on \"Allow notifications\" for OpenPal, then come back here.";

/** Every console error the page logs from here on. */
function collectConsoleErrors(page: Page) {
  const errors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") errors.push(msg.text());
  });
  page.on("pageerror", (error) => errors.push(String(error)));
  return errors;
}

test("notifications scene: Skip this step leaves them off all the way into Settings", async ({
  page,
}) => {
  const errors = collectConsoleErrors(page);
  await bootFreshInstall(page, { persist: true });
  await toNotifications(page);
  await expect(
    page.getByText(/OpenPal can notify you when a model loads, unloads or finishes downloading/),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "Turn on notifications" })).toBeVisible();

  await skipStep(page);

  await expect(scanHeading(page)).toBeVisible();
  expect(savedNotificationFlags(await invokes(page)).filter((on) => on === true)).toEqual([]);

  await skipSetup(page);
  await confirmSkip(page);
  await expect(shell(page)).toBeVisible();
  const settings = await openSettings(page);
  await expect(notificationsSwitch(settings)).not.toBeChecked();
  await expect(settings.getByText(BLOCKED)).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("notifications scene: Turn on notifications saves it on, asks the OS, and Settings shows it on, even after a relaunch", async ({
  page,
}) => {
  const errors = collectConsoleErrors(page);
  await bootFreshInstall(page, { persist: true });
  await toNotifications(page);

  await page.getByRole("button", { name: "Turn on notifications" }).click();

  // Straight on to the scan: the choice is saved, nothing waits for the OS.
  await expect(scanHeading(page)).toBeVisible();
  await expect.poll(async () => savedNotificationFlags(await invokes(page)).at(-1)).toBe(true);
  // ...and the OS is asked once the choice is safe on disk.
  await expect
    .poll(async () => {
      const calls = await invokes(page);
      const saved = calls.findLastIndex(
        (c) =>
          c.cmd === "update_settings" &&
          (c.args?.settings as { desktopNotifications?: boolean } | undefined)
            ?.desktopNotifications === true,
      );
      return (
        saved >= 0 && calls.slice(saved).some((c) => c.cmd === "request_notification_permission")
      );
    })
    .toBe(true);
  expect(completionIndex(await invokes(page))).toBe(-1);

  await skipSetup(page);
  await confirmSkip(page);
  await expect(shell(page)).toBeVisible();
  let settings = await openSettings(page);
  await expect(notificationsSwitch(settings)).toBeChecked();
  // The OS allowed it: no "blocked" line.
  await expect(settings.getByText(BLOCKED)).toHaveCount(0);

  // Quit and open it again: the choice was saved, not just held in memory.
  await page.reload();
  await expect(shell(page)).toBeVisible();
  settings = await openSettings(page);
  await expect(notificationsSwitch(settings)).toBeChecked();
  expect(errors).toEqual([]);
});

test("notifications scene: when the OS blocks them, onboarding goes on, the switch stays on, and Settings says why", async ({
  page,
}) => {
  const errors = collectConsoleErrors(page);
  // Both answer "denied", as the OS would: the ask during onboarding, and the
  // read Settings does when it opens.
  await bootFreshInstall(page, {
    persist: true,
    responses: { notification_permission: "denied", request_notification_permission: "denied" },
  });
  await toNotifications(page);

  await page.getByRole("button", { name: "Turn on notifications" }).click();

  await expect(scanHeading(page)).toBeVisible();
  await skipSetup(page);
  await confirmSkip(page);
  await expect(shell(page)).toBeVisible();
  const settings = await openSettings(page);

  const toggle = notificationsSwitch(settings);
  await expect(toggle).toBeChecked();
  await expect(toggle).toBeEnabled();
  const line = settings.getByText(BLOCKED);
  await expect(line).toBeVisible();
  // A polite status the switch is described by, not an interruption.
  await expect(line).toHaveAttribute("role", "status");
  await expect(toggle).toHaveAccessibleDescription(new RegExp(BLOCKED.replace(/\./g, "\\.")));
  await expect(settings.getByRole("alert")).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("notifications scene: Skip setup asks first and leaves them off", async ({ page }) => {
  await bootFreshInstall(page);
  await toNotifications(page);

  await skipSetup(page);
  await expect(skipDialog(page)).toBeVisible();
  await expect(skipDialog(page)).toContainText("You can't chat until you pick a model");
  // Backing out returns to the same step, still undecided.
  await skipDialog(page).getByRole("button", { name: "Cancel" }).click();
  await expect(skipDialog(page)).toBeHidden();
  await expect(notificationsHeading(page)).toBeVisible();

  await skipSetup(page);
  await confirmSkip(page);

  await expect(shell(page)).toBeVisible();
  expect(savedNotificationFlags(await invokes(page)).filter((on) => on === true)).toEqual([]);
});
