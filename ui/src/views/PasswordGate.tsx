import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../lib/api";
import { useStore } from "../state/store";

type Stage = "password" | "recovery";

/**
 * The password gate shown before anything else in the app once
 * `AppInfo.needsPassword` — first run, create your password (a brand-new
 * install, or a pre-password install's first launch after upgrade) — or
 * `AppInfo.needsUnlock` — the normal every-launch "enter your password"
 * gate. The backend won't build a store (or load `settings`) until this
 * passes, so this screen renders without it, exactly like `RecoveryUnlock`.
 *
 * Standard Notes-style: the password derives the root key that unwraps the
 * items key, which in turn unwraps every conversation's own key. `create`
 * derives a fresh root (adopting a legacy DEK where one exists) and writes
 * `password.wrap`; `unlock` verifies the same password back out of it. A
 * wrong password decrypts nothing — `unlock_with_password` answers every
 * failure with the exact string `"Wrong password"`, mapped here to the
 * locale's `auth.password.wrong`.
 *
 * Create mode also offers "continue without a password" when
 * `AppInfo.canStartWithoutPassword` (a fresh install, or a legacy plaintext
 * store): the Standard Notes "no account, no passcode" choice — nothing is
 * encrypted at rest from then on, and the marker the backend writes makes
 * every later launch skip this gate entirely. Never offered on a legacy
 * pre-password install whose data is already encrypted (the backend reports
 * `canStartWithoutPassword: false` there).
 *
 * The forgot-your-password fallback is the recovery code: the same form
 * `RecoveryUnlock` uses, rendered inline as a second stage (reachable in
 * portable too — with the OS-keychain primary slot gone, the recovery code
 * is the only way back in once the password is lost). Same full-screen shell
 * and focus-management idiom as `RecoveryUnlock`/`RecoverySetup`.
 */
