import { useId } from "react";
import { useTranslation } from "react-i18next";
import { useDialogFocus } from "../components/useDialogFocus";
import { useStore } from "../state/store";

/** "Waiting for you to finish signing in…" modal — mounted whenever
 * `signInOpen` is true (see App.tsx). The actual login form lives in the
 * system browser (opened by the `signIn` action, see store.ts); this dialog
 * just tracks that session and lets the user cancel or retry. */
export default function SignInDialog() {
  const { t } = useTranslation();
  const error = useStore((s) => s.signInError);
  const signIn = useStore((s) => s.signIn);
  const cancelSignIn = useStore((s) => s.cancelSignIn);
  const titleId = useId();
  const dialogRef = useDialogFocus<HTMLDivElement>(() => void cancelSignIn());

  return (
    <div className="modal-backdrop" onClick={() => void cancelSignIn()}>
      <div
        ref={dialogRef}
        tabIndex={-1}
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        onClick={(e) => e.stopPropagation()}
      >
        <h2 id={titleId}>{t("signInDialog.title")}</h2>
        {error ? (
          <p className="model-error" role="alert">{error}</p>
        ) : (
          <p className="settings-sub">
            {t("signInDialog.waitingHint")}
          </p>
        )}

        <div className="update-actions">
          <div />
          <div className="update-actions-right">
            <button className="btn-ghost" onClick={() => void cancelSignIn()}>
              {t("common.cancel")}
            </button>
            {error && (
              <button className="btn" onClick={() => void signIn()}>
                {t("signInDialog.tryAgain")}
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
