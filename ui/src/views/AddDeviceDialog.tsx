import { useId } from "react";
import { useTranslation } from "react-i18next";
import { useDialogFocus } from "../components/useDialogFocus";
import { useStore } from "../state/store";

/**
 * Device A's side of "Add a device" (docs/SYNC_PAIRING_PROTOCOL.md, Phase 2).
 * Mounted whenever `addDeviceOpen` is true (see App.tsx) — the Settings Sync
 * section's "Add a device…" button calls `startAddDevice`, which opens this
 * dialog and kicks off `sync_pair_listen` before this ever renders. Same
 * store-driven shape as SignInDialog: Cancel always available, Retry once a
 * session ends without success.
 *
 * The Confirm/Reject buttons on the incoming-request card are deliberately
 * never focused by default (see `SyncPairingEvent`'s doc comment in
 * lib/types.ts) — the fingerprint must actually be read and compared before
 * either is pressed, so nothing here uses `autoFocus` and there's no
 * keyboard shortcut bound to either action.
 */
export default function AddDeviceDialog() {
  const { t } = useTranslation();
  const status = useStore((s) => s.addDeviceStatus);
  const incoming = useStore((s) => s.addDeviceIncoming);
  const busy = useStore((s) => s.addDeviceBusy);
  const error = useStore((s) => s.addDeviceError);
  const startAddDevice = useStore((s) => s.startAddDevice);
  const respondAddDevice = useStore((s) => s.respondAddDevice);
  const closeAddDevice = useStore((s) => s.closeAddDevice);
  const titleId = useId();
  const dialogRef = useDialogFocus<HTMLDivElement>(() => void closeAddDevice());

  const canRetry = status === "expired" || status === "error";

  return (
    <div className="modal-backdrop" onClick={() => void closeAddDevice()}>
      <div
        ref={dialogRef}
        tabIndex={-1}
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        onClick={(e) => e.stopPropagation()}
      >
        <h2 id={titleId}>{t("pairingDialog.addDevice.title")}</h2>

        {status === "listening" && (
          <>
            <p className="settings-sub">{t("pairingDialog.addDevice.waitingHint")}</p>
            <div className="searching">
              <span className="searching-spinner" />
              <span className="searching-label">{t("pairingDialog.addDevice.listening")}</span>
            </div>
          </>
        )}

        {status === "incoming" && incoming && (
          <div className="pairing-card">
            <p className="settings-sub">
              {t("pairingDialog.addDevice.incomingHint", { deviceLabel: incoming.deviceLabel })}
            </p>
            <div className="pairing-code" role="status">{incoming.fingerprint}</div>
            <p className="pairing-code-hint">{t("pairingDialog.compareHint")}</p>
          </div>
        )}

        {status === "expired" && (
          <p className="model-error" role="alert">{t("pairingDialog.expired")}</p>
        )}
        {status === "error" && error && (
          <p className="model-error" role="alert">{error}</p>
        )}

        <div className="update-actions">
          <div />
          <div className="update-actions-right">
            {status === "incoming" ? (
              <>
                <button
                  className="btn-ghost"
                  disabled={busy}
                  onClick={() => void respondAddDevice(false)}
                >
                  {t("pairingDialog.reject")}
                </button>
                {/* Deliberately not autoFocus'd — see the module doc comment. */}
                <button className="btn" disabled={busy} onClick={() => void respondAddDevice(true)}>
                  {t("pairingDialog.confirm")}
                </button>
              </>
            ) : (
              <>
                <button className="btn-ghost" onClick={() => void closeAddDevice()}>
                  {t("common.cancel")}
                </button>
                {canRetry && (
                  <button className="btn" onClick={() => void startAddDevice()}>
                    {t("pairingDialog.tryAgain")}
                  </button>
                )}
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
