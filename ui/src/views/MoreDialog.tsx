import { useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  CompassIcon,
  ModelsIcon,
  PuzzleIcon,
  SETTINGS_ICONS,
  SkillIcon,
} from "../components/pageIcons";
import { useDialogFocus } from "../components/useDialogFocus";
import { searchScore, searchSettings } from "../lib/settingsSearch";
import {
  isSettingsSection,
  moreDialogPageAvailable,
  SETTINGS_SECTIONS,
  useStore,
  type MoreDialogPage,
} from "../state/store";
import Library from "./Library";
import Discover from "./Discover";
import Plugins from "./Plugins";
import SkillsView from "./SkillsView";
import SettingsSheet from "./SettingsSheet";

const iconProps = {
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: "1.9",
  strokeLinecap: "round",
  strokeLinejoin: "round",
  "aria-hidden": true,
} as const;

const CloseIcon = () => (
  <svg {...iconProps}>
    <path d="M6 6l12 12M18 6 6 18" />
  </svg>
);

const SearchIcon = () => (
  <svg {...iconProps}>
    <circle cx="10.5" cy="10.5" r="6.5" />
    <path d="M19.5 19.5 15 15" />
  </svg>
);

/** Every page but Settings', in nav order, labelled with the same strings as
 * the sidebar's More menu entries that open them. Settings' pages — one per
 * category, `SETTINGS_SECTIONS` — follow under their own heading. */
const PAGES = [
  { page: "library", label: "sidebar.models", Icon: ModelsIcon },
  { page: "discover", label: "sidebar.browse", Icon: CompassIcon },
  { page: "plugins", label: "sidebar.plugins", Icon: PuzzleIcon },
  { page: "skills", label: "sidebar.skills", Icon: SkillIcon },
] as const;

/** One thing the nav's search found: a page, or a setting on one of
 * Settings' pages — `where` names its category, and `reveal` is what to find
 * on that page once it's showing (see `revealSetting`). */
interface SearchResult {
  page: MoreDialogPage;
  label: string;
  Icon: typeof ModelsIcon;
  where?: string;
  reveal?: string[];
}

/**
 * Scrolls to the first of `texts` showing on `page` as a setting's label or
 * group title, flashes that setting and moves focus to its first control (or
 * to the setting itself, when it has none). False when none of them is
 * showing.
 */
function revealSetting(page: HTMLElement, texts: string[]): boolean {
  const labels = Array.from(
    page.querySelectorAll<HTMLElement>(".settings-label, .settings-group-title"),
  );
  const label = texts
    .map((text) => labels.find((l) => l.textContent?.trim() === text))
    .find((l) => l !== undefined);
  if (!label) return false;
  const setting = label.closest<HTMLElement>(".settings-field, .settings-group") ?? label;
  setting.scrollIntoView({ block: "center" });
  // Left on once played — the page remounts per category anyway — and
  // restarted when the same setting is picked again.
  page
    .querySelectorAll(".settings-search-hit")
    .forEach((el) => el.classList.remove("settings-search-hit"));
  void setting.offsetWidth;
  setting.classList.add("settings-search-hit");
  // A radio group has one Tab stop, the checked option: land there, not on
  // the first radio, or the arrow keys would start from the wrong place.
  const control =
    setting.querySelector<HTMLElement>("input[type=radio]:checked:not([disabled])") ??
    setting.querySelector<HTMLElement>(
      "input:not([disabled]), select:not([disabled]), textarea:not([disabled]), button:not([disabled])",
    );
  if (control) {
    control.focus({ preventScroll: true });
  } else {
    label.tabIndex = -1;
    label.focus({ preventScroll: true });
  }
  return true;
}

/**
 * Everything the sidebar's More menu leads to — Models, Browse, Plugins,
 * Skills and Settings — as pages of one large dialog, with a left nav to move
 * between them. Settings is split into categories, each its own page, listed
 * under a Settings heading. It sits over the app rather than replacing the main pane, so
 * the main pane stays reserved for talking to the model and closing this
 * lands back exactly where the user was (see `moreDialogPage`).
 */
