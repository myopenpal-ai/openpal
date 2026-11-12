/**
 * Mirrors `crates/core/src/types.rs` exactly.
 * Rust serde conventions: camelCase fields; data-carrying enums are
 * adjacently tagged as `{ type, data }` (unit variants have no `data`).
 */

export interface HardwareInfo {
  chip: string;
  isAppleSilicon: boolean;
  totalRamBytes: number;
  availableRamBytes: number;
  cpuCores: number;
  hasMetal: boolean;
  freeDiskBytes: number;
  osVersion: string;
  /** Vulkan GPU the engine reported (Windows/Linux only); absent when none. */
  gpuName?: string;
  /** Reported GPU memory — shared RAM on integrated GPUs, so an upper bound. */
  gpuVramBytes?: number;
}

export type FitLevel = "tooBig" | "diskSlow" | "tight" | "good" | "perfect";

export type ModelSource = "catalog" | "huggingFace" | "localFile";

/** One additional shard of a split GGUF (`model-00002-of-00003.gguf`, …).
 * `ModelSpec` always describes shard 1 through its own `url`/`fileName`/
 * `sizeBytes`/`sha256`; `extraShards` holds shards 2..N. */
export interface ModelShard {
  url: string;
  fileName: string;
  sizeBytes: number;
  sha256: string | null;
}

export interface ModelSpec {
  id: string;
  name: string;
  family: string;
  publisher: string;
  description: string;
  strengths: string[];
  /** Direct GGUF download URL for shard 1 (the only shard, for the
   * overwhelming majority of models that aren't split). */
  url: string;
  /** Shard 1's file name on disk. */
  fileName: string;
  /** Shard 1's exact download size in bytes. */
  sizeBytes: number;
  /** Shards 2..N of a split GGUF. Absent/empty for every single-file model. */
  extraShards?: ModelShard[];
  quant: string;
  sha256: string | null;
  paramsB: number;
  moe: boolean;
  /** For MoE models: active parameters per token, in billions. Drives the
   * "run from disk" fit estimate; absent for dense and user-added models. */
  activeParamsB?: number;
  contextMax: number;
  qualityScore: number;
  license: string;
  extraArgs: string[];
  hidden: boolean;
  /** One of the handful of catalog picks the simple model picker shows by
   * default. Always false for custom (Hugging Face / local file) models. */
  curated: boolean;
  /** This model's curated sampling recommendation, one entry per `TaskType`
   * it has specific published guidance for. Absent for every Hugging Face /
   * local-file model and for a catalog entry with no recommendation of its
   * own — see `recommendedSampling`, which resolves the general fallback.
   * For a model with a hybrid thinking mode (see `taskDefaultsThinking`),
   * this doubles as the instruct/non-thinking-mode table. */
  taskDefaults?: Partial<Record<TaskType, SamplingParams>>;
  /** Curated sampling recommendation for when this model is actually
   * running in "thinking" mode (`Settings.showThinking` on and the model
   * supports it). Absent for every model without a hybrid think/non-think
   * mode — most of the catalog. Consulted before `taskDefaults`, see
   * `recommendedSampling`. */
  taskDefaultsThinking?: Partial<Record<TaskType, SamplingParams>>;
  source: ModelSource;
  localPath: string | null;
  /** True for vision/multimodal models — the only ones that accept images.
   * Drives the composer's image attach + paste; documents work with any model. */
  vision: boolean;
  /** For vision models: the companion multimodal projector, downloaded with
   * the weights. Absent on text-only and user-added models. */
  mmprojUrl?: string;
  mmprojFileName?: string;
  mmprojSizeBytes?: number;
  mmprojSha256?: string;
  /** True when this curated entry's GGUF ships MTP (multi-token prediction)
   * heads usable by the bundled engine for speculative self-drafting
   * (`--spec-type draft-mtp`) — a bench-verified curator decision, never
   * auto-detected from tensor names. Absent/false for everything else. */
  mtp?: boolean;
  /** Hand-curated 0-100 fit score per `TaskType` — how *good* this model is
   * at a task, used by the opt-in `smart-model-router` plugin's `bestModelForTag`
   * (`triage.ts`) to rank installed models. A different question from
   * `taskDefaults` (what sampling this model should use). Always present
   * (possibly `{}`) regardless of whether the plugin is enabled — same
   * posture as `qualityScore`. */
  taskScores: Partial<Record<TaskType, number>>;
  /** Ties multiple quantizations of the same underlying model together for
   * the "All models" table's sub-rows (`views/Library.tsx`). Absent for most
   * entries — every entry is then its own group of one. Use `groupKey` from
   * `lib/format.ts` rather than reading this directly. */
  groupId?: string;
  /** ISO-8601 date (or full timestamp) this model was first published, when
   * known. Absent for most user-added models and for catalog entries no one
   * has verified a date for — the UI leaves it out (`releaseDateLabel` returns ""). */
  releaseDate?: string | null;
}

/** A model offered by the cloud backend, shown in Library's "Cloud models"
 * group alongside local ones. Not a `ModelSpec` — a cloud model has no disk
 * state or hardware fit. */
export interface CloudModelInfo {
  id: string;
  name: string;
  family: string;
  publisher: string;
  description: string;
  strengths: string[];
  /** Plan tier that unlocks this model, e.g. "Pro" — shown as a badge when
   * the signed-in account (or no account) doesn't have it yet. */
  requiredTier: string;
  /** Which Generation-panel sliders this model's provider actually honors
   * (see `SamplingParamKey`) — e.g. an OpenAI-backed model omits `topK`,
   * `minP`, `repeatPenalty`, `repeatLastN`; Anthropic additionally omits
   * `presencePenalty`, `frequencyPenalty`, `seed`. Empty until the backend
   * classifies a model. */
  supportedParams: SamplingParamKey[];
}

/** One additional shard of a split GGUF listed by a Hugging Face repo browse
 * — mirrors `ModelShard`, but keyed by repo-relative `path`. */
export interface HfShardFile {
  path: string;
  sizeBytes: number;
  sha256: string | null;
}

/** One GGUF a Hugging Face repository offers — a single file, or shard 1 of
 * a split model (with the rest in `extraShards`). */
export interface HfFile {
  /** Shard 1's path for a split model. */
  path: string;
  /** Shard 1's size. See `extraShards` for the rest of a split model. */
  sizeBytes: number;
  quant: string;
  sha256: string | null;
  /** Shards 2..N when this entry is a split GGUF; absent for a normal
   * single-file model. */
  extraShards?: HfShardFile[];
}

export interface HfListing {
  repoId: string;
  revision: string;
  /** One entry per model: a single file, or a complete split-GGUF shard
   * group (see `HfFile.extraShards`). Smallest total download first. */
  files: HfFile[];
  preselectPath: string | null;
}

/* --------------------------- Hub browser (search) ------------------------ */

/** How to sort a Hugging Face hub search. */
export type HfSortKey = "trending" | "downloads" | "likes" | "lastModified";

/** A browse/search request against the Hugging Face hub. */
export interface HfSearchQuery {
  /** Free-text search, e.g. "qwen". Empty/null = no text filter. */
  search: string | null;
  /** Restrict to one author/org. */
  author: string | null;
  /** Restrict to one pipeline tag, e.g. "text-generation". */
  pipelineTag: string | null;
  sort: HfSortKey;
  /** Opaque pagination cursor from a previous page's `nextCursor`; null for
   * the first page. */
  cursor: string | null;
}

/** One row of a Hugging Face hub search/trending listing. Every field beyond
 * `repoId` is best-effort — the hub API doesn't guarantee all of them. */
export interface HfModelSummary {
  repoId: string;
  author: string | null;
  pipelineTag: string | null;
  libraryName: string | null;
  /** Downloads in the last 30 days (the hub API's own window, not all-time). */
  downloads: number | null;
  likes: number | null;
  /** From the repo's `license:*` tag. */
  license: string | null;
  /** True if the repo requires a Hugging Face sign-in + license acceptance
   * before its files can be downloaded. OpenPal has no HF login, so gated
   * repos are shown but their files can't be fetched. */
  gated: boolean;
  /** ISO-8601 timestamp of the repo's last update, when available. */
  lastModified: string | null;
  /** GGUF metadata the hub parsed from the repo's files, when available. */
  gguf: HfGgufInfo | null;
  /** Rough "will this fit my machine" verdict from `gguf`'s parameter count
   * against a representative mid-range quantization — null when the hub
   * couldn't parse a parameter count for this repo. Coarser than the
   * exact-file fit badges on the model page (`HfQuantOption`); see
   * `fitWhy` for the hedged copy. */
  fit: FitLevel | null;
  fitWhy: string | null;
}

