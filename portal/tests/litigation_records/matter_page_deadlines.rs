//! LAW-126: the matter page at `/app/projects/{code}` shows the matter's open
//! Deadlines, and a client sees only the ones the firm chose to warn them
//! about — a row with a client lead time. An internal date never reaches a
//! client's markup. A Lawyer's deadline write also clears the assembled
//! router's Rego gate, not only the bare API routes.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use store::persons::Role;
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

const CLIENT_AUTHORITY: &str = "Notice of hearing on demurrer";
const INTERNAL_AUTHORITY: &str = "Draft opposition ready for review";

struct Fixture {
    app: axum::Router,
    code: String,
    project_id: Uuid,
    client_cookie: String,
    lawyer_cookie: String,
    lawyer_csrf: String,
}

async fn person(surreal: &store::surreal::SurrealDb, name: &str, email: &str, role: Role) -> Uuid {
    store::persons::create(
        surreal,
        &store::persons::NewPerson::with_role(name, email, role),
    )
    .await
    .unwrap()
    .id
}

fn deadline(
    project_id: Uuid,
    authority_kind: &str,
    authority: &str,
    calendar_role: &str,
    client_lead_days: Option<i64>,
) -> store::project_deadlines::NewDeadline {
    store::project_deadlines::NewDeadline {
        title: None,
        description: None,
        project_id,
        // Far enough out that the test never turns into an overdue one.
        due_at: "2099-11-02T17:00:00-08:00".into(),
        authority_kind: authority_kind.into(),
        authority: authority.into(),
        source: "lawyer".into(),
        source_document_id: None,
        owner_person_id: None,
        calendar_role: calendar_role.into(),
        internal_lead_days: Some(7),
        client_lead_days,
        replay_key: None,
    }
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: "libra-formation".into(),
            name: "libra-formation".into(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&surreal).await,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let client = person(&surreal, "Libra", "libra@example.com", Role::Client).await;
    store::projects::add_participation(&surreal, project.id, client, "client")
        .await
        .unwrap();
    let lawyer = person(&surreal, "Counsel", "counsel@example.com", Role::Lawyer).await;
    store::projects::add_participation(&surreal, project.id, lawyer, "lawyer")
        .await
        .unwrap();

    store::project_deadlines::create(
        &surreal,
        &deadline(
            project.id,
            "court_order",
            CLIENT_AUTHORITY,
            "hearing",
            Some(3),
        ),
    )
    .await
    .unwrap();
    store::project_deadlines::create(
        &surreal,
        &deadline(project.id, "internal", INTERNAL_AUTHORITY, "firm", None),
    )
    .await
    .unwrap();

    let sessions = portal::SessionStore::new(portal::test_support::TEST_SESSION_KEY);
    let session_for = |sub: &str, person_id: Uuid, role: Role| {
        let mut session = portal::SessionData::fresh(sub, role);
        session.person_id = Some(person_id);
        session
    };
    let cookie_for = |session: &portal::SessionData| {
        format!(
            "{}={}",
            portal::session::SESSION_COOKIE_NAME,
            sessions.encode(session)
        )
    };
    let client_session = session_for("libra-sub", client, Role::Client);
    let lawyer_session = session_for("counsel-sub", lawyer, Role::Lawyer);

    // The embedded Rego policy, not the test passthrough: the deadline write
    // below must clear the same gate production runs.
    let mut state = portal::test_support::app_state(surreal.clone()).await;
    state.policy = portal::policy::PolicyClient::embedded().expect("embedded policy compiles");
    Fixture {
        app: portal::router(state),
        code: project.code,
        project_id: project.id,
        client_cookie: cookie_for(&client_session),
        lawyer_cookie: cookie_for(&lawyer_session),
        lawyer_csrf: lawyer_session.csrf_token.clone(),
    }
}

async fn matter_page(f: &Fixture, cookie: &str) -> String {
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/app/projects/{}", f.code))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[tokio::test]
async fn the_firm_sees_every_open_deadline_on_the_matter_page() {
    let f = fixture().await;
    let html = matter_page(&f, &f.lawyer_cookie).await;
    assert!(html.contains("id=\"deadlines\""), "{html}");
    assert!(html.contains(CLIENT_AUTHORITY), "{html}");
    assert!(html.contains(INTERNAL_AUTHORITY), "{html}");
}

#[tokio::test]
async fn a_client_sees_only_deadlines_with_a_client_lead() {
    let f = fixture().await;
    let html = matter_page(&f, &f.client_cookie).await;
    assert!(html.contains("id=\"deadlines\""), "{html}");
    assert!(html.contains(CLIENT_AUTHORITY), "{html}");
    assert!(
        !html.contains(INTERNAL_AUTHORITY),
        "an internal date must not reach a client's markup or hydration payload"
    );
}

/// The writes behind `navigator project deadline` must reach a Lawyer through
/// the policy layer. Before LAW-126 only Owner and Admin got through, by the
/// route bypass, and a Lawyer's create answered 403.
#[tokio::test]
async fn a_lawyer_records_a_deadline_through_the_assembled_router() {
    let f = fixture().await;
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/app/api/projects/{}/deadlines", f.project_id))
                .header("cookie", &f.lawyer_cookie)
                .header("x-csrf-token", &f.lawyer_csrf)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "title": "Synthetic response",
                        "description": "Synthetic private preparation instructions",
                        "internal_lead_days": 7,
                        "client_lead_days": 3,
                        "due_at": "2099-12-01T17:00:00-08:00",
                        "authority_kind": "internal",
                        "authority": "Synthetic preparation target",
                        "source": "lawyer",
                        "calendar_role": "firm",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(
        status.is_success(),
        "{status}: {}",
        String::from_utf8_lossy(&bytes)
    );
    let html = matter_page(&f, &f.lawyer_cookie).await;
    assert!(html.contains("Synthetic preparation target"), "{html}");
    assert!(html.contains("Synthetic private preparation instructions"));
    let client = matter_page(&f, &f.client_cookie).await;
    assert!(client.contains("Synthetic private preparation instructions"));
    let row: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!(
                    "/app/api/projects/{}/deadlines/{}",
                    f.project_id,
                    row["id"].as_str().unwrap()
                ))
                .header("cookie", &f.lawyer_cookie)
                .header("x-csrf-token", &f.lawyer_csrf)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"client_lead_days":null}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response.status().is_success());
    let internal = matter_page(&f, &f.client_cookie).await;
    assert!(!internal.contains("Synthetic private preparation instructions"));
    assert!(!internal.contains("Synthetic response"));
    let lawyer = matter_page(&f, &f.lawyer_cookie).await;
    assert!(lawyer.contains("Synthetic private preparation instructions"));
}
