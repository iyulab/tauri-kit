//! Whether a location has a trash the person can restore a file from.

use std::path::Path;

/// Whether a file at `path` can go to a trash the person restores it from.
///
/// On Windows a network share (`\\server\share`, or a drive letter mapped to one) and a removable
/// drive have no Recycle Bin, and asked to recycle a file there, Windows deletes it for good
/// without asking. Check this before moving a file to the trash, and ask the person before deleting
/// it instead. A path whose root the system cannot place is reported as having a trash, leaving
/// the trash itself to refuse it. Elsewhere this is always `true`: the platforms' trash
/// implementations refuse a location they cannot take rather than delete from it.
#[cfg(windows)]
pub fn has_trash(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Component;
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    use windows_sys::Win32::System::WindowsProgramming::{DRIVE_REMOTE, DRIVE_REMOVABLE};

    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return true;
    };
    let root: Vec<u16> = prefix
        .as_os_str()
        .encode_wide()
        .chain("\\".encode_utf16())
        .chain(Some(0))
        .collect();
    // SAFETY: `root` is a NUL-terminated wide string that outlives the call.
    let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
    !matches!(kind, DRIVE_REMOTE | DRIVE_REMOVABLE)
}

/// See the Windows version: elsewhere the trash itself refuses a location it cannot take.
#[cfg(not(windows))]
pub fn has_trash(_path: &Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_folder_has_a_trash() {
        let dir = tempfile::tempdir().unwrap();
        assert!(has_trash(dir.path()));
    }

    #[cfg(windows)]
    #[test]
    fn a_network_share_has_none() {
        // This PC's administrative share of its system drive: the same folder, reached over SMB.
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let share = format!(r"\\localhost\{}$\Windows", system.trim_end_matches(':'));
        if Path::new(&share).exists() {
            assert!(!has_trash(Path::new(&share)));
        }
    }
}
