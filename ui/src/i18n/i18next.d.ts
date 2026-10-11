import "i18next";
import en from "./locales/en.json";
import enRecovery from "./locales/en.recovery.json";
import enRemoteAccess from "./locales/en.remoteAccess.json";
import enRemoteClient from "./locales/en.remoteClient.json";

// Standard i18next + TypeScript module augmentation: ties CustomTypeOptions
// to en.json's shape so every `t("...")` call is checked against its key
// paths at compile time. en.json (not fr.json) is the source of truth —
// both files carry the exact same key paths, enforced by a dedicated test.
//
// `recovery`/`remoteAccess` are declared as their own namespaces, not
// merged into `translation` — see the comment in `i18n/index.ts` for why
// (merging pushed TypeScript's key-union type past its "excessively deep"
// instantiation limit).
declare module "i18next" {
  interface CustomTypeOptions {
    defaultNS: "translation";
    resources: {
      translation: typeof en;
      recovery: typeof enRecovery;
      remoteAccess: typeof enRemoteAccess;
      remoteClient: typeof enRemoteClient;
    };
  }
}
