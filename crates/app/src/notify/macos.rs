//! macOS: permission and delivery through `UNUserNotificationCenter`, and
//! nothing else.
//!
//! `tauri-plugin-notification` posts through the legacy `NSUserNotificationCenter`
//! (via `notify-rust`). `usernoted` does not let one bundle id be both kinds of
//! client: once any process of that bundle has asked the modern center anything
//! (even just "what are my settings?"), every legacy post from that bundle id is
//! refused with "You can't mix modern clients with legacy clients" and nothing
//! is shown. The mark outlives the process, so it is not enough to stop asking
//! later. A bundled OpenPal therefore uses the modern center for the permission
//! probe, the permission request and the posts, so the two can never disagree.
//! Outside a bundle (`tauri dev`, test binaries) the modern center cannot be
//! used at all ([`UserNotifications::new`] refuses) and `select` falls back to
//! the plugin's legacy path; that never runs under OpenPal's own bundle id (the
//! plugin posts as Terminal under `tauri dev`), so it cannot poison it.
//!
//! Every `Notifier` method here blocks on an OS callback: call them from a
//! blocking thread, never from the main thread or an async worker.
//!
//! A click on a banner reaches the delegate's `didReceiveNotificationResponse`,
//! which reads the conversation id `post` put in the banner's `userInfo` and
//! reports it to [`Clicks`]. The delegate is installed when the backend is
//! built, during `setup()`, because Apple only promises to deliver the click
//! that launched the app to a delegate set before launching has finished.

use super::{
    is_inside_app_bundle, macos_settings_url, Clicks, Notification, NotificationClick,
    NotificationPermission, Notifier, NotifyError,
};
use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread};
use objc2_foundation::{NSDictionary, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
    UNNotificationSettings, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Once, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long to wait for the OS to answer a settings read or a post before
/// giving up. The callbacks normally arrive in milliseconds.
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// How long to wait for the user to answer the system's permission prompt. It
/// stays on screen until clicked, so this is generous; if it runs out, the
/// answer is simply read later (Settings re-reads when the window is focused).
const PROMPT_TIMEOUT: Duration = Duration::from_secs(300);

/// The `userInfo` key a banner carries the conversation it opens under.
const CONVERSATION_KEY: &str = "conversationId";

/// Where the delegate reports clicks. Set once, by [`UserNotifications::new`]:
/// the delegate is an Objective-C object with no instance variables, so this is
/// how it reaches the rest of the app.
static CLICKS: OnceLock<Clicks> = OnceLock::new();

/// Whether the modern center may be used by this process at all.
///
/// `UNUserNotificationCenter` needs the process to belong to a bundle; outside
/// one it raises an `NSInternalInconsistencyException` ("bundleProxyForCurrentProcess
/// is nil") that aborts the process, so nothing here may run unless this says
/// yes. [`UserNotifications::new`] is the one place that asks.
fn available() -> bool {
    std::env::current_exe().is_ok_and(|exe| is_inside_app_bundle(&exe))
}

define_class!(
    /// Shows a banner even while OpenPal is the active app.
    ///
    /// Without a delegate the OS drops a notification that arrives for the
    /// frontmost app. "Not the focused window" is not "not the active app": a
    /// minimized window (Cmd+M) leaves OpenPal active with no key window, so
    /// the notification we are sent there would otherwise be swallowed.
    // SAFETY: NSObject has no subclassing requirements, and `Delegate` neither
    // has instance variables nor implements `Drop`.
    ///
    /// It also hears about clicks on OpenPal's banners (see the module docs).
    #[unsafe(super(NSObject))]
    #[name = "OpenPalNotificationDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        #[allow(non_snake_case)]
        fn userNotificationCenter_willPresentNotification_withCompletionHandler(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion_handler: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            tracing::debug!("showing a notification while OpenPal is the active app");
            completion_handler
                .call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List,));
        }

        // A banner OpenPal posted has no actions or custom dismiss, so the
        // only response the OS sends here is the click itself.
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        #[allow(non_snake_case)]
        fn userNotificationCenter_didReceiveNotificationResponse_withCompletionHandler(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion_handler: &DynBlock<dyn Fn()>,
        ) {
            let user_info = response.notification().request().content().userInfo();
            if let Some(clicks) = CLICKS.get() {
                clicks.report(NotificationClick {
                    conversation_id: conversation_in(&user_info),
                });
            }
            completion_handler.call(());
        }
    }
);

impl Delegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        // SAFETY: `init` is NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

/// The conversation a banner's `userInfo` names, if it names one.
fn conversation_in(user_info: &NSDictionary) -> Option<String> {
    let key: &AnyObject = &NSString::from_str(CONVERSATION_KEY);
    let value = user_info.objectForKey(key)?;
    value.downcast::<NSString>().ok().map(|id| id.to_string())
}

