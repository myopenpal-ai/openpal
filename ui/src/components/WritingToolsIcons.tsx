/**
 * The icon set shared by the three Writing Tools screens and the sidebar
 * quick-action rows that lead to them. One definition each, so a page title
 * can carry exactly the icon its sidebar row shows (issue #125) instead of a
 * second, subtly different drawing.
 *
 * Same house style as the rest of the app's inline icons: 24×24 viewBox,
 * `currentColor` stroke, round caps/joins, and `aria-hidden` — every icon in
 * here is decorative, paired with text or with the button's accessible name.
 */

type IconProps = { className?: string };

/** Translate — the sidebar row and the page title share this drawing. */
export const LanguagesIcon = ({ className = "sidebar-action-icon" }: IconProps) => (
  <svg
    className={className}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M5 7h8M9 4.5v2.5M7 7c0 3 1.8 5.3 4.5 6.5M11.5 7c-.6 2-1.8 3.8-3.5 5" />
    <path d="M13 19l3.2-8 3.2 8M14.4 16.4h3.6" />
  </svg>
);

/** Correct — check plus lines of text. */
export const CorrectIcon = ({ className = "sidebar-action-icon" }: IconProps) => (
  <svg
    className={className}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M4.5 12.5 6.5 14.5 10 9" />
    <path d="M13 8.5h6.5M13 12h6.5M13 15.5h4.5" />
  </svg>
);

/** Improve — the four-point sparkle, as its sidebar row draws it. */
export const SparkleIcon = ({ className = "sidebar-action-icon" }: IconProps) => (
  <svg
    className={className}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8Z" />
    <path d="M19 15l.8 2.2L22 18l-2.2.8L19 21l-.8-2.2L16 18l2.2-.8Z" />
  </svg>
);

/** Keep every suggestion — two checks, "all of them". */
export const KeepAllIcon = ({ className = "writing-tools-icon" }: IconProps) => (
  <svg
    className={className}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M3 12.5 5.5 15 10 9.5" />
    <path d="M11 12.5 13.5 15 21 6.5" />
  </svg>
);

/** An undo arrow — Correct's "Discard all" and Improve's "Restore your
 * message" both mean "put the text back the way it was", so they share one
 * drawing. */
export const RestoreIcon = ({ className = "writing-tools-icon" }: IconProps) => (
  <svg
    className={className}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M4 9h10.5a5 5 0 0 1 0 10H9" />
    <path d="M7.5 5.5 4 9l3.5 3.5" />
  </svg>
);

/** Correct's "Discard all" is the same undo arrow. */
export const DiscardAllIcon = RestoreIcon;

export const CopyIcon = ({ className = "writing-tools-icon" }: IconProps) => (
  <svg
    className={className}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <rect x="9" y="9" width="11" height="11" rx="2.2" />
    <path d="M15 6.5V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h.5" />
  </svg>
);

/** The copied confirmation — same check the copy button swaps in. */
export const CheckIcon = ({ className = "writing-tools-icon" }: IconProps) => (
  <svg
    className={className}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M5 12.5 9.5 17 19 7" />
  </svg>
);

export const DownloadIcon = ({ className = "writing-tools-icon" }: IconProps) => (
  <svg
    className={className}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M12 4v10.5" />
    <path d="M7.5 10.5 12 15l4.5-4.5" />
    <path d="M5 19h14" />
  </svg>
);

/** Clear — a cross, the app's usual "empty this out" mark. */
export const ClearIcon = ({ className = "writing-tools-icon" }: IconProps) => (
  <svg
    className={className}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M6 6l12 12M18 6 6 18" />
  </svg>
);

/** Start a new chat with the improved message. */
export const StartChatIcon = ({ className = "writing-tools-icon" }: IconProps) => (
  <svg
    className={className}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M4.5 6.5h15a1 1 0 0 1 1 1v8a1 1 0 0 1-1 1H10l-4 3v-3H4.5a1 1 0 0 1-1-1v-8a1 1 0 0 1 1-1Z" />
  </svg>
);
