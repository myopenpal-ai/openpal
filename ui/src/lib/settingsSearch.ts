import { hasNativeNotifications } from "./appEvents";
import type { AppInfo, PluginInfo } from "./types";
import type { SettingsSection } from "../state/store";

/** What decides whether a searchable setting can show at all. */
export interface SettingsSearchContext {
  signedIn: boolean;
  plugins: PluginInfo[];
  appInfo: AppInfo | null;
}

interface SettingsSearchEntry<Key extends string = string> {
  section: SettingsSection;
  /** The key of a field label or group title, exactly as SettingsSheet
   * renders it — picking the result finds that text on the page. */
  label: Key;
  /** Its hint, whose words a query can find too ("pin" finds App lock). */
  hint?: Key;
  /** Where to land instead while `label` isn't showing — a field that only
   * appears while another setting is on. */
  fallback?: Key;
  /** False where the whole thing is hidden, so it isn't offered at all. */
  when?: (ctx: SettingsSearchContext) => boolean;
}

const hasVoice = ({ plugins }: SettingsSearchContext) => plugins.some((p) => p.id === "speech");
/** A build with no OS notifications (iOS) has no such group in Settings. */
const showsNotifications = ({ appInfo }: SettingsSearchContext) => hasNativeNotifications(appInfo);
/** Android words the same switch for a phone (the `_mobile` keys). */
const onAndroid = ({ appInfo }: SettingsSearchContext) => appInfo?.isAndroid ?? false;
/** The Remote inference section only shows on desktop, and only while the
 * `remote-inference` add-on is on (see `RemoteInferenceSection`). The
 * capability read is defensive — settingsSearch's tests build minimal
 * plugins (an `id` alone), and `remoteInferenceAvailable` expects the full
 * shape that real plugin state always carries. */
const hasRemoteInference = ({ plugins, appInfo }: SettingsSearchContext) =>
  !(appInfo?.isAndroid ?? false) &&
  plugins.some((p) => p.capabilities?.includes("remote-inference") && p.enabled);

/**
 * Everything the More dialog's search can find inside Settings, in nav
 * order, mirroring views/SettingsSheet.tsx. A setting isn't findable until
 * it's listed here; settingsSearch.test.ts fails on any label or group title
 * SettingsSheet renders that's neither listed nor deliberately left out.
 */
