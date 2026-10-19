//! OS-native notifications behind one contract, [`Notifier`], with one backend
//! per kind of platform:
//!
//! - [`macos`]: a bundled macOS app, through `UNUserNotificationCenter`.
//! - [`macos_dev`]: macOS outside an app bundle (`tauri dev`), through the
//!   legacy center, as Terminal.
//! - [`windows`]: WinRT toasts, under an app id OpenPal registers itself.
//! - [`freedesktop`]: Linux, the notification server over D-Bus.
//! - [`android`]: Android, through `NotifyPlugin.kt`.
//! - [`unsupported`]: iOS, which has no backend yet.
//!
//! [`select`] is the one place that picks between them. The OS is fixed when
//! the app is compiled, so that part is `cfg`; whether a macOS binary sits
//! inside an app bundle is only known once it runs, so that part is decided at
//! startup. `lib.rs` manages the result as [`Notifications`], and commands
//! reach notifications through it, never through a backend.
//!
//! A click (or tap) on a notification comes back through [`Clicks`]: the
//! backend reports it, OpenPal's window is brought forward, and the UI is
//! nudged to take it (`take_notification_click`), so a click that launched the
//! app waits for a UI that is ready to open a conversation.
//!
//! Best-effort by design: a notification the OS drops, or a missing
//! notification server, never reaches the user as an error.
//!
//! The pure parts (the length caps and escaping in [`content`], the
//! bundle-path guard, the settings deep link, the status mapping) are small free
//! functions with unit tests, because the OS calls themselves cannot run under
//! `cargo test`. What sits on top of a backend is tested with a fake one.

#[cfg(target_os = "android")]
mod android;
mod content;
#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "ios"))
))]
mod freedesktop;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod macos_dev;
mod unsupported;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "android")]
pub use android::plugin_init as android_plugin_init;

use std::path::Path;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Runtime};

/// The payload-less nudge sent when a notification is clicked; the UI answers
/// it with `take_notification_click`, the same nudge-then-read shape as
/// `engine-status-changed`.
pub const NOTIFICATION_CLICKED_EVENT: &str = "notification-clicked";

/// Whether the OS lets OpenPal show notifications (mirrored in
/// `ui/src/lib/types.ts`). No build constructs every variant: `Denied` and
/// `Prompt` come only from the macOS and Android backends and `Unsupported`
/// only from the iOS one, which is what the `allow` is for (clippy runs with
/// `-D warnings` on Linux in CI).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub enum NotificationPermission {
    /// Allowed; or there is nothing to ask on this OS (Windows, Linux); or the
    /// OS could not be asked, so the worst case is a missing "blocked" line,
    /// never a false one.
    Granted,
    /// The user said no. The OS will not ask again: only its settings can
    /// undo it.
    Denied,
    /// Not asked yet (or, on Android, refused once and still allowed to ask):
    /// a request puts the OS's prompt in front of the user.
    Prompt,
    /// This build has no notification backend at all.
    Unsupported,
}

/// Why a backend could not do what it was asked. `NotAllowed` comes only from
/// the macOS and Android backends, hence the `allow`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[cfg_attr(not(any(target_os = "macos", target_os = "android")), allow(dead_code))]
pub enum NotifyError {
    /// No backend, or no settings page, on this system.
    #[error("not available on this system")]
    Unsupported,
    /// The OS does not let OpenPal post right now (blocked, or its prompt
    /// went unanswered). Not a fault, so nothing to warn about.
    #[error("not allowed by the OS")]
    NotAllowed,
    /// The OS, the notification library or the opener reported a failure.
    #[error("{0}")]
    Os(String),
}

/// One notification, as the UI asked for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
    /// The conversation a click opens ("answer ready"); `None` for one that
    /// only brings OpenPal forward.
    pub conversation_id: Option<String>,
}

/// A click on one of OpenPal's notifications, as the UI reads it back
/// (mirrored in `ui/src/lib/types.ts`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationClick {
    pub conversation_id: Option<String>,
}

/// Where a backend reports a click. It keeps the latest one until the UI takes
/// it, and runs `on_click` (raise the window, nudge the UI) each time. Only the
/// latest is kept: two clicks before the UI reads them mean the user's last
/// choice wins. Cheap to clone; every clone shares the same slot.
#[derive(Clone)]
pub struct Clicks {
    pending: Arc<Mutex<Option<NotificationClick>>>,
    on_click: Arc<dyn Fn() + Send + Sync>,
}

