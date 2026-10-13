import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { contrastRatio } from "../lib/contrast";

// Reads the live tokens out of global.css itself (rather than duplicating
// their values here) so this test actually regresses if someone edits a
// token without re-checking contrast. See docs/design/ACCESSIBILITY_AUDIT.md
// pattern 1 and PR-1 of docs/design/ACCESSIBILITY_PLAN.md.
//
// There are three palettes to read, because the Theme setting can pin Light or
// Dark as well as follow the OS: the base `:root` block is the light palette,
// and the dark palette is written twice, under `:root[data-theme="dark"]` (Dark
// pinned) and inside `@media (prefers-color-scheme: dark)` (System, on a dark
// OS). All three go through the same contrast checks. "theme structure" then
// keeps the two dark copies identical and every other rule from reading the OS
// scheme behind a pinned theme's back, and the last block shows each of those
// checks failing on a broken in-memory copy of global.css: jsdom computes no
// CSS, so nothing else would notice.
const stylesDir = dirname(fileURLToPath(import.meta.url));
const cssPath = join(stylesDir, "global.css");
const css = readFileSync(cssPath, "utf-8");

/** CSS comments can say anything, a `}` or a `--name:` included, and none of it
 * is a rule. */
function stripComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\//g, "");
}

function extractBlock(source: string, pattern: RegExp): string {
  const match = source.match(pattern);
  if (!match) throw new Error(`Could not find block matching ${pattern}`);
  return match[1];
}

function parseTokens(block: string): Record<string, string> {
  const tokens: Record<string, string> = {};
  // Comments go first: the one above --ink-scrim in global.css quotes
  // `--ink-rgb:`, which would otherwise parse as a second --ink-rgb and
  // swallow the real --ink-scrim declaration.
  for (const m of stripComments(block).matchAll(/--([\w-]+):\s*([^;]+);/g)) {
    tokens[m[1]] = m[2].trim();
  }
  return tokens;
}

/** Resolves a token value to a literal hex color, following `var(--x)`
 * references within the same theme block (e.g. dark's --badge-ok-ink). */
function resolveHex(tokens: Record<string, string>, name: string): string {
  const value = tokens[name];
  if (!value) throw new Error(`Unknown token --${name}`);
  const varRef = value.match(/^var\(--([\w-]+)\)$/);
  if (varRef) return resolveHex(tokens, varRef[1]);
  if (!/^#[0-9a-f]{6}$/i.test(value)) {
    throw new Error(`--${name} is not a literal hex color: ${value}`);
  }
  return value;
}

function resolveRgbTriple(tokens: Record<string, string>, name: string): [number, number, number] {
  const parts = tokens[name]?.split(",").map((n) => parseInt(n.trim(), 10));
  if (!parts || parts.length !== 3 || parts.some(Number.isNaN)) {
    throw new Error(`--${name} is not an "r, g, b" triple: ${tokens[name]}`);
  }
  return parts as [number, number, number];
}

/** Alpha of a `rgba(var(--x-rgb), 0.13)`-shaped soft token. */
function resolveAlpha(tokens: Record<string, string>, name: string): number {
  const match = tokens[name]?.match(/rgba\([^,]+,\s*([\d.]+)\)/);
  if (!match) throw new Error(`--${name} is not an rgba() token: ${tokens[name]}`);
  return parseFloat(match[1]);
}

function hexToRgb(hex: string): [number, number, number] {
  const clean = hex.replace("#", "");
  return [
    parseInt(clean.slice(0, 2), 16),
    parseInt(clean.slice(2, 4), 16),
    parseInt(clean.slice(4, 6), 16),
  ];
}

function toHex([r, g, b]: [number, number, number]): string {
  return "#" + [r, g, b].map((c) => Math.round(c).toString(16).padStart(2, "0")).join("");
}

/** Flattens a translucent foreground over an opaque background. */
function composite(fg: [number, number, number], alpha: number, bg: [number, number, number]): string {
  return toHex([0, 1, 2].map((i) => fg[i] * alpha + bg[i] * (1 - alpha)) as [number, number, number]);
}

