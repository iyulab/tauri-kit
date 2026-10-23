//! Checking a downloaded update against who is expected to have signed it.
//!
//! A valid signature alone says only that *someone* with a certificate the OS trusts signed the
//! file. An update check needs more: that it was the app's publisher. Comparing the signer's display
//! name is the usual shortcut, and a weak one — any certificate issued to a similarly named
//! organization passes it.
//!
//! - **Windows** — [`authenticode`] (feature `authenticode`): [`Expect`] names the signer by what the
//!   publisher controls: its subject, the issuing CA, and, where the certificate is long-lived, its
//!   thumbprint.
//! - **macOS** — [`developer_id`] (feature `developer-id`): the bundle must be signed with a Developer
//!   ID certificate of one of the teams [`DeveloperId`] names, and notarized. The team ID is what
//!   Apple issues to the publisher alone, so it is the name to check.
//!
//! On any other platform each refuses every file with [`Refused::Unsupported`].

use std::fmt;
use std::path::Path;

/// Who must have signed a file. Each list is "any of these"; an empty list does not constrain.
/// At least one list must be non-empty — an expectation that accepts any signer is refused, since it
/// would trust every validly signed file.
///
/// ```
/// use tauri_kit_update::verify::Expect;
///
/// // A publisher whose CA issues from two intermediates.
/// let expect = Expect::subject("Example Software Ltd.")
///     .or_issuer("Example CA EV Code Signing 01")
///     .or_issuer("Example CA EV Code Signing 02");
/// assert!(!expect.is_empty());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Expect {
    /// The signing certificate's subject, as its simple display name (the organization or common
    /// name — what Windows shows as the signer). Compared exactly.
    pub subjects: Vec<String>,
    /// The issuing certificate's simple display name. Compared exactly.
    pub issuers: Vec<String>,
    /// The signing certificate's thumbprint, in hex: SHA-1 (40 digits, what Windows shows as
    /// «Thumbprint») or SHA-256 (64). Case, spaces and colons are ignored. Pin a thumbprint only
    /// for a certificate that lives long — a signing service that issues short-lived certificates
    /// changes it every few days.
    pub thumbprints: Vec<String>,
}

impl Expect {
    /// Signed by a certificate whose subject is `name`.
    pub fn subject(name: impl Into<String>) -> Self {
        Self {
            subjects: vec![name.into()],
            ..Self::default()
        }
    }

    /// Signed by the certificate with this thumbprint.
    pub fn thumbprint(hex: impl Into<String>) -> Self {
        Self {
            thumbprints: vec![hex.into()],
            ..Self::default()
        }
    }

    /// Also accepts a certificate whose subject is `name`.
    pub fn or_subject(mut self, name: impl Into<String>) -> Self {
        self.subjects.push(name.into());
        self
    }

    /// Requires the issuer to be `name`, or another issuer given the same way.
    pub fn or_issuer(mut self, name: impl Into<String>) -> Self {
        self.issuers.push(name.into());
        self
    }

    /// Requires the thumbprint to be `hex`, or another thumbprint given the same way.
    pub fn or_thumbprint(mut self, hex: impl Into<String>) -> Self {
        self.thumbprints.push(hex.into());
        self
    }

    /// Whether nothing is required — such an expectation is refused.
    pub fn is_empty(&self) -> bool {
        self.subjects.is_empty() && self.issuers.is_empty() && self.thumbprints.is_empty()
    }

    /// Whether `signer` is one this expects.
    pub fn accepts(&self, signer: &Signer) -> bool {
        if self.is_empty() {
            return false;
        }
        let subject = self.subjects.is_empty() || self.subjects.contains(&signer.subject);
        let issuer = self.issuers.is_empty() || self.issuers.contains(&signer.issuer);
        let thumbprint = self.thumbprints.is_empty()
            || self
                .thumbprints
                .iter()
                .map(|t| hex_digits(t))
                .any(|t| t == signer.sha1 || t == signer.sha256);
        subject && issuer && thumbprint
    }
}