impl Clicks {
    pub fn new(on_click: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            pending: Arc::default(),
            on_click: Arc::new(on_click),
        }
    }

    pub fn report(&self, click: NotificationClick) {
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(click);
        (self.on_click)();
    }

    /// The click waiting for the UI, once: a second call has nothing.
    pub fn take(&self) -> Option<NotificationClick> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}

/// One way of reaching the OS's notifications.
///
/// Any method may block on the OS (the macOS ones wait for a callback, the
/// Android ones for the Kotlin side), so [`Notifications`] calls them from a
/// blocking thread; a backend never has to care where it is called from.
pub trait Notifier: Send + Sync {
    /// Whether this build can show OS notifications at all. Cheap and fixed for
    /// the life of the process, unlike [`Self::permission`], so `get_app_info`
    /// reports it at boot.
    fn supported(&self) -> bool;

    /// What the OS allows right now. Never prompts.
    fn permission(&self) -> NotificationPermission;

    /// Ask the OS, then report where that left us. Where the OS has a prompt
    /// and may still show it, this puts it up and waits for the answer.
    fn request_permission(&self) -> NotificationPermission;

    /// Open the OS page where the user allows notifications for OpenPal: a
    /// deep link on desktop, an Intent on Android. `Unsupported` where this
    /// system has no such page.
    fn open_settings(&self) -> Result<(), NotifyError>;

    /// Whether the OS may read the body as markup, so it has to be escaped.
    fn renders_body_markup(&self) -> bool;

    /// Post one notification, its words already capped and escaped by
    /// [`content::prepare`]. A backend that can tell when it is clicked reports
    /// that to the [`Clicks`] it was built with. A backend whose OS has never
    /// been asked may ask first, prompt and all.
    fn post(&self, notification: &Notification) -> Result<(), NotifyError>;
}

/// The backend [`select`] picked and the clicks it reports, as Tauri managed
/// state: what every command that touches notifications takes. Managed from
/// the very start of `setup()`, so it answers before the app is unlocked.
#[derive(Clone)]
pub struct Notifications {
    backend: Arc<dyn Notifier>,
    clicks: Clicks,
}

impl Notifications {
    pub fn new(backend: impl Notifier + 'static, clicks: Clicks) -> Self {
        Self {
            backend: Arc::new(backend),
            clicks,
        }
    }

    pub fn supported(&self) -> bool {
        self.backend.supported()
    }

    /// [`Notifier::permission`], on a blocking thread so the macOS probe never
    /// holds up the main thread or the async runtime.
    pub async fn permission(&self) -> NotificationPermission {
        let backend = Arc::clone(&self.backend);
        tauri::async_runtime::spawn_blocking(move || backend.permission())
            .await
            .unwrap_or(NotificationPermission::Granted)
    }

    /// [`Notifier::request_permission`], on a blocking thread: on macOS and
    /// Android it waits for the user to answer the system prompt.
    pub async fn request_permission(&self) -> NotificationPermission {
        let backend = Arc::clone(&self.backend);
        tauri::async_runtime::spawn_blocking(move || backend.request_permission())
            .await
            .unwrap_or(NotificationPermission::Granted)
    }

    /// Open the OS page where the user can allow notifications for OpenPal,
    /// the place a "blocked" answer can only be undone: macOS never raises its
    /// prompt a second time, and Android stops after the second "no".
    pub async fn open_settings(&self) -> Result<(), NotifyError> {
        let backend = Arc::clone(&self.backend);
        tauri::async_runtime::spawn_blocking(move || backend.open_settings())
            .await
            .unwrap_or_else(|e| Err(NotifyError::Os(e.to_string())))
    }

    /// Post one OS notification. Fire-and-forget: delivery happens on a
    /// blocking thread, and anything that goes wrong is logged, not returned.
    pub fn show(&self, notification: Notification) {
        let backend = Arc::clone(&self.backend);
        tauri::async_runtime::spawn_blocking(move || {
            match deliver(backend.as_ref(), notification) {
                Ok(()) => {}
                // The OS said no, or there is nothing to post to: not faults.
                Err(e @ (NotifyError::NotAllowed | NotifyError::Unsupported)) => {
                    tracing::debug!("native notification skipped: {e}");
                }
                Err(e) => tracing::warn!("native notification failed: {e}"),
            }
        });
    }

