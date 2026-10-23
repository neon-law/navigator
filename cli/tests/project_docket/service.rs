//! `navigator project service` prints the row the portal stored, and
//! `add --dry-run` prints its request and makes no write.
use assert_cmd::Command;
use serde_json::{json, Value};
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn run(root: &std::path::Path, credentials: &std::path::Path, host: &str, args: &[&str]) -> Value {
    let mut full = args.to_vec();
    full.extend(["--project", "acme", "--host", host]);
    let stdout = Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root)
        .env("NAVIGATOR_CREDENTIALS_FILE", credentials)
        .args(full)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&stdout).unwrap()
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn add_list_and_show_print_the_stored_row_and_dry_run_sends_nothing() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let project_id = Uuid::now_v7();
    let service_id = Uuid::now_v7();
    let credentials = root.path().join("credentials.json");
    std::fs::write(
        &credentials,
        json!({"hosts":{host.clone():{
            "token":"test-token", "person_email":"lawyer@example.com", "role":"lawyer",
            "expires_at":i64::MAX
        }}})
        .to_string(),
    )
    .unwrap();
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": project_id, "code": "acme"}
        ])))
        .expect(3)
        .mount(&server)
        .await;
    let created = json!({
        "id": service_id,
        "project_id": project_id,
        "recipient": "Example Counsel",
        "method": "mail",
        "served_on": "2026-03-05",
        "reference": "D.I. 12"
    });
    Mock::given(method("POST"))
        .and(path(format!("/app/api/projects/{project_id}/service")))
        .and(header("authorization", "Bearer test-token"))
        .and(body_json(json!({
            "slug": "pleadings/motion.pdf",
            "recipient": "Example Counsel",
            "counsel_for": "Defendant Example Co.",
            "method": "mail",
            "served_on": "2026-03-05",
            "address": "200 Sample Street, Sampleton, CA 90000",
            "declarant": "Pat Sample"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(&created))
        .expect(1)
        .mount(&server)
        .await;
    let listed = json!([created.clone()]);
    Mock::given(method("GET"))
        .and(path(format!("/app/api/projects/{project_id}/service")))
        .respond_with(ResponseTemplate::new(200).set_body_json(&listed))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{project_id}/service/{service_id}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(&created))
        .expect(1)
        .mount(&server)
        .await;

    let root = root.path();
    let add = [
        "project",
        "service",
        "add",
        "--slug",
        "pleadings/motion.pdf",
        "--recipient",
        "Example Counsel",
        "--counsel-for",
        "Defendant Example Co.",
        "--method",
        "mail",
        "--served-on",
        "2026-03-05",
        "--address",
        "200 Sample Street, Sampleton, CA 90000",
        "--declarant",
        "Pat Sample",
    ];
    let mut dry = add.to_vec();
    dry.push("--dry-run");
    let planned = run(root, &credentials, &host, &dry);
    assert_eq!(planned["slug"], "pleadings/motion.pdf");
    assert_eq!(planned["method"], "mail");
    assert_eq!(run(root, &credentials, &host, &add), created);
    assert_eq!(
        run(root, &credentials, &host, &["project", "service", "list"]),
        listed
    );
    assert_eq!(
        run(
            root,
            &credentials,
            &host,
            &["project", "service", "show", &service_id.to_string()],
        ),
        created
    );
    server.verify().await;
    let posts = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.method == "POST")
        .count();
    assert_eq!(posts, 1, "a dry run makes no POST");
}
