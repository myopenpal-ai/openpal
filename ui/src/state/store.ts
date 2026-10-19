import { create, type StateCreator } from "zustand";
import i18n from "../i18n";
import { api } from "../lib/api";
import {
  type AppEvent,
  alertCopy,
  decideDelivery,
  hasNativeNotifications,
} from "../lib/appEvents";
import { createEngineTracker, type EngineAlertEvent, stepEngineTracker } from "../lib/engineEvents";
import { engineModelName } from "../lib/format";
import { applyTheme, normalizeTheme } from "../lib/theme";
import {
  branchAnchorKey,
  type BranchPoint,
  DEFAULT_CONTEXT_LENGTH,
  DEFAULT_SAMPLING,
  ROOT_BRANCH_KEY,
  type Account,
  type AppInfo,
  type ArtifactRef,
  type Attachment,
  type AvailableUpdate,
  type ChatMessage,
  type ChatStats,
  type ChatStep,
  type Citation,
  type CloudModelInfo,
  type ConsentDecision,
  type Conversation,
  type ConversationMeta,
  type ConversationSearchResult,
  type DangerFlag,
  type EngineStatus,
  type EngineUpdateInfo,
  type ModelInfo,
  type NotificationClick,
  type NotificationPermission,
  type PluginInfo,
  type Project,
  recommendedSampling,
  type ResolvedPath,
  type ServiceState,
  type Settings,
  type SetupState,
  type Skill,
  type SystemReport,
  type Theme,
  type ToolCallDecision,
} from "../lib/types";

/** Sidebar column width bounds, in px — narrow enough that "New chat" and
 * conversation titles stay legible, wide enough that a resize can't crowd
 * out the main pane on the app's 900px minimum window width. */
export const SIDEBAR_MIN_WIDTH = 220;
export const SIDEBAR_MAX_WIDTH = 440;
export const SIDEBAR_DEFAULT_WIDTH = 272;

/** The four things the user is told about with an in-app alert (and, with
 * "Desktop notifications" on and OpenPal in the background, an OS
 * notification carrying the same words). */
export type AlertKind = "modelLoaded" | "modelUnloaded" | "downloadFinished" | "answerReady";

/** One in-app alert. `title` and `body` are already translated: the native
 * notification for the same event is built from these same strings. `id` is
 * the React key and what `dismissAlert` takes. */
export interface AppAlert {
  id: string;
  kind: AlertKind;
  title: string;
  body: string;
}

/** How many alerts show at once; a new one beyond this pushes the oldest out. */
export const MAX_VISIBLE_ALERTS = 3;

export interface DownloadProgress {
  receivedBytes: number;
  totalBytes: number;
  bytesPerSec: number;
  etaSeconds: number;
}

/** An agent-mode tool call awaiting the user's Allow once / Always allow /
 * Deny decision — the live `toolConsentRequested` event, held in state so
 * `ToolConsentCard` can render it. */
export interface PendingToolConsent {
  requestId: string;
  toolCallId: string;
  toolId: string;
  summary: string;
  detail: string;
  resolvedPaths: ResolvedPath[];
  dangerFlags: DangerFlag[];
  /** The subagent kind that made this call, if any — see `ChatEvent.toolConsentRequested`. */
  via: string | null;
}

/** What the user just decided for a pending consent prompt, kept around
 * just long enough for the matching `toolCallStarted`/`toolCallDenied`
 * event (which doesn't repeat the decision itself) to build this turn's
 * `ChatStep.toolCall` entry. */
interface RespondedToolConsent {
  toolCallId: string;
  toolId: string;
  summary: string;
  decision: ConsentDecision;
}

/** An `ask_user` call awaiting the user's answer — the live
 * `clarifyingQuestionAsked` event, held in state so `AskUserCard` can render
 * it. Unlike `PendingToolConsent`, this never gates anything: `ask_user` has
 * no blast radius, so answering (or not) never blocks other actions. */
export interface PendingClarifyingQuestion {
  requestId: string;
  toolCallId: string;
  question: string;
  options: string[];
}

/** What the user just answered for a pending clarifying question, kept
 * around just long enough for the matching `toolCallResult` event (which
 * doesn't repeat the answer) to build this turn's `ChatStep.askedQuestion`
 * entry. */
interface RespondedClarifyingQuestion {
  toolCallId: string;
  answer: string;
}

/** What a streaming turn is doing right now, before (or between) its answer
 * text — kept structured rather than as display text so `ChatView` can
 * localize it. */
export type LiveActivity =
  /** Web search provisioning its local backend on first use; `message` is
   * the backend's own progress text. */
  | { kind: "setup"; message: string }
  | { kind: "searching"; query: string }
  | { kind: "readingResults" }
  | { kind: "reading"; host: string }
  | { kind: "tool"; toolId: string }
  /** A `spawn_subagent` helper is running; `helper` is its kind. */
  | { kind: "subagent"; helper: string };

/** What the main pane shows. Deliberately only the surfaces where the user
 * talks to their model — everything else opens over it in a dialog instead
 * (see `MoreDialogPage` and `conversationSearchOpen`), so closing one always
 * lands back on an untouched chat. */
export type View = "chat" | "translate" | "correct" | "improve";

/** Settings' categories, in the order the More dialog's left nav lists them
 * under its Settings heading. Each is a page of that dialog in its own right
 * (views/SettingsSheet.tsx renders whichever is showing). */
export const SETTINGS_SECTIONS = [
  "general",
  "chat",
  "model",
  "privacy",
  "devices",
  "data",
  "advanced",
] as const;
export type SettingsSection = (typeof SETTINGS_SECTIONS)[number];

/** The pages of the More dialog (views/MoreDialog.tsx) — what the sidebar's
 * More menu leads to, with Settings split into its categories. */
export type MoreDialogPage = "library" | "discover" | "plugins" | "skills" | SettingsSection;

export function isSettingsSection(page: MoreDialogPage): page is SettingsSection {
  return (SETTINGS_SECTIONS as readonly string[]).includes(page);
}

/** "downloading" covers both download and install — `install_update` does
 * both in one call, ending in either the app restarting (this frontend
 * instance disappears) or an "error". There's no separate "ready to
 * install" step. */
export type UpdateStatus = "idle" | "checking" | "available" | "downloading" | "error";

/** Like `UpdateStatus`, but "downloading" always resolves back to "idle" (or
 * "installed") on its own — an engine update never restarts the app, so
 * there's no "the frontend instance disappears" terminal case here. */
export type EngineUpdateStatus =
  | "idle"
  | "checking"
  | "available"
  | "downloading"
  | "installed"
  | "error";

interface Store {
  booted: boolean;
  settings: Settings | null;
  report: SystemReport | null;
  /** Portable-build flag + resolved data dir (from get_app_info). */
  appInfo: AppInfo | null;
  /** Session-only: whether the user has already passed `AppLockGate` this
   * launch. `AppInfo.needsAppUnlock` itself never durably clears (see its
   * own doc comment), so this is the frontend's own record of "already
   * unlocked this session" — checked alongside it in `App.tsx`'s gate. */
  appLockUnlocked: boolean;
  /** One-shot flag set when `AppLockGate`'s recovery-code stage turns the
   * PIN off and opens Settings — `SecuritySection` reads it once to show an
   * explanatory banner, then clears it. */
  appLockJustReset: boolean;
  /** The signed-in account, if any (local cache; kept fresh by `refreshAccount`). */
  account: Account | null;
  /** Whether the sign-in dialog is open. */
  signInOpen: boolean;
  /** Set when a sign-in attempt fails or expires; cleared on a fresh attempt. */
  signInError: string | null;

  // Multi-device sync — pairing (docs/SYNC_PAIRING_PROTOCOL.md, Phase 2).
  // Device list and group-secret state live locally in the Settings "Sync"
  // section instead (simple request/response, no streaming involved); only
  // the two multi-step *streamed* pairing flows below need store state, same
  // reasoning as `signInOpen`/`signInError` above.

  /** Device A ("Add a device"): whether that dialog is open. */
  addDeviceOpen: boolean;
  /** "listening" while `sync_pair_listen` waits for a request; "incoming"
   * once one's showing on the confirm card. */
  addDeviceStatus: "listening" | "incoming" | "expired" | "error";
  /** The card `sync_pair_listen`'s `incomingRequest` event showed, once one
   * arrives. `fingerprint` must render as the visually dominant element of
   * that card — see `SyncPairingEvent`'s doc comment in lib/types.ts. */
  addDeviceIncoming: { deviceLabel: string; fingerprint: string } | null;
  /** True while `sync_pair_confirm` is in flight, so Confirm/Reject can't double-fire. */
  addDeviceBusy: boolean;
  addDeviceError: string | null;

  /** Device B ("Join a group"): whether that dialog is open. */
  joinGroupOpen: boolean;
  /** Named after the `SyncPairingEvent` variant each status corresponds to;
   * "form" is the initial paste-secret/type-label step before
   * `sync_pair_request` is even called. */
  joinGroupStatus:
    | "form"
    | "awaitingChallenge"
    | "awaitingConfirmation"
    | "readyToInstall"
    | "expired"
    | "error";
  /** The fingerprint to compare against Device A's screen, set once
   * `sync_pair_request` reaches `awaitingConfirmation`. Same "must be the
   * dominant element" requirement as `addDeviceIncoming.fingerprint`. */
  joinGroupFingerprint: string | null;
  /** True while `sync_pair_finish` is in flight. */
  joinGroupBusy: boolean;
  joinGroupError: string | null;

  models: ModelInfo[];
  engine: EngineStatus;
  /** The context length the running/starting engine was actually launched
   * with (set by `useModel`) — null before any successful start this
   * session. Compared against the active conversation's `contextLength` to
   * show "restart to apply" only on a genuine mismatch, not on every load. */
  runningContextLength: number | null;
  /** True while a start/stop engine call is in flight. */
  engineBusy: boolean;
  /** True right after `stopModel` (the Sidebar's explicit "Unload" button) —
   * consumed and cleared by the composer's typing-triggered preload effect
   * the next time it sees it, so a deliberate unload isn't silently undone by
   * continuing to type, but a later, fresh burst of typing preloads normally
   * again. Also cleared by any `useModel` call (a fresh load supersedes it). */
  userStoppedModel: boolean;
  view: View;
  /** Set by `startChatWithDraft` (the standalone "Improve a message"
   * screen's "Start a new chat with this" button) and consumed once by the
   * Composer on mount, then cleared — a one-shot hand-off since the
   * composer's own draft is local component state, not store state. */
  draftToPrefill: string | null;
  /** Which page of the More dialog is showing, or null while it's closed.
   * The main pane underneath keeps whatever `view` it had — the dialog never
   * changes it, which is what keeps an unsent composer draft alive across an
   * open/close round trip (ChatView simply stays mounted). */
  moreDialogPage: MoreDialogPage | null;
  /** Whether the conversation search dialog (views/ConversationSearch.tsx)
   * is open — its own dialog, separate from the More dialog. */
  conversationSearchOpen: boolean;
  /** The sidebar-as-drawer's open/closed state below the mobile breakpoint
   * (see ui/src/styles/chat.css's `.sidebar-open` rule) — irrelevant above
   * it, where the sidebar is always a visible grid column. */
  mobileSidebarOpen: boolean;
  /** Above the mobile breakpoint only: whether the user has manually hidden
   * the sidebar column (see App.tsx's `.shell-sidebar-collapsed` class).
   * Session-only, like the other view-chrome toggles here — resets to
   * visible on the next launch rather than persisting a hidden sidebar. */
  sidebarCollapsed: boolean;
  /** Whether the sidebar footer's "More" menu is open. Lives here rather
   * than in Sidebar's own state because AutoUnloadTooltip needs it too: that
   * popover is anchored over the model row the menu opens across, and no
   * z-index can order the two (`.sidebar` sets `backdrop-filter`, so the
   * whole column paints as one unit beneath the popover's fixed sibling), so
   * the passive countdown hides while the menu a user deliberately opened is
   * up. Session-only, like the toggles around it. */
  sidebarMoreMenuOpen: boolean;
  /** The sidebar column's width in px, user-adjustable via the drag handle
   * on its trailing edge (see Sidebar.tsx) and clamped to
   * [SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH] by setSidebarWidth. Session-only,
   * like sidebarCollapsed above. Irrelevant while collapsed or below the
   * mobile breakpoint, where the sidebar isn't a resizable grid column. */
  sidebarWidth: number;

  /** Discovered plugins and their live setup/service state. */
  plugins: PluginInfo[];

  /** The cloud model catalog (see `loadCloudModels`). `null` until the first
   * load resolves — Library and the Generation panel both read this, so it's
   * fetched once here rather than each re-fetching its own copy. */
  cloudModels: CloudModelInfo[] | null;
  cloudModelsError: string | null;

  conversations: ConversationMeta[];
  /** User-named groups of conversations, shown in the sidebar — see
   * `Project`. Loaded lazily after first paint, same as `skills`. */
  projects: Project[];
  /** The sidebar search box's current text — kept in state (rather than
   * local component state) so it can be cleared from `newChat`/
   * `openConversation` too. */
  sidebarSearchQuery: string;
  /** Debounced results for `sidebarSearchQuery` (see `setSidebarSearchQuery`) —
   * empty whenever the query is empty, not "no results yet" vs "no matches". */
  sidebarSearchResults: ConversationSearchResult[];
  /** Saved `/<slug>` skills — instruction snippets folded into the outgoing
   * message when a chat starts with a matching trigger (see `send`). */
  skills: Skill[];
  active: Conversation | null;
  streaming: boolean;
  /** Set by `openConversationAtMessage` right after switching `active` — the
   * id of the message `ChatView` should scroll to and briefly highlight once
   * it's rendered. `ChatView` clears this itself once the jump has run, and
   * `resetPerConversationUiState` clears it on every other conversation
   * switch so a stale target never lingers. */
  scrollToMessageId: string | null;
  /** While a turn is gathering material (searching, reading a page, running
   * a tool): what it's doing right now, shown — localized — as the live
   * header of the reply's activity panel. Null at all other times, including
   * once the answer starts streaming. */
  liveActivity: LiveActivity | null;
  chatError: string | null;
  /** Per conversation: whether the safety classifier (`api.classifyRisk`,
   * run in `send` before the reply starts generating — see its own comment
   * for why it has to happen there and not after) has ever flagged one of
   * that conversation's user messages as expressing self-harm/crisis risk,
   * and whether the person has dismissed that specific flagged turn.
   * `turn` is the flagged message's index, so a later flagged message in
   * the same conversation gets a new `turn` and re-shows the banner even if
   * an earlier one was dismissed (`dismissedTurn !== turn`). Keyed by
   * conversationId — not reset by `newChat`/`openConversation` — so
   * switching conversations shows each conversation's own banner state:
   * revisiting a flagged conversation shows its banner again, an unrelated
   * one shows nothing. Lives here rather than as local component state in
   * `SafetyBanner` so it survives that component unmounting (e.g.
   * navigating to another view and back to Chat) without losing a
   * dismissal. `deleteConversation` prunes a conversation's entry. Never
   * cleared by a later "safe" classification. */
  safetyAlerts: Record<string, { turn: number; dismissedTurn: number | null }>;
  /** Whether web search is active for the current conversation. Session-only:
   * it resets to off on boot, a new chat, and switching/reopening a
   * conversation. Only takes effect while `webSearchAvailable`; the backend
   * enforces that global gate too. See `send`. */
  webSearch: boolean;
  /** Whether agent tools are active for the current conversation. Session-
   * only, same shape as `webSearch`: on by default when agent mode is
   * globally enabled (Settings) and a workspace folder is configured;
   * resets on a new chat and switching/reopening a conversation. */
  agentTools: boolean;
  /** The agent-mode tool call currently awaiting the user's decision, or
   * null when none is pending. At most one at a time — the orchestrator
   * dispatches tool calls within a turn one at a time. */
  pendingToolConsent: PendingToolConsent | null;
  /** Not rendered directly — bridges the decision made in `respondToolConsent`
   * to the later `toolCallStarted`/`toolCallDenied` event so `send()` can
   * build an accurate `ChatStep.toolCall` entry (those events don't repeat
   * which button was clicked). */
  lastRespondedToolConsent: RespondedToolConsent | null;
  /** The `ask_user` call currently awaiting the user's answer, or null when
   * none is pending. At most one at a time, same reasoning as
   * `pendingToolConsent`. */
  pendingClarifyingQuestion: PendingClarifyingQuestion | null;
  /** Bridges `respondClarifyingQuestion` to the later `toolCallResult` event —
   * see `RespondedToolConsent`'s doc comment for why this exists. */
  lastRespondedClarifyingQuestion: RespondedClarifyingQuestion | null;
  /** A short, friendly note shown as a passing toast (e.g. a failed action).
   * Nothing renders it today, and the in-app alerts below deliberately don't
   * use it: it is written by about thirty failure paths with raw error text. */
  notice: string | null;
  /** In-app alerts on screen now, newest first — rendered by
   * `components/AlertHost.tsx`, filled by the helpers in `lib/appEvents.ts`.
   * Capped at `MAX_VISIBLE_ALERTS`. */
  appAlerts: AppAlert[];
  showAlert: (alert: Omit<AppAlert, "id">) => void;
  /** A no-op for an id that is already gone (auto-dismiss and a click can race). */
  dismissAlert: (id: string) => void;
  /** Whether OpenPal's window has the OS's focus — the rule for sending a
   * native notification. Kept in step with the window's focus/blur events by
   * `App.tsx`. */
  windowFocused: boolean;
  setWindowFocused: (focused: boolean) => void;
  /** What the OS says about OpenPal posting notifications; only "denied" ever
   * shows anything (Settings' "blocked" line). Starts "granted" until asked. */
  notificationPermission: NotificationPermission;
  /** Reads the OS's answer without asking anything. */
  refreshNotificationPermission: () => Promise<void>;
  /** Asks the OS (on macOS: shows its one-time prompt, resolving once it is
   * answered) and keeps the answer. Only `setDesktopNotifications(true)` calls
   * it, so the prompt follows the user's own click. (Rust also asks once, on
   * the first post, for a switch that was already on before anything asked.) */
  requestNotificationPermission: () => Promise<void>;
  /** Acts on a click on one of OpenPal's notifications: takes it from Rust and,
   * if it names a conversation, opens that conversation in the chat with the
   * More dialog and conversation search closed. Does nothing until settings
   * have loaded (the click waits in Rust), so `finishBootAfterUnlock` calls it
   * once for a click that launched the app, and every `notification-clicked`
   * nudge after that calls it again. Never throws. */
  followNotificationClick: () => Promise<void>;