/// The certificate a file is signed with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signer {
    /// Its subject's simple display name.
    pub subject: String,
    /// Its issuer's simple display name.
    pub issuer: String,
    /// Its SHA-1 thumbprint, lowercase hex.
    pub sha1: String,
    /// Its SHA-256 thumbprint, lowercase hex.
    pub sha256: String,
}

/// Why a file was not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// The expectation names no signer.
    NoExpectation,
    /// The file has no valid signature the OS trusts — unsigned, altered after signing, or signed
    /// with a certificate whose chain does not end in a trusted root. The code is `WinVerifyTrust`'s
    /// on Windows and the Security framework's `OSStatus` on macOS.
    Untrusted(i32),
    /// Validly signed, but not by an expected signer.
    OtherSigner(Signer),
    /// Validly signed, but not a notarized Developer ID signature of an expected team: another team,
    /// another kind of certificate (ad hoc, a development certificate, Apple's own), or not notarized.
    OtherDeveloper(CodeSigner),
    /// Not on this platform.
    Unsupported,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refused::NoExpectation => f.write_str("no expected signer was given"),
            Refused::Untrusted(code) => write!(
                f,
                "the file has no trusted signature (0x{:08x})",
                *code as u32
            ),
            Refused::OtherSigner(signer) => write!(
                f,
                "the file is signed by {} (issued by {}, thumbprint {}), not an expected signer",
                signer.subject, signer.issuer, signer.sha1
            ),
            Refused::OtherDeveloper(signer) => write!(
                f,
                "the bundle {} is signed by {}, which is not a notarized Developer ID signature of an expected team",
                signer.identifier,
                signer.team.as_deref().unwrap_or("no team (ad hoc or Apple's own)")
            ),
            Refused::Unsupported => {
                f.write_str("this kind of signature cannot be checked on this platform")
            }
        }
    }
}

impl std::error::Error for Refused {}

/// Whether `path` carries a valid Authenticode signature whose signer `expect` accepts — call it on
/// the downloaded installer, and again just before running it (the file sat on disk in between).
/// The chain must end in a root Windows trusts. Revocation is not checked: a network that cannot
/// reach the CA's revocation list would otherwise block every update.
#[cfg(feature = "authenticode")]
pub fn authenticode(path: &Path, expect: &Expect) -> Result<Signer, Refused> {
    if expect.is_empty() {
        return Err(Refused::NoExpectation);
    }
    let signer = signer(path)?;
    if expect.accepts(&signer) {
        Ok(signer)
    } else {
        Err(Refused::OtherSigner(signer))
    }
}

/// Which publishers may have signed a macOS app: their Apple team IDs (ten letters and digits, what
/// the Developer ID certificate carries as its organizational unit). "Any of these" — a publisher
/// moving to a new team lists both for a while.
///
/// ```
/// use tauri_kit_update::verify::DeveloperId;
///
/// let expect = DeveloperId::team("A1B2C3D4E5");
/// assert_eq!(expect.teams, ["A1B2C3D4E5"]);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeveloperId {
    /// The accepted team IDs. Empty, or any entry that is not ten ASCII letters and digits, and every
    /// file is refused with [`Refused::NoExpectation`].
    pub teams: Vec<String>,
}

impl DeveloperId {
    /// Signed with a Developer ID certificate of team `id`.
    pub fn team(id: impl Into<String>) -> Self {
        Self {
            teams: vec![id.into()],
        }
    }

    /// Also accepts team `id`.
    pub fn or_team(mut self, id: impl Into<String>) -> Self {
        self.teams.push(id.into());
        self
    }

    /// The code requirement a bundle must satisfy, or `None` when the team list cannot make one.
    /// Apple's own Developer ID requirement (an Apple anchor, the Developer ID intermediate, the
    /// Developer ID Application leaf) narrowed to the teams, and `notarized`.
    pub fn requirement(&self) -> Option<String> {
        let valid = |id: &String| id.len() == 10 && id.bytes().all(|b| b.is_ascii_alphanumeric());
        if self.teams.is_empty() || !self.teams.iter().all(valid) {
            return None;
        }
        let teams = self
            .teams
            .iter()
            .map(|id| format!("certificate leaf[subject.OU] = \"{id}\""))
            .collect::<Vec<_>>()
            .join(" or ");
        Some(format!(
            "anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
             and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and ({teams}) and notarized"
        ))
    }
}

