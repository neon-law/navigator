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
        "authority_kind": "contract",
        "authority": "Synthetic agreement § 1",
        "source": "lawyer",
        "status": "open",
        "calendar_role": "firm"
    });
    Mock::given(method("POST"))
        .and(path(format!("/app/api/projects/{project_id}/deadlines")))
        .and(header("authorization", "Bearer test-token"))
        .and(|request: &wiremock::Request| {
            request.body_json::<Value>().ok().and_then(|mut body| {
                let key = body.as_object_mut()?.remove("replay_key")?;
                Uuid::parse_str(key.as_str()?).ok()?;
                Some(body)
            }) == Some(json!({
            "title": "Agreement response",
            "due_at": "2026-11-02T17:00:00-08:00",
            "authority_kind": "contract",
            "authority": "Synthetic agreement § 1",
            "calendar_role": "firm",
            "internal_lead_days": 14,
            "client_lead_days": 7
            }))
        })
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
                "Agreement response",
                "--due-at",
                "2026-11-02T17:00:00-08:00",
                "--authority-kind",
                "contract",
                "--authority",
                "Synthetic agreement § 1",
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
async fn deadline_list_identifies_unassigned_categories_without_changing_the_rows() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let project_id = Uuid::new_v4();
    let credentials = root.path().join("credentials.json");
    std::fs::write(
        &credentials,
        json!({"hosts": {host.clone(): {
            "token": "test-token", "person_email": "lawyer@example.com", "role": "lawyer",
            "expires_at": i64::MAX
        }}})
        .to_string(),
    )
    .unwrap();
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id":project_id, "code":"acme"}
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let rows = json!([
        {"id": "unassigned", "category": null, "client_lead_days": null, "authority": "Synthetic rule"},
        {"id": "assigned", "category": "discovery", "client_lead_days": 7, "authority": "Synthetic rule"}
    ]);
    Mock::given(method("GET"))
        .and(path(format!("/app/api/projects/{project_id}/deadlines")))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&rows))
        .expect(1)
        .mount(&server)
        .await;
    let result = run(
        root.path(),
        &credentials,
        &host,
        &["project", "deadline", "list"],
    );
    assert_eq!(result[0]["category_review_required"], true);
    assert_eq!(result[1]["category_review_required"], false);
    assert_eq!(result[0]["visibility"], "internal");
    assert_eq!(result[1]["visibility"], "client");
    for (index, row) in rows.as_array().unwrap().iter().enumerate() {
        for (key, value) in row.as_object().unwrap() {
            assert_eq!(
                &result[index][key], value,
                "the stored {key} survives listing"
            );
        }
    }
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
    assert!(calendar.contains("SUMMARY:acme: Statutory deadline\r\n"));
    assert!(calendar.contains("TRIGGER:-P14D\r\n"));
    server.verify().await;
}

/// The CLI reaches real API handlers and persistence over HTTP. Only session
/// establishment is supplied by the fixture; the API enforces Project scoping.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_api_round_trip_cancels_refuses_replay_and_reopens_the_same_row() {
    store::test_support::with_postgres(deadline_lifecycle()).await;
}

#[allow(clippy::too_many_lines)]
async fn deadline_lifecycle() {
    let pg = store::postgres::PostgresDb::application().await.unwrap();
    let application_store = store::application_store::ApplicationStore::uninitialized();
    let project =
        store::test_support::seed_project_on_firm(&application_store, None, "Synthetic deadline")
            .await;
    let project_id = project.id;
    let person = store::persons::create(
        &application_store,
        &store::persons::NewPerson::with_role(
            "Synthetic Lawyer",
            "lawyer@example.com",
            store::persons::Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    store::participation::add_participant(
        &application_store,
        &store::participation::AddParticipantCommand {
            project_id,
            person_id: person.id,
            dri: store::participation::DriRequest::Unchanged,
            actor: store::participation::DriActor::System,
        },
    )
    .await
    .unwrap();
    let state = portal::test_support::app_state(application_store.clone()).await;
    let app = portal::api::routes()
        .with_state(portal::api::ApiState {
            application_store: state.application_store,
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
            billing_provider: state.billing_provider.clone(),
            canonical_base_url: None,
            jev: None,
        })
        .layer(axum::middleware::from_fn(
            move |request, next: axum::middleware::Next| {
                store::postgres::PostgresDb::with_test_db(pg.clone(), next.run(request))
            },
        ))
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
        store::project_deadlines::list(&application_store, project_id)
            .await
            .unwrap()
            .len(),
        1
    );
    server.abort();
}

fn credentials(root: &std::path::Path, host: &str) -> std::path::PathBuf {
    let credentials = root.join("credentials.json");
    std::fs::write(
        &credentials,
        serde_json::json!({"hosts":{(host):{
            "token":"test-token", "person_email":"lawyer@example.com", "role":"lawyer",
            "expires_at":i64::MAX
        }}})
        .to_string(),
    )
    .unwrap();
    credentials
}

async fn mount_projects(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": Uuid::now_v7(), "code": "acme"}
        ])))
        .mount(server)
        .await;
}

