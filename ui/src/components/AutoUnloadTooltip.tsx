import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useStore } from "../state/store";

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

/** Scaled to the configured idle period: a short warning window before the
 * unload actually fires — at least 5s, at most 60s, roughly a third of the
 * total idle period. Long enough to react to, short enough not to nag for
 * the whole countdown. */
function unloadWarningMs(autoUnloadMinutes: number): number {
  const totalMs = autoUnloadMinutes * 60_000;
  return Math.min(60_000, Math.max(5_000, totalMs / 3));
}

/** Heads-up shortly before the idle-unload timer fires, sitting just above
 * the sidebar's model row — the thing it's warning about. It's rendered from
 * the app shell and positioned against the viewport rather than nested in
 * that row; `.auto-unload-tooltip` has the reasoning.
 *
 * Three ways this ends, and two of them are doing nothing: ignore it, or
 * close it — either way the unload fires when the countdown runs out — or
 * cancel, which resets the idle clock exactly the way typing does. Turning
 * the whole feature off used to be a third button here; it lives in Settings
 * now, which is where a change that outlasts this one countdown belongs. */
export default function AutoUnloadTooltip() {
  const { t } = useTranslation();
  const engine = useStore((s) => s.engine);
  const settings = useStore((s) => s.settings);
  const snoozeAutoUnload = useStore((s) => s.snoozeAutoUnload);
  const sidebarMoreMenuOpen = useStore((s) => s.sidebarMoreMenuOpen);
  const [now, setNow] = useState(() => Date.now());
  const [dismissed, setDismissed] = useState(false);
  const focusWasOn = useRef<Element | null>(null);

  const autoUnloadAt = engine.type === "running" ? engine.data.autoUnloadAt : null;

  // Keeps `now` fresh for as long as the engine could unload, so the very
  // first render after crossing into the warning window already reflects an
  // accurate `remaining` rather than waiting on the next 2s status poll.
  useEffect(() => {
    if (autoUnloadAt == null) return;
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [autoUnloadAt]);

  const warningMs = unloadWarningMs(settings?.autoUnloadMinutes ?? 15);
  const remaining = autoUnloadAt == null ? null : autoUnloadAt - now;
  const inWarningWindow = remaining != null && remaining <= warningMs;
  // Yields to the sidebar's "More" menu, which opens upward straight across
  // this popover — see `sidebarMoreMenuOpen` for why hiding is the only
  // option there.
  const show = inWarningWindow && !dismissed && !sidebarMoreMenuOpen;

  // A dismissal only covers the countdown it was made during. Anything that
  // ends that countdown — the unload firing, or the idle clock being reset
  // by Cancel or by typing — re-arms the warning for the next one. Keyed on
  // the window rather than on `autoUnloadAt` itself because the backend
  // recomputes that as `now + (limit - idle_for)` on every 2s poll, so it
  // jitters by a few ms even when nothing has actually changed.
  useEffect(() => {
    if (!inWarningWindow) setDismissed(false);
  }, [inWarningWindow]);

  // Where focus was before this appeared unprompted. Both buttons remove the
  // element focus is sitting on, and without this the browser drops it to
  // <body> — a keyboard user would restart from the skip link. Same intent
  // as ModelQuickPicker's closeMenu, which has a trigger to go back to.
  useEffect(() => {
    if (show) focusWasOn.current = document.activeElement;
  }, [show]);

  const restoreFocus = () => {
    const prev = focusWasOn.current;
    if (prev instanceof HTMLElement && prev !== document.body && prev.isConnected) {
      prev.focus();
    }
  };

  const seconds = remaining == null ? 0 : Math.max(0, Math.ceil(remaining / 1000));

  return (
    <>
      {/* Mounted for this component's whole life, not just while the warning
          is up: assistive tech registers a live region when it enters the DOM
          and announces later *changes* to it, so a region that arrives with
          its text already in place is routinely missed. The visible copy
          can't do this job — it reruns its countdown every second, and
          role="status" is implicitly aria-atomic, so it would re-read the
          whole popover once a second for up to a minute. */}
      <p className="sr-only" role="status">
        {show ? t("autoUnloadBanner.announce") : ""}
      </p>
      {show && (
        <div className="auto-unload-tooltip">
          <p className="auto-unload-tooltip-text" aria-hidden="true">
            {t("autoUnloadBanner.unloadingIn", { seconds })}
          </p>
          {/* "Close" alone would be ambiguous next to "Cancel" — one lets the
              unload happen, the other prevents it. Named for its consequence,
              the way SafetyBanner names its dismiss button. */}
          <button
            type="button"
            className="icon-btn auto-unload-tooltip-close"
            aria-label={t("autoUnloadBanner.dismiss")}
            title={t("autoUnloadBanner.dismiss")}
            onClick={() => {
              restoreFocus();
              setDismissed(true);
            }}
          >
            <CloseIcon />
          </button>
          <div className="auto-unload-tooltip-actions">
            <button
              type="button"
              className="btn-quiet btn-warn-quiet"
              onClick={() => {
                restoreFocus();
                void snoozeAutoUnload();
              }}
            >
              {t("common.cancel")}
            </button>
          </div>
        </div>
      )}
    </>
  );
}
