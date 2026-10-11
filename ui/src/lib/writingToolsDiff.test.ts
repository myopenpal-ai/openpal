import { describe, expect, it } from "vitest";
import { computeWordDiff, defaultAcceptedMap, reconcileDiffText, type DiffPart } from "./writingToolsDiff";

function changeParts(parts: DiffPart[]): Extract<DiffPart, { type: "change" }>[] {
  return parts.filter((p): p is Extract<DiffPart, { type: "change" }> => p.type === "change");
}

describe("computeWordDiff — identical text", () => {
  it("produces no change parts for identical non-empty strings", () => {
    const parts = computeWordDiff("The cat sat.", "The cat sat.");
    expect(changeParts(parts)).toHaveLength(0);
  });

  it("produces an empty array for identical empty strings", () => {
    expect(computeWordDiff("", "")).toEqual([]);
  });

  it("reconciling identical text reproduces the original regardless of the accepted map", () => {
    const parts = computeWordDiff("The cat sat.", "The cat sat.");
    expect(reconcileDiffText(parts, {})).toBe("The cat sat.");
    expect(reconcileDiffText(parts, { "some-id": false })).toBe("The cat sat.");
  });
});

describe("computeWordDiff — single word substitution", () => {
  const original = "The cat sat.";
  const corrected = "The dog sat.";

  it("produces exactly one change part", () => {
    const parts = computeWordDiff(original, corrected);
    expect(changeParts(parts)).toHaveLength(1);
    const change = changeParts(parts)[0].change;
    expect(change.removed).toBe("cat");
    expect(change.added).toBe("dog");
  });

  it("reconciling with the change accepted gives the corrected text", () => {
    const parts = computeWordDiff(original, corrected);
    const id = changeParts(parts)[0].change.id;
    expect(reconcileDiffText(parts, { [id]: true })).toBe(corrected);
  });

  it("reconciling with the change rejected gives back the original text", () => {
    const parts = computeWordDiff(original, corrected);
    const id = changeParts(parts)[0].change.id;
    expect(reconcileDiffText(parts, { [id]: false })).toBe(original);
  });
});

describe("computeWordDiff — pure insertion", () => {
  it("produces a change with removed === '' when corrected has extra words", () => {
    const original = "The cat sat.";
    const corrected = "The big cat sat.";
    const parts = computeWordDiff(original, corrected);
    const changes = changeParts(parts);
    expect(changes).toHaveLength(1);
    expect(changes[0].change.removed).toBe("");
    expect(changes[0].change.added).toBe("big ");

    expect(reconcileDiffText(parts, defaultAcceptedMap(parts))).toBe(corrected);
    expect(reconcileDiffText(parts, { [changes[0].change.id]: false })).toBe(original);
  });
});

describe("computeWordDiff — pure deletion", () => {
  it("produces a change with added === '' when corrected is missing words", () => {
    const original = "The big cat sat.";
    const corrected = "The cat sat.";
    const parts = computeWordDiff(original, corrected);
    const changes = changeParts(parts);
    expect(changes).toHaveLength(1);
    expect(changes[0].change.added).toBe("");
    expect(changes[0].change.removed).toBe("big ");

    expect(reconcileDiffText(parts, defaultAcceptedMap(parts))).toBe(corrected);
    expect(reconcileDiffText(parts, { [changes[0].change.id]: false })).toBe(original);
  });
});

describe("computeWordDiff — multiple independent changes", () => {
  const original = "The cat sat on the mat.";
  const corrected = "The dog sat on the rug.";

  it("produces multiple distinct change ids", () => {
    const parts = computeWordDiff(original, corrected);
    const changes = changeParts(parts);
    expect(changes).toHaveLength(2);
    expect(changes[0].change.id).not.toBe(changes[1].change.id);
  });

  it("toggling only one change affects only that part of the reconciled output", () => {
    const parts = computeWordDiff(original, corrected);
    const changes = changeParts(parts);

    // Reject only the first change (cat/dog), leave the second (mat/rug) at
    // its default (accepted).
    const accepted = defaultAcceptedMap(parts);
    accepted[changes[0].change.id] = false;

    const result = reconcileDiffText(parts, accepted);
    expect(result).toBe("The cat sat on the rug.");
  });

  it("rejecting the other change instead only reverts that one", () => {
    const parts = computeWordDiff(original, corrected);
    const changes = changeParts(parts);

    const accepted = defaultAcceptedMap(parts);
    accepted[changes[1].change.id] = false;

    const result = reconcileDiffText(parts, accepted);
    expect(result).toBe("The dog sat on the mat.");
  });
});

describe("defaultAcceptedMap", () => {
  it("reproduces the corrected text exactly when passed straight to reconcileDiffText", () => {
    const original = "The cat sat on the mat, quietly.";
    const corrected = "The dog sat near the rug, loudly!";
    const parts = computeWordDiff(original, corrected);
    expect(reconcileDiffText(parts, defaultAcceptedMap(parts))).toBe(corrected);
  });

  it("sets every change id to true", () => {
    const parts = computeWordDiff("a b c", "x y z");
    const accepted = defaultAcceptedMap(parts);
    for (const part of changeParts(parts)) {
      expect(accepted[part.change.id]).toBe(true);
    }
  });

  it("returns an empty map when there are no changes", () => {
    const parts = computeWordDiff("same text", "same text");
    expect(defaultAcceptedMap(parts)).toEqual({});
  });
});
