/** Friendly formatting — no jargon, no decimals nobody needs. */

import i18n from "../i18n";
import type { ChatStats, FitLevel, ModelInfo } from "./types";

/** Never chosen as the app's own suggestion — an opt-in, always-slow tier
 * like "diskSlow" must not appear as a recommended default or onboarding
 * alternative. Mirrors Rust's `FitLevel::is_auto_recommendable` in
 * crates/core/src/types.rs — keep both in sync. */
export function isAutoRecommendable(fit: FitLevel): boolean {
  return fit !== "diskSlow";
}

/** Shared FitLevel -> label lookup, used everywhere a fit verdict is shown
 * (Library's ModelCard, the Discover model page's quant picker) so the same
 * fit always reads the same way.
 *
 * These functions call i18n.t() directly rather than the useTranslation()
 * hook, so calling one does not itself trigger a React re-render on language
 * change — a calling component only re-renders with the new language because
 * it also calls useTranslation() for its own surrounding text. */
export function fitLabel(fit: FitLevel): string {
  return i18n.t(`format.fitLabel.${fit}`);
}

/** Badge CSS class per fit — unlike `fitLabel`, this is class names, not
 * copy, so it stays untranslated. */
export const FIT_BADGE: Record<FitLevel, string> = {
  perfect: "badge-perfect",
  good: "badge-good",
  tight: "badge-tight",
  diskSlow: "badge-diskslow",
  tooBig: "badge-toobig",
};

/** Binary "will this fit, roughly" framing for the HF search-result grid —
 * a repo's real MoE-ness and exact quant sizes aren't known before the user
 * opens it (the grid's `fit` is a synthetic Q4_K_M-sized estimate), so it
 * shows a plain yes/no hint instead of the full 5-level badge the model
 * page's precise quant picker earns once opened. */
export function roughFitLabel(fit: FitLevel): string {
  return i18n.t(fit === "tooBig" ? "format.roughFitLabel.no" : "format.roughFitLabel.yes");
}

/** Whether the engine's starting/current model (by id) is a disk-streamed
 * ("diskSlow") model — surfaces that show "Warming up…" copy while the
 * engine loads use this to add an honest "this can take a few minutes"
 * note. `models` may not have loaded yet right after launch; an unknown fit
 * reads as `false`, which is the safe default (falls back to the plain
 * "warming up" copy rather than claiming to know it's fast). */
export function isStartingModelDiskSlow(models: ModelInfo[], modelId: string): boolean {
  return models.find((m) => m.spec.id === modelId)?.fit === "diskSlow";
}

/** The display name of the model with this id — `spec.name`, the same label
 * the sidebar's engine pill shows — or null when `models` doesn't hold it
 * (not loaded yet, or the model was deleted). Callers that need words for the
 * user fall back to a generic name; they never show the raw id, which for a
 * custom model reads like `custom-qwen3-4b-q4-k-m`. */
export function engineModelName(models: ModelInfo[], modelId: string): string | null {
  return models.find((m) => m.spec.id === modelId)?.spec.name || null;
}

/** Total download/on-disk size across every shard (shard 1 + `extraShards`).
 * Model-size displays must use this, not `.sizeBytes` alone, or a split
 * model's real size reads as shard 1's size only. Structurally typed so it
 * works for both `ModelSpec` and the Hugging Face browser's `HfFile`, which
 * shares the same shard shape. Mirrors Rust's `ModelSpec::total_size_bytes`
 * in crates/core/src/types.rs. */
export function totalModelSizeBytes(spec: {
  sizeBytes: number;
  extraShards?: { sizeBytes: number }[];
}): number {
  return spec.sizeBytes + (spec.extraShards ?? []).reduce((sum, s) => sum + s.sizeBytes, 0);
}

export function formatBytes(bytes: number): string {
  const GB = 1024 ** 3;
  const MB = 1024 ** 2;
  if (bytes >= GB) {
    const gb = bytes / GB;
    return `${gb >= 10 ? Math.round(gb) : Math.round(gb * 10) / 10} GB`;
  }
  return `${Math.max(1, Math.round(bytes / MB))} MB`;
}

export function ramLabel(bytes: number): string {
  return `${Math.round(bytes / 1024 ** 3)} GB`;
}

export function formatSpeed(bytesPerSec: number): string {
  return `${formatBytes(bytesPerSec)}/s`;
}

/** A disk-streamed model's estimated generation speed, in beginner language —
 * rounded coarsely since this is an estimate, not a promise. Below ~1 token
 * per second, a rate is meaningless to a beginner ("0.1 tokens per second"),
 * so switch to what it means for them: how long a short (~100-token) answer
 * takes. The real-hardware bench in docs/design/DISK_OFFLOAD.md measured 0.124
 * tok/s — ~15 minutes for a short answer — so this copy must not undersell
 * the wait. */