#[tokio::test]
async fn create_accepts_civil_date_without_requiring_a_made_up_cutoff() {
    let server = MockServer::start().await;
    mount_projects(&server).await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let credentials = credentials(root.path(), &host);
    let day = json!({"date":"2028-07-12"});
    Mock::given(method("POST")).and(body_json(json!({
        "title":"Check docket","category":"tickler","calendar_role":"firm","authority_kind":"internal",
        "client_lead_days": null,"due_on":day,"replay_key":"tickler-1"
    }))).respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":Uuid::now_v7(),"due_on":day})))
        .expect(1).mount(&server).await;
    let row = run(
        root.path(),
        &credentials,
        &host,
        &[
            "project",
            "deadline",
            "create",
            "--title",
            "Check docket",
            "--replay-key",
            "tickler-1",
            "--category",
            "tickler",
            "--authority-kind",
            "internal",
            "--calendar-role",
            "firm",
            "--due-on",
            "2028-07-12",
        ],
    );
    assert_eq!(row["due_on"], day);
}

async fn mount_mirror(server: &MockServer, body: Value, status: u16) {
    Mock::given(method("POST"))
        .and(path("/app/api/integrations/calendar/deadline-mirror"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

/// `sync` prints one line per matter the door reports, with the detail
/// sentence where the door supplied one.
#[tokio::test]
async fn deadline_sync_prints_one_line_per_reported_matter() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let credentials = credentials(root.path(), &host);
    Mock::given(method("POST"))
        .and(path("/app/api/integrations/calendar/deadline-mirror"))
        .and(header("authorization", "Bearer test-token"))
        .and(body_json(json!({
            "project_code": "acme",
            "all": false,
            "dry_run": false
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                {"project_code": "acme", "outcome": "mirrored",
                 "detail": "3 events upserted, 1 deleted"},
                {"project_code": "acme", "outcome": "planned"}
            ]
        })))
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
            "sync",
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

    let stdout = String::from_utf8(stdout).unwrap();
    assert!(
        stdout.contains("acme: mirrored — 3 events upserted, 1 deleted"),
        "{stdout}"
    );
    assert!(stdout.contains("acme: planned"), "{stdout}");
    server.verify().await;
}

#[tokio::test]
async fn deadline_sync_after_gateway_timeout_checks_provider_state_without_retrying_the_write() {
    for pending in [0, 2] {
        let server = MockServer::start().await;
        let root = TempDir::new().unwrap();
        let host = server.uri();
        let credentials = credentials(root.path(), &host);
        for (dry_run, response) in [
            (false, ResponseTemplate::new(504)),
            (
                true,
                ResponseTemplate::new(200).set_body_json(json!({"results": [{
                    "project_code": "acme",
                    "outcome": "mirrored",
                    "calendar_recorded": true,
                    "pending_changes": pending,
                    "detail": format!("mirrored upserted={pending} in_sync=1 deleted=0 (dry run)")
                }]})),
            ),
        ] {
            Mock::given(method("POST"))
                .and(path("/app/api/integrations/calendar/deadline-mirror"))
                .and(header("authorization", "Bearer test-token"))
                .and(body_json(
                    json!({"project_code": "acme", "all": false, "dry_run": dry_run}),
                ))
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
        }

        let result = Command::cargo_bin("navigator")
            .unwrap()
            .current_dir(root.path())
            .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
            .args([
                "project",
                "deadline",
                "sync",
                "--project",
                "acme",
                "--host",
                &host,
            ])
            .assert();
        if pending == 0 {
            result
                .success()
                .stdout(predicates::str::contains("verified in sync"));
        } else {
            result
                .failure()
                .stderr(predicates::str::contains("2 pending event changes"));
        }
        server.verify().await;
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }
}

/// A planned pass tells the door `dry_run: true` — nothing writes.
#[tokio::test]
async fn deadline_sync_dry_run_tells_the_door() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let credentials = credentials(root.path(), &host);
    mount_projects(&server).await;
    Mock::given(method("POST"))
        .and(path("/app/api/integrations/calendar/deadline-mirror"))
        .and(header("authorization", "Bearer test-token"))
        .and(body_json(json!({
            "project_code": "acme",
            "all": false,
            "dry_run": true
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{"project_code": "acme", "outcome": "planned"}]
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
            "deadline",
            "sync",
            "--dry-run",
            "--project",
            "acme",
            "--host",
            &host,
        ])
        .assert()
        .success();
    server.verify().await;
}

#[tokio::test]
async fn deadline_sync_fails_when_the_provider_cannot_apply_the_report() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let credentials = credentials(root.path(), &host);
    mount_projects(&server).await;
    mount_mirror(
        &server,
        json!({"results": [
            {"project_code": "acme", "outcome": "provider_unavailable"}
        ]}),
        200,
    )
    .await;

    Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .args([
            "project",
            "deadline",
            "sync",
            "--project",
            "acme",
            "--host",
            &host,
        ])
        .assert()
        .failure()
        .stdout(predicates::str::contains("acme: provider_unavailable"))
        .stderr(predicates::str::contains(
            "check the current state with `navigator project deadline sync --project acme --dry-run`",
        ));
    server.verify().await;
}

