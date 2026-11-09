import { useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { api } from "../lib/api";
import ConfirmDialog from "../components/ConfirmDialog";
import DiskSlowDialog from "../components/DiskSlowDialog";
import { useCloseOnOutside, useCloseOnScroll } from "../components/Dropdown";
import FavoriteButton from "../components/FavoriteButton";
import InfoPopover from "../components/InfoPopover";
import ProgressBar from "../components/ProgressBar";
import {
  architectureLabel,
  estimateAnswerDuration,
  FIT_BADGE,
  fitLabel,
  formatBytes,
  formatEta,
  formatSpeed,
  formatSpeedCompact,
  formatTokensPerSec,
  paramsWithActiveLabel,
  ramLabel,
  releaseDateLabel,
  speedTier,
  totalModelSizeBytes,
} from "../lib/format";
import {
  buildModelList,
  modelSpeed,
  MODEL_FILTERS,
  MODEL_SORTS,
  type ModelFilter,
  type ModelSort,
} from "../lib/modelList";
import type { CloudModelInfo, FitLevel, HfFile, HfListing, ModelInfo } from "../lib/types";
import { cloudModelsAvailable, useStore } from "../state/store";

/** Marks models that can read images you attach (vision/multimodal). */
const ImageIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <rect x="3" y="3" width="18" height="18" rx="2" ry="2" />
    <circle cx="8.5" cy="8.5" r="1.5" />
    <path d="m21 15-4.5-4.5L5 21" />
  </svg>
);

const SearchIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <circle cx="11" cy="11" r="8" />
    <path d="m21 21-4.3-4.3" />
  </svg>
);

const MoreIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <circle cx="12" cy="5" r="1" />
    <circle cx="12" cy="12" r="1" />
    <circle cx="12" cy="19" r="1" />
  </svg>
);

/** Points right when collapsed, down when expanded (rotated in CSS). */
const DisclosureIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="m9 18 6-6-6-6" />
  </svg>
);

const GLYPH = {
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 2,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
  "aria-hidden": true,
};

/** It fits — green when there's room to spare, amber when it'll be tight. */
const CheckCircleIcon = () => (
  <svg {...GLYPH}>
    <circle cx="12" cy="12" r="9" />
    <path d="m8 12 2.5 2.5L16 9.5" />
  </svg>
);

/** It doesn't fit in memory — whether or not it can still stream from disk. */
const CrossCircleIcon = () => (
  <svg {...GLYPH}>
    <circle cx="12" cy="12" r="9" />
    <path d="m9 9 6 6" />
    <path d="m15 9-6 6" />
  </svg>
);

/** Rides alongside the check on a perfect fit: not just "it runs" but "it
 * runs fast". Filled rather than stroked, so it reads as a mark on top of
 * the verdict instead of a second verdict. */
const BoltIcon = () => (
  <svg viewBox="0 0 24 24" fill="currentColor" stroke="none" aria-hidden="true">
    <path d="M13 2 3 14h9l-1 8 10-12h-9l1-8Z" />
  </svg>
);

/**
 * One glyph answering one question — does it fit in memory? A green or amber
 * check if yes, a red cross if no — plus a yellow bolt on the fastest picks.
 *
 * Deliberately only two distinctions are carried visually. `good` and `tight`
 * share the check and differ by colour; `diskSlow` and `tooBig` share the
 * cross and are identical. What separates each pair is the wording, which
 * every indicator carries as `title` and `.sr-only` text, which the legend
 * beside the list heading lays out, and which the spotlight card shows in
 * full. In practice the two red states rarely sit side by side: a too-big
 * model that was never downloaded is hidden from the list entirely
 * (`buriedAsTooBig`), so a red cross on a listed model almost always means
 * the disk-streamed kind.
 */
const FIT_TONE: Record<FitLevel, string> = {
  perfect: "is-ok",
  good: "is-ok",
  tight: "is-warn",
  diskSlow: "is-bad",
  tooBig: "is-bad",
};

function FitGlyphs({ fit }: { fit: FitLevel }) {
  const fitsInMemory = fit === "perfect" || fit === "good" || fit === "tight";
  return (
    <>
      {fitsInMemory ? <CheckCircleIcon /> : <CrossCircleIcon />}
      {fit === "perfect" && (
        <span className="model-fit-bolt">
          <BoltIcon />
        </span>
      )}
    </>
  );
}

function FitIndicator({ fit }: { fit: FitLevel }) {
  const { t } = useTranslation();
  const label = fitLabel(fit);
  // The hover text is the legend's own sentence, so an icon always explains
  // itself in place — this is what tells someone that a disk-streamed model
  // doesn't fit in memory yet can still be used, just far more slowly.
  const hint = t(`library.list.fitInfo.${fit}`);
  return (
    <span className={`model-fit ${FIT_TONE[fit]}`} title={`${label}. ${hint}`}>
      <FitGlyphs fit={fit} />
      <span className="sr-only">{`${label}. ${hint}`}</span>
    </span>
  );
}

/** Best fit first — the order the icons are explained in, and the order
 * `FitLevel` itself is ranked in (crates/core/src/types.rs). */
const FIT_ORDER: FitLevel[] = ["perfect", "good", "tight", "diskSlow", "tooBig"];

/**
 * The key to `FitIndicator`'s glyphs, sitting next to the list heading.
 * Swapping wording for icons buys back the width that made the list
 * unreadable, but it costs a beginner the ability to guess — so the mapping
 * has to be one click away rather than something you're expected to infer.
 */
function FitLegend() {
  const { t } = useTranslation();
  return (
    <InfoPopover
      label={t("library.list.fitInfo.label")}
      heading={t("library.list.fitInfo.heading")}
    >
      <ul className="fit-legend">
        {FIT_ORDER.map((fit) => (
          <li key={fit}>
            <span className={`model-fit ${FIT_TONE[fit]}`}>
              <FitGlyphs fit={fit} />
            </span>
            <span>
              <strong>{fitLabel(fit)}</strong>: {t(`library.list.fitInfo.${fit}`)}
            </span>
          </li>
        ))}
      </ul>
    </InfoPopover>
  );
}

/**
 * "What even is a quantization?" — the one bit of unavoidable jargon left on
 * this page, and the kind of thing ARCHITECTURE.md's beginner-first rule says
 * shouldn't be a prerequisite for using the app.
 */
function QuantInfo() {
  const { t } = useTranslation();
  return (
    <InfoPopover
      label={t("library.list.quantInfo.label")}
      heading={t("library.list.quantInfo.heading")}
    >
      <p>{t("library.list.quantInfo.body")}</p>
      <p>{t("library.list.quantInfo.naming")}</p>
    </InfoPopover>
  );
}

