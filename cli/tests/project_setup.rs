//! Actual `navigator project setup` outcomes through the authenticated HTTP
//! doors. The CLI is intentionally tested as a process: a unit test of the
//! composition would not prove clap parsing, stored-login resolution, or
//! exit-code behavior.

use std::fs;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn credentials(dir: &TempDir, host: &str) -> std::path::PathBuf {
    let path = dir.path().join("navigator.json");
    fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "hosts": {
                host: {
                    "token": "test-token",
                    "person_email": "admin@example.com",
                    "role": "admin",
                    "expires_at": i64::MAX
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

async fn mount_setup_doors(
    server: &MockServer,
    project_id: Uuid,
    code: &str,
    integration_outcomes: [(&str, &str); 3],
) {
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": project_id, "code": code }
        ])))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/app/api/project-surfaces/{project_id}")))
        .and(header("authorization", "Bearer test-token"))
        .and(header("content-length", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": code,
            "documents_prefix": format!("projects/{code}/documents"),
            "drive_folder_id": "drive-1",
            "drive_status": "created",
            "repository_url": "https://forge.example/acme",
            "repository_status": "present"
        })))
        .mount(server)
        .await;
    for (provider, outcome) in integration_outcomes {
        Mock::given(method("POST"))
            .and(path(format!("/app/api/integrations/{provider}/ensure")))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [{ "project_code": code, "outcome": outcome }]
            })))
            .mount(server)
            .await;
    }
}

fn command() -> Command {
    let mut command = Command::cargo_bin("navigator").unwrap();
    command.env_remove("GITHUB_REPOSITORY");
    command.env_remove("GITHUB_TOKEN");
    command.env_remove("NAVIGATOR_GITHUB_TOKEN");
    command
}

