import { expect, test, type Locator, type Page } from "@playwright/test";
import { mockTauriIpc } from "./support/tauriMock";
import { freshInstallResponses } from "./support/fixtures";

async function openApp(
  page: Page,
  overrides: Record<string, unknown> = {},
  { stubChannels = false } = {},
) {
  const base = freshInstallResponses();
  await mockTauriIpc(page, {
    ...base,
    get_settings: {
      ...(base.get_settings as Record<string, unknown>),
      onboardingCompleted: true,
      userName: "Ada",
    },
    update_settings: null,
    ...overrides,
  });
  if (stubChannels) {
    // tauriMock deliberately has no Channel support (see its doc comment);
    // a page that merely opens one on mount only needs the constructor's
    // callback registration not to throw.
    await page.addInitScript(() => {
      (
        window as unknown as { __TAURI_INTERNALS__: { transformCallback: () => number } }
      ).__TAURI_INTERNALS__.transformCallback = () => 0;
    });
  }
  await page.goto("/");
  await expect(chatGreeting(page)).toBeVisible();
}

/** The empty chat's greeting — present for as long as the chat is mounted. */
const chatGreeting = (page: Page) => page.getByRole("heading", { name: /Hi, Ada/ });

async function openFromMoreMenu(page: Page, item: string) {
  await page.getByRole("button", { name: "More", exact: true }).click();
  await page.getByRole("button", { name: item, exact: true }).click();
  return page.getByRole("dialog", { name: item });
}

test("More menu pages open in one large dialog over the chat, and close back onto it", async ({
  page,
}) => {
  await openApp(page);
  const composer = page.getByRole("textbox", { name: "Message" });
  await composer.fill("half-written thought");

  const dialog = await openFromMoreMenu(page, "Skills");
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("heading", { level: 1, name: "Skills" })).toBeVisible();
  // The chat underneath is still there, not swapped out for the page.
  await expect(chatGreeting(page)).toBeAttached();

  // Most of the window, in both directions. Layout size, not the bounding
  // box, which is still scaled while the dialog's entrance animation runs.
  const [width, height] = await dialog.evaluate((el: HTMLElement) => [el.offsetWidth, el.offsetHeight]);
  const viewport = page.viewportSize()!;
  expect(width / viewport.width).toBeGreaterThan(0.9);
  expect(height / viewport.height).toBeGreaterThan(0.9);

  // The dialog's accessible name follows the page, so don't scope to it here.
  const nav = page.getByRole("dialog").getByRole("navigation", { name: "More" });
  await nav.getByRole("button", { name: "Models" }).click();
  await expect(page.getByRole("dialog", { name: "Models" })).toBeVisible();
  await expect(nav.getByRole("button", { name: "Models" })).toHaveAttribute("aria-current", "page");

  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(chatGreeting(page)).toBeVisible();
  await expect(page.getByRole("button", { name: "More", exact: true })).toBeFocused();
  // The unsent draft survived the round trip — the chat never unmounted.
  await expect(composer).toHaveValue("half-written thought");
});

