//! `navigator ops forgejo release mirror`: copy one published `navigator`
//! release into the Forgejo installation's own package registry.
//!
//! Generated Forgejo workflows install the release `navigator.yaml` pins from
//! `<forgejo>/api/packages/navigator/generic/navigator/<version>/`, so a
//! runner's egress is the Forgejo origin and the Navigator host and never a
//! rotating GitHub address (ENG-1234). This module downloads the Linux
//! archive and its `.sha256` from the GitHub Release, verifies the digest,
//! and uploads both. The runner controller calls [`ensure`] for every version
//! a waiting job pins, so a release is mirrored before its first job runs.

use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use reqwest::StatusCode;
use sha2::{Digest, Sha256};

use project_scaffold::{FORGEJO_RELEASE_OWNER, FORGEJO_RELEASE_PACKAGE, RELEASE_DOWNLOAD_BASE};

/// What [`ensure`] found or did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::devx) enum Mirrored {
    /// Both files were already in the registry.
    Present,
    /// The release was downloaded, verified, and uploaded.
    Uploaded,
}

/// An exact release tag: `YY.M.D` with an optional pre-release suffix, the
/// same shape the generated install step accepts.
pub(in crate::devx) fn release_version(version: &str) -> Result<()> {
    let (core, pre) = version.split_once('-').unwrap_or((version, ""));
    let core_ok = {
        let parts: Vec<_> = core.split('.').collect();
        parts.len() == 3
            && parts
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    };
    let pre_ok = !version.contains('-')
        || (!pre.is_empty()
            && pre.split('.').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            }));
    ensure!(
        core_ok && pre_ok,
        "release version must be an exact tag such as 26.11.5 or 26.11.5-rc.1"
    );
    Ok(())
}

/// The two files a Linux runner installs for `version`.
fn asset_names(version: &str) -> (String, String) {
    let archive = format!("navigator-{version}-linux.tar.gz");
    let digest = format!("{archive}.sha256");
    (archive, digest)
}