/// The `userInfo` for a banner that opens `conversation_id` when clicked.
fn user_info_for(conversation_id: &str) -> Retained<NSDictionary> {
    let key = NSString::from_str(CONVERSATION_KEY);
    let value = NSString::from_str(conversation_id);
    let typed = NSDictionary::<NSString, NSString>::from_slices(&[&*key], &[&*value]);
    // SAFETY: an `NSDictionary<NSString, NSString>` is an `NSDictionary`; the
    // key and object types exist only for the Rust type checker.
    unsafe { Retained::cast_unchecked(typed) }
}

/// The notification center, with [`Delegate`] installed on first use.
fn center() -> Retained<UNUserNotificationCenter> {
    static INSTALL_DELEGATE: Once = Once::new();
    let center = UNUserNotificationCenter::currentNotificationCenter();
    INSTALL_DELEGATE.call_once(|| {
        let delegate = Delegate::new();
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // The center holds its delegate weakly, so this one instance has to
        // outlive every use of the center: leak it, once.
        std::mem::forget(delegate);
    });
    center
}

/// The OS's current authorization status, or `None` if it did not answer in
/// time.
fn read_status() -> Option<UNAuthorizationStatus> {
    let (tx, rx) = mpsc::channel();
    let handler = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
        // SAFETY: the OS passes a live `UNNotificationSettings` that stays
        // valid for the duration of the callback, and we read one property.
        let status = unsafe { settings.as_ref() }.authorizationStatus();
        // The receiver is gone if the read already timed out.
        let _ = tx.send(status);
    });
    center().getNotificationSettingsWithCompletionHandler(&handler);
    rx.recv_timeout(REPLY_TIMEOUT).ok()
}

/// Only an explicit "Denied" counts as blocked, and only "NotDetermined" means
/// a request would still put the prompt up. Authorized, Provisional and
/// Ephemeral all let OpenPal post.
pub(super) fn permission_for_status(status: UNAuthorizationStatus) -> NotificationPermission {
    if status == UNAuthorizationStatus::Denied {
        NotificationPermission::Denied
    } else if status == UNAuthorizationStatus::NotDetermined {
        NotificationPermission::Prompt
    } else {
        NotificationPermission::Granted
    }
}

/// Request, wait for the answer, and read the status that results.
fn request_status(options: UNAuthorizationOptions) -> Option<UNAuthorizationStatus> {
    let (tx, rx) = mpsc::channel();
    let handler = RcBlock::new(move |_granted: Bool, _error: *mut NSError| {
        let _ = tx.send(());
    });
    center().requestAuthorizationWithOptions_completionHandler(options, &handler);
    // A timeout is fine: the status read below then reports whatever the
    // OS holds, which is "not determined" until the user answers.
    let _ = rx.recv_timeout(PROMPT_TIMEOUT);
    read_status()
}

/// Whether a post can be expected to show with this status. `None` (no answer)
/// tries anyway: a post to an unreachable center is dropped, which is no worse
/// than not trying.
fn may_post(status: Option<UNAuthorizationStatus>) -> bool {
    match status {
        Some(s) => {
            s == UNAuthorizationStatus::Authorized
                || s == UNAuthorizationStatus::Provisional
                || s == UNAuthorizationStatus::Ephemeral
        }
        None => true,
    }
}

/// A notification identifier no earlier one (this launch or a previous one)
/// can share, because a request with a reused identifier replaces the
/// notification that carried it instead of adding a new banner.
fn next_identifier() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("openpal-{nanos:x}-{}", SEQ.fetch_add(1, Ordering::Relaxed))
}

/// The backend for a bundled macOS app. Only [`UserNotifications::new`] makes
/// one, so holding one means the process is inside a bundle and the center is
/// safe to call.
pub(super) struct UserNotifications {
    /// The app's bundle id, for the settings deep link.
    identifier: String,
}

impl UserNotifications {
    /// `None` outside an app bundle, where touching the center would abort the
    /// process (see [`available`]). Installs the delegate at once, so a click
    /// that launched the app is not lost; that only hands the center an
    /// object, and waits on nothing.
    pub(super) fn new(identifier: String, clicks: Clicks) -> Option<Self> {
        if !available() {
            return None;
        }
        let _ = CLICKS.set(clicks);
        center();
        Some(Self { identifier })
    }
}

impl Notifier for UserNotifications {
    fn supported(&self) -> bool {
        true
    }

    /// Never prompts. No answer in time reads as `Granted`, so the worst case
    /// is a missing "blocked" line, never a false one.
    fn permission(&self) -> NotificationPermission {
        read_status()
            .map(permission_for_status)
            .unwrap_or(NotificationPermission::Granted)
    }