  /** modelId -> live progress for in-flight downloads. */
  downloads: Record<string, DownloadProgress>;
  /** modelId -> friendly error from the last failed download. */
  downloadErrors: Record<string, string>;

  /** App-update check/install state — see `checkForUpdate`/`startUpdateInstall`. */
  updateStatus: UpdateStatus;
  availableUpdate: AvailableUpdate | null;
  updateProgress: DownloadProgress | null;
  updateError: string | null;

  /** AI engine update check/install state — see `checkForEngineUpdate`/
   * `startEngineUpdateInstall`. Independent of `updateStatus` above. */
  engineUpdateStatus: EngineUpdateStatus;
  availableEngineUpdate: EngineUpdateInfo | null;
  engineUpdateProgress: DownloadProgress | null;
  engineUpdateError: string | null;

  boot: () => Promise<void>;
  finishBootAfterUnlock: () => Promise<void>;
  setView: (view: View) => void;
  /** Opens the More dialog on `page`, switches the page it shows, or closes
   * it with `null`. */
  setMoreDialogPage: (page: MoreDialogPage | null) => void;
  /** Closes the More dialog onto the chat (Library's "Go to chat"). */
  goToChat: () => void;
  setConversationSearchOpen: (open: boolean) => void;
  setMobileSidebarOpen: (open: boolean) => void;
  setSidebarCollapsed: (collapsed: boolean) => void;
  setSidebarMoreMenuOpen: (open: boolean) => void;
  setSidebarWidth: (width: number) => void;
  setNotice: (notice: string | null) => void;

  refreshModels: () => Promise<void>;
  startDownload: (modelId: string) => void;
  cancelDownload: (modelId: string) => Promise<void>;
  deleteModel: (modelId: string) => Promise<void>;
  /** Star/unstar a model as a favorite, from the Library page or the
   * composer's model quick-switcher (see `FavoriteButton`). */
  setModelFavorite: (modelId: string, favorite: boolean) => Promise<void>;
  /** (Re)start the engine with this model. `contextLength` defaults to the
   * active conversation's own setting (or the app default when none is
   * active) — pass it explicitly only to apply a value that hasn't been
   * committed to `active` yet (see `updateGeneration`). */
  useModel: (modelId: string, contextLength?: number) => Promise<void>;
  /** Unload the running model and stop its llama.cpp process. */
  stopModel: () => Promise<void>;
  /** Reset the auto-unload idle clock to now, so the countdown badge in the
   * sidebar starts over. Backs the pre-unload warning banner's "Keep it
   * loaded" button, and the composer's typing-pauses-the-countdown effect
   * (see ChatView's `Composer`). */
  snoozeAutoUnload: () => Promise<void>;
  /** Switch to a signed-in account's cloud model for chatting — the cloud
   * analogue of `useModel`. No engine/subprocess involved, just a settings
   * patch (`require_entitlement` on the Rust side is the real gate). */
  useCloudModel: (modelId: string) => Promise<void>;
  /** Fetch the cloud model catalog into `cloudModels` — the list is public
   * (shown signed-out too), so this never needs an account first. */
  loadCloudModels: () => Promise<void>;

  pollEngine: () => Promise<void>;

  loadPlugins: () => Promise<void>;
  setPluginEnabled: (id: string, enabled: boolean) => Promise<void>;
  /** Toggle web search for the current conversation (the composer globe).
   * Session-only — see `webSearch`. */
  setWebSearch: (on: boolean) => void;
  /** Toggle agent tools for the current conversation. Session-only — see
   * `agentTools`. */
  setAgentTools: (on: boolean) => void;
  /** Reply to the pending `toolConsentRequested` prompt (Allow once / Always
   * allow / Deny, with an optional note on deny). A no-op if nothing is
   * pending. */
  respondToolConsent: (decision: ConsentDecision, note?: string) => Promise<void>;
  /** Reply to the pending `clarifyingQuestionAsked` prompt with a quick-pick
   * or typed answer (empty string to skip/dismiss). A no-op if nothing is
   * pending. */
  respondClarifyingQuestion: (answer: string) => Promise<void>;

  newChat: () => void;
  /** Start a fresh conversation with `text` pre-filled in the composer
   * draft (not sent — the user reviews/edits before pressing Send). Used by
   * the standalone "Improve a message" screen's "Start a new chat with
   * this" button. A no-op while a reply is streaming, same as `newChat`
   * (which this calls). */
  startChatWithDraft: (text: string) => void;
  openConversation: (id: string) => Promise<void>;
  /** Opens `conversationId` (same as `openConversation`) and then requests a
   * scroll-to + highlight of `messageId` once it's rendered — used by the
   * fork-origin hint's "Jump to original message" link. Sets
   * `scrollToMessageId`, which `ChatView` watches and clears itself once the
   * jump has happened. */
  openConversationAtMessage: (conversationId: string, messageId: string) => Promise<void>;
  /** Clears `scrollToMessageId` once `ChatView` has performed the jump it
   * requested — a no-op if there's nothing pending. */
  clearScrollToMessage: () => void;
  deleteConversation: (id: string) => Promise<void>;
  /** Update the sidebar search box's text at once (for the input's own
   * value) and, debounced, run the actual search into `sidebarSearchResults`.
   * An empty (post-trim) query clears results immediately rather than
   * round-tripping to the backend for nothing. */
  setSidebarSearchQuery: (query: string) => void;
  loadProjects: () => Promise<void>;
  /** Create a new project with `name` and return it, or null on failure. */
  createProject: (name: string) => Promise<Project | null>;
  renameProject: (id: string, name: string) => Promise<void>;
  /** Deletes the project; conversations that were in it are kept, just
   * ungrouped (mirrors the backend — see `commands::delete_project`). */
  deleteProject: (id: string) => Promise<void>;
  /** Deletes the project AND every conversation still in it. Deletes the
   * conversations first (each through `deleteConversation`, so they get its
   * same safetyAlerts/active/search-result cleanup) and only then the now-
   * empty project, mirroring the order the sidebar's delete-project dialog
   * needs: nothing should call `deleteProject` (which just detaches) while a
   * conversation the user asked to delete along with it still exists. */
  deleteProjectWithConversations: (id: string) => Promise<void>;
  /** Move a conversation into `projectId`, or out of whatever project it's
   * in when `projectId` is null. */
  moveConversationToProject: (conversationId: string, projectId: string | null) => Promise<void>;
  /** Rename a conversation from the sidebar's row menu. */
  renameConversation: (conversationId: string, title: string) => Promise<void>;
  /** Pin/unpin a conversation from the sidebar's row menu. Device-local —
   * not synced across paired devices (see `api.setConversationPinned`). */
  setConversationPinned: (conversationId: string, pinned: boolean) => Promise<void>;
  loadSkills: () => Promise<void>;
  /** Create (empty `slug`) or update (existing `slug`) a skill. Returns the
   * saved skill (with its slug filled in for a new one), or null on failure —
   * the caller (SkillsView) uses this to decide whether to close its editor. */
  saveSkill: (skill: Skill) => Promise<Skill | null>;
  deleteSkill: (slug: string) => Promise<void>;
  /** Opens a native picker for a `.md` file and imports it as a new skill.
   * Resolves to null only if the picker was closed without a choice; throws
   * (with a message safe to show the user) if the import itself fails —
   * callers should catch and display that inline, see `SkillsView`. */
  importSkillFile: () => Promise<Skill | null>;
  /** Same, for a folder containing a `SKILL.md` plus any supporting Markdown
   * files. */
  importSkillFolder: () => Promise<Skill | null>;
  /** Same, for a `.zip` archive of a skill folder. */
  importSkillZip: () => Promise<Skill | null>;
  /** Saves `slug` to a location the user chooses via the native save dialog
   * — the export half of the encrypted-at-rest era's edit round trip (see
   * `importSkillZip`). Resolves to `true` if a file was written, `false` if
   * the dialog was closed without a choice. */
  exportSkill: (slug: string) => Promise<boolean>;
  /** Update the active conversation's generation settings (sampling, context
   * length, and/or request-triage task type), from the composer's Generation
   * panel or the silent `useTriageAutopilot` hook. Applies at once in memory
   * — used by the very next message — and, for a conversation that already
   * has a reply (so it exists on disk), persists shortly after (debounced,
   * so dragging a slider doesn't hammer disk I/O). A brand-new, never-sent
   * conversation persists it naturally with its first reply (see `send`).
   * Context length changing alone never restarts a running engine — that's
   * an explicit `useModel` call from the panel's own button; a task-type
   * change likewise never switches models on its own — see
   * `useTriageAutopilot`'s own model auto-pick rules. */
  updateGeneration: (
    patch: Partial<Pick<Conversation, "sampling" | "contextLength" | "taskType">>,
  ) => void;
  /** Returns the settled conversation once the turn finishes (or
   * `undefined` if it never started — already streaming, or nothing to
   * send). Callers that need to act on exactly this turn's own result
   * (e.g. `editMessage`'s follow-up branch bookkeeping) should use this
   * return value rather than re-reading `get().active`, which could by
   * then reflect a later, unrelated action. */
  send: (text: string, attachments?: Attachment[]) => Promise<Conversation | undefined>;
  editMessage: (index: number, text: string) => Promise<void>;
  /** Regenerates the assistant reply at `index` (must be an assistant
   * message) — drops it and everything after, re-answers the same,
   * unchanged preceding user turn. The old reply is kept as a navigable
   * sibling (see `Conversation.branches`), not discarded. */
  retryMessage: (index: number) => Promise<void>;
  /** Creates a new, separate conversation containing everything up to and
   * including the assistant message at `index`, and switches to it. Does
   * not carry over the source conversation's edit/retry history. */
  forkConversation: (index: number) => Promise<void>;
  /** Switches which sibling is currently spliced into `messages` at the
   * branch point keyed by `anchorKey` (see `branchAnchorKey`) — used by
   * the edit-history/retry-history arrow navigation. */
  setActiveVariant: (anchorKey: string, variantIndex: number) => Promise<void>;
  stopGeneration: () => Promise<void>;
  /** Marks `safetyAlerts[conversationId]`'s current turn as dismissed —
   * see that field's doc comment. No-op if there's no entry for it. */
  dismissSafetyAlert: (conversationId: string) => void;

  completeOnboarding: () => Promise<void>;
  saveSettings: (patch: Partial<Settings>) => Promise<void>;
  /** Toggle "Show model thinking". Hides thinking from new replies at once;
   * the engine stops producing it on its next load. Never restarts a running
   * model, so an in-flight reply is safe. */
  setShowThinking: (on: boolean) => Promise<void>;
  /** Choose the app's theme (Settings → General, the onboarding step). Unlike
   * `saveSettings` it is optimistic: the new theme is in the store — and, via
   * the subscriber at the bottom of this file, on `<html>` — before anything
   * is persisted, so the pick shows in the click's own frame and a control
   * driven by `settings.theme` never snaps back while the write is in flight.
   * It also never assigns a snapshot after the write, so two quick picks can't
   * flicker back to the first when the replies arrive out of order. */
  setTheme: (theme: Theme) => Promise<void>;
  /** The Settings → General switch and the onboarding "Turn on notifications"
   * button. Optimistic and rolled back on a failed write exactly like
   * `setTheme`, for the same reasons; turning it on then asks the OS for
   * permission (macOS shows its prompt) so the "blocked" line can show, unless
   * it was switched off again while the write was in flight. */
  setDesktopNotifications: (on: boolean) => Promise<void>;

  /** Check for a newer version. `manual` (the Settings "Check for updates
   * now" button) always shows the dialog and surfaces a failure; a silent
   * auto-check (on boot) skips a version the user dismissed with "Skip this
   * version" and never nags with an error dialog of its own. */
  checkForUpdate: (manual?: boolean) => Promise<void>;
  /** Download and install the update `checkForUpdate` found, then relaunch.
   * On success the app restarts/exits on its own — this frontend instance
   * is about to disappear, so there's nothing further to do here. */
  startUpdateInstall: () => Promise<void>;
  /** "Skip this version": won't reopen automatically for it again. */
  skipUpdateVersion: () => Promise<void>;
  /** "Later": close the dialog without persisting anything. */
  dismissUpdate: () => void;

  /** Check for a newer AI engine build, on its own channel independent of
   * `checkForUpdate`. Same `manual` semantics. */
  checkForEngineUpdate: (manual?: boolean) => Promise<void>;
  /** Download, verify, and stage the engine build `checkForEngineUpdate`
   * found. Never restarts the app or the running AI — the new build takes
   * effect the next time the AI engine starts. */
  startEngineUpdateInstall: () => Promise<void>;
  /** "Skip this version" for an engine tag. */
  skipEngineUpdateTag: () => Promise<void>;
  /** "Later" for the engine-update nudge. */
  dismissEngineUpdate: () => void;

  /** Silently refresh the cached account from a stored sign-in session, if
   * any. Called a few seconds after boot, same fire-and-forget shape as
   * `checkForUpdate`'s auto-check — never surfaces an error of its own. */
  refreshAccount: () => Promise<void>;
  /** Open the sign-in dialog and start a device-session sign-in; opens the
   * verification page in the system browser once the backend is ready. */
  signIn: () => Promise<void>;
  /** Cancel an in-flight sign-in and close the dialog. */
  cancelSignIn: () => Promise<void>;
  signOut: () => Promise<void>;

