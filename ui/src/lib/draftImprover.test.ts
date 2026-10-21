import { describe, expect, it } from "vitest";
import {
  improveBlocker,
  improveBlockerKey,
  joinImproved,
  keptStatus,
  MAX_DRAFT_CHARS,
  splitDraftForImprove,
  type ImproveBlockerInput,
} from "./draftImprover";
import type { KeepReason, Skill } from "./types";
import en from "../i18n/locales/en.json";

function skill(overrides: Partial<Skill> = {}): Skill {
  return {
    slug: "summarize",
    name: "Summarize",
    description: "Condense text.",
    body: "Summarize the text.",
    ...overrides,
  };
}

function baseInput(overrides: Partial<ImproveBlockerInput> = {}): ImproveBlockerInput {
  return {
    locked: false,
    improving: false,
    ready: true,
    starting: false,
    diskSlow: false,
    draftText: "write email boss vacation",
    hadSkillPrefix: false,
    slashMatch: false,
    ...overrides,
  };
}

/** Recursively flattens a nested object into dot-path keys — same helper
 * shape as `i18n/keys.test.ts`, used here only to assert every key
 * `improveBlockerKey`/`keptStatus` can return actually exists in en.json. */
function flattenKeys(obj: object, prefix = ""): string[] {
  return Object.entries(obj).flatMap(([key, value]) => {
    const path = prefix ? `${prefix}.${key}` : key;
    if (typeof value === "object" && value !== null && !Array.isArray(value)) {
      return flattenKeys(value, path);
    }
    return [path];
  });
}

describe("splitDraftForImprove", () => {
  it("returns the whole draft as text for plain text", () => {
    expect(splitDraftForImprove("hello there", [skill()])).toEqual({
      prefix: "",
      text: "hello there",
      hadSkillPrefix: false,
    });
  });

  it("treats an unknown /token as plain text", () => {
    expect(splitDraftForImprove("/nope hi", [skill()])).toEqual({
      prefix: "",
      text: "/nope hi",
      hadSkillPrefix: false,
    });
  });

  it("splits a known slug from the rest of the message", () => {
    expect(splitDraftForImprove("/summarize the meeting notes", [skill()])).toEqual({
      prefix: "/summarize ",
      text: "the meeting notes",
      hadSkillPrefix: true,
    });
  });

  it("splits a known slug alone into an empty text (nothing to improve)", () => {
    expect(splitDraftForImprove("/summarize", [skill()])).toEqual({
      prefix: "/summarize ",
      text: "",
      hadSkillPrefix: true,
    });
  });

  it("matches the slug case-insensitively", () => {
    expect(splitDraftForImprove("/SUMMARIZE notes", [skill()])).toEqual({
      prefix: "/summarize ",
      text: "notes",
      hadSkillPrefix: true,
    });
  });
});

describe("joinImproved", () => {
  it("round-trips a skill prefix with exactly one space", () => {
    expect(joinImproved("/summarize ", "the shorter notes")).toBe(
      "/summarize the shorter notes",
    );
  });

  it("returns the text unchanged when there is no prefix", () => {
    expect(joinImproved("", "improved text")).toBe("improved text");
  });
});

