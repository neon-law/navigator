//! Integration coverage for `hudson downloads` — drives the real binary
//! against a wiremock-backed deployment: the scan shape, the LAW-196 slug a
//! proposal carries, filing through the document door with a verified sha256
//! round-trip, the refusals (no login, no visible matter), the digest
//! mismatch that leaves the original in place, and `clean`'s already-filed
//! report. The triage cascade and Trash mechanics are unit-tested in
//! `downloads.rs`.

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn navigator() -> Command {
    let mut command = Command::cargo_bin("hudson").unwrap();
    command.env_remove("GITHUB_REPOSITORY");
    command
}

/// A stored login the binary accepts: the mock deployment's URI as the host
/// key, a bearer token, and an expiry far in the future.
fn credentials(root: &Path, host: &str) -> PathBuf {
    let path = root.join("credentials.json");
    fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "hosts": {
                host: {
                    "token": "test-token",
                    "person_email": "lawyer@example.com",
                    "role": "lawyer",
                    "expires_at": i64::MAX
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Where under `$HOME` the Trash keeps files on this platform — the same
/// root `downloads file` moves originals to.
fn trashed_path(home: &Path, filename: &str) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join(".Trash").join(filename)
    } else {
        home.join(".local/share/Trash/files").join(filename)
    }
}

fn trash_files_dir(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join(".Trash")
    } else {
        home.join(".local/share/Trash/files")
    }
}

/// The pointer JSON a successful upload answers with, carrying the sha256
/// the deployment recorded.
fn pointer_with_sha(asset_id: Uuid, sha256: &str, size_bytes: usize) -> serde_json::Value {
    serde_json::json!({
        "kind": "filing",
        "visibility": "internal",
        "current_version": {
            "version": 1,
            "asset_id": asset_id,
            "created_at": "2026-10-12T12:00:00Z",
            "sha256": sha256,
            "size_bytes": size_bytes
        }
    })
}

/// One document as `GET /app/api/projects/{id}/documents` reports it — the
/// shape `clean` joins local candidates against.
fn live_document(sha256: &str, filename: &str, kind: &str) -> serde_json::Value {
    serde_json::json!({
        "id": Uuid::now_v7(),
        "byte_size": 9,
        "sha256_hex": sha256,
        "filename": filename,
        "kind": kind,
        "visibility": "internal",
        "slug": filename.trim_end_matches(".pdf"),
        "inserted_at": "2026-10-13T12:00:00Z"
    })
}

async fn mount_project_lookup(server: &MockServer, project_id: Uuid) {
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": project_id, "code": "acme"}
        ])))
        .mount(server)
        .await;
}

/// `scan --json` reports one record per candidate with the intake facts a
/// script consumes — digest, detected type, size, age — and no provenance
/// off macOS.
#[test]
fn scan_reports_each_candidate_shape() {
    let downloads = TempDir::new().unwrap();
    fs::write(
        downloads.path().join("2026.10.12_complaint-annulment.pdf"),
        b"complaint",
    )
    .unwrap();
    fs::write(downloads.path().join("notes.txt"), b"notes").unwrap();

    let output = navigator()
        .args(["downloads", "scan", "--dir"])
        .arg(downloads.path())
        .arg("--json")
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let candidates: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(candidates.len(), 2);
    let complaint = candidates
        .iter()
        .find(|candidate| candidate["filename"] == "2026.10.12_complaint-annulment.pdf")
        .unwrap();
    assert_eq!(complaint["sha256"], sha256_hex(b"complaint"));
    assert_eq!(complaint["content_type"], "application/pdf");
    assert_eq!(complaint["size_bytes"], 9);
    assert_eq!(complaint["age_days"], 0);
    assert_eq!(complaint["class"], "unfiled");
    // Off macOS the provenance degrades to unknown, never a guess.
    assert_eq!(complaint["where_froms"], serde_json::Value::Null);
    let notes = candidates
        .iter()
        .find(|candidate| candidate["filename"] == "notes.txt")
        .unwrap();
    assert_eq!(notes["sha256"], sha256_hex(b"notes"));
}