  /** Device A: open the "Add a device" dialog and start `sync_pair_listen`. */
  startAddDevice: () => Promise<void>;
  /** Device A: the user tapped Confirm/Reject on the card `startAddDevice` showed. */
  respondAddDevice: (approve: boolean) => Promise<void>;
  /** Cancel (`sync_pair_cancel`) whatever "Add a device" session is in
   * flight, if any, and close the dialog. */
  closeAddDevice: () => Promise<void>;

  /** Device B: open the "Join a group" dialog on its initial form step
   * (does not call `sync_pair_request` yet). Also used to reset back to
   * that step for a retry after `expired`/`error`. */
  openJoinGroup: () => void;
  /** Device B: submit the join form and start `sync_pair_request`'s streamed flow. */
  submitJoinGroup: (groupSecret: string, deviceLabel: string) => Promise<void>;
  /** Device B: the user's second, explicit "codes match — finish joining?"
   * confirmation (or rejection) once `readyToInstall` fired. */
  finishJoinGroup: (approve: boolean) => Promise<void>;
  /** Cancel (`sync_pair_cancel`) whatever "Join a group" session is in
   * flight, if any, and close the dialog. */
  closeJoinGroup: () => Promise<void>;
}

function newConversation(): Conversation {
  const now = Date.now();
  return {
    id: crypto.randomUUID(),
    title: "New chat",
    createdAtMs: now,
    updatedAtMs: now,
    messages: [],
    sampling: DEFAULT_SAMPLING,
    contextLength: DEFAULT_CONTEXT_LENGTH,
    taskType: "general",
    pinned: false,
    branches: {},
  };
}

/** Pushes `tail` as a new sibling variant onto `branches[anchorKey]`
 * (creating the branch point on first touch) and marks it active. Shared
 * by `editMessage`/`retryMessage` (recording the tail about to be
 * discarded, and later the new tail that replaced it) — see
 * `Conversation.branches`'s doc comment in `lib/types.ts`. */
function pushVariant(
  branches: Record<string, BranchPoint>,
  anchorKey: string,
  tail: ChatMessage[],
): Record<string, BranchPoint> {
  const existing = branches[anchorKey];
  const variants = existing ? [...existing.variants, tail] : [tail];
  return { ...branches, [anchorKey]: { variants, active: variants.length - 1 } };
}

type SetState = Parameters<StateCreator<Store>>[0];
type GetState = Parameters<StateCreator<Store>>[1];

// ---------------------------------------------------------------------------
// App alerts: the four events (model loaded, model unloaded, download
// finished, answer ready) are announced from here. The rules for *what* is
// said and *where* it goes live in `lib/appEvents.ts` and
// `lib/engineEvents.ts`; this is the state they need and the one function that
// acts on them.
// ---------------------------------------------------------------------------

/** Follows every status written to `engine` (see the subscriber at the bottom
 * of this file) to find the loads and unloads in it. Module-level because it
 * must outlive any one `set()`; `resetAlertTracking` clears it for tests. */
const engineTracker = createEngineTracker();

/** Display names of the models seen Running or Starting, by id. A model can
 * drop out of `models` just before its unload is noticed — `deleteModel`
 * refreshes the list (no longer holding a deleted custom model) before it
 * re-reads the engine status — and the alert must still be able to name it. */
const engineNames = new Map<string, string>();

/** Keys of the events already announced, oldest first and bounded: a second
 * delivery of the same occurrence (it should not happen, but a duplicated alert
 * is the one thing the ticket forbids) is dropped. */
const announcedKeys = new Set<string>();
const MAX_ANNOUNCED_KEYS = 64;

/** Set by the `stopGeneration` action while a reply is streaming; cleared at
 * the start of every `runGeneration`. Pressing Stop makes the backend end the
 * stream with a perfectly normal `done` event, so this is the only way to tell
 * a stopped reply from a finished one. */
let stopRequestedForTurn = false;

/** Set once the Rust "engine status changed" listener is registered (by
 * `finishBootAfterUnlock`), so a second boot path can't stack a second one. */
let engineNudgeRegistered = false;

/** The same, for the "notification clicked" listener. */
let clickNudgeRegistered = false;

/** True from the moment an app update has finished downloading until it fails
 * or the app is gone: the install that follows verifies, unpacks and — in the
 * portable build — stops the engine and exits, so the "unloaded" that a poll
 * could catch in that window is the app leaving, not news. (Earlier in the
 * update the download can take minutes, and loads and unloads during it are
 * announced as usual.) */
let updateRestarting = false;

/** Counter behind the per-run part of a download's dedupe key, so downloading
 * a model, deleting it and downloading it again are two announcements. */
let downloadRunSeq = 0;

/** Engine-status fetches are numbered when they are issued, and
 * `engineAppliedSeq` is the newest number whose result is on screen. A reply
 * that lands after a *newer* one was applied — or after a local write that is
 * authoritative (the optimistic Starting, `stopModel`'s Stopped, `useModel`'s
 * Failed) — is older than what the user already sees and is dropped.
 * Tauri commands run concurrently, so replies can arrive out of order; an old
 * `running(A)` landing after the unload would re-arm the tracker and announce
 * the same unload a second time. This sits next to the existing `engineBusy`
 * snapshot in `pollEngine`, which it does not replace: that one drops replies
 * that straddle a start/stop, this one orders the rest (`deleteModel`, which
 * never flips `engineBusy`, relies on it alone). */
let engineFetchSeq = 0;
let engineAppliedSeq = 0;

/** Forget everything the alert wiring has seen. Module state survives
 * `useStore.setState(...)`, so tests that walk the engine through a scenario
 * call this between cases. */
export function resetAlertTracking(): void {
  Object.assign(engineTracker, createEngineTracker());
  engineNames.clear();
  announcedKeys.clear();
  stopRequestedForTurn = false;
  engineNudgeRegistered = false;
  clickNudgeRegistered = false;
  updateRestarting = false;
}

/** Say it: an in-app alert unless the rules skip it, and an OS notification
 * with the same words when they send one. Total — it runs inside a store
 * `set()` subscriber and right after a reply settles, where a throw would break
 * the caller — so a missing function in a test mock or the e2e harness, a
 * rejected IPC call, anything, is swallowed. */
function announceAppEvent(get: GetState, event: AppEvent): void {
  try {
    if (announcedKeys.has(event.key)) return;
    announcedKeys.add(event.key);
    if (announcedKeys.size > MAX_ANNOUNCED_KEYS) {
      const oldest = announcedKeys.values().next().value;
      if (oldest !== undefined) announcedKeys.delete(oldest);
    }

    const s = get();
    const { title, body } = alertCopy(event);
    const { inApp, native } = decideDelivery(event, {
      view: s.view,
      active: s.active,
      moreDialogPage: s.moreDialogPage,
      conversationSearchOpen: s.conversationSearchOpen,
      desktopNotifications: s.settings?.desktopNotifications,
      windowFocused: s.windowFocused,
      nativeNotifications: hasNativeNotifications(s.appInfo),
    });
    if (inApp) s.showAlert({ kind: event.kind, title, body });
    // Fire and forget: the user doesn't wait on the OS, and a failure (not
    // unlocked yet, blocked by the OS, no Tauri runtime) is not something to
    // tell them about. `Promise.resolve` so that a function which returns
    // nothing, as a bare mock does, is as harmless as one that rejects.
    if (native) {
      const conversationId = event.kind === "answerReady" ? event.conversationId : null;
      Promise.resolve(api.notifyNative(title, body, conversationId)).catch(() => {});
    }
  } catch {
    // See above: announcing must never break what it was called from. This
    // also covers `notifyNative` not being callable at all (a test mock, the
    // Playwright harness) — it throws synchronously.
  }
}

/** The name the alert for this engine event will use: the model list's
 * `spec.name` if it still holds the model, else the last name seen for it. */
function engineAppEvent(state: Store, e: EngineAlertEvent): AppEvent {
  return {
    kind: e.kind,
    key: `engine:${e.kind}:${e.occurrence}`,
    modelName: engineModelName(state.models, e.modelId) ?? engineNames.get(e.modelId) ?? null,
  };
}

/** The composer/search UI state that resets whenever the visible
 * conversation changes wholesale — a new chat, opening a different one, or
 * forking — never inherited from whatever was showing before. Shared by
 * `newChat`/`openConversation`/`forkConversation`. */
function resetPerConversationUiState(get: GetState) {
  return {
    // Off by default, even with the web-search add-on on: the composer
    // globe click is the per-conversation consent gesture for sending
    // anything outside the machine. See `setWebSearch`.
    webSearch: false,
    agentTools: agentToolsAvailable(get().settings),
    sidebarSearchQuery: "",
    sidebarSearchResults: [],
    // A pending jump-to-message target is only ever meant for the specific
    // conversation `openConversationAtMessage` set it for — never inherited
    // by whatever's opened next.
    scrollToMessageId: null,
  };
}

/** The shared core of `send()` and `retryMessage()`: streams a reply for
 * `history` into `conversation` (whose last message must already be the
 * empty assistant placeholder the UI fills in as it streams — same shape
 * `send()` built inline before this was extracted), settles it, and
 * persists it. `history` is exactly what's sent to `api.chatStream` — for
 * `send()` that's the existing turns plus a brand-new user message; for
 * `retryMessage()` it's the existing turns unchanged (no new user
 * message — regenerating a reply for the same preceding turn).
 *
 * `finalize`, if given, transforms the settled conversation (e.g. folding
 * the newly-generated tail into `Conversation.branches`) *before* it's set
 * as `active` and persisted — this is what lets `retryMessage()` save its
 * branch bookkeeping in the same `api.saveConversation` call this function
 * already makes, instead of a second one. `editMessage()` doesn't have
 * this hook available (it goes through the public `send()` action, to
 * reuse `send()`'s own skill-invocation/title logic untouched) and so
 * issues its own follow-up save instead — see `editMessage`'s own comment.
 */
