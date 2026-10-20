import {
  createContext,
  useContext,
  useDeferredValue,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import Markdown from "../components/Markdown";
import ConfirmDialog from "../components/ConfirmDialog";
import { useCloseOnOutside } from "../components/Dropdown";
import FavoriteButton from "../components/FavoriteButton";
import SafetyBanner from "../components/SafetyBanner";
import { ToolConsentCard } from "../components/ToolConsentCard";
import { AskUserCard } from "../components/AskUserCard";
import { InjectionReviewCard } from "../components/InjectionReviewCard";
import {
  InjectionNotices,
  injectionNoticeText,
  injectionShortName,
} from "../components/InjectionNotices";
import { useComposerGlide } from "../components/useComposerGlide";
import { useDialogFocus } from "../components/useDialogFocus";
import { useStickToBottom } from "../components/useStickToBottom";
import { api } from "../lib/api";
import { copyText } from "../lib/clipboard";
import {
  formatChatStats,
  formatSpeedCompact,
  isStartingModelDiskSlow,
  speedTier,
  timeAgo,
} from "../lib/format";
import { estimateTokensPerSec } from "../lib/speedEstimate";
import { bestModelForTag } from "../lib/triage";
import {
  improveBlocker,
  improveBlockerKey,
  joinImproved,
  keptStatus,
  splitDraftForImprove,
  type ImproveStatusKey,
} from "../lib/draftImprover";
import {
  branchAnchorKey,
  recommendedSampling,
  samplingParamsEqual,
  type Artifact,
  type ArtifactRef,
  type Attachment,
  type ChatMessage,
  type ChatStep,
  type Citation,
  type EngineStatus,
  type ForkOrigin,
  type ModelInfo,
  type ParentalStatus,
  type Skill,
  type TaskType,
} from "../lib/types";
import {
  agentToolsAvailable,
  isSafetyAlertVisible,
  smartModelRouterAvailable,
  type LiveActivity,
  parseSkillInvocationDisplay,
  useStore,
  webSearchAvailable,
  writingToolsAvailable,
} from "../state/store";

/** Bare host for a chip label when a source has no title. */
function hostOf(url: string): string {
  try {
    return new URL(url).hostname.replace(/^www\./, "");
  } catch {
    return url;
  }
}

/**
 * Per-host favicon cache, shared by every source chip for the app session.
 * Icons are fetched lazily through Rust (the webview can't load remote images
 * — see `Sources`) and deduped per host so a reply citing one site many times
 * makes a single request. Kept in memory only: never persisted onto a
 * conversation, so stored history stays small and icons refresh on reload.
 *   `undefined` → not fetched yet · `null` → fetched, no usable icon.
 */
const faviconCache = new Map<string, string | null>();
const faviconInflight = new Map<string, Promise<string | null>>();

function loadFavicon(url: string): Promise<string | null> {
  const host = hostOf(url);
  const cached = faviconCache.get(host);
  if (cached !== undefined) return Promise.resolve(cached);
  let inflight = faviconInflight.get(host);
  if (!inflight) {
    inflight = api
      .fetchFavicon(url)
      .catch(() => null)
      .then((icon) => {
        faviconCache.set(host, icon);
        faviconInflight.delete(host);
        return icon;
      });
    faviconInflight.set(host, inflight);
  }
  return inflight;
}

/** A source's favicon as a data URI, or null until one resolves (if ever). */
function useFavicon(url: string): string | null {
  const [icon, setIcon] = useState<string | null>(
    () => faviconCache.get(hostOf(url)) ?? null,
  );
  useEffect(() => {
    let alive = true;
    void loadFavicon(url).then((i) => {
      if (alive) setIcon(i);
    });
    return () => {
      alive = false;
    };
  }, [url]);
  return icon;
}

const CopyIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <rect x="9" y="9" width="11" height="11" rx="2.5" />
    <path d="M5 15a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2" />
  </svg>
);

const CheckIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M20 6 9 17l-5-5" />
  </svg>
);

const EditIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M12 20h9" />
    <path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z" />
  </svg>
);

const ForkIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <circle cx="6" cy="6" r="2.5" />
    <circle cx="6" cy="18" r="2.5" />
    <circle cx="18" cy="12" r="2.5" />
    <path d="M6 8.5v7" />
    <path d="M8.2 7.2c3.6 1 5.8 2.6 7.6 4.2" />
  </svg>
);

const RetryIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M3 12a9 9 0 0 1 15.4-6.4L21 8" />
    <path d="M21 3v5h-5" />
    <path d="M21 12a9 9 0 0 1-15.4 6.4L3 16" />
    <path d="M3 21v-5h5" />
  </svg>
);

const ChevronLeftIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="m15 6-6 6 6 6" />
  </svg>
);

const ChevronRightIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="m9 6 6 6-6 6" />
  </svg>
);

const ArrowDownIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M12 5v14" />
    <path d="m19 12-7 7-7-7" />
  </svg>
);

const ChatBubbleIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M4.5 6.5h15a1 1 0 0 1 1 1v8a1 1 0 0 1-1 1H10l-4 3v-3H4.5a1 1 0 0 1-1-1v-8a1 1 0 0 1 1-1Z" />
  </svg>
);

const GlobeIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <circle cx="12" cy="12" r="9" />
    <path d="M3 12h18" />
    <path d="M12 3a14 14 0 0 1 0 18a14 14 0 0 1 0-18Z" />
  </svg>
);

const ToolsIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z" />
  </svg>
);

const SpeakerIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M11 5 6 9H3v6h3l5 4V5Z" />
    <path d="M15.5 8.5a5 5 0 0 1 0 7" />
    <path d="M18.5 5.5a9 9 0 0 1 0 13" />
  </svg>
);

const SpeakerStopIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M11 5 6 9H3v6h3l5 4V5Z" />
    <rect x="15" y="9.5" width="5" height="5" rx="1" />
  </svg>
);

const MicIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <rect x="9" y="3" width="6" height="11" rx="3" />
    <path d="M5 11a7 7 0 0 0 14 0" />
    <path d="M12 18v3" />
  </svg>
);

const CommandIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M17 3 7 21" />
  </svg>
);

/** A recognized `/<skill>` turn's slug, shown as a small pill instead of raw
 * slash text — mirrors how the same invocation reads in the composer's own
 * `/`-autocomplete. Any trailing free text the user typed after it still
 * renders as a normal line underneath. */
function CommandChip({ skill }: { skill: Skill }) {
  return (
    <span className="msg-command-chip" title={skill.description || skill.name}>
      <CommandIcon />
      <span className="msg-command-chip-slug">/{skill.slug}</span>
    </span>
  );
}

/** Whether a plugin advertising `capability` is enabled, set up, and serving —
 * i.e. the matching Voice feature is ready to use right now. */
function useVoiceReady(capability: "tts" | "stt"): boolean {
  return useStore((s) =>
    s.plugins.some(
      (p) =>
        p.capabilities.includes(capability) &&
        p.enabled &&
        p.setup.type === "ready" &&
        p.service.type === "running",
    ),
  );
}

/** One source chip: its `[n]` marker, the site's favicon, and the page title
 * over its domain. The favicon is fetched lazily through Rust and falls back to
 * a globe while loading or when the site has none. */
function SourceChip({ citation }: { citation: Citation }) {
  const icon = useFavicon(citation.url);
  const host = hostOf(citation.url);
  return (
    <button
      className="source-chip"
      title={citation.url}
      onClick={() => void api.openLink(citation.url)}
    >
      <span className="source-num">{citation.id}</span>
      <span className="source-favicon" aria-hidden="true">
        {icon ? (
          <img src={icon} alt="" width={16} height={16} loading="lazy" />
        ) : (
          <GlobeIcon />
        )}
      </span>
      <span className="source-text">
        <span className="source-title">{citation.title || host}</span>
        {/* Title already falls back to the host, so only add the domain line
            when it says something the title doesn't. */}
        {citation.title ? <span className="source-domain">{host}</span> : null}
      </span>
    </button>
  );
}

/** The "Sources" strip under a web-search reply; clicking opens the page in the
 * system browser (the webview itself has no network access). */
function Sources({ citations }: { citations: Citation[] }) {
  const { t } = useTranslation();
  return (
    <div className="sources">
      <span className="sources-label">{t("chatView.sources.label")}</span>
      <div className="sources-list">
        {citations.map((c) => (
          <SourceChip key={c.id} citation={c} />
        ))}
      </div>
    </div>
  );
}

const ChevronIcon = () => (
  <svg className="trace-chevron" viewBox="0 0 24 24" fill="none" stroke="currentColor"
    strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="m9 6 6 6-6 6" />
  </svg>
);

const ThoughtIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M9.5 4.5a4 4 0 0 1 5 0 4 4 0 0 1 2.7 6.6A3.5 3.5 0 0 1 14.5 18a3 3 0 0 1-5 0 3.5 3.5 0 0 1-2.7-6.9 4 4 0 0 1 2.7-6.6Z" />
    <path d="M12 5v14" />
  </svg>
);

/** Friendly host label for a read step. */
function readLabel(step: { url: string; title: string }): string {
  return step.title && step.title !== step.url ? step.title : hostOf(step.url);
}

/** One `toolCall` step's line, shared between the top-level list and a
 * nested subagent's own tool calls. */
function ToolCallLine({ step }: { step: Extract<ChatStep, { type: "toolCall" }> }) {
  const { t } = useTranslation();
  return (
    <li className="trace-step">
      <span className="trace-step-kind">{step.data.toolId}</span>
      <span className="trace-step-text">
        {step.data.decision === "denied"
          ? t("chatView.toolConsent.denied")
          : step.data.decision === "rejected"
            ? t("chatView.toolConsent.rejected")
            : step.data.ok
              ? t("chatView.toolConsent.resultOk")
              : t("chatView.toolConsent.resultFailed")}
        {step.data.summary ? ` · ${step.data.summary}` : ""}
      </span>
    </li>
  );
}

/** One `askedQuestion` step's line — an `ask_user` call and its answer
 * (empty when the user dismissed it without one). */
function AskedQuestionLine({ step }: { step: Extract<ChatStep, { type: "askedQuestion" }> }) {
  const { t } = useTranslation();
  return (
    <li className="trace-step">
      <span className="trace-step-kind">{t("chatView.askUser.stepLabel")}</span>
      <span className="trace-step-text">
        {step.data.question}
        {step.data.answer ? ` → ${step.data.answer}` : ""}
      </span>
    </li>
  );
}

/** One step's line in `ReplyActivity`'s list. */
function StepLine({ step }: { step: ChatStep }) {
  const { t } = useTranslation();
  switch (step.type) {
    case "search":
      return (
        <li className="trace-step">
          <span className="trace-step-kind">{t("chatView.toolActivity.searchedLabel")}</span>
          <span className="trace-step-text">
            {t("chatView.toolActivity.searchedQuery", { query: step.data.query })}
          </span>
        </li>
      );
    case "read":
      return (
        <li className="trace-step">
          <span className="trace-step-kind">{t("chatView.toolActivity.readLabel")}</span>
          <button
            className="trace-step-link"
            title={step.data.url}
            onClick={() => void api.openLink(step.data.url)}
          >
            {readLabel(step.data)}
          </button>
        </li>
      );
    case "toolCall":
      return <ToolCallLine step={step} />;
    case "askedQuestion":
      return <AskedQuestionLine step={step} />;
    case "subagentRun":
      // A subagent's own tool calls are always flat `toolCall` steps
      // (nesting is capped at depth 1 — a subagent can't itself spawn
      // one), so `ToolCallLine` covers them without recursing.
      return (
        <li className="trace-step trace-step-subagent">
          <span className="trace-step-kind">
            {t("chatView.agentActivity.helperLabel", { kind: step.data.kind })}
          </span>
          <span className="trace-step-text">
            {step.data.ok === null
              ? t("chatView.toolConsent.resultRunning")
              : step.data.ok
                ? t("chatView.toolConsent.resultOk")
                : t("chatView.toolConsent.resultFailed")}
            {" · "}
            {step.data.task}
          </span>
          {step.data.steps.length > 0 && (
            <ul className="trace-steps trace-steps-nested">
              {step.data.steps.map((inner, j) =>
                inner.type === "toolCall" ? <ToolCallLine key={j} step={inner} /> : null,
              )}
            </ul>
          )}
        </li>
      );
  }
}