/// Who a macOS bundle says signed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeSigner {
    /// The signing identifier (the bundle identifier, for an app signed the usual way).
    pub identifier: String,
    /// The team ID of the signing certificate — `None` for an ad hoc signature or Apple's own.
    pub team: Option<String>,
}

/// Whether the macOS bundle (or program) at `path` is validly signed — every architecture and every
/// nested program and library, strictly — with a Developer ID certificate of a team `expect` names,
/// and notarized. Call it on the downloaded bundle once it is unpacked, and again just before moving
/// it into place.
///
/// Notarization is read from the ticket stapled to the bundle, so a stapled release passes with no
/// network; an unstapled one needs Apple's servers to answer. Revocation is not checked online, for
/// the same reason [`authenticode`] does not: a network that cannot reach it would block every update.
#[cfg(feature = "developer-id")]
pub fn developer_id(path: &Path, expect: &DeveloperId) -> Result<CodeSigner, Refused> {
    let Some(requirement) = expect.requirement() else {
        return Err(Refused::NoExpectation);
    };
    code_signature(path, &requirement)
}

#[cfg(all(feature = "developer-id", target_os = "macos"))]
fn code_signature(path: &Path, requirement: &str) -> Result<CodeSigner, Refused> {
    macos::check(path, requirement)
}

#[cfg(all(feature = "developer-id", not(target_os = "macos")))]
fn code_signature(_path: &Path, _requirement: &str) -> Result<CodeSigner, Refused> {
    Err(Refused::Unsupported)
}

#[cfg(all(feature = "developer-id", target_os = "macos"))]
mod macos {
    //! The Security framework's static code checks, called directly: the few functions this needs
    //! do not warrant a binding crate.
    use super::{CodeSigner, Refused};
    use std::ffi::{c_char, c_void, CStr};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    type CFTypeRef = *const c_void;
    type OSStatus = i32;

    const UTF8: u32 = 0x0800_0100;
    const CHECK_ALL_ARCHITECTURES: u32 = 1 << 0;
    const SIGNING_INFORMATION: u32 = 1 << 1;
    const CHECK_NESTED_CODE: u32 = 1 << 3;
    const STRICT_VALIDATE: u32 = 1 << 4;
    /// `errSecCSReqFailed`: validly signed, but the requirement is not met.
    const REQUIREMENT_FAILED: OSStatus = -67050;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(value: CFTypeRef);
        fn CFGetTypeID(value: CFTypeRef) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFStringCreateWithBytes(
            allocator: CFTypeRef,
            bytes: *const u8,
            length: isize,
            encoding: u32,
            external: u8,
        ) -> CFTypeRef;
        fn CFStringGetCString(
            string: CFTypeRef,
            buffer: *mut c_char,
            size: isize,
            encoding: u32,
        ) -> u8;
        fn CFURLCreateFromFileSystemRepresentation(
            allocator: CFTypeRef,
            path: *const u8,
            length: isize,
            directory: u8,
        ) -> CFTypeRef;
        fn CFDictionaryGetValue(dictionary: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    }

    #[link(name = "Security", kind = "framework")]
    extern "C" {
        static kSecCodeInfoIdentifier: CFTypeRef;
        static kSecCodeInfoTeamIdentifier: CFTypeRef;
        fn SecStaticCodeCreateWithPath(
            path: CFTypeRef,
            flags: u32,
            code: *mut CFTypeRef,
        ) -> OSStatus;
        fn SecRequirementCreateWithString(
            text: CFTypeRef,
            flags: u32,
            requirement: *mut CFTypeRef,
        ) -> OSStatus;
        fn SecStaticCodeCheckValidity(
            code: CFTypeRef,
            flags: u32,
            requirement: CFTypeRef,
        ) -> OSStatus;
        fn SecCodeCopySigningInformation(
            code: CFTypeRef,
            flags: u32,
            information: *mut CFTypeRef,
        ) -> OSStatus;
    }

