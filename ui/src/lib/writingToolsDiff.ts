import { diffWordsWithSpace } from "diff";

/**
 * Word-level diff + reconciliation for the Correct-mode tracked-changes view.
 * Pure functions only — no UI, no API calls. `computeWordDiff` turns the raw
 * `diff` package output into a sequence of unchanged text runs and grouped
 * changes the UI can render as strikethrough/underline spans; each change
 * carries a stable id so a consumer can track a per-change accept/reject
 * state (`reconcileDiffText` applies that state back into plain text).
 */

/** One correction: a word (or run of words) replaced, inserted, or deleted
 * between the original and corrected text. Exactly one of `removed`/`added`
 * is empty for a pure insertion/deletion; both are non-empty for a
 * replacement (a removed run immediately adjacent to an added run). */
export interface DiffChange {
  id: string;
  /** Text present in the original but not the correction. "" for a pure insertion. */
  removed: string;
  /** Text present in the correction but not the original. "" for a pure deletion. */
  added: string;
}

/** One segment of the diffed text, in original order: either untouched text
 * or a single grouped change. Concatenating every `equal` part's `text` with
 * either side of every `change` (per an accept/reject decision) reconstructs
 * a full document — see `reconcileDiffText`. */
export type DiffPart = { type: "equal"; text: string } | { type: "change"; change: DiffChange };

/**
 * Word-level diff between `original` and `corrected`, grouped into
 * `DiffPart`s for a tracked-changes UI. Built on `diffWordsWithSpace`, which
 * keeps whitespace as its own token so the `equal`/`removed`/`added` text
 * already carries every space/newline it needs verbatim — callers should
 * never insert extra separators between parts.
 *
 * A `removed` part immediately adjacent to an `added` part (in either order)
 * is grouped into a single replacement `DiffChange`; a `removed` part with no
 * adjacent `added` part is a pure deletion; an `added` part with no adjacent
 * `removed` part is a pure insertion. Identical input produces no `change`
 * parts at all (a single `equal` part, or an empty array for empty input).
 */
export function computeWordDiff(original: string, corrected: string): DiffPart[] {
  const rawParts = diffWordsWithSpace(original, corrected);

  const parts: DiffPart[] = [];
  let nextChangeId = 0;
  let i = 0;
  while (i < rawParts.length) {
    const part = rawParts[i];

    if (!part.added && !part.removed) {
      parts.push({ type: "equal", text: part.value });
      i++;
      continue;
    }

    // Group this removed/added part with an immediately adjacent
    // opposite-direction part (either order) into one replacement change.
    let removed = "";
    let added = "";
    if (part.removed) removed += part.value;
    if (part.added) added += part.value;

    const next = rawParts[i + 1];
    let consumed = 1;
    if (next && ((part.removed && next.added) || (part.added && next.removed))) {
      if (next.removed) removed += next.value;
      if (next.added) added += next.value;
      consumed = 2;
    }

    parts.push({
      type: "change",
      change: { id: `change-${nextChangeId++}`, removed, added },
    });
    i += consumed;
  }

  return parts;
}

/**
 * Applies an accept/reject decision to each change in `parts` and returns the
 * resulting plain text. For each `change` part, `accepted[change.id]` decides
 * which side wins — `change.added` when `true` (or when the id is missing
 * from the map, since corrections are accepted by default: that's the whole
 * point of Correct mode), `change.removed` otherwise. `equal` parts pass
 * through unchanged. Parts are joined with no extra separators — the parts
 * themselves already carry all needed whitespace as literal text.
 */
export function reconcileDiffText(parts: DiffPart[], accepted: Record<string, boolean>): string {
  let result = "";
  for (const part of parts) {
    if (part.type === "equal") {
      result += part.text;
    } else {
      const isAccepted = accepted[part.change.id] ?? true;
      result += isAccepted ? part.change.added : part.change.removed;
    }
  }
  return result;
}

/**
 * Seeds an accept-state map with every change in `parts` accepted — the
 * default state a tracked-changes UI should start from (corrections are
 * accepted by default; the user rejects the ones they disagree with).
 * Passing this straight into `reconcileDiffText` reproduces `corrected`.
 */
export function defaultAcceptedMap(parts: DiffPart[]): Record<string, boolean> {
  const accepted: Record<string, boolean> = {};
  for (const part of parts) {
    if (part.type === "change") accepted[part.change.id] = true;
  }
  return accepted;
}
