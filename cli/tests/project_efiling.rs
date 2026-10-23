//! `navigator project efiling` against a fake portal: an acceptance email files
//! its conformed copy through the document upload door and links it to the
//! transaction, and a notification-only email records `unknown` without
//! uploading anything. No test leaves the loopback mock server.
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::{json, Value};
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/efiling")
        .join(name)
}

struct Harness {
    server: MockServer,
    root: TempDir,
    credentials: PathBuf,
    project_id: Uuid,
}

async fn harness(existing: Value) -> Harness {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let credentials = root.path().join("credentials.json");
    std::fs::write(
        &credentials,
        json!({"hosts":{server.uri():{
            "token":"test-token", "person_email":"lawyer@example.com", "role":"lawyer",
            "expires_at":i64::MAX
        }}})
        .to_string(),
    )
    .unwrap();
    let project_id = Uuid::now_v7();
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": project_id, "code": "acme"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/app/api/projects/{project_id}/efilings")))
        .respond_with(ResponseTemplate::new(200).set_body_json(existing))
        .mount(&server)
        .await;
    Harness {
        server,
        root,
        credentials,
        project_id,
    }
}

impl Harness {
    fn run(&self, args: &[&str]) -> assert_cmd::assert::Assert {
        let host = self.server.uri();
        let mut full = args.to_vec();
        full.extend(["--project", "acme", "--host", host.as_str()]);
        Command::cargo_bin("navigator")
            .unwrap()
            .current_dir(self.root.path())
            .env("NAVIGATOR_CREDENTIALS_FILE", &self.credentials)
            .args(full)
            .assert()
    }

    async fn mount_upload(&self, conformed_id: Uuid) {
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{}/documents",
                self.project_id
            )))
            .and(body_partial_json(json!({
                "kind": "filing",
                "content_type": "application/pdf",
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "kind": "filing", "visibility": "internal",
                "current_version": {
                    "version": 1, "asset_id": conformed_id,
                    "created_at": "2026-10-06T16:20:00Z",
                    "sha256": "0".repeat(64), "size_bytes": 10
                }
            })))
            .expect(1)
            .mount(&self.server)
            .await;
    }

    async fn mount_record(&self, stored: &Value) {
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{}/efilings",
                self.project_id
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(stored))
            .expect(1)
            .mount(&self.server)
            .await;
    }

    async fn posted(&self, suffix: &str) -> Vec<Value> {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request: &&Request| {
                request.method.as_str() == "POST" && request.url.path().ends_with(suffix)
            })
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .collect()
    }
}

#[tokio::test]
async fn an_acceptance_email_files_its_conformed_copy_and_links_it() {
    let harness = harness(json!([])).await;
    let submitted = Uuid::now_v7();
    let conformed = Uuid::now_v7();
    harness.mount_upload(conformed).await;
    let stored = json!({"transaction_id": "EX-20261005-0042", "status": "accepted"});
    harness.mount_record(&stored).await;
    let email = fixture("accepted.eml");
    let submitted_arg = submitted.to_string();

    let output = harness
        .run(&[
            "project",
            "efiling",
            "record",
            email.to_str().unwrap(),
            "--submitted",
            &submitted_arg,
        ])
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(serde_json::from_slice::<Value>(&output).unwrap(), stored);

    let uploads = harness.posted("/documents").await;
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0]["slug"], "efilings/EX-20261005-0042.pdf");
    assert_eq!(uploads[0]["filename"], "EX-20261005-0042.pdf");
    assert_eq!(uploads[0]["metadata"]["filed_on"], "2026-10-05");
    assert_eq!(
        uploads[0]["metadata"]["submitted_document_id"],
        submitted_arg
    );

    let records = harness.posted("/efilings").await;
    assert_eq!(
        records,
        vec![json!({
            "transaction_id": "EX-20261005-0042",
            "status": "accepted",
            "efsp": "Example EFSP",
            "court": "Superior Court of California, County of Example",
            "case_number": "26EXCV00042",
            "submitted_document_id": submitted_arg,
            "filed_on": "2026-10-05",
            "accepted_on": "2026-10-06",
            "conformed_document_id": conformed.to_string(),
        })]
    );
    harness.server.verify().await;
}

