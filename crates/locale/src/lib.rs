//! The UI languages the person prefers, before any window exists.
//!
//! An app that speaks more than one language has to choose its words before the web view can tell it
//! anything: the message shown when the web view runtime is missing, a first dialog, the menu, the
//! locale the page starts in. [`preferred_languages`] asks the operating system — the order the person
//! set, not only the first — and [`pick`] chooses among the languages the app ships. The words
//! themselves, and the page's own translation, are the app's.
//!
//! ```
//! let preferred = tauri_kit_locale::preferred_languages();
//! let language = tauri_kit_locale::pick(&preferred, &["en", "ko", "zh-Hant"]).unwrap_or("en");
//! assert!(["en", "ko", "zh-Hant"].contains(&language));
//! ```
//!
//! Where the list comes from:
//! - **Windows** — the user's preferred UI languages (`GetUserPreferredUILanguages`), in their order.
//! - **macOS** — the preferred languages of the person's settings (`CFLocaleCopyPreferredLanguages`).
//! - **Linux and other Unix** — `LANGUAGE` (a colon-separated list), then `LC_ALL`, `LC_MESSAGES` and
//!   `LANG`, the order the C library reads them in; `C` and `POSIX` name no language.

/// The UI languages the person prefers, most preferred first, as BCP 47 tags (`ko-KR`, `zh-Hant-TW`,
/// `en`). Empty when the system names none.
pub fn preferred_languages() -> Vec<String> {
    let mut tags = Vec::new();
    for tag in platform::preferred()
        .into_iter()
        .filter_map(|raw| normalize(&raw))
    {
        if !tags.iter().any(|t: &String| t.eq_ignore_ascii_case(&tag)) {
            tags.push(tag);
        }
    }
    tags
}

/// The language from `available` that best fits `preferred` (most preferred first), or `None` when
/// none fits — the app then uses its default.
///
/// For each preferred language in turn: the available language that is the same tag, or the tag
/// with its last parts dropped (`zh-Hant-TW` → `zh-Hant` → `zh`, RFC 4647 lookup); then one that
/// differs only after the language (`ko` fits `ko-KR`). Case does not matter.
pub fn pick<'a, S: AsRef<str>>(preferred: &[S], available: &[&'a str]) -> Option<&'a str> {
    for wanted in preferred {
        let wanted = wanted.as_ref();
        let mut prefix = wanted;
        loop {
            if let Some(found) = available.iter().find(|a| a.eq_ignore_ascii_case(prefix)) {
                return Some(found);
            }
            match prefix.rfind('-') {
                Some(at) => prefix = &prefix[..at],
                None => break,
            }
        }
        let language = primary(wanted);
        if let Some(found) = available
            .iter()
            .find(|a| primary(a).eq_ignore_ascii_case(language))
        {
            return Some(found);
        }
    }
    None
}

fn primary(tag: &str) -> &str {
    tag.split('-').next().unwrap_or(tag)
}

/// A BCP 47 tag from what a platform hands over: `ko_KR.UTF-8@euro` → `ko-KR`. `None` for no language
/// (`C`, `POSIX`, empty) or for something that is not a tag.
fn normalize(raw: &str) -> Option<String> {
    let tag = raw.split(['.', '@']).next()?.trim().replace('_', "-");
    if tag.is_empty() || tag.eq_ignore_ascii_case("C") || tag.eq_ignore_ascii_case("POSIX") {
        return None;
    }
    let valid = tag.split('-').all(|part| {
        !part.is_empty() && part.len() <= 8 && part.chars().all(|c| c.is_ascii_alphanumeric())
    });
    valid.then_some(tag)
}

