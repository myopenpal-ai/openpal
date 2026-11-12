import i18next from "i18next";
import { initReactI18next } from "react-i18next";
import en from "./locales/en.json";
import fr from "./locales/fr.json";
import enRecovery from "./locales/en.recovery.json";
import frRecovery from "./locales/fr.recovery.json";
import enRemoteAccess from "./locales/en.remoteAccess.json";
import frRemoteAccess from "./locales/fr.remoteAccess.json";
import enRemoteClient from "./locales/en.remoteClient.json";
import frRemoteClient from "./locales/fr.remoteClient.json";
import enRemoteInference from "./locales/en.remoteInference.json";
import frRemoteInference from "./locales/fr.remoteInference.json";
import enInjection from "./locales/en.injection.json";
import frInjection from "./locales/fr.injection.json";

/** The only two UI languages OpenPal ships. Single source of truth for
 * anything that needs to enumerate or validate them. */
export const SUPPORTED_LANGUAGES = ["en", "fr"] as const;

void i18next.use(initReactI18next).init({
  resources: {
    // `recovery` is a separate namespace, not folded into `translation` —
    // RecoverySetup.tsx's key set pushed TypeScript's key-union type over
    // its "excessively deep" instantiation limit when merged into the main
    // resource (surfaced as a compile error in unrelated files, e.g.
    // ToolConsentCard.tsx). It keeps its own, separately type-checked key
    // union instead of inflating the shared one. See `i18next.d.ts` and
    // `keys.test.ts`, which cover it the same way. `remoteAccess`
    // (RemoteAccessSection, `SettingsSheet.tsx`) hit the exact same limit
    // when its ~25 keys were added directly to `translation` and follows
    // the same fix. `remoteClient` (RemoteClientSection, Android-only)
    // follows the same precedent from the start rather than waiting to
    // hit the limit, and so does `injection` (the prompt-injection review
    // card and notices, InjectionReviewCard/InjectionNotices).
    en: {
      translation: en,
      recovery: enRecovery,
      remoteAccess: enRemoteAccess,
      remoteClient: enRemoteClient,
      remoteInference: enRemoteInference,
      injection: enInjection,
    },
    fr: {
      translation: fr,
      recovery: frRecovery,
      remoteAccess: frRemoteAccess,
      remoteClient: frRemoteClient,
      remoteInference: frRemoteInference,
      injection: frInjection,
    },
  },
  lng: "en",
  fallbackLng: "en",
  interpolation: {
    // React already escapes interpolated values — double-escaping would
    // corrupt anything containing HTML-significant characters.
    escapeValue: false,
  },
  // Resources are statically bundled above, so init can resolve synchronously
  // rather than waiting on the (here, nonexistent) async backend/detector
  // pipeline. `initImmediate` was i18next's pre-v24 name for this flag; v26
  // renamed it to `initAsync` (inverted: `initAsync: false` == old
  // `initImmediate: false`) — see i18next's InitOptions type.
  initAsync: false,
});

// ui/index.html hardcodes lang="en" as a static default before any script
// runs; keep the live document in sync with whatever language is active.
i18next.on("languageChanged", (lng) => {
  document.documentElement.lang = lng;
});

export default i18next;
