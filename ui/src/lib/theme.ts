import type { Theme } from "./types";

/** The theme choices in the order every picker lists them. */
export const THEMES: readonly Theme[] = ["system", "light", "dark"];

/** What a stored value means. Anything that isn't an explicit "light" or
 * "dark" — "system", a settings row saved before the setting existed, the
 * loose e2e fixture, a value a newer build wrote — means follow the OS. */
export function normalizeTheme(value: unknown): Theme {
  return value === "light" || value === "dark" ? value : "system";
}

/** Mirrors the chosen theme onto `<html data-theme>`, which `global.css` keys
 * the pinned light and dark palettes (and their `color-scheme`) on. "system"
 * REMOVES the attribute rather than writing a resolved value: with no
 * attribute the palette is picked by `@media (prefers-color-scheme: dark)`
 * alone, so System keeps following the OS live with no JS listener.
 *
 * It has to be `<html>`, not `#root`: dialogs and popovers portal into
 * `document.body`, and `--focus-ring` and `--glow-gradient` resolve on `:root`.
 *
 * Total on purpose. It runs from a zustand listener inside `set()`, and zustand
 * doesn't catch a throwing listener — one would abort React's own notification
 * and leave the app on the bare titlebar. */
export function applyTheme(value: unknown): void {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  if (!root) return;
  const theme = normalizeTheme(value);
  if (theme === "system") {
    if (root.hasAttribute("data-theme")) root.removeAttribute("data-theme");
  } else if (root.getAttribute("data-theme") !== theme) {
    root.setAttribute("data-theme", theme);
  }
}
