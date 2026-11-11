import { defineConfig, devices } from "@playwright/test";

/**
 * Drives the plain Vite dev server (no Tauri, no Rust backend) with the
 * Tauri IPC layer mocked — see tests/e2e/support/tauriMock.ts. This is the
 * fast, cross-platform "mocked-IPC" tier: a regression net for UI state
 * machines and rendering, not a substitute for a real native e2e run against
 * the built app (which needs a real webview + IPC + subprocess engine, and
 * lives outside this fast PR-gating tier).
 */
export default defineConfig({
  testDir: "./tests/e2e",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? "github" : "list",
  // The CI runner has 4 vCPUs and Playwright's default caps workers at half of
  // them; run the e2e specs across all four on CI (the CI e2e tier was ~5.5m
  // of the frontend job's wall-clock with 2 workers). Local runs keep the
  // default so a dev laptop isn't saturated.
  workers: process.env.CI ? 4 : undefined,
  use: {
    baseURL: "http://localhost:5173",
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: "npm run dev -- --strictPort",
    url: "http://localhost:5173",
    reuseExistingServer: !process.env.CI,
    timeout: 30_000,
  },
});
