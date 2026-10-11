import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import FactoryResetConfirm from "../components/FactoryResetConfirm";
import { api } from "../lib/api";
import { useStore } from "../state/store";

/**
 * Unskippable gate shown before anything else in the app once
 * `AppInfo.needsRecovery` is true — the OS keychain that normally unlocks
 * the encryption key silently failed (a lost keychain entry, an OS
 * reinstall, …), and the only way back in is the recovery code the user
 * saved during `RecoverySetup`. The inverse of that screen: instead of
 * generating and displaying a code, this one collects one and submits it.
 *
 * Same full-screen shell and focus-management idiom as `RecoverySetup`:
 * focus the heading on mount so keyboard/AT users land on the new content
 * immediately.
 */
export default function RecoveryUnlock() {
  const { t } = useTranslation("recovery");
  const { t: tCommon } = useTranslation();
  const sceneRef = useRef<HTMLElement>(null);
  useEffect(() => {
    const el = sceneRef.current;
    if (!el) return;
    const heading = el.querySelector<HTMLElement>("h1");
    (heading ?? el).focus({ preventScroll: true });
  }, []);

  const [code, setCode] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Today this screen is a genuine dead end if the user's recovery code is
  // also lost — swaps the form for the shared factory-reset panel instead.
  const [showReset, setShowReset] = useState(false);

  const handleSubmit = async () => {
    const trimmed = code.trim();
    if (!trimmed || submitting) return;
    setSubmitting(true);
    setError(null);
    try {
      await api.attemptRecoveryUnlock(trimmed);
      // A user who just proved a valid recovery code is strictly more
      // verified than a PIN would require — they shouldn't also be
      // immediately hit with an app-lock PIN prompt right after this.
      useStore.setState({ appLockUnlocked: true });
      // The backend has just built its store for the first time this
      // launch (see `commands::attempt_recovery_unlock`'s call into
      // `state::finish_unlock`) — `finishBootAfterUnlock` both re-fetches
      // `appInfo` (so the App.tsx gate re-evaluates `needsRecovery` to
      // false) and loads everything else `boot()`'s normal path would have
      // loaded already, had the app not been locked out. Without this,
      // `settings` stays `null` and the app never gets past the titlebar.
      await useStore.getState().finishBootAfterUnlock();
    } catch (err: unknown) {
      // Deliberately don't clear `code` here — the user may have just typed
      // it out carefully and shouldn't have to redo that after a typo.
      setError(t("unlockError", { message: String(err) }));
      setSubmitting(false);
    }
  };

  return (
    <div className="onboarding" data-tauri-drag-region>
      <div className="ob-glow" aria-hidden />
      <section className="ob-scene" ref={sceneRef} tabIndex={-1}>
        <p className="ob-brand ob-appear">OpenPal</p>
        <h1 className="ob-title ob-appear" tabIndex={-1}>
          {t("unlockTitle")}
        </h1>
        <p className="ob-sub ob-appear">{t("unlockIntro")}</p>

        {showReset ? (
          <FactoryResetConfirm onCancel={() => setShowReset(false)} />
        ) : (
          <>
            <form
              className="rec-unlock-form ob-appear"
              onSubmit={(e) => {
                e.preventDefault();
                void handleSubmit();
              }}
            >
              <label className="rec-unlock-label" htmlFor="rec-unlock-code">
                {t("unlockInputLabel")}
              </label>
              <input
                id="rec-unlock-code"
                className="rec-unlock-input"
                type="text"
                autoComplete="off"
                autoCorrect="off"
                autoCapitalize="off"
                spellCheck={false}
                placeholder={t("unlockInputPlaceholder")}
                value={code}
                disabled={submitting}
                aria-invalid={error ? true : undefined}
                aria-describedby={error ? "rec-unlock-error" : undefined}
                onChange={(e) => setCode(e.target.value)}
              />

              {error && (
                <p className="rec-error" id="rec-unlock-error" role="alert">
                  {error}
                </p>
              )}

              <button
                type="submit"
                className="btn btn-big"
                disabled={!code.trim() || submitting}
              >
                {submitting ? t("unlockConfirming") : t("unlockButton")}
              </button>
            </form>

            <button
              type="button"
              className="btn-quiet ob-appear"
              onClick={() => setShowReset(true)}
            >
              {tCommon("appLock.gate.eraseLink")}
            </button>
          </>
        )}
      </section>
    </div>
  );
}