    /// Releases the object it holds when dropped.
    struct Owned(CFTypeRef);

    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) };
            }
        }
    }

    fn string(text: &str) -> Owned {
        Owned(unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                text.as_ptr(),
                text.len() as isize,
                UTF8,
                0,
            )
        })
    }

    /// The string in `dictionary` under `key`, if there is one.
    fn text(dictionary: CFTypeRef, key: CFTypeRef) -> Option<String> {
        unsafe {
            let value = CFDictionaryGetValue(dictionary, key);
            if value.is_null() || CFGetTypeID(value) != CFStringGetTypeID() {
                return None;
            }
            let mut buffer = [0 as c_char; 256];
            if CFStringGetCString(value, buffer.as_mut_ptr(), buffer.len() as isize, UTF8) == 0 {
                return None;
            }
            Some(
                CStr::from_ptr(buffer.as_ptr())
                    .to_string_lossy()
                    .into_owned(),
            )
        }
    }

    pub(super) fn check(path: &Path, requirement: &str) -> Result<CodeSigner, Refused> {
        let bytes = path.as_os_str().as_bytes();
        let directory = u8::from(path.is_dir());
        let url = Owned(unsafe {
            CFURLCreateFromFileSystemRepresentation(
                std::ptr::null(),
                bytes.as_ptr(),
                bytes.len() as isize,
                directory,
            )
        });
        if url.0.is_null() {
            return Err(Refused::Untrusted(-1));
        }
        let mut code = Owned(std::ptr::null());
        let status = unsafe { SecStaticCodeCreateWithPath(url.0, 0, &mut code.0) };
        if status != 0 {
            return Err(Refused::Untrusted(status));
        }
        let flags = CHECK_ALL_ARCHITECTURES | CHECK_NESTED_CODE | STRICT_VALIDATE;
        // First whether the signature holds at all, so a refusal can say who signed.
        let status = unsafe { SecStaticCodeCheckValidity(code.0, flags, std::ptr::null()) };
        if status != 0 {
            return Err(Refused::Untrusted(status));
        }
        let mut information = Owned(std::ptr::null());
        let status = unsafe {
            SecCodeCopySigningInformation(code.0, SIGNING_INFORMATION, &mut information.0)
        };
        if status != 0 {
            return Err(Refused::Untrusted(status));
        }
        let signer = unsafe {
            CodeSigner {
                identifier: text(information.0, kSecCodeInfoIdentifier).unwrap_or_default(),
                team: text(information.0, kSecCodeInfoTeamIdentifier),
            }
        };
        let text = string(requirement);
        let mut compiled = Owned(std::ptr::null());
        let status = unsafe { SecRequirementCreateWithString(text.0, 0, &mut compiled.0) };
        if status != 0 {
            return Err(Refused::Untrusted(status));
        }
        match unsafe { SecStaticCodeCheckValidity(code.0, flags, compiled.0) } {
            0 => Ok(signer),
            REQUIREMENT_FAILED => Err(Refused::OtherDeveloper(signer)),
            status => Err(Refused::Untrusted(status)),
        }
    }
}

