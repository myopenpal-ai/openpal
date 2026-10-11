import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import SettingsSheet, { ExperimentalSection } from "./SettingsSheet";
import { useStore } from "../state/store";
import type { PluginInfo, Settings } from "../lib/types";

vi.mock("../lib/api", () => ({ api: {} }));

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