/** One page of a Hugging Face hub search/trending listing. */
export interface HfSearchPage {
  models: HfModelSummary[];
  /** Pass back as `HfSearchQuery.cursor` to fetch the next page; null means
   * this was the last page. */
  nextCursor: string | null;
}

/** GGUF-specific metadata the hub API parses out of a repo's GGUF file
 * headers, when the repo is a GGUF model. */
export interface HfGgufInfo {
  /** Total parameter count, when the hub could determine it. */
  totalParams: number | null;
  architecture: string | null;
  contextLength: number | null;
}

/** The model-card page for one Hugging Face repo. */
export interface HfModelDetails {
  repoId: string;
  author: string | null;
  pipelineTag: string | null;
  downloads: number | null;
  likes: number | null;
  license: string | null;
  gated: boolean;
  lastModified: string | null;
  /** The repo's creation date on the hub — closer to a model's real "release
   * date" than `lastModified`, which changes on every file update. */
  createdAt: string | null;
  tags: string[];
  gguf: HfGgufInfo | null;
}

/** One selectable file/quant on a Hugging Face repo's model page, decorated
 * with a memory-fit verdict for the user's own machine. */
export interface HfQuantOption {
  file: HfFile;
  fit: FitLevel;
  why: string;
  /** The parameter count `fit` was computed from — real (from parsing the
   * repo's own GGUF header) when `precise` is true, otherwise the hub's
   * `gguf.total` or a file-size/quant heuristic. Lets `estimateTokensPerSec`
   * work on this option the same way it does for a `ModelSpec`. */
  paramsB: number;
  moe: boolean;
  activeParamsB?: number;
  /** True when `moe`/`activeParamsB` came from actually parsing this repo's
   * GGUF header rather than a same-as-always-false placeholder. */
  precise: boolean;
}

/** A repo's files/quants (see `HfListing`), each with a fit verdict attached. */
export interface HfQuantPicker {
  repoId: string;
  revision: string;
  /** Smallest total download first — same order as `HfListing.files`. */
  options: HfQuantOption[];
  preselectPath: string | null;
}

export type ModelStatus =
  | { type: "notInstalled" }
  | { type: "partiallyDownloaded"; data: { receivedBytes: number } }
  | { type: "installed"; data: { path: string; sizeBytes: number } };

export interface ModelInfo {
  spec: ModelSpec;
  status: ModelStatus;
  fit: FitLevel;
  why: string;
  isRecommended: boolean;
  /** Whether the user has starred this model, from `Settings.favoriteModelIds`
   * (resolved per-model server-side, see `commands::get_models`). Toggle via
   * `api.setModelFavorite`. */
  favorite: boolean;
}

export interface RankedModel {
  modelId: string;
  fit: FitLevel;
  why: string;
}

export interface Recommendation {
  primaryModelId: string | null;
  headline: string;
  reasons: string[];
  ranked: RankedModel[];
}

export interface SystemReport {
  hardware: HardwareInfo;
  recommendation: Recommendation;
}

/** Coarse classification of a storage volume's measured write bandwidth —
 * feeds the disk-slow download confirmation dialog and gates it: "tooSlow"
 * refuses the download entirely (`docs/design/DISK_OFFLOAD.md` Phase 3). */
export type DiskSpeedClass = "fast" | "slow" | "tooSlow";

/** Result of probing the model install volume's speed, plus (when a model is
 * known) an honest tok/s estimate. From `probeDiskSpeed`. */
export interface DiskSpeedInfo {
  bytesPerSec: number;
  class: DiskSpeedClass;
  /** Absent only when the model carries no size at all to estimate from. */
  estimatedTokensPerSec?: number;
  /** True when the estimate used a MoE model's real active-expert share —
   * the case the underlying formula was actually calibrated against real
   * benchmarks for. False (dense, or MoE without a known active size) means
   * "whole file touched every token," an unvalidated extrapolation — the UI
   * should hedge this estimate more conservatively. */
  calibrated: boolean;
}

export type DownloadEvent =
  | { type: "started"; data: { totalBytes: number; resumedFromBytes: number } }
  | {
      type: "progress";
      data: {
        receivedBytes: number;
        totalBytes: number;
        bytesPerSec: number;
        etaSeconds: number;
      };
    }
  | { type: "finished" }
  | { type: "failed"; data: { message: string } }
  | { type: "cancelled" };

export interface EngineConfig {
  contextLength: number;
  gpuLayers: number;
  threads: number | null;
  extraArgs: string[];
  /** For vision models: the projector loaded with `--mmproj`. Internal to the
   * backend — never constructed by the UI. */
  mmprojPath?: string | null;
}

export type EngineStatus =
  | { type: "stopped" }
  | {
      type: "starting";
      data: {
        modelId: string;
        /** A real (not simulated) loading percentage, 0-99 — see the
         * matching Rust `EngineStatus::Starting` doc comment for how it's
         * derived. Only ever set for a GPU-offloaded load; `null` for a
         * CPU-only load (nothing honest to report there — show the
         * spinner instead) and for `RemoteBackend` (no local process to
         * sample). */
        progressPercent: number | null;
      };
    }
  | {
      type: "running";
      data: {
        modelId: string;
        port: number;
        /** Backend-authoritative: classified at load time via the
         * toggle-independent `selection::exceeds_ram` check, not the
         * `diskOffload` setting. Never re-derive this from `ModelInfo.fit`
         * client-side — that's toggle-dependent and can diverge. */
        diskSlow: boolean;
        /** Epoch-ms this engine will auto-unload at, or `null` if the
         * feature is off, this model is disk-slow-exempt, or a reply is
         * generating right now. */
        autoUnloadAt: number | null;
      };
    }
  | { type: "failed"; data: { message: string } }
  /** `RemoteBackend` only (Android): this phone isn't paired with a
   * desktop yet. Never produced on desktop, which has no pairing concept. */
  | { type: "remoteUnpaired" }
  /** `RemoteBackend` only: paired, but the last request to the desktop's
   * gateway failed. `hostName` is the paired device's own name. */
  | { type: "remoteUnreachable"; data: { hostName: string } };

export type Role = "system" | "user" | "assistant";

/** One web source behind an assistant reply; `id` is the inline `[n]` marker. */
export interface Citation {
  id: number;
  url: string;
  title: string;
}

/** One step a tool-using turn took (e.g. web search, an agent-mode tool
 * call), persisted on the message so the UI can re-show "what it did" after
 * the fact. Mirrors Rust `ChatStep`. */
export type ChatStep =
  | { type: "search"; data: { query: string } }
  | { type: "read"; data: { url: string; title: string } }
  | {
      type: "toolCall";
      data: { toolId: string; summary: string; decision: ToolCallDecision; ok: boolean };
    }
  /** An `ask_user` call this turn. `answer` is empty when the user dismissed
   * the question without one. */
  | { type: "askedQuestion"; data: { question: string; answer: string } }
  /** A `spawn_subagent` call's nested tool-use loop, collapsed into one step
   * with its own tool calls nested in `steps` instead of flattened into the
   * parent turn's step list. `ok` is `null` while the helper is still
   * running — distinct from `false` (actually failed), since the helper can
   * take a while and the trace panel is visible live during that time. */
  | {
      type: "subagentRun";
      data: { kind: string; task: string; ok: boolean | null; steps: ChatStep[] };
    };

/** How an agent-mode tool call was resolved, for `ChatStep.toolCall`. */
export type ToolCallDecision = "allowOnce" | "alwaysAllow" | "denied" | "rejected";

/** What kind of file a user attached to a message. */
export type AttachmentKind = "image" | "document";

/** One file attached to a user message: an image (vision models only) or a
 * document whose text is fed to the model. Mirrors Rust `Attachment`. */
