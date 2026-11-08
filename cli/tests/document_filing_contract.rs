//! Document filing commands, flags, upload pointers, and revision verification use synthetic fixtures.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{body_string_contains, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn navigator() -> Command {
    let mut command = Command::cargo_bin("navigator").unwrap();
    command.env_remove("GITHUB_REPOSITORY");
    // Exercise this build's rules; mismatched-release dispatch has its own suite.
    command.env("NAVIGATOR_RELEASE_TAG", env!("CARGO_PKG_VERSION"));
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
        format!(
            "version: \"{}\"\nproject:\n  host: {host}\n  name: acme\n",
            env!("CARGO_PKG_VERSION")
        ),
    );
}

/// A repository root the gate's own README/`.git` discovery accepts.
fn root_with_manifest(host: &str) -> TempDir {
    let root = TempDir::new().unwrap();
    write(root.path(), "README.md", "Synthetic repository\n");
    write(root.path(), ".git", "gitdir: /tmp/synthetic\n");
    manifest(root.path(), host);
    root
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

fn sha256(bytes: &[u8]) -> String {
    store::documents::sha256_hex(bytes)
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

/// The chain read a conditional upload makes before appending (LAW-284):
/// one live revision, the head the upload will precondition on.
async fn mount_revisions_read(server: &MockServer, project_id: Uuid, asset_id: Uuid, sha: &str) {
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{project_id}/documents/revisions"
        )))
        .and(query_param("slug", "motion.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "kind": "filing",
            "revisions": [{
                "version": 1,
                "asset_id": asset_id,
                "created_at": "2026-09-05T12:00:00Z",
                "sha256": sha,
                "size_bytes": 48,
                "filename": "motion.pdf",
                "visibility": "internal",
                "operative": true
            }]
        })))
        .expect(1)
        .mount(server)
        .await;
}

/// The `409 head_conflict` answer a conditional append gets when the chain
/// moved past its expected head: the live head's triple in `current`, the
/// shape the restart adopts without a second read.
fn head_conflict_template(asset_id: Uuid, version: u32, sha: &str) -> ResponseTemplate {
    ResponseTemplate::new(409).set_body_json(serde_json::json!({
        "error": "head_conflict",
        "current": {
            "asset_id": asset_id,
            "version": version,
            "sha256": sha,
        }
    }))
}

fn pointer_body(asset_id: Uuid, version: usize, sha: &str, size: usize) -> serde_json::Value {
    serde_json::json!({
        "kind": "filing",
        "visibility": "internal",
        "current_version": {
            "version": version,
            "asset_id": asset_id,
            "created_at": "2026-09-05T12:00:00Z",
            "sha256": sha,
            "size_bytes": size
        }
    })
}

/// Every command the skill instructs a filer to run actually resolves, and
/// still accepts the exact flags the skill names.
#[test]
fn documented_filing_commands_resolve_their_flags() {
    let cases: &[(&[&str], &[&str])] = &[
        (
            &["site", "document", "upload", "--help"],
            &[
                "--project",
                "--file",
                "--kind",
                "--visibility",
                "--slug",
                "--description",
            ],
        ),
        (&["site", "document", "log", "--help"], &["<POINTER>"]),
        (&["project", "sync", "--help"], &["--dry-run"]),
        (&["project", "gate", "--help"], &["--check", "--ci"]),
        (&["site", "login", "--help"], &["--host", "--no-browser"]),
        (
            &["site", "authorities", "create", "--help"],
            &["--class", "--citation", "--title", "--file", "--checked-on"],
        ),
    ];
    for (args, flags) in cases {
        let output = navigator().args(*args).output().expect("run navigator");
        assert!(
            output.status.success(),
            "`navigator {}` failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        for flag in *flags {
            assert!(
                stdout.contains(flag),
                "`navigator {}` --help must mention `{flag}`, got:\n{stdout}",
                args.join(" ")
            );
        }
    }
}

