import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import ChatView from "./ChatView";
import { resetAlertTracking, useStore, type ModelRequest } from "../state/store";
import type { Conversation, PluginInfo } from "../lib/types";
import { DEFAULT_SAMPLING } from "../lib/types";

const { enhancePrompt } = vi.hoisted(() => ({
  enhancePrompt: vi.fn().mockResolvedValue({ type: "enhanced", data: { text: "A clearer ask." } }),
}));

const RUNNING = {
  type: "running",
  data: { modelId: "m", port: 1, diskSlow: false, autoUnloadAt: null },
} as const;

// Anything ChatView asks the backend for on its own (parental status, voice
// readiness, typing's idle-timer nudge…) answers "nothing", and the engine
// stays loaded.
vi.mock("../lib/api", () => ({
  api: new Proxy(
    {},
    {
      get: (_api, name) =>
        name === "getEngineStatus"
          ? vi.fn().mockResolvedValue(RUNNING)
          : name === "enhancePrompt"
            ? enhancePrompt
            : vi.fn().mockResolvedValue(null),
    },
  ),
}));

function conversation(id: string, lastReply: string): Conversation {
  return {
    id,
    title: `Chat ${id}`,
    createdAtMs: 0,
    updatedAtMs: 0,
    messages: [
      { id: `${id}-u0`, role: "user", content: "first question" },
      { id: `${id}-a0`, role: "assistant", content: "first answer" },
      { id: `${id}-u1`, role: "user", content: "second question" },
      { id: `${id}-a1`, role: "assistant", content: lastReply },
    ],
    sampling: DEFAULT_SAMPLING,
    contextLength: 4096,
    taskType: "general",
    // Two versions of the last reply, so its arrows show.
    branches: {
      [`${id}-u1`]: {
        active: 1,
        variants: [
          [{ id: `${id}-old`, role: "assistant", content: "older answer" }],
          [{ id: `${id}-a1`, role: "assistant", content: lastReply }],
        ],
      },
    },
    pinned: false,
  } as Conversation;
}

function show(active: Conversation, requests: ModelRequest[]) {
  useStore.setState({
    active,
    modelRequests: requests,
    streaming: requests.length > 0,
  });
  return render(<ChatView />);
}

const send = vi.fn(async () => undefined);
const cancelModelRequest = vi.fn(async () => {});
const retryMessage = vi.fn(async () => {});

beforeEach(() => {
  send.mockClear();
  cancelModelRequest.mockClear();
  retryMessage.mockClear();
  useStore.setState({
    engine: RUNNING,
    settings: null,
    plugins: [],
    models: [],
    skills: [],
    chatError: null,
    liveActivity: null,
    pendingToolConsent: null,
    pendingClarifyingQuestion: null,
    pendingInjectionReview: null,
    safetyAlerts: {},
    send,
    cancelModelRequest,
    retryMessage,
  });
});

describe("ChatView while another conversation's reply is generating", () => {
  const elsewhere: ModelRequest[] = [
    { id: "turn-x", kind: "chat", conversationId: "x", status: "running" },
  ];

  it("lets the composer send a new message", async () => {
    const user = userEvent.setup();
    show(conversation("b", "settled answer"), elsewhere);

    await user.type(screen.getByRole("textbox"), "my next message");
    const sendButton = screen.getByRole("button", { name: "Send" });
    expect(sendButton).toBeEnabled();
    await user.click(sendButton);

    expect(send).toHaveBeenCalledWith("my next message", []);
  });

  it("keeps edit, retry, fork and the version arrows usable", () => {
    show(conversation("b", "settled answer"), elsewhere);

    for (const button of screen.getAllByRole("button", { name: /^(Edit|Retry|Fork)$/ })) {
      expect(button).toBeEnabled();
    }
    expect(screen.getByRole("button", { name: "Previous version" })).toBeEnabled();
  });
});

describe("ChatView for a conversation whose own request is waiting", () => {
  const waiting: ModelRequest[] = [
    { id: "turn-x", kind: "chat", conversationId: "x", status: "running" },
    { id: "turn-b", kind: "chat", conversationId: "b", status: "waiting" },
  ];

  it("shows the reply as waiting, cancellable on its own", async () => {
    const user = userEvent.setup();
    show(conversation("b", ""), waiting);

    expect(
      screen.getByText("Waiting for the model to finish its current task…"),
    ).toBeInTheDocument();
    // One on the waiting reply itself, one in place of Send.
    const cancels = screen.getAllByRole("button", { name: "Cancel this request" });
    expect(cancels).toHaveLength(2);
    await user.click(cancels[0]);
    expect(cancelModelRequest).toHaveBeenCalledWith("turn-b");
    await user.click(cancels[1]);
    expect(cancelModelRequest).toHaveBeenLastCalledWith("turn-b");
    // Nothing to act on in the reply that hasn't started.
    expect(screen.getAllByRole("button", { name: /^Retry$/ })).toHaveLength(1);
  });
});

describe("ChatView for a conversation whose own reply is running", () => {
  const running: ModelRequest[] = [
    { id: "turn-b", kind: "chat", conversationId: "b", status: "running" },
    { id: "turn-c", kind: "chat", conversationId: "c", status: "waiting" },
  ];

  it("offers Stop for just that reply, and locks edit, retry and the arrows there", async () => {
    const user = userEvent.setup();
    show(conversation("b", "streaming so far"), running);

    await user.click(screen.getByRole("button", { name: "Stop" }));
    expect(cancelModelRequest).toHaveBeenCalledWith("turn-b");

    for (const button of screen.getAllByRole("button", { name: /^(Edit|Retry)$/ })) {
      expect(button).toBeDisabled();
    }
    // Every settled reply can still be forked.
    for (const button of screen.getAllByRole("button", { name: /^Fork$/ })) {
      expect(button).toBeEnabled();
    }
  });
});

describe("the composer's Message improver while a reply is running", () => {
  it("waits for it, shown as waiting with its own Cancel, then runs", async () => {
    resetAlertTracking();
    useStore.setState({
      modelRequests: [],
      plugins: [
        {
          id: "writing-tools",
          capabilities: ["writing-tools"],
          enabled: true,
          setup: { type: "ready" },
        } as unknown as PluginInfo,
      ],
      // The real queue, not the stand-in the other tests use.
      cancelModelRequest: useStore.getInitialState().cancelModelRequest,
    });
    // Another conversation's reply, at the head of the queue.
    let finishReply!: () => void;
    const reply = useStore
      .getState()
      .runModelRequest(
        { id: "reply", kind: "writingTool" },
        () => new Promise<void>((resolve) => (finishReply = resolve)),
      );
    const user = userEvent.setup();
    useStore.setState({ active: conversation("b", "settled answer") });
    render(<ChatView />);

    await user.type(screen.getByRole("textbox"), "make it clearer");
    await user.click(screen.getByRole("button", { name: /rewrites what you typed/ }));

    expect(
      screen.getByText("Waiting for the model to finish its current task…"),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Cancel this request" })).toBeInTheDocument();
    expect(enhancePrompt).not.toHaveBeenCalled();

    finishReply();
    await act(async () => {
      await reply;
    });

    expect(enhancePrompt).toHaveBeenCalledWith("make it clearer");
    expect(await screen.findByDisplayValue("A clearer ask.")).toBeInTheDocument();
  });
});
