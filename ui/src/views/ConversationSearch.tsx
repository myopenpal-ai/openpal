import { useEffect, useId, useRef, type KeyboardEvent, type ReactNode } from "react";
import { flushSync } from "react-dom";
import { useTranslation } from "react-i18next";
import { useDialogFocus } from "../components/useDialogFocus";
import { SETTINGS_ICONS } from "../components/pageIcons";
import { timeAgo } from "../lib/format";
import { SETTINGS_SECTIONS, useStore, type SettingsSection } from "../state/store";

const SearchIcon = () => (
  <svg
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <circle cx="10.5" cy="10.5" r="6.5" />
    <path d="M19.5 19.5 15 15" />
  </svg>
);

const CloseIcon = () => (
  <svg
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    aria-hidden="true"
  >
    <path d="M6 6l12 12M18 6 6 18" />
  </svg>
);

/** How many recent conversations the dialog offers before anything's typed. */
const RECENT_LIMIT = 5;

/**
 * Every place `query` occurs in `text`, as [start, end) code-point ranges —
 * the same whole-query, per-character case-insensitive match `store::search`
 * runs in Rust (crates/core/src/store.rs's `find_match_char_index`), so what
 * gets marked is exactly what made a result match. Walked per code point
 * rather than on a lowercased copy, for the same reason the Rust side is: a
 * few characters (`İ`) lowercase into more than one, which would shift every
 * offset after them.
 */
function findMatches(chars: string[], query: string): [number, number][] {
  const q = Array.from(query.trim(), (c) => c.toLowerCase());
  const ranges: [number, number][] = [];
  if (q.length === 0) return ranges;
  let i = 0;
  while (i + q.length <= chars.length) {
    if (q.every((c, j) => chars[i + j].toLowerCase() === c)) {
      ranges.push([i, i + q.length]);
      i += q.length;
    } else {
      i++;
    }
  }
  return ranges;
}

/** `text` with every match of `query` in it wrapped in a <mark>. */
function Highlight({ text, query }: { text: string; query: string }) {
  const chars = Array.from(text);
  const ranges = findMatches(chars, query);
  if (ranges.length === 0) return <>{text}</>;
  const parts: ReactNode[] = [];
  let from = 0;
  for (const [start, end] of ranges) {
    if (start > from) parts.push(chars.slice(from, start).join(""));
    parts.push(<mark key={start}>{chars.slice(start, end).join("")}</mark>);
    from = end;
  }
  if (from < chars.length) parts.push(chars.slice(from).join(""));
  return <>{parts}</>;
}

/**
 * The sidebar's search, promoted to its own dialog for a much bigger reading
 * area than the old cramped sidebar input ever had room for — the sidebar
 * itself now only carries a search icon (see Sidebar.tsx's
 * `.sidebar-list-toolbar`) that opens this. A dialog of its own rather than
 * a main-pane view or a More dialog page, so the conversation underneath
 * stays put until a result is actually picked. Reuses the store's existing
 * debounced `sidebarSearchQuery`/`sidebarSearchResults` (see
 * `setSidebarSearchQuery`) rather than adding a second search path — just a
 * roomier place to show the same results.
 *
 * Before anything's typed it offers somewhere to go instead of a blank page:
 * the latest conversations and a shortcut to each Settings category. Once
 * typing, each result marks what matched — in its title, or in the message
 * excerpt when only a message did.
 */
