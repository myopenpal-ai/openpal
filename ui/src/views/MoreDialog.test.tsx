import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import MoreDialog from "./MoreDialog";
import { useStore } from "../state/store";
import type { AppInfo, PluginInfo } from "../lib/types";

// The pages themselves have their own tests; this one is about the dialog
// around them, so each renders as nothing but a recognisable heading.
vi.mock("./Library", () => ({ default: () => <h1>Library page</h1> }));
vi.mock("./Discover", () => ({ default: () => <h1>Discover page</h1> }));
vi.mock("./Plugins", () => ({ default: () => <h1>Plugins page</h1> }));
vi.mock("./SkillsView", () => ({ default: () => <h1>Skills page</h1> }));
vi.mock("./SettingsSheet", () => ({
  default: ({ section }: { section: string }) => (
    <>
      <h1>Settings page: {section}</h1>
      {section === "general" && (
        <>
          <div className="settings-field" data-testid="theme-field">
            <span className="settings-label">Theme</span>
            <div role="radiogroup" aria-label="Theme">
              <input type="radio" name="theme" aria-label="System" />
              <input type="radio" name="theme" aria-label="Light" />
              <input type="radio" name="theme" aria-label="Dark" />
            </div>
          </div>
          <div className="settings-field" data-testid="language-field">
            <span className="settings-label">Language</span>
            <select aria-label="Language">
              <option>English</option>
            </select>
          </div>
          <div className="settings-field" data-testid="notifications-field">
            <span className="settings-label">Desktop notifications</span>
            <label className="settings-toggle">
              <input type="checkbox" aria-label="Desktop notifications" />
            </label>
          </div>
        </>
      )}
    </>
  ),
}));

function plugin(overrides: Partial<PluginInfo> = {}): PluginInfo {
  return {
    id: "test-plugin",
    name: "Test Plugin",
    description: "",
    version: "1.0.0",
    author: "",
    capabilities: [],
    enabled: false,
    setup: { type: "ready" },
    service: { type: "stopped" },
    hasChat: false,
    experimental: false,
    ...overrides,
  };
}

/** Mounts the dialog the way App.tsx does — from `moreDialogPage`, next to a
 * main pane it must never touch — plus a trigger to open it from. */
function Harness() {
  const page = useStore((s) => s.moreDialogPage);
  const view = useStore((s) => s.view);
  return (
    <>
      <main>
        <p>main pane: {view}</p>
        <textarea aria-label="draft" />
      </main>
      <button onClick={() => useStore.getState().setMoreDialogPage("skills")}>open skills</button>
      {page && <MoreDialog page={page} />}
    </>
  );
}

const navEntries = () =>
  within(screen.getByRole("navigation", { name: "More" }))
    .getAllByRole("listitem")
    .map((li) => li.textContent);

/** Settings' categories, in nav order — always all of them. */
const SETTINGS = [
  "General",
  "Chat",
  "AI model",
  "Privacy & safety",
  "Devices",
  "Your data",
  "Advanced",
];

const searchField = () => screen.getByRole("textbox", { name: "Search settings" });

