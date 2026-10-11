import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

export interface PinPadProps {
  /** Shown above the boxes, e.g. "Enter your PIN". Also seeds each digit
   * box's own aria-label. */
  title: string;
  subtitle?: string;
  /** Number of digit boxes. Defaults to 4 — every caller today uses 4, but
   * this stays a prop rather than a hardcoded constant since the PIN itself
   * is meant to be reused by features beyond parental controls later. */
  length?: number;
  /** A message to show below the boxes, e.g. "That PIN isn't right." Clears
   * the entered digits and refocuses the first box when it changes to a new
   * non-empty value. */
  error?: string | null;
  /** Disables the boxes and shows a spinner-less "working" state while a
   * submitted PIN is being checked. */
  busy?: boolean;
  /** Seconds remaining on an active lockout. While > 0 the boxes are
   * disabled and this component counts it down locally (the real gate is
   * always server-side — this is just so the UI doesn't sit stale). */
  lockedForSeconds?: number;
  /** Renders the lockout message from the live countdown; falls back to a
   * plain "{n}s" if not given (callers should normally supply translated
   * copy). */
  renderLocked?: (secondsRemaining: number) => string;
  /** Omit both `cancelLabel` and `onCancel` for a gate that can't be
   * dismissed (e.g. `AppLockGate`'s PIN stage) — the cancel button and its
   * Escape-key shortcut disappear entirely rather than rendering disabled. */
  cancelLabel?: string;
  onCancel?: () => void;
  /** Called once all boxes are filled. This component does not clear its
   * own digits on submit — pass a new `error` (even success can be modeled
   * as the caller unmounting/replacing this component) to reset the boxes
   * after a rejected attempt. */
  onSubmit: (pin: string) => void;
}

/**
 * A generic, full-screen numeric PIN entry pad: N single-digit boxes with
 * auto-advance, backspace-to-previous, and auto-submit once the last box is
 * filled. Deliberately free of any domain-specific naming or copy — every
 * label is a prop — so it isn't tied to parental controls and can be reused
 * by any other PIN-gated feature later.
 *
 * Rendered through a portal straight into `document.body` rather than in
 * place: `position: fixed` only escapes to the viewport when no ancestor
 * establishes its own containing block, and several ancestors here do
 * (e.g. `.modal`'s entrance animation touches `transform`, which — with
 * `animation-fill-mode: both` keeping it permanently "current" — pins fixed
 * descendants to the modal's box instead of the screen). A caller like
 * Settings can be inside a dialog like that; the pad still needs to cover
 * the whole app, so it opts out of the DOM hierarchy entirely.
 */
export default function PinPad({
  title,
  subtitle,
  length = 4,
  error = null,
  busy = false,
  lockedForSeconds = 0,
  renderLocked,
  cancelLabel,
  onCancel,
  onSubmit,
}: PinPadProps) {
  const [digits, setDigits] = useState<string[]>(() => Array(length).fill(""));
  const [remaining, setRemaining] = useState(lockedForSeconds);
  const boxRefs = useRef<(HTMLInputElement | null)[]>([]);
  const locked = remaining > 0;
  const disabled = busy || locked;

  useEffect(() => {
    if (lockedForSeconds > 0) setRemaining(lockedForSeconds);
  }, [lockedForSeconds]);

  useEffect(() => {
    // `locked`, not `remaining`, so this only re-arms when the countdown
    // starts/stops rather than every second — the tick itself is a
    // functional update below, independent of the render-time `remaining`.
    if (!locked) return;
    const id = window.setInterval(() => setRemaining((r) => Math.max(0, r - 1)), 1000);
    return () => window.clearInterval(id);
  }, [locked]);

  useEffect(() => {
    if (error || locked) {
      setDigits(Array(length).fill(""));
    }
    boxRefs.current[0]?.focus();
  }, [error, locked, length]);

  useEffect(() => {
    boxRefs.current[0]?.focus();
  }, []);

  const setDigit = (index: number, raw: string) => {
    const value = raw.replace(/\D/g, "").slice(-1);
    const next = [...digits];
    next[index] = value;
    setDigits(next);
    if (!value) return;
    if (index < length - 1) {
      boxRefs.current[index + 1]?.focus();
      return;
    }
    if (next.every((d) => d !== "")) {
      onSubmit(next.join(""));
    }
  };

  const onKeyDown = (index: number, e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Escape") {
      onCancel?.();
    } else if (e.key === "Backspace" && digits[index] === "" && index > 0) {
      boxRefs.current[index - 1]?.focus();
    }
  };

  return createPortal(
    <div className="pin-pad-overlay" role="dialog" aria-modal="true" aria-label={title}>
      <div className="pin-pad">
        <h2 className="pin-pad-title">{title}</h2>
        {subtitle && <p className="pin-pad-subtitle">{subtitle}</p>}
        <div className="pin-pad-boxes">
          {digits.map((digit, i) => (
            <input
              key={i}
              ref={(el) => {
                boxRefs.current[i] = el;
              }}
              type="tel"
              inputMode="numeric"
              pattern="[0-9]*"
              maxLength={1}
              autoComplete="off"
              className="pin-pad-box"
              value={digit}
              disabled={disabled}
              aria-label={`${title}, digit ${i + 1} of ${length}`}
              onChange={(e) => setDigit(i, e.target.value)}
              onKeyDown={(e) => onKeyDown(i, e)}
            />
          ))}
        </div>
        {locked ? (
          <p className="pin-pad-message">
            {renderLocked ? renderLocked(remaining) : `${remaining}s`}
          </p>
        ) : (
          error && <p className="pin-pad-message pin-pad-error" role="alert">{error}</p>
        )}
        {cancelLabel && onCancel && (
          <button type="button" className="btn-quiet pin-pad-cancel" disabled={busy} onClick={onCancel}>
            {cancelLabel}
          </button>
        )}
      </div>
    </div>,
    document.body,
  );
}
