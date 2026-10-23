//! `navigator project reference add`, `navigator project adverse add`, and
//! `navigator site import --dry-run --as-ci` against a mocked deployment.
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn credentials(dir: &Path, host: &str) -> PathBuf {
    let credentials = dir.join("credentials.json");
    std::fs::write(
        &credentials,
        serde_json::json!({"hosts":{host:{
            "token":"test-token", "person_email":"lawyer@example.com", "role":"lawyer",
            "expires_at":i64::MAX
        }}})
        .to_string(),
    )
    .unwrap();
    credentials
}

async fn seed_bodies(server: &MockServer) -> Vec<serde_json::Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path() == "/app/api/seed")
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect()
}

#[tokio::test]
async fn reference_add_sends_one_project_reference_seed_record() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let credentials = credentials(root.path(), &host);
    Mock::given(method("POST"))
        .and(path("/app/api/seed"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "model": "project_reference", "created": 1, "updated": 0, "unchanged": 0,
            "records": [{"key": "acme / entity:Widget Works LLC / LLC / counterparty_entity", "action": "new"}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .args([
            "project",
            "reference",
            "add",
            "--reason",
            "counterparty_entity",
            "--entity-name",
            "Widget Works LLC",
            "--entity-type",
            "LLC",
            "--jurisdiction",
            "Nevada",
            "--project",
            "acme",
            "--host",
            &host,
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("counterparty_entity"));

    let bodies = seed_bodies(&server).await;
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0]["model"], "project_reference");
    assert_eq!(bodies[0]["overwrite"], false);
    assert_eq!(bodies[0]["dry_run"], false);
    let yaml: serde_yaml::Value =
        serde_yaml::from_str(bodies[0]["yaml"].as_str().unwrap()).unwrap();
    assert_eq!(yaml["lookup_fields"][0], "project_id");
    assert_eq!(yaml["lookup_fields"][1], "reason");
    let record = &yaml["records"][0];
    assert_eq!(record["reason"], "counterparty_entity");
    assert_eq!(record["project"]["code"], "acme");
    assert_eq!(record["entity"]["name"], "Widget Works LLC");
    assert_eq!(
        record["entity"]["entity_type"]["jurisdiction"]["name"],
        "Nevada"
    );
    assert!(record.get("person").is_none());
    server.verify().await;
}

#[tokio::test]
async fn adverse_add_posts_to_the_matters_adverse_door() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let id = Uuid::now_v7();
    let credentials = credentials(root.path(), &host);
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id":id,"code":"acme"}
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/app/api/projects/{id}/relationships/adverse"
        )))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "kind": "adverse_to", "confidence_pct": 100, "source_kind": "manual"
        })))
        .expect(1)
        .mount(&server)
        .await;

    Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .args([
            "project",
            "adverse",
            "add",
            "--person-email",
            "agent@example.com",
            "--subject",
            "client_dri",
            "--project",
            "acme",
            "--host",
            &host,
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("adverse_to"));

    let posted: Vec<serde_json::Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path().ends_with("/relationships/adverse"))
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect();
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0]["person_email"], "agent@example.com");
    assert_eq!(posted[0]["subject"], "client_dri");
    assert!(posted[0].get("entity_name").is_none());
    server.verify().await;
}

/// `--as-ci` asks the deployment for the scoped rules CI carries, naming the
/// checkout's Project, and a scope violation comes back as the same
/// `403 scope_violation` CI would see on `main`: the command fails, and
/// names the refusal.
#[tokio::test]
async fn site_import_dry_run_as_ci_reports_the_scope_violation_ci_would_hit() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let credentials = credentials(root.path(), &host);
    std::fs::write(
        root.path().join("navigator.yaml"),
        format!("host: {host}\nproject: acme\n"),
    )
    .unwrap();
    std::fs::create_dir_all(root.path().join("seeds")).unwrap();
    std::fs::write(
        root.path().join("seeds/Entity.yaml"),
        "lookup_fields:\n  - name\n  - entity_type_id\nrecords:\n  - name: Widget Works LLC\n    entity_type:\n      name: LLC\n      jurisdiction:\n        name: Nevada\n",
    )
    .unwrap();
    Mock::given(method("POST"))
        .and(path("/app/api/seed"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "error": "scope_violation",
            "message": "a scoped session cannot create a new entity; only the entity already linked to its scoped project can be updated"
        })))
        .expect(1)
        .mount(&server)
        .await;

    Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .args(["site", "import", "--dry-run", "--as-ci"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicates::str::contains("scope_violation"))
        .stderr(predicates::str::contains("cannot create a new entity"));

    let bodies = seed_bodies(&server).await;
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0]["model"], "entity");
    assert_eq!(bodies[0]["dry_run"], true);
    assert_eq!(bodies[0]["as_ci"], true);
    assert_eq!(bodies[0]["project"], "acme");
    server.verify().await;
}

#[test]
fn as_ci_requires_a_dry_run_and_a_project_checkout() {
    let root = TempDir::new().unwrap();
    Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .args(["site", "import", "--as-ci"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--dry-run"));
    std::fs::create_dir_all(root.path().join("seeds")).unwrap();
    Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .args(["site", "import", "--dry-run", "--as-ci"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicates::str::contains("navigator.yaml"));
}