export const SETTINGS_SEARCH_ENTRIES = [
  { section: "general", label: "settingsSheet.account.label", when: (c) => c.signedIn },
  { section: "general", label: "settingsSheet.appearance.title" },
  {
    section: "general",
    label: "settingsSheet.appearance.theme.label",
    hint: "settingsSheet.appearance.theme.hint",
  },
  { section: "general", label: "settingsSheet.language.label" },
  { section: "general", label: "settingsSheet.notifications.title", when: showsNotifications },
  {
    section: "general",
    label: "settingsSheet.notifications.native.label",
    hint: "settingsSheet.notifications.native.hint",
    when: (c) => showsNotifications(c) && !onAndroid(c),
  },
  {
    section: "general",
    label: "settingsSheet.notifications.native.label_mobile",
    hint: "settingsSheet.notifications.native.hint_mobile",
    when: (c) => showsNotifications(c) && onAndroid(c),
  },
  { section: "general", label: "settingsSheet.updates.title" },
  { section: "general", label: "settingsSheet.diagnostics.updates.label" },
  {
    section: "general",
    label: "settingsSheet.diagnostics.engineUpdates.label",
    hint: "settingsSheet.diagnostics.engineUpdates.hint",
  },

  {
    section: "chat",
    label: "settingsSheet.userName.label",
    hint: "settingsSheet.userName.hint",
  },
  {
    section: "chat",
    label: "settingsSheet.personality.label",
    hint: "settingsSheet.personality.hint",
  },
  {
    section: "chat",
    label: "settingsSheet.showThinking.label",
    hint: "settingsSheet.showThinking.hint",
  },
  {
    section: "chat",
    label: "settingsSheet.generationStats.label",
    hint: "settingsSheet.generationStats.hint",
  },
  { section: "chat", label: "settingsSheet.triage.summary", hint: "settingsSheet.triage.hint" },
  {
    section: "chat",
    label: "settingsSheet.voice.title",
    hint: "settingsSheet.voice.subtitle",
    when: hasVoice,
  },
  {
    section: "chat",
    label: "settingsSheet.voice.pickerLabel",
    hint: "settingsSheet.voice.pickerHint",
    fallback: "settingsSheet.voice.title",
    when: hasVoice,
  },
  { section: "chat", label: "settingsSheet.voice.speed.label", when: hasVoice },
  { section: "chat", label: "settingsSheet.voice.liveliness.label", when: hasVoice },
  { section: "chat", label: "settingsSheet.voice.pause.label", when: hasVoice },

  {
    section: "model",
    label: "settingsSheet.autoUnload.label",
    hint: "settingsSheet.autoUnload.hint",
  },
  {
    section: "model",
    label: "settingsSheet.autoLoadStartup.label",
    hint: "settingsSheet.autoLoadStartup.hint",
  },
  {
    section: "model",
    label: "settingsSheet.defaultModel.label",
    hint: "settingsSheet.defaultModel.hint",
    fallback: "settingsSheet.autoLoadStartup.label",
  },
  {
    section: "model",
    label: "settingsSheet.customModels.label",
    hint: "settingsSheet.customModels.hint",
  },
  { section: "model", label: "settingsSheet.performance.title" },
  {
    section: "model",
    label: "settingsSheet.diagnostics.forceCpu.label",
    hint: "settingsSheet.diagnostics.forceCpu.hint",
  },
  {
    section: "model",
    label: "settingsSheet.diagnostics.diskOffload.label",
    hint: "settingsSheet.diagnostics.diskOffload.hint",
  },
  {
    section: "model",
    label: "settingsSheet.diagnostics.autoUnloadDiskSlow.label",
    fallback: "settingsSheet.diagnostics.diskOffload.label",
  },

  {
    section: "privacy",
    label: "settingsSheet.safetyDetection.summary",
    hint: "settingsSheet.safetyDetection.hint",
  },
  { section: "privacy", label: "appLock.settings.summary", hint: "appLock.settings.hint" },
  {
    section: "privacy",
    label: "settingsSheet.parentalControls.summary",
    hint: "settingsSheet.parentalControls.hint",
  },

  {
    section: "devices",
    label: "remoteClient:summary",
    hint: "remoteClient:hint",
    when: (c) => c.appInfo?.isAndroid ?? false,
  },
  {
    section: "devices",
    label: "remoteInference:pluginName",
    hint: "remoteInference:hint",
    when: (c) => hasRemoteInference(c),
  },
  { section: "devices", label: "settingsSheet.sync.summary" },
  {
    section: "devices",
    label: "settingsSheet.sync.create.label",
    hint: "settingsSheet.sync.create.hint",
    fallback: "settingsSheet.sync.summary",
  },
  {
    section: "devices",
    label: "settingsSheet.sync.join.label",
    hint: "settingsSheet.sync.join.hint",
    fallback: "settingsSheet.sync.summary",
  },
  {
    section: "devices",
    label: "settingsSheet.sync.devices.label",
    fallback: "settingsSheet.sync.summary",
  },
  {
    section: "devices",
    label: "settingsSheet.sync.dataSync.label",
    hint: "settingsSheet.sync.dataSync.hint",
  },
  {
    section: "devices",
    label: "settingsSheet.sync.dataSync.relayUrl.label",
    hint: "settingsSheet.sync.dataSync.relayUrl.hint",
    fallback: "settingsSheet.sync.dataSync.label",
  },
  { section: "devices", label: "remoteAccess:summary", hint: "remoteAccess:hint" },
  {
    section: "devices",
    label: "remoteAccess:pairing.label",
    hint: "remoteAccess:pairing.hint",
    fallback: "remoteAccess:summary",
  },
  { section: "devices", label: "remoteAccess:wol.label", fallback: "remoteAccess:summary" },

  {
    section: "data",
    label: "settingsSheet.diagnostics.dataDir.label",
    hint: "settingsSheet.diagnostics.dataDir.hint",
  },
  {
    section: "data",
    label: "settingsSheet.diagnostics.exportData.label",
    hint: "settingsSheet.diagnostics.exportData.hint",
  },
  {
    section: "data",
    label: "settingsSheet.diagnostics.importData.label",
    hint: "settingsSheet.diagnostics.importData.hint",
  },
  // Label only: its help text's "this long … on all your devices" would make
  // queries meant for the Notifications switch ("on this device") find it.
  { section: "data", label: "settingsSheet.autoDelete.label" },
  {
    section: "data",
    label: "settingsSheet.diagnostics.autoBackup.label",
    hint: "settingsSheet.diagnostics.autoBackup.hint",
    when: ({ appInfo }) => !appInfo?.portable && !appInfo?.isAndroid,
  },
  {
    section: "data",
    label: "appLock.settings.dangerZone.summary",
    hint: "appLock.settings.dangerZone.eraseButton",
  },

  {
    section: "advanced",
    label: "settingsSheet.agentMode.summary",
    hint: "settingsSheet.agentMode.hint",
  },
  {
    section: "advanced",
    label: "settingsSheet.agentMode.grants.label",
    fallback: "settingsSheet.agentMode.summary",
  },
  {
    section: "advanced",
    label: "settingsSheet.experimental.summary",
    hint: "settingsSheet.experimental.hint",
  },
  { section: "advanced", label: "settingsSheet.diagnostics.summary" },
  {
    section: "advanced",
    label: "settingsSheet.diagnostics.logLevel.label",
    hint: "settingsSheet.diagnostics.logLevel.hint",
  },
  {
    section: "advanced",
    label: "settingsSheet.diagnostics.logs.label",
    hint: "settingsSheet.diagnostics.logs.hint",
  },
] as const satisfies readonly SettingsSearchEntry[];

