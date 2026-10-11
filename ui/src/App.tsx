import { useEffect, type CSSProperties } from "react";
import { useTranslation } from "react-i18next";
import { useStore } from "./state/store";
import AppLockGate from "./views/AppLockGate";
import Onboarding from "./views/Onboarding";
import RecoverySetup from "./views/RecoverySetup";
import RecoveryUnlock from "./views/RecoveryUnlock";
import ChatView from "./views/ChatView";
import TranslateView from "./views/TranslateView";
import CorrectView from "./views/CorrectView";
import ImproveView from "./views/ImproveView";
import ConversationSearch from "./views/ConversationSearch";
import Sidebar from "./views/Sidebar";
import ChatToolbar from "./views/ChatToolbar";
import MoreDialog from "./views/MoreDialog";
import UpdateDialog from "./views/UpdateDialog";
import SignInDialog from "./views/SignInDialog";
import AddDeviceDialog from "./views/AddDeviceDialog";
import JoinGroupDialog from "./views/JoinGroupDialog";
import AutoUnloadTooltip from "./components/AutoUnloadTooltip";

const MenuIcon = () => (
  <svg
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="2"
    strokeLinecap="round"
    aria-hidden="true"
  >
    <line x1="4" y1="7" x2="20" y2="7" />
    <line x1="4" y1="12" x2="20" y2="12" />
    <line x1="4" y1="17" x2="20" y2="17" />
  </svg>
);

/** Panel-toggle glyph for showing/hiding the sidebar (see `sidebarCollapsed`)
 * — a stylized window split into a sidebar column and content pane. */
const PanelIcon = () => (
  <svg
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.9"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <rect x="3.5" y="5" width="17" height="14" rx="2" />
    <path d="M9.5 5v14" />
  </svg>
);

