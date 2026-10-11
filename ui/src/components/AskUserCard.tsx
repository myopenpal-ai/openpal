import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useDialogFocus } from "./useDialogFocus";
import { useStore, type PendingClarifyingQuestion } from "../state/store";

/**
 * The `ask_user` prompt: the model's question, any suggested quick-reply
 * chips, and a free-text fallback. Unlike `ToolConsentCard`, this never gates
 * a risky action — it's a normal conversational turn, so Escape dismisses it
 * (as a skip) instead of being disabled. `question.question`/`options` are
 * rendered as plain text children only, same rule as `ToolConsentCard`: this
 * content originates from the model and could be influenced by something it
 * read, so it must never go through the Markdown renderer.
 */
export function AskUserCard({ question }: { question: PendingClarifyingQuestion }) {
  const { t } = useTranslation();
  const respondClarifyingQuestion = useStore((s) => s.respondClarifyingQuestion);
  const [answer, setAnswer] = useState("");
  const [submitting, setSubmitting] = useState(false);

  const respond = async (text: string) => {
    if (submitting) return;
    setSubmitting(true);
    await respondClarifyingQuestion(text || t("chatView.askUser.skippedAnswer"));
  };

  const dialogRef = useDialogFocus<HTMLDivElement>(() => void respond(""));

  return (
    <div
      ref={dialogRef}
      tabIndex={-1}
      className="ask-user-card"
      role="group"
      aria-label={t("chatView.askUser.title")}
    >
      <div className="ask-user-question">{question.question}</div>
      {question.options.length > 0 && (
        <div className="ask-user-options">
          {question.options.map((option, i) => (
            <button
              key={i}
              type="button"
              className="ask-user-option"
              disabled={submitting}
              onClick={() => void respond(option)}
            >
              {option}
            </button>
          ))}
        </div>
      )}
      <form
        className="ask-user-freeform"
        onSubmit={(e) => {
          e.preventDefault();
          if (answer.trim()) void respond(answer.trim());
        }}
      >
        <input
          type="text"
          autoFocus
          value={answer}
          placeholder={t("chatView.askUser.answerPlaceholder")}
          disabled={submitting}
          onChange={(e) => setAnswer(e.target.value)}
        />
        <button
          type="submit"
          className="btn-ghost"
          disabled={submitting || !answer.trim()}
        >
          {t("chatView.askUser.send")}
        </button>
      </form>
      <div className="ask-user-actions">
        <button
          type="button"
          className="btn-quiet"
          disabled={submitting}
          onClick={() => void respond("")}
        >
          {t("chatView.askUser.skip")}
        </button>
      </div>
    </div>
  );
}
