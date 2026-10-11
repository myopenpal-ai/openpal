import { useEffect, useRef } from "react";

// How many panels using `useCloseOnOutside` are open right now.
let openPanels = 0;

/** Whether a popover or menu is open — useDialogFocus leaves Escape to it
 * rather than closing the dialog it was opened from: such a panel keeps
 * focus on its trigger (inside the dialog) and closes on the document-level
 * listener below, which the dialog's own listener would otherwise pre-empt. */
export function floatingPanelOpen(): boolean {
  return openPanels > 0;
}

/** Closes `onClose` on Escape or a pointerdown outside the returned ref's
 * subtree. Attach the returned ref to the floating panel's root element
 * (not the trigger button).
 *
 * `excludeRefs` additionally exempts other elements from counting as
 * "outside" — needed when the panel is rendered through a portal (e.g.
 * Sidebar.tsx's `ConvRowMenu`) so the trigger button, which then sits
 * outside the panel's own DOM subtree, doesn't register its own click as an
 * outside click and immediately reopen what it just closed. */
export function useCloseOnOutside<T extends HTMLElement>(
  open: boolean,
  onClose: () => void,
  excludeRefs?: Array<React.RefObject<HTMLElement | null>>,
): React.RefObject<T | null> {
  const ref = useRef<T>(null);
  useEffect(() => {
    if (!open) return;
    openPanels += 1;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    const onPointerDown = (e: MouseEvent) => {
      const target = e.target as Node;
      if (ref.current && ref.current.contains(target)) return;
      if (excludeRefs?.some((r) => r.current?.contains(target))) return;
      onClose();
    };
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("mousedown", onPointerDown);
    return () => {
      openPanels -= 1;
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("mousedown", onPointerDown);
    };
  }, [open, onClose, excludeRefs]);
  return ref;
}

/**
 * Closes a portalled, fixed-position panel when its anchor scrolls away —
 * such a panel has no sane position to track once the thing it points at has
 * moved, so it closes rather than drifting.
 *
 * Non-bubbling `scroll` events still reach a capture-phase window listener on
 * their way down to the actual scrolled element, which is what lets one
 * listener catch scrolling inside any container.
 *
 * **Arms on the next frame, deliberately.** Opening a panel from a trigger
 * that is partly below the fold makes the browser scroll it into view first,
 * and that scroll event lands just after the panel mounts — closing it in the
 * same breath it opened. Waiting a frame lets the opening scroll settle
 * before the listener starts acting on scrolls.
 */
export function useCloseOnScroll(open: boolean, onClose: () => void): void {
  // Held in a ref rather than an effect dependency: callers rebuild their
  // `close` on every render, and re-running the effect would re-arm the
  // listener each time, which is exactly what this is trying to avoid.
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useEffect(() => {
    if (!open) return;
    let armed = false;
    const frame = requestAnimationFrame(() => {
      armed = true;
    });
    const onScroll = () => {
      if (armed) onCloseRef.current();
    };
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onScroll);
    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onScroll);
    };
  }, [open]);
}
