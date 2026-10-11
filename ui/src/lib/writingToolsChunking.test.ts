import { describe, expect, it } from "vitest";
import { chunkText } from "./writingToolsChunking";

/** Non-whitespace content, in order -- used to assert no word is lost or
 * duplicated across a multi-chunk split regardless of how whitespace at
 * chunk boundaries gets normalized. */
function stripWhitespace(text: string): string {
  return text.replace(/\s+/g, "");
}

describe("chunkText", () => {
  it("returns an empty array for empty input", () => {
    expect(chunkText("")).toEqual([]);
  });

  it("returns an empty array for whitespace-only input", () => {
    expect(chunkText("   \n\n\t  ")).toEqual([]);
  });

  it("returns short text as a single chunk, unchanged", () => {
    const text = "Hello, world. This is a short document.";
    expect(chunkText(text, 4000)).toEqual([text]);
  });

  it("returns text exactly at the limit as a single chunk, unchanged", () => {
    const text = "a".repeat(100);
    expect(chunkText(text, 100)).toEqual([text]);
  });

  it("splits multi-paragraph text longer than the limit into multiple chunks", () => {
    const paragraphs = Array.from({ length: 10 }, (_, i) => `Paragraph ${i}. `.repeat(20));
    const text = paragraphs.join("\n\n");
    const maxChunkChars = 500;
    const chunks = chunkText(text, maxChunkChars);

    expect(chunks.length).toBeGreaterThan(1);
    for (const chunk of chunks) {
      expect(chunk.length).toBeLessThanOrEqual(maxChunkChars);
      expect(chunk.length).toBeGreaterThan(0);
    }
    // No paragraph's content is lost or duplicated.
    expect(stripWhitespace(chunks.join(""))).toBe(stripWhitespace(text));
  });

  it("packs consecutive short paragraphs greedily instead of one-per-chunk", () => {
    const text = ["Short one.", "Short two.", "Short three."].join("\n\n");
    // All three paragraphs comfortably fit in one chunk together.
    expect(chunkText(text, 4000)).toEqual([text]);
  });

  it("falls back to sentence splitting for a single oversized paragraph", () => {
    // One paragraph (no blank lines at all), but many sentences.
    const sentence = "This is one sentence in a very long paragraph. ";
    const text = sentence.repeat(50).trim();
    const maxChunkChars = 300;

    expect(text.includes("\n\n")).toBe(false);
    const chunks = chunkText(text, maxChunkChars);

    expect(chunks.length).toBeGreaterThan(1);
    for (const chunk of chunks) {
      expect(chunk.length).toBeLessThanOrEqual(maxChunkChars);
      expect(chunk.length).toBeGreaterThan(0);
    }
    expect(stripWhitespace(chunks.join(""))).toBe(stripWhitespace(text));
  });

  it("hard-slices a punctuation-free wall of text with no chunk exceeding the limit", () => {
    // No paragraph breaks, no sentence-ending punctuation anywhere.
    const text = "word ".repeat(2000).trim();
    const maxChunkChars = 250;

    expect(/[.!?]/.test(text)).toBe(false);
    const chunks = chunkText(text, maxChunkChars);

    expect(chunks.length).toBeGreaterThan(1);
    for (const chunk of chunks) {
      expect(chunk.length).toBeLessThanOrEqual(maxChunkChars);
      expect(chunk.length).toBeGreaterThan(0);
    }
    // Hard slicing can cut mid-word, so compare raw concatenation instead of
    // a word-by-word sequence -- no character may be lost or duplicated.
    expect(chunks.join("")).toBe(text);
  });

  it("hard-slices a single word longer than the limit into exact-size pieces", () => {
    const text = "x".repeat(1000);
    const maxChunkChars = 300;
    const chunks = chunkText(text, maxChunkChars);

    expect(chunks).toEqual(["x".repeat(300), "x".repeat(300), "x".repeat(300), "x".repeat(100)]);
    for (const chunk of chunks) {
      expect(chunk.length).toBeLessThanOrEqual(maxChunkChars);
      expect(chunk.length).toBeGreaterThan(0);
    }
  });

  it("never produces an empty chunk across paragraph, sentence, and hard-slice paths", () => {
    const text = [
      "Para one, short.",
      "B".repeat(5000), // forces sentence fallback (no punctuation at all within it)
      "Para three. Has two sentences.",
    ].join("\n\n");
    const chunks = chunkText(text, 200);

    expect(chunks.length).toBeGreaterThan(1);
    for (const chunk of chunks) {
      expect(chunk.length).toBeGreaterThan(0);
      expect(chunk.length).toBeLessThanOrEqual(200);
    }
    expect(stripWhitespace(chunks.join(""))).toBe(stripWhitespace(text));
  });

  it("preserves every word across a multi-chunk split (word-by-word)", () => {
    const paragraphs = Array.from(
      { length: 8 },
      (_, i) => `This is paragraph number ${i} with several words in it.`,
    );
    const text = paragraphs.join("\n\n");
    const chunks = chunkText(text, 120);

    expect(chunks.length).toBeGreaterThan(1);
    const originalWords = text.split(/\s+/).filter(Boolean);
    const chunkWords = chunks.join(" ").split(/\s+/).filter(Boolean);
    expect(chunkWords).toEqual(originalWords);
  });

  it("uses the default maxChunkChars of 4000 when not specified", () => {
    const text = "y".repeat(3999);
    expect(chunkText(text)).toEqual([text]);

    const longer = "para one ".repeat(500) + "\n\n" + "para two ".repeat(500);
    const chunks = chunkText(longer);
    for (const chunk of chunks) {
      expect(chunk.length).toBeLessThanOrEqual(4000);
    }
  });
});
