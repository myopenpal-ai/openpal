import { useTranslation } from "react-i18next";
import { api } from "../lib/api";

/** Shown inline above the composer when the safety classifier
 * (`api.classifyRisk`, gated by `Settings.riskDetectionEnabled`, see
 * `store.ts`'s `send`) flags a user message as expressing self-harm or
 * crisis risk. Informational, not blocking — sending stays fully available
 * underneath it. Stateless by design: `visible` and `onDismiss` are fully
 * derived from `store.safetyAlerts[conversationId]` by `ChatView` (see that
 * field's doc comment for the per-conversation turn/dismissedTurn shape) —
 * this component holds no state of its own, so it can't lose a dismissal
 * just from unmounting (e.g. navigating to another view and back to Chat).
 *
 * `role="alert"` (assertive), not `-info`'s usual `"status"`: this content
 * must reliably reach a screen-reader user even if another polite live
 * region (e.g. the just-finished reply's streaming announcer) is still
 * speaking — same reasoning as `EngineBanner`'s.
 *
 * Deliberately doesn't name a country-specific number: maintaining accurate,
 * current hotline numbers for many countries is its own ongoing liability,
 * and the UI language doesn't reliably indicate where someone actually is.
 * findahelpline.com (see `learnMoreUrl`) does that lookup and verification
 * itself, so this banner always points there instead. */
export default function SafetyBanner({
  visible,
  onDismiss,
}: {
  visible: boolean;
  onDismiss: () => void;
}) {
  const { t } = useTranslation();

  if (!visible) {
    return null;
  }

  return (
    <div
      className="engine-banner engine-banner-info engine-banner-inline safety-banner"
      role="alert"
    >
      <div className="engine-banner-inner safety-banner-inner">
        <div className="engine-banner-text">
          <strong>{t("chatView.safetyBanner.title")}</strong>
          <p>{t("chatView.safetyBanner.body")}</p>
          <p>{t("chatView.safetyBanner.hotline")}</p>
        </div>
        <div className="safety-banner-actions">
          <button
            className="btn-quiet btn-warn-quiet"
            onClick={() => void api.openLink(t("chatView.safetyBanner.learnMoreUrl"))}
          >
            {t("chatView.safetyBanner.learnMore")}
          </button>
          <button
            className="btn-quiet btn-warn-quiet"
            aria-label={t("chatView.safetyBanner.dismissAriaLabel")}
            onClick={onDismiss}
          >
            {t("chatView.safetyBanner.dismiss")}
          </button>
        </div>
      </div>
    </div>
  );
}
