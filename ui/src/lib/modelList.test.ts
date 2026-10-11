import { describe, expect, it } from "vitest";
import {
  buildModelList,
  filterModels,
  groupModels,
  sortGroups,
  type SortContext,
} from "./modelList";
import type { FitLevel, HardwareInfo, ModelInfo, ModelSpec } from "./types";

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

function model(
  overrides: Partial<ModelSpec> & { id: string },
  state: { fit?: FitLevel; favorite?: boolean; status?: ModelInfo["status"] } = {},
): ModelInfo {
  return {
    spec: spec(overrides),
    status: state.status ?? { type: "notInstalled" },
    fit: state.fit ?? "good",
    why: "",
    isRecommended: false,
    favorite: state.favorite ?? false,
  };
}

const INSTALLED: ModelInfo["status"] = {
  type: "installed",
  data: { path: "/models/x.gguf", sizeBytes: 1_000_000 },
};
const PARTIAL: ModelInfo["status"] = {
  type: "partiallyDownloaded",
  data: { receivedBytes: 500_000 },
};

const HARDWARE: HardwareInfo = {
  chip: "Apple M2",
  isAppleSilicon: true,
  totalRamBytes: 16 * 1024 ** 3,
  availableRamBytes: 12 * 1024 ** 3,
  cpuCores: 8,
  hasMetal: true,
  freeDiskBytes: 200 * 1024 ** 3,
  osVersion: "14.0",
};

const ctx = (overrides: Partial<SortContext> = {}): SortContext => ({
  hardware: null,
  runningModelId: null,
  ...overrides,
});

const ids = (groups: ModelInfo[][]) => groups.map((g) => g.map((m) => m.spec.id));

describe("filterModels", () => {
  it("matches a query against name, publisher, quant and family", () => {
    const models = [
      model({ id: "a", name: "Gemma 4 E4B", publisher: "Google" }),
      model({ id: "b", name: "GPT-OSS 20B", publisher: "OpenAI", quant: "MXFP4" }),
      model({ id: "c", name: "Qwen3 8B", publisher: "Alibaba", family: "Qwen" }),
    ];
    expect(filterModels(models, "gemma", "all").map((m) => m.spec.id)).toEqual(["a"]);
    expect(filterModels(models, "openai", "all").map((m) => m.spec.id)).toEqual(["b"]);
    expect(filterModels(models, "mxfp4", "all").map((m) => m.spec.id)).toEqual(["b"]);
    expect(filterModels(models, "qwen", "all").map((m) => m.spec.id)).toEqual(["c"]);
  });

  it("ignores case and accents, so a French query matches an accented name", () => {
    const models = [model({ id: "a", name: "Modèle Français" })];
    expect(filterModels(models, "modele francais", "all")).toHaveLength(1);
    expect(filterModels(models, "MODÈLE", "all")).toHaveLength(1);
  });

  it("treats a blank or whitespace-only query as no query at all", () => {
    const models = [model({ id: "a" }), model({ id: "b" })];
    expect(filterModels(models, "", "all")).toHaveLength(2);
    expect(filterModels(models, "   ", "all")).toHaveLength(2);
  });

  it("counts only fully-installed models as installed, not partial downloads", () => {
    const models = [
      model({ id: "done" }, { status: INSTALLED }),
      model({ id: "half" }, { status: PARTIAL }),
      model({ id: "none" }),
    ];
    expect(filterModels(models, "", "installed").map((m) => m.spec.id)).toEqual(["done"]);
  });

  it("excludes both tooBig and diskSlow from 'compatible'", () => {
    const models = [
      model({ id: "perfect" }, { fit: "perfect" }),
      model({ id: "tight" }, { fit: "tight" }),
      model({ id: "disk" }, { fit: "diskSlow" }),
      model({ id: "big" }, { fit: "tooBig" }),
    ];
    expect(filterModels(models, "", "compatible").map((m) => m.spec.id)).toEqual([
      "perfect",
      "tight",
    ]);
  });

  it("applies the query and the filter together", () => {
    const models = [
      model({ id: "a", name: "Gemma small" }, { status: INSTALLED }),
      model({ id: "b", name: "Gemma large" }),
    ];
    expect(filterModels(models, "gemma", "installed").map((m) => m.spec.id)).toEqual(["a"]);
  });
});

describe("groupModels", () => {
  it("puts models sharing a groupId in one group, in first-seen order", () => {
    const models = [
      model({ id: "repo-q4", groupId: "repo" }),
      model({ id: "solo" }),
      model({ id: "repo-q8", groupId: "repo" }),
    ];
    expect(ids(groupModels(models))).toEqual([["repo-q4", "repo-q8"], ["solo"]]);
  });

  it("gives a model with no groupId a group of its own", () => {
    expect(ids(groupModels([model({ id: "a" }), model({ id: "b" })]))).toEqual([["a"], ["b"]]);
  });
});

