import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { contrastRatio } from "../lib/contrast";

// Reads the live tokens out of global.css itself (rather than duplicating
// their values here) so this test actually regresses if someone edits a
// token without re-checking contrast. See docs/design/ACCESSIBILITY_AUDIT.md
// pattern 1 and PR-1 of docs/design/ACCESSIBILITY_PLAN.md.
const cssPath = join(dirname(fileURLToPath(import.meta.url)), "global.css");
const css = readFileSync(cssPath, "utf-8");

function extractBlock(source: string, pattern: RegExp): string {
  const match = source.match(pattern);
  if (!match) throw new Error(`Could not find block matching ${pattern}`);
  return match[1];
}

function parseTokens(block: string): Record<string, string> {
  const tokens: Record<string, string> = {};
  for (const m of block.matchAll(/--([\w-]+):\s*([^;]+);/g)) {
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

const lightBlock = extractBlock(css, /:root \{([^}]*)\}/);
const darkBlock = extractBlock(
  css,
  /@media \(prefers-color-scheme: dark\) \{\s*:root \{([^}]*)\}/,
);

describe.each([
  ["light", parseTokens(lightBlock)],
  ["dark", parseTokens(darkBlock)],
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
