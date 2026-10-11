/** ARIA attributes for a progress indicator, shared by `ProgressBar` (a div
 * pair) and `ProgressRing` (an `<svg>` — different DOM shape, same
 * semantics). Not a stateful hook — no internal state/effects — but named
 * `use*` since it's the one place both components' ARIA wiring lives, and
 * callers spread its result straight onto their root element's props.
 *
 * `value` is a 0..1 fraction of completion; pass `null` for an
 * indeterminate/unknown-duration bar (e.g. a plugin's setup step before it
 * reports real progress) — `aria-valuenow`/min/max are omitted entirely in
 * that case rather than set to a misleading number, matching how a native
 * `<progress>` element without a `value` behaves. */
export function useProgressAria(
  value: number | null,
  label: string,
): {
  role: "progressbar";
  "aria-label": string;
  "aria-valuemin"?: number;
  "aria-valuemax"?: number;
  "aria-valuenow"?: number;
} {
  if (value === null) {
    return { role: "progressbar", "aria-label": label };
  }
  const clamped = Math.min(1, Math.max(0, value));
  return {
    role: "progressbar",
    "aria-label": label,
    "aria-valuemin": 0,
    "aria-valuemax": 100,
    "aria-valuenow": Math.round(clamped * 100),
  };
}

export interface ProgressBarProps {
  /** 0..1 fraction of completion, or `null` for an indeterminate bar (no
   * inline width is set — the indeterminate sweep animation, if any, is the
   * caller's CSS, driven off `className`). */
  value: number | null;
  /** Accessible name — what's downloading/installing/setting up. */
  label: string;
  /** Class for the outer track element (e.g. "model-progress"). */
  className?: string;
  /** Class for the inner filled bar element (e.g. "model-progress-bar"). */
  barClassName?: string;
}

/**
 * Shared progress-bar primitive: a track `<div>` with a filled inner
 * `<div>`, exposing real `role="progressbar"` + `aria-valuenow` semantics
 * instead of the plain, ARIA-less hand-rolled bars previously duplicated
 * across Library, the Update dialog, and Plugins (findings L6, P2, D9).
 * Pure markup/ARIA — callers own all visual styling via `className`/
 * `barClassName`, so this is a drop-in replacement for the existing
 * `<div className="model-progress"><div className="model-progress-bar" .../></div>`
 * pairs with no CSS changes required.
 */
export default function ProgressBar({ value, label, className, barClassName }: ProgressBarProps) {
  const aria = useProgressAria(value, label);
  const pct = value === null ? null : Math.round(Math.min(1, Math.max(0, value)) * 100);

  return (
    <div className={className} {...aria}>
      <div className={barClassName} style={pct === null ? undefined : { width: `${pct}%` }} />
    </div>
  );
}
