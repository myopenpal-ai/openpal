import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import FactoryResetConfirm from "../components/FactoryResetConfirm";
import PinPad from "../components/PinPad";
import { api } from "../lib/api";
import { useStore } from "../state/store";

type Stage = "pin" | "recovery" | "reset";

/** Placeholder pending product tuning: 3 free attempts + 3 backoff rounds
 * before the gate stops trusting the PIN and falls back to the recovery
 * code. */
const APP_LOCK_ESCALATE_AFTER_FAILURES = 6;

/**
 * Unskippable gate shown before anything else in the app once
 * `AppInfo.needsAppUnlock` is true and the user hasn't already passed it
 * this session (`appLockUnlocked` — see `state/store.ts`). Same full-screen
 * shell and focus-management idiom as `RecoveryUnlock`/`RecoverySetup`.
 *
 * `PinPad` renders through a portal as a full-screen, opaque overlay (see
 * its own doc comment), so the "erase everything" escape hatch below is
 * rendered above it via a higher z-index (`.app-lock-erase-link`) rather
 * than inside the scene it would otherwise cover.
 */
export default function AppLockGate() {
  const { t } = useTranslation();
  const sceneRef = useRef<HTMLElement>(null);
  const [stage, setStage] = useState<Stage>("pin");

  useEffect(() => {
    const el = sceneRef.current;
    if (!el) return;
    const heading = el.querySelector<HTMLElement>("h1");
    (heading ?? el).focus({ preventScroll: true });
  }, [stage]);

  const [pinBusy, setPinBusy] = useState(false);
  const [pinError, setPinError] = useState<string | null>(null);
  const [lockedForSeconds, setLockedForSeconds] = useState(0);
  const [failCount, setFailCount] = useState(0);

  const handlePinSubmit = async (pin: string) => {
    if (pinBusy) return;
    setPinBusy(true);
    setPinError(null);
    setLockedForSeconds(0);
    try {
      const result = await api.verifyAppLockPin(pin);
      if (result.ok) {
        useStore.setState({ appLockUnlocked: true });
        await useStore.getState().finishBootAfterUnlock();
        return;
      }
      setFailCount(result.failCount);
      if (result.failCount >= APP_LOCK_ESCALATE_AFTER_FAILURES) {
        setStage("recovery");
      } else if (result.lockedForSeconds > 0) {
        setLockedForSeconds(result.lockedForSeconds);
      } else {
        setPinError(t("appLock.gate.wrongPin"));
      }
    } catch (err: unknown) {
      setPinError(String(err));
    } finally {
      setPinBusy(false);
    }
  };

  const [code, setCode] = useState("");
  const [recoverySubmitting, setRecoverySubmitting] = useState(false);
  const [recoveryError, setRecoveryError] = useState<string | null>(null);

  const handleRecoverySubmit = async () => {
    const trimmed = code.trim();
    if (!trimmed || recoverySubmitting) return;
    setRecoverySubmitting(true);
    setRecoveryError(null);
    try {
      await api.unlockAppLockWithRecoveryCode(trimmed);
      // Opens Settings → Privacy & safety straight into the banner explaining
      // the PIN was cleared — see SecuritySection's `appLockJustReset` handling.
      useStore.setState({
        appLockUnlocked: true,
        moreDialogPage: "privacy",
        appLockJustReset: true,
      });
      await useStore.getState().finishBootAfterUnlock();
    } catch (err: unknown) {
      setRecoveryError(t("appLock.gate.recoveryError", { message: String(err) }));
      setRecoverySubmitting(false);
    }
  };

  const attemptsRemaining = Math.max(APP_LOCK_ESCALATE_AFTER_FAILURES - failCount, 0);

  return (
    <div className="onboarding" data-tauri-drag-region>
      <div className="ob-glow" aria-hidden />
      <section className="ob-scene" ref={sceneRef} tabIndex={-1}>
        <p className="ob-brand ob-appear">OpenPal</p>

        {stage === "pin" && (
          <PinPad
            title={t("appLock.gate.pinTitle")}
            subtitle={
              failCount > 0
                ? t("appLock.gate.attemptsRemaining", { count: attemptsRemaining })
                : undefined
            }
            error={pinError}
            busy={pinBusy}
            lockedForSeconds={lockedForSeconds}
            renderLocked={(seconds) => t("appLock.gate.lockedFor", { seconds })}
            onSubmit={(pin) => void handlePinSubmit(pin)}
          />
        )}

        {stage === "recovery" && (
          <>
            <h1 className="ob-title ob-appear" tabIndex={-1}>
              {t("appLock.gate.recoveryTitle")}
            </h1>
            <p className="ob-sub ob-appear">{t("appLock.gate.recoveryIntro")}</p>

            <form
              className="rec-unlock-form ob-appear"
              onSubmit={(e) => {
                e.preventDefault();
                void handleRecoverySubmit();
              }}
            >
              <label className="rec-unlock-label" htmlFor="app-lock-recovery-code">
                {t("appLock.gate.recoveryInputLabel")}
              </label>
              <input
                id="app-lock-recovery-code"
                className="rec-unlock-input"
                type="text"
                autoComplete="off"
                autoCorrect="off"
                autoCapitalize="off"
                spellCheck={false}
                placeholder={t("appLock.gate.recoveryInputPlaceholder")}
                value={code}
                disabled={recoverySubmitting}
                aria-invalid={recoveryError ? true : undefined}
                aria-describedby={recoveryError ? "app-lock-recovery-error" : undefined}
                onChange={(e) => setCode(e.target.value)}
              />

              {recoveryError && (
                <p className="rec-error" id="app-lock-recovery-error" role="alert">
                  {recoveryError}
                </p>
              )}

              <button
                type="submit"
                className="btn btn-big"
                disabled={!code.trim() || recoverySubmitting}
              >
                {recoverySubmitting
                  ? t("appLock.gate.recoveryConfirming")
                  : t("appLock.gate.recoveryButton")}
              </button>
            </form>
          </>
        )}

        {stage === "reset" && (
          <>
            <h1 className="ob-title ob-appear" tabIndex={-1}>
              {t("appLock.gate.resetTitle")}
            </h1>
            <FactoryResetConfirm onCancel={() => setStage("pin")} />
          </>
        )}
      </section>

      {stage !== "reset" && (
        <button
          type="button"
          className="btn-quiet app-lock-erase-link"
          onClick={() => setStage("reset")}
        >
          {t("appLock.gate.eraseLink")}
        </button>
      )}
    </div>
  );
}
