import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../lib/api";
import { copyText } from "../lib/clipboard";
import { useStore } from "../state/store";

/**
 * One-time, unskippable gate shown before anything else in the app once
 * `AppInfo.needsRecoverySetup` is true — on a brand-new install, and on the
 * very next launch of every existing install (the encryption key this code
 * backs up was minted silently in a prior release, so nobody has a code
 * yet). Generates a fresh recovery code, lets the user copy/save it, and
 * only proceeds once they've explicitly acknowledged saving it and the
 * backend has recorded that confirmation.
 *
 * A single scene (unlike `Onboarding`'s multi-scene flow) but the same
 * full-screen shell and focus-management idiom: focus the heading on mount
 * so keyboard/AT users land on the new content immediately.
 */
export default function RecoverySetup() {
  const { t } = useTranslation("recovery");
  const sceneRef = useRef<HTMLElement>(null);
  useEffect(() => {
    const el = sceneRef.current;
    if (!el) return;
    const heading = el.querySelector<HTMLElement>("h1");
    (heading ?? el).focus({ preventScroll: true });
  }, []);

  const [code, setCode] = useState<string | null>(null);
  const [genError, setGenError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [acknowledged, setAcknowledged] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [confirmError, setConfirmError] = useState<string | null>(null);

  const generate = useCallback(() => {
    setGenError(null);
    setCode(null);
    api
      .generateRecoveryCode()
      .then(setCode)
      .catch((err: unknown) => setGenError(String(err)));
  }, []);

  // Fast, local, no-network — but still asynchronous IPC, so this can land
  // after first paint; show a brief loading state rather than assuming it's
  // available synchronously.
  useEffect(() => {
    generate();
  }, [generate]);

  const handleCopy = async () => {
    if (!code) return;
    if (await copyText(code)) {
      setCopied(true);
      setTimeout(() => setCopied(false), 2500);
    }
  };

  const handleContinue = async () => {
    if (!code || confirming) return;
    setConfirming(true);
    setConfirmError(null);
    try {
      await api.confirmRecoveryCode(code);
      // Re-check app state so the App.tsx gate re-evaluates to false and the
      // app proceeds normally — same "refetch and stash in the store"
      // pattern MealPlannerSpace uses to resync `conversations` after a
      // backend write, rather than a dedicated store action for a value
      // that's otherwise only ever set once, at boot.
      useStore.setState({ appInfo: await api.getAppInfo() });
    } catch (err: unknown) {
      setConfirmError(t("confirmError", { message: String(err) }));
      setConfirming(false);
    }
  };

  return (
    <div className="onboarding" data-tauri-drag-region>
      <div className="ob-glow" aria-hidden />
      <section className="ob-scene" ref={sceneRef} tabIndex={-1}>
        <p className="ob-brand ob-appear">OpenPal</p>
        <h1 className="ob-title ob-appear" tabIndex={-1}>
          {t("title")}
        </h1>
        <p className="ob-sub ob-appear">{t("intro")}</p>
        <p className="ob-sub ob-appear">{t("warning")}</p>

        {genError ? (
          <>
            <p className="ob-sub" role="alert">
              {genError}
            </p>
            <button className="btn-ghost" onClick={generate}>
              {t("retryGenerate")}
            </button>
          </>
        ) : code ? (
          <>
            <p className="ob-eyebrow ob-appear" id="rec-code-caption">
              {t("codeLabel")}
            </p>
            <code className="rec-code ob-appear" aria-labelledby="rec-code-caption">
              {code}
            </code>
            <div className="rec-code-actions ob-appear">
              <button className="btn-ghost" onClick={() => void handleCopy()}>
                {copied ? t("copied") : t("copy")}
              </button>
            </div>
          </>
        ) : (
          <p className="ob-sub" role="status">
            {t("generating")}
          </p>
        )}

        <label className="rec-checkbox ob-appear">
          <input
            type="checkbox"
            checked={acknowledged}
            disabled={!code}
            onChange={(e) => setAcknowledged(e.target.checked)}
          />
          <span>{t("checkboxLabel")}</span>
        </label>

        {confirmError && (
          <p className="rec-error" role="alert">
            {confirmError}
          </p>
        )}

        <div className="ob-appear">
          <button
            className="btn btn-big"
            disabled={!code || !acknowledged || confirming}
            onClick={() => void handleContinue()}
          >
            {confirming ? t("confirming") : t("continueButton")}
          </button>
        </div>
      </section>
    </div>
  );
}