export function formatTokensPerSec(tokensPerSec: number): string {
  if (tokensPerSec >= 10) {
    return i18n.t("format.tokensPerSec.fast", { count: Math.round(tokensPerSec) });
  }
  if (tokensPerSec >= 1) {
    return i18n.t("format.tokensPerSec.medium", {
      count: Math.round(tokensPerSec * 10) / 10,
    });
  }
  const minutesPerShortAnswer = 100 / Math.max(tokensPerSec, 0.001) / 60;
  if (minutesPerShortAnswer >= 60) {
    return i18n.t("format.tokensPerSec.veryLongWait");
  }
  if (minutesPerShortAnswer >= 10) {
    return i18n.t("format.tokensPerSec.longWait", {
      count: Math.round(minutesPerShortAnswer / 5) * 5,
    });
  }
  return i18n.t("format.tokensPerSec.slow");
}

/** Fast/medium/slow tier for a model's estimated generation speed — the same
 * cutoffs `formatTokensPerSec` already uses for its wording (>=10 reads as a
 * fluent "tokens per second", >=1 is still workable, below that switches to a
 * minutes-based warning), reused here so a speed indicator's color can never
 * disagree with its own text. */
export function speedTier(tokensPerSec: number): "fast" | "medium" | "slow" {
  if (tokensPerSec >= 10) return "fast";
  if (tokensPerSec >= 1) return "medium";
  return "slow";
}

/** Compact "12 tok/s" / "<1 tok/s" label for a speed indicator in a list
 * row — unlike `formatTokensPerSec`'s beginner prose (built for a single
 * model's spotlight and prone to full sentences at the slow end), this stays
 * short enough to sit next to a model's name in a dropdown or in the Models
 * page's compact row.
 *
 * Shares `speedTier`'s cutoffs, and the same `_one`/`_other` shape as
 * `formatTokensPerSec` — French's "one" category covers everything under 2,
 * so 1.2 reads "1.2 jeton/s" while 3.4 reads "3.4 jetons/s". Wherever this
 * is shown on its own, pair it with `formatTokensPerSec` in a `title` and an
 * `.sr-only` span so the beginner prose stays reachable. */
export function formatSpeedCompact(tokensPerSec: number): string {
  if (tokensPerSec >= 10) {
    return i18n.t("format.tokensPerSecCompact.fast", { count: Math.round(tokensPerSec) });
  }
  if (tokensPerSec >= 1) {
    return i18n.t("format.tokensPerSecCompact.medium", {
      count: Math.round(tokensPerSec * 10) / 10,
    });
  }
  return i18n.t("format.tokensPerSecCompact.slow");
}

/** Compact "2.3s" / "1m 4s" / "1h 12m" duration, rounded coarsely once past a
 * minute. Shared by `formatChatStats` (a reply's actual generation time) and
 * `estimateAnswerDuration` (a model's projected time for a long answer). */
export function formatDuration(seconds: number): string {
  const roundedSeconds = Math.round(seconds);
  const minutes = Math.floor(roundedSeconds / 60);
  if (roundedSeconds < 60) {
    return `${seconds < 10 ? Math.round(seconds * 10) / 10 : roundedSeconds}s`;
  }
  if (minutes < 60) return `${minutes}m ${roundedSeconds % 60}s`;
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}

/** Compact, precise "12.4 tok/s · 2.3s" readout for a reply's generation
 * stats — shown beneath a message when "Show generation stats" is on.
 * Unlike `formatTokensPerSec`'s beginner prose, this is the raw numbers for
 * people who want them. `durationMs` covers thinking + the answer combined
 * (see `ChatStats`). */
export function formatChatStats(stats: ChatStats): string {
  const tps =
    stats.tokensPerSec >= 10
      ? `${Math.round(stats.tokensPerSec)}`
      : `${Math.round(stats.tokensPerSec * 10) / 10}`;
  return `${tps} tok/s · ${formatDuration(stats.durationMs / 1000)}`;
}

/** Tokens a typical long answer runs to — the basis for the per-model "how
 * long would a long reply take on your machine" estimate shown alongside
 * `formatTokensPerSec`'s tokens-per-second framing. */
export const LONG_ANSWER_TOKENS = 3000;

/** "about 4m 12s"-style projection for a `LONG_ANSWER_TOKENS`-token answer at
 * `tokensPerSec` — a duration lands more concretely than a raw rate. */