// Where each rule lives in global.css. Every selector is spelled out in full
// and anchored to the start of a line, so a match can only be the rule itself:
// not a prose mention of `:root {`, not a selector list, not an
// `html[data-theme=dark]` that would quietly stop applying. Quote style is free
// (authors write "dark", the minifier emits dark). A block must not nest a `}`.
// If global.css is restructured these throw, loudly, rather than check the
// wrong thing. One catch: the light palette is simply the first plain `:root {`
// in the file, so keep the palette block first; a stray `:root { ... }` above
// it would be read as the palette.
const LIGHT_PALETTE = /^:root \{([^}]*)\}/m;
const PINNED_LIGHT = /^:root\[data-theme=(?:"light"|'light'|light)\]\s*\{([^}]*)\}/m;
const PINNED_DARK = /^:root\[data-theme=(?:"dark"|'dark'|dark)\]\s*\{([^}]*)\}/m;
// `:not([data-theme=light])` is what keeps a pinned Light on a dark OS from
// getting these tokens: without it the media rule ties with the base block on
// specificity and wins by coming later.
const OS_DARK =
  /@media \(prefers-color-scheme: dark\)\s*\{\s*:root:not\(\[data-theme=(?:"light"|'light'|light)\]\)\s*\{([^}]*)\}/;

interface Palettes {
  light: Record<string, string>;
  pinnedDark: Record<string, string>;
  osDark: Record<string, string>;
}

function extractPalettes(source: string): Palettes {
  const text = stripComments(source);
  return {
    light: parseTokens(extractBlock(text, LIGHT_PALETTE)),
    pinnedDark: parseTokens(extractBlock(text, PINNED_DARK)),
    osDark: parseTokens(extractBlock(text, OS_DARK)),
  };
}

// The structure checks return what is wrong, as sentences, instead of asserting
// it, so the code that passes on the real stylesheet can be pointed at a broken
// copy of it in the last describe block below.

/** The pinned and the OS copy are two routes to one dark palette (CSS can't
 * share a rule between an attribute and a media query), so they have to hold
 * the same tokens with the same values. */
function darkCopyProblems({ pinnedDark, osDark }: Palettes): string[] {
  const shown = (value: string | undefined) => value ?? "(missing)";
  return Object.keys({ ...pinnedDark, ...osDark })
    .filter((name) => pinnedDark[name] !== osDark[name])
    .map(
      (name) =>
        `--${name} differs between the dark copies: ` +
        `pinned ${shown(pinnedDark[name])}, OS ${shown(osDark[name])}`,
    );
}

/** A dark token that light lacks is a typo (`--paper-sunkn`) or a half-added
 * token: nothing reads the name, or light is left without a value. */
function darkOnlyProblems({ light, pinnedDark, osDark }: Palettes): string[] {
  return Object.keys({ ...pinnedDark, ...osDark })
    .filter((name) => !(name in light))
    .map((name) => `--${name} is set for dark but not for light`);
}

/** Guards the extraction itself: were the "light" block really the dark palette
 * (or the reverse), every contrast check would still pass, on one palette
 * checked twice. --paper's contrast against black says which one is lighter. */
function swappedProblems({ light, pinnedDark, osDark }: Palettes): string[] {
  const againstBlack = (tokens: Record<string, string>) =>
    contrastRatio(resolveHex(tokens, "paper"), "#000000");
  const copies = [
    ["pinned", pinnedDark],
    ["OS", osDark],
  ] as const;
  return copies
    .filter(([, dark]) => againstBlack(dark) >= againstBlack(light))
    .map(
      ([which]) => `the ${which} dark copy's --paper is not darker than light's: swapped palettes`,
    );
}

function declaredColorScheme(block: string): string | null {
  const match = block.match(/(?<![-\w])color-scheme:\s*([^;]+);/);
  return match ? match[1].trim() : null;
}

/** Pinning a theme has to pin `color-scheme` too, or native controls (select
 * menus, scrollbars, text fields) keep following the OS and turn illegible when
 * it disagrees with the pin. The OS copy must not set it: System's value comes
 * from index.html's `light dark` meta, and one here would make System differ
 * from what it was before the setting existed. */
function colorSchemeProblems(source: string): string[] {
  const text = stripComments(source);
  const problems: string[] = [];
  for (const [selector, pattern, expected] of [
    [':root[data-theme="light"]', PINNED_LIGHT, "light"],
    [':root[data-theme="dark"]', PINNED_DARK, "dark"],
  ] as const) {
    const declared = declaredColorScheme(extractBlock(text, pattern));
    if (declared !== expected) {
      problems.push(`${selector} must set color-scheme: ${expected} (found ${declared ?? "none"})`);
    }
  }
  const inOsCopy = declaredColorScheme(extractBlock(text, OS_DARK));
  if (inOsCopy !== null) {
    problems.push(`the prefers-color-scheme copy must not set color-scheme (found ${inOsCopy})`);
  }
  return problems;
}

