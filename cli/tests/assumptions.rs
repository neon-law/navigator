//! Integration coverage for `navigator assumptions` — drives the real binary
//! against a mock Firm API, with fake rows standing in for Firm data. The
//! store's own tests cover isolation and the append-only trigger; these pin
//! the command wire the operator sees: routes, bearer auth, JSON bodies, and
//! the refusal path a forbidden capability takes.

use assert_cmd::Command;
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn credentials(root: &TempDir, host: &str) -> std::path::PathBuf {
    let credentials = root.path().join("credentials.json");
    std::fs::write(
        &credentials,
        json!({"hosts":{host:{
            "token":"test-token", "person_email":"owner@example.com", "role":"owner",
            "expires_at":i64::MAX
        }}})
        .to_string(),
    )
    .unwrap();
    credentials
}

fn assumption_row(firm: Uuid) -> serde_json::Value {
    json!({
        "id": Uuid::now_v7(),
        "firm_id": firm,
        "key": "plan.target_minutes.document_review",
        "value": 45,
        "version": 1,
        "set_by": Uuid::now_v7(),
        "set_at": "2026-11-07T12:00:00Z",
        "reason": "first stated premise"
    })
}

#[tokio::test]
async fn assumptions_list_reads_the_firm_rows_over_the_authenticated_door() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let firm = Uuid::now_v7();
    let rows = json!([assumption_row(firm)]);
    Mock::given(method("GET"))
        .and(path(format!("/app/api/firms/{firm}/assumptions")))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&rows))
        .expect(1)
        .mount(&server)
        .await;
    let output = Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", credentials(&root, &host))
        .args([
            "assumptions",
            "--firm",
            &firm.to_string(),
            "--host",
            &host,
            "list",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output).unwrap(),
        rows
    );
    server.verify().await;
}

#[tokio::test]
async fn assumptions_set_posts_the_value_and_reason_and_prints_the_row() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let firm = Uuid::now_v7();
    let key = "plan.target_minutes.document_review";
    let row = assumption_row(firm);
    Mock::given(method("POST"))
        .and(path(format!("/app/api/firms/{firm}/assumptions/{key}")))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&row))
        .expect(1)
        .mount(&server)
        .await;
    let output = Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", credentials(&root, &host))
        .args([
            "assumptions",
            "--firm",
            &firm.to_string(),
            "--host",
            &host,
            "set",
            key,
            "--value",
            "45",
            "--reason",
            "first stated premise",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output).unwrap(),
        row
    );
    server.verify().await;
}

#[tokio::test]
async fn assumptions_show_and_history_read_one_key() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let firm = Uuid::now_v7();
    let key = "plan.target_minutes.document_review";
    let row = assumption_row(firm);
    for route in [
        format!("/app/api/firms/{firm}/assumptions/{key}"),
        format!("/app/api/firms/{firm}/assumptions/{key}/history"),
    ] {
        Mock::given(method("GET"))
            .and(path(route))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&row))
            .expect(1)
            .mount(&server)
            .await;
    }
    for action in ["show", "history"] {
        Command::cargo_bin("navigator")
            .unwrap()
            .current_dir(root.path())
            .env("NAVIGATOR_CREDENTIALS_FILE", credentials(&root, &host))
            .args([
                "assumptions",
                "--firm",
                &firm.to_string(),
                "--host",
                &host,
                action,
                key,
            ])
            .assert()
            .success();
    }
    server.verify().await;
}

/// A forbidden capability renders as `NotFound` on the API, so the command
/// refuses without ever distinguishing "wrong firm" from "wrong role".
#[tokio::test]
async fn assumptions_commands_refuse_when_the_capability_denies() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let firm = Uuid::now_v7();
    Mock::given(method("GET"))
        .and(path(format!("/app/api/firms/{firm}/assumptions")))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", credentials(&root, &host))
        .args([
            "assumptions",
            "--firm",
            &firm.to_string(),
            "--host",
            &host,
            "list",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "assumptions command refused (404)",
        ));
    server.verify().await;
}

/// A malformed --value never reaches the API: the command fails locally.
#[tokio::test]
async fn assumptions_set_rejects_a_malformed_value_before_any_request() {
    let root = TempDir::new().unwrap();
    Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .args([
            "assumptions",
            "--firm",
            &Uuid::now_v7().to_string(),
            "--host",
            "http://127.0.0.1:1",
            "set",
            "plan.target_minutes.document_review",
            "--value",
            "not-json",
            "--reason",
            "nope",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "the --value must be one JSON value",
        ));
}
