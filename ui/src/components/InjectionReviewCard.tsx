import { useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { useDialogFocus } from "./useDialogFocus";
import { injectionWhere } from "./InjectionNotices";
import type { FlaggedItem, InjectionReviewDecision } from "../lib/types";
import { useStore, type PendingInjectionReview } from "../state/store";

/** Sources whose own phrase already names the text ("the file “x”", "the
 * skill /x") — every other one gets its full label as a code line. */
const NAMED_IN_PHRASE = new Set(["attachment", "skill", "composer"]);

/**
 * The prompt-injection review: text the detector flagged is held back until
 * the user picks Continue anyway or Stop here. Every value from the review
 * (excerpts, names, the helper kind) is rendered as plain text ONLY —
 * never through the Markdown renderer used for ordinary chat messages. This
 * is a hard rule, not a style choice: the excerpt is, by definition, the
 * part of an attacker-influenceable page, file or command output that looked
 * most like instructions aimed at the AI, and every value here reaches this
 * component as a plain string prop rendered as a React text child, which
 * React always escapes — it must never be passed through `Markdown` or
 * `dangerouslySetInnerHTML`.
 *
 * Escape means Stop, the safe answer: dismissing the card must never let the
 * text through. So the card can't be answered by a key meant for something
 * else, it focuses itself, never a button, and only when the user isn't busy
 * elsewhere: it appears on its own mid-reply, maybe while they're typing
 * their next message (a Space or Enter would press a focused button) or
 * looking at a preview or image (whose Escape would answer this instead).
 * Stop stays the last, primary button.
 *
 * An item from an earlier message than the turn's own was already read by
 * the AI in a chat that isn't checked (plain chat, before agent tools or web
 * search were on): the card says so rather than promising the AI won't read
 * it, and Stop keeps it out of everything from then on.
 */
export function InjectionReviewCard({ review }: { review: PendingInjectionReview }) {
  const { t } = useTranslation("injection");
  const { t: tApp } = useTranslation();
  const respondInjectionReview = useStore((s) => s.respondInjectionReview);
  const [submitting, setSubmitting] = useState(false);
  const titleId = useId();
  const whyId = useId();

  const respond = async (decision: InjectionReviewDecision) => {
    if (submitting) return;
    setSubmitting(true);
    await respondInjectionReview(decision);
  };

  const dialogRef = useDialogFocus<HTMLDivElement>(() => void respond("stop"), {
    unprompted: true,
  });

  const readEarlier = (item: FlaggedItem) =>
    review.newestMessageId !== null &&
    item.messageId !== null &&
    item.messageId !== review.newestMessageId;
  const allEarlier = review.items.length > 0 && review.items.every(readEarlier);
  const anyEarlier = review.items.some(readEarlier);
  const many = review.items.length > 1;
  const title = allEarlier
    ? many
      ? t("review.titleEarlierMany")
      : t("review.titleEarlier")
    : many
      ? t("review.titleMany")
      : t("review.title");

  return (
    <div
      ref={dialogRef}
      tabIndex={-1}
      className="injection-review-card"
      role="alertdialog"
      aria-modal="true"
      aria-labelledby={titleId}
      aria-describedby={whyId}
    >
      <div id={titleId} className="injection-review-title">
        {title}
      </div>
      {review.items.map((item, i) => (
        // Only a mixed card marks the earlier items: otherwise the title
        // already says it.
        <ReviewItem key={i} item={item} earlier={!allEarlier && readEarlier(item)} />
      ))}
      {review.via && (
        <div className="injection-review-via">
          {t("review.foundBy", {
            helper: tApp("chatView.agentActivity.helperLabel", { kind: review.via }),
          })}
        </div>
      )}
      <p id={whyId} className="injection-review-why">
        {t("review.why")}
      </p>
      <p className="injection-review-advice">
        {anyEarlier ? t("review.adviceEarlier") : t("review.advice")}
      </p>
      <div className="injection-review-actions">
        <button
          type="button"
          className="btn-ghost"
          disabled={submitting}
          onClick={() => void respond("continue")}
        >
          {t("review.continue")}
        </button>
        <button
          type="button"
          className="btn-big injection-review-primary"
          disabled={submitting}
          onClick={() => void respond("stop")}
        >
          {t("review.stop")}
        </button>
      </div>
    </div>
  );
}

function ReviewItem({ item, earlier }: { item: FlaggedItem; earlier: boolean }) {
  const { t } = useTranslation("injection");
  const where = injectionWhere(t, item.source, item.name, { toolName: false });
  return (
    <div className="injection-review-item">
      <div className="injection-review-found">
        {item.unchecked ? t("unchecked", { where }) : t("found", { where })}
      </div>
      {earlier && <div className="injection-review-note">{t("review.readEarlier")}</div>}
      {item.name && !NAMED_IN_PHRASE.has(item.source) && (
        <code className="injection-review-name">{item.name}</code>
      )}
      {item.excerpt !== "" && (
        <>
          <div className="injection-review-excerpt-label">{t("review.excerptLabel")}</div>
          {/* Focusable so a long excerpt can be scrolled from the keyboard. */}
          <pre className="injection-review-excerpt" tabIndex={0}>
            {item.excerpt}
          </pre>
          {item.excerptTruncated && (
            <div className="injection-review-note">{t("review.excerptTruncated")}</div>
          )}
        </>
      )}
      {/* Outside the excerpt block: the backend sets this for the name too,
          which an unchecked item still shows. */}
      {item.hiddenChars && <div className="injection-review-note">{t("review.hiddenChars")}</div>}
    </div>
  );
}
