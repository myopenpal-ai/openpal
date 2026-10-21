import { useId } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { useDialogFocus } from "./useDialogFocus";

export interface ConfirmDialogProps {
  title: string;
  message: string;
  confirmLabel: string;
  /** The cancel button's text; `common.cancel` ("Cancel") when omitted. */
  cancelLabel?: string;
  /** Disables both buttons while the confirmed action is in flight. */
  busy?: boolean;
  /** Confirm-button styling. `"danger"` (the default, and every existing
   * caller's behavior) reads as a red destructive action — appropriate for
   * delete confirmations. `"primary"` uses the plain accent `.btn` styling
   * instead, for a confirm that isn't destructive (e.g. forking a
   * conversation). */
  confirmVariant?: "danger" | "primary";
  onConfirm: () => void;
  onCancel: () => void;
}

/**
 * Generic "are you sure" modal — same `.modal-backdrop`/`.modal` shell every
 * other dialog in this app uses (JoinGroupDialog, UpdateDialog, …), with the
 * `btn-quiet`/`btn-danger-quiet` button pair the app's existing inline
 * delete-confirm panels use (Library.tsx, SkillsView.tsx,
 * FactoryResetConfirm). Unlike those inline panels, this renders as a modal
 * so it works equally well for a single hover-revealed row action and for a
 * bulk action confirming several rows scattered across a scrolled list.
 *
 * Rendered through a portal to `document.body`, not inline where it's used —
 * `.modal-backdrop` is `position: fixed`, but several call sites (e.g.
 * Sidebar.tsx) sit under an ancestor with `backdrop-filter` (`.sidebar`),
 * which establishes its own containing block for fixed descendants per the
 * CSS spec, same as `filter`/`transform` would. Left inline, the dialog
 * would center within that ancestor's box instead of the whole app window.
 */
export default function ConfirmDialog({
  title,
  message,
  confirmLabel,
  cancelLabel,
  busy,
  confirmVariant = "danger",
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  const { t } = useTranslation();
  const titleId = useId();
  const messageId = useId();
  const dialogRef = useDialogFocus<HTMLDivElement>(onCancel);

  return createPortal(
    <div className="modal-backdrop" onClick={onCancel}>
      <div
        ref={dialogRef}
        tabIndex={-1}
        className="modal"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={messageId}
        onClick={(e) => e.stopPropagation()}
      >
        <h2 id={titleId}>{title}</h2>
        <div className="model-confirm">
          <p id={messageId} className="model-confirm-text">{message}</p>
          <div className="model-confirm-actions">
            <button className="btn-quiet" disabled={busy} onClick={onCancel}>
              {cancelLabel ?? t("common.cancel")}
            </button>
            <button
              className={confirmVariant === "danger" ? "btn-quiet btn-danger-quiet" : "btn"}
              disabled={busy}
              onClick={onConfirm}
            >
              {confirmLabel}
            </button>
          </div>
        </div>
      </div>
    </div>,
    document.body,
  );
}
