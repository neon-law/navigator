//! Attorney Calendar verbs talk to the site and fail closed on an unreadable calendar.

use assert_cmd::Command;
use serde_json::{json, Value};
use tempfile::TempDir;
use wiremock::matchers::{body_json, header, method, path, query_param};
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

#[tokio::test]
async fn personal_calendar_keeps_its_scope_inside_a_project_checkout() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let credentials = credentials(root.path(), &host);
    std::fs::write(
        root.path().join("navigator.yaml"),
        "version: 26.11.21\nproject:\n  name: sample-matter\n  host: other.example.com\n",
    )
    .unwrap();
    Mock::given(method("POST"))
        .and(path("/app/api/calendars/events"))
        .and(body_json(json!({
            "start": "2026-11-01T15:00:00Z",
            "end": "2026-11-02T15:00:00Z"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/api/calendars/check"))
        .and(body_json(json!({
            "project_code": null,
            "personal": true,
            "start": "2026-11-01T15:00:00Z",
            "end": "2026-11-01T16:00:00Z",
            "attorneys": []
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"clear": true})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/api/calendars/access"))
        .and(header("authorization", "Bearer test-token"))
        .and(body_json(json!({ "personal": true })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attorneys": [{"email": "ada@example.com", "calendars": []}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    for args in [
        vec!["calendar", "--start", "2026-11-01T15:00:00Z"],
        vec![
            "calendar",
            "check",
            "--start",
            "2026-11-01T15:00:00Z",
            "--end",
            "2026-11-01T16:00:00Z",
        ],
        vec!["calendar", "access"],
    ] {
        Command::cargo_bin("navigator")
            .unwrap()
            .current_dir(root.path())
            .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
            .args(args)
            .assert()
            .success();
    }
    std::fs::write(
        &credentials,
        json!({"hosts": {host: {
            "token": "second-token",
            "person_email": "grace@example.com",
            "role": "lawyer",
            "expires_at": i64::MAX
        }}})
        .to_string(),
    )
    .unwrap();
    let second_report = json!({
        "attorneys": [{"email": "grace@example.com", "calendars": []}]
    });
    Mock::given(method("POST"))
        .and(path("/app/api/calendars/access"))
        .and(header("authorization", "Bearer second-token"))
        .and(body_json(json!({ "personal": true })))
        .respond_with(ResponseTemplate::new(200).set_body_json(&second_report))
        .expect(1)
        .mount(&server)
        .await;
    let output = Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .args(["calendar", "access"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        second_report
    );
    server.verify().await;
}

#[tokio::test]
async fn project_calendar_events_resolve_the_checkout_and_explicit_project() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let credentials = credentials(root.path(), &host);
    std::fs::write(
        root.path().join("navigator.yaml"),
        format!("version: 26.11.21\nproject:\n  name: sample-matter\n  host: {host}\n"),
    )
    .unwrap();
    for project in ["sample-matter", "explicit-matter"] {
        Mock::given(method("POST"))
            .and(path("/app/api/calendars/events"))
            .and(body_json(json!({
                "project_code": project,
                "start": "2026-11-01T15:00:00Z",
                "end": "2026-11-02T15:00:00Z"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;
    }
    let child = root.path().join("documents");
    std::fs::create_dir(&child).unwrap();
    for extra in [vec![], vec!["events", "--project", "explicit-matter"]] {
        Command::cargo_bin("navigator")
            .unwrap()
            .current_dir(&child)
            .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
            .args(["project", "calendar"])
            .args(extra)
            .args(["--start", "2026-11-01T15:00:00Z"])
            .assert()
            .success();
    }
    server.verify().await;
}

#[tokio::test]
async fn dated_agenda_keeps_deadlines_when_calendar_access_is_missing() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let credentials = credentials(root.path(), &host);
    let project = uuid::Uuid::new_v4();
    Mock::given(method("GET"))
        .and(path("/app/api/lawyer/deadline-projects"))
        .and(query_param("include_deadlines", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "projects": [{"id": project, "code": "sample-matter", "name": "Sample Matter", "status": "open"}],
            "deadlines": [{"id": uuid::Uuid::new_v4(), "project_id": project, "title": "Synthetic hearing",
             "due_at": "2099-03-08T15:00:00Z", "status": "open", "category": "hearing"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/api/calendars/events"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "unreadable": [{"missing": "scope"}]
        })))
        .mount(&server)
        .await;
    let agenda = || {
        let mut command = Command::cargo_bin("navigator").unwrap();
        command
            .current_dir(root.path())
            .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
            .args([
                "calendar",
                "--host",
                &host,
                "--date",
                "2099-03-08",
                "--timezone",
                "UTC",
            ]);
        command
    };
    let output = agenda().arg("--json").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["entries"][0]["title"], "Synthetic hearing");
    assert_eq!(
        report["coverage"]["calendar"]["detail"]["unreadable"][0]["missing"],
        "scope"
    );
    let text = String::from_utf8(agenda().output().unwrap().stdout).unwrap();
    assert!(text.contains("Court • Synthetic hearing"));
    assert!(text.contains("Incomplete coverage"));
}
