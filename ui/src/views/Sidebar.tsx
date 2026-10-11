import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
} from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { getVersion } from "@tauri-apps/api/app";
import { isStartingModelDiskSlow, recencyBucket, recencyBucketLabel, type RecencyBucket } from "../lib/format";
import type { Conversation, ConversationMeta, Project } from "../lib/types";
import {
  moreDialogPageAvailable,
  SIDEBAR_MAX_WIDTH,
  SIDEBAR_MIN_WIDTH,
  useStore,
  writingToolsAvailable,
  type MoreDialogPage,
  type View,
} from "../state/store";
import { useCloseOnOutside } from "../components/Dropdown";
import ConfirmDialog from "../components/ConfirmDialog";
import DeleteProjectDialog from "../components/DeleteProjectDialog";

const CompassIcon = () => (
  <svg
    className="sidebar-action-icon"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <circle cx="12" cy="12" r="8.5" />
    <path d="M14.8 9.2 13 13l-3.8 1.8L11 11l3.8-1.8Z" />
  </svg>
);

const PuzzleIcon = () => (
  <svg
    className="sidebar-action-icon"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M9.5 3.5a1.6 1.6 0 0 1 3.1 0c0 .5-.2.9-.4 1.3a.6.6 0 0 0 .5.9H15a1.5 1.5 0 0 1 1.5 1.5v2.4a.6.6 0 0 0 .9.5c.4-.3.8-.4 1.3-.4a1.6 1.6 0 0 1 0 3.1c-.5 0-.9-.2-1.3-.4a.6.6 0 0 0-.9.5V16A1.5 1.5 0 0 1 15 17.5h-2.4a.6.6 0 0 1-.5-.9c.2-.4.4-.8.4-1.3a1.6 1.6 0 0 0-3.1 0c0 .5.2.9.4 1.3a.6.6 0 0 1-.5.9H6.5A1.5 1.5 0 0 1 5 16v-2.3a.6.6 0 0 0-.9-.5c-.4.2-.8.4-1.3.4a1.6 1.6 0 0 1 0-3.1c.5 0 .9.1 1.3.4a.6.6 0 0 0 .9-.5V7.7A1.5 1.5 0 0 1 6.5 6.2H9a.6.6 0 0 0 .5-.9c-.2-.4-.4-.8-.4-1.3Z" />
  </svg>
);

const LanguagesIcon = () => (
  <svg
    className="sidebar-action-icon"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M5 7h8M9 4.5v2.5M7 7c0 3 1.8 5.3 4.5 6.5M11.5 7c-.6 2-1.8 3.8-3.5 5" />
    <path d="M13 19l3.2-8 3.2 8M14.4 16.4h3.6" />
  </svg>
);

const SkillIcon = () => (
  <svg
    className="sidebar-action-icon"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M12 4.5v3M12 16.5v3M4.5 12h3M16.5 12h3M7 7l2 2M15 15l2 2M17 7l-2 2M9 15l-2 2" />
    <circle cx="12" cy="12" r="2.4" />
  </svg>
);

const CorrectIcon = () => (
  <svg
    className="sidebar-action-icon"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M4.5 12.5 6.5 14.5 10 9" />
    <path d="M13 8.5h6.5M13 12h6.5M13 15.5h4.5" />
  </svg>
);

const SparkleIcon = () => (
  <svg
    className="sidebar-action-icon"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8Z" />
    <path d="M19 15l.8 2.2L22 18l-2.2.8L19 21l-.8-2.2L16 18l2.2-.8Z" />
  </svg>
);

const ChatIcon = () => (
  <svg
    className="new-chat-icon"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M4.5 6.5h15a1 1 0 0 1 1 1v8a1 1 0 0 1-1 1H10l-4 3v-3H4.5a1 1 0 0 1-1-1v-8a1 1 0 0 1 1-1Z" />
  </svg>
);

const MoreIcon = () => (
  <svg
    className="sidebar-action-icon"
    viewBox="0 0 24 24"
    fill="currentColor"
    aria-hidden="true"
  >
    <circle cx="12" cy="5.5" r="1.7" />
    <circle cx="12" cy="12" r="1.7" />
    <circle cx="12" cy="18.5" r="1.7" />
  </svg>
);

const EditIcon = () => (
  <svg
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M4 20h4L18 10l-4-4L4 16v4Z" />
    <path d="M13.5 6.5l4 4" />
  </svg>
);

const PlusIcon = () => (
  <svg
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="2.2"
    strokeLinecap="round"
    aria-hidden="true"
  >
    <path d="M12 5v14M5 12h14" />
  </svg>
);

const ChevronIcon = ({ collapsed }: { collapsed: boolean }) => (
  <svg
    className={`project-chevron${collapsed ? " project-chevron-collapsed" : ""}`}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="2.2"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M9 6l6 6-6 6" />
  </svg>
);

const SearchIcon = () => (
  <svg
    className="sidebar-toolbar-icon"
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

/** Checklist glyph for the "select multiple conversations" toggle. */
const ChecklistIcon = () => (
  <svg
    className="sidebar-toolbar-icon"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <rect x="4" y="4.5" width="6.5" height="6.5" rx="1.4" />
    <path d="M5.6 7.7 6.7 8.8 8.9 6.3" />
    <rect x="4" y="13" width="6.5" height="6.5" rx="1.4" />
    <path d="M13.5 7.7h6M13.5 16.2h6" />
  </svg>
);

const TrashIcon = () => (
  <svg
    className="sidebar-toolbar-icon"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M5 7h14" />
    <path d="M9 7V5.5A1.5 1.5 0 0 1 10.5 4h3A1.5 1.5 0 0 1 15 5.5V7" />
    <path d="M6.5 7 7.3 19a1.5 1.5 0 0 0 1.5 1.4h6.4a1.5 1.5 0 0 0 1.5-1.4L17.5 7" />
    <path d="M10 11v6M14 11v6" />
  </svg>
);

const CloseIcon = () => (
  <svg
    className="sidebar-toolbar-icon"
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

const FolderIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M4 6.5A1.5 1.5 0 0 1 5.5 5h4l2 2.3h7A1.5 1.5 0 0 1 20 8.8v9.7A1.5 1.5 0 0 1 18.5 20h-13A1.5 1.5 0 0 1 4 18.5Z" />
  </svg>
);

const PinIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M9 4.5h6l-.7 5.3L17 12.5v2H7v-2l2.7-2.7Z" />
    <path d="M12 14.5V20" />
  </svg>
);

const BackChevronIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M15 6l-6 6 6 6" />
  </svg>
);

const ForwardChevronIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M9 6l6 6-6 6" />
  </svg>
);

const CheckIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M4.5 12.5 9 17l10.5-10.5" />
  </svg>
);

/** Power glyph for the engine pill's Load button (asleep → running). */
const PowerIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M12 4v7" />
    <path d="M7.5 6.8a7 7 0 1 0 9 0" />
  </svg>
);

/** Eject glyph for the engine pill's Unload button (running → stopped). */
const EjectIcon = () => (
  <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
    <path d="M12 5.2 19 15H5l7-9.8Z" />
    <rect x="5" y="17" width="14" height="2.4" rx="1.2" />
  </svg>
);

/** Buckets `conversations` into the decreasing-scale groups the sidebar
 * shows instead of an exact per-row date (Today / This week / This month /
 * Older) — see `recencyBucket`. Preserves each conversation's relative
 * order within its bucket; empty buckets are omitted. */
