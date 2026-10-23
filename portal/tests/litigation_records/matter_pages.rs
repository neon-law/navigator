//! Matter Pages at `/app/projects/{code}/{slug}`: a Lawyer stores the
//! manifest's `pages:` through the assembled router, and each reader then
//! sees only their own lens. A client never receives a lawyer-lens section or
//! a firm-only date; a non-participant and an unknown slug are the same 404.

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
    outsider_cookie: String,
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
    authority: &str,
    calendar_role: &str,
    client_lead_days: Option<i64>,
) -> store::project_deadlines::NewDeadline {
    store::project_deadlines::NewDeadline {
        project_id,
        due_at: "2099-11-02T17:00:00-08:00".into(),
        authority_kind: "court_order".into(),
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
            code: "sample-pages".into(),
            name: "Sample pages matter".into(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&surreal).await,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let client = person(
        &surreal,
        "Sample Client",
        "pages-client@example.com",
        Role::Client,
    )
    .await;
    store::projects::add_participation(&surreal, project.id, client, "client")
        .await
        .unwrap();
    let lawyer = person(
        &surreal,
        "Sample Counsel",
        "pages-counsel@example.com",
        Role::Lawyer,
    )
    .await;
    store::projects::add_participation(&surreal, project.id, lawyer, "lawyer")
        .await
        .unwrap();
    let outsider = person(
        &surreal,
        "Other Client",
        "pages-other@example.com",
        Role::Client,
    )
    .await;
    store::project_deadlines::create(
        &surreal,
        &deadline(project.id, CLIENT_AUTHORITY, "hearing", Some(3)),
    )
    .await
    .unwrap();
    store::project_deadlines::create(
        &surreal,
        &deadline(project.id, INTERNAL_AUTHORITY, "firm", None),
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
    let lawyer_session = session_for("pages-counsel", lawyer, Role::Lawyer);
    let mut state = portal::test_support::app_state(surreal.clone()).await;
    state.policy = portal::policy::PolicyClient::embedded().expect("embedded policy compiles");
    Fixture {
        app: portal::router(state),
        code: project.code,
        project_id: project.id,
        client_cookie: cookie_for(&session_for("pages-client", client, Role::Client)),
        lawyer_cookie: cookie_for(&lawyer_session),
        lawyer_csrf: lawyer_session.csrf_token.clone(),
        outsider_cookie: cookie_for(&session_for("pages-other", outsider, Role::Client)),
    }
}

fn pages() -> serde_json::Value {
    serde_json::json!({ "pages": [
        {
            "slug": "timeline",
            "kind": "docket_deadline_board",
            "title": "Case timeline",
            "lenses": {
                "lawyer": ["deadline_board", "boundary_note", "provenance_statement"],
                "client": ["key_dates_strip", "boundary_note", "provenance_statement"]
            }
        },
        {
            "slug": "billing",
            "kind": "engagement_billing_records",
            "title": "Billing review",
            "lenses": {
                "lawyer": ["engagement_letter", "invoice", "prebill_review", "boundary_note",
                           "provenance_statement"]
            }
        }
    ]})
}

async fn put_pages(f: &Fixture, body: serde_json::Value) -> (StatusCode, String) {
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/app/api/projects/{}/pages", f.project_id))
                .header("cookie", &f.lawyer_cookie)
                .header("x-csrf-token", &f.lawyer_csrf)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn get(f: &Fixture, path: &str, cookie: &str) -> (StatusCode, String) {
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn synced() -> Fixture {
    let f = fixture().await;
    let (status, body) = put_pages(&f, pages()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    f
}

#[tokio::test]
async fn a_lawyer_sees_the_lawyer_lens_with_every_open_deadline() {
    let f = synced().await;
    let (status, html) = get(
        &f,
        &format!("/app/projects/{}/timeline", f.code),
        &f.lawyer_cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("data-lens=\"lawyer\""), "{html}");
    assert!(html.contains("data-section=\"deadline_board\""), "{html}");
    assert!(html.contains(INTERNAL_AUTHORITY), "{html}");
    assert!(html.contains("data-section=\"boundary_note\""), "{html}");
    assert!(
        html.contains("data-section=\"provenance_statement\""),
        "{html}"
    );
    assert!(
        html.contains("href=\"/app/projects/sample-pages/billing\""),
        "{html}"
    );
}

#[tokio::test]
async fn a_client_sees_only_client_lens_sections_and_client_dates() {
    let f = synced().await;
    let (status, html) = get(
        &f,
        &format!("/app/projects/{}/timeline", f.code),
        &f.client_cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("data-lens=\"client\""), "{html}");
    assert!(html.contains("data-section=\"key_dates_strip\""), "{html}");
    assert!(html.contains(CLIENT_AUTHORITY), "{html}");
    assert!(html.contains("data-section=\"boundary_note\""), "{html}");
    assert!(
        html.contains("data-section=\"provenance_statement\""),
        "{html}"
    );
    assert!(!html.contains("data-section=\"deadline_board\""), "{html}");
    assert!(
        !html.contains(INTERNAL_AUTHORITY),
        "a firm-only date must not reach a client's markup or hydration payload"
    );
    assert!(
        !html.contains("/app/projects/sample-pages/billing"),
        "a page with no client face is not a client tab"
    );
    let (status, _) = get(
        &f,
        &format!("/app/projects/{}/billing", f.code),
        &f.client_cookie,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_non_participant_gets_404() {
    let f = synced().await;
    let (status, html) = get(
        &f,
        &format!("/app/projects/{}/timeline", f.code),
        &f.outsider_cookie,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!html.contains("Case timeline"), "{html}");
    assert!(!html.contains(CLIENT_AUTHORITY), "{html}");
}

#[tokio::test]
async fn an_unknown_slug_gets_404() {
    let f = synced().await;
    let (status, _) = get(
        &f,
        &format!("/app/projects/{}/no-such-page", f.code),
        &f.lawyer_cookie,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_set_the_gate_refuses_is_400_and_stores_nothing() {
    let f = fixture().await;
    let mut body = pages();
    body["pages"][0]["lenses"]["client"] = serde_json::json!(["key_dates_strip"]);
    let (status, message) = put_pages(&f, body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{message}");
    assert!(message.contains("D003"), "{message}");
    let (status, _) = get(
        &f,
        &format!("/app/projects/{}/timeline", f.code),
        &f.lawyer_cookie,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Every literal segment a route mounts under `/app/projects/{code}/` is a
/// reserved slug, so `Y021` refuses a page the route would shadow.
#[test]
fn every_literal_matter_subroute_is_a_reserved_slug() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut missing = std::collections::BTreeSet::new();
    for dir in ["portal/src", "webapp/src", "server/src"] {
        for entry in walkdir::WalkDir::new(root.join(dir))
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "rs"))
        {
            let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
            for prefix in ["/app/projects/{code}/", "/app/projects/{project_code}/"] {
                for (index, _) in text.match_indices(prefix) {
                    let segment: String = text[index + prefix.len()..]
                        .chars()
                        .take_while(|c| c.is_ascii_lowercase() || *c == '-' || *c == '_')
                        .collect();
                    if !segment.is_empty()
                        && !rules::pages::RESERVED_SLUGS.contains(&segment.as_str())
                    {
                        missing.insert(segment);
                    }
                }
            }
        }
    }
    assert!(
        missing.is_empty(),
        "add these to rules::pages::RESERVED_SLUGS: {missing:?}"
    );
}

#[tokio::test]
async fn the_show_page_leads_a_tab_strip_generated_from_pages() {
    let f = synced().await;
    let show = format!("/app/projects/{}", f.code);
    let (status, client) = get(&f, &show, &f.client_cookie).await;
    assert_eq!(status, StatusCode::OK);
    assert!(client.contains("aria-label=\"Matter pages\""), "{client}");
    assert!(
        client.contains("href=\"/app/projects/sample-pages/timeline\""),
        "{client}"
    );
    assert!(
        !client.contains("href=\"/app/projects/sample-pages/billing\""),
        "{client}"
    );
    let (_, lawyer) = get(&f, &show, &f.lawyer_cookie).await;
    assert!(
        lawyer.contains("href=\"/app/projects/sample-pages/billing\""),
        "{lawyer}"
    );
}

#[tokio::test]
async fn a_literal_matter_route_still_wins_over_a_page_slug() {
    let f = synced().await;
    let (status, html) = get(
        &f,
        &format!("/app/projects/{}/conversation", f.code),
        &f.lawyer_cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(!html.contains("id=\"matter-page\""), "{html}");
}
