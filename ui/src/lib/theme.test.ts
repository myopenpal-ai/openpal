import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { applyTheme, normalizeTheme, THEMES } from "./theme";

describe("THEMES", () => {
  it("lists System, Light, Dark: the order every picker shows them in", () => {
    expect(THEMES).toEqual(["system", "light", "dark"]);
  });

  it("holds only values normalizeTheme takes as they are", () => {
    expect(THEMES.map((theme) => normalizeTheme(theme))).toEqual([...THEMES]);
  });
});

describe("normalizeTheme", () => {
  it.each(["system", "light", "dark"] as const)("keeps %s", (theme) => {
    expect(normalizeTheme(theme)).toBe(theme);
  });

  it.each<[what: string, value: unknown]>([
    ["undefined, as in a settings row saved before the setting existed", undefined],
    ["null", null],
    ["an empty string", ""],
    ["a differently cased name", "Dark"],
    ["a name with a stray space", "light "],
    ["a theme this build doesn't know", "sepia"],
    ["a number", 1],
    ["a boolean", true],
    ["an object", { theme: "dark" }],
    ["an array holding a theme", ["dark"]],
    ["an object that prints as dark", { toString: () => "dark" }],
  ])("reads %s as system", (_what, value) => {
    expect(normalizeTheme(value)).toBe("system");
  });
});

describe("applyTheme", () => {
  const html = () => document.documentElement;

  // Counts the writes to <html data-theme> since it was last read: applyTheme
  // promises to make none when the attribute already says what it should.
  let observer: MutationObserver;
  const writes = () => observer.takeRecords().length;

  beforeEach(() => {
    observer = new MutationObserver(() => {});
    observer.observe(html(), { attributes: true, attributeFilter: ["data-theme"] });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    observer.disconnect();
    html().removeAttribute("data-theme");
  });

  it("pins light and dark on <html>, and only there", () => {
    applyTheme("light");
    expect(html()).toHaveAttribute("data-theme", "light");

    applyTheme("dark");
    expect(html()).toHaveAttribute("data-theme", "dark");

    // global.css keys the palettes on :root, and dialogs portal into <body>.
    expect(document.body).not.toHaveAttribute("data-theme");
  });

  it("removes the attribute for system, so the OS decides", () => {
    applyTheme("dark");
    applyTheme("system");

    expect(html()).not.toHaveAttribute("data-theme");
  });

  it.each(["sepia", "Dark", "", undefined, null, 1])(
    "reads %j as system, so it removes the attribute too",
    (value) => {
      applyTheme("dark");
      applyTheme(value);

      expect(html()).not.toHaveAttribute("data-theme");
    },
  );

  it("makes no write when the attribute already says what it should", () => {
    applyTheme("dark");
    expect(writes()).toBe(1);
    applyTheme("dark");
    applyTheme("dark");
    expect(writes()).toBe(0);

    applyTheme("system");
    expect(writes()).toBe(1);
    applyTheme("system");
    applyTheme("sepia"); // unrecognised means system, which is already the case
    applyTheme(undefined);
    expect(writes()).toBe(0);
  });

  it("does nothing, rather than throw, where there is no document to write to", () => {
    // It runs inside a zustand listener, and zustand doesn't catch a throwing one.
    vi.stubGlobal("document", undefined);
    expect(() => applyTheme("dark")).not.toThrow();

    vi.stubGlobal("document", { documentElement: null });
    expect(() => applyTheme("dark")).not.toThrow();

    vi.unstubAllGlobals();
    expect(writes()).toBe(0);
  });
});