export default function Library() {
  const { t } = useTranslation();
  const models = useStore((s) => s.models);
  const hardware = useStore((s) => s.report)?.hardware;
  const customModelsEnabled = useStore((s) => s.settings?.customModelsEnabled ?? false);

  // A model with any bytes on disk never hides behind "needs a bigger Mac"
  // — even a too-big fit doesn't mean unusable (e.g. a user-added local
  // file, or a model that was "diskSlow" until they turned the disk-offload
  // toggle back off, possibly mid-download). Only never-downloaded oversized
  // models get collapsed away.
  const buriedAsTooBig = (m: ModelInfo) =>
    m.fit === "tooBig" && m.status.type === "notInstalled";
  // `spec.curated` (crates/core/data/catalog.json) is the single source of
  // truth for the handful of beginner-friendly picks the simple picker below
  // shows — the backend uses the same flag to adopt an already-downloaded,
  // now-decurated catalog model into "your own models" (see `get_models` in
  // crates/app/src/commands.rs) rather than let it vanish.
  const curated = models.filter((m) => m.spec.curated && !buriedAsTooBig(m));
  // "All models" (`ModelList` below): the curated picks above (shown there
  // too — this list is meant to be literally comprehensive) plus every
  // model the user brought in themselves — via Discover's Hugging Face
  // browser, or "Add your own model" at the bottom of this page — which are
  // never catalog entries outside the curated four and would otherwise
  // become unreachable: no way to download, use, resume, or delete them. A
  // non-curated catalog entry stays out of both lists, same as before.
  const listModels = models.filter(
    (m) => (m.spec.curated || m.spec.source !== "catalog") && !buriedAsTooBig(m),
  );

  return (
    <div className="library">
      <div className="library-scroll">
        <div className="library-col">
          <header className="library-head">
            <h1>{t("library.title")}</h1>
            <p>{t("library.subtitle")}</p>
            {/* Memory and the Finder shortcut share one muted line rather
                than stacking — two fewer lines before the picker, which is
                what the page is actually for. */}
            <div className="library-head-meta">
              {hardware && (
                <>
                  <span className="library-ram">
                    {t("library.availableRam", { ram: ramLabel(hardware.availableRamBytes) })}
                  </span>
                  <span className="library-head-dot" aria-hidden="true">
                    ·
                  </span>
                </>
              )}
              <button className="btn-quiet" onClick={() => void api.openModelsFolder()}>
                {t("library.showInFinder")}
              </button>
            </div>
          </header>

          <SimpleModelPicker models={curated} />

          {listModels.length > 0 && (
            <section className="library-others">
              <div className="library-others-head">
                <h2>{t("library.otherModelsHeading")}</h2>
                <FitLegend />
              </div>
              <ModelList models={listModels} />
            </section>
          )}

          <CloudModelsSection />

          {customModelsEnabled && <AddYourOwnModel />}
        </div>
      </div>
    </div>
  );
}

/**
 * Cloud models offered by a signed-in account's backend, alongside the local
 * ones above. Shown to everyone — signed in or not — so a free/signed-out
 * user sees what they'd unlock rather than the section just being absent;
 * `cloudModelsAvailable` (an entitlement check, not a visibility check) gates
 * whether each row's action is "Use this model" or "Sign in to unlock".
 * Reads the catalog from the store (`loadCloudModels`, fetched once at boot)
 * rather than fetching its own copy — the Generation panel needs the same
 * catalog to filter its sliders per model, so it's loaded once, not twice.
 *
 * Still a card grid, deliberately: `CloudModelInfo` carries a description and
 * strengths but none of the size/fit/speed/quant fields the compact local row
 * is built around, so the row layout would have empty columns for what it
 * lacks and nowhere to put what it has.
 */
function CloudModelsSection() {
  const { t } = useTranslation();
  const entitled = useStore((s) => cloudModelsAvailable(s.account));
  const models = useStore((s) => s.cloudModels);
  const error = useStore((s) => s.cloudModelsError);

  if (error || (models && models.length === 0)) return null;

  return (
    <section className="library-others">
      <h2>{t("library.cloudModelsHeading")}</h2>
      <div className="model-grid">
        {models === null
          ? null
          : models.map((m) => (
              <CloudModelCard key={m.id} model={m} entitled={entitled} />
            ))}
      </div>
    </section>
  );
}

function CloudModelCard({
  model,
  entitled,
}: {
  model: CloudModelInfo;
  entitled: boolean;
}) {
  const { t } = useTranslation();
  const settings = useStore((s) => s.settings);
  const account = useStore((s) => s.account);
  const useCloudModel = useStore((s) => s.useCloudModel);
  const signIn = useStore((s) => s.signIn);
  const goToChat = useStore((s) => s.goToChat);
  const inUse =
    settings?.chatBackend === "cloud" && settings.selectedCloudModelId === model.id;

  return (
    <article className={`model-card${entitled ? "" : " model-card-dim"}`}>
      <div className="model-card-head">
        <h3>{model.name}</h3>
        <span className="badge badge-neutral">{model.requiredTier}</span>
      </div>
      <p className="model-meta">{t("library.byPublisher", { publisher: model.publisher })}</p>
      <p className="model-desc">{model.description}</p>
      <div className="model-chips">
        {model.strengths.map((s) => (
          <span key={s} className="badge badge-neutral">
            {s}
          </span>
        ))}
      </div>

      <footer className="model-actions">
        {inUse ? (
          <>
            <span className="model-inuse">{t("library.inUse")}</span>
            <button className="btn-quiet" onClick={goToChat}>
              {t("library.goToChat")}
            </button>
          </>
        ) : entitled ? (
          <button className="btn-ghost" onClick={() => void useCloudModel(model.id)}>
            {t("library.useThisModel")}
          </button>
        ) : account ? (
          // Signed in, but this plan doesn't include it — "Sign in" would be
          // a dead end here, so just say what's needed instead of a button
          // that does nothing useful.
          <span className="model-progress-text">
            {t("library.requiresPlan", { tier: model.requiredTier })}
          </span>
        ) : (
          <button className="btn-ghost" onClick={() => void signIn()}>
            {t("library.signInToUnlock")}
          </button>
        )}
      </footer>
    </article>
  );
}

