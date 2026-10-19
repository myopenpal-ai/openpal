//! iOS: no OS notifications yet (Android has `NotifyPlugin.kt`, and falls back
//! here only if that plugin failed to register). `get_app_info` reports this
//! backend as unsupported, which is what makes the UI leave the switch out.

use super::{Notification, NotificationPermission, Notifier, NotifyError};

/// Compiled everywhere so the CI host runs its tests; selected only on mobile.
#[cfg_attr(not(any(target_os = "android", target_os = "ios")), allow(dead_code))]
pub(super) struct Unsupported;

impl Notifier for Unsupported {
    fn supported(&self) -> bool {
        false
    }

    fn permission(&self) -> NotificationPermission {
        NotificationPermission::Unsupported
    }

    fn request_permission(&self) -> NotificationPermission {
        NotificationPermission::Unsupported
    }

    fn open_settings(&self) -> Result<(), NotifyError> {
        Err(NotifyError::Unsupported)
    }

    fn renders_body_markup(&self) -> bool {
        false
    }

    fn post(&self, _notification: &Notification) -> Result<(), NotifyError> {
        Err(NotifyError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unsupported_build_says_so_rather_than_blocked() {
        // "Denied" would put up a "blocked" line with a button to a settings
        // page that does not exist.
        let notifier = Unsupported;
        crate::notify::assert_contract(&notifier);
        assert!(!notifier.supported());
        assert_eq!(notifier.permission(), NotificationPermission::Unsupported);
        assert_eq!(
            notifier.request_permission(),
            NotificationPermission::Unsupported
        );
        assert_eq!(notifier.open_settings(), Err(NotifyError::Unsupported));
        let notification = Notification {
            title: "t".into(),
            body: "b".into(),
            conversation_id: None,
        };
        assert_eq!(notifier.post(&notification), Err(NotifyError::Unsupported));
    }
}
