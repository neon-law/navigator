//! The discovery cockpit on the sample-litigation fixture: a declared
//! `discovery_cockpit` page renders each served item with its objection,
//! its answer, and its status through the assembled router; the filter is a
//! query link; and the firm's commentary never reaches the client lens.
//! Every name and request is synthetic.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use store::persons::Role;
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

const CAPTION: &str = "Cruller v. Prine";
const UNANSWERED: &str = "State every synthetic fact supporting the first claim.";
const OBJECTED: &str = "Identify every synthetic witness to the boundary survey.";
const OBJECTION: &str = "Synthetic objection: overbroad as to time.";
const THEIR_ANSWER: &str = "Synthetic answer given subject to the objection.";
const DRAFTED: &str = "Describe the synthetic fence line.";
const OUR_DRAFT: &str = "Synthetic drafted answer awaiting signature.";

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

async fn served_set(
    surreal: &store::surreal::SurrealDb,
    case_id: Uuid,
    set_number: i32,
    propounding: &str,
    responding: &str,
) -> Uuid {
    store::cases::serve_discovery(
        surreal,
        &store::cases::NewDiscoveryRequest {
            case_id,
            docket_entry_id: None,
            device: store::cases::Device::Interrogatories,
            set_number,
            propounding_party: propounding,
            responding_party: responding,
            served_on: Some("2026-03-01"),
            responses_due_on: Some("2026-03-31"),
        },
    )
    .await
    .unwrap()
    .id
}