async function runGeneration(
  set: SetState,
  get: GetState,
  conversation: Conversation,
  history: ChatMessage[],
  finalize?: (settled: Conversation) => Conversation,
): Promise<Conversation> {
  // The model actually answering this turn, stamped on the placeholder by
  // the caller — read back off it rather than re-threading it as its own
  // parameter.
  const placeholder = conversation.messages[conversation.messages.length - 1];
  const replyModel = placeholder.model;
  const assistantId = placeholder.id;
  const assistantCreatedAtMs = placeholder.createdAtMs;

  // The local turn is the source of truth for the streamed reply; `active`
  // only mirrors it while it still shows this exact turn. That way a
  // mid-stream conversation swap can never corrupt or lose the reply.
  let reply = "";
  let thinking = "";
  // Set once the stream's "done" event lands; carries token/sec + duration
  // (thinking + answer combined) for the settled message's stats display.
  let stats: ChatStats | undefined;
  // How the stream ended, for deciding whether this turn earns an "Answer
  // ready" alert (see where it is announced, below). A `done` event alone
  // doesn't: the backend also sends it when the user presses Stop.
  stopRequestedForTurn = false;
  let sawDone = false;
  let sawError = false;
  let streamThrew = false;
  const citations: Citation[] = [];
  // Tool-call steps (searches run, pages read), accumulated in order so they
  // persist on the message as a "what it did" panel. Titles seen in search
  // results label the later read steps.
  const steps: ChatStep[] = [];
  const titleByUrl: Record<string, string> = {};
  // Files the model created this turn via the `create_artifact` tool, in
  // the order they were made — shown as downloadable chips on the message.
  const artifacts: ArtifactRef[] = [];
  // While a `spawn_subagent` call's nested loop is running, its own tool
  // calls arrive as ordinary toolCallStarted/Result/Denied/Rejected events
  // (dispatch_call doesn't know it's nested) — this buffer intercepts them
  // so they collapse into the subagent's own step instead of flattening
  // into the parent turn's step list. Only one can be active at a time:
  // tool calls are always dispatched sequentially, never concurrently, and
  // a subagent can't itself spawn a subagent (depth is capped at 1).
  let activeSubagent: {
    toolCallId: string;
    kind: string;
    task: string;
    steps: ChatStep[];
    stepRef: Extract<ChatStep, { type: "subagentRun" }>;
  } | null = null;
  // Snapshot the streamed assistant message from the locally-tracked turn.
  const assistantMessage = (): ChatMessage => ({
    id: assistantId,
    role: "assistant",
    content: reply,
    createdAtMs: assistantCreatedAtMs,
    model: replyModel,
    citations: citations.length ? [...citations] : undefined,
    thinking: thinking ? thinking : undefined,
    steps: steps.length ? [...steps] : undefined,
    artifacts: artifacts.length ? [...artifacts] : undefined,
    stats,
  });
  // Write the in-progress reply (text, thinking, steps, sources gathered so
  // far) into the streaming placeholder, but only while `active` still shows
  // this turn.
  const patchAssistant = () => {
    set((s) => {
      if (!s.active || s.active.id !== conversation.id) return {};
      if (s.active.messages.length !== conversation.messages.length) return {};
      const last = s.active.messages[s.active.messages.length - 1];
      if (last.role !== "assistant") return {};
      const messages = [...s.active.messages];
      messages[messages.length - 1] = assistantMessage();
      return { active: { ...s.active, messages } };
    });
  };

  // On for this conversation *and* the web-search add-on is on and set up
  // (the backend enforces that gate too, and falls back to a plain answer
  // itself if the search service won't start or the backend is Cloud).
  const useWebSearch = get().webSearch && webSearchAvailable(get().plugins);
  // Same shape for agent tools: on for this conversation *and* enabled +
  // configured globally (the backend enforces that gate too).
  const useAgentTools = get().agentTools && agentToolsAvailable(get().settings);
  // toolCallId -> what was approved, so `toolCallResult` (which only
  // carries ok/output, not the tool/summary/decision) can build this
  // turn's `ChatStep.toolCall` entry.
  const toolStepMeta = new Map<
    string,
    { toolId: string; summary: string; decision: ToolCallDecision }
  >();
  // toolCallId -> the question text, captured from `clarifyingQuestionAsked`
  // so `toolCallResult` (which doesn't repeat it) can build this turn's
  // `ChatStep.askedQuestion` entry even if the question was never actually
  // answered (e.g. the turn was stopped while it was pending).
  const askedQuestions = new Map<string, string>();

  // Always go through chatStream. With web search on, Rust routes the turn to
  // the enabled, ready web-search plugin (which streams the search/citation
  // events below); with agent tools on (and no web search plugin routing),
  // it routes through the agent-mode loop (the toolConsent*/toolCall* events
  // below); otherwise it answers with the plain engine.
  try {
    await api.chatStream(
      conversation.id,
      history,
      useWebSearch,
      useAgentTools,
      conversation.sampling,
      (event) => {
      switch (event.type) {
        case "token":
          reply += event.data.text;
          // The answer has begun — clear the "searching…" status.
          if (get().liveActivity !== null) set({ liveActivity: null });
          patchAssistant();
          break;
        case "reasoning":
          // Respect "Show model thinking": when off, drop reasoning so it is
          // neither shown nor persisted on this turn. (A model loaded while
          // the setting was on can still stream it until its next reload.)
          if (get().settings?.showThinking === false) break;
          thinking += event.data.text;
          patchAssistant();
          break;
        case "searchSetupProgress":
          // While the web-search add-on's local backend starts (or why it
          // couldn't) — surface it on the same status line `searchStarted`
          // etc. use below.
          set({ liveActivity: { kind: "setup", message: event.data.message } });
          patchAssistant();
          break;
        case "searchStarted":
          steps.push({ type: "search", data: { query: event.data.query } });
          set({ liveActivity: { kind: "searching", query: event.data.query } });
          patchAssistant();
          break;
        case "searchResult":
          // Remember titles so the read step can show a friendly name.
          if (event.data.url) titleByUrl[event.data.url] = event.data.title;
          if (get().liveActivity?.kind === "searching") {
            set({ liveActivity: { kind: "readingResults" } });
          }
          break;
        case "fetching":
          steps.push({
            type: "read",
            data: {
              url: event.data.url,
              title: titleByUrl[event.data.url] || event.data.url,
            },
          });
          set({ liveActivity: { kind: "reading", host: hostOf(event.data.url) } });
          patchAssistant();
          break;
        case "citation":
          if (!citations.some((c) => c.id === event.data.id)) {
            citations.push(event.data);
            citations.sort((a, b) => a.id - b.id);
            patchAssistant();
          }
          break;
        case "toolConsentRequested":
          set({
            pendingToolConsent: {
              requestId: event.data.requestId,
              toolCallId: event.data.toolCallId,
              toolId: event.data.toolId,
              summary: event.data.summary,
              detail: event.data.detail,
              resolvedPaths: event.data.resolvedPaths,
              dangerFlags: event.data.dangerFlags,
              via: event.data.via,
            },
          });
          break;
        case "clarifyingQuestionAsked":
          askedQuestions.set(event.data.toolCallId, event.data.question);
          set({
            pendingClarifyingQuestion: {
              requestId: event.data.requestId,
              toolCallId: event.data.toolCallId,
              question: event.data.question,
              options: event.data.options,
            },
          });
          break;
        case "toolCallStarted": {
          const responded = get().lastRespondedToolConsent;
          const decision: ToolCallDecision = event.data.autoApproved
            ? "alwaysAllow"
            : responded?.toolCallId === event.data.toolCallId && responded.decision === "alwaysAllow"
              ? "alwaysAllow"
              : "allowOnce";
          toolStepMeta.set(event.data.toolCallId, {
            toolId: event.data.toolId,
            summary: event.data.summary,
            decision,
          });
          set({ liveActivity: { kind: "tool", toolId: event.data.toolId } });
          patchAssistant();
          break;
        }
        case "toolCallResult": {
          // The outer `spawn_subagent` call's own result finalizes that
          // subagent's collapsed step (with everything it did nested
          // inside), rather than pushing a plain toolCall step for it.
          if (activeSubagent && event.data.toolCallId === activeSubagent.toolCallId) {
            activeSubagent.stepRef.data.ok = event.data.ok;
            activeSubagent = null;
            patchAssistant();
            break;
          }
          const meta = toolStepMeta.get(event.data.toolCallId);
          if (meta?.toolId === "ask_user") {
            const responded = get().lastRespondedClarifyingQuestion;
            const step: ChatStep = {
              type: "askedQuestion",
              data: {
                question: askedQuestions.get(event.data.toolCallId) ?? "",
                answer:
                  responded?.toolCallId === event.data.toolCallId ? responded.answer : "",
              },
            };
            (activeSubagent ? activeSubagent.steps : steps).push(step);
            set({ pendingClarifyingQuestion: null });
            if (get().liveActivity !== null) set({ liveActivity: null });
            patchAssistant();
            break;
          }
          const step: ChatStep = {
            type: "toolCall",
            data: {
              toolId: meta?.toolId ?? "tool",
              // On failure, `outputPreview` carries the actual reason (e.g.
              // "The add-on isn't set up"); the pre-call consent summary
              // only ever describes the *attempted* action, not why it
              // didn't happen — reusing it on failure hid the real error.
              summary: event.data.ok ? meta?.summary ?? "" : event.data.outputPreview,
              decision: meta?.decision ?? "allowOnce",
              ok: event.data.ok,
            },
          };
          (activeSubagent ? activeSubagent.steps : steps).push(step);
          if (get().liveActivity !== null) set({ liveActivity: null });
          patchAssistant();
          break;
        }
        case "toolCallDenied": {
          const responded = get().lastRespondedToolConsent;
          const known =
            responded?.toolCallId === event.data.toolCallId ? responded : undefined;
          const step: ChatStep = {
            type: "toolCall",
            data: {
              toolId: known?.toolId ?? "tool",
              summary: known?.summary ?? event.data.note ?? "",
              decision: "denied",
              ok: false,
            },
          };
          (activeSubagent ? activeSubagent.steps : steps).push(step);
          set({ pendingToolConsent: null });
          patchAssistant();
          break;
        }
        case "toolCallRejected": {
          const step: ChatStep = {
            type: "toolCall",
            data: {
              toolId: event.data.toolId,
              summary: event.data.reason,
              decision: "rejected",
              ok: false,
            },
          };
          (activeSubagent ? activeSubagent.steps : steps).push(step);
          patchAssistant();
          break;
        }
        case "subagentStarted": {
          const nested: ChatStep[] = [];
          const stepRef: Extract<ChatStep, { type: "subagentRun" }> = {
            type: "subagentRun",
            data: { kind: event.data.kind, task: event.data.task, ok: null, steps: nested },
          };
          steps.push(stepRef);
          activeSubagent = {
            toolCallId: event.data.toolCallId,
            kind: event.data.kind,
            task: event.data.task,
            steps: nested,
            stepRef,
          };
          set({ liveActivity: { kind: "subagent", helper: event.data.kind } });
          patchAssistant();
          break;
        }
        case "subagentFinished":
          if (get().liveActivity !== null) set({ liveActivity: null });
          break;
        case "artifactCreated":
          artifacts.push({
            id: event.data.id,
            filename: event.data.filename,
            sizeBytes: event.data.sizeBytes,
          });
          patchAssistant();
          break;
        case "error":
          sawError = true;
          set({ chatError: event.data.message });
          break;
        case "done":
          // Stream closes on "done"; the turn is settled below, where
          // `stats` is folded into the persisted message.
          stats = event.data.stats;
          sawDone = true;
          break;
      }
      },
    );
  } catch (err: unknown) {
    streamThrew = true;
    set((s) => ({ chatError: s.chatError || String(err) }));
  }

  // A subagent step still `ok: null` here means the stream ended (error or
  // Stop) before its result arrived — settle it as incomplete rather than
  // persisting a step that would claim to be forever "running".
  for (const step of steps) {
    if (step.type === "subagentRun" && step.data.ok === null) step.data.ok = false;
  }

  // Settle from the locally tracked turn — never from `active`. Drop the
  // placeholder only if truly nothing arrived (no answer, and no thinking,
  // tool steps, or created artifacts worth keeping); otherwise persist what
  // was generated. In practice `create_artifact` is itself a tool call, so
  // an artifact never arrives without a matching step — this check is just
  // a defensive belt-and-suspenders against a bare `artifactCreated`.
  const messages = [...conversation.messages];
  if (reply === "" && thinking === "" && steps.length === 0 && artifacts.length === 0) {
    messages.pop();
  } else {
    messages[messages.length - 1] = assistantMessage();
  }
  let settled: Conversation = {
    ...conversation,
    messages,
    updatedAtMs: Date.now(),
  };
  if (finalize) settled = finalize(settled);
  set((s) => ({
    streaming: false,
    liveActivity: null,
    // A turn always ends with every tool call resolved one way or another
    // (denied on Stop included, per the orchestrator's contract) — clear
    // defensively so a card can never linger past its own turn.
    pendingToolConsent: null,
    pendingClarifyingQuestion: null,
    active: s.active?.id === conversation.id ? settled : s.active,
  }));

  // "Answer ready" — only for a turn that really ran to its end: the stream
  // closed with `done` and no error, the user didn't press Stop, and there is
  // reply text (a turn of thinking or tool steps only isn't an answer yet).
  // Announced here, once per call and after `streaming` is false, and not from
  // the `done` event or a `streaming` true->false subscription: the former is
  // too early to know how the turn ended, the latter also fires for
  // Translate/Correct, which toggle `streaming` themselves. Before the save
  // below so a failing save can't swallow it; `announceAppEvent` can't throw.
  if (sawDone && !sawError && !streamThrew && !stopRequestedForTurn && reply.trim() !== "") {
    announceAppEvent(get, {
      kind: "answerReady",
      key: `answer:${assistantId}`,
      conversationId: conversation.id,
    });
  }

  if (messages.some((m) => m.role === "assistant")) {
    try {
      await api.saveConversation(settled);
      set({ conversations: await api.listConversations() });
    } catch {
      // Saving history must never interrupt a chat.
    }
  }
  return settled;
}

/** A skill's full text as folded into the prompt: its main body, followed by
 * every supporting file (`Skill.files`, empty for an ordinary single-file
 * skill) under a heading naming it, in the order the backend returns them
 * (`SKILL.md` first — `body` — then supporting files alphabetically by
 * path). Capped like a document attachment (`attachments.rs`'s
 * `MAX_DOC_CHARS`) so a large reference file can't blow up the prompt. */
const MAX_EXPANDED_SKILL_CHARS = 100_000;

export function expandSkillBody(skill: Skill): string {
  const parts = [skill.body, ...(skill.files ?? []).map((f) => `## ${f.path}\n\n${f.content}`)];
  const full = parts.join("\n\n");
  return full.length <= MAX_EXPANDED_SKILL_CHARS
    ? full
    : `${full.slice(0, MAX_EXPANDED_SKILL_CHARS)}\n\n…[truncated]`;
}

/** Recognizes a `/<slug>` prefix (the first "word" of the message) matching a
 * saved skill, and expands it: the skill's full text (see `expandSkillBody`)
 * goes to the model ahead of whatever else the user typed. Returns null for a
 * plain message, or for a `/token` that isn't a known skill's slug — that's
 * sent as plain text as-is. */
export function matchSkillInvocation(text: string, skills: Skill[]): { content: string } | null {
  const match = /^\/(\S+)\s*/.exec(text);
  if (!match) return null;
  const skill = skills.find((s) => s.slug === match[1].toLowerCase());
  if (!skill) return null;
  const rest = text.slice(match[0].length).trim();
  const body = expandSkillBody(skill);
  return { content: rest ? `${body}\n\n${rest}` : body };
}

/** Same recognition rule as `matchSkillInvocation`, but for *display*: returns
 * the invoked skill and whatever the user typed after it, unexpanded, so a
 * sent `/<skill>` turn can be shown as a command chip instead of raw text.
 * Returns null for plain text or an unrecognized `/token` — those are shown
 * as-is, exactly like `matchSkillInvocation` leaves them for sending. */
export function parseSkillInvocationDisplay(
  text: string,
  skills: Skill[],
): { skill: Skill; rest: string } | null {
  const match = /^\/(\S+)\s*/.exec(text);
  if (!match) return null;
  const skill = skills.find((s) => s.slug === match[1].toLowerCase());
  if (!skill) return null;
  return { skill, rest: text.slice(match[0].length).trim() };
}

function titleFrom(text: string): string {
  const clean = text.replace(/\s+/g, " ").trim();
  return clean.length > 46 ? `${clean.slice(0, 46)}…` : clean || "New chat";
}

/** Whether agent tools can be used at all right now: enabled in Settings.
 * Gates and provides the default for the composer's per-conversation
 * agent-tools toggle. The workspace folder tools run in is
 * system-managed (one per conversation) and isn't a user choice, so it isn't
 * part of this gate. */
export function agentToolsAvailable(settings: Settings | null): boolean {
  return Boolean(settings?.agentModeEnabled);
}

/** Whether web search can be used at all right now: a plugin advertising
 * `search` (the experimental `web-search` add-on) is enabled and set up.
 * Gates the composer's web-search menu item and its per-conversation toggle
 * (`send`). Unlike `modelBrowserAvailable` it checks setup too: turning the
 * add-on on installs its local SearXNG first, and there's nothing to search
 * with until that finishes. */
export function webSearchAvailable(plugins: PluginInfo[]): boolean {
  return plugins.some(
    (p) => p.enabled && p.setup.type === "ready" && p.capabilities.includes("search"),
  );
}

/** Whether Settings → Advanced → Experimental features is on: what any
 * experimental feature that isn't a plugin gates on. (An experimental plugin
 * needs no check of its own: `list_plugins` omits it while this is off.) A
 * feature with its own on/off choice also belongs in the list
 * `ExperimentalSection` names when the user turns this off. */
export function experimentalFeaturesOn(settings: Settings | null): boolean {
  return Boolean(settings?.experimentalFeatures);
}

/** Whether the Hugging Face model-browser plugin is enabled — gates the
 * Browse entry and More dialog page. This checks the capability
 * rather than a fixed plugin id, so any plugin
 * that advertises `model-browser` lights it up. Deliberately does NOT check
 * `service` (a python-less plugin's service is always "notApplicable", never
 * "running" — see plugins/PLUGINS.md §8) or `setup` (trivially "ready" the
 * instant a python-less plugin is enabled). */
export function modelBrowserAvailable(plugins: PluginInfo[]): boolean {
  return plugins.some((p) => p.capabilities.includes("model-browser") && p.enabled);
}

/** Whether the Writing Tools plugin is enabled -- gates the sidebar's
 * Translate/Correct/Improve entries, the "translate"/"correct"/"improve"
 * views they open, and the composer's inline "Improve my message" button.
 * Same capability-and-`enabled` check as `modelBrowserAvailable`, not a fixed
 * plugin id, so any plugin that advertises `writing-tools` lights it up.
 * Deliberately does NOT check `service` (a python-less plugin's service is
 * always "notApplicable", never "running" -- see plugins/PLUGINS.md section 8)
 * or `setup` (trivially "ready" the instant a python-less plugin is enabled).
 * Like `modelBrowserAvailable` (and unlike `smartModelRouterAvailable`), this gates
 * navigation destinations, so it needs a "translate"/"correct"/"improve"
 * counterpart to the mid-session capability-disappears redirect that the
 * Browse page already gets. */
export function writingToolsAvailable(plugins: PluginInfo[]): boolean {
  return plugins.some((p) => p.capabilities.includes("writing-tools") && p.enabled);
}

/** Whether a More dialog page can be reached right now — the sidebar's More
 * menu and the dialog's own left nav both list exactly the pages this
 * allows. Browse needs the model-browser capability; Plugins is hidden on
 * portable and Android builds, and while there's nothing to list. Every
 * Settings category is always there. */
export function moreDialogPageAvailable(
  page: MoreDialogPage,
  s: Pick<Store, "plugins" | "appInfo">,
): boolean {
  switch (page) {
    case "discover":
      return modelBrowserAvailable(s.plugins);
    case "plugins":
      return !s.appInfo?.portable && !s.appInfo?.isAndroid && s.plugins.length > 0;
    default:
      return true;
  }
}

/** Whether the Smart Model Router plugin is enabled — gates `useTriageAutopilot`'s
 * model-switch branch (`ChatView.tsx`). Same capability-only check as
 * `modelBrowserAvailable`; no `view`-redirect counterpart is needed since
 * this isn't a navigation destination, just a background behavior. */
export function smartModelRouterAvailable(plugins: PluginInfo[]): boolean {
  return plugins.some((p) => p.capabilities.includes("smart-model-router") && p.enabled);
}

/** Whether the signed-in account can chat with a cloud model. A UX guard
 * only — the real gate is the backend refusing the request server-side (see
 * `require_entitlement` on the Rust side); this just decides whether to show
 * cloud model rows as usable or as a "Sign in to unlock" upsell. */
export function cloudModelsAvailable(account: Account | null): boolean {
  return account?.capabilities.includes("cloud-models") ?? false;
}

/** Whether `ChatView` should show `SafetyBanner` for a conversation's
 * `safetyAlerts` entry (see that field's doc comment) — `undefined` for a
 * conversation nothing has ever flagged, otherwise visible exactly when the
 * flagged turn hasn't been dismissed. Pulled out as its own pure function so
 * this turn/dismissedTurn comparison — the exact thing that broke once
 * already, when it lived as ad hoc local component state — has a single,
 * directly testable definition instead of being inlined at the JSX call
 * site. */
export function isSafetyAlertVisible(
  entry: { turn: number; dismissedTurn: number | null } | undefined,
): boolean {
  return !!entry && entry.turn !== entry.dismissedTurn;
}

/** Bare host for a friendly "Reading example.com…" status. */
function hostOf(url: string): string {
  try {
    return new URL(url).hostname.replace(/^www\./, "");
  } catch {
    return url;
  }
}

