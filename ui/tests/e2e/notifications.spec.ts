import { expect, test, type Locator, type Page } from "@playwright/test";
import { freshInstallResponses } from "./support/fixtures";
import { persistSettings } from "./support/persistedSettings";
import { mockTauriIpc } from "./support/tauriMock";

/**
 * Settings -> General -> Notifications in a real browser: the "Desktop
 * notifications" switch, what it saves, and the "blocked" line. Getting there
 * from onboarding is in onboarding-skip.spec.ts; finding it through the More
 * dialog's search is in more-dialog.spec.ts.
 *
 * What the mocked IPC cannot show: the OS actually posting a notification, or
 * really reporting it blocked, which needs the shipped app. The OS side here is the
 * `notification_permission` command (a read) and `request_notification_permission`
 * (the ask a click on the switch makes), both answering one value that each test
 * can change while the page runs (`setOsPermission`) -- a user fixing it in system
 * settings and coming back, or answering "Don't Allow" at the system prompt.
 */

type Invoke = { cmd: string; args?: Record<string, unknown> };
type OsPermission = "granted" | "denied";

interface BootOptions {
  /** Saved settings, over an onboarded install's. `{ desktopNotifications:
   * undefined }` leaves the key out altogether, as a row saved before the
   * setting existed. */
  settings?: Record<string, unknown>;
  /** What the OS says about posting notifications to begin with. */
  permission?: OsPermission;
  /** Canned command responses, over the fixture's. */
  responses?: Record<string, unknown>;
  /** Let `page.reload()` act as a relaunch: saved settings are read back. */
  persist?: boolean;
  /** What shows once the app is up; the empty chat's greeting unless a spec
   * boots straight into something else. */
  ready?: (page: Page) => Locator;
}

/** Opens an onboarded install (the app shell), with every command the page sends
 * recorded (see `invokes`). */
async function boot(
  page: Page,
  {
    settings = {},
    permission = "granted",
    responses = {},
    persist = false,
    ready = chatGreeting,
  }: BootOptions = {},
) {
  const base = freshInstallResponses();
  const saved: Record<string, unknown> = {
    ...(base.get_settings as Record<string, unknown>),
    onboardingCompleted: true,
    userName: "Ada",
    ...settings,
  };
  for (const key of Object.keys(saved)) if (saved[key] === undefined) delete saved[key];
  await mockTauriIpc(page, {
    ...base,
    get_settings: saved,
    update_settings: null,
    "plugin:app|version": "0.1.0",
    // The "blocked" line's button. A spec for the case where the OS page cannot
    // be opened passes `{ open_notification_settings: undefined }`, which
    // drops the key (JSON has no undefined) so the mock rejects the call.
    open_notification_settings: null,
    // No notification has been clicked: what Rust answers the store's read at
    // the end of boot.
    take_notification_click: null,
    ...responses,
  });
  // Before the wrapper below, so that one still sees the writes this answers itself.
  if (persist) await persistSettings(page);
  await page.addInitScript((initial) => {
    const w = window as unknown as {
      __invokes: Invoke[];
      __osPermission: string;
      __TAURI_INTERNALS__: {
        invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
        transformCallback: () => number;
      };
    };
    const internals = w.__TAURI_INTERNALS__;
    const mocked = internals.invoke;
    w.__invokes = [];
    w.__osPermission = initial;
    // For anything that opens a Channel on mount (Android's pairing section).
    internals.transformCallback = () => 0;
    internals.invoke = (cmd, args) => {
      w.__invokes.push({ cmd, args });
      return cmd === "notification_permission" || cmd === "request_notification_permission"
        ? Promise.resolve(w.__osPermission)
        : mocked(cmd, args);
    };
  }, permission);
  await page.goto("/");
  await expect(ready(page)).toBeVisible();
}

/** The empty chat's greeting, in either language: present for as long as the
 * app shell is up. */
const chatGreeting = (page: Page) => page.getByRole("heading", { name: /Ada/ });

const invokes = (page: Page) =>
  page.evaluate(() => (window as unknown as { __invokes: Invoke[] }).__invokes);

/** What the OS says from now on, as if the user changed it in system settings. */
const setOsPermission = (page: Page, permission: OsPermission) =>
  page.evaluate((p) => {
    (window as unknown as { __osPermission: string }).__osPermission = p;
  }, permission);

/** The `desktopNotifications` of every `update_settings` sent, in order. */
const savedFlags = async (page: Page) =>
  (await invokes(page))
    .filter((c) => c.cmd === "update_settings")
    .map((c) => (c.args?.settings as { desktopNotifications?: boolean } | undefined)?.desktopNotifications);

/** Every console error the page logs from here on. */
function collectConsoleErrors(page: Page) {
  const errors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") errors.push(msg.text());
  });
  page.on("pageerror", (error) => errors.push(String(error)));
  return errors;
}

