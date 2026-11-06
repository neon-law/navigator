//! Authenticated, immutable local acquisition of the eligible raw JPL source.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use clap::Subcommand;
use ephemeris::{EphemerisFile, EphemerisManifest};
use sha2::{Digest, Sha256};

const MAX_CATALOGUE_BYTES: usize = 65_536;
const MANIFEST_NAME: &str = "source-manifest.json";

#[derive(Subcommand)]
pub(crate) enum EphemerisCmd {
    /// Generate verified local Parquet and CSV reference files from a raw DE440 bundle.
    Tables {
        /// Logged-in deployment; its server authorizes Owner/Admin before local reads.
        #[arg(long)]
        host: Option<String>,
        /// Verified immutable raw DE440 bundle produced by ephemeris download.
        #[arg(long)]
        source_dir: PathBuf,
        /// First included civil midnight (UT1 before 1972, UTC thereafter).
        #[arg(long)]
        from: chrono::NaiveDate,
        /// First excluded civil midnight; at most 366 days per bundle.
        #[arg(long)]
        to_exclusive: chrono::NaiveDate,
        /// New immutable completed table bundle; existing paths are never replaced.
        #[arg(long)]
        dir: PathBuf,
    },
    /// Acquire the pinned raw DE440 kernel into a new immutable local bundle.
    Download {
        /// Logged-in Navigator deployment; its server authorizes Owner/Admin access.
        #[arg(long)]
        host: Option<String>,
        /// New completed bundle directory. Existing paths are never replaced.
        #[arg(long)]
        dir: PathBuf,
    },
}

pub(crate) async fn run(action: EphemerisCmd) -> ExitCode {
    let result = match action {
        EphemerisCmd::Download { host, dir } => {
            async {
                let (base, token) = crate::remote::resolve(host.as_deref())?;
                acquire(&base, &token, &dir).await
            }
            .await
        }
        EphemerisCmd::Tables {
            host,
            source_dir,
            from,
            to_exclusive,
            dir,
        } => {
            async {
                let (base, token) = crate::remote::resolve(host.as_deref())?;
                let manifest = catalogue(&base, &token).await?;
                crate::ephemeris_tables::publish(&source_dir, &manifest, from, to_exclusive, &dir)
            }
            .await
        }
    };
    match result {
        Ok(()) => {
            println!("Verified local ephemeris bundle ready.");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Ephemeris operation failed: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(600))
        .build()
        .context("build bounded reference HTTP client")
}

pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}

pub(crate) fn expected_manifest() -> Result<EphemerisManifest> {
    let mut manifest: EphemerisManifest =
        serde_json::from_str(include_str!("../../ephemeris/manifest.json"))?;
    manifest.files.retain(|file| file.name == "de440.bsp");
    manifest.validate()?;
    ensure!(
        manifest.files.len() == 1,
        "eligible DE440 source is unavailable"
    );
    Ok(manifest)
}

