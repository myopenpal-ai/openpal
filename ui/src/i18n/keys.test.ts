import { describe, expect, it } from "vitest";
import en from "./locales/en.json";
import fr from "./locales/fr.json";
import enRecovery from "./locales/en.recovery.json";
import frRecovery from "./locales/fr.recovery.json";
import enRemoteAccess from "./locales/en.remoteAccess.json";
import frRemoteAccess from "./locales/fr.remoteAccess.json";
import enRemoteClient from "./locales/en.remoteClient.json";
import frRemoteClient from "./locales/fr.remoteClient.json";
import enInjection from "./locales/en.injection.json";
import frInjection from "./locales/fr.injection.json";

/** Recursively flattens a locale JSON object into dot-path keys, e.g.
 * `{ a: { b: "x", c: "y" } }` -> `["a.b", "a.c"]`. Leaf values (translation
 * strings) terminate the recursion; anything else is assumed to be a nested
 * namespace object. */
function flattenKeys(obj: object, prefix = ""): string[] {
  return Object.entries(obj).flatMap(([key, value]) => {
    const path = prefix ? `${prefix}.${key}` : key;
    if (typeof value === "object" && value !== null && !Array.isArray(value)) {
      return flattenKeys(value, path);
    }
    return [path];
  });
}

// The TypeScript module augmentation in i18next.d.ts only checks `t("...")`
// calls against the English resource's shape — it can't catch the French
// side silently missing (or adding) a key, since it's never used as the
// CustomTypeOptions source of truth. This test is the only thing that
// guards the French key set, so it must compare the full set, not a sample.
describe.each([
  { label: "translation", en, fr },
  { label: "recovery", en: enRecovery, fr: frRecovery },
  { label: "remoteAccess", en: enRemoteAccess, fr: frRemoteAccess },
  { label: "remoteClient", en: enRemoteClient, fr: frRemoteClient },
  { label: "injection", en: enInjection, fr: frInjection },
])("locale key parity ($label)", ({ en, fr }) => {
  it("en and fr declare exactly the same set of keys", () => {
    const enKeys = new Set(flattenKeys(en));
    const frKeys = new Set(flattenKeys(fr));

    const missingFromFr = [...enKeys].filter((key) => !frKeys.has(key)).sort();
    const missingFromEn = [...frKeys].filter((key) => !enKeys.has(key)).sort();

    expect(missingFromFr, "keys present in en.json but missing from fr.json").toEqual([]);
    expect(missingFromEn, "keys present in fr.json but missing from en.json").toEqual([]);
  });
});
