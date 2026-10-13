import type { Page } from "@playwright/test";

/**
 * Makes the mocked backend remember `update_settings`, the way the real one
 * does. Call it AFTER `mockTauriIpc` (init scripts run in registration order,
 * and this wraps the `invoke` that one installs).
 *
 * `mockTauriIpc` answers `get_settings` with the same canned object forever, so
 * anything that re-reads the backend copy — `completeOnboarding`, the refresh
 * after a download — silently resets a setting changed earlier in the same
 * page, and `page.reload()` forgets it altogether. That makes "pick a theme,
 * finish onboarding, it is still the theme" and "relaunch keeps the choice"
 * untestable on the bare mock. Here the saved settings go to `localStorage`
 * (per origin, and every Playwright test gets a fresh context, so nothing
 * leaks between tests) and `get_settings` returns them once there are any: a
 * reload then behaves like a relaunch of the real app.
 *
 * Only for specs that assert persistence. The rest keep the plain mock, whose
 * `update_settings: null` accepts a write and forgets it.
 */
export async function persistSettings(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const KEY = "__e2e_persisted_settings";
    const internals = (
      window as unknown as {
        __TAURI_INTERNALS__: { invoke: (cmd: string, args?: unknown) => Promise<unknown> };
      }
    ).__TAURI_INTERNALS__;
    const original = internals.invoke;
    internals.invoke = (cmd, args) => {
      if (cmd === "update_settings") {
        localStorage.setItem(KEY, JSON.stringify((args as { settings: unknown }).settings));
        return Promise.resolve(null);
      }
      if (cmd === "get_settings") {
        const saved = localStorage.getItem(KEY);
        if (saved) return Promise.resolve(JSON.parse(saved));
      }
      return original(cmd, args);
    };
  });
}