/**
 * Bring any model — a GGUF from any Hugging Face repository, or a
 * .gguf file already on disk. Shown at the bottom of this page only once
 * Settings → AI model's "Add your own models" switch is on
 * (`Settings.customModelsEnabled`); an added model lands in "All models"
 * above like any other. The Rust side does all the network work (the
 * webview has none).
 */
function AddYourOwnModel() {
  const refreshModels = useStore((s) => s.refreshModels);
  const { t } = useTranslation();

  const [query, setQuery] = useState("");
  const [searching, setSearching] = useState(false);
  const [listing, setListing] = useState<HfListing | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** Path of the file currently being added, if any. */
  const [busyPath, setBusyPath] = useState<string | null>(null);
  /** Paths from the current listing that were added. */
  const [addedPaths, setAddedPaths] = useState<ReadonlySet<string>>(new Set());
  const [pickerBusy, setPickerBusy] = useState(false);
  const [localAdded, setLocalAdded] = useState(false);

  const search = async () => {
    if (!query.trim() || searching) return;
    setSearching(true);
    setError(null);
    setListing(null);
    setAddedPaths(new Set());
    try {
      setListing(await api.listHfModels(query));
    } catch (err: unknown) {
      setError(String(err));
    } finally {
      setSearching(false);
    }
  };

  const addFromHf = async (file: HfFile) => {
    if (!listing || busyPath) return;
    setBusyPath(file.path);
    setError(null);
    try {
      await api.addHfModel(listing.repoId, listing.revision, file.path);
      setAddedPaths((prev) => new Set(prev).add(file.path));
      await refreshModels();
    } catch (err: unknown) {
      setError(String(err));
    } finally {
      setBusyPath(null);
    }
  };

  const addFromDisk = async () => {
    if (pickerBusy) return;
    setPickerBusy(true);
    setError(null);
    try {
      const id = await api.addLocalModel();
      if (id) {
        setLocalAdded(true);
        await refreshModels();
      }
    } catch (err: unknown) {
      setError(String(err));
    } finally {
      setPickerBusy(false);
    }
  };

  return (
    <section className="library-others">
      <h2>{t("library.addModel.label")}</h2>
      <div className="settings-field">
        <span className="settings-hint">{t("library.addModel.hint")}</span>

        <div className="settings-hf-search">
          <input
            type="text"
            placeholder={t("library.addModel.placeholder")}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void search();
            }}
          />
          <button
            className="btn-ghost"
            disabled={searching || !query.trim()}
            onClick={() => void search()}
          >
            {searching ? t("library.addModel.searching") : t("library.addModel.findFiles")}
          </button>
        </div>

        {error && <p className="model-error" role="alert">{error}</p>}

        {listing && (
          <ul className="settings-hf-files">
            {listing.files.map((f) => (
              <li
                key={f.path}
                className={
                  f.path === listing.preselectPath ||
                  f.extraShards?.some((s) => s.path === listing.preselectPath)
                    ? "is-preselected"
                    : ""
                }
              >
                <div className="settings-hf-file-info">
                  <span className="settings-hf-file-name" title={f.path}>
                    {f.path}
                  </span>
                  <span className="settings-hf-file-meta">
                    {f.quant} · {formatBytes(totalModelSizeBytes(f))}
                    {f.extraShards && f.extraShards.length > 0 && (
                      <> · {t("library.addModel.fileCount", { count: f.extraShards.length + 1 })}</>
                    )}
                  </span>
                </div>
                {addedPaths.has(f.path) ? (
                  <span className="settings-hf-added">{t("library.addModel.added")}</span>
                ) : (
                  <button
                    className="btn-quiet"
                    disabled={busyPath !== null}
                    onClick={() => void addFromHf(f)}
                  >
                    {busyPath === f.path ? t("library.addModel.adding") : t("library.addModel.add")}
                  </button>
                )}
              </li>
            ))}
          </ul>
        )}

        <div className="settings-local-row">
          <button
            className="btn-ghost"
            disabled={pickerBusy}
            onClick={() => void addFromDisk()}
          >
            {pickerBusy
              ? t("library.addModel.waitingForPick")
              : t("library.addModel.chooseFile")}
          </button>
          {localAdded && (
            <span className="settings-hf-added">{t("library.addModel.added")}</span>
          )}
        </div>
      </div>
    </section>
  );
}

/**
 * The default, beginner-friendly way to pick a model, sized to how many of
 * the curated picks fit this Mac: none gets an explanatory message, one is
 * just that one card, and two or more get a segmented dial from "fast &
 * lightweight" to "smart & heavy" (ordered by `qualityScore`) above the
 * currently-selected card. The actual download/use/delete affordances are
 * always the existing `ModelCard`.
 */
function SimpleModelPicker({ models }: { models: ModelInfo[] }) {
  const { t } = useTranslation();
  const ordered = useMemo(
    () => [...models].sort((a, b) => a.spec.qualityScore - b.spec.qualityScore),
    [models],
  );

  if (ordered.length === 0) {
    return <p className="model-picker-empty">{t("library.pickerEmpty")}</p>;
  }

  if (ordered.length === 1) {
    return (
      <div className="model-grid model-grid-single">
        <ModelCard model={ordered[0]} />
      </div>
    );
  }

  return <ModelDial ordered={ordered} />;
}

/**
 * The curated picker: a segmented stepper (one labelled stop per curated
 * model, fast/light on the left) above the selected model's card.
 *
 * Replaces a bare `<input type="range">`, which was the wrong control for
 * three or four discrete, nameable options — and which rendered as stock
 * browser chrome, since nothing in this app styles a range input beyond
 * `accent-color`. The card below re-renders on every change; that's pure
 * arithmetic (`estimateTokensPerSec`), so it stays instant.
 *
 * ARIA follows the app's existing radiogroup — SettingsSheet's voice picker —
 * except that focus roves (only the selected stop is tabbable) rather than
 * every stop being a tab stop, which is what the WAI-ARIA radiogroup pattern
 * actually calls for. Each stop's accessible name is the model name, which is
 * what the old input's `aria-valuetext` existed to provide.
 */
