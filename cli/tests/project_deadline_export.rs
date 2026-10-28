use assert_cmd::Command;
use serde_json::json;
use tempfile::TempDir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn saved_deadline_exports_follow_the_create_and_update_responses() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let credentials = root.path().join("credentials.json");
    write_credentials(&credentials, &host);
    let project = "11111111-1111-1111-1111-111111111111";
    let deadline = "22222222-2222-2222-2222-222222222222";
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": project, "code": "sample"}
        ])))
        .mount(&server)
        .await;
    for (verb, route, stamp, title) in [
        (
            "POST",
            format!("/app/api/projects/{project}/deadlines"),
            "2026-10-26T00:00:00Z",
            "Saved response",
        ),
        (
            "PATCH",
            format!("/app/api/projects/{project}/deadlines/{deadline}"),
            "2026-10-26T00:00:00Z",
            "Saved revision",
        ),
    ] {
        Mock::given(method(verb))
            .and(path(route))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": deadline, "title": title, "due_at": "2026-11-02T17:00:00-08:00",
                "updated_at": stamp, "status": "open", "calendar_sequence": i32::from(verb != "POST")
            })))
            .expect(1)
            .mount(&server)
            .await;
    }
    let export = root.path().join("deadline.ics");
    let mut prior = String::new();
    for action in ["create", "update"] {
        let mut command = Command::cargo_bin("navigator").unwrap();
        command
            .current_dir(root.path())
            .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
            .args(["project", "deadline", action]);
        if action == "update" {
            command.arg(deadline);
        }
        command.args([
            "--title",
            "Requested response",
            "--due-at",
            "2026-11-02T17:00:00-08:00",
        ]);
        if action == "create" {
            command.args(["--authority-kind", "internal", "--calendar-role", "firm"]);
        }
        command
            .args([
                "--host",
                &host,
                "--project",
                "sample",
                "--location",
                "https://court.test/join",
            ])
            .arg("--ics")
            .arg(&export)
            .assert()
            .success();
        let calendar = std::fs::read_to_string(&export).unwrap();
        assert!(calendar.contains(&format!("UID:{deadline}@127.0.0.1\r\n")));
        assert!(calendar.contains("LOCATION:https://court.test/join\r\n"));
        assert!(calendar.contains(if action == "create" {
            "Saved response"
        } else {
            "Saved revision"
        }));
        assert!(calendar.contains(if action == "create" {
            "SEQUENCE:0\r\n"
        } else {
            "SEQUENCE:1\r\n"
        }));
        if !prior.is_empty() {
            assert_ne!(calendar, prior);
        }
        prior = calendar;
    }
    assert_location_is_private(&server).await;
    server.verify().await;
}

#[test]
fn a_location_requires_an_export_path() {
    Command::cargo_bin("navigator")
        .unwrap()
        .args([
            "project",
            "deadline",
            "create",
            "--title",
            "Synthetic response",
            "--due-at",
            "2026-11-02T17:00:00-08:00",
            "--authority-kind",
            "internal",
            "--calendar-role",
            "firm",
            "--location",
            "https://court.test/join",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--ics"));
}

fn write_credentials(credentials: &std::path::Path, host: &str) {
    std::fs::write(
        credentials,
        json!({"hosts": {host: {
            "token": "test-token", "person_email": "lawyer@example.com",
            "role": "lawyer", "expires_at": i64::MAX
        }}})
        .to_string(),
    )
    .unwrap();
}

async fn assert_location_is_private(server: &MockServer) {
    let requests = server.received_requests().await.unwrap();
    for request in requests
        .iter()
        .filter(|request| request.method.as_str() != "GET")
    {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert!(
            body.get("location").is_none(),
            "location belongs only in the private export"
        );
    }
}