/**
 * Set synchronously on the first boot() call. React StrictMode re-runs the
 * mount effect, and a second boot would auto-start the engine twice.
 */
let bootStarted = false;

/** Which `setTheme` call is the latest, so a failed write only rolls back the
 * pick that is still the one on screen. */
let themeWriteSeq = 0;

/** Same job as `themeWriteSeq`, for `setDesktopNotifications`. */
let desktopNotificationsWriteSeq = 0;

/** What a failed `setDesktopNotifications` puts back: the value the last
 * settled write left on disk, or the loaded one when none is outstanding.
 * Restoring "the value before this pick" is wrong once picks overlap — if the
 * pick before it also failed, that "before" is its optimistic value, never
 * saved, and the switch would end up showing a state that is on no disk. */
let desktopNotificationsOnDisk = false;
let desktopNotificationsInFlight = 0;

/** Counter behind `AppAlert.id`. */
let alertSeq = 0;

/**
 * Model ids whose download was requested again while the previous one was
 * still settling (e.g. cancel, then an immediate re-download). The new
 * download starts as soon as the old one clears.
 */
const pendingRestarts = new Set<string>();

/** Debounce for persisting `updateGeneration` changes — a slider drag fires
 * many onChange events; only the value it settles on needs to hit disk. */
let saveGenerationTimer: ReturnType<typeof setTimeout> | null = null;

/** Debounce for `setSidebarSearchQuery` — only the text the user stops
 * typing on needs an actual backend search. */
let sidebarSearchTimer: ReturnType<typeof setTimeout> | null = null;

/** Alphabetical by name, case-insensitive — the sidebar's project list order. */
function byProjectName(a: Project, b: Project): number {
  return a.name.localeCompare(b.name, undefined, { sensitivity: "base" });
}