async fn catalogue(base: &str, token: &str) -> Result<EphemerisManifest> {
    // This client never handles source requests. The bearer is scoped to this one request.
    let mut response = client()?
        .get(format!("{base}/app/api/ephemeris/sources"))
        .bearer_auth(token)
        .send()
        .await
        .context("request authorized reference catalogue")?;
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        "reference catalogue authorization failed (HTTP {})",
        response.status().as_u16()
    );
    ensure!(
        response
            .content_length()
            .is_none_or(|n| n <= MAX_CATALOGUE_BYTES as u64),
        "reference catalogue exceeds its byte limit"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.context("read reference catalogue")? {
        ensure!(
            chunk.len() <= MAX_CATALOGUE_BYTES.saturating_sub(bytes.len()),
            "reference catalogue exceeds its byte limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    let received: serde_json::Value =
        serde_json::from_slice(&bytes).context("parse reference catalogue")?;
    let expected = expected_manifest()?;
    // Equality includes field inventory, URLs, source identity, retrieval provenance and pins.
    ensure!(
        received == serde_json::to_value(&expected)?,
        "reference catalogue differs from the eligible local pin"
    );
    Ok(expected)
}

async fn acquire(base: &str, token: &str, dir: &Path) -> Result<()> {
    let manifest = catalogue(base, token).await?;
    // Authorization precedes even an idempotent local read. The source client has no credentials.
    acquire_manifest(&client()?, &manifest, dir).await
}

pub(crate) fn open_regular(path: &Path) -> Result<std::fs::File> {
    let descriptor = rustix::fs::openat(
        rustix::fs::CWD,
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )
    .context("open ordinary bundle file without following links")?;
    let file = std::fs::File::from(descriptor);
    ensure!(
        file.metadata()?.is_file(),
        "bundle entry is not an ordinary file"
    );
    Ok(file)
}

fn verify_file(path: &Path, expected: &EphemerisFile) -> Result<()> {
    let mut file = open_regular(path)?;
    ensure!(
        file.metadata()?.len() == expected.bytes,
        "kernel byte count differs from its pin"
    );
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = vec![0_u8; 65_536];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        ensure!(
            total <= expected.bytes,
            "kernel exceeds its pinned byte count"
        );
        hasher.update(&buffer[..read]);
    }
    ensure!(
        total == expected.bytes && hex_digest(&hasher.finalize()) == expected.sha256,
        "kernel checksum differs from its pin"
    );
    Ok(())
}

pub(crate) fn verify_bundle(dir: &Path, manifest: &EphemerisManifest) -> Result<()> {
    ensure!(
        std::fs::symlink_metadata(dir)?.file_type().is_dir(),
        "existing bundle is not an ordinary directory"
    );
    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .take(3)
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<_>>()?;
    entries.sort();
    let mut expected_names = vec![
        std::ffi::OsString::from(MANIFEST_NAME),
        std::ffi::OsString::from(&manifest.files[0].name),
    ];
    expected_names.sort();
    ensure!(
        entries == expected_names,
        "existing directory is not this completed source bundle"
    );
    let file = open_regular(&dir.join(MANIFEST_NAME))?;
    ensure!(
        file.metadata()?.len() <= MAX_CATALOGUE_BYTES as u64,
        "local source manifest exceeds its limit"
    );
    let mut bytes = Vec::new();
    file.take((MAX_CATALOGUE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_CATALOGUE_BYTES,
        "local source manifest exceeds its limit"
    );
    let saved: serde_json::Value = serde_json::from_slice(&bytes)?;
    ensure!(
        saved == serde_json::to_value(manifest)?,
        "existing source manifest differs from its pin"
    );
    verify_file(&dir.join(&manifest.files[0].name), &manifest.files[0])
}

async fn acquire_manifest(
    source_client: &reqwest::Client,
    manifest: &EphemerisManifest,
    dir: &Path,
) -> Result<()> {
    manifest.validate()?;
    ensure!(
        manifest.files.len() == 1,
        "one eligible raw source is required"
    );
    let name = dir
        .file_name()
        .context("choose a new bundle directory name")?;
    let parent = dir
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .context("open existing bundle parent directory")?;
    let destination = parent.join(name);
    match std::fs::symlink_metadata(&destination) {
        Ok(_) => return verify_bundle(&destination, manifest),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect bundle destination"),
    }
    let stage = tempfile::Builder::new()
        .prefix(".navigator-ephemeris-")
        .tempdir_in(&parent)?;
    let expected = &manifest.files[0];
    let mut response = source_client
        .get(&expected.source)
        .send()
        .await
        .context("request pinned JPL source")?;
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        "JPL source request refused (HTTP {})",
        response.status().as_u16()
    );
    ensure!(
        response
            .content_length()
            .is_none_or(|n| n == expected.bytes),
        "source byte count differs from its pin"
    );
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(stage.path().join(&expected.name))?;
    let mut total = 0_u64;
    let mut hasher = Sha256::new();
    while let Some(chunk) = response.chunk().await.context("read pinned JPL source")? {
        ensure!(
            chunk.len() as u64 <= expected.bytes.saturating_sub(total),
            "source exceeds its pinned byte count"
        );
        total += chunk.len() as u64;
        hasher.update(&chunk);
        file.write_all(&chunk)?;
    }
    ensure!(
        total == expected.bytes && hex_digest(&hasher.finalize()) == expected.sha256,
        "source checksum or byte count differs from its pin"
    );
    file.sync_all()?;
    drop(file);
    let mut receipt = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(stage.path().join(MANIFEST_NAME))?;
    receipt.write_all(&serde_json::to_vec_pretty(manifest)?)?;
    receipt.sync_all()?;
    drop(receipt);
    verify_bundle(stage.path(), manifest)?;
    std::fs::File::open(stage.path())?.sync_all()?;
    crate::atomic_directory::publish_no_clobber(stage.path(), &destination)?;
    let _published_stage = stage.keep();
    std::fs::File::open(&parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn server_refusal_precedes_transfer_or_local_publication() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/ephemeris/sources"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("reference");
        assert!(
            acquire(&server.uri(), "lower-role-bearer", &dir)
                .await
                .is_err(),
            "an authenticated catalogue refusal must prevent acquisition"
        );
        assert!(!dir.exists());
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
    }
    #[tokio::test]
    async fn invalid_or_unbounded_catalogue_never_creates_a_bundle() {
        for body in [
            b"{truncated".to_vec(),
            vec![b' '; 65_537],
            include_bytes!("../../ephemeris/manifest.json").to_vec(),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/app/api/ephemeris/sources"))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
                .expect(1)
                .mount(&server)
                .await;
            let parent = tempfile::tempdir().unwrap();
            assert!(acquire(
                &server.uri(),
                "admin-bearer",
                &parent.path().join("reference")
            )
            .await
            .is_err());
            assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
        }
    }

    fn fixture_manifest(source: &str, bytes: &[u8]) -> EphemerisManifest {
        let mut manifest = expected_manifest().unwrap();
        let file = &mut manifest.files[0];
        file.source = source.into();
        file.bytes = bytes.len() as u64;
        file.sha256 = hex_digest(&Sha256::digest(bytes));
        manifest
    }

    #[tokio::test]
    async fn verified_bundle_is_immutable_idempotent_and_source_has_no_bearer() {
        let server = MockServer::start().await;
        let bytes = b"synthetic raw reference fixture";
        let manifest = fixture_manifest(&format!("{}/de440.bsp", server.uri()), bytes);
        Mock::given(method("GET"))
            .and(path("/de440.bsp"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.as_slice()))
            .expect(1)
            .mount(&server)
            .await;
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("reference");
        acquire_manifest(&client().unwrap(), &manifest, &destination)
            .await
            .unwrap();
        acquire_manifest(&client().unwrap(), &manifest, &destination)
            .await
            .unwrap();
        assert_eq!(std::fs::read(destination.join("de440.bsp")).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 1);
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].headers.get("authorization").is_none());
        assert!(requests[0].headers.get("cookie").is_none());
        std::fs::write(destination.join("de440.bsp"), b"user changed bytes").unwrap();
        assert!(
            acquire_manifest(&client().unwrap(), &manifest, &destination)
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read(destination.join("de440.bsp")).unwrap(),
            b"user changed bytes"
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn incomplete_corrupt_oversized_and_redirected_sources_leave_no_completed_or_staged_bundle(
    ) {
        let bytes = b"synthetic reference";
        for response in [
            ResponseTemplate::new(200).set_body_bytes(b"x".as_slice()),
            ResponseTemplate::new(200).set_body_bytes(vec![b'x'; bytes.len()]),
            ResponseTemplate::new(200).set_body_bytes(vec![b'x'; bytes.len() + 1]),
            ResponseTemplate::new(302).insert_header("location", "/unexpected"),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/de440.bsp"))
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
            let manifest = fixture_manifest(&format!("{}/de440.bsp", server.uri()), bytes);
            let parent = tempfile::tempdir().unwrap();
            assert!(acquire_manifest(
                &client().unwrap(),
                &manifest,
                &parent.path().join("reference")
            )
            .await
            .is_err());
            assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn catalogue_redirect_never_forwards_bearer_or_creates_paths() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/ephemeris/sources"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/unexpected"))
            .expect(1)
            .mount(&server)
            .await;
        let parent = tempfile::tempdir().unwrap();
        assert!(acquire(
            &server.uri(),
            "secret-test-bearer",
            &parent.path().join("reference")
        )
        .await
        .is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn existing_empty_user_directory_and_symlink_are_never_replaced() {
        let parent = tempfile::tempdir().unwrap();
        let user = parent.path().join("user");
        std::fs::create_dir(&user).unwrap();
        let manifest = fixture_manifest("http://127.0.0.1:1/no-transfer", b"fixture");
        assert!(acquire_manifest(&client().unwrap(), &manifest, &user)
            .await
            .is_err());
        assert!(user.is_dir());
        assert_eq!(std::fs::read_dir(&user).unwrap().count(), 0);
        let link = parent.path().join("link");
        std::os::unix::fs::symlink(&user, &link).unwrap();
        assert!(acquire_manifest(&client().unwrap(), &manifest, &link)
            .await
            .is_err());
        assert!(std::fs::symlink_metadata(link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 2);
    }

    #[tokio::test]
    async fn changed_pins_paths_urls_versions_and_extra_fields_fail_before_transfer() {
        let expected = serde_json::to_value(expected_manifest().unwrap()).unwrap();
        let mut changed = Vec::new();
        for (pointer, value) in [
            ("/version", serde_json::json!(3)),
            ("/files/0/name", serde_json::json!("../de440.bsp")),
            (
                "/files/0/source",
                serde_json::json!("https://example.com/de440.bsp"),
            ),
            ("/files/0/sha256", serde_json::json!("0".repeat(64))),
            ("/files/0/bytes", serde_json::json!(1)),
        ] {
            let mut manifest = expected.clone();
            *manifest.pointer_mut(pointer).unwrap() = value;
            changed.push(manifest);
        }
        let mut extra = expected;
        extra["unexpected"] = serde_json::json!(true);
        changed.push(extra);
        for manifest in changed {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/app/api/ephemeris/sources"))
                .respond_with(ResponseTemplate::new(200).set_body_json(manifest))
                .expect(1)
                .mount(&server)
                .await;
            let parent = tempfile::tempdir().unwrap();
            assert!(acquire(
                &server.uri(),
                "admin-bearer",
                &parent.path().join("reference")
            )
            .await
            .is_err());
            assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn concurrent_destination_creation_preserves_user_data_and_removes_only_own_stage() {
        let server = MockServer::start().await;
        let bytes = b"synthetic reference";
        let manifest = fixture_manifest(&format!("{}/de440.bsp", server.uri()), bytes);
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("reference");
        let competing = destination.clone();
        Mock::given(method("GET"))
            .and(path("/de440.bsp"))
            .respond_with(move |_: &wiremock::Request| {
                std::fs::create_dir(&competing).unwrap();
                std::fs::write(competing.join("user.txt"), b"preserve me").unwrap();
                ResponseTemplate::new(200).set_body_bytes(bytes.as_slice())
            })
            .expect(1)
            .mount(&server)
            .await;
        assert!(
            acquire_manifest(&client().unwrap(), &manifest, &destination)
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read(destination.join("user.txt")).unwrap(),
            b"preserve me"
        );
        assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 1);
    }
    #[tokio::test]
    async fn completed_bundle_links_or_extra_user_files_are_not_accepted_or_modified() {
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("reference");
        std::fs::create_dir(&destination).unwrap();
        let bytes = b"synthetic reference";
        let manifest = fixture_manifest("http://127.0.0.1:1/no-transfer", bytes);
        std::fs::write(
            destination.join(MANIFEST_NAME),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let original = parent.path().join("original");
        std::fs::write(&original, bytes).unwrap();
        std::os::unix::fs::symlink(&original, destination.join("de440.bsp")).unwrap();
        assert!(
            acquire_manifest(&client().unwrap(), &manifest, &destination)
                .await
                .is_err()
        );
        assert!(std::fs::symlink_metadata(destination.join("de440.bsp"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read(&original).unwrap(), bytes);
        std::fs::remove_file(destination.join("de440.bsp")).unwrap();
        std::fs::write(destination.join("de440.bsp"), bytes).unwrap();
        std::fs::write(destination.join("user.txt"), b"preserve").unwrap();
        assert!(
            acquire_manifest(&client().unwrap(), &manifest, &destination)
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read(destination.join("user.txt")).unwrap(),
            b"preserve"
        );
        assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 3);
    }
    #[tokio::test]
    async fn authorized_catalogue_accepts_the_exact_eligible_pin() {
        let server = MockServer::start().await;
        let expected = expected_manifest().unwrap();
        Mock::given(method("GET"))
            .and(path("/app/api/ephemeris/sources"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer synthetic-admin-token",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(&expected))
            .expect(1)
            .mount(&server)
            .await;
        let actual = catalogue(&server.uri(), "synthetic-admin-token")
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}
