import { useCallback, useEffect, useRef, useState } from "react";
import { floatingPanelOpen } from "./Dropdown";

const FOCUSABLE_SELECTOR =
  'a[href], button:not([disabled]), textarea:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])';

function getFocusable(root: HTMLElement): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR));
}

// The focusable element pressed with a pointer since focus last moved.
// WebKit — Safari, and the WKWebView this app runs in on macOS — never
// focuses a button on a mouse click, so for a dialog opened that way
// `document.activeElement` is still whatever held focus before (often just
// <body>), not the button that opened it; this stands in for it. Any focus
// move clears it — which a click in other engines does itself, focusing the
// button — so it only ever wins while fresher than the focused element.
// Tracked from module load, so even the very first dialog is covered.
let lastPressed: HTMLElement | null = null;
if (typeof document !== "undefined") {
  document.addEventListener(
    "pointerdown",
    (e) => {
      lastPressed = (e.target as Element | null)?.closest<HTMLElement>(FOCUSABLE_SELECTOR) ?? null;
    },
    true,
  );
  document.addEventListener("focusin", () => (lastPressed = null), true);
}

function currentOpener(): HTMLElement | null {
  if (lastPressed?.isConnected) return lastPressed;
  return document.activeElement as HTMLElement | null;
}

/**
 * Full modal-dialog contract for a component its parent conditionally
 * mounts/unmounts (every dialog in this app works this way — mounting *is*
 * opening). On mount: moves focus into the dialog root, unless something
 * inside it already grabbed focus itself (e.g. a field's own `autoFocus`).
 * While mounted: keeps Tab/Shift+Tab cycling among the dialog's own
 * focusable descendants, and calls `onClose` on Escape if provided (omit
 * `onClose` for a dialog that must be dismissed via its own buttons only —
 * e.g. a consent gate where Escape silently vanishing would be worse than no
 * shortcut — or when a parent already centrally manages Escape across
 * multiple stacked dialogs, as ChatView's image lightbox / doc preview do).
 * On unmount: restores focus to whatever opened it — the focused element
 * before it mounted or, in WebKit (which never focuses a clicked button),
 * the button that was clicked.
 *
 * Returns a callback ref rather than a plain RefObject — a component can
 * stay mounted while conditionally rendering `null` itself for a while
 * first (UpdateDialog does, across its "checking" status) before its real
 * dialog markup appears; a plain `useRef` + `useEffect(fn, [])` would fire
 * once against a still-null ref and never get another chance once the real
 * DOM shows up, since the *component* only mounts once. A callback ref
 * re-fires this hook's effect exactly when the actual DOM node attaches.
 * Passing it to `ref={...}` works the same as a RefObject would.
 *
 * The dialog root needs `tabIndex={-1}` so it's legally focusable as a
 * fallback target.
 */
export function useDialogFocus<T extends HTMLElement>(
  onClose?: () => void,
): (node: T | null) => void {
  const [node, setNode] = useState<T | null>(null);
  // Read through a ref, not a direct effect dependency — several call sites
  // pass a fresh closure every render, and a naive dependency would tear
  // down and re-run this effect (re-focusing the root) on every parent
  // re-render while the dialog is open, stealing focus mid-interaction.
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;
  // Whatever held focus on the last render before the dialog's DOM existed
  // — i.e. what opened it. A field inside can grab focus with its own
  // autoFocus/focus effect before the effect below runs, and "restore focus
  // to where it was" must not point at that field, gone once the dialog
  // unmounts. Tracked on every render while closed, not just the first, for
  // components that stay mounted between openings (SkillsView's editor).
  const openerRef = useRef<HTMLElement | null>(null);
  if (!node) openerRef.current = currentOpener();

  useEffect(() => {
    if (!node) return;
    const previous = openerRef.current;
    if (!node.contains(document.activeElement)) {
      node.focus({ preventScroll: true });
    }

    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        // A popover/menu open on top (e.g. a Models page explainer) closes
        // first, on its own document-level listener — let it through.
        if (!onCloseRef.current || floatingPanelOpen()) return;
        // Handled — stop it here so it closes only this dialog: not an
        // enclosing one this is nested inside (both listen on their own
        // node, innermost first), and not a document/window-level Escape
        // listener behind it (e.g. ChatView's preview panels, which stay
        // mounted under the More dialog).
        e.stopPropagation();
        onCloseRef.current();
        return;
      }
      if (e.key !== "Tab") return;
      const focusable = getFocusable(node);
      if (focusable.length === 0) {
        e.preventDefault();
        return;
      }
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      const active = document.activeElement;
      // Treat the root itself (its state right after the focus-in above, or
      // whenever nothing inside is focused) as being "at" both ends, so the
      // very first Tab/Shift+Tab is trapped too, not just later ones.
      const atStart = active === first || active === node;
      const atEnd = active === last || active === node;
      if (e.shiftKey && atStart) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && atEnd) {
        e.preventDefault();
        first.focus();
      }
    };
    node.addEventListener("keydown", onKeyDown);
    return () => {
      node.removeEventListener("keydown", onKeyDown);
      previous?.focus?.({ preventScroll: true });
    };
    // `onClose` intentionally omitted from deps — read via onCloseRef instead
    // (see comment above); this effect only re-runs when `node` itself changes.
  }, [node]);

  return useCallback((n: T | null) => setNode(n), []);
}
