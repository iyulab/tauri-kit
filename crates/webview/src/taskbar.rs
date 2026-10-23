//! The window's identity on the Windows taskbar.
//!
//! When the person pins a running window, Windows pins the window's *relaunch command* — and a window that does not name one
//! is pinned as the process that drew it. For an app that runs from where an updater put it (a versioned folder that a later
//! update replaces), or that is meant to be started through a launcher of its own, that pin starts the wrong file: an old
//! version, or the app without what its launcher does first. The same properties decide which taskbar button the window
//! groups under (its AppUserModelID).
//!
//! [`set_taskbar_identity`] writes them on the window — `System.AppUserModel.ID` and, when given, `RelaunchCommand`,
//! `RelaunchDisplayNameResource` and `RelaunchIconResource` — through the window's property store, the way the shell reads
//! them. Windows uses a relaunch command only together with an ID and a display name, so [`Relaunch`] carries all three.
//! On macOS and Linux there is no such property and it does nothing.

use std::io;

/// What a pin of the window starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relaunch {
    /// The command line, as a shortcut would hold it — the program quoted when its path has spaces.
    pub command: String,
    /// The name the pinned item shows.
    pub display_name: String,
    /// The pinned item's icon, as `path,index` (`C:\App\app.ico,0`); `None` leaves the window's own.
    pub icon: Option<String>,
}

/// The window's taskbar identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskbarIdentity {
    /// The AppUserModelID the window groups under — the same one the app's shortcuts and process use.
    pub app_id: String,
    /// What a pin starts; `None` sets the ID alone.
    pub relaunch: Option<Relaunch>,
}

/// Writes `identity` on `window` (the top-level window's handle, as an integer). Call it once the window exists; a pin made
/// before it keeps what it pinned.
#[cfg(windows)]
pub fn set_taskbar_identity(window: isize, identity: &TaskbarIdentity) -> io::Result<()> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HWND, PROPERTYKEY};
    use windows::Win32::Storage::EnhancedStorage::{
        PKEY_AppUserModel_ID, PKEY_AppUserModel_RelaunchCommand,
        PKEY_AppUserModel_RelaunchDisplayNameResource, PKEY_AppUserModel_RelaunchIconResource,
    };
    use windows::Win32::System::Com::StructuredStorage::{
        PropVariantClear, PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
    };
    use windows::Win32::System::Variant::VT_LPWSTR;
    use windows::Win32::UI::Shell::PropertiesSystem::{
        IPropertyStore, SHGetPropertyStoreForWindow,
    };
    use windows::Win32::UI::Shell::SHStrDupW;

    if identity.app_id.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the AppUserModelID is empty",
        ));
    }
    let mut values: Vec<(PROPERTYKEY, &str)> =
        vec![(PKEY_AppUserModel_ID, identity.app_id.as_str())];
    if let Some(relaunch) = &identity.relaunch {
        if relaunch.command.is_empty() || relaunch.display_name.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a relaunch needs both a command and a display name",
            ));
        }
        values.push((PKEY_AppUserModel_RelaunchCommand, relaunch.command.as_str()));
        values.push((
            PKEY_AppUserModel_RelaunchDisplayNameResource,
            relaunch.display_name.as_str(),
        ));
        if let Some(icon) = &relaunch.icon {
            values.push((PKEY_AppUserModel_RelaunchIconResource, icon.as_str()));
        }
    }

    // A handle that is no longer a window (closed meanwhile) is refused here, not handed to the shell.
    if unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::IsWindow(
            window as windows_sys::Win32::Foundation::HWND,
        )
    } == 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the handle is not a window",
        ));
    }
    let to_io = |e: windows::core::Error| io::Error::other(e.message());
    // SAFETY: the handle was checked to be a window; each value is built the way `InitPropVariantFromString` builds one (a
    // string the COM allocator owns, so the store may treat it as any other variant) and cleared once the store has copied it.
    unsafe {
        let store: IPropertyStore =
            SHGetPropertyStoreForWindow(HWND(window as *mut core::ffi::c_void)).map_err(to_io)?;
        for (key, text) in values {
            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let owned = SHStrDupW(PCWSTR(wide.as_ptr())).map_err(to_io)?;
            let mut value = PROPVARIANT {
                Anonymous: PROPVARIANT_0 {
                    Anonymous: core::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                        vt: VT_LPWSTR,
                        Anonymous: PROPVARIANT_0_0_0 { pwszVal: owned },
                        ..Default::default()
                    }),
                },
            };
            let set = store.SetValue(&key, &value);
            let _ = PropVariantClear(&mut value);
            set.map_err(to_io)?;
        }
        store.Commit().map_err(to_io)
    }
}

