/** Splits a document's extracted text into model-sized chunks so documents
 * longer than the loaded model's context window can still be translated or
 * corrected one chunk at a time.
 *
 * Splitting prefers the largest structural boundary that still fits:
 * paragraphs first, then sentences within an oversized paragraph, then a
 * hard character slice as a last resort for a single "sentence" (or any run
 * of punctuation-free text) that alone exceeds the limit. This keeps chunks
 * as semantically whole as possible, which matters for translation quality
 * at chunk edges.
 */

/** Runs of whitespace containing at least two newlines -- a paragraph break. */
const PARAGRAPH_BREAK = /\n\s*\n/;

/** A sentence-ending ".", "!" or "?" immediately followed by whitespace. */
const SENTENCE_BREAK = /(?<=[.!?])\s+/;

/**
 * Splits `text` into chunks of at most `maxChunkChars` characters, breaking
 * on paragraph boundaries where possible, falling back to sentence
 * boundaries within an oversized paragraph, and finally to a hard character
 * slice for a single sentence-less run that still doesn't fit.
 *
 * Returns `[]` for empty/whitespace-only input, and `[text]` unchanged
 * whenever it already fits -- the common case, since most writing-tools
 * input is far shorter than one chunk.
 */
export function chunkText(text: string, maxChunkChars = 4000): string[] {
  if (text.trim().length === 0) return [];
  if (text.length <= maxChunkChars) return [text];

  const paragraphs = text.split(PARAGRAPH_BREAK).filter((p) => p.length > 0);
  const chunks: string[] = [];

  for (const piece of packGreedily(paragraphs, maxChunkChars, "\n\n")) {
    if (piece.length <= maxChunkChars) {
      chunks.push(piece);
      continue;
    }
    // This single paragraph alone exceeds the limit -- fall back to
    // splitting it on sentence boundaries.
    const sentences = piece.split(SENTENCE_BREAK).filter((s) => s.length > 0);
    for (const sub of packGreedily(sentences, maxChunkChars, " ")) {
      if (sub.length <= maxChunkChars) {
        chunks.push(sub);
      } else {
        // Still too long -- no sentence-ending punctuation anywhere in this
        // run. Hard-slice it as a last resort.
        chunks.push(...hardSlice(sub, maxChunkChars));
      }
    }
  }

  return chunks;
}

/**
 * Greedily packs `pieces` into as few groups as possible, joining each
 * group's pieces with `separator`, without ever splitting a piece itself --
 * a single piece longer than `maxChars` is emitted alone (over length) for
 * the caller to split further.
 */
function packGreedily(pieces: string[], maxChars: number, separator: string): string[] {
  const groups: string[] = [];
  let current = "";

  for (const piece of pieces) {
    if (current.length === 0) {
      current = piece;
      continue;
    }
    if (current.length + separator.length + piece.length <= maxChars) {
      current += separator + piece;
    } else {
      groups.push(current);
      current = piece;
    }
  }
  if (current.length > 0) groups.push(current);

  return groups;
}

/** Hard-slices `text` into `maxChars`-character pieces, with no regard for
 * word or sentence boundaries -- the last resort for a run of text with no
 * punctuation to split on at all. Never produces an empty piece. */
function hardSlice(text: string, maxChars: number): string[] {
  // Guard against a non-positive step -- callers are expected to pass a
  // positive `maxChunkChars`, but this keeps a degenerate value from ever
  // looping forever instead of just producing over-sized chunks.
  const step = Math.max(1, maxChars);
  const pieces: string[] = [];
  for (let i = 0; i < text.length; i += step) {
    pieces.push(text.slice(i, i + step));
  }
  return pieces;
}