/// The hex digest a `sha256sum`-format line names for `archive`.
fn expected_digest(sha_file: &str, archive: &str) -> Result<String> {
    let line = sha_file
        .lines()
        .find(|line| !line.trim().is_empty())
        .context("the release digest file is empty")?;
    let mut fields = line.split_whitespace();
    let digest = fields.next().unwrap_or_default().to_ascii_lowercase();
    let named = fields.next().map(|name| name.trim_start_matches('*'));
    ensure!(
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "the release digest file does not hold a SHA-256"
    );
    ensure!(
        named == Some(archive),
        "the release digest file names a different archive"
    );
    Ok(digest)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// The Forgejo side of the mirror: the package registry and the one public
/// organization that owns the package.
pub(in crate::devx) struct Registry {
    client: reqwest::Client,
    origin: String,
    token: String,
    source: String,
}

impl Registry {
    /// `origin` is the Forgejo root URL; `token` an operator token allowed to
    /// write the `navigator` organization's packages.
    pub(in crate::devx) fn new(origin: &str, token: String) -> Result<Self> {
        let url = url::Url::parse(origin).context("parse Forgejo origin")?;
        ensure!(
            (url.scheme() == "https"
                || (url.scheme() == "http"
                    && url.host_str().is_some_and(|host| {
                        host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
                    })))
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none(),
            "the Forgejo origin must be HTTPS (HTTP only on loopback) without credentials or a query"
        );
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_mins(5))
                .build()?,
            origin: origin.trim_end_matches('/').to_owned(),
            token,
            source: RELEASE_DOWNLOAD_BASE.to_owned(),
        })
    }

    #[cfg(test)]
    fn with_source(mut self, source: &str) -> Self {
        self.source = source.trim_end_matches('/').to_owned();
        self
    }

    fn package_url(&self, version: &str, file: &str) -> String {
        format!(
            "{}/api/packages/{FORGEJO_RELEASE_OWNER}/generic/{FORGEJO_RELEASE_PACKAGE}/{version}/{file}",
            self.origin
        )
    }

    /// Whether the registry already serves `file`. Read anonymously with
    /// `GET` (the package routes answer no `HEAD`): the organization is
    /// public, so this is exactly what a runner sees.
    async fn has(&self, version: &str, file: &str) -> Result<bool> {
        let response = self
            .client
            .get(self.package_url(version, file))
            .send()
            .await
            .context("read the Forgejo package registry")?;
        match response.status() {
            status if status.is_success() => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            status => bail!("the Forgejo package registry answered {status}"),
        }
    }

    /// Create the public owning organization when it does not exist yet.
    /// An installation that closes regular organization creation still lets
    /// a site administrator create one.
    async fn ensure_owner(&self) -> Result<()> {
        let lookup = self
            .client
            .get(format!(
                "{}/api/v1/orgs/{FORGEJO_RELEASE_OWNER}",
                self.origin
            ))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if lookup.status().is_success() {
            return Ok(());
        }
        ensure!(
            lookup.status() == StatusCode::NOT_FOUND,
            "the release organization cannot be read ({})",
            lookup.status()
        );
        let created = self
            .client
            .post(format!("{}/api/v1/orgs", self.origin))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "username": FORGEJO_RELEASE_OWNER,
                "visibility": "public",
                "description": "Mirrored navigator releases for Forgejo Actions runners",
            }))
            .send()
            .await?;
        ensure!(
            created.status() == StatusCode::CREATED || created.status() == StatusCode::CONFLICT,
            "creating the public `{FORGEJO_RELEASE_OWNER}` organization was refused ({})",
            created.status()
        );
        Ok(())
    }

    async fn download(&self, version: &str, file: &str) -> Result<Vec<u8>> {
        let response = self
            .client
            .get(format!("{}/{version}/{file}", self.source))
            .send()
            .await
            .with_context(|| format!("download {file}"))?;
        ensure!(
            response.status().is_success(),
            "release {version} has no {file} ({})",
            response.status()
        );
        Ok(response.bytes().await?.to_vec())
    }

    async fn upload(&self, version: &str, file: &str, bytes: Vec<u8>) -> Result<()> {
        let response = self
            .client
            .put(self.package_url(version, file))
            .bearer_auth(&self.token)
            .body(bytes)
            .send()
            .await
            .with_context(|| format!("upload {file}"))?;
        // 409: an earlier, interrupted mirror already uploaded this file. The
        // registry never replaces a package file, so the digest check on the
        // runner is what proves it.
        ensure!(
            matches!(
                response.status(),
                StatusCode::CREATED | StatusCode::CONFLICT
            ),
            "the package registry refused {file} ({})",
            response.status()
        );
        Ok(())
    }

    /// Mirror `version` when the registry does not already serve both files.
    pub(in crate::devx) async fn ensure(&self, version: &str) -> Result<Mirrored> {
        release_version(version)?;
        let (archive, digest_file) = asset_names(version);
        if self.has(version, &archive).await? && self.has(version, &digest_file).await? {
            return Ok(Mirrored::Present);
        }
        let digest_bytes = self.download(version, &digest_file).await?;
        let expected = expected_digest(&String::from_utf8_lossy(&digest_bytes), &archive)?;
        let archive_bytes = self.download(version, &archive).await?;
        let actual = hex(&Sha256::digest(&archive_bytes));
        ensure!(
            actual == expected,
            "release {version}'s archive does not match its published SHA-256; nothing was uploaded"
        );
        self.ensure_owner().await?;
        // The archive first: a runner that finds the digest file finds the
        // archive it names.
        self.upload(version, &archive, archive_bytes).await?;
        self.upload(version, &digest_file, digest_bytes).await?;
        Ok(Mirrored::Uploaded)
    }
}

