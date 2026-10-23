//! `navigator project gate --check` through the compiled binary.
//!
//! The comparisons live in `cli/src/projects/document_check.rs`. These tests
//! pin the wiring: `--help` states the auto-fix, a plain `project gate` makes
//! no request, and `--check` reports a missing object.

use std::fs;
use std::path::Path;
use std::time::Duration;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn navigator() -> Command {
    let mut command = Command::cargo_bin("navigator").unwrap();
    command.env_remove("GITHUB_REPOSITORY");
    // `GITHUB_REF`/`GITHUB_EVENT_NAME` are ambient in a real GitHub Actions
    // job — including the one running *this* test binary — and a pull
    // request's `GITHUB_REF` (`refs/pull/<n>/merge`) satisfies
    // `can_mint_ci_session_for`. Left set, a `--ci` run here reaches
    // `live_status`'s own live-row request against this test's mock server,
    // which no fixture below mocks.
    command.env_remove("GITHUB_REF");
    command.env_remove("GITHUB_EVENT_NAME");
    command
}

fn write(root: &Path, relative: &str, bytes: impl AsRef<[u8]>) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

fn manifest(root: &Path, host: &str) {
    write(
        root,
        "navigator.yaml",
        format!("version: \"26.9.17\"\nproject:\n  host: {host}\n  name: acme\n"),
    );
}

fn credentials(root: &Path, host: &str) -> std::path::PathBuf {
    let path = root.join("credentials.json");
    write(
        root,
        "credentials.json",
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
    );
    path
}

const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn pointer(asset_id: Uuid) -> String {
    format!(
        "kind: filing\nvisibility: internal\ncurrent_version:\n  version: 1\n  \
         asset_id: {asset_id}\n  created_at: \"2026-09-05T12:00:00Z\"\n  sha256: {SHA}\n  \
         size_bytes: 18\n"
    )
}

fn root_with_manifest(host: &str) -> TempDir {
    let root = TempDir::new().unwrap();
    write(root.path(), "README.md", "Synthetic repository\n");
    write(root.path(), ".git", "gitdir: /tmp/synthetic\n");
    manifest(root.path(), host);
    root
}

fn validate_output(root: &Path, credentials: &Path) -> (i32, String, String) {
    let output = navigator()
        .current_dir(root)
        .env("NAVIGATOR_CREDENTIALS_FILE", credentials)
        .arg("validate")
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

fn live_revision(asset_id: Uuid, sha: &str, size: i64, operative: bool) -> serde_json::Value {
    serde_json::json!({
        "version": 1, "asset_id": asset_id, "created_at": "2026-09-05T12:00:00Z",
        "sha256": sha, "size_bytes": size, "filename": "summons.pdf",
        "visibility": "internal", "operative": operative
    })
}

async fn mount_validation_project(server: &MockServer, project_id: Uuid) {
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": project_id, "code": "acme"}
        ])))
        .mount(server)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn validate_skips_live_reads_without_an_exact_current_session() {
    let server = MockServer::start().await;
    let root = root_with_manifest(&server.uri());
    write(
        root.path(),
        "blobs/pleadings/summons.pdf.yaml",
        pointer(Uuid::now_v7()),
    );
    let absent = TempDir::new().unwrap();
    let missing_file = absent.path().join("missing.json");
    let (_, stdout, _) = validate_output(root.path(), &missing_file);
    assert!(stdout.contains("Validated 1 document pointer(s)"));
    assert!(
        stdout.contains("Live document verification skipped")
            && stdout.contains("no stored session")
    );

    let wrong = TempDir::new().unwrap();
    let wrong_host = credentials(wrong.path(), "http://127.0.0.1:1");
    let (_, stdout, _) = validate_output(root.path(), &wrong_host);
    assert!(stdout.contains("no stored session for the manifest host"));

    let expired = TempDir::new().unwrap();
    let expired_path = expired.path().join("credentials.json");
    write(
        expired.path(),
        "credentials.json",
        serde_json::to_vec(&serde_json::json!({
            "hosts": {server.uri(): {"token": "expired", "expires_at": 1}}
        }))
        .unwrap(),
    );
    let (_, stdout, _) = validate_output(root.path(), &expired_path);
    assert!(stdout.contains("has expired"));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[test]
fn validate_still_reports_offline_pointer_errors_without_a_session() {
    let root = root_with_manifest("https://example.test");
    write(
        root.path(),
        "blobs/pleadings/broken.pdf.yaml",
        b"kind: filing\ncurrent_verison: {}\n",
    );
    let creds = TempDir::new().unwrap();
    let (status, stdout, stderr) = validate_output(root.path(), &creds.path().join("missing.json"));
    assert_ne!(status, 0, "{stdout}{stderr}");
    assert!(
        stdout.contains("Validated 1 document pointer(s), found 1 error(s)"),
        "{stdout}{stderr}"
    );
    assert!(
        stdout.contains("Live document verification skipped"),
        "{stdout}{stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn validate_checks_live_pointer_and_reports_drift_without_writing() {
    let server = MockServer::start().await;
    let root = root_with_manifest(&server.uri());
    let creds = TempDir::new().unwrap();
    let credential_path = credentials(creds.path(), &server.uri());
    let project_id = Uuid::now_v7();
    let asset_id = Uuid::now_v7();
    let relative = "blobs/pleadings/summons.pdf.yaml";
    let original = pointer(asset_id);
    write(root.path(), relative, &original);
    mount_validation_project(&server, project_id).await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{project_id}/documents/revisions"
        )))
        .and(query_param("slug", "pleadings/summons.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "kind": "filing", "revisions": [live_revision(asset_id, SHA, 18, true)]
        })))
        .mount(&server)
        .await;
    let (_, stdout, stderr) = validate_output(root.path(), &credential_path);
    assert!(
        stdout.contains("Verified 1 document pointer(s) live, found 0 error(s)"),
        "{stdout}{stderr}"
    );
    assert_eq!(
        fs::read_to_string(root.path().join(relative)).unwrap(),
        original
    );
    assert!(server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|request| request.method == wiremock::http::Method::GET));
}

