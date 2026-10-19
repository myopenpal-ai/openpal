//! Windows: WinRT toasts through `tauri-winrt-notification`, under OpenPal's own
//! app id (AUMID), which is the bundle identifier.
//!
//! Windows shows a toast from an unpackaged app only if it knows the app's
//! AUMID; otherwise it drops the toast and `Show()` still succeeds, so nothing
//! can tell. An installer makes the AUMID known with a Start-menu shortcut, but
//! OpenPal ships on Windows only as a portable zip, which has none. So the
//! AUMID is registered under `HKEY_CURRENT_USER\Software\Classes\AppUserModelId`
//! (a display name is all a toast needs). That is the one trace a portable
//! build leaves on the host, and only once the user has turned Desktop
//! notifications on: `request_permission` (the switch turning on) and every
//! post write it, while reading the permission, as Settings does on every
//! visit, only looks.
//!
//! A click on a toast while it is on screen reaches `on_activated`, an
//! in-process event. A click on it later in the notification center would need
//! a COM activator registered with Windows, which OpenPal does not have, so
//! that one does not reach the app.

use super::{
    Clicks, Notification, NotificationClick, NotificationPermission, Notifier, NotifyError,
};
use tauri_winrt_notification::Toast;
use windows::core::HSTRING;
use windows::UI::Notifications::{NotificationSetting, ToastNotificationManager};
use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

/// Windows' own Settings page for notifications, where each app has its
/// switch.
const SETTINGS_URL: &str = "ms-settings:notifications";

/// The name Windows shows on OpenPal's toasts and in its Settings list.
const DISPLAY_NAME: &str = "OpenPal";

pub(super) struct WinToasts {
    app_id: String,
    clicks: Clicks,
}

impl WinToasts {
    pub(super) fn new(app_id: String, clicks: Clicks) -> Self {
        Self { app_id, clicks }
    }

    fn key_path(&self) -> String {
        format!(r"Software\Classes\AppUserModelId\{}", self.app_id)
    }

    fn is_registered(&self) -> bool {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(self.key_path())
            .is_ok()
    }

    /// Make the AUMID known to Windows. Idempotent: opening an existing key and
    /// writing the same name again is all a second call does.
    fn register(&self) -> Result<(), NotifyError> {
        let fail =
            |e: std::io::Error| NotifyError::Os(format!("couldn't register for toasts: {e}"));
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(self.key_path())
            .map_err(fail)?;
        key.set_value("DisplayName", &DISPLAY_NAME).map_err(fail)
    }

    /// The user's switch for OpenPal in Settings -> Notifications, or `None` if
    /// Windows would not say.
    fn setting(&self) -> Option<NotificationSetting> {
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(self.app_id.as_str()))
            .and_then(|notifier| notifier.Setting())
            .ok()
    }
}

/// Anything but "enabled" (switched off for OpenPal, for all apps, or by policy)
/// is blocked; no answer reads as allowed, the contract's fail-open.
fn permission_for_setting(setting: Option<NotificationSetting>) -> NotificationPermission {
    match setting {
        Some(setting) if setting != NotificationSetting::Enabled => NotificationPermission::Denied,
        _ => NotificationPermission::Granted,
    }
}

impl Notifier for WinToasts {
    fn supported(&self) -> bool {
        true
    }

    /// Until the AUMID is registered Windows has no switch for OpenPal to read,
    /// and registering it here would leave a trace for a user who never asked
    /// for notifications, so that reads as allowed.
    fn permission(&self) -> NotificationPermission {
        if !self.is_registered() {
            return NotificationPermission::Granted;
        }
        permission_for_setting(self.setting())
    }

    /// Windows has no prompt: asking is registering, then reading the switch.
    fn request_permission(&self) -> NotificationPermission {
        if let Err(e) = self.register() {
            tracing::warn!("{e}");
            return NotificationPermission::Granted;
        }
        permission_for_setting(self.setting())
    }

    fn open_settings(&self) -> Result<(), NotifyError> {
        crate::opener::open(SETTINGS_URL).map_err(NotifyError::Os)
    }

    /// The toast's text is set as XML inner text, which shows markup literally.
    fn renders_body_markup(&self) -> bool {
        false
    }

    fn post(&self, notification: &Notification) -> Result<(), NotifyError> {
        self.register()?;
        let clicks = self.clicks.clone();
        let conversation_id = notification.conversation_id.clone();
        Toast::new(&self.app_id)
            .title(&notification.title)
            .text1(&notification.body)
            .on_activated(move |_action| {
                clicks.report(NotificationClick {
                    conversation_id: conversation_id.clone(),
                });
                Ok(())
            })
            .show()
            .map_err(|e| NotifyError::Os(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_enabled_switch_or_no_answer_reads_as_allowed() {
        assert_eq!(
            permission_for_setting(Some(NotificationSetting::Enabled)),
            NotificationPermission::Granted
        );
        assert_eq!(
            permission_for_setting(None),
            NotificationPermission::Granted
        );
        for setting in [
            NotificationSetting::DisabledForApplication,
            NotificationSetting::DisabledForUser,
            NotificationSetting::DisabledByGroupPolicy,
            NotificationSetting::DisabledByManifest,
        ] {
            assert_eq!(
                permission_for_setting(Some(setting)),
                NotificationPermission::Denied,
                "{setting:?}"
            );
        }
    }

    #[test]
    fn reading_leaves_no_trace_and_asking_registers_the_app() {
        // A throwaway AUMID, removed again, so the test leaves nothing behind
        // and never touches the real app's entry.
        let app_id = format!("com.myopenpal.openpal.test-{}", std::process::id());
        let toasts = WinToasts::new(app_id, Clicks::new(|| {}));

        assert_eq!(toasts.permission(), NotificationPermission::Granted);
        assert!(!toasts.is_registered(), "reading must not register");

        crate::notify::assert_contract(&toasts);
        assert!(toasts.is_registered(), "asking registers");

        RegKey::predef(HKEY_CURRENT_USER)
            .delete_subkey_all(toasts.key_path())
            .unwrap();
    }
}
