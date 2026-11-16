//! Attorney Calendar verbs talk to the site and fail closed on an unreadable calendar.

use assert_cmd::Command;
use serde_json::{json, Value};
use tempfile::TempDir;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn credentials(root: &std::path::Path, host: &str) -> std::path::PathBuf {
    let credentials = root.join("credentials.json");
    std::fs::write(
        &credentials,
        json!({"hosts": {host: {
            "token": "test-token",
            "person_email": "ada@example.com",
            "role": "lawyer",
            "expires_at": i64::MAX
        }}})
        .to_string(),
    )
    .unwrap();
    credentials
}

#[tokio::test]
async fn freebusy_exits_nonzero_when_a_calendar_is_unreadable() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let credentials = credentials(root.path(), &host);
    let body = json!({
        "busy": [],
        "unreadable": [{
            "email": "ada@example.com",
            "role": "lawyer",
            "calendar_id": "ada@example.com",
            "missing": "share"
        }]
    });
    Mock::given(method("POST"))
        .and(path("/app/api/calendars/freebusy"))
        .and(header("authorization", "Bearer test-token"))
        .and(body_json(json!({
            "project_code": "sample-litigation",
            "start": "2026-11-01T15:00:00Z",
            "end": "2026-11-01T16:00:00Z",
            "attorneys": ["ada@example.com"]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .expect(1)
        .mount(&server)
        .await;

    let output = Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .args([
            "calendar",
            "freebusy",
            "--project",
            "sample-litigation",
            "--host",
            &host,
            "--start",
            "2026-11-01T15:00:00Z",
            "--end",
            "2026-11-01T16:00:00Z",
            "--attorney",
            "ada@example.com",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["unreadable"][0]["missing"],
        "share"
    );
    server.verify().await;
}

#[tokio::test]
async fn check_exits_zero_only_when_the_window_is_clear() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let credentials = credentials(root.path(), &host);
    Mock::given(method("POST"))
        .and(path("/app/api/calendars/check"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "clear": true,
            "overlaps": [],
            "duplicates": [],
            "unreadable": []
        })))
        .expect(1)
        .mount(&server)
        .await;

    Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .args([
            "calendar",
            "check",
            "--host",
            &host,
            "--start",
            "2026-11-01T15:00:00Z",
            "--end",
            "2026-11-01T16:00:00Z",
        ])
        .assert()
        .success();
    server.verify().await;
}
