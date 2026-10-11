import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import i18next from "i18next";
import "../i18n";
import en from "../i18n/locales/en.json";
import type { AppInfo, PluginInfo } from "./types";
import {
  foldForSearch,
  searchScore,
  searchSettings,
  SETTINGS_SEARCH_ENTRIES,
  type SettingsSearchContext,
} from "./settingsSearch";

const t = (key: string) => i18next.t(key as never);

const desktop: SettingsSearchContext = {
  signedIn: false,
  plugins: [],
  appInfo: { portable: false, isAndroid: false } as AppInfo,
};

const labels = (query: string, ctx = desktop) =>
  searchSettings(query, ctx, t).map((hit) => hit.label);

describe("searchScore", () => {
  it("finds every word of the query anywhere in the label, in any order", () => {
    expect(searchScore("thinking", "Show model thinking")).toBe(2);
    expect(searchScore("thinking show", "Show model thinking")).toBe(2);
    expect(searchScore("  THINK  ", "Show model thinking")).toBe(2);
    expect(searchScore("thinking hide", "Show model thinking")).toBe(0);
  });

  it("ignores accents and case, both ways", () => {
    expect(foldForSearch("Réglages")).toBe("reglages");
    expect(searchScore("reglages", "Réglages")).toBe(2);
    expect(searchScore("RÉGL", "reglages")).toBe(2);
  });

  it("falls back to words starting the hint, ranked below the label", () => {
    expect(searchScore("pin", "App lock", "Ask for a PIN whenever OpenPal starts.")).toBe(1);
    expect(searchScore("lock pin", "App lock", "Ask for a PIN whenever OpenPal starts.")).toBe(1);
    // Only word starts count in a hint, and only for 3+ letters.
    expect(searchScore("ever", "App lock", "Ask for a PIN whenever OpenPal starts.")).toBe(0);
    expect(searchScore("as", "App lock", "Ask for a PIN whenever OpenPal starts.")).toBe(0);
  });

  it("finds nothing for an empty query", () => {
    expect(searchScore("", "Language")).toBe(0);
    expect(searchScore("   ", "Language")).toBe(0);
  });
});

describe("searchSettings", () => {
  it("lists label matches before hint matches, each in nav order", () => {
    const hits = searchSettings("pin", desktop, t);
    expect(hits[0]).toMatchObject({ section: "privacy", label: "App lock" });
    expect(labels("updates")).toEqual(["Updates", "OpenPal updates", "AI engine updates"]);
  });

  it("points a setting that may be hidden at where it lives meanwhile", () => {
    const [hit] = searchSettings("default model", desktop, t);
    expect(hit).toEqual({
      section: "model",
      label: "Default model at startup",
      reveal: ["Default model at startup", "Load model on startup"],
    });
  });

  it("only offers what can show here", () => {
    expect(labels("account")).toEqual([]);
    expect(labels("account", { ...desktop, signedIn: true })).toEqual(["Account"]);

    expect(labels("speaking speed")).toEqual([]);
    const speech = { id: "speech" } as PluginInfo;
    expect(labels("speaking speed", { ...desktop, plugins: [speech] })).toEqual([
      "Speaking speed",
    ]);

    expect(labels("remote pairing")).toEqual([]);
    const android = { portable: false, isAndroid: true } as AppInfo;
    expect(labels("remote pairing", { ...desktop, appInfo: android })).toEqual(["Remote pairing"]);
  });

  it("searches in the app's language", async () => {
    await i18next.changeLanguage("fr");
    try {
      expect(labels("verrouillage")).toContain(t("appLock.settings.summary"));
    } finally {
      await i18next.changeLanguage("en");
    }
  });
});

describe("SETTINGS_SEARCH_ENTRIES", () => {
  it("only names keys that exist", () => {
    const keys = SETTINGS_SEARCH_ENTRIES.flatMap((e) => [
      e.label,
      ...("hint" in e ? [e.hint] : []),
      ...("fallback" in e ? [e.fallback] : []),
    ]);
    for (const key of keys) expect(i18next.exists(key), key).toBe(true);
  });

  // A setting added to SettingsSheet should be findable. Only keys from the
  // main namespace are checked — the Remote sections' own labels can't be
  // told apart from other namespaces' keys by the source alone.
  it("covers every label and group title Settings renders", () => {
    const source = readFileSync(
      join(dirname(fileURLToPath(import.meta.url)), "../views/SettingsSheet.tsx"),
      "utf-8",
    );
    const rendered = [
      ...source.matchAll(/className="settings-label">\s*\{t\("([^"]+)"/g),
      ...source.matchAll(/SettingsGroup title=\{t\("([^"]+)"/g),
    ].map((m) => m[1]);
    const inMainNamespace = (key: string) =>
      key.split(".").reduce<unknown>((o, part) => (o as Record<string, unknown>)?.[part], en) !==
      undefined;
    // Parts of a setting already listed, never worth a result of their own.
    const leftOut = [
      "settingsSheet.autoUnload.durationLabel", // the slider under Smart model loading
      "settingsSheet.sync.create.doneLabel", // only right after creating a sync group
    ];
    const listed = new Set<string>(SETTINGS_SEARCH_ENTRIES.map((e) => e.label));

    expect(rendered.length).toBeGreaterThan(30);
    const missing = rendered.filter(
      (key) => inMainNamespace(key) && !listed.has(key) && !leftOut.includes(key),
    );
    expect(missing, "add these to SETTINGS_SEARCH_ENTRIES (or to leftOut)").toEqual([]);
  });
});
