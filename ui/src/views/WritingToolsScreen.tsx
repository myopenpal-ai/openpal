import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../lib/api";
import { copyText } from "../lib/clipboard";
import type { ChatEvent } from "../lib/types";
import QueuedNotice from "../components/QueuedNotice";
import IconActionButton from "../components/IconActionButton";
import {
  CheckIcon,
  ClearIcon,
  CopyIcon,
  CorrectIcon,
  DiscardAllIcon,
  DownloadIcon,
  KeepAllIcon,
  LanguagesIcon,
} from "../components/WritingToolsIcons";
import { useStore } from "../state/store";
import { WRITING_TOOL_LANGUAGES } from "../lib/writingToolsLanguages";
import { chunkText } from "../lib/writingToolsChunking";
import {
  applyAllDiffMarks,
  applyDiffMark,
  computeWordDiff,
  diffMarks,
  type DiffMark,
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
 * One editing surface per mode (issue #125):
 * - **Translate** keeps two panes — the source and a separate, read-only
 *   result — because a translation is a different language and there is no
 *   meaningful word-level mark to put in place.
 * - **Correct** is a single editable surface: the user's own text, with each
 *   detected error marked *in place* as a focusable button. Hovering or
 *   focusing a mark reveals its suggestion; activating the mark (click, Enter
 *   or Space) writes it in and the old wording is gone — deliberately
 *   destructive, not a tracked-changes pane. There is no second "original"
 *   box and no strikethrough/underline view.
 *
 * The marks are painted by a mirror layer over the transparent-text textarea
 * so the surface stays a real, editable `<textarea>` (one accessible textbox,
 * native caret/selection/IME) rather than a hand-rolled contenteditable. The
 * mirror is typographically identical to the textarea and never affects its
 * layout: marks only change colour and underline. Typing into the textarea
 * drops the pending marks (their offsets no longer line up with the text),
 * which is why applying a suggestion and then continuing to type is safe.
 *
 * Document translation/correction is unchanged: "Upload a document" reuses
 * the existing `pick_attachments` command to fill the surface, and — only for
 * an uploaded document, not pasted text — Download saves the result via the
 * native save dialog. Long documents are split into model-sized chunks
 * (`chunkText`) and translated/corrected one chunk at a time, streaming each
 * chunk's tokens into the same growing result.
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
  // Stop was pressed while the model was writing: the partial answer stays
  // on screen, with the same stopped notice the composer's improver uses.
  const [stopped, setStopped] = useState(false);

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

  // Correct mode: the model's correction as it streams. It is shown in a
  // live region while it arrives (the answer appears progressively, like
  // Translate's result box), then turns into in-place marks once the run
  // finishes — diffing a partial stream against the whole original would
  // mark everything after the last received word as deleted.
  const [live, setLive] = useState("");
  // The marks currently offered on the surface. Applying one writes it into
  // `sourceText`; none survive a run, a Clear, or a manual edit.
  const [marks, setMarks] = useState<DiffMark[]>([]);
  // Whether Correct has produced a correction yet: gates Copy/Download, so
  // they act on a corrected text rather than on the user's own paste.
  const [hasResult, setHasResult] = useState(false);

  // StrictMode-safe request guard (same shape as Discover.tsx): a stale
  // response from an abandoned request, or one that lands after unmount,
  // never touches state.
  const requestSeq = useRef(0);
  const alive = useRef(true);
  const requestIdRef = useRef<string | null>(null);
  // Set by Stop, so a long document doesn't go on to its next chunk.
  const stopRequested = useRef(false);
  // The mirror layer tracks the textarea's scroll so a long document's marks
  // stay glued to their words. See the header comment.
  const mirrorRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  // Set by the actions that unmount their own button (`applyMark`, Keep all,
  // Discard all): where focus goes on the next render (see the effect below).
  const focusMarkAfterRender = useRef<string | null>(null);
  const focusSurfaceAfterRender = useRef(false);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      const id = requestIdRef.current;
      if (id) void useStore.getState().cancelModelRequest(id).catch(() => {});
    };
  }, []);

  // A mark is a real button that unmounts the moment it is applied (as do
  // Keep all / Discard all), which would otherwise drop focus to <body> and
  // leave the next suggestion a full Tab cycle away. Hand focus to the next
  // mark, or to the text after the last one.
  useEffect(() => {
    const nextId = focusMarkAfterRender.current;
    const toSurface = focusSurfaceAfterRender.current;
    focusMarkAfterRender.current = null;
    focusSurfaceAfterRender.current = false;
    if (nextId) {
      mirrorRef.current
        ?.querySelector<HTMLButtonElement>(`[data-mark-id="${nextId}"]`)
        ?.focus();
    } else if (toSurface) {
      textareaRef.current?.focus();
    }
  }, [marks]);

  const trimmed = sourceText.trim();
  const canRun = trimmed.length > 0 && !busy && (mode === "correct" || !!targetLang);

  // The text Copy/Download actually act on: Correct mode edits its own
  // surface in place, so that text — with every kept suggestion already
  // written in — is the result. Translate's result pane is read-only.
  const finalText = mode === "correct" ? sourceText : resultText;
  const hasFinalText = mode === "correct" ? hasResult && sourceText.length > 0 : finalText.length > 0;
  const canClear =
    !busy && (sourceText.length > 0 || finalText.length > 0 || marks.length > 0 || live.length > 0);

  const resetOutputs = () => {
    setResultText("");
    setError(null);
    setMarks([]);
    setLive("");
    setStopped(false);
    setHasResult(false);
  };

  const clear = () => {
    setSourceText("");
    setUploadedFileName(null);
    setCopied(false);
    resetOutputs();
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
            if (mode === "translate") setResultText(accumulated);
            else setLive(accumulated);
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
          if (mode === "translate") setResultText(accumulated);
          else setLive(accumulated);
        }
      }
      return true;
    };

    try {
      const ran = await runModelRequest({ id, kind: "writingTool" }, runChunks);
      if (ran === undefined) return; // cancelled before it started
      if (isStale()) return;
      if (stopRequested.current) {
        // Keep whatever the model wrote so far on screen instead of turning
        // a half-finished correction into marks.
        setStopped(true);
        return;
      }
      if (mode === "correct") {
        // The diff is against the trimmed text that was actually sent, so
        // every mark is shifted past any leading whitespace the surface
        // carries and still lands on the right words.
        const leading = sourceText.indexOf(trimmed);
        const parts = diffMarks(computeWordDiff(trimmed, accumulated));
        setMarks(
          parts.map((m) => ({
            ...m,
            start: m.start + (leading > 0 ? leading : 0),
            end: m.end + (leading > 0 ? leading : 0),
          })),
        );
        setLive("");
        setHasResult(true);
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

  // --- Correct mode's in-place suggestions --------------------------------

  /** Write one suggestion into the text and forget it. No undo: the replaced
   * wording is gone from `sourceText` the moment this returns. Focus moves to
   * the next mark so a keyboard user doesn't fall back to the top of the page
   * when the button they just pressed unmounts; after the last one it lands in
   * the text, ready for the next edit. */
  const applyMark = (id: string) => {
    const index = marks.findIndex((m) => m.id === id);
    const next = marks[index + 1];
    const result = applyDiffMark(sourceText, marks, id);
    setSourceText(result.text);
    setMarks(result.marks);
    focusMarkAfterRender.current = next ? next.id : null;
    focusSurfaceAfterRender.current = !next;
  };

  const keepAll = () => {
    setSourceText(applyAllDiffMarks(sourceText, marks));
    setMarks([]);
    focusSurfaceAfterRender.current = true;
  };

  const discardAll = () => {
    setMarks([]);
    focusSurfaceAfterRender.current = true;
  };

  /** The surface's pending text as alternating untouched runs and marks.
   * Marks are non-overlapping and already sorted by construction. */
  const segments: Array<{ text: string } | { mark: DiffMark }> = [];
  {
    let cursor = 0;
    for (const mark of marks) {
      if (mark.start > cursor) segments.push({ text: sourceText.slice(cursor, mark.start) });
      segments.push({ mark });
      cursor = mark.end;
    }
    if (cursor < sourceText.length) segments.push({ text: sourceText.slice(cursor) });
  }

  const title = mode === "translate" ? t("writingTools.modeTranslate") : t("writingTools.modeCorrect");
  const TitleIcon = mode === "translate" ? LanguagesIcon : CorrectIcon;

  return (
    <div className="writing-tools-view">
      <div className="writing-tools-scroll">
        <div className="writing-tools-col">
          <div className="writing-tools-head">
            <h1 className="writing-tools-title">
              <TitleIcon className="writing-tools-title-icon" />
              {title}
            </h1>
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

          {mode === "correct" ? (
            // One surface: the user's own text, marks in place, still a real
            // textarea underneath (see the header comment).
            <div className="writing-tools-pane writing-tools-pane-correct">
              <div className="writing-tools-source-head">
                <label htmlFor="writing-tools-source">{t("writingTools.sourceLabel")}</label>
                <div className="writing-tools-result-actions">
                  <button
                    type="button"
                    className="btn-quiet"
                    disabled={busy}
                    onClick={() => void uploadDocument()}
                  >
                    {t("writingTools.uploadDocument")}
                  </button>
                  {marks.length > 0 && (
                    <>
                      <IconActionButton label={t("writingTools.keepAll")} onClick={keepAll}>
                        <KeepAllIcon />
                      </IconActionButton>
                      <IconActionButton label={t("writingTools.discardAll")} onClick={discardAll}>
                        <DiscardAllIcon />
                      </IconActionButton>
                    </>
                  )}
                  {hasFinalText && (
                    <IconActionButton
                      label={copied ? t("writingTools.copied") : t("writingTools.copy")}
                      onClick={() => void copy()}
                    >
                      {copied ? <CheckIcon /> : <CopyIcon />}
                    </IconActionButton>
                  )}
                  {uploadedFileName && hasFinalText && !busy && (
                    <IconActionButton label={t("writingTools.download")} onClick={() => void download()}>
                      <DownloadIcon />
                    </IconActionButton>
                  )}
                  <IconActionButton label={t("writingTools.clear")} onClick={clear} disabled={!canClear}>
                    <ClearIcon />
                  </IconActionButton>
                </div>
              </div>
              {uploadedFileName && (
                <p className="writing-tools-uploaded-file">
                  {t("writingTools.uploadedFile", { name: uploadedFileName })}
                </p>
              )}
              <div className="writing-tools-editor">
                <div className="writing-tools-editor-mirror" ref={mirrorRef}>
                  {marks.length === 0 ? (
                    <span aria-hidden="true">{sourceText}</span>
                  ) : (
                    segments.map((segment, i) =>
                      "text" in segment ? (
                        <span key={`equal-${i}`} aria-hidden="true">
                          {segment.text}
                        </span>
                      ) : (
                        <CorrectionMark
                          key={segment.mark.id}
                          mark={segment.mark}
                          onApply={applyMark}
                        />
                      ),
                    )
                  )}
                </div>
                <textarea
                  ref={textareaRef}
                  id="writing-tools-source"
                  value={sourceText}
                  disabled={busy}
                  placeholder={t("writingTools.sourcePlaceholder")}
                  onChange={(e) => {
                    setSourceText(e.target.value);
                    // Offsets no longer line up with the edited text.
                    setMarks([]);
                    if (e.target.value.trim().length === 0) setUploadedFileName(null);
                  }}
                  onScroll={(e) => {
                    if (mirrorRef.current) mirrorRef.current.scrollTop = e.currentTarget.scrollTop;
                  }}
                />
              </div>
              {live && <p className="writing-tools-live-text">{live}</p>}
            </div>
          ) : (
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
                  <label id="writing-tools-result-label" htmlFor="writing-tools-result">
                    {t("writingTools.resultLabel")}
                  </label>
                  <div className="writing-tools-result-actions">
                    {hasFinalText && (
                      <IconActionButton
                        label={copied ? t("writingTools.copied") : t("writingTools.copy")}
                        onClick={() => void copy()}
                      >
                        {copied ? <CheckIcon /> : <CopyIcon />}
                      </IconActionButton>
                    )}
                    {uploadedFileName && hasFinalText && !busy && (
                      <IconActionButton
                        label={t("writingTools.download")}
                        onClick={() => void download()}
                      >
                        <DownloadIcon />
                      </IconActionButton>
                    )}
                    <IconActionButton label={t("writingTools.clear")} onClick={clear} disabled={!canClear}>
                      <ClearIcon />
                    </IconActionButton>
                  </div>
                </div>
                <textarea
                  id="writing-tools-result"
                  aria-labelledby="writing-tools-result-label"
                  value={resultText}
                  readOnly
                  placeholder={t("writingTools.resultPlaceholder")}
                />
              </div>
            </div>
          )}

          {busy && !waiting && (
            <p className="writing-tools-working" role="status">
              <span className="searching-spinner" aria-hidden="true" />
              {mode === "translate" ? t("writingTools.translating") : t("writingTools.correcting")}
            </p>
          )}

          {chunkProgress && (
            <p className="writing-tools-notice" role="status">
              {t("writingTools.chunkProgress", chunkProgress)}
            </p>
          )}
          {stopped && (
            <p className="writing-tools-notice" role="status">
              {t("chatView.composer.improve.stopped")}
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

/**
 * One in-place correction mark. It sits in the mirror layer exactly where the
 * wrong wording sits in the textarea, and is a real `<button>`: tab-reachable,
 * Enter/Space activates it, and its accessible name carries the suggestion
 * ("Keep: you're nice") so the affordance is never hover-only. Hover and
 * focus both reveal the suggestion in a small tip; activating the mark writes
 * the suggestion in.
 */
function CorrectionMark({
  mark,
  onApply,
}: {
  mark: DiffMark;
  onApply: (id: string) => void;
}) {
  const { t } = useTranslation();
  const keep = t("writingTools.keepSuggestion");
  const label = mark.added ? `${keep}: ${mark.added}` : keep;
  return (
    <span className="writing-tools-mark-wrap">
      <button
        type="button"
        className={`writing-tools-mark${mark.removed ? "" : " is-insert"}`}
        data-mark-id={mark.id}
        aria-label={label}
        onClick={() => onApply(mark.id)}
      >
        {mark.removed ? (
          mark.removed
        ) : (
          <span className="writing-tools-mark-insert" aria-hidden="true">
            +
          </span>
        )}
      </button>
      <span className="writing-tools-mark-tip" aria-hidden="true">
        <span
          className={`writing-tools-mark-tip-text${mark.added ? "" : " is-delete"}`}
        >
          {mark.added || mark.removed}
        </span>
        <span className="writing-tools-mark-tip-hint">{keep}</span>
      </span>
    </span>
  );
}