/// The documented procedure end to end, against synthetic fixtures only:
/// direct upload files the document and writes no local pointer (ENG-872 item
/// 6 / LAW-86 item 3), `project gate --check` writes the missing pointer, and
/// `site document log` is the verification step the skill names.
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)]
async fn direct_upload_then_gate_check_then_log_files_and_verifies_a_document() {
    const BYTES: &[u8] = b"SYNTHETIC MOTION (test fixture; no client data)";
    let server = MockServer::start().await;
    let host = server.uri();
    let root = root_with_manifest(&host);
    let creds = TempDir::new().unwrap();
    let credential_path = credentials(creds.path(), &host);
    write(root.path(), "motion.pdf", BYTES);
    let project_id = Uuid::now_v7();
    let asset_id = Uuid::now_v7();
    let sha = sha256(BYTES);

    mount_project_lookup(&server, project_id).await;
    Mock::given(method("POST"))
        .and(path(format!("/app/api/projects/{project_id}/documents")))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "kind": "filing",
            "visibility": "internal",
            "current_version": {
                "version": 1,
                "asset_id": asset_id,
                "created_at": "2026-09-05T12:00:00Z",
                "sha256": sha,
                "size_bytes": BYTES.len()
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    navigator()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credential_path)
        .args([
            "site",
            "document",
            "upload",
            "--project",
            "acme",
            "--file",
            "motion.pdf",
            "--kind",
            "filing",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("kind: filing"));
    let uploaded_pointer = root.path().join("blobs/motion.pdf.yaml");
    assert!(
        uploaded_pointer.is_file(),
        "direct upload from the matching Project checkout must write its source pointer"
    );
    let uploaded_yaml = fs::read_to_string(&uploaded_pointer).unwrap();
    assert!(uploaded_yaml.contains("kind: filing"));
    assert!(uploaded_yaml.contains(&asset_id.to_string()));

    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{project_id}/documents/integrity"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "assets": [{
                "asset_id": asset_id,
                "slug": "motion.pdf",
                "exists": true,
                "recorded_size": BYTES.len(),
                "size_bytes": BYTES.len()
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{project_id}/documents/revisions"
        )))
        .and(query_param("slug", "motion.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "kind": "filing",
            "revisions": [{
                "version": 1,
                "asset_id": asset_id,
                "created_at": "2026-09-05T12:00:00Z",
                "sha256": sha,
                "size_bytes": BYTES.len(),
                "filename": "motion.pdf",
                "visibility": "internal",
                "operative": true
            }]
        })))
        .mount(&server)
        .await;

    navigator()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credential_path)
        .args(["project", "gate", "--check"])
        .assert()
        .success();

    let pointer_yaml = fs::read_to_string(uploaded_pointer)
        .expect("the direct upload must leave a committed source pointer");
    assert!(pointer_yaml.contains("kind: filing"));
    assert!(pointer_yaml.contains(&asset_id.to_string()));
    assert!(
        !root.path().join("documents/.gitignore").exists(),
        "gate --check must never write the retired documents/.gitignore"
    );

    navigator()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credential_path)
        .args(["site", "document", "log", "blobs/motion.pdf.yaml"])
        .assert()
        .success()
        .stdout(predicate::str::contains("motion.pdf"))
        .stdout(predicate::str::contains("filing"));
}

/// The missing-extension diagnostic (ENG-872 item 5) still fires, and fires
/// before any network request: a slug without the source extension can never
/// be filed, guessed around, or silently sent to the API.
#[tokio::test(flavor = "multi_thread")]
async fn direct_upload_refuses_a_slug_missing_the_source_extension() {
    let server = MockServer::start().await;
    let host = server.uri();
    let root = root_with_manifest(&host);
    let creds = TempDir::new().unwrap();
    let credential_path = credentials(creds.path(), &host);
    write(
        root.path(),
        "motion.pdf",
        b"synthetic fixture; no client data",
    );
    let project_id = Uuid::now_v7();

    mount_project_lookup(&server, project_id).await;
    // Deliberately no mock for POST .../documents: if validation ran after
    // the network call instead of before it, this would fail with a
    // connection/404 error rather than the diagnostic asserted below.

    navigator()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credential_path)
        .args([
            "site",
            "document",
            "upload",
            "--project",
            "acme",
            "--file",
            "motion.pdf",
            "--kind",
            "filing",
            "--slug",
            "motion",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "document slug `motion` must carry the source extension from `motion.pdf`",
        ))
        .stderr(predicate::str::contains("use `motion.pdf`"));

    assert!(!root.path().join("documents").exists());
}

/// The setup for the conditional-upload restart tests: a checkout with one
/// motion to upload, and a mock deployment whose chain head at the pre-read
/// is `head_at_read` — with a concurrent writer's head `live_head` that the
/// server names in its conflicts.
struct RestartFixture {
    root: TempDir,
    /// Holds the credentials directory alive for the whole test — dropping
    /// it deletes the credentials file before the command runs.
    creds: TempDir,
    project_id: Uuid,
    head_at_read: Uuid,
    live_head: Uuid,
    live_head_sha: String,
}

impl RestartFixture {
    fn credential_path(&self) -> std::path::PathBuf {
        self.creds.path().join("credentials.json")
    }
}