export interface Attachment {
  kind: AttachmentKind;
  name: string;
  /** For images: a `data:<mime>;base64,…` URI (thumbnail + sent to the model). */
  dataUri?: string;
  /** For documents: the extracted text folded into the prompt. */
  text?: string;
}

/** One file the model created via the `create_artifact` agent-mode tool during
 * a turn — shown as a downloadable chip in the transcript, distinct from
 * `Attachment` (user-supplied input). Content is not included; fetch it with
 * `api.getArtifact` when the user opens the preview panel. Mirrors Rust
 * `ArtifactRef`. */
export interface ArtifactRef {
  id: string;
  filename: string;
  sizeBytes: number;
}

/** The full artifact (with content), fetched on demand. Mirrors Rust
 * `Artifact`. */
export interface Artifact {
  id: string;
  filename: string;
  content: string;
  sizeBytes: number;
}

/** Which parts of the user's data a data export includes, one checkbox each
 * in Settings → Your data. `chats` covers conversations, projects and
 * artifacts together. Mirrors Rust `ExportCategories`. */
export interface ExportCategories {
  chats: boolean;
  settings: boolean;
  skills: boolean;
  customModels: boolean;
  plugins: boolean;
}

/** How much each export category holds right now, shown beside the export
 * row's checkboxes. Mirrors Rust `ExportCounts`. */
export interface ExportCounts {
  conversations: number;
  projects: number;
  artifacts: number;
  /** Settings that differ from a fresh install's (all of them are exported). */
  changedSettings: number;
  skills: number;
  customModels: number;
  /** Plugins currently turned on. */
  enabledPlugins: number;
}

/** Result of a data export: how many zip entries were written per category,
 * plus how many source rows couldn't be read. Mirrors Rust
 * `ExportSummary`. */
export interface ExportSummary {
  conversations: number;
  projects: number;
  artifacts: number;
  skills: number;
  customModels: number;
  /** Whether the settings went in. */
  settings: boolean;
  /** How many plugin records went in. */
  plugins: number;
  skipped: number;
}

/** Result of a data import: how many entries were successfully imported per
 * category from a `.zip` produced by `exportData`, plus how many couldn't be
 * read/parsed/saved. Mirrors Rust `ImportSummary`. */
export interface ImportSummary {
  conversations: number;
  projects: number;
  artifacts: number;
  skills: number;
  customModels: number;
  /** Whether the zip's settings replaced this device's. */
  settings: boolean;
  /** How many plugin records the zip carried. */
  plugins: number;
  skipped: number;
}

/** How often automatic backup runs. Mirrors Rust `BackupFrequency`. */
export type BackupFrequency = "daily" | "weekly";

/** Settings → Your data's automatic backup: the user's choices and how the
 * last run went. Device-local — never exported or restored. Mirrors Rust
 * `openpal_core::backup::BackupState`. */
export interface BackupState {
  enabled: boolean;
  /** The folder backups are written to, `null` until one is chosen. */
  folder: string | null;
  frequency: BackupFrequency;
  lastSuccessMs: number | null;
  lastAttemptMs: number | null;
  /** Why the most recent attempt failed, safe to show as-is; `null` once
   * one succeeds. */
  lastError: string | null;
}

/** How a backup restore went. A wrong recovery code is an expected answer,
 * not an error. Mirrors Rust `commands::RestoreOutcome`. */
export type RestoreOutcome =
  | { kind: "restored"; summary: ImportSummary }
  | { kind: "wrongCode" };

export interface ChatMessage {
  /** Stable identity for this message within its conversation's message
   * array. Minted once (the frontend via `crypto.randomUUID()` for a
   * newly authored message; the backend backfills one for messages
   * persisted before this field existed) and never regenerated on reload
   * or re-render. Used as the key into `Conversation.branches` for
   * edit/retry history navigation, and as the React list key in
   * `ChatView`. Always present by the time the frontend sees a message —
   * every message round-trips through the Rust backend first. */
  id: string;
  role: Role;
  content: string;
  /** When this message was created, stamped by the frontend at send time
   * (user turns) or when the streaming placeholder is minted (assistant
   * turns) — same `Date.now()`-epoch-ms shape as `Conversation.createdAtMs`.
   * `undefined` for messages persisted before this field existed. */
  createdAtMs?: number;
  /** For assistant messages, the id of the model that produced the reply. */
  model?: string;
  /** For web-search replies, the sources the answer drew from, in `[n]` order. */
  citations?: Citation[];
  /** For assistant messages: the model's private reasoning ("thinking") shown
   * as a collapsible trace. Present only when thinking is on and the model
   * produced some. Never sent to the model as part of the answer. */
  thinking?: string;
  /** For tool-using replies (web search): the steps the turn took, in order. */
  steps?: ChatStep[];
  /** For user messages: files attached this turn — images (vision models) and/or
   * documents. The engine turns these into model input when sending. */
  attachments?: Attachment[];
  /** For assistant messages: files the model created this turn via the
   * `create_artifact` tool (agent mode only) — shown as downloadable chips,
   * distinct from `attachments` (user-supplied input files). */
  artifacts?: ArtifactRef[];
  /** For assistant messages: token/sec and wall-clock duration (thinking +
   * answer) for this reply. Shown when `showGenerationStats` is on. Absent
   * for user/system messages and older histories. */
  stats?: ChatStats;
  /** For user messages sent via a `/<skill>` invocation: the raw text the
   * user actually typed, shown in the transcript instead of `content` (which
   * carries the skill's body folded in ahead of the user's words — see
   * `matchSkillInvocation` in `state/store.ts`). Absent for a plain turn. */
  displayContent?: string;
  /** For assistant messages: one line per text the prompt-injection
   * detector flagged this turn, and what happened to it (continued, stopped,
   * or sent as typed). Never carries an excerpt. Absent when nothing was
   * flagged. */
  injectionNotices?: InjectionNotice[];
  /** For user messages: the user pressed "Stop here" on a review card for
   * text this message carried, so the backend leaves it — and the replies
   * after it — out of every request. Only ever removes text from what the
   * model sees. Retry clears it. Absent otherwise. */
  withheldFromModel?: boolean;
}

/** Where a text the prompt-injection detector flagged came from. Mirrors
 * Rust `injection::Source::id`. Persisted as a plain string, so a value from
 * a newer build may be none of these — the UI renders it with a generic
 * fallback phrase. */
export type InjectionSource =
  | "webPage"
  | "webSearch"
  | "file"
  | "commandOutput"
  | "attachment"
  | "projectConversation"
  | "skill"
  | "composer"
  | "toolOutput";

/** What happened to a flagged text: the user continued anyway, stopped
 * before the model read it, or (typed/pasted text only) it was sent as
 * written with a notice. Same unknown-string fallback as `InjectionSource`. */
export type InjectionOutcome = "continued" | "stopped" | "sent";

/** The two answers to a review card. Mirrors Rust `ReviewDecision`. */
export type InjectionReviewDecision = "continue" | "stop";

/** One flagged text on a review card. `excerpt` is the part of the text
 * that tripped the check (the classifier's most suspicious stretch, at most
 * 1,500 characters), with invisible characters shown as ▯ (`hiddenChars`);
 * an attached file's name too long for `name` (capped with "…") is in it
 * whole. Empty when `unchecked` (the check errored or ran out of time, so
 * there is nothing to point at) or past the first few items.
 * `messageId` names the user message that carried it (user-side texts
 * only). Mirrors Rust `FlaggedItem`. */
export interface FlaggedItem {
  source: InjectionSource;
  name: string | null;
  messageId: string | null;
  excerpt: string;
  excerptTruncated: boolean;
  hiddenChars: boolean;
  unchecked: boolean;
}

/** One persisted notice line on an assistant message — see
 * `ChatMessage.injectionNotices`. Mirrors Rust `InjectionNotice`. */
export interface InjectionNotice {
  source: InjectionSource;
  name?: string;
  messageId?: string;
  outcome: InjectionOutcome;
  unchecked: boolean;
}

export interface ChatStats {
  totalTokens: number;
  tokensPerSec: number;
  durationMs: number;
}

/** A file/dir argument resolved to its canonical, symlink-followed target —
 * shown on the consent card distinctly from the model's raw argument when
 * they differ. Mirrors Rust `ResolvedPath`. */
