/** Pure helper for the request-triage feature's model auto-pick
 * (`useTriageAutopilot`, `ChatView.tsx`) — ranks already-installed models for
 * a task type. Only ever recommends models the user has already downloaded;
 * never suggests downloading something new. Consulted only when the opt-in
 * `smart-model-router` plugin is enabled (`smartModelRouterAvailable`, `store.ts`). */

import type { ModelInfo, TaskType } from "./types";

/** Minimum `taskScores` point margin an installed model must beat the
 * currently-loaded one by before it's worth switching — avoids thrashing the
 * engine over marginal, noise-level differences. */
const SUGGESTION_MARGIN = 10;

/** The best already-installed model for `tag`, if one is a meaningfully
 * better fit than the model currently loaded (or nothing is loaded yet).
 * Ranks by `taskScores[tag]` (missing = neutral 0), tie-broken by
 * `qualityScore`. Returns `null` when nothing installed is worth switching
 * to — this never suggests downloading something new. */
export function bestModelForTag(
  models: ModelInfo[],
  tag: TaskType,
  currentModelId: string | null,
): ModelInfo | null {
  const installed = models.filter((m) => m.status.type === "installed");
  if (installed.length === 0) return null;

  const scoreFor = (m: ModelInfo) => m.spec.taskScores[tag] ?? 0;
  const ranked = [...installed].sort(
    (a, b) => scoreFor(b) - scoreFor(a) || b.spec.qualityScore - a.spec.qualityScore,
  );
  const best = ranked[0];
  if (best.spec.id === currentModelId) return null;

  const current = installed.find((m) => m.spec.id === currentModelId);
  const currentScore = current ? scoreFor(current) : -Infinity;
  if (scoreFor(best) < currentScore + SUGGESTION_MARGIN) return null;

  return best;
}