export default function ConversationSearch() {
  const { t } = useTranslation();
  const query = useStore((s) => s.sidebarSearchQuery);
  const results = useStore((s) => s.sidebarSearchResults);
  const conversations = useStore((s) => s.conversations);
  const activeId = useStore((s) => s.active?.id);
  const setQuery = useStore((s) => s.setSidebarSearchQuery);
  const streaming = useStore((s) => s.streaming);
  const openConversation = useStore((s) => s.openConversation);
  const setOpen = useStore((s) => s.setConversationSearchOpen);
  const setMoreDialogPage = useStore((s) => s.setMoreDialogPage);
  const setMobileSidebarOpen = useStore((s) => s.setMobileSidebarOpen);
  const inputRef = useRef<HTMLInputElement>(null);
  const resultsRef = useRef<HTMLDivElement>(null);
  const listHeadingId = useId();
  const settingsHeadingId = useId();

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // openConversation() already clears sidebarSearchQuery/Results itself on
  // success (see state/store.ts) — closing without picking a result just
  // needs the same reset, so the next search starts empty.
  const close = () => {
    setQuery("");
    setOpen(false);
  };
  const dialogRef = useDialogFocus<HTMLDivElement>(close);

  const pick = async (id: string) => {
    setMobileSidebarOpen(false);
    await openConversation(id);
    // A failed open leaves its notice up and the conversation it came from
    // on screen — stay open so the user can try another result.
    if (useStore.getState().active?.id === id) close();
  };

  const openSettings = (section: SettingsSection) => {
    setMobileSidebarOpen(false);
    // Unmount first, so this dialog has handed focus back to what opened it
    // before the More dialog mounts and records where to return focus on its
    // own close — in one render it would record the shortcut just clicked,
    // which is gone by then.
    flushSync(close);
    setMoreDialogPage(section);
  };

  const trimmed = query.trim();
  const recent = trimmed
    ? []
    : conversations
        .filter((c) => c.id !== activeId)
        .sort((a, b) => b.updatedAtMs - a.updatedAtMs)
        .slice(0, RECENT_LIMIT);
  // Every category before anything's typed; after, just the ones whose name
  // matches, matched the same way conversations are.
  const sections = SETTINGS_SECTIONS.map((section) => ({
    section,
    label: t(`settingsSheet.sections.${section}.label`),
  })).filter(({ label }) => !trimmed || findMatches(Array.from(label), trimmed).length > 0);

  // Arrow keys walk from the field down through every result and shortcut in
  // on-screen order and back up to the field; Enter on the one focused is
  // just the button's own.
  const onDialogKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const buttons = resultsRef.current?.querySelectorAll<HTMLElement>("button:not(:disabled)") ?? [];
    const stops = [inputRef.current, ...buttons];
    const at = stops.indexOf(document.activeElement as HTMLElement);
    const next = at === -1 ? undefined : stops[at + (e.key === "ArrowDown" ? 1 : -1)];
    if (!next) return;
    e.preventDefault();
    next.focus();
  };

  // Enter in the field opens the top result, so a search that already found
  // it needs no reach for the mouse.
  const onInputKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key !== "Enter" || e.nativeEvent.isComposing || !trimmed) return;
    const top = results[0];
    if (!top || streaming) return;
    e.preventDefault();
    void pick(top.id);
  };

  const conversationRow = (c: { id: string; title: string; updatedAtMs: number; snippet?: string }) => (
    <button
      key={c.id}
      type="button"
      className="conv-open conv-open-search conv-search-result"
      disabled={streaming}
      onClick={() => void pick(c.id)}
      title={c.title}
    >
      <span className="conv-search-result-head">
        <span className="conv-title">
          <Highlight text={c.title} query={trimmed} />
        </span>
        <span className="conv-search-when">{timeAgo(c.updatedAtMs)}</span>
      </span>
      {/* A title match's snippet is the title itself (see `store::search`) —
          shown once, already marked, above. */}
      {c.snippet && c.snippet !== c.title && (
        <span className="conv-snippet">
          <Highlight text={c.snippet} query={trimmed} />
        </span>
      )}
    </button>
  );

  return (
    <div className="modal-backdrop" onClick={close}>
      <div
        ref={dialogRef}
        tabIndex={-1}
        className="conv-search-dialog"
        role="dialog"
        aria-modal="true"
        aria-label={t("sidebar.search")}
        onClick={(e) => e.stopPropagation()}
        onKeyDown={onDialogKeyDown}
      >
        <div className="conv-search-header">
          <div className="conv-search-input-wrap">
            <SearchIcon />
            <input
              ref={inputRef}
              type="text"
              className="conv-search-input"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={onInputKeyDown}
              placeholder={t("sidebar.searchPlaceholder")}
              aria-label={t("sidebar.searchPlaceholder")}
            />
          </div>
          <button
            type="button"
            className="btn-quiet conv-search-close"
            onClick={close}
            aria-label={t("common.close")}
            title={t("common.close")}
          >
            <CloseIcon />
          </button>
        </div>
        <div ref={resultsRef} className="conv-search-results">
          {(trimmed || recent.length > 0) && (
            <section className="conv-search-section" aria-labelledby={listHeadingId}>
              <h2 id={listHeadingId} className="conv-search-section-title">
                {trimmed ? t("sidebar.searchView.conversations") : t("sidebar.searchView.recent")}
                {trimmed && results.length > 0 && (
                  <span className="conv-search-count">{results.length}</span>
                )}
              </h2>
              {trimmed ? (
                results.length === 0 ? (
                  <p className="conv-empty">{t("sidebar.noSearchResults")}</p>
                ) : (
                  results.map(conversationRow)
                )
              ) : (
                recent.map(conversationRow)
              )}
            </section>
          )}
          {sections.length > 0 && (
            <section className="conv-search-section" aria-labelledby={settingsHeadingId}>
              <h2 id={settingsHeadingId} className="conv-search-section-title">
                {t("sidebar.settings")}
              </h2>
              <div className="conv-search-shortcuts">
                {sections.map(({ section, label }) => {
                  const Icon = SETTINGS_ICONS[section];
                  return (
                    <button
                      key={section}
                      type="button"
                      className="conv-search-shortcut"
                      onClick={() => openSettings(section)}
                    >
                      <Icon />
                      <span className="conv-search-shortcut-label">
                        <Highlight text={label} query={trimmed} />
                      </span>
                    </button>
                  );
                })}
              </div>
            </section>
          )}
          {!trimmed && <p className="conv-search-hint">{t("sidebar.searchView.hint")}</p>}
          {/* Announces the outcome of each search, not the whole list. */}
          <p className="sr-only" role="status">
            {!trimmed
              ? ""
              : results.length === 0
                ? t("sidebar.noSearchResults")
                : t("sidebar.searchView.resultCount", { count: results.length })}
          </p>
        </div>
      </div>
    </div>
  );
}