function ModelDial({ ordered }: { ordered: ModelInfo[] }) {
  const { t } = useTranslation();
  const railRef = useRef<HTMLDivElement>(null);
  const defaultId = useMemo(() => {
    const recommended = ordered.find((m) => m.isRecommended);
    if (recommended) return recommended.spec.id;
    return ordered[Math.floor((ordered.length - 1) / 2)]?.spec.id ?? null;
  }, [ordered]);

  const [selectedId, setSelectedId] = useState(defaultId);
  // Covers both "the selection disappeared" (deleted, or the catalog/fit
  // list changed) and the initial mount racing `ordered` still being empty
  // (models haven't loaded yet, so `defaultId` started out null) — either
  // way, fall back to the current default pick rather than getting stuck.
  useEffect(() => {
    if (!ordered.some((m) => m.spec.id === selectedId)) {
      setSelectedId(defaultId);
    }
  }, [ordered, selectedId, defaultId]);

  const index = Math.max(
    0,
    ordered.findIndex((m) => m.spec.id === selectedId),
  );
  const selected = ordered[index];

  /** Moves the selection and takes focus with it — with a roving tabindex the
   * newly-selected stop is the only tabbable one, so focus has to follow or
   * it would land back on the document. */
  const moveTo = (next: number) => {
    const clamped = Math.max(0, Math.min(ordered.length - 1, next));
    setSelectedId(ordered[clamped].spec.id);
    railRef.current
      ?.querySelectorAll<HTMLButtonElement>(".model-dial-stop")
      [clamped]?.focus();
  };

  // On the stops rather than the rail: with a roving tabindex the stops are
  // what hold focus, so that's where the keys actually land — and a
  // `radiogroup` that isn't itself focusable has no business owning a key
  // handler.
  const onStopKeyDown = (e: React.KeyboardEvent<HTMLButtonElement>, at: number) => {
    const keys: Record<string, number> = {
      ArrowLeft: at - 1,
      ArrowUp: at - 1,
      ArrowRight: at + 1,
      ArrowDown: at + 1,
      Home: 0,
      End: ordered.length - 1,
    };
    const next = keys[e.key];
    if (next === undefined) return;
    e.preventDefault();
    moveTo(next);
  };

  return (
    <div className="model-picker-simple">
      <div className="model-dial">
        <div
          ref={railRef}
          className="model-dial-rail"
          role="radiogroup"
          aria-label={t("library.sliderAriaLabel")}
        >
          {ordered.map((m, i) => {
            const isSelected = i === index;
            return (
              <button
                key={m.spec.id}
                type="button"
                role="radio"
                aria-checked={isSelected}
                tabIndex={isSelected ? 0 : -1}
                className={`model-dial-stop${isSelected ? " is-selected" : ""}`}
                onClick={() => setSelectedId(m.spec.id)}
                onKeyDown={(e) => onStopKeyDown(e, i)}
              >
                <span className="model-dial-stop-name">{m.spec.name}</span>
                <span className="model-dial-stop-size">
                  {formatBytes(totalModelSizeBytes(m.spec))}
                </span>
              </button>
            );
          })}
        </div>
        <div className="model-dial-captions" aria-hidden="true">
          <span>{t("library.fastLightweight")}</span>
          <span>{t("library.smartHeavy")}</span>
        </div>
      </div>

      <div className="model-grid model-grid-single model-grid-wide">
        <ModelCard key={selected.spec.id} model={selected} />
      </div>
    </div>
  );
}

/**
 * All the state and derived values a model's card/row needs. Purely derived
 * plus the store actions — the confirm dialogs' open/closed state lives in
 * `useModelActions` below, with the components that render their triggers,
 * so a row that never opens a dialog doesn't carry its state or its
 * disk-speed probe.
 */
function useModelState(model: ModelInfo) {
  const { spec, status, fit } = model;
  const settings = useStore((s) => s.settings);
  const engine = useStore((s) => s.engine);
  const downloads = useStore((s) => s.downloads);
  const downloadErrors = useStore((s) => s.downloadErrors);
  const startDownload = useStore((s) => s.startDownload);
  const cancelDownload = useStore((s) => s.cancelDownload);
  const deleteModel = useStore((s) => s.deleteModel);
  const useModel = useStore((s) => s.useModel);
  const goToChat = useStore((s) => s.goToChat);
  const hardware = useStore((s) => s.report)?.hardware;
  const speed = modelSpeed(model, hardware ?? null);
  const longAnswerDuration = estimateAnswerDuration(speed);

  const progress = downloads[spec.id];
  const error = downloadErrors[spec.id];
  const installed = status.type === "installed";
  const partial = status.type === "partiallyDownloaded";
  const inUse = engine.type === "running" && engine.data.modelId === spec.id;
  const waking = engine.type === "starting" && engine.data.modelId === spec.id;
  // Pinned in Settings > "Default model at startup" — independent of `inUse`,
  // which only reflects what's active *this session* (see DefaultModelSection
  // in SettingsSheet.tsx). Surfaced here too so switching models elsewhere
  // doesn't quietly desync from what actually boots next time. Also gated on
  // "Load model on startup": with that off, nothing auto-loads at all, so
  // the pin wouldn't actually take effect and the badge would overpromise.
  const isStartupDefault =
    (settings?.autoLoadOnStartup ?? true) && settings?.defaultModelId === spec.id;
  const isCustom = spec.source !== "catalog";
  const isLocalFile = spec.source === "localFile";
  // A local file the user moved or deleted since adding it. Its spec has an
  // empty `url` (crates/core/src/custom.rs) — there is nothing to download —
  // so this must be checked before any download affordance, or the row grows
  // a "Download" button that cannot possibly work.
  const localFileMissing = isLocalFile && !installed;
  // A fresh (non-resumed) download of a disk-streamed model needs an
  // explicit "yes, this will be slow" confirmation first.
  const needsSlowConfirm = fit === "diskSlow" && !partial;

  return {
    spec,
    fit,
    hardware,
    speed,
    longAnswerDuration,
    progress,
    error,
    installed,
    partial,
    inUse,
    waking,
    isStartupDefault,
    isCustom,
    isLocalFile,
    localFileMissing,
    needsSlowConfirm,
    startDownload,
    cancelDownload,
    deleteModel,
    useModel,
    goToChat,
  };
}

type ModelState = ReturnType<typeof useModelState>;

/**
 * `useModelState` plus the two dialogs that back a model's destructive and
 * consent actions, and the handlers that open them. Shared by the spotlight
 * card's footer and the compact list row, which agree on what every action
 * *means* and differ only in where they put the buttons.
 *
 * Both dialogs are modals rather than the inline panels this page used to
 * render inside a row, which changed the row's height mid-interaction.
 */