export default function PasswordGate() {
  const { t } = useTranslation();
  const { t: tRecovery } = useTranslation("recovery");
  const appInfo = useStore((s) => s.appInfo);
  const sceneRef = useRef<HTMLElement>(null);
  useEffect(() => {
    const el = sceneRef.current;
    if (!el) return;
    const heading = el.querySelector<HTMLElement>("h1");
    (heading ?? el).focus({ preventScroll: true });
  }, []);

  // Backend guarantee: `needsPassword` and `needsUnlock` are mutually
  // exclusive (see `get_app_info`), but prefer "create" whenever both were
  // somehow reported — creating (or re-creating on the legacy path) is the
  // strictest interpretation and always converges.
  const create = !!appInfo?.needsPassword && !appInfo?.needsUnlock;

  const [stage, setStage] = useState<Stage>("password");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Create mode: password + confirmation (checked for a match before submit).
  const [newPassword, setNewPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  // Unlock mode: the password itself.
  const [password, setPassword] = useState("");
  // Recovery stage (unlock mode's "forgot your password?" fallback).
  const [code, setCode] = useState("");
  const [recoveryError, setRecoveryError] = useState<string | null>(null);

  const handleCreate = async () => {
    if (submitting) return;
    if (newPassword !== confirmPassword) {
      setError(t("auth.password.mismatch"));
      return;
    }
    if (!newPassword) return;
    setSubmitting(true);
    setError(null);
    try {
      // `create_password` mints (or adopts) the items key, migrates any
      // legacy plaintext rows, and finishes the backend's boot itself —
      // `AppState` is built by the time this resolves. A password just
      // proven is at least as strong as a PIN, so don't stack the app-lock
      // gate on top, mirroring `RecoveryUnlock`.
      await api.createPassword(newPassword);
      useStore.setState({ appLockUnlocked: true });
      await useStore.getState().finishBootAfterUnlock();
    } catch (err: unknown) {
      setError(String(err));
      setSubmitting(false);
    }
  };

  const handleUnlock = async () => {
    if (submitting) return;
    if (!password) return;
    setSubmitting(true);
    setError(null);
    try {
      await api.unlockWithPassword(password);
      useStore.setState({ appLockUnlocked: true });
      await useStore.getState().finishBootAfterUnlock();
    } catch (err: unknown) {
      // `unlock_with_password` answers every failure (wrong password, missing
      // wrap file, I/O) with the exact string "Wrong password" — the only
      // honest message is that the password didn't work, so map it to the
      // locale key rather than leaking an internal error.
      setError(String(err) === "Wrong password" ? t("auth.password.wrong") : String(err));
      setSubmitting(false);
    }
  };

  const handleRecovery = async () => {
    const trimmed = code.trim();
    if (!trimmed || submitting) return;
    setSubmitting(true);
    setRecoveryError(null);
    try {
      const outcome = await api.attemptRecoveryUnlock(trimmed);
      useStore.setState({ appLockUnlocked: true });
      if (outcome.kind === "unlocked") {
        await useStore.getState().finishBootAfterUnlock();
      } else {
        // Legacy migration window: the code proved the old DEK and re-minted
        // it, but the mandatory password still has to be created. The backend
        // hasn't built `AppState` (and now reports `needsPassword`) — re-fetch
        // `appInfo` so this gate re-renders in create mode, and drop back to
        // the password stage to show it.
        const appInfo = await api.getAppInfo();
        useStore.setState({ appInfo });
        setStage("password");
        setSubmitting(false);
      }
    } catch (err: unknown) {
      setRecoveryError(tRecovery("unlockError", { message: String(err) }));
      setSubmitting(false);
    }
  };

  const handleStartWithoutPassword = async () => {
    if (submitting) return;
    setSubmitting(true);
    setError(null);
    try {
      // `start_without_password` writes the `keys/nopass.marker` (idempotent)
      // and finishes the backend's boot with no DEK — the plaintext
      // "no password = no encryption at rest" layout — so `AppState` exists
      // by the time this resolves, and every later launch skips this gate.
      await api.startWithoutPassword();
      useStore.setState({ appLockUnlocked: true });
      await useStore.getState().finishBootAfterUnlock();
    } catch (err: unknown) {
      setError(String(err));
      setSubmitting(false);
    }
  };

  const pwValid = create ? newPassword.length > 0 && confirmPassword.length > 0 : password.length > 0;

  return (
    <div className="onboarding" data-tauri-drag-region>
      <div className="ob-glow" aria-hidden />
      <section className="ob-scene" ref={sceneRef} tabIndex={-1}>
        <p className="ob-brand ob-appear">OpenPal</p>

        {stage === "recovery" ? (
          <>
            <h1 className="ob-title ob-appear" tabIndex={-1}>
              {tRecovery("unlockTitle")}
            </h1>
            <p className="ob-sub ob-appear">{tRecovery("unlockIntro")}</p>

            <form
              className="rec-unlock-form ob-appear"
              onSubmit={(e) => {
                e.preventDefault();
                void handleRecovery();
              }}
            >
              <label className="rec-unlock-label" htmlFor="password-gate-recovery-code">
                {tRecovery("unlockInputLabel")}
              </label>
              <input
                id="password-gate-recovery-code"
                className="rec-unlock-input"
                type="text"
                autoComplete="off"
                autoCorrect="off"
                autoCapitalize="off"
                spellCheck={false}
                placeholder={tRecovery("unlockInputPlaceholder")}
                value={code}
                disabled={submitting}
                aria-invalid={recoveryError ? true : undefined}
                aria-describedby={recoveryError ? "password-gate-recovery-error" : undefined}
                onChange={(e) => setCode(e.target.value)}
              />

              {recoveryError && (
                <p className="rec-error" id="password-gate-recovery-error" role="alert">
                  {recoveryError}
                </p>
              )}

              <button type="submit" className="btn btn-big" disabled={!code.trim() || submitting}>
                {submitting ? tRecovery("unlockConfirming") : tRecovery("unlockButton")}
              </button>
            </form>

            <button
              type="button"
              className="btn-quiet ob-appear"
              onClick={() => {
                setStage("password");
                setRecoveryError(null);
              }}
            >
              {t("auth.password.backToPassword")}
            </button>
          </>
        ) : (
          <>
            <h1 className="ob-title ob-appear" tabIndex={-1}>
              {create ? t("auth.password.createTitle") : t("auth.password.unlockTitle")}
            </h1>
            <p className="ob-sub ob-appear">
              {create ? t("auth.password.createBody") : t("auth.password.unlockBody")}
            </p>

            <form
              className="rec-unlock-form ob-appear"
              onSubmit={(e) => {
                e.preventDefault();
                void (create ? handleCreate() : handleUnlock());
              }}
            >
              {create && (
                <>
                  <label className="rec-unlock-label" htmlFor="password-gate-create">
                    {t("auth.password.createTitle")}
                  </label>
                  <input
                    id="password-gate-create"
                    className="rec-unlock-input"
                    type="password"
                    autoComplete="new-password"
                    autoCorrect="off"
                    autoCapitalize="off"
                    spellCheck={false}
                    value={newPassword}
                    disabled={submitting}
                    aria-invalid={error ? true : undefined}
                    onChange={(e) => setNewPassword(e.target.value)}
                  />
                  <label className="rec-unlock-label" htmlFor="password-gate-confirm">
                    {t("auth.password.confirmLabel")}
                  </label>
                  <input
                    id="password-gate-confirm"
                    className="rec-unlock-input"
                    type="password"
                    autoComplete="new-password"
                    autoCorrect="off"
                    autoCapitalize="off"
                    spellCheck={false}
                    value={confirmPassword}
                    disabled={submitting}
                    onChange={(e) => setConfirmPassword(e.target.value)}
                  />
                </>
              )}
              {!create && (
                <>
                  <label className="rec-unlock-label" htmlFor="password-gate-unlock">
                    {t("auth.password.unlockTitle")}
                  </label>
                  <input
                    id="password-gate-unlock"
                    className="rec-unlock-input"
                    type="password"
                    autoComplete="current-password"
                    autoCorrect="off"
                    autoCapitalize="off"
                    spellCheck={false}
                    value={password}
                    disabled={submitting}
                    aria-invalid={error ? true : undefined}
                    aria-describedby={error ? "password-gate-error" : undefined}
                    onChange={(e) => setPassword(e.target.value)}
                  />
                </>
              )}

              {error && (
                <p className="rec-error" id="password-gate-error" role="alert">
                  {error}
                </p>
              )}

              <button type="submit" className="btn btn-big" disabled={!pwValid || submitting}>
                {submitting
                  ? create
                    ? t("auth.password.creating")
                    : t("auth.password.unlocking")
                  : create
                    ? t("auth.password.createTitle")
                    : t("auth.password.unlockTitle")}
              </button>
            </form>

            {create && appInfo?.canStartWithoutPassword && (
              <button
                type="button"
                className="btn-quiet ob-appear"
                onClick={() => void handleStartWithoutPassword()}
                disabled={submitting}
              >
                {t("auth.password.continueWithout")}
              </button>
            )}

            {!create && (
              <button
                type="button"
                className="btn-quiet ob-appear"
                onClick={() => {
                  setStage("recovery");
                  setError(null);
                }}
              >
                {t("auth.password.recoveryHint")}
              </button>
            )}
          </>
        )}
      </section>
    </div>
  );
}