/// `propose` pairs the triage cascade's verdict with a proposed matter
/// (the filename token naming a visible Project code) and the LAW-196 slug.
#[tokio::test]
async fn propose_proposes_the_matter_kind_and_law_196_slug() {
    let server = MockServer::start().await;
    mount_project_lookup(&server, Uuid::now_v7()).await;
    let home = TempDir::new().unwrap();
    let credential_path = credentials(home.path(), &server.uri());

    let downloads = TempDir::new().unwrap();
    fs::write(
        downloads
            .path()
            .join("2026.10.12_complaint-annulment_acme.pdf"),
        b"complaint",
    )
    .unwrap();

    let output = navigator()
        .env("HOME", home.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credential_path)
        .args(["downloads", "propose", "--dir"])
        .arg(downloads.path())
        .args(["--host", &server.uri(), "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let proposals: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(proposals.len(), 1);
    let proposal = &proposals[0];
    assert_eq!(proposal["project"], "acme");
    assert_eq!(proposal["kind"], "unclassified");
    assert_eq!(proposal["slug"], "2026.10.12_complaint-annulment-acme.pdf");
    let evidence = proposal["evidence"].as_array().unwrap();
    assert!(evidence.iter().any(|line| line
        .as_str()
        .unwrap()
        .contains("2026.10.12_complaint-annulment-acme.pdf")));
    assert!(evidence
        .iter()
        .any(|line| line.as_str().unwrap().contains("matter code `acme`")));
}

/// `file` uploads through the document door, verifies the filed pointer's
/// sha256 against the local bytes, and only then moves the original to the
/// Trash.
#[tokio::test]
async fn file_uploads_verifies_and_moves_the_original() {
    let server = MockServer::start().await;
    let project_id = Uuid::now_v7();
    mount_project_lookup(&server, project_id).await;
    let bytes = b"complaint bytes";
    Mock::given(method("POST"))
        .and(path(format!("/app/api/projects/{project_id}/documents")))
        .respond_with(ResponseTemplate::new(201).set_body_json(pointer_with_sha(
            Uuid::now_v7(),
            &sha256_hex(bytes),
            bytes.len(),
        )))
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    let downloads = TempDir::new().unwrap();
    let original = downloads.path().join("2026.10.12_complaint-annulment.pdf");
    fs::write(&original, bytes).unwrap();
    let credential_path = credentials(home.path(), &server.uri());

    navigator()
        .env("HOME", home.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credential_path)
        .args(["downloads", "file"])
        .arg(&original)
        .args([
            "--project",
            "acme",
            "--kind",
            "filing",
            "--host",
            &server.uri(),
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("sha256 verified"));

    assert!(!original.exists(), "a verified file's original moves");
    let trashed = trashed_path(home.path(), "2026.10.12_complaint-annulment.pdf");
    assert_eq!(fs::read(&trashed).unwrap(), bytes);
}

/// Filing without a stored login refuses before anything is uploaded,
/// naming `navigator site login`.
#[tokio::test]
async fn file_refuses_without_credentials() {
    let server = MockServer::start().await;
    let home = TempDir::new().unwrap();
    let downloads = TempDir::new().unwrap();
    let original = downloads.path().join("2026.10.12_complaint-annulment.pdf");
    fs::write(&original, b"complaint bytes").unwrap();

    navigator()
        .env("HOME", home.path())
        // A credentials file that does not exist: no login anywhere.
        .env(
            "NAVIGATOR_CREDENTIALS_FILE",
            home.path().join("absent.json"),
        )
        .args(["downloads", "file"])
        .arg(&original)
        .args([
            "--project",
            "acme",
            "--kind",
            "filing",
            "--host",
            &server.uri(),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("navigator site login"));

    assert!(original.exists(), "a refused file stays put");
}

/// Filing onto a matter the login cannot see refuses, naming the code.
#[tokio::test]
async fn file_refuses_without_a_visible_project() {
    let server = MockServer::start().await;
    mount_project_lookup(&server, Uuid::now_v7()).await;

    let home = TempDir::new().unwrap();
    let downloads = TempDir::new().unwrap();
    let original = downloads.path().join("2026.10.12_complaint-annulment.pdf");
    fs::write(&original, b"complaint bytes").unwrap();
    let credential_path = credentials(home.path(), &server.uri());

    navigator()
        .env("HOME", home.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credential_path)
        .args(["downloads", "file"])
        .arg(&original)
        .args([
            "--project",
            "nowhere",
            "--kind",
            "filing",
            "--host",
            &server.uri(),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "no visible matter with code `nowhere`",
        ));

    assert!(original.exists(), "a refused file stays put");
}

/// A pointer whose sha256 does not match the local bytes aborts the move —
/// the original stays in Downloads.
#[tokio::test]
async fn a_digest_mismatch_aborts_the_move() {
    let server = MockServer::start().await;
    let project_id = Uuid::now_v7();
    mount_project_lookup(&server, project_id).await;
    let bytes = b"complaint bytes";
    // The deployment recorded different bytes than the file carried.
    Mock::given(method("POST"))
        .and(path(format!("/app/api/projects/{project_id}/documents")))
        .respond_with(ResponseTemplate::new(201).set_body_json(pointer_with_sha(
            Uuid::now_v7(),
            &sha256_hex(b"different bytes entirely"),
            bytes.len(),
        )))
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    let downloads = TempDir::new().unwrap();
    let original = downloads.path().join("2026.10.12_complaint-annulment.pdf");
    fs::write(&original, bytes).unwrap();
    let credential_path = credentials(home.path(), &server.uri());

    navigator()
        .env("HOME", home.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credential_path)
        .args(["downloads", "file"])
        .arg(&original)
        .args([
            "--project",
            "acme",
            "--kind",
            "filing",
            "--host",
            &server.uri(),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("was NOT moved to the Trash"));

    assert_eq!(
        fs::read(&original).unwrap(),
        bytes,
        "the original stays put"
    );
    assert!(
        fs::read_dir(trash_files_dir(home.path()))
            .map_or(true, |mut entries| entries.next().is_none()),
        "nothing landed in the Trash"
    );
}

/// `clean --already-filed` reports, per candidate, whether its digest is
/// confirmed on a visible matter — and which matter.
#[tokio::test]
async fn clean_reports_what_is_already_filed() {
    let server = MockServer::start().await;
    let project_id = Uuid::now_v7();
    mount_project_lookup(&server, project_id).await;
    let filed_sha = sha256_hex(b"complaint");
    Mock::given(method("GET"))
        .and(path(format!("/app/api/projects/{project_id}/documents")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([live_document(
                &filed_sha,
                "2026.10.12_complaint-annulment.pdf",
                "pleading"
            )])),
        )
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    let downloads = TempDir::new().unwrap();
    fs::write(
        downloads.path().join("2026.10.12_complaint-annulment.pdf"),
        b"complaint",
    )
    .unwrap();
    fs::write(downloads.path().join("notes.txt"), b"notes").unwrap();
    let credential_path = credentials(home.path(), &server.uri());

    navigator()
        .env("HOME", home.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credential_path)
        .args(["downloads", "clean", "--already-filed", "--dir"])
        .arg(downloads.path())
        .args(["--host", &server.uri()])
        .assert()
        .success()
        .stdout(
            predicates::str::contains(
                "already filed on acme as `2026.10.12_complaint-annulment.pdf` (kind `pleading`)",
            )
            .and(predicates::str::contains(
                "notes.txt` — not filed on any visible matter",
            )),
        );
}
