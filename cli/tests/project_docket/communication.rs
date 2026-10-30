//! `navigator project communication` logs a call and prints the stored row,
//! lists the log filtered by channel, confirms a call's agreements by
//! email, and sends nothing on `--dry-run`.
use assert_cmd::Command;
use serde_json::{json, Value};
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
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
async fn create_list_and_confirms_print_the_stored_row_and_dry_run_sends_nothing() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let project_id = Uuid::now_v7();
    let call_id = Uuid::now_v7();
    let email_id = Uuid::now_v7();
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
    let note = root.path().join("call.md");
    std::fs::write(&note, "Discussed the discovery schedule.\n").unwrap();

    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": project_id, "code": "acme"}
        ])))
        .mount(&server)
        .await;
    let call = json!({
        "id": call_id,
        "channel": "call",
        "direction": "internal",
        "call": {"direction": "outbound", "counterparty_role": "opposing_counsel"},
        "agreements": [{"text": "Responses extended to 2026-11-02", "status": "unconfirmed"}]
    });
    Mock::given(method("POST"))
        .and(path(format!(
            "/app/api/projects/{project_id}/communications"
        )))
        .and(body_partial_json(json!({
            "channel": "call",
            "direction": "outbound",
            "counterparty": "counsel@example.com",
            "role": "opposing_counsel",
            "at": "2026-10-09T15:00:00-07:00",
            "duration_minutes": 20,
            "recorded": "none",
            "meet_and_confer": "Responses to Set One",
            "agreements": ["Responses extended to 2026-11-02"],
            "body": "Discussed the discovery schedule.\n"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(&call))
        .expect(1)
        .mount(&server)
        .await;
    let email = json!({ "id": email_id, "channel": "email_outbound", "direction": "outbound" });
    Mock::given(method("POST"))
        .and(path(format!(
            "/app/api/projects/{project_id}/communications"
        )))
        .and(body_partial_json(json!({
            "channel": "email_outbound",
            "confirms": call_id
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(&email))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{project_id}/communications"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!([call.clone(), email.clone()])),
        )
        .expect(1)
        .mount(&server)
        .await;

    let root = root.path();
    let create = [
        "project",
        "communication",
        "create",
        "--channel",
        "call",
        "--direction",
        "outbound",
        "--counterparty",
        "counsel@example.com",
        "--role",
        "opposing_counsel",
        "--at",
        "2026-10-09T15:00:00-07:00",
        "--duration",
        "20",
        "--meet-and-confer",
        "Responses to Set One",
        "--agreement",
        "Responses extended to 2026-11-02",
        "--body-file",
        note.to_str().unwrap(),
    ];
    let mut dry = create.to_vec();
    dry.push("--dry-run");
    let planned = run(root, &credentials, &host, &dry);
    assert_eq!(planned["role"], "opposing_counsel");
    assert_eq!(planned["agreements"][0], "Responses extended to 2026-11-02");
    assert_eq!(run(root, &credentials, &host, &create), call);

    let call_id_text = call_id.to_string();
    assert_eq!(
        run(
            root,
            &credentials,
            &host,
            &[
                "project",
                "communication",
                "create",
                "--channel",
                "email_outbound",
                "--direction",
                "outbound",
                "--counterparty",
                "counsel@example.com",
                "--at",
                "2026-10-09T16:00:00-07:00",
                "--body-file",
                note.to_str().unwrap(),
                "--confirms",
                &call_id_text,
            ],
        ),
        email
    );
    let listed = run(
        root,
        &credentials,
        &host,
        &["project", "communication", "list", "--channel", "call"],
    );
    assert_eq!(
        listed,
        json!([call]),
        "the channel filter keeps only the call"
    );
    server.verify().await;
    let posts = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.method == "POST")
        .count();
    assert_eq!(posts, 2, "a dry run makes no POST");
}

/// `list --direction/--attachments` reaches the door as query parameters
/// and the response's attachment join — asset ids, filenames, kinds —
/// surfaces in the default output (LAW-238 D3, AC10).
#[tokio::test]
async fn list_forwards_direction_and_attachments_and_prints_the_join() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let project_id = Uuid::now_v7();
    let attachment_id = Uuid::now_v7();
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
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": project_id, "code": "acme"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{project_id}/communications"
        )))
        .and(query_param("direction", "inbound"))
        .and(query_param("attachments", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id": Uuid::now_v7(),
            "channel": "email_inbound",
            "direction": "inbound",
            "attachments": [{
                "asset_id": attachment_id,
                "filename": "exhibit-a.pdf",
                "kind": "unclassified"
            }]
        }])))
        .expect(1)
        .mount(&server)
        .await;

    let stdout = Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .args([
            "project",
            "communication",
            "list",
            "--direction",
            "inbound",
            "--attachments",
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
    let listed: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(
        listed[0]["attachments"][0]["asset_id"],
        attachment_id.to_string()
    );
    assert_eq!(
        listed[0]["attachments"][0]["filename"], "exhibit-a.pdf",
        "the join surfaces filenames"
    );
    assert_eq!(listed[0]["attachments"][0]["kind"], "unclassified");
    server.verify().await;
}