/** Exactly one rule may look at the OS scheme: global.css's OS copy of the dark
 * palette. Any other keeps following the OS and ignores a pinned theme, and
 * `light-dark()` is a second way to pick colours that the token blocks know
 * nothing about (and newer than the safari15 build target). Comments don't
 * count: theme-picker.css's header names prefers-color-scheme to say why it
 * must not use it. */
function schemeLeakProblems(sheets: Record<string, string>): string[] {
  const problems: string[] = [];
  for (const [file, source] of Object.entries(sheets)) {
    const text = stripComments(source);
    const allowed = file === "global.css" ? 1 : 0;
    const found = text.match(/prefers-color-scheme/g)?.length ?? 0;
    if (found !== allowed) {
      problems.push(`${file}: prefers-color-scheme appears ${found} time(s), expected ${allowed}`);
    }
    if (/light-dark\s*\(/.test(text)) problems.push(`${file}: uses light-dark()`);
  }
  return problems;
}

/** `source` with the block `pattern` finds rewritten by `edit`. Throws when the
 * edit changes nothing, so a case can't pass because its mutation missed. */
function editBlock(source: string, pattern: RegExp, edit: (body: string) => string): string {
  const match = source.match(pattern);
  if (!match || match.index === undefined) {
    throw new Error(`Could not find block matching ${pattern}`);
  }
  const body = edit(match[1]);
  if (body === match[1]) {
    throw new Error(`The edit changed nothing in the block matching ${pattern}`);
  }
  const close = match.index + match[0].length - 1; // every pattern ends on the block's `}`
  return source.slice(0, close - match[1].length) + body + source.slice(close);
}

const palettes = extractPalettes(css);

// Every stylesheet beside global.css, global.css itself included.
const stylesheets: Record<string, string> = Object.fromEntries(
  readdirSync(stylesDir)
    .filter((file) => file.endsWith(".css"))
    .map((file) => [file, readFileSync(join(stylesDir, file), "utf-8")]),
);

describe.each([
  ["light", palettes.light],
  ["dark (pinned)", palettes.pinnedDark],
  ["dark (OS)", palettes.osDark],
])("%s theme", (_themeName, tokens) => {
  const paperBackgrounds = ["paper", "paper-raised", "paper-sunken"] as const;

  it("--ink-soft clears 4.5:1 (normal text) against every paper background", () => {
    const inkSoft = resolveHex(tokens, "ink-soft");
    for (const bg of paperBackgrounds) {
      expect(contrastRatio(inkSoft, resolveHex(tokens, bg))).toBeGreaterThanOrEqual(4.5);
    }
  });

  it("--ink-faint clears 3:1 (icon/decorative use only) against every paper background", () => {
    const inkFaint = resolveHex(tokens, "ink-faint");
    for (const bg of paperBackgrounds) {
      expect(contrastRatio(inkFaint, resolveHex(tokens, bg))).toBeGreaterThanOrEqual(3);
    }
  });

  it("--badge-ok-ink clears 4.5:1 on .badge-perfect/.badge-good's --ok-soft background", () => {
    const bg = composite(
      resolveRgbTriple(tokens, "ok-rgb"),
      resolveAlpha(tokens, "ok-soft"),
      hexToRgb(resolveHex(tokens, "paper-raised")),
    );
    expect(contrastRatio(resolveHex(tokens, "badge-ok-ink"), bg)).toBeGreaterThanOrEqual(4.5);
  });

  it("--badge-warn-ink clears 4.5:1 on both .badge-tight's and .badge-diskslow's backgrounds", () => {
    const warnSoftBg = composite(
      resolveRgbTriple(tokens, "warn-rgb"),
      resolveAlpha(tokens, "warn-soft"),
      hexToRgb(resolveHex(tokens, "paper-raised")),
    );
    const badgeWarnInk = resolveHex(tokens, "badge-warn-ink");
    expect(contrastRatio(badgeWarnInk, warnSoftBg)).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio(badgeWarnInk, resolveHex(tokens, "paper-sunken"))).toBeGreaterThanOrEqual(4.5);
  });

  it("AutoUnloadTooltip's close glyph (--ink-soft) clears 3:1 on its warn-tinted surface", () => {
    // `.auto-unload-tooltip` layers --warn-soft over --paper-raised to get an
    // opaque warning surface. --ink-faint, which .icon-btn would otherwise
    // supply, drops to 2.56:1 against it in dark mode — below WCAG 1.4.11.
    const surface = composite(
      resolveRgbTriple(tokens, "warn-rgb"),
      resolveAlpha(tokens, "warn-soft"),
      hexToRgb(resolveHex(tokens, "paper-raised")),
    );
    expect(contrastRatio(resolveHex(tokens, "ink-soft"), surface)).toBeGreaterThanOrEqual(3);
  });

  it("--badge-toobig text (--ink-soft) clears 4.5:1 on .badge-toobig's --paper-sunken background", () => {
    expect(
      contrastRatio(resolveHex(tokens, "ink-soft"), resolveHex(tokens, "paper-sunken")),
    ).toBeGreaterThanOrEqual(4.5);
  });
});

