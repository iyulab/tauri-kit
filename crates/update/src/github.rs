//! Releases published on GitHub as a place to look for updates.
//!
//! - **The newest release.** On the stable channel, one request to `…/releases/latest` without
//!   following its redirect: the tag is in the `Location` (`…/releases/tag/v1.4.0`). No API call,
//!   so no API rate limit (60 an hour per address without a token — an office behind one address
//!   shares it) and no JSON. GitHub's «latest» skips pre-releases and drafts. On the beta channel the
//!   API's release list is read once, and the highest version in semver order is taken, pre-release
//!   or not.
//! - **The installer.** An asset of that release, named from the version
//!   (`…/releases/download/v1.4.0/Example_1.4.0_x64-setup.exe`), saved beside its destination and
//!   renamed into place only when it arrived whole.
//!
//! The requests go through a [`Fetch`] the app hands in — the HTTP client it already has, with the
//! proxy and certificate store its users' networks need. The `github` feature adds [`Ureq`], one
//! over `ureq` with the OS's TLS and the proxy the PC is set up with.
//!
//! [`Releases`] finds and fetches; checking what arrived (an Authenticode signer — see
//! `verify::authenticode` — or an updater signature) and running it stay with the app's
//! [`Source`](crate::Source).

use std::io::Write;
use std::path::Path;

use crate::{Channel, Version};

/// The requests [`Releases`] makes. Each blocks; an `Err` is a request that did not get an answer
/// (no network, a timeout, a refused connection) — an HTTP error status is an answer.
pub trait Fetch {
    /// GETs `url` without following a redirect: the `Location` of a redirect, `None` for any other
    /// answer.
    fn redirect(&self, url: &str) -> Result<Option<String>, String>;

    /// GETs `url`, following redirects, and answers the body of a `200`; any other status is an
    /// `Err`. Used for the release list, which is small.
    fn get(&self, url: &str) -> Result<Vec<u8>, String>;

    /// GETs `url`, following redirects, and writes the body of a `200` to `to`; any other status is
    /// an `Err`.
    fn save(&self, url: &str, to: &mut dyn Write) -> Result<(), String>;
}

/// A repository's releases, and how its installers are named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Releases {
    /// `https://github.com/<owner>/<repo>`, without a trailing slash.
    pub web: String,
    /// `https://api.github.com/repos/<owner>/<repo>`, without a trailing slash.
    pub api: String,
    /// The installer's file name in each release, with `{version}` where the version goes:
    /// `Example_{version}_x64-setup.exe`.
    pub asset: String,
    /// What a tag puts before the version. Default `v`.
    pub tag_prefix: String,
}

impl Releases {
    /// The releases of `owner/repo` on github.com, whose installers are named `asset`
    /// (`{version}` replaced) under tags `v<version>`.
    pub fn new(owner: &str, repo: &str, asset: impl Into<String>) -> Self {
        Self {
            web: format!("https://github.com/{owner}/{repo}"),
            api: format!("https://api.github.com/repos/{owner}/{repo}"),
            asset: asset.into(),
            tag_prefix: "v".into(),
        }
    }

    /// The version of the newest release on `channel`, or `None` when there is none to read — no
    /// release yet, or a tag that is not a version. Whether it is newer than the running version is
    /// [`Updates`](crate::Updates)' to decide.
    pub fn latest(&self, fetch: &dyn Fetch, channel: Channel) -> Result<Option<String>, String> {
        match channel {
            Channel::Stable => {
                let location = fetch.redirect(&format!("{}/releases/latest", self.web))?;
                Ok(location.and_then(|location| self.version_of_location(&location)))
            }
            Channel::Beta => {
                let body = fetch.get(&format!("{}/releases?per_page=30", self.api))?;
                self.newest_listed(&body)
            }
        }
    }

    /// Where the installer of `version` is.
    pub fn asset_url(&self, version: &str) -> String {
        format!(
            "{}/releases/download/{}{version}/{}",
            self.web,
            self.tag_prefix,
            self.asset.replace("{version}", version)
        )
    }