    /// The click the UI has not acted on yet, if any (see [`Clicks::take`]).
    pub fn take_click(&self) -> Option<NotificationClick> {
        self.clicks.take()
    }
}

/// Pick this process's backend. Called once, at the start of `setup()`.
pub fn select<R: Runtime>(app: &AppHandle<R>) -> Notifications {
    let clicks = {
        let app = app.clone();
        Clicks::new(move || {
            bring_forward(&app);
            if let Err(e) = app.emit(NOTIFICATION_CLICKED_EVENT, ()) {
                tracing::warn!("could not emit {NOTIFICATION_CLICKED_EVENT}: {e}");
            }
        })
    };
    #[cfg(target_os = "macos")]
    if let Some(center) =
        macos::UserNotifications::new(app.config().identifier.clone(), clicks.clone())
    {
        return Notifications::new(center, clicks);
    }
    #[cfg(target_os = "android")]
    if let Some(android) = android::AndroidNotifier::new(clicks.clone()) {
        return Notifications::new(android, clicks);
    }
    #[cfg(target_os = "macos")]
    let backend = macos_dev::LegacyCenter;
    #[cfg(windows)]
    let backend = windows::WinToasts::new(app.config().identifier.clone(), clicks.clone());
    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "android", target_os = "ios"))
    ))]
    let backend = freedesktop::Freedesktop::new(clicks.clone());
    #[cfg(any(target_os = "android", target_os = "ios"))]
    let backend = unsupported::Unsupported;
    Notifications::new(backend, clicks)
}

/// Raise OpenPal's window for a click: unminimized, shown and focused. On
/// mobile the OS has already brought the app forward by opening it.
fn bring_forward<R: Runtime>(app: &AppHandle<R>) {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        use tauri::Manager;
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    let _ = app;
}

/// Cap, escape and post: the synchronous core of [`Notifications::show`],
/// apart so a test can hand it a fake backend and see what reached it.
fn deliver(backend: &dyn Notifier, notification: Notification) -> Result<(), NotifyError> {
    let (title, body) = content::prepare(
        &notification.title,
        &notification.body,
        backend.renders_body_markup(),
    );
    backend.post(&Notification {
        title,
        body,
        ..notification
    })
}

/// `NotifyPlugin.kt`'s answer, which is "granted", "denied" (asked, refused, and
/// Android will not ask again) or "prompt" (never asked, or refused once and
/// still allowed to ask). Anything else is a Kotlin side this Rust side does
/// not know, and reads as "prompt": not blocked, and asking is still worth a
/// try. Compiled everywhere so the CI host can test it, used only on Android.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn android_permission(state: &str) -> NotificationPermission {
    match state {
        "granted" => NotificationPermission::Granted,
        "denied" => NotificationPermission::Denied,
        _ => NotificationPermission::Prompt,
    }
}

/// System Settings' Notifications pane (macOS 13 and later), with the app's
/// row selected when `identifier` names a bundle it knows. Compiled everywhere
/// so the CI host can test it, used only on macOS.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn macos_settings_url(identifier: &str) -> String {
    format!("x-apple.systempreferences:com.apple.Notifications-Settings.extension?id={identifier}")
}

/// Whether `exe` is a binary inside a macOS app bundle:
/// `<Name>.app/Contents/MacOS/<binary>`.
///
/// `UNUserNotificationCenter` needs the running process to belong to a bundle;
/// outside one (`cargo run`, `tauri dev`, the test binaries) it raises an
/// `NSInternalInconsistencyException` ("bundleProxyForCurrentProcess is nil")
/// that aborts the process. So the probe must never run unless this says yes.
/// Compiled everywhere so the CI host can test it, used only on macOS.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn is_inside_app_bundle(exe: &Path) -> bool {
    let parts: Vec<&str> = exe
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    parts.windows(3).any(|w| {
        w[0].strip_suffix(".app")
            .is_some_and(|stem| !stem.is_empty())
            && w[1] == "Contents"
            && w[2] == "MacOS"
    })
}