#[test]
fn project_set_exposes_the_live_fill_flags() {
    command()
        .args(["project", "set", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("--from-live"))
        .stdout(predicates::str::contains("--lawyer-dri"))
        .stdout(predicates::str::contains("--client-dri"))
        .stdout(predicates::str::contains("--private-slack-channel"))
        .stdout(predicates::str::contains("--private-notion-page"))
        .stdout(predicates::str::contains("--calendar"))
        .stdout(predicates::str::contains("--booking-url"));
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_runs_every_door_and_repeat_execution_is_successful() {
    let server = MockServer::start().await;
    let host = server.uri();
    let credentials_dir = TempDir::new().unwrap();
    let credentials_path = credentials(&credentials_dir, &host);
    let project_id = Uuid::now_v7();
    mount_setup_doors(
        &server,
        project_id,
        "acme",
        [
            ("slack", "adopted"),
            ("notion", "adopted"),
            ("calendar", "present"),
        ],
    )
    .await;

    for _ in 0..2 {
        let output = command()
            .env("NAVIGATOR_CREDENTIALS_FILE", &credentials_path)
            .args(["project", "setup", "acme", "--host", &host, "--json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["results"][0]["project_code"], "acme");
        assert!(
            report["results"][0]["resources"]["publisher"].is_null(),
            "Portal publisher provisioning is retired"
        );
        assert_eq!(
            report["results"][0]["resources"]["drive"]["outcome"],
            "created"
        );
        assert_eq!(
            report["results"][0]["resources"]["github"]["outcome"],
            "present"
        );
        assert_eq!(
            report["results"][0]["resources"]["slack"]["outcome"],
            "adopted"
        );
        assert_eq!(
            report["results"][0]["resources"]["notion"]["outcome"],
            "adopted"
        );
        assert_eq!(
            report["results"][0]["resources"]["calendar"]["outcome"],
            "present"
        );
    }
}

/// A closed Project's calendar is made read-only rather than re-shared, and
/// setup still counts that as converged.
#[tokio::test(flavor = "multi_thread")]
async fn setup_reports_a_read_only_calendar_as_converged() {
    let server = MockServer::start().await;
    let host = server.uri();
    let credentials_dir = TempDir::new().unwrap();
    let credentials_path = credentials(&credentials_dir, &host);
    mount_setup_doors(
        &server,
        Uuid::now_v7(),
        "acme",
        [
            ("slack", "adopted"),
            ("notion", "adopted"),
            ("calendar", "read_only"),
        ],
    )
    .await;

    let output = command()
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials_path)
        .args(["project", "setup", "acme", "--host", &host, "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["results"][0]["resources"]["calendar"]["outcome"],
        "read_only"
    );
}

/// `--calendar` writes the manifest and the live row; `--booking-url` writes
/// the live row only.
#[tokio::test(flavor = "multi_thread")]
async fn project_set_records_the_calendar_and_booking_link() {
    let server = MockServer::start().await;
    let host = server.uri();
    let credentials_dir = TempDir::new().unwrap();
    let credentials_path = credentials(&credentials_dir, &host);
    let project_id = Uuid::now_v7();
    let checkout = TempDir::new().unwrap();
    fs::write(
        checkout.path().join("navigator.yaml"),
        concat!(
            "version: \"26.9.26\"\nproject:\n  host: staging.neonlaw.com\n  name: acme\n",
            "  lawyer_dri: lawyer@example.com\n  client_dri: client@example.com\n",
            "  private_slack_channel: not provisioned\n  private_notion_page: not provisioned\n",
        ),
    )
    .unwrap();
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": project_id, "code": "acme" }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("/app/api/projects/{project_id}")))
        .and(header("authorization", "Bearer test-token"))
        .and(wiremock::matchers::body_json(serde_json::json!({
            "calendar_id": "c_0123@group.calendar.example.com",
            "booking_url": "https://calendar.app.google/synthetic"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .expect(1)
        .mount(&server)
        .await;

    let output = command()
        .current_dir(checkout.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials_path)
        .args([
            "project",
            "set",
            "--calendar",
            "c_0123@group.calendar.example.com",
            "--booking-url",
            "https://calendar.app.google/synthetic",
            "--host",
            &host,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest = fs::read_to_string(checkout.path().join("navigator.yaml")).unwrap();
    assert!(
        manifest.contains("  calendar_id: c_0123@group.calendar.example.com\n"),
        "{manifest}"
    );
    assert!(
        !manifest.contains("booking"),
        "the booking link lives on the live Project only: {manifest}"
    );
}

/// A malformed value is refused before anything is written.
#[test]
fn project_set_refuses_a_malformed_calendar_without_writing() {
    let checkout = TempDir::new().unwrap();
    let original = "version: \"26.9.26\"\nproject:\n  host: staging.neonlaw.com\n  name: acme\n";
    fs::write(checkout.path().join("navigator.yaml"), original).unwrap();
    command()
        .current_dir(checkout.path())
        .args(["project", "set", "--calendar", "not a calendar"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("calendar id"));
    assert_eq!(
        fs::read_to_string(checkout.path().join("navigator.yaml")).unwrap(),
        original
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn project_set_from_live_uses_the_stored_bearer_and_fills_required_coordinates() {
    let server = MockServer::start().await;
    let host = server.uri();
    let credentials_dir = TempDir::new().unwrap();
    let credentials_path = credentials(&credentials_dir, &host);
    let project_id = Uuid::now_v7();
    let checkout = TempDir::new().unwrap();
    fs::write(
        checkout.path().join("navigator.yaml"),
        "version: \"26.9.26\"\nproject:\n  host: staging.neonlaw.com\n  name: acme\n",
    )
    .unwrap();
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": project_id, "code": "acme" }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/app/api/project-surfaces/{project_id}")))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": "acme",
            "repository_url": "https://github.com/org/acme",
            "lawyer_dri": ["lawyer@example.com"],
            "client_dri": ["client@example.com"],
            "private_slack_channel": "https://slack.com/archives/private",
            "private_notion_page": "https://notion.so/private"
        })))
        .mount(&server)
        .await;

    let output = command()
        .current_dir(checkout.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials_path)
        .args(["project", "set", "--from-live", "--host", &host])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest = fs::read_to_string(checkout.path().join("navigator.yaml")).unwrap();
    for value in [
        "lawyer@example.com",
        "client@example.com",
        "https://slack.com/archives/private",
        "https://notion.so/private",
    ] {
        assert!(manifest.contains(value), "missing {value}: {manifest}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_all_uses_the_visible_project_list() {
    let server = MockServer::start().await;
    let host = server.uri();
    let credentials_dir = TempDir::new().unwrap();
    let credentials_path = credentials(&credentials_dir, &host);
    let project_id = Uuid::now_v7();
    mount_setup_doors(
        &server,
        project_id,
        "acme",
        [
            ("slack", "created"),
            ("notion", "created"),
            ("calendar", "created"),
        ],
    )
    .await;

    let output = command()
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials_path)
        .args(["project", "setup", "--all", "--host", &host, "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["results"].as_array().unwrap().len(), 1);
    assert_eq!(report["results"][0]["project_code"], "acme");
    assert_eq!(
        report["results"][0]["resources"]["drive"]["outcome"],
        "created"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_reports_partial_failure_and_returns_nonzero() {
    let server = MockServer::start().await;
    let host = server.uri();
    let credentials_dir = TempDir::new().unwrap();
    let credentials_path = credentials(&credentials_dir, &host);
    let project_id = Uuid::now_v7();
    mount_setup_doors(
        &server,
        project_id,
        "acme",
        [
            ("slack", "credential_missing"),
            ("notion", "created"),
            ("calendar", "created"),
        ],
    )
    .await;

    let output = command()
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials_path)
        .args(["project", "setup", "acme", "--host", &host, "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["results"][0]["resources"]["drive"]["outcome"],
        "created"
    );
    assert_eq!(
        report["results"][0]["resources"]["slack"]["outcome"],
        "credential_missing"
    );
    assert_eq!(
        report["results"][0]["resources"]["notion"]["outcome"],
        "created"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_keeps_running_after_an_authorization_refusal() {
    let server = MockServer::start().await;
    let host = server.uri();
    let credentials_dir = TempDir::new().unwrap();
    let credentials_path = credentials(&credentials_dir, &host);
    let project_id = Uuid::now_v7();
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": project_id, "code": "acme" }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/app/api/project-surfaces/{project_id}")))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "error": "forbidden",
            "message": "admin tier required"
        })))
        .mount(&server)
        .await;
    for provider in ["slack", "notion", "calendar"] {
        Mock::given(method("POST"))
            .and(path(format!("/app/api/integrations/{provider}/ensure")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [{ "project_code": "acme", "outcome": "created" }]
            })))
            .mount(&server)
            .await;
    }

    let output = command()
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials_path)
        .args(["project", "setup", "acme", "--host", &host, "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["results"][0]["resources"]["drive"]["outcome"],
        "request_failed"
    );
    assert_eq!(
        report["results"][0]["resources"]["github"]["outcome"],
        "request_failed"
    );
    for resource in ["drive", "github"] {
        let detail = report["results"][0]["resources"][resource]["detail"]
            .as_str()
            .unwrap_or_default();
        assert!(detail.contains("403"), "{resource}: {detail}");
        assert!(
            detail.contains("admin tier required"),
            "{resource}: {detail}"
        );
    }
    assert_eq!(
        report["results"][0]["resources"]["slack"]["outcome"],
        "created"
    );
    assert_eq!(
        report["results"][0]["resources"]["notion"]["outcome"],
        "created"
    );
}