async fn restart_fixture(server: &MockServer) -> RestartFixture {
    const BYTES: &[u8] = b"SYNTHETIC MOTION v3 (test fixture; no client data)";
    let host = server.uri();
    let root = root_with_manifest(&host);
    let creds = TempDir::new().unwrap();
    credentials(creds.path(), &host);
    write(root.path(), "motion.pdf", BYTES);
    let project_id = Uuid::now_v7();
    let head_at_read = Uuid::now_v7();
    let head_at_read_sha = sha256(b"SYNTHETIC MOTION (test fixture; no client data)");
    let live_head = Uuid::now_v7();
    let live_head_sha = sha256(b"SYNTHETIC MOTION v2 (test fixture; no client data)");

    mount_project_lookup(server, project_id).await;
    // Exactly one chain read per upload: the restart adopts the head the
    // conflict named instead of reading again.
    mount_revisions_read(server, project_id, head_at_read, &head_at_read_sha).await;

    RestartFixture {
        root,
        creds,
        project_id,
        head_at_read,
        live_head,
        live_head_sha,
    }
}

fn upload_command(root: &Path, credential_path: &Path) -> Command {
    let mut command = navigator();
    command
        .current_dir(root)
        .env("NAVIGATOR_CREDENTIALS_FILE", credential_path)
        .args([
            "site",
            "document",
            "upload",
            "--project",
            "acme",
            "--file",
            "motion.pdf",
            "--kind",
            "filing",
        ]);
    command
}

/// LAW-284: a conditional upload that loses the append race restarts once
/// against the head the server named — no second chain read — and files.
/// The body-matched mocks make adoption observable: the first append can
/// only match the mock keyed to the head it read, the restart only the one
/// keyed to the server-named current head.
#[tokio::test(flavor = "multi_thread")]
async fn a_conditional_upload_restarts_once_against_the_head_the_server_named() {
    const BYTES: &[u8] = b"SYNTHETIC MOTION v3 (test fixture; no client data)";
    let server = MockServer::start().await;
    let f = restart_fixture(&server).await;
    let documents_path = format!("/app/api/projects/{}/documents", f.project_id);

    // The first conditional append still holds the head it read: refused,
    // exactly once, with the live head named.
    Mock::given(method("POST"))
        .and(path(documents_path.clone()))
        .and(body_string_contains(format!(
            "\"asset_id\":\"{}\"",
            f.head_at_read
        )))
        .respond_with(head_conflict_template(f.live_head, 2, &f.live_head_sha))
        .expect(1)
        .mount(&server)
        .await;
    // The restart must carry the server-named current head, and files.
    Mock::given(method("POST"))
        .and(path(documents_path))
        .and(body_string_contains(format!(
            "\"asset_id\":\"{}\"",
            f.live_head
        )))
        .respond_with(ResponseTemplate::new(201).set_body_json(pointer_body(
            f.live_head,
            2,
            &f.live_head_sha,
            BYTES.len(),
        )))
        .expect(1)
        .mount(&server)
        .await;

    upload_command(f.root.path(), &f.credential_path())
        .assert()
        .success()
        .stdout(predicate::str::contains("kind: filing"));

    let uploaded_yaml = fs::read_to_string(f.root.path().join("blobs/motion.pdf.yaml"))
        .expect("the upload writes its source pointer");
    assert!(
        uploaded_yaml.contains(&f.live_head.to_string()),
        "the pointer names the adopted head"
    );
}

/// LAW-284: a conflict that survives the one restart is a terminal error —
/// naming the slug, both head triples, and the reconcile guidance — after
/// exactly two conditional appends and no third.
#[tokio::test(flavor = "multi_thread")]
async fn a_persistent_head_conflict_is_a_terminal_error_after_one_restart() {
    let server = MockServer::start().await;
    let f = restart_fixture(&server).await;
    let documents_path = format!("/app/api/projects/{}/documents", f.project_id);

    // First append: refused, naming the concurrent writer's head.
    Mock::given(method("POST"))
        .and(path(documents_path.clone()))
        .and(body_string_contains(format!(
            "\"asset_id\":\"{}\"",
            f.head_at_read
        )))
        .respond_with(head_conflict_template(f.live_head, 2, &f.live_head_sha))
        .expect(1)
        .mount(&server)
        .await;
    // The restart against that head conflicts AGAIN — the chain moved once
    // more before the second append landed, so a third head is current.
    let second_live_head = Uuid::now_v7();
    let second_live_sha = sha256(b"SYNTHETIC MOTION v4 (test fixture; no client data)");
    Mock::given(method("POST"))
        .and(path(documents_path))
        .and(body_string_contains(format!(
            "\"asset_id\":\"{}\"",
            f.live_head
        )))
        .respond_with(head_conflict_template(
            second_live_head,
            3,
            &second_live_sha,
        ))
        .expect(1)
        .mount(&server)
        .await;

    upload_command(f.root.path(), &f.credential_path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("lost the expected-head race"))
        .stderr(predicate::str::contains("motion.pdf"))
        .stderr(predicate::str::contains(f.live_head.to_string()))
        .stderr(predicate::str::contains(second_live_head.to_string()))
        .stderr(predicate::str::contains("navigator project gate --check"));
}