/// What every backend owes its callers, whatever it does underneath: a build
/// without notifications says so from every method and has no page to send
/// anyone to, and asking never makes a supported build look unsupported. Each
/// backend's tests run it on whatever the host can build. (A supported
/// backend's `open_settings` is left alone: it would open the real settings
/// of whoever runs the tests.)
#[cfg(test)]
fn assert_contract(backend: &dyn Notifier) {
    let unsupported = !backend.supported();
    assert_eq!(
        backend.permission() == NotificationPermission::Unsupported,
        unsupported
    );
    assert_eq!(
        backend.request_permission() == NotificationPermission::Unsupported,
        unsupported
    );
    if unsupported {
        assert_eq!(backend.open_settings(), Err(NotifyError::Unsupported));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A backend that records what it was handed and answers as told.
    struct Fake {
        permission: NotificationPermission,
        markup: bool,
        settings: Result<(), NotifyError>,
        refusal: Option<NotifyError>,
        posted: Mutex<Vec<Notification>>,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                permission: NotificationPermission::Granted,
                markup: false,
                settings: Ok(()),
                refusal: None,
                posted: Mutex::default(),
            }
        }

        fn posted(&self) -> Vec<Notification> {
            self.posted.lock().unwrap().clone()
        }
    }

    impl Notifier for Fake {
        fn supported(&self) -> bool {
            true
        }
        fn permission(&self) -> NotificationPermission {
            self.permission
        }
        fn request_permission(&self) -> NotificationPermission {
            self.permission
        }
        fn open_settings(&self) -> Result<(), NotifyError> {
            self.settings.clone()
        }
        fn renders_body_markup(&self) -> bool {
            self.markup
        }
        fn post(&self, notification: &Notification) -> Result<(), NotifyError> {
            self.posted.lock().unwrap().push(notification.clone());
            self.refusal.clone().map_or(Ok(()), Err)
        }
    }

    fn quiet_clicks() -> Clicks {
        Clicks::new(|| {})
    }

    fn notification(title: &str, body: &str) -> Notification {
        Notification {
            title: title.to_owned(),
            body: body.to_owned(),
            conversation_id: None,
        }
    }

    // The UI's Settings "blocked" line is `permission === "denied"`, a string
    // `ui/src/lib/types.ts` mirrors by hand, with nothing else to catch drift.
    #[test]
    fn permission_serializes_the_way_the_ui_spells_it() {
        for (permission, spelled) in [
            (NotificationPermission::Granted, "granted"),
            (NotificationPermission::Denied, "denied"),
            (NotificationPermission::Prompt, "prompt"),
            (NotificationPermission::Unsupported, "unsupported"),
        ] {
            assert_eq!(serde_json::to_value(permission).unwrap(), spelled);
        }
    }

    // Same seam as above, for what `take_notification_click` hands the UI and
    // the event name `ui/src/lib/api.ts` listens under.
    #[test]
    fn a_click_reaches_the_ui_under_the_names_it_reads() {
        let click = NotificationClick {
            conversation_id: Some("c1".into()),
        };
        assert_eq!(
            serde_json::to_value(click).unwrap(),
            serde_json::json!({ "conversationId": "c1" })
        );
        assert_eq!(NOTIFICATION_CLICKED_EVENT, "notification-clicked");
    }

    #[test]
    fn the_fake_honours_the_contract_it_stands_in_for() {
        assert_contract(&Fake::new());
    }

    #[test]
    fn a_backend_is_handed_capped_text_escaped_only_if_it_reads_markup() {
        let long_title = "t".repeat(500);
        let plain = Fake::new();
        deliver(&plain, notification(&long_title, "x < y")).unwrap();
        let markup = Fake {
            markup: true,
            ..Fake::new()
        };
        deliver(&markup, notification("A & B", "x < y")).unwrap();

        let posted = &plain.posted()[0];
        assert_eq!(posted.title.chars().count(), content::MAX_TITLE_CHARS);
        assert_eq!(posted.body, "x < y");
        assert_eq!(markup.posted(), vec![notification("A & B", "x &lt; y")]);
    }

    #[test]
    fn the_conversation_a_click_opens_reaches_the_backend_untouched() {
        let fake = Fake {
            markup: true,
            ..Fake::new()
        };
        let answer = Notification {
            conversation_id: Some("a<b&c".into()),
            ..notification("Answer ready", "OpenPal has finished answering.")
        };
        deliver(&fake, answer).unwrap();
        // Escaping is for what the OS shows, never for the id a click sends back.
        assert_eq!(fake.posted()[0].conversation_id.as_deref(), Some("a<b&c"));
    }

    #[test]
    fn a_backends_refusal_reaches_the_caller_unchanged() {
        let refusing = Fake {
            refusal: Some(NotifyError::NotAllowed),
            ..Fake::new()
        };
        assert_eq!(
            deliver(&refusing, notification("t", "b")),
            Err(NotifyError::NotAllowed)
        );
    }

    #[test]
    fn the_os_answer_comes_back_from_the_blocking_thread() {
        let notifications = Notifications::new(
            Fake {
                permission: NotificationPermission::Denied,
                settings: Err(NotifyError::Unsupported),
                ..Fake::new()
            },
            quiet_clicks(),
        );
        assert_eq!(
            tauri::async_runtime::block_on(notifications.permission()),
            NotificationPermission::Denied
        );
        assert_eq!(
            tauri::async_runtime::block_on(notifications.request_permission()),
            NotificationPermission::Denied
        );
        assert_eq!(
            tauri::async_runtime::block_on(notifications.open_settings()),
            Err(NotifyError::Unsupported)
        );
    }

    #[test]
    fn a_click_waits_for_the_ui_once_and_the_latest_one_wins() {
        let nudges = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&nudges);
        let clicks = Clicks::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        let notifications = Notifications::new(Fake::new(), clicks.clone());
        assert_eq!(notifications.take_click(), None);

        let on = |id: &str| NotificationClick {
            conversation_id: Some(id.into()),
        };
        clicks.report(on("first"));
        clicks.report(on("second"));

        // Every click raises the window and nudges the UI; the UI then reads
        // the one the user clicked last, and only once.
        assert_eq!(nudges.load(Ordering::SeqCst), 2);
        assert_eq!(notifications.take_click(), Some(on("second")));
        assert_eq!(notifications.take_click(), None);
    }

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    #[test]
    fn a_test_binary_gets_a_desktop_backend_that_cannot_abort_it() {
        // The test binary is not an installed app, so on macOS it must get the
        // dev backend, never the center (which would abort the process). Only
        // reads: asking could register an app id on Windows.
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let notifications = select(app.handle());
        assert!(notifications.supported());
        assert_ne!(
            tauri::async_runtime::block_on(notifications.permission()),
            NotificationPermission::Unsupported
        );
        assert_eq!(notifications.take_click(), None);
    }

    #[test]
    fn android_answers_map_onto_the_permission_the_ui_reads() {
        assert_eq!(
            android_permission("granted"),
            NotificationPermission::Granted
        );
        assert_eq!(android_permission("denied"), NotificationPermission::Denied);
        assert_eq!(android_permission("prompt"), NotificationPermission::Prompt);
        // A newer Kotlin side's word is never mistaken for a block.
        assert_eq!(
            android_permission("prompt-with-rationale"),
            NotificationPermission::Prompt
        );
    }

    #[test]
    fn macos_opens_the_notifications_pane_on_openpals_own_row() {
        assert_eq!(
            macos_settings_url("com.myopenpal.openpal"),
            "x-apple.systempreferences:com.apple.Notifications-Settings.extension\
             ?id=com.myopenpal.openpal"
        );
    }

    #[test]
    fn a_binary_inside_an_app_bundle_is_recognised() {
        assert!(is_inside_app_bundle(Path::new(
            "/Applications/OpenPal.app/Contents/MacOS/openpal"
        )));
        assert!(is_inside_app_bundle(Path::new(
            "/Users/ada/Downloads/OpenPal portable/OpenPal.app/Contents/MacOS/openpal"
        )));
    }

    #[test]
    fn a_binary_outside_an_app_bundle_is_not() {
        for path in [
            "/Users/ada/openpal/target/debug/openpal",
            "/Users/ada/openpal/target/debug/deps/openpal_app-1a2b3c",
            // Inside a bundle, but not the executable directory.
            "/Applications/OpenPal.app/Contents/Resources/llama/llama-server",
            // The directory names alone do not make a bundle.
            "/opt/openpal/Contents/MacOS/openpal",
            "/opt/.app/Contents/MacOS/openpal",
            "/Applications/OpenPal.app/openpal",
            "",
        ] {
            assert!(!is_inside_app_bundle(Path::new(path)), "{path:?}");
        }
    }
}
