import { describe, expect, it } from "vitest";
import {
  applyAllDiffMarks,
  applyDiffMark,
  computeWordDiff,
  diffMarks,
  type DiffPart,
} from "./writingToolsDiff";

function changeParts(parts: DiffPart[]): Extract<DiffPart, { type: "change" }>[] {
  return parts.filter((p): p is Extract<DiffPart, { type: "change" }> => p.type === "change");
}

/** The correctness invariant every test below leans on: concatenating each
 * part's original side reproduces the input verbatim. */
function originalOf(parts: DiffPart[]): string {
  return parts.map((p) => (p.type === "equal" ? p.text : p.change.removed)).join("");
}

function correctedOf(parts: DiffPart[]): string {
  return parts.map((p) => (p.type === "equal" ? p.text : p.change.added)).join("");
}

describe("computeWordDiff — identical text", () => {
  it("produces no change parts for identical non-empty strings", () => {
    expect(changeParts(computeWordDiff("The cat sat.", "The cat sat."))).toHaveLength(0);
  });

  it("produces an empty array for identical empty strings", () => {
    expect(computeWordDiff("", "")).toEqual([]);
    expect(diffMarks(computeWordDiff("", ""))).toEqual([]);
  });
});

describe("computeWordDiff — single word substitution", () => {
  const original = "The cat sat.";
  const corrected = "The dog sat.";

  it("produces exactly one change part that reproduces both sides", () => {
    const parts = computeWordDiff(original, corrected);
    const changes = changeParts(parts);
    expect(changes).toHaveLength(1);
    expect(changes[0].change.removed).toBe("cat");
    expect(changes[0].change.added).toBe("dog");
    expect(originalOf(parts)).toBe(original);
    expect(correctedOf(parts)).toBe(corrected);
  });
});

describe("computeWordDiff — pure insertion", () => {
  it("produces a change with removed === '' and a zero-width mark", () => {
    const parts = computeWordDiff("The cat sat.", "The big cat sat.");
    const changes = changeParts(parts);
    expect(changes).toHaveLength(1);
    expect(changes[0].change.removed).toBe("");
    expect(changes[0].change.added).toBe("big ");

    const [mark] = diffMarks(parts);
    expect(mark.start).toBe(mark.end);
    expect(applyDiffMark("The cat sat.", [mark], mark.id).text).toBe("The big cat sat.");
  });
});

describe("computeWordDiff — pure deletion", () => {
  it("produces a change with added === '' that removing the mark applies", () => {
    const parts = computeWordDiff("The big cat sat.", "The cat sat.");
    const changes = changeParts(parts);
    expect(changes).toHaveLength(1);
    expect(changes[0].change.added).toBe("");
    expect(changes[0].change.removed).toBe("big ");
    expect(applyAllDiffMarks("The big cat sat.", diffMarks(parts))).toBe("The cat sat.");
  });
});

describe("computeWordDiff — adjacent corrections separated only by whitespace", () => {
  it("groups two corrections into a single suggestion", () => {
    const parts = computeWordDiff("Thanks, you'r nise", "Thanks, you're nice");
    const changes = changeParts(parts);
    expect(changes).toHaveLength(1);
    expect(changes[0].change.removed).toBe("r nise");
    expect(changes[0].change.added).toBe("re nice");

    const [mark] = diffMarks(parts);
    expect("Thanks, you'r nise".slice(mark.start, mark.end)).toBe("r nise");
    expect(applyAllDiffMarks("Thanks, you'r nise", [mark])).toBe("Thanks, you're nice");
  });

  it("groups a run of three corrections across whitespace into one suggestion", () => {
    const parts = computeWordDiff("a b c", "x y z");
    const changes = changeParts(parts);
    expect(changes).toHaveLength(1);
    expect(changes[0].change.removed).toBe("a b c");
    expect(changes[0].change.added).toBe("x y z");
    expect(applyAllDiffMarks("a b c", diffMarks(parts))).toBe("x y z");
  });

  it("keeps corrections apart when real words sit between them", () => {
    const parts = computeWordDiff("The cat sat on the mat.", "The dog sat on the rug.");
    const changes = changeParts(parts);
    expect(changes).toHaveLength(2);
    expect(changes[0].change.removed).toBe("cat");
    expect(changes[1].change.removed).toBe("mat");
  });
});

describe("diffMarks", () => {
  it("locates every change at an offset into the original text", () => {
    const original = "The cat sat on the mat.";
    const marks = diffMarks(computeWordDiff(original, "The dog sat on the rug."));
    expect(marks.map((m) => original.slice(m.start, m.end))).toEqual(["cat", "mat"]);
  });
});

describe("applyDiffMark", () => {
  const original = "The cat sat on the mat.";

  it("writes the suggestion in, drops its mark, and shifts the ones after it", () => {
    const marks = diffMarks(computeWordDiff(original, "The dog sat on the rug."));
    const first = marks[0];
    const applied = applyDiffMark(original, marks, first.id);

    expect(applied.text).toBe("The dog sat on the mat.");
    expect(applied.marks).toHaveLength(1);
    const [second] = applied.marks;
    expect(applied.text.slice(second.start, second.end)).toBe("mat");

    const finished = applyDiffMark(applied.text, applied.marks, second.id);
    expect(finished.text).toBe("The dog sat on the rug.");
    expect(finished.marks).toEqual([]);
  });

  it("is a no-op for an unknown id", () => {
    const marks = diffMarks(computeWordDiff(original, "The dog sat on the mat."));
    expect(applyDiffMark(original, marks, "nope")).toEqual({ text: original, marks });
  });
});

describe("applyAllDiffMarks", () => {
  it("applies every suggestion regardless of order", () => {
    const original = "The cat sat on the mat, quietly.";
    const corrected = "The dog sat near the rug, loudly!";
    const marks = diffMarks(computeWordDiff(original, corrected)).reverse();
    expect(applyAllDiffMarks(original, marks)).toBe(corrected);
  });

  it("returns the text unchanged when there are no marks", () => {
    expect(applyAllDiffMarks("same text", [])).toBe("same text");
  });
});