#[tokio::test(flavor = "multi_thread")]
async fn validate_fails_each_attempted_live_drift_and_access_error() {
    for (name, response, expected) in [
        ("missing slug", ResponseTemplate::new(404), "404"),
        ("denied", ResponseTemplate::new(403), "403"),
        ("rejected", ResponseTemplate::new(401), "401"),
        ("missing asset", ResponseTemplate::new(200).set_body_json(serde_json::json!({"kind":"filing","revisions":[]})), "not in the live chain"),
        ("stale", ResponseTemplate::new(200).set_body_json(serde_json::json!({"kind":"filing","revisions":[
            live_revision(Uuid::nil(), SHA, 18, false), live_revision(Uuid::now_v7(), SHA, 18, true)
        ]})), "not the chain's operative revision"),
        ("hash", ResponseTemplate::new(200).set_body_json(serde_json::json!({"kind":"filing","revisions":[
            live_revision(Uuid::nil(), &"f".repeat(64), 18, true)
        ]})), "sha256 mismatch"),
        ("size", ResponseTemplate::new(200).set_body_json(serde_json::json!({"kind":"filing","revisions":[
            live_revision(Uuid::nil(), SHA, 19, true)
        ]})), "size mismatch"),
    ] {
        let server = MockServer::start().await;
        let root = root_with_manifest(&server.uri());
        let creds = TempDir::new().unwrap();
        let credential_path = credentials(creds.path(), &server.uri());
        let project_id = Uuid::now_v7();
        write(root.path(), "blobs/pleadings/summons.pdf.yaml", pointer(Uuid::nil()));
        mount_validation_project(&server, project_id).await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{project_id}/documents/revisions")))
            .respond_with(response)
            .mount(&server).await;
        let (status, stdout, stderr) = validate_output(root.path(), &credential_path);
        assert_ne!(status, 0, "{name}: attempted live failures must fail validation");
        assert!(stderr.contains(expected), "{name}: {stdout}{stderr}");
        assert!(stdout.contains("found 1 error(s)"), "{name}: {stdout}{stderr}");
        assert!(server.received_requests().await.unwrap().iter().all(|request| request.method == wiremock::http::Method::GET));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn validate_with_no_pointers_does_no_document_network_work() {
    let server = MockServer::start().await;
    let root = root_with_manifest(&server.uri());
    let creds = TempDir::new().unwrap();
    let credential_path = credentials(creds.path(), &server.uri());
    let (_, stdout, _) = validate_output(root.path(), &credential_path);
    assert!(stdout.contains("Validated 0 document pointer(s)"));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn validate_non_project_with_a_pointer_makes_no_document_request() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    write(
        root.path(),
        "blobs/example.pdf.yaml",
        pointer(Uuid::now_v7()),
    );
    let creds = TempDir::new().unwrap();
    let credential_path = credentials(creds.path(), &server.uri());
    let (status, stdout, _) = validate_output(root.path(), &credential_path);
    assert_eq!(status, 0, "{stdout}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn validate_fails_an_unreachable_host_after_a_matching_login() {
    let root = root_with_manifest("http://127.0.0.1:9");
    write(
        root.path(),
        "blobs/pleadings/summons.pdf.yaml",
        pointer(Uuid::now_v7()),
    );
    let creds = TempDir::new().unwrap();
    let credential_path = credentials(creds.path(), "http://127.0.0.1:9");
    let (status, stdout, stderr) = validate_output(root.path(), &credential_path);
    assert_ne!(status, 0);
    assert!(
        stderr.contains("live document verification could not read the Project"),
        "{stdout}{stderr}"
    );
    assert!(!stdout.contains("verification skipped"));
}

#[tokio::test(flavor = "multi_thread")]
async fn validate_times_out_instead_of_reporting_an_offline_pass() {
    let server = MockServer::start().await;
    let root = root_with_manifest(&server.uri());
    write(
        root.path(),
        "blobs/pleadings/summons.pdf.yaml",
        pointer(Uuid::now_v7()),
    );
    let creds = TempDir::new().unwrap();
    let credential_path = credentials(creds.path(), &server.uri());
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(11)))
        .mount(&server)
        .await;
    let (status, stdout, stderr) = validate_output(root.path(), &credential_path);
    assert_ne!(status, 0);
    assert!(
        stderr.contains("live document verification could not read the Project"),
        "{stdout}{stderr}"
    );
    assert!(!stdout.contains("verification skipped"));
}

#[test]
fn project_gate_check_help_states_the_autofix() {
    navigator()
        .args(["project", "gate", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--check"))
        .stdout(predicate::str::contains("Never writes to the live site"))
        .stdout(predicate::str::contains("--ci"));
}

#[tokio::test(flavor = "multi_thread")]
async fn project_gate_without_check_makes_no_network_calls() {
    let server = MockServer::start().await;
    let host = server.uri();
    let root = root_with_manifest(&host);
    let creds = TempDir::new().unwrap();
    let credential_path = credentials(creds.path(), &host);

    navigator()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credential_path)
        .args(["project", "gate"])
        .assert()
        .code(predicate::ne(3));

    let requests = server.received_requests().await.unwrap();
    assert!(
        requests.is_empty(),
        "plain project gate requested {:?}",
        requests
            .iter()
            .map(|request| request.url.to_string())
            .collect::<Vec<_>>()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn project_gate_check_reports_a_missing_object() {
    let server = MockServer::start().await;
    let host = server.uri();
    let root = root_with_manifest(&host);
    let creds = TempDir::new().unwrap();
    let credential_path = credentials(creds.path(), &host);
    let project_id = Uuid::now_v7();
    let asset_id = Uuid::now_v7();
    let relative = "blobs/pleadings/summons.pdf.yaml";
    let yaml = pointer(asset_id);
    write(root.path(), relative, &yaml);
    write(root.path(), ".gitignore", "/documents/\n");

    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": project_id, "code": "acme"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{project_id}/documents/integrity"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "assets": [{
                "asset_id": asset_id,
                "slug": "pleadings/summons.pdf",
                "exists": false,
                "recorded_size": 18
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{project_id}/documents/revisions"
        )))
        .and(query_param("slug", "pleadings/summons.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "kind": "filing",
            "revisions": [{
                "version": 1,
                "asset_id": asset_id,
                "created_at": "2026-09-05T12:00:00Z",
                "sha256": SHA,
                "size_bytes": 18,
                "filename": "summons.pdf",
                "visibility": "internal",
                "operative": true
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    navigator()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credential_path)
        .args(["project", "gate", "--check"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("storage object missing"));

    assert_eq!(
        fs::read_to_string(root.path().join(relative)).unwrap(),
        yaml
    );
}

/// LAW-62: `--check --ci` runs only the live document check, not the offline
/// gate (content rules, YAML, seeds, layout).
#[tokio::test(flavor = "multi_thread")]
async fn project_gate_check_ci_skips_the_offline_gate() {
    let server = MockServer::start().await;
    let host = server.uri();
    let root = root_with_manifest(&host);
    write(root.path(), ".gitignore", "/documents/\n");
    let project_id = Uuid::now_v7();
    let asset_id = Uuid::now_v7();
    let relative = "blobs/pleadings/summons.pdf.yaml";
    write(root.path(), relative, pointer(asset_id));

    Mock::given(method("GET"))
        .and(path("/actions/oidc"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "value": "github-oidc-token" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/ci/document-token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "token": "test-token", "project_code": "acme" }),
            ),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": project_id, "code": "acme"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{project_id}/documents/integrity"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "assets": [{
                "asset_id": asset_id,
                "slug": "pleadings/summons.pdf",
                "exists": true,
                "size_bytes": 18,
                "recorded_size": 18
            }],
            "integrations": []
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{project_id}/documents/revisions"
        )))
        .and(query_param("slug", "pleadings/summons.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "kind": "filing",
            "revisions": [{
                "version": 1,
                "asset_id": asset_id,
                "created_at": "2026-09-05T12:00:00Z",
                "sha256": SHA,
                "size_bytes": 18,
                "filename": "summons.pdf",
                "visibility": "internal",
                "operative": true
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    navigator()
        .current_dir(root.path())
        .env(
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            format!("{host}/actions/oidc"),
        )
        .env("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "runner-token")
        .env("CI", "true")
        .args(["project", "gate", "--check", "--ci"])
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("documents:"))
        .stdout(predicate::str::contains("dist/").not())
        .stderr(predicate::str::contains(
            "live Project check is inconclusive",
        ));
}

const MEMO: &str = "Engagement memo, synthetic fixture.\n";
const MEMO_POINTER: &str = "blobs/memo.md.yaml";

/// A checkout whose one pointer and live record agree with `MEMO`, behind a
/// mock deployment that reports a clean deep integrity result.
async fn deep_fixture() -> (MockServer, TempDir, TempDir, std::path::PathBuf) {
    let server = MockServer::start().await;
    let host = server.uri();
    let root = root_with_manifest(&host);
    let creds = TempDir::new().unwrap();
    let credential_path = credentials(creds.path(), &host);
    let project_id = Uuid::now_v7();
    let asset_id = Uuid::now_v7();
    let sha = store::documents::sha256_hex(MEMO.as_bytes());
    let size = i64::try_from(MEMO.len()).unwrap();
    write(
        root.path(),
        MEMO_POINTER,
        format!(
            "kind: filing\nvisibility: internal\ncurrent_version:\n  version: 1\n  \
             asset_id: {asset_id}\n  created_at: \"2026-09-05T12:00:00Z\"\n  sha256: {sha}\n  \
             size_bytes: {size}\n"
        ),
    );
    write(root.path(), "documents/memo.md", MEMO);
    write(root.path(), ".gitignore", "/documents/\n");
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": project_id, "code": "acme"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{project_id}/documents/integrity"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "assets": [{
                "asset_id": asset_id,
                "slug": "memo.md",
                "sha256": sha,
                "operative": true,
                "version": 1,
                "exists": true,
                "size_bytes": size,
                "recorded_size": size,
                "sha256_matches": true
            }],
            "integrations": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{project_id}/documents/revisions"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "kind": "filing",
            "revisions": [{
                "version": 1,
                "asset_id": asset_id,
                "created_at": "2026-09-05T12:00:00Z",
                "sha256": sha,
                "size_bytes": size,
                "filename": "memo.md",
                "visibility": "internal",
                "operative": true
            }]
        })))
        .mount(&server)
        .await;
    (server, root, creds, credential_path)
}

#[tokio::test(flavor = "multi_thread")]
async fn project_gate_check_deep_passes_a_clean_checkout() {
    let (_server, root, _creds, credential_path) = deep_fixture().await;
    navigator()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credential_path)
        .args(["project", "gate", "--check", "--deep"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "1 match, 0 modified locally, 0 missing locally",
        ));
}

#[tokio::test(flavor = "multi_thread")]
async fn project_gate_check_deep_fails_a_tampered_file() {
    let (_server, root, _creds, credential_path) = deep_fixture().await;
    let bytes = root.path().join("documents/memo.md");
    fs::write(&bytes, format!("{MEMO}tamper\n")).unwrap();
    navigator()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credential_path)
        .args(["project", "gate", "--check", "--deep"])
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains(MEMO_POINTER))
        .stdout(predicate::str::contains("modified locally"))
        .stdout(predicate::str::contains("0 match, 1 modified locally"));
    // The check reads the bytes; it never rewrites them.
    assert_eq!(
        fs::read_to_string(bytes).unwrap(),
        format!("{MEMO}tamper\n")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn project_gate_check_deep_reports_missing_bytes_and_passes() {
    let (_server, root, _creds, credential_path) = deep_fixture().await;
    fs::remove_file(root.path().join("documents/memo.md")).unwrap();
    navigator()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credential_path)
        .args(["project", "gate", "--check", "--deep"])
        .assert()
        .success()
        .stdout(predicate::str::contains("missing locally"))
        .stdout(predicate::str::contains(
            "0 match, 0 modified locally, 1 missing locally",
        ));
}

#[tokio::test(flavor = "multi_thread")]
async fn project_gate_check_without_deep_does_not_read_local_bytes() {
    let (_server, root, _creds, credential_path) = deep_fixture().await;
    fs::write(root.path().join("documents/memo.md"), "edited\n").unwrap();
    navigator()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credential_path)
        .args(["project", "gate", "--check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("local bytes").not());
}
