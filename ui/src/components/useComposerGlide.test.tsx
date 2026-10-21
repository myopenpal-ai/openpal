import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";
import { api } from "../lib/api";
import type { ChatMessage, Conversation } from "../lib/types";
import { useStore } from "../state/store";
import { useComposerGlide } from "./useComposerGlide";

vi.mock("../lib/api", () => ({ api: { getConversation: vi.fn() } }));

/** Stands in for ChatView: the empty-chat layout is `data-empty`, and — jsdom
 * having no layout — a stubbed `getBoundingClientRect` below reports the
 * composer mid-screen while that's set and at the bottom once it isn't. */
function Harness() {
  const active = useStore((s) => s.active);
  const isEmpty = (active?.messages.length ?? 0) === 0;
  const ref = useComposerGlide<HTMLDivElement>(isEmpty);
  return <div data-testid="slot" ref={ref} data-empty={isEmpty} />;
}

const MID_SCREEN = 300;
const BOTTOM = 700;

const message = (role: ChatMessage["role"], content: string): ChatMessage => ({
  id: crypto.randomUUID(),
  role,
  content,
  createdAtMs: 0,
});

const conversation = (id: string, messages: ChatMessage[] = []): Conversation => ({
  id,
  title: "New chat",
  createdAtMs: 0,
  updatedAtMs: 0,
  messages,
  sampling: {} as Conversation["sampling"],
  contextLength: 4096,
  taskType: "general",
  pinned: false,
  branches: {},
});

const animate = vi.fn();

/** The composer's slot, as the browser would lay it out for each state. */
function mount() {
  const rect = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (
    this: HTMLElement,
  ) {
    const top = this.dataset.empty === "true" ? MID_SCREEN : BOTTOM;
    return { top } as DOMRect;
  });
  Object.defineProperty(HTMLElement.prototype, "animate", {
    configurable: true,
    writable: true,
    value: animate,
  });
  render(<Harness />);
  return rect;
}

/** A change of conversation that is not a send: a chat opened, started or emptied. */
function setActive(active: Conversation | null) {
  act(() => useStore.setState({ active }));
}

/** What `send()` does to the store: the message and its queued request in
 * one update. */
function sendFirstMessage(active: Conversation) {
  act(() =>
    useStore.setState({
      active,
      modelRequests: [
        { id: `turn-${active.id}`, kind: "chat", conversationId: active.id, status: "running" },
      ],
      streaming: true,
    }),
  );
}

/** The turn settling: its request leaves the queue. */
function settle() {
  act(() => useStore.setState({ modelRequests: [], streaming: false }));
}

function reducedMotion(on: boolean) {
  window.matchMedia = vi.fn().mockReturnValue({ matches: on }) as unknown as typeof window.matchMedia;
}

beforeEach(() => {
  animate.mockReset();
  reducedMotion(false);
  useStore.setState({ active: conversation("a"), modelRequests: [], streaming: false });
});

afterEach(() => {
  vi.restoreAllMocks();
  Reflect.deleteProperty(HTMLElement.prototype, "animate");
  Reflect.deleteProperty(window, "matchMedia");
});

