//! `navigator project deadline` prints the row the portal stored.
use assert_cmd::Command;
use serde_json::{json, Value};
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Stored {
    created: Value,
    listed: Value,
    updated: Value,
    completed: Value,
}

async fn mount(server: &MockServer, project_id: Uuid, deadline_id: Uuid) -> Stored {
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": project_id, "code": "acme"}
        ])))
        .expect(4)
        .mount(server)
        .await;
    let created = json!({
        "id": deadline_id,
        "project_id": project_id,
        "due_at": "2026-11-03T01:00:00+00:00",
        "authority_kind": "statute",
        "authority": "15 U.S.C. § 1",
        "source": "lawyer",
        "status": "open",
        "calendar_role": "firm"
    });
    Mock::given(method("POST"))
        .and(path(format!("/app/api/projects/{project_id}/deadlines")))
        .and(header("authorization", "Bearer test-token"))
        .and(body_json(json!({
            "title": "Answer to the complaint",
            "due_at": "2026-11-02T17:00:00-08:00",
            "authority_kind": "statute",
            "authority": "15 U.S.C. § 1",
            "calendar_role": "firm",
            "internal_lead_days": 14,
            "client_lead_days": 7
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(&created))
        .expect(1)
        .mount(server)
        .await;
    let listed = json!([created.clone()]);
    Mock::given(method("GET"))
        .and(path(format!("/app/api/projects/{project_id}/deadlines")))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&listed))
        .expect(1)
        .mount(server)
        .await;
    let updated = json!({
        "id": deadline_id,
        "status": "open",
        "calendar_role": "opponent"
    });
    Mock::given(method("PATCH"))
        .and(path(format!(
            "/app/api/projects/{project_id}/deadlines/{deadline_id}"
        )))
        .and(header("authorization", "Bearer test-token"))
        .and(body_json(
            json!({"title": "Reply brief", "calendar_role": "opponent"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(&updated))
        .expect(1)
        .mount(server)
        .await;
    let completed = json!({
        "id": deadline_id,
        "status": "complete",
        "calendar_role": "opponent"
    });
    Mock::given(method("POST"))
        .and(path(format!(
            "/app/api/projects/{project_id}/deadlines/{deadline_id}/complete"
        )))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&completed))
        .expect(1)
        .mount(server)
        .await;
    Stored {
        created,
        listed,
        updated,
        completed,
    }
}

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
async fn deadline_create_list_update_and_complete_print_the_stored_row() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let project_id = Uuid::now_v7();
    let deadline_id = Uuid::now_v7();
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
    let stored = mount(&server, project_id, deadline_id).await;
    let id = deadline_id.to_string();
    let root = root.path();

    assert_eq!(
        run(
            root,
            &credentials,
            &host,
            &[
                "project",
                "deadline",
                "create",
                "--title",
                "Answer to the complaint",
                "--due-at",
                "2026-11-02T17:00:00-08:00",
                "--authority-kind",
                "statute",
                "--authority",
                "15 U.S.C. § 1",
                "--calendar-role",
                "firm",
                "--internal-lead-days",
                "14",
                "--client-lead-days",
                "7",
                "--visibility",
                "client",
            ],
        ),
        stored.created
    );
    assert_eq!(
        run(root, &credentials, &host, &["project", "deadline", "list"]),
        stored.listed
    );
    assert_eq!(
        run(
            root,
            &credentials,
            &host,
            &[
                "project",
                "deadline",
                "update",
                id.as_str(),
                "--title",
                "Reply brief",
                "--calendar-role",
                "opponent",
            ],
        ),
        stored.updated
    );
    assert_eq!(
        run(
            root,
            &credentials,
            &host,
            &["project", "deadline", "complete", id.as_str()],
        ),
        stored.completed
    );
    server.verify().await;
}

#[tokio::test]
async fn deadline_ics_prints_one_calendar_event_per_row() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let project_id = Uuid::now_v7();
    let deadline_id = Uuid::now_v7();
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
        .and(path(format!("/app/api/projects/{project_id}/deadlines")))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id": deadline_id,
            "due_at": "2026-11-02T17:00:00-08:00",
            "authority_kind": "statute",
            "authority": "15 U.S.C. § 1",
            "status": "open",
            "calendar_role": "firm",
            "internal_lead_days": 14,
            "updated_at": "2026-10-06T00:00:00+00:00"
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
            "deadline",
            "ics",
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
    let calendar = String::from_utf8(stdout).unwrap();
    assert!(calendar.starts_with("BEGIN:VCALENDAR\r\n"));
    assert!(calendar.ends_with("END:VCALENDAR\r\n"));
    assert_eq!(calendar.matches("BEGIN:VEVENT").count(), 1);
    assert!(calendar.contains(&format!("UID:{deadline_id}@127.0.0.1\r\n")));
    assert!(calendar.contains("SUMMARY:Statutory deadline - acme\r\n"));
    assert!(calendar.contains("TRIGGER:-P14D\r\n"));
    server.verify().await;
}