/// A Project with no calendar says so and names the provisioning step,
/// whether the pass was applied or only planned.
#[tokio::test]
async fn deadline_sync_names_an_unprovisioned_calendar() {
    for dry_run in [false, true] {
        let server = MockServer::start().await;
        let root = TempDir::new().unwrap();
        let host = server.uri();
        let credentials = credentials(root.path(), &host);
        mount_projects(&server).await;
        mount_mirror(
            &server,
            json!({"results": [{
                "project_code": "acme",
                "outcome": "calendar_unprovisioned",
                "detail": "no calendar recorded; creating it failed with provider_unauthorized"
            }]}),
            200,
        )
        .await;
        let mut args = vec!["project", "deadline", "sync", "--project", "acme"];
        if dry_run {
            args.push("--dry-run");
        }
        args.extend(["--host", host.as_str()]);

        Command::cargo_bin("navigator")
            .unwrap()
            .current_dir(root.path())
            .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
            .args(&args)
            .assert()
            .failure()
            .stdout(predicates::str::contains(
                "acme: calendar_unprovisioned — no calendar recorded; \
                 creating it failed with provider_unauthorized",
            ))
            .stderr(predicates::str::contains(
                "acme: the Project has no calendar yet",
            ));
    }
}

/// A door answer without its results array is a failure, not a silent pass.
#[tokio::test]
async fn deadline_sync_fails_when_the_report_is_not_an_array() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let credentials = credentials(root.path(), &host);
    mount_projects(&server).await;
    mount_mirror(&server, json!({"results": "no"}), 200).await;

    Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .args([
            "project",
            "deadline",
            "sync",
            "--project",
            "acme",
            "--host",
            &host,
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("mirror report is not an array"));
    server.verify().await;
}

#[tokio::test]
async fn calendar_events_reads_the_synthetic_project_window() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    let credentials = root.path().join("credentials.json");
    std::fs::write(
        &credentials,
        json!({"hosts":{host.clone():{
            "token":"test-token", "person_email":"admin@example.com", "role":"admin",
            "expires_at":i64::MAX
        }}})
        .to_string(),
    )
    .unwrap();
    let events = json!([{
        "id": "evt_1", "summary": "Synthetic call",
        "start": {"dateTime": "2026-11-01T13:00:00Z"},
        "end": {"dateTime": "2026-11-01T14:00:00Z"}
    }]);
    Mock::given(method("POST"))
        .and(path("/app/api/integrations/calendar/events"))
        .and(header("authorization", "Bearer test-token"))
        .and(body_json(json!({
            "project_code": "sample-litigation",
            "start": "2026-11-01T00:00:00Z",
            "end": "2026-11-02T00:00:00Z"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(&events))
        .expect(1)
        .mount(&server)
        .await;

    let output = Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .args([
            "calendar",
            "events",
            "--project",
            "sample-litigation",
            "--host",
            &host,
            "--start",
            "2026-11-01T00:00:00Z",
            "--end",
            "2026-11-02T00:00:00Z",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(serde_json::from_slice::<Value>(&output).unwrap(), events);
}
