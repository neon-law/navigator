//! The real command treats locally cached roles as informational.

use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn promotion_checks_server_authorization_before_local_access() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app/api/ephemeris/sources"))
        .and(header("authorization", "Bearer synthetic-lower-role-token"))
        .respond_with(ResponseTemplate::new(403))
        .expect(1)
        .mount(&server)
        .await;
    let parent = tempfile::tempdir().unwrap();
    let credentials = parent.path().join("credentials.json");
    std::fs::write(
        &credentials,
        serde_json::to_vec(&serde_json::json!({"hosts": {server.uri(): {
            "token": "synthetic-lower-role-token", "role": "owner", "expires_at": i64::MAX
        }}}))
        .unwrap(),
    )
    .unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_scorpio"))
        .args(["promote", "--host", &server.uri()])
        .args(["--bundle", "previous", "--expect-current", "absent"])
        .args(["--from", "2000-01-01", "--to-exclusive", "2000-01-02"])
        .arg("--release-dir")
        .arg(parent.path().join("missing-root"))
        .arg("--source-dir")
        .arg(parent.path().join("missing-source"))
        .arg("--duckdb-bin")
        .arg(parent.path().join("missing-tool"))
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("HTTP 403"), "{stderr}");
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn tampered_cached_owner_role_cannot_authorize_reference_acquisition() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app/api/ephemeris/sources"))
        .and(header("authorization", "Bearer synthetic-lower-role-token"))
        .respond_with(ResponseTemplate::new(403))
        .expect(1)
        .mount(&server)
        .await;
    let parent = tempfile::tempdir().unwrap();
    let credentials = parent.path().join("credentials.json");
    std::fs::write(
        &credentials,
        serde_json::to_vec(&serde_json::json!({"hosts": {server.uri(): {
            "token": "synthetic-lower-role-token", "role": "owner", "expires_at": i64::MAX
        }}}))
        .unwrap(),
    )
    .unwrap();
    let destination = parent.path().join("reference");
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_scorpio"))
        .args(["download", "--host", &server.uri(), "--dir"])
        .arg(&destination)
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("HTTP 403"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-lower-role-token"));
    assert!(!destination.exists());
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 1);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn promotion_verifies_native_bundles_and_keeps_the_prior_release_on_refusal() {
    use std::os::unix::fs::MetadataExt;

    let Some(kernel_root) = std::env::var_os("NAVIGATOR_EPHEMERIS_DIR") else {
        assert_ne!(
            std::env::var("NAVIGATOR_REQUIRE_TABLE_BUNDLE_KERNELS").as_deref(),
            Ok("1"),
            "required DE440 source unavailable"
        );
        return;
    };
    let duckdb = std::env::var_os("NAVIGATOR_TEST_DUCKDB_BIN");
    if duckdb.is_none() {
        assert_ne!(
            std::env::var("NAVIGATOR_REQUIRE_DUCKDB").as_deref(),
            Ok("1")
        );
    }

    let (server, parent) = promotion_fixture(std::path::Path::new(&kernel_root)).await;
    let host = server.uri();
    let release = parent.path().join("release");
    let command = |operation: &str, bundle: &str, expected: &str| {
        promotion_command(
            parent.path(),
            &host,
            duckdb.as_deref(),
            operation,
            bundle,
            expected,
        )
    };
    let success = |output: std::process::Output| {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    for name in ["old", "native"] {
        success(command("tables", name, "").output().await.unwrap());
    }
    success(command("promote", "old", "absent").output().await.unwrap());
    let latest = release.join("latest-validated.json");
    let pointer = || {
        let meta = std::fs::metadata(&latest).unwrap();
        (
            std::fs::read(&latest).unwrap(),
            meta.mode(),
            meta.modified().unwrap(),
            meta.ino(),
        )
    };
    let original = pointer();
    let old: serde_json::Value = serde_json::from_slice(&original.0).unwrap();
    let old_id = old["release_id"].as_str().unwrap();
    success(command("promote", "old", "absent").output().await.unwrap());
    assert_eq!(pointer(), original);

    let valid_receipt = refuse_incomplete_or_corrupt(
        &release,
        old_id,
        duckdb.is_some(),
        command,
        pointer,
        &original,
    )
    .await;
    let native = release.join("native");
    assert_refused(
        &command("promote", "native", "absent")
            .output()
            .await
            .unwrap(),
        "expected current release differs",
    );
    assert_eq!(pointer(), original);
    success(command("promote", "native", old_id).output().await.unwrap());
    let next = pointer();
    let promoted: serde_json::Value = serde_json::from_slice(&next.0).unwrap();
    let next_id = promoted["release_id"].as_str().unwrap();
    assert_ne!(next_id, old_id);
    assert_eq!(
        promoted["reference"]["snapshot_id"],
        old["reference"]["snapshot_id"]
    );
    assert_eq!(promoted["bundle"], "native");
    assert_receipt_hashes(&promoted, &native, &valid_receipt, duckdb.is_some());
    success(command("promote", "old", next_id).output().await.unwrap());
    assert_eq!(pointer().0, original.0);

    std::fs::write(&latest, br#"{"schema_version":999}"#).unwrap();
    let malformed = pointer();
    assert_refused(
        &command("promote", "old", old_id).output().await.unwrap(),
        "invalid latest-validated manifest",
    );
    assert_eq!(pointer(), malformed);
    assert!(!std::fs::read_dir(&release).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".latest-validated-")
    }));
}