describe("improveBlocker", () => {
  it("returns null when the action is usable", () => {
    expect(improveBlocker(baseInput())).toBeNull();
  });

  it("locked takes priority over everything else", () => {
    expect(
      improveBlocker(baseInput({ locked: true, improving: true, draftText: "" })),
    ).toBe("locked");
  });

  it("improving takes priority over the draft's own blockers", () => {
    expect(improveBlocker(baseInput({ improving: true, draftText: "" }))).toBe("improving");
  });

  it("empty when there is no text and no skill prefix", () => {
    expect(improveBlocker(baseInput({ draftText: "   " }))).toBe("empty");
  });

  it("skillOnly for a lone /word (slashMatch)", () => {
    expect(improveBlocker(baseInput({ draftText: "/sum", slashMatch: true }))).toBe(
      "skillOnly",
    );
  });

  it("skillOnly for a recognized skill with nothing after it", () => {
    expect(improveBlocker(baseInput({ draftText: "", hadSkillPrefix: true }))).toBe(
      "skillOnly",
    );
  });

  it("is usable for a recognized skill WITH text after it", () => {
    expect(
      improveBlocker(baseInput({ draftText: "the meeting notes", hadSkillPrefix: true })),
    ).toBeNull();
  });

  it("tooLong past MAX_DRAFT_CHARS", () => {
    expect(improveBlocker(baseInput({ draftText: "x".repeat(MAX_DRAFT_CHARS + 1) }))).toBe(
      "tooLong",
    );
  });

  it("usable exactly at MAX_DRAFT_CHARS", () => {
    expect(improveBlocker(baseInput({ draftText: "x".repeat(MAX_DRAFT_CHARS) }))).toBeNull();
  });

  it("diskSlow when the running model is disk-slow", () => {
    expect(improveBlocker(baseInput({ diskSlow: true }))).toBe("diskSlow");
  });

  it("starting when not ready and the engine is starting", () => {
    expect(improveBlocker(baseInput({ ready: false, starting: true }))).toBe("starting");
  });

  it("notReady when not ready and not starting", () => {
    expect(improveBlocker(baseInput({ ready: false, starting: false }))).toBe("notReady");
  });
});

describe("improveBlockerKey", () => {
  const cases: Array<[Parameters<typeof improveBlockerKey>[0], string]> = [
    ["locked", "chatView.parentalLock.placeholder"],
    ["improving", "chatView.composer.improve.offImproving"],
    ["empty", "chatView.composer.improve.offEmpty"],
    ["skillOnly", "chatView.composer.improve.offSkillOnly"],
    ["tooLong", "chatView.composer.improve.offTooLong"],
    ["diskSlow", "chatView.composer.improve.offDiskSlow"],
    ["starting", "chatView.composer.improve.offStarting"],
    ["notReady", "chatView.composer.improve.offNotReady"],
  ];

  it.each(cases)("maps %s to %s", (blocker, key) => {
    expect(improveBlockerKey(blocker)).toBe(key);
  });

  it("maps every blocker to a key that exists in en.json", () => {
    const flat = new Set(flattenKeys(en));
    for (const [, key] of cases) {
      expect(flat.has(key)).toBe(true);
    }
  });
});

describe("keptStatus", () => {
  const allReasons: KeepReason[] = [
    "empty",
    "noAnswer",
    "tooLong",
    "tooShort",
    "refused",
    "answered",
    "languageChanged",
    "startsWithSlash",
    "cancelled",
    "noChange",
  ];

  it("maps every KeepReason to a key that exists in en.json", () => {
    const flat = new Set(flattenKeys(en));
    for (const reason of allReasons) {
      expect(flat.has(keptStatus(reason).key)).toBe(true);
    }
  });

  it("marks only failures as alerts", () => {
    expect(keptStatus("noChange").alert).toBe(false);
    expect(keptStatus("cancelled").alert).toBe(false);
    expect(keptStatus("noAnswer").alert).toBe(true);
    for (const reason of [
      "empty",
      "tooLong",
      "tooShort",
      "refused",
      "answered",
      "languageChanged",
      "startsWithSlash",
    ] as KeepReason[]) {
      expect(keptStatus(reason).alert).toBe(true);
    }
  });

  it("collapses the seven quality-guard reasons onto the same generic key", () => {
    const failed = "chatView.composer.improve.failed";
    for (const reason of [
      "empty",
      "tooLong",
      "tooShort",
      "refused",
      "answered",
      "languageChanged",
      "startsWithSlash",
    ] as KeepReason[]) {
      expect(keptStatus(reason).key).toBe(failed);
    }
  });

  it("gives noChange, noAnswer, and cancelled their own distinct keys", () => {
    expect(keptStatus("noChange").key).toBe("chatView.composer.improve.noChange");
    expect(keptStatus("noAnswer").key).toBe("chatView.composer.improve.noAnswer");
    expect(keptStatus("cancelled").key).toBe("chatView.composer.improve.stopped");
  });
});
