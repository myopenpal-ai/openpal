import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../lib/api";
import {
  estimateAnswerDuration,
  FIT_BADGE,
  fitLabel,
  formatBytes,
  formatTokensPerSec,
  roughFitLabel,
  totalModelSizeBytes,
} from "../lib/format";
import { estimateTokensPerSec } from "../lib/speedEstimate";
import Markdown from "../components/Markdown";
import type { HfModelDetails, HfModelSummary, HfQuantPicker, HfSortKey } from "../lib/types";
import { useStore } from "../state/store";

const SORT_OPTIONS: HfSortKey[] = ["trending", "downloads", "likes", "lastModified"];

const SEARCH_DEBOUNCE_MS = 400;

/** All browsing happens on the default branch — there's no revision picker
 * in v1 (see docs/design/HF_BROWSER.md open questions). */
const REVISION = "main";

function formatCount(n: number | null): string {
  if (n === null) return "-";
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${Math.round(n / 1000)}K`;
  return String(n);
}

/** "owner/repo" -> ("owner", "repo"), falling back to the whole id if it's
 * ever missing the slash (shouldn't happen for anything the hub returns). */
function splitRepoId(repoId: string): { owner: string; name: string } {
  const slash = repoId.indexOf("/");
  return slash === -1
    ? { owner: repoId, name: repoId }
    : { owner: repoId.slice(0, slash), name: repoId.slice(slash + 1) };
}

export default function Discover() {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<HfSortKey>("trending");
  const [results, setResults] = useState<HfModelSummary[]>([]);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [openRepoId, setOpenRepoId] = useState<string | null>(null);
  const [onlyFitting, setOnlyFitting] = useState(false);

  // The fit hint is a rough estimate (typical quant, not the file the user
  // will actually pick — see HfModelSummary.fit), so "only fitting" also
  // hides repos the hub couldn't estimate at all rather than assume they fit.
  const visibleResults = onlyFitting
    ? results.filter((m) => m.fit !== null && m.fit !== "tooBig")
    : results;

  // Tauri invokes aren't cancellable, so a slow response for an earlier
  // query/sort must never clobber a later one's results — every fetch below
  // is stamped with the sequence number current when it started and checks
  // it's still current before touching state. This also makes React
  // StrictMode's double-invoked effects safe: the first (cleaned-up)
  // invocation's response arrives with a stale sequence and is dropped.
  const requestSeq = useRef(0);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => { alive.current = false; };
  }, []);

  // Debounced search-or-sort fetch, replacing the result set from page 1.
  useEffect(() => {
    const seq = ++requestSeq.current;
    setLoading(true);
    setError(null);
    const timer = window.setTimeout(() => {
      void api
        .searchHfHub({ search: query.trim() || null, author: null, pipelineTag: null, sort, cursor: null })
        .then((page) => {
          if (!alive.current || seq !== requestSeq.current) return;
          setResults(page.models);
          setNextCursor(page.nextCursor);
        })
        .catch((err: unknown) => {
          if (!alive.current || seq !== requestSeq.current) return;
          setResults([]);
          setNextCursor(null);
          setError(String(err));
        })
        .finally(() => {
          if (!alive.current || seq !== requestSeq.current) return;
          setLoading(false);
        });
    }, SEARCH_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [query, sort]);

  const loadMore = () => {
    if (loadingMore || loading || !nextCursor) return;
    setLoadingMore(true);
    setError(null);
    const seq = ++requestSeq.current;
    void api
      .searchHfHub({ search: query.trim() || null, author: null, pipelineTag: null, sort, cursor: nextCursor })
      .then((page) => {
        if (!alive.current || seq !== requestSeq.current) return;
        setResults((prev) => [...prev, ...page.models]);
        setNextCursor(page.nextCursor);
      })
      .catch((err: unknown) => {
        if (!alive.current || seq !== requestSeq.current) return;
        setError(String(err));
      })
      .finally(() => {
        if (!alive.current || seq !== requestSeq.current) return;
        setLoadingMore(false);
      });
  };

  // Infinite scroll: fetch the next page once the sentinel below the grid
  // enters the viewport. Re-observing on every render is cheap (one element)
  // and keeps `loadMore` a plain closure over current state, rather than
  // threading everything through refs. Suspended while the model page is open
  // (the sentinel isn't mounted then, so this is mostly a no-op either way).
  const sentinelRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const node = sentinelRef.current;
    if (!node) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries[0]?.isIntersecting) loadMore();
      },
      { rootMargin: "600px" },
    );
    observer.observe(node);
    return () => observer.disconnect();
  });

  return (
    <div className="discover">
      <div className="discover-scroll">
        <div className="discover-col">
          {openRepoId ? (
            <ModelPage repoId={openRepoId} onBack={() => setOpenRepoId(null)} />
          ) : (
            <>
              <header className="discover-head">
                <h1>{t("discover.title")}</h1>
                <p>{t("discover.subtitle")}</p>
              </header>

              <div className="discover-controls">
                <input
                  type="text"
                  aria-label={t("discover.searchLabel")}
                  placeholder={t("discover.searchPlaceholder")}
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                />
                <select
                  className="settings-select"
                  aria-label={t("discover.sortLabel")}
                  value={sort}
                  onChange={(e) => setSort(e.target.value as HfSortKey)}
                >
                  {SORT_OPTIONS.map((key) => (
                    <option key={key} value={key}>
                      {t(`discover.sort.${key}`)}
                    </option>
                  ))}
                </select>
                <label className="discover-fit-toggle">
                  <input
                    type="checkbox"
                    checked={onlyFitting}
                    onChange={(e) => setOnlyFitting(e.target.checked)}
                  />
                  {t("discover.fitToggle")}
                </label>
              </div>

              {error && <p className="model-error" role="alert">{error}</p>}

              {loading ? (
                <p className="discover-status" role="status">{t("discover.looking")}</p>
              ) : results.length === 0 ? (
                <div className="discover-empty" role="status">
                  <p>{t("discover.emptyResults")}</p>
                </div>
              ) : visibleResults.length === 0 ? (
                <div className="discover-empty" role="status">
                  <p>{t("discover.emptyFitting")}</p>
                </div>
              ) : (
                <>
                  <div className="discover-grid">
                    {visibleResults.map((m) => (
                      <DiscoverCard key={m.repoId} model={m} onOpen={() => setOpenRepoId(m.repoId)} />
                    ))}
                  </div>
                  <div ref={sentinelRef} className="discover-sentinel" />
                  {loadingMore && <p className="discover-status" role="status">{t("discover.loadingMore")}</p>}
                </>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function DiscoverCard({ model, onOpen }: { model: HfModelSummary; onOpen: () => void }) {
  const { t } = useTranslation();
  const { owner: fallbackOwner, name } = splitRepoId(model.repoId);
  const owner = model.author ?? fallbackOwner;

  return (
    <article className="discover-card" role="button" tabIndex={0} onClick={onOpen}
      onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onOpen(); } }}
    >
      <div className="discover-card-head">
        <h2 title={model.repoId}>{name}</h2>
        {model.gated && <span className="badge badge-tight">{t("discover.gatedBadge")}</span>}
        {model.fit && (
          <span className="discover-card-fit">
            <span className={`badge ${FIT_BADGE[model.fit]}`} title={model.fitWhy ?? undefined}>
              {roughFitLabel(model.fit)}
            </span>
            <span className="discover-card-fit-hint">{t("discover.fitEstimateHint")}</span>
          </span>
        )}
      </div>
      <p className="discover-card-owner">{owner}</p>
      <div className="discover-card-stats">
        <span>⬇ {formatCount(model.downloads)}</span>
        <span>♥ {formatCount(model.likes)}</span>
      </div>
      {(model.pipelineTag || model.license) && (
        <div className="discover-card-chips">
          {model.pipelineTag && <span className="badge badge-neutral">{model.pipelineTag}</span>}
          {model.license && <span className="badge badge-neutral">{model.license}</span>}
        </div>
      )}
    </article>
  );
}

function ModelPage({ repoId, onBack }: { repoId: string; onBack: () => void }) {
  const { t } = useTranslation();
  const refreshModels = useStore((s) => s.refreshModels);
  const setMoreDialogPage = useStore((s) => s.setMoreDialogPage);
  const hardware = useStore((s) => s.report)?.hardware;

  const [details, setDetails] = useState<HfModelDetails | null>(null);
  const [detailsError, setDetailsError] = useState<string | null>(null);
  const [loadingDetails, setLoadingDetails] = useState(true);

  const [picker, setPicker] = useState<HfQuantPicker | null>(null);
  const [pickerError, setPickerError] = useState<string | null>(null);
  const [loadingPicker, setLoadingPicker] = useState(true);

  const [readme, setReadme] = useState<string | null>(null);
  const [readmeError, setReadmeError] = useState<string | null>(null);
  const [loadingReadme, setLoadingReadme] = useState(false);

  const [busyPath, setBusyPath] = useState<string | null>(null);
  const [addedPaths, setAddedPaths] = useState<ReadonlySet<string>>(new Set());
  const [addError, setAddError] = useState<string | null>(null);

  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => { alive.current = false; };
  }, []);

  // Every fetch below is keyed off `repoId` and guards on `alive` — opening a
  // different model while one is still loading (or navigating back and into
  // another) must never let a stale response land on the new page.
  useEffect(() => {
    setLoadingDetails(true);
    setDetailsError(null);
    setDetails(null);
    void api
      .getHfModelDetails(repoId)
      .then((d) => { if (alive.current) setDetails(d); })
      .catch((err: unknown) => { if (alive.current) setDetailsError(String(err)); })
      .finally(() => { if (alive.current) setLoadingDetails(false); });
  }, [repoId]);

  useEffect(() => {
    setLoadingPicker(true);
    setPickerError(null);
    setPicker(null);
    setAddedPaths(new Set());
    setAddError(null);
    void api
      .getHfQuantPicker(repoId, REVISION)
      .then((p) => { if (alive.current) setPicker(p); })
      .catch((err: unknown) => { if (alive.current) setPickerError(String(err)); })
      .finally(() => { if (alive.current) setLoadingPicker(false); });
  }, [repoId]);

  useEffect(() => {
    setReadme(null);
    setReadmeError(null);
  }, [repoId]);

  const loadReadme = () => {
    if (readme !== null || loadingReadme) return;
    setLoadingReadme(true);
    setReadmeError(null);
    void api
      .getHfModelReadme(repoId, REVISION)
      .then((text) => { if (alive.current) setReadme(text); })
      .catch((err: unknown) => { if (alive.current) setReadmeError(String(err)); })
      .finally(() => { if (alive.current) setLoadingReadme(false); });
  };

  const addFile = async (path: string) => {
    if (!picker || busyPath) return;
    setBusyPath(path);
    setAddError(null);
    try {
      await api.addHfModel(picker.repoId, picker.revision, path);
      setAddedPaths((prev) => new Set(prev).add(path));
      await refreshModels();
    } catch (err: unknown) {
      setAddError(String(err));
    } finally {
      setBusyPath(null);
    }
  };

  const { owner: fallbackOwner, name } = splitRepoId(repoId);
  const owner = details?.author ?? fallbackOwner;
  const gated = details?.gated ?? false;

  return (
    <div className="model-page">
      <button className="btn-quiet model-page-back" onClick={onBack}>
        {t("discover.backToBrowse")}
      </button>

      <header className="model-page-head">
        <h1 title={repoId}>{name}</h1>
        <p className="discover-card-owner">{t("discover.byOwner", { owner })}</p>

        {gated && <p className="model-page-gated">{t("discover.gatedNotice")}</p>}

        <div className="discover-card-chips">
          {details?.pipelineTag && <span className="badge badge-neutral">{details.pipelineTag}</span>}
          {details?.license && <span className="badge badge-neutral">{details.license}</span>}
          {details?.gguf?.architecture && (
            <span className="badge badge-neutral">{details.gguf.architecture}</span>
          )}
          {details?.downloads != null && (
            <span className="badge badge-neutral">
              {t("discover.downloadsCount", { count: formatCount(details.downloads) })}
            </span>
          )}
          {details?.likes != null && <span className="badge badge-neutral">♥ {formatCount(details.likes)}</span>}
        </div>

        {details?.gguf?.contextLength != null && (
          <p className="model-page-note">
            {t("discover.contextNote", { count: details.gguf.contextLength.toLocaleString() })}
          </p>
        )}

        <button className="btn-quiet" onClick={() => void api.openLink(`https://huggingface.co/${repoId}`)}>
          {t("discover.openOnHf")}
        </button>

        {detailsError && <p className="model-error" role="alert">{detailsError}</p>}
        {loadingDetails && !details && !detailsError && (
          <p className="discover-status" role="status">{t("discover.loadingGeneric")}</p>
        )}
      </header>

      <details className="model-page-readme" onToggle={(e) => (e.target as HTMLDetailsElement).open && loadReadme()}>
        <summary>{t("discover.modelCardSummary")}</summary>
        {loadingReadme ? (
          <p className="discover-status" role="status">{t("discover.loadingGeneric")}</p>
        ) : readmeError ? (
          <p className="model-error" role="alert">{readmeError}</p>
        ) : readme !== null ? (
          readme.trim() ? (
            <Markdown>{readme}</Markdown>
          ) : (
            <p className="discover-status" role="status">{t("discover.noModelCard")}</p>
          )
        ) : null}
      </details>

      <section className="model-page-files">
        <h2>{t("discover.filesHeading")}</h2>
        {loadingPicker ? (
          <p className="discover-status" role="status">{t("discover.looking")}</p>
        ) : pickerError ? (
          <p className="model-error" role="alert">{pickerError}</p>
        ) : picker && picker.options.length > 0 ? (
          <ul className="settings-hf-files">
            {picker.options.map((opt) => {
              const speed = hardware
                ? estimateTokensPerSec(
                    {
                      sizeBytes: opt.file.sizeBytes,
                      extraShards: opt.file.extraShards,
                      moe: opt.moe,
                      activeParamsB: opt.activeParamsB,
                      paramsB: opt.paramsB,
                    },
                    hardware,
                    opt.fit,
                  )
                : 0;
              const duration = estimateAnswerDuration(speed);
              return (
                <li
                  key={opt.file.path}
                  className={opt.file.path === picker.preselectPath ? "is-preselected" : ""}
                >
                  <div className="settings-hf-file-info">
                    <span className="settings-hf-file-name" title={opt.file.path}>
                      {opt.file.path}
                    </span>
                    <span className="settings-hf-file-meta">
                      {opt.file.quant} · {formatBytes(totalModelSizeBytes(opt.file))}
                      {opt.file.extraShards && opt.file.extraShards.length > 0 && (
                        <> · {t("discover.filesCount", { count: opt.file.extraShards.length + 1 })}</>
                      )}
                    </span>
                    <span className={`badge ${FIT_BADGE[opt.fit]}`} title={opt.why}>
                      {fitLabel(opt.fit)}
                    </span>
                    {!opt.precise && (
                      <span className="discover-card-fit-hint">{t("discover.moeUncertain")}</span>
                    )}
                    {speed > 0 && duration && (
                      <span className="settings-hf-file-meta">
                        {t("discover.longAnswerEstimate", { speed: formatTokensPerSec(speed), duration })}
                      </span>
                    )}
                  </div>
                  {addedPaths.has(opt.file.path) ? (
                    <span className="settings-hf-added" role="status">{t("discover.addedBadge")}</span>
                  ) : (
                    <button
                      className="btn-quiet"
                      disabled={busyPath !== null || opt.fit === "tooBig" || gated}
                      onClick={() => void addFile(opt.file.path)}
                    >
                      {busyPath === opt.file.path ? t("discover.adding") : t("discover.addToLibrary")}
                    </button>
                  )}
                </li>
              );
            })}
          </ul>
        ) : null}

        {addError && <p className="model-error" role="alert">{addError}</p>}
        {addedPaths.size > 0 && (
          // role="status" on the wrapper (not the button itself, which would
          // lose its own semantics) so the button's appearance is announced.
          <div role="status">
            <button className="btn-quiet settings-see-library" onClick={() => setMoreDialogPage("library")}>
              {t("discover.seeInLibrary")}
            </button>
          </div>
        )}
      </section>
    </div>
  );
}
