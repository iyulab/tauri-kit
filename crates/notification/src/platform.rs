#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
pub(crate) use mac::*;

#[cfg(not(target_os = "macos"))]
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
#[cfg(not(target_os = "macos"))]
pub(crate) use none::*;
