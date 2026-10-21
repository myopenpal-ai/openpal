import { beforeEach, describe, expect, it, vi } from "vitest";
import { chatRequestFor, resetAlertTracking, useStore } from "./store";
import type { ChatEvent, ChatMessage, Conversation, ConversationMeta } from "../lib/types";
import { DEFAULT_SAMPLING } from "../lib/types";

const {
  chatStream,
  stopGeneration,
  classifyRisk,
  saveConversation,
  listConversations,
  getConversation,
  deleteConversation,
} = vi.hoisted(() => ({
  chatStream: vi.fn(),
  stopGeneration: vi.fn().mockResolvedValue(undefined),
  classifyRisk: vi.fn().mockResolvedValue(null),
  saveConversation: vi.fn().mockResolvedValue(undefined),
  listConversations: vi.fn().mockResolvedValue([]),
  getConversation: vi.fn(),
  deleteConversation: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("../lib/api", () => ({
  api: {
    chatStream,
    stopGeneration,
    classifyRisk,
    saveConversation,
    listConversations,
    getConversation,
    deleteConversation,
  },
}));

/** One `api.chatStream` call the test drives by hand: what Rust would have
 * streamed back, and when the turn ends. */
interface Stream {
  conversationId: string;
  messages: ChatMessage[];
  emit: (event: ChatEvent) => void;
  finish: () => Promise<void>;
}

let streams: Stream[] = [];

/** Let every pending promise continuation run. */
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

function token(text: string): ChatEvent {
  return { type: "token", data: { text } };
}

const DONE: ChatEvent = {
  type: "done",
  data: { stats: { totalTokens: 3, tokensPerSec: 5, durationMs: 100 } },
};

function conversation(id: string, messages: ChatMessage[] = []): Conversation {
  return {
    id,
    title: `Chat ${id}`,
    createdAtMs: 0,
    updatedAtMs: 0,
    messages,
    sampling: DEFAULT_SAMPLING,
    contextLength: 4096,
    taskType: "general",
    branches: {},
    pinned: false,
  } as Conversation;
}

function meta(id: string): ConversationMeta {
  return {
    id,
    title: `Chat ${id}`,
    createdAtMs: 0,
    updatedAtMs: 0,
    messageCount: 2,
    taskType: "general",
    pinned: false,
  };
}

/** An existing, listed conversation with one finished exchange. */
function existing(id: string): Conversation {
  return conversation(id, [
    { id: `${id}-u0`, role: "user", content: "earlier question" },
    { id: `${id}-a0`, role: "assistant", content: "earlier answer" },
  ]);
}

/** Show `id` (already listed) and send `text` in it, without waiting for
 * the turn to settle — `settled` is that. (Wrapped: an async function
 * returning the promise itself would wait for it.) */
async function sendIn(id: string, text: string) {
  useStore.setState({ active: existing(id) });
  const settled = useStore.getState().send(text);
  await flush();
  return { settled };
}

function requests() {
  return useStore.getState().modelRequests.map((r) => [r.conversationId ?? r.kind, r.status]);
}

beforeEach(() => {
  resetAlertTracking();
  streams = [];
  chatStream.mockReset();
  chatStream.mockImplementation(
    (
      conversationId: string,
      messages: ChatMessage[],
      _webSearch: boolean,
      _agentTools: boolean,
      _sampling: unknown,
      onEvent: (e: ChatEvent) => void,
    ) =>
      new Promise<void>((resolve) => {
        streams.push({
          conversationId,
          messages,
          emit: onEvent,
          finish: async () => {
            resolve();
            await flush();
          },
        });
      }),
  );
  stopGeneration.mockClear();
  classifyRisk.mockClear();
  classifyRisk.mockResolvedValue(null);
  saveConversation.mockClear();
  listConversations.mockReset();
  listConversations.mockResolvedValue([meta("a"), meta("b"), meta("c")]);
  getConversation.mockReset();
  getConversation.mockImplementation(async (id: string) => existing(id));
  deleteConversation.mockClear();
  useStore.setState({
    active: null,
    conversations: [meta("a"), meta("b"), meta("c")],
    modelRequests: [],
    streaming: false,
    chatError: null,
    webSearch: false,
    agentTools: false,
    settings: null,
    plugins: [],
    models: [],
    safetyAlerts: {},
  });
});

describe("queued chat turns", () => {
  it("a message sent while another reply runs waits in its own conversation, then runs in order", async () => {
    const first = await sendIn("a", "first");
    const second = await sendIn("b", "second");
    const third = await sendIn("c", "third");

    expect(requests()).toEqual([
      ["a", "running"],
      ["b", "waiting"],
      ["c", "waiting"],
    ]);
    expect(chatStream).toHaveBeenCalledTimes(1);
    // Shown in its own conversation: the message, and an empty reply to come.
    const shown = useStore.getState().active!;
    expect(shown.id).toBe("c");
    expect(shown.messages.slice(-2).map((m) => [m.role, m.content])).toEqual([
      ["user", "third"],
      ["assistant", ""],
    ]);

    streams[0].emit(DONE);
    await streams[0].finish();
    expect(streams.map((s) => s.conversationId)).toEqual(["a", "b"]);
    expect(requests()).toEqual([
      ["b", "running"],
      ["c", "waiting"],
    ]);

    await streams[1].finish();
    expect(streams.map((s) => s.conversationId)).toEqual(["a", "b", "c"]);
    // Each turn sends its own conversation's history, not the one on screen.
    expect(streams[1].messages.at(-1)?.content).toBe("second");

    await streams[2].finish();
    await Promise.all([first.settled, second.settled, third.settled]);
    expect(useStore.getState().modelRequests).toEqual([]);
    expect(useStore.getState().streaming).toBe(false);
  });

  it("refuses a second send in the conversation whose own turn is in flight", async () => {
    await sendIn("a", "first");
    expect(await useStore.getState().send("again")).toBeUndefined();
    expect(useStore.getState().modelRequests).toHaveLength(1);
    await streams[0].finish();
  });

  it("cancels a waiting turn on its own: it never reaches the model and the next one runs", async () => {
    await sendIn("a", "first");
    const second = await sendIn("b", "second");
    await sendIn("c", "third");
    const waiting = chatRequestFor(useStore.getState(), "b")!;

    await useStore.getState().cancelModelRequest(waiting.id);
    const settled = await second.settled;

    expect(stopGeneration).not.toHaveBeenCalled();
    expect(requests()).toEqual([
      ["a", "running"],
      ["c", "waiting"],
    ]);
    // Settled like a reply stopped before its first token: the question
    // stays, the empty reply goes.
    expect(settled?.messages.at(-1)).toEqual(
      expect.objectContaining({ role: "user", content: "second" }),
    );

    await streams[0].finish();
    expect(streams.map((s) => s.conversationId)).toEqual(["a", "c"]);
    await streams[1].finish();
  });

  it("Stop halts only the running reply, and the next waiting one then starts", async () => {
    await sendIn("a", "first");
    await sendIn("b", "second");
    const running = chatRequestFor(useStore.getState(), "a")!;

    await useStore.getState().cancelModelRequest(running.id);
    expect(stopGeneration).toHaveBeenCalledTimes(1);
    expect(chatRequestFor(useStore.getState(), "b")?.status).toBe("waiting");

    // What Rust does on Stop: end the stream with a normal `done`.
    streams[0].emit(DONE);
    await streams[0].finish();
    expect(streams.map((s) => s.conversationId)).toEqual(["a", "b"]);
    expect(chatRequestFor(useStore.getState(), "b")?.status).toBe("running");
    await streams[1].finish();
  });

  it("runs the safety check of a queued message once the reply ahead of it is done, before its own request", async () => {
    useStore.setState({ settings: { riskDetectionEnabled: true } as never });
    await sendIn("a", "first");
    expect(classifyRisk).toHaveBeenCalledTimes(1);

    classifyRisk.mockResolvedValueOnce("risk");
    await sendIn("b", "I feel hopeless");
    // Nothing would answer it while a reply is generating.
    expect(classifyRisk).toHaveBeenCalledTimes(1);

    await streams[0].finish();
    expect(classifyRisk).toHaveBeenCalledTimes(2);
    expect(classifyRisk).toHaveBeenLastCalledWith("I feel hopeless");
    expect(classifyRisk.mock.invocationCallOrder[1]).toBeLessThan(
      chatStream.mock.invocationCallOrder[1],
    );
    expect(useStore.getState().safetyAlerts.b).toEqual({ turn: 3, dismissedTurn: null });
    await streams[1].finish();
  });
});

describe("switching conversations while a reply runs", () => {
  it("keeps streaming into its own conversation, shows it live on return, and saves it there", async () => {
    const { settled } = await sendIn("a", "tell me a story");
    streams[0].emit(token("Once upon"));

    await useStore.getState().openConversation("b");
    expect(useStore.getState().active?.id).toBe("b");
    streams[0].emit(token(" a time"));
    // Nothing leaks into the conversation on screen.
    expect(useStore.getState().active?.messages.map((m) => m.content)).toEqual([
      "earlier question",
      "earlier answer",
    ]);

    // Back mid-reply: shown as the turn has it, not as last saved.
    await useStore.getState().openConversation("a");
    expect(getConversation).toHaveBeenCalledTimes(1);
    expect(useStore.getState().active?.messages.at(-1)?.content).toBe("Once upon a time");
    streams[0].emit(token("."));
    expect(useStore.getState().active?.messages.at(-1)?.content).toBe("Once upon a time.");

    // Away again when it ends: it's saved into its own conversation.
    useStore.getState().newChat();
    streams[0].emit(DONE);
    await streams[0].finish();
    await settled;
    const saved = saveConversation.mock.calls.at(-1)![0] as Conversation;
    expect(saved.id).toBe("a");
    expect(saved.messages.at(-1)).toEqual(
      expect.objectContaining({ role: "assistant", content: "Once upon a time." }),
    );

    // And complete when the user returns to it.
    getConversation.mockResolvedValueOnce(saved);
    await useStore.getState().openConversation("a");
    expect(useStore.getState().active?.messages.at(-1)?.content).toBe("Once upon a time.");
  });

  it("keeps an error for its own conversation until the user is back there", async () => {
    await sendIn("a", "first");
    await useStore.getState().openConversation("b");
    streams[0].emit({ type: "error", data: { message: "The reply was cut off." } });
    await streams[0].finish();
    expect(useStore.getState().chatError).toBeNull();

    await useStore.getState().openConversation("a");
    expect(useStore.getState().chatError).toBe("The reply was cut off.");
  });

  it("saves a brand-new chat that has to wait, so the Sidebar can lead back to it", async () => {
    await sendIn("a", "first");
    useStore.getState().newChat();
    const newId = useStore.getState().active!.id;
    const second = useStore.getState().send("a new topic");
    await flush();

    const early = saveConversation.mock.calls.at(-1)![0] as Conversation;
    expect(early.id).toBe(newId);
    expect(early.messages.map((m) => [m.role, m.content])).toEqual([["user", "a new topic"]]);

    // Cancelled before it ran: nothing came of it, so it's taken back.
    await useStore.getState().cancelModelRequest(chatRequestFor(useStore.getState(), newId)!.id);
    await second;
    expect(deleteConversation).toHaveBeenCalledWith(newId);
    await streams[0].finish();
  });

  it("deleting a conversation cancels its waiting turn and never saves it back", async () => {
    await sendIn("a", "first");
    const second = await sendIn("b", "second");
    saveConversation.mockClear();

    await useStore.getState().deleteConversation("b");
    await second.settled;

    expect(deleteConversation).toHaveBeenCalledWith("b");
    expect(chatRequestFor(useStore.getState(), "b")).toBeUndefined();
    await streams[0].finish();
    expect(streams).toHaveLength(1);
    expect(saveConversation.mock.calls.every(([c]) => (c as Conversation).id !== "b")).toBe(true);
  });

  it("deleting a conversation stops its running turn and never saves it back", async () => {
    const first = await sendIn("a", "first");
    streams[0].emit(token("partial"));
    saveConversation.mockClear();

    await useStore.getState().deleteConversation("a");
    expect(stopGeneration).toHaveBeenCalledTimes(1);
    streams[0].emit(DONE);
    await streams[0].finish();
    await first.settled;

    expect(saveConversation).not.toHaveBeenCalled();
    expect(useStore.getState().active).toBeNull();
  });
});

describe("runModelRequest", () => {
  it("waits behind a running chat reply, then runs", async () => {
    await sendIn("a", "first");
    const job = vi.fn().mockResolvedValue("translated");
    const result = useStore.getState().runModelRequest({ id: "wt-1", kind: "writingTool" }, job);
    await flush();

    expect(job).not.toHaveBeenCalled();
    expect(requests()).toEqual([
      ["a", "running"],
      ["writingTool", "waiting"],
    ]);

    await streams[0].finish();
    expect(await result).toBe("translated");
    expect(job).toHaveBeenCalledTimes(1);
    expect(useStore.getState().modelRequests).toEqual([]);
  });

  it("holds a chat sent meanwhile until it's done", async () => {
    let finishJob!: () => void;
    const result = useStore
      .getState()
      .runModelRequest(
        { id: "improve-1", kind: "improve" },
        () => new Promise<void>((resolve) => (finishJob = resolve)),
      );
    await flush();
    await sendIn("a", "after the improve");
    expect(chatStream).not.toHaveBeenCalled();

    finishJob();
    await result;
    await flush();
    expect(chatStream).toHaveBeenCalledTimes(1);
    await streams[0].finish();
  });

  it("resolves to undefined, without running, when cancelled while it waits", async () => {
    await sendIn("a", "first");
    const job = vi.fn();
    const result = useStore.getState().runModelRequest({ id: "wt-2", kind: "writingTool" }, job);
    await flush();

    await useStore.getState().cancelModelRequest("wt-2");
    expect(await result).toBeUndefined();
    expect(job).not.toHaveBeenCalled();
    await streams[0].finish();
  });
});
