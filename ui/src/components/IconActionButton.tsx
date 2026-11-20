import type { ReactNode } from "react";

/**
 * An icon-only action for the Writing Tools result rows. Its `label` is both
 * the hover text (`title`) and the accessible name (`aria-label`), so the
 * action is never discoverable by hover alone: a keyboard or screen-reader
 * user gets exactly the same words a sighted mouse user sees.
 *
 * Same pattern as `Markdown.tsx`'s code-block copy button (icon + `title`),
 * reduced to the icon because these rows can hold five actions.
 */
export default function IconActionButton({
  label,
  onClick,
  disabled = false,
  children,
}: {
  label: string;
  onClick: () => void;
  disabled?: boolean;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      className="btn-quiet writing-tools-icon-btn"
      title={label}
      aria-label={label}
      disabled={disabled}
      onClick={onClick}
    >
      {children}
    </button>
  );
}