export function estimateAnswerDuration(tokensPerSec: number): string | null {
  if (!(tokensPerSec > 0)) return null;
  return formatDuration(LONG_ANSWER_TOKENS / tokensPerSec);
}

export function formatEta(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return i18n.t("format.eta.almostDone");
  if (seconds < 60) return i18n.t("format.eta.underMinute");
  if (seconds < 90) return i18n.t("format.eta.aboutMinute");
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return i18n.t("format.eta.minutesLeft", { count: minutes });
  const hours = Math.round(minutes / 60);
  return i18n.t("format.eta.hoursLeft", { count: hours });
}

export function timeAgo(ms: number): string {
  const diff = Date.now() - ms;
  const min = 60_000;
  if (diff < min) return i18n.t("format.timeAgo.justNow");
  if (diff < 60 * min) {
    return i18n.t("format.timeAgo.minAgo", { count: Math.round(diff / min) });
  }
  if (diff < 24 * 60 * min) {
    const h = Math.round(diff / (60 * min));
    return i18n.t("format.timeAgo.hourAgo", { count: h });
  }
  const days = Math.round(diff / (24 * 60 * min));
  if (days === 1) return i18n.t("format.timeAgo.yesterday");
  if (days < 7) return i18n.t("format.timeAgo.daysAgo", { count: days });
  return new Date(ms).toLocaleDateString(i18n.language === "fr" ? "fr-FR" : undefined, {
    month: "short",
    day: "numeric",
  });
}

export type RecencyBucket = "today" | "thisWeek" | "thisMonth" | "older";

/** Coarse, decreasing-scale recency bucket for grouping the sidebar's
 * conversation list — deliberately rolling windows (not calendar-aligned),
 * same "close enough" approach `timeAgo` already takes above, rather than
 * exact dates the user would have to mentally place on a calendar. */
export function recencyBucket(ms: number): RecencyBucket {
  const diff = Date.now() - ms;
  const day = 86_400_000;
  if (diff < day) return "today";
  if (diff < 7 * day) return "thisWeek";
  if (diff < 30 * day) return "thisMonth";
  return "older";
}

/** Label for a `recencyBucket()` result — same call-i18n.t()-directly shape
 * as `fitLabel` above, so grouping headers re-render on language change only
 * through their caller's own `useTranslation()`. */
export function recencyBucketLabel(bucket: RecencyBucket): string {
  return i18n.t(`format.recencyBucket.${bucket}`);
}

export function paramsLabel(paramsB: number): string {
  if (paramsB < 1) return `${Math.round(paramsB * 1000)}M`;
  return `${Math.round(paramsB)}B`;
}

/** `paramsLabel`, plus the active-per-token count for a MoE model with a
 * known one, e.g. "30B (3.3B active)" — a MoE model's total size overstates
 * how much it actually costs to run per token (see
 * `ModelSpec::active_fraction` in crates/core/src/types.rs), so the total
 * alone is misleading on its own. Falls back to plain `paramsLabel` for a
 * dense model, or a MoE entry without a known active count (today's
 * user-added models — see `ModelSpec.activeParamsB`). */
export function paramsWithActiveLabel(spec: {
  paramsB: number;
  moe: boolean;
  activeParamsB?: number;
}): string {
  if (spec.moe && spec.activeParamsB != null) {
    return i18n.t("format.paramsActive", {
      total: paramsLabel(spec.paramsB),
      active: paramsLabel(spec.activeParamsB),
    });
  }
  return paramsLabel(spec.paramsB);
}

/** "MoE" / "Dense" for the "All models" table's architecture column. Only
 * distinction the data model actually carries today (`ModelSpec.moe`) — not
 * a raw llama.cpp architecture string. */
export function architectureLabel(spec: { moe: boolean }): string {
  return i18n.t(spec.moe ? "format.architecture.moe" : "format.architecture.dense");
}

/** Grouping key for the "All models" table's sub-rows: `groupId` when set,
 * otherwise the model's own `id` (so an ungrouped model is simply a group of
 * one). Mirrors Rust's `ModelSpec::group_key` in crates/core/src/types.rs —
 * keep both in sync. */
export function groupKey(spec: { id: string; groupId?: string | null }): string {
  return spec.groupId ?? spec.id;
}

/** "Mar 2025" for a known `releaseDate` (day-precision or month-only, both
 * ISO-8601), "" when unknown. Never a promise of precision beyond what the
 * underlying data actually has. */
export function releaseDateLabel(releaseDate?: string | null): string {
  if (!releaseDate) return "";
  const date = new Date(releaseDate);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleDateString(i18n.language === "fr" ? "fr-FR" : "en-US", {
    year: "numeric",
    month: "short",
    timeZone: "UTC",
  });
}