#[tokio::test]
async fn a_notification_only_email_records_unknown_and_uploads_nothing() {
    let harness = harness(json!([])).await;
    harness
        .mount_record(&json!({
            "transaction_id": "EX-20261005-0044",
            "status": "unknown",
            "note": "status unknown — log in to the EFSP to read it"
        }))
        .await;
    let email = fixture("notification.eml");

    harness
        .run(&["project", "efiling", "record", email.to_str().unwrap()])
        .success();

    assert!(harness.posted("/documents").await.is_empty());
    let records = harness.posted("/efilings").await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["status"], "unknown");
    assert_eq!(records[0]["transaction_id"], "EX-20261005-0044");
    assert!(records[0].get("filed_on").is_none());
    assert!(records[0].get("conformed_document_id").is_none());
}

#[tokio::test]
async fn a_filed_date_on_a_pending_email_is_refused_before_any_request() {
    let harness = harness(json!([])).await;
    let email = fixture("notification.eml");

    harness
        .run(&[
            "project",
            "efiling",
            "record",
            email.to_str().unwrap(),
            "--filed-on",
            "2026-10-05",
        ])
        .failure();

    assert!(harness.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_conformed_copy_with_no_submitted_version_is_refused_before_upload() {
    let harness = harness(json!([])).await;
    let email = fixture("accepted.eml");

    harness
        .run(&["project", "efiling", "record", email.to_str().unwrap()])
        .failure();

    assert!(harness.posted("/documents").await.is_empty());
    assert!(harness.posted("/efilings").await.is_empty());
}

fn stamped_pdf(root: &Path) -> PathBuf {
    let file = root.join("stamped-motion.pdf");
    std::fs::write(
        &file,
        b"%PDF-1.4\n% Synthetic stamped copy for tests.\n%%EOF\n",
    )
    .unwrap();
    file
}

#[tokio::test]
async fn pairing_a_judge_signed_return_links_it_and_flags_the_entered_order() {
    let submitted = Uuid::now_v7();
    let harness = harness(json!([{
        "transaction_id": "EX-20261005-0045",
        "status": "submitted",
        "submitted_document_id": submitted,
    }]))
    .await;
    let conformed = Uuid::now_v7();
    harness.mount_upload(conformed).await;
    let stored = json!({"transaction_id": "EX-20261005-0045", "status": "accepted"});
    harness.mount_record(&stored).await;
    let file = stamped_pdf(harness.root.path());

    harness
        .run(&[
            "project",
            "efiling",
            "pair",
            file.to_str().unwrap(),
            "--transaction",
            "EX-20261005-0045",
            "--filed-on",
            "2026-10-05",
            "--judge-signed",
        ])
        .success();

    let uploads = harness.posted("/documents").await;
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0]["slug"], "efilings/EX-20261005-0045.pdf");
    assert_eq!(uploads[0]["kind"], "filing");
    assert_eq!(
        uploads[0]["metadata"]["submitted_document_id"],
        submitted.to_string()
    );
    let records = harness.posted("/efilings").await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["status"], "accepted");
    assert_eq!(records[0]["filed_on"], "2026-10-05");
    assert_eq!(records[0]["conformed_document_id"], conformed.to_string());
    assert_eq!(records[0]["needs_entered_order"], true);
    harness.server.verify().await;
}

#[tokio::test]
async fn pairing_against_a_rejected_transaction_is_refused_before_upload() {
    let harness = harness(json!([{
        "transaction_id": "EX-20261005-0043",
        "status": "rejected",
        "submitted_document_id": Uuid::now_v7(),
    }]))
    .await;
    let file = stamped_pdf(harness.root.path());

    harness
        .run(&[
            "project",
            "efiling",
            "pair",
            file.to_str().unwrap(),
            "--transaction",
            "EX-20261005-0043",
            "--filed-on",
            "2026-10-05",
        ])
        .failure();

    assert!(harness.posted("/documents").await.is_empty());
    assert!(harness.posted("/efilings").await.is_empty());
}

#[tokio::test]
async fn pairing_without_a_filed_date_is_refused_before_any_request() {
    let harness = harness(json!([])).await;
    let file = stamped_pdf(harness.root.path());

    harness
        .run(&[
            "project",
            "efiling",
            "pair",
            file.to_str().unwrap(),
            "--transaction",
            "EX-20261005-0045",
        ])
        .failure();

    assert!(harness.server.received_requests().await.unwrap().is_empty());
}
