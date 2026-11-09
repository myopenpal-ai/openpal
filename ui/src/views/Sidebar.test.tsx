import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import Sidebar from "./Sidebar";
import { useStore } from "../state/store";
import type { ConversationMeta, ModelInfo, PluginInfo, Project, Settings } from "../lib/types";

vi.mock("../lib/api", () => ({
  api: {
    // The engine pill drives the real store actions (not mocks), so the
    // pill's own buttons need the engine commands they call to exist.
    stopEngine: vi.fn().mockResolvedValue(undefined),
    startEngine: vi.fn().mockResolvedValue(undefined),
    getEngineStatus: vi.fn().mockResolvedValue({ type: "stopped" }),
    getSettings: vi.fn().mockResolvedValue({}),
  },
}));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: vi.fn().mockResolvedValue("0.0.0") }));

function meta(id: string, title: string): ConversationMeta {
  const now = Date.now();
  return { id, title, createdAtMs: now, updatedAtMs: now, messageCount: 2, taskType: "general", pinned: false, unread: false };
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
      // The row's own name carries its hint: see the hint tests below.
      "Generating here Answering…",
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

describe("conversation row hints", () => {
  const CONSENT = {
    requestId: "r1",
    toolCallId: "t1",
    toolId: "fs.write",
    summary: "Write a file",
    detail: "",
    resolvedPaths: [],
    dangerFlags: [],
    via: null,
  };

  beforeEach(() => {
    useStore.setState({
      conversations: [meta("a", "First chat"), meta("b", "Second chat")],
      projects: [],
      plugins: [],
      active: null,
      view: "chat",
      engine: { type: "stopped" },
      modelRequests: [],
      streaming: false,
      pendingToolConsent: null,
      pendingClarifyingQuestion: null,
      pendingInjectionReview: null,
    });
  });

  /** The hint names showing on `title`'s row, in order. */
  function hintsOn(title: string): string[] {
    const row = screen.getByText(title).closest(".conv-row") as HTMLElement;
    return within(row)
      .queryAllByRole("img")
      .map((el) => el.getAttribute("aria-label") ?? "");
  }

  it("shows Answering while its reply runs, and nothing once it ends", () => {
    useStore.setState({
      modelRequests: [{ id: "r-a", kind: "chat", conversationId: "a", status: "running" }],
    });
    render(<Sidebar />);
    expect(hintsOn("First chat")).toEqual(["Answering…"]);
    expect(hintsOn("Second chat")).toEqual([]);
    expect(screen.getByRole("img", { name: "Answering…" })).toHaveAttribute("title", "Answering…");

    act(() => useStore.setState({ modelRequests: [] }));
    expect(hintsOn("First chat")).toEqual([]);
  });

  it("shows Waiting behind another request, Answering once it starts, and nothing if cancelled", () => {
    useStore.setState({
      modelRequests: [
        { id: "r-a", kind: "chat", conversationId: "a", status: "running" },
        { id: "r-b", kind: "chat", conversationId: "b", status: "waiting" },
      ],
    });
    render(<Sidebar />);
    expect(hintsOn("Second chat")).toEqual(["Waiting for the model"]);

    act(() =>
      useStore.setState({
        modelRequests: [{ id: "r-b", kind: "chat", conversationId: "b", status: "running" }],
      }),
    );
    expect(hintsOn("Second chat")).toEqual(["Answering…"]);
    expect(hintsOn("First chat")).toEqual([]);

    act(() =>
      useStore.setState({
        modelRequests: [
          { id: "r-a2", kind: "chat", conversationId: "a", status: "running" },
          { id: "r-b2", kind: "chat", conversationId: "b", status: "waiting" },
        ],
      }),
    );
    act(() =>
      useStore.setState({
        modelRequests: [{ id: "r-a2", kind: "chat", conversationId: "a", status: "running" }],
      }),
    );
    expect(hintsOn("Second chat")).toEqual([]);
  });

  it.each([
    ["a tool-consent prompt", { pendingToolConsent: CONSENT }],
    [
      "a clarifying question",
      { pendingClarifyingQuestion: { requestId: "r1", toolCallId: "t1", question: "Which?", options: [] } },
    ],
    [
      "a prompt-injection review",
      {
        pendingInjectionReview: {
          requestId: "r1",
          toolCallId: null,
          via: null,
          items: [],
          newestMessageId: null,
        },
      },
    ],
  ])("shows Needs your input over Answering while paused on %s, until answered", (_, pending) => {
    useStore.setState({
      modelRequests: [
        { id: "r-a", kind: "chat", conversationId: "a", status: "running" },
        { id: "r-b", kind: "chat", conversationId: "b", status: "waiting" },
      ],
      ...pending,
    });
    render(<Sidebar />);
    expect(hintsOn("First chat")).toEqual(["Needs your input"]);
    // Only the running turn can be paused — not the one waiting behind it.
    expect(hintsOn("Second chat")).toEqual(["Waiting for the model"]);

    act(() =>
      useStore.setState({
        pendingToolConsent: null,
        pendingClarifyingQuestion: null,
        pendingInjectionReview: null,
      }),
    );
    expect(hintsOn("First chat")).toEqual(["Answering…"]);
  });

  it("shows New reply for an unread conversation until it is read", () => {
    useStore.setState({
      conversations: [{ ...meta("a", "First chat"), unread: true }, meta("b", "Second chat")],
    });
    render(<Sidebar />);
    expect(hintsOn("First chat")).toEqual(["New reply"]);
    expect(screen.getByRole("button", { name: "First chat New reply" })).toBeInTheDocument();
    expect(hintsOn("Second chat")).toEqual([]);

    act(() =>
      useStore.setState({ conversations: [meta("a", "First chat"), meta("b", "Second chat")] }),
    );
    expect(hintsOn("First chat")).toEqual([]);
  });

  it("shows hints on rows in the Pinned section, the date groups and project groups", () => {
    useStore.setState({
      projects: [PROJECT],
      conversations: [
        { ...meta("a", "Pinned chat"), pinned: true, unread: true },
        { ...meta("b", "Project chat"), projectId: PROJECT.id },
        meta("c", "Dated chat"),
      ],
      modelRequests: [
        { id: "r-c", kind: "chat", conversationId: "c", status: "running" },
        { id: "r-b", kind: "chat", conversationId: "b", status: "waiting" },
      ],
    });
    render(<Sidebar />);
    expect(screen.getByText("Pinned chat").closest(".conv-date-group")).toBeNull();
    expect(hintsOn("Pinned chat")).toEqual(["New reply"]);
    expect(screen.getByText("Project chat").closest(".project-group")).not.toBeNull();
    expect(hintsOn("Project chat")).toEqual(["Waiting for the model"]);
    expect(screen.getByText("Dated chat").closest(".conv-date-group")).not.toBeNull();
    expect(hintsOn("Dated chat")).toEqual(["Answering…"]);
  });
});

describe("the engine pill while a model is loading", () => {
  const INSTALLED: ModelInfo = {
    spec: { id: "m", name: "Model M" },
    status: { type: "installed", data: { path: "/models/m.gguf", sizeBytes: 1 } },
  } as unknown as ModelInfo;

  it("offers a stop control in place of the spinner, and clicking it stops the load", async () => {
    const user = userEvent.setup();
    useStore.setState({
      engine: { type: "starting", data: { modelId: "m", progressPercent: null } },
      engineBusy: true,
      models: [INSTALLED],
      settings: { selectedModelId: "m" } as unknown as Settings,
    });
    render(<Sidebar />);

    // The slot that holds Load/Unload in other states holds a real stop
    // button here — a load must never trap the user behind a spinner.
    const stop = screen.getByRole("button", { name: "Cancel loading" });
    expect(stop).toBeEnabled();

    await user.click(stop);

    await waitFor(() => expect(useStore.getState().engine.type).toBe("stopped"));
    expect(useStore.getState().engineBusy).toBe(false);
    // The pill lands on the Asleep state (the model name, tooltiped Asleep).
    await waitFor(() => expect(screen.getByTitle("Asleep")).toBeInTheDocument());
  });
});