test("Settings is a page of the same dialog, and its Done button closes it", async ({ page }) => {
  await openApp(page);

  const dialog = await openFromMoreMenu(page, "Settings");
  // It opens on its first category, listed under a Settings heading in the nav.
  await expect(dialog.getByRole("heading", { level: 1, name: "General" })).toBeVisible();
  const categories = dialog
    .getByRole("navigation", { name: "More" })
    .getByRole("list", { name: "Settings" });
  await expect(categories.getByRole("button", { name: "General" })).toHaveAttribute(
    "aria-current",
    "page",
  );

  await dialog.getByRole("button", { name: "Done" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(chatGreeting(page)).toBeVisible();
});

test("every Settings category shows its own settings, and only those", async ({ page }) => {
  await openApp(page);
  const dialog = await openFromMoreMenu(page, "Settings");
  const categories = dialog
    .getByRole("navigation", { name: "More" })
    .getByRole("list", { name: "Settings" });
  const groups = (names: string[]) =>
    names.map((name) => dialog.getByRole("heading", { level: 2, name }));
  const text = (t: string) => dialog.getByText(t, { exact: true });

  const expected: [string, Locator[]][] = [
    ["General", [text("Language"), ...groups(["Updates"])]],
    ["Chat", [dialog.getByRole("textbox", { name: /Personality/ }), text("Smart tuning")]],
    ["AI model", [text("Smart model loading"), text("Add your own models"), ...groups(["Performance"])]],
    ["Privacy & safety", [text("Crisis support banner"), ...groups(["App lock", "Parental controls"])]],
    ["Devices", groups(["Sync", "Remote access"])],
    ["Your data", [text("Export your data"), ...groups(["Danger zone"])]],
    ["Advanced", groups(["Agent tools", "Experimental features", "Logs & diagnostics"])],
  ];
  for (const [category, shown] of expected) {
    await categories.getByRole("button", { name: category }).click();
    await expect(dialog.getByRole("heading", { level: 1, name: category })).toBeVisible();
    for (const locator of shown) await expect(locator).toBeVisible();
  }
  // Nothing from one category leaks onto another's page.
  await expect(dialog.getByRole("textbox", { name: /Personality/ })).toHaveCount(0);
  await expect(dialog.getByRole("heading", { level: 2, name: "App lock" })).toHaveCount(0);
});

test("the nav's search finds a setting and opens its category right on it", async ({ page }) => {
  await openApp(page);
  await openFromMoreMenu(page, "Skills");
  const dialog = page.getByRole("dialog");
  const nav = dialog.getByRole("navigation", { name: "More" });
  const search = nav.getByRole("textbox", { name: "Search settings" });

  // "pin" is only in App lock's hint, not its name.
  await search.fill("pin");
  await expect(nav.getByRole("list", { name: "Settings" })).toHaveCount(0);
  await nav.getByRole("button", { name: "App lock Privacy & safety" }).click();

  await expect(page.getByRole("dialog", { name: "Settings" })).toBeVisible();
  await expect(dialog.getByRole("heading", { level: 1, name: "Privacy & safety" })).toBeVisible();
  const appLock = dialog.getByRole("region", { name: "App lock" });
  await expect(appLock).toHaveClass(/settings-search-hit/);
  await expect(appLock).toBeInViewport();
  await expect(search).toHaveValue("");
  await expect(nav.getByRole("button", { name: "Privacy & safety" })).toHaveAttribute(
    "aria-current",
    "page",
  );

  // Escape empties a search first, and only then closes the dialog.
  await search.fill("language");
  await search.press("Escape");
  await expect(search).toHaveValue("");
  await expect(dialog).toBeVisible();
});

test("at phone width the search sits above the pages and its results fit", async ({ page }) => {
  await openApp(page);
  await openFromMoreMenu(page, "Skills");
  await page.setViewportSize({ width: 390, height: 844 });
  const nav = page.getByRole("navigation", { name: "More" });
  const search = nav.getByRole("textbox", { name: "Search settings" });

  await expect(search).toBeInViewport();
  await search.fill("sync");
  const result = nav.getByRole("button", { name: "Sync conversations Devices" });
  await expect(result).toBeInViewport();
  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
  );
  expect(overflow).toBeLessThanOrEqual(0);

  await result.click();
  await expect(page.getByRole("heading", { level: 1, name: "Devices" })).toBeVisible();
  await expect(page.getByText("Sync conversations", { exact: true })).toBeInViewport();
});

test("Settings keeps what was typed when leaving it for another page, or closing", async ({
  page,
}) => {
  await openApp(page);
  const personality = () => page.getByRole("textbox", { name: /Personality/ });

  await openFromMoreMenu(page, "Settings");
  const nav = page.getByRole("navigation", { name: "More" });
  await nav.getByRole("button", { name: "Chat" }).click();
  await personality().fill("Be brief.");
  // Another Settings category and back, then another page and back.
  await nav.getByRole("button", { name: "General" }).click();
  await nav.getByRole("button", { name: "Chat" }).click();
  await expect(personality()).toHaveValue("Be brief.");
  await nav.getByRole("button", { name: "Skills" }).click();
  await nav.getByRole("button", { name: "Chat" }).click();
  await expect(personality()).toHaveValue("Be brief.");

  await personality().fill("Be brief and kind.");
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await openFromMoreMenu(page, "Settings");
  await nav.getByRole("button", { name: "Chat" }).click();
  await expect(personality()).toHaveValue("Be brief and kind.");
});

test("Settings' name field changes the chat's greeting once Settings closes", async ({ page }) => {
  await openApp(page); // saved name: Ada
  const name = () => page.getByRole("textbox", { name: "Your name" });

  await openFromMoreMenu(page, "Settings");
  const nav = page.getByRole("navigation", { name: "More" });
  await nav.getByRole("button", { name: "Chat" }).click();
  await expect(name()).toHaveValue("Ada");

  // Typed, then straight out through Done without leaving the field first.
  await name().fill("  Grace ");
  await page.getByRole("dialog").getByRole("button", { name: "Done" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(
    page.getByRole("heading", { name: "Hi, Grace. What can I help with?" }),
  ).toBeVisible();

  // Emptied: the generic greeting is back, and the field is empty next time.
  await openFromMoreMenu(page, "Settings");
  await nav.getByRole("button", { name: "Chat" }).click();
  await expect(name()).toHaveValue("Grace");
  await name().fill("");
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "What can I help with?" })).toBeVisible();
  await expect(page.getByRole("heading", { name: /^Hi, / })).toHaveCount(0);
  await openFromMoreMenu(page, "Settings");
  await nav.getByRole("button", { name: "Chat" }).click();
  await expect(name()).toHaveValue("");
});