export interface ResolvedPath {
  raw: string;
  canonical: string;
  insideWorkspace: boolean;
}

/** Non-blocking warning badges on an agent-mode consent card. Never gate
 * execution by themselves — every call still goes through the same
 * allow-once/always-allow/deny prompt. Mirrors Rust `DangerFlag`. */
export type DangerFlag =
  | { type: "outsideWorkspace" }
  | { type: "forkBombShape" }
  | { type: "rawDeviceIo" }
  | { type: "networkFetchPipeShell" }
  | { type: "sudoOrPrivEsc" }
  | { type: "derivedFromRecentOutput"; data: { step: number } }
  /** Something read earlier in the same turn was flagged by the
   * prompt-injection detector (Rust `crate::injection`). */
  | { type: "promptInjectionSuspected" };

/** "Allow once" / "Always allow" / "Deny" — the three outcomes of an
 * agent-mode consent prompt. Mirrors Rust `ConsentDecision`. */
export type ConsentDecision = "allowOnce" | "alwaysAllow" | "deny";

/** A persisted "Always allow" grant, shown in Settings' standing-permissions
 * list. Mirrors Rust `ToolGrant`. */
export interface ToolGrant {
  grantKey: string;
  toolId: string;
  summary: string;
  createdAtMs: number;
  expiresAtMs?: number;
}

/** Why the composer's "Improve my message" action left the draft untouched.
 * Mirrors the Rust `KeepReason` enum (`crates/core/src/prompt_enhancer.rs`)
 * under `rename_all = "camelCase"`. */
export type KeepReason =
  | "empty"
  | "noAnswer"
  | "tooLong"
  | "tooShort"
  | "refused"
  | "answered"
  | "languageChanged"
  | "startsWithSlash"
  | "cancelled"
  | "noChange";

/** The result of one "Improve my message" request — mirrors the Rust
 * `EnhanceOutcome` enum's `{ type, data }` shape (same serde convention as
 * `ChatEvent` below). */
export type EnhanceOutcome =
  | { type: "enhanced"; data: { text: string } }
  | { type: "kept"; data: { reason: KeepReason } };

export type ChatEvent =
  | { type: "token"; data: { text: string } }
  /** A chunk of the model's private reasoning, streamed before/with the answer. */
  | { type: "reasoning"; data: { text: string } }
  | { type: "done"; data: { stats: ChatStats } }
  | { type: "error"; data: { message: string } }
  // Web search (only when the turn is routed to a search-capable chat plugin):
  /** Progress while the web-search add-on's local SearXNG service starts,
   * or why it couldn't (see `PluginManager::ensure_capability_service` in
   * `crates/core/src/plugin.rs`). */
  | { type: "searchSetupProgress"; data: { message: string } }
  | { type: "searchStarted"; data: { query: string } }
  | { type: "searchResult"; data: { title: string; url: string } }
  | { type: "fetching"; data: { url: string } }
  | { type: "citation"; data: { id: number; url: string; title: string } }
  // Agent mode (only when the turn is routed through the agent-mode loop):
  | {
      type: "toolConsentRequested";
      data: {
        requestId: string;
        toolCallId: string;
        toolId: string;
        summary: string;
        detail: string;
        resolvedPaths: ResolvedPath[];
        dangerFlags: DangerFlag[];
        /** The `spawn_subagent` kind (e.g. "math") that made this call, if
         * it came from a subagent's own nested loop rather than directly
         * from the top-level turn. */
        via: string | null;
      };
    }
  /** The model called `ask_user`. Unlike `toolConsentRequested`, this never
   * gates anything — `ask_user` has no blast radius, so it never reaches a
   * consent prompt of its own; reply via `api.respondClarifyingQuestion`. */
  | {
      type: "clarifyingQuestionAsked";
      data: { requestId: string; toolCallId: string; question: string; options: string[] };
    }
  | {
      type: "toolCallStarted";
      data: {
        toolCallId: string;
        toolId: string;
        summary: string;
        autoApproved: boolean;
        via: string | null;
      };
    }
  | { type: "toolCallResult"; data: { toolCallId: string; ok: boolean; outputPreview: string } }
  | { type: "toolCallDenied"; data: { toolCallId: string; note: string | null } }
  | { type: "toolCallRejected"; data: { toolCallId: string; toolId: string; reason: string } }
  // A `spawn_subagent` call's own nested tool-use loop starting/finishing —
  // lets the UI collapse it into one step instead of showing the helper's
  // internal tool calls at the top level. `toolCallId` matches the
  // `spawn_subagent` call's own `toolCallStarted`/`toolCallResult` pair.
  | { type: "subagentStarted"; data: { toolCallId: string; kind: string; task: string } }
  | { type: "subagentFinished"; data: { toolCallId: string; ok: boolean; totalTokens: number } }
  /** The `create_artifact` agent-mode tool created a file this turn. Content
   * isn't streamed — the UI fetches it on demand via `api.getArtifact` when
   * the user opens the artifact's preview panel. */
  | { type: "artifactCreated"; data: { id: string; filename: string; sizeBytes: number } }
  // Prompt-injection review (any mode — see `crates/core/src/injection/`):
  /** A check of text the model is about to read is taking noticeably long
   * (cold classifier, long text); `source`/`name` describe the first item
   * still pending. Followed by `injectionCheckFinished`. */
  | { type: "injectionCheckStarted"; data: { source: InjectionSource; name: string | null } }
  | { type: "injectionCheckFinished"; data: Record<string, never> }
  /** Flagged text is held back until the user answers — reply via
   * `api.respondInjectionReview` with this `requestId`. `toolCallId` is set
   * for a tool result, `null` for user-side text (attachments, skills);
   * `via` names the helper kind when a subagent's tool returned it. */
  | {
      type: "injectionReviewRequested";
      data: {
        requestId: string;
        toolCallId: string | null;
        via: string | null;
        items: FlaggedItem[];
      };
    }
  /** One notice line for the streaming reply — see
   * `ChatMessage.injectionNotices`. */
  | { type: "injectionNotice"; data: { notice: InjectionNotice } };

/** A conversation's task type — selects which curated sampling defaults
 * apply (see `ModelSpec.taskDefaults` and `GENERAL_TASK_SAMPLING_DEFAULTS`).
 * `"general"` is both "classified as general" and "not classified yet" —
 * every conversation starts here and the request-triage classifier
 * (`api.classifyMessage`) may move it to a more specific type as the user
 * types; never shown in the chat UI, only reflected in the app's log file.
 * Mirrors Rust `TaskType`. */
export type TaskType = "general" | "coding" | "writing" | "mathScience" | "agentic";

/** Every `TaskType`, for iteration (the classifier's expected label set,
 * routing). Mirrors Rust `TaskType::ALL`. */
export const ALL_TASK_TYPES: TaskType[] = ["general", "coding", "writing", "mathScience", "agentic"];

/** The safety classifier's binary signal for a settled turn's user message
 * (`api.classifyRisk`) — whether it calls for the in-chat `SafetyBanner`.
 * Mirrors Rust `RiskSignal`. */
export type RiskSignal = "safe" | "risk";

/** Standard llama.cpp sampling/generation parameters, tuned per conversation
 * from the composer's Generation panel. Mirrors Rust `SamplingParams`. */
export interface SamplingParams {
  temperature: number;
  /** Nucleus sampling threshold (0-1); 1.0 disables it. llama.cpp default 0.95. */
  topP: number;
  /** Only consider this many top candidate tokens; 0 disables it. llama.cpp default 40. */
  topK: number;
  /** Drop tokens below this fraction of the top token's probability; 0 disables it. llama.cpp default 0.05. */
  minP: number;
  /** Discourage repeating recent tokens; 1.0 disables it. llama.cpp default 1.1. */
  repeatPenalty: number;
  /** Token window `repeatPenalty` looks back over; 0 disables it, -1 = whole context. llama.cpp default 64. */
  repeatLastN: number;
  /** Push the model toward topics it hasn't mentioned yet. llama.cpp default 0.0. */
  presencePenalty: number;
  /** Push the model away from words it's already used often this reply. llama.cpp default 0.0. */
  frequencyPenalty: number;
  /** Cap on reply length in tokens (llama.cpp's n_predict, OpenAI alias). -1 = unlimited. */
  maxTokens: number;
  /** Fixed RNG seed for reproducible replies. -1 = a fresh random seed every turn. */
  seed: number;
}