/// `navigator ops forgejo release mirror --url <forgejo> --version <tag>`.
pub(in crate::devx) fn run(url: &str, version: &str) -> Result<()> {
    let token = std::env::var("FORGEJO_TOKEN")
        .context("FORGEJO_TOKEN must be configured on the trusted operator host")?;
    let registry = Registry::new(url, token)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    // The operator named the version; echoing a value parsed from the
    // command line trips CodeQL's cleartext-logging rule, so it is not repeated.
    match runtime.block_on(registry.ensure(version))? {
        Mirrored::Present => println!("The release is already mirrored"),
        Mirrored::Uploaded => println!("The release was mirrored and its SHA-256 verified"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_bytes, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const VERSION: &str = "26.11.5";
    const ARCHIVE: &[u8] = b"synthetic archive bytes";

    fn digest_line() -> String {
        format!(
            "{}  navigator-{VERSION}-linux.tar.gz\n",
            hex(&Sha256::digest(ARCHIVE))
        )
    }

    fn package(file: &str) -> String {
        format!("/api/packages/navigator/generic/navigator/{VERSION}/{file}")
    }

    /// `CodeQL` treats every value parsed from the command line as a secret
    /// (`rust/cleartext-logging`); the verb's report must not print one.
    #[test]
    fn the_mirror_report_does_not_echo_the_version_flag() {
        let source = include_str!("release.rs");
        let run = &source[source.find("pub(in crate::devx) fn run(").unwrap()..];
        let run = &run[..run.find("\n}\n").unwrap()];
        for line in run.lines().filter(|line| line.contains("println!")) {
            assert!(!line.contains("{version}"), "{line}");
        }
    }

    #[test]
    fn only_exact_release_tags_are_mirrored() {
        for good in ["26.11.5", "26.11.5-rc.1", "1.2.3-alpha-2"] {
            assert!(release_version(good).is_ok(), "{good}");
        }
        for bad in [
            "latest",
            "26.11",
            "26.11.5-",
            "../26.11.5",
            "26.11.5/x",
            "v26.11.5",
        ] {
            assert!(release_version(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_digest_file_must_name_the_archive_it_covers() {
        let archive = "navigator-26.11.5-linux.tar.gz";
        assert!(expected_digest(&digest_line(), archive).is_ok());
        assert!(expected_digest(&digest_line(), "navigator-26.11.5-macos.tar.gz").is_err());
        assert!(expected_digest("short  navigator-26.11.5-linux.tar.gz", archive).is_err());
    }

    #[tokio::test]
    async fn a_present_release_is_not_downloaded_again() {
        let forgejo = MockServer::start().await;
        Mock::given(method("GET"))
            .and(wiremock::matchers::path_regex("^/api/packages/"))
            .respond_with(ResponseTemplate::new(200))
            .expect(2)
            .mount(&forgejo)
            .await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(201))
            .expect(0)
            .mount(&forgejo)
            .await;
        let registry = Registry::new(&forgejo.uri(), "operator".into())
            .unwrap()
            .with_source("https://github.invalid/");
        assert_eq!(registry.ensure(VERSION).await.unwrap(), Mirrored::Present);
    }

    #[tokio::test]
    async fn a_missing_release_is_verified_then_uploaded_archive_first() {
        let forgejo = MockServer::start().await;
        let github = MockServer::start().await;
        Mock::given(method("GET"))
            .and(wiremock::matchers::path_regex("^/api/packages/"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&forgejo)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/{VERSION}/navigator-{VERSION}-linux.tar.gz.sha256"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_string(digest_line()))
            .mount(&github)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/{VERSION}/navigator-{VERSION}-linux.tar.gz")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(ARCHIVE))
            .mount(&github)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/orgs/navigator"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&forgejo)
            .await;
        Mock::given(method("PUT"))
            .and(path(package(&format!("navigator-{VERSION}-linux.tar.gz"))))
            .and(header("authorization", "Bearer operator"))
            .and(body_bytes(ARCHIVE))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&forgejo)
            .await;
        Mock::given(method("PUT"))
            .and(path(package(&format!(
                "navigator-{VERSION}-linux.tar.gz.sha256"
            ))))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&forgejo)
            .await;
        let registry = Registry::new(&forgejo.uri(), "operator".into())
            .unwrap()
            .with_source(&github.uri());
        assert_eq!(registry.ensure(VERSION).await.unwrap(), Mirrored::Uploaded);
    }

    #[tokio::test]
    async fn a_tampered_archive_uploads_nothing() {
        let forgejo = MockServer::start().await;
        let github = MockServer::start().await;
        Mock::given(method("GET"))
            .and(wiremock::matchers::path_regex("^/api/packages/"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&forgejo)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/{VERSION}/navigator-{VERSION}-linux.tar.gz.sha256"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_string(digest_line()))
            .mount(&github)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/{VERSION}/navigator-{VERSION}-linux.tar.gz")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"other bytes".to_vec()))
            .mount(&github)
            .await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(201))
            .expect(0)
            .mount(&forgejo)
            .await;
        let registry = Registry::new(&forgejo.uri(), "operator".into())
            .unwrap()
            .with_source(&github.uri());
        assert!(registry.ensure(VERSION).await.is_err());
    }

    #[test]
    fn credentials_and_cleartext_origins_are_refused() {
        for origin in [
            "http://forge.example.test",
            "https://user:secret@forge.example.test",
            "https://forge.example.test?token=secret",
        ] {
            assert!(
                Registry::new(origin, "operator".into()).is_err(),
                "{origin}"
            );
        }
    }
}
