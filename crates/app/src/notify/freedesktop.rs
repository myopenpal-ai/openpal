//! Linux (and any other freedesktop desktop): the session's notification
//! server over D-Bus, through `notify-rust`.
//!
//! A click on the body is the spec's "default" action. Where the server
//! supports actions, each notification gets one and a thread of its own waits
//! for it; that wait also keeps the notification's D-Bus handle alive, without
//! which GNOME withdraws the notification as soon as it is posted. A server
//! without actions just shows it, and a click does whatever the server does.
//!
//! There is no per-app permission to read and no settings page every desktop
//! shares.

use super::{
    Clicks, Notification, NotificationClick, NotificationPermission, Notifier, NotifyError,
};
use std::sync::OnceLock;

/// The app name the server shows, and the label of the default action: most
/// servers never show that one (the click is on the notification itself), and
/// the few that show it as a button get the app's name, which needs no
/// translation.
const APP_NAME: &str = "OpenPal";

pub(super) struct Freedesktop {
    clicks: Clicks,
}

impl Freedesktop {
    pub(super) fn new(clicks: Clicks) -> Self {
        Self { clicks }
    }
}

/// Whether the notification server reports actions, asked once per process. No
/// answer (no server running yet) reads as no.
fn server_has_actions() -> bool {
    static ACTIONS: OnceLock<bool> = OnceLock::new();
    *ACTIONS.get_or_init(|| {
        notify_rust::get_capabilities().is_ok_and(|caps| caps.iter().any(|cap| cap == "actions"))
    })
}

impl Notifier for Freedesktop {
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

    /// The spec lets a server render `<b>`, `<a>` and friends in the body (see
    /// `content::escape_markup`).
    fn renders_body_markup(&self) -> bool {
        true
    }

    fn post(&self, notification: &Notification) -> Result<(), NotifyError> {
        let actions = server_has_actions();
        let mut message = notify_rust::Notification::new();
        message
            .appname(APP_NAME)
            .summary(&notification.title)
            .body(&notification.body)
            .auto_icon();
        if actions {
            message.action("default", APP_NAME);
        }
        let handle = message.show().map_err(|e| NotifyError::Os(e.to_string()))?;
        if !actions {
            return Ok(());
        }

        // Blocks until the notification is clicked or closed, which for one
        // left in the tray can be hours: a thread of its own, not the shared
        // blocking pool.
        let clicks = self.clicks.clone();
        let conversation_id = notification.conversation_id.clone();
        std::thread::Builder::new()
            .name("openpal-notification".into())
            .spawn(move || {
                handle.wait_for_action(|action| {
                    if action == "default" {
                        clicks.report(NotificationClick { conversation_id });
                    }
                });
            })
            .map(|_| ())
            .map_err(|e| NotifyError::Os(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_freedesktop_backend_reports_what_it_can_do() {
        // Everything but `post`, which would put a real notification on the
        // screen of whoever runs the tests.
        let backend = Freedesktop::new(Clicks::new(|| {}));
        crate::notify::assert_contract(&backend);
        assert!(backend.renders_body_markup());
        assert_eq!(backend.open_settings(), Err(NotifyError::Unsupported));
    }
}
