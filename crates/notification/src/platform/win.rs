//! Windows: the toast notifications of the shell. An app that is not packaged needs an application user
//! model id for its toasts: the one the app set for its process, or the one given to
//! [`set_app_user_model_id`](crate::set_app_user_model_id); without either every entry reports
//! [`Permission::Unavailable`]. Installing the id's display name and icon is the installer's job.

use std::sync::Mutex;

use windows::core::{Interface, HSTRING, PWSTR};
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::Win32::Foundation::{ERROR_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::Shell::GetCurrentProcessExplicitAppUserModelID;
use windows::UI::Notifications::{
    NotificationSetting, ToastActivatedEventArgs, ToastNotification, ToastNotificationManager,
};

use crate::{Error, Notice, Permission};

type Handler = Box<dyn Fn(&str) + Send + Sync>;

static HANDLER: Mutex<Option<Handler>> = Mutex::new(None);
static APP_ID: Mutex<Option<String>> = Mutex::new(None);

pub(crate) fn set_app_user_model_id(id: String) {
    if let Ok(mut slot) = APP_ID.lock() {
        *slot = Some(id);
    }
}

/// The calling thread joins the multithreaded apartment unless it already belongs to one; either outcome
/// is fine for the calls below.
fn join_apartment() {
    // SAFETY: plain initialisation of the calling thread; a failure means it is already initialised.
    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
}

fn app_id() -> Option<HSTRING> {
    if let Some(id) = APP_ID.lock().ok().and_then(|slot| slot.clone()) {
        return Some(HSTRING::from(id));
    }
    // SAFETY: the call returns a string allocated for the caller, freed below.
    let explicit: Result<PWSTR, _> = unsafe { GetCurrentProcessExplicitAppUserModelID() };
    let pwstr = explicit.ok()?;
    // SAFETY: a valid, null-terminated string until freed.
    let id = unsafe { pwstr.to_string() }.ok();
    unsafe { CoTaskMemFree(Some(pwstr.0 as *const _)) };
    id.filter(|id| !id.is_empty()).map(HSTRING::from)
}

pub(crate) fn permission() -> Permission {
    join_apartment();
    let Some(id) = app_id() else {
        return Permission::Unavailable;
    };
    let Ok(notifier) = ToastNotificationManager::CreateToastNotifierWithId(&id) else {
        return Permission::Unavailable;
    };
    match notifier.Setting() {
        Ok(NotificationSetting::Enabled) => Permission::Granted,
        // Turned off by the person, the administrator or the app's manifest: only the person can turn it
        // back on, in the system settings.
        Ok(_) => Permission::Denied,
        // An app that is not packaged, with its id registered only under the user's classes, gets «element not
        // found» here although its toasts show: the system keeps its setting where the Settings app writes it.
        Err(e) if e.code() == ERROR_NOT_FOUND.to_hresult() => registry_setting(&id.to_string()),
        Err(_) => Permission::Unavailable,
    }
}

const POLICY: &str = r"Software\Policies\Microsoft\Windows\CurrentVersion\PushNotifications";
const PUSH: &str = r"Software\Microsoft\Windows\CurrentVersion\PushNotifications";
const SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Notifications\Settings";

/// The toast setting as the Settings app and the administrator's policy leave it in the user's registry: off for
/// every app (`ToastEnabled` 0), off by policy (`NoToastApplicationNotification` 1), or off for this app
/// (`Enabled` 0 under its id). A value that is absent is the default, on.
fn registry_setting(id: &str) -> Permission {
    let off = dword(POLICY, "NoToastApplicationNotification") == Some(1)
        || dword(PUSH, "ToastEnabled") == Some(0)
        || dword(&format!(r"{SETTINGS}\{id}"), "Enabled") == Some(0);
    if off { Permission::Denied } else { Permission::Granted }
}

fn dword(key: &str, name: &str) -> Option<u32> {
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: the buffer is a u32 and its size is given; the strings live through the call.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            &HSTRING::from(name),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    (status == ERROR_SUCCESS).then_some(value)
}

/// The system asks no question on Windows: the setting is what it is.
pub(crate) fn request_permission() -> Permission {
    permission()
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

pub(crate) fn show(notice: &Notice) -> Result<(), Error> {
    match permission() {
        Permission::Granted => {}
        Permission::Unavailable => return Err(Error::Unavailable),
        other => return Err(Error::NotPermitted(other)),
    }
    let id = app_id().ok_or(Error::Unavailable)?;
    let system = |e: windows::core::Error| Error::System(e.message());
    let launch = notice
        .activation_id
        .as_deref()
        .map(|a| format!(" launch=\"{}\" activationType=\"foreground\"", escape(a)))
        .unwrap_or_default();
    let xml = format!(
        "<toast{launch}><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        escape(&notice.title),
        escape(&notice.body)
    );
    let doc = XmlDocument::new().map_err(system)?;
    doc.LoadXml(&HSTRING::from(xml)).map_err(system)?;
    let toast = ToastNotification::CreateToastNotification(&doc).map_err(system)?;
    if notice.activation_id.is_some() {
        toast
            .Activated(&TypedEventHandler::new(
                |_: windows::core::Ref<ToastNotification>,
                 args: windows::core::Ref<windows::core::IInspectable>| {
                    let launch: Option<String> = args
                        .as_ref()
                        .and_then(|a| a.cast::<ToastActivatedEventArgs>().ok())
                        .and_then(|a| a.Arguments().ok())
                        .map(|a| a.to_string());
                    if let (Some(launch), Ok(handler)) = (launch, HANDLER.lock()) {
                        if let Some(handler) = handler.as_ref() {
                            handler(&launch);
                        }
                    }
                    Ok(())
                },
            ))
            .map_err(system)?;
    }
    ToastNotificationManager::CreateToastNotifierWithId(&id)
        .and_then(|notifier| notifier.Show(&toast))
        .map_err(system)
}

pub(crate) fn on_activation(handler: Handler) {
    if let Ok(mut slot) = HANDLER.lock() {
        *slot = Some(handler);
    }
}