fn hex_digits(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_hexdigit())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(all(feature = "authenticode", windows))]
fn signer(path: &Path) -> Result<Signer, Refused> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::Cryptography::{
        CertGetCertificateContextProperty, CertGetNameStringW, CERT_CONTEXT, CERT_NAME_ISSUER_FLAG,
        CERT_NAME_SIMPLE_DISPLAY_TYPE, CERT_SHA1_HASH_PROP_ID, CERT_SHA256_HASH_PROP_ID,
    };
    use windows_sys::Win32::Security::WinTrust::*;

    unsafe fn name(certificate: *const CERT_CONTEXT, flags: u32) -> String {
        let mut buffer = [0u16; 512];
        let length = unsafe {
            CertGetNameStringW(
                certificate,
                CERT_NAME_SIMPLE_DISPLAY_TYPE,
                flags,
                std::ptr::null(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
            )
        };
        if length > 1 {
            String::from_utf16_lossy(&buffer[..length as usize - 1])
        } else {
            String::new()
        }
    }

    unsafe fn hash(certificate: *const CERT_CONTEXT, property: u32) -> String {
        let mut buffer = [0u8; 64];
        let mut size = buffer.len() as u32;
        let ok = unsafe {
            CertGetCertificateContextProperty(
                certificate,
                property,
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if ok == 0 {
            String::new()
        } else {
            buffer[..size as usize]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        }
    }

    let file: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut info = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: file.as_ptr(),
        ..Default::default()
    };
    let mut data = WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        dwStateAction: WTD_STATEACTION_VERIFY,
        ..Default::default()
    };
    data.Anonymous.pFile = &mut info;
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    unsafe {
        let code = WinVerifyTrust(std::ptr::null_mut(), &mut action, (&raw mut data).cast());
        let found = if code != 0 {
            Err(Refused::Untrusted(code))
        } else {
            let provider = WTHelperProvDataFromStateData(data.hWVTStateData);
            let signer = if provider.is_null() {
                std::ptr::null_mut()
            } else {
                WTHelperGetProvSignerFromChain(provider, 0, 0, 0)
            };
            if signer.is_null() || (*signer).csCertChain == 0 || (*signer).pasCertChain.is_null() {
                Err(Refused::Untrusted(code))
            } else {
                let certificate = (*(*signer).pasCertChain).pCert;
                Ok(Signer {
                    subject: name(certificate, 0),
                    issuer: name(certificate, CERT_NAME_ISSUER_FLAG),
                    sha1: hash(certificate, CERT_SHA1_HASH_PROP_ID),
                    sha256: hash(certificate, CERT_SHA256_HASH_PROP_ID),
                })
            }
        };
        // Release the state the verify call kept, whatever it found.
        data.dwStateAction = WTD_STATEACTION_CLOSE;
        WinVerifyTrust(std::ptr::null_mut(), &mut action, (&raw mut data).cast());
        found
    }
}

