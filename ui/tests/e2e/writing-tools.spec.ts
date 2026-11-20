import { expect, test, type Page } from "@playwright/test";
import { freshInstallResponses } from "./support/fixtures";
import { persistSettings } from "./support/persistedSettings";
import { mockTauriIpc } from "./support/tauriMock";

// Writing Tools only appears with a plugin that advertises the capability —
// the same fixture theme.spec.ts uses for the Translate screen.
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

/** An onboarded install with the Writing Tools plugin, optionally remembering
 * `update_settings` so a reload behaves like a relaunch. */
async function boot(page: Page, { persist = false } = {}) {
  const base = freshInstallResponses();
  await mockTauriIpc(page, {
    ...base,
    get_settings: { ...(base.get_settings as object), onboardingCompleted: true, userName: "Ada" },
    update_settings: null,
    "plugin:app|version": "0.1.0",
    list_plugins: [WRITING_TOOLS_PLUGIN],
  } as Record<string, unknown>);
  if (persist) await persistSettings(page);
  await page.goto("/");
  await expect(page.getByRole("button", { name: "More", exact: true })).toBeVisible();
}

test("Improve takes exactly one tone and remembers Custom instructions across a relaunch", async ({
  page,
}) => {
  await boot(page, { persist: true });
  await page.getByRole("button", { name: "Improve a message" }).click();

  // Natural is the default, and is the only checked chip.
  const natural = page.getByRole("radio", { name: "Natural" });
  await expect(natural).toBeChecked();
  await expect(page.getByRole("radio", { name: "Professional" })).not.toBeChecked();
  await expect(page.getByLabel("Your instructions")).toHaveCount(0);

  // Picking Custom (the chip itself, as a pointer reaches it) replaces the
  // choice and reveals the one free-text field.
  await page.getByRole("radio", { name: "Custom" }).click();
  await expect(page.getByRole("radio", { name: "Custom" })).toBeChecked();
  await expect(natural).not.toBeChecked();
  await expect(page.getByRole("radio", { name: "Direct" })).not.toBeChecked();

  const instructions = page.getByLabel("Your instructions");
  await instructions.fill("Make it warmer and shorter.");
  // Blur is what persists; clicking the message box is the ordinary way there.
  await page.getByLabel("Your message").click();
  // Speed up the next assertion: wait for the write to reach the mocked
  // backend (persistedSettings.ts's localStorage stand-in) rather than reloading
  // before the save has landed.
  await page.waitForFunction(
    () =>
      JSON.parse(localStorage.getItem("__e2e_persisted_settings") ?? "{}")
        .improveToneInstructions === "Make it warmer and shorter.",
  );

  await page.reload();
  await expect(page.getByRole("button", { name: "More", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Improve a message" }).click();
  // The tone choice itself is not persisted, but the instructions must be —
  // one value, exactly as typed.
  await expect(page.getByRole("radio", { name: "Natural" })).toBeChecked();
  await page.getByRole("radio", { name: "Custom" }).click();
  await expect(page.getByLabel("Your instructions")).toHaveValue("Make it warmer and shorter.");
});

test("Correct is one editable surface, and its painted mirror matches the textarea", async ({
  page,
}) => {
  await boot(page);
  await page.getByRole("button", { name: "Correct", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Correct" })).toBeVisible();

  // One textbox: the user's own text. No duplicate original box, no result
  // pane, and the page title carries its sidebar drawing decoratively.
  await expect(page.getByRole("textbox")).toHaveCount(1);
  await expect(
    page.getByRole("heading", { name: "Correct" }).locator("svg[aria-hidden='true']"),
  ).toHaveCount(1);

  const clear = page.getByRole("button", { name: "Clear" });
  await expect(clear).toBeDisabled();
  await expect(clear).toHaveAttribute("title", "Clear");

  const text = "Thanks, you'r nise and the cat sat on the mat.";
  await page.getByRole("textbox", { name: "Your text" }).fill(text);

  // The mirror is what actually paints the text, over a transparent-text
  // textarea. If the two ever drift apart (font, padding, border), the marks
  // land beside their words instead of on them, so pin the metrics here.
  const painted = await page.evaluate(() => {
    const textarea = document.querySelector<HTMLTextAreaElement>("#writing-tools-source")!;
    const mirror = document.querySelector<HTMLElement>(".writing-tools-editor-mirror")!;
    const read = (el: Element) => {
      const style = getComputedStyle(el);
      return {
        fontFamily: style.fontFamily,
        fontSize: style.fontSize,
        lineHeight: style.lineHeight,
        letterSpacing: style.letterSpacing,
        padding: style.padding,
        border: style.borderWidth,
      };
    };
    const box = (el: Element) => {
      const r = el.getBoundingClientRect();
      return { x: r.x, y: r.y, width: r.width, height: r.height };
    };
    return {
      value: textarea.value,
      mirrorText: mirror.textContent,
      textareaStyle: read(textarea),
      mirrorStyle: read(mirror),
      textareaBox: box(textarea),
      mirrorBox: box(mirror),
      textareaColor: getComputedStyle(textarea).color,
    };
  });
  expect(painted.mirrorText).toBe(painted.value);
  expect(painted.mirrorStyle).toEqual(painted.textareaStyle);
  for (const key of ["x", "y", "width", "height"] as const) {
    expect(Math.abs(painted.textareaBox[key] - painted.mirrorBox[key])).toBeLessThan(1);
  }
  // The textarea's own glyphs are hidden; the mirror is what shows them.
  expect(painted.textareaColor).toBe("rgba(0, 0, 0, 0)");

  // Clear survives the marked surface's rewrite and empties it.
  await expect(clear).toBeEnabled();
  await clear.click();
  await expect(page.getByRole("textbox", { name: "Your text" })).toHaveValue("");
});
