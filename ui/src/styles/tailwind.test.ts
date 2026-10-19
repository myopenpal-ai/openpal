import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

// tailwind.css points shadcn's colors at global.css's palette tokens with
// `@theme inline`. Nothing else checks that those names exist, or that muted
// text still uses the one grey (--ink-soft) contrast.test.ts holds to 4.5:1.
const dir = dirname(fileURLToPath(import.meta.url));
const strip = (s: string) => s.replace(/\/\*[\s\S]*?\*\//g, "");
const tailwind = strip(readFileSync(join(dir, "tailwind.css"), "utf-8"));
const global = strip(readFileSync(join(dir, "global.css"), "utf-8"));

const lightTokens = new Set(
  [...(global.match(/^:root \{([^}]*)\}/m)?.[1] ?? "").matchAll(/--([\w-]+):/g)].map((m) => m[1]),
);
const themeBlock = tailwind.match(/@theme inline \{([^}]*)\}/)?.[1] ?? "";
const mapped = [...themeBlock.matchAll(/--([\w-]+):\s*var\(--([\w-]+)\)\s*;/g)].map(
  (m) => [m[1], m[2]] as const,
);

describe("tailwind.css theme mapping", () => {
  it("maps something (the regex found the @theme inline block)", () => {
    expect(mapped.length).toBeGreaterThan(0);
  });

  it.each(mapped)("%s reads an existing palette token, --%s", (_shadcn, token) => {
    expect(lightTokens.has(token)).toBe(true);
  });

  it("keeps muted text on --ink-soft, the only grey cleared for body text", () => {
    expect(themeBlock).toMatch(/--color-muted-foreground:\s*var\(--ink-soft\)\s*;/);
  });

  it("brings in no preflight and no automatic source scanning", () => {
    expect(tailwind).not.toMatch(/@import\s+["']tailwindcss["']/);
    expect(tailwind).not.toMatch(/preflight/);
    expect(tailwind).toMatch(/utilities\.css"\s+layer\(utilities\)\s+source\(none\)/);
  });
});