/// Our responses to their first set (one unanswered, one drafted), and
/// their answered response to our second set, with an objection.
async fn seed_discovery(surreal: &store::surreal::SurrealDb, project_id: Uuid) {
    let case = store::cases::open_case(
        surreal,
        &store::cases::NewCase {
            project_id,
            caption: CAPTION,
            forum: Some("Synthetic County District Court"),
            jurisdiction: None,
            docket_number: Some("A-26-000001"),
            judge: None,
            posture: "plaintiff",
        },
    )
    .await
    .unwrap();
    let inbound = served_set(
        surreal,
        case.id,
        1,
        "Defendant Prine",
        "Plaintiff Dermot Cruller",
    )
    .await;
    store::cases::add_item(surreal, inbound, 1, UNANSWERED)
        .await
        .unwrap();
    let drafted = store::cases::add_item(surreal, inbound, 2, DRAFTED)
        .await
        .unwrap();
    store::cases::answer_item(surreal, drafted.id, OUR_DRAFT, None)
        .await
        .unwrap();

    let outbound = served_set(
        surreal,
        case.id,
        2,
        "Plaintiff Dermot Cruller",
        "Defendant Prine",
    )
    .await;
    let objected = store::cases::add_item(surreal, outbound, 1, OBJECTED)
        .await
        .unwrap();
    store::cases::answer_item(surreal, objected.id, THEIR_ANSWER, Some(OBJECTION))
        .await
        .unwrap();
    surreal
        .query("UPDATE $id SET status = 'responded'")
        .bind((
            "id",
            store::surreal::record_id("discovery_request", outbound),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let entity = store::entities::create(
        &surreal,
        &store::entities::NewEntity {
            name: "Dermot Cruller".into(),
            entity_type_id: store::test_support::SEED_ENTITY_TYPE_ID,
            jurisdiction_id: store::test_support::SEED_ENTITY_JURISDICTION_ID,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .unwrap();
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: store::seed::SAMPLE_LITIGATION_CODE.into(),
            name: CAPTION.into(),
            status: "open".into(),
            entity_id: entity.id,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let client = person(
        &surreal,
        "Dermot Cruller",
        "discovery-client@example.com",
        Role::Client,
    )
    .await;
    store::projects::add_participation(&surreal, project.id, client, "client")
        .await
        .unwrap();
    let lawyer = person(
        &surreal,
        "Sample Counsel",
        "discovery-counsel@example.com",
        Role::Lawyer,
    )
    .await;
    store::projects::add_participation(&surreal, project.id, lawyer, "lawyer")
        .await
        .unwrap();
    seed_discovery(&surreal, project.id).await;

    let mut state = portal::test_support::app_state(surreal.clone()).await;
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
    let lawyer_session = session_for("discovery-counsel", lawyer, Role::Lawyer);
    state.policy = portal::policy::PolicyClient::embedded().expect("embedded policy compiles");
    Fixture {
        app: portal::router(state),
        code: project.code,
        project_id: project.id,
        client_cookie: cookie_for(&session_for("discovery-client", client, Role::Client)),
        lawyer_cookie: cookie_for(&lawyer_session),
        lawyer_csrf: lawyer_session.csrf_token.clone(),
    }
}

fn pages() -> serde_json::Value {
    serde_json::json!({ "pages": [{
        "slug": "discovery",
        "kind": "discovery_cockpit",
        "title": "Discovery",
        "lenses": {
            "lawyer": ["discovery_pairs", "progress_counter", "item_status_setter",
                       "boundary_note", "provenance_statement"],
            "client": ["discovery_pairs", "boundary_note", "provenance_statement"]
        }
    }]})
}

async fn synced() -> Fixture {
    let f = fixture().await;
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
                .body(Body::from(pages().to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    f
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

#[tokio::test]
async fn a_lawyer_reads_every_state_with_attribution_progress_and_commentary() {
    let f = synced().await;
    let (status, html) = get(
        &f,
        &format!("/app/projects/{}/discovery", f.code),
        &f.lawyer_cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("data-section=\"discovery_pairs\""), "{html}");
    assert!(html.contains("Interrogatories · Set 1 · No. 1"), "{html}");
    assert!(html.contains(UNANSWERED), "{html}");
    assert!(html.contains("data-status=\"needs-response\""), "{html}");
    assert!(html.contains(OBJECTION), "{html}");
    assert!(html.contains("Counsel for Defendant Prine"), "{html}");
    assert!(html.contains(THEIR_ANSWER), "{html}");
    assert!(html.contains("Defendant Prine, under oath"), "{html}");
    assert!(html.contains("Answered subject to objection"), "{html}");
    assert!(html.contains("data-status=\"ready-to-sign\""), "{html}");
    assert!(html.contains(OUR_DRAFT), "{html}");
    assert!(html.contains("Where it leaves us"), "{html}");
    assert!(html.contains("2 of 3 done"), "progress: {html}");
    assert!(
        html.contains("data-section=\"item_status_setter\""),
        "{html}"
    );
    assert!(html.contains("Read-only"), "{html}");
}

#[tokio::test]
async fn the_filter_is_a_query_link_the_server_honours() {
    let f = synced().await;
    let page = format!("/app/projects/{}/discovery", f.code);
    let (status, html) = get(&f, &format!("{page}?discovery=objecting"), &f.lawyer_cookie).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("data-filter=\"objecting\""), "{html}");
    assert!(html.contains(OBJECTED), "{html}");
    assert!(!html.contains(UNANSWERED), "{html}");
    assert!(!html.contains(DRAFTED), "{html}");
    assert!(
        html.contains(&format!("href=\"{page}?discovery=ready-to-sign\"")),
        "{html}"
    );

    let (_, ready) = get(
        &f,
        &format!("{page}?discovery=ready-to-sign"),
        &f.lawyer_cookie,
    )
    .await;
    assert!(ready.contains(OUR_DRAFT), "{ready}");
    assert!(!ready.contains(OBJECTED), "{ready}");

    let (_, needs) = get(&f, &format!("{page}?discovery=needs-you"), &f.lawyer_cookie).await;
    assert!(needs.contains(UNANSWERED), "{needs}");
    assert!(!needs.contains(DRAFTED), "{needs}");
}

#[tokio::test]
async fn a_client_reads_the_pairs_without_the_firms_commentary() {
    let f = synced().await;
    let (status, html) = get(
        &f,
        &format!("/app/projects/{}/discovery", f.code),
        &f.client_cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("data-lens=\"client\""), "{html}");
    assert!(html.contains(UNANSWERED), "{html}");
    assert!(html.contains(OUR_DRAFT), "{html}");
    assert!(!html.contains("Where it leaves us"), "{html}");
    assert!(!html.contains("data-firm-only"), "{html}");
    assert!(
        !html.contains("Nothing is served until a lawyer signs"),
        "{html}"
    );
    assert!(
        !html.contains("data-section=\"item_status_setter\""),
        "{html}"
    );
}