/** The localized status line for what a streaming turn is doing right now. */
function useLiveActivityLabel(activity: LiveActivity | null): string | null {
  const { t } = useTranslation();
  const { t: tInjection } = useTranslation("injection");
  if (!activity) return null;
  switch (activity.kind) {
    case "setup":
      return activity.message;
    case "searching":
      return t("chatView.activity.searching", { query: activity.query });
    case "readingResults":
      return t("chatView.activity.readingResults");
    case "reading":
      return t("chatView.activity.reading", { host: activity.host });
    case "tool":
      return t("chatView.activity.tool", { tool: activity.toolId });
    case "subagent":
      return activity.helper === "web-research"
        ? t("chatView.activity.researching")
        : t("chatView.activity.workingItOut");
    case "checking":
      return activity.name
        ? tInjection("activity.checkingNamed", {
            name: injectionShortName(activity.source, activity.name),
          })
        : tInjection("activity.checking");
  }
}

/**
 * Everything a reply did before answering — the model's private reasoning,
 * web searches, pages read, agent tool calls — folded into one collapsible
 * panel above the answer, so a turn that thinks, searches and runs tools
 * still reads as a single line. Collapsed by default, even while streaming:
 * the header narrates what the turn is doing right now (`liveStatus`), then
 * settles into a summary of what it did. Expanding shows the reasoning, then
 * each step in the order it happened. Steps are persisted on the message (built
 * by the frontend from the streamed events; see `runGeneration`), so the panel
 * stays re-openable after the fact.
 */
export function ReplyActivity({
  thinking,
  steps,
  liveStatus,
}: {
  thinking: string;
  steps: ChatStep[];
  /** What the turn is doing right now, or null once it's past gathering. */
  liveStatus: string | null;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const contentId = useId();
  const live = liveStatus !== null;
  const hasDetail = thinking !== "" || steps.length > 0;

  const searches = steps.filter((s) => s.type === "search").length;
  const reads = steps.filter((s) => s.type === "read").length;
  const actions = steps.filter(
    (s) => s.type === "toolCall" || s.type === "subagentRun" || s.type === "askedQuestion",
  ).length;
  const summary = [
    searches ? t("chatView.toolActivity.searchesCount", { count: searches }) : "",
    reads ? t("chatView.toolActivity.pagesReadCount", { count: reads }) : "",
    actions ? t("chatView.agentActivity.callCount", { count: actions }) : "",
  ]
    .filter(Boolean)
    .join(" · ");

  const head = (
    <>
      <ChevronIcon />
      {live ? (
        <span className="searching-spinner" aria-hidden="true" />
      ) : thinking ? (
        <ThoughtIcon />
      ) : actions ? (
        <ToolsIcon />
      ) : (
        <GlobeIcon />
      )}
      <span className="trace-title">
        {live
          ? liveStatus
          : thinking
            ? t("chatView.thinking.done")
            : actions
              ? t("chatView.agentActivity.title")
              : t("chatView.toolActivity.title")}
      </span>
      {summary && <span className="trace-count">{summary}</span>}
    </>
  );

  return (
    <div className={`trace${open ? " is-open" : ""}`}>
      {hasDetail ? (
        <button
          className="trace-head"
          onClick={() => setOpen((o) => !o)}
          aria-expanded={open}
          aria-controls={contentId}
        >
          {head}
        </button>
      ) : (
        // Nothing to expand yet (e.g. a first tool call still running) —
        // just the live status line, chevron slot kept so nothing shifts
        // once there is.
        <div className="trace-head is-static">{head}</div>
      )}
      {open && hasDetail && (
        <div id={contentId}>
          {thinking && <div className="trace-text">{thinking}</div>}
          {steps.length > 0 && (
            <ul className="trace-steps">
              {steps.map((step, i) => (
                <StepLine key={i} step={step} />
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}

/** Copy button with brief "Copied" feedback; shared by both message roles. */
function CopyButton({ text }: { text: string }) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);
  return (
    <button
      className="msg-action"
      title={t("chatView.copyButton.copy")}
      onClick={async () => {
        if (await copyText(text)) {
          setCopied(true);
          setTimeout(() => setCopied(false), 1400);
        }
      }}
    >
      {copied ? <CheckIcon /> : <CopyIcon />}
      <span className="msg-action-label">
        {copied ? t("chatView.copyButton.copied") : t("chatView.copyButton.copy")}
      </span>
    </button>
  );
}

/**
 * Prev/next arrows + "i/N" counter for a branch point with more than one
 * sibling variant — edit history on a user message, retry history on an
 * assistant message, both sharing this same component/keying scheme (see
 * `branchAnchorKey`/`Conversation.branches` in `lib/types.ts`). Always
 * visible, unlike the hover-gated `.msg-actions` row, so the "which version
 * am I looking at" counter is discoverable without hovering.
 */
function VariantNav({ anchorKey, active, total }: { anchorKey: string; active: number; total: number }) {
  const { t } = useTranslation();
  const setActiveVariant = useStore((s) => s.setActiveVariant);
  const streaming = useStore((s) => s.streaming);
  if (total <= 1) return null;
  return (
    <div className="msg-variant-nav" aria-label={t("chatView.variantNav.counter", { current: active + 1, total })}>
      <button
        className="msg-variant-nav-btn"
        title={t("chatView.variantNav.previous")}
        aria-label={t("chatView.variantNav.previous")}
        disabled={streaming || active === 0}
        onClick={() => void setActiveVariant(anchorKey, active - 1)}
      >
        <ChevronLeftIcon />
      </button>
      <span className="msg-variant-nav-count">{t("chatView.variantNav.counter", { current: active + 1, total })}</span>
      <button
        className="msg-variant-nav-btn"
        title={t("chatView.variantNav.next")}
        aria-label={t("chatView.variantNav.next")}
        disabled={streaming || active === total - 1}
        onClick={() => void setActiveVariant(anchorKey, active + 1)}
      >
        <ChevronRightIcon />
      </button>
    </div>
  );
}

/**
 * The fork-origin hint: "This conversation was forked from" + a small
 * clickable chip (chat icon + the source conversation's title, CSS-truncated
 * rather than manually cut, + how long ago it was last updated). Looks the
 * source up live in `conversations` (rather than a title snapshotted at fork
 * time) so a rename shows up here too. `conversations` is the full list,
 * loaded at boot before any chat can render, so a miss means the source was
 * deleted — say so in plain text instead of offering a link that can only
 * fail.
 */
function ForkOriginNote({ origin }: { origin: ForkOrigin }) {
  const { t } = useTranslation();
  const openConversationAtMessage = useStore((s) => s.openConversationAtMessage);
  const meta = useStore((s) => s.conversations.find((c) => c.id === origin.conversationId));
  if (!meta) {
    return (
      <div className="fork-origin-note">
        <span className="fork-origin-label">{t("chatView.forkOriginDeleted")}</span>
      </div>
    );
  }
  const title = meta.title || t("chatView.forkOriginBadge.unknownSource");
  return (
    <div className="fork-origin-note">
      <span className="fork-origin-label">{t("chatView.forkOriginHint")}</span>
      <button
        type="button"
        className="fork-origin-badge"
        onClick={() => void openConversationAtMessage(origin.conversationId, origin.messageId)}
        title={title}
        aria-label={t("chatView.forkOriginBadge.ariaLabel", { title })}
      >
        <ChatBubbleIcon />
        <span className="fork-origin-badge-title">{title}</span>
        <span className="fork-origin-badge-date">{timeAgo(meta.updatedAtMs)}</span>
      </button>
    </div>
  );
}

/**
 * An assistant reply's markdown body. `content` grows once per streamed
 * token — deferring it lets React coalesce the expensive markdown+syntax-
 * highlight re-parse of the whole message under a fast token stream instead
 * of running it synchronously on every single one.
 */
function AssistantBody({ content, isStreamingReply }: { content: string; isStreamingReply: boolean }) {
  const deferredContent = useDeferredValue(content);
  return (
    <>
      <Markdown>{deferredContent}</Markdown>
      {isStreamingReply && <span className="caret" />}
    </>
  );
}

const PlusIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M12 5v14M5 12h14" />
  </svg>
);

const ModelsIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <rect x="4" y="4" width="16" height="16" rx="3" />
    <path d="M4 10h16M10 4v16" />
  </svg>
);

const FileIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M14 3v5h5" />
    <path d="M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8Z" />
  </svg>
);

/** The "Improve my message" menu item's icon. */
const SparkleIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8Z" />
    <path d="M19 15l.8 2.2L22 18l-2.2.8L19 21l-.8-2.2L16 18l2.2-.8Z" />
  </svg>
);

const DownloadIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M12 3v12" />
    <path d="m7 10 5 5 5-5" />
    <path d="M5 21h14" />
  </svg>
);

const CloseIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M18 6 6 18M6 6l12 12" />
  </svg>
);

/** A magnifier hint shown over an image thumbnail on hover — clicking opens the
 * full-size lightbox. */
const ZoomIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <circle cx="11" cy="11" r="7" />
    <path d="m21 21-4.35-4.35M11 8v6M8 11h6" />
  </svg>
);

/**
 * Opening the in-chat file previews: a full-screen image lightbox and the
 * document side-panel. Provided by `ChatView` and consumed by every
 * `AttachmentList` (sent messages, the composer preview, the inline editor) so
 * a thumbnail or file chip is clickable wherever it appears. Null when rendered
 * outside a provider, which simply leaves attachments non-interactive.
 */
interface PreviewApi {
  openImage: (att: Attachment) => void;
  openDocument: (att: Attachment) => void;
  /** Close any preview currently showing this exact attachment — used when its
   * source goes away (e.g. the × removes it from the composer). */
  close: (att: Attachment) => void;
}
const PreviewContext = createContext<PreviewApi | null>(null);

/** Largest pasted image we embed — mirrors the Rust picker's cap
 * (`attachments::MAX_IMAGE_BYTES`) so both entry paths behave the same and a
 * huge screenshot can't bloat the conversation or get re-sent every turn. */
const MAX_IMAGE_BYTES = 16 * 1024 * 1024;

/**
 * The image thumbnails and document chips on a message — both the live preview
 * in the composer (with a remove button) and the read-only strip shown on a
 * sent user message. Images render straight from their `data:` URI (the webview
 * can't load remote images, but `data:` is allowed by the CSP).
 *
 * Each item is clickable: an image opens the full-screen lightbox, a document
 * opens the side-panel text preview (both via `PreviewContext`). The remove ×
 * is a separate sibling button so the two actions never nest.
 */
function AttachmentList({
  items,
  onRemove,
}: {
  items: Attachment[];
  onRemove?: (index: number) => void;
}) {
  const { t } = useTranslation();
  const preview = useContext(PreviewContext);

  const open = (a: Attachment) => {
    if (!preview) return;
    if (a.kind === "image") {
      if (a.dataUri) preview.openImage(a);
    } else {
      preview.openDocument(a);
    }
  };

  return (
    <div className="attachments">
      {items.map((a, i) => {
        // An image needs its data URI to be viewable; a document always opens
        // (the panel shows its text, or a "nothing readable" note).
        const canOpen = !!preview && (a.kind === "image" ? !!a.dataUri : true);
        return (
          <div key={i} className={`attachment attachment-${a.kind}`} title={a.name}>
            <button
              type="button"
              className="attachment-open"
              onClick={() => open(a)}
              disabled={!canOpen}
              aria-label={
                a.kind === "image"
                  ? t("chatView.attachment.view", { name: a.name })
                  : t("chatView.attachment.preview", { name: a.name })
              }
            >
              {a.kind === "image" && a.dataUri ? (
                <>
                  <img className="attachment-thumb" src={a.dataUri} alt={a.name} />
                  <span className="attachment-zoom" aria-hidden="true">
                    <ZoomIcon />
                  </span>
                </>
              ) : (
                <span className="attachment-doc">
                  <FileIcon />
                  <span className="attachment-name">{a.name}</span>
                </span>
              )}
            </button>
            {onRemove && (
              <button
                className="attachment-remove"
                onClick={() => {
                  // Don't leave a preview open for an item we just removed.
                  preview?.close(a);
                  onRemove(i);
                }}
                aria-label={t("chatView.attachment.remove", { name: a.name })}
                title={t("chatView.attachment.removeTitle")}
              >
                ×
              </button>
            )}
          </div>
        );
      })}
    </div>
  );
}

