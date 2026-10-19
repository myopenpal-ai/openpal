//! Android: posts, the POST_NOTIFICATIONS permission, the notification
//! settings page and taps, through `NotifyPlugin.kt` (a Tauri mobile plugin
//! with the same shape as `secret_store_android.rs`).
//!
//! `run_mobile_plugin` blocks until Kotlin answers and has no timeout, so every
//! Kotlin command answers on every path. Kotlin runs its commands on the
//! Android UI thread, which is why nothing here may be called from that thread:
//! [`super::Notifications`] calls these from a blocking thread, and the tap
//! channel's callback hands its report off instead of calling back in.
//!
//! A tap comes back on a `Channel` registered once, when the backend is built;
//! Kotlin holds a tap that arrives before that (the one that launched the app)
//! until then.

use super::{
    android_permission, Clicks, Notification, NotificationClick, NotificationPermission, Notifier,
    NotifyError,
};
use std::sync::OnceLock;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::Wry;

static HANDLE: OnceLock<PluginHandle<Wry>> = OnceLock::new();

/// Registered in `lib.rs`'s builder chain: a plugin's own `setup()` runs during
/// `.build()`, so the handle is there by the time `notify::select` runs.
pub fn plugin_init() -> TauriPlugin<Wry> {
    Builder::new("openpal-notify")
        .setup(|_app, api| {
            let handle = api.register_android_plugin("com.myopenpal.openpal", "NotifyPlugin")?;
            let _ = HANDLE.set(handle);
            Ok(())
        })
        .build()
}

#[derive(serde::Serialize)]
struct WatchTaps {
    channel: Channel,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Tap {
    conversation_id: Option<String>,
}

#[derive(serde::Deserialize)]
struct PermissionAnswer {
    state: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Post<'a> {
    title: &'a str,
    body: &'a str,
    conversation_id: Option<&'a str>,
}

pub(super) struct AndroidNotifier {
    handle: &'static PluginHandle<Wry>,
}

impl AndroidNotifier {
    /// `None` if `NotifyPlugin` never registered. Starts listening for taps.
    pub(super) fn new(clicks: Clicks) -> Option<Self> {
        let handle = HANDLE.get()?;
        let channel = Channel::new(move |body| {
            if let InvokeResponseBody::Json(json) = body {
                match serde_json::from_str::<Tap>(&json) {
                    Ok(tap) => {
                        // This runs on the Android UI thread: report from a
                        // task, so raising and emitting never happen here.
                        let clicks = clicks.clone();
                        tauri::async_runtime::spawn(async move {
                            clicks.report(NotificationClick {
                                conversation_id: tap.conversation_id,
                            });
                        });
                    }
                    Err(e) => tracing::warn!("unreadable notification tap: {e}"),
                }
            }
            Ok(())
        });
        if let Err(e) =
            handle.run_mobile_plugin::<serde_json::Value>("watchTaps", WatchTaps { channel })
        {
            tracing::warn!("couldn't listen for notification taps: {e}");
        }
        Some(Self { handle })
    }

    /// Ask Kotlin for a permission answer; one it could not give is the
    /// contract's fail-open `Granted`.
    fn permission_from(&self, command: &str) -> NotificationPermission {
        match self
            .handle
            .run_mobile_plugin::<PermissionAnswer>(command, serde_json::json!({}))
        {
            Ok(answer) => android_permission(&answer.state),
            Err(e) => {
                tracing::warn!("notification permission ({command}) failed: {e}");
                NotificationPermission::Granted
            }
        }
    }
}

impl Notifier for AndroidNotifier {
    fn supported(&self) -> bool {
        true
    }

    fn permission(&self) -> NotificationPermission {
        self.permission_from("permissionState")
    }

    /// Shows Android's "Allow OpenPal to send you notifications?" prompt where
    /// it may still be shown, and waits for the answer.
    fn request_permission(&self) -> NotificationPermission {
        self.permission_from("askPermission")
    }

    fn open_settings(&self) -> Result<(), NotifyError> {
        self.handle
            .run_mobile_plugin::<serde_json::Value>("openSettings", serde_json::json!({}))
            .map(|_| ())
            .map_err(|e| NotifyError::Os(e.to_string()))
    }

    fn renders_body_markup(&self) -> bool {
        false
    }

    fn post(&self, notification: &Notification) -> Result<(), NotifyError> {
        let post = Post {
            title: &notification.title,
            body: &notification.body,
            conversation_id: notification.conversation_id.as_deref(),
        };
        self.handle
            .run_mobile_plugin::<serde_json::Value>("post", post)
            .map(|_| ())
            .map_err(|e| match e.to_string().as_str() {
                "NOT_ALLOWED" => NotifyError::NotAllowed,
                other => NotifyError::Os(other.to_string()),
            })
    }
}
