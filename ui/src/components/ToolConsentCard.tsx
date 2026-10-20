import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useDialogFocus } from "./useDialogFocus";
import type { DangerFlag } from "../lib/types";
import { useStore, type PendingToolConsent } from "../state/store";

/**
 * The agent-mode consent prompt: Allow once / Always allow / Deny for one
 * pending tool call. `summary`/`detail`/resolved paths are rendered as
 * plain text ONLY — never through the Markdown renderer used for ordinary
 * chat messages. This is a hard rule, not a style choice: this content can
 * be attacker-influenceable (a file's content, a command's own stdout,
 * fed back in from an earlier step), and every value here reaches this
 * component as a plain string prop rendered as a React text child, which
 * React always escapes — it must never be passed through `Markdown` or
 * `dangerouslySetInnerHTML`.
 */
export function ToolConsentCard({ consent }: { consent: PendingToolConsent }) {
  const { t } = useTranslation();
  const respondToolConsent = useStore((s) => s.respondToolConsent);
  const [denying, setDenying] = useState(false);
  const [note, setNote] = useState("");
  const [submitting, setSubmitting] = useState(false);
  // No onClose: this gates a potentially dangerous action, so Escape must
  // not be able to silently dismiss it — only the explicit Allow/Deny
  // buttons resolve a pending consent.
  const dialogRef = useDialogFocus<HTMLDivElement>();

  const respond = async (decision: "allowOnce" | "alwaysAllow" | "deny", withNote?: string) => {
    if (submitting) return;
    setSubmitting(true);
    await respondToolConsent(decision, withNote);
  };

  return (
    <div
      ref={dialogRef}
      tabIndex={-1}
      className="tool-consent-card"
      role="alertdialog"
      aria-modal="true"
      aria-label={t("chatView.toolConsent.wantsTo", { tool: consent.toolId })}
    >
      <div className="tool-consent-title">
        {t("chatView.toolConsent.wantsTo", { tool: consent.toolId })}
      </div>
      {consent.via && (
        <div className="tool-consent-via">
          {t("chatView.toolConsent.requestedBy", { kind: consent.via })}
        </div>
      )}
      <div className="tool-consent-summary">{consent.summary}</div>
      {consent.detail !== consent.summary && (
        <pre className="tool-consent-detail">{consent.detail}</pre>
      )}
      {consent.resolvedPaths
        .filter((p) => p.raw !== p.canonical)
        .map((p, i) => (
          <div key={i} className="tool-consent-resolved">
            {t("chatView.toolConsent.resolvedPathPrefix")} <code>{p.canonical}</code>
          </div>
        ))}
      {consent.resolvedPaths.some((p) => !p.insideWorkspace) && (
        <div className="tool-consent-flag tool-consent-flag-warn">
          {t("chatView.toolConsent.outsideWorkspace")}
        </div>
      )}
      {consent.dangerFlags.map((flag, i) => (
        <div key={i} className="tool-consent-flag tool-consent-flag-warn">
          {/* `as unknown as ...`: a direct structural check here (instead of
           * this two-step cast) is what actually re-triggers the "excessively
           * deep" instantiation this function's signature was narrowed to
           * avoid — see that comment. */}
          {dangerFlagText(flag, t)}
        </div>
      ))}

      {!denying ? (
        <div className="tool-consent-actions">
          <button
            type="button"
            className="btn-quiet"
            disabled={submitting}
            onClick={() => setDenying(true)}
          >
            {t("chatView.toolConsent.deny")}
          </button>
          <button
            type="button"
            className="btn-ghost"
            disabled={submitting}
            onClick={() => void respond("allowOnce")}
          >
            {t("chatView.toolConsent.allowOnce")}
          </button>
          <button
            type="button"
            className="btn-big tool-consent-primary"
            disabled={submitting}
            onClick={() => void respond("alwaysAllow")}
          >
            {t("chatView.toolConsent.alwaysAllow")}
          </button>
        </div>
      ) : (
        <div className="tool-consent-deny-note">
          <label className="settings-field">
            <span className="settings-label">{t("chatView.toolConsent.denyNoteLabel")}</span>
            <input
              type="text"
              autoFocus
              value={note}
              placeholder={t("chatView.toolConsent.denyNotePlaceholder")}
              onChange={(e) => setNote(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void respond("deny", note.trim() || undefined);
              }}
            />
          </label>
          <div className="tool-consent-actions">
            <button
              type="button"
              className="btn-quiet"
              disabled={submitting}
              onClick={() => setDenying(false)}
            >
              {t("common.cancel")}
            </button>
            <button
              type="button"
              className="btn-big tool-consent-primary"
              disabled={submitting}
              onClick={() => void respond("deny", note.trim() || undefined)}
            >
              {t("chatView.toolConsent.deny")}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

function dangerFlagText(flag: DangerFlag, tFull: ReturnType<typeof useTranslation>["t"]): string {
  // Cast once, here, to a plain `(key: string) => string` — every call
  // below passes a literal key and nothing else, so nothing real is lost.
  // Calling `tFull` directly (its real, heavily overloaded i18next type)
  // repeatedly in this switch hits TS's "type instantiation is excessively
  // deep" limit as CustomTypeOptions' resources grow (see `i18n/index.ts`'s
  // comment on the `recovery`/`remoteAccess` namespaces) — resolving the
  // full overload set per call is far more work than this helper needs.
  const t = tFull as (key: string) => string;
  switch (flag.type) {
    case "outsideWorkspace":
      return t("chatView.toolConsent.outsideWorkspace");
    case "forkBombShape":
      return t("chatView.toolConsent.dangerFlags.forkBombShape");
    case "rawDeviceIo":
      return t("chatView.toolConsent.dangerFlags.rawDeviceIo");
    case "networkFetchPipeShell":
      return t("chatView.toolConsent.dangerFlags.networkFetchPipeShell");
    case "sudoOrPrivEsc":
      return t("chatView.toolConsent.dangerFlags.sudoOrPrivEsc");
    case "derivedFromRecentOutput":
      return t("chatView.toolConsent.dangerFlags.derivedFromRecentOutput");
    case "promptInjectionSuspected":
      return t("chatView.toolConsent.dangerFlags.promptInjectionSuspected");
  }
}