/** One of `SamplingParams`'s own field names — tied to it with `keyof` so the
 * two can never drift apart. Used by `CloudModelInfo.supportedParams` to name
 * which sliders a cloud model's provider actually honors; mirrors Rust
 * `SamplingParamKey`. */
export type SamplingParamKey = keyof SamplingParams;

/** llama.cpp's own defaults — what a brand-new conversation starts with. */
export const DEFAULT_SAMPLING: SamplingParams = {
  temperature: 0.7,
  topP: 0.95,
  topK: 40,
  minP: 0.05,
  repeatPenalty: 1.1,
  repeatLastN: 64,
  presencePenalty: 0,
  frequencyPenalty: 0,
  maxTokens: -1,
  seed: -1,
};

/** Every `SamplingParams` field, in the order the Generation panel shows
 * them. Shared so the panel and any comparison logic use one list instead of
 * duplicating it. */
export const SAMPLING_PARAM_KEYS: SamplingParamKey[] = [
  "temperature",
  "topP",
  "topK",
  "minP",
  "repeatPenalty",
  "repeatLastN",
  "presencePenalty",
  "frequencyPenalty",
  "maxTokens",
  "seed",
];

/** Model-agnostic sampling defaults per `TaskType` — what a conversation
 * falls back to when its model isn't in the curated catalog (a Hugging Face
 * or local-file model) or has no override for this task type. Mirrors Rust
 * `general_sampling_defaults`; non-general values are a hand-curated
 * compromise, not individually verified per model (same disclaimer posture
 * as `docs/CATALOG_MODEL_SPECS.md`'s task-defaults table). */
export const GENERAL_TASK_SAMPLING_DEFAULTS: Record<TaskType, SamplingParams> = {
  general: DEFAULT_SAMPLING,
  // Deterministic: coding wants the same correct answer reliably, not
  // creative variance.
  coding: { ...DEFAULT_SAMPLING, temperature: 0.2, topP: 0.9, topK: 20 },
  // A deliberate middle ground: this bucket now also covers what used to be
  // "translating" (wants low temperature/precision) alongside prose (wants
  // more variety), so it sits between `coding` and `general`.
  writing: { ...DEFAULT_SAMPLING, temperature: 0.7, topP: 0.9, topK: 40 },
  // Precise reasoning chains — low-variance like `coding`, plus a tighter
  // minP to cut off unlikely reasoning-step tokens.
  mathScience: { ...DEFAULT_SAMPLING, temperature: 0.2, topP: 0.9, topK: 20, minP: 0 },
  // Near-greedy: favors well-formed tool calls over creative variance.
  agentic: { ...DEFAULT_SAMPLING, temperature: 0.3, topP: 0.9, topK: 20 },
};

/** Recommended sampling defaults for `spec` at `taskType`: when
 * `showThinking` is true and the catalog declared a thinking-mode override
 * for this task (`taskDefaultsThinking`), that wins; otherwise the model's
 * regular (instruct/non-thinking) override (`taskDefaults`) when declared;
 * otherwise the model-agnostic general default. `spec` is `undefined` for a
 * cloud model or when nothing's selected yet — both get the general
 * default, same as any model outside the curated catalog. Mirrors Rust
 * `Catalog::recommended_sampling`. */
export function recommendedSampling(
  spec: ModelSpec | undefined,
  taskType: TaskType,
  showThinking: boolean,
): SamplingParams {
  const thinking = showThinking ? spec?.taskDefaultsThinking?.[taskType] : undefined;
  return thinking ?? spec?.taskDefaults?.[taskType] ?? GENERAL_TASK_SAMPLING_DEFAULTS[taskType];
}

/** Whether every `SamplingParams` field in `a` matches `b` — used to show
 * the Generation panel's "auto-suggested" hint only while the conversation's
 * sampling still matches its recommendation exactly. */
export function samplingParamsEqual(a: SamplingParams, b: SamplingParams): boolean {
  return SAMPLING_PARAM_KEYS.every((key) => a[key] === b[key]);
}

/** This app's historical default context window. */
export const DEFAULT_CONTEXT_LENGTH = 8192;

export interface Conversation {
  id: string;
  title: string;
  createdAtMs: number;
  updatedAtMs: number;
  messages: ChatMessage[];
  /** This conversation's sampling parameters, tuned from the composer's
   * Generation panel — takes effect on the very next message. */
  sampling: SamplingParams;
  /** This conversation's context window. Unlike `sampling`, this is an
   * engine-startup parameter: changing it only takes effect the next time
   * the model (re)loads, via the panel's "Restart model to apply". */
  contextLength: number;
  /** This conversation's request-triage classification (see `TaskType`),
   * set automatically as the user types. Never shown in the chat UI —
   * drives `useTriageAutopilot`'s silent sampling/model auto-pick and the
   * Generation panel's "auto-suggested" hint (`recommendedSampling`). */
  taskType: TaskType;
  /** The project this conversation belongs to, if any — see `Project`. A
   * conversation belongs to at most one project at a time; `null`/`undefined`
   * means it isn't grouped into any. */
  projectId?: string | null;
  /** Whether this conversation is pinned to the top of the sidebar.
   * Synced across paired devices, so a pin keeps it out of every device's
   * "Delete old conversations" cleanup (see the Rust `app_db::SCHEMA_V10`
   * doc comment). */
  pinned: boolean;
  /** Edit-history and retry-history variants, keyed by the id of the
   * message immediately preceding a branch point (or `ROOT_BRANCH_KEY`
   * for a branch at array index 0). Like `pinned`, this DOES sync
   * across paired devices — it rides along inside the same whole-conversation
   * snapshot the sync engine already broadcasts on any edit/retry-shaped
   * rewrite (see `crate::sync::conversations`' doc comment on the Rust
   * side), so no extra sync plumbing is needed here. */
  branches: Record<string, BranchPoint>;
  /** Set when this conversation was created via "Fork" (assistant messages
   * only) from another conversation's message — lets the UI show a "forked
   * from another conversation" hint anchored to `ForkOrigin.messageId`.
   * `undefined` for every ordinary conversation. Synced the same way as
   * `branches`: it rides along inside the same whole-conversation snapshot.
   * Mirrors Rust `Conversation.forked_from`. */
  forkedFrom?: ForkOrigin;
}

/** Where a forked conversation came from — see `Conversation.forkedFrom`.
 * Mirrors Rust `ForkOrigin`. */
export interface ForkOrigin {
  conversationId: string;
  /** Id of the last message copied from the source conversation at fork
   * time. Looked up by id (not position) when rendering the hint, so it
   * stays correctly anchored even as the forked conversation grows. */
  messageId: string;
}

/** The `Conversation.branches` key for a branch point at message index 0
 * (no preceding message to anchor on). Mirrors Rust `ROOT_BRANCH_KEY`. */
export const ROOT_BRANCH_KEY = "__root__";

/** One edit-history or retry-history branch point: the sibling versions of
 * whatever used to follow the anchor message, plus which one is currently
 * spliced into `Conversation.messages`. Each `variants[i]` is a full
 * snapshot of the message-array *tail* from the branch point onward —
 * switching variants is `messages[0..branchIndex] + variants[active]`.
 * Storing full tails (rather than a single message) is what lets a variant
 * itself contain further nested branch points for free: their own entries
 * just live elsewhere in this same flat map, keyed by ids that happen to
 * appear inside this tail. Mirrors Rust `BranchPoint`. */
export interface BranchPoint {
  variants: ChatMessage[][];
  active: number;
}

/** The `Conversation.branches` key for the branch point right after
 * `messages[index - 1]` (or `ROOT_BRANCH_KEY` when `index === 0`) — i.e.
 * the anchor whose siblings are `messages[index]` and everything after. */
export function branchAnchorKey(messages: ChatMessage[], index: number): string {
  return index === 0 ? ROOT_BRANCH_KEY : messages[index - 1].id;
}