fn assert_refused(output: &std::process::Output, message: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success() && stderr.contains(message),
        "{stderr}"
    );
}

fn promotion_command(
    root: &std::path::Path,
    host: &str,
    duckdb: Option<&std::ffi::OsStr>,
    operation: &str,
    bundle: &str,
    expected: &str,
) -> tokio::process::Command {
    let source = root.join("source");
    let release = root.join("release");
    let credentials = root.join("credentials.json");
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_scorpio"));
    command
        .args([operation, "--host", host])
        .args(["--from", "2000-01-01", "--to-exclusive", "2000-01-02"])
        .arg("--source-dir")
        .arg(&source)
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials);
    if operation == "tables" {
        command.arg("--dir").arg(release.join(bundle));
    } else {
        command
            .args(["--bundle", bundle, "--expect-current", expected])
            .arg("--release-dir")
            .arg(&release);
    }
    if bundle == "native" {
        command.arg("--iceberg");
        if let Some(binary) = duckdb {
            command.arg("--duckdb-bin").arg(binary);
        }
    }
    command
}

async fn promotion_fixture(kernel_root: &std::path::Path) -> (MockServer, tempfile::TempDir) {
    let server = MockServer::start().await;
    let host = server.uri();
    let parent = tempfile::tempdir().unwrap();
    let credentials = parent.path().join("credentials.json");
    std::fs::write(
        &credentials,
        serde_json::to_vec(&serde_json::json!({"hosts": {&host: {
            "token": "synthetic-admin-token", "role": "client", "expires_at": i64::MAX
        }}}))
        .unwrap(),
    )
    .unwrap();
    let mut manifest: ephemeris::EphemerisManifest =
        serde_json::from_str(include_str!("../../ephemeris/manifest.json")).unwrap();
    manifest.files.retain(|file| file.name == "de440.bsp");
    Mock::given(method("GET"))
        .and(path("/app/api/ephemeris/sources"))
        .and(header("authorization", "Bearer synthetic-admin-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&manifest))
        .mount(&server)
        .await;

    let source = parent.path().join("source");
    let release = parent.path().join("release");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&release).unwrap();
    std::fs::hard_link(kernel_root.join("de440.bsp"), source.join("de440.bsp")).unwrap();
    std::fs::write(
        source.join("source-manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    (server, parent)
}

type PointerState = (Vec<u8>, u32, std::time::SystemTime, u64);

async fn refuse_incomplete_or_corrupt(
    release: &std::path::Path,
    old_id: &str,
    with_duckdb: bool,
    command: impl Fn(&str, &str, &str) -> tokio::process::Command,
    pointer: impl Fn() -> PointerState,
    original: &PointerState,
) -> Vec<u8> {
    use std::os::unix::fs::symlink;

    std::fs::create_dir(release.join("partial")).unwrap();
    symlink("old", release.join("linked")).unwrap();
    for (name, reason) in [
        ("partial", "not a complete reference bundle"),
        ("linked", "open existing reference bundle"),
        ("../old", "single ASCII directory name"),
    ] {
        let output = command("promote", name, old_id).output().await.unwrap();
        assert_refused(&output, reason);
        assert_eq!(&pointer(), original);
    }

    let native = release.join("native");
    let receipt = native.join("iceberg-manifest.json");
    let valid_receipt = std::fs::read(&receipt).unwrap();
    std::fs::write(&receipt, b"{}").unwrap();
    assert!(!command("promote", "native", old_id)
        .output()
        .await
        .unwrap()
        .status
        .success());
    assert_eq!(&pointer(), original);
    std::fs::write(&receipt, &valid_receipt).unwrap();

    if with_duckdb {
        let database = native.join("reference.duckdb");
        let valid_database = std::fs::read(&database).unwrap();
        std::fs::write(&database, b"corrupt").unwrap();
        assert!(!command("promote", "native", old_id)
            .output()
            .await
            .unwrap()
            .status
            .success());
        assert_eq!(&pointer(), original);
        std::fs::write(&database, valid_database).unwrap();
    }
    valid_receipt
}

fn assert_receipt_hashes(
    promoted: &serde_json::Value,
    native: &std::path::Path,
    valid_receipt: &[u8],
    with_duckdb: bool,
) {
    use sha2::{Digest, Sha256};

    let digest = |bytes: &[u8]| {
        use std::fmt::Write;
        Sha256::digest(bytes)
            .iter()
            .fold(String::new(), |mut hex, byte| {
                write!(hex, "{byte:02x}").unwrap();
                hex
            })
    };
    assert_eq!(
        promoted["reference"]["iceberg_sha256"],
        digest(valid_receipt)
    );
    if with_duckdb {
        assert_eq!(
            promoted["reference"]["duckdb_sha256"],
            digest(&std::fs::read(native.join("duckdb-manifest.json")).unwrap()),
        );
    }
}
