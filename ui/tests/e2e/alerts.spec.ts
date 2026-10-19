import { expect, test, type Page } from "@playwright/test";
import { freshInstallResponses } from "./support/fixtures";
import { mockTauriIpc } from "./support/tauriMock";

/**
 * The in-app alerts and the native-notification calls, driven the only way the
 * mocked IPC can: by changing what `get_engine_status` answers while the page
 * polls it (every 2s, on a fake clock here, so a test doesn't sit through the
 * waits). Answers and downloads arrive over Channels, which this harness cannot
 * stream -- those events are covered by the store's Vitest tests -- and an OS
 * notification actually appearing needs the shipped app. What this does show,
 * in a real browser, is everything around them: that the host paints above a
 * dialog, never takes focus from the composer, clears itself, can be dismissed
 * from the keyboard, and that the window's focus and blur reach the rule that
 * decides whether `notify_native` is called at all.
 */

const MODEL_ID = "gemma-4-e4b";

const MODEL = {
  spec: {
    id: MODEL_ID,
    name: "Gemma 4 E4B",
    family: "Test",
    description: "A model used by the alerts e2e.",
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
    curated: true,
    source: "catalog",
    localPath: null,
    vision: false,
    taskScores: {},
    releaseDate: "2025-08-01",
    publisher: "Google",
  },
  status: { type: "installed", data: { path: "/m/a.gguf", sizeBytes: 4_590_807_392 } },
  fit: "perfect",
  why: "Fits easily in your memory.",
  isRecommended: false,
  favorite: false,
};

type Engine =
  | { type: "stopped" }
  | { type: "starting"; data: { modelId: string; progressPercent: number | null } }
  | { type: "running"; data: { modelId: string; port: number; diskSlow: boolean; autoUnloadAt: number | null } };

const STOPPED: Engine = { type: "stopped" };
const STARTING: Engine = { type: "starting", data: { modelId: MODEL_ID, progressPercent: null } };
const RUNNING: Engine = {
  type: "running",
  data: { modelId: MODEL_ID, port: 8080, diskSlow: false, autoUnloadAt: null },
};

type Invoke = { cmd: string; args?: Record<string, unknown> };

/** Opens an install on a fake clock, with `get_engine_status` answering
 * whatever `setEngine` last said and every command recorded. By default an
 * onboarded one (the app shell); `onboarding` opens the first-run flow instead. */
async function boot(
  page: Page,
  { engine = STOPPED, desktopNotifications = false, onboarding = false } = {},
) {
  const base = freshInstallResponses();
  await mockTauriIpc(page, {
    ...base,
    get_settings: {
      ...(base.get_settings as Record<string, unknown>),
      onboardingCompleted: !onboarding,
      userName: onboarding ? null : "Ada",
      autoLoadOnStartup: false,
      desktopNotifications,
    },
    get_models: [MODEL],
    update_settings: null,
    notify_native: null,
    "plugin:app|version": "0.1.0",
  });
  await page.addInitScript((initial) => {
    const w = window as unknown as {
      __engine: unknown;
      __invokes: Invoke[];
      __TAURI_INTERNALS__: {
        invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
      };
    };
    const mocked = w.__TAURI_INTERNALS__.invoke;
    w.__engine = initial;
    w.__invokes = [];
    w.__TAURI_INTERNALS__.invoke = (cmd, args) => {
      w.__invokes.push({ cmd, args });
      return cmd === "get_engine_status" ? Promise.resolve(w.__engine) : mocked(cmd, args);
    };
  }, engine);
  await page.clock.install({ time: 0 });
  await page.goto("/");
  if (onboarding) await expect(page.locator(".ob-scene")).toBeVisible();
  else await expect(page.getByRole("heading", { name: /Hi, Ada/ })).toBeVisible();
  // An installed clock keeps running in real time. Stopped, it moves only when
  // `tick` says so: an alert lives 6s, and a test that spends real time on its
  // assertions (a loaded machine spends a lot) must not lose it to the wall clock.
  await page.clock.pauseAt(60_000);
  // The first status read is the baseline: whatever it shows is not news.
  await tick(page);
}

const setEngine = (page: Page, engine: Engine) =>
  page.evaluate((e) => {
    (window as unknown as { __engine: unknown }).__engine = e;
  }, engine);