function useModelActions(model: ModelInfo) {
  const { t } = useTranslation();
  const st = useModelState(model);
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const [confirmingSlow, setConfirmingSlow] = useState(false);
  const { spec } = st;

  // Throwing away a half-finished download reads differently from removing a
  // model you have: different wording, different button, same mechanism.
  const discardMode = st.partial && !st.installed;
  const removeLabel = discardMode
    ? t("library.discard")
    : st.isLocalFile
      ? t("library.removeFromLibrary")
      : t("library.remove");
  const removeAriaLabel = discardMode
    ? t("library.discardAriaLabel", { name: spec.name })
    : st.isLocalFile
      ? t("library.removeFromLibraryAriaLabel", { name: spec.name })
      : t("library.removeAriaLabel", { name: spec.name });

  const download = () => {
    if (st.needsSlowConfirm) setConfirmingSlow(true);
    else st.startDownload(spec.id);
  };

  const dialogs = (
    <>
      {confirmingDelete && (
        <ConfirmDialog
          title={
            discardMode
              ? t("library.deleteConfirm.discardTitle")
              : t("library.deleteConfirm.removeTitle", { name: spec.name })
          }
          message={
            discardMode
              ? t("library.deleteConfirm.discard", { name: spec.name })
              : t("library.deleteConfirm.remove", { name: spec.name })
          }
          confirmLabel={removeLabel}
          onConfirm={() => {
            setConfirmingDelete(false);
            void st.deleteModel(spec.id);
          }}
          onCancel={() => setConfirmingDelete(false)}
        />
      )}
      {confirmingSlow && (
        <DiskSlowDialog
          model={model}
          onConfirm={() => {
            setConfirmingSlow(false);
            st.startDownload(spec.id);
          }}
          onCancel={() => setConfirmingSlow(false)}
        />
      )}
    </>
  );

  return {
    st,
    dialogs,
    download,
    askDelete: () => setConfirmingDelete(true),
    removeLabel,
    removeAriaLabel,
  };
}

/**
 * The spotlight card's download/use/delete row. Same actions as a list row's,
 * laid out for a card that has room for two buttons side by side instead of a
 * button and an overflow menu.
 */
function ModelActionFooter({ actions }: { actions: ReturnType<typeof useModelActions> }) {
  const { t } = useTranslation();
  const { st, dialogs, download, askDelete, removeLabel, removeAriaLabel } = actions;
  const { spec } = st;

  const removeButton = (
    <button
      className="btn-quiet btn-danger-quiet"
      aria-label={removeAriaLabel}
      onClick={askDelete}
    >
      {removeLabel}
    </button>
  );

  return (
    <>
      {st.error && (
        <p className="model-error" role="alert">
          {st.error}
        </p>
      )}
      <div className="model-actions">
        {st.progress ? (
          <>
            <ProgressBar
              className="model-progress"
              barClassName="model-progress-bar"
              value={
                st.progress.totalBytes > 0
                  ? st.progress.receivedBytes / st.progress.totalBytes
                  : 0
              }
              label={t("library.downloadProgressAriaLabel", { name: spec.name })}
            />
            <span className="model-progress-text">
              {t("library.downloadProgressOf", {
                received: formatBytes(st.progress.receivedBytes),
                total: formatBytes(st.progress.totalBytes || totalModelSizeBytes(spec)),
              })}
              {st.progress.bytesPerSec > 0 && <> · {formatSpeed(st.progress.bytesPerSec)}</>}
              {st.progress.etaSeconds > 0 && <> · {formatEta(st.progress.etaSeconds)}</>}
            </span>
            <button
              className="btn-quiet btn-danger-quiet"
              aria-label={t("library.cancelDownloadAriaLabel", { name: spec.name })}
              onClick={() => void st.cancelDownload(spec.id)}
            >
              {t("common.cancel")}
            </button>
          </>
        ) : st.inUse ? (
          <>
            <span className="model-inuse">{t("library.inUse")}</span>
            <button className="btn-quiet" onClick={st.goToChat}>
              {t("library.goToChat")}
            </button>
          </>
        ) : st.waking ? (
          <span className="model-inuse">
            {st.fit === "diskSlow" ? t("library.wakingSlow") : t("library.waking")}
          </span>
        ) : st.localFileMissing ? (
          <>
            <span className="model-progress-text">{t("library.fileMissing")}</span>
            {removeButton}
          </>
        ) : st.installed ? (
          <>
            <button
              className="btn-ghost"
              // Never disabled by a load in flight: another model's card must
              // stay switchable, because picking it mid-load is how a long
              // load gets redirected — `useModel` supersedes it (engineOpSeq).
              onClick={() => void st.useModel(spec.id)}
            >
              {t("library.useThisModel")}
            </button>
            {removeButton}
          </>
        ) : (
          <>
            <button className="btn-ghost" disabled={st.fit === "tooBig"} onClick={download}>
              {st.partial
                ? t("library.resumeDownload")
                : t("library.downloadSize", { size: formatBytes(totalModelSizeBytes(spec)) })}
            </button>
            {st.partial && removeButton}
          </>
        )}
      </div>
      {dialogs}
    </>
  );
}

function ModelCard({ model }: { model: ModelInfo }) {
  const { t } = useTranslation();
  const { spec, why, isRecommended } = model;
  const actions = useModelActions(model);
  const { st } = actions;

  return (
    <article
      className={`model-card${isRecommended ? " model-card-recommended" : ""}${
        st.fit === "tooBig" ? " model-card-dim" : ""
      }`}
    >
      {isRecommended && <span className="model-flag">{t("library.recommendedFlag")}</span>}
      {st.isCustom && <span className="model-flag model-flag-you">{t("library.customFlag")}</span>}
      <div className="model-card-head">
        <h2>{spec.name}</h2>
        <div className="model-card-head-actions">
          <FavoriteButton modelId={spec.id} name={spec.name} favorite={model.favorite} />
          <span className={`badge ${FIT_BADGE[st.fit]}`}>{fitLabel(st.fit)}</span>
        </div>
      </div>
      <p className="model-meta">
        {t("library.modelMeta", {
          publisher: spec.publisher,
          params: paramsWithActiveLabel(spec),
          size: formatBytes(totalModelSizeBytes(spec)),
        })}
      </p>
      <p className="model-desc">{spec.description}</p>
      <div className="model-chips">
        {st.isStartupDefault && (
          <span className="badge badge-accent" title={t("library.defaultAtStartupTitle")}>
            {t("library.defaultAtStartup")}
          </span>
        )}
        {spec.vision && (
          <span className="badge badge-accent" title={t("library.readsImagesTitle")}>
            <ImageIcon />
            {t("library.readsImages")}
          </span>
        )}
        {spec.strengths.map((s) => (
          <span key={s} className="badge badge-neutral">
            {s}
          </span>
        ))}
      </div>
      <p className="model-why">{why}</p>
      {st.speed > 0 && (
        <p className="model-speed">
          {t("library.speedEstimateLabel", { chip: st.hardware?.chip })}{" "}
          <strong>{formatTokensPerSec(st.speed)}</strong>
          {st.longAnswerDuration && (
            <> · {t("library.speedLongAnswer", { duration: st.longAnswerDuration })}</>
          )}
        </p>
      )}

      <footer>
        <ModelActionFooter actions={actions} />
      </footer>
    </article>
  );
}