type Entry = (typeof SETTINGS_SEARCH_ENTRIES)[number];
/** Every translation key the search reads. */
export type SettingsSearchKey =
  | Entry["label"]
  | Extract<Entry, { hint: string }>["hint"]
  | Extract<Entry, { fallback: string }>["fallback"];

/** A setting the search found. */
export interface SettingsSearchHit {
  section: SettingsSection;
  label: string;
  /** The texts to look for on its page, in order: the setting's own label,
   * then where to land while that isn't showing. */
  reveal: string[];
}

/** The settings `query` finds, label matches first, each group in nav
 * order. */
export function searchSettings(
  query: string,
  ctx: SettingsSearchContext,
  t: (key: SettingsSearchKey) => string,
): SettingsSearchHit[] {
  const entries: readonly SettingsSearchEntry<SettingsSearchKey>[] = SETTINGS_SEARCH_ENTRIES;
  return entries
    .filter((e) => e.when?.(ctx) ?? true)
    .map((e) => {
      const label = t(e.label);
      return { e, label, score: searchScore(query, label, e.hint ? t(e.hint) : "") };
    })
    .filter((hit) => hit.score > 0)
    .sort((a, b) => b.score - a.score)
    .map(({ e, label }) => ({
      section: e.section,
      label,
      reveal: e.fallback ? [label, t(e.fallback)] : [label],
    }));
}

/** Lowercased with accents stripped, so "reglages" finds "Réglages". */
export function foldForSearch(text: string): string {
  return text.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();
}

/**
 * How well `query` finds something labelled `label`: 2 when every word of
 * the query is somewhere in the label, 1 when some are only found starting a
 * word of its `hint`, 0 when it doesn't match. The hint only counts for words
 * of 3+ letters — hints are whole sentences, and "a" or "to" would find
 * nearly every one.
 */
export function searchScore(query: string, label: string, hint = ""): number {
  const terms = foldForSearch(query).split(/\s+/).filter(Boolean);
  if (terms.length === 0) return 0;
  const folded = foldForSearch(label);
  const hintWords = foldForSearch(hint).split(/[^\p{L}\p{N}]+/u);
  let score = 2;
  for (const term of terms) {
    if (folded.includes(term)) continue;
    if (term.length >= 3 && hintWords.some((w) => w.startsWith(term))) {
      score = 1;
      continue;
    }
    return 0;
  }
  return score;
}
