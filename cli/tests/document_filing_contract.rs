//! ENG-872: the `legal-writing` skill's "Filing a document into a matter"
//! section must name commands that actually exist and behave exactly as
//! documented — a lawyer or an unattended agent follows that prose verbatim.
//!
//! Two kinds of coverage: a prose-drift check (the skill still names every
//! documented command and behavior), and an end-to-end run of the documented
//! procedure — direct upload writing its source pointer, the gate checking it,
//! and `site document log` verifying the revision landed — plus a
//! regression lock on the missing-extension diagnostic the skill tells a
//! filer to expect. Every fixture below is synthetic (AGENTS.md: no client
//! data, this must work for everyone).

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SKILL: &str = include_str!("../../.agents/skills/legal-writing/SKILL.md");

#[test]
fn memo_authoring_contract_preserves_the_source_and_storage_boundary() {
    let contract = include_str!("../src/projects/agent_contract.md");
    let scope = include_str!("../../.agents/skills/stay-in-repo/SKILL.md");
    for required in [
        "kind: memo",
        "templates/<code>.md",
        "navigator notation preview",
        "confidential: true",
    ] {
        assert!(
            SKILL.contains(required),
            "memo procedure must name {required}"
        );
    }
    assert!(contract.contains("Memo notations"));
    assert!(contract.contains("client data"));
    assert!(!scope.contains("an uncommitted memo"));
    for firm in ["Wachtell", "Cravath", "Quinn Emanuel"] {
        assert!(
            !SKILL.contains(firm),
            "legal writing must use the plain house style"
        );
    }
}

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

/// Every command name and behavior the skill's filing section states must
/// actually be present in the shipped skill text — a coupling test, not a CLI
/// check: if this fails, the prose has drifted from what it names.
#[test]
fn legal_writing_skill_names_every_documented_filing_command() {
    for expected in [
        "navigator site document upload",
        "navigator project gate",
        "navigator project sync",
        "navigator site document log",
        "navigator site login",
        "--no-browser",
        "project.repository_url",
        "blobs/",
        "must carry the source extension from",
        "~/.navigator.json",
    ] {
        assert!(
            SKILL.contains(expected),
            "legal-writing/SKILL.md must document `{expected}` — the filing procedure has drifted"
        );
    }
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