/**
 * The downloadable chips for files the model created this turn via the
 * `create_artifact` agent-mode tool, shown under an assistant reply. Distinct
 * from `AttachmentList` (user-supplied input): every artifact renders the same
 * plain file chip regardless of type (preview is always source-only), and
 * none are ever removable — only the model creates them, so there's no ×.
 *
 * Opening one is async — the message only carries an `ArtifactRef` (id,
 * filename, size); the content is fetched on demand via `api.getArtifact`
 * when a chip is clicked. `loadingId` marks the chip currently being fetched
 * so its button can show a busy state instead of double-firing the fetch.
 */
function ArtifactList({
  items,
  onOpen,
  loadingId,
}: {
  items: ArtifactRef[];
  onOpen: (a: ArtifactRef) => void;
  loadingId?: string | null;
}) {
  const { t } = useTranslation();
  return (
    <div className="attachments artifacts">
      {items.map((a) => (
        <div key={a.id} className="attachment attachment-document artifact-chip" title={a.filename}>
          <button
            type="button"
            className="attachment-open"
            onClick={() => onOpen(a)}
            disabled={loadingId === a.id}
            aria-busy={loadingId === a.id}
            aria-label={t("chatView.artifact.open", { name: a.filename })}
          >
            <span className="attachment-doc">
              <FileIcon />
              <span className="attachment-name">{a.filename}</span>
            </span>
          </button>
        </div>
      ))}
    </div>
  );
}

/**
 * Full-screen image viewer, shown when an image thumbnail is clicked. Dismisses
 * on the backdrop, the × button, or Escape (the key is handled in `ChatView` so
 * a stacked lightbox closes before the document panel — useDialogFocus's own
 * Escape handling is unused here, hence no `onClose` passed to it). Renders
 * nothing without a data URI to show. Focus moves into the dialog on open and
 * Tab is kept inside it (the close button is its only control).
 */
function ImageModal({ att, onClose }: { att: Attachment; onClose: () => void }) {
  const { t } = useTranslation();
  const rootRef = useDialogFocus<HTMLDivElement>();
  if (!att.dataUri) return null;
  return (
    <div
      ref={rootRef}
      tabIndex={-1}
      className="lightbox"
      role="dialog"
      aria-modal="true"
      aria-label={att.name}
      onClick={onClose}
    >
      <button
        className="lightbox-close"
        onClick={onClose}
        aria-label={t("common.close")}
        title={t("common.close")}
      >
        <CloseIcon />
      </button>
      {/* Clicks on the image and caption shouldn't reach the dismiss backdrop. */}
      <img
        className="lightbox-img"
        src={att.dataUri}
        alt={att.name}
        onClick={(e) => e.stopPropagation()}
      />
      <div className="lightbox-caption" onClick={(e) => e.stopPropagation()}>
        {att.name}
      </div>
    </div>
  );
}

/**
 * The right-hand side panel previewing an attached document's extracted text —
 * the same text the model was given. Opened by clicking a document chip; images
 * use the lightbox instead.
 */
function DocPreviewPanel({ att, onClose }: { att: Attachment; onClose: () => void }) {
  const { t } = useTranslation();
  const ref = useDialogFocus<HTMLElement>();
  const text = att.text ?? "";
  const hasText = text.trim() !== "";
  return (
    <aside
      ref={ref}
      tabIndex={-1}
      className="preview-panel"
      aria-label={t("chatView.docPreview.ariaLabel", { name: att.name })}
    >
      <div className="preview-head">
        <FileIcon />
        <span className="preview-title" title={att.name}>
          {att.name}
        </span>
        {hasText && <CopyButton text={text} />}
        <button
          className="preview-close"
          onClick={onClose}
          aria-label={t("chatView.docPreview.closeAriaLabel")}
          title={t("common.close")}
        >
          <CloseIcon />
        </button>
      </div>
      <div className="preview-body">
        {hasText ? (
          <pre className="preview-text">{text}</pre>
        ) : (
          <p className="preview-empty">{t("chatView.docPreview.noText")}</p>
        )}
      </div>
    </aside>
  );
}

/** File extension -> `rehype-highlight` language tag. `Markdown` runs
 * highlighting with `{ detect: false }` (no auto-detect), so the fenced code
 * block built below needs an explicit tag; anything unrecognized falls back
 * to `plaintext` rather than going unhighlighted-and-unlabeled. */
const EXTENSION_LANG: Record<string, string> = {
  md: "markdown",
  markdown: "markdown",
  html: "html",
  htm: "html",
  js: "javascript",
  jsx: "javascript",
  ts: "typescript",
  tsx: "typescript",
  py: "python",
  json: "json",
  css: "css",
  csv: "plaintext",
  sh: "bash",
  yaml: "yaml",
  yml: "yaml",
  txt: "plaintext",
};

function langFor(filename: string): string {
  const ext = filename.split(".").pop()?.toLowerCase() ?? "";
  return EXTENSION_LANG[ext] ?? "plaintext";
}

/**
 * The right-hand side panel previewing a model-created artifact's source.
 * Always shown as syntax-highlighted source — a fenced code block through the
 * same `Markdown` component every reply uses (see `langFor`) — never rendered
 * as live HTML/Markdown: an artifact is something the user is inspecting, and
 * letting its own markup execute/render would be surprising. A 4-backtick
 * fence wraps the content so an artifact that itself contains a triple-
 * backtick block (e.g. a Markdown file with a code sample) can't break out of
 * it (CommonMark only closes a fence with one of equal or greater length).
 * Opened by clicking an artifact chip (`ArtifactList`); `conversationId` is
 * threaded through for the Download button.
 */