#[cfg(windows)]
mod platform {
    use windows_sys::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};

    pub fn preferred() -> Vec<String> {
        let (mut count, mut length) = (0u32, 0u32);
        // SAFETY: the first call only reports the buffer length; the second fills a buffer of that
        // length. Both pointers outlive the calls.
        unsafe {
            if GetUserPreferredUILanguages(
                MUI_LANGUAGE_NAME,
                &mut count,
                std::ptr::null_mut(),
                &mut length,
            ) == 0
            {
                return Vec::new();
            }
            let mut buffer = vec![0u16; length as usize];
            if GetUserPreferredUILanguages(
                MUI_LANGUAGE_NAME,
                &mut count,
                buffer.as_mut_ptr(),
                &mut length,
            ) == 0
            {
                return Vec::new();
            }
            // A list of NUL-terminated names, ended by an empty one.
            buffer
                .split(|&c| c == 0)
                .filter(|name| !name.is_empty())
                .map(String::from_utf16_lossy)
                .collect()
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{c_char, c_void, CStr};

    type CFIndex = isize;
    const UTF8: u32 = 0x0800_0100; // kCFStringEncodingUTF8

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFLocaleCopyPreferredLanguages() -> *const c_void;
        fn CFArrayGetCount(array: *const c_void) -> CFIndex;
        fn CFArrayGetValueAtIndex(array: *const c_void, index: CFIndex) -> *const c_void;
        fn CFStringGetCString(
            string: *const c_void,
            buffer: *mut c_char,
            size: CFIndex,
            encoding: u32,
        ) -> u8;
        fn CFRelease(object: *const c_void);
    }

    pub fn preferred() -> Vec<String> {
        // SAFETY: the array is owned by this call (a Copy function) and released once; its values are
        // borrowed strings that live as long as the array; the buffer outlives each conversion.
        unsafe {
            let array = CFLocaleCopyPreferredLanguages();
            if array.is_null() {
                return Vec::new();
            }
            let mut tags = Vec::new();
            for index in 0..CFArrayGetCount(array) {
                let string = CFArrayGetValueAtIndex(array, index);
                let mut buffer = [0 as c_char; 128];
                if !string.is_null()
                    && CFStringGetCString(
                        string,
                        buffer.as_mut_ptr(),
                        buffer.len() as CFIndex,
                        UTF8,
                    ) != 0
                {
                    tags.push(
                        CStr::from_ptr(buffer.as_ptr())
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
            CFRelease(array);
            tags
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    pub fn preferred() -> Vec<String> {
        super::from_environment(|name| std::env::var(name).ok())
    }
}

/// The C library's order: `LANGUAGE` is a list, and is read only when a locale is set at all (as GNU
/// gettext does); then the first of `LC_ALL`, `LC_MESSAGES`, `LANG` that is set.
#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
fn from_environment(var: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let set = |name: &str| var(name).filter(|v| !v.is_empty());
    let locale = set("LC_ALL")
        .or_else(|| set("LC_MESSAGES"))
        .or_else(|| set("LANG"));
    let mut list: Vec<String> = Vec::new();
    if locale.as_deref().and_then(normalize).is_some() {
        if let Some(languages) = set("LANGUAGE") {
            list.extend(languages.split(':').map(str::to_owned));
        }
    }
    list.extend(locale);
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preferred_language_finds_its_shipped_form() {
        let shipped = ["en", "ko", "zh-Hant", "pt-BR"];
        assert_eq!(pick(&["ko-KR"], &shipped), Some("ko"));
        assert_eq!(pick(&["zh-Hant-TW"], &shipped), Some("zh-Hant"));
        assert_eq!(pick(&["PT-br"], &shipped), Some("pt-BR"));
        assert_eq!(
            pick(&["pt-PT"], &shipped),
            Some("pt-BR"),
            "same language, another region"
        );
        assert_eq!(
            pick(&["fr-FR", "ko-KR"], &shipped),
            Some("ko"),
            "the next preference when the first is not shipped"
        );
        assert_eq!(pick(&["fr-FR"], &shipped), None);
        assert_eq!(pick::<&str>(&[], &shipped), None);
    }

    #[test]
    fn the_order_of_preference_wins_over_how_close_a_match_is() {
        assert_eq!(pick(&["ja", "en-US"], &["en-US", "ja-JP"]), Some("ja-JP"));
    }

    #[test]
    fn platform_names_become_tags() {
        assert_eq!(normalize("ko_KR.UTF-8").as_deref(), Some("ko-KR"));
        assert_eq!(normalize("de_DE@euro").as_deref(), Some("de-DE"));
        assert_eq!(normalize("zh-Hant-TW").as_deref(), Some("zh-Hant-TW"));
        assert_eq!(normalize("C"), None);
        assert_eq!(normalize("POSIX"), None);
        assert_eq!(normalize("C.UTF-8"), None);
        assert_eq!(normalize(""), None);
        assert_eq!(normalize("en US"), None);
    }

    #[test]
    fn the_environment_is_read_in_the_c_librarys_order() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            from_environment(env(&[("LANG", "ko_KR.UTF-8")])),
            ["ko_KR.UTF-8"]
        );
        assert_eq!(
            from_environment(env(&[("LANG", "en_US.UTF-8"), ("LC_ALL", "de_DE.UTF-8")])),
            ["de_DE.UTF-8"]
        );
        assert_eq!(
            from_environment(env(&[("LANG", "en_US.UTF-8"), ("LC_MESSAGES", "fr_FR")])),
            ["fr_FR"]
        );
        assert_eq!(
            from_environment(env(&[("LANGUAGE", "ko:en_GB"), ("LANG", "en_US.UTF-8")])),
            ["ko", "en_GB", "en_US.UTF-8"]
        );
        assert!(
            from_environment(env(&[("LANGUAGE", "ko:en"), ("LANG", "C")]))
                .iter()
                .all(|v| v == "C"),
            "LANGUAGE is ignored under the C locale"
        );
        assert!(from_environment(env(&[])).is_empty());
    }

    #[test]
    fn the_system_names_its_languages_as_tags_without_repeats() {
        let tags = preferred_languages();
        for tag in &tags {
            assert_eq!(normalize(tag).as_deref(), Some(tag.as_str()), "{tag}");
        }
        let mut lower: Vec<_> = tags.iter().map(|t| t.to_ascii_lowercase()).collect();
        lower.dedup();
        assert_eq!(lower.len(), tags.len());
        // Desktop systems name at least one; a bare CI container may name none.
        #[cfg(any(windows, target_os = "macos"))]
        assert!(!tags.is_empty());
    }
}
