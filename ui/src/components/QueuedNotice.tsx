import { useTranslation } from "react-i18next";

/**
 * What a request to the model shows while it waits behind another one (see
 * `Store.modelRequests`): one line saying so, and a control to cancel just
 * this request. Shared by a waiting chat reply, Translate/Correct and the
 * Message improver, so a queued request reads the same wherever it was
 * started. `label` replaces the line for a chat reply that is waiting for the
 * model to load rather than for another request.
 */
export default function QueuedNotice({
  label,
  onCancel,
}: {
  label?: string;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  return (
    <span className="queued-notice">
      <span role="status" className="queued-notice-label">
        <span className="searching-spinner" aria-hidden="true" />
        {label ?? t("chatView.queued.waiting")}
      </span>
      <button type="button" className="queued-notice-cancel" onClick={onCancel}>
        {t("chatView.queued.cancel")}
      </button>
    </span>
  );
}