test("the sidebar's 'No model installed' pill opens the dialog on Models", async ({ page }) => {
  await openApp(page);

  await page.getByRole("button", { name: /No model installed/ }).click();

  const dialog = page.getByRole("dialog", { name: "Models" });
  await expect(dialog.getByRole("heading", { level: 1, name: "Models" })).toBeVisible();
  await expect(chatGreeting(page)).toBeAttached();
});

test("the Android pairing pill opens the dialog on Settings' Devices", async ({ page }) => {
  // Devices on Android shows the remote-pairing section, which opens a
  // discovery Channel on mount.
  await openApp(
    page,
    {
      get_app_info: { portable: false, isAndroid: true, dataDir: "/tmp/openpal-e2e", version: "0.1.0" },
      get_engine_status: { type: "remoteUnpaired" },
    },
    { stubChannels: true },
  );

  await page.getByRole("button", { name: /Pair a desktop to chat/ }).click();

  const dialog = page.getByRole("dialog", { name: "Settings" });
  await expect(dialog.getByRole("heading", { level: 1, name: "Devices" })).toBeVisible();
  await expect(dialog.getByRole("heading", { level: 2, name: "Remote pairing" })).toBeVisible();
});

const HF_FILE = { path: "tiny-q4.gguf", sizeBytes: 1_000_000, quant: "Q4_K_M", sha256: null };

test("Settings' 'Add your own models' switch puts the add form at the bottom of Models", async ({
  page,
}) => {
  await openApp(page, {
    list_hf_models: { repoId: "org/tiny-GGUF", revision: "main", files: [HF_FILE], preselectPath: null },
    add_hf_model: "org/tiny-GGUF/tiny-q4.gguf",
  });
  await openFromMoreMenu(page, "Models");
  // The dialog's accessible name follows the page, so don't scope to it here.
  const dialog = page.getByRole("dialog");
  const nav = dialog.getByRole("navigation", { name: "More" });
  const addModel = dialog.getByRole("heading", { level: 2, name: "Add your own model" });
  // Off by default: the beginner path is the curated picks.
  await expect(dialog.getByRole("heading", { level: 1, name: "Models" })).toBeVisible();
  await expect(addModel).toHaveCount(0);

  await nav.getByRole("button", { name: "AI model" }).click();
  const toggle = dialog
    .locator(".settings-field", { hasText: "Add your own models" })
    .getByRole("checkbox");
  await expect(toggle).not.toBeChecked();
  await toggle.check();

  await nav.getByRole("button", { name: "Models" }).click();
  await expect(addModel).toBeVisible();
  // The page's last section.
  await expect(dialog.locator(".library-col > :last-child")).toContainText("Add your own model");
  await dialog.getByPlaceholder("huggingface.co/TheOrg/TheModel-GGUF").fill("org/tiny-GGUF");
  await dialog.getByRole("button", { name: "Find files" }).click();
  await dialog.getByRole("button", { name: "Add", exact: true }).click();
  await expect(dialog.getByText("✓ Added")).toBeVisible();
});

