//! Run a Project's gate with the release declared in its manifest.

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{anyhow, Context as _, Result};
use sha2::{Digest as _, Sha256};

/// The mutually exclusive gate modes admitted by the command parser.
pub(crate) enum GateMode {
    TrackedFiles,
    Content { ci: bool, check: bool, deep: bool },
}

/// Select the manifest's release for every gate mode before any content pass.
pub(crate) async fn dispatch(mode: GateMode) -> ExitCode {
    let (ci, check, deep, tracked_files) = match mode {
        GateMode::TrackedFiles => (false, false, false, true),
        GateMode::Content { ci, check, deep } => (ci, check, deep, false),
    };
    let root = match crate::gate_root() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("navigator: {error}");
            return ExitCode::from(2);
        }
    };
    let arguments: Vec<_> = [
        (ci, "--ci"),
        (check, "--check"),
        (deep, "--deep"),
        (tracked_files, "--tracked-files"),
    ]
    .into_iter()
    .filter_map(|(enabled, flag)| enabled.then_some(flag))
    .collect();
    match run(&root, crate::published_cli_version(), &arguments).await {
        Ok(Some(outcome)) => outcome,
        Ok(None) if tracked_files => crate::run_tracked_files_gate(),
        Ok(None) => crate::run_gate(ci, check, deep).await,
        Err(error) => {
            eprintln!("navigator: pinned Project gate is unavailable: {error:#}");
            ExitCode::from(2)
        }
    }
}

/// Delegate before the installed release reads or fixes Project content. A
/// matching release (or a repository without a pin) keeps the normal gate.
async fn run(root: &Path, running_release: &str, arguments: &[&str]) -> Result<Option<ExitCode>> {
    let manifest = root.join(super::manifest::FILE);
    let contents = match std::fs::read_to_string(&manifest) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("read navigator.yaml for the gate release"),
    };
    // Read only the version: a newer manifest schema must not reject content
    // before the pinned release has a chance to check its own schema.
    let Some(pin) =
        rules::project_manifest::pinned_version(&contents).map_err(anyhow::Error::msg)?
    else {
        return Ok(None);
    };
    if !crate::devx::registry::is_release_tag(&pin) {
        return Err(anyhow!(
            "navigator.yaml version `{pin}` must be an exact Navigator release tag"
        ));
    }
    if pin == running_release {
        return Ok(None);
    }
    let platform = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "macos",
        ("linux", "x86_64") => "linux",
        (os, arch) => return Err(anyhow!("no pinned Navigator release for {os}/{arch}")),
    };
    let binary = cache_root()?
        .join("navigator/releases")
        .join(&pin)
        .join(platform)
        .join("navigator");
    if !binary.exists() {
        eprintln!("navigator: downloading gate release {pin}");
        download(&pin, platform, &binary).await?;
    }
    verify_version(&binary, &pin).await?;
    let status = tokio::process::Command::new(&binary)
        .args(["project", "gate"])
        .args(arguments)
        .current_dir(root)
        .env_remove("NAVIGATOR_RELEASE_TAG")
        .status()
        .await
        .with_context(|| format!("run gate release {pin} at {}", binary.display()))?;
    let code = status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(1);
    Ok(Some(ExitCode::from(code)))
}

fn cache_root() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_CACHE_HOME").filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    std::env::var_os("HOME")
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(path).join(".cache"))
        .ok_or_else(|| anyhow!("set XDG_CACHE_HOME or HOME to cache the pinned gate release"))
}

async fn verify_version(binary: &Path, pin: &str) -> Result<()> {
    let output = tokio::process::Command::new(binary)
        .arg("--version")
        .env_remove("NAVIGATOR_RELEASE_TAG")
        .output()
        .await
        .with_context(|| format!("check gate release at {}", binary.display()))?;
    if !output.status.success()
        || String::from_utf8_lossy(&output.stdout).trim() != format!("navigator {pin}")
    {
        return Err(anyhow!(
            "{} does not report navigator {pin}; remove this cached executable and retry",
            binary.display()
        ));
    }
    Ok(())
}

async fn download(pin: &str, platform: &str, destination: &Path) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .user_agent("navigator-cli")
        .build()?;
    download_from(
        &client,
        crate::lsp_download::RELEASE_DOWNLOAD_BASE,
        pin,
        platform,
        destination,
    )
    .await
}

async fn download_from(
    client: &reqwest::Client,
    release_base: &str,
    pin: &str,
    platform: &str,
    destination: &Path,
) -> Result<()> {
    let asset = format!("navigator-{pin}-{platform}.tar.gz");
    let base = format!("{release_base}/{pin}/{asset}");
    let checksum = client
        .get(format!("{base}.sha256"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await
        .context("read the pinned release checksum")?;
    let bytes = client
        .get(&base)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    verify_checksum(&bytes, &checksum, &asset)?;
    let executable = extract_executable(&bytes)?;
    let parent = destination
        .parent()
        .context("pinned release cache has no parent")?;
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&executable)?;
    temporary
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o755))?;
    temporary.as_file().sync_all()?;
    let temporary = temporary.into_temp_path();
    verify_version(&temporary, pin).await?;
    match temporary.persist_noclobber(destination) {
        Ok(()) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.error).context("publish the pinned gate executable"),
    }
}

