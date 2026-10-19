//! macOS outside an app bundle (`tauri dev`, `cargo run`), where the modern
//! center cannot be used at all (see [`super::macos`]): posts through the
//! legacy `NSUserNotificationCenter`, via `notify-rust`, as Terminal.
//!
//! Never as OpenPal's own bundle id: once any process posts for that id as a
//! legacy client, `usernoted` refuses the bundled app's modern posts for good.
//! A development convenience only, so there is no permission to read, no
//! settings page, and no click handling.

use super::{Notification, NotificationPermission, Notifier, NotifyError};
use std::sync::Once;

/// Who the legacy center is told is posting.
const POSTING_AS: &str = "com.apple.Terminal";

pub(super) struct LegacyCenter;

impl Notifier for LegacyCenter {
    fn supported(&self) -> bool {
        true
    }

    fn permission(&self) -> NotificationPermission {
        NotificationPermission::Granted
    }

    fn request_permission(&self) -> NotificationPermission {
        NotificationPermission::Granted
    }

    fn open_settings(&self) -> Result<(), NotifyError> {
        Err(NotifyError::Unsupported)
    }

    fn renders_body_markup(&self) -> bool {
        false
    }

    fn post(&self, notification: &Notification) -> Result<(), NotifyError> {
        // On the first post, not at construction: it changes which bundle the
        // whole process posts as, which nothing but a post should do.
        static POSTING_AS_TERMINAL: Once = Once::new();
        POSTING_AS_TERMINAL.call_once(|| {
            let _ = notify_rust::set_application(POSTING_AS);
        });
        notify_rust::Notification::new()
            .summary(&notification.title)
            .body(&notification.body)
            .show()
            .map(|_| ())
            .map_err(|e| NotifyError::Os(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dev_backend_has_nothing_to_ask_and_no_page_to_open() {
        crate::notify::assert_contract(&LegacyCenter);
        assert_eq!(LegacyCenter.open_settings(), Err(NotifyError::Unsupported));
    }
}