describe("useComposerGlide", () => {
  it("glides the composer from where it was to where it lands when the first message arrives", () => {
    mount();
    expect(screen.getByTestId("slot")).toHaveAttribute("data-empty", "true");

    sendFirstMessage(conversation("a", [message("user", "hi"), message("assistant", "")]));

    expect(screen.getByTestId("slot")).toHaveAttribute("data-empty", "false");
    expect(animate).toHaveBeenCalledTimes(1);
    // Starts back up where it was (300 - 700 = 400px above its new place)
    // and comes to rest.
    expect(animate.mock.calls[0][0]).toEqual([
      { transform: `translateY(${MID_SCREEN - BOTTOM}px)` },
      { transform: "none" },
    ]);
    // Without a duration the animation would be instant, and nothing would show.
    expect(animate.mock.calls[0][1]).toEqual({
      duration: 450,
      easing: expect.stringMatching(/^cubic-bezier\(/),
    });
  });

  it("starts from where the composer is when the message lands, not where it was last rendered", () => {
    mount();
    // E.g. the window was resized, or a long draft grew the composer, since
    // the last render — nothing re-rendered, so only a live measure sees it.
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (
      this: HTMLElement,
    ) {
      return { top: this.dataset.empty === "true" ? 250 : BOTTOM } as DOMRect;
    });

    sendFirstMessage(conversation("a", [message("user", "hi")]));

    expect(animate.mock.calls[0][0][0]).toEqual({ transform: `translateY(${250 - BOTTOM}px)` });
  });

  it("glides on the very first send, which creates the conversation as it goes", () => {
    useStore.setState({ active: null });
    mount();

    sendFirstMessage(conversation("fresh", [message("user", "hi")]));

    expect(animate).toHaveBeenCalledTimes(1);
  });

  it("does not glide on mount, whether the chat is empty or not", () => {
    mount();
    expect(animate).not.toHaveBeenCalled();
  });

  it("does not glide when opening a conversation that already has messages", () => {
    mount();

    setActive(conversation("b", [message("user", "hi"), message("assistant", "hello")]));

    expect(screen.getByTestId("slot")).toHaveAttribute("data-empty", "false");
    expect(animate).not.toHaveBeenCalled();
  });

  it("does not glide when a saved chat is opened from the empty screen at launch, where nothing is active yet", async () => {
    useStore.setState({ active: null });
    mount();
    vi.mocked(api.getConversation).mockResolvedValue(
      conversation("saved", [message("user", "hi"), message("assistant", "hello")]),
    );

    await act(() => useStore.getState().openConversation("saved"));

    expect(useStore.getState().active?.id).toBe("saved");
    expect(screen.getByTestId("slot")).toHaveAttribute("data-empty", "false");
    expect(animate).not.toHaveBeenCalled();
  });

  it("does not glide when the open chat is deleted and another one opened", () => {
    mount();
    setActive(null);

    setActive(conversation("b", [message("user", "hi")]));

    expect(animate).not.toHaveBeenCalled();
  });

  it("does not glide when the chat empties again, or on later messages", () => {
    mount();
    sendFirstMessage(conversation("a", [message("user", "hi")]));
    animate.mockClear();

    sendFirstMessage(conversation("a", [message("user", "hi"), message("assistant", "hello")]));
    setActive(conversation("c"));

    expect(animate).not.toHaveBeenCalled();
  });

  it("does not carry a send's start position over into a chat opened later", () => {
    mount();
    sendFirstMessage(conversation("a", [message("user", "hi")]));
    settle();
    setActive(conversation("c"));
    animate.mockClear();

    setActive(conversation("b", [message("user", "earlier"), message("assistant", "reply")]));

    expect(animate).not.toHaveBeenCalled();
  });

  it("stops a glide still under way when a new chat is started", () => {
    const cancel = vi.fn();
    animate.mockReturnValue({ cancel });
    mount();
    sendFirstMessage(conversation("a", [message("user", "hi")]));
    expect(animate).toHaveBeenCalledTimes(1);
    expect(cancel).not.toHaveBeenCalled();

    settle();
    setActive(conversation("c"));

    expect(screen.getByTestId("slot")).toHaveAttribute("data-empty", "true");
    expect(cancel).toHaveBeenCalledTimes(1);
  });

  it("only moves for a reader who hasn't asked for reduced motion", () => {
    reducedMotion(true);
    mount();

    sendFirstMessage(conversation("a", [message("user", "hi")]));

    expect(screen.getByTestId("slot")).toHaveAttribute("data-empty", "false");
    expect(animate).not.toHaveBeenCalled();
  });

  it("stays put when the composer didn't actually move", () => {
    const rect = mount();
    rect.mockImplementation(() => ({ top: BOTTOM }) as DOMRect);

    sendFirstMessage(conversation("a", [message("user", "hi")]));

    expect(animate).not.toHaveBeenCalled();
  });

  it("stops listening for sends once unmounted", () => {
    const unsubscribe = vi.fn();
    const subscribe = vi.spyOn(useStore, "subscribe").mockReturnValue(unsubscribe);
    const view = render(<Harness />);
    expect(subscribe).toHaveBeenCalledTimes(1);

    view.unmount();

    expect(unsubscribe).toHaveBeenCalledTimes(1);
  });
});
