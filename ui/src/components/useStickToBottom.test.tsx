import { useEffect } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { create } from "zustand";
import { useStickToBottom } from "./useStickToBottom";

type StickToBottom = ReturnType<typeof useStickToBottom<HTMLDivElement>>;

let hook: StickToBottom;

function Harness() {
  hook = useStickToBottom<HTMLDivElement>();
  return (
    <>
      <div data-testid="scroller" ref={hook.scrollRef} {...hook.scrollHandlers} />
      {hook.showJumpButton && (
        <button onClick={() => hook.scrollToBottom(true)}>jump</button>
      )}
    </>
  );
}

// jsdom has no layout: fake a 400px-tall scroll box whose content height the
// test controls, clamping scrollTop the way a real browser does.
const VIEWPORT = 400;
let contentHeight = 0;
let top = 0;
const maxTop = () => Math.max(0, contentHeight - VIEWPORT);

function mount(initialContentHeight: number) {
  contentHeight = initialContentHeight;
  top = 0;
  render(<Harness />);
  const el = screen.getByTestId("scroller");
  Object.defineProperty(el, "clientHeight", { get: () => VIEWPORT });
  Object.defineProperty(el, "scrollHeight", { get: () => contentHeight });
  Object.defineProperty(el, "scrollTop", {
    get: () => top,
    set: (v: number) => {
      top = Math.min(Math.max(0, v), maxTop());
    },
  });
  el.scrollTo = vi.fn();
  return el;
}

/** New content rendered (or removed), then the effect that reacts to it. */
function contentChanges(height: number) {
  contentHeight = height;
  top = Math.min(top, maxTop());
  act(() => hook.follow());
}

/** The user scrolls to `to` (the browser applies it, then fires `scroll`). */
function userScrolls(el: HTMLElement, to: number) {
  el.scrollTop = to;
  fireEvent.scroll(el);
}

describe("useStickToBottom", () => {
  beforeEach(() => {
    contentHeight = 0;
    top = 0;
  });

  it("follows new content while attached", () => {
    mount(1000);
    contentChanges(1000);
    expect(top).toBe(600);
    contentChanges(1200);
    expect(top).toBe(800);
    expect(screen.queryByText("jump")).toBeNull();
  });

  it("an upward wheel detaches right away, so the next token doesn't snap back", () => {
    const el = mount(1000);
    contentChanges(1000);
    // Only a few px up — well inside the old 120px "still pinned" zone.
    fireEvent.wheel(el, { deltaY: -8 });
    userScrolls(el, 592);
    contentChanges(1300);
    expect(top).toBe(592);
    expect(screen.getByText("jump")).toBeInTheDocument();
  });

  it("detaches on a wheel even if a token re-snapped before the scroll event", () => {
    const el = mount(1000);
    contentChanges(1000);
    fireEvent.wheel(el, { deltaY: -40 });
    contentChanges(1100);
    expect(top).toBe(600);
  });

  it("a scroll up without a wheel (scrollbar drag, keyboard) detaches too", () => {
    const el = mount(1000);
    contentChanges(1000);
    userScrolls(el, 590);
    contentChanges(1300);
    expect(top).toBe(590);
  });

  it("respects a scroll that landed before its scroll event fired", () => {
    mount(1000);
    contentChanges(1000);
    top = 550;
    contentChanges(1300);
    expect(top).toBe(550);
  });

  it("a finger dragging down (scrolling up) detaches", () => {
    const el = mount(1000);
    contentChanges(1000);
    fireEvent.touchStart(el, { touches: [{ clientY: 100 }] });
    fireEvent.touchMove(el, { touches: [{ clientY: 130 }] });
    contentChanges(1300);
    expect(top).toBe(600);
  });

  it("content shrinking under an attached view doesn't detach it", () => {
    const el = mount(1000);
    contentChanges(1000);
    // e.g. the thinking trace collapsing as the answer starts: the browser
    // clamps scrollTop down, which fires a scroll event.
    contentHeight = 700;
    top = maxTop();
    fireEvent.scroll(el);
    contentChanges(900);
    expect(top).toBe(500);
  });

  it("an upward wheel before the conversation overflows doesn't stop it following", () => {
    const el = mount(300);
    fireEvent.wheel(el, { deltaY: -20 });
    contentChanges(1000);
    expect(top).toBe(600);
  });

  it("scrolling back down to the bottom re-attaches and hides the button", () => {
    const el = mount(1000);
    contentChanges(1000);
    fireEvent.wheel(el, { deltaY: -100 });
    userScrolls(el, 200);
    expect(screen.getByText("jump")).toBeInTheDocument();
    userScrolls(el, 590);
    expect(screen.queryByText("jump")).toBeNull();
    contentChanges(1200);
    expect(top).toBe(800);
  });

  it("the jump button smooth-scrolls to the bottom and re-attaches", () => {
    const el = mount(1000);
    contentChanges(1000);
    fireEvent.wheel(el, { deltaY: -100 });
    userScrolls(el, 200);
    fireEvent.click(screen.getByText("jump"));
    expect(el.scrollTo).toHaveBeenCalledWith({ top: 1000, behavior: "smooth" });
    expect(screen.queryByText("jump")).toBeNull();
    // Attached again: the next token snaps to the bottom.
    contentChanges(1200);
    expect(top).toBe(800);
  });

  it("detach() parks the view without scrolling it", () => {
    const el = mount(1000);
    contentChanges(1000);
    act(() => hook.detach());
    userScrolls(el, 300);
    contentChanges(1200);
    expect(top).toBe(300);
    expect(screen.getByText("jump")).toBeInTheDocument();
  });

  it("keeps up with a burst of streamed tokens", async () => {
    // The chat's real shape: each token is a store update, and an effect
    // follows the new content. Delivered back-to-back (no macrotask for
    // React to catch up in between), a setState per follow() — even an
    // unchanged one — used to pile up until React threw "Maximum update
    // depth exceeded" a few dozen tokens in.
    const useTokens = create<{ count: number }>(() => ({ count: 0 }));
    function Stream() {
      const count = useTokens((s) => s.count);
      const { scrollRef, follow, scrollHandlers } = useStickToBottom<HTMLDivElement>();
      useEffect(() => follow(), [count, follow]);
      return (
        <div ref={scrollRef} {...scrollHandlers}>
          tokens: {count}
        </div>
      );
    }
    const env = globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean };
    const wasActEnv = env.IS_REACT_ACT_ENVIRONMENT;
    // Real scheduling, not act()'s batched queue — the pile-up only happens
    // with React's own microtask flush between tokens.
    env.IS_REACT_ACT_ENVIRONMENT = false;
    const errors: unknown[] = [];
    const onError = (e: ErrorEvent) => errors.push(e.error);
    window.addEventListener("error", onError);
    try {
      render(<Stream />);
      for (let i = 1; i <= 200; i++) {
        try {
          useTokens.setState({ count: i });
        } catch (err) {
          errors.push(err);
          break;
        }
        await Promise.resolve();
      }
      await new Promise((r) => setTimeout(r, 0));
      expect(errors).toEqual([]);
      expect(screen.getByText("tokens: 200")).toBeInTheDocument();
    } finally {
      window.removeEventListener("error", onError);
      env.IS_REACT_ACT_ENVIRONMENT = wasActEnv;
    }
  });
});