describe("MoreDialog", () => {
  beforeEach(() => {
    useStore.setState({
      moreDialogPage: null,
      view: "chat",
      plugins: [],
      appInfo: { portable: false, isAndroid: false } as AppInfo,
      account: null,
    });
    // Not implemented by jsdom.
    Element.prototype.scrollIntoView = vi.fn();
  });

  it("opens on the chosen page and switches pages from its left nav", async () => {
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(screen.getByRole("button", { name: "open skills" }));
    const dialog = screen.getByRole("dialog", { name: "Skills" });
    expect(within(dialog).getByRole("heading", { name: "Skills page" })).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Skills" })).toHaveAttribute(
      "aria-current",
      "page",
    );

    await user.click(within(dialog).getByRole("button", { name: "Chat" }));
    expect(screen.getByRole("dialog", { name: "Settings" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Settings page: chat" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Skills page" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Chat" })).toHaveAttribute("aria-current", "page");
    expect(screen.getByRole("button", { name: "Skills" })).not.toHaveAttribute("aria-current");
  });

  it("lists Settings' categories under a Settings heading, each a page of its own", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await user.click(screen.getByRole("button", { name: "open skills" }));

    const settings = within(screen.getByRole("navigation", { name: "More" })).getByRole("list", {
      name: "Settings",
    });
    expect(within(settings).getAllByRole("button").map((b) => b.textContent)).toEqual(SETTINGS);

    await user.click(within(settings).getByRole("button", { name: "Privacy & safety" }));
    expect(screen.getByRole("heading", { name: "Settings page: privacy" })).toBeInTheDocument();
    await user.click(within(settings).getByRole("button", { name: "Devices" }));
    // Still the one Settings dialog, now on another of its categories.
    expect(screen.getByRole("dialog", { name: "Settings" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Settings page: devices" })).toBeInTheDocument();
    expect(within(settings).getByRole("button", { name: "Devices" })).toHaveAttribute(
      "aria-current",
      "page",
    );
    expect(within(settings).getByRole("button", { name: "Privacy & safety" })).not.toHaveAttribute(
      "aria-current",
    );
  });

  it("closes back onto an untouched main pane, with focus back on what opened it", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await user.type(screen.getByRole("textbox", { name: "draft" }), "half-written");

    const trigger = screen.getByRole("button", { name: "open skills" });
    await user.click(trigger);
    await user.click(screen.getByRole("button", { name: "Models" }));
    await user.keyboard("{Escape}");

    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(useStore.getState().moreDialogPage).toBeNull();
    expect(screen.getByText("main pane: chat")).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "draft" })).toHaveValue("half-written");
    expect(trigger).toHaveFocus();
  });

  it("closes from its close button and from a click outside it", async () => {
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(screen.getByRole("button", { name: "open skills" }));
    await user.click(screen.getByRole("button", { name: "Close" }));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "open skills" }));
    // A click inside the dialog must not count as outside it.
    await user.click(screen.getByRole("heading", { name: "Skills page" }));
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    await user.click(screen.getByRole("dialog").parentElement!);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("lists exactly the pages the More menu offers", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await user.click(screen.getByRole("button", { name: "open skills" }));
    // No add-ons listed and no model browser: Browse and Plugins are hidden.
    expect(navEntries()).toEqual(["Models", "Skills", ...SETTINGS]);

    useStore.setState({
      plugins: [plugin({ capabilities: ["model-browser"], enabled: true })],
    });
    expect(await screen.findByRole("button", { name: "Browse" })).toBeInTheDocument();
    expect(navEntries()).toEqual(["Models", "Browse", "Plugins", "Skills", ...SETTINGS]);

    // Nothing to install add-ons into on Android (nor on portable builds).
    useStore.setState({ appInfo: { portable: false, isAndroid: true } as AppInfo });
    expect(await screen.findByRole("button", { name: "Browse" })).toBeInTheDocument();
    expect(navEntries()).toEqual(["Models", "Browse", "Skills", ...SETTINGS]);
  });

  it("searches pages and settings from above the nav, and opens what's picked", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await user.click(screen.getByRole("button", { name: "open skills" }));
    const nav = screen.getByRole("navigation", { name: "More" });

    await user.type(searchField(), "chat");
    // The pages give way to what the query finds: pages by name first.
    expect(within(nav).queryByRole("list", { name: "Settings" })).not.toBeInTheDocument();
    const found = within(within(nav).getByRole("list")).getAllByRole("button");
    expect(found[0]).toHaveAccessibleName("Chat");
    expect(found.slice(1).map((b) => b.textContent)).toContain("Personality Chat");
    expect(screen.getByRole("status")).toHaveTextContent(`${found.length} results`);

    await user.keyboard("{Enter}");
    expect(screen.getByRole("heading", { name: "Settings page: chat" })).toBeInTheDocument();
    expect(searchField()).toHaveValue("");
    expect(navEntries()).toEqual(["Models", "Skills", ...SETTINGS]);
    expect(within(nav).getByRole("button", { name: "Chat" })).toHaveFocus();
  });

  it("opens a setting's category on it, scrolled to and focused", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await user.click(screen.getByRole("button", { name: "open skills" }));

    await user.type(searchField(), "langu");
    await user.click(screen.getByRole("button", { name: "Language General" }));

    expect(screen.getByRole("heading", { name: "Settings page: general" })).toBeInTheDocument();
    const field = screen.getByTestId("language-field");
    expect(field).toHaveClass("settings-search-hit");
    expect(field.scrollIntoView).toHaveBeenCalled();
    expect(screen.getByRole("combobox", { name: "Language" })).toHaveFocus();
  });

  it("finds the Theme setting, and opens General on its options", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await user.click(screen.getByRole("button", { name: "open skills" }));

    await user.type(searchField(), "theme");
    expect(screen.getByRole("status")).toHaveTextContent("1 result");
    await user.click(screen.getByRole("button", { name: "Theme General" }));

    expect(screen.getByRole("heading", { name: "Settings page: general" })).toBeInTheDocument();
    const field = screen.getByTestId("theme-field");
    expect(field).toHaveClass("settings-search-hit");
    expect(field.scrollIntoView).toHaveBeenCalled();
    // Focus lands on the field's first control, as it does for any setting.
    expect(screen.getByRole("radio", { name: "System" })).toHaveFocus();
  });

  it("finds the Desktop notifications switch, and opens General right on it", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await user.click(screen.getByRole("button", { name: "open skills" }));

    await user.type(searchField(), "desktop notif");
    expect(screen.getByRole("status")).toHaveTextContent("1 result");
    await user.click(screen.getByRole("button", { name: "Desktop notifications General" }));

    expect(screen.getByRole("heading", { name: "Settings page: general" })).toBeInTheDocument();
    const field = screen.getByTestId("notifications-field");
    expect(field).toHaveClass("settings-search-hit");
    expect(field.scrollIntoView).toHaveBeenCalled();
    // Focus lands on the field's first control: the switch itself.
    expect(screen.getByRole("checkbox", { name: "Desktop notifications" })).toHaveFocus();
  });

  it("doesn't offer the Desktop notifications switch in a build with none (Android, iOS)", async () => {
    useStore.setState({
      appInfo: { portable: false, isAndroid: false, nativeNotifications: false } as AppInfo,
    });
    const user = userEvent.setup();
    render(<Harness />);
    await user.click(screen.getByRole("button", { name: "open skills" }));

    await user.type(searchField(), "desktop notif");

    expect(screen.getByRole("status")).toHaveTextContent("No results for “desktop notif”");
    expect(
      screen.queryByRole("button", { name: "Desktop notifications General" }),
    ).not.toBeInTheDocument();
  });

  it("says when nothing matches, and Escape clears the search before it closes", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await user.click(screen.getByRole("button", { name: "open skills" }));

    await user.type(searchField(), "zzz");
    expect(screen.getByRole("status")).toHaveTextContent("No results for “zzz”");

    await user.keyboard("{Escape}");
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    expect(searchField()).toHaveValue("");
    expect(navEntries()).toEqual(["Models", "Skills", ...SETTINGS]);

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });
});
