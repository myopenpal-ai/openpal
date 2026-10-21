/** Canned command responses for the mocked-IPC e2e tier. Kept intentionally
 * loose-typed (plain objects, not the `ui/src/lib/types.ts` interfaces) since
 * these cross into `page.addInitScript` as JSON — see tauriMock.ts. */

export function freshInstallResponses(overrides: Record<string, unknown> = {}) {
  return {
    get_settings: {
      onboardingCompleted: false,
      userName: null,
      selectedModelId: null,
      systemPrompt: "",
      showThinking: true,
      showGenerationStats: false,
      forceCpu: false,
      diskOffload: false,
      autoUnloadEnabled: true,
      autoUnloadMinutes: 15,
      autoUnloadIncludeDiskSlow: false,
      autoLoadOnStartup: true,
      logLevel: "info",
      language: "en",
      voiceSpeed: 1,
      voiceExpressiveness: 1,
      voiceSentencePause: 0.2,
      voiceId: "en_US-amy-medium",
      autoCheckUpdates: true,
      skippedUpdateVersion: null,
      chatBackend: "local",
      selectedCloudModelId: null,
      defaultModelId: null,
      theme: "system",
      desktopNotifications: false,
    },
    get_engine_status: { type: "stopped" },
    list_conversations: [],
    get_app_info: { portable: false, dataDir: "/tmp/openpal-e2e", version: "0.1.0" },
    get_account: null,
    get_models: [],
    // Fired once from `boot()` (state/store.ts) for Hugging Face models added
    // before OpenPal tracked group/license/release-date metadata. Resolves to
    // how many models it updated; 0 means "nothing to do", so nothing
    // re-fetches. Without it the mock rejects, and `main.tsx` forwards the
    // unhandled rejection to `frontend_log` — which, unmocked, rejects in
    // turn and floods every spec's console-error assertion.
    backfill_custom_model_metadata: 0,
    frontend_log: null,
    // Settings -> General -> Notifications reads the OS permission when it
    // opens (`notification_permission`, which never prompts), and turning the
    // switch on asks for it (`request_notification_permission`, which on macOS
    // shows the system prompt; state/store.ts). "granted" is also what every
    // desktop OS but macOS ever answers. Without them the mock rejects, the
    // store swallows that, and nothing shows -- but a failure path that logged
    // it would flood every spec's console-error assertion. A spec for the
    // "blocked" line overrides both with "denied".
    notification_permission: "granted",
    request_notification_permission: "granted",
    // Asking Rust to post the OS notification that mirrors an in-app alert.
    // Rust posts it only with the switch on and the window unfocused, so the
    // reply carries nothing; registered so no call is ever an unknown command.
    notify_native: null,
    list_plugins: [],
    list_cloud_models: [],
    // Settings -> Your data asks for the export's size as soon as it opens,
    // and again on every checkbox tick: 1 MB in bytes.
    estimate_export_size: 1_048_576,
    // ...and how much each of its checkboxes' categories holds.
    export_counts: {
      conversations: 0,
      projects: 0,
      artifacts: 0,
      changedSettings: 0,
      skills: 0,
      customModels: 0,
      enabledPlugins: 0,
    },
    get_system_report: {
      hardware: {
        chip: "Apple M4",
        isAppleSilicon: true,
        totalRamBytes: 16_000_000_000,
        availableRamBytes: 10_000_000_000,
        cpuCores: 10,
        hasMetal: true,
        freeDiskBytes: 200_000_000_000,
        osVersion: "macOS 26.5",
      },
      recommendation: { primaryModelId: null, headline: "", reasons: [], ranked: [] },
    },
    ...overrides,
  };
}