/* ------------------------------ All models ------------------------------ */

/**
 * "All models" — literally all of them: the curated picks shown above in the
 * friendly picker too (so this list stays a genuine, complete reference),
 * plus every model the user brought in themselves.
 *
 * One compact two-line row per model rather than a table. The table this
 * replaces carried thirteen `white-space: nowrap` columns inside a content
 * column that is 746px wide in the default window and 546px at its minimum,
 * so it always overflowed sideways and hid half of itself — including the
 * action buttons. Everything those columns held is still here: the
 * structural detail (author, parameters, architecture, quantization, release,
 * license) moved to a muted second line, and the verbose tokens-per-second
 * prose became a coloured dot plus a compact rate, with the full sentence
 * kept for screen readers and as a tooltip.
 */
function ModelList({ models }: { models: ModelInfo[] }) {
  const { t } = useTranslation();
  const hardware = useStore((s) => s.report)?.hardware ?? null;
  const engine = useStore((s) => s.engine);
  const runningModelId = engine.type === "running" ? engine.data.modelId : null;

  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<ModelFilter>("all");
  const [sort, setSort] = useState<ModelSort>("recommended");

  const groups = useMemo(
    () => buildModelList(models, query, filter, sort, { hardware, runningModelId }),
    [models, query, filter, sort, hardware, runningModelId],
  );
  const shown = groups.reduce((n, g) => n + g.length, 0);
  const filtered = query.trim() !== "" || filter !== "all";

  // The toolbar earns its space only once there's something to navigate. On a
  // default install this list is the four curated picks, where a search box
  // and a sort menu are pure noise — exactly the kind of default-path
  // complexity ARCHITECTURE.md's beginner-first rule exists to prevent.
  const showToolbar = models.length > 8 || models.some((m) => m.spec.source !== "catalog");

  const reset = () => {
    setQuery("");
    setFilter("all");
  };

  return (
    <>
      {showToolbar && (
        <div className="model-list-toolbar">
          <div className="model-search">
            <SearchIcon />
            {/* Deliberately type="text", not type="search": nothing in this
                app styles `input[type="search"]`, so it would ignore the
                global input rules and render the engine's own searchfield
                chrome inside this pill. Both existing search fields in the
                app do the same. */}
            <input
              type="text"
              className="model-search-input"
              aria-label={t("library.list.searchAriaLabel")}
              placeholder={t("library.list.searchPlaceholder")}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </div>
          <div className="model-filters" role="group" aria-label={t("library.list.filtersAriaLabel")}>
            {MODEL_FILTERS.map((f) => (
              <button
                key={f}
                type="button"
                className={`model-filter-chip${filter === f ? " is-active" : ""}`}
                aria-pressed={filter === f}
                onClick={() => setFilter(f)}
              >
                {t(`library.list.filter.${f}`)}
              </button>
            ))}
          </div>
          <select
            className="settings-select model-sort"
            aria-label={t("library.list.sortLabel")}
            value={sort}
            onChange={(e) => setSort(e.target.value as ModelSort)}
          >
            {MODEL_SORTS.map((s) => (
              <option key={s} value={s}>
                {t(`library.list.sort.${s}`)}
              </option>
            ))}
          </select>
          {shown > 0 && (
            // Plain text, not a live region: the search filters on every
            // keystroke, so announcing a running tally would talk over the
            // character echo. The empty state below is the one that announces.
            <p className="model-list-count">{t("library.list.count", { count: shown })}</p>
          )}
        </div>
      )}

      {groups.length === 0 ? (
        <div className="model-list-empty" role="status">
          <p>{t("library.list.emptyFiltered")}</p>
          {filtered && (
            <button className="btn-quiet" onClick={reset}>
              {t("library.list.resetFilters")}
            </button>
          )}
        </div>
      ) : (
        <ul className="model-list">
          {groups.map((group) =>
            group.length === 1 ? (
              <ModelRow key={group[0].spec.id} model={group[0]} />
            ) : (
              <ModelGroupRow key={group[0].spec.id} models={group} />
            ),
          )}
        </ul>
      )}
    </>
  );
}

/** Non-animated speed dots. `.pill-dot-warm` pulses in chat, where it means
 * "working"; on a static list that would just be a distraction, so
 * `library.css` stops the animation inside a row. */
const SPEED_DOT: Record<"fast" | "medium" | "slow", string> = {
  fast: "pill-dot-ok",
  medium: "pill-dot-warm",
  slow: "pill-dot-bad",
};

/**
 * The badges a row shows, split by how much horizontal room they deserve.
 *
 * Only "in use" earns a place on line 1: it is short, and it is a live state
 * rather than a property of the model. Everything else — our pick, startup
 * default, OpenPal default, added by you — is a label that would otherwise
 * crowd out the model name, which is the one thing on the row that must
 * always be readable. At a 546px column with French labels, a
 * "Défaut OpenPal" badge next to the name leaves the name literally zero
 * pixels, so these ride the meta line as plain text instead.
 */
function rowTags(
  st: ModelState,
  model: ModelInfo,
  tFull: ReturnType<typeof useTranslation>["t"],
): { badge: string | null; meta: string[] } {
  // Cast once, here, to a plain, cheap function type — every call below
  // passes a literal key and nothing i18next-specific, so nothing real is
  // lost. Calling `tFull` directly (its real, heavily overloaded i18next
  // type) six times over hits TS's "type instantiation is excessively deep"
  // limit, same as `DiskSlowDialog`'s `diskSlowConfirmCopy` and
  // `ToolConsentCard.tsx`'s `dangerFlagText`.
  const t = tFull as (key: string, options?: Record<string, unknown>) => string;
  const meta: string[] = [];
  if (model.isRecommended) meta.push(t("library.recommendedFlag"));
  if (st.isStartupDefault) meta.push(t("library.defaultAtStartup"));
  if (model.spec.curated) meta.push(t("library.list.curatedFlag"));
  else if (st.isCustom) meta.push(t("library.customFlag"));
  return { badge: st.inUse ? t("library.inUse") : null, meta };
}