    /// Downloads the installer of `version` to `dest`: into `dest` with a `.part` extension first,
    /// synced, then renamed — so `dest` is either a whole download or absent. A failed download
    /// leaves nothing behind.
    pub fn download(&self, fetch: &dyn Fetch, version: &str, dest: &Path) -> Result<(), String> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let part = dest.with_extension("part");
        let fetched = (|| {
            let mut file =
                std::io::BufWriter::new(std::fs::File::create(&part).map_err(|e| e.to_string())?);
            fetch.save(&self.asset_url(version), &mut file)?;
            file.into_inner()
                .map_err(|e| e.to_string())?
                .sync_all()
                .map_err(|e| e.to_string())?;
            std::fs::rename(&part, dest).map_err(|e| e.to_string())
        })();
        if fetched.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        fetched
    }

    /// The version in a release page's address (`…/releases/tag/v1.4.0`), when the tag is one.
    fn version_of_location(&self, location: &str) -> Option<String> {
        let tag = location
            .trim()
            .trim_end_matches('/')
            .rsplit_once("/releases/tag/")?
            .1;
        self.version_of_tag(tag)
    }

    fn version_of_tag(&self, tag: &str) -> Option<String> {
        let version = tag.strip_prefix(self.tag_prefix.as_str())?;
        Version::parse(version).map(|_| version.to_owned())
    }

    /// The highest version among the listed releases that are not drafts.
    fn newest_listed(&self, body: &[u8]) -> Result<Option<String>, String> {
        #[derive(serde::Deserialize)]
        struct Listed {
            tag_name: String,
            #[serde(default)]
            draft: bool,
        }
        let listed: Vec<Listed> = serde_json::from_slice(body)
            .map_err(|e| format!("the release list did not read: {e}"))?;
        Ok(listed
            .iter()
            .filter(|release| !release.draft)
            .filter_map(|release| {
                Some((
                    Version::parse(&self.version_of_tag(&release.tag_name)?)?,
                    release,
                ))
            })
            .max_by(|(a, _), (b, _)| a.cmp(b))
            .and_then(|(_, release)| self.version_of_tag(&release.tag_name)))
    }
}

/// A [`Fetch`] over `ureq` (the `github` feature): the OS's TLS and certificate store, the proxy the
/// PC is set up with (on Windows the system proxy settings — a proxy given by a configuration script
/// is not read; an app on networks that use one hands in its own [`Fetch`]), a `User-Agent` of the
/// app's, a minute to connect and to answer, and no limit on the time a download takes.
#[cfg(feature = "github")]
pub struct Ureq {
    agent: ureq::Agent,
    no_redirect: ureq::Agent,
}

#[cfg(feature = "github")]
impl Ureq {
    /// `user_agent` names the app: `Example/1.4.0`.
    pub fn new(user_agent: &str) -> Self {
        use std::time::Duration;
        use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
        let build = |redirects: u32| -> ureq::Agent {
            let tls = TlsConfig::builder()
                .provider(TlsProvider::NativeTls)
                .root_certs(RootCerts::PlatformVerifier)
                .build();
            ureq::Agent::config_builder()
                .tls_config(tls)
                .user_agent(user_agent)
                .http_status_as_error(false)
                .max_redirects(redirects)
                .timeout_connect(Some(Duration::from_secs(60)))
                .timeout_recv_response(Some(Duration::from_secs(60)))
                .build()
                .into()
        };
        Self {
            agent: build(10),
            no_redirect: build(0),
        }
    }

    fn ok(&self, url: &str) -> Result<ureq::Body, String> {
        let response = self.agent.get(url).call().map_err(|e| e.to_string())?;
        match response.status().as_u16() {
            200 => Ok(response.into_body()),
            status => Err(format!("{url} answered {status}")),
        }
    }
}

