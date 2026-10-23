//! The Project status command reads the authenticated live state without writes.
use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn status_uses_manifest_coordinates_and_emits_live_json() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    std::fs::write(
        root.path().join("navigator.yaml"),
        format!("version: 26.10.1\nproject:\n  host: {host}\n  name: acme\n"),
    )
    .unwrap();
    let credentials = root.path().join("credentials.json");
    std::fs::write(
        &credentials,
        serde_json::json!({"hosts": {host.clone(): {
            "token": "test-token", "person_email": "lawyer@example.com",
            "role": "lawyer", "expires_at": i64::MAX
        }}})
        .to_string(),
    )
    .unwrap();
    let id = Uuid::now_v7();
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": id, "code": "acme"}
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let payload = serde_json::json!({"code":"acme", "status":"active",
        "opened_at":"2026-01-01T00:00:00Z", "closed_at":null,
        "source_state":"initialized", "onboarding":{"filed":true,"documents":[
            {"id":Uuid::now_v7(),"visibility":"internal"}
        ]}, "missing":[]});
    Mock::given(method("GET"))
        .and(path(format!("/app/api/projects/{id}/status")))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&payload))
        .expect(1)
        .mount(&server)
        .await;
    let output = Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .args(["project", "status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output).unwrap(),
        payload
    );
    server.verify().await;
}

#[test]
fn status_outside_a_checkout_requires_a_project_code() {
    Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(TempDir::new().unwrap().path())
        .args(["project", "status"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Project code"));
}