/** One engine poll (they come every 2s), and the time for its answer to land. */
async function tick(page: Page, times = 1) {
  for (let i = 0; i < times; i++) {
    await page.clock.runFor(2000);
    await page.waitForTimeout(50);
  }
}

const calls = (page: Page, cmd: string) =>
  page
    .evaluate(() => (window as unknown as { __invokes: Invoke[] }).__invokes)
    .then((all) => all.filter((c) => c.cmd === cmd));

const alertCards = (page: Page) => page.locator('[data-slot="alert"]');
const composer = (page: Page) => page.getByRole("textbox", { name: "Message" });

/** The window losing and regaining focus, the way the webview reports it. */
const blurWindow = (page: Page) => page.evaluate(() => window.dispatchEvent(new Event("blur")));
const focusWindow = (page: Page) => page.evaluate(() => window.dispatchEvent(new Event("focus")));

test("a model finishing loading shows one alert naming it, and leaves the composer's focus alone", async ({
  page,
}) => {
  await boot(page);
  await composer(page).focus();

  await setEngine(page, STARTING);
  await tick(page);
  await expect(alertCards(page)).toHaveCount(0);

  await setEngine(page, RUNNING);
  await tick(page);
  const card = alertCards(page).first();
  await expect(card).toBeVisible();
  await expect(card).toContainText("Model ready");
  await expect(card).toContainText("Gemma 4 E4B is loaded and ready to chat.");
  await expect(composer(page)).toBeFocused();

  // It is the styled shadcn Alert, not bare text: Tailwind generated its
  // utilities (nothing else would catch a mis-pointed `@source`), on the
  // palette's raised-paper surface.
  await expect(card).toHaveCSS("display", "grid");
  await expect(card).toHaveCSS("border-top-width", "1px");
  await expect(card).not.toHaveCSS("background-color", "rgba(0, 0, 0, 0)");
  await expect(card).not.toHaveCSS("padding-top", "0px");

  // Announced politely, from a region that was already there, and the visible
  // card is not a second (assertive) one.
  await expect(
    page.locator('[role="status"][aria-live="polite"]').filter({ hasText: "Model ready" }),
  ).toBeAttached();
  await expect(page.locator('[role="alert"]')).toHaveCount(0);

  // The same Running status read again and again is not another load.
  await tick(page, 2);
  await expect(alertCards(page)).toHaveCount(1);

  // It clears itself after a few seconds, and stays cleared.
  await page.clock.runFor(6000);
  await expect(alertCards(page)).toHaveCount(0);
  await tick(page, 3);
  await expect(alertCards(page)).toHaveCount(0);
  await expect(composer(page)).toBeFocused();
});

test("an unload is announced once, and the first status read after launch announces nothing", async ({
  page,
}) => {
  // Already loaded when the app opens (an auto-wake that beat the first read):
  // not news. The unload that follows is.
  await boot(page, { engine: RUNNING });
  await expect(alertCards(page)).toHaveCount(0);

  await setEngine(page, STOPPED);
  await tick(page, 3);
  await expect(alertCards(page)).toHaveCount(1);
  await expect(alertCards(page).first()).toContainText("Model unloaded");
  await expect(alertCards(page).first()).toContainText("Gemma 4 E4B is no longer loaded.");
});

test("a swap from one model to another is a load, not an unload followed by a load", async ({
  page,
}) => {
  await boot(page, { engine: RUNNING });
  await setEngine(page, STARTING);
  await tick(page);
  await setEngine(page, RUNNING);
  await tick(page);
  await expect(alertCards(page)).toHaveCount(1);
  await expect(alertCards(page).first()).toContainText("Model ready");
});

