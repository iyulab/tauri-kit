//! Recognising the copies sync clients make when two devices changed the same file.
//!
//! A sync client that finds a file changed on two devices keeps both: one under the original name,
//! the other renamed to say it is a conflicting copy. An app that lists a synced folder sees that
//! second file as just another file — and, because the marker goes in front of the *last*
//! extension, a file named with a compound suffix such as `notes.draft.md` becomes
//! `notes.draft (… conflicted copy …).md`, which no longer ends in `.draft.md` and drops out of a
//! listing filtered by it. [`conflict_copy_of`] reads the name and says which file it is a copy
//! of, so the app can show both and leave the choice to the person.
//!
//! Recognised names:
//!
//! | Client | Copy of `a.txt` |
//! |---|---|
//! | Syncthing | `a.sync-conflict-20260929-143015-ABCDEFG.txt` |
//! | Dropbox | `a (Kim's conflicted copy 2026-09-29).txt`, in Korean `a (Kim의 충돌된 사본 2026-09-29).txt` |
//! | Nextcloud, ownCloud | `a (conflicted copy 2026-09-29 143015).txt` |
//! | Google Drive | `a (conflicted copy from Laptop on 2026-09-29).txt` |
//!
//! OneDrive keeps the other side as `a-<device>.txt`, the device being the computer whose change
//! lost. By its name alone that cannot be told apart from a name a person chose (`회의-서울.md`), so
//! [`conflict_copy_of`] leaves it out; [`conflict_copy_of_on`] takes the names of the devices that
//! sync the folder and recognises it for those. Copies of copies (`a (conflicted copy …) (1).txt`)
//! are not recognised.

/// The name of the file that `file_name` is a sync client's conflict copy of, or `None` when the
/// name does not mark a conflict copy.
///
/// Takes a file name, not a path, and returns a file name in the same folder.
///
/// ```
/// use tauri_kit_fs::conflict_copy_of;
///
/// assert_eq!(
///     conflict_copy_of("notes.draft (Kim's conflicted copy 2026-09-29).md").as_deref(),
///     Some("notes.draft.md"),
/// );
/// assert_eq!(conflict_copy_of("notes.draft.md"), None);
/// ```
pub fn conflict_copy_of(file_name: &str) -> Option<String> {
    let (stem, ext) = split_extension(file_name);
    syncthing(stem)
        .or_else(|| parenthesized(stem))
        .map(|original| format!("{original}{ext}"))
        // A name with no extension, where the last dot belongs to the Syncthing marker itself.
        .or_else(|| syncthing(file_name).map(str::to_owned))
}

/// Like [`conflict_copy_of`], and also OneDrive's copy — `<original>-<device>` before the last
/// extension — for a device named in `devices`: the names of the computers that sync the folder,
/// as OneDrive writes them (compared without regard to ASCII case). The device a copy names is the
/// one whose change lost, so this computer's own name recognises the copies made here; a copy made
/// on another computer is recognised once its name is known.
///
/// A name like that is still one a person could have chosen: give it only device names, and check
/// that the original is there before treating the file as a copy.
///
/// ```
/// use tauri_kit_fs::conflict_copy_of_on;
///
/// assert_eq!(conflict_copy_of_on("접수-1-DESKTOP-7Q2.md", &["desktop-7q2"]).as_deref(), Some("접수-1.md"));
/// assert_eq!(conflict_copy_of_on("회의-서울.md", &["desktop-7q2"]), None);
/// ```
pub fn conflict_copy_of_on(file_name: &str, devices: &[&str]) -> Option<String> {
    conflict_copy_of(file_name).or_else(|| {
        let (stem, ext) = split_extension(file_name);
        devices
            .iter()
            .map(|d| d.trim())
            .filter(|d| !d.is_empty())
            .find_map(|device| {
                let marker = stem.len().checked_sub(device.len() + 1)?;
                let original = stem.get(..marker)?;
                let named = stem.get(marker + 1..)?;
                (!original.is_empty()
                    && stem[marker..].starts_with('-')
                    && named.eq_ignore_ascii_case(device))
                .then(|| format!("{original}{ext}"))
            })
    })
}

/// Splits off the last extension, dot included. A leading dot (`.profile`) is not an extension.
pub(crate) fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    }
}

const SYNCTHING_MARKER: &str = ".sync-conflict-";

/// `<original>.sync-conflict-<YYYYMMDD>-<HHMMSS>[-<device>]`
fn syncthing(stem: &str) -> Option<&str> {
    let at = stem.rfind(SYNCTHING_MARKER)?;
    let (original, rest) = (&stem[..at], &stem[at + SYNCTHING_MARKER.len()..]);
    let mut parts = rest.split('-');
    let date = parts.next()?;
    let time = parts.next()?;
    let device = parts.next();
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    let device_ok =
        device.is_none_or(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_alphanumeric()));
    (!original.is_empty()
        && digits(date, 8)
        && digits(time, 6)
        && device_ok
        && parts.next().is_none())
    .then_some(original)
}

