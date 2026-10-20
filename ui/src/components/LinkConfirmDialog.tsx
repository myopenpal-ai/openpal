import { useId, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { copyText } from "../lib/clipboard";
import type { LinkCheck } from "../lib/linkSafety";
import { useDialogFocus } from "./useDialogFocus";

/**
 * Asks before a link in rendered Markdown opens in the system browser, and
 * shows where it really goes. One decision on the default path, so focus
 * starts on the dialog itself rather than on Open, and Escape or a click on
 * the scrim cancels. The description the screen reader reads with the title
 * carries the destination host and any warning. Portaled to `document.body`
 * for the same reason as ConfirmDialog.
 */
export default function LinkConfirmDialog({
  link,
  onOpen,
  onCancel,
}: {
  link: LinkCheck;
  onOpen: () => void;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  const titleId = useId();
  const destinationId = useId();
  const mismatchId = useId();
  const localId = useId();
  const dialogRef = useDialogFocus<HTMLDivElement>(onCancel);
  const [copied, setCopied] = useState(false);
  const describedBy = [
    destinationId,
    link.shownHost !== null && mismatchId,
    link.local && localId,
  ]
    .filter(Boolean)
    .join(" ");

  return createPortal(
    <div className="modal-backdrop" onClick={onCancel}>
      <div
        ref={dialogRef}
        tabIndex={-1}
        className="modal link-confirm"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={describedBy}
        onClick={(e) => e.stopPropagation()}
      >
        <h2 id={titleId}>{t("markdown.link.title")}</h2>
        <div id={destinationId}>
          <p className="link-confirm-text">{t("markdown.link.intro")}</p>
          <p className="link-confirm-host">{link.host}</p>
        </div>
        {link.shownHost !== null && (
          <p id={mismatchId} className="link-confirm-warning">
            {t("markdown.link.mismatch", { shown: link.shownHost, host: link.host })}
          </p>
        )}
        {link.local && (
          <p id={localId} className="link-confirm-warning">
            {t("markdown.link.local")}
          </p>
        )}
        <p className="link-confirm-text">{t("markdown.link.fullAddress")}</p>
        <p className="link-confirm-address">{link.href}</p>
        <p className="link-confirm-text">{t("markdown.link.advice")}</p>
        <div className="model-confirm-actions link-confirm-actions">
          <button className="btn-quiet" onClick={onCancel}>
            {t("common.cancel")}
          </button>
          <button
            className="btn-quiet"
            onClick={async () => {
              if (await copyText(link.href)) {
                setCopied(true);
                setTimeout(() => setCopied(false), 1400);
              }
            }}
          >
            {copied ? t("markdown.copied") : t("markdown.link.copyAddress")}
          </button>
          <button className="btn" onClick={onOpen}>
            {t("markdown.link.open")}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
