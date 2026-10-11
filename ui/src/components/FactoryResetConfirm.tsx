import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../lib/api";

export interface FactoryResetConfirmProps {
  onCancel: () => void;
}

/** Deliberately not localized — a fixed, unambiguous phrase is safer for a
 * destructive confirmation than a translated one that could vary subtly
 * between locales. */
const CONFIRM_PHRASE = "ERASE";

/**
 * Shared "type a phrase to confirm" panel for the one truly destructive
 * action in the app: wiping every conversation, project, setting, and
 * skill. Reused from both dead-end recovery screens (`RecoveryUnlock`,
 * `AppLockGate`) and Settings' own danger zone (`SecuritySection`) — same
 * inline-swap-in shape as `Library.tsx`'s delete-confirm panels, never a
 * native `window.confirm()`.
 */
export default function FactoryResetConfirm({ onCancel }: FactoryResetConfirmProps) {
  const { t } = useTranslation();
  const [phrase, setPhrase] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const doReset = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.factoryReset();
      // The whole process exits and relaunches on success — nothing else to
      // do here; this component won't survive to render another state.
    } catch (err: unknown) {
      setError(String(err));
      setBusy(false);
    }
  };

  return (
    <div className="model-confirm reset-confirm">
      <p className="model-confirm-text">{t("appLock.reset.warning")}</p>
      <label className="reset-confirm-label">
        {t("appLock.reset.instruction", { phrase: CONFIRM_PHRASE })}
        <input
          value={phrase}
          autoComplete="off"
          autoCorrect="off"
          autoCapitalize="off"
          spellCheck={false}
          aria-label={t("appLock.reset.instruction", { phrase: CONFIRM_PHRASE })}
          onChange={(e) => setPhrase(e.target.value)}
        />
      </label>
      {error && (
        <p className="model-error" role="alert">
          {error}
        </p>
      )}
      <div className="model-confirm-actions">
        <button
          className="btn-quiet btn-danger-quiet"
          disabled={phrase.trim().toUpperCase() !== CONFIRM_PHRASE || busy}
          onClick={() => void doReset()}
        >
          {busy ? t("appLock.reset.working") : t("appLock.reset.confirmButton")}
        </button>
        <button className="btn-quiet" disabled={busy} onClick={onCancel}>
          {t("common.cancel")}
        </button>
      </div>
    </div>
  );
}