/// The CLI reaches real API handlers and persistence over HTTP. Only session
/// establishment is supplied by the fixture; the API enforces Project scoping.
#[allow(clippy::too_many_lines)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_api_round_trip_cancels_refuses_replay_and_reopens_the_same_row() {
    let surreal = store::test_support::mem_surreal().await;
    let project_id =
        store::test_support::seed_project_surreal(&surreal, "Synthetic deadline").await;
    let project = store::projects::find_by_id(&surreal, project_id)
        .await
        .unwrap()
        .unwrap();
    let person = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Synthetic Lawyer",
            "lawyer@example.com",
            store::persons::Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    store::participation::add_participant(
        &surreal,
        &store::participation::AddParticipantCommand {
            project_id,
            person_id: person.id,
            dri: store::participation::DriRequest::Unchanged,
            actor: store::participation::DriActor::System,
        },
    )
    .await
    .unwrap();
    let state = portal::test_support::app_state(surreal.clone()).await;
    let app = portal::api::routes()
        .with_state(portal::api::ApiState {
            surreal: state.surreal,
            email: state.email,
            bootstrap_owner_email: state.bootstrap_owner_email,
            bootstrap_company: "Synthetic Firm".into(),
            questionnaire_runtime: state.questionnaire_runtime,
            storage: state.storage,
            workflow_runtime: state.workflow_runtime,
            assets_storage: state.assets_storage,
            forms_registry: state.forms_registry,
            signature_provider: state.signature_provider,
            contract_reviewer: state.contract_reviewer,
            integration_providers: state.integration_providers,
        })
        .layer(axum::Extension(portal::SessionData {
            person_id: Some(person.id),
            ..portal::SessionData::fresh("lawyer@example.com", store::persons::Role::Lawyer)
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let root = TempDir::new().unwrap();
    let credentials = root.path().join("credentials.json");
    std::fs::write(&credentials, json!({"hosts": {host.clone(): {
        "token":"test-token", "person_email":"lawyer@example.com", "role":"lawyer", "expires_at":i64::MAX
    }}}).to_string()).unwrap();
    let invoke = |args: &[&str]| {
        let mut command = Command::cargo_bin("navigator").unwrap();
        command
            .current_dir(root.path())
            .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
            .args(["project", "deadline"])
            .args(args)
            .args(["--project", &project.code, "--host", &host]);
        command
    };
    let create = [
        "create",
        "--title",
        "Synthetic response",
        "--due-at",
        "2026-11-02T17:00:00-08:00",
        "--authority-kind",
        "internal",
        "--calendar-role",
        "firm",
        "--replay-key",
        "notice-1",
    ];
    let created: Value =
        serde_json::from_slice(&invoke(&create).assert().success().get_output().stdout).unwrap();
    let id = created["id"].as_str().unwrap();
    let cancelled: Value = serde_json::from_slice(
        &invoke(&["cancel", id])
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .unwrap();
    assert_eq!(cancelled["status"], "cancelled");
    invoke(&create)
        .assert()
        .failure()
        .stderr(predicates::str::contains("cancelled"))
        .stderr(predicates::str::contains("reopen"));
    let listed: Value =
        serde_json::from_slice(&invoke(&["list"]).assert().success().get_output().stdout).unwrap();
    assert_eq!(listed, json!([cancelled]));
    let reopened: Value = serde_json::from_slice(
        &invoke(&["reopen", id])
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .unwrap();
    assert_eq!(reopened["status"], "open");
    assert_eq!(reopened["id"], created["id"]);
    assert_eq!(reopened["inserted_at"], created["inserted_at"]);
    assert_eq!(reopened["replay_key"], "notice-1");
    let repeated: Value = serde_json::from_slice(
        &invoke(&["reopen", id])
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .unwrap();
    assert_eq!(repeated, reopened);
    let replayed: Value =
        serde_json::from_slice(&invoke(&create).assert().success().get_output().stdout).unwrap();
    assert_eq!(replayed["id"], id);
    assert_eq!(replayed["status"], "open");
    invoke(&["complete", id]).assert().success();
    invoke(&["reopen", id])
        .assert()
        .failure()
        .stderr(predicates::str::contains("complete"));
    assert_eq!(
        store::project_deadlines::list(&surreal, project_id)
            .await
            .unwrap()
            .len(),
        1
    );
    server.abort();
}