describe("theme structure", () => {
  it("keeps the two dark copies identical", () => {
    expect(darkCopyProblems(palettes)).toEqual([]);
  });

  it("defines every dark token for light too", () => {
    expect(darkOnlyProblems(palettes)).toEqual([]);
  });

  it("has a light palette that is lighter than both dark copies", () => {
    expect(swappedProblems(palettes)).toEqual([]);
  });

  it("pins color-scheme on the pinned rules and leaves it out of the OS copy", () => {
    expect(colorSchemeProblems(css)).toEqual([]);
  });

  it("keeps every rule but the OS copy from reading the OS colour scheme", () => {
    // The directory read must at least find the stylesheet this file is about,
    // or the check below would pass on nothing.
    expect(Object.keys(stylesheets)).toContain("global.css");
    expect(schemeLeakProblems(stylesheets)).toEqual([]);
  });

  it("reads --ink-rgb and --ink-scrim as declared, not out of the comment between them", () => {
    // Straight from the raw block, comments and all.
    const light = parseTokens(extractBlock(css, LIGHT_PALETTE));
    expect(() => resolveRgbTriple(light, "ink-rgb")).not.toThrow();
    expect(light["ink-scrim"]).toMatch(/^rgba\(\d+, \d+, \d+, [\d.]+\)$/);
  });

  it("parseTokens skips a --name: that only appears inside a comment", () => {
    expect(
      parseTokens(`
        --ink-rgb: 36, 29, 21;
        /* Deliberately NOT derived from --ink-rgb: it must stay dark
           regardless of theme. */
        --ink-scrim: rgba(39, 33, 24, 0.45);
      `),
    ).toEqual({ "ink-rgb": "36, 29, 21", "ink-scrim": "rgba(39, 33, 24, 0.45)" });
  });
});

// The cases below break a copy of the real stylesheet, so they need the real one
// to be sound to begin with. If it isn't, the checks above already say so, and
// these would only repeat it as noise.
function stylesheetIsSound(): boolean {
  try {
    return [
      darkCopyProblems(palettes),
      darkOnlyProblems(palettes),
      swappedProblems(palettes),
      colorSchemeProblems(css),
      schemeLeakProblems(stylesheets),
    ].every((problems) => problems.length === 0);
  } catch {
    return false;
  }
}

