//! A Project reference never reaches a party list.
//!
//! The participation ledger `GET /app/api/projects/{id}/participants` serves
//! a Project portal's party surface, and the matter workbench renders the
//! same ledger as "Project people". A counterparty the matter references
//! through `project_reference` appears in neither: a reference grants no
//! standing, so it has no row to render.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use portal::session::{SessionData, SESSION_COOKIE_NAME};
use portal::{AppState, SessionStore};
use store::persons::{NewPerson, Role};
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

const KEY: &str = "project-reference-party-list-test-key";

struct Fixture {
    app: axum::Router,
    project_id: Uuid,
    project_code: String,
    lawyer: Uuid,
    /// The referenced registered agent: the row no party list may carry.
    agent: Uuid,
}

async fn build() -> Fixture {
    let surreal = mem_surreal().await;
    let project = store::test_support::seed_project(&surreal, "Widget matter").await;

    let lawyer = store::persons::create(
        &surreal,
        &NewPerson::with_role("Matter Lawyer", "lawyer@example.com", Role::Lawyer),
    )
    .await
    .unwrap()
    .id;
    store::projects::designate_dri_in_surreal(
        &surreal,
        project.id,
        lawyer,
        store::projects::DriSide::Lawyer,
    )
    .await
    .unwrap();
    let client = store::persons::create(
        &surreal,
        &NewPerson::with_role("Client Example", "client@example.com", Role::Client),
    )
    .await
    .unwrap()
    .id;
    store::projects::add_participation(&surreal, project.id, client, "client")
        .await
        .unwrap();

    // The counterparty's registered agent, referenced and nothing more.
    let agent = store::persons::create(
        &surreal,
        &NewPerson::with_role("Registered Agent", "agent@example.com", Role::Client),
    )
    .await
    .unwrap()
    .id;
    store::project_references::reference(
        &surreal,
        project.id,
        store::project_references::Party::person(agent),
        store::project_references::REASON_REGISTERED_AGENT,
    )
    .await
    .unwrap();

    let state = AppState {
        sessions: SessionStore::new(KEY),
        ..portal::test_support::app_state(surreal.clone()).await
    };
    Fixture {
        app: server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR)),
        project_id: project.id,
        project_code: project.code,
        lawyer,
        agent,
    }
}

fn session(person_id: Uuid) -> String {
    let mut session = SessionData::fresh("party-list-sub", Role::Lawyer);
    session.person_id = Some(person_id);
    SessionStore::new(KEY).encode(&session)
}

#[tokio::test]
async fn the_participation_ledger_omits_a_referenced_person() {
    let fixture = build().await;
    let resp = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/app/api/projects/{}/participants",
                    fixture.project_id
                ))
                .header(
                    "authorization",
                    format!("Bearer {}", session(fixture.lawyer)),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    let participations: Vec<&str> = rows
        .iter()
        .filter_map(|row| row["participation"].as_str())
        .collect();
    assert_eq!(
        rows.len(),
        2,
        "the lawyer and the client, nobody else: {rows:?}"
    );
    assert!(participations.contains(&"lawyer") && participations.contains(&"client"));
    assert!(
        !rows
            .iter()
            .any(|row| row["person_id"] == fixture.agent.to_string()),
        "no reference row in the ledger: {rows:?}"
    );
}

#[tokio::test]
async fn the_matter_workbench_people_table_omits_a_referenced_person() {
    let fixture = build().await;
    let resp = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/app/projects/{}", fixture.project_code))
                .header(
                    header::COOKIE,
                    format!("{SESSION_COOKIE_NAME}={}", session(fixture.lawyer)),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("Project people"), "{html}");
    assert!(
        html.contains("Client Example"),
        "the participant renders: {html}"
    );
    assert!(
        !html.contains("Registered Agent") && !html.contains("agent@example.com"),
        "a referenced person never renders on the matter: {html}"
    );
}
