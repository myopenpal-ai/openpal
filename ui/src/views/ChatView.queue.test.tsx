import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import ChatView from "./ChatView";
import { resetAlertTracking, useStore, type ModelRequest } from "../state/store";
import type {
  ChatEvent,
  Conversation,
  EngineStatus,
  ModelInfo,
  PluginInfo,
  Settings,
} from "../lib/types";
import { DEFAULT_SAMPLING } from "../lib/types";

const apiMocks = vi.hoisted(() => ({
  enhancePrompt: vi.fn().mockResolvedValue({ type: "enhanced", data: { text: "A clearer ask." } }),
  chatStream: vi.fn(),
  classifyRisk: vi.fn().mockResolvedValue(null),
  getParentalStatus: vi.fn().mockResolvedValue(null),
  listConversations: vi.fn().mockResolvedValue([]),
}));
const { enhancePrompt, chatStream, classifyRisk, getParentalStatus } = apiMocks;

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
          : (apiMocks[name as keyof typeof apiMocks] ?? vi.fn().mockResolvedValue(null)),
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

describe("sending before the model has loaded", () => {
  const installed = {
    spec: { id: "m", name: "Model M" },
    status: { type: "installed", data: { path: "/models/m.gguf", sizeBytes: 1 } },
  } as unknown as ModelInfo;

  /** The real `send` and queue, with the engine as given and `useModel`
   * standing in for a load: it only shows the model as starting, like the
   * real one does at once. */
  function showUnloaded(engine: EngineStatus, settings: Partial<Settings> = {}) {
    resetAlertTracking();
    chatStream.mockReset();
    classifyRisk.mockClear();
    const useModel = vi.fn(async (modelId: string) => {
      useStore.setState({ engine: { type: "starting", data: { modelId, progressPercent: null } } });
    });
    const initial = useStore.getInitialState();
    useStore.setState({
      engine,
      settings: {
        // Off, so it's Send — not typing — that starts the load.
        autoUnloadEnabled: false,
        chatBackend: "local",
        selectedModelId: "m",
        ...settings,
      } as Settings,
      models: [installed],
      conversations: [],
      active: conversation("b", "settled answer"),
      modelRequests: [],
      streaming: false,
      engineBusy: false,
      send: initial.send,
      cancelModelRequest: initial.cancelModelRequest,
      useModel,
    });
    render(<ChatView />);
    return { user: userEvent.setup(), useModel };
  }

  it("sends while unloaded: the message waits, then runs once the engine reports running", async () => {
    const { user, useModel } = showUnloaded({ type: "stopped" });
    let emit!: (event: ChatEvent) => void;
    let finish!: () => void;
    chatStream.mockImplementation(
      (_id: string, _messages: unknown, _ws: boolean, _at: boolean, _s: unknown, onEvent) =>
        new Promise<void>((resolve) => {
          emit = onEvent;
          finish = resolve;
        }),
    );

    await user.type(screen.getByRole("textbox"), "a question");
    const sendButton = screen.getByRole("button", { name: "Send" });
    expect(sendButton).toBeEnabled();
    await user.click(sendButton);

    expect(useModel).toHaveBeenCalledWith("m");
    expect(await screen.findByText("a question")).toBeInTheDocument();
    expect(screen.getByText("Waiting for the model to load…")).toBeInTheDocument();
    expect(chatStream).not.toHaveBeenCalled();
    expect(classifyRisk).not.toHaveBeenCalled();

    await act(async () => useStore.setState({ engine: RUNNING }));

    await waitFor(() => expect(chatStream).toHaveBeenCalledTimes(1));
    expect(classifyRisk).toHaveBeenCalledWith("a question");
    expect(screen.queryByText("Waiting for the model to load…")).not.toBeInTheDocument();
    act(() => emit({ type: "token", data: { text: "Here is the answer." } }));
    expect(await screen.findAllByText("Here is the answer.")).not.toHaveLength(0);

    await act(async () => finish());
  });

  it("keeps the message on screen, and shows why, when the load fails", async () => {
    const { user, useModel } = showUnloaded({ type: "stopped" });
    useModel.mockImplementationOnce(async () => {
      useStore.setState({ engine: { type: "failed", data: { message: "Not enough memory" } } });
    });

    await user.type(screen.getByRole("textbox"), "a question");
    await user.click(screen.getByRole("button", { name: "Send" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Not enough memory");
    expect(screen.getByText("a question")).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "Cancel this request" })).toHaveLength(2);
    expect(chatStream).not.toHaveBeenCalled();

    await act(async () => {
      await useStore.getState().cancelModelRequest(useStore.getState().modelRequests[0].id);
    });
  });

  it("still keeps Send disabled while the parental lock is on", async () => {
    getParentalStatus.mockResolvedValueOnce({ locked: true });
    const { user } = showUnloaded({ type: "stopped" });

    await user.type(screen.getByRole("textbox"), "a question");

    await waitFor(() => expect(screen.getByRole("button", { name: "Send" })).toBeDisabled());
  });

  it("with no model selected, takes the message and says to pick one", async () => {
    const { user, useModel } = showUnloaded({ type: "stopped" }, { selectedModelId: null });

    await user.type(screen.getByRole("textbox"), "a question");
    await user.click(screen.getByRole("button", { name: "Send" }));

    expect(useModel).not.toHaveBeenCalled();
    expect(
      await screen.findByText(/No model is ready yet\. Pick or download one in Models/),
    ).toBeInTheDocument();
    expect(chatStream).not.toHaveBeenCalled();

    await act(async () => {
      await useStore.getState().cancelModelRequest(useStore.getState().modelRequests[0].id);
    });
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
