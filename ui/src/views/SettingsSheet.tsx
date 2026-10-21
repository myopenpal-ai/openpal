import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import QRCode from "qrcode";
import ConfirmDialog from "../components/ConfirmDialog";
import FactoryResetConfirm from "../components/FactoryResetConfirm";
import PinPad from "../components/PinPad";
import ThemePicker from "../components/ThemePicker";
import { api } from "../lib/api";
import { hasNativeNotifications } from "../lib/appEvents";
import { formatBytesAtLeast, timeAgo } from "../lib/format";
import { usePluginDisplayName } from "../lib/pluginName";
import { normalizeTheme, THEMES } from "../lib/theme";
import type {
  AppLockStatusView,
  BackupFrequency,
  BackupState,
  DeviceSummary,
  DiscoveredHost,
  ExportCategories,
  ExportCounts,
  Language,
  PairedDeviceInfo,
  PairingPayload,
  ParentalSettingsView,
  RemoteAccessStatusInfo,
  RemotePairingRecord,
  Settings,
  ToolGrant,
  VoiceInfo,
  WolStatus,
} from "../lib/types";
import { experimentalFeaturesOn, useStore, type SettingsSection } from "../state/store";

/** The settings drafted locally and saved later — see `SettingsSheet`.
 * `userName` is drafted as the field's raw text (never null) and only turned
 * into a trimmed name, or none when it's blank, on the way out. */
type Drafts = Pick<
  Settings,
  "systemPrompt" | "autoUnloadMinutes" | "voiceSpeed" | "voiceExpressiveness" | "voiceSentencePause"
> & { userName: string };

interface DraftProps {
  drafts: Drafts;
  setDrafts: (patch: Partial<Drafts>) => void;
}

/** Settings' pages of the More dialog (views/MoreDialog.tsx) — one per
 * category, `section` being the one showing. The page body only; the dialog
 * around it owns the backdrop, focus trap, close and the nav between
 * categories. */
export default function SettingsSheet({ section }: { section: SettingsSection }) {
  const settings = useStore((s) => s.settings);
  const saveSettings = useStore((s) => s.saveSettings);
  const setMoreDialogPage = useStore((s) => s.setMoreDialogPage);
  const { t } = useTranslation();

  // The personality text, the name and the sliders are drafted locally and
  // saved when Settings goes away — the dialog closing, or switching to a
  // page that isn't one of its categories — rather than on every keystroke
  // or drag.
  // Held here, not in the category showing them, so a draft survives moving
  // between categories and they all go out in one save: two `saveSettings`
  // calls in flight at once each overwrite the other's patch. Read through a
  // ref so the unmount cleanup sees the latest values, not the first
  // render's. Defaults match the Rust Settings defaults, so an older
  // settings file (saved before these existed) reads as a fresh one would.
  const [drafts, setDraftsState] = useState<Drafts>(() => ({
    userName: settings?.userName ?? "",
    systemPrompt: settings?.systemPrompt ?? "",
    autoUnloadMinutes: settings?.autoUnloadMinutes ?? 15,
    voiceSpeed: settings?.voiceSpeed ?? 1,
    voiceExpressiveness: settings?.voiceExpressiveness ?? 1,
    voiceSentencePause: settings?.voiceSentencePause ?? 0.2,
  }));
  const setDrafts = (patch: Partial<Drafts>) => setDraftsState((d) => ({ ...d, ...patch }));
  const pendingSave = useRef(drafts);
  pendingSave.current = drafts;
  useEffect(
    () => () => {
      const { userName, ...rest } = pendingSave.current;
      // Same trim as onboarding's name step; a blank field means no name, so
      // the chat goes back to its generic greeting.
      void saveSettings({ ...rest, userName: userName.trim() || null });
    },
    [saveSettings],
  );

  return (
    // Keyed by category so each one opens scrolled to its top.
    <div className="settings-page" key={section}>
      <div className="settings-page-col">
        <header className="settings-page-head">
          <h1>{t(`settingsSheet.sections.${section}.label`)}</h1>
          <p className="settings-sub">{t(`settingsSheet.sections.${section}.subtitle`)}</p>
        </header>

        {section === "general" ? (
          <GeneralSettings />
        ) : section === "chat" ? (
          <ChatSettings drafts={drafts} setDrafts={setDrafts} />
        ) : section === "model" ? (
          <ModelSettings drafts={drafts} setDrafts={setDrafts} />
        ) : section === "privacy" ? (
          <PrivacySettings />
        ) : section === "devices" ? (
          <DevicesSettings />
        ) : section === "data" ? (
          <DataSettings />
        ) : (
          <AdvancedSettings />
        )}

        <div className="settings-footer">
          <button className="btn" onClick={() => setMoreDialogPage(null)}>
            {t("settingsSheet.done")}
          </button>
        </div>
      </div>
    </div>
  );
}

/** One headed group of related settings within a category. */
function SettingsGroup({ title, children }: { title: string; children: ReactNode }) {
  const titleId = useId();
  return (
    <section className="settings-group" aria-labelledby={titleId}>
      <h2 id={titleId} className="settings-group-title">
        {title}
      </h2>
      {children}
    </section>
  );
}

/** General: who's signed in, the app's appearance and language, notifications,
 * and updates. */
function GeneralSettings() {
  const settings = useStore((s) => s.settings);
  const saveSettings = useStore((s) => s.saveSettings);
  const { t, i18n } = useTranslation();

  return (
    <>
      <AccountSection />

      <AppearanceSection />

      <div className="settings-field">
        <span className="settings-label">{t("settingsSheet.language.label")}</span>
        <select
          className="settings-select"
          value={settings?.language ?? "en"}
          onChange={(e) => {
            const language = e.target.value as Language;
            void i18n.changeLanguage(language);
            void saveSettings({ language });
          }}
        >
          <option value="en">English</option>
          <option value="fr">Français</option>
        </select>
      </div>

      <NotificationsSection />

      <UpdatesSection />
    </>
  );
}

/** Chat: how the assistant talks, what a reply shows, and how it sounds
 * read aloud. */
function ChatSettings({ drafts, setDrafts }: DraftProps) {
  const settings = useStore((s) => s.settings);
  const saveSettings = useStore((s) => s.saveSettings);
  const setShowThinking = useStore((s) => s.setShowThinking);
  const { t } = useTranslation();
  // Defaults to on; older settings files (saved before this existed) have no
  // value, in which case thinking is on.
  const showThinking = settings?.showThinking ?? true;
  // Defaults to off; older settings files have no value.
  const showGenerationStats = settings?.showGenerationStats ?? false;
  const nameId = useId();
  const nameHintId = useId();

  return (
    <>
      {/* A `<label for>` rather than a wrapping one, so the field's name is
          just "Your name" and the hint is read as its description. */}
      <div className="settings-field">
        <label htmlFor={nameId} className="settings-label">
          {t("settingsSheet.userName.label")}
        </label>
        <span id={nameHintId} className="settings-hint">
          {t("settingsSheet.userName.hint")}
        </span>
        <input
          id={nameId}
          type="text"
          value={drafts.userName}
          onChange={(e) => setDrafts({ userName: e.target.value })}
          placeholder={t("settingsSheet.userName.placeholder")}
          aria-describedby={nameHintId}
          autoComplete="name"
          autoCapitalize="words"
          maxLength={80}
        />
      </div>

      <label className="settings-field">
        <span className="settings-label">{t("settingsSheet.personality.label")}</span>
        <span className="settings-hint">{t("settingsSheet.personality.hint")}</span>
        <textarea
          rows={4}
          value={drafts.systemPrompt}
          onChange={(e) => setDrafts({ systemPrompt: e.target.value })}
        />
      </label>

      <div className="settings-field">
        <span className="settings-label">{t("settingsSheet.showThinking.label")}</span>
        <span className="settings-hint">{t("settingsSheet.showThinking.hint")}</span>
        <label className="settings-toggle">
          <input
            type="checkbox"
            checked={showThinking}
            onChange={(e) => void setShowThinking(e.target.checked)}
          />
          <span>
            {t(showThinking ? "settingsSheet.toggle.on" : "settingsSheet.toggle.off")}
          </span>
        </label>
      </div>

      <div className="settings-field">
        <span className="settings-label">{t("settingsSheet.generationStats.label")}</span>
        <span className="settings-hint">{t("settingsSheet.generationStats.hint")}</span>
        <label className="settings-toggle">
          <input
            type="checkbox"
            checked={showGenerationStats}
            onChange={(e) =>
              void saveSettings({ showGenerationStats: e.target.checked })
            }
          />
          <span>
            {t(showGenerationStats ? "settingsSheet.toggle.on" : "settingsSheet.toggle.off")}
          </span>
        </label>
      </div>

      <TriageSection />

      <VoiceSection drafts={drafts} setDrafts={setDrafts} />
    </>
  );
}

/**
 * Read-aloud: how the Voice add-on sounds. Voice is an add-on, so with no
 * Voice plugin listed (it's experimental and that switch is off, or a build
 * without plugins) nothing here would work and the group isn't shown.
 */
function VoiceSection({ drafts, setDrafts }: DraftProps) {
  const saveSettings = useStore((s) => s.saveSettings);
  const { t } = useTranslation();
  // The Voice add-on powers read-aloud; its controls only do something once it's
  // enabled and set up. We still show them either way (so they can be tuned
  // ahead of time) but nudge the user to turn Voice on when it isn't ready.
  const speechPlugin = useStore((s) => s.plugins).find((p) => p.id === "speech");
  const voiceReady =
    !!speechPlugin?.enabled && speechPlugin.setup.type === "ready";
  const [testing, setTesting] = useState(false);
  const [voiceError, setVoiceError] = useState<string | null>(null);

  if (!speechPlugin) return null;

  const { voiceSpeed, voiceExpressiveness, voiceSentencePause } = drafts;

  // Play a short sample with the current sliders. Persist them first so the
  // native read-aloud (which reads from Settings) uses exactly what's shown.
  const testVoice = async () => {
    if (testing) return;
    setVoiceError(null);
    setTesting(true);
    try {
      await saveSettings({ voiceSpeed, voiceExpressiveness, voiceSentencePause });
      await api.speakText(t("settingsSheet.voice.sampleText"));
    } catch (err: unknown) {
      setVoiceError(String(err));
    } finally {
      setTesting(false);
    }
  };

  return (
    <SettingsGroup title={t("settingsSheet.voice.title")}>
      <p className="settings-hint settings-group-intro">
        {t("settingsSheet.voice.subtitle")}
        {!voiceReady && t("settingsSheet.voice.notReadyHint")}
      </p>

      {voiceReady && (
        <div className="settings-field">
          <span className="settings-label">{t("settingsSheet.voice.pickerLabel")}</span>
          <span className="settings-hint">{t("settingsSheet.voice.pickerHint")}</span>
          <VoicePicker />
        </div>
      )}

      <label className="settings-field">
        <span className="settings-label">{t("settingsSheet.voice.speed.label")}</span>
        <div className="settings-slider">
          <span>{t("settingsSheet.voice.speed.slow")}</span>
          <input
            type="range"
            aria-label={t("settingsSheet.voice.speed.label")}
            min={0.5}
            max={2}
            step={0.05}
            value={voiceSpeed}
            onChange={(e) => setDrafts({ voiceSpeed: Number(e.target.value) })}
          />
          <span>{t("settingsSheet.voice.speed.fast")}</span>
        </div>
      </label>

      <label className="settings-field">
        <span className="settings-label">{t("settingsSheet.voice.liveliness.label")}</span>
        <div className="settings-slider">
          <span>{t("settingsSheet.voice.liveliness.low")}</span>
          <input
            type="range"
            aria-label={t("settingsSheet.voice.liveliness.label")}
            min={0}
            max={2}
            step={0.05}
            value={voiceExpressiveness}
            onChange={(e) => setDrafts({ voiceExpressiveness: Number(e.target.value) })}
          />
          <span>{t("settingsSheet.voice.liveliness.high")}</span>
        </div>
      </label>

      <label className="settings-field">
        <span className="settings-label">{t("settingsSheet.voice.pause.label")}</span>
        <div className="settings-slider">
          <span>{t("settingsSheet.voice.pause.short")}</span>
          <input
            type="range"
            aria-label={t("settingsSheet.voice.pause.label")}
            min={0}
            max={1}
            step={0.05}
            value={voiceSentencePause}
            onChange={(e) => setDrafts({ voiceSentencePause: Number(e.target.value) })}
          />
          <span>{t("settingsSheet.voice.pause.long")}</span>
        </div>
      </label>

      <div className="settings-voice-test">
        <button
          className="btn-ghost"
          disabled={testing}
          onClick={() => void testVoice()}
        >
          {testing ? t("settingsSheet.voice.testing") : t("settingsSheet.voice.testButton")}
        </button>
        {voiceError && <p className="model-error" role="alert">{voiceError}</p>}
      </div>
    </SettingsGroup>
  );
}

/** AI model: when it loads and unloads, how it runs on this computer, and
 * bringing a model of your own. */