describe("sortGroups", () => {
  it("ranks favorites first, then the loaded model, then what's on disk", () => {
    const groups = groupModels([
      model({ id: "plain" }),
      model({ id: "ondisk" }, { status: INSTALLED }),
      model({ id: "loaded" }, { status: INSTALLED }),
      model({ id: "fav" }, { favorite: true }),
    ]);
    expect(ids(sortGroups(groups, "recommended", ctx({ runningModelId: "loaded" })))).toEqual([
      ["fav"],
      ["loaded"],
      ["ondisk"],
      ["plain"],
    ]);
  });

  it("counts a partial download as on disk, ahead of an untouched model", () => {
    const groups = groupModels([
      model({ id: "none" }),
      model({ id: "half" }, { status: PARTIAL }),
    ]);
    expect(ids(sortGroups(groups, "recommended", ctx()))).toEqual([["half"], ["none"]]);
  });

  it("orders by fit, then quality, when nothing else separates two models", () => {
    const groups = groupModels([
      model({ id: "big", qualityScore: 99 }, { fit: "tooBig" }),
      model({ id: "snug", qualityScore: 10 }, { fit: "tight" }),
      model({ id: "great-low", qualityScore: 20 }, { fit: "perfect" }),
      model({ id: "great-high", qualityScore: 80 }, { fit: "perfect" }),
    ]);
    expect(ids(sortGroups(groups, "recommended", ctx()))).toEqual([
      ["great-high"],
      ["great-low"],
      ["snug"],
      ["big"],
    ]);
  });

  it("sorts by name and by size (smallest first) on request", () => {
    const groups = groupModels([
      model({ id: "c", name: "Charlie", sizeBytes: 1_000 }),
      model({ id: "a", name: "Alpha", sizeBytes: 3_000 }),
      model({ id: "b", name: "Bravo", sizeBytes: 2_000 }),
    ]);
    expect(ids(sortGroups(groups, "name", ctx()))).toEqual([["a"], ["b"], ["c"]]);
    expect(ids(sortGroups(groups, "size", ctx()))).toEqual([["c"], ["b"], ["a"]]);
  });

  it("counts every shard of a split model when sorting by size", () => {
    const groups = groupModels([
      model({ id: "single", sizeBytes: 5_000 }),
      model({
        id: "split",
        sizeBytes: 3_000,
        extraShards: [{ url: "", fileName: "", sizeBytes: 4_000, sha256: null }],
      }),
    ]);
    // 3k + 4k = 7k, so the split model is the larger one despite its smaller
    // first shard.
    expect(ids(sortGroups(groups, "size", ctx()))).toEqual([["single"], ["split"]]);
  });

  it("breaks the 0-speed tie among unusable models by fit instead of leaving it arbitrary", () => {
    // estimateTokensPerSec returns 0 for both diskSlow and tooBig, so without
    // a tiebreak this whole tail would compare equal.
    const groups = groupModels([
      model({ id: "big", name: "Big" }, { fit: "tooBig" }),
      model({ id: "disk", name: "Disk" }, { fit: "diskSlow" }),
      model({ id: "fine", name: "Fine", paramsB: 1, sizeBytes: 500_000_000 }, { fit: "perfect" }),
    ]);
    expect(ids(sortGroups(groups, "speed", ctx({ hardware: HARDWARE })))).toEqual([
      ["fine"],
      ["disk"],
      ["big"],
    ]);
  });

  it("puts the faster model first when hardware is known", () => {
    const groups = groupModels([
      model({ id: "slow", paramsB: 30, sizeBytes: 20_000_000_000 }, { fit: "good" }),
      model({ id: "fast", paramsB: 1, sizeBytes: 600_000_000 }, { fit: "good" }),
    ]);
    expect(ids(sortGroups(groups, "speed", ctx({ hardware: HARDWARE })))).toEqual([
      ["fast"],
      ["slow"],
    ]);
  });

  it("ranks a group by its best member and never reorders members within it", () => {
    const groups = groupModels([
      model({ id: "solo", qualityScore: 60 }, { fit: "good" }),
      model({ id: "repo-q4", groupId: "repo", qualityScore: 10 }, { fit: "tight" }),
      model({ id: "repo-q8", groupId: "repo", qualityScore: 90 }, { fit: "perfect" }),
    ]);
    // The group leads on its q8 member's perfect fit, but q4 stays first
    // inside it because that's the order it was declared in.
    expect(ids(sortGroups(groups, "recommended", ctx()))).toEqual([
      ["repo-q4", "repo-q8"],
      ["solo"],
    ]);
  });

  it("keeps the input order for groups that tie", () => {
    const groups = groupModels([
      model({ id: "first", name: "Same", qualityScore: 50 }),
      model({ id: "second", name: "Same", qualityScore: 50 }),
    ]);
    expect(ids(sortGroups(groups, "recommended", ctx()))).toEqual([["first"], ["second"]]);
  });
});

describe("buildModelList", () => {
  it("filters, then groups, then sorts — so a group's first member never depends on the sort", () => {
    const models = [
      model({ id: "repo-q4", name: "Repo Q4", groupId: "repo", qualityScore: 10 }),
      model({ id: "repo-q8", name: "Repo Q8", groupId: "repo", qualityScore: 90 }),
      model({ id: "zeta", name: "Zeta" }, { favorite: true }),
      model({ id: "big", name: "Big" }, { fit: "tooBig" }),
    ];

    const recommended = buildModelList(models, "", "compatible", "recommended", ctx());
    const byName = buildModelList(models, "", "compatible", "name", ctx());

    // "big" is filtered out by "compatible" in both.
    expect(ids(recommended)).toEqual([["zeta"], ["repo-q4", "repo-q8"]]);
    expect(ids(byName)).toEqual([["repo-q4", "repo-q8"], ["zeta"]]);
    // Same parent identity under either sort — the property the pipeline order exists for.
    expect(recommended[1][0].spec.id).toBe("repo-q4");
    expect(byName[0][0].spec.id).toBe("repo-q4");
  });

  it("returns an empty list when nothing matches, rather than falling back to everything", () => {
    const models = [model({ id: "a", name: "Gemma" })];
    expect(buildModelList(models, "nothing-matches-this", "all", "recommended", ctx())).toEqual([]);
  });
});
