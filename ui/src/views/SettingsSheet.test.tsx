import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import i18n from "../i18n";
import SettingsSheet, { ExperimentalSection } from "./SettingsSheet";
import { useStore } from "../state/store";
import type { AppInfo, PluginInfo, Settings } from "../lib/types";

const {
  updateSettings,
  notificationPermission,
  requestNotificationPermission,
  openNotificationSettings,
} = vi.hoisted(() => ({
  updateSettings: vi.fn(),
  notificationPermission: vi.fn(),
  requestNotificationPermission: vi.fn(),
  openNotificationSettings: vi.fn(),
}));

// The only calls the pages rendered here may make are the write behind the theme
// picker and the notifications switch (the store's `setTheme` and
// `setDesktopNotifications`), the OS permission read the Notifications group
// does when it opens, the OS permission request that turning the switch on
// makes, and the "blocked" line's button that opens the OS notification
// settings; any other still throws.
vi.mock("../lib/api", () => ({
  api: {
    updateSettings,
    notificationPermission,
    requestNotificationPermission,
    openNotificationSettings,
  },
}));

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
    notificationPermission.mockReset().mockResolvedValue("granted");
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

describe("General settings: notifications", () => {
  beforeEach(() => {
    saveSettings.mockClear();
    updateSettings.mockReset().mockResolvedValue(undefined);
    notificationPermission.mockReset().mockResolvedValue("granted");
    requestNotificationPermission.mockReset().mockResolvedValue("granted");
    openNotificationSettings.mockReset().mockResolvedValue(undefined);
    useStore.setState({
      settings: null,
      account: null,
      notice: null,
      appInfo: null,
      notificationPermission: "granted",
    });
  });

  function renderGeneral(settings: Partial<Settings> = {}) {
    useStore.setState({ settings: settings as Settings, plugins: [], saveSettings });
    return render(<SettingsSheet section="general" />);
  }
  const toggle = () => screen.getByRole("checkbox", { name: "Desktop notifications" });
  const OPEN_SETTINGS = "Open notification settings";
  const OPEN_FAILED =
    "Couldn't open your system settings. Open them yourself, find OpenPal under Notifications and turn on \"Allow notifications\".";
  const BLOCKED = "Notifications are blocked for OpenPal. In your system settings, turn on \"Allow notifications\" for OpenPal, then come back here.";

  it("has a Notifications group with a Desktop notifications switch, described by its hint", () => {
    renderGeneral({ desktopNotifications: false });
    expect(screen.getByRole("heading", { level: 2, name: "Notifications" })).toBeInTheDocument();
    // Named by the visible label (not the "On"/"Off" beside it), described by the hint.
    expect(toggle()).toHaveAccessibleDescription(/system notification about models and answers/);
    expect(toggle().closest("label")).toHaveTextContent("Off");
  });

  it.each([
    { saved: undefined, checked: false, when: "a settings file saved before it existed has none" },
    { saved: false, checked: false, when: "it is saved off" },
    { saved: true, checked: true, when: "it is saved on" },
  ])("is checked: $checked, when $when", ({ saved, checked }) => {
    renderGeneral({ desktopNotifications: saved });
    if (checked) expect(toggle()).toBeChecked();
    else expect(toggle()).not.toBeChecked();
  });

  it("turns on with one click: saves the whole settings, asks the OS, no dialog of ours", async () => {
    renderGeneral({ desktopNotifications: false, userName: "Ada" });
    await waitFor(() => expect(notificationPermission).toHaveBeenCalledTimes(1)); // on opening
    notificationPermission.mockClear();
    // Merely opening the page reads; it never asks (and so never prompts).
    expect(requestNotificationPermission).not.toHaveBeenCalled();

    await userEvent.click(toggle());

    expect(toggle()).toBeChecked();
    expect(updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({ desktopNotifications: true, userName: "Ada" }),
    );
    await waitFor(() => expect(requestNotificationPermission).toHaveBeenCalledTimes(1));
    expect(notificationPermission).not.toHaveBeenCalled();
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    // Not one of the drafts saved when Settings closes: it saved itself.
    expect(saveSettings).not.toHaveBeenCalled();
  });

  it("turns off with one click, saved, without asking the OS", async () => {
    renderGeneral({ desktopNotifications: true });
    await waitFor(() => expect(notificationPermission).toHaveBeenCalledTimes(1));
    notificationPermission.mockClear();

    await userEvent.click(toggle());

    expect(toggle()).not.toBeChecked();
    expect(updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({ desktopNotifications: false }),
    );
    await act(async () => {});
    expect(notificationPermission).not.toHaveBeenCalled();
    expect(requestNotificationPermission).not.toHaveBeenCalled();
  });

  it("can be flipped from the keyboard with Space", async () => {
    const user = userEvent.setup();
    renderGeneral({ desktopNotifications: false });

    toggle().focus();
    await user.keyboard(" ");

    expect(toggle()).toBeChecked();
    expect(updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({ desktopNotifications: true }),
    );
  });

  it("goes back to the saved choice, with a notice, when the write fails", async () => {
    updateSettings.mockRejectedValueOnce(new Error("disk full"));
    renderGeneral({ desktopNotifications: false });

    await userEvent.click(toggle());

    await waitFor(() => expect(toggle()).not.toBeChecked());
    expect(useStore.getState().notice).toContain("disk full");
  });

  it("reads the OS permission when the page opens, and shows no blocked line while it allows", async () => {
    renderGeneral({ desktopNotifications: true });

    await waitFor(() => expect(notificationPermission).toHaveBeenCalledTimes(1));
    expect(screen.queryByText(BLOCKED)).not.toBeInTheDocument();
  });

  it("shows the blocked line, as a description and not an alert, when the switch is on and the OS says no", async () => {
    notificationPermission.mockResolvedValue("denied");
    renderGeneral({ desktopNotifications: true });

    const line = await screen.findByText(BLOCKED);
    expect(toggle()).toHaveAccessibleDescription(/system notification about models/);
    expect(toggle()).toHaveAccessibleDescription(/blocked for OpenPal/);
    // Announced politely when it fills in, never an interrupting alert, and
    // the switch is still theirs to use.
    expect(line).toHaveAttribute("role", "status");
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(toggle()).toBeEnabled();
    expect(toggle()).toBeChecked();
  });

  it("has the blocked line's status region in place, empty, before the OS has said anything", async () => {
    // A status message is only announced when it is added to a region that is
    // already there, and the answer arrives after the switch is turned on.
    renderGeneral({ desktopNotifications: false });
    const region = document.querySelector(".settings-status");
    expect(region).toHaveAttribute("role", "status");
    expect(region).toBeEmptyDOMElement();
    requestNotificationPermission.mockResolvedValue("denied");
    await userEvent.click(toggle());
    expect(await screen.findByText(BLOCKED)).toBe(region);
  });

  it("doesn't show the blocked line while the switch is off, whatever the OS says", async () => {
    notificationPermission.mockResolvedValue("denied");
    renderGeneral({ desktopNotifications: false });

    await waitFor(() => expect(useStore.getState().notificationPermission).toBe("denied"));
    expect(screen.queryByText(BLOCKED)).not.toBeInTheDocument();
    expect(toggle()).toHaveAccessibleDescription(/^Get a system notification/);
  });

  it("shows the blocked line as soon as the OS refuses a switch just turned on", async () => {
    // Open and untouched: nothing was blocked, the OS had not been asked yet.
    renderGeneral({ desktopNotifications: false });
    await waitFor(() => expect(notificationPermission).toHaveBeenCalledTimes(1));
    expect(screen.queryByText(BLOCKED)).not.toBeInTheDocument();

    // The user says "Don't Allow" at the system prompt that the click raises.
    requestNotificationPermission.mockResolvedValue("denied");
    await userEvent.click(toggle());

    expect(await screen.findByText(BLOCKED)).toBeInTheDocument();
  });

  it("re-reads the OS when the window regains focus, so fixing it in system settings clears the line", async () => {
    notificationPermission.mockResolvedValue("denied");
    renderGeneral({ desktopNotifications: true });
    expect(await screen.findByText(BLOCKED)).toBeInTheDocument();

    // The user allowed OpenPal in system settings and came back.
    notificationPermission.mockResolvedValue("granted");
    act(() => {
      window.dispatchEvent(new Event("focus"));
    });

    await waitFor(() => expect(screen.queryByText(BLOCKED)).not.toBeInTheDocument());
  });

  it("offers a button to the OS notification settings with the blocked line, and with nothing else", async () => {
    notificationPermission.mockResolvedValue("denied");
    renderGeneral({ desktopNotifications: true });

    const button = await screen.findByRole("button", { name: OPEN_SETTINGS });
    // Next to the line but outside its status region (a button in a live region
    // is read out again with every change), and not part of what describes the
    // switch.
    const region = document.querySelector(".settings-status");
    expect(region).not.toContainElement(button);
    expect(button.closest(".settings-field")).toContainElement(toggle());
    expect(toggle()).not.toHaveAccessibleDescription(/Open notification settings/);
    // Showing it opens nothing.
    expect(openNotificationSettings).not.toHaveBeenCalled();
  });

  it("opens the OS settings when the button is pressed, and says nothing more when that works", async () => {
    notificationPermission.mockResolvedValue("denied");
    renderGeneral({ desktopNotifications: true });

    await userEvent.click(await screen.findByRole("button", { name: OPEN_SETTINGS }));

    expect(openNotificationSettings).toHaveBeenCalledTimes(1);
    expect(screen.queryByText(OPEN_FAILED, { exact: false })).not.toBeInTheDocument();
    // Still blocked until the OS says otherwise.
    expect(screen.getByText(BLOCKED)).toBeInTheDocument();
  });

  it("says where to look when the OS settings cannot be opened, and clears that on the next try", async () => {
    notificationPermission.mockResolvedValue("denied");
    openNotificationSettings.mockRejectedValueOnce(new Error("Not available on this system."));
    renderGeneral({ desktopNotifications: true });

    await userEvent.click(await screen.findByRole("button", { name: OPEN_SETTINGS }));

    // Appended to the polite status the switch is described by: no alert, no
    // raw error text, and the button is still there to try again.
    const region = document.querySelector(".settings-status");
    await waitFor(() => expect(region).toHaveTextContent(OPEN_FAILED));
    expect(region).toHaveAttribute("role", "status");
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(region).not.toHaveTextContent("Not available on this system");
    expect(toggle()).toHaveAccessibleDescription(/Allow notifications/);

    await userEvent.click(screen.getByRole("button", { name: OPEN_SETTINGS }));
    expect(openNotificationSettings).toHaveBeenCalledTimes(2);
    await waitFor(() => expect(region).not.toHaveTextContent(OPEN_FAILED));
    expect(region).toHaveTextContent(BLOCKED);
  });

  it("has no button while the OS allows it or the switch is off", async () => {
    notificationPermission.mockResolvedValue("granted");
    renderGeneral({ desktopNotifications: true });
    await waitFor(() => expect(notificationPermission).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole("button", { name: OPEN_SETTINGS })).not.toBeInTheDocument();

    // Not asked yet is not blocked: turning the switch on is what asks.
    act(() => useStore.setState({ notificationPermission: "prompt" }));
    expect(screen.queryByText(BLOCKED)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: OPEN_SETTINGS })).not.toBeInTheDocument();

    // Denied, but nobody asked for notifications: nothing to fix.
    useStore.setState({ notificationPermission: "denied" });
    await userEvent.click(toggle()); // off
    await waitFor(() => expect(toggle()).not.toBeChecked());
    expect(screen.queryByRole("button", { name: OPEN_SETTINGS })).not.toBeInTheDocument();
  });

  it("drops the button when the OS setting is fixed and the window comes back", async () => {
    notificationPermission.mockResolvedValue("denied");
    renderGeneral({ desktopNotifications: true });
    expect(await screen.findByRole("button", { name: OPEN_SETTINGS })).toBeInTheDocument();

    notificationPermission.mockResolvedValue("granted");
    act(() => {
      window.dispatchEvent(new Event("focus"));
    });

    await waitFor(() =>
      expect(screen.queryByRole("button", { name: OPEN_SETTINGS })).not.toBeInTheDocument(),
    );
  });

  it("stops listening for focus once the page is gone", async () => {
    const { unmount } = renderGeneral({ desktopNotifications: true });
    await waitFor(() => expect(notificationPermission).toHaveBeenCalledTimes(1));
    unmount();
    notificationPermission.mockClear();

    window.dispatchEvent(new Event("focus"));

    await act(async () => {});
    expect(notificationPermission).not.toHaveBeenCalled();
  });

  it("words the switch for a phone on Android, with the same hint-and-switch shape", async () => {
    useStore.setState({ appInfo: { isAndroid: true, nativeNotifications: true } as AppInfo });
    renderGeneral({ desktopNotifications: false });

    const mobile = screen.getByRole("checkbox", { name: "Notifications on this device" });
    expect(mobile).toHaveAccessibleDescription(/about downloads and answers while you're using another app/);
    expect(screen.queryByRole("checkbox", { name: "Desktop notifications" })).not.toBeInTheDocument();
    await act(async () => {});
  });

  it("is left out of a build with no OS notifications (Android, iOS)", async () => {
    useStore.setState({ appInfo: { isAndroid: false, nativeNotifications: false } as AppInfo });
    renderGeneral({ desktopNotifications: false });

    expect(screen.queryByRole("heading", { level: 2, name: "Notifications" })).not.toBeInTheDocument();
    expect(screen.queryByRole("checkbox", { name: "Desktop notifications" })).not.toBeInTheDocument();
    // Updates is still there after Language, and nothing asked the OS anything.
    expect(screen.getByRole("heading", { level: 2, name: "Updates" })).toBeInTheDocument();
    await act(async () => {});
    expect(notificationPermission).not.toHaveBeenCalled();
  });

  // MoreDialog.test.tsx runs against a stub of this page, so this holds the real
  // markup to what that stub assumes of it (see the same test for the theme).
  it("is laid out the way Settings search finds it", () => {
    renderGeneral();
    const named = (text: string) =>
      Array.from(document.querySelectorAll(".settings-label, .settings-group-title")).find(
        (el) => el.textContent?.trim() === text,
      );
    expect(named("Desktop notifications")?.closest(".settings-field")).toContainElement(toggle());
    expect(named("Notifications")?.closest(".settings-group")).toContainElement(toggle());
    // Search focuses the first enabled control of the field: the switch itself.
    expect(toggle()).toBeEnabled();
  });

  it("sits after Language and before Updates", () => {
    renderGeneral();
    const order = ["Appearance", "Language", "Notifications", "Desktop notifications", "Updates"];
    const shown = Array.from(document.querySelectorAll(".settings-label, .settings-group-title"))
      .map((el) => el.textContent ?? "")
      .filter((text) => order.includes(text));
    expect(shown).toEqual(order);
  });

  it("is named in the General subtitle", () => {
    renderGeneral();
    expect(screen.getByText(/notifications/i, { selector: ".settings-sub" })).toBeInTheDocument();
  });

  it("is all in French when the app is", async () => {
    await i18n.changeLanguage("fr");
    try {
      notificationPermission.mockResolvedValue("denied");
      const { unmount } = renderGeneral({ desktopNotifications: true });
      expect(screen.getByText(/notifications/i, { selector: ".settings-sub" })).toBeInTheDocument();
      expect(screen.getByRole("heading", { level: 2, name: "Notifications" })).toBeInTheDocument();
      const field = screen.getByRole("checkbox", { name: "Notifications de bureau" });
      expect(field).toBeChecked();
      expect(field).toHaveAccessibleDescription(/notification système au sujet des modèles/);
      expect(
        await screen.findByText(
          "Les notifications sont bloquées pour OpenPal. Dans les réglages de votre système, activez « Autoriser les notifications » pour OpenPal, puis revenez ici.",
        ),
      ).toBeInTheDocument();
      expect(
        screen.getByRole("button", { name: "Ouvrir les réglages des notifications" }),
      ).toBeInTheDocument();
      // Out before the language changes back, so the page isn't re-rendered
      // outside act.
      unmount();
    } finally {
      await i18n.changeLanguage("en");
    }
  });
});
