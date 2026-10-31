//! `navigator inbox import` (LAW-238 D2): the CLI verb is a thin client over
//! the server door `POST /app/api/inbox/import`, so the contract lives at
//! three seams — the prose (`docs/inbox-import-cli.md` must not drift from
//! the shipped behavior), the dry run (`--dry-run` prints the request JSON
//! and sends nothing, AC11), and the idempotent replay (a second import of
//! the same payload exits 0 reporting the existing ids as deduped, AC3).

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use base64::Engine as _;
use predicates::prelude::*;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DOC: &str = include_str!("../../docs/inbox-import-cli.md");

/// AC13: every command and behavior the documentation states must exist in
/// the shipped surface, and the surface's no-send, ingest-only posture must
/// hold in prose — a coupling test, not a CLI check.
#[test]
fn inbox_import_prose_names_the_shipped_behavior() {
    for expected in [
        "navigator inbox import",
        "--source=email",
        "--source=linkedin",
        "--file",
        "--dir",
        "--mailbox",
        "--project",
        "--dry-run",
        "dedup",
        "exits 0",
        "no `navigator inbox send`",
        "ingest-only",
        "navigator site login",
        "reconcile step",
    ] {
        assert!(
            DOC.contains(expected),
            "inbox-import prose must name {expected}"
        );
    }
}

fn navigator() -> Command {
    let mut command = Command::cargo_bin("navigator").unwrap();
    command.env_remove("GITHUB_REPOSITORY");
    command.env("NAVIGATOR_RELEASE_TAG", env!("CARGO_PKG_VERSION"));
    command.env_remove("GITHUB_REF");
    command.env_remove("GITHUB_EVENT_NAME");
    command
}

/// A credentials file valid for `host`, written per-process so tests never
/// share `~/.navigator.json` state.
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

fn eml(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(
        &path,
        "From: counsel@example.com\r\nTo: nick@neonlaw.com\r\nSubject: discovery schedule\r\n\r\nBody.\r\n",
    )
    .unwrap();
    path
}

fn import_response(deduped: bool) -> serde_json::Value {
    serde_json::json!({
        "source": "email",
        "mailbox": "nick@neonlaw.com",
        "archived_key": "mail/1791000000-notice.eml",
        "imported": [{
            "message_id": "0f0e0d0c-0b0a-49d8-8c07-6f5e4d3c2b1a",
            "conversation_id": "01020304-0506-4708-891a-bcdef0123456",
            "deduped": deduped,
            "direction": "outbound",
            "attachments": []
        }]
    })
}

/// AC11: `--dry-run` prints the exact request JSON the door would receive —
/// source, mailbox, matter code, and the base64 payload — and sends nothing.
/// No server is running and no credentials exist; the run still succeeds.
#[test]
fn dry_run_prints_the_request_and_sends_nothing() {
    let root = TempDir::new().unwrap();
    let payload = eml(root.path(), "notice.eml");

    navigator()
        .env(
            "NAVIGATOR_CREDENTIALS_FILE",
            root.path().join("absent.json"),
        )
        .args([
            "inbox",
            "import",
            "--source=email",
            "--mailbox=nick@neonlaw.com",
            "--project=24-1043",
            "--dry-run",
        ])
        .arg("--file")
        .arg(&payload)
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"source\":\"email\"")
                .and(predicate::str::contains("\"mailbox\":\"nick@neonlaw.com\""))
                .and(predicate::str::contains("\"project\":\"24-1043\""))
                .and(predicate::str::contains(format!(
                    "\"payload_base64\":\"{}\"",
                    base64::engine::general_purpose::STANDARD.encode(fs::read(&payload).unwrap())
                ))),
        )
        .stderr(predicate::str::is_empty());
}

/// AC3: importing the same payload twice yields the door's dedup on the
/// second run — the CLI reports the existing ids as deduped and exits 0,
/// because idempotency is the store's, inherited by the verb.
#[tokio::test]
async fn replayed_import_reports_the_existing_ids_as_deduped_and_exits_0() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let creds = credentials(root.path(), &server.uri());
    let payload = eml(root.path(), "notice.eml");

    // The first mock answers only once (`up_to_n_times`; `expect` verifies
    // but never retires a mock), so run 1 gets the fresh response and run 2
    // falls through to the deduped one.
    Mock::given(method("POST"))
        .and(path("/app/api/inbox/import"))
        .respond_with(ResponseTemplate::new(201).set_body_json(import_response(false)))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/api/inbox/import"))
        .respond_with(ResponseTemplate::new(200).set_body_json(import_response(true)))
        .expect(1)
        .mount(&server)
        .await;

    let mut run = navigator();
    run.env("NAVIGATOR_CREDENTIALS_FILE", &creds)
        .args([
            "inbox",
            "import",
            "--source=email",
            "--mailbox=nick@neonlaw.com",
        ])
        .arg("--host")
        .arg(server.uri())
        .arg("--file")
        .arg(&payload);

    // First import: the fresh ingest reports the new ids, not deduped.
    let first = run.assert().success();
    let stdout = String::from_utf8(first.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("0f0e0d0c-0b0a-49d8-8c07-6f5e4d3c2b1a"));
    assert!(stdout.contains("conversation 01020304-0506-4708-891a-bcdef0123456"));
    assert!(!stdout.contains("(deduped)"));

    // Replayed import: the door dedups; the same ids print, marked deduped,
    // and the exit is still 0.
    run.assert().success().stdout(
        predicate::str::contains("0f0e0d0c-0b0a-49d8-8c07-6f5e4d3c2b1a")
            .and(predicate::str::contains("(deduped)")),
    );
}
