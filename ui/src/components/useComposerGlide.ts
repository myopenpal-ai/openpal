import { useEffect, useLayoutEffect, useRef } from "react";
import { useStore } from "../state/store";

/** How long the composer takes to settle into its place at the bottom. */
const GLIDE_MS = 450;

/**
 * An empty chat shows the composer mid-screen, under the greeting; once the
 * conversation has a message it lives at the bottom like any chat. This
 * glides it from the one to the other when the first message is sent, instead
 * of letting it jump.
 *
 * It's a FLIP: the layout change itself is plain CSS (`.chat.is-empty`), so
 * all that's needed is where the composer *was*. That has to be read before
 * React commits the new layout, and a store subscriber is the one place that
 * is guaranteed to run ahead of it — it fires inside `set()`, while React's own
 * re-render is only ever scheduled from there, never run in it. Measuring in an
 * effect instead would see a position that's stale by whatever happened since
 * the last render, such as the window being resized or a long draft growing the
 * composer. Once the layout effect finds the composer at its new place it
 * animates from the old one.
 *
 * Only a send out of an empty chat glides. Opening a conversation, or starting
 * a new chat, still swaps instantly, and so does a reader who has asked the OS
 * for reduced motion. A send is told apart from an open by `streaming`:
 * `send()` flips it in the very update that adds the first message, whereas
 * opening a conversation also takes `active` from nothing to something, as it
 * does from the empty screen at launch, but never touches it.
 *
 * @param isEmpty whether the chat being shown has no messages yet
 * @returns a ref for the element that moves, the composer's wrapper
 */
export function useComposerGlide<T extends HTMLElement>(isEmpty: boolean) {
  const ref = useRef<T>(null);
  // Where the composer's top edge was when the first message was sent; null
  // when no glide is pending.
  const fromTop = useRef<number | null>(null);
  // The glide in flight, so a new chat started mid-way can stop it rather than
  // leave the new empty layout shifted by what remains of it.
  const gliding = useRef<Animation | undefined>(undefined);

  useEffect(
    () =>
      useStore.subscribe((state, prev) => {
        const before = prev.active;
        const after = state.active;
        const isFirstSend =
          state.streaming &&
          !prev.streaming &&
          (before?.messages.length ?? 0) === 0 &&
          (after?.messages.length ?? 0) > 0 &&
          // The same conversation, or the very first one, created by this send.
          (before === null || before.id === after?.id);
        if (isFirstSend) fromTop.current = ref.current?.getBoundingClientRect().top ?? null;
      }),
    [],
  );

  useLayoutEffect(() => {
    gliding.current?.cancel();
    gliding.current = undefined;
    const from = fromTop.current;
    fromTop.current = null;
    const el = ref.current;
    // jsdom has neither layout nor the Web Animations API.
    if (isEmpty || from === null || !el || typeof el.animate !== "function") return;
    if (window.matchMedia?.("(prefers-reduced-motion: reduce)").matches) return;
    const shift = from - el.getBoundingClientRect().top;
    if (Math.abs(shift) < 1) return;
    gliding.current = el.animate(
      [{ transform: `translateY(${shift}px)` }, { transform: "none" }],
      { duration: GLIDE_MS, easing: "cubic-bezier(0.22, 1, 0.36, 1)" },
    );
  }, [isEmpty]);

  return ref;
}
