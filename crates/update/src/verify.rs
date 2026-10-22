//! Checking a downloaded installer against who is expected to have signed it.
//!
//! A valid signature alone says only that *someone* with a certificate Windows trusts signed the
//! file. An update check needs more: that it was the app's publisher. Comparing the signer's display
//! name is the usual shortcut, and a weak one — any certificate issued to a similarly named
//! organization passes it. [`Expect`] names the signer by what the publisher controls: its subject,
//! the issuing CA, and, where the certificate is long-lived, its thumbprint.
//!
//! Windows only (Authenticode). Elsewhere [`authenticode`] refuses every file with
//! [`Refused::Unsupported`].

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
    /// The file has no valid signature Windows trusts — unsigned, altered after signing, or signed
    /// with a certificate whose chain does not end in a trusted root. The code is `WinVerifyTrust`'s.
    Untrusted(i32),
    /// Validly signed, but not by an expected signer.
    OtherSigner(Signer),
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
            Refused::Unsupported => {
                f.write_str("Authenticode signatures can be checked on Windows only")
            }
        }
    }
}

impl std::error::Error for Refused {}

/// Whether `path` carries a valid Authenticode signature whose signer `expect` accepts — call it on
/// the downloaded installer, and again just before running it (the file sat on disk in between).
/// The chain must end in a root Windows trusts. Revocation is not checked: a network that cannot
/// reach the CA's revocation list would otherwise block every update.
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

fn hex_digits(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_hexdigit())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(windows)]
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

#[cfg(not(windows))]
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

    #[cfg(not(windows))]
    #[test]
    fn elsewhere_every_file_is_refused() {
        assert_eq!(
            authenticode(Path::new("anything.exe"), &Expect::subject("x")),
            Err(Refused::Unsupported)
        );
    }

    #[cfg(windows)]
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

    #[cfg(windows)]
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
}
