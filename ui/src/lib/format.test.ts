import { afterAll, afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import i18n from "../i18n";
import {
  architectureLabel,
  formatBytesAtLeast,
  formatChatStats,
  formatDuration,
  formatEta,
  formatSpeedCompact,
  formatTokensPerSec,
  groupKey,
  isAutoRecommendable,
  paramsWithActiveLabel,
  releaseDateLabel,
  roughFitLabel,
  speedTier,
  timeAgo,
  totalModelSizeBytes,
} from "./format";

// Guards every English assertion below from a French test elsewhere in this
// suite leaking its language change forward — each test starts from a known
// "en" baseline regardless of what the previous test left i18n in.
beforeEach(() => {
  void i18n.changeLanguage("en");
});

describe("isAutoRecommendable", () => {
  it("excludes diskSlow and nothing else", () => {
    expect(isAutoRecommendable("diskSlow")).toBe(false);
    expect(isAutoRecommendable("perfect")).toBe(true);
    expect(isAutoRecommendable("good")).toBe(true);
    expect(isAutoRecommendable("tight")).toBe(true);
    expect(isAutoRecommendable("tooBig")).toBe(true);
  });
});

describe("roughFitLabel", () => {
  it("is the only fit level that reads as 'could not fit'", () => {
    expect(roughFitLabel("tooBig")).toBe("Could not fit");
    for (const fit of ["perfect", "good", "tight", "diskSlow"] as const) {
      expect(roughFitLabel(fit)).toBe("Could fit");
    }
  });
});

describe("architectureLabel", () => {
  it("reads MoE only for a moe spec, Dense otherwise", () => {
    expect(architectureLabel({ moe: true })).toBe("MoE");
    expect(architectureLabel({ moe: false })).toBe("Dense");
  });
});

describe("paramsWithActiveLabel", () => {
  it("shows only the total for a dense model", () => {
    expect(paramsWithActiveLabel({ paramsB: 8, moe: false })).toBe("8B");
    expect(paramsWithActiveLabel({ paramsB: 8, moe: false, activeParamsB: 3 })).toBe("8B");
  });

  it("shows only the total for a MoE model without a known active count", () => {
    expect(paramsWithActiveLabel({ paramsB: 30, moe: true })).toBe("30B");
  });

  it("adds the active count for a MoE model that has one", () => {
    expect(paramsWithActiveLabel({ paramsB: 30, moe: true, activeParamsB: 3.3 })).toBe(
      "30B (3B active)",
    );
  });
});

describe("groupKey", () => {
  it("falls back to id without a groupId", () => {
    expect(groupKey({ id: "solo-model" })).toBe("solo-model");
    expect(groupKey({ id: "solo-model", groupId: null as unknown as undefined })).toBe(
      "solo-model",
    );
  });

  it("prefers groupId when set", () => {
    expect(groupKey({ id: "custom-a", groupId: "org/repo" })).toBe("org/repo");
  });
});

describe("releaseDateLabel", () => {
  it("reads '' for an unknown or unparseable date", () => {
    expect(releaseDateLabel(undefined)).toBe("");
    expect(releaseDateLabel(null)).toBe("");
    expect(releaseDateLabel("not-a-date")).toBe("");
  });

  it("formats a day-precision ISO date as month + year", () => {
    expect(releaseDateLabel("2024-09-25")).toBe("Sep 2024");
  });

  it("formats a full timestamp the same way", () => {
    expect(releaseDateLabel("2024-07-23T00:00:00.000Z")).toBe("Jul 2024");
  });
});

describe("totalModelSizeBytes", () => {
  it("sums shard 1 alone when there are no extra shards", () => {
    expect(totalModelSizeBytes({ sizeBytes: 1_000 })).toBe(1_000);
    expect(totalModelSizeBytes({ sizeBytes: 1_000, extraShards: [] })).toBe(1_000);
  });

  it("adds every extra shard's size for a split GGUF", () => {
    expect(
      totalModelSizeBytes({
        sizeBytes: 1_000,
        extraShards: [{ sizeBytes: 2_000 }, { sizeBytes: 3_000 }],
      }),
    ).toBe(6_000);
  });
});

describe("formatBytesAtLeast", () => {
  const MB = 1024 ** 2;
  const GB = 1024 ** 3;

  it("rounds up to the step formatBytes shows, never down", () => {
    expect(formatBytesAtLeast(22)).toBe("1 MB");
    expect(formatBytesAtLeast(1.2 * MB)).toBe("2 MB");
    expect(formatBytesAtLeast(5 * MB)).toBe("5 MB");
    expect(formatBytesAtLeast(1023.5 * MB)).toBe("1 GB");
    expect(formatBytesAtLeast(1.21 * GB)).toBe("1.3 GB");
    expect(formatBytesAtLeast(9.95 * GB)).toBe("10 GB");
    expect(formatBytesAtLeast(12.2 * GB)).toBe("13 GB");
  });
});

describe("formatTokensPerSec", () => {
  it("reports a rounded whole number at or above 10 tok/s", () => {
    expect(formatTokensPerSec(12.4)).toBe("about 12 tokens per second");
  });

  it("keeps one decimal between 1 and 10 tok/s", () => {
    expect(formatTokensPerSec(1.23)).toBe("about 1.2 tokens per second");
  });

  it("switches to a minutes-based warning under 1 tok/s", () => {
    // 100 tokens / 0.5 tok/s / 60 = ~3.3 minutes -> the plain "slow" branch.
    expect(formatTokensPerSec(0.5)).toBe("slow (expect to wait several minutes for each answer)");
    // 100 / 0.1 / 60 = ~16.7 minutes -> the "very slow" branch (>= 10 min).
    expect(formatTokensPerSec(0.1)).toContain("very slow");
    // 100 / 0.01 / 60 = ~166.7 minutes -> over an hour.
    expect(formatTokensPerSec(0.01)).toBe(
      "so slow that even a short answer could take over an hour",
    );
  });

  describe("French pluralization", () => {
    beforeEach(async () => {
      await i18n.changeLanguage("fr");
    });

    afterAll(async () => {
      await i18n.changeLanguage("en");
    });

    it("uses the singular 'jeton' for French's 'one' category, which (unlike English) covers any value under 2 — not just exactly 1", () => {
      // count === 1 exactly.
      expect(formatTokensPerSec(1)).toBe("environ 1 jeton par seconde");
      // count === 1.2, still French "one" (contrast: English's medium_one
      // and medium_other are identical text, so this distinction only shows
      // up in French).
      expect(formatTokensPerSec(1.23)).toBe("environ 1.2 jeton par seconde");
    });

    it("uses the plural 'jetons' once count reaches 2", () => {
      expect(formatTokensPerSec(3.4)).toBe("environ 3.4 jetons par seconde");
    });
  });
});

describe("formatSpeedCompact", () => {
  it("reports a rounded whole number at or above 10 tok/s", () => {
    expect(formatSpeedCompact(12.4)).toBe("12 tok/s");
  });

  it("keeps one decimal between 1 and 10 tok/s", () => {
    expect(formatSpeedCompact(1.23)).toBe("1.2 tok/s");
  });

  it("collapses anything under 1 tok/s to a single label", () => {
    expect(formatSpeedCompact(0.5)).toBe("<1 tok/s");
    expect(formatSpeedCompact(0)).toBe("<1 tok/s");
  });

  it("shares speedTier's cutoffs, so the dot's color can never disagree with the label", () => {
    expect(speedTier(12.4)).toBe("fast");
    expect(speedTier(1.23)).toBe("medium");
    expect(speedTier(0.5)).toBe("slow");
  });

  describe("French pluralization", () => {
    beforeEach(async () => {
      await i18n.changeLanguage("fr");
    });

    afterAll(async () => {
      await i18n.changeLanguage("en");
    });

    it("uses the singular 'jeton' for French's 'one' category (anything under 2)", () => {
      expect(formatSpeedCompact(1)).toBe("1 jeton/s");
      expect(formatSpeedCompact(1.23)).toBe("1.2 jeton/s");
    });

    it("uses the plural 'jetons' once count reaches 2", () => {
      expect(formatSpeedCompact(12.4)).toBe("12 jetons/s");
    });

    it("translates the under-1 label rather than leaving it English", () => {
      expect(formatSpeedCompact(0.5)).toBe("< 1 jeton/s");
    });
  });
});

describe("formatDuration", () => {
  it("shows fractional seconds under 10s", () => {
    expect(formatDuration(2.3)).toBe("2.3s");
  });

  it("shows whole seconds between 10s and 60s", () => {
    expect(formatDuration(45)).toBe("45s");
  });

  it("shows minutes and seconds under an hour", () => {
    expect(formatDuration(125)).toBe("2m 5s");
  });

  it("shows hours and minutes at or above an hour", () => {
    expect(formatDuration(3661)).toBe("1h 1m");
  });
});

describe("formatChatStats", () => {
  it("combines the tok/s readout with a formatted duration", () => {
    expect(formatChatStats({ totalTokens: 100, tokensPerSec: 42, durationMs: 2_300 })).toBe(
      "42 tok/s · 2.3s",
    );
    expect(formatChatStats({ totalTokens: 20, tokensPerSec: 3.456, durationMs: 500 })).toBe(
      "3.5 tok/s · 0.5s",
    );
  });
});

describe("timeAgo", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-01-15T12:00:00Z"));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  const now = () => Date.now();

  it("reads as 'just now' under a minute", () => {
    expect(timeAgo(now() - 30_000)).toBe("just now");
  });

  it("reads as minutes ago under an hour", () => {
    expect(timeAgo(now() - 5 * 60_000)).toBe("5 min ago");
  });

  it("uses the singular 'an hour ago'", () => {
    expect(timeAgo(now() - 60 * 60_000)).toBe("an hour ago");
  });

  it("uses the plural 'N hours ago'", () => {
    expect(timeAgo(now() - 3 * 60 * 60_000)).toBe("3 hours ago");
  });

  it("uses the singular 'yesterday'", () => {
    expect(timeAgo(now() - 24 * 60 * 60_000)).toBe("yesterday");
  });

  it("uses 'N days ago' short of a week", () => {
    expect(timeAgo(now() - 3 * 24 * 60 * 60_000)).toBe("3 days ago");
  });

  describe("French pluralization", () => {
    beforeEach(async () => {
      await i18n.changeLanguage("fr");
    });

    afterAll(async () => {
      await i18n.changeLanguage("en");
    });

    it("uses the singular 'il y a une heure' at the 1-hour boundary", () => {
      expect(timeAgo(now() - 60 * 60_000)).toBe("il y a une heure");
    });

    it("uses the plural 'il y a N heures' from 2 hours on", () => {
      expect(timeAgo(now() - 3 * 60 * 60_000)).toBe("il y a 3 heures");
    });
  });
});

describe("formatEta", () => {
  describe("French pluralization", () => {
    beforeEach(async () => {
      await i18n.changeLanguage("fr");
    });

    afterAll(async () => {
      await i18n.changeLanguage("en");
    });

    it("uses the singular 'environ une heure restante' at the 1-hour boundary", () => {
      expect(formatEta(3600)).toBe("environ une heure restante");
    });

    it("uses the plural 'environ N heures restantes' from 2 hours on", () => {
      expect(formatEta(3 * 3600)).toBe("environ 3 heures restantes");
    });
  });
});