describe.skipIf(!stylesheetIsSound())("the theme structure checks", () => {
  // Each case breaks an in-memory copy of the real global.css (the file itself
  // is never touched) and expects the check that owns that mistake to name it,
  // and only it.

  it("notice a token missing from one dark copy", () => {
    const broken = editBlock(css, OS_DARK, (body) => body.replace(/^\s*--ink-faint:[^;]*;/m, ""));
    expect(darkCopyProblems(extractPalettes(broken))).toEqual([
      `--ink-faint differs between the dark copies: pinned ${palettes.pinnedDark["ink-faint"]}, ` +
        "OS (missing)",
    ]);
  });

  it("notice a value changed in one dark copy", () => {
    // --ink-soft drifting to the dimmer --ink-faint in one copy only.
    const dimmer = palettes.pinnedDark["ink-faint"];
    const broken = editBlock(css, PINNED_DARK, (body) =>
      body.replace(/--ink-soft: [^;]+;/, `--ink-soft: ${dimmer};`),
    );
    expect(darkCopyProblems(extractPalettes(broken))).toEqual([
      `--ink-soft differs between the dark copies: pinned ${dimmer}, ` +
        `OS ${palettes.osDark["ink-soft"]}`,
    ]);
  });

  it("notice a dark token that light doesn't define", () => {
    // Misspelled in both copies, so the copies still agree with each other.
    const typo = (body: string) => body.replace("--paper-sunken:", "--paper-sunkn:");
    const broken = editBlock(editBlock(css, PINNED_DARK, typo), OS_DARK, typo);
    const mutated = extractPalettes(broken);
    expect(darkCopyProblems(mutated)).toEqual([]);
    expect(darkOnlyProblems(mutated)).toEqual(["--paper-sunkn is set for dark but not for light"]);
  });

  it("notice light and dark swapped", () => {
    const lightBody = extractBlock(css, LIGHT_PALETTE);
    const darkBody = extractBlock(css, PINNED_DARK);
    let broken = editBlock(css, LIGHT_PALETTE, () => darkBody);
    broken = editBlock(broken, PINNED_DARK, () => lightBody);
    broken = editBlock(broken, OS_DARK, () => lightBody);
    expect(swappedProblems(extractPalettes(broken))).toEqual([
      "the pinned dark copy's --paper is not darker than light's: swapped palettes",
      "the OS dark copy's --paper is not darker than light's: swapped palettes",
    ]);
  });

  it.each<[what: string, mutate: (text: string) => string, problems: string[]]>([
    [
      "dropped from the pinned dark rule",
      (text) => editBlock(text, PINNED_DARK, (body) => body.replace(/color-scheme:\s*dark;/, "")),
      [':root[data-theme="dark"] must set color-scheme: dark (found none)'],
    ],
    [
      "commented out of the pinned dark rule",
      (text) =>
        editBlock(text, PINNED_DARK, (body) =>
          body.replace("color-scheme: dark;", "/* color-scheme: dark; */"),
        ),
      [':root[data-theme="dark"] must set color-scheme: dark (found none)'],
    ],
    [
      "widened on the pinned light rule",
      (text) => editBlock(text, PINNED_LIGHT, (body) => body.replace("light;", "light dark;")),
      [':root[data-theme="light"] must set color-scheme: light (found light dark)'],
    ],
    [
      "swapped between the pinned rules",
      (text) =>
        editBlock(
          editBlock(text, PINNED_LIGHT, (body) => body.replace("light;", "dark;")),
          PINNED_DARK,
          (body) => body.replace("color-scheme: dark;", "color-scheme: light;"),
        ),
      [
        ':root[data-theme="light"] must set color-scheme: light (found dark)',
        ':root[data-theme="dark"] must set color-scheme: dark (found light)',
      ],
    ],
    [
      "added to the OS copy",
      (text) => editBlock(text, OS_DARK, (body) => `${body}color-scheme: dark;`),
      ["the prefers-color-scheme copy must not set color-scheme (found dark)"],
    ],
  ])("notice color-scheme %s", (_what, mutate, problems) => {
    expect(colorSchemeProblems(mutate(css))).toEqual(problems);
  });

  it.each<[what: string, sheets: Record<string, string>, problems: string[]]>([
    [
      "a prefers-color-scheme rule in another stylesheet",
      {
        ...stylesheets,
        "extra.css": ".a { margin: 0 }\n@media (prefers-color-scheme: dark) { .a { margin: 1px } }",
      },
      ["extra.css: prefers-color-scheme appears 1 time(s), expected 0"],
    ],
    [
      "light-dark() in another stylesheet",
      { ...stylesheets, "extra.css": ".a { color: light-dark(var(--ink), var(--paper)) }" },
      ["extra.css: uses light-dark()"],
    ],
    [
      "a second prefers-color-scheme rule in global.css",
      {
        ...stylesheets,
        "global.css": `${css}\n@media (prefers-color-scheme: dark) { .a { display: none } }`,
      },
      ["global.css: prefers-color-scheme appears 2 time(s), expected 1"],
    ],
  ])("notice %s", (_what, sheets, problems) => {
    expect(schemeLeakProblems(sheets)).toEqual(problems);
  });

  it("don't count a comment that only talks about the OS colour scheme", () => {
    const sheets = {
      ...stylesheets,
      "notes.css": "/* never prefers-color-scheme or light-dark() here */\n.a { display: none }",
    };
    expect(schemeLeakProblems(sheets)).toEqual([]);
  });

  it("skip a palette that is commented out", () => {
    const stale = "/*\n:root {\n  --paper: oops;\n}\n*/\n";
    expect(extractPalettes(stale + css)).toEqual(palettes);
  });

  it("make the extraction throw when the OS copy stops excluding a pinned Light", () => {
    const broken = css.replace(':root:not([data-theme="light"])', ":root");
    expect(() => extractPalettes(broken)).toThrow(/Could not find block matching/);
  });

  it("make the extraction throw when a pinned rule moves off :root", () => {
    const broken = css.replace(':root[data-theme="dark"]', 'html[data-theme="dark"]');
    expect(() => extractPalettes(broken)).toThrow(/Could not find block matching/);
  });

  it("accept the same rules written without quotes, as the minifier emits them", () => {
    const unquoted = css.replaceAll('"light"', "light").replaceAll('"dark"', "dark");
    expect(unquoted).not.toBe(css);
    expect(extractPalettes(unquoted)).toEqual(palettes);
    expect(colorSchemeProblems(unquoted)).toEqual([]);
  });
});
