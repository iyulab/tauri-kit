//! macOS: the user-notification centre. It needs a real app bundle — started from a terminal the process
//! has no bundle identifier and the framework raises an exception instead of answering — so every entry
//! checks for the bundle first and reports [`Permission::Unavailable`] without touching the framework.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread};
use objc2_foundation::{NSBundle, NSDictionary, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification,
    UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNNotificationSettings,
    UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

use crate::{Error, Notice, Permission};

/// How long a call into the framework may take to answer before this crate gives up on it.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// The key the activation id is stored under in the notice, for the click report.
pub(crate) const ACTIVATION_KEY: &str = "tauri-kit-notification.activation-id";

fn is_bundled() -> bool {
    let bundle = NSBundle::mainBundle();
    bundle.bundleIdentifier().is_some() && bundle.bundlePath().to_string().ends_with(".app")
}

fn status() -> Option<UNAuthorizationStatus> {
    if !is_bundled() {
        return None;
    }
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let (tx, rx) = mpsc::channel();
    let handler = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
        // SAFETY: the framework passes a live settings object for the duration of the call.
        let _ = tx.send(unsafe { settings.as_ref() }.authorizationStatus());
    });
    center.getNotificationSettingsWithCompletionHandler(&handler);
    rx.recv_timeout(ANSWER_TIMEOUT).ok()
}

fn map(status: UNAuthorizationStatus) -> Permission {
    match status {
        UNAuthorizationStatus::NotDetermined => Permission::NotDetermined,
        UNAuthorizationStatus::Denied => Permission::Denied,
        // Provisional and ephemeral grants deliver (quietly, or for a while): the app may show a notice.
        UNAuthorizationStatus::Authorized
        | UNAuthorizationStatus::Provisional
        | UNAuthorizationStatus::Ephemeral => Permission::Granted,
        _ => Permission::Unavailable,
    }
}

pub(crate) fn permission() -> Permission {
    status().map_or(Permission::Unavailable, map)
}

pub(crate) fn request_permission() -> Permission {
    if permission() != Permission::NotDetermined {
        return permission();
    }
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let (tx, rx) = mpsc::channel();
    let handler = RcBlock::new(move |_granted: Bool, _error: *mut NSError| {
        let _ = tx.send(());
    });
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
        &handler,
    );
    // The answer is read back from the settings: it is the single source of what the system now holds.
    let _ = rx.recv();
    permission()
}

pub(crate) fn show(notice: &Notice) -> Result<(), Error> {
    match permission() {
        Permission::Granted => {}
        Permission::Unavailable => return Err(Error::Unavailable),
        other => return Err(Error::NotPermitted(other)),
    }
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(&notice.title));
    content.setBody(&NSString::from_str(&notice.body));
    if let Some(id) = &notice.activation_id {
        let key = NSString::from_str(ACTIVATION_KEY);
        let value = NSString::from_str(id);
        let info: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::from_retained_objects(
            &[&*key],
            &[Retained::into_super(Retained::into_super(value))],
        );
        // SAFETY: a property list (string key, string value), as the framework requires of user info; the
        // key type only narrows the generic parameter of the same object.
        unsafe {
            content.setUserInfo(
                &*(Retained::as_ptr(&info) as *const NSDictionary<AnyObject, AnyObject>),
            )
        };
    }
    let identifier = NSString::from_str(&format!(
        "tauri-kit-notification.{}",
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let request =
        UNNotificationRequest::requestWithIdentifier_content_trigger(&identifier, &content, None);
    let (tx, rx) = mpsc::channel();
    let handler = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: the framework passes null or a live error for the duration of the call.
        let message = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string());
        let _ = tx.send(message);
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .addNotificationRequest_withCompletionHandler(&request, Some(&handler));
    match rx.recv_timeout(ANSWER_TIMEOUT) {
        Ok(None) => Ok(()),
        Ok(Some(message)) => Err(Error::System(message)),
        Err(_) => Err(Error::System(
            "the notification centre did not answer".into(),
        )),
    }
}

type Handler = Box<dyn Fn(&str) + Send + Sync>;

static HANDLER: Mutex<Option<Handler>> = Mutex::new(None);

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the class holds no state of its own.
    #[unsafe(super(NSObject))]
    #[name = "TauriKitNotificationDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        // A notice the app asked to show is shown even while its window is in front: the app decides when
        // to call `show`, the system's default would swallow it.
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::Sound,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &block2::DynBlock<dyn Fn()>,
        ) {
            // SAFETY: a constant string the framework defines.
            let default_action = unsafe { UNNotificationDefaultActionIdentifier };
            if response.actionIdentifier().isEqualToString(default_action) {
                let info = response.notification().request().content().userInfo();
                let key = NSString::from_str(ACTIVATION_KEY);
                // The user info is the property list `show` put there.
                let id = info
                    .objectForKey(&key)
                    .and_then(|v| v.downcast::<NSString>().ok())
                    .map(|v| v.to_string());
                if let (Some(id), Ok(handler)) = (id, HANDLER.lock()) {
                    if let Some(handler) = handler.as_ref() {
                        handler(&id);
                    }
                }
            }
            completion.call(());
        }
    }
);

/// The one delegate object; the notification centre holds its delegate weakly, so it is kept here.
#[allow(dead_code)] // held only to keep the delegate alive
struct Kept(Retained<Delegate>);
// SAFETY: the delegate keeps no state and its methods only read the notice they are given and the
// handler behind its mutex.
unsafe impl Send for Kept {}
unsafe impl Sync for Kept {}

static DELEGATE: OnceLock<Kept> = OnceLock::new();

pub(crate) fn on_activation(handler: Handler) {
    if !is_bundled() {
        return;
    }
    if let Ok(mut slot) = HANDLER.lock() {
        *slot = Some(handler);
    }
    let kept = DELEGATE.get_or_init(|| {
        let delegate: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(), init] };
        let center = UNUserNotificationCenter::currentNotificationCenter();
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        Kept(delegate)
    });
    let _ = kept;
}