function groupByRecency(
  conversations: ConversationMeta[],
): [RecencyBucket, ConversationMeta[]][] {
  const buckets: Record<RecencyBucket, ConversationMeta[]> = {
    today: [],
    thisWeek: [],
    thisMonth: [],
    older: [],
  };
  for (const conv of conversations) buckets[recencyBucket(conv.updatedAtMs)].push(conv);
  return (["today", "thisWeek", "thisMonth", "older"] as const)
    .map((bucket): [RecencyBucket, ConversationMeta[]] => [bucket, buckets[bucket]])
    .filter(([, items]) => items.length > 0);
}

export default function Sidebar() {
  const { t } = useTranslation();
  const conversations = useStore((s) => s.conversations);
  const projects = useStore((s) => s.projects);
  const createProject = useStore((s) => s.createProject);
  const renameProject = useStore((s) => s.renameProject);
  const deleteProject = useStore((s) => s.deleteProject);
  const deleteProjectWithConversations = useStore((s) => s.deleteProjectWithConversations);
  const moveConversationToProject = useStore((s) => s.moveConversationToProject);
  const renameConversation = useStore((s) => s.renameConversation);
  const setConversationPinned = useStore((s) => s.setConversationPinned);
  const active = useStore((s) => s.active);
  const view = useStore((s) => s.view);
  const streaming = useStore((s) => s.streaming);
  const newChat = useStore((s) => s.newChat);
  const openConversation = useStore((s) => s.openConversation);
  const deleteConversation = useStore((s) => s.deleteConversation);
  const setView = useStore((s) => s.setView);
  const setMoreDialogPage = useStore((s) => s.setMoreDialogPage);
  const setConversationSearchOpen = useStore((s) => s.setConversationSearchOpen);
  const mobileSidebarOpen = useStore((s) => s.mobileSidebarOpen);
  const setMobileSidebarOpen = useStore((s) => s.setMobileSidebarOpen);
  // Portable builds don't ship the Python plugins, so hide the Plugins tab.
  // Same treatment on Android: none of the 4 built-in plugins do anything
  // useful there (see state.rs's resolve_plugin_dirs) and there's no way to
  // drop a custom one into a sandboxed app's data dir. Also hidden while
  // there's no add-on to show: experimental ones aren't listed until
  // Settings → Advanced → Experimental features is on.
  const pluginsAvailable = useStore((s) => moreDialogPageAvailable("plugins", s));
  // Browse only appears once the hf-browser plugin is enabled (see
  // modelBrowserAvailable) — invisible by default, an opt-in power-user surface.
  const browseAvailable = useStore((s) => moreDialogPageAvailable("discover", s));
  // Writing Tools only appears once a plugin advertising that capability is
  // enabled (see writingToolsAvailable) — same opt-in shape as Browse. Gates
  // Translate/Correct below and the standalone "Improve a message" entry.
  const writingToolsOn = useStore((s) => writingToolsAvailable(s.plugins));
  const account = useStore((s) => s.account);

  // Drag-to-resize on .sidebar-resize-handle below. `resizing` mirrors
  // whether the window-level listeners are attached (see the effect below);
  // resizeStartRef holds the pointer position and width the current drag
  // began from, so each mousemove computes an absolute width instead of
  // drifting from accumulated deltas.
  const sidebarWidth = useStore((s) => s.sidebarWidth);
  const setSidebarWidth = useStore((s) => s.setSidebarWidth);
  const [resizing, setResizing] = useState(false);
  const resizeStartRef = useRef<{ x: number; width: number } | null>(null);

  useEffect(() => {
    if (!resizing) return;
    const onMove = (e: MouseEvent) => {
      const start = resizeStartRef.current;
      if (!start) return;
      setSidebarWidth(start.width + (e.clientX - start.x));
    };
    const onUp = () => setResizing(false);
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, [resizing, setSidebarWidth]);

  const startResize = (e: ReactMouseEvent) => {
    // Not text selection — this is a drag gesture.
    e.preventDefault();
    resizeStartRef.current = { x: e.clientX, width: sidebarWidth };
    setResizing(true);
  };

  // Arrow keys move the handle by increments a mouse-only affordance
  // wouldn't otherwise offer keyboard access to — role="separator" implies
  // this. Home/End jump to the bounds, same convention as a native <input
  // type="range">.
  const RESIZE_STEP = 16;
  const onResizeKeyDown = (e: ReactKeyboardEvent) => {
    if (e.key === "ArrowLeft") setSidebarWidth(sidebarWidth - RESIZE_STEP);
    else if (e.key === "ArrowRight") setSidebarWidth(sidebarWidth + RESIZE_STEP);
    else if (e.key === "Home") setSidebarWidth(SIDEBAR_MIN_WIDTH);
    else if (e.key === "End") setSidebarWidth(SIDEBAR_MAX_WIDTH);
    else return;
    e.preventDefault();
  };

  // The bundle version (from tauri.conf.json) — fetched once, async.
  const [version, setVersion] = useState("");
  useEffect(() => {
    void getVersion().then(setVersion);
  }, []);

  const [creatingProject, setCreatingProject] = useState(false);
  // Top-level Pinned/Projects section collapse — same accordion pattern as
  // each ConvList's own date-bucket headings (Today/This week/…). Expanded
  // by default.
  const [pinnedCollapsed, setPinnedCollapsed] = useState(false);
  const [projectsCollapsed, setProjectsCollapsed] = useState(false);

  // Models/Browse/Plugins/Sign in/Settings live behind a single "More" menu
  // to keep the sidebar footer to one compact row. Closes on outside click
  // or Escape, same as the composer's GenerationPanel. Open state lives in
  // the store rather than here because AutoUnloadTooltip hides while this is
  // up — the menu opens straight across the model row that popover sits over
  // (see `sidebarMoreMenuOpen`).
  const menuOpen = useStore((s) => s.sidebarMoreMenuOpen);
  const setMenuOpen = useStore((s) => s.setSidebarMoreMenuOpen);
  // Shared state outlives this component, and the sidebar unmounts whenever
  // it's collapsed — leave a stale `true` behind and the tooltip would stay
  // hidden for good.
  useEffect(() => () => setMenuOpen(false), [setMenuOpen]);
  const triggerRef = useRef<HTMLButtonElement>(null);
  // However the menu closes (selection, Escape, outside click), focus
  // should land back on its trigger rather than silently reverting to
  // <body> — used in place of a bare `setMenuOpen(false)` everywhere below.
  const closeMenu = () => {
    setMenuOpen(false);
    triggerRef.current?.focus();
  };
  const setSidebarSearchQuery = useStore((s) => s.setSidebarSearchQuery);

  // Every menu item's onClick calls this instead of closeMenu() directly —
  // picking something from the menu is a navigation event, so it should also
  // dismiss the mobile drawer (which must already be open to have reached
  // this menu) and leave selection mode / an in-progress search behind, same
  // as openViewAndClose below (defined later in this component, but a plain
  // closure reference here is fine — this function only ever runs from a
  // later click, by which point every const below has already been
  // assigned). Dismissing the popover via outside-click/Escape (closeMenu()
  // itself) is a "never mind" gesture, not a navigation, so it leaves the
  // drawer/selection/search exactly as the user had them.
  const selectMenuItem = () => {
    closeMenu();
    setMobileSidebarOpen(false);
    exitSelectionMode();
    setSidebarSearchQuery("");
  };
  // Every entry opens the More dialog on its page, over whatever the main
  // pane shows — the menu closes first so focus is back on its trigger by
  // the time the dialog mounts, which is where the dialog returns it on close.
  const openMoreDialog = (page: MoreDialogPage) => {
    selectMenuItem();
    setMoreDialogPage(page);
  };
  const menuRef = useCloseOnOutside<HTMLDivElement>(menuOpen, closeMenu);

  // Multi-select "manage conversations" mode, toggled from the settings icon
  // atop the list (see `.sidebar-list-toolbar`) — lets a user delete several
  // chats in one pass instead of one hover-revealed row at a time.
  const [selectionMode, setSelectionMode] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const exitSelectionMode = () => {
    setSelectionMode(false);
    setSelectedIds(new Set());
  };
  const toggleSelected = (id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };
  // "Select all" toggles between everything and nothing in the ungrouped
  // list — the only conversations bulk-select actually applies to. Pinned
  // conversations and conversations inside a project each get their own
  // dedicated section/group, and neither renders a selection checkbox (see
  // the hardcoded `selectionMode={false}` on their own ConvList calls
  // below), so they're excluded here too rather than silently selectable
  // only through "select all".
  const selectableConversations = conversations.filter((c) => !c.pinned && !c.projectId);
  const allSelected =
    selectableConversations.length > 0 &&
    selectableConversations.every((c) => selectedIds.has(c.id));
  const toggleSelectAll = () =>
    setSelectedIds(
      allSelected ? new Set() : new Set(selectableConversations.map((c) => c.id)),
    );

  // A pending delete — one row's "…" menu, or the selection bar's bulk
  // delete — always lands here first and goes through ConfirmDialog below,
  // never a silent click. `title` is only set for a single-row delete (the
  // bulk case shows a count instead). `bulk` drives whether confirming also
  // exits selection mode — deliberately not inferred from `ids.length`,
  // since bulk-deleting exactly one checked row must still exit selection
  // mode the same as deleting several.
  const [pendingDelete, setPendingDelete] = useState<{
    ids: string[];
    title?: string;
    bulk?: boolean;
  } | null>(null);
  const [deleting, setDeleting] = useState(false);
  const requestDelete = (id: string) =>
    setPendingDelete({ ids: [id], title: conversations.find((c) => c.id === id)?.title });
  const requestBulkDelete = () => setPendingDelete({ ids: Array.from(selectedIds), bulk: true });
  const confirmDelete = async () => {
    if (!pendingDelete) return;
    setDeleting(true);
    await Promise.all(pendingDelete.ids.map((id) => deleteConversation(id)));
    setDeleting(false);
    setPendingDelete(null);
    if (pendingDelete.bulk) exitSelectionMode();
  };

  // A pending project delete — same "always confirm first" shape as
  // pendingDelete above, but deleting a project has a second question a
  // conversation delete doesn't: what happens to the conversations still in
  // it. Plain `deleteProject` only ever detaches them (see state/store.ts);
  // deleting them too goes through `deleteProjectWithConversations` instead,
  // which deletes them first and only then the now-empty project.
  const [pendingDeleteProject, setPendingDeleteProject] = useState<{
    id: string;
    name: string;
    conversationCount: number;
  } | null>(null);
  const [deletingProject, setDeletingProject] = useState(false);
  const requestDeleteProject = (project: Project) =>
    setPendingDeleteProject({
      id: project.id,
      name: project.name,
      conversationCount: conversations.filter((c) => c.projectId === project.id).length,
    });
  const confirmDeleteProject = async (alsoDeleteConversations: boolean) => {
    if (!pendingDeleteProject) return;
    setDeletingProject(true);
    if (alsoDeleteConversations) {
      await deleteProjectWithConversations(pendingDeleteProject.id);
    } else {
      await deleteProject(pendingDeleteProject.id);
    }
    setDeletingProject(false);
    setPendingDeleteProject(null);
  };

  // Below the mobile breakpoint the sidebar is a drawer, not a permanent
  // column — every action that navigates away from "just browsing the
  // sidebar" should close it. A no-op above the breakpoint, where nothing
  // reads `mobileSidebarOpen` (see chat.css's `.sidebar-open` rule).
  const openConversationAndClose = (id: string) => {
    setMobileSidebarOpen(false);
    void openConversation(id);
  };

  // Same "navigate + close the mobile drawer" shape as openConversationAndClose,
  // plus leaving selection mode and clearing any stale search — jumping to
  // another view (or opening Search) mid multi-select would otherwise strand
  // the selection bar behind it, and Search should always open empty.
  const openViewAndClose = (v: View) => {
    setMobileSidebarOpen(false);
    exitSelectionMode();
    setSidebarSearchQuery("");
    setView(v);
  };
  const openSearchAndClose = () => {
    setMobileSidebarOpen(false);
    exitSelectionMode();
    setSidebarSearchQuery("");
    setConversationSearchOpen(true);
  };

  // Pinned conversations get their own section at the top of the list,
  // regardless of which project (if any) they belong to — excluded from
  // both the Projects groups and the ungrouped list below so each
  // conversation renders in exactly one place.
  const pinned = conversations.filter((c) => c.pinned);
  const ungrouped = conversations.filter((c) => !c.projectId && !c.pinned);

  return (
    <nav className={`sidebar${mobileSidebarOpen ? " sidebar-open" : ""}`} aria-labelledby="sidebar-brand">
      <div className="sidebar-top" data-tauri-drag-region>
        <span className="sidebar-brand" id="sidebar-brand">OpenPal</span>
      </div>

      {/* While a reply is streaming, switching chats would lose the turn —
          rest these quietly until it finishes. */}
      <button
        className="new-chat"
        onClick={() => {
          setMobileSidebarOpen(false);
          exitSelectionMode();
          newChat();
        }}
        disabled={streaming}
      >
        <ChatIcon /> {t("sidebar.newChat")}
      </button>

      {/* Improve is part of the Writing Tools plugin, same gate as Translate/
          Correct below, but gets its own row, above them: a single
          full-width chip, not sharing their two-up row. */}
      {writingToolsOn && (
        <div className="sidebar-quick-actions">
          <button
            type="button"
            className={`sidebar-quick-action${view === "improve" ? " sidebar-quick-action-active" : ""}`}
            disabled={streaming}
            onClick={() => openViewAndClose("improve")}
          >
            <SparkleIcon /> {t("improveView.title")}
          </button>
        </div>
      )}

      {/* Translate/Correct only appear once a plugin advertising the
          `writing-tools` capability is enabled (see writingToolsAvailable) —
          same opt-in shape the single "Writing Tools" entry used to have
          under the More menu, just promoted to their own row now that
          there are two of them. */}
      {writingToolsOn && (
        <div className="sidebar-quick-actions">
          <button
            type="button"
            className={`sidebar-quick-action${view === "translate" ? " sidebar-quick-action-active" : ""}`}
            disabled={streaming}
            onClick={() => openViewAndClose("translate")}
          >
            <LanguagesIcon /> {t("writingTools.modeTranslate")}
          </button>
          <button
            type="button"
            className={`sidebar-quick-action${view === "correct" ? " sidebar-quick-action-active" : ""}`}
            disabled={streaming}
            onClick={() => openViewAndClose("correct")}
          >
            <CorrectIcon /> {t("writingTools.modeCorrect")}
          </button>
        </div>
      )}

      <nav className="conv-list" aria-label={t("sidebar.conversationsAriaLabel")}>
        {pinned.length > 0 && (
          <div className="project-section">
            <div className="project-section-header">
              <button
                type="button"
                className="project-section-toggle"
                onClick={() => setPinnedCollapsed((c) => !c)}
                aria-expanded={!pinnedCollapsed}
              >
                <ChevronIcon collapsed={pinnedCollapsed} />
                <span className="project-section-title">{t("sidebar.pinned")}</span>
              </button>
            </div>
            {!pinnedCollapsed && (
              // Pinned conversations sit outside bulk-select entirely — no
              // checkbox, no "select all" membership (see
              // selectableConversations above). selectionMode is hardcoded
              // false here (not the outer toggle) so these rows keep
              // opening/menuing normally even while the rest of the list is
              // mid-selection.
              <ConvList
                conversations={pinned}
                active={active}
                view={view}
                streaming={streaming}
                projects={projects}
                selectionMode={false}
                selectedIds={selectedIds}
                onOpen={openConversationAndClose}
                onToggleSelect={toggleSelected}
                onDelete={requestDelete}
                onMoveToProject={(id, projectId) => void moveConversationToProject(id, projectId)}
                onRename={(id, title) => void renameConversation(id, title)}
                onTogglePin={(id, next) => void setConversationPinned(id, next)}
                onCreateProject={createProject}
                groupByDate={false}
              />
            )}
          </div>
        )}

        <div className="project-section">
          <div className="project-section-header">
            <button
              type="button"
              className="project-section-toggle"
              onClick={() => setProjectsCollapsed((c) => !c)}
              aria-expanded={!projectsCollapsed}
            >
              <ChevronIcon collapsed={projectsCollapsed} />
              <span className="project-section-title">{t("sidebar.projects")}</span>
            </button>
            <button
              type="button"
              className="btn-quiet project-add-btn"
              disabled={streaming}
              onClick={() => {
                setCreatingProject(true);
                setProjectsCollapsed(false);
              }}
              aria-label={t("sidebar.newProject")}
            >
              <PlusIcon />
            </button>
          </div>
          {!projectsCollapsed && creatingProject && (
            <NewProjectRow
              onCreate={(name) => {
                setCreatingProject(false);
                void createProject(name);
              }}
              onCancel={() => setCreatingProject(false)}
            />
          )}
          {!projectsCollapsed && projects.map((project) => (
            <ProjectGroup
              key={project.id}
              project={project}
              conversations={conversations.filter(
                (c) => c.projectId === project.id && !c.pinned,
              )}
              active={active}
              view={view}
              streaming={streaming}
              projects={projects}
              selectedIds={selectedIds}
              onOpen={openConversationAndClose}
              onToggleSelect={toggleSelected}
              onDelete={requestDelete}
              onMoveToProject={(id, projectId) =>
                void moveConversationToProject(id, projectId)
              }
              onRenameConversation={(id, title) => void renameConversation(id, title)}
              onTogglePin={(id, next) => void setConversationPinned(id, next)}
              onCreateProject={createProject}
              onRename={(name) => void renameProject(project.id, name)}
              onDeleteProject={() => requestDeleteProject(project)}
            />
          ))}
        </div>

        {/* Settings (multi-select) + search — search used to be an inline
            box up in the header; it now opens ConversationSearch as its own
            main-panel view, with far more room to read snippets. In
            selection mode this row swaps to a count + delete/cancel bar
            instead. Lives below the Projects list (rather than above, in the
            sidebar's fixed header) so it scrolls together with everything
            it acts on. */}
        <div className="sidebar-list-toolbar">
          {selectionMode ? (
            <>
              <label className="sidebar-select-all-label">
                <input
                  type="checkbox"
                  className="sidebar-select-all-checkbox"
                  checked={allSelected}
                  disabled={streaming}
                  aria-label={t(allSelected ? "sidebar.deselectAll" : "sidebar.selectAll")}
                  onChange={toggleSelectAll}
                />
                {/* Below count===0, the string is `sidebar.selectConversations`
                    rather than a plural form of `sidebar.selectedCount` — same
                    "never invoke a pluralized key at count=0" rule this file's
                    delete-confirm strings already follow (see
                    `requestBulkDelete`'s button, disabled at 0), needed here
                    because French's plural rules group 0 into "one" ({{count}}
                    sélectionnée), not "other" ({{count}} sélectionnées). */}
                <span className="sidebar-list-toolbar-count" role="status">
                  {selectedIds.size > 0
                    ? t("sidebar.selectedCount", { count: selectedIds.size })
                    : t("sidebar.selectConversations")}
                </span>
              </label>
              <div className="sidebar-list-toolbar-actions">
                <button
                  type="button"
                  className="btn-quiet sidebar-toolbar-btn"
                  disabled={selectedIds.size === 0 || streaming}
                  aria-label={t("sidebar.deleteSelected")}
                  onClick={requestBulkDelete}
                >
                  <TrashIcon />
                </button>
                <button
                  type="button"
                  className="btn-quiet sidebar-toolbar-btn"
                  aria-label={t("common.cancel")}
                  onClick={exitSelectionMode}
                >
                  <CloseIcon />
                </button>
              </div>
            </>
          ) : (
            <>
              <button
                type="button"
                className="btn-quiet sidebar-toolbar-btn"
                disabled={streaming || conversations.length === 0}
                aria-label={t("sidebar.selectConversations")}
                title={t("sidebar.selectConversations")}
                onClick={() => setSelectionMode(true)}
              >
                <ChecklistIcon />
              </button>
              <button
                type="button"
                className="btn-quiet sidebar-toolbar-btn"
                aria-label={t("sidebar.search")}
                title={t("sidebar.search")}
                onClick={openSearchAndClose}
              >
                <SearchIcon />
              </button>
            </>
          )}
        </div>

        {conversations.length === 0 && (
          <p className="conv-empty">{t("sidebar.emptyState")}</p>
        )}
        <ConvList
          conversations={ungrouped}
          active={active}
          view={view}
          streaming={streaming}
          projects={projects}
          selectionMode={selectionMode}
          selectedIds={selectedIds}
          onOpen={openConversationAndClose}
          onToggleSelect={toggleSelected}
          onDelete={requestDelete}
          onMoveToProject={(id, projectId) => void moveConversationToProject(id, projectId)}
          onRename={(id, title) => void renameConversation(id, title)}
          onTogglePin={(id, next) => void setConversationPinned(id, next)}
          onCreateProject={createProject}
        />
      </nav>

      <div className="sidebar-bottom">
        <EnginePill />
        <div className="sidebar-menu-row">
          <div className="menu-wrap" ref={menuRef}>
            <button
              ref={triggerRef}
              type="button"
              className={`btn-quiet sidebar-menu-trigger${menuOpen ? " sidebar-action-active" : ""}`}
              onClick={() => setMenuOpen(!menuOpen)}
              aria-expanded={menuOpen}
            >
              <MoreIcon /> {t("sidebar.more")}
            </button>
            {menuOpen && (
              <div className="menu">
                <button type="button" className="menu-item" onClick={() => openMoreDialog("library")}>
                  {t("sidebar.models")}
                </button>
                {browseAvailable && (
                  <button type="button" className="menu-item" onClick={() => openMoreDialog("discover")}>
                    <CompassIcon /> {t("sidebar.browse")}
                  </button>
                )}
                {pluginsAvailable && (
                  <button type="button" className="menu-item" onClick={() => openMoreDialog("plugins")}>
                    <PuzzleIcon /> {t("sidebar.plugins")}
                  </button>
                )}
                <button type="button" className="menu-item" onClick={() => openMoreDialog("skills")}>
                  <SkillIcon /> {t("sidebar.skills")}
                </button>
                <div className="menu-divider" />
                {/* Sign in isn't plugged into a working backend yet, so the
                    entry point to start one is hidden for now — an existing,
                    already-signed-in session (account truthy) still shows
                    normally. */}
                {account && (
                  <button
                    type="button"
                    className="menu-item"
                    onClick={() => openMoreDialog("general")}
                    title={t("sidebar.signedInAs", { email: account.email })}
                  >
                    {t("sidebar.account")}
                  </button>
                )}
                <button type="button" className="menu-item" onClick={() => openMoreDialog("general")}>
                  {t("sidebar.settings")}
                </button>
              </div>
            )}
          </div>
          {version && <span className="app-version">{t("sidebar.appVersion", { version })}</span>}
        </div>
      </div>

      {pendingDelete && (
        <ConfirmDialog
          title={t("sidebar.deleteConfirm.title")}
          message={
            pendingDelete.ids.length === 1 && pendingDelete.title
              ? t("sidebar.deleteConfirm.message", { title: pendingDelete.title })
              : t("sidebar.deleteConfirm.messageMany", { count: pendingDelete.ids.length })
          }
          confirmLabel={t("sidebar.deleteConfirm.confirmButton")}
          busy={deleting}
          onConfirm={() => void confirmDelete()}
          onCancel={() => setPendingDelete(null)}
        />
      )}

      {pendingDeleteProject && (
        <DeleteProjectDialog
          projectName={pendingDeleteProject.name}
          conversationCount={pendingDeleteProject.conversationCount}
          busy={deletingProject}
          onDetach={() => void confirmDeleteProject(false)}
          onDeleteConversations={() => void confirmDeleteProject(true)}
          onCancel={() => setPendingDeleteProject(null)}
        />
      )}

      <div
        className={`sidebar-resize-handle${resizing ? " sidebar-resize-handle-active" : ""}`}
        role="separator"
        aria-orientation="vertical"
        aria-label={t("sidebar.resizeHandle")}
        aria-valuenow={Math.round(sidebarWidth)}
        aria-valuemin={SIDEBAR_MIN_WIDTH}
        aria-valuemax={SIDEBAR_MAX_WIDTH}
        tabIndex={0}
        onMouseDown={startResize}
        onKeyDown={onResizeKeyDown}
      />
    </nav>
  );
}

/** Renders `conversations` grouped into decreasing-scale date sections
 * (Today / This week / This month / Older) — shared by both the top-level
 * "Other chats" list and each `ProjectGroup`, so every conversation list in
 * the sidebar groups the same way. `groupByDate={false}` (the Pinned
 * section) skips the date buckets entirely and renders a flat list instead —
 * pinned status, not recency, is already what's grouping these rows. */
function ConvList({
  conversations,
  active,
  view,
  streaming,
  projects,
  selectionMode,
  selectedIds,
  onOpen,
  onToggleSelect,
  onDelete,
  onMoveToProject,
  onRename,
  onTogglePin,
  onCreateProject,
  groupByDate = true,
}: {
  conversations: ConversationMeta[];
  active: Conversation | null;
  view: View;
  streaming: boolean;
  projects: Project[];
  selectionMode: boolean;
  selectedIds: Set<string>;
  onOpen: (id: string) => void;
  onToggleSelect: (id: string) => void;
  onDelete: (id: string) => void;
  onMoveToProject: (id: string, projectId: string | null) => void;
  onRename: (id: string, title: string) => void;
  onTogglePin: (id: string, pinned: boolean) => void;
  onCreateProject: (name: string) => Promise<Project | null>;
  groupByDate?: boolean;
}) {
  // Which date buckets are collapsed — expanded by default, and local to
  // this ConvList instance, so the ungrouped list and each ProjectGroup's
  // own nested ConvList collapse independently (same as ProjectGroup's own
  // `collapsed` state does for the project itself).
  const [collapsedBuckets, setCollapsedBuckets] = useState<Set<RecencyBucket>>(new Set());
  const toggleBucket = (bucket: RecencyBucket) =>
    setCollapsedBuckets((prev) => {
      const next = new Set(prev);
      if (next.has(bucket)) next.delete(bucket);
      else next.add(bucket);
      return next;
    });

  const renderRow = (conv: ConversationMeta) => (
    <ConvRow
      key={conv.id}
      conv={conv}
      isActive={view === "chat" && active?.id === conv.id}
      streaming={streaming}
      projects={projects}
      selectionMode={selectionMode}
      selected={selectedIds.has(conv.id)}
      onOpen={() => onOpen(conv.id)}
      onToggleSelect={() => onToggleSelect(conv.id)}
      onDelete={() => onDelete(conv.id)}
      onMoveToProject={(projectId) => onMoveToProject(conv.id, projectId)}
      onRename={(title) => onRename(conv.id, title)}
      onTogglePin={(pinned) => onTogglePin(conv.id, pinned)}
      onCreateProject={onCreateProject}
    />
  );

  if (!groupByDate) {
    return <>{conversations.map(renderRow)}</>;
  }

  const groups = groupByRecency(conversations);
  return (
    <>
      {groups.map(([bucket, items]) => {
        const collapsed = collapsedBuckets.has(bucket);
        return (
          <div key={bucket} className="conv-date-group">
            <button
              type="button"
              className="conv-date-heading"
              onClick={() => toggleBucket(bucket)}
              aria-expanded={!collapsed}
            >
              <ChevronIcon collapsed={collapsed} />
              {recencyBucketLabel(bucket)}
            </button>
            {!collapsed && items.map(renderRow)}
          </div>
        );
      })}
    </>
  );
}

/** One conversation row — the ordinary list item both the ungrouped list and
 * each `ProjectGroup` use (via `ConvList`). `projects` (the full list, not
 * just this conversation's own) and `onCreateProject` are only needed for
 * the "Add to project" submenu in `ConvRowMenu`, which always shows —
 * including its own "New project" option — so a user with zero projects can
 * still create their first one straight from here. In selection mode the
 * row's own open/menu affordance is replaced by a checkbox — the whole row
 * toggles selection. */
function ConvRow({
  conv,
  isActive,
  streaming,
  projects,
  selectionMode,
  selected,
  onOpen,
  onToggleSelect,
  onDelete,
  onMoveToProject,
  onRename,
  onTogglePin,
  onCreateProject,
}: {
  conv: ConversationMeta;
  isActive: boolean;
  streaming: boolean;
  projects: Project[];
  selectionMode: boolean;
  selected: boolean;
  onOpen: () => void;
  onToggleSelect: () => void;
  onDelete: () => void;
  onMoveToProject: (projectId: string | null) => void;
  onRename: (title: string) => void;
  onTogglePin: (pinned: boolean) => void;
  onCreateProject: (name: string) => Promise<Project | null>;
}) {
  const { t } = useTranslation();
  // Inline rename, triggered from ConvRowMenu's "Rename" item — same
  // one-shot-submit shape as ProjectGroup's own rename input (Enter blurs to
  // submit, Escape cancels, a ref guards against a double-submit from both
  // paths landing on the same blur).
  const [renaming, setRenaming] = useState(false);
  const [name, setName] = useState(conv.title);
  const settledRef = useRef(false);
  // Mirrors ConvRowMenu's own open state so the row keeps its hover-style
  // highlight while the "…" menu is open, even after the pointer leaves
  // (the menu panel is portal-rendered to document.body, so it falls
  // outside `.conv-row`'s DOM subtree and can't be reached by :hover or
  // :focus-within on the row itself).
  const [menuOpen, setMenuOpen] = useState(false);
  // The rename `<input>` (autoFocus'd) unmounts once renaming settles,
  // swapping back in this plain open button — without this, focus would
  // silently fall back to <body> instead of staying in the tab sequence.
  const openButtonRef = useRef<HTMLButtonElement>(null);
  const wasRenamingRef = useRef(false);
  useEffect(() => {
    if (wasRenamingRef.current && !renaming) openButtonRef.current?.focus();
    wasRenamingRef.current = renaming;
  }, [renaming]);

  const startRename = () => {
    setName(conv.title);
    settledRef.current = false;
    setRenaming(true);
  };

  const submitRename = () => {
    if (settledRef.current) return;
    settledRef.current = true;
    const trimmed = name.trim();
    if (trimmed && trimmed !== conv.title) onRename(trimmed);
    setRenaming(false);
  };

  const cancelRename = () => {
    settledRef.current = true;
    setRenaming(false);
  };

  return (
    <div
      className={`conv-row${isActive ? " conv-row-active" : ""}${
        selectionMode ? " conv-row-selectable" : ""
      }${menuOpen ? " conv-row-menu-open" : ""}`}
    >
      {selectionMode && (
        <input
          type="checkbox"
          className="conv-row-checkbox"
          checked={selected}
          disabled={streaming}
          aria-label={t("sidebar.selectConversation", { title: conv.title })}
          onChange={onToggleSelect}
        />
      )}
      {renaming ? (
        <input
          type="text"
          autoFocus
          value={name}
          className="conv-rename-input"
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.currentTarget.blur();
            if (e.key === "Escape") cancelRename();
          }}
          onBlur={submitRename}
        />
      ) : (
        <button
          ref={openButtonRef}
          className="conv-open"
          disabled={streaming}
          aria-current={isActive ? "page" : undefined}
          onClick={selectionMode ? onToggleSelect : onOpen}
          title={conv.title}
        >
          <span className="conv-title">{conv.title}</span>
        </button>
      )}
      {!selectionMode && !renaming && (
        <div className="conv-row-actions">
          <ConvRowMenu
            conv={conv}
            projects={projects}
            streaming={streaming}
            onDelete={onDelete}
            onMoveToProject={onMoveToProject}
            onRename={startRename}
            onTogglePin={() => onTogglePin(!conv.pinned)}
            onCreateProject={onCreateProject}
            onOpenChange={setMenuOpen}
          />
        </div>
      )}
    </div>
  );
}