function ArtifactPreviewPanel({
  artifact,
  conversationId,
  onClose,
}: {
  artifact: Artifact;
  conversationId: string;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const ref = useDialogFocus<HTMLElement>();
  const setNotice = useStore((s) => s.setNotice);
  const [downloading, setDownloading] = useState(false);
  const hasContent = artifact.content.trim() !== "";
  const fenced = `\`\`\`\`${langFor(artifact.filename)}\n${artifact.content}\n\`\`\`\``;

  const download = async () => {
    setDownloading(true);
    try {
      await api.downloadArtifact(conversationId, artifact.id);
    } catch (err) {
      setNotice(String(err));
    } finally {
      setDownloading(false);
    }
  };

  return (
    <aside
      ref={ref}
      tabIndex={-1}
      className="preview-panel"
      aria-label={t("chatView.artifactPreview.ariaLabel", { name: artifact.filename })}
    >
      <div className="preview-head">
        <FileIcon />
        <span className="preview-title" title={artifact.filename}>
          {artifact.filename}
        </span>
        {hasContent && <CopyButton text={artifact.content} />}
        <button
          className="msg-action"
          title={t("chatView.artifactPreview.download")}
          disabled={downloading}
          onClick={() => void download()}
          aria-label={t("chatView.artifactPreview.downloadAriaLabel", { name: artifact.filename })}
        >
          <DownloadIcon />
          <span className="msg-action-label">{t("chatView.artifactPreview.download")}</span>
        </button>
        <button
          className="preview-close"
          onClick={onClose}
          aria-label={t("chatView.artifactPreview.closeAriaLabel")}
          title={t("common.close")}
        >
          <CloseIcon />
        </button>
      </div>
      <div className="preview-body">
        {hasContent ? (
          <Markdown>{fenced}</Markdown>
        ) : (
          <p className="preview-empty">{t("chatView.artifactPreview.empty")}</p>
        )}
      </div>
    </aside>
  );
}

/**
 * Read a message aloud through the Voice plugin. While playing it becomes a stop
 * control; starting another message's playback stops this one (the backend plays
 * one clip at a time, which resolves this button's in-flight call). Renders
 * nothing unless a text-to-speech plugin is enabled and ready.
 */
function SpeakButton({ text }: { text: string }) {
  const { t } = useTranslation();
  const ready = useVoiceReady("tts");
  const [speaking, setSpeaking] = useState(false);
  if (!ready || text.trim() === "") return null;
  const label = speaking ? t("chatView.speakButton.stop") : t("chatView.speakButton.readAloud");
  return (
    <button
      className={`msg-action${speaking ? " is-speaking" : ""}`}
      title={label}
      onClick={async () => {
        if (speaking) {
          await api.stopSpeaking();
          return; // the in-flight speakText resolves and clears `speaking`
        }
        setSpeaking(true);
        try {
          await api.speakText(text);
        } catch {
          // A read-aloud failure is non-critical; the button just resets.
        } finally {
          setSpeaking(false);
        }
      }}
    >
      {speaking ? <SpeakerStopIcon /> : <SpeakerIcon />}
      <span className="msg-action-label">{label}</span>
    </button>
  );
}

/**
 * Live parental-lock state for the composer. Polled on the same
 * fire-and-forget footing as the engine status (see App.tsx) — a lock can
 * begin mid-session at a schedule boundary or once a daily cap runs out —
 * and re-read on every `streaming` transition, so a cap reached by the
 * message just sent shows up at once instead of up to half a minute later.
 */
function useParentalStatus(): ParentalStatus | null {
  const streaming = useStore((s) => s.streaming);
  const [status, setStatus] = useState<ParentalStatus | null>(null);

  useEffect(() => {
    let alive = true;
    const refresh = () =>
      api
        .getParentalStatus()
        .then((s) => alive && setStatus(s))
        .catch(() => {
          // Polling must never surface errors — and `chat_stream` refuses a
          // locked send on its own regardless of what this shows.
        });
    void refresh();
    const id = window.setInterval(() => void refresh(), 30_000);
    return () => {
      alive = false;
      window.clearInterval(id);
    };
  }, [streaming]);

  return status;
}

/** Why sending is paused right now, right above the composer. History stays
 * fully readable — only new messages wait. */
function ParentalLockBanner({ status }: { status: ParentalStatus | null }) {
  const { t } = useTranslation();
  if (!status?.locked) return null;
  return (
    <div className="engine-banner engine-banner-info engine-banner-inline" role="status">
      <div className="engine-banner-inner">
        <span className="engine-banner-text">
          {status.reason === "schedule"
            ? t("chatView.parentalLock.schedule", { time: status.allowedStart })
            : t("chatView.parentalLock.tomorrow")}
        </span>
      </div>
    </div>
  );
}

/** How long a chunk of new reply text may sit un-announced before it's
 * flushed anyway — keeps a long run-on reply (no punctuation) from going
 * silent, without reading every token. */
const STREAM_ANNOUNCE_INTERVAL_MS = 2000;

/** True once `text` ends with sentence/paragraph punctuation — a natural
 * place to flush a chunk to the live region instead of waiting out the
 * fixed interval. */
function endsAtBoundary(text: string): boolean {
  return /[.!?\n]\s*$/.test(text);
}

/**
 * Throttles the active streaming assistant reply into a handful of polite
 * live-region announcements — never one per token (finding C1). Flushes a
 * new chunk when the streamed-so-far text hits a sentence/paragraph
 * boundary, or after `STREAM_ANNOUNCE_INTERVAL_MS` of new text either way,
 * plus once more for whatever's left over when the reply finishes. Also
 * announces the "searching"/"thinking" lead-in once each starts, since
 * those precede any reply text and are otherwise completely silent to
 * screen readers.
 */
function useStreamingAnnouncement(
  messages: ChatMessage[],
  streaming: boolean,
  activityLabel: string | null,
): string[] {
  const { t } = useTranslation();
  const [chunks, setChunks] = useState<string[]>([]);
  const bookkeeping = useRef({ turnKey: -1, announcedLen: 0, lastFlush: 0, lastLeadIn: "" });

  const lastIndex = messages.length - 1;
  const last = messages[lastIndex];
  const isStreamingReply = streaming && last?.role === "assistant";

  useEffect(() => {
    if (!isStreamingReply) return;
    const state = bookkeeping.current;
    if (state.turnKey !== lastIndex) {
      bookkeeping.current = { turnKey: lastIndex, announcedLen: 0, lastFlush: 0, lastLeadIn: "" };
      setChunks([]);
    }
  }, [isStreamingReply, lastIndex]);

  useEffect(() => {
    if (!isStreamingReply || !last) return;
    const state = bookkeeping.current;
    if (state.turnKey !== lastIndex) return; // reset effect above hasn't run yet

    const content = last.content;
    const noBody = content === "";
    const leadIn = noBody
      ? (activityLabel ?? (last.thinking ? t("chatView.liveRegion.thinking") : ""))
      : "";

    if (leadIn && leadIn !== state.lastLeadIn) {
      state.lastLeadIn = leadIn;
      setChunks((c) => [...c, leadIn]);
      return;
    }

    if (content.length <= state.announcedLen) return;
    const unannounced = content.slice(state.announcedLen);
    const now = Date.now();
    if (endsAtBoundary(unannounced) || now - state.lastFlush > STREAM_ANNOUNCE_INTERVAL_MS) {
      state.announcedLen = content.length;
      state.lastFlush = now;
      setChunks((c) => [...c, unannounced]);
    }
  }, [isStreamingReply, lastIndex, last, activityLabel, t]);

  // Flush whatever's left once the reply stops streaming, so the final
  // (sub-boundary, sub-interval) tail of a reply is never dropped silently.
  useEffect(() => {
    if (streaming) return;
    const state = bookkeeping.current;
    const finished = messages[state.turnKey];
    if (state.turnKey === -1 || !finished || finished.role !== "assistant") return;
    const remaining = finished.content.slice(state.announcedLen);
    if (remaining) {
      state.announcedLen = finished.content.length;
      setChunks((c) => [...c, remaining]);
    }
  }, [streaming, messages]);

  return chunks;
}

export default function ChatView() {
  const { t } = useTranslation();
  const active = useStore((s) => s.active);
  const streaming = useStore((s) => s.streaming);
  const liveActivity = useStore((s) => s.liveActivity);
  const liveActivityLabel = useLiveActivityLabel(liveActivity);
  const pendingToolConsent = useStore((s) => s.pendingToolConsent);
  const pendingClarifyingQuestion = useStore((s) => s.pendingClarifyingQuestion);
  const pendingInjectionReview = useStore((s) => s.pendingInjectionReview);
  const { t: tInjection } = useTranslation("injection");
  const chatError = useStore((s) => s.chatError);
  const safetyAlertEntry = useStore((s) => (active ? s.safetyAlerts[active.id] : undefined));
  const dismissSafetyAlert = useStore((s) => s.dismissSafetyAlert);
  const models = useStore((s) => s.models);
  const editMessage = useStore((s) => s.editMessage);
  const retryMessage = useStore((s) => s.retryMessage);
  const forkConversation = useStore((s) => s.forkConversation);
  const scrollToMessageId = useStore((s) => s.scrollToMessageId);
  const clearScrollToMessage = useStore((s) => s.clearScrollToMessage);
  const skills = useStore((s) => s.skills);
  const showGenerationStats = useStore((s) => s.settings?.showGenerationStats ?? false);
  const userName = useStore((s) => s.settings?.userName?.trim() || null);
  const setNotice = useStore((s) => s.setNotice);

  // Friendly display name for a stored model id, falling back to the id
  // itself when the model is no longer installed.
  const modelName = (id: string) =>
    models.find((m) => m.spec.id === id)?.spec.name ?? id;

  const parental = useParentalStatus();
  const locked = parental?.locked ?? false;

  const messages = active?.messages ?? [];
  // A chat with nothing said yet: the greeting and the composer sit mid-screen
  // together, and the composer glides down once the first message is sent.
  const isEmpty = messages.length === 0;
  const composerSlotRef = useComposerGlide<HTMLDivElement>(isEmpty);
  const streamAnnouncements = useStreamingAnnouncement(messages, streaming, liveActivityLabel);
  // The streaming reply's newest "sent as typed" notice, for the status
  // region below. The other outcomes follow the user's own answer on a
  // review card (announced as an alertdialog), but this one shows up with
  // nothing else to draw attention to it.
  const lastMessage = messages[messages.length - 1];
  const latestSentNotice =
    streaming && lastMessage?.role === "assistant"
      ? [...(lastMessage.injectionNotices ?? [])].reverse().find((n) => n.outcome === "sent")
      : undefined;
  const activeId = active?.id;
  const { scrollRef, showJumpButton, follow, detach, scrollToBottom, scrollHandlers } =
    useStickToBottom<HTMLDivElement>();

  // Which user message (by index) is being edited inline, and its draft.
  const [editingIndex, setEditingIndex] = useState<number | null>(null);
  const [editDraft, setEditDraft] = useState("");

  // The assistant message index the Fork button was clicked on, awaiting
  // confirmation — null when the fork confirm dialog is closed.
  const [pendingForkIndex, setPendingForkIndex] = useState<number | null>(null);

  // In-chat previews: a clicked image opens the full-screen lightbox, a clicked
  // document opens the right-hand text panel. Both are chat-local view state.
  const [lightbox, setLightbox] = useState<Attachment | null>(null);
  const [docPreview, setDocPreview] = useState<Attachment | null>(null);
  const preview = useMemo<PreviewApi>(
    () => ({
      openImage: (att) => setLightbox(att),
      openDocument: (att) => setDocPreview(att),
      close: (att) => {
        setLightbox((cur) => (cur === att ? null : cur));
        setDocPreview((cur) => (cur === att ? null : cur));
      },
    }),
    [],
  );

  // The artifact preview panel: separate, independent state from `preview`
  // above (artifacts aren't user-supplied `Attachment`s, so they don't go
  // through `PreviewContext`) but the same open/close/Escape shape. Opening
  // one is async — a message only carries an `ArtifactRef`; the content is
  // fetched on demand — so `loadingArtifactId` marks the chip currently being
  // fetched, both to show a busy state and to ignore a repeat click on it.
  const [artifactPreview, setArtifactPreview] = useState<Artifact | null>(null);
  const [loadingArtifactId, setLoadingArtifactId] = useState<string | null>(null);
  const openArtifact = async (ref: ArtifactRef) => {
    if (!active || loadingArtifactId) return;
    setLoadingArtifactId(ref.id);
    try {
      const artifact = await api.getArtifact(active.id, ref.id);
      setArtifactPreview(artifact);
    } catch (err) {
      setNotice(String(err));
    } finally {
      setLoadingArtifactId(null);
    }
  };

  const cancelEdit = () => {
    setEditingIndex(null);
    setEditDraft("");
  };
  const saveEdit = (index: number) => {
    const text = editDraft;
    cancelEdit();
    // Editing rewinds the conversation past this turn; close any open preview so
    // it can't keep showing an attachment that was just truncated away.
    setLightbox(null);
    setDocPreview(null);
    void editMessage(index, text);
  };

  // A new chat, switching chats, or a fresh turn closes any open editor.
  useEffect(cancelEdit, [activeId, streaming]);

  // Editing an earlier turn is typing too — same reasoning as the composer's
  // own effect (see Composer), just the touch/pause half: an edit doesn't
  // need the preload half, since there's nothing new to send until the edit
  // is confirmed, at which point `editMessage` re-sends through the normal
  // `send` path anyway.
  const editTouchRef = useRef(0);
  useEffect(() => {
    if (editingIndex == null || editDraft.trim() === "") return;
    const s = useStore.getState();
    if (!(s.settings?.autoUnloadEnabled ?? true) || s.settings?.chatBackend === "cloud") return;
    const now = Date.now();
    if (s.engine.type === "running" && now - editTouchRef.current > 5_000) {
      editTouchRef.current = now;
      void s.snoozeAutoUnload();
    }
  }, [editDraft, editingIndex]);

  // Switching conversations closes any open preview.
  useEffect(() => {
    setLightbox(null);
    setDocPreview(null);
    setArtifactPreview(null);
  }, [activeId]);

  // Escape closes the top-most preview first (the lightbox), then whichever
  // side panel is open (a document and an artifact are never open at once —
  // opening one already goes through a chip click, and there's only one
  // right-hand panel slot on screen — but check both defensively).
  useEffect(() => {
    if (!lightbox && !docPreview && !artifactPreview) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (lightbox) setLightbox(null);
      else if (docPreview) setDocPreview(null);
      else setArtifactPreview(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [lightbox, docPreview, artifactPreview]);

  // Streams each new token into view — unless the user scrolled up to
  // re-read, which detaches this until they scroll back down or hit the
  // jump-to-bottom button (see useStickToBottom).
  useEffect(() => {
    follow();
  }, [messages, follow]);

  // Switching chats always lands at the latest messages.
  useEffect(() => {
    scrollToBottom();
  }, [activeId, scrollToBottom]);

  // The fork-origin hint's "jump to original message" link sets
  // `scrollToMessageId` (via `openConversationAtMessage`) right alongside
  // `active`/`messages`, so this runs in the same commit as — and,
  // declared after, later than — the activeId effect above, overriding its
  // land-at-the-bottom default with the specific target instead. Not pinned
  // to the bottom while parked on an old message, so a message arriving
  // moments later (background sync) doesn't yank the view away from it.
  useEffect(() => {
    if (!scrollToMessageId) return;
    detach();
    document.getElementById(`msg-${scrollToMessageId}`)?.scrollIntoView({
      behavior: "smooth",
      block: "end",
    });
    const timeout = setTimeout(() => clearScrollToMessage(), 2000);
    return () => clearTimeout(timeout);
  }, [scrollToMessageId, clearScrollToMessage, detach]);

  // Sending a message re-pins, so the new turn streams into view even if
  // the user had scrolled up to re-read. Only on the send itself: scrolling
  // up while the reply is streaming stays put.
  useEffect(() => {
    if (!streaming) return;
    scrollToBottom();
  }, [streaming, scrollToBottom]);

  return (
    <PreviewContext.Provider value={preview}>
      <div className="chat-layout">
        <div className={`chat${isEmpty ? " is-empty" : ""}`}>
      <div className="chat-scroll" ref={scrollRef} {...scrollHandlers}>
        <div className="chat-col">
          {/* Hidden from sighted users (the message list already renders the
              growing reply in real time) — this is purely the throttled
              announcement channel for screen readers. See
              useStreamingAnnouncement. */}
          <div className="sr-only" role="log" aria-live="polite" aria-relevant="additions text">
            {streamAnnouncements.map((chunk, i) => (
              <p key={i}>{chunk}</p>
            ))}
          </div>
          {/* Always mounted, so a screen reader is already listening when a
              notice lands — a live region inserted along with its text is
              often not announced. */}
          <div className="sr-only" role="status">
            {latestSentNotice ? injectionNoticeText(tInjection, latestSentNotice) : ""}
          </div>
          {isEmpty ? (
            <div className="chat-empty">
              <h1 className="chat-hello">
                {userName
                  ? t("chatView.emptyState.titleWithName", { name: userName })
                  : t("chatView.emptyState.title")}
              </h1>
              <p className="chat-hello-sub">{t("chatView.emptyState.subtitle")}</p>
            </div>
          ) : (
            <div className="messages" role="list">
              {messages.map((message, i) => {
                const isLast = i === messages.length - 1;
                if (message.role === "user") {
                  const editAnchorKey = branchAnchorKey(messages, i);
                  const editBranch = active?.branches[editAnchorKey];
                  if (editingIndex === i) {
                    return (
                      <div key={message.id} className="msg-user-row" role="listitem">
                        <span className="sr-only">{t("chatView.message.youSaid")}</span>
                        <div className="msg-edit">
                          {message.attachments && message.attachments.length > 0 && (
                            <AttachmentList items={message.attachments} />
                          )}
                          <textarea
                            className="msg-edit-area"
                            aria-label={t("chatView.messageEdit.textareaLabel")}
                            value={editDraft}
                            autoFocus
                            onChange={(e) => setEditDraft(e.target.value)}
                            onKeyDown={(e) => {
                              if (e.key === "Escape") {
                                e.preventDefault();
                                // Escape here only cancels the edit — don't let
                                // the global preview-Escape handler also fire.
                                e.stopPropagation();
                                cancelEdit();
                              } else if (
                                e.key === "Enter" &&
                                !e.shiftKey &&
                                !e.nativeEvent.isComposing &&
                                e.keyCode !== 229
                              ) {
                                e.preventDefault();
                                saveEdit(i);
                              }
                            }}
                          />
                          <div className="msg-edit-actions">
                            <button className="btn-quiet" onClick={cancelEdit}>
                              {t("common.cancel")}
                            </button>
                            <button
                              className="msg-edit-send"
                              onClick={() => saveEdit(i)}
                              // An image/document-only turn (no text) stays
                              // sendable; its attachments carry through the edit.
                              disabled={
                                editDraft.trim() === "" &&
                                !(message.attachments && message.attachments.length > 0)
                              }
                            >
                              {t("chatView.messageEdit.send")}
                            </button>
                          </div>
                        </div>
                      </div>
                    );
                  }
                  // A `/<skill>` turn shows what was actually typed, not the
                  // expanded `content` the model saw (see `send` in store.ts).
                  const shown = message.displayContent ?? message.content;
                  // `displayContent` is only ever set at send time when this
                  // turn *was* a recognized invocation (`send` in store.ts) —
                  // that's the source of truth, not a fresh match against the
                  // live (and possibly since-changed) `skills` list, which
                  // would relabel a past plain "/foo" turn as a chip the
                  // moment a matching skill happens to get created later.
                  const command =
                    message.displayContent !== undefined
                      ? parseSkillInvocationDisplay(shown, skills)
                      : null;
                  return (
                    <div
                      key={message.id}
                      className={
                        message.withheldFromModel ? "msg-user-row msg-user-withheld" : "msg-user-row"
                      }
                      role="listitem"
                    >
                      <span className="sr-only">{t("chatView.message.youSaid")}</span>
                      {message.attachments && message.attachments.length > 0 && (
                        <AttachmentList items={message.attachments} />
                      )}
                      {shown !== "" && (
                        <div className="msg-user">
                          {command ? (
                            <>
                              <CommandChip skill={command.skill} />
                              {command.rest !== "" && (
                                <div className="msg-command-rest">{command.rest}</div>
                              )}
                            </>
                          ) : (
                            shown
                          )}
                        </div>
                      )}
                      {/* The user stopped this message's flagged text at a
                          review card: it and the replies after it stay out
                          of every request until a Retry asks again. */}
                      {message.withheldFromModel && (
                        <span className="msg-withheld-tag">{tInjection("message.withheld")}</span>
                      )}
                      {editBranch && (
                        <VariantNav
                          anchorKey={editAnchorKey}
                          active={editBranch.active}
                          total={editBranch.variants.length}
                        />
                      )}
                      <div className="msg-actions-row">
                        <div className="msg-actions">
                          {/* Nothing to copy or read aloud on an attachment-only turn. */}
                          {shown !== "" && <CopyButton text={shown} />}
                          {shown !== "" && <SpeakButton text={shown} />}
                          <button
                            className="msg-action"
                            title={t("chatView.message.edit")}
                            disabled={streaming}
                            onClick={() => {
                              setEditingIndex(i);
                              setEditDraft(shown);
                            }}
                          >
                            <EditIcon />
                            <span className="msg-action-label">{t("chatView.message.edit")}</span>
                          </button>
                        </div>
                        {message.createdAtMs !== undefined && (
                          <div className="msg-date">{timeAgo(message.createdAtMs)}</div>
                        )}
                      </div>
                    </div>
                  );
                }
                const isStreamingReply = streaming && isLast;
                const noBody = message.content === "";
                const hasThinking = !!message.thinking;
                const hasSteps = !!message.steps && message.steps.length > 0;
                // What the streaming turn is doing right now — waiting on
                // the user first, since a pending card blocks everything
                // else. Null once it's past gathering (answer streaming, or
                // not the live turn at all).
                const liveStatus = !isStreamingReply
                  ? null
                  : pendingInjectionReview
                    ? tInjection("activity.waitingReview")
                    : pendingToolConsent
                      ? t("chatView.activity.waitingApproval")
                      : pendingClarifyingQuestion
                        ? t("chatView.activity.waitingAnswer")
                        : (liveActivityLabel ?? (noBody ? t("chatView.thinking.live") : null));
                // A plain reply still waiting on its first token has nothing
                // to fold away — it keeps the bare thinking dots instead.
                const showActivity =
                  hasThinking ||
                  hasSteps ||
                  (isStreamingReply &&
                    (liveActivity !== null ||
                      !!pendingToolConsent ||
                      !!pendingClarifyingQuestion ||
                      !!pendingInjectionReview));
                const retryAnchorKey = branchAnchorKey(messages, i);
                const retryBranch = active?.branches[retryAnchorKey];
                const forkOrigin =
                  active?.forkedFrom?.messageId === message.id ? active.forkedFrom : undefined;
                const isScrollTarget = scrollToMessageId === message.id;
                return (
                  <div
                    key={message.id}
                    id={`msg-${message.id}`}
                    className={isScrollTarget ? "msg-assistant-row msg-highlight" : "msg-assistant-row"}
                    role="listitem"
                  >
                    <span className="sr-only">{t("chatView.message.assistantSaid")}</span>
                    {showActivity && (
                      <ReplyActivity
                        thinking={message.thinking ?? ""}
                        steps={message.steps ?? []}
                        liveStatus={liveStatus}
                      />
                    )}
                    {message.injectionNotices?.length ? (
                      <InjectionNotices
                        notices={message.injectionNotices}
                        turnMessageId={
                          messages
                            .slice(0, i)
                            .reverse()
                            .find((m) => m.role === "user")?.id
                        }
                      />
                    ) : null}
                    {isStreamingReply && pendingToolConsent && (
                      // Keyed on requestId (not toolId, which is just the
                      // tool's name and can repeat across two different
                      // consecutive requests) so a second consent prompt for
                      // the same tool remounts and re-grabs focus, instead of
                      // reusing this instance and never re-firing
                      // useDialogFocus's focus-in effect.
                      <ToolConsentCard
                        key={pendingToolConsent.requestId}
                        consent={pendingToolConsent}
                      />
                    )}
                    {isStreamingReply && pendingClarifyingQuestion && (
                      // Keyed on requestId for the same reason as
                      // `ToolConsentCard` above.
                      <AskUserCard
                        key={pendingClarifyingQuestion.requestId}
                        question={pendingClarifyingQuestion}
                      />
                    )}
                    {isStreamingReply && pendingInjectionReview && (
                      // Keyed on requestId for the same reason as
                      // `ToolConsentCard` above: one card per tool result,
                      // each gets its own focus-in on mount (when the user
                      // isn't busy elsewhere — see the card's doc comment).
                      <InjectionReviewCard
                        key={pendingInjectionReview.requestId}
                        review={pendingInjectionReview}
                      />
                    )}
                    <div className="msg-assistant">
                      {noBody && isStreamingReply ? (
                        showActivity ? null : (
                          <span className="thinking">
                            <span className="thinking-dot" />
                            <span className="thinking-dot" />
                            <span className="thinking-dot" />
                          </span>
                        )
                      ) : (
                        <AssistantBody content={message.content} isStreamingReply={isStreamingReply} />
                      )}
                    </div>
                    {message.artifacts && message.artifacts.length > 0 && (
                      <ArtifactList
                        items={message.artifacts}
                        onOpen={(a) => void openArtifact(a)}
                        loadingId={loadingArtifactId}
                      />
                    )}
                    {message.citations && message.citations.length > 0 && (
                      <Sources citations={message.citations} />
                    )}
                    {!isStreamingReply && (
                      <VariantNav
                        anchorKey={retryAnchorKey}
                        active={retryBranch?.active ?? 0}
                        total={retryBranch?.variants.length ?? 1}
                      />
                    )}
                    {!isStreamingReply && (
                      <div className="msg-actions-row">
                        <div className="msg-actions">
                          {/* Nothing to copy or read aloud on a tool-only turn
                              with no final text. */}
                          {message.content !== "" && <CopyButton text={message.content} />}
                          {message.content !== "" && <SpeakButton text={message.content} />}
                          <button
                            className="msg-action"
                            title={t("chatView.message.fork")}
                            disabled={streaming}
                            onClick={() => setPendingForkIndex(i)}
                          >
                            <ForkIcon />
                            <span className="msg-action-label">{t("chatView.message.fork")}</span>
                          </button>
                          <button
                            className="msg-action"
                            title={t("chatView.message.retry")}
                            disabled={streaming}
                            onClick={() => void retryMessage(i)}
                          >
                            <RetryIcon />
                            <span className="msg-action-label">{t("chatView.message.retry")}</span>
                          </button>
                        </div>
                        {/* Read-only turn metadata — hover-gated together,
                            unlike `.msg-actions` beside it which stays
                            always-on. */}
                        <div className="msg-meta">
                          {message.createdAtMs !== undefined && (
                            <span className="msg-date">{timeAgo(message.createdAtMs)}</span>
                          )}
                          {message.content !== "" &&
                            (message.model ||
                              (showGenerationStats && message.stats && !isStreamingReply)) && (
                              <span className="msg-model" title={message.model}>
                                <span className="msg-model-sep">·</span>
                                {[
                                  message.model && modelName(message.model),
                                  showGenerationStats &&
                                    message.stats &&
                                    !isStreamingReply &&
                                    formatChatStats(message.stats),
                                ]
                                  .filter(Boolean)
                                  .join(" · ")}
                              </span>
                            )}
                        </div>
                      </div>
                    )}
                    {forkOrigin && <ForkOriginNote origin={forkOrigin} />}
                  </div>
                );
              })}
            </div>
          )}
          {/* Outside the role="list" above: ARIA requires a list's direct
              children to be listitems, and this isn't part of the turn
              sequence. Same visibility condition as before (only once
              there's at least one message). */}
          {messages.length > 0 && chatError && (
            <div className="chat-error" role="alert">
              {chatError}
            </div>
          )}
        </div>
      </div>
      {/* Zero-height anchor on the scroll area's bottom edge, so the button
          floats over the end of the conversation, just above the banners
          and composer, without taking any layout space itself. */}
      <div className="chat-jump-anchor">
        {showJumpButton && (
          <button
            type="button"
            className="chat-jump-btn"
            onClick={() => scrollToBottom(true)}
            aria-label={t("chatView.scrollToBottom")}
            title={t("chatView.scrollToBottom")}
          >
            <ArrowDownIcon />
          </button>
        )}
      </div>
      {/* One box for the banners, so an empty chat can seat them below the
          composer as a unit and keep the composer exactly centred. */}
      <div className="chat-banners">
        <EngineBanner />
        <ParentalLockBanner status={parental} />
        <SafetyBanner
          visible={isSafetyAlertVisible(safetyAlertEntry)}
          onDismiss={() => active && dismissSafetyAlert(active.id)}
        />
      </div>
      {/* The element the composer glides on (see useComposerGlide). A wrapper
          rather than a ref into Composer, which stays mounted at this spot so
          the draft and focus survive the empty chat's first message. */}
      <div className="chat-composer-slot" ref={composerSlotRef}>
        <Composer locked={locked} />
      </div>
        </div>
        {docPreview && (
          <DocPreviewPanel att={docPreview} onClose={() => setDocPreview(null)} />
        )}
        {artifactPreview && active && (
          <ArtifactPreviewPanel
            artifact={artifactPreview}
            conversationId={active.id}
            onClose={() => setArtifactPreview(null)}
          />
        )}
      </div>
      {lightbox && (
        <ImageModal att={lightbox} onClose={() => setLightbox(null)} />
      )}
      {pendingForkIndex !== null && (
        <ConfirmDialog
          title={t("chatView.forkDialog.title")}
          message={t("chatView.forkDialog.message")}
          confirmLabel={t("chatView.forkDialog.confirm")}
          confirmVariant="primary"
          onConfirm={() => {
            const index = pendingForkIndex;
            setPendingForkIndex(null);
            void forkConversation(index);
          }}
          onCancel={() => setPendingForkIndex(null)}
        />
      )}
    </PreviewContext.Provider>
  );
}

/** When your AI couldn't start, say why — and offer a one-click retry. */
function EngineBanner() {
  const { t } = useTranslation();
  const engine = useStore((s) => s.engine);
  const engineBusy = useStore((s) => s.engineBusy);
  const settings = useStore((s) => s.settings);
  const useModel = useStore((s) => s.useModel);
  const [dismissed, setDismissed] = useState(false);

  // A fresh failure gets a fresh banner.
  useEffect(() => {
    if (engine.type !== "failed") setDismissed(false);
  }, [engine.type]);

  if (engine.type !== "failed" || dismissed) return null;
  const selected = settings?.selectedModelId;

  return (
    <div className="engine-banner engine-banner-inline" role="alert">
      <div className="engine-banner-inner">
        <span className="engine-banner-text">{engine.data.message}</span>
        {selected && (
          <button
            className="btn-quiet btn-danger-quiet"
            disabled={engineBusy}
            onClick={() => void useModel(selected)}
          >
            {t("chatView.engineBanner.tryAgain")}
          </button>
        )}
        <button
          className="btn-quiet btn-danger-quiet"
          onClick={() => setDismissed(true)}
        >
          {t("chatView.engineBanner.dismiss")}
        </button>
      </div>
    </div>
  );
}

type ComposerState = "cloud" | "running" | "startingSlow" | "startingNormal" | "stopped";

/** Collapses the cloud/running/starting(-slow)/stopped branching used by both
 * the placeholder and the hint below the input, so the two can't drift out
 * of sync with each other. */
function composerState(
  cloudMode: boolean,
  engineType: EngineStatus["type"],
  diskSlow: boolean,
): ComposerState {
  if (cloudMode) return "cloud";
  if (engineType === "running") return "running";
  if (engineType === "starting") return diskSlow ? "startingSlow" : "startingNormal";
  return "stopped";
}

/** Quick model switch, next to Send — lists only downloaded models with a
 * hardware-estimated speed, color-coded fast/medium/slow. Absent entirely
 * when nothing is downloaded yet (nothing to switch to). */
function ModelQuickPicker({
  openTool,
  setOpenTool,
}: {
  openTool: "add" | "model" | null;
  setOpenTool: (t: "add" | "model" | null) => void;
}) {
  const { t } = useTranslation();
  const models = useStore((s) => s.models);
  const hardware = useStore((s) => s.report)?.hardware;
  const engine = useStore((s) => s.engine);
  const engineBusy = useStore((s) => s.engineBusy);
  const useModel = useStore((s) => s.useModel);
  const open = openTool === "model";
  const triggerRef = useRef<HTMLButtonElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  // Every close path (selection, Escape, outside click) should return focus
  // to the trigger rather than silently reverting to <body>.
  const closeMenu = () => {
    setOpenTool(null);
    triggerRef.current?.focus();
  };
  const ref = useCloseOnOutside<HTMLDivElement>(open, closeMenu);
  const installed = models
    .filter((m) => m.status.type === "installed")
    .map((m) => ({ model: m, speed: hardware ? estimateTokensPerSec(m.spec, hardware, m.fit) : 0 }))
    .sort((a, b) => b.speed - a.speed);
  const currentId =
    engine.type === "running" || engine.type === "starting" ? engine.data.modelId : null;

  // Opening the list moves focus onto the currently-loaded model's own row
  // (or the first row if none is loaded) — real DOM focus, not just a
  // visual highlight, so arrow-key navigation below has something to move
  // from/to and a screen reader announces the right option immediately.
  useEffect(() => {
    if (!open) return;
    const options = listRef.current?.querySelectorAll<HTMLButtonElement>('[role="option"]');
    if (!options || options.length === 0) return;
    const current = currentId
      ? Array.from(options).find((o) => o.dataset.modelId === currentId)
      : undefined;
    (current ?? options[0]).focus();
  }, [open, currentId]);

  if (installed.length === 0) return null;

  const onListKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const options = listRef.current?.querySelectorAll<HTMLButtonElement>(
      '[role="option"]:not(:disabled)',
    );
    if (!options || options.length === 0) return;
    const items = Array.from(options);
    const index = items.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === "ArrowDown") {
      e.preventDefault();
      items[(index + 1) % items.length].focus();
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      items[(index - 1 + items.length) % items.length].focus();
    } else if (e.key === "Home") {
      e.preventDefault();
      items[0].focus();
    } else if (e.key === "End") {
      e.preventDefault();
      items[items.length - 1].focus();
    }
  };

  return (
    <div className="menu-wrap" ref={ref}>
      <button
        ref={triggerRef}
        type="button"
        className={`composer-tool${open ? " is-on" : ""}`}
        onClick={() => setOpenTool(open ? null : "model")}
        aria-haspopup="listbox"
        aria-expanded={open}
        title={t("chatView.modelPicker.switchModel")}
        aria-label={t("chatView.modelPicker.switchModel")}
      >
        <ModelsIcon />
      </button>
      {open && (
        <div
          ref={listRef}
          className="menu menu-right"
          role="listbox"
          aria-label={t("chatView.modelPicker.switchModel")}
          onKeyDown={onListKeyDown}
        >
          {installed.map(({ model: m, speed }) => {
            const tier = speed > 0 ? speedTier(speed) : null;
            const dotClass =
              tier === "fast"
                ? "pill-dot-ok"
                : tier === "medium"
                  ? "pill-dot-warm"
                  : tier === "slow"
                    ? "pill-dot-bad"
                    : "pill-dot-off";
            return (
              <div key={m.spec.id} className="model-menu-row">
                <button
                  type="button"
                  role="option"
                  aria-selected={m.spec.id === currentId}
                  data-model-id={m.spec.id}
                  className="model-menu-item"
                  disabled={engineBusy}
                  onClick={() => {
                    closeMenu();
                    void useModel(m.spec.id);
                  }}
                >
                  <span className={`pill-dot ${dotClass}`} />
                  <span className="model-menu-name">
                    {m.spec.name}
                    {m.spec.id === currentId ? ` (${t("chatView.modelPicker.current")})` : ""}
                  </span>
                  {speed > 0 && <span className="model-menu-speed">{formatSpeedCompact(speed)}</span>}
                </button>
                <FavoriteButton modelId={m.spec.id} name={m.spec.name} favorite={m.favorite} />
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

/** Silent request-triage sampling auto-pick: classifies the in-progress
 * draft (debounced ~600ms after the last keystroke) and, when
 * `Settings.autoPickSampling` is on, applies the classified task's
 * recommended sampling to the active conversation — never shown in the chat
 * UI, visible only as a log line (`api.logTriageAction`) when it actually
 * changes something.
 *
 * Reuses the Generation panel's own "auto-suggested" comparison
 * (`recommendedSampling`/`samplingParamsEqual`, `lib/types.ts`): if the
 * conversation's sampling still matches the recommendation for its
 * *previous* task type, it's safe to also move it to the new task type's
 * recommendation; if the user has since hand-tuned a slider, that's left
 * alone (the override mechanism — no separate flag needed). */
function useTriageAutopilot(draft: string, currentModelId: string | null) {
  const settings = useStore((s) => s.settings);
  const updateGeneration = useStore((s) => s.updateGeneration);
  const useModel = useStore((s) => s.useModel);
  const smartModelRouterOn = useStore((s) => smartModelRouterAvailable(s.plugins));

  const cloudMode = settings?.chatBackend === "cloud";
  const autoPickSampling = settings?.autoPickSampling ?? true;

  // Model-switch state: only meaningful while smartModelRouterOn — the Smart
  // Model Router plugin gates whether any of this ever fires (see §1 of the
  // effect below).
  const pinnedRef = useRef(false);
  const lastAutoPickedIdRef = useRef<string | null>(null);
  const stableSuggestionRef = useRef<{ id: string; count: number } | null>(null);
  const pendingSuggestionRef = useRef<ModelInfo | null>(null);

  // The moment the loaded model changes to something auto-pick didn't just
  // pick itself, treat it as a manual choice (`ModelQuickPicker`) and pin
  // auto-pick off for the rest of this typing burst.
  useEffect(() => {
    if (lastAutoPickedIdRef.current !== null && currentModelId !== lastAutoPickedIdRef.current) {
      pinnedRef.current = true;
    }
  }, [currentModelId]);

  useEffect(() => {
    if (draft.trim() === "") {
      pinnedRef.current = false;
      lastAutoPickedIdRef.current = null;
      stableSuggestionRef.current = null;
      pendingSuggestionRef.current = null;
    }
  }, [draft]);

  const switchTo = (model: ModelInfo, tag: TaskType, reason: string): Promise<void> => {
    lastAutoPickedIdRef.current = model.spec.id;
    stableSuggestionRef.current = null;
    pendingSuggestionRef.current = null;
    void api.logTriageAction(`auto-picked model '${model.spec.name}' for task '${tag}' (${reason})`);
    return useModel(model.spec.id);
  };

  const classifyTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    if (classifyTimerRef.current) clearTimeout(classifyTimerRef.current);
    if ((!smartModelRouterOn && !autoPickSampling) || cloudMode || draft.trim() === "") {
      return;
    }
    const text = draft;
    classifyTimerRef.current = setTimeout(() => {
      void api
        .classifyMessage(text)
        .then((tag) => {
          if (!tag) return;
          // Fresh state at resolution time, not whatever was current when
          // this timer was scheduled 600ms ago.
          const s = useStore.getState();
          const conversation = s.active;
          const liveModelId =
            s.engine.type === "running" || s.engine.type === "starting"
              ? s.engine.data.modelId
              : (s.settings?.selectedModelId ?? null);
          const selectedSpec = s.models.find((m) => m.spec.id === liveModelId)?.spec;
          const showThinking = s.settings?.showThinking ?? true;

          if (autoPickSampling) {
            const previousRecommended = recommendedSampling(
              selectedSpec,
              conversation?.taskType ?? "general",
              showThinking,
            );
            const userCustomized =
              !!conversation && !samplingParamsEqual(conversation.sampling, previousRecommended);
            if (userCustomized) {
              updateGeneration({ taskType: tag });
            } else {
              updateGeneration({
                taskType: tag,
                sampling: recommendedSampling(selectedSpec, tag, showThinking),
              });
              void api.logTriageAction(`applied ${tag} sampling defaults`);
            }
          } else {
            updateGeneration({ taskType: tag });
          }

          if (!smartModelRouterOn || pinnedRef.current) return;
          const suggestion = bestModelForTag(s.models, tag, liveModelId);
          if (!suggestion) {
            stableSuggestionRef.current = null;
            pendingSuggestionRef.current = null;
            return;
          }
          if (s.engine.type === "stopped") {
            void switchTo(suggestion, tag, "engine was idle");
            return;
          }
          if (s.engine.type === "running") {
            const prevStable = stableSuggestionRef.current;
            const count = prevStable && prevStable.id === suggestion.spec.id ? prevStable.count + 1 : 1;
            stableSuggestionRef.current = { id: suggestion.spec.id, count };
            pendingSuggestionRef.current = suggestion;
            if (count >= 2) void switchTo(suggestion, tag, "classification was stable");
          }
        })
        .catch(() => {
          // Best-effort — a transient failure just means no auto-pick this
          // time, not something to surface to the user.
        });
    }, 600);
    return () => {
      if (classifyTimerRef.current) clearTimeout(classifyTimerRef.current);
    };
    // `models`/`useModel` intentionally omitted from deps: the callback
    // re-reads live store state via `useStore.getState()` instead of
    // closing over render-time values.
  }, [draft, smartModelRouterOn, autoPickSampling, cloudMode, updateGeneration]);

  /** Applies a pending model suggestion that never stabilized while typing
   * — called from `submit()` right before a message actually sends, so "the
   * right model" is guaranteed by send time either way. */
  const resolvePendingModelSwitch = async (): Promise<void> => {
    if (!smartModelRouterOn || pinnedRef.current) return;
    const pending = pendingSuggestionRef.current;
    const s = useStore.getState();
    const liveModelId =
      s.engine.type === "running" || s.engine.type === "starting"
        ? s.engine.data.modelId
        : (s.settings?.selectedModelId ?? null);
    if (pending && pending.spec.id !== liveModelId) {
      await switchTo(pending, s.active?.taskType ?? "general", "resolved at send time");
    }
  };

  return { resolvePendingModelSwitch };
}

function Composer({ locked }: { locked: boolean }) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState("");
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  // One-shot hand-off from the standalone "Improve a message" screen's
  // "Start a new chat with this" button (`startChatWithDraft`, store.ts):
  // `newChat()` already remounts this component (switching away from
  // "improve" unmounts it, `view: "chat"` mounts a fresh one), so reading
  // and clearing the pending draft once on mount is enough — no risk of a
  // later, unrelated store update resurrecting stale text.
  useEffect(() => {
    const pending = useStore.getState().draftToPrefill;
    if (pending) {
      setDraft(pending);
      useStore.setState({ draftToPrefill: null });
    }
  }, []);
  const engine = useStore((s) => s.engine);
  const streaming = useStore((s) => s.streaming);
  const send = useStore((s) => s.send);
  const stopGeneration = useStore((s) => s.stopGeneration);
  const plugins = useStore((s) => s.plugins);
  const webSearch = useStore((s) => s.webSearch);
  const setWebSearch = useStore((s) => s.setWebSearch);
  const agentTools = useStore((s) => s.agentTools);
  const setAgentTools = useStore((s) => s.setAgentTools);
  const models = useStore((s) => s.models);
  const settings = useStore((s) => s.settings);
  const setNotice = useStore((s) => s.setNotice);
  const skills = useStore((s) => s.skills);
  const isAndroid = useStore((s) => s.appInfo?.isAndroid ?? false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const [dictating, setDictating] = useState(false);
  const [openTool, setOpenTool] = useState<"add" | "model" | null>(null);
  const [skillHighlight, setSkillHighlight] = useState(0);
  // Dismissed by Escape or a click outside the composer (see the
  // `useCloseOnOutside` wiring below) until the draft next changes — unlike
  // `openTool`'s menus, the skill dropdown has no toggle button of its own;
  // its visibility is otherwise driven purely by whether `draft` still
  // matches a `/`-prefix.
  const [skillSuggestDismissed, setSkillSuggestDismissed] = useState(false);
  // "Improve my message": in-flight state, the pre-improve draft (kept so
  // Undo/Escape can restore it verbatim), and the one-line status shown in
  // the hint slot. `improveStatus.raw`, when set, is a backend error string
  // shown as-is instead of an i18n key.
  const [improving, setImproving] = useState(false);
  const [previousDraft, setPreviousDraft] = useState<string | null>(null);
  const [improveStatus, setImproveStatus] = useState<{
    key: ImproveStatusKey;
    alert: boolean;
    raw?: string;
  } | null>(null);
  // Read inside `improve()`'s async continuation to detect a manual edit
  // (or a dictation result) that landed while the request was in flight, so
  // a stale rewrite never clobbers text the user has since changed.
  const draftRef = useRef(draft);
  useEffect(() => {
    draftRef.current = draft;
  }, [draft]);
  // StrictMode-safe request guard, same shape as WritingToolsView.
  const improveRequestSeq = useRef(0);
  const improveAlive = useRef(true);
  useEffect(() => {
    improveAlive.current = true;
    return () => {
      improveAlive.current = false;
    };
  }, []);
  const addTriggerRef = useRef<HTMLButtonElement>(null);
  // However the "+" menu closes (selection, Escape, outside click), focus
  // should land back on its trigger rather than silently reverting to
  // <body> — used in place of a bare `setOpenTool(null)` in its own handlers.
  const closeAddMenu = () => {
    setOpenTool(null);
    addTriggerRef.current?.focus();
  };
  const addMenuRef = useCloseOnOutside<HTMLDivElement>(openTool === "add", closeAddMenu);

  // Web search is the experimental `web-search` add-on: the menu item
  // appears once it's on and set up (`webSearchAvailable`), and toggles it
  // for the current conversation only. A cloud model has no local engine endpoint to
  // search/fetch against (see commands::web_search_chat_turn), so it's
  // disabled there — the backend routes cloud-mode turns straight to the
  // cloud model regardless of this toggle's state, but leaving it clickable
  // would be a confusing dead end. Same treatment on Android: the local
  // SearXNG sidecar needs a host Python this platform doesn't have, and
  // there's no hosted-API replacement yet (docs/design/MOBILE.md item 13) — the
  // backend already degrades gracefully if this were reached anyway, but a
  // dead toggle is worse than a labeled-unavailable one — so on Android the
  // item shows even with no plugin to back it.
  const cloudMode = settings?.chatBackend === "cloud";
  const webSearchReady = webSearchAvailable(plugins);
  const webSearchUsable = webSearchReady && !cloudMode && !isAndroid;
  const webSearchOn = webSearchUsable && webSearch;

  // The Message improver: gated by the `writing-tools` plugin, same as
  // Translate/Correct (unlike web search/agent tools, no cloud/Android
  // restriction — `enhance_prompt` runs through the same `chat_stream`-shaped
  // path those already use everywhere, including Android's RemoteBackend and
  // the cloud backend).
  const improverOn = writingToolsAvailable(plugins);

  // Same shape as web search: shown once agent mode is enabled in Settings,
  // usable only once a workspace folder is also configured there, off for a
  // cloud model for the same reason (no local engine endpoint to drive the
  // agent loop against). Also off on Android: `RemoteBackend::endpoint()`
  // always returns `None` by design (crates/core/src/remote/client/
  // backend.rs), so agent mode can never actually start there — without this
  // gate, tapping the toggle would silently hit `agent_chat_turn`'s "Your AI
  // isn't ready yet. Pick a model, then try again." error, which is accurate
  // for desktop but wrong on a phone that has no model to pick.
  const agentToolsCanBeUsed = !cloudMode && !isAndroid && agentToolsAvailable(settings);
  const agentToolsOn = agentToolsCanBeUsed && agentTools;

  // The mic is shown only when a voice-typing (stt) plugin is installed; it's
  // usable only once that plugin is enabled and its service is running. Clicking
  // records until clicked again, then drops the recognized text into the draft.
  const stt = plugins.find((p) => p.capabilities.includes("stt"));
  const sttReady =
    !!stt && stt.enabled && stt.setup.type === "ready" && stt.service.type === "running";

  const dictate = async () => {
    if (dictating) {
      await api.stopDictation();
      return; // the in-flight transcribeSpeech resolves with the text below
    }
    setDictating(true);
    try {
      const text = await api.transcribeSpeech();
      if (text) {
        setDraft((d) => (d && !/\s$/.test(d) ? d + " " : d) + text);
      }
    } catch {
      // A dictation failure is non-critical; the button just resets.
    } finally {
      setDictating(false);
    }
  };

  // A cloud model needs no local engine — it's always "ready" the moment
  // it's selected (no subprocess to wake up), unlike a local model. Only
  // Send waits on this — typing, attaching, and toggling web search all work
  // regardless of whether a model is loaded yet.
  const ready = cloudMode || engine.type === "running";

  // The model that will answer — the running/starting engine, else the saved
  // choice — decides whether images are allowed: only vision models can see
  // them. Documents work with any model, so the "+" menu itself is always
  // available. Cloud models don't declare vision support yet (`CloudModelInfo`
  // has no such field) — assume no until they do, rather than trusting a
  // stale local selection left over from before switching to cloud mode.
  const currentModelId =
    engine.type === "running" || engine.type === "starting"
      ? engine.data.modelId
      : (settings?.selectedModelId ?? null);
  const visionOk =
    !cloudMode && !!models.find((m) => m.spec.id === currentModelId)?.spec.vision;

  const { resolvePendingModelSwitch } = useTriageAutopilot(draft, currentModelId);

  const state = composerState(
    cloudMode,
    engine.type,
    isStartingModelDiskSlow(models, currentModelId ?? ""),
  );

  const placeholder = locked
    ? t("chatView.parentalLock.placeholder")
    : state === "cloud" || state === "running"
      ? webSearchOn
        ? t("chatView.composer.placeholderWebSearch")
        : t("chatView.composer.placeholderDefault")
      : state === "startingSlow"
        ? t("chatView.composer.startingSlow")
        : state === "startingNormal"
          ? t("chatView.composer.startingNormal")
          : t("chatView.composer.placeholderStopped");

  // Replaces the old static "Private by design" note — surfaces the same
  // loading state the placeholder describes, but as a persistent line that
  // doesn't disappear once the user starts typing.
  const hint =
    state === "startingSlow"
      ? t("chatView.composer.startingSlow")
      : state === "startingNormal"
        ? t("chatView.composer.startingNormal")
        : state === "stopped"
          ? t("chatView.composer.hintStopped")
          : null;

  // Read a pasted/clipboard image into a data: URI attachment, in the webview
  // (small, user-chosen, and the bytes are needed for the thumbnail anyway).
  const addImage = (file: File) => {
    if (file.size > MAX_IMAGE_BYTES) {
      setNotice(t("chatView.composer.imageTooLarge"));
      return;
    }
    const reader = new FileReader();
    reader.onload = () =>
      setAttachments((a) => [
        ...a,
        {
          kind: "image",
          name: file.name || t("chatView.composer.pastedImageName"),
          dataUri: reader.result as string,
        },
      ]);
    reader.readAsDataURL(file);
  };

  // The "+" menu's "Add files" item: pick images (vision models only) and/or
  // documents through a native dialog. Rust reads the files — images to
  // data: URIs, documents to extracted text. Works regardless of model
  // loading state — only Send waits on `ready`. Documents are pre-checked
  // for prompt injection only in a chat whose turns are checked: one with
  // agent tools or web search on.
  const addFiles = async () => {
    try {
      const picked = await api.pickAttachments(visionOk, agentToolsOn || webSearchOn);
      if (picked.length) setAttachments((a) => [...a, ...picked]);
    } catch (err: unknown) {
      setNotice(String(err));
    }
  };

  // Pasting a screenshot or copied picture attaches it — but only for vision
  // models, with a gentle nudge otherwise.
  const onPaste = (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
    const images = Array.from(e.clipboardData.items)
      .filter((it) => it.kind === "file" && it.type.startsWith("image/"))
      .map((it) => it.getAsFile())
      .filter((f): f is File => f !== null);
    if (images.length === 0) return; // let normal text paste through
    e.preventDefault();
    if (!visionOk) {
      setNotice(t("chatView.composer.visionOnlyPaste"));
      return;
    }
    images.forEach(addImage);
  };

  const removeAttachment = (index: number) =>
    setAttachments((a) => a.filter((_, i) => i !== index));

  const canSend =
    ready &&
    !locked &&
    !streaming &&
    !improving &&
    (draft.trim() !== "" || attachments.length > 0);

  // "/" as the very start of the draft, with no space yet, offers matching
  // skills — accepting one just completes the trigger text. Recognizing and
  // expanding a `/<slug>` invocation itself happens in `send` (store.ts),
  // regardless of whether it was typed by hand or completed from here.
  const slashMatch = /^\/(\S*)$/.exec(draft);
  const skillSuggestions =
    slashMatch && skills.length > 0
      ? skills.filter((s) => s.slug.startsWith(slashMatch[1].toLowerCase()))
      : [];
  const skillSuggestOpen = skillSuggestions.length > 0 && !skillSuggestDismissed;
  const skillSuggestRef = useCloseOnOutside<HTMLDivElement>(skillSuggestOpen, () =>
    setSkillSuggestDismissed(true),
  );

  useEffect(() => {
    setSkillHighlight(0);
    setSkillSuggestDismissed(false);
  }, [draft]);

  // Not available while a model runs disk-slow: the KV-cache measurement
  // that makes an improve request "lightweight" (a ~60ms next-turn cost,
  // not a multi-second re-prefill) depends on llama-server's host-RAM
  // prompt cache, and disk-slow mode has no RAM headroom for it — see
  // docs/design/PROMPT_ENHANCER.md's Context policy.
  const diskSlow = engine.type === "running" && engine.data.diskSlow;
  const { text: improveDraftText, hadSkillPrefix: improveHadSkillPrefix } =
    splitDraftForImprove(draft, skills);
  const blocker = improveBlocker({
    locked,
    improving,
    streaming,
    ready,
    starting: engine.type === "starting",
    diskSlow,
    draftText: improveDraftText,
    hadSkillPrefix: improveHadSkillPrefix,
    slashMatch: slashMatch !== null,
  });

  const improve = async () => {
    const { prefix, text: textToImprove } = splitDraftForImprove(draft, skills);
    const seq = ++improveRequestSeq.current;
    const isStale = () => !improveAlive.current || improveRequestSeq.current !== seq;
    const original = draft;

    setPreviousDraft(original);
    setImproving(true);
    setImproveStatus({ key: "chatView.composer.improve.working", alert: false });

    try {
      const outcome = await api.enhancePrompt(textToImprove);
      if (isStale()) return;
      if (outcome.type === "enhanced") {
        if (draftRef.current !== original) {
          // The user (or a finishing dictation) changed the draft while
          // this request was in flight — never overwrite their newer text.
          setPreviousDraft(null);
          setImproveStatus(null);
        } else {
          setDraft(joinImproved(prefix, outcome.data.text));
          setImproveStatus({ key: "chatView.composer.improve.done", alert: false });
          requestAnimationFrame(() => {
            const el = textareaRef.current;
            if (el) {
              el.focus();
              const len = el.value.length;
              el.setSelectionRange(len, len);
            }
          });
        }
      } else {
        setPreviousDraft(null);
        setImproveStatus(keptStatus(outcome.data.reason));
      }
    } catch (err: unknown) {
      if (isStale()) return;
      setPreviousDraft(null);
      setImproveStatus({
        key: "chatView.composer.improve.failed",
        alert: true,
        raw: String(err),
      });
    } finally {
      if (!isStale()) setImproving(false);
    }
  };

  const undoImprove = () => {
    if (previousDraft === null) return;
    setDraft(previousDraft);
    setPreviousDraft(null);
    setImproveStatus(null);
    textareaRef.current?.focus();
  };

  // Escape stops an in-flight improve even though the textarea isn't
  // focused at that point: clicking the improve button leaves focus on
  // that button (the default for a plain click), not the textarea, so the
  // textarea's own onKeyDown below never sees the keystroke while improving.
  useEffect(() => {
    if (!improving) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        void api.stopGeneration();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [improving]);

  const submit = async () => {
    if (!ready || locked || streaming || improving) return;
    if (draft.trim() === "" && attachments.length === 0) return;
    const text = draft;
    const atts = attachments;
    setDraft("");
    setAttachments([]);
    setPreviousDraft(null);
    setImproveStatus(null);
    await resolvePendingModelSwitch();
    void send(text, atts);
  };

  // Typing is a strong "about to chat" signal: while the smart load/unload
  // feature is on, it counts as activity (pausing the idle-unload countdown —
  // see EngineManager::touch_activity) and, if the model's asleep, starts
  // waking it up now instead of waiting for Send, so the first reply feels
  // faster. Reads fresh store state on every keystroke rather than closing
  // over subscribed values, so a setting flipped mid-type takes effect at
  // once without needing to list every field as an effect dependency.
  // Throttled so a fast typist doesn't spam the backend.
  //
  // `suppressPreloadRef` — not "have I already preloaded this burst" (that's
  // already covered: once `useModel` kicks in, `engine.type` flips away from
  // "stopped" to "starting", so the condition below stops matching on its
  // own) — instead remembers "the user just explicitly unloaded the model
  // mid-burst, so don't undo that by reloading it on the next keystroke".
  // It consumes (and clears) the store's `userStoppedModel` flag the first
  // time it observes it, and itself resets once the draft empties — a later,
  // fresh burst of typing preloads normally again.
  const typingTouchRef = useRef(0);
  const suppressPreloadRef = useRef(false);
  useEffect(() => {
    if (draft.trim() === "") {
      suppressPreloadRef.current = false;
      return;
    }
    const s = useStore.getState();
    if (!(s.settings?.autoUnloadEnabled ?? true) || s.settings?.chatBackend === "cloud") return;

    const now = Date.now();
    if (s.engine.type === "running" && now - typingTouchRef.current > 5_000) {
      typingTouchRef.current = now;
      void s.snoozeAutoUnload();
    }

    if (s.userStoppedModel) {
      suppressPreloadRef.current = true;
      useStore.setState({ userStoppedModel: false });
    }

    if (s.engine.type === "stopped" && !s.engineBusy && !suppressPreloadRef.current) {
      const selected = s.settings?.selectedModelId;
      const installed =
        !!selected && s.models.find((m) => m.spec.id === selected)?.status.type === "installed";
      if (installed) void s.useModel(selected as string);
    }
  }, [draft]);

  useEffect(() => {
    const el = textareaRef.current;
    if (el) {
      el.style.height = "auto";
      el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
    }
  }, [draft]);

  return (
    <div className="composer-wrap">
      <div className="composer">
        {attachments.length > 0 && (
          <AttachmentList items={attachments} onRemove={removeAttachment} />
        )}
        <div className="composer-row">
          <div className="menu-wrap" ref={addMenuRef}>
            <button
              ref={addTriggerRef}
              type="button"
              className={`composer-tool${openTool === "add" ? " is-on" : ""}`}
              onClick={() => setOpenTool((cur) => (cur === "add" ? null : "add"))}
              aria-expanded={openTool === "add"}
              title={t("chatView.composer.addMenuTitle")}
              aria-label={t("chatView.composer.addMenuTitle")}
            >
              <PlusIcon />
            </button>
            {openTool === "add" && (
              <div className="menu">
                <button
                  type="button"
                  className="menu-item"
                  onClick={() => {
                    closeAddMenu();
                    void addFiles();
                  }}
                >
                  <FileIcon />
                  {visionOk
                    ? t("chatView.composer.addImagesOrFiles")
                    : t("chatView.composer.addFilesOnly")}
                </button>
                {settings && (webSearchReady || isAndroid) && (
                  <button
                    type="button"
                    className={`menu-item${webSearchOn ? " is-on" : ""}`}
                    disabled={!webSearchUsable}
                    onClick={() => {
                      setWebSearch(!webSearch);
                      closeAddMenu();
                    }}
                    title={
                      isAndroid
                        ? t("chatView.composer.webSearchOffMobile")
                        : cloudMode
                          ? t("chatView.composer.webSearchOffCloud")
                          : webSearchOn
                            ? t("chatView.composer.webSearchOnForChat")
                            : t("chatView.composer.webSearchOffForChat")
                    }
                  >
                    <GlobeIcon />
                    {webSearchOn
                      ? t("chatView.composer.webSearchOnLabel")
                      : t("chatView.composer.webSearchLabel")}
                  </button>
                )}
                {settings && (
                  <button
                    type="button"
                    className={`menu-item${agentToolsOn ? " is-on" : ""}`}
                    disabled={!agentToolsCanBeUsed}
                    onClick={() => {
                      setAgentTools(!agentTools);
                      closeAddMenu();
                    }}
                    title={
                      isAndroid
                        ? t("chatView.composer.agentToolsOffMobile")
                        : cloudMode
                          ? t("chatView.composer.agentToolsOffDisabled")
                          : agentToolsCanBeUsed
                            ? agentToolsOn
                              ? t("chatView.composer.agentToolsOnForChat")
                              : t("chatView.composer.agentToolsOffForChat")
                            : t("chatView.composer.agentToolsOffDisabled")
                    }
                  >
                    <ToolsIcon />
                    {agentToolsOn
                      ? t("chatView.composer.agentToolsOnLabel")
                      : t("chatView.composer.agentToolsLabel")}
                  </button>
                )}
              </div>
            )}
          </div>
          <div className="composer-input-wrap" ref={skillSuggestRef}>
            {skillSuggestOpen && (
              <div className="menu skill-suggest" role="listbox">
                {skillSuggestions.map((s, idx) => (
                  <button
                    key={s.slug}
                    type="button"
                    role="option"
                    aria-selected={idx === skillHighlight}
                    className={`menu-item${idx === skillHighlight ? " is-on" : ""}`}
                    onClick={() => {
                      setDraft(`/${s.slug} `);
                      textareaRef.current?.focus();
                    }}
                  >
                    <span className="skill-suggest-name">/{s.slug}</span>
                    {s.description && (
                      <span className="skill-suggest-desc">{s.description}</span>
                    )}
                  </button>
                ))}
              </div>
            )}
            <textarea
              ref={textareaRef}
              aria-label={t("chatView.composer.textareaLabel")}
              rows={1}
              value={draft}
              disabled={locked}
              readOnly={improving}
              aria-busy={improving || undefined}
              className={improving ? "is-improving" : undefined}
              placeholder={placeholder}
              onChange={(e) => {
                setDraft(e.target.value);
                setPreviousDraft(null);
                setImproveStatus(null);
              }}
              onPaste={onPaste}
              onKeyDown={(e) => {
                if (
                  e.key === "Escape" &&
                  previousDraft !== null &&
                  openTool === null &&
                  !skillSuggestOpen
                ) {
                  e.preventDefault();
                  undoImprove();
                  return;
                }
                if (skillSuggestOpen) {
                  if (e.key === "ArrowDown") {
                    e.preventDefault();
                    setSkillHighlight((h) => (h + 1) % skillSuggestions.length);
                    return;
                  }
                  if (e.key === "ArrowUp") {
                    e.preventDefault();
                    setSkillHighlight(
                      (h) => (h - 1 + skillSuggestions.length) % skillSuggestions.length,
                    );
                    return;
                  }
                  if ((e.key === "Tab" || e.key === "Enter") && !e.nativeEvent.isComposing) {
                    e.preventDefault();
                    setDraft(`/${skillSuggestions[skillHighlight].slug} `);
                    return;
                  }
                }
                // Skip Enter while an IME is composing (WebKit reports the
                // confirming keystroke with isComposing=false but keyCode=229).
                if (
                  e.key === "Enter" &&
                  !e.shiftKey &&
                  !e.nativeEvent.isComposing &&
                  e.keyCode !== 229
                ) {
                  e.preventDefault();
                  submit();
                }
              }}
            />
          </div>
          <ModelQuickPicker openTool={openTool} setOpenTool={setOpenTool} />
          {stt && (
            <button
              type="button"
              className={`composer-tool${dictating ? " is-recording" : ""}`}
              onClick={() => void dictate()}
              disabled={!sttReady || improving}
              aria-pressed={dictating}
              title={
                sttReady
                  ? dictating
                    ? t("chatView.composer.micListening")
                    : t("chatView.composer.micTypeByTalking")
                  : t("chatView.composer.micTurnOnVoice")
              }
            >
              <MicIcon />
            </button>
          )}
          {improverOn && (
            <button
              type="button"
              className={`composer-tool${improving ? " is-improving" : ""}`}
              onClick={() => void improve()}
              disabled={blocker !== null}
              aria-label={
                blocker ? t(improveBlockerKey(blocker)) : t("chatView.composer.improve.title")
              }
              title={
                blocker ? t(improveBlockerKey(blocker)) : t("chatView.composer.improve.title")
              }
            >
              <SparkleIcon />
            </button>
          )}
          {streaming || improving ? (
            <button
              className="composer-send composer-stop"
              onClick={() => void stopGeneration()}
              aria-label={t(
                improving ? "chatView.composer.improve.stopTitle" : "chatView.composer.stop",
              )}
              title={t(
                improving ? "chatView.composer.improve.stopTitle" : "chatView.composer.stop",
              )}
            >
              ■
            </button>
          ) : (
            <button
              className="composer-send"
              onClick={submit}
              disabled={!canSend}
              aria-label={t("chatView.composer.send")}
              title={t("chatView.composer.send")}
            >
              ↑
            </button>
          )}
        </div>
      </div>
      <p
        className="composer-hint"
        style={{ visibility: hint || improveStatus ? "visible" : "hidden" }}
      >
        {improveStatus ? (
          <>
            <span
              role={improveStatus.alert ? "alert" : "status"}
              className="composer-hint-status"
            >
              {improving && <span className="searching-spinner" aria-hidden="true" />}
              {improveStatus.raw ?? t(improveStatus.key)}
            </span>
            {previousDraft !== null && !improving && (
              <button
                type="button"
                className="composer-hint-action"
                onClick={undoImprove}
                aria-label={t("chatView.composer.improve.undoLabel")}
              >
                {t("chatView.composer.improve.undo")}
              </button>
            )}
          </>
        ) : (
          hint || " "
        )}
      </p>
    </div>
  );
}
