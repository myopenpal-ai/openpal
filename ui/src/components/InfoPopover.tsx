import { useId, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useCloseOnOutside, useCloseOnScroll } from "./Dropdown";

const InfoIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <circle cx="12" cy="12" r="9" />
    <path d="M12 11v5" />
    <path d="M12 8h.01" />
  </svg>
);

/** Roughly how tall a panel gets, for the "would this open off the bottom of
 * the window?" flip. */
const PANEL_HEIGHT_ESTIMATE = 220;
/** Must match `.info-pop`'s `max-width` in library.css — used to keep the
 * panel on screen before it has been laid out and can be measured. */
const PANEL_MAX_WIDTH = 330;
/** Breathing room kept between the panel and the window edge. */
const EDGE_GUTTER = 12;

/**
 * A small "i" button that explains a piece of jargon in place.
 *
 * The app had no tooltip of any kind — every explanation was a native `title`
 * attribute, which is mouse-only and so reaches neither keyboard nor touch
 * users. This is a real popover: a labelled button, Escape and outside-click
 * to close, focus returned to the trigger.
 *
 * Portal-rendered at a measured fixed position rather than positioned inside
 * its parent, because every current caller sits inside a scroll container
 * (`.library-scroll`) whose `overflow` would clip it — same reasoning as the
 * sidebar's per-row menu.
 */
export default function InfoPopover({
  label,
  heading,
  children,
}: {
  /** Accessible name for the trigger — say what it explains, e.g. "What is a
   * quantization?", not just "Info". */
  label: string;
  heading: string;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState<{ left: number; offset: number; openUpward: boolean } | null>(
    null,
  );
  const triggerRef = useRef<HTMLButtonElement>(null);
  const headingId = useId();

  const close = () => {
    setOpen(false);
    triggerRef.current?.focus();
  };

  const panelRef = useCloseOnOutside<HTMLDivElement>(open, close, [triggerRef]);
  useCloseOnScroll(open, close);

  const toggle = () => {
    if (open) {
      close();
      return;
    }
    const rect = triggerRef.current?.getBoundingClientRect();
    if (!rect) return;
    const openUpward = window.innerHeight - rect.bottom < PANEL_HEIGHT_ESTIMATE;
    setPos({
      // Starts just left of the trigger, then clamps so the panel can't run
      // off either edge — these triggers sit at the end of a row, inches from
      // the right edge, where an unclamped panel loses most of its text.
      // Clamped against the CSS max-width rather than a measurement, since
      // the panel hasn't been laid out yet at this point.
      left: Math.min(
        Math.max(EDGE_GUTTER, rect.left - EDGE_GUTTER),
        Math.max(EDGE_GUTTER, window.innerWidth - PANEL_MAX_WIDTH - EDGE_GUTTER),
      ),
      offset: openUpward ? window.innerHeight - rect.top + 8 : rect.bottom + 8,
      openUpward,
    });
    setOpen(true);
  };

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        className={`info-pop-trigger${open ? " is-open" : ""}`}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={label}
        onClick={toggle}
      >
        <InfoIcon />
      </button>
      {open &&
        pos &&
        createPortal(
          <div
            ref={panelRef}
            className="info-pop"
            role="dialog"
            aria-labelledby={headingId}
            style={{
              position: "fixed",
              left: pos.left,
              top: pos.openUpward ? "auto" : pos.offset,
              bottom: pos.openUpward ? pos.offset : "auto",
            }}
          >
            <h3 id={headingId} className="info-pop-heading">
              {heading}
            </h3>
            {children}
          </div>,
          document.body,
        )}
    </>
  );
}