function ModelSettings({ drafts, setDrafts }: DraftProps) {
  const settings = useStore((s) => s.settings);
  const saveSettings = useStore((s) => s.saveSettings);
  const { t } = useTranslation();
  // Defaults to on, 15 minutes; older settings files have no value.
  const autoUnloadEnabled = settings?.autoUnloadEnabled ?? true;
  // Defaults to on; older settings files have no value.
  const autoLoadOnStartup = settings?.autoLoadOnStartup ?? true;
  // Off by default; older settings files have no value.
  const customModelsEnabled = settings?.customModelsEnabled ?? false;

  return (
    <>
      <div className="settings-field">
        <span className="settings-label">{t("settingsSheet.autoUnload.label")}</span>
        <span className="settings-hint">{t("settingsSheet.autoUnload.hint")}</span>
        <label className="settings-toggle">
          <input
            type="checkbox"
            checked={autoUnloadEnabled}
            onChange={(e) =>
              void saveSettings({ autoUnloadEnabled: e.target.checked })
            }
          />
          <span>
            {t(autoUnloadEnabled ? "settingsSheet.toggle.on" : "settingsSheet.toggle.off")}
          </span>
        </label>
      </div>

      {autoUnloadEnabled && (
        <label className="settings-field">
          <span className="settings-label">{t("settingsSheet.autoUnload.durationLabel")}</span>
          <div className="settings-slider">
            <span>{t("settingsSheet.autoUnload.min1")}</span>
            <input
              type="range"
              aria-label={t("settingsSheet.autoUnload.durationAriaLabel")}
              min={1}
              max={120}
              step={1}
              value={drafts.autoUnloadMinutes}
              onChange={(e) => setDrafts({ autoUnloadMinutes: Number(e.target.value) })}
            />
            <span>{t("settingsSheet.autoUnload.max2hr")}</span>
          </div>
          <span className="settings-hint">
            {t("settingsSheet.autoUnload.idleMinutes", { count: drafts.autoUnloadMinutes })}
          </span>
        </label>
      )}

      <div className="settings-field">
        <span className="settings-label">{t("settingsSheet.autoLoadStartup.label")}</span>
        <span className="settings-hint">{t("settingsSheet.autoLoadStartup.hint")}</span>
        <label className="settings-toggle">
          <input
            type="checkbox"
            checked={autoLoadOnStartup}
            onChange={(e) =>
              void saveSettings({ autoLoadOnStartup: e.target.checked })
            }
          />
          <span>
            {t(autoLoadOnStartup ? "settingsSheet.toggle.on" : "settingsSheet.toggle.off")}
          </span>
        </label>
      </div>

      {autoLoadOnStartup && <DefaultModelSection />}

      {/* The form itself sits at the bottom of the Models page
          (`AddYourOwnModel` in Library.tsx); this only shows or hides it. */}
      <div className="settings-field">
        <span className="settings-label">{t("settingsSheet.customModels.label")}</span>
        <span className="settings-hint">{t("settingsSheet.customModels.hint")}</span>
        <label className="settings-toggle">
          <input
            type="checkbox"
            checked={customModelsEnabled}
            onChange={(e) =>
              void saveSettings({ customModelsEnabled: e.target.checked })
            }
          />
          <span>
            {t(customModelsEnabled ? "settingsSheet.toggle.on" : "settingsSheet.toggle.off")}
          </span>
        </label>
      </div>

      <PerformanceSection />
    </>
  );
}

/** Privacy & safety: the crisis banner, locking the app, and limits for
 * children. */
function PrivacySettings() {
  return (
    <>
      <SafetyDetectionSection />
      <SecuritySection />
      <ParentalControlsSection />
    </>
  );
}

/**
 * Devices: keeping this device's conversations in step with the user's
 * others (Sync), and one device using another's AI — this computer serving
 * it (Remote access) or, on Android, this phone pairing with a computer
 * (Remote pairing). A phone can't chat at all until it's paired, so that
 * comes first there.
 */
function DevicesSettings() {
  const isAndroid = useStore((s) => s.appInfo?.isAndroid ?? false);
  return (
    <>
      {isAndroid && <RemoteClientSection />}
      <SyncSection />
      <RemoteAccessSection />
    </>
  );
}

/** Your data: where it lives, backing it up or bringing it over, and
 * erasing it. */
function DataSettings() {
  return (
    <>
      <YourDataSection />
      <AutoDeleteSection />
      <DangerZoneSection />
    </>
  );
}

/** Advanced: power-user switches nobody needs to use OpenPal. */
function AdvancedSettings() {
  return (
    <>
      <AgentModeSection />
      <ExperimentalSection />
      <LogsSection />
    </>
  );
}

/**
 * Sign in / plan status. The first thing Settings shows (top of General) —
 * this is the monetization surface, so it stays in view rather than being
 * buried in a category of its own.
 */
function AccountSection() {
  const { t } = useTranslation();
  const account = useStore((s) => s.account);
  const signOut = useStore((s) => s.signOut);

  // Sign in isn't plugged into a working backend yet, so the entry point to
  // start one is hidden for now — an existing, already-signed-in session
  // (account truthy) still shows normally, with sign-out still available.
  if (!account) return null;

  return (
    <div className="settings-field">
      <span className="settings-label">{t("settingsSheet.account.label")}</span>
      <span className="settings-hint">
        {t("settingsSheet.account.signedInAs", {
          email: account.email,
          tier: account.tier,
        })}
        {account.quota &&
          ` ${t("settingsSheet.account.quotaUsed", {
            used: account.quota.used,
            limit: account.quota.limit,
          })}`}
      </span>
      <div className="logs-actions">
        <button className="btn-ghost" onClick={() => void signOut()}>
          {t("settingsSheet.account.signOut")}
        </button>
      </div>
    </div>
  );
}

/**
 * Theme: light, dark, or follow the device. Like Language it applies at once,
 * with no draft and no Save, so it isn't one of the `Drafts` saved when
 * Settings closes. The pick goes through the store's `setTheme`, which
 * re-themes the whole app before it persists anything.
 */
function AppearanceSection() {
  const { t } = useTranslation();
  const setTheme = useStore((s) => s.setTheme);
  // Older settings files (saved before this existed) have no value, which
  // reads as System, the same as on a fresh install.
  const theme = normalizeTheme(useStore((s) => s.settings?.theme));
  const labelId = useId();
  const hintId = useId();

  return (
    <SettingsGroup title={t("settingsSheet.appearance.title")}>
      {/* The visible label names the group and the hint describes it, so a
          screen reader says both before it reads the options. The options'
          own names aren't `settings-label`s: settingsSearch.test.ts would
          want each one listed as a setting of its own. */}
      <div className="settings-field">
        <span id={labelId} className="settings-label">
          {t("settingsSheet.appearance.theme.label")}
        </span>
        <span id={hintId} className="settings-hint">
          {t("settingsSheet.appearance.theme.hint")}
        </span>
        <ThemePicker
          variant="segmented"
          value={theme}
          onChange={(next) => void setTheme(next)}
          labelledBy={labelId}
          describedBy={hintId}
          options={THEMES.map((value) => ({
            value,
            label: t(`settingsSheet.appearance.theme.${value}`),
          }))}
        />
      </div>
    </SettingsGroup>
  );
}

/**
 * "Desktop notifications": whether the four model/answer alerts are also sent
 * as OS notifications while OpenPal is in the background. Like the theme it
 * applies at once, with no draft and no Save, so it isn't one of the `Drafts`:
 * the pick goes through the store's `setDesktopNotifications`, which persists it
 * and, when turning it on, asks the OS for permission (on macOS: its one-time
 * prompt). Opening this page only reads the OS's answer and never prompts.
 *
 * The "blocked" line is a plain hint, not an alert, and the switch stays
 * enabled either way: the user may well be about to fix it in system settings,
 * and Settings search focuses only an enabled control. It shows only while the
 * switch is on, since a user who never asked for notifications has nothing to be
 * told is blocked. Because the OS never asks twice, the line says what to do
 * and carries a button that opens the OS page where "Allow notifications" is;
 * if that page can't be opened, the line says where to find it instead.
 *
 * A build with no OS notifications at all (`AppInfo.nativeNotifications`:
 * Android and iOS today) leaves the group out, and settingsSearch.ts doesn't
 * offer it either.
 */
