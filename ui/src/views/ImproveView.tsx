import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../lib/api";
import { copyText } from "../lib/clipboard";
import { keptStatus, MAX_DRAFT_CHARS } from "../lib/draftImprover";
import type { ChatEvent, ImproveTone } from "../lib/types";
import QueuedNotice from "../components/QueuedNotice";
import IconActionButton from "../components/IconActionButton";
import {
  CheckIcon,
  ClearIcon,
  CopyIcon,
  SparkleIcon,
  StartChatIcon,
} from "../components/WritingToolsIcons";
import { useStore } from "../state/store";

/** The tone picker's choices, in display order. `natural` is the default and
 * sends the same request the composer's inline improver sends — see
 * `prompt_enhancer::build_request` in `crates/core`. */
const TONES = [
  { id: "natural", labelKey: "improveView.toneNatural" },
  { id: "professional", labelKey: "improveView.toneProfessional" },
  { id: "direct", labelKey: "improveView.toneDirect" },
  { id: "custom", labelKey: "improveView.toneCustom" },
] as const;

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
 * Library/`app_db` persistence, one request in the store's `modelRequests`
 * queue — waiting, cancellable, behind a reply already running, and
 * cancelled if the user leaves the screen). Unlike the composer's inline
 * action, which stays a non-streaming, atomic replace of the draft
 * (`docs/design/PROMPT_ENHANCER.md` decision 4 — a half-written rewrite must
 * never land in the draft), this screen has its own read-only result box, so
 * it streams: tokens land there as they arrive (`api.enhancePromptStream`),
 * and the command's own promise still resolves to the cleaned
 * `EnhanceOutcome`, which replaces the preview with the final text — or
 * leaves the partial rewrite on screen with a notice when the user stops or
 * the guards reject it.
 *
 * The tone picker is another thing the composer deliberately doesn't have:
 * one of Natural (default, today's rewrite verbatim), Professional, Direct or
 * Custom. Choosing Custom reveals a single free-text instructions field,
 * persisted in `Settings.improveToneInstructions` on blur — one value,
 * replaced on every edit, never a growing list.
 */
export default function ImproveView() {
  const { t } = useTranslation();
  const runModelRequest = useStore((s) => s.runModelRequest);
  const cancelModelRequest = useStore((s) => s.cancelModelRequest);
  const startChatWithDraft = useStore((s) => s.startChatWithDraft);
  const saveSettings = useStore((s) => s.saveSettings);

  const [draft, setDraft] = useState("");
  const [resultText, setResultText] = useState("");
  const [status, setStatus] = useState<{ text: string; alert: boolean } | null>(null);
  const [busy, setBusy] = useState(false);
  // This screen's request in `modelRequests` while `busy`.
  const [requestId, setRequestId] = useState<string | null>(null);
  const waiting = useStore(
    (s) =>
      requestId !== null && s.modelRequests.some((r) => r.id === requestId && r.status === "waiting"),
  );
  const [copied, setCopied] = useState(false);
  const [tone, setTone] = useState<ImproveTone>("natural");
  // Seeded once, from whatever the last persisted value was; edited locally
  // and written back on blur/unmount so typing never fights `saveSettings`.
  const [customInstructions, setCustomInstructions] = useState(
    () => useStore.getState().settings?.improveToneInstructions ?? "",
  );

  // StrictMode-safe request guard, same shape as WritingToolsScreen.
  const requestSeq = useRef(0);
  const alive = useRef(true);
  const requestIdRef = useRef<string | null>(null);
  // The latest typed instruction, for the unmount save (whose closure would
  // otherwise see the first render's value).
  const instructionsRef = useRef(customInstructions);
  instructionsRef.current = customInstructions;

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      const id = requestIdRef.current;
      if (id) void useStore.getState().cancelModelRequest(id).catch(() => {});
      // Leaving the screen is a blur as far as persistence goes: whatever was
      // typed must survive the restart even if the field itself never blurred.
      const typed = instructionsRef.current.trim();
      const saved = useStore.getState().settings?.improveToneInstructions ?? "";
      if (typed !== saved) {
        void useStore.getState().saveSettings({ improveToneInstructions: typed });
      }
    };
  }, []);

  const persistInstructions = () => {
    const typed = customInstructions.trim();
    const saved = useStore.getState().settings?.improveToneInstructions ?? "";
    if (typed !== saved) void saveSettings({ improveToneInstructions: typed });
  };

  const trimmed = draft.trim();
  const tooLong = draft.length > MAX_DRAFT_CHARS;
  const canRun = trimmed.length > 0 && !tooLong && !busy;
  const canClear = !busy && (draft.length > 0 || resultText.length > 0 || status !== null);

  const clear = () => {
    setDraft("");
    setResultText("");
    setStatus(null);
    setCopied(false);
  };

  const run = async () => {
    if (!canRun) return;
    const seq = ++requestSeq.current;
    const isStale = () => !alive.current || requestSeq.current !== seq;

    // The field may not have blurred (Run was clicked straight from it in
    // some webviews), so persist before sending rather than relying on blur.
    persistInstructions();

    setBusy(true);
    setResultText("");
    setStatus(null);
    const id = crypto.randomUUID();
    requestIdRef.current = id;
    setRequestId(id);

    const onEvent = (event: ChatEvent) => {
      if (isStale()) return;
      if (event.type === "token") {
        // Progressive: the answer shows up as the model writes it. The final
        // cleaned text replaces this preview when the invoke resolves.
        setResultText((prev) => prev + event.data.text);
      }
    };

    try {
      const outcome = await runModelRequest({ id, kind: "improve" }, () =>
        api.enhancePromptStream(
          trimmed,
          tone,
          tone === "custom" ? customInstructions.trim() : "",
          onEvent,
        ),
      );
      if (isStale()) return;
      if (outcome === undefined) {
        // Cancelled before it started.
        const { key, alert } = keptStatus("cancelled");
        setStatus({ text: t(key), alert });
      } else if (outcome.type === "enhanced") {
        setResultText(outcome.data.text);
      } else {
        // `cancelled` mid-stream: the partial rewrite stays on screen and
        // the stopped notice explains why it stopped there. Every other
        // reason means the guards rejected the output, so the raw preview
        // (which may carry scaffolding) is cleared.
        if (outcome.data.reason !== "cancelled") setResultText("");
        const { key, alert } = keptStatus(outcome.data.reason);
        setStatus({ text: t(key), alert });
      }
    } catch (err: unknown) {
      if (!isStale()) setStatus({ text: String(err), alert: true });
    } finally {
      if (requestIdRef.current === id) requestIdRef.current = null;
      if (!isStale()) {
        setBusy(false);
        setRequestId(null);
      }
    }
  };

  const stop = () => {
    if (requestId) void cancelModelRequest(requestId);
  };

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
            <h1 className="writing-tools-title">
              <SparkleIcon className="writing-tools-title-icon" />
              {t("improveView.title")}
            </h1>
            <p>{t("improveView.subtitle")}</p>
          </div>

          <div className="writing-tools-tone" role="group" aria-labelledby="improve-tone-label">
            <span className="writing-tools-tone-label" id="improve-tone-label">
              {t("improveView.tonesLabel")}
            </span>
            <div className="writing-tools-tone-options">
              {TONES.map((option) => (
                <label
                  key={option.id}
                  htmlFor={`improve-tone-${option.id}`}
                  className={`writing-tools-tone-option${tone === option.id ? " is-on" : ""}`}
                >
                  <input
                    id={`improve-tone-${option.id}`}
                    type="radio"
                    name="improve-tone"
                    value={option.id}
                    checked={tone === option.id}
                    disabled={busy}
                    onChange={() => setTone(option.id)}
                  />
                  <span>{t(option.labelKey)}</span>
                </label>
              ))}
            </div>
            {tone === "custom" && (
              <label className="writing-tools-tone-custom" htmlFor="improve-custom-tone">
                <span>{t("improveView.customToneLabel")}</span>
                <textarea
                  id="improve-custom-tone"
                  rows={2}
                  value={customInstructions}
                  disabled={busy}
                  placeholder={t("improveView.customTonePlaceholder")}
                  onChange={(e) => setCustomInstructions(e.target.value)}
                  onBlur={persistInstructions}
                />
              </label>
            )}
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
                  {resultText && !busy && (
                    <>
                      <IconActionButton
                        label={copied ? t("improveView.copied") : t("improveView.copy")}
                        onClick={() => void copy()}
                      >
                        {copied ? <CheckIcon /> : <CopyIcon />}
                      </IconActionButton>
                      <IconActionButton
                        label={t("improveView.startChat")}
                        onClick={() => startChatWithDraft(resultText)}
                      >
                        <StartChatIcon />
                      </IconActionButton>
                    </>
                  )}
                  <IconActionButton label={t("improveView.clear")} onClick={clear} disabled={!canClear}>
                    <ClearIcon />
                  </IconActionButton>
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

          {busy && !waiting && (
            <p className="writing-tools-working" role="status">
              <span className="searching-spinner" aria-hidden="true" />
              {t("improveView.generating")}
            </p>
          )}
          {tooLong && (
            <p className="writing-tools-notice" role="status">
              {t("improveView.tooLong")}
            </p>
          )}
          {status && (
            <p className="writing-tools-error" role={status.alert ? "alert" : "status"}>
              {status.text}
            </p>
          )}

          <div className="writing-tools-actions">
            {busy && waiting ? (
              <QueuedNotice onCancel={stop} />
            ) : busy ? (
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
