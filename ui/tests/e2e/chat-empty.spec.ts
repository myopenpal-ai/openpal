import { expect, test, type Page } from "@playwright/test";
import { mockTauriIpc } from "./support/tauriMock";
import { freshInstallResponses } from "./support/fixtures";

/** Opens the app on a fresh, empty chat with a model "running", so the
 * composer can send. `chat_stream` resolves at once — what the reply does is
 * not under test, only where the composer is when the first message lands. */
async function openEmptyChat(page: Page, responses: Record<string, unknown> = {}) {
  const base = freshInstallResponses();
  await mockTauriIpc(page, {
    ...base,
    get_settings: {
      ...(base.get_settings as Record<string, unknown>),
      onboardingCompleted: true,
      userName: "Ada",
    },
    get_engine_status: {
      type: "running",
      data: { modelId: "tiny", port: 8080, diskSlow: false, autoUnloadAt: null },
    },
    update_settings: null,
    chat_stream: null,
    ...responses,
  });
  await page.addInitScript(() => {
    // `chat_stream` takes a Channel, which the IPC mock has no wire protocol
    // for — see tauriMock.ts. Only its constructor's registration is needed.
    (
      window as unknown as { __TAURI_INTERNALS__: { transformCallback: () => number } }
    ).__TAURI_INTERNALS__.transformCallback = () => 0;
    // Record every Web Animations call on the composer's slot, so a test can
    // tell a glide from a jump without racing a 450ms animation.
    const glides: { from: string; to: string }[] = [];
    (window as unknown as { __composerGlides: typeof glides }).__composerGlides = glides;
    const animate = Element.prototype.animate;
    Element.prototype.animate = function (this: Element, keyframes, options) {
      if (this.classList.contains("chat-composer-slot")) {
        const [from, to] = keyframes as Keyframe[];
        glides.push({ from: String(from.transform), to: String(to.transform) });
      }
      return animate.call(this, keyframes, options);
    };
  });
  await page.goto("/");
  await expect(page.getByRole("heading", { name: /Hi, Ada/ })).toBeVisible();
}

const glides = (page: Page) =>
  page.evaluate(
    () => (window as unknown as { __composerGlides: { from: string; to: string }[] }).__composerGlides,
  );