    /// This is what makes macOS show its "Allow OpenPal to send
    /// notifications?" prompt, once; after the first answer it returns at once
    /// with that answer. Blocks until the user has answered (up to
    /// [`PROMPT_TIMEOUT`]).
    fn request_permission(&self) -> NotificationPermission {
        request_status(UNAuthorizationOptions::Alert)
            .map(permission_for_status)
            .unwrap_or(NotificationPermission::Granted)
    }

    fn open_settings(&self) -> Result<(), NotifyError> {
        crate::opener::open(&macos_settings_url(&self.identifier)).map_err(NotifyError::Os)
    }

    /// The center shows the body as plain text.
    fn renders_body_markup(&self) -> bool {
        false
    }

    /// Post one banner. A delivery error the OS reports within
    /// [`REPLY_TIMEOUT`] is an `Err`; no answer in time is not.
    ///
    /// If the OS has not been asked for permission yet (a switch that was
    /// turned on before we asked, or an app that has never run the request),
    /// this asks now rather than silently dropping the first banner, so it can
    /// put the system prompt up.
    fn post(&self, notification: &Notification) -> Result<(), NotifyError> {
        let mut status = read_status();
        if status == Some(UNAuthorizationStatus::NotDetermined) {
            status = request_status(UNAuthorizationOptions::Alert);
        }
        if !may_post(status) {
            tracing::debug!("OS notification status is {status:?}");
            return Err(NotifyError::NotAllowed);
        }

        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&notification.title));
        content.setBody(&NSString::from_str(&notification.body));
        if let Some(id) = &notification.conversation_id {
            // SAFETY: the dictionary holds only `NSString`s, which are
            // property-list values, as `userInfo` requires.
            unsafe { content.setUserInfo(&user_info_for(id)) };
        }
        let identifier = NSString::from_str(&next_identifier());
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &identifier,
            &content,
            None,
        );

        let (tx, rx) = mpsc::channel();
        let handler = RcBlock::new(move |error: *mut NSError| {
            // SAFETY: the OS passes either null or a live `NSError` that stays
            // valid for the duration of the callback.
            let message = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string());
            let _ = tx.send(message);
        });
        center().addNotificationRequest_withCompletionHandler(&request, Some(&handler));
        match rx.recv_timeout(REPLY_TIMEOUT) {
            Ok(Some(message)) => Err(NotifyError::Os(message)),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_os_status_maps_to_one_permission() {
        assert_eq!(
            permission_for_status(UNAuthorizationStatus::Denied),
            NotificationPermission::Denied
        );
        assert_eq!(
            permission_for_status(UNAuthorizationStatus::NotDetermined),
            NotificationPermission::Prompt
        );
        for status in [
            UNAuthorizationStatus::Authorized,
            UNAuthorizationStatus::Provisional,
            UNAuthorizationStatus::Ephemeral,
        ] {
            assert_eq!(
                permission_for_status(status),
                NotificationPermission::Granted,
                "{status:?}"
            );
        }
    }

    #[test]
    fn a_post_is_attempted_unless_the_os_said_no() {
        assert!(may_post(Some(UNAuthorizationStatus::Authorized)));
        assert!(may_post(Some(UNAuthorizationStatus::Provisional)));
        assert!(may_post(Some(UNAuthorizationStatus::Ephemeral)));
        // The caller asks first when the status is NotDetermined; if that
        // still leaves it undetermined (prompt unanswered), nothing is shown.
        assert!(!may_post(Some(UNAuthorizationStatus::NotDetermined)));
        assert!(!may_post(Some(UNAuthorizationStatus::Denied)));
        assert!(may_post(None));
    }

    #[test]
    fn identifiers_never_repeat() {
        let a = next_identifier();
        let b = next_identifier();
        assert_ne!(a, b);
        assert!(a.starts_with("openpal-"));
    }

    #[test]
    fn a_test_binary_gets_no_backend_to_touch_the_os_with() {
        // The test binary lives in target/, not in a bundle: a backend here
        // would abort the whole test process on its first call.
        assert!(!available());
        let clicks = Clicks::new(|| {});
        assert!(UserNotifications::new("com.myopenpal.openpal".to_string(), clicks).is_none());
        assert!(CLICKS.get().is_none());
    }

    #[test]
    fn a_banner_carries_the_conversation_its_click_opens() {
        // Plain Foundation objects, not the center: safe outside a bundle.
        let info = user_info_for("convo-1");
        assert_eq!(conversation_in(&info).as_deref(), Some("convo-1"));
        let empty = NSDictionary::new();
        assert_eq!(conversation_in(&empty), None);
    }
}
