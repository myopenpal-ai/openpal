import { useCallback, useEffect, useRef, useState, type ReactElement } from "react";
import { useTranslation } from "react-i18next";
import { Alert, AlertDescription, AlertTitle } from "./ui/alert";
import { useStore, type AlertKind, type AppAlert } from "../state/store";

/** How long an alert stays up when nothing is holding it. Long enough to read
 * a sentence, short enough that three of them never pile up into a wall. */
export const ALERT_DURATION_MS = 6000;

const iconProps = {
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: "1.9",
  strokeLinecap: "round",
  strokeLinejoin: "round",
  "aria-hidden": true,
} as const;

// One icon per kind, each a DIRECT <svg> child of <Alert>: shadcn's
// `has-[>svg]` grid only lays out an svg that is an immediate child. The
// colour comes from alerts.css (`.alert-host-icon-*`), tokens that
// contrast.test.ts holds to 3:1 on --paper-raised in all three palettes; the
// shape differs per kind too, so no alert is told apart by hue alone.
const ICONS: Record<AlertKind, ReactElement> = {
  modelLoaded: (
    <svg {...iconProps} className="alert-host-icon alert-host-icon-ok">
      <circle cx="12" cy="12" r="8.5" />
      <path d="m8.4 12.3 2.5 2.5 4.7-5" />
    </svg>
  ),
  modelUnloaded: (
    <svg {...iconProps} className="alert-host-icon alert-host-icon-quiet">
      <path d="M12 4v7" />
      <path d="M7.2 7.4a7.5 7.5 0 1 0 9.6 0" />
    </svg>
  ),
  downloadFinished: (
    <svg {...iconProps} className="alert-host-icon alert-host-icon-accent">
      <path d="M12 4v9.5M8 10.2l4 4 4-4" />
      <path d="M5 19h14" />
    </svg>
  ),
  answerReady: (
    <svg {...iconProps} className="alert-host-icon alert-host-icon-accent">
      <path d="M4.5 5.5h15a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1H10l-4 3v-3H4.5a1 1 0 0 1-1-1v-9a1 1 0 0 1 1-1Z" />
      <path d="m8.7 11 2.1 2.1 4.2-4.3" />
    </svg>
  ),
};

const CloseIcon = () => (
  <svg
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="2.2"
    strokeLinecap="round"
    aria-hidden="true"
  >
    <path d="M6 6l12 12M18 6 6 18" />
  </svg>
);

/** Counts `ALERT_DURATION_MS` down while `paused` is false and calls `onExpire`
 * at zero. Pausing keeps what is left, so an alert the pointer left after 4s
 * has 2s to go, not a fresh 6. The cleanup also runs on unmount, which is what
 * keeps a dismissed or evicted alert from firing later (and StrictMode's
 * mount, unmount, mount from firing twice: the first timer is cleared before
 * the second starts). */
function useAutoDismiss(paused: boolean, onExpire: () => void) {
  const onExpireRef = useRef(onExpire);
  useEffect(() => {
    onExpireRef.current = onExpire;
  });
  const remaining = useRef(ALERT_DURATION_MS);
  useEffect(() => {
    if (paused) return;
    const startedAt = Date.now();
    const timer = window.setTimeout(() => onExpireRef.current(), remaining.current);
    return () => {
      window.clearTimeout(timer);
      remaining.current = Math.max(0, remaining.current - (Date.now() - startedAt));
    };
  }, [paused]);
}

/** Whether OpenPal's window has the OS's focus, from the window's own focus and
 * blur events. Starts true rather than from `document.hasFocus()`: that is
 * false in jsdom until something is focused, which would freeze every timer
 * in the tests, and a window that is showing UI has focus in practice. The
 * stack waits while this is false because an alert that expires while the user
 * is in another window is one they never saw (the native notification, when
 * enabled, is what reaches them there). */
function useWindowFocused(): boolean {
  const [focused, setFocused] = useState(true);
  useEffect(() => {
    const onFocus = () => setFocused(true);
    const onBlur = () => setFocused(false);
    window.addEventListener("focus", onFocus);
    window.addEventListener("blur", onBlur);
    return () => {
      window.removeEventListener("focus", onFocus);
      window.removeEventListener("blur", onBlur);
    };
  }, []);
  return focused;
}

