/**
 * Typed wrappers around every Tauri command. The rest of the UI never calls
 * `invoke` directly — this file is the single seam between TS and Rust.
 */

import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  Account,
  AccountEvent,
  AppInfo,
  AppLockPinCheckResult,
  AppLockStatusView,
  Artifact,
  Attachment,
  AvailableUpdate,
  ChatEvent,
  ChatMessage,
  CloudModelInfo,
  ConsentDecision,
  Conversation,
  ConversationMeta,
  ConversationSearchResult,
  DeviceSummary,
  DiscoveredHost,
  DiskSpeedInfo,
  DownloadEvent,
  EnhanceOutcome,
  EngineStatus,
  EngineUpdateInfo,
  ExportCategories,
  ExportCounts,
  ExportSummary,
  HfListing,
  HfModelDetails,
  HfQuantPicker,
  HfSearchPage,
  HfSearchQuery,
  ImportSummary,
  InjectionReviewDecision,
  ModelInfo,
  NotificationClick,
  NotificationPermission,
  ParentalConfigInput,
  ParentalSettingsView,
  ParentalStatus,
  PairedDeviceInfo,
  PairingEvent,
  PinCheckResult,
  PluginEvent,
  PluginInfo,
  Project,
  RemoteAccessStatusInfo,
  RemotePairingRecord,
  RiskSignal,
  SamplingParams,
  Settings,
  Skill,
  SyncPairingEvent,
  SystemReport,
  TaskType,
  ToolGrant,
  VoiceInfo,
  WolStatus,
} from "./types";