export default function MoreDialog({ page }: { page: MoreDialogPage }) {
  // The Remote sections' titles live in their own namespaces, and the
  // search reads them too. Each namespaced section must be listed here or
  // `t` (whose key union comes from exactly these namespaces) won't
  // type-check its search-labelled keys.
  const { t } = useTranslation(["translation", "remoteAccess", "remoteClient", "remoteInference"]);
  const setPage = useStore((s) => s.setMoreDialogPage);
  const plugins = useStore((s) => s.plugins);
  const appInfo = useStore((s) => s.appInfo);
  const signedIn = useStore((s) => s.account !== null);
  const close = () => setPage(null);
  const dialogRef = useDialogFocus<HTMLDivElement>(close);
  const settingsHeadingId = useId();
  const navRef = useRef<HTMLElement>(null);
  const pageRef = useRef<HTMLDivElement>(null);
  const [query, setQuery] = useState("");
  const entries = PAGES.filter((p) => moreDialogPageAvailable(p.page, { plugins, appInfo }));
  // Any Settings category names the dialog "Settings", like the More menu
  // entry that opened it; the category itself is the page's heading.
  const label = isSettingsSection(page)
    ? t("sidebar.settings")
    : t((PAGES.find((p) => p.page === page) ?? PAGES[0]).label);
  const navItem = (p: MoreDialogPage) => ({
    type: "button" as const,
    className: `more-dialog-nav-item${p === page ? " more-dialog-nav-item-active" : ""}`,
    "aria-current": p === page ? ("page" as const) : undefined,
    onClick: () => setPage(p),
  });

  // While there's a query the nav lists what it finds instead of the pages:
  // pages by name first, then single settings, so "language" or "PIN" leads
  // straight to where it's changed.
  const trimmed = query.trim();
  const results: SearchResult[] = trimmed
    ? [
        ...entries.map(({ page: p, label, Icon }) => ({ page: p, label: t(label), Icon })),
        ...SETTINGS_SECTIONS.map((section) => ({
          page: section,
          label: t(`settingsSheet.sections.${section}.label`),
          Icon: SETTINGS_ICONS[section],
        })),
      ]
        .filter((r) => searchScore(trimmed, r.label) > 0)
        .concat(
          searchSettings(trimmed, { signedIn, plugins, appInfo }, (key) => t(key)).map((hit) => ({
            page: hit.section,
            label: hit.label,
            Icon: SETTINGS_ICONS[hit.section],
            where: t(`settingsSheet.sections.${hit.section}.label`),
            reveal: hit.reveal,
          })),
        )
    : [];

  // The result just picked, until its page has rendered: then its setting is
  // revealed, or — for a page, or a setting that isn't showing — focus lands
  // on that page's nav entry, as if it had been clicked. Either way focus
  // leaves the results, which are gone once the query is cleared.
  const picked = useRef<SearchResult | null>(null);
  const pick = (result: SearchResult) => {
    picked.current = result;
    setQuery("");
    setPage(result.page);
  };
  useEffect(() => {
    const result = picked.current;
    if (!result || result.page !== page) return;
    picked.current = null;
    if (result.reveal && pageRef.current && revealSetting(pageRef.current, result.reveal)) return;
    navRef.current?.querySelector<HTMLElement>('[aria-current="page"]')?.focus();
  });

  return (
    <div className="modal-backdrop" onClick={close}>
      <div
        ref={dialogRef}
        tabIndex={-1}
        className="more-dialog"
        role="dialog"
        aria-modal="true"
        aria-label={label}
        onClick={(e) => e.stopPropagation()}
      >
        <nav ref={navRef} className="more-dialog-nav" aria-label={t("sidebar.more")}>
          <div className="more-dialog-nav-top">
            <button
              type="button"
              className="more-dialog-close"
              onClick={close}
              aria-label={t("common.close")}
              title={t("common.close")}
            >
              <CloseIcon />
            </button>
            <div className="more-dialog-search">
              <SearchIcon />
              {/* type="text", like the app's other search fields — see
                  Library's model search for why not type="search". */}
              <input
                type="text"
                className="more-dialog-search-input"
                placeholder={t("moreDialog.search.placeholder")}
                aria-label={t("moreDialog.search.placeholder")}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.nativeEvent.isComposing && results.length > 0) {
                    e.preventDefault();
                    pick(results[0]);
                  }
                }}
                onKeyDownCapture={(e) => {
                  // Escape empties the search before it closes the dialog.
                  // Caught on the way down: useDialogFocus's Escape listener
                  // sits on the dialog's own node, which the event reaches
                  // before a bubbling React handler here would run.
                  if (e.key === "Escape" && query) {
                    e.stopPropagation();
                    setQuery("");
                  }
                }}
              />
            </div>
          </div>
          <p className="sr-only" role="status">
            {trimmed &&
              (results.length > 0
                ? t("moreDialog.search.results", { count: results.length })
                : t("moreDialog.search.noResults", { query: trimmed }))}
          </p>
          {trimmed ? (
            results.length > 0 ? (
              <ul className="more-dialog-nav-list more-dialog-search-results">
                {results.map((r) => (
                  <li key={`${r.page}:${r.label}`}>
                    <button
                      type="button"
                      className="more-dialog-nav-item more-dialog-search-result"
                      onClick={() => pick(r)}
                    >
                      <r.Icon />
                      <span className="more-dialog-search-result-text">
                        <span>{r.label}</span>
                        {/* Unrendered between flex items; keeps the two
                            apart in the button's accessible name. */}
                        {r.where && " "}
                        {r.where && (
                          <span className="more-dialog-search-result-where">{r.where}</span>
                        )}
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="more-dialog-search-empty" aria-hidden="true">
                {t("moreDialog.search.noResults", { query: trimmed })}
              </p>
            )
          ) : (
            <div className="more-dialog-nav-pages">
              <ul className="more-dialog-nav-list">
                {entries.map(({ page: p, label, Icon }) => (
                  <li key={p}>
                    <button {...navItem(p)}>
                      <Icon />
                      <span>{t(label)}</span>
                    </button>
                  </li>
                ))}
              </ul>
              <div className="more-dialog-nav-section">
                <p id={settingsHeadingId} className="more-dialog-nav-heading">
                  {t("sidebar.settings")}
                </p>
                <ul className="more-dialog-nav-list" aria-labelledby={settingsHeadingId}>
                  {SETTINGS_SECTIONS.map((section) => {
                    const Icon = SETTINGS_ICONS[section];
                    return (
                      <li key={section}>
                        <button {...navItem(section)}>
                          <Icon />
                          <span>{t(`settingsSheet.sections.${section}.label`)}</span>
                        </button>
                      </li>
                    );
                  })}
                </ul>
              </div>
            </div>
          )}
        </nav>
        <div ref={pageRef} className="more-dialog-page">
          {page === "library" ? (
            <Library />
          ) : page === "discover" ? (
            <Discover />
          ) : page === "plugins" ? (
            <Plugins />
          ) : page === "skills" ? (
            <SkillsView />
          ) : (
            <SettingsSheet section={page} />
          )}
        </div>
      </div>
    </div>
  );
}