function AlertItem({
  alert,
  paused,
  onDismiss,
  onExpire,
}: {
  alert: AppAlert;
  paused: boolean;
  onDismiss: (id: string) => void;
  onExpire: (id: string) => void;
}) {
  const { t } = useTranslation();
  useAutoDismiss(paused, () => onExpire(alert.id));
  return (
    <div className="alert-host-item" data-alert-id={alert.id} data-kind={alert.kind}>
      {/* shadcn's Alert hard-codes role="alert", which screen readers announce
          at once and assertively. The announcer in AlertHost is the one polite
          announcement, so the visible card drops the role (props spread after
          it) rather than be read a second time. */}
      <Alert role={undefined} className="alert-host-alert">
        {ICONS[alert.kind]}
        <AlertTitle>{alert.title}</AlertTitle>
        <AlertDescription>{alert.body}</AlertDescription>
      </Alert>
      {/* A sibling of the Alert, not a child: a child would become a cell of
          its grid. The title is in the name so three stacked "Dismiss"
          buttons are told apart by a screen reader's button list. */}
      <button
        type="button"
        data-alert-dismiss
        // Explicit, although a button is a tab stop anyway: WebKit (macOS's
        // WKWebView) skips plain buttons when Tab is pressed unless the user
        // turned on full keyboard access, and an explicit tabindex is what
        // makes it stop here. Without it the keyboard path to this button,
        // and so to dismissing an alert, would not exist on a default Mac.
        tabIndex={0}
        className="icon-btn alert-host-dismiss"
        aria-label={`${t("alerts.dismiss")} ${alert.title}`}
        onClick={() => onDismiss(alert.id)}
      >
        <CloseIcon />
      </button>
    </div>
  );
}

/** The in-app alerts (model loaded / unloaded, download finished, answer
 * ready): a stack in the top-right corner, fed by `useStore().appAlerts`.
 * Presentational; what counts as an event, and its words, are decided where
 * `showAlert` is called.
 *
 * It never takes focus (an alert can arrive while the user is typing), so its
 * rules are about leaving the user alone and being reachable on request:
 *
 * - Announcing: a polite live region that is in the DOM before the first
 *   alert and holds one line per alert, in the Radix Toast manner. The cards
 *   themselves are not live regions.
 * - Timing: every alert clears itself after `ALERT_DURATION_MS`, but the whole
 *   stack waits while the pointer is over it, while focus is inside it and
 *   while the window is in the background (WCAG 2.2.1).
 * - Keyboard: each card has a Dismiss button, the next Tab stop after the
 *   composer (the host is mounted right after <main>). Escape dismisses the
 *   alert holding focus and is consumed there: it is deliberately not a global
 *   key, because Escape already closes dialogs, menus and previews, and one
 *   press must never close an alert and one of those together. */
