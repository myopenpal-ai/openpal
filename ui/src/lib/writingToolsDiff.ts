import { diffWordsWithSpace } from "diff";

/**
 * Word-level diff for Correct mode's in-place correction marks, as pure
 * functions — no UI, no API calls.
 *
 * `computeWordDiff` turns the raw `diff` package output into a sequence of
 * unchanged text runs and grouped changes; `diffMarks` locates those changes
 * in the original text as `[start, end)` ranges the editor can render as
 * clickable marks; `applyDiffMark`/`applyAllDiffMarks` write a suggestion into
 * the text destructively — the pre-correction wording is gone afterwards, the
 * model the reporter asked for (no tracked-changes state, no undo).
 *
 * Two corrections separated only by whitespace are deliberately **one**
 * change: `Thanks, you'r nise` → `Thanks, you're nice` is a single suggestion
 * ("you're nice"), not two ("you're" and "nice"), so one Keep click fixes the
 * phrase. A run of unchanged words between two corrections keeps them apart.
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
 * the `removed` side of every `change` reproduces the original exactly;
 * doing the same with the `added` side reproduces the correction. */
export type DiffPart = { type: "equal"; text: string } | { type: "change"; change: DiffChange };

/** A `DiffChange` located in the original text: replacing
 * `text.slice(start, end)` with `added` applies it. Offsets are JS string
 * (UTF-16 code unit) indices, the same unit a textarea/`slice` uses. */
export interface DiffMark {
  id: string;
  start: number;
  end: number;
  removed: string;
  added: string;
}

/**
 * Word-level diff between `original` and `corrected`, grouped into
 * `DiffPart`s. Built on `diffWordsWithSpace`, which keeps whitespace as its
 * own token so the `equal`/`removed`/`added` text already carries every
 * space/newline it needs verbatim — callers should never insert extra
 * separators between parts.
 *
 * Grouping rules:
 * - a `removed` part immediately adjacent to an `added` part (in either
 *   order) is one replacement;
 * - two changes separated by an `equal` part that is *only* whitespace are
 *   merged into one change, whitespace included on both sides — see the
 *   module doc;
 * - a `removed` part with no adjacent `added` part is a pure deletion; an
 *   `added` part with no adjacent `removed` part is a pure insertion.
 *
 * Identical input produces no `change` parts at all (a single `equal` part,
 * or an empty array for empty input).
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

  return mergeWhitespaceSeparatedChanges(parts);
}

/** A run of text that is only whitespace (no letters, digits, punctuation).
 * Newlines count: the rule is about adjacency in the text, and a corrected
 * phrase that wraps across a line break is still one phrase. */
function isWhitespaceOnly(text: string): boolean {
  return text.length > 0 && text.trim() === "";
}

/**
 * Fold `change`, equal-whitespace-only, `change` sequences into one change
 * that carries the whitespace on both sides. Runs until no such sequence is
 * left, so three corrections in a row become one suggestion. Ids are kept
 * from the first change of each merged group, and are unique across the
 * result.
 */
function mergeWhitespaceSeparatedChanges(parts: DiffPart[]): DiffPart[] {
  const merged: DiffPart[] = [];
  for (const part of parts) {
    const previous = merged.length >= 2 ? merged[merged.length - 2] : undefined;
    const gap = merged.length >= 1 ? merged[merged.length - 1] : undefined;
    if (
      part.type === "change" &&
      gap?.type === "equal" &&
      isWhitespaceOnly(gap.text) &&
      previous?.type === "change"
    ) {
      const joined = previous.change;
      merged.pop(); // the whitespace gap
      merged.pop(); // the previous change
      merged.push({
        type: "change",
        change: {
          id: joined.id,
          removed: `${joined.removed}${gap.text}${part.change.removed}`,
          added: `${joined.added}${gap.text}${part.change.added}`,
        },
      });
      continue;
    }
    merged.push(part);
  }
  return merged;
}

/**
 * Locate every change in `parts` as a `[start, end)` range of the original
 * text, in original order. A pure insertion (`removed === ""`) still gets a
 * zero-width range, so the UI can anchor a mark for it instead of silently
 * dropping the correction.
 */
export function diffMarks(parts: DiffPart[]): DiffMark[] {
  const marks: DiffMark[] = [];
  let offset = 0;
  for (const part of parts) {
    if (part.type === "equal") {
      offset += part.text.length;
      continue;
    }
    const start = offset;
    const end = start + part.change.removed.length;
    marks.push({
      id: part.change.id,
      start,
      end,
      removed: part.change.removed,
      added: part.change.added,
    });
    offset = end;
  }
  return marks;
}

/**
 * Apply one suggestion: replace the mark's span in `text` with its `added`
 * text and drop the mark, shifting every later mark by the length change so
 * the remaining marks still point at the right words. The removed wording is
 * gone from the returned text — there is no undo. Unknown ids are a no-op.
 */
export function applyDiffMark(
  text: string,
  marks: DiffMark[],
  id: string,
): { text: string; marks: DiffMark[] } {
  const mark = marks.find((m) => m.id === id);
  if (!mark) return { text, marks };
  const delta = mark.added.length - (mark.end - mark.start);
  return {
    text: `${text.slice(0, mark.start)}${mark.added}${text.slice(mark.end)}`,
    marks: marks
      .filter((m) => m.id !== id)
      .map((m) =>
        m.start >= mark.end ? { ...m, start: m.start + delta, end: m.end + delta } : m,
      ),
  };
}

/**
 * Apply every suggestion at once — "Keep all". Applies right to left so each
 * earlier mark's offsets are still valid, then returns the corrected text
 * (identical to `reconcile`-style output: every `added` side, every `equal`
 * run verbatim).
 */
export function applyAllDiffMarks(text: string, marks: DiffMark[]): string {
  let result = text;
  for (const mark of [...marks].sort((a, b) => b.start - a.start)) {
    result = `${result.slice(0, mark.start)}${mark.added}${result.slice(mark.end)}`;
  }
  return result;
}