#[cfg(all(feature = "authenticode", not(windows)))]
fn signer(_path: &Path) -> Result<Signer, Refused> {
    Err(Refused::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> Signer {
        Signer {
            subject: "Example Software Ltd.".into(),
            issuer: "Example CA EV Code Signing 02".into(),
            sha1: "0123456789abcdef0123456789abcdef01234567".into(),
            sha256: "ab".repeat(32),
        }
    }

    #[cfg(feature = "authenticode")]
    #[test]
    fn an_empty_expectation_accepts_no_signer() {
        assert!(!Expect::default().accepts(&signer()));
        assert_eq!(
            authenticode(Path::new("anything.exe"), &Expect::default()),
            Err(Refused::NoExpectation)
        );
    }

    #[test]
    fn every_given_list_must_match_and_each_is_any_of() {
        let s = signer();
        assert!(Expect::subject("Example Software Ltd.").accepts(&s));
        assert!(
            !Expect::subject("Example Software").accepts(&s),
            "the subject is compared exactly"
        );
        assert!(Expect::subject("Other")
            .or_subject("Example Software Ltd.")
            .accepts(&s));
        assert!(Expect::subject("Example Software Ltd.")
            .or_issuer("Example CA EV Code Signing 01")
            .or_issuer("Example CA EV Code Signing 02")
            .accepts(&s));
        assert!(
            !Expect::subject("Example Software Ltd.")
                .or_issuer("Example CA EV Code Signing 01")
                .accepts(&s),
            "a wrong issuer refuses a right subject"
        );
    }

    #[test]
    fn a_thumbprint_matches_either_hash_whatever_its_spelling() {
        let s = signer();
        assert!(
            Expect::thumbprint("01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67")
                .accepts(&s)
        );
        assert!(Expect::thumbprint("AB".repeat(32)).accepts(&s));
        assert!(!Expect::thumbprint("ff".repeat(20)).accepts(&s));
        assert!(
            !Expect::subject("Example Software Ltd.")
                .or_thumbprint("ff".repeat(20))
                .accepts(&s),
            "a pinned thumbprint refuses a renewed certificate"
        );
    }

    #[cfg(all(feature = "authenticode", not(windows)))]
    #[test]
    fn elsewhere_every_file_is_refused() {
        assert_eq!(
            authenticode(Path::new("anything.exe"), &Expect::subject("x")),
            Err(Refused::Unsupported)
        );
    }

    #[cfg(all(feature = "authenticode", windows))]
    #[test]
    fn an_unsigned_or_missing_file_is_untrusted() {
        let dir = tempfile::tempdir().unwrap();
        let unsigned = dir.path().join("unsigned.exe");
        std::fs::write(&unsigned, b"MZ not really a program").unwrap();
        assert!(matches!(
            authenticode(&unsigned, &Expect::subject("x")),
            Err(Refused::Untrusted(_))
        ));
        assert!(matches!(
            authenticode(&dir.path().join("missing.exe"), &Expect::subject("x")),
            Err(Refused::Untrusted(_))
        ));
    }

    #[cfg(all(feature = "authenticode", windows))]
    #[test]
    fn a_system_file_names_its_signer() {
        // A file signed by the platform's own publisher: catalog-signed system files carry no embedded
        // signature, so take a program that does.
        let program =
            std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("explorer.exe");
        match authenticode(&program, &Expect::subject("nobody")) {
            Err(Refused::OtherSigner(signer)) => {
                assert!(!signer.subject.is_empty() && !signer.issuer.is_empty());
                assert_eq!(signer.sha1.len(), 40);
                assert_eq!(signer.sha256.len(), 64);
            }
            // Catalog-signed on this edition: nothing embedded to read.
            Err(Refused::Untrusted(_)) => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_team_list_makes_a_requirement_only_from_team_ids() {
        assert_eq!(DeveloperId::default().requirement(), None);
        assert_eq!(
            DeveloperId::team("A1B2C3D4E").requirement(),
            None,
            "nine characters"
        );
        assert_eq!(
            DeveloperId::team("A1B2C3D4E5")
                .or_team("A1B2\" or 1")
                .requirement(),
            None,
            "an entry that is not a team ID refuses the whole list"
        );
        let requirement = DeveloperId::team("A1B2C3D4E5")
            .or_team("Z9Y8X7W6V5")
            .requirement()
            .unwrap();
        assert!(requirement.starts_with("anchor apple generic and "));
        assert!(requirement.contains(
            "(certificate leaf[subject.OU] = \"A1B2C3D4E5\" or certificate leaf[subject.OU] = \"Z9Y8X7W6V5\")"
        ));
        assert!(requirement.ends_with(" and notarized"));
    }

    #[cfg(feature = "developer-id")]
    #[test]
    fn no_team_refuses_every_bundle() {
        assert_eq!(
            developer_id(Path::new("Anything.app"), &DeveloperId::default()),
            Err(Refused::NoExpectation)
        );
    }

    #[cfg(all(feature = "developer-id", not(target_os = "macos")))]
    #[test]
    fn elsewhere_every_bundle_is_refused() {
        assert_eq!(
            developer_id(Path::new("Anything.app"), &DeveloperId::team("A1B2C3D4E5")),
            Err(Refused::Unsupported)
        );
    }

    #[cfg(all(feature = "developer-id", target_os = "macos"))]
    #[test]
    fn an_unsigned_or_missing_file_is_untrusted_on_macos() {
        let dir = tempfile::tempdir().unwrap();
        let unsigned = dir.path().join("unsigned");
        std::fs::write(&unsigned, b"#!/bin/sh\n").unwrap();
        let expect = DeveloperId::team("A1B2C3D4E5");
        assert!(matches!(
            developer_id(&unsigned, &expect),
            Err(Refused::Untrusted(_))
        ));
        assert!(matches!(
            developer_id(&dir.path().join("missing.app"), &expect),
            Err(Refused::Untrusted(_))
        ));
    }

    #[cfg(all(feature = "developer-id", target_os = "macos"))]
    #[test]
    fn a_system_program_is_validly_signed_but_not_by_a_developer_id() {
        match developer_id(Path::new("/bin/ls"), &DeveloperId::team("A1B2C3D4E5")) {
            Err(Refused::OtherDeveloper(signer)) => {
                assert_eq!(signer.identifier, "com.apple.ls");
                assert_eq!(signer.team, None, "Apple's own signature has no team");
            }
            other => panic!("{other:?}"),
        }
    }
}
