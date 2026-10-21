import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import ConversationSearch from "./ConversationSearch";
import { useDialogFocus } from "../components/useDialogFocus";
import { useStore } from "../state/store";
import type { Conversation, ConversationMeta } from "../lib/types";

vi.mock("../lib/api", () => ({ api: {} }));

/** Stands in for the More dialog: just enough to record and restore focus
 * the way the real one does. */
function FakeMoreDialog() {
  const page = useStore((s) => s.moreDialogPage);
  const ref = useDialogFocus<HTMLDivElement>(() => useStore.getState().setMoreDialogPage(null));
  return (
    <div ref={ref} tabIndex={-1} role="dialog" aria-label="More">
      more dialog: {page}
    </div>
  );
}

/** Mounts search the way App.tsx does — its own dialog, from
 * `conversationSearchOpen`, beside a main pane it leaves alone. */
function Harness() {
  const open = useStore((s) => s.conversationSearchOpen);
  const moreOpen = useStore((s) => s.moreDialogPage !== null);
  const activeId = useStore((s) => s.active?.id ?? "none");
  return (
    <>
      <main>showing conversation: {activeId}</main>
      <button onClick={() => useStore.getState().setConversationSearchOpen(true)}>search</button>
      {moreOpen && <FakeMoreDialog />}
      {open && <ConversationSearch />}
    </>
  );
}

const RESULT = { id: "lyon", title: "Weekend in Lyon", updatedAtMs: 0, snippet: "…bouchons…" };

const HOUR = 3_600_000;
function meta(id: string, title: string, hoursAgo: number): ConversationMeta {
  const updatedAtMs = Date.now() - hoursAgo * HOUR;
  return { id, title, createdAtMs: updatedAtMs, updatedAtMs, messageCount: 2, taskType: "general", pinned: false, unread: false };
}

/** The marked (matched) text inside `el`, in order. */
function marks(el: HTMLElement): string[] {
  return Array.from(el.querySelectorAll("mark"), (m) => m.textContent ?? "");
}

async function openSearch() {
  const user = userEvent.setup();
  render(<Harness />);
  await user.click(screen.getByRole("button", { name: "search" }));
  return user;
}

