//! `hudson lsp` — download the platform-matching `navigator-lsp` release
//! archive for this binary's own version and write a working
//! `navigator-lsp` executable into the caller's Downloads directory.
//!
//! Distinct from `hudson ops lsp publish` (`deploy/src/lsp_publish.rs`),
//! which is the operator upload side that mirrors a "latest" key to the
//! public assets bucket for the site's own use — not a resolvable target
//! for a human who just wants the binary. This command instead resolves the
//! exact `navigator-lsp-<tag>-<platform>` archive
//! `.github/workflows/deploy.yml` attaches to the public distribution host for
//! the *running CLI's own tag* (never "latest"), via
//! [`views::lsp::LSP_RELEASE_ARCHIVES`] — the same metadata an editor
//! extension (ENG-447) resolves against at runtime, so the mapping cannot
//! drift between the two.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use views::lsp::{lsp_release_archive_for_host, lsp_release_asset_name};

/// The repository release archives are attached to.
///
/// The public Navigator repository: `deploy.yml` attaches every CLI and LSP
/// archive to its Release, so an anonymous `hudson lsp` resolves there.
const RELEASES_REPO: &str = "neon-law/navigator";

/// `api.github.com`'s own base. A test overrides this with a `wiremock`
/// server's address so no test makes a live network call.
const GITHUB_API_BASE: &str = "https://api.github.com";

/// Entry point for the top-level `hudson lsp` command. `tag` is the
/// running binary's own version (`crate::cli_version()`); `dir` overrides
/// the resolved Downloads directory.
#[must_use]
pub fn run_download(tag: &str, dir: Option<PathBuf>) -> ExitCode {
    let host = (std::env::consts::OS, std::env::consts::ARCH);
    let home = std::env::var("HOME").ok();
    let written = tokio::runtime::Runtime::new()
        .context("tokio runtime")
        .and_then(|runtime| runtime.block_on(download(GITHUB_API_BASE, host, tag, dir, home)));
    match written {
        Ok(path) => {
            println!("navigator: lsp: wrote {}", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("navigator: lsp: {e:#}");
            ExitCode::from(2)
        }
    }
}

/// Resolve the Downloads directory: `$HOME/Downloads`. Pure, taking the env
/// value as an argument, so it is unit-testable without mutating
/// process-global env — the same pattern
/// `credentials::resolve_credentials_path` uses.
pub fn default_downloads_dir(home: Option<String>) -> Result<PathBuf> {
    let base = home.filter(|h| !h.is_empty()).ok_or_else(|| {
        anyhow!("cannot resolve a home directory ($HOME is unset) to find Downloads")
    })?;
    Ok(PathBuf::from(base).join("Downloads"))
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct Release {
    assets: Vec<ReleaseAsset>,
}

/// Look up the exact asset download URL for `tag`. Every failure names the
/// tag and asset it looked for — never a silent fallback to another
/// release, per ENG-867's acceptance criteria.
async fn find_asset_url(
    client: &reqwest::Client,
    api_base: &str,
    repo: &str,
    tag: &str,
    asset_name: &str,
) -> Result<String> {
    let url = format!("{api_base}/repos/{repo}/releases/tags/{tag}");
    let response = client
        .get(&url)
        .header("User-Agent", "navigator-cli")
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(anyhow!("no GitHub Release found for tag `{tag}` in {repo}"));
    }
    let response = response
        .error_for_status()
        .with_context(|| format!("GET {url}"))?;
    let release: Release = response
        .json()
        .await
        .with_context(|| format!("parse the `{tag}` release from {repo}"))?;
    release
        .assets
        .into_iter()
        .find(|asset| asset.name == asset_name)
        .map(|asset| asset.browser_download_url)
        .ok_or_else(|| anyhow!("release `{tag}` in {repo} has no asset named `{asset_name}` — looked for {asset_name}"))
}

/// Download `tag`'s archive for the `(os, arch)` host from the release
/// `api_base` serves, extract the `navigator-lsp` executable from it, and
/// write it into `dir`, or into `home`'s Downloads directory when no `dir`
/// is given. Returns the written path.
async fn download(
    api_base: &str,
    (os, arch): (&str, &str),
    tag: &str,
    dir: Option<PathBuf>,
    home: Option<String>,
) -> Result<PathBuf> {
    let archive = lsp_release_archive_for_host(os, arch).ok_or_else(|| {
        anyhow!(
            "no navigator-lsp release archive is built for this platform \
             ({os}/{arch}) — see docs/lsp/README.md"
        )
    })?;
    let dest_dir = dir.map_or_else(|| default_downloads_dir(home), Ok)?;
    let client = reqwest::Client::new();
    let asset_name = lsp_release_asset_name(tag, archive);
    let download_url = find_asset_url(&client, api_base, RELEASES_REPO, tag, &asset_name).await?;
    let bytes = client
        .get(&download_url)
        .header("User-Agent", "navigator-cli")
        .send()
        .await
        .with_context(|| format!("GET {download_url}"))?
        .error_for_status()
        .with_context(|| format!("GET {download_url}"))?
        .bytes()
        .await
        .with_context(|| format!("read body of {download_url}"))?;
    std::fs::create_dir_all(&dest_dir).with_context(|| format!("create {}", dest_dir.display()))?;
    let dest_path = dest_dir.join(archive.binary_name);
    let executable = extract_from_tar_gz(&bytes, archive.binary_name)?;
    write_executable(&dest_path, &executable)?;
    Ok(dest_path)
}

/// Extract `member`'s bytes out of the downloaded `.tar.gz`.
fn extract_from_tar_gz(bytes: &[u8], member: &str) -> Result<Vec<u8>> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive
        .entries()
        .context("read the downloaded tar.gz archive")?
    {
        let mut entry = entry.context("read a tar.gz archive entry")?;
        if entry.path().context("read a tar.gz entry path")?.to_str() == Some(member) {
            let mut out = Vec::new();
            std::io::copy(&mut entry, &mut out).context("read the archived executable")?;
            return Ok(out);
        }
    }
    Err(anyhow!("`{member}` is not in the downloaded archive"))
}

