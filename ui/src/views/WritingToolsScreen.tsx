import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../lib/api";
import { copyText } from "../lib/clipboard";
import type { ChatEvent } from "../lib/types";
import QueuedNotice from "../components/QueuedNotice";
import { useStore } from "../state/store";
import { WRITING_TOOL_LANGUAGES } from "../lib/writingToolsLanguages";
import { chunkText } from "../lib/writingToolsChunking";
import {
  computeWordDiff,
  defaultAcceptedMap,
  reconcileDiffText,
  type DiffPart,
} from "../lib/writingToolsDiff";

export type WritingToolsMode = "translate" | "correct";

/**
 * The shared Translate/Correct screen, gated behind the `writing-tools`
 * plugin capability. `TranslateView`/`CorrectView` are thin wrappers that
 * fix `mode` for their own route — see those files for why they're separate
 * pages rather than one screen with an in-page toggle. Single-turn,
 * non-conversational: no Library entry, no `app_db` persistence, so this
 * owns its own local state and calls `api.translateStream`/`api.correctStream`
 * directly rather than going through a store action the way ordinary chat's
 * `send` does.
 *
 * Runs as one request in the store's `modelRequests` queue (every chunk of
 * a long document included), so started while a chat reply is running it
 * shows as waiting — cancellable — and then runs once the model is free,
 * and a chat sent meanwhile waits for it in turn. `translate_stream`/
 * `correct_stream` claim the same Rust-side queue every chat/agent/
 * web-search turn claims (`crates/app/src/generation.rs`), so Stop reaches
 * an in-flight translation exactly like a chat reply. Leaving the screen
 * cancels its request: the result would have nowhere to show.
 *
 * Two later additions on top of the original paste-box:
 * - **Document translation/correction**: "Upload a document" reuses the
 *   existing `pick_attachments` command (already extracts text from PDFs/
 *   text files for chat attachments) to fill the source box, and — only for
 *   an uploaded document, not pasted text — a "Download" button saves the
 *   result via the native save dialog. Long documents are split into
 *   model-sized chunks (`chunkText`) and translated/corrected one chunk at a
 *   time, streaming each chunk's tokens into the same growing result.
 * - **Tracked changes for Correct mode**: once a correction finishes, a
 *   word-level diff (`computeWordDiff`) against the original replaces the
 *   plain result textarea with an inline strikethrough/underline view where
 *   every individual change can be accepted or reverted — Translate mode
 *   never diffs (there's no meaningful word-level diff across languages), so
 *   it keeps the plain textarea unconditionally.
 */