describe("ConversationSearch", () => {
  beforeEach(() => {
    useStore.setState({
      conversationSearchOpen: false,
      moreDialogPage: null,
      active: { id: "current" } as Conversation,
      conversations: [],
      modelRequests: [],
      streaming: false,
      sidebarSearchQuery: "",
      sidebarSearchResults: [],
      // Stands in for the real open: the store's own version fetches over IPC.
      openConversation: vi.fn(async (id: string) => {
        useStore.setState({
          active: { id } as Conversation,
          sidebarSearchQuery: "",
          sidebarSearchResults: [],
        });
      }),
    });
  });

  it("opens as its own dialog with the search field focused, over an untouched main pane", async () => {
    await openSearch();

    expect(screen.getByRole("dialog", { name: "Search conversations" })).toBeInTheDocument();
    expect(screen.getByRole("textbox")).toHaveFocus();
    expect(screen.getByText("showing conversation: current")).toBeInTheDocument();
  });

  it("closes on Escape without picking, clearing the query and keeping the conversation", async () => {
    const user = await openSearch();
    useStore.setState({ sidebarSearchQuery: "lyon", sidebarSearchResults: [RESULT] });

    await user.keyboard("{Escape}");

    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(useStore.getState().sidebarSearchQuery).toBe("");
    expect(screen.getByText("showing conversation: current")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "search" })).toHaveFocus();
  });

  it("opens the picked conversation and closes", async () => {
    const user = await openSearch();
    useStore.setState({ sidebarSearchQuery: "lyon", sidebarSearchResults: [RESULT] });

    await user.click(await screen.findByRole("button", { name: /Weekend in Lyon/ }));

    expect(useStore.getState().openConversation).toHaveBeenCalledWith("lyon");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(screen.getByText("showing conversation: lyon")).toBeInTheDocument();
  });

  it("opens a result, by click or Enter, while a reply is still generating", async () => {
    useStore.setState({
      modelRequests: [{ id: "turn-1", kind: "chat", conversationId: "current", status: "running" }],
      streaming: true,
    });
    const user = await openSearch();
    useStore.setState({ sidebarSearchQuery: "lyon", sidebarSearchResults: [RESULT] });

    const row = await screen.findByRole("button", { name: /Weekend in Lyon/ });
    expect(row).toBeEnabled();
    await user.keyboard("{Enter}");

    expect(useStore.getState().openConversation).toHaveBeenCalledWith("lyon");
    expect(screen.getByText("showing conversation: lyon")).toBeInTheDocument();
  });

  it("stays open when the picked conversation fails to open", async () => {
    useStore.setState({ openConversation: vi.fn(async () => {}) });
    const user = await openSearch();
    useStore.setState({ sidebarSearchQuery: "lyon", sidebarSearchResults: [RESULT] });

    await user.click(await screen.findByRole("button", { name: /Weekend in Lyon/ }));

    expect(screen.getByRole("dialog")).toBeInTheDocument();
    expect(screen.getByText("showing conversation: current")).toBeInTheDocument();
  });

  describe("before anything's typed", () => {
    it("offers the latest other conversations, newest first", async () => {
      useStore.setState({
        conversations: [
          meta("old", "Tax questions", 400),
          meta("current", "The one already open", 0),
          ...["a", "b", "c", "d", "e"].map((id, i) => meta(id, `Chat ${id}`, i + 1)),
        ],
      });
      const user = await openSearch();

      const recent = screen.getByRole("region", { name: "Recent" });
      const rows = within(recent).getAllByRole("button");
      expect(rows.map((r) => r.getAttribute("title"))).toEqual([
        "Chat a",
        "Chat b",
        "Chat c",
        "Chat d",
        "Chat e",
      ]);
      expect(rows[0]).toHaveTextContent("an hour ago");

      await user.click(rows[1]);
      expect(useStore.getState().openConversation).toHaveBeenCalledWith("b");
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });

    it("leaves Recent out when there's nothing else to go back to", async () => {
      useStore.setState({ conversations: [meta("current", "The one already open", 0)] });
      await openSearch();

      expect(screen.queryByRole("region", { name: "Recent" })).not.toBeInTheDocument();
      expect(screen.getByRole("region", { name: "Settings" })).toBeInTheDocument();
    });

    it("links every Settings category to its page, handing focus back to the sidebar after", async () => {
      const user = await openSearch();

      const settings = screen.getByRole("region", { name: "Settings" });
      expect(within(settings).getAllByRole("button").map((b) => b.textContent)).toEqual([
        "General",
        "Chat",
        "AI model",
        "Privacy & safety",
        "Devices",
        "Your data",
        "Advanced",
      ]);
      // Each with the icon its More dialog nav entry has.
      for (const b of within(settings).getAllByRole("button")) {
        expect(b.querySelector("svg[aria-hidden='true']")).not.toBeNull();
      }

      await user.click(within(settings).getByRole("button", { name: "Privacy & safety" }));

      expect(screen.queryByRole("dialog", { name: "Search conversations" })).not.toBeInTheDocument();
      expect(useStore.getState().conversationSearchOpen).toBe(false);
      expect(screen.getByRole("dialog", { name: "More" })).toHaveTextContent("more dialog: privacy");

      // Closing the page it opened returns focus to what opened the search,
      // not to the shortcut that no longer exists.
      await user.keyboard("{Escape}");
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: "search" })).toHaveFocus();
    });
  });

  describe("results", () => {
    it("marks a title match in the title, without repeating the title as its excerpt", async () => {
      await openSearch();
      useStore.setState({
        sidebarSearchQuery: "lyon",
        sidebarSearchResults: [{ ...RESULT, snippet: RESULT.title }],
      });

      const row = await screen.findByRole("button", { name: /Weekend in Lyon/ });
      expect(marks(row)).toEqual(["Lyon"]);
      expect(row.querySelector(".conv-snippet")).toBeNull();
    });

    it("marks every place a message excerpt matched, whatever its case", async () => {
      await openSearch();
      useStore.setState({
        sidebarSearchQuery: " Bouchon ",
        sidebarSearchResults: [
          { ...RESULT, snippet: "…the best bouchons in town. BOUCHON means cork, or…" },
        ],
      });

      const row = await screen.findByRole("button", { name: /Weekend in Lyon/ });
      expect(marks(row.querySelector(".conv-title") as HTMLElement)).toEqual([]);
      expect(marks(row.querySelector(".conv-snippet") as HTMLElement)).toEqual(["bouchon", "BOUCHON"]);
      expect(row.querySelector(".conv-snippet")).toHaveTextContent(
        "…the best bouchons in town. BOUCHON means cork, or…",
      );
    });

    it("heads the results with their count, and keeps only the Settings categories that match", async () => {
      await openSearch();
      useStore.setState({ sidebarSearchQuery: "priv", sidebarSearchResults: [RESULT] });

      const found = await screen.findByRole("region", { name: /Conversations/ });
      expect(found).toHaveTextContent("1");
      const settings = screen.getByRole("region", { name: "Settings" });
      const shortcuts = within(settings).getAllByRole("button");
      expect(shortcuts.map((b) => b.textContent)).toEqual(["Privacy & safety"]);
      expect(marks(shortcuts[0])).toEqual(["Priv"]);
      expect(screen.getByRole("status")).toHaveTextContent("1 matching conversation");
      expect(screen.queryByRole("region", { name: "Recent" })).not.toBeInTheDocument();
    });

    it("says so when nothing matches, and drops Settings when no category does either", async () => {
      await openSearch();
      useStore.setState({ sidebarSearchQuery: "zzz", sidebarSearchResults: [] });

      expect(await screen.findByText("No matching conversations.", { selector: "p.conv-empty" })).toBeInTheDocument();
      expect(screen.queryByRole("region", { name: "Settings" })).not.toBeInTheDocument();
    });
  });

  describe("keyboard", () => {
    it("opens the top result on Enter", async () => {
      const user = await openSearch();
      useStore.setState({
        sidebarSearchQuery: "lyon",
        sidebarSearchResults: [RESULT, { ...RESULT, id: "lyon-2", title: "Lyon again" }],
      });

      await user.keyboard("{Enter}");

      expect(useStore.getState().openConversation).toHaveBeenCalledWith("lyon");
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });

    it("walks results with the arrow keys and back up to the field", async () => {
      const user = await openSearch();
      useStore.setState({
        sidebarSearchQuery: "lyon",
        sidebarSearchResults: [RESULT, { ...RESULT, id: "lyon-2", title: "Lyon again" }],
      });
      const field = screen.getByRole("textbox");
      const [first, second] = await screen.findAllByRole("button", { name: /Lyon/ });

      await user.keyboard("{ArrowDown}");
      expect(first).toHaveFocus();
      await user.keyboard("{ArrowDown}");
      expect(second).toHaveFocus();
      await user.keyboard("{ArrowUp}{ArrowUp}");
      expect(field).toHaveFocus();

      await user.keyboard("{ArrowDown}{Enter}");
      expect(useStore.getState().openConversation).toHaveBeenCalledWith("lyon");
    });
  });
});