/** The "…" popup on a conversation row: Delete / Add to project / Pin /
 * Rename. Rendered through a portal at a fixed position computed from the
 * trigger's own rect, same reasoning as `components/PinPad.tsx`'s portal —
 * an ordinary `.menu-wrap`/`.menu` pair positioned `absolute` inside
 * `.conv-list` would get clipped the moment the row sits near either edge of
 * that scrolled `overflow-y: auto` container (see `ProjectGroup`'s own note
 * about exactly this for its header actions). Closes on Escape, an outside
 * click, or the list scrolling out from under it — repositioning to track a
 * scroll wasn't worth the complexity for a menu this short-lived. */
// ConvRowMenu's root page (Add to project / Pin / Rename / divider / Delete)
// runs about 190px tall; the project submenu can run a little taller with
// several projects. Comfortably covers both without measuring the actual
// rendered panel before it's shown.
const MENU_HEIGHT_ESTIMATE = 260;

function ConvRowMenu({
  conv,
  projects,
  streaming,
  onDelete,
  onMoveToProject,
  onRename,
  onTogglePin,
  onCreateProject,
  onOpenChange,
}: {
  conv: ConversationMeta;
  projects: Project[];
  streaming: boolean;
  onDelete: () => void;
  onMoveToProject: (projectId: string | null) => void;
  onRename: () => void;
  onTogglePin: () => void;
  onCreateProject: (name: string) => Promise<Project | null>;
  onOpenChange?: (open: boolean) => void;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [page, setPage] = useState<"root" | "project">("root");
  const [pos, setPos] = useState<{ right: number; offset: number; openUpward: boolean } | null>(
    null,
  );
  const triggerRef = useRef<HTMLButtonElement>(null);
  // Inline "create a project and drop this conversation straight into it"
  // row at the top of the project subpage — same one-shot-submit shape as
  // NewProjectRow's own input (Enter blurs to submit, Escape cancels, a ref
  // guards a double-submit from both paths landing on the same blur).
  const [creatingProject, setCreatingProject] = useState(false);
  const [newProjectName, setNewProjectName] = useState("");
  const newProjectSettledRef = useRef(false);

  // However the menu closes (an item picked, Escape, outside click, or the
  // list scrolling it away), focus should land back on its trigger rather
  // than silently reverting to <body> — the panel is portal-rendered outside
  // the trigger's DOM subtree, so closing it removes whatever item held
  // focus from the document entirely. Same reasoning as this file's own
  // bottom "More" menu's `closeMenu`.
  const close = () => {
    setOpen(false);
    setPage("root");
    setCreatingProject(false);
    setNewProjectName("");
    triggerRef.current?.focus();
  };

  const startCreatingProject = () => {
    setNewProjectName("");
    newProjectSettledRef.current = false;
    setCreatingProject(true);
  };

  const submitNewProject = async () => {
    if (newProjectSettledRef.current) return;
    newProjectSettledRef.current = true;
    const trimmed = newProjectName.trim();
    setCreatingProject(false);
    if (!trimmed) return;
    const project = await onCreateProject(trimmed);
    if (project) {
      close();
      onMoveToProject(project.id);
    }
  };

  const cancelNewProject = () => {
    newProjectSettledRef.current = true;
    setCreatingProject(false);
  };

  // The panel is rendered through a portal (see below), so the trigger sits
  // outside its DOM subtree and must be passed as an exclusion — otherwise
  // the trigger's own click would first close the menu via the outside-click
  // check, then reopen it via its onClick. Handles Escape too.
  const panelRef = useCloseOnOutside<HTMLDivElement>(open, close, [triggerRef]);

  useEffect(() => {
    onOpenChange?.(open);
  }, [open, onOpenChange]);

  useEffect(() => {
    if (!open) return;
    // Non-bubbling `scroll` events still reach a capture-phase window
    // listener on their way down to the actual scrolled element (`.conv-list`
    // here) — this is what lets a single listener catch it despite scroll
    // never bubbling. A fixed-position popup has no sane position to track
    // once its anchor has scrolled, so it just closes instead.
    const onScroll = () => close();
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onScroll);
    return () => {
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onScroll);
    };
  }, [open]);

  const toggle = () => {
    if (open) {
      close();
      return;
    }
    const rect = triggerRef.current?.getBoundingClientRect();
    if (!rect) return;
    // Flip to opening upward once there's under MENU_HEIGHT_ESTIMATE of
    // viewport left below the trigger.
    const openUpward = window.innerHeight - rect.bottom < MENU_HEIGHT_ESTIMATE;
    setPos({
      right: window.innerWidth - rect.right,
      offset: openUpward ? window.innerHeight - rect.top + 6 : rect.bottom + 6,
      openUpward,
    });
    setPage("root");
    setOpen(true);
  };

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        className={`btn-quiet conv-row-menu-trigger${open ? " sidebar-action-active" : ""}`}
        disabled={streaming}
        aria-haspopup="true"
        aria-expanded={open}
        aria-label={t("sidebar.rowMenu.ariaLabel", { title: conv.title })}
        onClick={(e) => {
          e.stopPropagation();
          toggle();
        }}
      >
        <MoreIcon />
      </button>
      {open &&
        pos &&
        createPortal(
          <div
            ref={panelRef}
            className="menu conv-row-menu"
            style={{
              position: "fixed",
              left: "auto",
              right: pos.right,
              top: pos.openUpward ? "auto" : pos.offset,
              bottom: pos.openUpward ? pos.offset : "auto",
            }}
            onClick={(e) => e.stopPropagation()}
          >
            {page === "root" ? (
              <>
                <button
                  type="button"
                  className="menu-item"
                  onClick={() => setPage("project")}
                >
                  <FolderIcon />
                  <span className="menu-item-label">{t("sidebar.rowMenu.addToProject")}</span>
                  <ForwardChevronIcon />
                </button>
                <button
                  type="button"
                  className="menu-item"
                  onClick={() => {
                    close();
                    onTogglePin();
                  }}
                >
                  <PinIcon />
                  {t(conv.pinned ? "sidebar.rowMenu.unpin" : "sidebar.rowMenu.pin")}
                </button>
                <button
                  type="button"
                  className="menu-item"
                  onClick={() => {
                    close();
                    onRename();
                  }}
                >
                  <EditIcon />
                  {t("sidebar.rowMenu.rename")}
                </button>
                <div className="menu-divider" />
                <button
                  type="button"
                  className="menu-item menu-item-danger"
                  onClick={() => {
                    close();
                    onDelete();
                  }}
                >
                  <TrashIcon />
                  {t("sidebar.rowMenu.delete")}
                </button>
              </>
            ) : (
              <>
                <button
                  type="button"
                  className="menu-item menu-item-back"
                  onClick={() => {
                    setPage("root");
                    setCreatingProject(false);
                  }}
                >
                  <BackChevronIcon />
                  {t("sidebar.moveToProject")}
                </button>
                {creatingProject ? (
                  <div className="menu-item menu-new-project-row">
                    <input
                      type="text"
                      autoFocus
                      value={newProjectName}
                      placeholder={t("sidebar.newProjectPlaceholder")}
                      className="project-rename-input"
                      onClick={(e) => e.stopPropagation()}
                      onChange={(e) => setNewProjectName(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") e.currentTarget.blur();
                        if (e.key === "Escape") cancelNewProject();
                      }}
                      onBlur={() => void submitNewProject()}
                    />
                  </div>
                ) : (
                  <button type="button" className="menu-item" onClick={startCreatingProject}>
                    <PlusIcon />
                    {t("sidebar.newProject")}
                  </button>
                )}
                <button
                  type="button"
                  className="menu-item"
                  onClick={() => {
                    close();
                    onMoveToProject(null);
                  }}
                >
                  <span className="menu-item-check">{!conv.projectId && <CheckIcon />}</span>
                  {t("sidebar.noProject")}
                </button>
                {projects.map((p) => (
                  <button
                    key={p.id}
                    type="button"
                    className="menu-item"
                    onClick={() => {
                      close();
                      onMoveToProject(p.id);
                    }}
                  >
                    <span className="menu-item-check">
                      {conv.projectId === p.id && <CheckIcon />}
                    </span>
                    {p.name}
                  </button>
                ))}
              </>
            )}
          </div>,
          document.body,
        )}
    </>
  );
}

