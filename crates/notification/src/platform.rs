#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
pub(crate) use mac::*;

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub(crate) use win::*;

#[cfg(not(any(target_os = "macos", windows)))]
mod none {
    use crate::{Error, Notice, Permission};
    pub(crate) fn permission() -> Permission {
        Permission::Unavailable
    }
    pub(crate) fn request_permission() -> Permission {
        Permission::Unavailable
    }
    pub(crate) fn show(_: &Notice) -> Result<(), Error> {
        Err(Error::Unavailable)
    }
    pub(crate) fn on_activation(_: Box<dyn Fn(&str) + Send + Sync>) {}
}
#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) use none::*;

#[cfg(not(windows))]
pub(crate) fn set_app_user_model_id(_: String) {}