/**
 * One model, as a two-line row. `asQuant` renders it as a sub-row of a
 * multi-quantization group: the quantization is what distinguishes it from
 * its siblings, so that leads instead of the (identical) model name, and the
 * shared identity stays on the parent row above.
 */
function ModelRow({ model, asQuant = false }: { model: ModelInfo; asQuant?: boolean }) {
  const { t } = useTranslation();
  const { st, dialogs, download, askDelete, removeLabel, removeAriaLabel } =
    useModelActions(model);
  const { spec } = st;
  const tags = asQuant ? { badge: null, meta: [] } : rowTags(st, model, t);

  const size = formatBytes(totalModelSizeBytes(spec));

  let primary: ReactNode = null;
  if (st.progress) {
    primary = null;
  } else if (st.inUse) {
    primary = (
      <button className="btn-quiet" onClick={st.goToChat}>
        {t("library.goToChat")}
      </button>
    );
  } else if (st.waking) {
    primary = (
      <span className="model-row-status">
        {st.fit === "diskSlow" ? t("library.wakingSlow") : t("library.waking")}
      </span>
    );
  } else if (st.localFileMissing) {
    // Its spec has no download URL, so this must never fall through to the
    // download branch below — that button could not do anything. Removing it
    // from the library is the only action available, so it gets the primary
    // slot rather than hiding in the overflow menu.
    primary = (
      <button
        className="btn-quiet btn-danger-quiet"
        aria-label={removeAriaLabel}
        onClick={askDelete}
      >
        {t("library.removeFromLibrary")}
      </button>
    );
  } else if (st.installed) {
    primary = (
      <button
        className="btn-ghost model-row-btn"
        // Never disabled by a load in flight — see the card's own comment.
        onClick={() => void st.useModel(spec.id)}
      >
        {t("library.useThisModel")}
      </button>
    );
  } else {
    primary = (
      <button
        className="btn-ghost model-row-btn"
        disabled={st.fit === "tooBig"}
        onClick={download}
      >
        {st.partial ? t("library.resumeDownload") : t("library.downloadSize", { size })}
      </button>
    );
  }

  // Mirrors the primary-action branches above, so the menu never repeats what
  // the button already offers — and a row with nothing destructive to offer
  // gets no trigger at all rather than an empty menu.
  const menuItems: MenuItem[] = [];
  if (st.progress) {
    menuItems.push({
      label: t("common.cancel"),
      ariaLabel: t("library.cancelDownloadAriaLabel", { name: spec.name }),
      danger: true,
      onClick: () => void st.cancelDownload(spec.id),
    });
  } else if (st.localFileMissing) {
    // Its only action is already the primary button.
  } else if ((st.installed && !st.inUse && !st.waking) || st.partial) {
    menuItems.push({
      label: removeLabel,
      ariaLabel: removeAriaLabel,
      danger: true,
      onClick: askDelete,
    });
  }

  // One line of secondary text, whichever matters most right now. A failed
  // download's message outlives the progress entry (it is cleared only by the
  // next attempt), so it takes precedence over the static spec line.
  let meta: ReactNode = null;
  if (st.error) {
    meta = (
      <p className="model-row-meta model-row-error" role="alert">
        {st.error}
      </p>
    );
  } else if (st.progress) {
    meta = (
      <p className="model-row-meta">
        {t("library.downloadProgressOf", {
          received: formatBytes(st.progress.receivedBytes),
          total: formatBytes(st.progress.totalBytes || totalModelSizeBytes(spec)),
        })}
        {st.progress.bytesPerSec > 0 && <> · {formatSpeed(st.progress.bytesPerSec)}</>}
        {st.progress.etaSeconds > 0 && <> · {formatEta(st.progress.etaSeconds)}</>}
      </p>
    );
  } else if (st.localFileMissing) {
    meta = <p className="model-row-meta">{t("library.fileMissing")}</p>;
  } else {
    // The spec detail that used to need six of the table's columns. A quant
    // sub-row leaves it out: its parent row already carries the shared
    // identity, and the quantization itself is the sub-row's own name.
    const specParts = asQuant
      ? []
      : [
          ...tags.meta,
          spec.publisher,
          paramsWithActiveLabel(spec),
          architectureLabel(spec),
          spec.quant,
          releaseDateLabel(spec.releaseDate),
          spec.license,
        ].filter(Boolean);
    // Size and speed have their own columns on a wide window; below ~1000px
    // those columns are dropped and this copy takes over (CSS decides which,
    // and `display: none` keeps the hidden one out of the accessibility tree
    // too, so neither is ever announced twice).
    const compactParts = [size, st.speed > 0 ? formatSpeedCompact(st.speed) : null].filter(
      Boolean,
    );
    meta = (
      <p className="model-row-meta">
        <span className="model-row-meta-compact">
          {compactParts.join(" · ")}
          {compactParts.length > 0 && specParts.length > 0 ? " · " : ""}
        </span>
        {specParts.join(" · ")}
      </p>
    );
  }

  return (
    <li className={`model-row${asQuant ? " model-row-quant" : ""}`}>
      <div className="model-row-star">
        <FavoriteButton modelId={spec.id} name={spec.name} favorite={model.favorite} />
      </div>
      <div className="model-row-name">
        <span className="model-row-name-text">{asQuant ? spec.quant : spec.name}</span>
        {tags.badge && (
          <span className="badge badge-accent model-row-flag">{tags.badge}</span>
        )}
        {spec.vision && !asQuant && (
          <span className="model-row-icon" title={t("library.readsImagesTitle")}>
            <ImageIcon />
            <span className="sr-only">{t("library.readsImages")}</span>
          </span>
        )}
      </div>
      <div className="model-row-fit">
        <FitIndicator fit={st.fit} />
      </div>
      <div className="model-row-size">{size}</div>
      <div className="model-row-speed">
        {st.speed > 0 ? (
          <>
            <span className={`pill-dot ${SPEED_DOT[speedTier(st.speed)]}`} />
            <span title={formatTokensPerSec(st.speed)}>{formatSpeedCompact(st.speed)}</span>
            {/* The beginner-facing prose stays in the accessibility tree —
                `title` alone is mouse-only. */}
            <span className="sr-only">{formatTokensPerSec(st.speed)}</span>
          </>
        ) : (
          <>
            <span className="pill-dot pill-dot-off" />
            <span aria-hidden="true">-</span>
          </>
        )}
      </div>
      <div className="model-row-action">{primary}</div>
      <div className="model-row-menu-slot">
        <ModelRowMenu name={spec.name} items={menuItems} />
      </div>
      {meta}
      {st.progress && (
        <ProgressBar
          className="model-row-progress"
          barClassName="model-progress-bar"
          value={
            st.progress.totalBytes > 0 ? st.progress.receivedBytes / st.progress.totalBytes : 0
          }
          label={t("library.downloadProgressAriaLabel", { name: spec.name })}
        />
      )}
      {dialogs}
    </li>
  );
}

