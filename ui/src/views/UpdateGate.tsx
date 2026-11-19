import { useRef, useEffect } from "react";
import { useTranslation } from "react-i18next";
import ProgressBar from "../components/ProgressBar";
import { formatBytes, formatEta, formatSpeed } from "../lib/format";
import { useStore } from "../state/store";

/**
 * The startup gate that replaces the normal UI when the update manifest
 * declares the running version unsafe (below the declared minimum, or on the
 * blocked list — see `checkUpdateGate`/`get_update_gate`). This is the
 * enforcement half of the update flow: unlike the voluntary "New version
 * available" `UpdateDialog`, there is **no skip, no "Later", no dismiss** —
 * the only way out is to install the update, and the gate stays up until a
 * safe version is actually running (a failed attempt just shows the error and
 * the same single button).
 *
 * Mounted from `App.tsx` whenever `updateGate.appBlockedVersion` is set, and
 * ordered so the rest of the shell (including `UpdateDialog`'s mount point)
 * never renders underneath. Rendering decisions here must stay local to the
 * store's `updateGate` so "Skip this version" can never apply to a gated
 * version — it lives on `updateStatus`, which the gate deliberately ignores.
 *
 * Reuses the `updateStatus`/`updateProgress`/`updateError` state and
 * `startUpdateInstall` flow the voluntary dialog uses; the two surfaces are
 * never up at the same time, so sharing is safe.
 */
export default function UpdateGate() {
  const { t } = useTranslation();
  const appBlockedVersion = useStore((s) => s.updateGate.appBlockedVersion);
  const status = useStore((s) => s.updateStatus);
  const progress = useStore((s) => s.updateProgress);
  const error = useStore((s) => s.updateError);
  const startUpdateInstall = useStore((s) => s.startUpdateInstall);

  const sceneRef = useRef<HTMLElement>(null);
  useEffect(() => {
    const el = sceneRef.current;
    if (!el) return;
    const heading = el.querySelector<HTMLElement>("h1");
    (heading ?? el).focus({ preventScroll: true });
  }, []);

  // Safe (or un-gated): nothing to block on. This is also the "absent for a
  // safe one" state the frontend test pins.
  if (!appBlockedVersion) return null;

  const installing = status === "downloading";
  const retrying = status === "error";

  return (
    <div className="onboarding" data-tauri-drag-region>
      <div className="ob-glow" aria-hidden />
      <section className="ob-scene" ref={sceneRef} tabIndex={-1}>
        <p className="ob-brand ob-appear">OpenPal</p>
        <h1 className="ob-title ob-appear" tabIndex={-1}>
          {t("updateGate.title")}
        </h1>
        <p className="ob-sub ob-appear">{t("updateGate.body")}</p>

        {installing && (
          <>
            <ProgressBar
              className="model-progress ob-appear"
              barClassName="model-progress-bar"
              value={
                progress && progress.totalBytes > 0
                  ? progress.receivedBytes / progress.totalBytes
                  : 0
              }
              label={t("updateDialog.progressAriaLabel")}
            />
            {progress && (
              <span className="model-progress-text ob-appear" role="status">
                {t("updateDialog.progressOf", {
                  received: formatBytes(progress.receivedBytes),
                  total: formatBytes(progress.totalBytes),
                })}
                {progress.bytesPerSec > 0 && <> · {formatSpeed(progress.bytesPerSec)}</>}
                {progress.etaSeconds > 0 && <> · {formatEta(progress.etaSeconds)}</>}
              </span>
            )}
          </>
        )}

        {status === "error" && error && (
          <p className="rec-error ob-appear" role="alert">
            {error}
          </p>
        )}

        <div className="ob-appearance-actions ob-appear">
          <button
            type="button"
            className="btn btn-big"
            disabled={installing}
            onClick={() => void startUpdateInstall()}
          >
            {installing
              ? t("updateDialog.installing")
              : retrying
                ? t("updateDialog.tryAgain")
                : t("updateGate.updateNow")}
          </button>
        </div>
      </section>
    </div>
  );
}