export default function WritingToolsScreen({ mode }: { mode: WritingToolsMode }) {
  const { t } = useTranslation();
  const runModelRequest = useStore((s) => s.runModelRequest);
  const cancelModelRequest = useStore((s) => s.cancelModelRequest);

  const [sourceText, setSourceText] = useState("");
  const [sourceLang, setSourceLang] = useState<string | null>(null); // null = Detect
  const [targetLang, setTargetLang] = useState<string | null>(null);
  const [resultText, setResultText] = useState("");
  const [busy, setBusy] = useState(false);
  // This screen's request in `modelRequests` while `busy`, to show it
  // waiting and to stop or cancel it.
  const [requestId, setRequestId] = useState<string | null>(null);
  const waiting = useStore(
    (s) =>
      requestId !== null && s.modelRequests.some((r) => r.id === requestId && r.status === "waiting"),
  );
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  // Document translation/correction: set only when the source box was filled
  // by "Upload a document" (never by pasting), since it also gates whether
  // "Download" makes sense — for pasted text, Copy is the natural output.
  const [uploadedFileName, setUploadedFileName] = useState<string | null>(null);
  // Set only while running more than one chunk, so the UI can say which
  // section is in flight instead of a plain "translating…" with no signal
  // of how long is left.
  const [chunkProgress, setChunkProgress] = useState<{ current: number; total: number } | null>(
    null,
  );

  // Correct-mode tracked changes: computed once (on the finished result, not
  // incrementally per token — diffing a partial stream would visibly thrash)
  // and only for Correct mode. `null` means "show the plain result textarea
  // instead" (Translate mode, still streaming, or no run yet).
  const [diffParts, setDiffParts] = useState<DiffPart[] | null>(null);
  const [accepted, setAccepted] = useState<Record<string, boolean>>({});

  // StrictMode-safe request guard (same shape as Discover.tsx): a stale
  // response from an abandoned request, or one that lands after unmount,
  // never touches state.
  const requestSeq = useRef(0);
  const alive = useRef(true);
  const requestIdRef = useRef<string | null>(null);
  // Set by Stop, so a long document doesn't go on to its next chunk.
  const stopRequested = useRef(false);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      const id = requestIdRef.current;
      if (id) void useStore.getState().cancelModelRequest(id).catch(() => {});
    };
  }, []);

  const trimmed = sourceText.trim();
  const canRun = trimmed.length > 0 && !busy && (mode === "correct" || !!targetLang);

  // The text Copy/Download actually act on: the reconciled diff (respecting
  // each change's current accept/reject state) once one exists, otherwise
  // the plain streamed result.
  const finalText = diffParts ? reconcileDiffText(diffParts, accepted) : resultText;

  const resetOutputs = () => {
    setResultText("");
    setError(null);
    setDiffParts(null);
    setAccepted({});
  };

  const uploadDocument = async () => {
    if (busy) return;
    let picked;
    try {
      // No prompt-injection pre-check: Writing Tools give the model no tools.
      picked = await api.pickAttachments(false, false);
    } catch (err: unknown) {
      setError(String(err));
      return;
    }
    if (picked.length === 0) return; // user closed the picker
    const doc = picked.find((a) => a.kind === "document" && a.text);
    if (!doc?.text) {
      setError(t("writingTools.unsupportedFileError"));
      return;
    }
    setSourceText(doc.text);
    setUploadedFileName(doc.name);
    resetOutputs();
  };

  const run = async () => {
    if (!canRun) return;
    const seq = ++requestSeq.current;
    const isStale = () => !alive.current || requestSeq.current !== seq;

    setBusy(true);
    resetOutputs();
    const id = crypto.randomUUID();
    requestIdRef.current = id;
    setRequestId(id);
    stopRequested.current = false;

    const chunks = chunkText(trimmed);
    let accumulated = "";

    // The whole document is one request: its chunks run back to back, with
    // nothing else using the model in between.
    const runChunks = async (): Promise<true> => {
      for (let i = 0; i < chunks.length; i++) {
        if (isStale() || stopRequested.current) break;
        if (chunks.length > 1) setChunkProgress({ current: i + 1, total: chunks.length });

        const onEvent = (event: ChatEvent) => {
          if (isStale()) return;
          if (event.type === "token") {
            accumulated += event.data.text;
            setResultText(accumulated);
          } else if (event.type === "error") {
            setError(event.data.message);
          }
        };

        if (mode === "translate") {
          await api.translateStream(chunks[i], sourceLang, targetLang as string, onEvent);
        } else {
          await api.correctStream(chunks[i], onEvent);
        }

        if (isStale() || stopRequested.current) break;
        if (i < chunks.length - 1) {
          accumulated += "\n\n";
          setResultText(accumulated);
        }
      }
      return true;
    };

    try {
      const ran = await runModelRequest({ id, kind: "writingTool" }, runChunks);
      if (ran === undefined) return; // cancelled before it started
      if (!isStale() && mode === "correct") {
        const parts = computeWordDiff(trimmed, accumulated);
        setDiffParts(parts);
        setAccepted(defaultAcceptedMap(parts));
      }
    } catch (err: unknown) {
      if (!isStale()) setError(String(err));
    } finally {
      if (requestIdRef.current === id) requestIdRef.current = null;
      if (!isStale()) {
        setBusy(false);
        setRequestId(null);
        setChunkProgress(null);
      }
    }
  };

  const stop = () => {
    if (!requestId) return;
    stopRequested.current = true;
    void cancelModelRequest(requestId);
  };

  const copy = async () => {
    if (await copyText(finalText)) {
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
    }
  };

  const download = async () => {
    if (!uploadedFileName || !finalText) return;
    const base = uploadedFileName.replace(/\.[^./]+$/, "");
    const suffix =
      mode === "translate"
        ? ` (${WRITING_TOOL_LANGUAGES.find((l) => l.id === targetLang)?.name ?? targetLang})`
        : ` (${t("writingTools.correctedSuffix")})`;
    try {
      await api.saveWritingToolResult(finalText, `${base}${suffix}.txt`);
    } catch (err: unknown) {
      setError(String(err));
    }
  };

  const toggleChange = (id: string) => {
    setAccepted((prev) => ({ ...prev, [id]: !(prev[id] ?? true) }));
  };

  const acceptAllChanges = () => diffParts && setAccepted(defaultAcceptedMap(diffParts));
  const rejectAllChanges = () => {
    if (!diffParts) return;
    const all: Record<string, boolean> = {};
    for (const part of diffParts) if (part.type === "change") all[part.change.id] = false;
    setAccepted(all);
  };

  return (
    <div className="writing-tools-view">
      <div className="writing-tools-scroll">
        <div className="writing-tools-col">
          <div className="writing-tools-head">
            <h1>{mode === "translate" ? t("writingTools.modeTranslate") : t("writingTools.modeCorrect")}</h1>
            <p>
              {mode === "translate" ? t("writingTools.translateSubtitle") : t("writingTools.correctSubtitle")}
            </p>
          </div>

          {mode === "translate" && (
            <div className="writing-tools-lang-row">
              <select
                value={sourceLang ?? ""}
                disabled={busy}
                aria-label={t("writingTools.sourceLangAriaLabel")}
                onChange={(e) => setSourceLang(e.target.value || null)}
              >
                <option value="">{t("writingTools.detectLanguage")}</option>
                {WRITING_TOOL_LANGUAGES.map((lang) => (
                  <option key={lang.id} value={lang.id}>
                    {lang.name}
                  </option>
                ))}
              </select>
              <span className="writing-tools-lang-arrow" aria-hidden="true">
                →
              </span>
              <select
                value={targetLang ?? ""}
                disabled={busy}
                aria-label={t("writingTools.targetLangAriaLabel")}
                onChange={(e) => setTargetLang(e.target.value || null)}
              >
                <option value="" disabled>
                  {t("writingTools.chooseLanguage")}
                </option>
                {WRITING_TOOL_LANGUAGES.map((lang) => (
                  <option key={lang.id} value={lang.id}>
                    {lang.name}
                  </option>
                ))}
              </select>
            </div>
          )}

          <div className="writing-tools-panes">
            <div className="writing-tools-pane">
              <div className="writing-tools-source-head">
                <label htmlFor="writing-tools-source">{t("writingTools.sourceLabel")}</label>
                <button
                  type="button"
                  className="btn-quiet"
                  disabled={busy}
                  onClick={() => void uploadDocument()}
                >
                  {t("writingTools.uploadDocument")}
                </button>
              </div>
              {uploadedFileName && (
                <p className="writing-tools-uploaded-file">
                  {t("writingTools.uploadedFile", { name: uploadedFileName })}
                </p>
              )}
              <textarea
                id="writing-tools-source"
                value={sourceText}
                disabled={busy}
                placeholder={t("writingTools.sourcePlaceholder")}
                onChange={(e) => {
                  setSourceText(e.target.value);
                  if (e.target.value.trim().length === 0) setUploadedFileName(null);
                }}
              />
            </div>
            <div className="writing-tools-pane">
              <div className="writing-tools-result-head">
                <label
                  id="writing-tools-result-label"
                  htmlFor={diffParts ? undefined : "writing-tools-result"}
                >
                  {t("writingTools.resultLabel")}
                </label>
                <div className="writing-tools-result-actions">
                  {diffParts && (
                    <>
                      <button type="button" className="btn-quiet" onClick={acceptAllChanges}>
                        {t("writingTools.acceptAllChanges")}
                      </button>
                      <button type="button" className="btn-quiet" onClick={rejectAllChanges}>
                        {t("writingTools.rejectAllChanges")}
                      </button>
                    </>
                  )}
                  {finalText && (
                    <button type="button" className="btn-quiet" onClick={() => void copy()}>
                      {copied ? t("writingTools.copied") : t("writingTools.copy")}
                    </button>
                  )}
                  {uploadedFileName && finalText && !busy && (
                    <button type="button" className="btn-quiet" onClick={() => void download()}>
                      {t("writingTools.download")}
                    </button>
                  )}
                </div>
              </div>
              {diffParts ? (
                <div
                  className="writing-tools-diff"
                  role="region"
                  aria-labelledby="writing-tools-result-label"
                >
                  {diffParts.map((part, i) => {
                    if (part.type === "equal") {
                      return <span key={i}>{part.text}</span>;
                    }
                    const isAccepted = accepted[part.change.id] ?? true;
                    return (
                      <span key={part.change.id} className="writing-tools-diff-change">
                        {part.change.removed && (
                          <del
                            className={
                              isAccepted
                                ? "writing-tools-diff-removed"
                                : "writing-tools-diff-removed-active"
                            }
                          >
                            {part.change.removed}
                          </del>
                        )}
                        {part.change.added && (
                          <ins
                            className={
                              isAccepted
                                ? "writing-tools-diff-added-active"
                                : "writing-tools-diff-added"
                            }
                          >
                            {part.change.added}
                          </ins>
                        )}
                        <button
                          type="button"
                          className="writing-tools-diff-toggle"
                          onClick={() => toggleChange(part.change.id)}
                          aria-label={t(
                            isAccepted ? "writingTools.rejectChange" : "writingTools.acceptChange",
                          )}
                          title={t(
                            isAccepted ? "writingTools.rejectChange" : "writingTools.acceptChange",
                          )}
                        >
                          {isAccepted ? "↺" : "✓"}
                        </button>
                      </span>
                    );
                  })}
                </div>
              ) : (
                <textarea
                  id="writing-tools-result"
                  aria-labelledby="writing-tools-result-label"
                  value={resultText}
                  readOnly
                  placeholder={t("writingTools.resultPlaceholder")}
                />
              )}
            </div>
          </div>

          {chunkProgress && (
            <p className="writing-tools-notice" role="status">
              {t("writingTools.chunkProgress", chunkProgress)}
            </p>
          )}
          {error && (
            <p className="writing-tools-error" role="alert">
              {error}
            </p>
          )}

          <div className="writing-tools-actions">
            {busy && waiting ? (
              <QueuedNotice onCancel={stop} />
            ) : busy ? (
              <button type="button" className="btn-ghost" onClick={stop}>
                {t("writingTools.stop")}
              </button>
            ) : (
              <button type="button" className="btn" disabled={!canRun} onClick={() => void run()}>
                {mode === "translate" ? t("writingTools.runTranslate") : t("writingTools.runCorrect")}
              </button>
            )}
          </div>

          <p className="writing-tools-local-note">{t("writingTools.localFirstNote")}</p>
        </div>
      </div>
    </div>
  );
}