const WORDS = {
  en: {
    more: "More",
    settings: "Settings",
    group: "Notifications",
    label: "Desktop notifications",
    blocked: "Notifications are blocked for OpenPal. In your system settings, turn on \"Allow notifications\" for OpenPal, then come back here.",
    openSettings: "Open notification settings",
    openFailed: "Couldn't open your system settings. Open them yourself, find OpenPal under Notifications and turn on \"Allow notifications\".",
  },
  fr: {
    more: "Plus",
    settings: "Réglages",
    group: "Notifications",
    label: "Notifications de bureau",
    blocked: "Les notifications sont bloquées pour OpenPal. Dans les réglages de votre système, activez « Autoriser les notifications » pour OpenPal, puis revenez ici.",
    openSettings: "Ouvrir les réglages des notifications",
    openFailed: "Impossible d'ouvrir les réglages de votre système. Ouvrez-les vous-même, trouvez OpenPal dans Notifications et activez « Autoriser les notifications ».",
  },
} as const;

/** Opens Settings on its first page, General, from the More menu. */
async function openSettings(page: Page, words: (typeof WORDS)[keyof typeof WORDS] = WORDS.en) {
  await page.getByRole("button", { name: words.more, exact: true }).click();
  await page.getByRole("button", { name: words.settings, exact: true }).click();
  return page.getByRole("dialog", { name: words.settings });
}

const switchIn = (dialog: Locator, label: string = WORDS.en.label) =>
  dialog.getByRole("checkbox", { name: label });