test("Browse's 'see it in your library' link switches the dialog to Models", async ({ page }) => {
  const gguf = { totalParams: 1_000_000_000, architecture: "llama", contextLength: 4096 };
  const summary = {
    repoId: "org/tiny-GGUF",
    author: "org",
    pipelineTag: "text-generation",
    libraryName: "gguf",
    downloads: 10,
    likes: 1,
    license: "mit",
    gated: false,
    lastModified: null,
    gguf,
    fit: "perfect",
    fitWhy: "Plenty of room.",
  };
  await openApp(page, {
    list_plugins: [
      {
        id: "hf-browser",
        name: "Hugging Face browser",
        description: "",
        version: "1.0.0",
        author: "",
        capabilities: ["model-browser"],
        enabled: true,
        setup: { type: "ready" },
        service: { type: "notApplicable" },
        hasChat: false,
        experimental: false,
      },
    ],
    search_hf_hub: { models: [summary], nextCursor: null },
    get_hf_model_details: { ...summary, createdAt: null, tags: [] },
    get_hf_quant_picker: {
      repoId: "org/tiny-GGUF",
      revision: "main",
      options: [{ file: HF_FILE, fit: "perfect", why: "Plenty of room.", paramsB: 1, moe: false, precise: true }],
      preselectPath: null,
    },
    get_hf_model_readme: "",
    add_hf_model: "org/tiny-GGUF/tiny-q4.gguf",
  });
  const dialog = await openFromMoreMenu(page, "Browse");

  await dialog.getByRole("button", { name: /tiny-GGUF/ }).first().click();
  await dialog.getByRole("button", { name: "Add to library" }).click();
  await dialog.getByRole("button", { name: /See it in your model library/ }).click();

  await expect(page.getByRole("dialog", { name: "Models" })).toBeVisible();
});

test("Escape in a dialog stacked on a page closes only that dialog", async ({ page }) => {
  await openApp(page);
  await openFromMoreMenu(page, "Skills");

  await page.getByRole("button", { name: "New skill" }).click();
  const editor = page.getByRole("dialog", { name: "New skill" });
  await expect(editor).toBeVisible();
  await page.keyboard.press("Escape");

  await expect(editor).toHaveCount(0);
  await expect(page.getByRole("dialog", { name: "Skills" })).toBeVisible();
  await expect(page.getByRole("button", { name: "New skill" })).toBeFocused();
});

test("at phone width the dialog fits the screen and every page stays reachable", async ({
  page,
}) => {
  await openApp(page);
  // The More menu lives in the sidebar, which is a drawer below the mobile
  // breakpoint — open the dialog first, then narrow the window under it.
  await openFromMoreMenu(page, "Skills");
  await page.setViewportSize({ width: 390, height: 844 });

  // Settings' first and last categories bracket the rest of its entries,
  // which sit at the far end of the nav's sideways-scrolling row.
  for (const [entry, name] of [
    ["Models", "Models"],
    ["Skills", "Skills"],
    ["General", "Settings"],
    ["Advanced", "Settings"],
  ]) {
    const nav = page.getByRole("navigation", { name: "More" });
    await nav.getByRole("button", { name: entry }).click();
    const dialog = page.getByRole("dialog", { name });
    await expect(dialog).toBeVisible();
    await expect(dialog.getByRole("heading", { level: 1, name: entry })).toBeVisible();

    const box = (await dialog.boundingBox())!;
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(390);
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
    );
    expect(overflow, `the page scrolls sideways on ${entry}`).toBeLessThanOrEqual(0);
  }

  // Search, too — its own dialog, fitted the same way.
  await page.keyboard.press("Escape");
  await page.setViewportSize({ width: 1100, height: 740 });
  await page.getByRole("button", { name: "Search conversations" }).click();
  await page.setViewportSize({ width: 390, height: 844 });
  const search = page.getByRole("dialog", { name: "Search conversations" });
  const box = (await search.boundingBox())!;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(390);
  await expect(search.getByRole("button", { name: "Close" })).toBeInViewport();
});

test("conversation search opens in its own dialog, without the More dialog's nav", async ({
  page,
}) => {
  await openApp(page);

  const trigger = page.getByRole("button", { name: "Search conversations" });
  await trigger.click();
  const dialog = page.getByRole("dialog", { name: "Search conversations" });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("textbox")).toBeFocused();
  await expect(dialog.getByRole("navigation")).toHaveCount(0);
  await expect(chatGreeting(page)).toBeAttached();

  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(chatGreeting(page)).toBeVisible();
  await expect(trigger).toBeFocused();
});