/// Write `bytes` to `path`, marking it executable on Unix (archives carry no
/// permission bits worth trusting across the zip/tar split above).
fn write_executable(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file =
        std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write {}", path.display()))?;
    set_executable(path)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).with_context(|| format!("chmod {}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// ENG-931: `hudson lsp` must resolve archives from the public
    /// distribution host, never the source tree that is going private.
    #[test]
    fn releases_repo_names_the_tap_not_the_source_tree() {
        assert_eq!(RELEASES_REPO, "neon-law/navigator");
        assert_eq!(
            project_scaffold::RELEASE_DOWNLOAD_BASE,
            format!("https://github.com/{RELEASES_REPO}/releases/download")
        );
    }

    #[test]
    fn downloads_dir_sits_under_home() {
        assert_eq!(
            default_downloads_dir(Some("/home/nick".into())).expect("home resolves"),
            PathBuf::from("/home/nick/Downloads"),
        );
    }

    #[test]
    fn downloads_dir_refuses_a_blank_or_unset_home() {
        assert!(default_downloads_dir(Some(String::new())).is_err());
        let err = default_downloads_dir(None).expect_err("no home to resolve");
        assert!(err.to_string().contains("cannot resolve a home directory"));
    }

    #[tokio::test]
    async fn find_asset_url_returns_the_matching_asset() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/neon-law-source-code/navigator/releases/tags/26.9.23"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "assets": [
                    {
                        "name": "navigator-lsp-26.9.23-linux.tar.gz",
                        "browser_download_url": "https://example.test/navigator-lsp-26.9.23-linux.tar.gz",
                    },
                    {
                        "name": "navigator-26.9.23-linux.tar.gz",
                        "browser_download_url": "https://example.test/navigator-26.9.23-linux.tar.gz",
                    },
                ],
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let url = find_asset_url(
            &client,
            &server.uri(),
            "neon-law-source-code/navigator",
            "26.9.23",
            "navigator-lsp-26.9.23-linux.tar.gz",
        )
        .await
        .expect("asset is found");
        assert_eq!(
            url,
            "https://example.test/navigator-lsp-26.9.23-linux.tar.gz"
        );
    }

    #[tokio::test]
    async fn find_asset_url_names_the_tag_when_no_release_exists() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/repos/neon-law-source-code/navigator/releases/tags/99.9.99",
            ))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let err = find_asset_url(
            &client,
            &server.uri(),
            "neon-law-source-code/navigator",
            "99.9.99",
            "navigator-lsp-99.9.99-linux.tar.gz",
        )
        .await
        .expect_err("no release for this tag");
        assert!(
            err.to_string().contains("99.9.99"),
            "error names the tag it looked for: {err}"
        );
    }

    #[tokio::test]
    async fn find_asset_url_names_the_asset_when_the_release_lacks_it() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/repos/neon-law-source-code/navigator/releases/tags/26.9.23",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "assets": [
                    {
                        "name": "navigator-lsp-26.9.23-macos.tar.gz",
                        "browser_download_url": "https://example.test/navigator-lsp-26.9.23-macos.tar.gz",
                    },
                ],
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let err = find_asset_url(
            &client,
            &server.uri(),
            "neon-law-source-code/navigator",
            "26.9.23",
            "navigator-lsp-26.9.23-linux.tar.gz",
        )
        .await
        .expect_err("the release exists but carries no matching asset");
        assert!(
            err.to_string()
                .contains("navigator-lsp-26.9.23-linux.tar.gz"),
            "error names the asset it looked for: {err}"
        );
    }

    /// A `.tar.gz` archive holding one `member` with `data` as its bytes.
    fn tar_gz(member: &str, data: &[u8]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, member, data)
            .expect("append tar entry");
        let tar_bytes = builder.into_inner().expect("finish tar");
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar_bytes).expect("gzip tar");
        encoder.finish().expect("finish gzip")
    }

    #[test]
    fn extracts_the_named_member_from_a_tar_gz_archive() {
        let gz_bytes = tar_gz("navigator-lsp", b"fake unix binary");
        let extracted = extract_from_tar_gz(&gz_bytes, "navigator-lsp").expect("extract");
        assert_eq!(extracted, b"fake unix binary");
    }

    #[test]
    fn tar_gz_extraction_names_the_missing_member() {
        let gz_bytes = tar_gz("LICENSE", b"unrelated");
        let err = extract_from_tar_gz(&gz_bytes, "navigator-lsp").expect_err("member absent");
        assert!(err.to_string().contains("navigator-lsp"));
        let err = extract_from_tar_gz(b"not an archive", "navigator-lsp").expect_err("not gzip");
        assert!(format!("{err:#}").contains("tar.gz"), "{err:#}");
    }

    const LINUX: (&str, &str) = ("linux", "x86_64");

    /// A release double: tag `26.9.23` carries the Linux archive, whose
    /// download answers `archive` with `status`.
    async fn release(status: u16, archive: Vec<u8>) -> MockServer {
        let server = MockServer::start().await;
        let asset = "navigator-lsp-26.9.23-linux.tar.gz";
        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/{RELEASES_REPO}/releases/tags/26.9.23"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "assets": [{
                    "name": asset,
                    "browser_download_url": format!("{}/download/{asset}", server.uri()),
                }],
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/download/{asset}")))
            .respond_with(ResponseTemplate::new(status).set_body_bytes(archive))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn the_running_tags_archive_lands_as_an_executable_in_downloads() {
        use std::os::unix::fs::PermissionsExt;
        let server = release(200, tar_gz("navigator-lsp", b"fake unix binary")).await;
        let home = tempfile::tempdir().expect("temporary home");
        let home_path = home.path().to_string_lossy().into_owned();

        let written = download(&server.uri(), LINUX, "26.9.23", None, Some(home_path))
            .await
            .expect("the archive downloads");

        assert_eq!(written, home.path().join("Downloads/navigator-lsp"));
        assert_eq!(std::fs::read(&written).unwrap(), b"fake unix binary");
        let mode = std::fs::metadata(&written).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    #[tokio::test]
    async fn a_download_that_cannot_finish_writes_nothing() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let into = || Some(dir.path().join("bin"));
        let cases = [
            (release(500, Vec::new()).await, LINUX, "26.9.23", "500"),
            (
                release(200, tar_gz("LICENSE", b"unrelated")).await,
                LINUX,
                "26.9.23",
                "is not in the downloaded archive",
            ),
            (
                release(200, Vec::new()).await,
                LINUX,
                "99.9.99",
                "no GitHub Release found for tag `99.9.99`",
            ),
            (
                release(200, Vec::new()).await,
                ("macos", "x86_64"),
                "26.9.23",
                "this platform (macos/x86_64)",
            ),
        ];
        for (server, host, tag, reason) in cases {
            let err = download(&server.uri(), host, tag, into(), None)
                .await
                .expect_err("the download is refused");
            assert!(format!("{err:#}").contains(reason), "{reason}: {err:#}");
        }
        assert!(!dir.path().join("bin/navigator-lsp").exists());
        let err = download("http://127.0.0.1:1", LINUX, "26.9.23", None, None)
            .await
            .expect_err("no home, no directory");
        assert!(err.to_string().contains("cannot resolve a home directory"));
    }
}