#[cfg(feature = "github")]
impl Fetch for Ureq {
    fn redirect(&self, url: &str) -> Result<Option<String>, String> {
        let response = self
            .no_redirect
            .get(url)
            .call()
            .map_err(|e| e.to_string())?;
        if !response.status().is_redirection() {
            return Ok(None);
        }
        Ok(response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned))
    }

    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        self.ok(url)?
            .with_config()
            .limit(4 * 1024 * 1024)
            .read_to_vec()
            .map_err(|e| e.to_string())
    }

    fn save(&self, url: &str, to: &mut dyn Write) -> Result<(), String> {
        let mut body = self.ok(url)?.into_reader();
        std::io::copy(&mut body, to)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Answers from a table, and remembers what was asked.
    #[derive(Default)]
    struct Table {
        redirects: Vec<(&'static str, Option<&'static str>)>,
        bodies: Vec<(&'static str, &'static [u8])>,
        fail: bool,
        asked: RefCell<Vec<String>>,
    }

    impl Fetch for Table {
        fn redirect(&self, url: &str) -> Result<Option<String>, String> {
            self.asked.borrow_mut().push(url.to_owned());
            if self.fail {
                return Err("no network".into());
            }
            Ok(self
                .redirects
                .iter()
                .find(|(u, _)| *u == url)
                .and_then(|(_, l)| l.map(str::to_owned)))
        }

        fn get(&self, url: &str) -> Result<Vec<u8>, String> {
            self.asked.borrow_mut().push(url.to_owned());
            self.bodies
                .iter()
                .find(|(u, _)| *u == url)
                .map(|(_, b)| b.to_vec())
                .ok_or_else(|| format!("{url} answered 404"))
        }

        fn save(&self, url: &str, to: &mut dyn Write) -> Result<(), String> {
            let body = self.get(url)?;
            to.write_all(&body[..body.len() / 2])
                .map_err(|e| e.to_string())?;
            if self.fail {
                return Err("the connection was reset".into());
            }
            to.write_all(&body[body.len() / 2..])
                .map_err(|e| e.to_string())
        }
    }

    fn releases() -> Releases {
        Releases::new("example", "app", "Example_{version}_x64-setup.exe")
    }

    #[test]
    fn stable_reads_the_tag_from_the_latest_redirect() {
        let fetch = Table {
            redirects: vec![(
                "https://github.com/example/app/releases/latest",
                Some("https://github.com/example/app/releases/tag/v1.4.0"),
            )],
            ..Table::default()
        };
        assert_eq!(
            releases().latest(&fetch, Channel::Stable),
            Ok(Some("1.4.0".into()))
        );
        assert_eq!(fetch.asked.borrow().len(), 1, "one request, no API call");
    }

    #[test]
    fn a_tag_that_is_not_a_version_or_no_release_offers_nothing() {
        let r = releases();
        assert_eq!(
            r.version_of_location("https://github.com/example/app/releases/tag/nightly"),
            None
        );
        assert_eq!(
            r.version_of_location("https://github.com/example/app/releases/tag/1.4.0"),
            None,
            "the tag prefix is required"
        );
        assert_eq!(
            r.version_of_location("https://github.com/example/app/releases/tag/v1.4.0/"),
            Some("1.4.0".into())
        );
        assert_eq!(
            r.version_of_location("https://github.com/example/app/releases"),
            None
        );
        // A repository with no release answers 404, not a redirect.
        assert_eq!(r.latest(&Table::default(), Channel::Stable), Ok(None));
    }

    #[test]
    fn no_network_is_an_error_not_an_empty_answer() {
        let fetch = Table {
            fail: true,
            ..Table::default()
        };
        assert!(releases().latest(&fetch, Channel::Stable).is_err());
    }

    #[test]
    fn beta_takes_the_highest_listed_version_and_skips_drafts() {
        let list = br#"[
            {"tag_name": "v1.5.0-beta.1", "draft": true},
            {"tag_name": "v1.4.0", "draft": false},
            {"tag_name": "v1.4.1-beta.2", "draft": false, "prerelease": true},
            {"tag_name": "v1.4.1-beta.10", "draft": false, "prerelease": true},
            {"tag_name": "nightly", "draft": false}
        ]"#;
        let fetch = Table {
            bodies: vec![(
                "https://api.github.com/repos/example/app/releases?per_page=30",
                list,
            )],
            ..Table::default()
        };
        assert_eq!(
            releases().latest(&fetch, Channel::Beta),
            Ok(Some("1.4.1-beta.10".into())),
            "semver order, numbers by value"
        );
    }

    #[test]
    fn the_installer_is_named_from_the_version() {
        assert_eq!(
            releases().asset_url("1.4.0"),
            "https://github.com/example/app/releases/download/v1.4.0/Example_1.4.0_x64-setup.exe"
        );
    }

    #[test]
    fn a_download_is_whole_or_absent() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir
            .path()
            .join("updates")
            .join("Example_1.4.0_x64-setup.exe");
        let url =
            "https://github.com/example/app/releases/download/v1.4.0/Example_1.4.0_x64-setup.exe";

        let broken = Table {
            bodies: vec![(url, b"MZ an installer")],
            fail: true,
            ..Table::default()
        };
        assert!(releases().download(&broken, "1.4.0", &dest).is_err());
        assert!(
            !dest.exists() && !dest.with_extension("part").exists(),
            "nothing left behind"
        );

        let whole = Table {
            bodies: vec![(url, b"MZ an installer")],
            ..Table::default()
        };
        releases().download(&whole, "1.4.0", &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"MZ an installer");
        assert!(!dest.with_extension("part").exists());
    }

    /// The real «latest» redirect of a public repository — by hand: `cargo test -p tauri-kit-update --features github -- --ignored`.
    #[cfg(feature = "github")]
    #[test]
    #[ignore = "network"]
    fn the_real_latest_redirect_names_a_tag() {
        let fetch = Ureq::new("tauri-kit-update-test");
        let version =
            Releases::new("tauri-apps", "tauri", "unused").latest(&fetch, Channel::Stable);
        // tauri's tags are not `v<version>` (`tauri-v2.x.y`), so the redirect is read but names no version of this shape.
        assert_eq!(version, Ok(None));
        let location = fetch
            .redirect("https://github.com/tauri-apps/tauri/releases/latest")
            .unwrap();
        assert!(location.is_some_and(|l| l.contains("/releases/tag/")));
    }
}
