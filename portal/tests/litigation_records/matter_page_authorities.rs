//! The hearing console on the sample-litigation fixture: a declared
//! `hearing_console` page renders the authority table with linked
//! citations, the relationship graph as server-rendered SVG with its table,
//! and the stage list's empty state; a `verifier_split_view` page shows a
//! client the authorities as text, without firm reasoning. Every name and
//! citation is synthetic.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rules::citation::Disposition;
use store::persons::Role;
use store::relationships::Endpoint;
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

const CAPTION: &str = "Cruller v. Prine";
const CLIENT_ENTITY: &str = "Cruller Synthetic Holdings";
const RELIED: &str = "Alpha v. Synthetic, 1 Syn. 2d 1 (2020)";
const RELIED_URL: &str = "https://example.com/authorities/alpha";
const REVIEWED: &str = "Hidden v. Synthetic, 2 Syn. 2d 2 (2021)";
const QUOTE: &str = "Synthetic verbatim passage the firm relies on.";
const WHY: &str = "Synthetic reason the passage supports the motion.";
const OPPONENT: &str = "Synthetic Adverse Party";

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

async fn authority(
    surreal: &store::surreal::SurrealDb,
    project_id: Uuid,
    citation: &str,
    url: Option<&str>,
    disposition: Disposition,
) -> Uuid {
    let row = store::authorities::record(
        surreal,
        &store::authorities::NewAuthority {
            class: rules::citation::AuthorityClass::CaseLaw,
            citation,
            short_cite: None,
            title: "Synthetic opinion",
            publisher: Some("Synthetic Supreme Court"),
            issued_on: Some("2020-01-02"),
            canonical_url: url,
            checked_on: None,
            archived_asset_id: None,
        },
    )
    .await
    .unwrap();
    store::authorities::cite_in_matter(surreal, project_id, row.id, "ours", disposition, None)
        .await
        .unwrap()
        .id
}

