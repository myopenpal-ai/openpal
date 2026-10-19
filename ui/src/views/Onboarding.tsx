import { useEffect, useId, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import ConfirmDialog from "../components/ConfirmDialog";
import PinPad from "../components/PinPad";
import ProgressRing from "../components/ProgressRing";
import ThemePicker from "../components/ThemePicker";
import { api } from "../lib/api";
import { hasNativeNotifications } from "../lib/appEvents";
import {
  formatBytes,
  formatEta,
  formatSpeed,
  isAutoRecommendable,
  ramLabel,
  totalModelSizeBytes,
} from "../lib/format";
import { normalizeTheme } from "../lib/theme";
import type { ModelInfo } from "../lib/types";
import { useStore } from "../state/store";

type Scene =
  | "welcome"
  | "name"
  | "appearance"
  | "notifications"
  | "scan"
  | "recommend"
  | "download"
  | "security"
  | "ready";

/** A trailing-edge throttle for a polite live-region announcement: updates
 * right away if `intervalMs` has already elapsed since the last flush,
 * otherwise schedules one update for the latest value at the interval
 * boundary — so a value that changes every second or so (byte/speed/ETA
 * download progress) doesn't re-announce on every single tick. */
function useThrottledStatus(text: string, intervalMs: number): string {
  const [announced, setAnnounced] = useState(text);
  const lastFlush = useRef(0);

  useEffect(() => {
    const now = Date.now();
    const elapsed = now - lastFlush.current;
    if (elapsed >= intervalMs) {
      lastFlush.current = now;
      setAnnounced(text);
      return;
    }
    const id = window.setTimeout(() => {
      lastFlush.current = Date.now();
      setAnnounced(text);
    }, intervalMs - elapsed);
    return () => window.clearTimeout(id);
  }, [text, intervalMs]);

  return announced;
}

export default function Onboarding() {
  const { t } = useTranslation();
  const [scene, setScene] = useState<Scene>("welcome");
  // Scene transitions fully unmount/remount the <section> below with no
  // focus programmatically moved — keyboard/AT users don't land on the new
  // step's content. Focus the scene's own heading (or the section itself as
  // a fallback, for a scene whose heading isn't yet wired up) each time.
  const sceneRef = useRef<HTMLElement>(null);
  useEffect(() => {
    const el = sceneRef.current;
    if (!el) return;
    const heading = el.querySelector<HTMLElement>("h1");
    (heading ?? el).focus({ preventScroll: true });
  }, [scene]);
  const [chosenId, setChosenId] = useState<string | null>(null);
  const [showAlternatives, setShowAlternatives] = useState(false);
  // Opening the list swaps the model card for it, and closing swaps it back —
  // which unmounts whichever control had keyboard focus. Hand focus to the
  // counterpart (the first row / the "pick a different one" button) instead of
  // letting it fall to <body>. Only after the user toggled it, never on mount.
  const pickDifferentRef = useRef<HTMLButtonElement>(null);
  const firstAltRef = useRef<HTMLButtonElement>(null);
  const alternativesToggled = useRef(false);
  const toggleAlternatives = (open: boolean) => {
    alternativesToggled.current = true;
    setShowAlternatives(open);
  };
  useEffect(() => {
    if (!alternativesToggled.current) return;
    (showAlternatives ? firstAltRef : pickDifferentRef).current?.focus();
  }, [showAlternatives]);
  // Skipping the whole setup asks first ("Skip setup?"); skipping one step never
  // does. The question is about the scene it was opened on: if the scene moves
  // on by itself underneath it (the download finishes and the engine comes up),
  // what it says no longer applies — so it is derived from that scene rather
  // than kept as a flag. `skipping` is the window while the confirmed skip is
  // saving, during which it stays up wherever the scene goes.
  const [skipFrom, setSkipFrom] = useState<Scene | null>(null);
  const [skipping, setSkipping] = useState(false);
  const confirmingSkip = skipping || skipFrom === scene;
  const askToSkipSetup = () => setSkipFrom(scene);
  // Skipping the model setup stopped a download that was in flight.
  const [stoppedDownload, setStoppedDownload] = useState(false);

  const report = useStore((s) => s.report);
  const models = useStore((s) => s.models);
  const engine = useStore((s) => s.engine);
  const downloads = useStore((s) => s.downloads);
  const downloadErrors = useStore((s) => s.downloadErrors);
  const startDownload = useStore((s) => s.startDownload);
  const cancelDownload = useStore((s) => s.cancelDownload);
  const useModel = useStore((s) => s.useModel);
  const completeOnboarding = useStore((s) => s.completeOnboarding);
  const saveSettings = useStore((s) => s.saveSettings);
  const savedUserName = useStore((s) => s.settings?.userName ?? "");
  const [autoCheckUpdates, setAutoCheckUpdates] = useState(false);
  const [nameDraft, setNameDraft] = useState(savedUserName);
  const [savingName, setSavingName] = useState(false);

  // "appearance" scene. The checked option is read straight from the store, never
  // kept here: a theme saved before quitting mid-onboarding is the one shown
  // checked on the way back, and a pick that couldn't be saved is taken back on
  // screen along with the store. A missing or unrecognised stored value reads as
  // System, which is also what is checked when nothing has been picked.
  const theme = useStore((s) => normalizeTheme(s.settings?.theme));
  const setTheme = useStore((s) => s.setTheme);
  // The group is named by the scene's heading and described by the line under it.
  const appearanceTitleId = useId();
  const appearanceSubId = useId();

  // "security" scene: an optional, skippable offer to set an app-lock PIN.
  // `RecoverySetup` always runs before `Onboarding` (see App.tsx's gate
  // order), so a recovery code has already been saved by this point —
  // "offer" can truthfully remind the user of that as their PIN fallback.
  const [securityStep, setSecurityStep] = useState<"offer" | "create" | "confirm">("offer");
  const [pendingPin, setPendingPin] = useState<string | null>(null);
  const [pinFlowError, setPinFlowError] = useState<string | null>(null);
  const [savingPin, setSavingPin] = useState(false);

  const recommendedId = report?.recommendation.primaryModelId ?? null;
  const effectiveId = chosenId ?? recommendedId;
  const chosen: ModelInfo | undefined = useMemo(
    () => models.find((m) => m.spec.id === effectiveId),
    [models, effectiveId],
  );

  // "Pick a different one" lists the recommended model first, then every other
  // model that fits, so the recommendation reads as one option among equals.
  const ranked = report?.recommendation.ranked ?? [];
  const pickerRows = [
    ...ranked.filter((r) => r.modelId === recommendedId),
    ...ranked.filter(
      (r) => r.modelId !== recommendedId && r.fit !== "tooBig" && isAutoRecommendable(r.fit),
    ),
  ];

  const progress = effectiveId ? downloads[effectiveId] : undefined;
  const downloadError = effectiveId ? downloadErrors[effectiveId] : undefined;
  const installed = chosen?.status.type === "installed";
  // The chosen model is actually mid-download (not already installed).
  const downloadInFlight = chosen !== undefined && !installed && progress !== undefined;
  // A model is loaded or loading — what the last screen, and the wording of
  // "Skip setup?", can truthfully say about chatting.
  const modelOnTheWay = engine.type === "running" || engine.type === "starting";
  const skipOutcome = downloadInFlight ? "downloading" : modelOnTheWay ? "modelReady" : "noModel";
  // The last screen names the model the engine is actually loading or running,
  // which can differ from the one recommended.
  const engineModel =
    engine.type === "running" || engine.type === "starting"
      ? models.find((m) => m.spec.id === engine.data.modelId)
      : undefined;
  const readyName = engineModel?.spec.name ?? chosen?.spec.name ?? t("onboarding.ready.yourAi");

  // Plain-text mirror of the visible byte/speed/ETA readout below, throttled
  // to a screen reader every few seconds instead of on every progress tick
  // (finding O2) — the visible text still updates in real time.
  const progressAnnouncementText =
    !installed && progress && chosen
      ? [
          t("onboarding.download.progressOf", {
            received: formatBytes(progress.receivedBytes),
            total: formatBytes(progress.totalBytes || totalModelSizeBytes(chosen.spec)),
          }),
          progress.bytesPerSec > 0 ? formatSpeed(progress.bytesPerSec) : null,
          progress.etaSeconds > 0 ? formatEta(progress.etaSeconds) : null,
        ]
          .filter(Boolean)
          .join(" · ")
      : "";
  const throttledProgressText = useThrottledStatus(progressAnnouncementText, 3000);

  // Download finished -> wake the engine. Engine running -> offer a PIN
  // before the final "ready" celebration.
  useEffect(() => {
    if (scene !== "download") return;
    if (engine.type === "running") {
      setScene("security");
      return;
    }
    if (installed && !progress && effectiveId && engine.type === "stopped") {
      void useModel(effectiveId);
    }
  }, [scene, installed, progress, effectiveId, engine.type, useModel]);

  const begin = (id: string) => {
    setChosenId(id);
    setScene("download");
    const model = models.find((m) => m.spec.id === id);
    if (model?.status.type !== "installed") {
      startDownload(id);
    }
  };

  const continueWithName = async (name: string | null) => {
    if (savingName || (name !== null && name.trim() === "")) return;
    setSavingName(true);
    await saveSettings({ userName: name?.trim() || null });
    setSavingName(false);
    setScene("appearance");
  };

  // The appearance step has nothing to save on the way out: a pick persisted
  // itself the moment it was made (`setTheme`), and Continue or Skip with nothing
  // picked leaves the theme as it was. So neither button writes or waits on
  // anything, and both are live from the first frame. What comes next is the
  // notifications step, except in a build with no OS notifications (Android and
  // iOS today; see NotificationsSection in Settings): there it is left out.
  const nativeNotifications = useStore((s) => hasNativeNotifications(s.appInfo));
  // The step's words for Android, which has no desktop and no model events.
  const notificationsContext = useStore((s) => (s.appInfo?.isAndroid ? "mobile" : undefined));
  const continueFromAppearance = () => setScene(nativeNotifications ? "notifications" : "scan");

  // The notifications step: placed before the model setup so the choice is made
  // before the first multi-gigabyte download, the very moment "tell me when
  // it's done" is useful. Turning them on goes through the store's
  // `setDesktopNotifications`, which saves the choice at once and then asks the
  // OS for permission (on macOS that is its one-time "Allow notifications?"
  // prompt, which is why it only ever follows this click) — and the scene moves
  // on without waiting for either. If the OS says no, the switch stays on and
  // Settings says so. Skipping writes nothing:
  // the switch stays off, as it is for everyone who never sees this step.
  const setDesktopNotifications = useStore((s) => s.setDesktopNotifications);
  const continueFromNotifications = () => setScene("scan");
  const enableNotifications = () => {
    void setDesktopNotifications(true);
    continueFromNotifications();
  };

  // Skip just the model setup (scan → recommend → download) and carry on to the
  // optional steps. A download in flight is stopped (its partial file is kept,
  // so the Models page can resume it) and the last screen says so; it also says
  // there is no model yet, instead of claiming one is awake.
  const skipModelSetup = () => {
    if (chosen && downloadInFlight) {
      void cancelDownload(chosen.spec.id);
      setStoppedDownload(true);
    }
    setScene("security");
  };

  // Skip the whole setup — only ever reached through the confirmation dialog.
  // Completes onboarding so App.tsx unmounts Onboarding and the shell takes
  // over. A download in flight is stopped only once that is saved:
  // `completeOnboarding` swallows a failure (the store records it as a notice —
  // which nothing renders yet) and leaves onboarding open, where the download
  // should still be running; and cancelling first would race the store's
  // post-download settings re-read against the save, which could flip
  // `onboardingCompleted` back.
  const finishOnboardingEarly = async () => {
    setSkipping(true);
    const droppedId = chosen && downloadInFlight ? chosen.spec.id : null;
    try {
      await completeOnboarding();
      if (droppedId && useStore.getState().settings?.onboardingCompleted) {
        void cancelDownload(droppedId);
      }
    } finally {
      // Only still mounted if saving failed: hand the scene back.
      setSkipping(false);
      setSkipFrom(null);
    }
  };

  return (
    <div className="onboarding" data-tauri-drag-region>
      <div className="ob-glow" aria-hidden />

      {scene === "welcome" && (
        <section className="ob-scene ob-welcome" key="welcome" ref={sceneRef} tabIndex={-1}>
          <p className="ob-brand ob-appear">OpenPal</p>
          <h1 className="ob-display ob-appear" tabIndex={-1} style={{ animationDelay: "0.12s" }}>
            {t("onboarding.welcome.headline1")}
            <br />
            {t("onboarding.welcome.headline2")}
            <br />
            {t("onboarding.welcome.headline3")}
          </h1>
          <ul className="ob-perks ob-appear" style={{ animationDelay: "0.28s" }}>
            {(t("onboarding.welcome.perks", { returnObjects: true }) as string[]).map((perk) => (
              <li key={perk} className="ob-perk">
                {perk}
              </li>
            ))}
          </ul>
          <p className="ob-note ob-appear" style={{ animationDelay: "0.38s" }}>
            {t("onboarding.welcome.note")}
          </p>
          <div className="ob-appear" style={{ animationDelay: "0.44s" }}>
            <button className="btn btn-big" onClick={() => setScene("name")}>
              {t("onboarding.welcome.start")}
            </button>
          </div>
          <SkipButtons appear={0.6} onSkipSetup={askToSkipSetup} />
        </section>
      )}

      {scene === "name" && (
        <section className="ob-scene" key="name" ref={sceneRef} tabIndex={-1}>
          <p className="ob-brand ob-appear">OpenPal</p>
          <h1 className="ob-title ob-appear" tabIndex={-1} style={{ animationDelay: "0.1s" }}>
            {t("onboarding.name.title")}
          </h1>
          <p className="ob-sub ob-appear" style={{ animationDelay: "0.2s" }}>
            {t("onboarding.name.sub")}
          </p>
          <form
            className="ob-name-form ob-appear"
            style={{ animationDelay: "0.3s" }}
            onSubmit={(event) => {
              event.preventDefault();
              void continueWithName(nameDraft);
            }}
          >
            <label className="ob-name-label" htmlFor="onboarding-name">
              <span>{t("onboarding.name.label")}</span>
              <input
                id="onboarding-name"
                className="ob-name-input"
                type="text"
                value={nameDraft}
                onChange={(event) => setNameDraft(event.target.value)}
                placeholder={t("onboarding.name.placeholder")}
                autoComplete="name"
                autoCapitalize="words"
                maxLength={80}
              />
            </label>
            <div className="ob-name-actions">
              <button
                className="btn btn-big"
                type="submit"
                disabled={savingName || nameDraft.trim() === ""}
              >
                {t("onboarding.name.continue")}
              </button>
              <SkipButtons
                disabled={savingName}
                onSkipStep={() => void continueWithName(null)}
                onSkipSetup={askToSkipSetup}
              />
            </div>
          </form>
        </section>
      )}

      {scene === "appearance" && (
        <section className="ob-scene" key="appearance" ref={sceneRef} tabIndex={-1}>
          <p className="ob-brand ob-appear">OpenPal</p>
          <h1
            id={appearanceTitleId}
            className="ob-title ob-appear"
            tabIndex={-1}
            style={{ animationDelay: "0.1s" }}
          >
            {t("onboarding.appearance.title")}
          </h1>
          <p id={appearanceSubId} className="ob-sub ob-appear" style={{ animationDelay: "0.2s" }}>
            {t("onboarding.appearance.sub")}
          </p>
          {/* One block, one delay: the options and the way on enter together. */}
          <div className="ob-appearance ob-appear" style={{ animationDelay: "0.3s" }}>
            <ThemePicker
              variant="cards"
              value={theme}
              onChange={(next) => void setTheme(next)}
              labelledBy={appearanceTitleId}
              describedBy={appearanceSubId}
              options={[
                {
                  value: "system",
                  label: t("onboarding.appearance.system"),
                  hint: t("onboarding.appearance.systemHint"),
                },
                { value: "light", label: t("onboarding.appearance.light") },
                { value: "dark", label: t("onboarding.appearance.dark") },
              ]}
            />
            <div className="ob-appearance-actions">
              <button className="btn btn-big" onClick={continueFromAppearance}>
                {t("onboarding.appearance.continue")}
              </button>
              <SkipButtons onSkipStep={continueFromAppearance} onSkipSetup={askToSkipSetup} />
            </div>
          </div>
        </section>
      )}

      {scene === "notifications" && (
        <section className="ob-scene" key="notifications" ref={sceneRef} tabIndex={-1}>
          <p className="ob-brand ob-appear">OpenPal</p>
          <h1 className="ob-title ob-appear" tabIndex={-1} style={{ animationDelay: "0.1s" }}>
            {t("onboarding.notifications.title")}
          </h1>
          <p className="ob-sub ob-appear" style={{ animationDelay: "0.2s" }}>
            {t("onboarding.notifications.sub", { context: notificationsContext })}
          </p>
          <div className="ob-name-actions ob-appear" style={{ animationDelay: "0.3s" }}>
            <button className="btn btn-big" onClick={enableNotifications}>
              {t("onboarding.notifications.enable")}
            </button>
            <SkipButtons onSkipStep={continueFromNotifications} onSkipSetup={askToSkipSetup} />
          </div>
        </section>
      )}

      {scene === "scan" && (
        <section className="ob-scene" key="scan" ref={sceneRef} tabIndex={-1}>
          <h1 className="ob-title ob-appear" tabIndex={-1}>{t("onboarding.scan.title")}</h1>
          {report ? (
            <>
              <div className="ob-stats">
                <StatCard
                  delay={0.15}
                  label={t("onboarding.scan.chip")}
                  value={report.hardware.chip}
                />
                <StatCard
                  delay={0.35}
                  label={t("onboarding.scan.memory")}
                  value={ramLabel(report.hardware.totalRamBytes)}
                />
                <StatCard
                  delay={0.55}
                  label={t("onboarding.scan.graphics")}
                  value={
                    report.hardware.hasMetal
                      ? t("onboarding.scan.graphicsReady")
                      : t("onboarding.scan.graphicsStandard")
                  }
                  good={report.hardware.hasMetal}
                />
                <StatCard
                  delay={0.75}
                  label={t("onboarding.scan.freeSpace")}
                  value={formatBytes(report.hardware.freeDiskBytes)}
                />
              </div>
              <p className="ob-headline ob-appear" style={{ animationDelay: "1.1s" }}>
                {report.recommendation.headline}
              </p>
              <div className="ob-appear" style={{ animationDelay: "1.3s" }}>
                <button className="btn btn-big" onClick={() => setScene("recommend")}>
                  {t("onboarding.scan.showMatch")}
                </button>
              </div>
            </>
          ) : (
            <p className="ob-sub ob-appear">{t("onboarding.scan.loading")}</p>
          )}
          <SkipButtons appear={1.5} onSkipStep={skipModelSetup} onSkipSetup={askToSkipSetup} />
        </section>
      )}

      {scene === "recommend" && report && (
        <section className="ob-scene" key="recommend" ref={sceneRef} tabIndex={-1}>
          {chosen ? (
            <>
              <p className="ob-eyebrow ob-appear">{t("onboarding.recommend.madeForMac")}</p>
              {showAlternatives ? (
                <>
                  <div id="ob-alts" className="ob-alts ob-appear">
                    {pickerRows.map((row, i) => {
                      const m = models.find((x) => x.spec.id === row.modelId);
                      if (!m) return null;
                      return (
                        <button
                          key={row.modelId}
                          ref={i === 0 ? firstAltRef : undefined}
                          className="ob-alt-row"
                          aria-current={row.modelId === chosen.spec.id ? "true" : undefined}
                          onClick={() => {
                            setChosenId(row.modelId);
                            toggleAlternatives(false);
                          }}
                        >
                          <span className="ob-alt-title">
                            <span className="ob-alt-name">{m.spec.name}</span>
                            {row.modelId === recommendedId && (
                              <span className="badge badge-accent">
                                {t("onboarding.recommend.recommendedTag")}
                              </span>
                            )}
                          </span>
                          <span className="ob-alt-why">{row.why}</span>
                          <span className="badge badge-neutral">
                            {formatBytes(totalModelSizeBytes(m.spec))}
                          </span>
                        </button>
                      );
                    })}
                  </div>
                  <button
                    className="btn-quiet ob-appear"
                    aria-expanded
                    aria-controls="ob-alts"
                    onClick={() => toggleAlternatives(false)}
                  >
                    {t("onboarding.recommend.hideAlternatives")}
                  </button>
                </>
              ) : (
                <div className="ob-card ob-appear" style={{ animationDelay: "0.1s" }}>
                  <div className="ob-card-head">
                    <h1 className="ob-model-name">{chosen.spec.name}</h1>
                    <span className="badge badge-neutral">
                      {t("onboarding.recommend.byPublisher", { publisher: chosen.spec.publisher })}
                    </span>
                  </div>
                  <p className="ob-model-desc">{chosen.spec.description}</p>
                  <ul className="ob-reasons">
                    {report.recommendation.reasons.map((reason) => (
                      <li key={reason}>{reason}</li>
                    ))}
                  </ul>
                  <div className="ob-card-actions">
                    <button className="btn btn-big" onClick={() => begin(chosen.spec.id)}>
                      {installed
                        ? t("onboarding.recommend.useModel", { name: chosen.spec.name })
                        : t("onboarding.recommend.download", {
                            size: formatBytes(totalModelSizeBytes(chosen.spec)),
                          })}
                    </button>
                    <button
                      ref={pickDifferentRef}
                      className="btn-quiet"
                      aria-expanded={false}
                      aria-controls="ob-alts"
                      onClick={() => toggleAlternatives(true)}
                    >
                      {t("onboarding.recommend.pickDifferent")}
                    </button>
                  </div>
                  <p className="ob-reassure">{t("onboarding.recommend.reassure")}</p>
                </div>
              )}
            </>
          ) : (
            <p className="ob-sub">{t("onboarding.recommend.preparing")}</p>
          )}
          <SkipButtons onSkipStep={skipModelSetup} onSkipSetup={askToSkipSetup} />
        </section>
      )}

      {scene === "download" && chosen && (
        <section className="ob-scene" key="download" ref={sceneRef} tabIndex={-1}>
          {downloadError ? (
            <>
              <h1 className="ob-title" tabIndex={-1}>{t("onboarding.download.failedTitle")}</h1>
              <p className="ob-sub" role="alert">{downloadError}</p>
              <button className="btn btn-big" onClick={() => begin(chosen.spec.id)}>
                {t("onboarding.download.tryAgain")}
              </button>
              <button className="btn-quiet" onClick={() => setScene("recommend")}>
                {t("onboarding.download.goBack")}
              </button>
            </>
          ) : !installed && progress ? (
            <>
              <div className="ob-ring ob-appear">
                <ProgressRing
                  progress={
                    progress.totalBytes > 0
                      ? progress.receivedBytes / progress.totalBytes
                      : 0
                  }
                  label={t("onboarding.download.bringingHome", { name: chosen.spec.name })}
                />
              </div>
              <h1 className="ob-title" tabIndex={-1}>
                {t("onboarding.download.bringingHome", { name: chosen.spec.name })}
              </h1>
              <p className="ob-sub">
                {t("onboarding.download.progressOf", {
                  received: formatBytes(progress.receivedBytes),
                  total: formatBytes(progress.totalBytes || totalModelSizeBytes(chosen.spec)),
                })}
                {progress.bytesPerSec > 0 && <> · {formatSpeed(progress.bytesPerSec)}</>}
                {progress.etaSeconds > 0 && <> · {formatEta(progress.etaSeconds)}</>}
              </p>
              {/* Throttled announcement of the readout above — see
                  useThrottledStatus — so a screen reader hears progress
                  every few seconds, not on every download tick. */}
              <p className="sr-only" role="status">{throttledProgressText}</p>
              <p className="ob-reassure">{t("onboarding.download.reassure")}</p>
              <button
                className="btn-quiet"
                onClick={() => {
                  void cancelDownload(chosen.spec.id);
                  setScene("recommend");
                }}
              >
                {t("common.cancel")}
              </button>
            </>
          ) : engine.type === "failed" ? (
            <>
              <h1 className="ob-title" tabIndex={-1}>{t("onboarding.download.needsNudgeTitle")}</h1>
              <p className="ob-sub" role="alert">{engine.data.message}</p>
              <button className="btn btn-big" onClick={() => void useModel(chosen.spec.id)}>
                {t("onboarding.download.tryAgain")}
              </button>
            </>
          ) : (
            <>
              <div className="ob-waking ob-appear" aria-hidden>
                ●
              </div>
              <h1 className="ob-title" tabIndex={-1}>{t("onboarding.download.wakingTitle")}</h1>
              <p className="ob-sub" role="status">
                {t("onboarding.download.loadingIntoMemory", { name: chosen.spec.name })}
              </p>
            </>
          )}
          <SkipButtons onSkipStep={skipModelSetup} onSkipSetup={askToSkipSetup} />
        </section>
      )}

      {scene === "security" && (
        <section className="ob-scene" key="security" ref={sceneRef} tabIndex={-1}>
          {securityStep === "offer" && (
            <>
              <h1 className="ob-title ob-appear" tabIndex={-1}>
                {t("appLock.onboarding.offerTitle")}
              </h1>
              <p className="ob-sub ob-appear">{t("appLock.onboarding.offerBody")}</p>
              <div className="ob-appear">
                <button
                  className="btn btn-big"
                  onClick={() => {
                    setPendingPin(null);
                    setPinFlowError(null);
                    setSecurityStep("create");
                  }}
                >
                  {t("appLock.onboarding.setPinButton")}
                </button>
              </div>
              <SkipButtons
                appear={0.15}
                onSkipStep={() => setScene("ready")}
                onSkipSetup={askToSkipSetup}
              />
            </>
          )}

          {securityStep === "create" && (
            <PinPad
              title={t("appLock.onboarding.createPinTitle")}
              cancelLabel={t("appLock.onboarding.backButton")}
              onCancel={() => setSecurityStep("offer")}
              onSubmit={(pin) => {
                setPendingPin(pin);
                setPinFlowError(null);
                setSecurityStep("confirm");
              }}
            />
          )}

          {securityStep === "confirm" && (
            <PinPad
              title={t("appLock.onboarding.confirmPinTitle")}
              error={pinFlowError}
              busy={savingPin}
              cancelLabel={t("appLock.onboarding.backButton")}
              onCancel={() => setSecurityStep("offer")}
              onSubmit={(pin) => {
                if (pin !== pendingPin) {
                  setPinFlowError(t("appLock.onboarding.pinMismatch"));
                  return;
                }
                setSavingPin(true);
                setPinFlowError(null);
                // Wait for the save (busy state shows on this same PinPad
                // meanwhile) before moving on — advancing first would
                // unmount this step before a failure could ever be shown.
                // Still never blocks finishing onboarding: whether the save
                // succeeds or fails, "ready" follows either way, since
                // falling back to "PIN off" is always a safe default.
                void api
                  .setAppLockPin(null, true, pin)
                  .catch((err: unknown) => setPinFlowError(String(err)))
                  .finally(() => {
                    setSavingPin(false);
                    setScene("ready");
                  });
              }}
            />
          )}
        </section>
      )}

      {scene === "ready" && (
        <section className="ob-scene" key="ready" ref={sceneRef} tabIndex={-1}>
          <p className="ob-brand ob-appear">OpenPal</p>
          <h1 className="ob-display ob-appear" tabIndex={-1} style={{ animationDelay: "0.1s" }}>
            {t(modelOnTheWay ? "onboarding.ready.allSet" : "onboarding.ready.almostThere")}
          </h1>
          {/* A live region: this line follows the engine while the screen is up. */}
          <p className="ob-sub ob-appear" role="status" style={{ animationDelay: "0.25s" }}>
            {engine.type === "running"
              ? t("onboarding.ready.awake", { name: readyName })
              : engine.type === "starting"
                ? t("onboarding.download.loadingIntoMemory", { name: readyName })
                : t(stoppedDownload ? "onboarding.ready.downloadStopped" : "onboarding.ready.noModel")}
          </p>
          <label className="ob-update-choice ob-appear" style={{ animationDelay: "0.32s" }}>
            <input
              type="checkbox"
              checked={autoCheckUpdates}
              onChange={(e) => setAutoCheckUpdates(e.target.checked)}
            />
            <span>{t("onboarding.ready.updateChoice")}</span>
          </label>
          <div className="ob-appear" style={{ animationDelay: "0.4s" }}>
            <button
              className="btn btn-big"
              onClick={async () => {
                await saveSettings({ autoCheckUpdates });
                await completeOnboarding();
              }}
            >
              {t(modelOnTheWay ? "onboarding.ready.sayHello" : "onboarding.ready.getStarted")}
            </button>
          </div>
        </section>
      )}

      {/* Escape and a backdrop click dismiss it too — but it stays up mid-save. */}
      {confirmingSkip && (
        <ConfirmDialog
          title={t("onboarding.skipConfirm.title")}
          message={t(`onboarding.skipConfirm.${skipOutcome}`)}
          confirmLabel={t("onboarding.skipSetup")}
          confirmVariant="primary"
          busy={skipping}
          onConfirm={() => void finishOnboardingEarly()}
          onCancel={() => setSkipFrom(null)}
        />
      )}
    </div>
  );
}

/** The two ways out of a scene: skip just this step (where the step can be
 * skipped on its own), or the whole setup — which asks first. */
function SkipButtons(props: {
  onSkipStep?: () => void;
  onSkipSetup: () => void;
  disabled?: boolean;
  /** Fade in, after this many seconds (just after the scene's main button). */
  appear?: number;
}) {
  const { t } = useTranslation();
  return (
    <div
      className={`ob-skips${props.appear === undefined ? "" : " ob-appear"}`}
      style={props.appear ? { animationDelay: `${props.appear}s` } : undefined}
    >
      {props.onSkipStep && (
        <button
          className="btn-quiet"
          type="button"
          disabled={props.disabled}
          onClick={props.onSkipStep}
        >
          {t("onboarding.skipStep")}
        </button>
      )}
      <button
        className="btn-quiet"
        type="button"
        disabled={props.disabled}
        onClick={props.onSkipSetup}
      >
        {t("onboarding.skipSetup")}
      </button>
    </div>
  );
}

function StatCard(props: {
  delay: number;
  label: string;
  value: string;
  good?: boolean;
}) {
  return (
    <div className="ob-stat ob-appear" style={{ animationDelay: `${props.delay}s` }}>
      <span className="ob-stat-label">{props.label}</span>
      <span className={`ob-stat-value${props.good ? " ob-stat-good" : ""}`}>
        {props.value}
        {props.good && <span className="ob-stat-check"> ✓</span>}
      </span>
    </div>
  );
}
