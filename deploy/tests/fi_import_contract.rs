//! Fi's CLI validates its explicit mapping locally and defaults to server preview.
use assert_cmd::Command;
use serde_json::json;
use std::path::Path;
use tempfile::TempDir;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn mapping() -> serde_json::Value {
    json!({"columns":{"kind":"type","direction":"way","counterparty":"phone","timestamp":["at"],"duration":"length","identity":"id","status":null},
        "kinds":{"C":"call","S":"sms"},"directions":{"I":"inbound","O":"outbound"},"timestamp_format":"%+",
        "timestamp_separator":" ","timezone":{"mode":"embedded_offset"},"duration_unit":"seconds"})
}

fn command(root: &Path, host: &str) -> Command {
    let payload = root.join("metadata.csv");
    let map = root.join("mapping.json");
    let credentials = root.join("credentials.json");
    std::fs::write(
        &payload,
        "type,way,phone,at,length,id\nS,O,+15555550456,2026-10-30T18:00:00Z,,one\n",
    )
    .unwrap();
    std::fs::write(&map, serde_json::to_vec(&mapping()).unwrap()).unwrap();
    std::fs::write(&credentials,serde_json::to_vec(&json!({"hosts":{host:{"token":"synthetic-token","person_email":"lawyer@example.test","role":"lawyer","expires_at":i64::MAX}}})).unwrap()).unwrap();
    let mut command = Command::cargo_bin("navigator-deploy").unwrap();
    command
        .env_remove("GITHUB_REPOSITORY")
        .env_remove("GITHUB_REF")
        .env_remove("GITHUB_EVENT_NAME")
        .env("NAVIGATOR_RELEASE_TAG", env!("CARGO_PKG_VERSION"))
        .env("NAVIGATOR_CREDENTIALS_FILE", credentials)
        .args([
            "inbox",
            "import-fi",
            "--account=+15555550123",
            "--brand=example",
            "--host",
            host,
        ])
        .arg("--file")
        .arg(payload)
        .arg("--mapping")
        .arg(map);
    command
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fi_defaults_to_preview_and_only_apply_requests_mutation() {
    let root = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/app/api/inbox/import/fi"))
        .and(body_partial_json(json!({"dry_run":true})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"dry_run":true,"new":1,"existing":0,"conflicts":0})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/api/inbox/import/fi"))
        .and(body_partial_json(json!({"dry_run":false})))
        .respond_with(ResponseTemplate::new(201).set_body_json(
            json!({"dry_run":false,"imported":[{"message_id":"synthetic-id","deduped":false}]}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let output = command(root.path(), &server.uri())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let preview: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(preview["dry_run"], true);
    assert!(preview.get("payload_base64").is_none());
    command(root.path(), &server.uri())
        .arg("--apply")
        .assert()
        .success();
    server.verify().await;
}

#[test]
fn fi_requires_an_explicit_mapping_and_the_parser_refuses_ambiguous_rows_before_network() {
    let root = TempDir::new().unwrap();
    let mut command = command(root.path(), "http://127.0.0.1:1");
    std::fs::write(root.path().join("metadata.csv"),"type,way,phone,at,length,id\nC,I,+15555550456,2026-10-30T18:00:00Z,1,one\nC,I,+15555550456,2026-10-30T18:00:00Z,1,one\n").unwrap();
    let output = command.assert().failure().get_output().stderr.clone();
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("repeat an identity"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fi_metadata_and_credentials_never_follow_a_redirect() {
    let root = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let destination = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/app/api/inbox/import/fi"))
        .respond_with(ResponseTemplate::new(307).insert_header("location", destination.uri()))
        .expect(1)
        .mount(&server)
        .await;
    command(root.path(), &server.uri())
        .arg("--apply")
        .assert()
        .failure();
    assert!(destination.received_requests().await.unwrap().is_empty());
    server.verify().await;
}