test("an alert can be reached and dismissed from the keyboard, without focus being lost", async ({
  page,
}) => {
  await boot(page, { engine: RUNNING });
  await setEngine(page, STOPPED);
  await tick(page);
  await expect(alertCards(page)).toHaveCount(1);

  await composer(page).focus();
  const dismiss = page.getByRole("button", { name: "Dismiss Model unloaded" });
  // The host sits right after the main pane, so Tab gets there from the
  // composer in a handful of presses -- not by being sent focus.
  let reached = false;
  for (let i = 0; i < 12 && !reached; i++) {
    await page.keyboard.press("Tab");
    reached = await dismiss.evaluate((el) => el === document.activeElement);
  }
  expect(reached, "Tab reaches the Dismiss button").toBe(true);

  // Focused, the countdown holds: well past the 6s it would have lasted.
  await page.clock.runFor(10_000);
  await expect(alertCards(page)).toHaveCount(1);

  await page.keyboard.press("Escape");
  await expect(alertCards(page)).toHaveCount(0);
  // Focus was handed back to something real rather than dropped on <body>.
  expect(await page.evaluate(() => document.activeElement === document.body)).toBe(false);
});

test("Escape anywhere else leaves an alert alone", async ({ page }) => {
  await boot(page, { engine: RUNNING });
  await setEngine(page, STOPPED);
  await tick(page);
  await composer(page).focus();
  await page.keyboard.press("Escape");
  await expect(alertCards(page)).toHaveCount(1);
});

test("an alert paints above an open dialog and can be clicked through it", async ({ page }) => {
  await boot(page, { engine: RUNNING });
  await page.getByRole("button", { name: "More", exact: true }).click();
  await page.getByRole("button", { name: "Models", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "Models" })).toBeVisible();

  await setEngine(page, STOPPED);
  await tick(page);
  const card = alertCards(page).first();
  await expect(card).toBeVisible();

  // Whatever is on top at the card's centre is the card, not the dialog's backdrop.
  const onTop = await card.evaluate((el) => {
    const r = el.getBoundingClientRect();
    const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
    return !!hit && el.contains(hit);
  });
  expect(onTop).toBe(true);

  // Below the titlebar band, and inside the window.
  const box = (await card.boundingBox())!;
  const viewport = page.viewportSize()!;
  expect(box.y).toBeGreaterThanOrEqual(44);
  expect(box.x + box.width).toBeLessThanOrEqual(viewport.width);

  // Dismissing it with the mouse does not close the dialog behind it.
  await page.getByRole("button", { name: "Dismiss Model unloaded" }).click();
  await expect(alertCards(page)).toHaveCount(0);
  await expect(page.getByRole("dialog", { name: "Models" })).toBeVisible();
});

test("several alerts at once stack newest first and stay within the cap", async ({ page }) => {
  await boot(page, { engine: RUNNING });
  // Keep the first alert's countdown held (focus inside the stack pauses it) so
  // the four events, a few seconds apart, are on screen together.
  await setEngine(page, STOPPED);
  await tick(page);
  await page.getByRole("button", { name: "Dismiss Model unloaded" }).focus();
  const cards = alertCards(page);
  await expect(cards).toHaveCount(1);

  for (const engine of [STARTING, RUNNING, STOPPED]) {
    await setEngine(page, engine);
    await tick(page);
  }
  // unloaded, loaded, unloaded -- newest first, nothing pushed out yet.
  await expect(cards).toHaveCount(3);
  await expect(cards.nth(0)).toContainText("Model unloaded");
  await expect(cards.nth(1)).toContainText("Model ready");
  await expect(cards.nth(2)).toContainText("Model unloaded");

  // A fourth pushes the oldest out, so there are still three.
  for (const engine of [STARTING, RUNNING]) {
    await setEngine(page, engine);
    await tick(page);
  }
  await expect(cards).toHaveCount(3);
  await expect(cards.nth(0)).toContainText("Model ready");
  await expect(cards.nth(1)).toContainText("Model unloaded");
  await expect(cards.nth(2)).toContainText("Model ready");
});

test("dismissing the lower card with the mouse does not leave the rest held", async ({ page }) => {
  // Chromium (WebView2, Android's WebView) sends no pointerleave when the card
  // under a resting pointer is removed; the stack must not stay frozen on it.
  await boot(page, { engine: RUNNING });
  await setEngine(page, STOPPED);
  await tick(page);
  await setEngine(page, STARTING);
  await tick(page);
  await setEngine(page, RUNNING);
  await tick(page);
  await expect(alertCards(page)).toHaveCount(2);

  await page.getByRole("button", { name: "Dismiss Model unloaded" }).click();
  await expect(alertCards(page)).toHaveCount(1);
  // The pointer now rests where the removed card was; move it well away. The
  // host re-checks `:hover` a frame after the list changed, and only then does
  // the survivor's countdown start, so give that frame its own tick.
  await page.mouse.move(5, 400);
  await page.clock.runFor(100);
  await page.waitForTimeout(100);

  await page.clock.runFor(8000);
  await expect(alertCards(page)).toHaveCount(0);
});