fn sha256(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

fn verify_checksum(bytes: &[u8], checksum: &str, asset: &str) -> Result<()> {
    let fields: Vec<_> = checksum.split_whitespace().collect();
    let actual = sha256(bytes);
    if fields.len() != 2 || fields[0] != actual || fields[1].trim_start_matches('*') != asset {
        return Err(anyhow!(
            "checksum verification failed for pinned gate archive {asset}"
        ));
    }
    Ok(())
}

fn extract_executable(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()?.as_ref() == Path::new("navigator") && entry.header().entry_type().is_file()
        {
            let mut executable = Vec::new();
            std::io::copy(&mut entry, &mut executable)?;
            return Ok(executable);
        }
    }
    Err(anyhow!(
        "the pinned gate archive contains no navigator executable"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_manifest_or_matching_pin_keeps_the_installed_gate() {
        let temporary = tempfile::tempdir().unwrap();
        assert!(run(temporary.path(), "26.10.24", &[])
            .await
            .unwrap()
            .is_none());
        std::fs::write(
            temporary.path().join("navigator.yaml"),
            "version: 26.10.24\n",
        )
        .unwrap();
        assert!(run(temporary.path(), "26.10.24", &[])
            .await
            .unwrap()
            .is_none());
        std::fs::write(
            temporary.path().join("navigator.yaml"),
            "project: synthetic\n",
        )
        .unwrap();
        assert!(run(temporary.path(), "26.10.24", &[])
            .await
            .unwrap()
            .is_none());
    }

    #[test]
    fn checksum_must_match_both_bytes_and_exact_archive_name() {
        let bytes = b"synthetic archive";
        let digest = sha256(bytes);
        let checksum = format!("{digest}  navigator-26.10.24-macos.tar.gz\n");
        assert!(verify_checksum(bytes, &checksum, "navigator-26.10.24-macos.tar.gz").is_ok());
        assert!(verify_checksum(b"changed", &checksum, "navigator-26.10.24-macos.tar.gz").is_err());
        assert!(verify_checksum(bytes, &checksum, "another.tar.gz").is_err());
    }

    #[test]
    fn extraction_ignores_other_archive_members() {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut archive = tar::Builder::new(encoder);
        for (name, bytes) in [
            ("LICENSE", b"license".as_slice()),
            ("navigator", b"executable".as_slice()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            archive.append_data(&mut header, name, bytes).unwrap();
        }
        let bytes = archive.into_inner().unwrap().finish().unwrap();
        assert_eq!(extract_executable(&bytes).unwrap(), b"executable");
        assert!(extract_executable(b"invalid archive").is_err());
    }

    #[tokio::test]
    async fn unavailable_release_fails_without_installing_a_fallback() {
        let server = wiremock::MockServer::start().await;
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("navigator");
        let result = download_from(
            &reqwest::Client::new(),
            &server.uri(),
            "26.10.24",
            "macos",
            &destination,
        )
        .await;
        assert!(result.is_err());
        assert!(!destination.exists());
    }

    #[tokio::test]
    async fn corrupted_archive_is_rejected_before_extracting_or_running_it() {
        use wiremock::{matchers::path, Mock, ResponseTemplate};
        let server = wiremock::MockServer::start().await;
        let asset = "navigator-26.10.24-macos.tar.gz";
        Mock::given(path(format!("/26.10.24/{asset}.sha256")))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(format!("{}  {asset}", "0".repeat(64))),
            )
            .mount(&server)
            .await;
        Mock::given(path(format!("/26.10.24/{asset}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"corrupt archive"))
            .mount(&server)
            .await;
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("navigator");
        let error = download_from(
            &reqwest::Client::new(),
            &server.uri(),
            "26.10.24",
            "macos",
            &destination,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("checksum verification failed"));
        assert!(!destination.exists());
    }

    #[tokio::test]
    async fn a_verified_download_is_published_as_an_executable() {
        use wiremock::{matchers::path, Mock, ResponseTemplate};
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut archive = tar::Builder::new(encoder);
        let script = b"#!/bin/sh\nprintf 'navigator 26.10.24\\n'\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(script.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(&mut header, "navigator", script.as_slice())
            .unwrap();
        let bytes = archive.into_inner().unwrap().finish().unwrap();
        let server = wiremock::MockServer::start().await;
        let asset = "navigator-26.10.24-macos.tar.gz";
        Mock::given(path(format!("/26.10.24/{asset}.sha256")))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(format!("{}  {asset}", sha256(&bytes))),
            )
            .mount(&server)
            .await;
        Mock::given(path(format!("/26.10.24/{asset}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
            .mount(&server)
            .await;
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("releases/navigator");
        download_from(
            &reqwest::Client::new(),
            &server.uri(),
            "26.10.24",
            "macos",
            &destination,
        )
        .await
        .unwrap();
        verify_version(&destination, "26.10.24").await.unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), script);
    }
}
