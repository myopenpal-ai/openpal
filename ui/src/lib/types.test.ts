import { describe, expect, it } from "vitest";
import { DEFAULT_SAMPLING, GENERAL_TASK_SAMPLING_DEFAULTS, recommendedSampling } from "./types";
import type { ModelSpec } from "./types";

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

describe("recommendedSampling", () => {
  it("falls back to the general default for an undefined spec", () => {
    expect(recommendedSampling(undefined, "coding", true)).toEqual(
      GENERAL_TASK_SAMPLING_DEFAULTS.coding,
    );
  });

  it("uses the model's taskDefaults override when there's no thinking table", () => {
    const codingOverride = { ...DEFAULT_SAMPLING, temperature: 0.1 };
    const m = spec({ id: "plain", taskDefaults: { coding: codingOverride } });
    expect(recommendedSampling(m, "coding", true)).toEqual(codingOverride);
    expect(recommendedSampling(m, "coding", false)).toEqual(codingOverride);
  });

  it("prefers the thinking-mode table over taskDefaults when thinking is on", () => {
    const thinkingCoding = { ...DEFAULT_SAMPLING, temperature: 0.6, presencePenalty: 0 };
    const instructCoding = { ...DEFAULT_SAMPLING, temperature: 0.7, presencePenalty: 1.5 };
    const m = spec({
      id: "hybrid",
      taskDefaults: { coding: instructCoding },
      taskDefaultsThinking: { coding: thinkingCoding },
    });
    expect(recommendedSampling(m, "coding", true)).toEqual(thinkingCoding);
  });

  it("falls back to taskDefaults when thinking is on but the task has no thinking entry", () => {
    const instructWriting = { ...DEFAULT_SAMPLING, temperature: 0.7 };
    const m = spec({
      id: "hybrid",
      taskDefaults: { writing: instructWriting },
      taskDefaultsThinking: { coding: { ...DEFAULT_SAMPLING, temperature: 0.6 } },
    });
    expect(recommendedSampling(m, "writing", true)).toEqual(instructWriting);
  });

  it("ignores the thinking table entirely when show thinking is off", () => {
    const thinkingCoding = { ...DEFAULT_SAMPLING, temperature: 0.6 };
    const m = spec({ id: "hybrid", taskDefaultsThinking: { coding: thinkingCoding } });
    expect(recommendedSampling(m, "coding", false)).toEqual(GENERAL_TASK_SAMPLING_DEFAULTS.coding);
  });
});