export const api = {
  // System
  getSystemReport: () => invoke<SystemReport>("get_system_report"),
  /** Portable-build flag + resolved data dir (hides the Plugins tab; shown in Settings). */
  getAppInfo: () => invoke<AppInfo>("get_app_info"),

  // Encryption recovery (the one-time `RecoverySetup` gate)
  /** Mint a fresh recovery code for the encryption key that's already been
   * silently generated for this install. Pure/local — no state changes until
   * `confirmRecoveryCode`. */
  generateRecoveryCode: () => invoke<string>("generate_recovery_code"),
  /** Seals a second copy of the encryption key under `code` — the recovery
   * code the user just confirmed they saved — clearing
   * `AppInfo.needsRecoverySetup`. There's no server-side check that `code`
   * matches what `generateRecoveryCode` returned; whatever string is passed
   * here is what gets sealed under, so the caller must pass back exactly
   * what it showed the user. Rejects with a message safe to show as-is on
   * failure — the caller should let the user retry rather than silently
   * proceeding. */
  confirmRecoveryCode: (code: string) => invoke<void>("confirm_recovery_code", { code }),
  /** Try to unlock with a recovery code typed back in by the user, from the
   * `RecoveryUnlock` gate (`AppInfo.needsRecovery`) — shown when the OS
   * keychain that normally unlocks the encryption key silently failed.
   * Rejects with a message safe to show as-is on a wrong code; the caller
   * should let the user retry rather than clearing their input. */
  attemptRecoveryUnlock: (code: string) => invoke<void>("attempt_recovery_unlock", { code }),

  // App lock (the optional launch-time PIN gate, `AppInfo.needsAppUnlock`)
  /** Whether app-lock is currently enabled. No PIN needed to check. */
  getAppLockStatus: () => invoke<AppLockStatusView>("get_app_lock_status"),
  /** Check a PIN typed into `AppLockGate`'s pad. Rate-limited server-side,
   * same shape as `verifyParentalPin` — see `AppLockPinCheckResult`. */
  verifyAppLockPin: (pin: string) =>
    invoke<AppLockPinCheckResult>("verify_app_lock_pin", { pin }),
  /** Turn app-lock on/off or change its PIN. `currentPin` must be provided
   * (and correct) whenever a PIN is already set — including to turn the
   * feature back off; pass `null` for `newPin` when `enabled` is false. */
  setAppLockPin: (currentPin: string | null, enabled: boolean, newPin: string | null) =>
    invoke<void>("set_app_lock_pin", { currentPin, enabled, newPin }),
  /** Turn app-lock off entirely using the recovery code instead of the PIN —
   * the `AppLockGate` "recovery" stage. Unlike a successful PIN check, this
   * doesn't leave the PIN in place; it's cleared server-side so the user
   * isn't locked out again by a PIN they can't remember. */
  unlockAppLockWithRecoveryCode: (code: string) =>
    invoke<void>("unlock_app_lock_with_recovery_code", { code }),
  /** Erases every conversation, project, setting, and skill (downloaded
   * models are kept) and relaunches the app. The whole process exits on
   * success, so this promise only ever resolves by rejecting — a network/IPC
   * failure before the backend even attempts the reset. */
  factoryReset: () => invoke<void>("factory_reset"),

  // Models
  getModels: () => invoke<ModelInfo[]>("get_models"),
  downloadModel: (modelId: string, onEvent: (e: DownloadEvent) => void) => {
    const channel = new Channel<DownloadEvent>();
    channel.onmessage = onEvent;
    return invoke<void>("download_model", { modelId, onEvent: channel });
  },
  cancelDownload: (modelId: string) =>
    invoke<void>("cancel_download", { modelId }),
  /** Measure the model install volume's speed and estimate this model's
   * tok/s if it streamed from disk — shown in the "run from disk" download
   * confirmation dialog. */
  probeDiskSpeed: (modelId: string) =>
    invoke<DiskSpeedInfo>("probe_disk_speed", { modelId }),
  deleteModel: (modelId: string) => invoke<void>("delete_model", { modelId }),
  /** Star/unstar a model as a favorite, from the Library page or the
   * composer's model quick-switcher. Device-local. */
  setModelFavorite: (modelId: string, favorite: boolean) =>
    invoke<void>("set_model_favorite", { modelId, favorite }),
  /** One-time, best-effort backfill for Hugging Face custom models added
   * before OpenPal tracked group/license/release-date metadata — re-fetches
   * and fills them in for anything still missing it. Naturally idempotent
   * (a second call finds nothing left to do), so it's safe to call on every
   * boot rather than needing a real migration. Resolves to how many models
   * were updated. */
  backfillCustomModelMetadata: () => invoke<number>("backfill_custom_model_metadata"),
  // Custom models (Advanced settings)
  listHfModels: (query: string) => invoke<HfListing>("list_hf_models", { query }),
  addHfModel: (repoId: string, revision: string, filePath: string) =>
    invoke<string>("add_hf_model", { repoId, revision, filePath }),
  // Hugging Face hub browser (the "hf-browser" plugin's Browse view). Search/
  // trending, model card, and README are read-only hub lookups; adding a
  // picked file still goes through addHfModel above.
  searchHfHub: (query: HfSearchQuery) =>
    invoke<HfSearchPage>("search_hf_hub", { query }),
  getHfModelDetails: (repoId: string) =>
    invoke<HfModelDetails>("get_hf_model_details", { repoId }),
  getHfModelReadme: (repoId: string, revision: string) =>
    invoke<string>("get_hf_model_readme", { repoId, revision }),
  /** The repo's files/quants, each decorated with a fit verdict for this Mac. */
  getHfQuantPicker: (repoId: string, revision: string) =>
    invoke<HfQuantPicker>("get_hf_quant_picker", { repoId, revision }),
  /** Opens a native picker; resolves to the new model id, or null if the user cancelled. */
  addLocalModel: () => invoke<string | null>("add_local_model"),
  /**
   * Open a native picker for chat attachments. Images are offered only when
   * `allowImages` (a vision model is active); documents (PDF/text/code) always.
   * Resolves to the attachments that could be read (empty if cancelled).
   * `prescan` starts the prompt-injection check of the documents right away —
   * only for a chat whose next turn is checked (agent tools or web search on).
   */
  pickAttachments: (allowImages: boolean, prescan: boolean) =>
    invoke<Attachment[]>("pick_attachments", { allowImages, prescan }),
  openModelsFolder: () => invoke<void>("open_models_folder"),
  openLink: (url: string) => invoke<void>("open_link", { url }),
  /**
   * A web-search source's favicon as a `data:` URI, or null if the site has no
   * usable icon. Fetched by Rust because the webview can't load remote images
   * (CSP) and only the source's own origin is contacted.
   */
  fetchFavicon: (url: string) =>
    invoke<string | null>("fetch_favicon", { url }),
  /** Copy text to the OS clipboard via Rust (works on macOS, Windows, Linux). */
  copyText: (text: string) => invoke<void>("copy_text", { text }),

  // Engine
  startEngine: (modelId: string, contextLength: number) =>
    invoke<void>("start_engine", { modelId, contextLength }),
  stopEngine: () => invoke<void>("stop_engine"),
  getEngineStatus: () => invoke<EngineStatus>("get_engine_status"),
  /** Reset the auto-unload idle clock to now — same effect as any other
   * activity. Backs AutoUnloadTooltip's "Cancel" button
   * and the composer's typing-pauses-the-countdown behavior. */
  snoozeAutoUnload: () => invoke<void>("snooze_auto_unload"),
  /** Subscribe to Rust's "the engine's state may have changed" nudge — sent
   * when the idle loop auto-unloads the model, which nothing in the webview
   * would otherwise notice until its next 2s poll (and a hidden window's
   * timers are throttled). It carries no payload on purpose: the caller
   * re-reads `getEngineStatus` and diffs, so this and the poll can never
   * disagree. Resolves to the unsubscribe function; resolves to a no-op where
   * there is no event bridge (the Playwright mock, jsdom) instead of rejecting. */
  onEngineStatusChanged: async (cb: () => void): Promise<() => void> => {
    try {
      return await listen("engine-status-changed", () => cb());
    } catch {
      return () => {};
    }
  },

  // Notifications
  /** Whether the OS currently lets OpenPal post notifications. Never prompts.
   * "denied" (only macOS can say so today) is what puts up Settings' "blocked"
   * line; see `NotificationPermission` for the other answers. */
  notificationPermission: () =>
    invoke<NotificationPermission>("notification_permission"),
  /** Ask the OS to let OpenPal post notifications, and resolve to where that
   * left us. On macOS this is what shows the system's one-time "Allow
   * notifications?" prompt, and it resolves once the user has answered; it is
   * called when the user turns the switch on, never on its own. Elsewhere there
   * is nothing to ask and it reads like `notificationPermission`. */
  requestNotificationPermission: () =>
    invoke<NotificationPermission>("request_notification_permission"),
  /** Open the OS page where the user allows notifications for OpenPal (on
   * macOS: System Settings -> Notifications, on OpenPal's row). Rejects where
   * there is no such page to open, so a caller can tell the user where to look. */
  openNotificationSettings: () => invoke<void>("open_notification_settings"),
  /** Post an OS notification. Rust re-checks the "Desktop notifications"
   * switch and that OpenPal is not the focused window, and posts nothing
   * otherwise — so a caller that decided wrongly can't produce a stray one.
   * `conversationId` is what a click on it opens (see `takeNotificationClick`).
   * Rejects before the app is unlocked; callers swallow that. */
  notifyNative: (title: string, body: string, conversationId: string | null = null) =>
    invoke<void>("notify_native", { title, body, conversationId }),
  /** Subscribe to Rust's "a notification was clicked" nudge. Rust has already
   * brought the window forward; the nudge carries no payload, and the caller
   * reads the click itself with `takeNotificationClick`, so a click that
   * arrived before anyone listened (the one that launched the app) is read the
   * same way. Resolves to the unsubscribe function; resolves to a no-op where
   * there is no event bridge (the Playwright mock, jsdom) instead of rejecting. */
  onNotificationClicked: async (cb: () => void): Promise<() => void> => {
    try {
      return await listen("notification-clicked", () => cb());
    } catch {
      return () => {};
    }
  },
  /** The notification click not acted on yet, once (a second call has
   * nothing); null when there is none. Works before unlock. */
  takeNotificationClick: () => invoke<NotificationClick | null>("take_notification_click"),

  // Cloud models
  /** The cloud model catalog — shown to everyone, signed in or not. */
  listCloudModels: () => invoke<CloudModelInfo[]>("list_cloud_models"),

  // Chat
  chatStream: (
    conversationId: string,
    messages: ChatMessage[],
    webSearch: boolean,
    agentTools: boolean,
    sampling: SamplingParams,
    onEvent: (e: ChatEvent) => void,
  ) => {
    const channel = new Channel<ChatEvent>();
    channel.onmessage = onEvent;
    return invoke<void>("chat_stream", {
      conversationId,
      messages,
      webSearch,
      agentTools,
      sampling,
      onEvent: channel,
    });
  },
  stopGeneration: () => invoke<void>("stop_generation"),
  // Writing Tools (translate/correct, plugin-gated screen). No conversation
  // persistence — the view consumes the Channel directly in local state.
  translateStream: (
    text: string,
    sourceLang: string | null,
    targetLang: string,
    onEvent: (e: ChatEvent) => void,
  ) => {
    const channel = new Channel<ChatEvent>();
    channel.onmessage = onEvent;
    return invoke<void>("translate_stream", {
      text,
      sourceLang,
      targetLang,
      onEvent: channel,
    });
  },
  correctStream: (text: string, onEvent: (e: ChatEvent) => void) => {
    const channel = new Channel<ChatEvent>();
    channel.onmessage = onEvent;
    return invoke<void>("correct_stream", { text, onEvent: channel });
  },
  /** The composer's "Improve my message" action. Plain (non-streaming)
   * invoke, not a Channel — the result is cleaned once in Rust and either
   * replaces the draft atomically or is kept out entirely, so there is
   * nothing to stream into the textarea. */
  enhancePrompt: (text: string) => invoke<EnhanceOutcome>("enhance_prompt", { text }),
  /** Save a Writing Tools translate/correct result to disk via the native
   * save dialog, defaulting to `suggestedFileName`. Resolves to `false` if
   * the dialog was closed without a choice (not an error) — same convention
   * as `downloadArtifact`. */
  saveWritingToolResult: (text: string, suggestedFileName: string) =>
    invoke<boolean>("save_writing_tool_result", { text, suggestedFileName }),
  /** Classify the composer's in-progress draft for the request-triage
   * feature's silent auto-pick (`useTriageAutopilot`). Resolves to `null`
   * (never rejects on "nothing to suggest") whenever both auto-pick settings
   * are off, the chat backend is Cloud, or the engine has nothing useful to
   * say right now. */
  classifyMessage: (text: string) => invoke<TaskType | null>("classify_message", { text }),
  /** Write one line to the app's log file — request triage's only visible
   * trace of an auto-pick action; there's no chat-UI indicator. */
  logTriageAction: (message: string) => invoke<void>("log_triage_action", { message }),
  /** Classify a just-settled turn's user message for the safety-banner
   * feature. Resolves to `null` (never rejects on "nothing to check") when
   * `riskDetectionEnabled` is off, the chat backend is Cloud, or the engine
   * has nothing useful to say right now. */
  classifyRisk: (text: string) => invoke<RiskSignal | null>("classify_risk", { text }),

  // Agent mode
  /** Reply to a pending `toolConsentRequested` event. */
  respondToolConsent: (requestId: string, decision: ConsentDecision, note?: string | null) =>
    invoke<void>("respond_tool_consent", { requestId, decision, note: note ?? null }),
  /** Reply to a pending `clarifyingQuestionAsked` event (an `ask_user` call)
   * with the user's chosen quick-reply or typed answer. `answer` may be
   * empty when the user dismissed the question without one. */
  respondClarifyingQuestion: (requestId: string, answer: string) =>
    invoke<void>("respond_clarifying_question", { requestId, answer }),
  /** Reply to a pending `injectionReviewRequested` event (agent mode or a
   * web-search turn, the only turns checked): "continue" lets the held-back
   * text reach the model unchanged, "stop" keeps it from the model and ends
   * the turn. */
  respondInjectionReview: (requestId: string, decision: InjectionReviewDecision) =>
    invoke<void>("respond_injection_review", { requestId, decision }),
  listToolGrants: () => invoke<ToolGrant[]>("list_tool_grants"),
  revokeToolGrant: (grantKey: string) => invoke<void>("revoke_tool_grant", { grantKey }),
  /** The full content of a `create_artifact`-created file, fetched when the
   * user opens its preview panel (the chip carries only the `ArtifactRef`). */
  getArtifact: (conversationId: string, artifactId: string) =>
    invoke<Artifact>("get_artifact", { conversationId, artifactId }),
  /** Opens a native "save as" picker and writes the artifact to disk.
   * Resolves to `false` if the user cancelled the picker. */
  downloadArtifact: (conversationId: string, artifactId: string) =>
    invoke<boolean>("download_artifact", { conversationId, artifactId }),

  // Voice (the speech plugin). Audio capture/playback happen natively in Rust;
  // these just trigger and stop them. `speakText` resolves when playback
  // finishes (or `stopSpeaking` cuts it short); `transcribeSpeech` records until
  // `stopDictation`, then resolves with the recognized text.
  /** Read text aloud (TTV). Resolves when playback ends or is stopped. */
  speakText: (text: string) => invoke<void>("speak_text", { text }),
  stopSpeaking: () => invoke<void>("stop_speaking"),
  /** Selectable voices (catalog + which are installed). Needs Voice enabled. */
  listVoices: () => invoke<VoiceInfo[]>("list_voices"),
  /** Download a voice by id; resolves with the refreshed list. Needs Voice on. */
  downloadVoice: (id: string) =>
    invoke<VoiceInfo[]>("download_voice", { voiceId: id }),
  /** Play a voice's preview sample (a VoiceInfo.previewUrl). Resolves when the
   * sample finishes or `stopSpeaking` cuts it. Works for not-installed voices. */
  previewVoice: (url: string) => invoke<void>("preview_voice", { url }),
  /** Record + transcribe speech (VTT). Resolves with the recognized text. */
  transcribeSpeech: () => invoke<string>("transcribe_speech"),
  stopDictation: () => invoke<void>("stop_dictation"),

  // Plugins
  listPlugins: () => invoke<PluginInfo[]>("list_plugins"),
  /**
   * Enable/disable a plugin. Enabling streams `PluginEvent`s while it runs
   * setup, then starts the service; re-calling with `true` retries a failed or
   * incomplete plugin. Disabling stops the service.
   */
  setPluginEnabled: (
    id: string,
    enabled: boolean,
    onEvent: (e: PluginEvent) => void,
  ) => {
    const channel = new Channel<PluginEvent>();
    channel.onmessage = onEvent;
    return invoke<void>("set_plugin_enabled", { id, enabled, onEvent: channel });
  },

  // Remote Access
  getRemoteAccessStatus: () => invoke<RemoteAccessStatusInfo>("get_remote_access_status"),
  setRemoteAccessEnabled: (enabled: boolean) =>
    invoke<void>("set_remote_access_enabled", { enabled }),
  /**
   * Open one pairing session ("Add a device"). Streams `PairingEvent`s:
   * `waiting` (with the QR payload) immediately, then `paired`/`expired`/
   * `failed` once the session resolves.
   */
  startPairingSession: (onEvent: (e: PairingEvent) => void) => {
    const channel = new Channel<PairingEvent>();
    channel.onmessage = onEvent;
    return invoke<void>("start_pairing_session", { onEvent: channel });
  },
  listPairedDevices: () => invoke<PairedDeviceInfo[]>("list_paired_devices"),
  revokePairedDevice: (id: string) => invoke<void>("revoke_paired_device", { id }),
  renamePairedDevice: (id: string, name: string) =>
    invoke<void>("rename_paired_device", { id, name }),
  getWolStatus: () => invoke<WolStatus>("get_wol_status"),

  // Remote Client (Android): this device pairing with someone else's
  // gateway. `getEngineStatus` above already reflects chat availability
  // once paired — these cover finding a desktop, pairing, and managing it.
  /**
   * Start browsing for desktops on the LAN, streaming each one found.
   * Replaces any discovery session already in progress. Won't find
   * anything on most emulators/VMs or an overlay network — the pairing
   * UI's manual "enter an address" field is the real fallback there, not
   * just a test workaround.
   */
  remoteDiscoverHosts: (onEvent: (host: DiscoveredHost) => void) => {
    const channel = new Channel<DiscoveredHost>();
    channel.onmessage = onEvent;
    return invoke<void>("remote_discover_hosts", { onEvent: channel });
  },
  remoteStopDiscovery: () => invoke<void>("remote_stop_discovery"),
  /** Complete pairing with a desktop using the code shown on its screen.
   * Resolves with the new pairing record on success. */
  remotePair: (addr: string, port: number, code: string, deviceName: string) =>
    invoke<RemotePairingRecord>("remote_pair", { addr, port, code, deviceName }),
  /** The current pairing, if any. */
  remoteClientStatus: () => invoke<RemotePairingRecord | null>("remote_client_status"),
  remoteForget: () => invoke<void>("remote_forget"),

  // Conversations
  listConversations: () => invoke<ConversationMeta[]>("list_conversations"),
  getConversation: (id: string) => invoke<Conversation>("get_conversation", { id }),
  saveConversation: (conversation: Conversation) =>
    invoke<void>("save_conversation", { conversation }),
  deleteConversation: (id: string) =>
    invoke<void>("delete_conversation", { id }),
  /** How many conversations "Delete old conversations" would remove right
   * now with a period of `afterDays` — what Settings confirms before saving
   * one. */
  countExpiredConversations: (afterDays: number) =>
    invoke<number>("count_expired_conversations", { afterDays }),
  /** Run the "Delete old conversations" cleanup now with the saved period;
   * resolves to the deleted ids. */
  deleteExpiredConversations: () => invoke<string[]>("delete_expired_conversations"),
  /** Subscribe to Rust's "the cleanup deleted these conversations" event,
   * sent by its background ticker (`crates/app/src/retention.rs`). Resolves
   * to the unsubscribe function; resolves to a no-op where there is no event
   * bridge (the Playwright mock, jsdom) instead of rejecting. */
  onConversationsPruned: async (cb: (ids: string[]) => void): Promise<() => void> => {
    try {
      return await listen<string[]>("conversations-pruned", (event) => cb(event.payload));
    } catch {
      return () => {};
    }
  },
  /** Case-insensitive search over every conversation's title and message
   * content, for the sidebar's search box. */
  searchConversations: (query: string) =>
    invoke<ConversationSearchResult[]>("search_conversations", { query }),

  // Projects
  listProjects: () => invoke<Project[]>("list_projects"),
  /** Create (new id) or rename (existing id) a project. */
  saveProject: (project: Project) => invoke<void>("save_project", { project }),
  /** Deletes the project and unassigns every conversation that was in it —
   * the conversations themselves are untouched, only ungrouped. */
  deleteProject: (id: string) => invoke<void>("delete_project", { id }),
  /** Move a conversation into `projectId`, or out of whatever project it's
   * in when `projectId` is `null`. */
  setConversationProject: (conversationId: string, projectId: string | null) =>
    invoke<void>("set_conversation_project", { conversationId, projectId }),
  /** Rename a conversation from the sidebar's row menu. */
  setConversationTitle: (conversationId: string, title: string) =>
    invoke<void>("set_conversation_title", { conversationId, title }),
  /** Pin/unpin a conversation from the sidebar's row menu. Synced across
   * paired devices, so a pin keeps the conversation out of every device's
   * "Delete old conversations" cleanup. */
  setConversationPinned: (conversationId: string, pinned: boolean) =>
    invoke<void>("set_conversation_pinned", { conversationId, pinned }),
  /** Set or clear the sidebar's "New reply" hint. Synced across paired
   * devices, like `setConversationPinned`. */
  setConversationUnread: (conversationId: string, unread: boolean) =>
    invoke<void>("set_conversation_unread", { conversationId, unread }),

  // Skills
  listSkills: () => invoke<Skill[]>("list_skills"),
  /** Create (empty `skill.slug`) or update (existing slug) a skill; returns
   * it with the slug filled in. */
  saveSkill: (skill: Skill) => invoke<Skill>("save_skill", { skill }),
  deleteSkill: (slug: string) => invoke<void>("delete_skill", { slug }),
  /** Opens a native picker for a `.md` file and imports it as a new skill.
   * `null` means the picker was closed without a choice. */
  importSkillFile: () => invoke<Skill | null>("import_skill_file"),
  /** Same, for a folder containing a `SKILL.md` plus any supporting Markdown
   * files. */
  importSkillFolder: () => invoke<Skill | null>("import_skill_folder"),
  /** Same, for a `.zip` archive of a skill folder. */
  importSkillZip: () => invoke<Skill | null>("import_skill_zip"),
  /** Saves `slug` to a location the user chooses via the native save dialog
   * — a `.md` for a flat skill, a `.zip` (importable by importSkillZip) for
   * one with supporting files. Resolves to `false` if the dialog was closed
   * without a choice (not an error). */
  exportSkill: (slug: string) => invoke<boolean>("export_skill", { slug }),

  // Settings
  getSettings: () => invoke<Settings>("get_settings"),
  updateSettings: (settings: Settings) =>
    invoke<void>("update_settings", { settings }),

  // Parental controls
  /** Live lock state for the composer. No PIN needed — reveals nothing. */
  getParentalStatus: () => invoke<ParentalStatus>("get_parental_status"),
  /** The stored configuration minus the PIN. No PIN needed just to look. */
  getParentalSettings: () => invoke<ParentalSettingsView>("get_parental_settings"),
  /** Side-effect-free (beyond its own rate-limiting bookkeeping) PIN check,
   * so Settings can unlock its edit form before the parent has typed any new
   * values. Rate-limited — see `PinCheckResult`. */
  verifyParentalPin: (pin: string) => invoke<PinCheckResult>("verify_parental_pin", { pin }),
  /** Create, edit, or turn off parental controls. `currentPin` must match the
   * stored PIN once one exists — including to turn the feature back off. */
  setParentalControls: (currentPin: string | null, config: ParentalConfigInput) =>
    invoke<void>("set_parental_controls", { currentPin, config }),

  // Account
  /** The signed-in account, if any — the local cache only, no network call. */
  getAccount: () => invoke<Account | null>("get_account"),
  /** Refresh the cached account from a stored sign-in session, if any. Silent
   * on failure (offline, never signed in, expired) — returns whatever was
   * already cached. */
  refreshAccount: () => invoke<Account | null>("refresh_account"),
  /**
   * Start signing in. Streams `AccountEvent`s: `awaitingBrowser` once a
   * device session is ready (open `verificationUrl` with `openLink`), then
   * `complete`/`expired`/`failed` once the user finishes (or doesn't) there.
   */
  signIn: (onEvent: (e: AccountEvent) => void) => {
    const channel = new Channel<AccountEvent>();
    channel.onmessage = onEvent;
    return invoke<void>("sign_in", { onEvent: channel });
  },
  /** Stop an in-flight `signIn` call early. */
  cancelSignIn: () => invoke<void>("cancel_sign_in"),
  signOut: () => invoke<void>("sign_out"),

  // Updates
  /** Whether a newer version is published — no download. */
  checkForUpdate: () => invoke<AvailableUpdate | null>("check_for_update"),
  /** Download and install the update found by `checkForUpdate`, then
   * relaunch. Progress streams as the same shape `downloadModel` uses. */
  installUpdate: (onEvent: (e: DownloadEvent) => void) => {
    const channel = new Channel<DownloadEvent>();
    channel.onmessage = onEvent;
    return invoke<void>("install_update", { onEvent: channel });
  },

  // Engine (llama.cpp) updates — a separate channel from app updates above.
  /** Whether a newer AI engine build is published — no download. */
  checkForEngineUpdate: () => invoke<EngineUpdateInfo | null>("check_engine_update"),
  /** Download, verify, and stage the engine build found by
   * `checkForEngineUpdate`. Never restarts the app or the running AI — the
   * new build takes effect the next time the AI engine starts. */
  installEngineUpdate: (onEvent: (e: DownloadEvent) => void) => {
    const channel = new Channel<DownloadEvent>();
    channel.onmessage = onEvent;
    return invoke<void>("install_engine_update", { onEvent: channel });
  },

  // Logs / diagnostics
  /**
   * The tail of a log file for the in-app viewer. `which` is "engine" for the
   * AI engine's log or "app" for OpenPal's own log. Secrets are masked by Rust.
   */
  getRecentLogs: (which: "app" | "engine", maxLines?: number) =>
    invoke<string>("get_recent_logs", { which, maxLines: maxLines ?? null }),
  /** Write a decrypted, redacted copy of every log to a location the user
   * chooses via the native save dialog. Resolves to `false` if the dialog
   * was closed without a choice (not an error). */
  exportLogs: () => invoke<boolean>("export_logs"),
  /** Delete old daily log files (keeps the current one). Resolves to the count removed. */
  clearLogs: () => invoke<number>("clear_logs"),
  /** Export the categories `include` ticks (chats with their projects and
   * files, settings, skills, custom models, plugins) as one plaintext `.zip`
   * via the native save dialog. Resolves to `null` if the dialog was closed
   * without a choice (not an error). */
  exportData: (include: ExportCategories) =>
    invoke<ExportSummary | null>("export_data", { include }),
  /** The exact size in bytes of the `.zip` `exportData(include)` would write
   * right now. */
  estimateExportSize: (include: ExportCategories) =>
    invoke<number>("estimate_export_size", { include }),
  /** How much each export category holds right now (chats, projects,
   * files, changed settings, skills, custom models, plugins turned on). */
  exportCounts: () => invoke<ExportCounts>("export_counts"),
  /** Read back a `.zip` produced by `exportData` (on this device or another)
   * via the native open dialog, bringing back whatever it contains — chats,
   * projects, files, settings (replacing this device's), skills, custom
   * models and plugin choices. Resolves to `null` if the dialog was closed
   * without a choice (not an error). */
  importData: () => invoke<ImportSummary | null>("import_data"),
  /** Record a webview-side message in the same log file as the backend. */
  logFrontend: (level: "error" | "warn" | "info" | "debug", message: string) =>
    invoke<void>("frontend_log", { level, message }),

  // Multi-device sync — pairing and roster (docs/SYNC_PAIRING_PROTOCOL.md, Phase 2).
  /** Creates a new sync group on this device. Returns the `group_secret` to
   * show the user once — they relay it out-of-band to a second device. */
  syncGroupCreate: (deviceLabel: string) =>
    invoke<string>("sync_group_create", { deviceLabel }),
  /** This device's current `group_secret`, or `null` if it hasn't created or
   * joined a sync group yet. */
  syncGroupSecret: () => invoke<string | null>("sync_group_secret"),
  /**
   * Device A's side of "Add a device": listens for one incoming join
   * request and streams exactly one `incomingRequest` event (or
   * `expired`/`failed`) before resolving. Does not wait for the user's tap —
   * call `syncPairConfirm` separately once they respond to the card.
   */
  syncPairListen: (onEvent: (e: SyncPairingEvent) => void) => {
    const channel = new Channel<SyncPairingEvent>();
    channel.onmessage = onEvent;
    return invoke<void>("sync_pair_listen", { onEvent: channel });
  },
  /** Device A: the user tapped Confirm/Reject on the card `syncPairListen`
   * showed. `approve = false` rejects without installing anything. */
  syncPairConfirm: (approve: boolean) =>
    invoke<SyncPairingEvent>("sync_pair_confirm", { approve }),
  /**
   * Device B's side of "Join a group": streams `awaitingChallenge` ->
   * `awaitingConfirmation` -> `readyToInstall` (or `expired`/`failed`)
   * before resolving. Does not install anything yet — call `syncPairFinish`
   * separately once the user gives their second, explicit confirmation.
   */
  syncPairRequest: (
    groupSecret: string,
    deviceLabel: string,
    onEvent: (e: SyncPairingEvent) => void,
  ) => {
    const channel = new Channel<SyncPairingEvent>();
    channel.onmessage = onEvent;
    return invoke<void>("sync_pair_request", { groupSecret, deviceLabel, onEvent: channel });
  },
  /** Device B: the user gave the second "codes match, finish joining?"
   * confirmation after `syncPairRequest` reached `readyToInstall`. */
  syncPairFinish: (approve: boolean) =>
    invoke<SyncPairingEvent>("sync_pair_finish", { approve }),
  /** Cancels whichever pairing session (listen or request) is in flight on
   * this device — call when the "Add a device"/"Join a group" dialog closes. */
  syncPairCancel: () => invoke<void>("sync_pair_cancel"),
  /** This device's sync-group roster, for Settings -> Advanced -> Sync's device list. */
  syncDeviceList: () => invoke<DeviceSummary[]>("sync_device_list"),
  /** Removes a device from the sync group and rotates the shared key. */
  syncDeviceRemove: (deviceId: string) =>
    invoke<void>("sync_device_remove", { deviceId }),
};
