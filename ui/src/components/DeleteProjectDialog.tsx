import { useId } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { useDialogFocus } from "./useDialogFocus";

export interface DeleteProjectDialogProps {
  projectName: string;
  /** How many conversations currently belong to the project — 0 skips the
   * keep-vs-delete choice entirely, since there's nothing to decide. */
  conversationCount: number;
  /** Disables every button while the confirmed action is in flight. */
  busy?: boolean;
  /** Delete the project and leave its conversations in place, ungrouped —
   * the app's original (and only) behavior before this dialog existed. */
  onDetach: () => void;
  /** Delete the project and every conversation that was in it. */
  onDeleteConversations: () => void;
  onCancel: () => void;
}

/**
 * Confirmation for deleting a project — same `.modal-backdrop`/`.modal`
 * shell as `ConfirmDialog`, portaled to `document.body` for the same
 * containing-block reason (see that component's docstring). Unlike a plain
 * delete confirm, a project's conversations survive by default, so this
 * asks explicitly whether they should be kept (just detached from the
 * project) or deleted along with it, rather than silently picking one.
 */
export default function DeleteProjectDialog({
  projectName,
  conversationCount,
  busy,
  onDetach,
  onDeleteConversations,
  onCancel,
}: DeleteProjectDialogProps) {
  const { t } = useTranslation();
  const titleId = useId();
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
        onClick={(e) => e.stopPropagation()}
      >
        <h2 id={titleId}>{t("sidebar.deleteProjectConfirm.title", { name: projectName })}</h2>
        <div className="model-confirm">
          {conversationCount > 0 ? (
            <>
              <p className="model-confirm-text">
                {t("sidebar.deleteProjectConfirm.message", { count: conversationCount })}
              </p>
              <div className="model-confirm-actions model-confirm-actions-stacked">
                <button className="btn-quiet" disabled={busy} onClick={onDetach}>
                  {t("sidebar.deleteProjectConfirm.keepConversations")}
                </button>
                <button
                  className="btn-quiet btn-danger-quiet"
                  disabled={busy}
                  onClick={onDeleteConversations}
                >
                  {t("sidebar.deleteProjectConfirm.deleteConversations")}
                </button>
                <button className="btn-quiet" disabled={busy} onClick={onCancel}>
                  {t("common.cancel")}
                </button>
              </div>
            </>
          ) : (
            <>
              <p className="model-confirm-text">
                {t("sidebar.deleteProjectConfirm.messageEmpty", { name: projectName })}
              </p>
              <div className="model-confirm-actions">
                <button className="btn-quiet" disabled={busy} onClick={onCancel}>
                  {t("common.cancel")}
                </button>
                <button className="btn-quiet btn-danger-quiet" disabled={busy} onClick={onDetach}>
                  {t("sidebar.deleteProjectConfirm.confirmButton")}
                </button>
              </div>
            </>
          )}
        </div>
      </div>
    </div>,
    document.body,
  );
}