export interface ConversationMeta {
  id: string;
  title: string;
  createdAtMs: number;
  updatedAtMs: number;
  messageCount: number;
  /** Copied from `Conversation.taskType` — not shown in the sidebar, kept
   * alongside the rest of this lightweight per-conversation summary. */
  taskType: TaskType;
  /** Copied from `Conversation.projectId`. */
  projectId?: string | null;
  /** Copied from `Conversation.pinned`. */
  pinned: boolean;
  /** A reply finished while this conversation wasn't in front of the user,
   * and they haven't opened it since — the sidebar's "New reply" hint. Not
   * on `Conversation`: only `api.setConversationUnread` writes it. Synced
   * across paired devices. */
  unread: boolean;
}

/** A user-named group of conversations (see `crates/core/src/projects.rs`) —
 * membership lives on the conversation side (`Conversation.projectId`), not
 * as a list here. */
export interface Project {
  id: string;
  name: string;
  createdAtMs: number;
  updatedAtMs: number;
}

/** One conversation's match against a sidebar search query — see
 * `api.searchConversations`. */
export interface ConversationSearchResult {
  id: string;
  title: string;
  updatedAtMs: number;
  projectId?: string | null;
  /** A short excerpt around the match — the title itself when that's what
   * matched, otherwise an excerpt from the matching message. */
  snippet: string;
}

/** One supporting Markdown file inside a folder-based [`Skill`], alongside
 * its `SKILL.md` — see `Skill.files`. */
export interface SkillFile {
  /** Relative to the skill's own folder, e.g. "reference.md" or
   * "examples/checklist.md" — also shown to the user so they know what to go
   * edit by hand. */
  path: string;
  content: string;
}

/** A user-authored, reusable instruction snippet (see
 * `crates/core/src/skills.rs`) — either a single Markdown file, or a folder
 * (a `SKILL.md` plus supporting Markdown files) when there's more to it than
 * one page. Typing `/<slug>` as the first word of a chat message invokes it. */
export interface Skill {
  /** The filename/folder stem and `/<slug>` chat trigger. Generated
   * server-side from `name` the first time a new skill (empty `slug`) is
   * saved; stable after that — renaming only changes `name`, never the
   * trigger. */
  slug: string;
  name: string;
  /** A short one-line summary shown in the `/`-autocomplete list. */
  description: string;
  /** The instructions themselves, as Markdown. For a folder skill, this is
   * its `SKILL.md` body — the part the in-app editor can actually edit. */
  body: string;
  /** Supporting Markdown files bundled alongside `body` when this skill is a
   * folder rather than a single file — absent (not just empty) for an
   * ordinary skill, since the backend omits the field entirely when there's
   * nothing in it. Added via Skills → Import or by hand; not editable from
   * the Skills UI itself. Folded into the prompt after `body` when the skill
   * is invoked — see `expandSkillBody` in `state/store.ts`. */
  files?: SkillFile[];
}

/** Which backend answers a chat turn — the local engine, or a signed-in
 * account's cloud model (`crate::cloud`). Orthogonal to the web-search
 * plugin toggle, which only applies to "local". */
export type ChatBackend = "local" | "cloud";

export interface Settings {
  onboardingCompleted: boolean;
  /** The optional name collected during onboarding for a personalized greeting. */
  userName: string | null;
  selectedModelId: string | null;
  systemPrompt: string;
  /** When on (default), reasoning models think and the UI shows the trace.
   * When off, the engine is started with thinking suppressed. */
  showThinking: boolean;
  /** Show each reply's generation speed (tokens/sec) and wall-clock duration
   * (thinking + answer combined) beneath it. Off by default. */
  showGenerationStats: boolean;
  /** Run the model on the CPU only, even when a GPU is available (off by
   * default). Useful when a GPU driver is unstable; takes effect next load. */
  forceCpu: boolean;
  /** Let huge MoE models that don't fit in memory run anyway, streaming their
   * weights from disk — much slower, but works (off by default). When on, a
   * model may show fit "diskSlow" instead of "tooBig". */
  diskOffload: boolean;
  /** Master switch for the smart model load/unload feature (on by default —
   * a resource-management feature, not a privacy opt-out): unloads the
   * running model after it's sat idle this long, to free RAM/VRAM, and —
   * while it's on — the composer treats typing as activity (pausing the
   * idle countdown) and starts loading the model as soon as typing begins if
   * it isn't loaded yet. See ChatView's `Composer`. */
  autoUnloadEnabled: boolean;
  /** Idle minutes before auto-unload fires. Only meaningful when
   * `autoUnloadEnabled`. Clamped to 1-120 in the Settings UI. */
  autoUnloadMinutes: number;
  /** Disk-slow (mmap-streamed) models are exempt from auto-unload unless
   * this is on (off by default) — they already use no VRAM, and reloading
   * one can take several minutes. */
  autoUnloadIncludeDiskSlow: boolean;
  /** Load the selected model automatically shortly after OpenPal opens, so
   * it's ready before the user starts typing (on by default). Independent of
   * `autoUnloadEnabled`. See store's `boot()`. */
  autoLoadOnStartup: boolean;
  /** How much detail OpenPal records in its log file: "error" | "warn" |
   * "info" (default) | "debug" | "trace". Read at startup, so a change takes
   * effect the next time the app opens. */
  logLevel: string;
  /** UI display language, "en" or "fr" — validity owned by the Settings UI's
   * <select>, same pattern as `logLevel`. */
  language: "en" | "fr";
  /** Read-aloud (Voice plugin) controls. `voiceSpeed` is a multiplier on the
   * voice's natural pace (1 = normal, >1 faster); `voiceExpressiveness` scales
   * its liveliness (1 = natural, 0 = flat/even, >1 = livelier);
   * `voiceSentencePause` is the gap between sentences in seconds. Rust maps
   * these to Piper's synthesis parameters in `speak_text`. */
  voiceSpeed: number;
  voiceExpressiveness: number;
  voiceSentencePause: number;
  /** The Piper voice the read-aloud uses, as a `<locale>-<name>-<quality>` id.
   * Picked in Settings from the installed voices; defaults to en_US-amy-medium. */
  voiceId: string;
  /** Check for a newer OpenPal version shortly after startup and offer to
   * install it (on by default). The update itself still always asks for
   * confirmation before downloading or installing. */
  autoCheckUpdates: boolean;
  /** A version the user chose "Skip this version" for — the update dialog
   * won't reopen automatically for it, though "Check for updates now" always
   * shows it. */
  skippedUpdateVersion: string | null;
  /** Check for a newer llama.cpp AI engine build shortly after startup, on
   * its own schedule independent of `autoCheckUpdates` — the engine ships
   * far more often than full OpenPal releases. Off by default. */
  autoCheckEngineUpdates: boolean;
  /** An engine tag (e.g. "b10520") the user chose "Skip this version" for. */
  skippedEngineTag: string | null;
  /** Which backend answers a chat turn. Defaults to "local". */
  chatBackend: ChatBackend;
  /** The cloud model to use when `chatBackend` is "cloud", chosen from the
   * "Cloud models" group in the Library. `null` until the user picks one. */
  selectedCloudModelId: string | null;
  /** The model to auto-load when OpenPal starts, chosen explicitly in
   * Settings — independent of `selectedModelId` (whatever's currently being
   * chatted with). `null` means no explicit choice: boot() falls back to
   * `selectedModelId`, i.e. resuming whatever was last used. */
  defaultModelId: string | null;
  /** Model ids the user has starred as a favorite, from the Library page or
   * the composer's model quick-switcher — surfaced per-model as
   * `ModelInfo.favorite`. Device-local; order doesn't matter, only
   * membership. Toggled via `api.setModelFavorite`, not `updateSettings`. */
  favoriteModelIds: string[];
  /** Settings → AI model's "Add your own models" switch: shows the "add a
   * model from a Hugging Face link or a .gguf file" section at the bottom of
   * the Models page. Off by default. Only hides the way in — models already
   * added stay listed and usable either way. */
  customModelsEnabled: boolean;
  /** Master switch for agent mode (tool use: shell/code execution,
   * filesystem read/write). Off by default — every tool call still needs
   * per-call consent even when this is on. */
  agentModeEnabled: boolean;
  /** Auto-apply the request-triage classification's recommended sampling
   * (`recommendedSampling`) to the active conversation. On by default —
   * classification stays entirely local, via the running engine. Never
   * shown in the chat UI; visible only as a log line when it actually
   * applies a preset. */
  autoPickSampling: boolean;
  /** Master switch for the safety-classifier feature: after each settled chat
   * turn, checks the user's message for signs of self-harm or a severe
   * mental-health crisis and, on a match, shows an in-chat banner pointing to
   * crisis support resources (`api.classifyRisk`). On by default — unlike
   * `autoPickSampling`, this is a safety net meant to protect people who
   * never open Settings, not a power-user convenience. Classification stays
   * entirely local, via the running engine, and is skipped when `chatBackend`
   * is "cloud". */
  riskDetectionEnabled: boolean;
  /** Multi-device conversation sync (Phase 3), gated behind Settings →
   * Advanced → Sync like every other opt-in feature — off by default. Only
   * does anything once pairing (device-list UI, same section) already
   * exists; the Rust `sync_loop` reads this before doing any background
   * work. */
  syncEnabled: boolean;
  /** Overrides the default sync server when set — the Settings → Advanced →
   * Sync "Server" field, for pointing at a self-hosted relay instead of
   * OpenPal's own instance. `null` (the default) means "use the default". */
  syncRelayUrl: string | null;
  /** The Settings → Advanced "Experimental features" switch, off by
   * default: the one gate for everything still being tested. A plugin marked
   * experimental in its manifest is hidden and inactive while it's off (the
   * backend's `list_plugins` omits it), whatever its own Plugins switch says;
   * any other experimental feature gates on `experimentalFeaturesOn` in
   * `store.ts`. Turning it off never rewrites a feature's own choice, so
   * turning it back on restores each one as it was. */
  experimentalFeatures: boolean;
  /** The app's colour theme (Settings → General, and the onboarding step):
   * "system" follows the OS's light or dark setting and is the default,
   * "light" and "dark" pin one whatever the OS says. A `String` on the Rust
   * side, like `language`, so code that reads this must treat a missing or
   * unrecognised value as "system" — `normalizeTheme` in `lib/theme.ts` does. */
  theme: "system" | "light" | "dark";
  /** Settings → General → Notifications "Desktop notifications": mirror the
   * four model/answer alerts as OS notifications while OpenPal is not the
   * focused window. Off by default, and off for a settings row saved before the
   * field existed — so code that reads it can compare with `=== true`. */
  desktopNotifications: boolean;
  /** Settings → Your data "Delete old conversations": conversations not used
   * (`updatedAtMs`) in this many days are deleted automatically, on every
   * paired device, unless pinned or in a project. `null` (the default, and
   * what a settings row saved before the field existed reads as) means never.
   * Mirrors the Rust `Settings::auto_delete_after_days` (`Option<u32>`). */
  autoDeleteAfterDays: number | null;
}