/** A project's header (collapsible, renamable, deletable) plus its member
 * conversations. Rename and delete are plain hover-revealed icon buttons
 * rather than a floating menu — `.conv-list` scrolls, and a popover
 * positioned near the top of a scrolled `overflow-y: auto` container would
 * get clipped by it. (Each member conversation's own row menu sidesteps this
 * with a portal instead — see `ConvRowMenu`.) */
function ProjectGroup({
  project,
  conversations,
  active,
  view,
  streaming,
  projects,
  selectedIds,
  onOpen,
  onToggleSelect,
  onDelete,
  onMoveToProject,
  onRenameConversation,
  onTogglePin,
  onCreateProject,
  onRename,
  onDeleteProject,
}: {
  project: Project;
  conversations: ConversationMeta[];
  active: Conversation | null;
  view: View;
  streaming: boolean;
  projects: Project[];
  selectedIds: Set<string>;
  onOpen: (id: string) => void;
  onToggleSelect: (id: string) => void;
  onDelete: (id: string) => void;
  onMoveToProject: (id: string, projectId: string | null) => void;
  onRenameConversation: (id: string, title: string) => void;
  onTogglePin: (id: string, pinned: boolean) => void;
  onCreateProject: (name: string) => Promise<Project | null>;
  onRename: (name: string) => void;
  onDeleteProject: () => void;
}) {
  const { t } = useTranslation();
  const [collapsed, setCollapsed] = useState(false);
  const [renaming, setRenaming] = useState(false);
  const [name, setName] = useState(project.name);
  // Enter blurs the input (see below), which then submits — but Escape also
  // wants to bail out via the same blur without submitting. This ref is the
  // single source of truth for "already handled", set synchronously before
  // either path's blur can fire, so a rename is never submitted twice (once
  // from Enter's blur, once from an unrelated later blur) and Escape's
  // cancel is never overridden by the blur that follows it.
  const settledRef = useRef(false);
  // Same reasoning as ConvRow's own openButtonRef: the rename `<input>`
  // unmounts once renaming settles, swapping back in this plain name
  // button, so focus needs to be restored explicitly or it falls to <body>.
  const nameButtonRef = useRef<HTMLButtonElement>(null);
  const wasRenamingRef = useRef(false);
  useEffect(() => {
    if (wasRenamingRef.current && !renaming) nameButtonRef.current?.focus();
    wasRenamingRef.current = renaming;
  }, [renaming]);

  const startRename = () => {
    setName(project.name);
    settledRef.current = false;
    setRenaming(true);
  };

  const submitRename = () => {
    if (settledRef.current) return;
    settledRef.current = true;
    const trimmed = name.trim();
    if (trimmed && trimmed !== project.name) onRename(trimmed);
    setRenaming(false);
  };

  const cancelRename = () => {
    settledRef.current = true;
    setRenaming(false);
  };

  return (
    <div className="project-group">
      <div className="project-group-header">
        <button
          type="button"
          className="project-collapse-btn"
          disabled={renaming}
          onClick={() => setCollapsed((c) => !c)}
          aria-expanded={!collapsed}
          aria-label={
            collapsed
              ? t("sidebar.expandProject", { name: project.name })
              : t("sidebar.collapseProject", { name: project.name })
          }
        >
          <ChevronIcon collapsed={collapsed} />
        </button>
        {renaming ? (
          <input
            type="text"
            autoFocus
            value={name}
            className="project-rename-input"
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") e.currentTarget.blur();
              if (e.key === "Escape") cancelRename();
            }}
            onBlur={submitRename}
          />
        ) : (
          <button
            ref={nameButtonRef}
            type="button"
            className="project-name-btn"
            title={project.name}
            onClick={() => setCollapsed((c) => !c)}
          >
            {project.name}
          </button>
        )}
        <span className="project-count">{conversations.length}</span>
        {!renaming && (
          <div className="project-group-actions">
            <button
              type="button"
              className="btn-quiet project-action-btn"
              disabled={streaming}
              onClick={startRename}
              aria-label={t("sidebar.renameProject", { name: project.name })}
            >
              <EditIcon />
            </button>
            <button
              type="button"
              className="btn-quiet project-action-btn"
              disabled={streaming}
              onClick={onDeleteProject}
              aria-label={t("sidebar.deleteProjectAriaLabel", { name: project.name })}
            >
              <TrashIcon />
            </button>
          </div>
        )}
      </div>
      {!collapsed &&
        (conversations.length === 0 ? (
          <p className="conv-empty project-empty">{t("sidebar.emptyProject")}</p>
        ) : (
          // Same "no bulk-select here" treatment as the Pinned section's own
          // ConvList (see the Sidebar render above) — conversations grouped
          // into a project aren't part of bulk-select, so this always renders
          // unselected regardless of the sidebar-wide selectionMode toggle.
          // groupByDate={false} for the same reason Pinned skips it: project
          // membership, not recency, is already what's grouping these rows.
          <ConvList
            conversations={conversations}
            active={active}
            view={view}
            streaming={streaming}
            projects={projects}
            selectionMode={false}
            selectedIds={selectedIds}
            onOpen={onOpen}
            onToggleSelect={onToggleSelect}
            onDelete={onDelete}
            onMoveToProject={onMoveToProject}
            onRename={onRenameConversation}
            onTogglePin={onTogglePin}
            onCreateProject={onCreateProject}
            groupByDate={false}
          />
        ))}
    </div>
  );
}

