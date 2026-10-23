//! DRI changes use the existing audited participation command doors.
use assert_cmd::Command;
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn participant_dri_designates_and_clears_through_the_authenticated_door() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let id = Uuid::now_v7();
    let row = Uuid::now_v7();
    let credentials = root.path().join("credentials.json");
    std::fs::write(
        &credentials,
        serde_json::json!({"hosts":{host.clone():{
            "token":"test-token", "person_email":"lawyer@example.com", "role":"lawyer",
            "expires_at":i64::MAX
        }}})
        .to_string(),
    )
    .unwrap();
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id":id,"code":"acme"}
        ])))
        .expect(3)
        .mount(&server)
        .await;
    let rows = serde_json::json!([{"id":row,"person_id":Uuid::now_v7(),
        "participation":"client","is_client_dri":false,"is_lawyer_dri":false}]);
    Mock::given(method("GET"))
        .and(path(format!("/app/api/projects/{id}/participants")))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&rows))
        .expect(1)
        .mount(&server)
        .await;
    let output = Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .args([
            "project",
            "participant",
            "list",
            "--project",
            "acme",
            "--host",
            &host,
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
    for verb in ["PUT", "DELETE"] {
        Mock::given(method(verb))
            .and(path(format!(
                "/app/api/projects/{id}/participants/{row}/dri"
            )))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
    }
    for clear in [false, true] {
        let mut cmd = Command::cargo_bin("navigator").unwrap();
        cmd.current_dir(root.path())
            .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
            .args([
                "project",
                "participant",
                "dri",
                &row.to_string(),
                "--project",
                "acme",
                "--host",
                &host,
            ]);
        if clear {
            cmd.arg("--clear");
        }
        cmd.assert().success();
    }
    server.verify().await;
}