/** One saved chat, as the sidebar lists it and as opening it returns it. */
const SAVED_CHAT = {
  list_conversations: [
    {
      id: "c-old",
      title: "Old chat",
      createdAtMs: 1,
      updatedAtMs: 2,
      messageCount: 2,
      taskType: "general",
      pinned: false,
    },
  ],
  get_conversation: {
    id: "c-old",
    title: "Old chat",
    createdAtMs: 1,
    updatedAtMs: 2,
    messages: [
      { id: "m1", role: "user", content: "An earlier question", createdAtMs: 1 },
      { id: "m2", role: "assistant", content: "An earlier answer", createdAtMs: 2 },
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

/** How far the box you type in sits from the middle of the window, in px
 * (negative: above it). The chat fills the window top to bottom. */
async function composerOffsetFromMiddle(page: Page) {
  const box = await page.locator(".composer").boundingBox();
  return box!.y + box!.height / 2 - page.viewportSize()!.height / 2;
}

/** The composer's box, relative to the viewport. */
async function composerBox(page: Page) {
  const box = await page.locator(".chat-composer-slot").boundingBox();
  const viewport = page.viewportSize()!;
  return { top: box!.y, bottom: box!.y + box!.height, viewportHeight: viewport.height };
}

test("an empty chat has no canned prompts, and the box you type in sits in the middle of the window", async ({
  page,
}) => {
  await openEmptyChat(page);

  // Nothing to click in the chat but the composer's own controls: no canned
  // prompts anywhere, whatever they might be called.
  await expect(page.locator(".chat.is-empty")).toBeVisible();
  await expect(page.locator(".chat").getByRole("button")).toHaveCount(
    await page.locator(".chat-composer-slot").getByRole("button").count(),
  );

  // The input itself is what's centred, not the greeting-and-input group, whose
  // middle would leave the input well below the window's.
  expect(Math.abs(await composerOffsetFromMiddle(page))).toBeLessThan(6);

  // With the greeting directly above it, not stranded far away.
  const { top } = await composerBox(page);
  const heading = await page.getByRole("heading", { name: /Hi, Ada/ }).boundingBox();
  expect(heading!.y + heading!.height).toBeLessThan(top);
  expect(top - (heading!.y + heading!.height)).toBeLessThan(120);
});

test("a banner under the composer leaves it exactly where it was", async ({ page, context }) => {
  await openEmptyChat(page);
  const without = await composerOffsetFromMiddle(page);

  const failed = await context.newPage();
  await openEmptyChat(failed, {
    get_engine_status: { type: "failed", data: { message: "The model could not be loaded." } },
  });
  await expect(failed.getByRole("alert")).toBeVisible();
  const withBanner = await composerOffsetFromMiddle(failed);

  expect(Math.abs(withBanner - without)).toBeLessThan(1);
  expect(Math.abs(withBanner)).toBeLessThan(6);
  // The banner is a full row of the chat, under the composer, not squeezed in beside it.
  const banner = await failed.getByRole("alert").boundingBox();
  const composer = await failed.locator(".composer").boundingBox();
  expect(banner!.y).toBeGreaterThan(composer!.y + composer!.height);
  expect(banner!.width).toBeGreaterThanOrEqual(composer!.width);
});

test("the first message glides the composer to the bottom, keeping focus", async ({ page }) => {
  await openEmptyChat(page);
  const composer = page.getByRole("textbox", { name: "Message" });
  await composer.fill("Hello there");
  await composer.press("Enter");

  await expect(page.getByText("Hello there")).toBeVisible();
  await expect(page.locator(".chat")).not.toHaveClass(/is-empty/);

  // It moved down by gliding — an animation that starts where the composer
  // was (above where it now is) and ends at rest — and ended up at the bottom.
  const [glide] = await glides(page);
  expect(glide.to).toBe("none");
  expect(glide.from).toMatch(/^translateY\(-\d/);
  await expect
    .poll(async () => {
      const { bottom, viewportHeight } = await composerBox(page);
      return viewportHeight - bottom;
    })
    .toBeLessThan(40);

  // Same composer, never remounted: focus carried straight on to the next message.
  await expect(composer).toBeFocused();
  await expect(composer).toHaveValue("");
  expect(await glides(page)).toHaveLength(1);
});

test("with reduced motion the composer goes straight to the bottom, with no frame in between", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await openEmptyChat(page);
  const composer = page.getByRole("textbox", { name: "Message" });
  await composer.fill("Hello there");
  const start = (await composerBox(page)).top;
  // Where the composer is on every frame from here on. The OS-level
  // reduced-motion rule gives every element a 0.01ms transition, which would
  // otherwise let the swap between the two layouts paint a frame of neither.
  await page.evaluate(() => {
    const tops: number[] = [];
    (window as unknown as { __composerTops: number[] }).__composerTops = tops;
    const sample = () => {
      const slot = document.querySelector(".chat-composer-slot");
      if (slot) tops.push(slot.getBoundingClientRect().top);
      requestAnimationFrame(sample);
    };
    sample();
  });
  await composer.press("Enter");

  await expect(page.getByText("Hello there")).toBeVisible();
  await expect
    .poll(async () => {
      const { bottom, viewportHeight } = await composerBox(page);
      return viewportHeight - bottom;
    })
    .toBeLessThan(40);
  const end = (await composerBox(page)).top;
  const tops = await page.evaluate(
    () => (window as unknown as { __composerTops: number[] }).__composerTops,
  );
  expect(tops.length).toBeGreaterThan(1);
  for (const top of tops) {
    expect(Math.min(Math.abs(top - start), Math.abs(top - end))).toBeLessThan(1);
  }
  expect(await glides(page)).toEqual([]);
});

test("opening a saved chat from the empty screen at launch swaps it in without gliding", async ({
  page,
}) => {
  await openEmptyChat(page, SAVED_CHAT);

  await page.getByRole("button", { name: "Old chat", exact: true }).click();

  await expect(page.getByText("An earlier answer")).toBeVisible();
  await expect(page.locator(".chat")).not.toHaveClass(/is-empty/);
  expect(await glides(page)).toEqual([]);
});

test("an empty chat fits a short window: greeting and composer both stay in view", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 480 });
  await openEmptyChat(page);

  await expect(page.getByRole("heading", { name: /Hi, Ada/ })).toBeInViewport();
  await expect(page.getByRole("textbox", { name: "Message" })).toBeInViewport();
  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
  );
  expect(overflow).toBeLessThanOrEqual(0);
});
