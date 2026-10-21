import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import Sidebar from "./Sidebar";
import { useStore } from "../state/store";
import type { ConversationMeta, PluginInfo, Project } from "../lib/types";

vi.mock("../lib/api", () => ({ api: {} }));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: vi.fn().mockResolvedValue("0.0.0") }));

function meta(id: string, title: string): ConversationMeta {
  const now = Date.now();
  return { id, title, createdAtMs: now, updatedAtMs: now, messageCount: 2, taskType: "general", pinned: false };
}

const WRITING_TOOLS = {
  id: "writing-tools",
  capabilities: ["writing-tools"],
  enabled: true,
} as unknown as PluginInfo;

const PROJECT: Project = { id: "p1", name: "Research", createdAtMs: 0, updatedAtMs: 0 };

describe("Sidebar while a reply is generating", () => {
  beforeEach(() => {
    useStore.setState({
      conversations: [meta("a", "Generating here"), meta("b", "Another chat")],
      projects: [PROJECT],
      plugins: [WRITING_TOOLS],
      active: null,
      view: "chat",
      engine: { type: "stopped" },
      // A reply streaming, and another request waiting behind it.
      modelRequests: [
        { id: "turn-a", kind: "chat", conversationId: "a", status: "running" },
        { id: "wt-1", kind: "writingTool", conversationId: null, status: "waiting" },
      ],
      streaming: true,
      newChat: vi.fn(),
      openConversation: vi.fn(async () => {}),
      deleteConversation: vi.fn(async () => {}),
      setView: vi.fn(),
    });
  });

  it("keeps new chat, the quick actions, projects and every conversation row usable", async () => {
    const user = userEvent.setup();
    render(<Sidebar />);

    for (const name of [
      "New chat",
      "Improve a message",
      "Translate",
      "Correct",
      "New project",
      "Select conversations",
      "Generating here",
      "Another chat",
      'Rename "Research"',
      'Delete "Research"',
      'More actions for "Generating here"',
      'More actions for "Another chat"',
    ]) {
      expect(screen.getByRole("button", { name })).toBeEnabled();
    }

    await user.click(screen.getByRole("button", { name: "Another chat" }));
    expect(useStore.getState().openConversation).toHaveBeenCalledWith("b");
    await user.click(screen.getByRole("button", { name: "New chat" }));
    expect(useStore.getState().newChat).toHaveBeenCalled();
  });

  it("keeps selection and bulk delete usable", async () => {
    const user = userEvent.setup();
    render(<Sidebar />);

    await user.click(screen.getByRole("button", { name: "Select conversations" }));
    const selectAll = screen.getByRole("checkbox", { name: "Select all conversations" });
    expect(selectAll).toBeEnabled();
    await user.click(selectAll);

    const bulkDelete = screen.getByRole("button", { name: "Delete selected conversations" });
    expect(bulkDelete).toBeEnabled();
  });
});