test("a press on an alert does not close the panel the user has open", async ({ page }) => {
  await boot(page, { engine: RUNNING });
  await page.getByRole("button", { name: "Conversation settings" }).click();
  const panel = page.getByRole("dialog", { name: "Conversation settings" });
  await expect(panel).toBeVisible();

  await setEngine(page, STOPPED);
  await tick(page);
  await expect(alertCards(page)).toHaveCount(1);
  await page.getByRole("button", { name: "Dismiss Model unloaded" }).click();

  // Reaching for the alert is not a click outside the panel.
  await expect(alertCards(page)).toHaveCount(0);
  await expect(panel).toBeVisible();
});

test("an alert raised during onboarding is shown too", async ({ page }) => {
  // The first model download and load happen here; the host is mounted
  // beside the first-run flow, not only in the app shell.
  await boot(page, { onboarding: true });
  await setEngine(page, STARTING);
  await tick(page);
  await setEngine(page, RUNNING);
  await tick(page);
  await expect(alertCards(page)).toHaveCount(1);
  await expect(alertCards(page).first()).toContainText("Model ready");
});

test("a native notification is sent only with the switch on and the window not focused", async ({
  page,
}) => {
  await boot(page, { engine: RUNNING, desktopNotifications: true });

  // Focused: the in-app alert, no native one.
  await focusWindow(page);
  await setEngine(page, STOPPED);
  await tick(page);
  await expect(alertCards(page)).toHaveCount(1);
  expect(await calls(page, "notify_native")).toHaveLength(0);

  // In another window: the same words go to the OS, once.
  await blurWindow(page);
  await setEngine(page, STARTING);
  await tick(page);
  await setEngine(page, RUNNING);
  await tick(page, 3);
  const sent = await calls(page, "notify_native");
  expect(sent).toHaveLength(1);
  // A model event opens no conversation when clicked.
  expect(sent[0].args).toEqual({
    title: "Model ready",
    body: "Gemma 4 E4B is loaded and ready to chat.",
    conversationId: null,
  });
  // Same title and text as the card.
  await expect(alertCards(page).first()).toContainText("Model ready");
  await expect(alertCards(page).first()).toContainText("Gemma 4 E4B is loaded and ready to chat.");

  // Back in the window: nothing more is sent.
  await focusWindow(page);
  await setEngine(page, STOPPED);
  await tick(page, 2);
  expect(await calls(page, "notify_native")).toHaveLength(1);
});

test("with the switch off nothing is ever sent to the OS, focused or not", async ({ page }) => {
  await boot(page, { engine: RUNNING, desktopNotifications: false });
  await blurWindow(page);
  await setEngine(page, STOPPED);
  await tick(page, 2);
  await expect(alertCards(page)).toHaveCount(1);
  expect(await calls(page, "notify_native")).toHaveLength(0);
});

test("a failing native notification surfaces nothing", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  page.on("console", (m) => {
    if (m.type() === "error") errors.push(m.text());
  });
  await boot(page, { engine: RUNNING, desktopNotifications: true });
  // The command is not even registered: the harness rejects, as a locked app would.
  await page.evaluate(() => {
    const internals = (
      window as unknown as {
        __TAURI_INTERNALS__: { invoke: (cmd: string, args?: unknown) => Promise<unknown> };
      }
    ).__TAURI_INTERNALS__;
    const before = internals.invoke;
    internals.invoke = (cmd, args) =>
      cmd === "notify_native" ? Promise.reject(new Error("not unlocked")) : before(cmd, args);
  });
  await blurWindow(page);
  await setEngine(page, STOPPED);
  await tick(page, 2);
  await expect(alertCards(page)).toHaveCount(1);
  expect(errors.filter((e) => !/favicon/i.test(e))).toEqual([]);
  await expect(page.locator('[role="alert"]')).toHaveCount(0);
});