/** The Settings UI's language picker, and every other file that needs to name
 * this type — single source of truth is `Settings["language"]` itself. */
export type Language = Settings["language"];

/** Whether the OS lets OpenPal post notifications, as far as it can tell
 * (mirrors `NotificationPermission` in `crates/app/src/notify/mod.rs`):
 * - "granted": allowed, or nothing to ask on this OS (Windows and Linux answer
 *   this whatever the user's system settings are), or the OS couldn't be asked;
 * - "denied": the user said no, which only the OS's settings can undo (only
 *   macOS can say so today);
 * - "prompt": not asked yet, so turning the switch on shows the OS's prompt;
 * - "unsupported": this build has no notification backend, which
 *   `AppInfo.nativeNotifications` already said at boot. */
export type NotificationPermission = "granted" | "denied" | "prompt" | "unsupported";

/** A click (or tap) on one of OpenPal's notifications, as `take_notification_click`
 * hands it back (mirrors `NotificationClick` in `crates/app/src/notify/mod.rs`).
 * `conversationId` is the conversation the notification was about ("answer
 * ready"); null for one that only brings OpenPal forward. */
export interface NotificationClick {
  conversationId: string | null;
}

/** The theme picker's choices — single source of truth is `Settings["theme"]`
 * itself, same as `Language`. */
export type Theme = Settings["theme"];

/** Build/runtime info: whether this is the no-install Portable build, where
 * its data lives, and the running app's own version. From `get_app_info`. */
export interface AppInfo {
  portable: boolean;
  dataDir: string;
  version: string;
  /** True until the user has seen and acknowledged their one-time data
   * encryption recovery code (`RecoverySetup`) — set for every launch of an
   * existing install that predates the recovery-code feature, not just a
   * brand-new one, since the encryption key it backs up already exists for
   * everyone. Checked independently of `Settings.onboardingCompleted`. Means
   * "no code has ever been generated yet, run the one-time save ceremony" —
   * distinct from `needsRecovery` below. */
  needsRecoverySetup: boolean;
  /** True when the OS keychain that normally unlocks the encryption key
   * silently failed (a lost keychain entry, an OS reinstall, …) and the app
   * needs the user to type their existing recovery code back in
   * (`RecoveryUnlock`) before anything else can proceed. Distinct from
   * `needsRecoverySetup` above, which means no code exists yet. */
  needsRecovery: boolean;
  /** True whenever app-lock (the optional PIN gate) is enabled. Unlike
   * `needsRecoverySetup`/`needsRecovery`, this doesn't durably resolve to
   * false once satisfied — it re-arms every launch, so the frontend tracks
   * its own session-only "already passed the gate" flag instead. */
  needsAppUnlock: boolean;
  /** Compile-time platform fact — gates the Settings "Remote pairing"
   * section, which only makes sense on the one platform where the engine
   * itself is `RemoteBackend` (see `crates/app/src/state.rs`). */
  isAndroid: boolean;
  /** Whether this build can post OS notifications at all (false on Android and
   * iOS, which have no backend yet). Gates the "Desktop notifications" switch,
   * its onboarding step, Settings search and native delivery; read it through
   * `hasNativeNotifications`. What the OS allows is `NotificationPermission`,
   * which is only known later. */
  nativeNotifications: boolean;
}

/** A newer OpenPal version, as returned by `checkForUpdate`. */
export interface AvailableUpdate {
  version: string;
  notes: string;
  pubDate: string;
}

/** A newer llama.cpp AI engine build, as returned by `checkForEngineUpdate`. */
export interface EngineUpdateInfo {
  currentTag: string;
  tag: string;
}

/** A selectable Piper voice from the Voice plugin's catalog (see `listVoices`).
 * `installed` is true once its model is downloaded; not-yet-installed voices can
 * be fetched on demand with `downloadVoice`. */
export interface VoiceInfo {
  id: string;
  name: string;
  language: string;
  gender: string;
  quality: string;
  sizeMb: number;
  /** Short MP3 sample URL to preview the voice (empty if unavailable). */
  previewUrl: string;
  installed: boolean;
}

/* ------------------------------- Plugins -------------------------------- */

/** One-time install state of a plugin (adjacently tagged, camelCase). */
export type SetupState =
  | { type: "notSetup" }
  | { type: "settingUp"; data: { message: string; fraction: number | null } }
  | { type: "ready" }
  | { type: "failed"; data: { message: string } };

/** Long-lived sidecar state for a plugin. */
export type ServiceState =
  | { type: "stopped" }
  | { type: "starting" }
  | { type: "running"; data: { port: number } }
  | { type: "failed"; data: { message: string } }
  | { type: "notApplicable" };

/** A discovered plugin and its live state (flat; returned by `list_plugins`). */
export interface PluginInfo {
  id: string;
  name: string;
  description: string;
  version: string;
  author: string;
  capabilities: string[];
  enabled: boolean;
  setup: SetupState;
  service: ServiceState;
  hasChat: boolean;
  /** Sits behind "Experimental features" (`"experimental": true` in its
   * manifest). Only ever listed while that switch is on. */
  experimental: boolean;
}

/** Streamed by `set_plugin_enabled` while a plugin is being enabled. */
export type PluginEvent =
  | { type: "setup"; data: { message: string; fraction: number | null } }
  | { type: "ready" }
  | { type: "failed"; data: { message: string } };

/* ---------------------------- Remote Access ------------------------------ */

/** One of this desktop's network interfaces — `mac` is a plain
 * `"aa:bb:cc:dd:ee:ff"` string on the wire (Rust's `MacAddress` has a
 * custom `Serialize`/`Deserialize`, not the raw byte array its `[u8; 6]`
 * shape might suggest). */
