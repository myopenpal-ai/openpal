import { useId } from "react";
import { useTranslation } from "react-i18next";
import Markdown from "../components/Markdown";
import ProgressBar from "../components/ProgressBar";
import { useDialogFocus } from "../components/useDialogFocus";
import { formatBytes, formatEta, formatSpeed } from "../lib/format";
import { useStore } from "../state/store";

/** "New version available" modal — mounted whenever `updateStatus` isn't
 * "idle" (see App.tsx). Covers both the initial offer and the download/
 * install progress that follows "Update & Restart"; on success the app
 * restarts or exits on its own, so there's no "done" state to show here. */
export default function UpdateDialog() {
  const { t } = useTranslation();
  const status = useStore((s) => s.updateStatus);
  const update = useStore((s) => s.availableUpdate);
  const progress = useStore((s) => s.updateProgress);
  const error = useStore((s) => s.updateError);
  const startUpdateInstall = useStore((s) => s.startUpdateInstall);
  const skipUpdateVersion = useStore((s) => s.skipUpdateVersion);
  const dismissUpdate = useStore((s) => s.dismissUpdate);
  const installing = status === "downloading";

  const close = () => {
    // A backdrop click shouldn't interrupt an install already in flight.
    if (!installing) dismissUpdate();
  };
  const titleId = useId();
  // No onClose (so Escape does nothing) while an install is in flight —
  // keeps the same "can't dismiss while installing" guard as the backdrop.
  const dialogRef = useDialogFocus<HTMLDivElement>(installing ? undefined : close);

  if (status === "idle" || status === "checking") return null;

  return (
    <div className="modal-backdrop" onClick={close}>
      <div
        ref={dialogRef}
        tabIndex={-1}
        className="modal"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby={titleId}
        onClick={(e) => e.stopPropagation()}
      >
        <h2 id={titleId}>{t("updateDialog.title")}</h2>
        {update && (
          <p className="settings-sub">
            {t("updateDialog.readyToInstall", { version: update.version })}
          </p>
        )}

        {update?.notes && (
          <div className="update-notes">
            <Markdown confirmLinks={false}>{update.notes}</Markdown>
          </div>
        )}

        {installing && (
          <ProgressBar
            className="model-progress"
            barClassName="model-progress-bar"
            value={progress && progress.totalBytes > 0 ? progress.receivedBytes / progress.totalBytes : 0}
            label={t("updateDialog.progressAriaLabel")}
          />
        )}
        {installing && progress && (
          <span className="model-progress-text" role="status">
            {t("updateDialog.progressOf", {
              received: formatBytes(progress.receivedBytes),
              total: formatBytes(progress.totalBytes),
            })}
            {progress.bytesPerSec > 0 && <> · {formatSpeed(progress.bytesPerSec)}</>}
            {progress.etaSeconds > 0 && <> · {formatEta(progress.etaSeconds)}</>}
          </span>
        )}

        {status === "error" && error && <p className="model-error" role="alert">{error}</p>}

        <div className="update-actions">
          <div>
            {!installing && (
              <button className="btn-quiet" onClick={() => void skipUpdateVersion()}>
                {t("updateDialog.skip")}
              </button>
            )}
          </div>
          <div className="update-actions-right">
            {!installing && (
              <button className="btn-ghost" onClick={dismissUpdate}>
                {t("updateDialog.later")}
              </button>
            )}
            <button
              className="btn"
              disabled={installing}
              onClick={() => void startUpdateInstall()}
            >
              {installing
                ? t("updateDialog.installing")
                : status === "error"
                  ? t("updateDialog.tryAgain")
                  : t("updateDialog.updateAndRestart")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
