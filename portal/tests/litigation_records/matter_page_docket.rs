//! LAW-133: the matter page at `/app/projects/{code}` renders the matter
//! timeline — docket entries joined with open deadlines in date order — and a
//! Lawyer's docket write clears the assembled router's Rego gate.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use store::persons::Role;
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

const ENTRY_TITLE: &str = "MOTION to Dismiss the synthetic complaint";
const DEADLINE_AUTHORITY: &str = "Order setting the synthetic briefing schedule";

struct Fixture {
    app: axum::Router,
    code: String,
    project_id: Uuid,
    lawyer_cookie: String,
    lawyer_csrf: String,
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: "libra-litigation".into(),
            name: "libra-litigation".into(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&surreal).await,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let lawyer = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role("Counsel", "counsel@example.com", Role::Lawyer),
    )
    .await
    .unwrap()
    .id;
    store::projects::add_participation(&surreal, project.id, lawyer, "lawyer")
        .await
        .unwrap();

    let case = store::cases::open_case(
        &surreal,
        &store::cases::NewCase {
            project_id: project.id,
            caption: "Alpha v. Beta",
            forum: None,
            jurisdiction: None,
            docket_number: Some("9:26-cv-00042"),
            judge: None,
            posture: "plaintiff",
        },
    )
    .await
    .unwrap();
    store::cases::record_entry(
        &surreal,
        &store::cases::NewDocketEntry {
            case_id: case.id,
            entry_number: "12",
            kind: store::cases::EntryKind::Motion,
            title: ENTRY_TITLE,
            party: None,
            filed_or_served_on: Some("2026-03-04"),
            scheduled_on: None,
            supersedes: None,
            document_asset_id: None,
            notation_id: None,
        },
    )
    .await
    .unwrap();
    store::project_deadlines::create(
        &surreal,
        &store::project_deadlines::NewDeadline {
            title: None,
            project_id: project.id,
            due_at: "2099-11-02T17:00:00-08:00".into(),
            authority_kind: "court_order".into(),
            authority: DEADLINE_AUTHORITY.into(),
            source: "lawyer".into(),
            source_document_id: None,
            owner_person_id: None,
            calendar_role: "firm".into(),
            internal_lead_days: Some(7),
            client_lead_days: None,
            replay_key: None,
        },
    )
    .await
    .unwrap();

    let sessions = portal::SessionStore::new(portal::test_support::TEST_SESSION_KEY);
    let mut lawyer_session = portal::SessionData::fresh("counsel-sub", Role::Lawyer);
    lawyer_session.person_id = Some(lawyer);
    let lawyer_cookie = format!(
        "{}={}",
        portal::session::SESSION_COOKIE_NAME,
        sessions.encode(&lawyer_session)
    );

    let mut state = portal::test_support::app_state(surreal.clone()).await;
    state.policy = portal::policy::PolicyClient::embedded().expect("embedded policy compiles");
    Fixture {
        app: portal::router(state),
        code: project.code,
        project_id: project.id,
        lawyer_cookie,
        lawyer_csrf: lawyer_session.csrf_token.clone(),
    }
}

async fn matter_page(f: &Fixture) -> String {
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/app/projects/{}", f.code))
                .header("cookie", &f.lawyer_cookie)
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
async fn the_timeline_lists_a_docket_row_and_a_deadline_row_in_date_order() {
    let f = fixture().await;
    let page = matter_page(&f).await;
    // The Deadlines panel above the calendar names the deadline too; the
    // order under test is the calendar's.
    let start = page
        .find("project-calendar")
        .unwrap_or_else(|| panic!("the calendar is on the page: {page}"));
    let html = &page[start..];
    let entry = html
        .find("D.I. 12 · MOTION to Dismiss the synthetic complaint")
        .unwrap_or_else(|| panic!("the docket entry is in the calendar: {html}"));
    let deadline = html
        .find("3 Nov 99 01:00 UTC")
        .unwrap_or_else(|| panic!("the deadline is in the calendar: {html}"));
    assert!(
        entry < deadline,
        "the 2026 filing precedes the 2099 deadline in the timeline"
    );
    assert!(html.contains("D.I. 12"), "{html}");
    assert!(html.contains("Source pending"), "{html}");
    assert!(html.contains("Deadline · Our deadline"), "{html}");
    assert!(!html.contains("No calendar events scheduled"), "{html}");
}

/// The write behind `navigator project docket add` must reach a Lawyer
/// through the policy layer, not only through the Owner/Admin bypass.
#[tokio::test]
async fn a_lawyer_records_an_entry_through_the_assembled_router() {
    let f = fixture().await;
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/app/api/projects/{}/docket", f.project_id))
                .header("cookie", &f.lawyer_cookie)
                .header("x-csrf-token", &f.lawyer_csrf)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "docket_number": "9:26-cv-00042",
                        "entry_number": "13",
                        "kind": "order",
                        "title": "ORDER granting the synthetic motion",
                        "filed_or_served_on": "2026-04-01"
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
    let html = matter_page(&f).await;
    assert!(
        html.contains("ORDER granting the synthetic motion"),
        "{html}"
    );
}