export interface NetworkInterfaceInfo {
  name: string;
  mac: string;
  ipv4Addrs: string[];
}

/** Current Remote Access status (`get_remote_access_status`). */
export interface RemoteAccessStatusInfo {
  /** The persisted user setting — independent of `running`, so the UI can
   * show "starting…"/"failed to start" instead of the toggle silently
   * disagreeing with reality. */
  enabled: boolean;
  running: boolean;
  port: number | null;
  interfaces: NetworkInterfaceInfo[];
  pairedDeviceCount: number;
}

/** One paired device (`list_paired_devices`). The bearer token itself is
 * never returned once issued — only this. */
export interface PairedDeviceInfo {
  id: string;
  name: string;
  createdAtMs: number;
  lastSeenMs: number | null;
}

/** Best-effort "is Wake-on-LAN enabled on this machine" check
 * (`get_wol_status`). `unknown` (never a false `disabled`) whenever this
 * build couldn't determine the real answer on this OS. */
export type WolStatus =
  | { type: "enabled" }
  | { type: "disabled" }
  | { type: "unknown"; data: { reason: string } };

/** Everything a future mobile client needs to complete pairing — shown as
 * a QR code, with `code` also enterable by hand. */
export interface PairingPayload {
  v: number;
  code: string;
  addrs: string[];
  port: number;
  fingerprint: string;
  mac: string | null;
  expiresAtMs: number;
}

/** Streamed by `start_pairing_session` while one pairing session is open. */
export type PairingEvent =
  | { type: "waiting"; data: { payload: PairingPayload } }
  | { type: "paired"; data: { deviceName: string } }
  | { type: "expired" }
  | { type: "failed"; data: { message: string } };

/** The other side of Remote Access — this device (Android) paired with
 * someone else's gateway (`remote_pair`/`remote_client_status`). */
export interface RemotePairingRecord {
  deviceId: string;
  deviceName: string;
  hostAddr: string;
  port: number;
  fingerprintHex: string;
  pairedAtMs: number;
}

/** One gateway seen while browsing the LAN (`remote_discover_hosts`).
 * `fingerprintPrefix` is a pre-filter for the device list only — never a
 * trust decision; the real fingerprint is pinned from the actual TLS
 * handshake during `remotePair`. */
export interface DiscoveredHost {
  deviceId: string;
  deviceName: string;
  fingerprintPrefix: string;
  addrs: string[];
  port: number;
}

/** The full picture the remote-inference Settings section shows
 * (`remote_inference_status`): whether the app's engine surface is routed to
 * the paired device, who it's paired with (if anyone), and what the active
 * engine reports — the remote engine while connected, the local one
 * otherwise. */
export interface RemoteInferenceStatus {
  active: boolean;
  pairing: RemotePairingRecord | null;
  engine: EngineStatus;
}

/* ------------------------------ PIN entry --------------------------------- */

/** Outcome of one PIN-entry attempt (`verifyParentalPin`, `setParentalControls`).
 * `lockedForSeconds` is non-zero either because this attempt landed during an
 * active lockout, or because it was the failure that just triggered one —
 * either way, `PinPad` can start counting down immediately. Mirrors Rust
 * `PinCheckResult`; deliberately not parental-specific, since the PIN itself
 * is meant to be reused by other PIN-gated features later. */
export interface PinCheckResult {
  ok: boolean;
  lockedForSeconds: number;
}

/** Outcome of one app-lock PIN-entry attempt (`verifyAppLockPin`). Same
 * shape as `PinCheckResult` plus a running `failCount`, which
 * `AppLockGate` watches to auto-escalate from the PIN pad to the recovery-
 * code form after too many misses. */
export interface AppLockPinCheckResult {
  ok: boolean;
  lockedForSeconds: number;
  failCount: number;
}

/** Live app-lock state (`getAppLockStatus`) — whether a PIN is currently
 * required at launch. */
export interface AppLockStatusView {
  enabled: boolean;
}

/* --------------------------- Parental controls --------------------------- */

/** Which of the three limits is blocking new messages right now. */
export type LockReason = "schedule" | "usageCap" | "messageCap";

/** Live lock state for the chat composer — safe to read without the PIN. */
export interface ParentalStatus {
  locked: boolean;
  reason: LockReason | null;
  scheduleEnabled: boolean;
  /** "HH:MM", 24h local time. `allowedStart > allowedEnd` wraps past midnight. */
  allowedStart: string;
  allowedEnd: string;
  messagesSentToday: number;
  /** null when that particular cap is off. */
  messageCapPerDay: number | null;
  usageSecondsToday: number;
  usageCapSeconds: number | null;
}

/** The stored configuration minus the PIN itself. Readable without the PIN;
 * changing any of it goes through `setParentalControls`. */
export interface ParentalSettingsView {
  enabled: boolean;
  hasPin: boolean;
  scheduleEnabled: boolean;
  allowedStart: string;
  allowedEnd: string;
  usageCapEnabled: boolean;
  usageCapMinutes: number;
  messageCapEnabled: boolean;
  messageCapPerDay: number;
}

/** What Settings submits to `setParentalControls`. `newPin`, when non-empty,
 * replaces the stored PIN — required the first time `enabled` becomes true. */
export interface ParentalConfigInput {
  enabled: boolean;
  scheduleEnabled: boolean;
  allowedStart: string;
  allowedEnd: string;
  usageCapEnabled: boolean;
  usageCapMinutes: number;
  messageCapEnabled: boolean;
  messageCapPerDay: number;
  newPin?: string | null;
}

/* -------------------------------- Account -------------------------------- */

/** Cloud request usage for the current billing period. Absent when the
 * backend doesn't report one for this plan. */
export interface Quota {
  used: number;
  limit: number;
}

export interface Account {
  email: string;
  tier: string;
  capabilities: string[];
  quota?: Quota;
}

/** Streamed by `sign_in` while a device-session sign-in is in flight. */
export type AccountEvent =
  | { type: "awaitingBrowser"; data: { verificationUrl: string } }
  /** `persisted` is false when the refresh token couldn't be saved to the OS
   * keychain (e.g. no Secret Service provider on a headless/minimal Linux
   * desktop) — signed in for this run, but will need to sign in again next
   * launch. */
  | { type: "complete"; data: { account: Account; persisted: boolean } }
  | { type: "failed"; data: { message: string } }
  | { type: "expired" };

// ---------------------------------------------------------------------------
// Multi-device sync — device pairing and revocation (docs/SYNC_PAIRING_PROTOCOL.md,
// Phase 2). Mirrors `openpal_core::sync::pairing::PairingEvent` and
// `openpal_core::sync::roster::DeviceSummary` exactly (same
// `#[serde(rename_all = "camelCase")]` tag/data shape as `AccountEvent` above).
// ---------------------------------------------------------------------------

/**
 * Streamed by `sync_pair_listen` (Device A, "add a device") and
 * `sync_pair_request` (Device B, "join a group") while a pairing session is
 * in flight. Both `incomingRequest` (Device A) and `awaitingConfirmation`
 * (Device B) carry a `fingerprint` that MUST be rendered as the visually
 * dominant element of its confirm card, never a small caption under the
 * button — the whole protocol's security rests on the human actually
 * comparing it against the other device's screen before confirming.
 *
 * Named `SyncPairingEvent` (not the more obvious `PairingEvent`) to avoid
 * colliding with Remote Access's own, differently-shaped `PairingEvent`
 * (its QR/LAN device-pairing flow, `startPairingSession` in `api.ts`) —
 * two unrelated features independently reached for the same generic name.
 */
export type SyncPairingEvent =
  | { type: "incomingRequest"; data: { deviceLabel: string; fingerprint: string } }
  | { type: "awaitingChallenge" }
  | { type: "awaitingConfirmation"; data: { fingerprint: string } }
  | { type: "readyToInstall" }
  | { type: "complete"; data: { deviceId: string } }
  | { type: "rejected" }
  | { type: "expired" }
  | { type: "failed"; data: { message: string } };

/** One row of `sync_device_list()` — this device plus every device it has paired with. */
export interface DeviceSummary {
  deviceId: string;
  label: string;
  genAdded: number;
  isSelf: boolean;
}