export const useStore = create<Store>()((set, get) => ({
  booted: false,
  settings: null,
  report: null,
  appInfo: null,
  appLockUnlocked: false,
  appLockJustReset: false,
  account: null,
  signInOpen: false,
  signInError: null,
  addDeviceOpen: false,
  addDeviceStatus: "listening",
  addDeviceIncoming: null,
  addDeviceBusy: false,
  addDeviceError: null,
  joinGroupOpen: false,
  joinGroupStatus: "form",
  joinGroupFingerprint: null,
  joinGroupBusy: false,
  joinGroupError: null,
  models: [],
  engine: { type: "stopped" },
  runningContextLength: null,
  engineBusy: false,
  userStoppedModel: false,
  view: "chat",
  draftToPrefill: null,
  moreDialogPage: null,
  conversationSearchOpen: false,
  mobileSidebarOpen: false,
  sidebarCollapsed: false,
  sidebarMoreMenuOpen: false,
  sidebarWidth: SIDEBAR_DEFAULT_WIDTH,

  plugins: [],

  cloudModels: null,
  cloudModelsError: null,

  conversations: [],
  projects: [],
  sidebarSearchQuery: "",
  sidebarSearchResults: [],
  skills: [],
  active: null,
  streaming: false,
  scrollToMessageId: null,
  liveActivity: null,
  chatError: null,
  safetyAlerts: {},
  webSearch: false,
  agentTools: false,
  pendingToolConsent: null,
  lastRespondedToolConsent: null,
  pendingClarifyingQuestion: null,
  lastRespondedClarifyingQuestion: null,
  notice: null,
  appAlerts: [],
  // The webview can only say whether it has focus; `App.tsx` keeps this in
  // step with the window's focus/blur events and re-syncs it on mount.
  windowFocused: typeof document !== "undefined" && document.hasFocus(),
  notificationPermission: "granted",

  downloads: {},
  downloadErrors: {},

  updateStatus: "idle",
  availableUpdate: null,
  updateProgress: null,
  updateError: null,

  engineUpdateStatus: "idle",
  availableEngineUpdate: null,
  engineUpdateProgress: null,
  engineUpdateError: null,

  boot: async () => {
    if (bootStarted) return;
    bootStarted = true;

    // Phase 1 of the backend's own boot split: `getAppInfo` is the one
    // command that works before the encryption key exists. Everything else
    // below needs a store the backend won't build until unlock succeeds —
    // calling it first, alone, is what lets a locked-out launch reach
    // `RecoveryUnlock` instead of the backend rejecting `getSettings` et al.
    // and leaving the app stuck on the bare titlebar forever.
    const appInfo = await api.getAppInfo();
    if (appInfo.needsRecovery) {
      set({ appInfo, booted: true });
      return;
    }

    // Same two-phase split as `needsRecovery` above: app-lock's PIN gate
    // must be cleared before real data loads, so stop here too and let
    // `AppLockGate` call `finishBootAfterUnlock` itself once the PIN (or a
    // recovery code) checks out.
    if (appInfo.needsAppUnlock && !get().appLockUnlocked) {
      set({ appInfo, booted: true });
      return;
    }

    set({ appInfo });
    await get().finishBootAfterUnlock();
  },

  // Phase 2: everything that needs a store. Shared by `boot()`'s normal
  // path (called immediately, once `getAppInfo` says the key is already
  // available) and `RecoveryUnlock`, which calls this itself once
  // `attemptRecoveryUnlock` succeeds — the backend has just built `AppState`
  // for the first time this launch, and until this runs, `settings` stays
  // `null` and `App.tsx`'s gate never lets the user past the titlebar.
  finishBootAfterUnlock: async () => {
    const [settings, engine, conversations, appInfo, account] = await Promise.all([
      api.getSettings(),
      api.getEngineStatus(),
      api.listConversations(),
      api.getAppInfo(),
      api.getAccount(),
    ]);

    // A fresh install has never had the chance to set a language explicitly —
    // give it a one-time nudge from the browser/OS locale instead of always
    // defaulting to English. Reuses `onboardingCompleted` as the "fresh
    // install" signal, same as the rest of boot()'s first-run logic.
    let effectiveSettings = settings;
    if (!settings.onboardingCompleted) {
      const detected = navigator.language.toLowerCase().startsWith("fr") ? "fr" : "en";
      if (detected !== settings.language) {
        effectiveSettings = { ...settings, language: detected };
        void get().saveSettings({ language: detected });
      }
    }
    // Resolve before the first render, so the app never flashes English then
    // switches to French (or vice versa).
    await i18n.changeLanguage(effectiveSettings.language);

    set({
      settings: effectiveSettings,
      engine,
      conversations,
      appInfo,
      account,
      booted: true,
      agentTools: agentToolsAvailable(effectiveSettings),
    });

    // Rust nudges the webview when it auto-unloads the model (see
    // `api.onEngineStatusChanged`); the 2s poll alone would hear about it late
    // when the window is hidden and its timers are throttled. Once per
    // session, and never allowed to fail boot: the poll still sees every
    // change, the nudge only makes it prompt.
    if (!engineNudgeRegistered) {
      engineNudgeRegistered = true;
      try {
        Promise.resolve(api.onEngineStatusChanged(() => void get().pollEngine())).catch(() => {});
      } catch {
        // Not callable here (a test mock).
      }
    }

    // A click on one of OpenPal's notifications: Rust nudges, the store reads
    // the click. Also once now, for a click that launched the app (or came in
    // while it was locked) and so arrived before anything was listening.
    if (!clickNudgeRegistered) {
      clickNudgeRegistered = true;
      try {
        Promise.resolve(api.onNotificationClicked(() => void get().followNotificationClick())).catch(
          () => {},
        );
      } catch {
        // Not callable here (a test mock).
      }
    }
    void get().followNotificationClick();

    // Models + report + plugins can land after first paint.
    const modelsReady = get().refreshModels();
    // Silent, fire-and-forget backfill for custom models added before OpenPal
    // tracked group/license/release-date metadata (see
    // `api.backfillCustomModelMetadata`'s doc comment) — idempotent, so
    // running it on every boot is harmless once everything's already filled
    // in. Only re-fetches the model list when it actually changed something.
    void api.backfillCustomModelMetadata().then((updated) => {
      if (updated > 0) void get().refreshModels();
    });
    void get().loadPlugins();
    void get().loadCloudModels();
    void get().loadSkills();
    void get().loadProjects();
    void api.getSystemReport().then((report) => set({ report }));

    // The app's promise: open it and it's ready to chat. If the chosen model
    // is on disk but the engine is off, warm it up without being asked (unless
    // the user turned that off in Settings) — once models have loaded (a fast
    // local call), so the "starting" status a disk-streamed model flips to
    // right after launch can already be told apart from a normal one (its fit
    // would otherwise briefly read as unknown against a still-empty `models`
    // array).
    //
    // `defaultModelId` (an explicit "always start with this one" choice made
    // in Settings) wins when set; otherwise fall back to `selectedModelId`,
    // i.e. resume whatever was last used — the original behavior, unchanged
    // for anyone who hasn't opted into a fixed default.
    void modelsReady.then(() => {
      if (!settings.onboardingCompleted || !settings.autoLoadOnStartup || get().engine.type !== "stopped") {
        return;
      }

      const isInstalled = (id: string) =>
        get().models.find((m) => m.spec.id === id)?.status.type === "installed";

      // Both ids are settings.json pointers that can go stale (the model was
      // deleted, or — a settings.json write races another in-flight save,
      // see delete_model in commands.rs — a clearing write got clobbered).
      // selected_model_id quietly heals itself the next time any model
      // starts successfully (start_engine always overwrites it); default_model_id
      // has no such path, so a dangling one would otherwise fail to boot on
      // every single launch forever. Validate against the installed list
      // and clear rather than trust it blindly.
      let startupModelId = settings.defaultModelId;
      if (startupModelId && !isInstalled(startupModelId)) {
        void get().saveSettings({ defaultModelId: null });
        startupModelId = null;
      }
      startupModelId ??= settings.selectedModelId;

      if (startupModelId && isInstalled(startupModelId)) {
        void get().useModel(startupModelId);
      }
    });

    // A few seconds after startup, not blocking first paint — silent, so a
    // failed check (offline, endpoint not reachable) never nags the user.
    if (settings.autoCheckUpdates) {
      setTimeout(() => void get().checkForUpdate(false), 5000);
    }
    if (settings.autoCheckEngineUpdates) {
      setTimeout(() => void get().checkForEngineUpdate(false), 5000);
    }

    // Same fire-and-forget shape: silently pick up a still-valid sign-in
    // session so the cached account (possibly stale/absent) is fresh shortly
    // after launch, without ever blocking or nagging on failure.
    setTimeout(() => void get().refreshAccount(), 5000);
  },

  setView: (view) => set({ view }),
  setMoreDialogPage: (moreDialogPage) => set({ moreDialogPage }),
  goToChat: () => set({ moreDialogPage: null, view: "chat" }),
  setConversationSearchOpen: (conversationSearchOpen) => set({ conversationSearchOpen }),
  setMobileSidebarOpen: (mobileSidebarOpen) => set({ mobileSidebarOpen }),
  setSidebarCollapsed: (sidebarCollapsed) => set({ sidebarCollapsed }),
  setSidebarMoreMenuOpen: (sidebarMoreMenuOpen) => set({ sidebarMoreMenuOpen }),
  setSidebarWidth: (width) =>
    set({
      sidebarWidth: Math.min(SIDEBAR_MAX_WIDTH, Math.max(SIDEBAR_MIN_WIDTH, width)),
    }),
  setNotice: (notice) => set({ notice }),
  showAlert: (alert) =>
    set((s) => ({
      appAlerts: [{ ...alert, id: `alert-${++alertSeq}` }, ...s.appAlerts].slice(
        0,
        MAX_VISIBLE_ALERTS,
      ),
    })),
  dismissAlert: (id) =>
    set((s) =>
      s.appAlerts.some((a) => a.id === id)
        ? { appAlerts: s.appAlerts.filter((a) => a.id !== id) }
        : s,
    ),
  setWindowFocused: (windowFocused) => {
    if (get().windowFocused !== windowFocused) set({ windowFocused });
  },
  refreshNotificationPermission: async () => {
    try {
      set({ notificationPermission: await api.notificationPermission() });
    } catch {
      // Not knowing is not "blocked": keep whatever we had.
    }
  },
  requestNotificationPermission: async () => {
    try {
      set({ notificationPermission: await api.requestNotificationPermission() });
    } catch {
      // Same as above: a failed ask is not a "no".
    }
  },
  followNotificationClick: async () => {
    if (!get().settings) return;
    let click: NotificationClick | null = null;
    try {
      click = await api.takeNotificationClick();
    } catch {
      // No bridge (a test mock) or nothing readable: there is no click to follow.
      return;
    }
    // A click that names no conversation only brings OpenPal forward, which
    // Rust has already done.
    const id = click?.conversationId;
    if (!id) return;
    set({ moreDialogPage: null, conversationSearchOpen: false });
    await get().openConversation(id);
  },

  refreshModels: async () => {
    const models = await api.getModels();
    set({ models });
  },

  startDownload: (modelId) => {
    const { downloads } = get();
    if (downloads[modelId]) {
      // The previous download for this model hasn't settled yet (e.g. it was
      // just cancelled). Start again as soon as it clears.
      pendingRestarts.add(modelId);
      return;
    }
    pendingRestarts.delete(modelId);

    set((s) => ({
      downloads: {
        ...s.downloads,
        [modelId]: { receivedBytes: 0, totalBytes: 0, bytesPerSec: 0, etaSeconds: 0 },
      },
      downloadErrors: { ...s.downloadErrors, [modelId]: "" },
    }));

    const clear = () =>
      set((s) => {
        const next = { ...s.downloads };
        delete next[modelId];
        return { downloads: next };
      });

    // Whether this run already announced its `finished`; also names the run in
    // the announcement's dedupe key.
    let announced = false;
    const run = ++downloadRunSeq;

    api
      .downloadModel(modelId, (event) => {
        // Channel events can land after the download has settled and been
        // cleared; never let a stray one resurrect the progress entry.
        if (event.type === "started") {
          set((s) =>
            modelId in s.downloads
              ? {
                  downloads: {
                    ...s.downloads,
                    [modelId]: {
                      receivedBytes: event.data.resumedFromBytes,
                      totalBytes: event.data.totalBytes,
                      bytesPerSec: 0,
                      etaSeconds: 0,
                    },
                  },
                }
              : {},
          );
        } else if (event.type === "progress") {
          set((s) =>
            modelId in s.downloads
              ? { downloads: { ...s.downloads, [modelId]: { ...event.data } } }
              : {},
          );
        } else if (event.type === "failed") {
          set((s) => ({
            downloadErrors: { ...s.downloadErrors, [modelId]: event.data.message },
          }));
        } else if (event.type === "finished" && !announced) {
          // The `finished` event is the one true success signal: the command
          // also resolves OK when the user cancels, so announcing from
          // `.then()` below would say "Download complete" for a cancel. One
          // `finished` comes per model, not per file (the backend forwards
          // only the last part's). Named here, before the refresh below
          // replaces `models`.
          announced = true;
          announceAppEvent(get, {
            kind: "downloadFinished",
            key: `download:${modelId}:${run}`,
            modelName: engineModelName(get().models, modelId),
          });
        }
      })
      .then(async () => {
        clear();
        // A restart queued behind a run that just finished would re-run
        // against complete files and report a second `finished` at once, so
        // it is dropped; after a cancel or failure it restarts as before.
        if (pendingRestarts.delete(modelId) && !announced) {
          get().startDownload(modelId);
        }
        await get().refreshModels();
        // Backend may have auto-selected the first installed model.
        set({ settings: await api.getSettings() });
      })
      .catch((err: unknown) => {
        clear();
        set((s) => ({
          downloadErrors: {
            ...s.downloadErrors,
            [modelId]: s.downloadErrors[modelId] || String(err),
          },
        }));
        if (pendingRestarts.delete(modelId) && !announced) {
          get().startDownload(modelId);
        }
        void get().refreshModels();
      });
  },

  cancelDownload: async (modelId) => {
    // Also drop a restart queued behind a download that hasn't settled yet
    // (Download pressed again right after Cancel): left alone, it would start
    // the moment the first one settles — after the user cancelled it.
    pendingRestarts.delete(modelId);
    try {
      await api.cancelDownload(modelId);
    } catch (err: unknown) {
      set({ notice: String(err) });
    }
  },

  deleteModel: async (modelId) => {
    try {
      await api.deleteModel(modelId);
      await get().refreshModels();
      const ticket = ++engineFetchSeq;
      const [settings, engine] = await Promise.all([
        api.getSettings(),
        api.getEngineStatus(),
      ]);
      // Deleting the running model stops the engine, so this reply carries the
      // unload. A poll that was already in flight and lands after it must not
      // put the old Running status back — see `engineFetchSeq`.
      if (ticket >= engineAppliedSeq) {
        engineAppliedSeq = ticket;
        set({ settings, engine });
      } else {
        set({ settings });
      }
    } catch (err: unknown) {
      set({ notice: String(err) });
    }
  },

  setModelFavorite: async (modelId, favorite) => {
    try {
      await api.setModelFavorite(modelId, favorite);
      set((s) => ({
        models: s.models.map((m) =>
          m.spec.id === modelId ? { ...m, favorite } : m,
        ),
      }));
    } catch (err: unknown) {
      set({ notice: String(err) });
    }
  },

  useModel: async (modelId, contextLength) => {
    if (get().engineBusy) return;
    const length = contextLength ?? get().active?.contextLength ?? DEFAULT_CONTEXT_LENGTH;
    // The Starting below is a local write that is newer than any status fetch
    // still in flight — see `engineFetchSeq`.
    engineAppliedSeq = ++engineFetchSeq;
    // A fresh load attempt (manual or the composer's typing-triggered preload)
    // supersedes any earlier explicit stop — see `userStoppedModel`.
    set({
      engineBusy: true,
      // progressPercent starts null — App.tsx's 2s pollEngine (which keeps
      // polling through a steady `engineBusy`, see its own comment) picks up
      // the backend's live figure (GPU-offloaded loads only) well before
      // this async call itself resolves, since `startEngine` doesn't return
      // until the model is fully ready.
      engine: { type: "starting", data: { modelId, progressPercent: null } },
      userStoppedModel: false,
    });
    try {
      await api.startEngine(modelId, length);
      const ticket = ++engineFetchSeq;
      const [settings, engine] = await Promise.all([
        api.getSettings(),
        api.getEngineStatus(),
      ]);
      if (ticket >= engineAppliedSeq) {
        engineAppliedSeq = ticket;
        set({ settings, engine, runningContextLength: length });
      } else {
        // A newer status is already on screen; keep it.
        set({ settings, runningContextLength: length });
      }
    } catch (err: unknown) {
      engineAppliedSeq = ++engineFetchSeq;
      set({ engine: { type: "failed", data: { message: String(err) } } });
    } finally {
      set({ engineBusy: false });
    }
  },

  stopModel: async () => {
    if (get().engineBusy) return;
    set({ engineBusy: true });
    try {
      await api.stopEngine();
      // stop() is idempotent and always leaves the engine Stopped — see
      // EngineManager::stop in crates/core/src/engine.rs. Flag it as a
      // deliberate stop so the composer's typing-triggered preload (which
      // would otherwise treat "stopped" as "please load it back") leaves it
      // alone for the rest of the current draft — see `userStoppedModel`.
      engineAppliedSeq = ++engineFetchSeq;
      set({ engine: { type: "stopped" }, userStoppedModel: true });
    } catch (err: unknown) {
      set({ notice: String(err) });
    } finally {
      set({ engineBusy: false });
    }
  },

  snoozeAutoUnload: async () => {
    try {
      await api.snoozeAutoUnload();
      void get().pollEngine(); // pull the freshly reset autoUnloadAt right away
    } catch (err: unknown) {
      set({ notice: String(err) });
    }
  },

  useCloudModel: async (modelId) => {
    await get().saveSettings({ chatBackend: "cloud", selectedCloudModelId: modelId });
  },

  loadCloudModels: async () => {
    try {
      const cloudModels = await api.listCloudModels();
      set({ cloudModels, cloudModelsError: null });
    } catch (err: unknown) {
      set({ cloudModelsError: String(err) });
    }
  },

  pollEngine: async () => {
    // Snapshot instead of an outright `if (engineBusy) return`: a *steady*
    // busy period (an ongoing useModel()/stopModel() call) is exactly when
    // this poll is most useful — it's the only path that surfaces the
    // backend's live `EngineStatus::Starting.progressPercent` while a model
    // is loading, since useModel() itself doesn't get an answer back until
    // the model is fully ready (see EnginePill's progress fill). What must
    // still be dropped is a snapshot whose busy flag *changed* underneath
    // it — engineBusy flipping true mid-flight means a start/stop began
    // after this fetch was already sent, so it predates that call's own
    // fresh optimistic state; flipping false mid-flight means the operation
    // just finished and already set its own authoritative final state.
    // Applying either would flicker the UI backward.
    const wasBusy = get().engineBusy;
    const ticket = ++engineFetchSeq;
    try {
      const engine = await api.getEngineStatus();
      if (get().engineBusy !== wasBusy) return;
      // Also drop a reply older than one already applied — see `engineFetchSeq`.
      // (Not "skip if a poll is in flight": the poll `snoozeAutoUnload` fires
      // must still run, it is how the fresh `autoUnloadAt` arrives.)
      if (ticket < engineAppliedSeq) return;
      engineAppliedSeq = ticket;
      set({ engine });
    } catch {
      // Polling must never surface errors.
    }
  },

  loadPlugins: async () => {
    try {
      const plugins = await api.listPlugins();
      set({ plugins });
      // The Browse nav entry unmounts the instant the model-browser
      // capability disappears, but nothing else moves the More dialog off
      // the Browse page — do that here too, as a backstop alongside the same
      // check in setPluginEnabled's optimistic disable.
      if (get().moreDialogPage === "discover" && !modelBrowserAvailable(plugins)) {
        set({ moreDialogPage: "library" });
      }
      const view = get().view;
      if (
        (view === "translate" || view === "correct" || view === "improve") &&
        !writingToolsAvailable(plugins)
      ) {
        set({ view: "chat" });
      }
      // The Plugins entry is gone while there's nothing to list (every add-on
      // can be experimental, and hidden with that switch off) — land on the
      // dialog's first page rather than an empty one.
      if (get().moreDialogPage === "plugins" && plugins.length === 0) {
        set({ moreDialogPage: "library" });
      }
    } catch {
      // Plugins are optional; a discovery hiccup shouldn't break the app.
    }
  },

  setPluginEnabled: async (id, enabled) => {
    // Patch one plugin's live state in place; ignored if it's gone from the list.
    const patch = (next: Partial<PluginInfo>) =>
      set((s) => ({
        plugins: s.plugins.map((p) => (p.id === id ? { ...p, ...next } : p)),
      }));

    // Optimistically reflect the new enable state and the first transition.
    if (enabled) {
      const setup: SetupState = { type: "settingUp", data: { message: "Starting…", fraction: null } };
      patch({ enabled: true, setup });
    } else {
      const service: ServiceState = { type: "stopped" };
      patch({ enabled: false, service });
      // Instant redirect (don't wait for the reconciling loadPlugins below):
      // if this was the model-browser plugin and Browse is showing, its nav
      // entry just unmounted — follow it off that page now.
      if (get().moreDialogPage === "discover" && !modelBrowserAvailable(get().plugins)) {
        set({ moreDialogPage: "library" });
      }
      const view = get().view;
      if (
        (view === "translate" || view === "correct" || view === "improve") &&
        !writingToolsAvailable(get().plugins)
      ) {
        set({ view: "chat" });
      }
    }

    try {
      await api.setPluginEnabled(id, enabled, (event) => {
        switch (event.type) {
          case "setup":
            patch({
              setup: { type: "settingUp", data: { message: event.data.message, fraction: event.data.fraction } },
            });
            break;
          case "ready":
            patch({ setup: { type: "ready" } });
            break;
          case "failed":
            patch({ setup: { type: "failed", data: { message: event.data.message } } });
            set({ notice: event.data.message });
            break;
        }
      });
    } catch (err: unknown) {
      const message = String(err);
      patch({ setup: { type: "failed", data: { message } } });
      set({ notice: message });
    } finally {
      // The framework is the source of truth for the settled setup/service/enabled
      // state (it may have started/stopped the service) — reconcile from it.
      await get().loadPlugins();
    }
  },

  setWebSearch: (on) => set({ webSearch: on }),
  setAgentTools: (on) => set({ agentTools: on }),

  respondToolConsent: async (decision, note) => {
    const pending = get().pendingToolConsent;
    if (!pending) return;
    set({
      pendingToolConsent: null,
      lastRespondedToolConsent: {
        toolCallId: pending.toolCallId,
        toolId: pending.toolId,
        summary: pending.summary,
        decision,
      },
    });
    try {
      await api.respondToolConsent(pending.requestId, decision, note);
    } catch (err: unknown) {
      set({ chatError: String(err) });
    }
  },

  respondClarifyingQuestion: async (answer) => {
    const pending = get().pendingClarifyingQuestion;
    if (!pending) return;
    set({
      pendingClarifyingQuestion: null,
      lastRespondedClarifyingQuestion: { toolCallId: pending.toolCallId, answer },
    });
    try {
      await api.respondClarifyingQuestion(pending.requestId, answer);
    } catch (err: unknown) {
      set({ chatError: String(err) });
    }
  },

  newChat: () => {
    // While a reply is streaming the sidebar is disabled; this guard keeps
    // the in-flight turn safe even if a stray call slips through.
    if (get().streaming) return;
    const { models, settings } = get();
    // Seed the new conversation with its model's curated sampling
    // recommendation (task type is always "general" for now) rather than one
    // flat app-wide default — falls back to the general default itself for an
    // uncatalogued model or nothing selected yet. `ModelSpec.taskDefaults` is
    // a local-catalog-only concept, so cloud mode always gets the general
    // default, same as `ChatToolbar`'s `cloudMode` gate.
    const cloudMode = settings?.chatBackend === "cloud";
    const selectedSpec = cloudMode
      ? undefined
      : models.find((m) => m.spec.id === settings?.selectedModelId)?.spec;
    set({
      active: {
        ...newConversation(),
        sampling: recommendedSampling(selectedSpec, "general", settings?.showThinking ?? true),
      },
      chatError: null,
      view: "chat",
      ...resetPerConversationUiState(get),
    });
  },

  startChatWithDraft: (text) => {
    get().newChat();
    // `newChat()` no-ops (leaving `streaming` unchanged) while a reply is
    // in flight — checking it here, right after, is how we tell whether it
    // actually proceeded without duplicating its own guard.
    if (get().streaming) return;
    set({ draftToPrefill: text });
  },

  openConversation: async (id) => {
    if (get().streaming) return;
    try {
      const active = await api.getConversation(id);
      set({
        active,
        chatError: null,
        view: "chat",
        // Same reasoning as `newChat` — reset per open, never inherited.
        ...resetPerConversationUiState(get),
      });
    } catch (err: unknown) {
      set({ notice: String(err) });
    }
  },

  openConversationAtMessage: async (conversationId, messageId) => {
    await get().openConversation(conversationId);
    // Only set the scroll target if the open actually landed on this
    // conversation — `openConversation` swallows its own errors (e.g. the
    // source conversation was since deleted) rather than throwing, so this
    // guards against requesting a jump inside whatever's active instead.
    if (get().active?.id === conversationId) {
      set({ scrollToMessageId: messageId });
    }
  },

  clearScrollToMessage: () => set({ scrollToMessageId: null }),

  deleteConversation: async (id) => {
    if (get().streaming) return;
    try {
      await api.deleteConversation(id);
      set((s) => {
        const safetyAlerts = { ...s.safetyAlerts };
        delete safetyAlerts[id];
        return {
          conversations: s.conversations.filter((c) => c.id !== id),
          active: s.active?.id === id ? null : s.active,
          safetyAlerts,
          sidebarSearchResults: s.sidebarSearchResults.filter((r) => r.id !== id),
        };
      });
    } catch (err: unknown) {
      set({ notice: String(err) });
    }
  },

  setSidebarSearchQuery: (query) => {
    set({ sidebarSearchQuery: query });
    if (sidebarSearchTimer) clearTimeout(sidebarSearchTimer);
    if (!query.trim()) {
      set({ sidebarSearchResults: [] });
      return;
    }
    sidebarSearchTimer = setTimeout(() => {
      sidebarSearchTimer = null;
      // The query may have changed (or been cleared) while this was in
      // flight — only apply results if they're still for the latest text.
      const inFlightQuery = query;
      void api
        .searchConversations(inFlightQuery)
        .then((results) => {
          if (get().sidebarSearchQuery === inFlightQuery) set({ sidebarSearchResults: results });
        })
        .catch((err: unknown) => set({ notice: String(err) }));
    }, 250);
  },

  loadProjects: async () => {
    try {
      set({ projects: await api.listProjects() });
    } catch {
      // Projects are a nice-to-have; a failed load just leaves the list empty.
    }
  },

  createProject: async (name) => {
    const now = Date.now();
    const project: Project = { id: crypto.randomUUID(), name, createdAtMs: now, updatedAtMs: now };
    try {
      await api.saveProject(project);
      set((s) => ({ projects: [...s.projects, project].sort(byProjectName) }));
      return project;
    } catch (err: unknown) {
      set({ notice: String(err) });
      return null;
    }
  },

  renameProject: async (id, name) => {
    const existing = get().projects.find((p) => p.id === id);
    if (!existing) return;
    const updated: Project = { ...existing, name, updatedAtMs: Date.now() };
    try {
      await api.saveProject(updated);
      set((s) => ({
        projects: s.projects.map((p) => (p.id === id ? updated : p)).sort(byProjectName),
      }));
    } catch (err: unknown) {
      set({ notice: String(err) });
    }
  },

  deleteProject: async (id) => {
    try {
      await api.deleteProject(id);
      set((s) => ({
        projects: s.projects.filter((p) => p.id !== id),
        conversations: s.conversations.map((c) =>
          c.projectId === id ? { ...c, projectId: null } : c,
        ),
        active: s.active?.projectId === id ? { ...s.active, projectId: null } : s.active,
      }));
    } catch (err: unknown) {
      set({ notice: String(err) });
    }
  },

  deleteProjectWithConversations: async (id) => {
    const ids = get()
      .conversations.filter((c) => c.projectId === id)
      .map((c) => c.id);
    await Promise.all(ids.map((conversationId) => get().deleteConversation(conversationId)));
    await get().deleteProject(id);
  },

  moveConversationToProject: async (conversationId, projectId) => {
    try {
      await api.setConversationProject(conversationId, projectId);
      set((s) => ({
        conversations: s.conversations.map((c) =>
          c.id === conversationId ? { ...c, projectId } : c,
        ),
        active: s.active?.id === conversationId ? { ...s.active, projectId } : s.active,
      }));
    } catch (err: unknown) {
      set({ notice: String(err) });
    }
  },

  renameConversation: async (conversationId, title) => {
    try {
      await api.setConversationTitle(conversationId, title);
      set((s) => ({
        conversations: s.conversations.map((c) =>
          c.id === conversationId ? { ...c, title } : c,
        ),
        active: s.active?.id === conversationId ? { ...s.active, title } : s.active,
      }));
    } catch (err: unknown) {
      set({ notice: String(err) });
    }
  },

  setConversationPinned: async (conversationId, pinned) => {
    try {
      await api.setConversationPinned(conversationId, pinned);
      set((s) => ({
        conversations: s.conversations.map((c) =>
          c.id === conversationId ? { ...c, pinned } : c,
        ),
        active: s.active?.id === conversationId ? { ...s.active, pinned } : s.active,
      }));
    } catch (err: unknown) {
      set({ notice: String(err) });
    }
  },

  loadSkills: async () => {
    try {
      set({ skills: await api.listSkills() });
    } catch {
      // Skills are a nice-to-have; a failed load just leaves the list empty.
    }
  },

  saveSkill: async (skill) => {
    try {
      const saved = await api.saveSkill(skill);
      set((s) => ({
        skills: skill.slug
          ? s.skills.map((sk) => (sk.slug === saved.slug ? saved : sk))
          : [...s.skills, saved],
      }));
      return saved;
    } catch (err: unknown) {
      set({ notice: String(err) });
      return null;
    }
  },

  deleteSkill: async (slug) => {
    try {
      await api.deleteSkill(slug);
      set((s) => ({ skills: s.skills.filter((sk) => sk.slug !== slug) }));
    } catch (err: unknown) {
      set({ notice: String(err) });
    }
  },

  // Unlike `saveSkill`/`deleteSkill`, these deliberately let a failure throw
  // instead of swallowing it into `notice` — the Skills view catches it
  // itself and shows it inline next to the option the user picked, since
  // import has real, actionable failure modes (wrong file type, a folder
  // with no SKILL.md) the user needs to see to correct. A `null` return
  // means only "the picker was closed without a choice".
  importSkillFile: async () => {
    const imported = await api.importSkillFile();
    if (imported) set((s) => ({ skills: [...s.skills, imported] }));
    return imported;
  },

  importSkillFolder: async () => {
    const imported = await api.importSkillFolder();
    if (imported) set((s) => ({ skills: [...s.skills, imported] }));
    return imported;
  },

  importSkillZip: async () => {
    const imported = await api.importSkillZip();
    if (imported) set((s) => ({ skills: [...s.skills, imported] }));
    return imported;
  },

  exportSkill: async (slug) => {
    try {
      return await api.exportSkill(slug);
    } catch (err: unknown) {
      set({ notice: String(err) });
      return false;
    }
  },

  updateGeneration: (patch) => {
    const base = get().active ?? newConversation();
    const next: Conversation = { ...base, ...patch, updatedAtMs: Date.now() };
    set({ active: next });

    // Not yet a real, listed conversation (no reply yet) — nothing to persist
    // yet; it'll save naturally with the conversation's first reply.
    if (!next.messages.some((m) => m.role === "assistant")) return;

    if (saveGenerationTimer) clearTimeout(saveGenerationTimer);
    saveGenerationTimer = setTimeout(() => {
      saveGenerationTimer = null;
      void api
        .saveConversation(next)
        .then(async () => set({ conversations: await api.listConversations() }))
        .catch((err: unknown) => set({ notice: String(err) }));
    }, 500);
  },

  send: async (text, attachments) => {
    const trimmed = text.trim();
    // A turn needs either text or at least one attachment (e.g. an image with
    // no caption).
    const atts = attachments?.length ? attachments : undefined;
    if ((!trimmed && !atts) || get().streaming) return;

    // A `/<slug>` prefix invokes a saved skill: its body is folded in ahead of
    // the rest of the user's words — the same way a document attachment is
    // folded in ahead of the user's own text (see `build_api_message` in
    // `crates/core/src/engine.rs`) — while the transcript keeps showing what
    // was actually typed (`displayContent`).
    const invocation = matchSkillInvocation(trimmed, get().skills);

    const base = get().active ?? newConversation();
    const userMessage: ChatMessage = {
      id: crypto.randomUUID(),
      role: "user",
      content: invocation ? invocation.content : trimmed,
      createdAtMs: Date.now(),
      displayContent: invocation ? trimmed : undefined,
      attachments: atts,
    };
    const history = [...base.messages, userMessage];

    // The model actually answering this turn — a cloud model when that's the
    // active backend, else the running engine (with the selected model as a
    // fallback). Stamped onto the assistant message so the UI can attribute
    // each reply.
    const engine = get().engine;
    const settingsNow = get().settings;
    const replyModel =
      settingsNow?.chatBackend === "cloud"
        ? (settingsNow.selectedCloudModelId ?? undefined)
        : engine.type === "running"
          ? engine.data.modelId
          : (settingsNow?.selectedModelId ?? undefined);

    const conversation: Conversation = {
      ...base,
      title:
        base.messages.length === 0
          ? titleFrom(trimmed || atts?.[0]?.name || "")
          : base.title,
      updatedAtMs: Date.now(),
      // Empty assistant message = the streaming placeholder the UI fills in.
      messages: [
        ...history,
        {
          id: crypto.randomUUID(),
          role: "assistant",
          content: "",
          createdAtMs: Date.now(),
          model: replyModel,
        },
      ],
    };
    set({ active: conversation, streaming: true, chatError: null, liveActivity: null });

    // Best-effort safety check on the user's own words (not a `/slug`
    // invocation's expanded skill body) — never blocks or errors out the
    // chat. Must run — and be awaited — here, before `api.chatStream`
    // below, not after the reply settles: the engine has a single
    // inference slot, so `classify_risk` silently no-ops (returns `None`)
    // whenever a real chat turn is already generating (see
    // `EngineManager::raw_classify` in `crates/core/src/engine.rs`).
    // Running it first also means someone sending a message that needs
    // this doesn't have to wait through a possibly long reply to see it.
    // `streaming` already blocks a second `send()` call for the whole
    // duration of this one, so unlike a check made after settling, this
    // can't race a later turn's classification — always safe to just set
    // the result. Front-loading the settings/backend check is purely an
    // optimization (skip the IPC round trip when nothing would use the
    // result); `classify_risk` re-checks both itself. Only a "risk" result
    // touches `safetyAlerts` — see that field's doc comment for why a
    // later "safe" turn never clears it.
    const textToClassify = userMessage.displayContent ?? userMessage.content;
    if (
      textToClassify.trim() !== "" &&
      get().settings?.riskDetectionEnabled !== false &&
      get().settings?.chatBackend !== "cloud"
    ) {
      try {
        const signal = await api.classifyRisk(textToClassify);
        if (signal === "risk") {
          const turn = history.length;
          set((s) => ({
            safetyAlerts: {
              ...s.safetyAlerts,
              [conversation.id]: { turn, dismissedTurn: null },
            },
          }));
        }
      } catch {
        // A missed safety check must never interrupt or error the chat.
      }
    }

    return await runGeneration(set, get, conversation, history);
  },

  // Editing a past user turn rewinds the conversation to just before it,
  // then resends — dropping that turn's old reply and everything after it,
  // exactly like asking the question again with new wording. Unlike a plain
  // resend, the dropped tail isn't lost: it's kept as a sibling variant in
  // `branches` (see `pushVariant`'s doc comment), navigable via the arrows
  // `ChatView` renders next to the edited message.
  editMessage: async (index, text) => {
    const trimmed = text.trim();
    const base = get().active;
    if (!base || get().streaming) return;
    const target = base.messages[index];
    if (target?.role !== "user") return;
    // Carry the original turn's attachments through the edit, so editing the
    // text of an image/document turn doesn't silently drop the files — and an
    // image-only turn stays sendable with no text.
    const atts = target.attachments;
    if (!trimmed && !atts?.length) return;

    const anchorKey = branchAnchorKey(base.messages, index);
    const branchesWithOldTail = pushVariant(base.branches, anchorKey, base.messages.slice(index));

    set((s) => {
      // Dropping every message from `index` on can drop the very turn a
      // stale `safetyAlerts` entry points at — prune it so a resend starts
      // from a clean slate rather than showing (or hiding) a banner tied to
      // content that no longer exists in this conversation. `send` below
      // sets a fresh entry itself if the resend turns out to be risky too.
      const safetyAlerts = { ...s.safetyAlerts };
      delete safetyAlerts[base.id];
      return {
        active: {
          ...base,
          messages: base.messages.slice(0, index),
          branches: branchesWithOldTail,
        },
        chatError: null,
        safetyAlerts,
      };
    });
    const settledBase = await get().send(trimmed, atts);

    // `send()` already saved the resent conversation — this second, cheap
    // save folds the newly-generated tail into the same branch entry as the
    // discarded one above, so the arrow nav can switch between them. Done
    // as a follow-up rather than threading a `finalize` hook through the
    // public `send()` action, to keep `send()`'s own body (shared with the
    // plain composer path) untouched by edit-specific bookkeeping.
    //
    // Uses `send()`'s own return value (this specific turn's settled
    // result) rather than re-reading `get().active` — the latter could, by
    // the time this line runs, already reflect a LATER action (e.g. the
    // user firing `retryMessage`/`setActiveVariant` in the brief window
    // after `send()` flips `streaming` back to `false` but before its own
    // trailing save finishes), which would mislabel that other action's
    // result as this edit's new tail.
    if (!settledBase || settledBase.id !== base.id) return;
    const newTail = settledBase.messages.slice(index);
    if (newTail.length === 0) return;
    const finalConversation: Conversation = {
      ...settledBase,
      branches: pushVariant(settledBase.branches, anchorKey, newTail),
      updatedAtMs: Date.now(),
    };
    set((s) => ({ active: s.active?.id === finalConversation.id ? finalConversation : s.active }));
    try {
      await api.saveConversation(finalConversation);
      set({ conversations: await api.listConversations() });
    } catch {
      // Saving branch history must never interrupt the chat.
    }
  },

  retryMessage: async (index) => {
    const base = get().active;
    if (!base || get().streaming) return;
    if (base.messages[index]?.role !== "assistant") return;

    const anchorKey = branchAnchorKey(base.messages, index);
    const branchesWithOldTail = pushVariant(base.branches, anchorKey, base.messages.slice(index));
    // Unlike `send`, no new user message — regenerating a reply for the
    // same, unchanged preceding turn.
    const history = base.messages.slice(0, index);

    const engine = get().engine;
    const settingsNow = get().settings;
    const replyModel =
      settingsNow?.chatBackend === "cloud"
        ? (settingsNow.selectedCloudModelId ?? undefined)
        : engine.type === "running"
          ? engine.data.modelId
          : (settingsNow?.selectedModelId ?? undefined);

    const conversation: Conversation = {
      ...base,
      branches: branchesWithOldTail,
      updatedAtMs: Date.now(),
      messages: [
        ...history,
        {
          id: crypto.randomUUID(),
          role: "assistant",
          content: "",
          createdAtMs: Date.now(),
          model: replyModel,
        },
      ],
    };
    set({ active: conversation, streaming: true, chatError: null, liveActivity: null });

    // Folding the new tail into `branches` via `finalize` (rather than a
    // second save, unlike `editMessage`) is possible here because this
    // action calls `runGeneration` directly instead of going through the
    // public `send()` action.
    await runGeneration(set, get, conversation, history, (settled) => {
      if (settled.messages.length === index) {
        // The regeneration produced nothing at all (no tokens, thinking,
        // steps, or artifacts) — `runGeneration`'s settle step already
        // popped the empty placeholder, which would otherwise leave the
        // reply that was showing before Retry was clicked simply gone,
        // with no message row left for the arrow nav to attach to. Splice
        // the old tail (already recorded in `branchesWithOldTail` above)
        // back in rather than leaving that gap; any stream error is still
        // surfaced separately via `chatError`.
        return {
          ...settled,
          messages: [...settled.messages, ...base.messages.slice(index)],
          branches: branchesWithOldTail,
        };
      }
      const newTail = settled.messages.slice(index);
      return { ...settled, branches: pushVariant(settled.branches, anchorKey, newTail) };
    });
  },

  forkConversation: async (index) => {
    const base = get().active;
    if (!base || get().streaming) return;
    if (base.messages[index]?.role !== "assistant") return;

    const now = Date.now();
    const forked: Conversation = {
      ...base,
      id: crypto.randomUUID(),
      createdAtMs: now,
      updatedAtMs: now,
      // Artifact chips reference files stored under the SOURCE
      // conversation's id (`crates/core/src/artifacts.rs`'s `get` looks
      // them up by exactly that `(conversationId, artifactId)` pair, and
      // `api.getArtifact` is always called with the *current* conversation's
      // id) — they don't travel with a fork, so strip them here rather than
      // leave a chip that fails to open/download in the new conversation.
      // `attachments` are untouched: those are self-contained inline data
      // (`dataUri`/`text`), not file-referenced, so they copy correctly.
      messages: base.messages
        .slice(0, index + 1)
        .map((m) => (m.artifacts ? { ...m, artifacts: undefined } : m)),
      // A fork starts clean — it doesn't inherit the source conversation's
      // edit/retry history, only its currently-active content up to here.
      branches: {},
      // Lets the UI show a "forked from another conversation" hint anchored
      // to the last copied message — see `ForkOrigin`'s doc comment.
      forkedFrom: { conversationId: base.id, messageId: base.messages[index].id },
      pinned: false,
      projectId: undefined,
    };
    try {
      await api.saveConversation(forked);
      const conversations = await api.listConversations();
      // Only switch the visible conversation to the fork if the user
      // hasn't already navigated away from `base` while the save/list round
      // trip above was in flight — mirrors the same cross-await `active`
      // guard `runGeneration`/`editMessage` already use; still refreshes
      // the sidebar list either way, since the fork now genuinely exists.
      const stillOnSource = get().active?.id === base.id;
      set({
        conversations,
        ...(stillOnSource
          ? {
              active: forked,
              chatError: null,
              view: "chat",
              // Same reasoning as `newChat`/`openConversation` — reset per
              // fork, never inherited from the source conversation.
              ...resetPerConversationUiState(get),
            }
          : {}),
      });
    } catch (err: unknown) {
      set({ notice: String(err) });
    }
  },

  setActiveVariant: async (anchorKey, variantIndex) => {
    const base = get().active;
    if (!base || get().streaming) return;
    const group = base.branches[anchorKey];
    if (!group || variantIndex < 0 || variantIndex >= group.variants.length) return;

    // The anchor's position in `messages` isn't fixed — switching an outer
    // branch can shift where an inner one currently sits — so it's resolved
    // by id every time, not assumed from a stored index.
    let branchIndex: number;
    if (anchorKey === ROOT_BRANCH_KEY) {
      branchIndex = 0;
    } else {
      const anchorIndex = base.messages.findIndex((m) => m.id === anchorKey);
      if (anchorIndex === -1) return;
      branchIndex = anchorIndex + 1;
    }

    const messages = [...base.messages.slice(0, branchIndex), ...group.variants[variantIndex]];
    const settled: Conversation = {
      ...base,
      messages,
      branches: { ...base.branches, [anchorKey]: { ...group, active: variantIndex } },
      updatedAtMs: Date.now(),
    };
    set({ active: settled });
    try {
      await api.saveConversation(settled);
      set({ conversations: await api.listConversations() });
    } catch {
      // Saving branch navigation must never interrupt the chat.
    }
  },

  stopGeneration: async () => {
    // Only the chat Stop button comes through here (Translate/Correct/Improve
    // call the api directly). The backend ends a stopped reply with a normal
    // `done`, so remember the press — see where `runGeneration` announces
    // "Answer ready", and resets this for the next turn.
    stopRequestedForTurn = true;
    try {
      await api.stopGeneration();
    } catch (err: unknown) {
      // The stop didn't happen, so the reply will run to its end.
      stopRequestedForTurn = false;
      throw err;
    }
  },

  dismissSafetyAlert: (conversationId) => {
    set((s) => {
      const entry = s.safetyAlerts[conversationId];
      if (!entry) return {};
      return {
        safetyAlerts: {
          ...s.safetyAlerts,
          [conversationId]: { ...entry, dismissedTurn: entry.turn },
        },
      };
    });
  },

  completeOnboarding: async () => {
    try {
      const settings = await api.getSettings();
      const next = { ...settings, onboardingCompleted: true };
      await api.updateSettings(next);
      set({ settings: next });
    } catch (err: unknown) {
      set({ notice: String(err) });
    }
  },

  saveSettings: async (patch) => {
    try {
      const current = get().settings ?? (await api.getSettings());
      const next = { ...current, ...patch };
      await api.updateSettings(next);
      set({ settings: next });
      // diskOffload changes a model's fit server-side (selection::fit_for) —
      // refetch so the Library view's badges/availability stay live. Other
      // settings (temperature, voice sliders, ...) don't affect fit, so this
      // stays targeted rather than refreshing on every save.
      if ("diskOffload" in patch) {
        void get().refreshModels();
      }
      // Same resync `loadPlugins` does for `webSearch`: turning agent mode
      // off in Settings turns it off for the current chat too, and turning
      // it on defaults the current chat on.
      if ("agentModeEnabled" in patch) {
        set({ agentTools: agentToolsAvailable(next) });
      }
      // Experimental features gate experimental plugins server-side
      // (`list_plugins` omits them while they're off), so every surface
      // gated on one follows this reload. Turning them back on restores
      // each as it was: an enabled one whose service was stopped by turning
      // them off gets it started again through the same path the Plugins
      // switch uses.
      if (
        "experimentalFeatures" in patch &&
        patch.experimentalFeatures !== current.experimentalFeatures
      ) {
        await get().loadPlugins();
        if (next.experimentalFeatures) {
          for (const p of get().plugins) {
            if (p.experimental && p.enabled && p.service.type === "stopped") {
              void get().setPluginEnabled(p.id, true);
            }
          }
        }
      }
    } catch (err: unknown) {
      set({ notice: String(err) });
    }
  },

  setShowThinking: async (on) => {
    if (get().settings?.showThinking === on) return;
    // Persist the choice. Hiding thinking from new replies takes effect right
    // away (the chat stream stops accumulating it — see send()); the engine
    // only stops *producing* thinking the next time the model loads. We never
    // restart a running model from a settings toggle, so an in-flight reply is
    // never truncated as a side effect of flipping this switch.
    await get().saveSettings({ showThinking: on });
  },

  setTheme: async (theme) => {
    const before = get().settings;
    // The controls only exist once settings have loaded. Re-picking the theme
    // already in force (a missing or unrecognised stored value counts as
    // System) has nothing to change or write.
    if (!before || normalizeTheme(before.theme) === theme) return;
    const mine = ++themeWriteSeq;
    // Optimistic on purpose: the subscriber below applies it inside this `set`,
    // and every later `saveSettings` merges onto a base that already carries it
    // — Settings' draft save on close can't write the old theme back.
    set({ settings: { ...before, theme } });
    try {
      await api.updateSettings({ ...(get().settings ?? before), theme });
    } catch (err: unknown) {
      if (mine === themeWriteSeq) {
        const now = get().settings;
        if (now) set({ settings: { ...now, theme: before.theme } });
      }
      set({ notice: String(err) });
    }
  },

  setDesktopNotifications: async (on) => {
    const before = get().settings;
    // The switch only exists once settings have loaded; a missing field
    // (a row saved before it existed) counts as off, so choosing "off" again
    // has nothing to write.
    if (!before || (before.desktopNotifications === true) === on) return;
    if (desktopNotificationsInFlight === 0) {
      desktopNotificationsOnDisk = before.desktopNotifications === true;
    }
    desktopNotificationsInFlight++;
    const mine = ++desktopNotificationsWriteSeq;
    set({ settings: { ...before, desktopNotifications: on } });
    try {
      await api.updateSettings({ ...(get().settings ?? before), desktopNotifications: on });
      desktopNotificationsOnDisk = on;
    } catch (err: unknown) {
      if (mine === desktopNotificationsWriteSeq) {
        const now = get().settings;
        if (now) set({ settings: { ...now, desktopNotifications: desktopNotificationsOnDisk } });
      }
      set({ notice: String(err) });
      return;
    } finally {
      desktopNotificationsInFlight--;
    }
    // The switch is saved first so a slow or never-answered prompt can't hold
    // the choice hostage; the prompt (macOS) then follows the user's own click,
    // and a "no" in it is what puts up Settings' "blocked" line. Not if the
    // user has already turned it off again while the write was in flight: a
    // prompt for a switch that is off would be a surprise.
    if (on && get().settings?.desktopNotifications === true) {
      await get().requestNotificationPermission();
    }
  },

  checkForUpdate: async (manual = false) => {
    if (get().updateStatus === "checking" || get().updateStatus === "downloading") return;
    set({ updateStatus: "checking", updateError: null });
    try {
      const update = await api.checkForUpdate();
      if (!update) {
        set({ updateStatus: "idle", availableUpdate: null });
        return;
      }
      const skipped = get().settings?.skippedUpdateVersion;
      if (!manual && skipped && skipped === update.version) {
        set({ updateStatus: "idle", availableUpdate: null });
        return;
      }
      set({ updateStatus: "available", availableUpdate: update });
    } catch (err: unknown) {
      // A silent auto-check must never surface an error dialog on its own —
      // only a manual "Check for updates now" does.
      set(manual ? { updateStatus: "error", updateError: String(err) } : { updateStatus: "idle" });
    }
  },

  startUpdateInstall: async () => {
    if (get().updateStatus === "downloading") return;
    updateRestarting = false;
    set({
      updateStatus: "downloading",
      updateProgress: { receivedBytes: 0, totalBytes: 0, bytesPerSec: 0, etaSeconds: 0 },
      updateError: null,
    });
    try {
      await api.installUpdate((event) => {
        if (event.type === "started") {
          set({
            updateProgress: {
              receivedBytes: event.data.resumedFromBytes,
              totalBytes: event.data.totalBytes,
              bytesPerSec: 0,
              etaSeconds: 0,
            },
          });
        } else if (event.type === "progress") {
          set({ updateProgress: { ...event.data } });
        } else if (event.type === "finished") {
          updateRestarting = true;
        } else if (event.type === "failed") {
          updateRestarting = false;
          set({ updateStatus: "error", updateError: event.data.message });
        }
      });
      // Success means the app is restarting or exiting on its own — this
      // frontend instance is about to disappear, nothing further to do.
    } catch (err: unknown) {
      updateRestarting = false;
      set({ updateStatus: "error", updateError: String(err) });
    }
  },

  skipUpdateVersion: async () => {
    const version = get().availableUpdate?.version;
    set({ updateStatus: "idle", availableUpdate: null });
    if (version) {
      await get().saveSettings({ skippedUpdateVersion: version });
    }
  },

  dismissUpdate: () => set({ updateStatus: "idle", availableUpdate: null, updateError: null }),

  checkForEngineUpdate: async (manual = false) => {
    const status = get().engineUpdateStatus;
    if (status === "checking" || status === "downloading") return;
    set({ engineUpdateStatus: "checking", engineUpdateError: null });
    try {
      const update = await api.checkForEngineUpdate();
      if (!update) {
        set({ engineUpdateStatus: "idle", availableEngineUpdate: null });
        return;
      }
      const skipped = get().settings?.skippedEngineTag;
      if (!manual && skipped && skipped === update.tag) {
        set({ engineUpdateStatus: "idle", availableEngineUpdate: null });
        return;
      }
      set({ engineUpdateStatus: "available", availableEngineUpdate: update });
    } catch (err: unknown) {
      // A silent auto-check must never surface an error on its own — only a
      // manual "Check for updates now" does.
      set(
        manual
          ? { engineUpdateStatus: "error", engineUpdateError: String(err) }
          : { engineUpdateStatus: "idle" },
      );
    }
  },

  startEngineUpdateInstall: async () => {
    if (get().engineUpdateStatus === "downloading") return;
    set({
      engineUpdateStatus: "downloading",
      engineUpdateProgress: { receivedBytes: 0, totalBytes: 0, bytesPerSec: 0, etaSeconds: 0 },
      engineUpdateError: null,
    });
    try {
      await api.installEngineUpdate((event) => {
        if (event.type === "started") {
          set({
            engineUpdateProgress: {
              receivedBytes: event.data.resumedFromBytes,
              totalBytes: event.data.totalBytes,
              bytesPerSec: 0,
              etaSeconds: 0,
            },
          });
        } else if (event.type === "progress") {
          set({ engineUpdateProgress: { ...event.data } });
        } else if (event.type === "failed") {
          set({ engineUpdateStatus: "error", engineUpdateError: event.data.message });
        }
      });
      set({ engineUpdateStatus: "installed", availableEngineUpdate: null });
    } catch (err: unknown) {
      set({ engineUpdateStatus: "error", engineUpdateError: String(err) });
    }
  },

  skipEngineUpdateTag: async () => {
    const tag = get().availableEngineUpdate?.tag;
    set({ engineUpdateStatus: "idle", availableEngineUpdate: null });
    if (tag) {
      await get().saveSettings({ skippedEngineTag: tag });
    }
  },

  dismissEngineUpdate: () =>
    set({ engineUpdateStatus: "idle", availableEngineUpdate: null, engineUpdateError: null }),

  refreshAccount: async () => {
    try {
      const account = await api.refreshAccount();
      set({ account });
    } catch {
      // Silent — offline or no stored session is an expected, quiet outcome;
      // only an explicit `signIn` surfaces an error to the user.
    }
  },

  signIn: async () => {
    set({ signInOpen: true, signInError: null });
    try {
      await api.signIn((event) => {
        if (event.type === "awaitingBrowser") {
          void api.openLink(event.data.verificationUrl);
        } else if (event.type === "complete") {
          set({ account: event.data.account, signInOpen: false, signInError: null });
          // The sign-in itself succeeded either way — this is a heads-up
          // about *next* launch, not an error to retry.
          if (!event.data.persisted) {
            set({
              notice:
                "Signed in, but couldn't save your session securely, so you'll need to sign in again next time you open OpenPal.",
            });
          }
        } else if (event.type === "failed") {
          set({ signInError: event.data.message });
        } else if (event.type === "expired") {
          set({ signInError: "That sign-in link expired. Please try again." });
        }
      });
    } catch (err: unknown) {
      set({ signInError: String(err) });
    }
  },

  cancelSignIn: async () => {
    set({ signInOpen: false, signInError: null });
    await api.cancelSignIn();
  },

  signOut: async () => {
    set({ account: null });
    await api.signOut();
  },

  startAddDevice: async () => {
    set({
      addDeviceOpen: true,
      addDeviceStatus: "listening",
      addDeviceIncoming: null,
      addDeviceBusy: false,
      addDeviceError: null,
    });
    try {
      await api.syncPairListen((event) => {
        if (event.type === "incomingRequest") {
          set({ addDeviceStatus: "incoming", addDeviceIncoming: event.data });
        } else if (event.type === "expired") {
          set({ addDeviceStatus: "expired" });
        } else if (event.type === "failed") {
          set({ addDeviceStatus: "error", addDeviceError: event.data.message });
        }
        // Every other variant (awaitingChallenge/awaitingConfirmation/
        // readyToInstall/complete/rejected) belongs to Device B's side of
        // the protocol — `sync_pair_listen` never emits them.
      });
    } catch (err: unknown) {
      set({ addDeviceStatus: "error", addDeviceError: String(err) });
    }
  },

  respondAddDevice: async (approve) => {
    if (get().addDeviceBusy) return;
    set({ addDeviceBusy: true });
    try {
      const event = await api.syncPairConfirm(approve);
      if (event.type === "failed") {
        set({ addDeviceBusy: false, addDeviceStatus: "error", addDeviceError: event.data.message });
      } else if (event.type === "expired") {
        set({ addDeviceBusy: false, addDeviceStatus: "expired" });
      } else {
        // "complete" or "rejected" — either way this session is over.
        set({ addDeviceOpen: false, addDeviceBusy: false, addDeviceIncoming: null });
      }
    } catch (err: unknown) {
      set({ addDeviceBusy: false, addDeviceStatus: "error", addDeviceError: String(err) });
    }
  },

  closeAddDevice: async () => {
    set({ addDeviceOpen: false, addDeviceIncoming: null, addDeviceBusy: false, addDeviceError: null });
    await api.syncPairCancel();
  },

  openJoinGroup: () => {
    set({
      joinGroupOpen: true,
      joinGroupStatus: "form",
      joinGroupFingerprint: null,
      joinGroupBusy: false,
      joinGroupError: null,
    });
  },

  submitJoinGroup: async (groupSecret, deviceLabel) => {
    set({ joinGroupStatus: "awaitingChallenge", joinGroupFingerprint: null, joinGroupError: null });
    try {
      await api.syncPairRequest(groupSecret, deviceLabel, (event) => {
        if (event.type === "awaitingChallenge") {
          set({ joinGroupStatus: "awaitingChallenge" });
        } else if (event.type === "awaitingConfirmation") {
          set({ joinGroupStatus: "awaitingConfirmation", joinGroupFingerprint: event.data.fingerprint });
        } else if (event.type === "readyToInstall") {
          set({ joinGroupStatus: "readyToInstall" });
        } else if (event.type === "expired") {
          set({ joinGroupStatus: "expired" });
        } else if (event.type === "failed") {
          set({ joinGroupStatus: "error", joinGroupError: event.data.message });
        }
        // "complete"/"rejected" belong to Device A's side (`sync_pair_confirm`'s
        // return value) — `sync_pair_request` never emits them.
      });
    } catch (err: unknown) {
      set({ joinGroupStatus: "error", joinGroupError: String(err) });
    }
  },

  finishJoinGroup: async (approve) => {
    if (get().joinGroupBusy) return;
    set({ joinGroupBusy: true });
    try {
      const event = await api.syncPairFinish(approve);
      if (event.type === "failed") {
        set({ joinGroupBusy: false, joinGroupStatus: "error", joinGroupError: event.data.message });
      } else if (event.type === "expired") {
        set({ joinGroupBusy: false, joinGroupStatus: "expired" });
      } else {
        // "complete" or "rejected" — either way this session is over.
        set({ joinGroupOpen: false, joinGroupBusy: false, joinGroupFingerprint: null });
      }
    } catch (err: unknown) {
      set({ joinGroupBusy: false, joinGroupStatus: "error", joinGroupError: String(err) });
    }
  },

  closeJoinGroup: async () => {
    set({ joinGroupOpen: false, joinGroupFingerprint: null, joinGroupBusy: false, joinGroupError: null });
    await api.syncPairCancel();
  },
}));

