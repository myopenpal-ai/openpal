import { describe, expect, it } from "vitest";
import { estimateTokensPerSec, type SpeedEstimateSpec } from "./speedEstimate";
import type { HardwareInfo } from "./types";

function hardware(overrides: Partial<HardwareInfo> = {}): HardwareInfo {
  return {
    chip: "Apple M4",
    isAppleSilicon: true,
    totalRamBytes: 16 * 1_000_000_000,
    availableRamBytes: 12 * 1_000_000_000,
    cpuCores: 10,
    hasMetal: true,
    freeDiskBytes: 100 * 1_000_000_000,
    osVersion: "macOS 26.5",
    ...overrides,
  };
}

function denseSpec(sizeBytes: number): SpeedEstimateSpec {
  return { sizeBytes, moe: false, paramsB: 8 };
}

describe("estimateTokensPerSec — fit branching", () => {
  it("returns the unqualified bandwidth/active-bytes number for perfect/good/undefined fit", () => {
    const hw = hardware();
    const spec = denseSpec(1_000_000_000);
    const naive = estimateTokensPerSec(spec, hw);
    expect(naive).toBeGreaterThan(0);
    expect(estimateTokensPerSec(spec, hw, "perfect")).toBe(naive);
    expect(estimateTokensPerSec(spec, hw, "good")).toBe(naive);
  });

  it("returns exactly 0 for diskSlow and tooBig, regardless of the naive number", () => {
    const hw = hardware();
    const spec = denseSpec(1_000_000_000);
    expect(estimateTokensPerSec(spec, hw, "diskSlow")).toBe(0);
    expect(estimateTokensPerSec(spec, hw, "tooBig")).toBe(0);
  });

  it("returns 0 when the spec carries no size at all", () => {
    expect(estimateTokensPerSec(denseSpec(0), hardware())).toBe(0);
  });

  it("derates 'tight' continuously between 0 and the naive number", () => {
    const hw = hardware({ totalRamBytes: 16 * 1_000_000_000 });
    // Above the 70% cacheable budget (11.2 GB), so residencyMissRate > 0 —
    // still resident (fit="tight" is asserted directly here), not disk-streamed.
    const spec = denseSpec(12 * 1_000_000_000);
    const naive = estimateTokensPerSec(spec, hw);
    const tight = estimateTokensPerSec(spec, hw, "tight");
    expect(tight).toBeGreaterThan(0);
    expect(tight).toBeLessThan(naive);
  });

  it("derates more as the model grows relative to total RAM", () => {
    const hw = hardware({ totalRamBytes: 16 * 1_000_000_000 });
    const smallerTight = estimateTokensPerSec(denseSpec(8 * 1_000_000_000), hw, "tight");
    const biggerTight = estimateTokensPerSec(denseSpec(15 * 1_000_000_000), hw, "tight");
    expect(biggerTight).toBeLessThan(smallerTight);
  });
});

describe("estimateTokensPerSec — Apple bandwidth table ordering", () => {
  it("matches the most specific chip name, not a shorter substring", () => {
    const spec = denseSpec(1_000_000_000);
    const m4 = estimateTokensPerSec(spec, hardware({ chip: "Apple M4" }));
    const m4Max = estimateTokensPerSec(spec, hardware({ chip: "Apple M4 Max" }));
    const m4Pro = estimateTokensPerSec(spec, hardware({ chip: "Apple M4 Pro" }));
    // M4 Max (546 GB/s) and M4 Pro (273 GB/s) must resolve to their own,
    // faster bandwidth rather than falling through to the bare "M4" (120 GB/s).
    expect(m4Max).toBeGreaterThan(m4Pro);
    expect(m4Pro).toBeGreaterThan(m4);
  });
});

describe("estimateTokensPerSec — MoE vs dense", () => {
  it("estimates a MoE model faster than treating the same bytes as dense", () => {
    const hw = hardware();
    const totalBytes = 10 * 1_000_000_000;
    const dense: SpeedEstimateSpec = { sizeBytes: totalBytes, moe: false, paramsB: 30 };
    const moe: SpeedEstimateSpec = {
      sizeBytes: totalBytes,
      moe: true,
      paramsB: 30,
      activeParamsB: 3,
    };
    const denseSpeed = estimateTokensPerSec(dense, hw, "good");
    const moeSpeed = estimateTokensPerSec(moe, hw, "good");
    expect(moeSpeed).toBeGreaterThan(denseSpeed);
  });
});