/**
 * Several quantizations of the same model — in practice, two or more quants
 * of one Hugging Face repo, since `hf_spec` gives every Discover-added model
 * its repo id as a group id and no catalog entry sets one.
 *
 * The shared identity sits on a parent row; each quantization is a sub-row
 * behind a disclosure, carrying its own size, fit, speed and actions (they
 * are independent models with independent install states). Collapsed
 * sub-rows aren't mounted at all.
 */
function ModelGroupRow({ models }: { models: ModelInfo[] }) {
  const { t } = useTranslation();
  const primary = models[0];
  const downloads = useStore((s) => s.downloads);
  const engine = useStore((s) => s.engine);
  const st = useModelState(primary);
  const panelId = useId();

  const runningId = engine.type === "running" ? engine.data.modelId : null;
  // Never hide a quantization that's actually doing something. Knowable from
  // the models themselves, so the parent decides without mounting children.
  const live = models.some(
    (m) =>
      m.status.type !== "notInstalled" || m.spec.id === runningId || Boolean(downloads[m.spec.id]),
  );
  const [open, setOpen] = useState(live);
  // Expand if a quantization becomes live later (a download starts, one
  // finishes). Deliberately one-way: it never collapses a group the user
  // opened.
  useEffect(() => {
    if (live) setOpen(true);
  }, [live]);

  const tags = rowTags(st, primary, t);

  return (
    <li className="model-row model-row-group">
      <div className="model-row-star">
        <FavoriteButton
          modelId={primary.spec.id}
          name={primary.spec.name}
          favorite={primary.favorite}
        />
      </div>
      <div className="model-row-name">
        <span className="model-row-name-text">{primary.spec.name}</span>
        {tags.badge && (
          <span className="badge badge-accent model-row-flag">{tags.badge}</span>
        )}
        {primary.spec.vision && (
          <span className="model-row-icon" title={t("library.readsImagesTitle")}>
            <ImageIcon />
            <span className="sr-only">{t("library.readsImages")}</span>
          </span>
        )}
      </div>
      <div className="model-row-summary">
        <span>{t("library.list.quantCount", { count: models.length })}</span>
        <QuantInfo />
      </div>
      <div className="model-row-action">
        <button
          type="button"
          className={`btn-quiet model-row-disclosure${open ? " is-open" : ""}`}
          aria-expanded={open}
          aria-controls={panelId}
          aria-label={t("library.list.quantsAriaLabel", {
            count: models.length,
            name: primary.spec.name,
          })}
          onClick={() => setOpen((v) => !v)}
        >
          <DisclosureIcon />
        </button>
      </div>
      <div className="model-row-menu-slot" />
      <p className="model-row-meta">
        {[
          ...tags.meta,
          primary.spec.publisher,
          paramsWithActiveLabel(primary.spec),
          architectureLabel(primary.spec),
          releaseDateLabel(primary.spec.releaseDate),
          primary.spec.license,
        ]
          .filter(Boolean)
          .join(" · ")}
      </p>
      {open && (
        <ul className="model-row-quants" id={panelId}>
          {models.map((m) => (
            <ModelRow key={m.spec.id} model={m} asQuant />
          ))}
        </ul>
      )}
    </li>
  );
}

interface MenuItem {
  label: string;
  ariaLabel?: string;
  danger?: boolean;
  onClick: () => void;
}

/** Roughly how tall the panel gets, for the "would this open off the bottom
 * of the window?" flip. A couple of items plus padding. */
const MENU_HEIGHT_ESTIMATE = 110;

/**
 * A row's "…" overflow menu, holding the destructive actions that used to sit
 * inline in every table row. Portal-rendered at a fixed, measured position
 * rather than positioned within the row, which `.library-scroll`'s
 * `overflow-y: auto` would clip near either scrolled edge — the same problem
 * and the same fix as the sidebar's per-conversation menu.
 */
function ModelRowMenu({ name, items }: { name: string; items: MenuItem[] }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState<{ right: number; offset: number; openUpward: boolean } | null>(
    null,
  );
  const triggerRef = useRef<HTMLButtonElement>(null);

  // However the menu closes, focus belongs back on its trigger: the panel is
  // portal-rendered outside the trigger's subtree, so closing it removes
  // whatever held focus from the document entirely.
  const close = () => {
    setOpen(false);
    triggerRef.current?.focus();
  };

  // The trigger sits outside the portalled panel's subtree, so it must be
  // exempted from the outside-click check or its own click would close and
  // immediately reopen the menu. Handles Escape too.
  const panelRef = useCloseOnOutside<HTMLDivElement>(open, close, [triggerRef]);
  useCloseOnScroll(open, close);

  if (items.length === 0) return null;

  const toggle = () => {
    if (open) {
      close();
      return;
    }
    const rect = triggerRef.current?.getBoundingClientRect();
    if (!rect) return;
    const openUpward = window.innerHeight - rect.bottom < MENU_HEIGHT_ESTIMATE;
    setPos({
      right: window.innerWidth - rect.right,
      offset: openUpward ? window.innerHeight - rect.top + 6 : rect.bottom + 6,
      openUpward,
    });
    setOpen(true);
  };

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        className="btn-quiet model-row-menu-trigger"
        aria-haspopup="true"
        aria-expanded={open}
        aria-label={t("library.list.moreActionsAriaLabel", { name })}
        onClick={toggle}
      >
        <MoreIcon />
      </button>
      {open &&
        pos &&
        createPortal(
          <div
            ref={panelRef}
            className="menu model-row-menu"
            style={{
              position: "fixed",
              left: "auto",
              right: pos.right,
              top: pos.openUpward ? "auto" : pos.offset,
              bottom: pos.openUpward ? pos.offset : "auto",
            }}
          >
            {items.map((item) => (
              <button
                key={item.label}
                type="button"
                className={`menu-item${item.danger ? " menu-item-danger" : ""}`}
                aria-label={item.ariaLabel}
                onClick={() => {
                  close();
                  item.onClick();
                }}
              >
                <span className="menu-item-label">{item.label}</span>
              </button>
            ))}
          </div>,
          document.body,
        )}
    </>
  );
}
