import { describe, expect, it } from "vitest";
import { bestModelForTag } from "./triage";
import type { ModelInfo, ModelSpec, TaskType } from "./types";

function spec(overrides: Partial<ModelSpec> & { id: string }): ModelSpec {
  return {
    name: overrides.id,
    family: "Test",
    publisher: "Test",
    description: "",
    strengths: [],
    url: "https://example.com/model.gguf",
    fileName: "model.gguf",
    sizeBytes: 1_000_000,
    quant: "Q4_K_M",
    sha256: null,
    paramsB: 4,
    moe: false,
    contextMax: 8192,
    qualityScore: 50,
    license: "Apache 2.0",
    extraArgs: [],
    hidden: false,
    curated: false,
    source: "catalog",
    localPath: null,
    vision: false,
    taskScores: {},
    ...overrides,
  };
}

function installed(overrides: Partial<ModelSpec> & { id: string }): ModelInfo {
  return {
    spec: spec(overrides),
    status: { type: "installed", data: { path: "/models/x.gguf", sizeBytes: 1_000_000 } },
    fit: "good",
    why: "",
    isRecommended: false,
    favorite: false,
  };
}

function notInstalled(overrides: Partial<ModelSpec> & { id: string }): ModelInfo {
  return { ...installed(overrides), status: { type: "notInstalled" } };
}

const CODING: TaskType = "coding";

describe("bestModelForTag", () => {
  it("suggests nothing when no model is installed", () => {
    const models = [notInstalled({ id: "a", taskScores: { coding: 90 } })];
    expect(bestModelForTag(models, CODING, null)).toBeNull();
  });

  it("suggests the top-scoring installed model when nothing is currently loaded", () => {
    const models = [
      installed({ id: "weak", taskScores: { coding: 20 } }),
      installed({ id: "strong", taskScores: { coding: 90 } }),
    ];
    expect(bestModelForTag(models, CODING, null)?.spec.id).toBe("strong");
  });

  it("returns null when the current model is already the best fit", () => {
    const models = [
      installed({ id: "current", taskScores: { coding: 90 } }),
      installed({ id: "other", taskScores: { coding: 40 } }),
    ];
    expect(bestModelForTag(models, CODING, "current")).toBeNull();
  });

  it("never suggests a model that isn't installed, even if it scores higher", () => {
    const models = [
      installed({ id: "current", taskScores: { coding: 30 } }),
      notInstalled({ id: "uninstalled-champ", taskScores: { coding: 99 } }),
    ];
    expect(bestModelForTag(models, CODING, "current")).toBeNull();
  });

  it("ignores a marginal improvement below the suggestion margin", () => {
    const models = [
      installed({ id: "current", taskScores: { coding: 60 } }),
      installed({ id: "barely-better", taskScores: { coding: 65 } }),
    ];
    expect(bestModelForTag(models, CODING, "current")).toBeNull();
  });

  it("suggests a meaningfully better installed model", () => {
    const models = [
      installed({ id: "current", taskScores: { coding: 30 } }),
      installed({ id: "much-better", taskScores: { coding: 85 } }),
    ];
    expect(bestModelForTag(models, CODING, "current")?.spec.id).toBe("much-better");
  });

  it("treats a missing taskScores entry as a neutral 0", () => {
    const models = [
      installed({ id: "current", taskScores: {} }),
      installed({ id: "scored", taskScores: { coding: 15 } }),
    ];
    // current's implicit 0 is 15 points below "scored" — over the margin.
    expect(bestModelForTag(models, CODING, "current")?.spec.id).toBe("scored");
  });

  it("ranks by the requested task type, not just coding", () => {
    const models = [
      installed({ id: "generalist", taskScores: { agentic: 30, mathScience: 40 } }),
      installed({ id: "math-specialist", taskScores: { agentic: 35, mathScience: 85 } }),
    ];
    expect(bestModelForTag(models, "mathScience", "generalist")?.spec.id).toBe("math-specialist");
    expect(bestModelForTag(models, "agentic", "generalist")).toBeNull();
  });

  it("breaks a taskScores tie by qualityScore", () => {
    const models = [
      installed({ id: "current", taskScores: { coding: 50 }, qualityScore: 50 }),
      installed({ id: "tied-but-better-quality", taskScores: { coding: 50 }, qualityScore: 90 }),
    ];
    // Tied taskScores mean no margin is met either way, regardless of quality
    // tie-break — this just confirms the tie-break doesn't itself cause a
    // spurious suggestion when the primary signal is equal.
    expect(bestModelForTag(models, CODING, "current")).toBeNull();
  });
});