/** Inline "name this project" row shown under the Projects header while
 * creating one — same one-shot-submit shape as `ProjectGroup`'s rename
 * input (Enter blurs to submit, Escape cancels, a ref guards against a
 * double-submit). An empty name cancels instead of creating a nameless
 * project. */
function NewProjectRow({
  onCreate,
  onCancel,
}: {
  onCreate: (name: string) => void;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const settledRef = useRef(false);

  const submit = () => {
    if (settledRef.current) return;
    settledRef.current = true;
    const trimmed = name.trim();
    if (trimmed) onCreate(trimmed);
    else onCancel();
  };

  const cancel = () => {
    settledRef.current = true;
    onCancel();
  };

  return (
    <div className="project-create-row">
      <input
        type="text"
        autoFocus
        value={name}
        placeholder={t("sidebar.newProjectPlaceholder")}
        className="project-rename-input"
        onChange={(e) => setName(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") e.currentTarget.blur();
          if (e.key === "Escape") cancel();
        }}
        onBlur={submit}
      />
    </div>
  );
}

function EnginePill() {
  const { t } = useTranslation();
  const engine = useStore((s) => s.engine);
  const models = useStore((s) => s.models);
  const settings = useStore((s) => s.settings);
  const engineBusy = useStore((s) => s.engineBusy);
  const streaming = useStore((s) => s.streaming);
  const useModel = useStore((s) => s.useModel);
  const stopModel = useStore((s) => s.stopModel);
  const setMoreDialogPage = useStore((s) => s.setMoreDialogPage);
  const setMobileSidebarOpen = useStore((s) => s.setMobileSidebarOpen);
  const appInfo = useStore((s) => s.appInfo);

  const nameOf = (id: string) =>
    models.find((m) => m.spec.id === id)?.spec.name ?? id;

  // The selected model, once it's the one loaded (or loadable): drives both
  // the "Asleep" label's name and the Load button's target.
  const selected = settings?.selectedModelId;
  const isInstalled =
    selected != null &&
    models.find((m) => m.spec.id === selected)?.status.type === "installed";
  const loadableId = isInstalled ? (selected as string) : null;

  let dot = "pill-dot-off";
  let label: string;
  // Hover-only text for the status dot. Set only for the three routine
  // states (ready/warming up/asleep) — there the always-visible label is
  // just the model name, so the status word moves to a tooltip on the badge
  // instead of being appended inline. Every other state is an actionable
  // message (retry, pair a device, install a model…) and stays visible
  // outright rather than hiding behind a hover.
  let statusTooltip: string | undefined;
  let onClick: (() => void) | undefined;

  switch (engine.type) {
    case "running":
      dot = "pill-dot-ok";
      label = nameOf(engine.data.modelId);
      statusTooltip = t("sidebar.engine.ready");
      break;
    case "starting":
      dot = "pill-dot-warm";
      label = nameOf(engine.data.modelId);
      statusTooltip = isStartingModelDiskSlow(models, engine.data.modelId)
        ? t("sidebar.engine.warmingUpSlow")
        : engine.data.progressPercent != null
          ? t("sidebar.engine.warmingUpPercent", { percent: engine.data.progressPercent })
          : t("sidebar.engine.warmingUp");
      break;
    case "failed":
      dot = "pill-dot-bad";
      label = t("sidebar.engine.needsAttention");
      onClick = () => {
        if (settings?.selectedModelId && !engineBusy) {
          void useModel(settings.selectedModelId);
        }
      };
      break;
    // RemoteBackend only (Android) — this device chats through a paired
    // desktop instead of running a model itself; see docs/design/MOBILE.md.
    case "remoteUnpaired":
      dot = "pill-dot-off";
      label = t("sidebar.engine.remoteUnpaired");
      onClick = () => {
        setMoreDialogPage("devices");
        setMobileSidebarOpen(false);
      };
      break;
    case "remoteUnreachable":
      dot = "pill-dot-bad";
      label = t("sidebar.engine.remoteUnreachable", { name: engine.data.hostName });
      onClick = () => {
        setMoreDialogPage("devices");
        setMobileSidebarOpen(false);
      };
      break;
    default:
      // "stopped" only ever reaches this device on Android once paired
      // (RemoteBackend short-circuits to remoteUnpaired above otherwise) —
      // it describes the *desktop's* engine, so neither the asleep label
      // nor "No model installed" (which also navigates to a Library view
      // Android has no local-install use for) makes sense here.
      if (appInfo?.isAndroid) {
        label = t("sidebar.engine.remotePairedIdle");
        onClick = () => {
          setMoreDialogPage("devices");
          setMobileSidebarOpen(false);
        };
      } else if (loadableId) {
        label = nameOf(loadableId);
        statusTooltip = t("sidebar.engine.asleep");
      } else {
        label = t("sidebar.engine.noModelInstalled");
        onClick = () => {
          setMoreDialogPage("library");
          setMobileSidebarOpen(false);
        };
      }
  }

  return (
    // Ready/Warming up/Needs attention/Asleep, polled every 2s by App.tsx —
    // announce transitions the same way AutoUnloadTooltip (now rendered from
    // App.tsx's shell, not from here) does: role="status" wrapping both the
    // status text and its action button(s). The bordered
    // pill is a plain container, not a button itself — it holds both the
    // model-name button and the load/unload icon button so we don't end up
    // nesting a <button> inside a <button>.
    <div className="engine-pill-row" role="status">
      <div className={`engine-pill${onClick ? " engine-pill-clickable" : ""}`}>
        {/* The real loading percentage (see EngineStatus::Starting's Rust
            doc), as a fill sweeping across the box itself rather than a
            separate widget — GPU-offloaded loads only, since a CPU-only
            mmap load has nothing honest to report. `.engine-pill`'s own
            `z-index: -1` stacking (chat.css) keeps this behind the dot/
            label/button painted below instead of covering them. */}
        {engine.type === "starting" && engine.data.progressPercent != null && (
          <div
            className="engine-pill-progress-fill"
            style={{ width: `${engine.data.progressPercent}%` }}
            aria-hidden="true"
          />
        )}
        <button
          type="button"
          className="engine-pill-info"
          onClick={onClick}
          disabled={!onClick}
        >
          <span className={`pill-dot ${dot}`} title={statusTooltip} />
          <span className="pill-label">
            {label}
            {engine.type === "starting" && engine.data.progressPercent != null && (
              <span className="pill-label-progress"> {engine.data.progressPercent}%</span>
            )}
          </span>
        </button>
        {engine.type === "starting" && (
          // No action is available mid-load — this just fills the same slot
          // the Load/Unload button occupies in every other state, so the
          // pill doesn't visually jump once loading finishes.
          <span className="engine-power-btn" aria-hidden="true">
            <span className="searching-spinner" />
          </span>
        )}
        {engine.type === "running" && (
          <button
            type="button"
            className="btn-quiet engine-power-btn"
            disabled={engineBusy || streaming}
            aria-label={t("sidebar.engine.unload")}
            title={
              streaming
                ? t("sidebar.engine.waitForReply")
                : t("sidebar.engine.unloadTitle")
            }
            onClick={() => void stopModel()}
          >
            <EjectIcon />
          </button>
        )}
        {engine.type === "stopped" && loadableId && (
          <button
            type="button"
            className="btn-quiet engine-power-btn"
            disabled={engineBusy}
            aria-label={t("sidebar.engine.load")}
            title={t("sidebar.engine.loadTitle")}
            onClick={() => void useModel(loadableId)}
          >
            <PowerIcon />
          </button>
        )}
      </div>
    </div>
  );
}