export default function AlertHost() {
  const { t } = useTranslation();
  const alerts = useStore((s) => s.appAlerts);
  const dismissAlert = useStore((s) => s.dismissAlert);
  const rootRef = useRef<HTMLDivElement>(null);
  // Where keyboard focus was just before it entered the stack: where to give
  // it back when the last alert goes. Only read while focus is inside, and
  // overwritten on every entry that has an origin (see onFocus).
  const cameFrom = useRef<HTMLElement | null>(null);
  const [hovered, setHovered] = useState(false);
  const [focusWithin, setFocusWithin] = useState(false);
  const windowFocused = useWindowFocused();
  const paused = hovered || focusWithin || !windowFocused;

  // Browsers don't reliably fire blur or pointerleave when the node that has
  // focus or the pointer is removed, and a flag left set would freeze the next
  // alert's timer for good. So whenever the list changes, focus is re-read
  // from the DOM, an empty stack (nothing left to hover) clears hover, and
  // otherwise the browser is asked one frame later whether the pointer is
  // still over the stack (`:hover` can lag the DOM change): Chromium, which is
  // WebView2 and Android's WebView, sends no pointerleave when the card under
  // a stationary pointer is removed, while WebKit does.
  const focusWithinRef = useRef(false);
  useEffect(() => {
    const root = rootRef.current;
    const wasInside = focusWithinRef.current;
    const inside = root?.contains(document.activeElement) ?? false;
    focusWithinRef.current = inside;
    setFocusWithin(inside);

    // The card that had focus was removed by something other than the user's
    // own dismissal (a fourth alert pushing the oldest out). Dropped on <body>,
    // the next Tab would start over at the skip link, so focus goes to the card
    // nearest where it was, or back to where it came from. This only repairs a
    // loss: focus is never moved when it was not already in the stack.
    const active = document.activeElement;
    if (root && wasInside && !inside && (!active || active === document.body)) {
      const buttons = Array.from(root.querySelectorAll<HTMLElement>("[data-alert-dismiss]"));
      const target = buttons[buttons.length - 1] ?? cameFrom.current;
      if (target?.isConnected) {
        target.focus({ preventScroll: true });
        focusWithinRef.current = root.contains(target);
        setFocusWithin(focusWithinRef.current);
      }
    }

    if (alerts.length === 0) {
      setHovered(false);
      return;
    }
    const frame = window.requestAnimationFrame(() => {
      if (rootRef.current && !rootRef.current.matches(":hover")) setHovered(false);
    });
    return () => window.cancelAnimationFrame(frame);
  }, [alerts]);

  // A user's dismissal (click, Enter on the button, Escape). If the alert
  // going away is the one holding focus, focus moves on before it is removed:
  // to the next alert's Dismiss button, else the previous one's, else back to
  // where it came from. Otherwise it would drop to <body> and a keyboard user
  // would restart from the skip link. Auto-expiry never comes through here: it
  // can't happen while focus is inside, and moving focus when the user did
  // nothing would be the very thing this host promises not to do.
  const dismiss = useCallback(
    (id: string) => {
      const root = rootRef.current;
      const active = document.activeElement;
      const item = root
        ? Array.from(root.querySelectorAll<HTMLElement>("[data-alert-id]")).find(
            (el) => el.dataset.alertId === id,
          )
        : undefined;
      if (root && item && active && item.contains(active)) {
        const buttons = Array.from(root.querySelectorAll<HTMLElement>("[data-alert-dismiss]"));
        const at = buttons.indexOf(active as HTMLElement);
        const neighbour = at < 0 ? undefined : (buttons[at + 1] ?? buttons[at - 1]);
        const target = neighbour ?? cameFrom.current;
        if (target?.isConnected) target.focus({ preventScroll: true });
      }
      dismissAlert(id);
    },
    [dismissAlert],
  );

  // Escape, only when the key press starts inside the stack, consumed here. A
  // native listener on the host's own node runs before every document and
  // window listener (the Dropdown's close-on-Escape, ChatView's preview and
  // lightbox close, the improve-draft stop), so stopPropagation is enough to
  // keep one press from doing two things. Dialogs handle Escape on their own
  // node, which is not an ancestor of this one.
  useEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      const target = e.target instanceof Element ? e.target : null;
      const id = target?.closest<HTMLElement>("[data-alert-id]")?.dataset.alertId;
      if (!id) return;
      e.preventDefault();
      e.stopPropagation();
      dismiss(id);
    };
    root.addEventListener("keydown", onKeyDown);
    return () => root.removeEventListener("keydown", onKeyDown);
  }, [dismiss]);

  return (
    <>
      {/* Present from the first render and empty when idle: assistive tech
          registers a live region when it enters the DOM and announces later
          *additions* to it, so one that arrives with its text already in place
          is routinely missed (AutoUnloadTooltip does the same). Not
          display:none, which would hide it from AT too. aria-atomic="false"
          because role="status" implies true, and a whole-region re-read on
          every new alert would repeat the ones still on screen. Text only, so
          the Dismiss buttons are not read out with it. One line per alert,
          keyed by its id: two identical alerts in a row are two additions. */}
      <div
        className="sr-only"
        role="status"
        aria-live="polite"
        aria-atomic="false"
        aria-relevant="additions"
      >
        {alerts.map((a) => (
          <p key={a.id}>{`${a.title}. ${a.body}`}</p>
        ))}
      </div>
      {/* The labelled group only exists while there is something in it: an
          empty "Notifications" group would just be noise in the a11y tree. */}
      <div
        ref={rootRef}
        className="alert-host"
        role={alerts.length > 0 ? "group" : undefined}
        aria-label={alerts.length > 0 ? t("alerts.region") : undefined}
        // Touch emulates pointerenter on a tap and never sends the matching
        // leave until the next tap elsewhere, which would freeze the stack on
        // Android. A mouse or a pen really hovers.
        onPointerEnter={(e) => {
          if (e.pointerType !== "touch") setHovered(true);
        }}
        onPointerLeave={() => setHovered(false)}
        // A click on an alert (its text, or its Dismiss button in Chromium)
        // would otherwise take focus out of the composer or out of an open
        // dialog, and removing the alert would then leave it on <body>. The
        // click itself still happens; only the focus move is cancelled.
        // Nor is it a click "outside" for an open popover or menu, which close
        // on a document-level mousedown: reaching for an alert must not close
        // the panel the user is working in, so the press stops here.
        onMouseDown={(e) => {
          e.preventDefault();
          e.stopPropagation();
        }}
        onFocus={(e) => {
          // A window coming back to the foreground re-focuses its focused
          // element with no relatedTarget at all; that is not a new origin, and
          // overwriting the real one with null would leave a later dismissal
          // nowhere to hand focus back to.
          const from = e.relatedTarget as HTMLElement | null;
          if (from && !e.currentTarget.contains(from)) cameFrom.current = from;
          focusWithinRef.current = true;
          setFocusWithin(true);
        }}
        onBlur={(e) => {
          if (!e.currentTarget.contains(e.relatedTarget as Node | null)) {
            focusWithinRef.current = false;
            setFocusWithin(false);
          }
        }}
      >
        {alerts.map((a) => (
          <AlertItem
            key={a.id}
            alert={a}
            paused={paused}
            onDismiss={dismiss}
            onExpire={dismissAlert}
          />
        ))}
      </div>
    </>
  );
}
