/** Filtering, grouping and ordering for the Models page's "All models" list
 * (`views/Library.tsx`). Kept out of the view as pure functions, because the
 * ordering rules are fiddly and easy to regress silently — the list renders
 * fine whether or not it's in a sensible order.
 *
 * The pipeline is deliberately **filter -> group -> sort groups**, never
 * sort-then-group: `groupModels` preserves first-seen order inside each
 * group, so `group[0]` (whose name and author the parent row shows) stays
 * the same model no matter which sort is active. Sorting first would let the
 * parent row's label flip as you change the sort. */

import { groupKey, totalModelSizeBytes } from "./format";
import { estimateTokensPerSec } from "./speedEstimate";
import type { FitLevel, HardwareInfo, ModelInfo } from "./types";

export type ModelFilter = "all" | "installed" | "compatible";
export type ModelSort = "recommended" | "name" | "size" | "speed";

export const MODEL_FILTERS: readonly ModelFilter[] = ["all", "installed", "compatible"];
export const MODEL_SORTS: readonly ModelSort[] = ["recommended", "name", "size", "speed"];

/** What ordering needs beyond the models themselves: a speed estimate depends
 * on this Mac, and "currently loaded" lives on the engine, not on `ModelInfo`. */
export interface SortContext {
  hardware: HardwareInfo | null;
  runningModelId: string | null;
}

/** Best-first, so a group can be ranked by its best-fitting member. Mirrors
 * the ordering of Rust's `FitLevel` enum (crates/core/src/types.rs), which
 * runs TooBig < DiskSlow < Tight < Good < Perfect. */
const FIT_RANK: Record<FitLevel, number> = {
  perfect: 0,
  good: 1,
  tight: 2,
  diskSlow: 3,
  tooBig: 4,
};

/** Case- and accent-insensitive, because the app ships in French: typing
 * "modele" has to match "Modèle". */
function fold(value: string): string {
  return value
    .normalize("NFD")
    .replace(/\p{Diacritic}/gu, "")
    .toLowerCase();
}

/** A model's estimated generation speed on this Mac, or 0 when hardware
 * hasn't loaded yet. Exported so the row's speed indicator and the speed sort
 * read from the same source and can't disagree. `estimateTokensPerSec`
 * already returns 0 for `diskSlow`/`tooBig`. */
export function modelSpeed(model: ModelInfo, hardware: HardwareInfo | null): number {
  return hardware ? estimateTokensPerSec(model.spec, hardware, model.fit) : 0;
}

function matchesQuery(model: ModelInfo, folded: string): boolean {
  const { spec } = model;
  return [spec.name, spec.publisher, spec.quant, spec.family].some((field) =>
    fold(field ?? "").includes(folded),
  );
}

function matchesFilter(model: ModelInfo, filter: ModelFilter): boolean {
  switch (filter) {
    case "installed":
      // Strictly on-disk-and-usable. A partial download deliberately does not
      // count: it isn't installed, it's a resumable download, and saying
      // otherwise would make "Installed" mean two different things.
      return model.status.type === "installed";
    case "compatible":
      // "Runs properly on this Mac" — `diskSlow` is excluded alongside
      // `tooBig` because a disk-streamed model is technically runnable but
      // 30-140x slower (docs/design/DISK_OFFLOAD.md), which is not what someone
      // filtering for compatible models is asking for.
      return model.fit !== "tooBig" && model.fit !== "diskSlow";
    case "all":
      return true;
  }
}

export function filterModels(
  models: ModelInfo[],
  query: string,
  filter: ModelFilter,
): ModelInfo[] {
  const folded = fold(query.trim());
  return models.filter(
    (m) => matchesFilter(m, filter) && (folded === "" || matchesQuery(m, folded)),
  );
}

/** Buckets models by `groupKey` — several quantizations of the same Hugging
 * Face repo — preserving each group's first-seen order. Today only user-added
 * HF models ever group: `hf_spec` sets `group_id` to the repo id, while no
 * catalog entry sets one, so every catalog model is its own group of one. */
export function groupModels(models: ModelInfo[]): ModelInfo[][] {
  const order: string[] = [];
  const byKey = new Map<string, ModelInfo[]>();
  for (const m of models) {
    const key = groupKey(m.spec);
    const group = byKey.get(key);
    if (group) {
      group.push(m);
    } else {
      byKey.set(key, [m]);
      order.push(key);
    }
  }
  return order.map((key) => byKey.get(key)!);
}

/** The comparable shape of a whole group: each field is the best value across
 * its members, so a group sorts as well as its best quantization does. */
interface GroupRank {
  favorite: boolean;
  inUse: boolean;
  onDisk: boolean;
  fit: number;
  quality: number;
  size: number;
  speed: number;
  name: string;
}

function rankGroup(group: ModelInfo[], ctx: SortContext): GroupRank {
  return {
    favorite: group.some((m) => m.favorite),
    inUse: group.some((m) => m.spec.id === ctx.runningModelId),
    onDisk: group.some((m) => m.status.type !== "notInstalled"),
    fit: Math.min(...group.map((m) => FIT_RANK[m.fit])),
    quality: Math.max(...group.map((m) => m.spec.qualityScore)),
    // A group's smallest option: what matters when you're picking by how much
    // disk or memory you can spare.
    size: Math.min(...group.map((m) => totalModelSizeBytes(m.spec))),
    speed: Math.max(...group.map((m) => modelSpeed(m, ctx.hardware))),
    // group[0] is stable (see the module comment), so this label never flips.
    name: group[0].spec.name,
  };
}

/** `false` before `true`, i.e. flag-set-first once negated. */
const flag = (a: boolean, b: boolean) => Number(b) - Number(a);

/** Orders whole groups, never members within one — a group's quantizations
 * always stay together, under their parent row.
 *
 * `Array.prototype.sort` is stable, so groups that tie keep the order
 * `groupModels` gave them, which is the order the backend returned. */
export function sortGroups(
  groups: ModelInfo[][],
  sort: ModelSort,
  ctx: SortContext,
): ModelInfo[][] {
  const ranked = groups.map((group) => ({ group, rank: rankGroup(group, ctx) }));

  ranked.sort((a, b) => {
    const x = a.rank;
    const y = b.rank;
    switch (sort) {
      case "name":
        return x.name.localeCompare(y.name);
      case "size":
        // Ascending: someone sorting by size is usually looking for something
        // that fits, not for the biggest thing they can install.
        return x.size - y.size || x.name.localeCompare(y.name);
      case "speed":
        // `modelSpeed` returns 0 for every `diskSlow` and `tooBig` model, so
        // without the fit tiebreak the entire unusable tail would tie and
        // land in arbitrary order.
        return y.speed - x.speed || x.fit - y.fit || x.name.localeCompare(y.name);
      case "recommended":
        return (
          flag(x.favorite, y.favorite) ||
          flag(x.inUse, y.inUse) ||
          flag(x.onDisk, y.onDisk) ||
          x.fit - y.fit ||
          y.quality - x.quality ||
          x.name.localeCompare(y.name)
        );
    }
  });

  return ranked.map((r) => r.group);
}

/** filter -> group -> sort, in the one order that keeps parent rows stable. */
export function buildModelList(
  models: ModelInfo[],
  query: string,
  filter: ModelFilter,
  sort: ModelSort,
  ctx: SortContext,
): ModelInfo[][] {
  return sortGroups(groupModels(filterModels(models, query, filter)), sort, ctx);
}