/// The words a client puts inside the parentheses. Compared without regard to ASCII case.
const PARENTHESIZED_MARKERS: &[&str] = &["conflicted copy", "충돌된 사본"];

/// `<original> (<… marker …>)`
fn parenthesized(stem: &str) -> Option<&str> {
    let inner = stem.strip_suffix(')')?;
    let open = inner.rfind(" (")?;
    let (original, note) = (&inner[..open], &inner[open + 2..]);
    if original.is_empty() || note.contains(['(', ')']) {
        return None;
    }
    let note = note.to_lowercase();
    PARENTHESIZED_MARKERS
        .iter()
        .any(|marker| note.contains(marker))
        .then_some(original)
}

#[cfg(test)]
mod tests {
    use super::{conflict_copy_of, conflict_copy_of_on};

    #[test]
    fn onedrive_copies_of_named_devices() {
        let devices = ["desk-02", "Laptop-3"];
        // OneDrive names the copy after the computer whose change lost, as it spells it.
        assert_eq!(
            conflict_copy_of_on("접수-1-desk-02.md", &devices).as_deref(),
            Some("접수-1.md")
        );
        assert_eq!(
            conflict_copy_of_on("접수-1-LAPTOP-3.md", &devices).as_deref(),
            Some("접수-1.md"),
            "a device name is compared without regard to case"
        );
        assert_eq!(
            conflict_copy_of_on("회의.fd-desk-02.md", &devices).as_deref(),
            Some("회의.fd.md"),
            "the marker goes before the last extension"
        );
        // A name that only looks like one: another device, no device at all, or nothing before it.
        assert_eq!(conflict_copy_of_on("회의-서울.md", &devices), None);
        assert_eq!(conflict_copy_of_on("접수-1.md", &devices), None);
        assert_eq!(conflict_copy_of_on("-desk-02.md", &devices), None);
        assert_eq!(conflict_copy_of_on("접수-1-desk-02.md", &[]), None);
        assert_eq!(conflict_copy_of_on("접수-1-desk-02.md", &[" "]), None);
        // The other clients' copies are recognised as they are without devices.
        assert_eq!(
            conflict_copy_of_on("a (conflicted copy 2026-09-29 143015).txt", &devices).as_deref(),
            Some("a.txt")
        );
    }

    fn copy_of(name: &str) -> Option<String> {
        conflict_copy_of(name)
    }

    #[test]
    fn syncthing_copies() {
        // Format from the Syncthing documentation, "Conflicting Changes":
        // <filename>.sync-conflict-<date>-<time>-<modifiedBy>.<ext>
        assert_eq!(
            copy_of("a.sync-conflict-20260929-143015-ABCDEFG.txt").as_deref(),
            Some("a.txt")
        );
        assert_eq!(
            copy_of("a.b.sync-conflict-20260929-143015-ABCDEFG.md").as_deref(),
            Some("a.b.md")
        );
        // Older releases leave out the device.
        assert_eq!(
            copy_of("a.sync-conflict-20260929-143015.txt").as_deref(),
            Some("a.txt")
        );
        // No extension: the last dot is the marker's own.
        assert_eq!(
            copy_of("Makefile.sync-conflict-20260929-143015-ABCDEFG").as_deref(),
            Some("Makefile")
        );
    }

    #[test]
    fn dropbox_copies() {
        assert_eq!(
            copy_of("a (Kim's conflicted copy 2026-09-29).txt").as_deref(),
            Some("a.txt")
        );
        // The Korean client writes 충돌된 사본 — not 충돌 사본.
        assert_eq!(
            copy_of("보고서 (김의 충돌된 사본 2026-09-29).md").as_deref(),
            Some("보고서.md")
        );
        assert_eq!(
            copy_of("회의.fd (김의 충돌된 사본 2026-09-29).md").as_deref(),
            Some("회의.fd.md"),
            "the marker goes before the last extension, so a compound suffix is split around it",
        );
    }

    #[test]
    fn nextcloud_and_google_drive_copies() {
        assert_eq!(
            copy_of("test (conflicted copy 2022-02-03 084856).txt").as_deref(),
            Some("test.txt")
        );
        assert_eq!(
            copy_of("a (Conflicted Copy 2022-02-03 084856).txt").as_deref(),
            Some("a.txt")
        );
        assert_eq!(
            copy_of("a (conflicted copy from Laptop on 2026-09-29).txt").as_deref(),
            Some("a.txt")
        );
    }

    #[test]
    fn names_a_person_chose_are_not_copies() {
        for name in [
            "a.txt",
            "a.fd.md",
            "충돌 해결 방안.md",
            "a (draft).txt",
            "a (1).txt",
            "notes about a conflicted copy.txt",
            "a-LAPTOP.txt",
            "(conflicted copy).txt",
            ".sync-conflict-20260929-143015-ABCDEFG.txt",
            "a.sync-conflict-2026-143015.txt",
            "a.sync-conflict-20260929-143015-AB-CD.txt",
            "a (conflicted copy 2026-09-29) (1).txt",
            ".profile",
        ] {
            assert_eq!(copy_of(name), None, "{name}");
        }
    }
}