/// One relied-on Authority with a verified-pending Citation, one the firm
/// reviewed and did not use, and the matter's parties tied by the graph.
async fn seed(
    surreal: &store::surreal::SurrealDb,
    project_id: Uuid,
    entity_id: Uuid,
    client: Uuid,
    lawyer: Uuid,
) {
    let relied = authority(
        surreal,
        project_id,
        RELIED,
        Some(RELIED_URL),
        Disposition::ReliedOn,
    )
    .await;
    let citation = store::authorities::cite(
        surreal,
        &store::authorities::NewCitation {
            authority_use_id: relied,
            quote: QUOTE,
            why: WHY,
            source_pin: None,
            draft_pin: None,
        },
    )
    .await
    .unwrap();
    store::verifications::record(surreal, citation.id, "synthetic-sha", lawyer)
        .await
        .unwrap();
    authority(
        surreal,
        project_id,
        REVIEWED,
        None,
        Disposition::ReviewedNotUsed,
    )
    .await;

    store::entity_roles::grant(surreal, client, entity_id, "manages")
        .await
        .unwrap();
    let opponent = person(
        surreal,
        OPPONENT,
        "authorities-opponent@example.com",
        Role::Client,
    )
    .await;
    store::project_references::reference(
        surreal,
        project_id,
        store::project_references::Party::person(opponent),
        store::project_references::REASON_ADVERSE_PARTY,
    )
    .await
    .unwrap();
    store::relationships::record(
        surreal,
        &store::relationships::NewRelationship {
            from: Endpoint::Entity,
            from_id: entity_id,
            to: Endpoint::Person,
            to_id: opponent,
            kind: store::relationships::KIND_ADVERSE_TO.into(),
            confidence_pct: 100,
            source_kind: store::relationships::SOURCE_MANUAL.into(),
            source_id: None,
            detail: None,
        },
    )
    .await
    .unwrap();
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let entity = store::entities::create(
        &surreal,
        &store::entities::NewEntity {
            name: CLIENT_ENTITY.into(),
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
        "authorities-client@example.com",
        Role::Client,
    )
    .await;
    store::projects::add_participation(&surreal, project.id, client, "client")
        .await
        .unwrap();
    let lawyer = person(
        &surreal,
        "Sample Counsel",
        "authorities-counsel@example.com",
        Role::Lawyer,
    )
    .await;
    store::projects::add_participation(&surreal, project.id, lawyer, "lawyer")
        .await
        .unwrap();
    seed(&surreal, project.id, entity.id, client, lawyer).await;

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
    let lawyer_session = session_for("authorities-counsel", lawyer, Role::Lawyer);
    state.policy = portal::policy::PolicyClient::embedded().expect("embedded policy compiles");
    Fixture {
        app: portal::router(state),
        code: project.code,
        project_id: project.id,
        client_cookie: cookie_for(&session_for("authorities-client", client, Role::Client)),
        lawyer_cookie: cookie_for(&lawyer_session),
        lawyer_csrf: lawyer_session.csrf_token.clone(),
    }
}

fn pages() -> serde_json::Value {
    serde_json::json!({ "pages": [
        {
            "slug": "hearing",
            "kind": "hearing_console",
            "title": "Hearing",
            "lenses": {
                "lawyer": ["stage_list", "authority_table", "relationship_graph",
                           "boundary_note", "provenance_statement"],
                "client": ["boundary_note", "provenance_statement"]
            }
        },
        {
            "slug": "verify",
            "kind": "verifier_split_view",
            "title": "Citations",
            "lenses": {
                "lawyer": ["assertion_panel", "source_page_panel", "item_status_setter",
                           "authority_table", "boundary_note", "provenance_statement"],
                "client": ["authority_table", "boundary_note", "provenance_statement"]
            }
        }
    ]})
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
async fn a_lawyer_reads_linked_authorities_with_their_quotes_and_verification() {
    let f = synced().await;
    let (status, html) = get(
        &f,
        &format!("/app/projects/{}/hearing", f.code),
        &f.lawyer_cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("data-section=\"authority_table\""), "{html}");
    assert!(html.contains(RELIED), "{html}");
    assert!(html.contains(&format!("href=\"{RELIED_URL}\"")), "{html}");
    assert!(html.contains("noopener noreferrer"), "{html}");
    assert!(html.contains(QUOTE) && html.contains(WHY), "{html}");
    assert!(html.contains("Unverified"), "{html}");
    assert!(html.contains(REVIEWED), "the firm sees its own record");
    assert!(html.contains("No link recorded"), "{html}");
}

#[tokio::test]
async fn a_lawyer_reads_the_relationship_graph_as_svg_with_its_table() {
    let f = synced().await;
    let (status, html) = get(
        &f,
        &format!("/app/projects/{}/hearing", f.code),
        &f.lawyer_cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(
        html.contains("data-section=\"relationship_graph\""),
        "{html}"
    );
    assert!(html.contains("matter-graph__svg"), "{html}");
    assert!(html.contains("matter-graph__edge--adverse"), "{html}");
    assert!(html.contains("<table"), "{html}");
    assert!(
        html.contains(OPPONENT) && html.contains(CLIENT_ENTITY),
        "{html}"
    );
    assert!(
        html.contains("Adverse to") && html.contains("Manages"),
        "{html}"
    );
    assert!(html.contains("data-section=\"stage_list\""), "{html}");
    assert!(html.contains("No stages are planned yet."), "{html}");
}

#[tokio::test]
async fn a_client_reads_citations_as_text_without_firm_reasoning() {
    let f = synced().await;
    let (status, html) = get(
        &f,
        &format!("/app/projects/{}/verify", f.code),
        &f.client_cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("data-lens=\"client\""), "{html}");
    assert!(html.contains(RELIED), "{html}");
    assert!(html.contains(QUOTE), "{html}");
    assert!(!html.contains(RELIED_URL), "no off-origin link: {html}");
    assert!(
        !html.contains(REVIEWED),
        "reviewed-not-used stays firm-side"
    );
    assert!(!html.contains(WHY), "{html}");
    assert!(!html.contains("Unverified"), "{html}");
    assert!(!html.contains(OPPONENT), "{html}");

    let (hearing, _) = get(
        &f,
        &format!("/app/projects/{}/hearing", f.code),
        &f.client_cookie,
    )
    .await;
    assert_eq!(hearing, StatusCode::OK);
}
