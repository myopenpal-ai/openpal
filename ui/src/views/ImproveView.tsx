import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../lib/api";
import { copyText } from "../lib/clipboard";
import { keptStatus, MAX_DRAFT_CHARS } from "../lib/draftImprover";
import { useStore } from "../state/store";

/**
 * The standalone "Improve a message" screen — reachable from the sidebar
 * quick-action row below "+ New chat", gated by the `writing-tools` plugin
 * (`writingToolsAvailable`), same as Translate/Correct — see `loadPlugins`/
 * `setPluginEnabled`'s matching view redirect. Lets the user improve a
 * message before there's a conversation to put it in — the composer's own
 * inline "Improve my message" button (`ChatView.tsx`) covers the
 * in-conversation case; this covers "I have a draft but haven't started
 * chatting yet."
 *
 * Same shape as `WritingToolsScreen` (single-turn, non-conversational, no
 * Library/`app_db` persistence, sets the store's top-level `streaming` flag
 * around its own request so chat-side guards transparently cover this too,
 * and vice versa) but simpler: `enhance_prompt` is a single, non-streaming
 * `invoke` (the result is cleaned once in Rust, never partial), not a
 * `Channel`-streamed one, and there's no document upload, chunking, or
 * tracked-changes diff — just a draft in, a result out.
 */
export default function ImproveView() {
  const { t } = useTranslation();
  const engineStreaming = useStore((s) => s.streaming);
  const startChatWithDraft = useStore((s) => s.startChatWithDraft);

  const [draft, setDraft] = useState("");
  const [resultText, setResultText] = useState("");
  const [status, setStatus] = useState<{ text: string; alert: boolean } | null>(null);
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);

  // StrictMode-safe request guard, same shape as WritingToolsScreen.
  const requestSeq = useRef(0);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const trimmed = draft.trim();
  const blockedByChat = engineStreaming && !busy;
  const tooLong = draft.length > MAX_DRAFT_CHARS;
  const canRun = trimmed.length > 0 && !tooLong && !busy && !blockedByChat;

  const run = async () => {
    if (!canRun) return;
    const seq = ++requestSeq.current;
    const isStale = () => !alive.current || requestSeq.current !== seq;

    setBusy(true);
    setResultText("");
    setStatus(null);
    useStore.setState({ streaming: true });

    try {
      const outcome = await api.enhancePrompt(trimmed);
      if (isStale()) return;
      if (outcome.type === "enhanced") {
        setResultText(outcome.data.text);
      } else {
        const { key, alert } = keptStatus(outcome.data.reason);
        setStatus({ text: t(key), alert });
      }
    } catch (err: unknown) {
      if (!isStale()) setStatus({ text: String(err), alert: true });
    } finally {
      if (!isStale()) setBusy(false);
      useStore.setState({ streaming: false });
    }
  };

  const stop = () => void api.stopGeneration();

  const copy = async () => {
    if (await copyText(resultText)) {
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
    }
  };

  return (
    <div className="writing-tools-view">
      <div className="writing-tools-scroll">
        <div className="writing-tools-col">
          <div className="writing-tools-head">
            <h1>{t("improveView.title")}</h1>
            <p>{t("improveView.subtitle")}</p>
          </div>

          <div className="writing-tools-panes">
            <div className="writing-tools-pane">
              <div className="writing-tools-source-head">
                <label htmlFor="improve-view-source">{t("improveView.sourceLabel")}</label>
              </div>
              <textarea
                id="improve-view-source"
                value={draft}
                disabled={busy}
                placeholder={t("improveView.sourcePlaceholder")}
                onChange={(e) => {
                  setDraft(e.target.value);
                  setResultText("");
                  setStatus(null);
                }}
              />
            </div>
            <div className="writing-tools-pane">
              <div className="writing-tools-result-head">
                <label id="improve-view-result-label" htmlFor="improve-view-result">
                  {t("improveView.resultLabel")}
                </label>
                <div className="writing-tools-result-actions">
                  {resultText && (
                    <>
                      <button type="button" className="btn-quiet" onClick={() => void copy()}>
                        {copied ? t("improveView.copied") : t("improveView.copy")}
                      </button>
                      <button
                        type="button"
                        className="btn-quiet"
                        onClick={() => startChatWithDraft(resultText)}
                      >
                        {t("improveView.startChat")}
                      </button>
                    </>
                  )}
                </div>
              </div>
              <textarea
                id="improve-view-result"
                aria-labelledby="improve-view-result-label"
                value={resultText}
                readOnly
                placeholder={t("improveView.resultPlaceholder")}
              />
            </div>
          </div>

          {tooLong && (
            <p className="writing-tools-notice" role="status">
              {t("improveView.tooLong")}
            </p>
          )}
          {blockedByChat && (
            <p className="writing-tools-notice" role="status">
              {t("improveView.engineBusy")}
            </p>
          )}
          {status && (
            <p className="writing-tools-error" role={status.alert ? "alert" : "status"}>
              {status.text}
            </p>
          )}

          <div className="writing-tools-actions">
            {busy ? (
              <button type="button" className="btn-ghost" onClick={stop}>
                {t("improveView.stop")}
              </button>
            ) : (
              <button type="button" className="btn" disabled={!canRun} onClick={() => void run()}>
                {t("improveView.run")}
              </button>
            )}
          </div>

          <p className="writing-tools-local-note">{t("improveView.localNote")}</p>
        </div>
      </div>
    </div>
  );
}
