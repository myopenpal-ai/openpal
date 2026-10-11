import type { Page } from "@playwright/test";

/**
 * Minimal `window.__TAURI_INTERNALS__.invoke` shim, injected before any app
 * script runs (`page.addInitScript`) so `ui/src/lib/api.ts`'s `invoke()`
 * calls resolve against these canned responses instead of erroring (there is
 * no real Tauri backend behind the plain Vite dev server this config drives).
 *
 * Deliberately does NOT implement the real `Channel`/`transformCallback`
 * wire protocol `download_model`/`chat_stream`/etc. use for streaming — under
 * a real Tauri runtime, `invoke()` replaces a `Channel` argument with a
 * numeric callback id before this shim would ever see it, so faking that
 * faithfully needs its own per-command support. Every fixture registered
 * here must be a plain request/response command; flows that stream events
 * need a purpose-built mock (see ui/src/lib/api.ts's own Vitest unit tests,
 * which cover the Channel contract in isolation instead).
 */
export async function mockTauriIpc(page: Page, responses: Record<string, unknown>): Promise<void> {
  await page.addInitScript((responsesJson: string) => {
    const responses = JSON.parse(responsesJson) as Record<string, unknown>;
    (window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {
      invoke: (cmd: string) => {
        if (!(cmd in responses)) {
          return Promise.reject(new Error(`tauriMock: no fixture registered for command "${cmd}"`));
        }
        return Promise.resolve(responses[cmd]);
      },
    };
  }, JSON.stringify(responses));
}
