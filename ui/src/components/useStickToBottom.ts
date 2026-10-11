import { useCallback, useRef, useState, type TouchEvent, type WheelEvent } from "react";

/** How close (px) to the bottom still counts as "at the bottom": scrolling
 * back down into this zone re-attaches auto-follow and hides the jump
 * button. */
const AT_BOTTOM_PX = 32;

function distanceFromBottom(el: HTMLElement): number {
  return el.scrollHeight - el.scrollTop - el.clientHeight;
}

/**
 * Chat-style "stick to the bottom" scrolling. While attached, `follow()`
 * keeps the latest content in view; any upward scroll by the user detaches
 * it at once, and scrolling back down to the bottom (or `scrollToBottom()`)
 * re-attaches it. `showJumpButton` is true while detached and away from the
 * bottom.
 *
 * Detaching has to win against a reply streaming in a token every few ms:
 * each token re-snaps an attached view to the bottom, so a small wheel or
 * trackpad step could be undone before any scroll event reported it (the
 * old "within 120px of the bottom counts as pinned" check never saw the
 * user leave, making it impossible to scroll up mid-reply). So:
 *  - an upward wheel or a downward finger drag detaches synchronously,
 *    before the browser even applies the scroll;
 *  - `onScroll` catches the rest (scrollbar drags, keyboard) by direction —
 *    auto-follow only ever moves the view down, so moving up is the user;
 *  - `follow()` itself re-checks that direction before snapping, in case
 *    the scroll landed but its event hasn't been dispatched yet.
 */
export function useStickToBottom<T extends HTMLElement>() {
  const scrollRef = useRef<T>(null);
  const attached = useRef(true);
  const lastScrollTop = useRef(0);
  const touchY = useRef<number | null>(null);
  const [showJumpButton, setShowJumpButton] = useState(false);
  // Mirrors `showJumpButton` so an unchanged value never reaches setState.
  // `follow()` runs after every streamed token, and a same-value setState
  // there isn't free: right after a store-driven render React often can't
  // bail out early, so it queues a re-render per token. Under a fast stream
  // those pile up as nested updates until React throws "Maximum update
  // depth exceeded" — from inside the stream's event handler, stalling the
  // reply.
  const jumpShown = useRef(false);

  const setJumpButton = useCallback((show: boolean) => {
    if (jumpShown.current === show) return;
    jumpShown.current = show;
    setShowJumpButton(show);
  }, []);

  const syncJumpButton = useCallback(() => {
    const el = scrollRef.current;
    setJumpButton(!!el && !attached.current && distanceFromBottom(el) > AT_BOTTOM_PX);
  }, [setJumpButton]);

  // Moving up while not at the very bottom can only be the user. Content
  // shrinking under an attached view (a block collapsing) clamps scrollTop
  // down too, but leaves it at the bottom, so that alone doesn't count.
  const userMovedUp = (el: HTMLElement) =>
    el.scrollTop < lastScrollTop.current && distanceFromBottom(el) > 1;

  const snapToBottom = (el: HTMLElement) => {
    el.scrollTop = el.scrollHeight;
    lastScrollTop.current = el.scrollTop;
  };

  // Only when there's somewhere to go: an upward gesture on a view that's
  // already at the top (or too short to scroll yet) mustn't stop it
  // following a reply that later overflows.
  const detachIfScrollable = useCallback(() => {
    const el = scrollRef.current;
    if (el && el.scrollTop > 0) attached.current = false;
  }, []);

  /** Detach without scrolling — for parking the view on an older message. */
  const detach = useCallback(() => {
    attached.current = false;
    syncJumpButton();
  }, [syncJumpButton]);

  /** Call after content changes: snaps back to the bottom if attached. */
  const follow = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    if (attached.current && userMovedUp(el)) attached.current = false;
    if (attached.current) snapToBottom(el);
    syncJumpButton();
  }, [syncJumpButton]);

  /** Re-attach and go to the bottom — instantly unless `smooth`. */
  const scrollToBottom = useCallback((smooth = false) => {
    attached.current = true;
    setJumpButton(false);
    const el = scrollRef.current;
    if (!el) return;
    const reducedMotion = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    if (smooth && !reducedMotion) el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
    else snapToBottom(el);
  }, [setJumpButton]);

  const onScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    if (userMovedUp(el)) attached.current = false;
    else if (distanceFromBottom(el) <= AT_BOTTOM_PX) attached.current = true;
    lastScrollTop.current = el.scrollTop;
    syncJumpButton();
  }, [syncJumpButton]);

  const onWheel = useCallback(
    (e: WheelEvent) => {
      if (e.deltaY < 0) detachIfScrollable();
    },
    [detachIfScrollable],
  );

  const onTouchStart = useCallback((e: TouchEvent) => {
    touchY.current = e.touches[0]?.clientY ?? null;
  }, []);

  const onTouchMove = useCallback(
    (e: TouchEvent) => {
      const y = e.touches[0]?.clientY ?? null;
      // A finger moving down drags the content down, i.e. scrolls up.
      if (touchY.current != null && y != null && y > touchY.current) detachIfScrollable();
      touchY.current = y;
    },
    [detachIfScrollable],
  );

  return {
    scrollRef,
    showJumpButton,
    follow,
    detach,
    scrollToBottom,
    scrollHandlers: { onScroll, onWheel, onTouchStart, onTouchMove },
  };
}