test("an install saved before the setting existed has it off; one click turns it on, saved with the rest", async ({
  page,
}) => {
  const errors = collectConsoleErrors(page);
  await boot(page, { settings: { desktopNotifications: undefined } });
  const dialog = await openSettings(page);

  // The group is headed, and the switch is named by its label (not by "Off").
  await expect(dialog.getByRole("heading", { level: 2, name: "Notifications" })).toBeVisible();
  const toggle = switchIn(dialog);
  await expect(toggle).not.toBeChecked();
  await expect(toggle).toHaveAccessibleDescription(
    "Get a system notification about models and answers while OpenPal is in the background.",
  );
  await expect(dialog.getByText(WORDS.en.blocked)).toHaveCount(0);

  await toggle.check();

  await expect(toggle).toBeChecked();
  await expect.poll(async () => (await savedFlags(page)).at(-1)).toBe(true);
  // The whole settings go out with it, so nothing else is lost.
  const write = (await invokes(page)).filter((c) => c.cmd === "update_settings").at(-1);
  expect(write?.args?.settings).toMatchObject({ desktopNotifications: true, userName: "Ada" });
  await expect(dialog.getByText(WORDS.en.blocked)).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("the choice survives a relaunch, on and off", async ({ page }) => {
  await boot(page, { persist: true });
  let dialog = await openSettings(page);
  await expect(switchIn(dialog)).not.toBeChecked();

  await switchIn(dialog).check();
  await expect.poll(async () => (await savedFlags(page)).at(-1)).toBe(true);

  await page.reload();
  await expect(chatGreeting(page)).toBeVisible();
  dialog = await openSettings(page);
  await expect(switchIn(dialog)).toBeChecked();

  await switchIn(dialog).uncheck();
  await expect.poll(async () => (await savedFlags(page)).at(-1)).toBe(false);

  await page.reload();
  await expect(chatGreeting(page)).toBeVisible();
  dialog = await openSettings(page);
  await expect(switchIn(dialog)).not.toBeChecked();
});

test("the switch works from the keyboard alone", async ({ page }) => {
  await boot(page);
  const dialog = await openSettings(page);
  const toggle = switchIn(dialog);

  await toggle.focus();
  await page.keyboard.press("Space");
  await expect(toggle).toBeChecked();
  await page.keyboard.press("Space");
  await expect(toggle).not.toBeChecked();
  await expect.poll(async () => (await savedFlags(page)).slice(-2)).toEqual([true, false]);
});

test("the blocked line shows only while the switch is on and the OS says no", async ({ page }) => {
  const errors = collectConsoleErrors(page);
  await boot(page, { permission: "denied" });
  const dialog = await openSettings(page);
  const toggle = switchIn(dialog);
  const line = dialog.getByText(WORDS.en.blocked);

  // Off: nobody asked for notifications, so there is nothing to say is blocked.
  await expect(toggle).not.toBeChecked();
  await expect(line).toHaveCount(0);

  await toggle.check();

  await expect(line).toBeVisible();
  // The switch stays on and usable, and the line is a polite status that
  // describes it, not an interrupting alert.
  await expect(toggle).toBeChecked();
  await expect(toggle).toBeEnabled();
  await expect(toggle).toHaveAccessibleDescription(/Get a system notification[\s\S]*blocked for OpenPal/);
  await expect(line).toHaveAttribute("role", "status");
  await expect(dialog.getByRole("alert")).toHaveCount(0);

  await toggle.uncheck();
  await expect(line).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("the blocked line offers a button to the OS notification settings, and only while it shows", async ({
  page,
}) => {
  const errors = collectConsoleErrors(page);
  await boot(page, { settings: { desktopNotifications: true }, permission: "denied" });
  const dialog = await openSettings(page);
  const button = dialog.getByRole("button", { name: WORDS.en.openSettings });
  const count = async (cmd: string) => (await invokes(page)).filter((c) => c.cmd === cmd).length;

  // Next to the line, outside its status region, and reached by Tab from the switch.
  await expect(dialog.getByText(WORDS.en.blocked)).toBeVisible();
  await expect(button).toBeVisible();
  await expect(button).toBeEnabled();
  await switchIn(dialog).focus();
  await page.keyboard.press("Tab");
  await expect(button).toBeFocused();

  // Showing it opens nothing; clicking it asks Rust to open the OS page once.
  expect(await count("open_notification_settings")).toBe(0);
  await button.click();
  await expect.poll(() => count("open_notification_settings")).toBe(1);
  await expect(dialog.getByText(WORDS.en.openFailed)).toHaveCount(0);

  // The user allowed it in system settings and came back: line and button go.
  await setOsPermission(page, "granted");
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(button).toHaveCount(0);
  await expect(dialog.getByText(WORDS.en.blocked)).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("when the OS page cannot be opened, the blocked line says where to look instead", async ({
  page,
}) => {
  await boot(page, {
    settings: { desktopNotifications: true },
    permission: "denied",
    responses: { open_notification_settings: undefined },
  });
  const dialog = await openSettings(page);

  await dialog.getByRole("button", { name: WORDS.en.openSettings }).click();

  // Appended to the same polite status, which the switch is described by; the
  // button stays for another try.
  const status = dialog.locator(".settings-status");
  await expect(status).toContainText(WORDS.en.openFailed);
  await expect(status).toHaveAttribute("role", "status");
  await expect(dialog.getByRole("alert")).toHaveCount(0);
  await expect(dialog.getByRole("button", { name: WORDS.en.openSettings })).toBeEnabled();
});

test("turning the switch on asks the OS; opening Settings only reads, and a refusal there shows the blocked line", async ({
  page,
}) => {
  const errors = collectConsoleErrors(page);
  await boot(page);
  const dialog = await openSettings(page);
  const toggle = switchIn(dialog);
  const line = dialog.getByText(WORDS.en.blocked);
  const count = async (cmd: string) => (await invokes(page)).filter((c) => c.cmd === cmd).length;

  // Looking at the page must never raise the system prompt: it reads.
  await expect.poll(() => count("notification_permission")).toBeGreaterThan(0);
  expect(await count("request_notification_permission")).toBe(0);
  await expect(line).toHaveCount(0);

  // The user clicks the switch and answers "Don't Allow" at the system prompt.
  await setOsPermission(page, "denied");
  await toggle.check();

  await expect.poll(() => count("request_notification_permission")).toBe(1);
  await expect(line).toBeVisible();
  await expect(toggle).toBeChecked();

  // Turning it off again asks nothing more.
  await toggle.uncheck();
  await expect(line).toHaveCount(0);
  expect(await count("request_notification_permission")).toBe(1);
  expect(errors).toEqual([]);
});

test("the blocked line clears when the window comes back after the OS setting is fixed", async ({
  page,
}) => {
  await boot(page, { settings: { desktopNotifications: true }, permission: "denied" });
  const dialog = await openSettings(page);
  const line = dialog.getByText(WORDS.en.blocked);
  await expect(line).toBeVisible();

  // The user allowed OpenPal in system settings and switched back to it.
  await setOsPermission(page, "granted");
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));

  await expect(line).toHaveCount(0);
  await expect(switchIn(dialog)).toBeChecked();
});

test("a failed permission read says nothing and breaks nothing", async ({ page }) => {
  const errors = collectConsoleErrors(page);
  await boot(page, { settings: { desktopNotifications: true } });
  // From here the command rejects, as it would with no bridge behind it.
  await page.evaluate(() => {
    const internals = (
      window as unknown as {
        __TAURI_INTERNALS__: { invoke: (cmd: string, args?: unknown) => Promise<unknown> };
      }
    ).__TAURI_INTERNALS__;
    const before = internals.invoke;
    internals.invoke = (cmd, args) =>
      cmd === "notification_permission" ? Promise.reject(new Error("no bridge")) : before(cmd, args);
  });

  const dialog = await openSettings(page);

  await expect(switchIn(dialog)).toBeChecked();
  await expect(dialog.getByText(WORDS.en.blocked)).toHaveCount(0);
  await expect(dialog.getByRole("alert")).toHaveCount(0);
  expect(errors).toEqual([]);
});

/** A conversation an answer finished in, as the sidebar lists it and as
 * opening it returns it. */
const ANSWERED = {
  list_conversations: [
    {
      id: "c-answered",
      title: "Trip plan",
      createdAtMs: 1,
      updatedAtMs: 2,
      messageCount: 2,
      taskType: "general",
      pinned: false,
    },
  ],
  get_conversation: {
    id: "c-answered",
    title: "Trip plan",
    createdAtMs: 1,
    updatedAtMs: 2,
    messages: [
      { id: "m1", role: "user", content: "Plan a weekend in Lyon", createdAtMs: 1 },
      { id: "m2", role: "assistant", content: "Here is a two-day plan for Lyon.", createdAtMs: 2 },
    ],
    sampling: {
      temperature: 0.7,
      topP: 0.95,
      topK: 40,
      minP: 0.05,
      repeatPenalty: 1.1,
      repeatLastN: 64,
      presencePenalty: 0,
      frequencyPenalty: 0,
      maxTokens: -1,
      seed: -1,
    },
    contextLength: 8192,
    taskType: "general",
    pinned: false,
    branches: {},
  },
};

test("a click on an 'answer ready' notification that launched the app opens that answer once the app is up", async ({
  page,
}) => {
  await boot(page, {
    responses: { ...ANSWERED, take_notification_click: { conversationId: "c-answered" } },
    ready: (p) => p.getByText("Here is a two-day plan for Lyon."),
  });

  await expect(chatGreeting(page)).toHaveCount(0);
  const sent = await invokes(page);
  // Read once, at the end of boot, and the conversation it names is the one opened.
  expect(sent.filter((c) => c.cmd === "take_notification_click")).toHaveLength(1);
  expect(sent.filter((c) => c.cmd === "get_conversation").map((c) => c.args)).toEqual([
    { id: "c-answered" },
  ]);
});

test("with no click waiting, the app opens on the empty chat as always", async ({ page }) => {
  await boot(page, { responses: ANSWERED });

  const sent = await invokes(page);
  expect(sent.filter((c) => c.cmd === "take_notification_click")).toHaveLength(1);
  expect(sent.filter((c) => c.cmd === "get_conversation")).toHaveLength(0);
});

test("Settings has no notifications group on Android, and search doesn't offer one", async ({
  page,
}) => {
  await boot(page, {
    responses: {
      // What an Android build's `get_app_info` says: no notification backend.
      get_app_info: {
        portable: false,
        isAndroid: true,
        nativeNotifications: false,
        dataDir: "/tmp/openpal-e2e",
        version: "0.1.0",
      },
      get_engine_status: { type: "remoteUnpaired" },
    },
  });
  const dialog = await openSettings(page);

  await expect(dialog.getByRole("heading", { level: 1, name: "General" })).toBeVisible();
  // The neighbours are there; the group between them is not.
  await expect(dialog.getByRole("heading", { level: 2, name: "Appearance" })).toBeVisible();
  await expect(dialog.getByRole("heading", { level: 2, name: "Updates" })).toBeVisible();
  await expect(dialog.getByRole("heading", { level: 2, name: "Notifications" })).toHaveCount(0);
  await expect(switchIn(dialog)).toHaveCount(0);

  const nav = dialog.getByRole("navigation", { name: "More" });
  await nav.getByRole("textbox", { name: "Search settings" }).fill("desktop notifications");
  await expect(nav.getByRole("button", { name: /Desktop notifications/ })).toHaveCount(0);
  await expect(nav.getByRole("status")).toHaveText(/^No results for/);
});

test.describe("in French", () => {
  test.use({ locale: "fr-FR" });

  test("the group, the switch and the blocked line are all French", async ({ page }) => {
    const words = WORDS.fr;
    await boot(page, {
      settings: { language: "fr", desktopNotifications: true },
      permission: "denied",
    });
    const dialog = await openSettings(page, words);

    await expect(dialog.getByRole("heading", { level: 2, name: words.group })).toBeVisible();
    await expect(switchIn(dialog, words.label)).toBeChecked();
    await expect(dialog.getByText(words.blocked)).toBeVisible();
    await expect(dialog.getByRole("button", { name: words.openSettings })).toBeVisible();
    // Nothing of the English is left on the page.
    await expect(dialog.getByText(WORDS.en.label)).toHaveCount(0);
    await expect(dialog.getByText(WORDS.en.blocked)).toHaveCount(0);
    await expect(dialog.getByRole("button", { name: WORDS.en.openSettings })).toHaveCount(0);
  });
});