function NotificationsSection() {
  const { t } = useTranslation();
  const supported = useStore((s) => hasNativeNotifications(s.appInfo));
  // A phone or tablet has no desktop, and nothing about models to say: the
  // same switch, in words for Android.
  const context = useStore((s) => (s.appInfo?.isAndroid ? "mobile" : undefined));
  // A settings row saved before this existed has no value, which reads as off,
  // the same as on a fresh install.
  const on = useStore((s) => s.settings?.desktopNotifications === true);
  const permission = useStore((s) => s.notificationPermission);
  const setDesktopNotifications = useStore((s) => s.setDesktopNotifications);
  const refreshNotificationPermission = useStore((s) => s.refreshNotificationPermission);
  const labelId = useId();
  const hintId = useId();
  const blockedId = useId();
  const [openFailed, setOpenFailed] = useState(false);

  // The user may have changed the OS setting since the app last asked, so read
  // it when this page opens and again whenever the window regains focus, which
  // is where they come back to after visiting system settings. The store's read
  // never throws and keeps the last answer when it can't tell.
  useEffect(() => {
    if (!supported) return;
    void refreshNotificationPermission();
    const onFocus = () => void refreshNotificationPermission();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [supported, refreshNotificationPermission]);

  if (!supported) return null;
  const blocked = on && permission === "denied";

  const openSettings = async () => {
    setOpenFailed(false);
    try {
      await api.openNotificationSettings();
    } catch {
      setOpenFailed(true);
    }
  };

  return (
    <SettingsGroup title={t("settingsSheet.notifications.title")}>
      {/* Named by the visible label, not by the toggle's "On"/"Off" text, so a
          screen reader says what the switch is for; the hint, and the blocked
          line while it shows, describe it. */}
      <div className="settings-field">
        <span id={labelId} className="settings-label">
          {t("settingsSheet.notifications.native.label", { context })}
        </span>
        <span id={hintId} className="settings-hint">
          {t("settingsSheet.notifications.native.hint", { context })}
        </span>
        <label className="settings-toggle">
          <input
            type="checkbox"
            checked={on}
            onChange={(e) => void setDesktopNotifications(e.target.checked)}
            aria-labelledby={labelId}
            aria-describedby={blocked ? `${hintId} ${blockedId}` : hintId}
          />
          <span aria-hidden="true">
            {t(on ? "settingsSheet.toggle.on" : "settingsSheet.toggle.off")}
          </span>
        </label>
        {/* Always in the DOM, empty until the OS says no: a status message is
            only announced when it is added to a region that already exists, and
            the "blocked" answer arrives a moment after the switch is turned on. */}
        <span id={blockedId} className="settings-hint settings-status" role="status">
          {blocked ? t("settingsSheet.notifications.native.blocked") : null}
          {blocked && openFailed ? ` ${t("settingsSheet.notifications.native.openFailed")}` : null}
        </span>
        {/* Outside the status region, which is read out as text: a button in it
            would be announced with every change. A sibling of the switch, so
            Tab goes switch -> button. */}
        {blocked && (
          <div className="settings-notification-help">
            <button className="btn-ghost" onClick={() => void openSettings()}>
              {t("settingsSheet.notifications.native.openSettings")}
            </button>
          </div>
        )}
      </div>
    </SettingsGroup>
  );
}

/**
 * Which model OpenPal wakes up with. Defaults to resuming whatever was last
 * used (today's implicit behavior, unchanged unless this is touched); picking
 * one here pins it as a fixed startup default regardless of what gets used
 * later in a session — switching models elsewhere (Library, the engine pill)
 * only changes what's active *now*, not what boots next time.
 */
function DefaultModelSection() {
  const models = useStore((s) => s.models);
  const settings = useStore((s) => s.settings);
  const saveSettings = useStore((s) => s.saveSettings);
  const { t } = useTranslation();

  const installed = models.filter((m) => m.status.type === "installed");
  const lastUsed = installed.find((m) => m.spec.id === settings?.selectedModelId);

  return (
    <div className="settings-field">
      <span className="settings-label">{t("settingsSheet.defaultModel.label")}</span>
      <span className="settings-hint">{t("settingsSheet.defaultModel.hint")}</span>
      {installed.length === 0 ? (
        <span className="settings-hint">
          {t("settingsSheet.defaultModel.noModels")}
        </span>
      ) : (
        <select
          className="settings-select"
          value={settings?.defaultModelId ?? ""}
          onChange={(e) =>
            void saveSettings({ defaultModelId: e.target.value || null })
          }
        >
          <option value="">
            {lastUsed
              ? t("settingsSheet.defaultModel.lastUsedWithName", { name: lastUsed.spec.name })
              : t("settingsSheet.defaultModel.lastUsed")}
          </option>
          {installed.map((m) => (
            <option key={m.spec.id} value={m.spec.id}>
              {m.spec.name}
            </option>
          ))}
        </select>
      )}
    </div>
  );
}

/**
 * Agent tools: the master enable switch, and the standing "Always allow"
 * grants — with a Revoke button for each, so a grant given in a moment of
 * trust doesn't quietly outlive its usefulness. The workspace folder tools
 * are jailed to is system-managed (one folder per conversation, under the
 * app's data directory) and isn't user-configurable.
 */
function AgentModeSection() {
  const { t } = useTranslation();
  const settings = useStore((s) => s.settings);
  const saveSettings = useStore((s) => s.saveSettings);
  const agentModeEnabled = settings?.agentModeEnabled ?? false;
  // `RemoteBackend::endpoint()` always returns `None` by design
  // (crates/core/src/remote/client/backend.rs), so agent mode can never
  // actually start on Android — without this, turning the toggle on would
  // silently hit `agent_chat_turn`'s "Your AI isn't ready yet. Pick a
  // model, then try again." error, accurate for desktop, wrong on a phone
  // with no model to pick. Section stays visible (not hidden, unlike
  // Plugins) so a curious user can still read why.
  const isAndroid = useStore((s) => s.appInfo?.isAndroid ?? false);

  const [grants, setGrants] = useState<ToolGrant[]>([]);

  const loadGrants = () => {
    api
      .listToolGrants()
      .then(setGrants)
      .catch(() => {
        // Standing permissions are a nice-to-have list; a fetch hiccup
        // shouldn't block the rest of Settings.
      });
  };

  useEffect(() => {
    loadGrants();
  }, []);

  const revoke = async (grantKey: string) => {
    await api.revokeToolGrant(grantKey);
    loadGrants();
  };

  return (
    <SettingsGroup title={t("settingsSheet.agentMode.summary")}>
      <div className="settings-field">
        <span className="settings-hint">
          {isAndroid
            ? t("settingsSheet.agentMode.unavailableOnMobile")
            : t("settingsSheet.agentMode.hint")}
        </span>
        <label className="settings-toggle">
          <input
            type="checkbox"
            checked={agentModeEnabled}
            disabled={isAndroid}
            onChange={(e) => void saveSettings({ agentModeEnabled: e.target.checked })}
          />
          <span>{t("settingsSheet.agentMode.enableLabel")}</span>
        </label>
      </div>

      {grants.length > 0 && (
        <div className="settings-field">
          <span className="settings-label">{t("settingsSheet.agentMode.grants.label")}</span>
          <span className="settings-hint">{t("settingsSheet.agentMode.grants.hint")}</span>
          <ul className="settings-hf-files">
            {grants.map((g) => (
              <li key={g.grantKey}>
                <div className="settings-hf-file-info">
                  <span className="settings-hf-file-name" title={g.summary}>
                    {g.toolId}
                  </span>
                  <span className="settings-hf-file-meta">{g.summary}</span>
                </div>
                <button className="btn-quiet" onClick={() => void revoke(g.grantKey)}>
                  {t("settingsSheet.agentMode.grants.revoke")}
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
    </SettingsGroup>
  );
}

/** The request-triage feature's one setting: not shown anywhere in the chat
 * UI (no chip, no indicator) — classification is only ever visible in the
 * app's log file. Hand-tuning a sampling slider always overrides it in the
 * moment, per `useTriageAutopilot` (`ChatView.tsx`) / the Generation panel's
 * own "auto-suggested" comparison. */
function TriageSection() {
  const { t } = useTranslation();
  const settings = useStore((s) => s.settings);
  const saveSettings = useStore((s) => s.saveSettings);
  const autoPickSampling = settings?.autoPickSampling ?? true;

  return (
    <div className="settings-field">
      <span className="settings-label">{t("settingsSheet.triage.summary")}</span>
      <span className="settings-hint">{t("settingsSheet.triage.hint")}</span>
      <label className="settings-toggle">
        <input
          type="checkbox"
          checked={autoPickSampling}
          onChange={(e) => void saveSettings({ autoPickSampling: e.target.checked })}
        />
        <span>{t(autoPickSampling ? "settingsSheet.toggle.on" : "settingsSheet.toggle.off")}</span>
      </label>
    </div>
  );
}

/** The safety-classifier feature's one setting: unlike most opt-in features
 * in Settings, it's a safety net rather than a power-user convenience, so
 * it's the first thing Privacy & safety shows (not tucked away under
 * Advanced) and defaults on. See `Settings.riskDetectionEnabled` /
 * `SafetyBanner`. */
function SafetyDetectionSection() {
  const { t } = useTranslation();
  const settings = useStore((s) => s.settings);
  const saveSettings = useStore((s) => s.saveSettings);
  const riskDetectionEnabled = settings?.riskDetectionEnabled ?? true;

  return (
    <div className="settings-field">
      <span className="settings-label">{t("settingsSheet.safetyDetection.summary")}</span>
      <span className="settings-hint">{t("settingsSheet.safetyDetection.hint")}</span>
      <label className="settings-toggle">
        <input
          type="checkbox"
          checked={riskDetectionEnabled}
          onChange={(e) => void saveSettings({ riskDetectionEnabled: e.target.checked })}
        />
        <span>
          {t(riskDetectionEnabled ? "settingsSheet.toggle.on" : "settingsSheet.toggle.off")}
        </span>
      </label>
    </div>
  );
}

/**
 * The app-lock PIN gate (`AppLockGate`, `AppInfo.needsAppUnlock`) — whether
 * OpenPal asks for a PIN at launch, at all. Same summary/unlocking/editing
 * shape and full-screen `PinPad` idiom as `ParentalControlsSection` below,
 * stripped to just the PIN itself (no schedule/usage/message fields to gate
 * behind it).
 */
function SecuritySection() {
  const { t } = useTranslation();
  const [status, setStatus] = useState<AppLockStatusView | null>(null);
  const [mode, setMode] = useState<"summary" | "unlocking" | "editing">("summary");
  /** The PIN just proven correct, replayed as `currentPin` on save/turn-off. */
  const [unlockedPin, setUnlockedPin] = useState<string | null>(null);
  const [unlockError, setUnlockError] = useState<string | null>(null);
  const [unlockLockedFor, setUnlockLockedFor] = useState(0);
  const [pinFlow, setPinFlow] = useState<"idle" | "create" | "confirm">("idle");
  const [pendingPin, setPendingPin] = useState<string | null>(null);
  const [pinFlowError, setPinFlowError] = useState<string | null>(null);
  /** The new PIN, once entered twice and matching — what actually gets sent
   * as `newPin` on save. */
  const [savedNewPin, setSavedNewPin] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Captured once at mount — `AppLockGate`'s recovery-code stage sets this
  // and opens Settings straight to this section's category; clear the store's copy
  // right away so it doesn't reappear on a later visit, but keep showing
  // the banner for the rest of this mount's lifetime.
  const [showResetBanner] = useState(() => useStore.getState().appLockJustReset);
  useEffect(() => {
    if (showResetBanner) useStore.setState({ appLockJustReset: false });
  }, [showResetBanner]);

  const load = () =>
    api
      .getAppLockStatus()
      .then(setStatus)
      .catch((err: unknown) => setError(String(err)));

  useEffect(() => {
    void load();
  }, []);

  if (!status) {
    return (
      <SettingsGroup title={t("appLock.settings.summary")}>
        <p className="settings-hint">{error ?? t("settingsSheet.parentalControls.loading")}</p>
      </SettingsGroup>
    );
  }

  const backToSummary = () => {
    setMode("summary");
    setUnlockedPin(null);
    setUnlockError(null);
    setUnlockLockedFor(0);
    setPinFlow("idle");
    setPendingPin(null);
    setPinFlowError(null);
    setSavedNewPin(null);
    setError(null);
  };

  const startSetup = () => {
    setSavedNewPin(null);
    setError(null);
    setMode("editing");
  };

  const unlock = async (pin: string) => {
    if (busy) return;
    setBusy(true);
    setUnlockError(null);
    setUnlockLockedFor(0);
    try {
      const result = await api.verifyAppLockPin(pin);
      if (result.ok) {
        setUnlockedPin(pin);
        setSavedNewPin(null);
        setMode("editing");
      } else if (result.lockedForSeconds > 0) {
        setUnlockLockedFor(result.lockedForSeconds);
      } else {
        setUnlockError(t("appLock.settings.pin.wrongPin"));
      }
    } catch (err: unknown) {
      setUnlockError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const submit = async (enabled: boolean) => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await api.setAppLockPin(unlockedPin, enabled, enabled ? savedNewPin : null);
      await load();
      backToSummary();
    } catch (err: unknown) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const pinOk = status.enabled || savedNewPin !== null;
  const canSave = pinOk && !busy;

  return (
    <SettingsGroup title={t("appLock.settings.summary")}>
      <div className="settings-field">
        <span className="settings-hint">{t("appLock.settings.hint")}</span>

        {showResetBanner && (
          <p className="settings-hint app-lock-reset-banner" role="status">
            {t("appLock.settings.resetBanner")}
          </p>
        )}

        {mode === "summary" && (
          <>
            <span className="settings-label">
              {t(status.enabled ? "appLock.settings.stateOn" : "appLock.settings.stateOff")}
            </span>
            <div className="logs-actions">
              <button
                className="btn-ghost"
                onClick={() => (status.enabled ? setMode("unlocking") : startSetup())}
              >
                {t(status.enabled ? "appLock.settings.changeSettings" : "appLock.settings.turnOn")}
              </button>
            </div>
          </>
        )}

        {mode === "unlocking" && (
          <PinPad
            title={t("appLock.settings.enterPin")}
            error={unlockError}
            busy={busy}
            lockedForSeconds={unlockLockedFor}
            renderLocked={(seconds) => t("appLock.settings.pin.lockedFor", { seconds })}
            cancelLabel={t("common.cancel")}
            onCancel={backToSummary}
            onSubmit={(pin) => void unlock(pin)}
          />
        )}

        {error && mode !== "editing" && <p className="model-error" role="alert">{error}</p>}
      </div>

      {mode === "editing" && (
        <>
          <div className="settings-field">
            <span className="settings-label">
              {t(
                status.enabled || savedNewPin
                  ? "appLock.settings.pin.changeLabel"
                  : "appLock.settings.pin.createLabel",
              )}
            </span>
            <span className="settings-hint">
              {savedNewPin
                ? t("appLock.settings.pin.newPinSet")
                : t(
                    status.enabled
                      ? "appLock.settings.pin.changeHint"
                      : "appLock.settings.pin.createHint",
                  )}
            </span>
            <div className="logs-actions">
              <button
                type="button"
                className="btn-ghost"
                onClick={() => {
                  setPendingPin(null);
                  setPinFlowError(null);
                  setPinFlow("create");
                }}
              >
                {t(
                  status.enabled || savedNewPin
                    ? "appLock.settings.pin.changeLabel"
                    : "appLock.settings.pin.createLabel",
                )}
              </button>
            </div>
          </div>

          <div className="settings-field">
            <div className="logs-actions">
              <button className="btn-ghost" disabled={!canSave} onClick={() => void submit(true)}>
                {busy ? t("appLock.settings.saving") : t("common.save")}
              </button>
              {status.enabled && (
                <button className="btn-quiet" disabled={busy} onClick={() => void submit(false)}>
                  {t("appLock.settings.turnOff")}
                </button>
              )}
              <button className="btn-quiet" disabled={busy} onClick={backToSummary}>
                {t("common.cancel")}
              </button>
            </div>
            {error && <p className="model-error" role="alert">{error}</p>}
          </div>
        </>
      )}

      {pinFlow === "create" && (
        <PinPad
          title={t("appLock.settings.pin.enterNew")}
          cancelLabel={t("common.cancel")}
          onCancel={() => setPinFlow("idle")}
          onSubmit={(pin) => {
            setPendingPin(pin);
            setPinFlowError(null);
            setPinFlow("confirm");
          }}
        />
      )}

      {pinFlow === "confirm" && (
        <PinPad
          title={t("appLock.settings.pin.confirmNew")}
          error={pinFlowError}
          cancelLabel={t("common.cancel")}
          onCancel={() => setPinFlow("idle")}
          onSubmit={(pin) => {
            if (pin === pendingPin) {
              setSavedNewPin(pin);
              setPinFlow("idle");
            } else {
              setPinFlowError(t("appLock.settings.pin.mismatch"));
            }
          }}
        />
      )}
    </SettingsGroup>
  );
}

/** The editable half of `ParentalSettingsView` — everything the form owns. */
type ParentalForm = Omit<ParentalSettingsView, "enabled" | "hasPin">;

function parentalForm(view: ParentalSettingsView): ParentalForm {
  return {
    scheduleEnabled: view.scheduleEnabled,
    allowedStart: view.allowedStart,
    allowedEnd: view.allowedEnd,
    usageCapEnabled: view.usageCapEnabled,
    usageCapMinutes: view.usageCapMinutes,
    messageCapEnabled: view.messageCapEnabled,
    messageCapPerDay: view.messageCapPerDay,
  };
}

/**
 * A soft daily lock a parent can put on sending new messages —
 * a schedule window, a time cap, a message cap, any of which pauses the
 * composer. Conversation history is never touched. One PIN guards the whole
 * configuration, including switching it back off, so the form only opens
 * after that PIN is proven (`verifyParentalPin`) rather than making the
 * parent refill everything just to find out they mistyped it. Both PIN entry
 * points (unlocking, and choosing/changing a PIN) go through the generic,
 * full-screen `PinPad` — never an inline field — and attempts are
 * rate-limited server-side (`PinCheckResult`), so `PinPad`'s lockout
 * countdown reflects a real backoff, not just UI decoration.
 */
function ParentalControlsSection() {
  const { t } = useTranslation();
  const [view, setView] = useState<ParentalSettingsView | null>(null);
  const [mode, setMode] = useState<"summary" | "unlocking" | "editing">("summary");
  const [form, setForm] = useState<ParentalForm | null>(null);
  /** The PIN just proven correct, replayed as `currentPin` on save/turn-off. */
  const [unlockedPin, setUnlockedPin] = useState<string | null>(null);
  const [unlockError, setUnlockError] = useState<string | null>(null);
  const [unlockLockedFor, setUnlockLockedFor] = useState(0);
  /** Full-screen create/confirm flow for a new or replacement PIN, opened
   * from a button inside the edit form rather than inline fields. */
  const [pinFlow, setPinFlow] = useState<"idle" | "create" | "confirm">("idle");
  const [pendingPin, setPendingPin] = useState<string | null>(null);
  const [pinFlowError, setPinFlowError] = useState<string | null>(null);
  /** The new PIN, once entered twice and matching — what actually gets sent
   * as `newPin` on save. */
  const [savedNewPin, setSavedNewPin] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = () =>
    api
      .getParentalSettings()
      .then(setView)
      .catch((err: unknown) => setError(String(err)));

  useEffect(() => {
    void load();
  }, []);

  if (!view) {
    return (
      <SettingsGroup title={t("settingsSheet.parentalControls.summary")}>
        <p className="settings-hint">{error ?? t("settingsSheet.parentalControls.loading")}</p>
      </SettingsGroup>
    );
  }

  const patch = (p: Partial<ParentalForm>) => setForm((f) => (f ? { ...f, ...p } : f));

  const backToSummary = () => {
    setMode("summary");
    setForm(null);
    setUnlockedPin(null);
    setUnlockError(null);
    setUnlockLockedFor(0);
    setPinFlow("idle");
    setPendingPin(null);
    setPinFlowError(null);
    setSavedNewPin(null);
    setError(null);
  };

  const startSetup = () => {
    // Nothing is limited by default, so a first setup would otherwise open
    // with all three switches off and Save unavailable.
    setForm({ ...parentalForm(view), scheduleEnabled: true });
    setSavedNewPin(null);
    setError(null);
    setMode("editing");
  };

  const unlock = async (pin: string) => {
    if (busy) return;
    setBusy(true);
    setUnlockError(null);
    // Reset first so a fresh lockout of the same duration as the last one
    // (e.g. both capped at the max) still transitions through 0 — a prop
    // that never actually changes value wouldn't re-seed PinPad's countdown.
    setUnlockLockedFor(0);
    try {
      const result = await api.verifyParentalPin(pin);
      if (result.ok) {
        setUnlockedPin(pin);
        setForm(parentalForm(view));
        setSavedNewPin(null);
        setMode("editing");
      } else if (result.lockedForSeconds > 0) {
        setUnlockLockedFor(result.lockedForSeconds);
      } else {
        setUnlockError(t("settingsSheet.parentalControls.wrongPin"));
      }
    } catch (err: unknown) {
      setUnlockError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const submit = async (enabled: boolean) => {
    if (!form || busy) return;
    setBusy(true);
    setError(null);
    try {
      await api.setParentalControls(unlockedPin, {
        ...form,
        enabled,
        // Turning off ignores a freshly-chosen replacement PIN.
        newPin: enabled ? savedNewPin : null,
      });
      await load();
      backToSummary();
    } catch (err: unknown) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const pinOk = view.hasPin || savedNewPin !== null;
  const anyLimit =
    !!form && (form.scheduleEnabled || form.usageCapEnabled || form.messageCapEnabled);
  const amountsOk =
    !!form &&
    (!form.usageCapEnabled || form.usageCapMinutes > 0) &&
    (!form.messageCapEnabled || form.messageCapPerDay > 0);
  const canSave = !!form && pinOk && anyLimit && amountsOk && !busy;

  const limits = [
    view.scheduleEnabled &&
      t("settingsSheet.parentalControls.limitSchedule", {
        start: view.allowedStart,
        end: view.allowedEnd,
      }),
    view.usageCapEnabled &&
      t("settingsSheet.parentalControls.limitUsage", { count: view.usageCapMinutes }),
    view.messageCapEnabled &&
      t("settingsSheet.parentalControls.limitMessage", { count: view.messageCapPerDay }),
  ].filter((line): line is string => typeof line === "string");

  return (
    <SettingsGroup title={t("settingsSheet.parentalControls.summary")}>
      <div className="settings-field">
        <span className="settings-hint">{t("settingsSheet.parentalControls.hint")}</span>

        {mode === "summary" && (
          <>
            <span className="settings-label">
              {t(
                view.enabled
                  ? "settingsSheet.parentalControls.stateOn"
                  : "settingsSheet.parentalControls.stateOff",
              )}
            </span>
            {view.enabled &&
              (limits.length > 0 ? (
                limits.map((line) => (
                  <span key={line} className="settings-hint">
                    {line}
                  </span>
                ))
              ) : (
                <span className="settings-hint">
                  {t("settingsSheet.parentalControls.noLimits")}
                </span>
              ))}
            <div className="logs-actions">
              <button
                className="btn-ghost"
                onClick={() => (view.hasPin ? setMode("unlocking") : startSetup())}
              >
                {t(
                  view.hasPin
                    ? "settingsSheet.parentalControls.changeSettings"
                    : "settingsSheet.parentalControls.turnOn",
                )}
              </button>
            </div>
          </>
        )}

        {mode === "unlocking" && (
          <PinPad
            title={t("settingsSheet.parentalControls.enterPin")}
            error={unlockError}
            busy={busy}
            lockedForSeconds={unlockLockedFor}
            renderLocked={(seconds) =>
              t("settingsSheet.parentalControls.pin.lockedFor", { seconds })
            }
            cancelLabel={t("common.cancel")}
            onCancel={backToSummary}
            onSubmit={(pin) => void unlock(pin)}
          />
        )}

        {error && mode !== "editing" && <p className="model-error" role="alert">{error}</p>}
      </div>

      {mode === "editing" && form && (
        <>
          <div className="settings-field">
            <span className="settings-hint">{t("settingsSheet.parentalControls.schedule.hint")}</span>
            <label className="settings-toggle">
              <input
                type="checkbox"
                checked={form.scheduleEnabled}
                onChange={(e) => patch({ scheduleEnabled: e.target.checked })}
              />
              <span>{t("settingsSheet.parentalControls.schedule.label")}</span>
            </label>
            {form.scheduleEnabled && (
              <div className="settings-local-row">
                <span className="settings-hint">
                  {t("settingsSheet.parentalControls.schedule.from")}
                </span>
                <input
                  type="time"
                  value={form.allowedStart}
                  aria-label={t("settingsSheet.parentalControls.schedule.startAria")}
                  onChange={(e) => patch({ allowedStart: e.target.value })}
                />
                <span className="settings-hint">
                  {t("settingsSheet.parentalControls.schedule.to")}
                </span>
                <input
                  type="time"
                  value={form.allowedEnd}
                  aria-label={t("settingsSheet.parentalControls.schedule.endAria")}
                  onChange={(e) => patch({ allowedEnd: e.target.value })}
                />
              </div>
            )}
          </div>

          <div className="settings-field">
            <span className="settings-hint">{t("settingsSheet.parentalControls.usage.hint")}</span>
            <label className="settings-toggle">
              <input
                type="checkbox"
                checked={form.usageCapEnabled}
                onChange={(e) => patch({ usageCapEnabled: e.target.checked })}
              />
              <span>{t("settingsSheet.parentalControls.usage.label")}</span>
            </label>
            {form.usageCapEnabled && (
              <div className="settings-local-row">
                <input
                  type="number"
                  min={1}
                  max={1440}
                  value={form.usageCapMinutes}
                  aria-label={t("settingsSheet.parentalControls.usage.minutesLabel")}
                  onChange={(e) => patch({ usageCapMinutes: Number(e.target.value) })}
                />
                <span className="settings-hint">
                  {t("settingsSheet.parentalControls.usage.minutesLabel")}
                </span>
              </div>
            )}
          </div>

          <div className="settings-field">
            <span className="settings-hint">{t("settingsSheet.parentalControls.message.hint")}</span>
            <label className="settings-toggle">
              <input
                type="checkbox"
                checked={form.messageCapEnabled}
                onChange={(e) => patch({ messageCapEnabled: e.target.checked })}
              />
              <span>{t("settingsSheet.parentalControls.message.label")}</span>
            </label>
            {form.messageCapEnabled && (
              <div className="settings-local-row">
                <input
                  type="number"
                  min={1}
                  max={9999}
                  value={form.messageCapPerDay}
                  aria-label={t("settingsSheet.parentalControls.message.countLabel")}
                  onChange={(e) => patch({ messageCapPerDay: Number(e.target.value) })}
                />
                <span className="settings-hint">
                  {t("settingsSheet.parentalControls.message.countLabel")}
                </span>
              </div>
            )}
          </div>

          <div className="settings-field">
            <span className="settings-label">
              {t(
                view.hasPin || savedNewPin
                  ? "settingsSheet.parentalControls.pin.changeLabel"
                  : "settingsSheet.parentalControls.pin.createLabel",
              )}
            </span>
            <span className="settings-hint">
              {savedNewPin
                ? t("settingsSheet.parentalControls.pin.newPinSet")
                : t(
                    view.hasPin
                      ? "settingsSheet.parentalControls.pin.changeHint"
                      : "settingsSheet.parentalControls.pin.createHint",
                  )}
            </span>
            <div className="logs-actions">
              <button
                type="button"
                className="btn-ghost"
                onClick={() => {
                  setPendingPin(null);
                  setPinFlowError(null);
                  setPinFlow("create");
                }}
              >
                {t(
                  view.hasPin || savedNewPin
                    ? "settingsSheet.parentalControls.pin.changeLabel"
                    : "settingsSheet.parentalControls.pin.createLabel",
                )}
              </button>
            </div>
          </div>

          <div className="settings-field">
            {!anyLimit && (
              <span className="settings-hint">
                {t("settingsSheet.parentalControls.needsLimit")}
              </span>
            )}
            <div className="logs-actions">
              <button className="btn-ghost" disabled={!canSave} onClick={() => void submit(true)}>
                {busy ? t("settingsSheet.parentalControls.saving") : t("common.save")}
              </button>
              {view.hasPin && (
                <button className="btn-quiet" disabled={busy} onClick={() => void submit(false)}>
                  {t("settingsSheet.parentalControls.turnOff")}
                </button>
              )}
              <button className="btn-quiet" disabled={busy} onClick={backToSummary}>
                {t("common.cancel")}
              </button>
            </div>
            {error && <p className="model-error" role="alert">{error}</p>}
          </div>
        </>
      )}

      {pinFlow === "create" && (
        <PinPad
          title={t("settingsSheet.parentalControls.pin.enterNew")}
          subtitle={t(
            view.hasPin
              ? "settingsSheet.parentalControls.pin.changeHint"
              : "settingsSheet.parentalControls.pin.createHint",
          )}
          cancelLabel={t("common.cancel")}
          onCancel={() => setPinFlow("idle")}
          onSubmit={(pin) => {
            setPendingPin(pin);
            setPinFlowError(null);
            setPinFlow("confirm");
          }}
        />
      )}

      {pinFlow === "confirm" && (
        <PinPad
          title={t("settingsSheet.parentalControls.pin.confirmNew")}
          error={pinFlowError}
          cancelLabel={t("common.cancel")}
          onCancel={() => setPinFlow("idle")}
          onSubmit={(pin) => {
            if (pin === pendingPin) {
              setSavedNewPin(pin);
              setPinFlow("idle");
            } else {
              setPinFlowError(t("settingsSheet.parentalControls.pin.mismatch"));
            }
          }}
        />
      )}
    </SettingsGroup>
  );
}

type PairingUiState =
  | { state: "idle" }
  | { state: "waiting"; payload: PairingPayload; qrDataUrl: string | null }
  | { state: "paired"; deviceName: string }
  | { state: "expired" }
  | { state: "failed"; message: string };

/**
 * Remote Access: let another device (a future OpenPal mobile client, or a
 * curl-based test today) pair with this desktop and use it as the
 * inference source over the LAN or an already-connected overlay network
 * (Tailscale/WireGuard/router port-forward). Off by default. The gateway
 * itself, pairing, wake-on-LAN, and discovery are all Rust-side
 * (`openpal_core::remote`); this is just status display + the pairing UI.
 */
function RemoteAccessSection() {
  // Its own namespace (`remoteAccess`), not the default `translation` one —
  // see `i18n/index.ts`'s comment: this many keys added directly to
  // `translation` pushed TypeScript's key-union type past its "excessively
  // deep" instantiation limit (surfaced as a compile error in unrelated
  // files, e.g. ToolConsentCard.tsx), the same issue `recovery` hit first.
  const { t } = useTranslation("remoteAccess");
  const [status, setStatus] = useState<RemoteAccessStatusInfo | null>(null);
  const [wolStatus, setWolStatus] = useState<WolStatus | null>(null);
  const [devices, setDevices] = useState<PairedDeviceInfo[]>([]);
  const [toggling, setToggling] = useState(false);
  const [pairing, setPairing] = useState<PairingUiState>({ state: "idle" });
  const [editingDeviceId, setEditingDeviceId] = useState<string | null>(null);
  const [editingName, setEditingName] = useState("");

  const loadStatus = () => {
    api
      .getRemoteAccessStatus()
      .then(setStatus)
      .catch(() => {
        // A status hiccup shouldn't block the rest of Settings.
      });
  };
  const loadDevices = () => {
    api
      .listPairedDevices()
      .then(setDevices)
      .catch(() => {});
  };

  useEffect(() => {
    loadStatus();
    loadDevices();
    api
      .getWolStatus()
      .then(setWolStatus)
      .catch(() => {});
  }, []);

  const onToggle = async (enabled: boolean) => {
    setToggling(true);
    try {
      await api.setRemoteAccessEnabled(enabled);
    } finally {
      setToggling(false);
      loadStatus();
    }
  };

  const startPairing = () => {
    setPairing({ state: "idle" });
    void api
      .startPairingSession((event) => {
        switch (event.type) {
          case "waiting": {
            const { payload } = event.data;
            setPairing({ state: "waiting", payload, qrDataUrl: null });
            void QRCode.toDataURL(JSON.stringify(payload)).then((qrDataUrl) => {
              setPairing((prev) =>
                prev.state === "waiting" && prev.payload.code === payload.code
                  ? { ...prev, qrDataUrl }
                  : prev,
              );
            });
            break;
          }
          case "paired":
            setPairing({ state: "paired", deviceName: event.data.deviceName });
            loadDevices();
            loadStatus();
            break;
          case "expired":
            setPairing({ state: "expired" });
            break;
          case "failed":
            setPairing({ state: "failed", message: event.data.message });
            break;
        }
      })
      .catch((err: unknown) => setPairing({ state: "failed", message: String(err) }));
  };

  const revoke = async (id: string) => {
    await api.revokePairedDevice(id);
    loadDevices();
    loadStatus();
  };

  const startRename = (device: PairedDeviceInfo) => {
    setEditingDeviceId(device.id);
    setEditingName(device.name);
  };
  const saveRename = async (id: string) => {
    const name = editingName.trim();
    setEditingDeviceId(null);
    if (name.length === 0) return;
    await api.renamePairedDevice(id, name);
    loadDevices();
  };

  const enabled = status?.enabled ?? false;

  return (
    <SettingsGroup title={t("summary")}>
      <div className="settings-field">
        <span className="settings-hint">{t("hint")}</span>
        <label className="settings-toggle">
          <input
            type="checkbox"
            checked={enabled}
            disabled={toggling}
            onChange={(e) => void onToggle(e.target.checked)}
          />
          <span>{t("enableLabel")}</span>
        </label>
        {enabled && !status?.running && (
          <span className="settings-hint">{t("starting")}</span>
        )}
      </div>

      {enabled && status?.running && (
        <>
          <div className="settings-field">
            <span className="settings-label">{t("pairing.label")}</span>
            <span className="settings-hint">{t("pairing.hint")}</span>
            {pairing.state === "idle" && (
              <button className="btn-ghost" onClick={startPairing}>
                {t("pairing.addDevice")}
              </button>
            )}
            {pairing.state === "waiting" && (
              <div className="settings-remote-pairing">
                {pairing.qrDataUrl && (
                  <img
                    src={pairing.qrDataUrl}
                    alt={t("pairing.qrAlt")}
                    width={200}
                    height={200}
                  />
                )}
                <p className="settings-hint">
                  {t("pairing.manualCode", { code: pairing.payload.code })}
                </p>
                <p className="settings-hint">{t("pairing.waiting")}</p>
              </div>
            )}
            {pairing.state === "paired" && (
              <p className="settings-hint">
                {t("pairing.paired", { name: pairing.deviceName })}
              </p>
            )}
            {pairing.state === "expired" && (
              <>
                <p className="settings-hint">{t("pairing.expired")}</p>
                <button className="btn-ghost" onClick={startPairing}>
                  {t("pairing.addDevice")}
                </button>
              </>
            )}
            {pairing.state === "failed" && (
              <>
                <p className="model-error" role="alert">
                  {pairing.message}
                </p>
                <button className="btn-ghost" onClick={startPairing}>
                  {t("pairing.addDevice")}
                </button>
              </>
            )}
          </div>

          {devices.length > 0 && (
            <div className="settings-field">
              <span className="settings-label">{t("devices.label")}</span>
              <ul className="settings-hf-files">
                {devices.map((d) => (
                  <li key={d.id}>
                    <div className="settings-hf-file-info">
                      {editingDeviceId === d.id ? (
                        <input
                          className="settings-text"
                          value={editingName}
                          autoFocus
                          onChange={(e) => setEditingName(e.target.value)}
                          onBlur={() => void saveRename(d.id)}
                          onKeyDown={(e) => {
                            if (e.key === "Enter") void saveRename(d.id);
                            if (e.key === "Escape") setEditingDeviceId(null);
                          }}
                        />
                      ) : (
                        <button
                          type="button"
                          className="settings-hf-file-name settings-remote-device-name"
                          onClick={() => startRename(d)}
                          title={t("devices.renameHint")}
                        >
                          {d.name}
                        </button>
                      )}
                    </div>
                    <button className="btn-quiet" onClick={() => void revoke(d.id)}>
                      {t("devices.revoke")}
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}

          <div className="settings-field">
            <span className="settings-label">{t("wol.label")}</span>
            {wolStatus?.type === "enabled" && (
              <span className="settings-hint">{t("wol.enabled")}</span>
            )}
            {wolStatus?.type === "disabled" && (
              <span className="settings-hint">{t("wol.disabled")}</span>
            )}
            {wolStatus?.type === "unknown" && (
              <span className="settings-hint">{wolStatus.data.reason}</span>
            )}
            {status.interfaces.length > 0 && (
              <span className="settings-hint">
                {t("wol.mac", { mac: status.interfaces[0].mac })}
              </span>
            )}
          </div>

          <div className="settings-field">
            <span className="settings-hint">{t("offLan")}</span>
          </div>
        </>
      )}
    </SettingsGroup>
  );
}

type RemoteClientUiState =
  | { state: "loading" }
  | { state: "unpaired" }
  | { state: "pairing" }
  | { state: "paired"; record: RemotePairingRecord }
  | { state: "error"; message: string };

/** Best-effort, human-readable default the desktop's paired-device list
 * shows for this phone — renameable there. Not derived from any device
 * API: most Android WebViews don't expose a model name to the page. */
function defaultDeviceName(): string {
  return "Android phone";
}

/**
 * The other side of Remote Access (Android only): pair this phone with a
 * desktop's gateway and use it as the inference source, since Android
 * can't run the engine itself — see `docs/design/MOBILE.md`. Discovery, pairing,
 * and status are all Rust-side (`openpal_core::remote::client`); this is
 * status display + the pairing UI, mirroring `RemoteAccessSection` above.
 */
function RemoteClientSection() {
  const { t } = useTranslation("remoteClient");
  const [ui, setUi] = useState<RemoteClientUiState>({ state: "loading" });
  const [hosts, setHosts] = useState<DiscoveredHost[]>([]);
  const [addr, setAddr] = useState("");
  const [port, setPort] = useState("");
  const [code, setCode] = useState("");
  const [deviceName] = useState(defaultDeviceName);

  const loadStatus = () => {
    api
      .remoteClientStatus()
      .then((record) => setUi(record ? { state: "paired", record } : { state: "unpaired" }))
      .catch(() => setUi({ state: "unpaired" }));
  };

  useEffect(() => {
    loadStatus();
  }, []);

  // Browse the LAN whenever there's a reason to (not yet paired, or the
  // last pairing attempt failed) — never while a pairing request is
  // in-flight or once paired. Won't find anything from most emulators/VMs
  // or an overlay network; the manual fields below are the real fallback
  // for those, not just a test workaround.
  useEffect(() => {
    if (ui.state !== "unpaired" && ui.state !== "error") return;
    setHosts([]);
    void api.remoteDiscoverHosts((host) => {
      setHosts((prev) => (prev.some((h) => h.deviceId === host.deviceId) ? prev : [...prev, host]));
    });
    return () => {
      void api.remoteStopDiscovery();
    };
  }, [ui.state]);

  const selectHost = (host: DiscoveredHost) => {
    setAddr(host.addrs[0] ?? "");
    setPort(String(host.port));
  };

  const pair = async () => {
    const portNum = Number(port);
    if (!addr.trim() || !Number.isInteger(portNum) || portNum <= 0 || code.trim().length === 0) {
      return;
    }
    setUi({ state: "pairing" });
    try {
      const record = await api.remotePair(addr.trim(), portNum, code.trim(), deviceName);
      setCode("");
      setUi({ state: "paired", record });
    } catch (err) {
      setUi({ state: "error", message: String(err) });
    }
  };

  const forget = async () => {
    await api.remoteForget();
    setAddr("");
    setPort("");
    setCode("");
    setUi({ state: "unpaired" });
  };

  const busy = ui.state === "pairing";

  return (
    <SettingsGroup title={t("summary")}>      <div className="settings-field">
        <span className="settings-hint">{t("hint")}</span>
      </div>

      {ui.state === "paired" ? (
        <div className="settings-field">
          <span className="settings-label">{t("connectedLabel")}</span>
          <span className="settings-hint">
            {t("connected", {
              name: ui.record.deviceName,
              addr: ui.record.hostAddr,
              port: ui.record.port,
            })}
          </span>
          <button className="btn-ghost" onClick={() => void forget()}>
            {t("forget")}
          </button>
        </div>
      ) : (
        <>
          {hosts.length > 0 && (
            <div className="settings-field">
              <span className="settings-label">{t("discovered.label")}</span>
              <ul className="settings-hf-files">
                {hosts.map((host) => (
                  <li key={host.deviceId}>
                    <button
                      type="button"
                      className="settings-hf-file-name"
                      onClick={() => selectHost(host)}
                      disabled={busy}
                    >
                      {host.deviceName}
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}

          <div className="settings-field">
            <span className="settings-label">{t("manual.label")}</span>
            <span className="settings-hint">{t("manual.hint")}</span>
            <input
              className="settings-text"
              placeholder={t("manual.addrPlaceholder")}
              value={addr}
              onChange={(e) => setAddr(e.target.value)}
              disabled={busy}
            />
            <input
              className="settings-text"
              placeholder={t("manual.portPlaceholder")}
              inputMode="numeric"
              value={port}
              onChange={(e) => setPort(e.target.value.replace(/[^0-9]/g, ""))}
              disabled={busy}
            />
            <input
              className="settings-text"
              placeholder={t("manual.codePlaceholder")}
              value={code}
              onChange={(e) => setCode(e.target.value)}
              disabled={busy}
            />
            <button className="btn-ghost" onClick={() => void pair()} disabled={busy}>
              {busy ? t("pairingInProgress") : t("pair")}
            </button>
          </div>

          {ui.state === "error" && (
            <p className="model-error" role="alert">
              {ui.message}
            </p>
          )}
        </>
      )}
    </SettingsGroup>
  );
}

/**
 * OpenPal's own updates and the AI engine's: whether to look for new
 * versions after each launch, and a button to look right now.
 */
function UpdatesSection() {
  const settings = useStore((s) => s.settings);
  const saveSettings = useStore((s) => s.saveSettings);
  const appInfo = useStore((s) => s.appInfo);
  const updateStatus = useStore((s) => s.updateStatus);
  const checkForUpdate = useStore((s) => s.checkForUpdate);
  const engineUpdateStatus = useStore((s) => s.engineUpdateStatus);
  const availableEngineUpdate = useStore((s) => s.availableEngineUpdate);
  const engineUpdateError = useStore((s) => s.engineUpdateError);
  const checkForEngineUpdate = useStore((s) => s.checkForEngineUpdate);
  const startEngineUpdateInstall = useStore((s) => s.startEngineUpdateInstall);
  const { t } = useTranslation();
  const autoCheckUpdates = settings?.autoCheckUpdates ?? false;
  const autoCheckEngineUpdates = settings?.autoCheckEngineUpdates ?? false;

  const [checkedNote, setCheckedNote] = useState<string | null>(null);
  const [checkedEngineNote, setCheckedEngineNote] = useState<string | null>(null);

  // "You're up to date" / a failure only shows after a manual check — an
  // "available" result opens UpdateDialog instead, so nothing to say here.
  const checkNow = async () => {
    setCheckedNote(null);
    await checkForUpdate(true);
    const after = useStore.getState();
    if (after.updateStatus === "idle") {
      setCheckedNote(t("settingsSheet.diagnostics.upToDate"));
    } else if (after.updateStatus === "error") {
      setCheckedNote(after.updateError);
    }
  };

  // Same shape as `checkNow`, on the engine's own status/note.
  const checkEngineNow = async () => {
    setCheckedEngineNote(null);
    await checkForEngineUpdate(true);
    const after = useStore.getState();
    if (after.engineUpdateStatus === "idle") {
      setCheckedEngineNote(t("settingsSheet.diagnostics.upToDate"));
    } else if (after.engineUpdateStatus === "error") {
      setCheckedEngineNote(after.engineUpdateError);
    }
  };

  return (
    <SettingsGroup title={t("settingsSheet.updates.title")}>
      <div className="settings-field">
        <span className="settings-label">{t("settingsSheet.diagnostics.updates.label")}</span>
        <span className="settings-hint">
          {appInfo?.isAndroid
            ? t("settingsSheet.diagnostics.updates.unavailableOnMobile")
            : t("settingsSheet.diagnostics.updates.hint", {
                version: appInfo?.version ? `v${appInfo.version}` : "",
              })}
        </span>
        <label className="settings-toggle">
          <input
            type="checkbox"
            checked={autoCheckUpdates}
            disabled={appInfo?.isAndroid}
            onChange={(e) => void saveSettings({ autoCheckUpdates: e.target.checked })}
          />
          <span>{t(autoCheckUpdates ? "settingsSheet.toggle.on" : "settingsSheet.toggle.off")}</span>
        </label>
        <div className="logs-actions">
          <button
            className="btn-ghost"
            disabled={updateStatus === "checking" || appInfo?.isAndroid}
            onClick={() => void checkNow()}
          >
            {updateStatus === "checking"
              ? t("settingsSheet.diagnostics.updates.checking")
              : t("settingsSheet.diagnostics.updates.checkNow")}
          </button>
        </div>
        {checkedNote && <p className="settings-hint logs-status" role="status">{checkedNote}</p>}
      </div>

      <div className="settings-field">
        <span className="settings-label">{t("settingsSheet.diagnostics.engineUpdates.label")}</span>
        <span className="settings-hint">{t("settingsSheet.diagnostics.engineUpdates.hint")}</span>
        <label className="settings-toggle">
          <input
            type="checkbox"
            checked={autoCheckEngineUpdates}
            onChange={(e) => void saveSettings({ autoCheckEngineUpdates: e.target.checked })}
          />
          <span>
            {t(autoCheckEngineUpdates ? "settingsSheet.toggle.on" : "settingsSheet.toggle.off")}
          </span>
        </label>
        <div className="logs-actions">
          {availableEngineUpdate ? (
            <button
              className="btn-ghost"
              disabled={engineUpdateStatus === "downloading"}
              onClick={() => void startEngineUpdateInstall()}
            >
              {engineUpdateStatus === "downloading"
                ? t("settingsSheet.diagnostics.engineUpdates.installing")
                : t("settingsSheet.diagnostics.engineUpdates.install", {
                    tag: availableEngineUpdate.tag,
                  })}
            </button>
          ) : (
            <button
              className="btn-ghost"
              disabled={engineUpdateStatus === "checking"}
              onClick={() => void checkEngineNow()}
            >
              {engineUpdateStatus === "checking"
                ? t("settingsSheet.diagnostics.engineUpdates.checking")
                : t("settingsSheet.diagnostics.engineUpdates.checkNow")}
            </button>
          )}
        </div>
        {engineUpdateStatus === "installed" && (
          <p className="settings-hint logs-status" role="status">
            {t("settingsSheet.diagnostics.engineUpdates.installed")}
          </p>
        )}
        {engineUpdateStatus === "error" && engineUpdateError && (
          <p className="settings-hint logs-status" role="status">{engineUpdateError}</p>
        )}
        {checkedEngineNote && (
          <p className="settings-hint logs-status" role="status">{checkedEngineNote}</p>
        )}
      </div>
    </SettingsGroup>
  );
}

/** How the AI engine uses this computer: skipping the graphics chip, and
 * streaming a model too big for memory from disk. */
function PerformanceSection() {
  const settings = useStore((s) => s.settings);
  const saveSettings = useStore((s) => s.saveSettings);
  const { t } = useTranslation();
  const forceCpu = settings?.forceCpu ?? false;
  const diskOffload = settings?.diskOffload ?? false;
  const autoUnloadIncludeDiskSlow = settings?.autoUnloadIncludeDiskSlow ?? false;

  return (
    <SettingsGroup title={t("settingsSheet.performance.title")}>
      <div className="settings-field">
        <span className="settings-label">{t("settingsSheet.diagnostics.forceCpu.label")}</span>
        <span className="settings-hint">{t("settingsSheet.diagnostics.forceCpu.hint")}</span>
        <label className="settings-toggle">
          <input
            type="checkbox"
            checked={forceCpu}
            onChange={(e) => void saveSettings({ forceCpu: e.target.checked })}
          />
          <span>{t(forceCpu ? "settingsSheet.toggle.on" : "settingsSheet.toggle.off")}</span>
        </label>
      </div>

      <div className="settings-field">
        <span className="settings-label">{t("settingsSheet.diagnostics.diskOffload.label")}</span>
        <span className="settings-hint">{t("settingsSheet.diagnostics.diskOffload.hint")}</span>
        <label className="settings-toggle">
          <input
            type="checkbox"
            checked={diskOffload}
            onChange={(e) => void saveSettings({ diskOffload: e.target.checked })}
          />
          <span>{t(diskOffload ? "settingsSheet.toggle.on" : "settingsSheet.toggle.off")}</span>
        </label>
      </div>

      {diskOffload && (
        <div className="settings-field">
          <span className="settings-label">
            {t("settingsSheet.diagnostics.autoUnloadDiskSlow.label")}
          </span>
          <span className="settings-hint">
            {t("settingsSheet.diagnostics.autoUnloadDiskSlow.hint")}
          </span>
          <label className="settings-toggle">
            <input
              type="checkbox"
              checked={autoUnloadIncludeDiskSlow}
              onChange={(e) =>
                void saveSettings({ autoUnloadIncludeDiskSlow: e.target.checked })
              }
            />
            <span>
              {t(
                autoUnloadIncludeDiskSlow ? "settingsSheet.toggle.on" : "settingsSheet.toggle.off",
              )}
            </span>
          </label>
        </div>
      )}
    </SettingsGroup>
  );
}

/** The export row's checkboxes, in the order they're shown. */
const EXPORT_CATEGORIES: (keyof ExportCategories)[] = [
  "chats",
  "settings",
  "skills",
  "customModels",
  "plugins",
];

const ALL_EXPORT_CATEGORIES: ExportCategories = {
  chats: true,
  settings: true,
  skills: true,
  customModels: true,
  plugins: true,
};

/** Where everything is kept, and taking it out (a backup) or bringing it in
 * from another device. */
export function YourDataSection() {
  const appInfo = useStore((s) => s.appInfo);
  const reloadAfterImport = useStore((s) => s.reloadAfterImport);
  const { t } = useTranslation();
  const idPrefix = useId();
  const [include, setInclude] = useState<ExportCategories>(ALL_EXPORT_CATEGORIES);
  // The export's size for `include`, re-asked whenever a box changes and
  // after an export or import (`sizeRefresh`), since either can change it.
  // Shown rounded up, never down, so the zip is never larger than this.
  const [size, setSize] = useState<number | "calculating" | "unavailable">("calculating");
  const [sizeRefresh, setSizeRefresh] = useState(0);
  // How much each category holds, for the line under its checkbox. `null`
  // until it arrives (or if it can't be read): the line then shows only what
  // doesn't depend on it.
  const [counts, setCounts] = useState<ExportCounts | null>(null);
  const [exporting, setExporting] = useState(false);
  const [exportStatus, setExportStatus] = useState<string | null>(null);
  const [importing, setImporting] = useState(false);
  const [importStatus, setImportStatus] = useState<string | null>(null);
  const nothingTicked = !EXPORT_CATEGORIES.some((c) => include[c]);

  useEffect(() => {
    let current = true;
    api.exportCounts().then(
      (next) => {
        if (current) setCounts(next);
      },
      () => {
        if (current) setCounts(null);
      },
    );
    return () => {
      current = false;
    };
  }, [sizeRefresh]);

  /** The lighter line under a category's label: how many it holds, plus
   * what custom models leave out. */
  const categoryDetail = (category: keyof ExportCategories): string | null => {
    const count = (key: keyof ExportCategories | "projects" | "files", n: number) =>
      t(`settingsSheet.diagnostics.exportData.include.count.${key}`, { count: n });
    const parts: string[] = [];
    if (counts) {
      switch (category) {
        case "chats":
          parts.push(
            [
              count("chats", counts.conversations),
              count("projects", counts.projects),
              count("files", counts.artifacts),
            ].join(", "),
          );
          break;
        case "settings":
          parts.push(count("settings", counts.changedSettings));
          break;
        case "skills":
          parts.push(count("skills", counts.skills));
          break;
        case "customModels":
          parts.push(count("customModels", counts.customModels));
          break;
        case "plugins":
          parts.push(count("plugins", counts.enabledPlugins));
          break;
      }
    }
    if (category === "customModels") {
      parts.push(t("settingsSheet.diagnostics.exportData.include.customModelsHint"));
    }
    return parts.length > 0 ? parts.join(" · ") : null;
  };

  useEffect(() => {
    if (nothingTicked) return;
    // Only the latest request's answer counts: ticking boxes quickly fires
    // several, and an earlier, slower one mustn't land last.
    let current = true;
    setSize("calculating");
    api.estimateExportSize(include).then(
      (bytes) => {
        if (current) setSize(bytes);
      },
      () => {
        if (current) setSize("unavailable");
      },
    );
    return () => {
      current = false;
    };
  }, [include, nothingTicked, sizeRefresh]);

  const runExport = async () => {
    setExporting(true);
    setExportStatus(null);
    try {
      const summary = await api.exportData(include);
      if (summary) {
        setExportStatus(
          t(
            summary.skipped > 0
              ? "settingsSheet.diagnostics.exportData.doneWithSkipped"
              : "settingsSheet.diagnostics.exportData.done",
            { ...summary },
          ),
        );
        setSizeRefresh((n) => n + 1);
      }
      // summary === null: the dialog was closed, nothing to say (mirrors
      // download_artifact's cancel convention).
    } catch (err: unknown) {
      setExportStatus(String(err));
    } finally {
      setExporting(false);
    }
  };

  const runImport = async () => {
    setImporting(true);
    setImportStatus(null);
    try {
      const summary = await api.importData();
      if (summary) {
        await reloadAfterImport(summary);
        setImportStatus(
          t(
            summary.skipped > 0
              ? "settingsSheet.diagnostics.importData.doneWithSkipped"
              : "settingsSheet.diagnostics.importData.done",
            { ...summary },
          ),
        );
        setSizeRefresh((n) => n + 1);
      }
      // summary === null: the dialog was closed, nothing to say (mirrors
      // download_artifact's cancel convention).
    } catch (err: unknown) {
      setImportStatus(String(err));
    } finally {
      setImporting(false);
    }
  };

  return (
    <>
      <div className="settings-field">
        <span className="settings-label">{t("settingsSheet.diagnostics.dataDir.label")}</span>
        <span className="settings-hint">
          {t("settingsSheet.diagnostics.dataDir.hint")}
          {appInfo?.portable && t("settingsSheet.diagnostics.dataDir.portableHint")}
        </span>
        <code className="settings-path">{appInfo?.dataDir ?? "-"}</code>
      </div>

      <div className="settings-field">
        <span className="settings-label">{t("settingsSheet.diagnostics.exportData.label")}</span>
        <span className="settings-hint">{t("settingsSheet.diagnostics.exportData.hint")}</span>
        <fieldset className="export-include">
          <legend className="settings-hint">
            {t("settingsSheet.diagnostics.exportData.include.label")}
          </legend>
          {EXPORT_CATEGORIES.map((category) => {
            const hint = categoryDetail(category);
            // The sub-text describes the checkbox rather than naming it, so
            // the label alone stays its accessible name.
            const labelId = `${idPrefix}-${category}`;
            const hintId = `${labelId}-hint`;
            return (
              <label key={category} className="settings-toggle export-include-option">
                <input
                  type="checkbox"
                  checked={include[category]}
                  aria-labelledby={labelId}
                  aria-describedby={hint ? hintId : undefined}
                  onChange={(e) => setInclude({ ...include, [category]: e.target.checked })}
                />
                <span className="export-include-text">
                  <span id={labelId}>
                    {t(`settingsSheet.diagnostics.exportData.include.${category}`)}
                  </span>
                  {hint && (
                    <span id={hintId} className="settings-hint">
                      {hint}
                    </span>
                  )}
                </span>
              </label>
            );
          })}
        </fieldset>
        {!nothingTicked && size !== "unavailable" && (
          <span className="settings-hint" aria-live="polite">
            {size === "calculating"
              ? t("settingsSheet.diagnostics.exportData.sizeCalculating")
              : t("settingsSheet.diagnostics.exportData.size", { size: formatBytesAtLeast(size) })}
          </span>
        )}
        <div className="logs-actions">
          <button
            className="btn-ghost"
            disabled={exporting || nothingTicked}
            onClick={() => void runExport()}
          >
            {exporting
              ? t("settingsSheet.diagnostics.exportData.exporting")
              : t("settingsSheet.diagnostics.exportData.button")}
          </button>
        </div>
        {exportStatus && (
          <p className="settings-hint logs-status" role="status">
            {exportStatus}
          </p>
        )}
      </div>

      <div className="settings-field">
        <span className="settings-label">{t("settingsSheet.diagnostics.importData.label")}</span>
        <span className="settings-hint">{t("settingsSheet.diagnostics.importData.hint")}</span>
        <div className="logs-actions">
          <button className="btn-ghost" disabled={importing} onClick={() => void runImport()}>
            {importing
              ? t("settingsSheet.diagnostics.importData.importing")
              : t("settingsSheet.diagnostics.importData.button")}
          </button>
        </div>
        {importStatus && (
          <p className="settings-hint logs-status" role="status">
            {importStatus}
          </p>
        )}
      </div>

      {/* Portable has no recovery code to unlock a backup with, and Android
          has no folder picker. */}
      {appInfo && !appInfo.portable && !appInfo.isAndroid && <AutoBackupField />}
    </>
  );
}

/** Settings → Your data's automatic, encrypted backup to a folder the user
 * picks, and restoring one with the recovery code. The schedule runs in the
 * backend (`crates/app/src/backup.rs`); this only shows and changes its
 * choices, and re-reads them whenever a run finishes. */
export function AutoBackupField() {
  const appInfo = useStore((s) => s.appInfo);
  const reloadAfterImport = useStore((s) => s.reloadAfterImport);
  const { t } = useTranslation();
  const idPrefix = useId();
  const [backup, setBackup] = useState<BackupState | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Restore: the picked file, waiting for its recovery code.
  const [restorePath, setRestorePath] = useState<string | null>(null);
  const [code, setCode] = useState("");
  const [restoring, setRestoring] = useState(false);
  const [restoreStatus, setRestoreStatus] = useState<string | null>(null);
  // `RecoverySetup` normally gates the whole app until a code exists, so
  // this only shows if that state changed under a running Settings.
  const needsRecoveryCode = appInfo?.needsRecoverySetup ?? false;

  useEffect(() => {
    let current = true;
    const load = () =>
      api.getBackupState().then(
        (next) => {
          if (current) setBackup(next);
        },
        () => {},
      );
    void load();
    let unlisten: (() => void) | null = null;
    void api
      .onBackupStateChanged(() => void load())
      .then((stop) => {
        if (current) unlisten = stop;
        else stop();
      });
    return () => {
      current = false;
      unlisten?.();
    };
  }, []);

  const change = async (run: () => Promise<BackupState | null>) => {
    setError(null);
    try {
      const next = await run();
      if (next) setBackup(next);
    } catch (err: unknown) {
      setError(String(err));
    }
  };

  /** Re-read the app's state so `App.tsx`'s gate opens `RecoverySetup` —
   * the same refetch `RecoverySetup` itself does once a code is saved. */
  const setUpRecoveryCode = async () => {
    useStore.setState({ appInfo: await api.getAppInfo() });
  };

  const toggle = (enabled: boolean) =>
    change(async () => {
      // Turning it on with nowhere to write asks where first; closing the
      // picker leaves it off.
      if (enabled && !backup?.folder) {
        const withFolder = await api.chooseBackupFolder();
        if (!withFolder?.folder) return null;
      }
      return api.setBackupEnabled(enabled);
    });

  const pickRestoreFile = async () => {
    setRestoreStatus(null);
    try {
      const path = await api.pickBackupFile();
      if (path) {
        setRestorePath(path);
        setCode("");
      }
    } catch (err: unknown) {
      setRestoreStatus(String(err));
    }
  };

  const runRestore = async () => {
    if (!restorePath) return;
    setRestoring(true);
    setRestoreStatus(null);
    try {
      const outcome = await api.restoreBackup(restorePath, code);
      if (outcome.kind === "wrongCode") {
        setRestoreStatus(t("settingsSheet.diagnostics.autoBackup.restore.wrongCode"));
        return;
      }
      await reloadAfterImport(outcome.summary);
      setRestorePath(null);
      setCode("");
      setRestoreStatus(
        t("settingsSheet.diagnostics.autoBackup.restore.done", { ...outcome.summary }),
      );
    } catch (err: unknown) {
      setRestoreStatus(String(err));
    } finally {
      setRestoring(false);
    }
  };

  const enabled = backup?.enabled ?? false;
  const frequencyId = `${idPrefix}-frequency`;
  const codeId = `${idPrefix}-code`;
  const lastSuccess =
    backup?.lastSuccessMs != null
      ? t("settingsSheet.diagnostics.autoBackup.lastSuccess", {
          when: timeAgo(backup.lastSuccessMs),
        })
      : null;

  return (
    <div className="settings-field">
      <span className="settings-label">{t("settingsSheet.diagnostics.autoBackup.label")}</span>
      <span className="settings-hint">{t("settingsSheet.diagnostics.autoBackup.hint")}</span>
      <label className="settings-toggle">
        <input
          type="checkbox"
          checked={enabled}
          disabled={backup === null || (needsRecoveryCode && !enabled)}
          onChange={(e) => void toggle(e.target.checked)}
        />
        <span>{t(enabled ? "settingsSheet.toggle.on" : "settingsSheet.toggle.off")}</span>
      </label>
      {needsRecoveryCode && (
        <>
          <span className="settings-hint">
            {t("settingsSheet.diagnostics.autoBackup.needsRecoveryCode")}
          </span>
          <div className="logs-actions">
            <button className="btn-ghost" onClick={() => void setUpRecoveryCode()}>
              {t("settingsSheet.diagnostics.autoBackup.setUpRecoveryCode")}
            </button>
          </div>
        </>
      )}
      {backup?.folder && <code className="settings-path">{backup.folder}</code>}
      <div className="logs-actions">
        <button
          className="btn-ghost"
          disabled={backup === null}
          onClick={() => void change(() => api.chooseBackupFolder())}
        >
          {t("settingsSheet.diagnostics.autoBackup.chooseFolder")}
        </button>
      </div>
      <label className="settings-hint" htmlFor={frequencyId}>
        {t("settingsSheet.diagnostics.autoBackup.frequency.label")}
      </label>
      <select
        id={frequencyId}
        className="settings-select"
        value={backup?.frequency ?? "daily"}
        disabled={backup === null}
        onChange={(e) =>
          void change(() => api.setBackupFrequency(e.target.value as BackupFrequency))
        }
      >
        <option value="daily">{t("settingsSheet.diagnostics.autoBackup.frequency.daily")}</option>
        <option value="weekly">
          {t("settingsSheet.diagnostics.autoBackup.frequency.weekly")}
        </option>
      </select>
      {backup?.lastError ? (
        <p className="settings-hint logs-status" role="status">
          {t("settingsSheet.diagnostics.autoBackup.failed", { reason: backup.lastError })}
          {lastSuccess && (
            <>
              <br />
              {lastSuccess}
            </>
          )}
        </p>
      ) : (
        enabled && (
          <p className="settings-hint logs-status" role="status">
            {lastSuccess ?? t("settingsSheet.diagnostics.autoBackup.never")}
          </p>
        )
      )}
      {error && (
        <p className="settings-hint logs-status" role="alert">
          {error}
        </p>
      )}

      <div className="logs-actions">
        <button
          className="btn-ghost"
          disabled={restoring}
          onClick={() => void pickRestoreFile()}
        >
          {t("settingsSheet.diagnostics.autoBackup.restore.button")}
        </button>
      </div>
      {restorePath && (
        <form
          className="backup-restore"
          onSubmit={(e) => {
            e.preventDefault();
            void runRestore();
          }}
        >
          <code className="settings-path">{restorePath}</code>
          <label className="settings-hint" htmlFor={codeId}>
            {t("settingsSheet.diagnostics.autoBackup.restore.prompt")}
          </label>
          <input
            id={codeId}
            className="settings-text"
            autoComplete="off"
            spellCheck={false}
            value={code}
            onChange={(e) => setCode(e.target.value)}
          />
          <div className="logs-actions">
            <button
              type="submit"
              className="btn"
              disabled={restoring || code.trim() === ""}
            >
              {restoring
                ? t("settingsSheet.diagnostics.autoBackup.restore.restoring")
                : t("settingsSheet.diagnostics.autoBackup.restore.confirm")}
            </button>
            <button
              type="button"
              className="btn-ghost"
              disabled={restoring}
              onClick={() => {
                setRestorePath(null);
                setCode("");
              }}
            >
              {t("common.cancel")}
            </button>
          </div>
        </form>
      )}
      {restoreStatus && (
        <p className="settings-hint logs-status" role="status">
          {restoreStatus}
        </p>
      )}
    </div>
  );
}

/** The fixed choices of "Delete old conversations", in days. */
const AUTO_DELETE_PRESETS = [30, 90, 365] as const;

/** The `<select>` value a saved period shows as. */
function autoDeleteChoice(days: number | null | undefined): string {
  if (days == null) return "never";
  return (AUTO_DELETE_PRESETS as readonly number[]).includes(days) ? String(days) : "custom";
}

/** A Custom entry as a period, or null when it isn't a whole number of days,
 * 1 or more (that also fits the `u32` the Rust side stores it in). */
function parseCustomDays(raw: string): number | null {
  const trimmed = raw.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  const days = Number(trimmed);
  return days >= 1 && days <= 0xffffffff ? days : null;
}

/**
 * Settings → Your data "Delete old conversations": off ("Never") by default.
 * Picking or shortening a period asks first when conversations already
 * qualify, showing how many, and saves nothing until confirmed; the deletion
 * itself happens in Rust (`commands::delete_expired_conversations`, then
 * `crates/app/src/retention.rs` on every unlock and hourly).
 */
export function AutoDeleteSection() {
  const saved = useStore((s) => s.settings?.autoDeleteAfterDays ?? null);
  const saveSettings = useStore((s) => s.saveSettings);
  const forgetConversations = useStore((s) => s.forgetConversations);
  const { t } = useTranslation();
  const idPrefix = useId();
  // What the select shows while a choice isn't saved yet (Custom being typed
  // in, or a period waiting on the confirmation); the saved value otherwise.
  const [draftChoice, setDraftChoice] = useState<string | null>(null);
  const [customDays, setCustomDays] = useState(() =>
    autoDeleteChoice(saved) === "custom" ? String(saved) : "",
  );
  const [invalid, setInvalid] = useState(false);
  const [pending, setPending] = useState<{ days: number; count: number } | null>(null);
  const [busy, setBusy] = useState(false);
  const choice = draftChoice ?? autoDeleteChoice(saved);

  const commit = async (days: number | null) => {
    setBusy(true);
    try {
      await saveSettings({ autoDeleteAfterDays: days });
      // `saveSettings` reports its own failure; only delete once it held.
      if (days !== null && useStore.getState().settings?.autoDeleteAfterDays === days) {
        forgetConversations(await api.deleteExpiredConversations());
      }
    } catch (err: unknown) {
      useStore.setState({ notice: String(err) });
    } finally {
      setBusy(false);
      setPending(null);
      setDraftChoice(null);
    }
  };

  const choose = async (days: number | null) => {
    if (days === saved) {
      setDraftChoice(null);
      return;
    }
    if (days === null) {
      await commit(null);
      return;
    }
    let count: number;
    try {
      count = await api.countExpiredConversations(days);
    } catch (err: unknown) {
      useStore.setState({ notice: String(err) });
      return;
    }
    if (count > 0) setPending({ days, count });
    else await commit(days);
  };

  const onSelect = (value: string) => {
    setInvalid(false);
    if (value === "custom") {
      setDraftChoice("custom");
      return;
    }
    setDraftChoice(value);
    void choose(value === "never" ? null : Number(value));
  };

  const submitCustom = () => {
    // Enter, then the blur the dialog's focus causes, must not ask twice.
    if (busy || pending) return;
    const days = parseCustomDays(customDays);
    if (days === null) {
      setInvalid(true);
      return;
    }
    setInvalid(false);
    void choose(days);
  };

  const labelId = `${idPrefix}-label`;
  const hintId = `${idPrefix}-hint`;
  const errorId = `${idPrefix}-error`;
  return (
    <div className="settings-field">
      <span id={labelId} className="settings-label">
        {t("settingsSheet.autoDelete.label")}
      </span>
      <span id={hintId} className="settings-hint">
        {t("settingsSheet.autoDelete.help")}
      </span>
      <select
        className="settings-select"
        aria-labelledby={labelId}
        aria-describedby={hintId}
        value={choice}
        disabled={busy}
        onChange={(e) => onSelect(e.target.value)}
      >
        <option value="never">{t("settingsSheet.autoDelete.never")}</option>
        <option value="30">{t("settingsSheet.autoDelete.after30")}</option>
        <option value="90">{t("settingsSheet.autoDelete.after90")}</option>
        <option value="365">{t("settingsSheet.autoDelete.after365")}</option>
        <option value="custom">{t("settingsSheet.autoDelete.custom")}</option>
      </select>
      {choice === "custom" && (
        <form
          className="settings-local-row"
          onSubmit={(e) => {
            e.preventDefault();
            submitCustom();
          }}
        >
          <input
            type="text"
            inputMode="numeric"
            value={customDays}
            disabled={busy}
            aria-label={t("settingsSheet.autoDelete.customDays")}
            aria-invalid={invalid}
            aria-describedby={invalid ? errorId : undefined}
            onChange={(e) => {
              setCustomDays(e.target.value);
              setInvalid(false);
            }}
            onBlur={submitCustom}
          />
          <span className="settings-hint">{t("settingsSheet.autoDelete.customDays")}</span>
        </form>
      )}
      {invalid && (
        <p id={errorId} className="model-error" role="alert">
          {t("settingsSheet.autoDelete.customInvalid")}
        </p>
      )}
      {pending && (
        <ConfirmDialog
          title={t("settingsSheet.autoDelete.confirmTitle")}
          message={t("settingsSheet.autoDelete.confirmBody", { count: pending.count })}
          confirmLabel={t("settingsSheet.autoDelete.confirmAction")}
          cancelLabel={t("settingsSheet.autoDelete.cancel")}
          busy={busy}
          onCancel={() => {
            setPending(null);
            setDraftChoice(null);
          }}
          onConfirm={() => void commit(pending.days)}
        />
      )}
    </div>
  );
}

/** The one truly destructive action in the app: erasing everything and
 * starting over. A confirm panel is all that stands in front of it — reaching
 * Settings at all already means every other gate (the app lock) passed. */
function DangerZoneSection() {
  const { t } = useTranslation();
  const [showResetPanel, setShowResetPanel] = useState(false);

  return (
    <SettingsGroup title={t("appLock.settings.dangerZone.summary")}>
      <div className="settings-field">
        {showResetPanel ? (
          <FactoryResetConfirm onCancel={() => setShowResetPanel(false)} />
        ) : (
          <div className="logs-actions">
            <button
              className="btn-quiet btn-danger-quiet"
              onClick={() => setShowResetPanel(true)}
            >
              {t("appLock.settings.dangerZone.eraseButton")}
            </button>
          </div>
        )}
      </div>
    </SettingsGroup>
  );
}

type LogSource = "app" | "engine";

/**
 * A window into what OpenPal is recording. Lets the user set how much detail
 * is logged, read the latest lines (app or AI-engine log), and grab the
 * files for a support request. Everything stays on the machine; secrets are
 * masked by the Rust side before anything is shown or copied.
 */
function LogsSection() {
  const settings = useStore((s) => s.settings);
  const saveSettings = useStore((s) => s.saveSettings);
  const { t } = useTranslation();
  const logLevel = settings?.logLevel ?? "info";

  const [which, setWhich] = useState<LogSource>("app");
  const [text, setText] = useState("");
  const [loading, setLoading] = useState(false);
  const [copied, setCopied] = useState(false);
  const [status, setStatus] = useState<string | null>(null);

  const load = async (target: LogSource) => {
    setLoading(true);
    setStatus(null);
    try {
      setText(await api.getRecentLogs(target, 500));
    } catch (err: unknown) {
      setStatus(String(err));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void load("app");
  }, []);

  const switchTo = (target: LogSource) => {
    setWhich(target);
    void load(target);
  };

  const copy = async () => {
    try {
      await api.copyText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch (err: unknown) {
      setStatus(String(err));
    }
  };

  const exportLogs = async () => {
    try {
      const wrote = await api.exportLogs();
      if (wrote) {
        setStatus(t("settingsSheet.diagnostics.logs.exported"));
      }
      // wrote === false: the dialog was closed, nothing to say.
    } catch (err: unknown) {
      setStatus(String(err));
    }
  };

  const clear = async () => {
    try {
      const removed = await api.clearLogs();
      setStatus(t("settingsSheet.diagnostics.clearedLogs", { count: removed }));
      void load(which);
    } catch (err: unknown) {
      setStatus(String(err));
    }
  };

  return (
    <SettingsGroup title={t("settingsSheet.diagnostics.summary")}>
      <div className="settings-field">
        <span className="settings-label">{t("settingsSheet.diagnostics.logLevel.label")}</span>
        <span className="settings-hint">{t("settingsSheet.diagnostics.logLevel.hint")}</span>
        <select
          className="settings-select"
          value={logLevel}
          onChange={(e) => void saveSettings({ logLevel: e.target.value })}
        >
          <option value="error">{t("settingsSheet.diagnostics.logLevel.error")}</option>
          <option value="warn">{t("settingsSheet.diagnostics.logLevel.warn")}</option>
          <option value="info">{t("settingsSheet.diagnostics.logLevel.info")}</option>
          <option value="debug">{t("settingsSheet.diagnostics.logLevel.debug")}</option>
          <option value="trace">{t("settingsSheet.diagnostics.logLevel.trace")}</option>
        </select>
      </div>

      <div className="settings-field">
        <span className="settings-label">{t("settingsSheet.diagnostics.logs.label")}</span>
        <span className="settings-hint">{t("settingsSheet.diagnostics.logs.hint")}</span>

        <div className="logs-tabs">
          <button
            className={which === "app" ? "is-active" : ""}
            onClick={() => switchTo("app")}
          >
            {t("settingsSheet.diagnostics.logs.tabApp")}
          </button>
          <button
            className={which === "engine" ? "is-active" : ""}
            onClick={() => switchTo("engine")}
          >
            {t("settingsSheet.diagnostics.logs.tabEngine")}
          </button>
        </div>

        <pre className="logs-view">
          {loading
            ? t("settingsSheet.diagnostics.logs.loading")
            : text || t("settingsSheet.diagnostics.logs.empty")}
        </pre>

        {status && <p className="settings-hint logs-status" role="status">{status}</p>}

        <div className="logs-actions">
          <button className="btn-ghost" disabled={loading} onClick={() => void load(which)}>
            {t("settingsSheet.diagnostics.logs.refresh")}
          </button>
          <button className="btn-ghost" disabled={!text} onClick={() => void copy()}>
            {copied ? t("settingsSheet.diagnostics.logs.copied") : t("settingsSheet.diagnostics.logs.copy")}
          </button>
          <button className="btn-ghost" onClick={() => void exportLogs()}>
            {t("settingsSheet.diagnostics.logs.exportLogs")}
          </button>
          <button className="btn-quiet" onClick={() => void clear()}>
            {t("settingsSheet.diagnostics.logs.clear")}
          </button>
        </div>
      </div>
    </SettingsGroup>
  );
}

/**
 * Advanced: the "Experimental features" switch, off by default
 * (`Settings.experimentalFeatures`) and the one gate for everything still
 * being tested. While it's off, every plugin marked experimental is hidden
 * and inactive whatever its own Plugins switch says, and every other
 * experimental feature gates itself on `experimentalFeaturesOn`. Turning it
 * off while any of them is on asks first, naming each one that will stop
 * working; their own choices are kept either way.
 */
export function ExperimentalSection() {
  const { t } = useTranslation();
  const settings = useStore((s) => s.settings);
  const saveSettings = useStore((s) => s.saveSettings);
  const plugins = useStore((s) => s.plugins);
  const pluginDisplayName = usePluginDisplayName();
  const experimentalFeatures = experimentalFeaturesOn(settings);
  const [confirmingOff, setConfirmingOff] = useState(false);
  // What turning the switch off stops, by name. An experimental feature that
  // isn't a plugin but has its own on/off choice joins this list.
  const inUse = plugins
    .filter((p) => p.experimental && p.enabled)
    .map((p) => pluginDisplayName(p));

  const toggle = (on: boolean) => {
    if (!on && inUse.length > 0) {
      setConfirmingOff(true);
      return;
    }
    void saveSettings({ experimentalFeatures: on });
  };

  return (
    <SettingsGroup title={t("settingsSheet.experimental.summary")}>
      <div className="settings-field">
        <span className="settings-hint">{t("settingsSheet.experimental.hint")}</span>
        <label className="settings-toggle">
          <input
            type="checkbox"
            checked={experimentalFeatures}
            onChange={(e) => toggle(e.target.checked)}
          />
          <span>{t("settingsSheet.experimental.enableLabel")}</span>
        </label>
      </div>

      {confirmingOff && (
        <ConfirmDialog
          title={t("settingsSheet.experimental.disableDialog.title")}
          message={t("settingsSheet.experimental.disableDialog.body", {
            features: inUse.join(", "),
          })}
          confirmLabel={t("settingsSheet.experimental.disableDialog.confirm")}
          confirmVariant="primary"
          onCancel={() => setConfirmingOff(false)}
          onConfirm={() => {
            setConfirmingOff(false);
            void saveSettings({ experimentalFeatures: false });
          }}
        />
      )}
    </SettingsGroup>
  );
}

/**
 * Settings → Devices → Sync (docs/SYNC_PAIRING_PROTOCOL.md, Phase 2).
 * Two states, gated on whether this device has a sync group yet
 * (`sync_group_secret()` returning `null` or not): create-a-group/join-a-group
 * options, or the device roster with an "Add a device" button. The two
 * pairing flows themselves are separate dialogs (AddDeviceDialog.tsx,
 * JoinGroupDialog.tsx) driven by store state — see `startAddDevice`/
 * `openJoinGroup` in state/store.ts; this section only reloads its own
 * (non-streamed) `syncGroupSecret`/`syncDeviceList` snapshot once either
 * dialog closes, in case pairing just changed either.
 */
function SyncSection() {
  const { t } = useTranslation();
  const settings = useStore((s) => s.settings);
  const saveSettings = useStore((s) => s.saveSettings);
  const [groupSecret, setGroupSecret] = useState<string | null>(null);
  const [devices, setDevices] = useState<DeviceSummary[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);

  // Create-group form.
  const [newDeviceLabel, setNewDeviceLabel] = useState("");
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  /** The `group_secret` `sync_group_create` just returned — shown once with
   * a copy button, then never again (v1 scope: no re-show/QR later). */
  const [justCreatedSecret, setJustCreatedSecret] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  // Remove-device confirm (inline swap-in panel, same shape as
  // FactoryResetConfirm rather than a native confirm()).
  const [removingId, setRemovingId] = useState<string | null>(null);
  const [removeBusy, setRemoveBusy] = useState(false);
  const [removeError, setRemoveError] = useState<string | null>(null);

  // Data-sync toggle + optional relay server override (Phase 3). The URL
  // field commits on blur rather than per keystroke, like this file's other
  // single-field text commits; empty input is saved as `null` ("use the
  // default"), matching the Rust side's `Option<String>`.
  const [relayUrl, setRelayUrl] = useState(settings?.syncRelayUrl ?? "");

  const addDeviceOpen = useStore((s) => s.addDeviceOpen);
  const joinGroupOpen = useStore((s) => s.joinGroupOpen);
  const startAddDevice = useStore((s) => s.startAddDevice);
  const openJoinGroup = useStore((s) => s.openJoinGroup);

  const load = async () => {
    setLoadError(null);
    try {
      const secret = await api.syncGroupSecret();
      setGroupSecret(secret);
      setDevices(secret ? await api.syncDeviceList() : []);
    } catch (err: unknown) {
      setLoadError(String(err));
    }
  };

  useEffect(() => {
    void load();
  }, []);

  // Reload after either pairing dialog closes, in case it just succeeded —
  // harmless to also reload after a plain reject/cancel.
  const wasAddDeviceOpen = useRef(addDeviceOpen);
  const wasJoinGroupOpen = useRef(joinGroupOpen);
  useEffect(() => {
    const justClosed =
      (wasAddDeviceOpen.current && !addDeviceOpen) || (wasJoinGroupOpen.current && !joinGroupOpen);
    wasAddDeviceOpen.current = addDeviceOpen;
    wasJoinGroupOpen.current = joinGroupOpen;
    if (justClosed) void load();
  }, [addDeviceOpen, joinGroupOpen]);

  const createGroup = async () => {
    const label = newDeviceLabel.trim();
    if (!label || creating) return;
    setCreating(true);
    setCreateError(null);
    try {
      const secret = await api.syncGroupCreate(label);
      setJustCreatedSecret(secret);
      setGroupSecret(secret);
      setDevices(await api.syncDeviceList());
    } catch (err: unknown) {
      setCreateError(String(err));
    } finally {
      setCreating(false);
    }
  };

  const copySecret = async () => {
    if (!justCreatedSecret) return;
    try {
      await navigator.clipboard.writeText(justCreatedSecret);
      setCopied(true);
    } catch {
      // Clipboard permission denied/unavailable — the secret is still
      // shown as plain, selectable text; nothing else to do here.
    }
  };

  const removeDevice = async (deviceId: string) => {
    setRemoveBusy(true);
    setRemoveError(null);
    try {
      await api.syncDeviceRemove(deviceId);
      setRemovingId(null);
      setDevices(await api.syncDeviceList());
    } catch (err: unknown) {
      setRemoveError(String(err));
    } finally {
      setRemoveBusy(false);
    }
  };

  const removingDevice = devices.find((d) => d.deviceId === removingId);

  return (
    <SettingsGroup title={t("settingsSheet.sync.summary")}>
      {loadError && <p className="model-error" role="alert">{loadError}</p>}

      {justCreatedSecret ? (
        <div className="settings-field">
          <span className="settings-label">{t("settingsSheet.sync.create.doneLabel")}</span>
          <div className="pairing-card">
            <p className="pairing-code-hint">{t("settingsSheet.sync.create.saveWarning")}</p>
            <div className="pairing-code">{justCreatedSecret}</div>
          </div>
          <div className="logs-actions">
            <button className="btn-ghost" onClick={() => void copySecret()}>
              {copied ? t("settingsSheet.sync.create.copied") : t("settingsSheet.sync.create.copy")}
            </button>
            <button
              className="btn"
              onClick={() => {
                setJustCreatedSecret(null);
                setCopied(false);
              }}
            >
              {t("settingsSheet.sync.create.done")}
            </button>
          </div>
        </div>
      ) : groupSecret === null ? (
        <>
          <div className="settings-field">
            <span className="settings-label">{t("settingsSheet.sync.create.label")}</span>
            <span className="settings-hint">{t("settingsSheet.sync.create.hint")}</span>
            <div className="settings-local-row">
              <input
                value={newDeviceLabel}
                placeholder={t("settingsSheet.sync.create.labelPlaceholder")}
                onChange={(e) => setNewDeviceLabel(e.target.value)}
              />
              <button
                className="btn-ghost"
                disabled={!newDeviceLabel.trim() || creating}
                onClick={() => void createGroup()}
              >
                {creating
                  ? t("settingsSheet.sync.create.creating")
                  : t("settingsSheet.sync.create.button")}
              </button>
            </div>
            {createError && <p className="model-error" role="alert">{createError}</p>}
          </div>

          <div className="settings-field">
            <span className="settings-label">{t("settingsSheet.sync.join.label")}</span>
            <span className="settings-hint">{t("settingsSheet.sync.join.hint")}</span>
            <div className="logs-actions">
              <button className="btn-ghost" onClick={() => openJoinGroup()}>
                {t("settingsSheet.sync.join.button")}
              </button>
            </div>
          </div>
        </>
      ) : (
        <>
          <div className="settings-field">
            <span className="settings-label">{t("settingsSheet.sync.devices.label")}</span>
            <ul className="settings-hf-files">
              {devices.map((d) => (
                <li key={d.deviceId}>
                  <div className="settings-hf-file-info">
                    <span className="settings-hf-file-name">{d.label}</span>
                    {d.isSelf && (
                      <span className="settings-hf-file-meta">
                        {t("settingsSheet.sync.devices.thisDevice")}
                      </span>
                    )}
                  </div>
                  {!d.isSelf && (
                    <button
                      className="btn-quiet btn-danger-quiet"
                      onClick={() => {
                        setRemoveError(null);
                        setRemovingId(d.deviceId);
                      }}
                    >
                      {t("settingsSheet.sync.devices.remove")}
                    </button>
                  )}
                </li>
              ))}
            </ul>

            {removingDevice && (
              <div className="model-confirm">
                <p className="model-confirm-text">
                  {t("settingsSheet.sync.devices.removeConfirm", { label: removingDevice.label })}
                </p>
                {removeError && <p className="model-error" role="alert">{removeError}</p>}
                <div className="model-confirm-actions">
                  <button
                    className="btn-quiet btn-danger-quiet"
                    disabled={removeBusy}
                    onClick={() => void removeDevice(removingDevice.deviceId)}
                  >
                    {removeBusy
                      ? t("settingsSheet.sync.devices.removing")
                      : t("settingsSheet.sync.devices.removeButton")}
                  </button>
                  <button
                    className="btn-quiet"
                    disabled={removeBusy}
                    onClick={() => setRemovingId(null)}
                  >
                    {t("common.cancel")}
                  </button>
                </div>
              </div>
            )}
          </div>

          <div className="settings-field">
            <div className="logs-actions">
              <button className="btn-ghost" onClick={() => void startAddDevice()}>
                {t("settingsSheet.sync.addDevice")}
              </button>
            </div>
          </div>
        </>
      )}

      <div className="settings-field">
        <span className="settings-label">{t("settingsSheet.sync.dataSync.label")}</span>
        <span className="settings-hint">{t("settingsSheet.sync.dataSync.hint")}</span>
        <label className="settings-toggle">
          <input
            type="checkbox"
            checked={settings?.syncEnabled ?? false}
            onChange={(e) => void saveSettings({ syncEnabled: e.target.checked })}
          />
          <span>
            {t(settings?.syncEnabled ? "settingsSheet.toggle.on" : "settingsSheet.toggle.off")}
          </span>
        </label>
      </div>

      {settings?.syncEnabled && (
        <label className="settings-field">
          <span className="settings-label">{t("settingsSheet.sync.dataSync.relayUrl.label")}</span>
          <span className="settings-hint">{t("settingsSheet.sync.dataSync.relayUrl.hint")}</span>
          <input
            value={relayUrl}
            placeholder={t("settingsSheet.sync.dataSync.relayUrl.placeholder")}
            onChange={(e) => setRelayUrl(e.target.value)}
            onBlur={() => void saveSettings({ syncRelayUrl: relayUrl.trim() || null })}
          />
        </label>
      )}
    </SettingsGroup>
  );
}

const PlayGlyph = () => (
  <svg width="11" height="11" viewBox="0 0 12 12" aria-hidden="true">
    <path d="M3 2l7 4-7 4z" fill="currentColor" />
  </svg>
);

const StopGlyph = () => (
  <svg width="11" height="11" viewBox="0 0 12 12" aria-hidden="true">
    <rect x="2.5" y="2.5" width="7" height="7" rx="1" fill="currentColor" />
  </svg>
);

/**
 * Pick the read-aloud voice, or add another. Lists the Voice plugin's catalog
 * grouped by language; installed voices are selectable, the rest show a sized
 * Download button that installs then selects them. Every voice (installed or
 * not) has a play button that previews a short remote sample. Only mounted when
 * the Voice add-on is ready, so listVoices always has a running service.
 */
function VoicePicker() {
  const voiceId = useStore((s) => s.settings?.voiceId ?? "en_US-amy-medium");
  const saveSettings = useStore((s) => s.saveSettings);
  const { t } = useTranslation();

  const [voices, setVoices] = useState<VoiceInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [downloadingId, setDownloadingId] = useState<string | null>(null);
  const [previewingId, setPreviewingId] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    api
      .listVoices()
      .then((v) => alive && setVoices(v))
      .catch((err: unknown) => alive && setError(String(err)));
    return () => {
      alive = false;
    };
  }, []);

  // Stop a sample that's still playing if the sheet is closed mid-preview.
  const previewingRef = useRef(false);
  previewingRef.current = previewingId !== null;
  useEffect(
    () => () => {
      if (previewingRef.current) void api.stopSpeaking();
    },
    [],
  );

  const select = (id: string) => void saveSettings({ voiceId: id });

  const preview = async (v: VoiceInfo) => {
    if (previewingId === v.id) {
      await api.stopSpeaking();
      setPreviewingId(null);
      return;
    }
    if (!v.previewUrl) return;
    setError(null);
    setPreviewingId(v.id);
    try {
      // Resolves when the sample finishes or stopSpeaking cuts it.
      await api.previewVoice(v.previewUrl);
    } catch (err: unknown) {
      setError(String(err));
    } finally {
      setPreviewingId((cur) => (cur === v.id ? null : cur));
    }
  };

  const download = async (id: string) => {
    if (downloadingId) return;
    setDownloadingId(id);
    setError(null);
    try {
      setVoices(await api.downloadVoice(id));
      // A freshly downloaded voice becomes the active one.
      await saveSettings({ voiceId: id });
    } catch (err: unknown) {
      setError(String(err));
    } finally {
      setDownloadingId(null);
    }
  };

  if (!voices) {
    return (
      <p className="settings-voice-loading">
        {error ?? t("settingsSheet.voice.loadingVoices")}
      </p>
    );
  }

  // Group by language, preserving the catalog's order.
  const groups: { language: string; items: VoiceInfo[] }[] = [];
  for (const v of voices) {
    const g = groups.find((x) => x.language === v.language);
    if (g) g.items.push(v);
    else groups.push({ language: v.language, items: [v] });
  }

  // The active voice was deleted (or is a stale id) — it's in no row, so without
  // this the picker would show nothing selected and read-aloud would just fail.
  const currentMissing = !voices.some((v) => v.id === voiceId);

  return (
    <div className="voice-picker" role="radiogroup" aria-label={t("settingsSheet.voice.title")}>
      {currentMissing && (
        <p className="voice-picker-notice">{t("settingsSheet.voice.missingNotice")}</p>
      )}
      {groups.map((g) => (
        <div key={g.language} className="voice-group">
          <div className="voice-group-title">{g.language}</div>
          {g.items.map((v) => {
            const selected = v.id === voiceId;
            const meta = [v.gender, v.quality, v.sizeMb ? `~${v.sizeMb} MB` : ""]
              .filter(Boolean)
              .join(" · ");
            return (
              <div
                key={v.id}
                className={`voice-row${selected ? " is-selected" : ""}${
                  v.installed ? " is-installed" : ""
                }`}
                onClick={v.installed ? () => select(v.id) : undefined}
                role={v.installed ? "radio" : undefined}
                aria-checked={v.installed ? selected : undefined}
                tabIndex={v.installed ? 0 : undefined}
                onKeyDown={
                  v.installed
                    ? (e) => {
                        if (e.key === "Enter" || e.key === " ") {
                          e.preventDefault();
                          select(v.id);
                        }
                      }
                    : undefined
                }
              >
                <span className="voice-radio" aria-hidden>
                  {selected ? "●" : v.installed ? "○" : ""}
                </span>
                <div className="voice-row-info">
                  <span className="voice-row-name">{v.name}</span>
                  {meta && <span className="voice-row-meta">{meta}</span>}
                </div>
                {v.previewUrl && (
                  <button
                    className="voice-preview-btn"
                    title={
                      previewingId === v.id
                        ? t("settingsSheet.voice.stop")
                        : t("settingsSheet.voice.preview")
                    }
                    aria-label={
                      previewingId === v.id
                        ? t("settingsSheet.voice.stopPreview")
                        : t("settingsSheet.voice.previewName", { name: v.name })
                    }
                    disabled={previewingId !== null && previewingId !== v.id}
                    onClick={(e) => {
                      e.stopPropagation();
                      void preview(v);
                    }}
                  >
                    {previewingId === v.id ? <StopGlyph /> : <PlayGlyph />}
                  </button>
                )}
                {!v.installed && (
                  <button
                    className="btn-quiet"
                    disabled={downloadingId !== null || previewingId !== null}
                    onClick={(e) => {
                      e.stopPropagation();
                      void download(v.id);
                    }}
                  >
                    {downloadingId === v.id
                      ? t("settingsSheet.voice.downloading")
                      : t("settingsSheet.voice.download")}
                  </button>
                )}
              </div>
            );
          })}
        </div>
      ))}
      {error && <p className="model-error" role="alert">{error}</p>}
    </div>
  );
}
