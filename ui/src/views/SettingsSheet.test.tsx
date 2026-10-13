import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import i18n from "../i18n";
import SettingsSheet, { ExperimentalSection } from "./SettingsSheet";
import { useStore } from "../state/store";
import type { PluginInfo, Settings } from "../lib/types";

const { updateSettings } = vi.hoisted(() => ({ updateSettings: vi.fn() }));

// The one call the pages rendered here may make is the write behind the theme
// picker (the store's `setTheme`); any other still throws.
vi.mock("../lib/api", () => ({ api: { updateSettings } }));

function plugin(overrides: Partial<PluginInfo> & { id: string; name: string }): PluginInfo {
  return {
    description: "",
    version: "0.1.0",
    author: "OpenPal",
    capabilities: [],
    enabled: false,
    setup: { type: "ready" },
    service: { type: "notApplicable" },
    hasChat: false,
    experimental: true,
    ...overrides,
  };
}

// The store's own, before the tests below swap a mock in for it.
const realSaveSettings = useStore.getState().saveSettings;
const saveSettings = vi.fn().mockResolvedValue(undefined);

function renderSection(experimentalFeatures: boolean, plugins: PluginInfo[]) {
  useStore.setState({
    settings: { experimentalFeatures } as Settings,
    plugins,
    saveSettings,
  });
  render(<ExperimentalSection />);
}

