import { expect, test, type Page } from "@playwright/test";
import { mockTauriIpc } from "./support/tauriMock";
import { freshInstallResponses } from "./support/fixtures";

/** The app shell after onboarding finishes early: the empty chat composer
 * (rendered unconditionally by ChatView, with no model or conversation). */
const shell = (page: Page) => page.getByRole("textbox", { name: "Message" });

/** Skipping the whole setup asks first, since it leaves the app with no model. */
const skipDialog = (page: Page) => page.getByRole("alertdialog", { name: "Skip setup?" });
const confirmSkip = (page: Page) =>
  skipDialog(page).getByRole("button", { name: "Skip setup" }).click();
const skipStep = (page: Page) => page.getByRole("button", { name: "Skip this step" }).click();
const skipSetup = (page: Page) => page.getByRole("button", { name: "Skip setup" }).click();

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
 * genuinely in flight — since the plain request/response mock can't stream. */
async function bootFreshInstall(page: Page) {
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
  });
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

  await expect(
    page.getByRole("heading", { name: "First, let's see what your Mac can do." }),
  ).toBeVisible();
  await expect(skipDialog(page)).toHaveCount(0);
});

test("scan scene: Skip this step skips the model setup, through to the final screen", async ({
  page,
}) => {
  await bootFreshInstall(page);

  await page.getByRole("button", { name: "Let's set things up" }).click();
  await skipStep(page); // the name step
  await expect(
    page.getByRole("heading", { name: "First, let's see what your Mac can do." }),
  ).toBeVisible();
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

  await page.getByRole("button", { name: "Let's set things up" }).click();
  await skipStep(page);
  await expect(
    page.getByRole("heading", { name: "First, let's see what your Mac can do." }),
  ).toBeVisible();

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

  await page.getByRole("button", { name: "Let's set things up" }).click();
  await skipStep(page);
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

  await page.getByRole("button", { name: "Let's set things up" }).click();
  await skipStep(page);
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

  await page.getByRole("button", { name: "Let's set things up" }).click();
  await skipStep(page);
  await skipStep(page);
  await expect(page.getByRole("heading", { name: "Add a PIN?" })).toBeVisible();

  await skipSetup(page);
  await expect(skipDialog(page)).toBeVisible();
  await confirmSkip(page);

  await expect(page.getByRole("heading", { name: "What can I help with?" })).toBeVisible();
  await expect(shell(page)).toBeVisible();
});
