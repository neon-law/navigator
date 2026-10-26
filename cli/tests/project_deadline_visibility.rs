use assert_cmd::Command;
use serde_json::json;
use tempfile::TempDir;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn deadline_visibility_and_description_use_the_existing_lead_fields() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let credentials = root.path().join("credentials.json");
    std::fs::write(
        &credentials,
        json!({"hosts": {&host: {
            "token": "test-token", "person_email": "lawyer@example.com",
            "role": "lawyer", "expires_at": i64::MAX
        }}})
        .to_string(),
    )
    .unwrap();
    let project = "11111111-1111-1111-1111-111111111111";
    let deadline = "22222222-2222-2222-2222-222222222222";
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": project, "code": "sample"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("POST")).and(path(format!("/app/api/projects/{project}/deadlines")))
        .and(body_partial_json(json!({"description":"Synthetic instructions", "client_lead_days":3, "internal_lead_days":7})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": deadline, "description":"Synthetic instructions", "client_lead_days":3})))
        .expect(1).mount(&server).await;
    Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .args([
            "project",
            "deadline",
            "create",
            "--title",
            "Synthetic response",
            "--description",
            "Synthetic instructions",
            "--due-at",
            "2026-11-02T17:00:00-08:00",
            "--authority-kind",
            "internal",
            "--calendar-role",
            "firm",
            "--visibility",
            "client",
            "--client-lead-days",
            "3",
            "--internal-lead-days",
            "7",
            "--host",
            &host,
            "--project",
            "sample",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains(r#""visibility":"client""#));
    Mock::given(method("PATCH"))
        .and(path(format!(
            "/app/api/projects/{project}/deadlines/{deadline}"
        )))
        .and(body_partial_json(json!({"client_lead_days":null})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": deadline, "client_lead_days":null})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .args([
            "project",
            "deadline",
            "update",
            deadline,
            "--visibility",
            "internal",
            "--host",
            &host,
            "--project",
            "sample",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains(r#""visibility":"internal""#));
    server.verify().await;
}

#[test]
fn deadline_creation_requires_a_title() {
    Command::cargo_bin("navigator")
        .unwrap()
        .args([
            "project",
            "deadline",
            "create",
            "--due-at",
            "2026-11-02T17:00:00-08:00",
            "--authority-kind",
            "internal",
            "--calendar-role",
            "firm",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--title"));
}