describe("ExperimentalSection", () => {
  beforeEach(() => {
    saveSettings.mockClear();
  });

  it("turns experimental features on without asking", async () => {
    renderSection(false, []);
    await userEvent.click(screen.getByRole("checkbox", { name: "Enable experimental features" }));
    expect(saveSettings).toHaveBeenCalledWith({ experimentalFeatures: true });
  });

  it("turns them off without a dialog when no plugin is on", async () => {
    renderSection(true, [plugin({ id: "hf-browser", name: "Model Browser" })]);
    await userEvent.click(screen.getByRole("checkbox", { name: "Enable experimental features" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(saveSettings).toHaveBeenCalledWith({ experimentalFeatures: false });
  });

  it("turns them off without a dialog when only non-experimental plugins are on", async () => {
    renderSection(true, [
      plugin({ id: "stable", name: "Stable", enabled: true, experimental: false }),
    ]);
    await userEvent.click(screen.getByRole("checkbox", { name: "Enable experimental features" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(saveSettings).toHaveBeenCalledWith({ experimentalFeatures: false });
  });

  it("asks first, naming every enabled experimental plugin, when some are on", async () => {
    renderSection(true, [
      plugin({ id: "speech", name: "Voice", enabled: true }),
      plugin({ id: "hf-browser", name: "Model Browser" }),
      plugin({ id: "stable", name: "Stable", enabled: true, experimental: false }),
      plugin({ id: "smart-model-router", name: "Smart Model Router", enabled: true }),
    ]);
    const toggle = screen.getByRole("checkbox", { name: "Enable experimental features" });

    await userEvent.click(toggle);
    const dialog = screen.getByRole("alertdialog", { name: "Turn off experimental features?" });
    expect(dialog.textContent).toContain("Voice, Smart Model Router");
    expect(dialog.textContent).not.toContain("Model Browser");
    expect(dialog.textContent).not.toContain("Stable");
    expect(saveSettings).not.toHaveBeenCalled();

    // Cancel leaves the setting on.
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(toggle).toBeChecked();
    expect(saveSettings).not.toHaveBeenCalled();

    await userEvent.click(toggle);
    await userEvent.click(screen.getByRole("button", { name: "Turn off" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(saveSettings).toHaveBeenCalledWith({ experimentalFeatures: false });
  });
});

describe("Chat settings: your name", () => {
  beforeEach(() => {
    saveSettings.mockClear();
  });

  function renderChat(settings: Partial<Settings>, plugins: PluginInfo[] = []) {
    useStore.setState({ settings: settings as Settings, plugins, saveSettings });
    return render(<SettingsSheet section="chat" />);
  }
  const nameField = () => screen.getByRole("textbox", { name: "Your name" });

  it("shows the name saved during onboarding", () => {
    renderChat({ userName: "Ada" });
    expect(nameField()).toHaveValue("Ada");
  });

  it("is empty, showing its placeholder, when onboarding's name step was skipped", () => {
    renderChat({ userName: null });
    expect(nameField()).toHaveValue("");
    expect(nameField()).toHaveAttribute("placeholder", "Enter your name");
  });

  it("is named by its label alone, with the hint as its description, and limited to 80 characters", () => {
    renderChat({ userName: null });
    expect(nameField()).toHaveAccessibleDescription(/OpenPal uses it to greet you/);
    expect(nameField()).toHaveAttribute("maxlength", "80");
  });

  it("saves a changed name, trimmed, when Settings closes", async () => {
    const { unmount } = renderChat({ userName: "Ada" });
    await userEvent.clear(nameField());
    await userEvent.type(nameField(), "  Grace Hopper ");
    expect(saveSettings).not.toHaveBeenCalled();

    unmount();
    expect(saveSettings).toHaveBeenCalledTimes(1);
    expect(saveSettings).toHaveBeenCalledWith(expect.objectContaining({ userName: "Grace Hopper" }));
  });

  it("lets someone who skipped onboarding's name step set one", async () => {
    const { unmount } = renderChat({ userName: null });
    await userEvent.type(nameField(), "Grace");

    unmount();
    expect(saveSettings).toHaveBeenCalledWith(expect.objectContaining({ userName: "Grace" }));
  });

  it("removes the name when the field is emptied", async () => {
    const { unmount } = renderChat({ userName: "Ada" });
    await userEvent.clear(nameField());

    unmount();
    expect(saveSettings).toHaveBeenCalledWith(expect.objectContaining({ userName: null }));
  });

  it("treats a field holding only spaces as no name", async () => {
    const { unmount } = renderChat({ userName: "Ada" });
    await userEvent.clear(nameField());
    await userEvent.type(nameField(), "   ");

    unmount();
    expect(saveSettings).toHaveBeenCalledWith(expect.objectContaining({ userName: null }));
  });

  it("goes out in the same single save as the personality text and a voice slider, so none undoes another", async () => {
    // A Voice add-on that isn't enabled still shows its sliders (so they can be
    // tuned ahead of time) and, unlike an enabled one, needs no voice list.
    const { unmount } = renderChat(
      { userName: "Ada", systemPrompt: "", voiceSpeed: 1 },
      [plugin({ id: "speech", name: "Voice" })],
    );
    await userEvent.type(screen.getByRole("textbox", { name: /Personality/ }), "Be brief.");
    fireEvent.change(screen.getByRole("slider", { name: "Speaking speed" }), {
      target: { value: "1.5" },
    });
    await userEvent.clear(nameField());
    await userEvent.type(nameField(), "Grace");

    unmount();
    expect(saveSettings).toHaveBeenCalledTimes(1);
    expect(saveSettings).toHaveBeenCalledWith(
      expect.objectContaining({ userName: "Grace", systemPrompt: "Be brief.", voiceSpeed: 1.5 }),
    );
  });
});

describe("General settings: appearance", () => {
  beforeEach(() => {
    saveSettings.mockClear();
    updateSettings.mockReset().mockResolvedValue(undefined);
    // Starting from nothing loaded makes seeding a theme a real change, which
    // the store pins on <html> the way boot does.
    useStore.setState({ settings: null, account: null, notice: null });
  });

  afterEach(() => {
    // Picking a theme re-themes the page the test itself runs in.
    document.documentElement.removeAttribute("data-theme");
  });

  function renderGeneral(settings: Partial<Settings> = {}) {
    useStore.setState({ settings: settings as Settings, plugins: [], saveSettings });
    return render(<SettingsSheet section="general" />);
  }
  const themeGroup = () => screen.getByRole("radiogroup", { name: "Theme" });
  const themeRadio = (name: string) => within(themeGroup()).getByRole("radio", { name });
  const pinnedTheme = () => document.documentElement.getAttribute("data-theme");

  it("has an Appearance group whose Theme choice is described by its hint", () => {
    renderGeneral({ theme: "system" });
    expect(screen.getByRole("heading", { level: 2, name: "Appearance" })).toBeInTheDocument();
    expect(themeGroup()).toHaveAccessibleDescription(/follows your device/);
  });

  it("offers System, Light and Dark, in that order, each named by those words alone", () => {
    renderGeneral({ theme: "system" });
    const radios = within(themeGroup()).getAllByRole("radio");
    expect(radios).toHaveLength(3);
    ["System", "Light", "Dark"].forEach((name, i) => expect(radios[i]).toHaveAccessibleName(name));
  });

  it.each([
    { saved: undefined, checked: "System", when: "an older settings file has none" },
    { saved: "system", checked: "System", when: "System is saved" },
    { saved: "sepia", checked: "System", when: "an unknown theme is saved" },
    { saved: "light", checked: "Light", when: "Light is saved" },
    { saved: "dark", checked: "Dark", when: "Dark is saved" },
  ])("checks $checked, and only that, when $when", ({ saved, checked }) => {
    renderGeneral({ theme: saved as Settings["theme"] });
    expect(themeRadio(checked)).toBeChecked();
    expect(within(themeGroup()).getAllByRole("radio", { checked: true })).toHaveLength(1);
  });

  it("sits between the account and the language, with updates after", () => {
    useStore.setState({ account: { email: "ada@example.com", tier: "free", capabilities: [] } });
    renderGeneral();

    const order = ["Account", "Appearance", "Theme", "Language", "Updates"];
    const shown = Array.from(document.querySelectorAll(".settings-label, .settings-group-title"))
      .map((el) => el.textContent ?? "")
      .filter((text) => order.includes(text));
    expect(shown).toEqual(order);
  });

  it("re-themes the app as soon as one is picked, with no Save and no confirmation", async () => {
    const user = userEvent.setup();
    // The write never comes back: nothing on screen may wait for it.
    updateSettings.mockReturnValue(new Promise(() => {}));
    renderGeneral({ theme: "system" });

    await user.click(themeRadio("Dark"));

    expect(themeRadio("Dark")).toBeChecked();
    expect(useStore.getState().settings?.theme).toBe("dark");
    expect(pinnedTheme()).toBe("dark");
    expect(updateSettings).toHaveBeenCalledWith(expect.objectContaining({ theme: "dark" }));
    expect(screen.queryByRole("button", { name: /save/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("lets the device decide again when System is picked", async () => {
    const user = userEvent.setup();
    renderGeneral({ theme: "dark" });
    expect(pinnedTheme()).toBe("dark");

    await user.click(themeRadio("System"));

    expect(pinnedTheme()).toBeNull();
    expect(updateSettings).toHaveBeenCalledWith(expect.objectContaining({ theme: "system" }));
  });

  it("goes back to the saved choice when a pick can't be written", async () => {
    const user = userEvent.setup();
    let failWrite: (reason: Error) => void = () => {};
    updateSettings.mockReturnValue(
      new Promise<never>((_, reject) => {
        failWrite = reject;
      }),
    );
    renderGeneral({ theme: "light" });

    await user.click(themeRadio("Dark"));
    expect(themeRadio("Dark")).toBeChecked();

    failWrite(new Error("disk full"));
    await waitFor(() => expect(themeRadio("Light")).toBeChecked());
    expect(pinnedTheme()).toBe("light");
    expect(useStore.getState().notice).toContain("disk full");
  });

  it("isn't one of the drafts Settings saves when it closes", async () => {
    const { unmount } = renderGeneral({ theme: "system" });
    await userEvent.click(themeRadio("Light"));

    unmount();
    expect(saveSettings).toHaveBeenCalledTimes(1);
    expect(saveSettings.mock.calls[0][0]).not.toHaveProperty("theme");
  });

  it("keeps a pick made just before Settings closes, when its drafts are saved", async () => {
    // The pick's own write is still in flight when the drafts go out.
    updateSettings.mockReturnValueOnce(new Promise(() => {}));
    useStore.setState({
      settings: { theme: "system", userName: "Ada" } as Settings,
      plugins: [],
      saveSettings: realSaveSettings,
    });
    const { unmount } = render(<SettingsSheet section="general" />);
    await userEvent.click(themeRadio("Dark"));

    unmount();
    expect(updateSettings).toHaveBeenCalledTimes(2);
    expect(updateSettings).toHaveBeenLastCalledWith(
      expect.objectContaining({ theme: "dark", userName: "Ada" }),
    );
  });

  it("is a single Tab stop, on the choice that is checked", async () => {
    const user = userEvent.setup();
    renderGeneral({ theme: "light" });

    await user.tab();
    expect(themeRadio("Light")).toHaveFocus();
    // System and Dark are no stops of their own: next comes the Language field.
    await user.tab();
    expect(screen.getByRole("combobox")).toHaveFocus();
  });

  it("moves with the arrow keys, each move being the new choice, saved", async () => {
    const user = userEvent.setup();
    renderGeneral({ theme: "light" });
    await user.tab();

    await user.keyboard("{ArrowRight}");
    expect(themeRadio("Dark")).toBeChecked();
    expect(themeRadio("Dark")).toHaveFocus();
    expect(pinnedTheme()).toBe("dark");
    expect(updateSettings).toHaveBeenLastCalledWith(expect.objectContaining({ theme: "dark" }));

    // Past the last option it comes round to the first.
    await user.keyboard("{ArrowRight}");
    expect(themeRadio("System")).toBeChecked();
    expect(pinnedTheme()).toBeNull();
    expect(updateSettings).toHaveBeenLastCalledWith(expect.objectContaining({ theme: "system" }));
  });

  // MoreDialog.test.tsx runs against a stub of this page, so this holds the real
  // markup to what that stub assumes of it: `revealSetting` finds a setting by
  // the text of its label or group title, then scrolls to and focuses the first
  // control of its field.
  it("is laid out the way Settings search finds it", () => {
    renderGeneral();
    const named = (text: string) =>
      Array.from(document.querySelectorAll(".settings-label, .settings-group-title")).find(
        (el) => el.textContent?.trim() === text,
      );
    expect(named("Theme")?.closest(".settings-field")).toContainElement(themeGroup());
    expect(named("Appearance")?.closest(".settings-group")).toContainElement(themeGroup());
  });

  it("is named in the General subtitle in English", () => {
    renderGeneral();
    expect(screen.getByText(/appearance/i, { selector: ".settings-sub" })).toBeInTheDocument();
  });

  it("is all in French when the app is", async () => {
    await i18n.changeLanguage("fr");
    try {
      const { unmount } = renderGeneral({ theme: "dark" });
      expect(screen.getByText(/apparence/i, { selector: ".settings-sub" })).toBeInTheDocument();
      expect(screen.getByRole("heading", { level: 2, name: "Apparence" })).toBeInTheDocument();
      const group = screen.getByRole("radiogroup", { name: "Thème" });
      expect(group).toHaveAccessibleDescription(/suit le mode clair ou sombre/);
      const radios = within(group).getAllByRole("radio");
      ["Système", "Clair", "Sombre"].forEach((name, i) =>
        expect(radios[i]).toHaveAccessibleName(name),
      );
      expect(within(group).getByRole("radio", { checked: true })).toHaveAccessibleName("Sombre");
      // Out before the language changes back, so the page isn't re-rendered
      // outside act.
      unmount();
    } finally {
      await i18n.changeLanguage("en");
    }
  });
});
