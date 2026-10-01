use std::path::Path;

/// A directory name for `folder`'s state: the folder's own name, for recognising it by eye, and a
/// digest of its full path, so two folders with the same name do not share state.
///
/// The path is resolved first ([`Path::canonicalize`]), so every way of writing the same folder —
/// relative, through a link, with `..` — gives the same key; a folder that cannot be resolved (it
/// does not exist yet, or cannot be reached) is keyed by the path as given. On Windows and macOS,
/// whose file systems ignore case by default, case is ignored here too; elsewhere `Notes` and
/// `notes` are different folders and get different keys.
///
/// The key is part of this crate's contract: it does not change between releases, since an app
/// that found a different key would find no state.
///
/// ```
/// let key = tauri_kit_state::folder_key(std::path::Path::new("/nowhere/My Notes"));
/// assert!(key.starts_with("MyNotes-"));
/// assert_eq!(key.len(), "MyNotes-".len() + 16);
/// ```
pub fn folder_key(folder: &Path) -> String {
    let resolved = folder
        .canonicalize()
        .unwrap_or_else(|_| folder.to_path_buf());
    let text = resolved.to_string_lossy();
    let digest = fnv1a(if cfg!(any(windows, target_os = "macos")) {
        text.to_lowercase()
    } else {
        text.into_owned()
    });
    let readable: String = resolved
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .take(24)
        .collect();
    if readable.is_empty() {
        format!("{digest:016x}")
    } else {
        format!("{readable}-{digest:016x}")
    }
}

/// 64-bit FNV-1a. Not for security — only to tell paths apart in a name.
fn fnv1a(text: String) -> u64 {
    let mut digest: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        digest ^= u64::from(byte);
        digest = digest.wrapping_mul(0x0000_0100_0000_01b3);
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // These values are the contract: a key that changes strands every folder's state.
    #[test]
    fn a_key_does_not_change_between_releases() {
        #[cfg(windows)]
        assert_eq!(
            folder_key(Path::new(r"Z:\nowhere\Garden Notes")),
            "GardenNotes-7aaf906946e7a371"
        );
        #[cfg(not(windows))]
        let _ = folder_key(Path::new("/nowhere/Garden Notes"));
    }

    #[test]
    fn the_readable_part_keeps_letters_digits_dashes_and_underscores() {
        let key = folder_key(Path::new("/nowhere/my notes (2026)_v-1"));
        assert!(key.starts_with("mynotes2026_v-1-"), "{key}");
    }

    #[test]
    fn the_readable_part_is_at_most_24_characters() {
        let key = folder_key(Path::new("/nowhere/abcdefghijklmnopqrstuvwxyz"));
        assert!(key.starts_with("abcdefghijklmnopqrstuvwx-"), "{key}");
    }

    #[test]
    fn a_folder_with_no_usable_name_is_the_digest_alone() {
        let key = folder_key(Path::new("/nowhere/!!!"));
        assert_eq!(key.len(), 16, "{key}");
    }

    #[test]
    fn two_folders_with_the_same_name_get_different_keys() {
        assert_ne!(
            folder_key(Path::new("/one/notes")),
            folder_key(Path::new("/two/notes"))
        );
    }

    #[test]
    fn every_way_of_writing_an_existing_folder_gives_one_key() {
        let tmp = tempfile::tempdir().unwrap();
        let inner = tmp.path().join("inner");
        std::fs::create_dir(&inner).unwrap();
        let roundabout: PathBuf = inner.join("..").join("inner");
        assert_eq!(folder_key(&inner), folder_key(&roundabout));
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn case_is_ignored_where_the_file_system_ignores_it() {
        assert_eq!(
            folder_key(Path::new("/Nowhere/Notes")).rsplit('-').next(),
            folder_key(Path::new("/nowhere/notes")).rsplit('-').next()
        );
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn case_is_kept_where_the_file_system_keeps_it() {
        assert_ne!(
            folder_key(Path::new("/nowhere/Notes")),
            folder_key(Path::new("/nowhere/notes"))
        );
    }
}
