//! `POST /app/api/seed` with the `relationship_log` model, through the full
//! router and the embedded Rego policy — the door `navigator project docket
//! ingest` records a Service List through.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use store::persons::{NewPerson, Role};
use store::test_support::mem_surreal;
use tower::ServiceExt;

async fn seed(
    app: &axum::Router,
    bearer: &str,
    model: &str,
    yaml: &str,
) -> (StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/app/api/seed")
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "model": model,
                        "yaml": yaml,
                        "overwrite": false,
                        "dry_run": false,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap_or_default())
}

#[tokio::test]
async fn a_lawyer_records_a_service_list_person_and_their_relationship_log_row() {
    let surreal = mem_surreal().await;
    let lawyer = store::persons::create(
        &surreal,
        &NewPerson::with_role("Lawyer Example", "lawyer@neonlaw.com", Role::Lawyer),
    )
    .await
    .unwrap();
    let mut state = portal::test_support::app_state(surreal.clone()).await;
    state.policy = portal::policy::PolicyClient::embedded().expect("embedded policy compiles");
    let app = portal::router(state);
    let sessions = portal::SessionStore::new(portal::test_support::TEST_SESSION_KEY);
    let mut session = portal::SessionData::fresh("lawyer-sub", Role::Lawyer);
    session.person_id = Some(lawyer.id);
    let bearer = format!("Bearer {}", sessions.encode(&session));

    let (status, report) = seed(
        &app,
        &bearer,
        "person",
        "lookup_fields:\n  - email\nrecords:\n  - email: gus.gavel@example.com\n    name: Gus Gavel\n",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");

    let entry = "lookup_fields:\n  - subject_id\n  - action\n  - detail\nrecords:\n  - subject:\n      email: gus.gavel@example.com\n    action: on_service_list\n    detail: Gus Gavel is on the service list in 9:26-cv-00042-XYZ, per D.I. 12\n";
    let (status, report) = seed(&app, &bearer, "relationship_log", entry).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["created"], 1, "{report}");
    let (status, report) = seed(&app, &bearer, "relationship_log", entry).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["unchanged"], 1, "a re-run appends nothing: {report}");

    let person = store::persons::find_by_email_ci(&surreal, "gus.gavel@example.com")
        .await
        .unwrap()
        .expect("the service-list person exists");
    assert_eq!(person.role, Role::Client);
    let rows = store::relationship_logs::for_subject(&surreal, "person", person.id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].actor_person_id, Some(lawyer.id));
    assert!(
        store::projects::participations_for_person(&surreal, person.id)
            .await
            .unwrap()
            .is_empty(),
        "a service-list person is never placed on a Project"
    );
}

#[tokio::test]
async fn a_client_cannot_write_the_relationship_log() {
    let surreal = mem_surreal().await;
    let mut state = portal::test_support::app_state(surreal).await;
    state.policy = portal::policy::PolicyClient::embedded().expect("embedded policy compiles");
    let app = portal::router(state);
    let (status, _) = seed(
        &app,
        &portal::test_support::bearer_header(Role::Client),
        "relationship_log",
        "lookup_fields:\n  - subject_id\n  - action\n  - detail\nrecords: []\n",
    )
    .await;
    assert!(
        status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND,
        "{status}"
    );
}
