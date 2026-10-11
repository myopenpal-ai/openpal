import { useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { useDialogFocus } from "../components/useDialogFocus";
import { useStore } from "../state/store";

/**
 * Device B's side of "Join a group" (docs/SYNC_PAIRING_PROTOCOL.md, Phase 2).
 * Mounted whenever `joinGroupOpen` is true (see App.tsx) — the Settings Sync
 * section's "Join a group…" button calls `openJoinGroup`, which just opens
 * this dialog on its form step; `submitJoinGroup` (below) is what actually
 * starts `sync_pair_request`'s streamed flow.
 *
 * `groupSecret`/`deviceLabel` are kept as local component state rather than
 * in the store — they're only needed to make the one `submitJoinGroup` call
 * (or to resubmit them after a retry), so there's no reason for that
 * short-lived, moderately sensitive input to round-trip through global
 * state.
 *
 * The "finish joining" button is deliberately never focused by default
 * (see `SyncPairingEvent`'s doc comment in lib/types.ts) — the fingerprint must
 * actually be read and compared against Device A's screen before it's
 * pressed.
 */
export default function JoinGroupDialog() {
  const { t } = useTranslation();
  const status = useStore((s) => s.joinGroupStatus);
  const fingerprint = useStore((s) => s.joinGroupFingerprint);
  const busy = useStore((s) => s.joinGroupBusy);
  const error = useStore((s) => s.joinGroupError);
  const openJoinGroup = useStore((s) => s.openJoinGroup);
  const submitJoinGroup = useStore((s) => s.submitJoinGroup);
  const finishJoinGroup = useStore((s) => s.finishJoinGroup);
  const closeJoinGroup = useStore((s) => s.closeJoinGroup);
  const titleId = useId();
  const dialogRef = useDialogFocus<HTMLDivElement>(() => void closeJoinGroup());

  const [groupSecret, setGroupSecret] = useState("");
  const [deviceLabel, setDeviceLabel] = useState("");

  const canSubmit = groupSecret.trim() !== "" && deviceLabel.trim() !== "";
  const submit = () => {
    if (!canSubmit) return;
    void submitJoinGroup(groupSecret.trim(), deviceLabel.trim());
  };

  const canRetry = status === "expired" || status === "error";
  const showFingerprint =
    (status === "awaitingConfirmation" || status === "readyToInstall") && fingerprint;

  return (
    <div className="modal-backdrop" onClick={() => void closeJoinGroup()}>
      <div
        ref={dialogRef}
        tabIndex={-1}
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        onClick={(e) => e.stopPropagation()}
      >
        <h2 id={titleId}>{t("pairingDialog.joinGroup.title")}</h2>

        {status === "form" && (
          <>
            <p className="settings-sub">{t("pairingDialog.joinGroup.hint")}</p>
            <label className="settings-field">
              <span className="settings-label">{t("pairingDialog.joinGroup.secretLabel")}</span>
              <input
                autoFocus
                value={groupSecret}
                autoComplete="off"
                autoCorrect="off"
                autoCapitalize="off"
                spellCheck={false}
                onChange={(e) => setGroupSecret(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") submit();
                }}
              />
            </label>
            <label className="settings-field">
              <span className="settings-label">{t("pairingDialog.joinGroup.labelLabel")}</span>
              <input
                value={deviceLabel}
                autoComplete="off"
                onChange={(e) => setDeviceLabel(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") submit();
                }}
              />
            </label>
          </>
        )}

        {status === "awaitingChallenge" && (
          <div className="searching">
            <span className="searching-spinner" />
            <span className="searching-label">{t("pairingDialog.joinGroup.connecting")}</span>
          </div>
        )}

        {showFingerprint && (
          <div className="pairing-card">
            <p className="settings-sub">{t("pairingDialog.compareHint")}</p>
            <div className="pairing-code" role="status">{fingerprint}</div>
            {status === "awaitingConfirmation" && (
              <div className="searching">
                <span className="searching-spinner" />
                <span className="searching-label">
                  {t("pairingDialog.joinGroup.awaitingOtherDevice")}
                </span>
              </div>
            )}
            {status === "readyToInstall" && (
              <p className="pairing-code-hint">{t("pairingDialog.joinGroup.readyPrompt")}</p>
            )}
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
            {status === "form" ? (
              <>
                <button className="btn-ghost" onClick={() => void closeJoinGroup()}>
                  {t("common.cancel")}
                </button>
                <button className="btn" disabled={!canSubmit} onClick={submit}>
                  {t("pairingDialog.joinGroup.joinButton")}
                </button>
              </>
            ) : status === "readyToInstall" ? (
              <>
                <button
                  className="btn-ghost"
                  disabled={busy}
                  onClick={() => void finishJoinGroup(false)}
                >
                  {t("pairingDialog.reject")}
                </button>
                {/* Deliberately not autoFocus'd — see the module doc comment. */}
                <button className="btn" disabled={busy} onClick={() => void finishJoinGroup(true)}>
                  {t("pairingDialog.joinGroup.finishButton")}
                </button>
              </>
            ) : (
              <>
                <button className="btn-ghost" onClick={() => void closeJoinGroup()}>
                  {t("common.cancel")}
                </button>
                {canRetry && (
                  <button className="btn" onClick={() => openJoinGroup()}>
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