// Mirrors `settings.theme` onto <html data-theme> in the same tick as every
// `set()` that changes it — boot (all three entry points), the theme pickers,
// a post-download re-read of the settings — so no path can paint a frame in the
// wrong theme and nothing relies on a React effect running before paint.
// Zustand calls listeners synchronously inside `set()`, before React can render
// (React only schedules its own re-render from there), and doesn't catch a
// throwing one — hence `applyTheme` being total. The comparison keeps this to
// one cheap check on the many updates that don't touch the theme.
useStore.subscribe((state, prev) => {
  if (state.settings?.theme !== prev.settings?.theme) applyTheme(state.settings?.theme);
});

// Announces "model loaded" / "model unloaded". Listening here rather than in
// each writer of `engine` (boot, the 2s poll, useModel, stopModel, deleteModel)
// means no path can forget to, and none can announce twice: they all end up as
// one more status handed to the same tracker, which reports transitions, not
// statuses (see `lib/engineEvents.ts`). Registered at module scope, never in a
// React effect: StrictMode runs effects twice and would double every alert.
// The body is wrapped because zustand doesn't catch a throwing listener, and a
// throw here would break whichever `set()` caused it.
useStore.subscribe((state, prev) => {
  try {
    if (state.engine !== prev.engine || state.models !== prev.models) {
      const st = state.engine;
      if (st.type === "running" || st.type === "starting") {
        const name = engineModelName(state.models, st.data.modelId);
        if (name) engineNames.set(st.data.modelId, name);
      }
    }
    if (state.engine !== prev.engine) {
      const events = stepEngineTracker(engineTracker, state.engine, {
        busy: state.engineBusy,
        suppress:
          state.appInfo?.isAndroid === true || state.settings === null || updateRestarting,
      });
      for (const e of events) announceAppEvent(useStore.getState, engineAppEvent(state, e));
    }
    // A start/stop call has ended, so a load still being waited on can't
    // complete any more (a final read that came back `stopped`).
    if (prev.engineBusy && !state.engineBusy) engineTracker.loading = null;
  } catch {
    // See above.
  }
});