export default function App() {
  const { t } = useTranslation();
  const booted = useStore((s) => s.booted);
  const settings = useStore((s) => s.settings);
  const appInfo = useStore((s) => s.appInfo);
  const appLockUnlocked = useStore((s) => s.appLockUnlocked);
  const view = useStore((s) => s.view);
  const moreDialogPage = useStore((s) => s.moreDialogPage);
  const conversationSearchOpen = useStore((s) => s.conversationSearchOpen);
  const mobileSidebarOpen = useStore((s) => s.mobileSidebarOpen);
  const setMobileSidebarOpen = useStore((s) => s.setMobileSidebarOpen);
  const sidebarCollapsed = useStore((s) => s.sidebarCollapsed);
  const setSidebarCollapsed = useStore((s) => s.setSidebarCollapsed);
  const sidebarWidth = useStore((s) => s.sidebarWidth);
  // The mobile drawer toggle only makes sense on a real narrow/touch device
  // (Android today — see docs/design/MOBILE.md). On desktop the sidebar is always a
  // permanent grid column or explicitly collapsed via sidebarCollapsed below,
  // never a drawer, so the button has no job there even at a narrow window
  // width and would just be a confusing extra control.
  const isAndroid = useStore((s) => s.appInfo?.isAndroid ?? false);
  const updateStatus = useStore((s) => s.updateStatus);
  const signInOpen = useStore((s) => s.signInOpen);
  const addDeviceOpen = useStore((s) => s.addDeviceOpen);
  const joinGroupOpen = useStore((s) => s.joinGroupOpen);
  const boot = useStore((s) => s.boot);
  const pollEngine = useStore((s) => s.pollEngine);

  useEffect(() => {
    void boot();
  }, [boot]);

  useEffect(() => {
    const id = window.setInterval(() => void pollEngine(), 2000);
    return () => window.clearInterval(id);
  }, [pollEngine]);

  if (!booted) {
    return <header className="titlebar" data-tauri-drag-region />;
  }

  // Checked first, ahead of both recovery setup and onboarding, and ahead of
  // the `!settings` check below: a user who's actually locked out (the OS
  // keychain that normally unlocks the encryption key silently failed)
  // takes priority over everything else, and the backend won't build a
  // store to read `settings` from until recovery succeeds — see `boot()`'s
  // two-phase split in `state/store.ts`. This screen must render without it.
  if (appInfo?.needsRecovery) {
    return (
      <>
        <header className="titlebar" data-tauri-drag-region />
        <RecoveryUnlock />
      </>
    );
  }

  // Checked next, still ahead of `!settings` below: same two-phase boot
  // split as `needsRecovery` above (see `boot()` in `state/store.ts`) — the
  // backend won't build a store to read `settings` from until the PIN (or a
  // recovery code) checks out. `appLockUnlocked` is session-only, since
  // `needsAppUnlock` itself never durably clears (see its own doc comment).
  if (appInfo?.needsAppUnlock && !appLockUnlocked) {
    return (
      <>
        <header className="titlebar" data-tauri-drag-region />
        <AppLockGate />
      </>
    );
  }

  if (!settings) {
    return <header className="titlebar" data-tauri-drag-region />;
  }

  // Checked independently of onboarding — and ahead of it — since this must
  // also interrupt an existing user's very next launch, not just a brand-new
  // install: the encryption key it backs up already exists for everyone, but
  // nobody has a recovery code for it yet.
  if (appInfo?.needsRecoverySetup) {
    return (
      <>
        <header className="titlebar" data-tauri-drag-region />
        <RecoverySetup />
      </>
    );
  }

  if (!settings.onboardingCompleted) {
    return (
      <>
        <header className="titlebar" data-tauri-drag-region />
        <Onboarding />
      </>
    );
  }

  return (
    <div
      className={`shell${sidebarCollapsed ? " shell-sidebar-collapsed" : ""}`}
      style={{ "--sidebar-width": `${sidebarWidth}px` } as CSSProperties}
    >
      {/* First focusable element in the shell: invisible until it receives
          keyboard focus, then lets a Tab-only user jump straight past the
          titlebar and the (potentially long) conversation list. */}
      <a href="#main-content" className="skip-link">
        {t("appShell.skipToContent")}
      </a>
      <header className="titlebar" data-tauri-drag-region>
        {/* Android only — a real narrow/touch device where the sidebar is a
            drawer, not a permanent column (see docs/design/MOBILE.md). On desktop
            the sidebar is either the permanent grid column or explicitly
            collapsed below, so this drawer toggle has no job there. */}
        {isAndroid && (
          <button
            type="button"
            className="mobile-sidebar-toggle icon-btn"
            aria-label={t(mobileSidebarOpen ? "appShell.closeSidebar" : "appShell.openSidebar")}
            aria-expanded={mobileSidebarOpen}
            onClick={() => setMobileSidebarOpen(!mobileSidebarOpen)}
          >
            <MenuIcon />
          </button>
        )}
        {/* The desktop "OpenPal" brand + show/hide-sidebar control, always
            rendered here — never inside Sidebar.tsx's own .sidebar-top —
            for two reasons: (1) .titlebar is `position: fixed` with a
            z-index above everything else and spans the full window width
            (it's the drag region), so it sits directly over .sidebar-top and
            would intercept clicks meant for a button rendered there instead;
            (2) staying here keeps both visible even when Sidebar unmounts on
            collapse, which is the point — the only way back. Sidebar.tsx
            still renders its own "OpenPal" for the mobile drawer, hidden
            above the mobile breakpoint (chat.css) where this one takes over.
            Positioned via CSS to clear the macOS traffic lights, same as
            .sidebar-top normally does; hidden below the mobile breakpoint
            itself (global.css), like .mobile-sidebar-toggle is hidden above
            it — below it there's no permanent sidebar column to collapse. */}
        <div className="titlebar-brand">
          <span className="sidebar-brand">OpenPal</span>
          <button
            type="button"
            className="sidebar-collapse-toggle icon-btn"
            aria-label={t(sidebarCollapsed ? "appShell.openSidebar" : "appShell.closeSidebar")}
            aria-expanded={!sidebarCollapsed}
            onClick={() => setSidebarCollapsed(!sidebarCollapsed)}
          >
            <PanelIcon />
          </button>
        </div>
        {view === "chat" && <ChatToolbar />}
      </header>
      {!sidebarCollapsed && <Sidebar />}
      {/* Below the mobile breakpoint only (mobileSidebarOpen is otherwise
          always false) — dismisses the open drawer on a tap outside it. */}
      {mobileSidebarOpen && (
        <div className="sidebar-scrim" onClick={() => setMobileSidebarOpen(false)} />
      )}
      {/* Only ever where the user talks to their model — everything else
          (the More menu's pages, conversation search) opens over it in a
          dialog below, leaving whatever is here mounted and untouched. */}
      <main className="main-pane" id="main-content" tabIndex={-1}>
        {view === "chat" ? (
          <ChatView />
        ) : view === "translate" ? (
          <TranslateView />
        ) : view === "correct" ? (
          <CorrectView />
        ) : (
          <ImproveView />
        )}
      </main>
      {/* Renders over the sidebar's model row but from up here, not inside
          that column — see `.auto-unload-tooltip` for why. Shows nothing
          unless a loaded model is seconds from being unloaded. */}
      <AutoUnloadTooltip />
      {moreDialogPage && <MoreDialog page={moreDialogPage} />}
      {conversationSearchOpen && <ConversationSearch />}
      {updateStatus !== "idle" && <UpdateDialog />}
      {signInOpen && <SignInDialog />}
      {addDeviceOpen && <AddDeviceDialog />}
      {joinGroupOpen && <JoinGroupDialog />}
    </div>
  );
}