/// Does nothing: the taskbar identity is a Windows property.
#[cfg(not(windows))]
pub fn set_taskbar_identity(_window: isize, _identity: &TaskbarIdentity) -> io::Result<()> {
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn an_empty_id_or_a_half_relaunch_is_refused_before_the_window_is_touched() {
        let no_id = TaskbarIdentity {
            app_id: String::new(),
            relaunch: None,
        };
        assert_eq!(
            set_taskbar_identity(0, &no_id).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let half = TaskbarIdentity {
            app_id: "Example.App".into(),
            relaunch: Some(Relaunch {
                command: "app.exe".into(),
                display_name: String::new(),
                icon: None,
            }),
        };
        assert_eq!(
            set_taskbar_identity(0, &half).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    fn read(window: isize, key: &windows::Win32::Foundation::PROPERTYKEY) -> Option<String> {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::System::Variant::VT_LPWSTR;
        use windows::Win32::UI::Shell::PropertiesSystem::{
            IPropertyStore, SHGetPropertyStoreForWindow,
        };
        unsafe {
            let store: IPropertyStore =
                SHGetPropertyStoreForWindow(HWND(window as *mut core::ffi::c_void)).ok()?;
            let mut value = store.GetValue(key).ok()?;
            let inner = &value.Anonymous.Anonymous;
            let text = (inner.vt == VT_LPWSTR)
                .then(|| inner.Anonymous.pwszVal.to_string().unwrap_or_default());
            let _ = windows::Win32::System::Com::StructuredStorage::PropVariantClear(&mut value);
            text
        }
    }

    #[test]
    fn a_real_window_carries_the_id_and_the_relaunch_the_shell_reads() {
        use windows::Win32::Storage::EnhancedStorage::{
            PKEY_AppUserModel_ID, PKEY_AppUserModel_RelaunchCommand,
            PKEY_AppUserModel_RelaunchDisplayNameResource, PKEY_AppUserModel_RelaunchIconResource,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, WS_OVERLAPPEDWINDOW,
        };
        let class: Vec<u16> = "STATIC".encode_utf16().chain(std::iter::once(0)).collect();
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                10,
                10,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!hwnd.is_null());
        let window = hwnd as isize;

        let identity = TaskbarIdentity {
            app_id: "Example.App".into(),
            relaunch: Some(Relaunch {
                command: r#""C:\Program Files\Example\launcher.exe""#.into(),
                display_name: "Example".into(),
                icon: Some(r"C:\Program Files\Example\app.ico,0".into()),
            }),
        };
        set_taskbar_identity(window, &identity).unwrap();
        assert_eq!(
            read(window, &PKEY_AppUserModel_ID).as_deref(),
            Some("Example.App")
        );
        assert_eq!(
            read(window, &PKEY_AppUserModel_RelaunchCommand).as_deref(),
            Some(r#""C:\Program Files\Example\launcher.exe""#)
        );
        assert_eq!(
            read(window, &PKEY_AppUserModel_RelaunchDisplayNameResource).as_deref(),
            Some("Example")
        );
        assert_eq!(
            read(window, &PKEY_AppUserModel_RelaunchIconResource).as_deref(),
            Some(r"C:\Program Files\Example\app.ico,0")
        );
        unsafe { DestroyWindow(hwnd) };
    }

    #[test]
    fn a_window_handle_that_is_not_a_window_is_an_error_not_a_crash() {
        let identity = TaskbarIdentity {
            app_id: "Example.App".into(),
            relaunch: None,
        };
        assert_eq!(
            set_taskbar_identity(0x7fff_fff0, &identity)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
