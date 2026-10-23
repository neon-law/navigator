//! The Notation sections on the sample-transactional fixture: a declared
//! `document_workbench` page renders the page Draft's Notation through the
//! assembled router, verified against the digest it pinned. A tampered body
//! fails closed on every lens, and a lawyer previews a Draft no Notation has
//! been opened from yet. A `matter_status_console` page's client lens leads
//! with the one step waiting on the client, then the Notation list and the
//! hold-derived checklist. Every name and word is synthetic.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use store::persons::Role;
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

const MATTER: &str = "Widget Works — Outside Counsel";
const DRAFT_CODE: &str = "engagement__letter";

struct Fixture {
    app: axum::Router,
    surreal: store::surreal::SurrealDb,
    storage: std::sync::Arc<dyn cloud::StorageService>,
    code: String,
    project_id: Uuid,
    client: Uuid,
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

/// A body unique to this test run, so tampering with its content-addressed
/// blob in the shared test storage root touches no other test.
fn body(marker: &str) -> String {
    format!(
        "---\ntitle: Sample Engagement Letter\n---\n\nDear {{{{person__client.name}}}},\n\n\
         ## I. Scope\n\nThe firm will review one synthetic supply agreement ({marker}).\n\n\
         [Confirm the flat fee before sending.]\n\n## II. Fees\n\nThe fee is \
         `{{{{custom_money__fee}}}}`.\n"
    )
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let entity = store::entities::create(
        &surreal,
        &store::entities::NewEntity {
            name: "Widget Works".into(),
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
            code: store::seed::SAMPLE_TRANSACTIONAL_CODE.into(),
            name: MATTER.into(),
            status: "open".into(),
            entity_id: entity.id,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let client = person(
        &surreal,
        "Sample Widget Client",
        "notation-client@example.com",
        Role::Client,
    )
    .await;
    store::projects::add_participation(&surreal, project.id, client, "client")
        .await
        .unwrap();
    let lawyer = person(
        &surreal,
        "Sample Counsel",
        "notation-counsel@example.com",
        Role::Lawyer,
    )
    .await;
    store::projects::add_participation(&surreal, project.id, lawyer, "lawyer")
        .await
        .unwrap();

    let mut state = portal::test_support::app_state(surreal.clone()).await;
    let storage = state.storage.clone();
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
    let lawyer_session = session_for("notation-counsel", lawyer, Role::Lawyer);
    state.policy = portal::policy::PolicyClient::embedded().expect("embedded policy compiles");
    let f = Fixture {
        app: portal::router(state),
        surreal,
        storage,
        code: project.code,
        project_id: project.id,
        client,
        client_cookie: cookie_for(&session_for("notation-client", client, Role::Client)),
        lawyer_cookie: cookie_for(&lawyer_session),
        lawyer_csrf: lawyer_session.csrf_token.clone(),
    };
    sync_pages(&f).await;
    f
}

/// The page's Draft as a project-scoped template version, its body in the
/// router's storage.
async fn draft(f: &Fixture, body: &str) -> store::templates::Template {
    let asset_id =
        store::assets::ingest_content(&f.surreal, &f.storage, body.as_bytes(), "text/markdown")
            .await
            .unwrap();
    store::templates::save_version(
        &f.surreal,
        Some(f.project_id),
        DRAFT_CODE,
        store::templates::Version {
            title: "Sample Engagement Letter".into(),
            respondent_type: "person".into(),
            asset_id: Some(asset_id),
            form_code: None,
            kind: Some("letter".into()),
            source_commit_sha: Some(store::test_support::SYNTHETIC_COMMIT_SHA.into()),
        },
    )
    .await
    .unwrap()
    .into_model()
}

async fn notation(f: &Fixture, template: &store::templates::Template, state: &str) {
    store::notations::create(
        &f.surreal,
        &store::notations::NewNotation::new(template.id, f.client, f.project_id, state),
    )
    .await
    .unwrap();
}

/// Overwrite the stored bytes behind `template`'s body asset.
async fn tamper(f: &Fixture, template: &store::templates::Template) {
    let asset = store::assets::find_by_id(&f.surreal, template.asset_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    f.storage
        .put(
            &asset.storage_key,
            b"---\ntitle: Forged\n---\n\n## I. Scope\n\nForged synthetic words.\n",
            "text/markdown",
        )
        .await
        .unwrap();
}

fn pages() -> serde_json::Value {
    serde_json::json!({ "pages": [{
        "slug": "letter",
        "kind": "document_workbench",
        "title": "Engagement letter",
        "draft": format!("drafts/{DRAFT_CODE}.md"),
        "lenses": {
            "lawyer": ["notation_document", "document_body", "boundary_note",
                       "provenance_statement"],
            "client": ["notation_document", "notation_checklist", "boundary_note",
                       "provenance_statement"]
        }
    }, {
        "slug": "status",
        "kind": "matter_status_console",
        "title": "Where things stand",
        "lenses": {
            "lawyer": ["metric_strip", "workstream_cards", "decision_queue", "notation_list",
                       "boundary_note", "provenance_statement"],
            "client": ["decision_queue", "notation_list", "notation_checklist",
                       "boundary_note", "provenance_statement"]
        }
    }]})
}

async fn sync_pages(f: &Fixture) {
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
}

async fn get(f: &Fixture, cookie: &str) -> (StatusCode, String) {
    get_page(f, "letter", cookie).await
}

async fn get_page(f: &Fixture, slug: &str, cookie: &str) -> (StatusCode, String) {
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/app/projects/{}/{slug}", f.code))
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
async fn a_client_reads_the_page_drafts_notation_as_a_verified_outline() {
    let f = fixture().await;
    let marker = Uuid::now_v7().to_string();
    let template = draft(&f, &body(&marker)).await;
    notation(&f, &template, "BEGIN").await;

    let (status, html) = get(&f, &f.client_cookie).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(
        html.contains("data-section=\"notation_document\""),
        "{html}"
    );
    assert!(html.contains("data-state=\"notation\""), "{html}");
    assert!(html.contains("Sample Engagement Letter"), "{html}");
    assert!(html.contains(&marker), "{html}");
    assert!(html.contains("id=\"section-ii\""), "{html}");
    assert!(html.contains("href=\"#section-i-p2\""), "{html}");
    assert!(html.contains("notation-block--hold"), "{html}");
    assert!(
        html.contains("/public/js/notation-anchor.js"),
        "the deep-link script is hoisted into the page: {html}"
    );
    assert!(
        html.contains("data-section=\"notation_checklist\""),
        "{html}"
    );
    assert!(
        html.contains("Confirm the flat fee before sending."),
        "{html}"
    );
    assert!(
        html.contains("href=\"#section-i-p2\""),
        "a checklist step links to its paragraph on the same page: {html}"
    );
}

#[tokio::test]
async fn a_tampered_body_fails_closed_on_every_lens() {
    let f = fixture().await;
    let template = draft(&f, &body(&Uuid::now_v7().to_string())).await;
    notation(&f, &template, "BEGIN").await;
    tamper(&f, &template).await;

    for cookie in [&f.client_cookie, &f.lawyer_cookie] {
        let (status, html) = get(&f, cookie).await;
        assert_eq!(status, StatusCode::OK, "{html}");
        assert!(html.contains("data-state=\"unverified\""), "{html}");
        assert!(html.contains("could not be verified"), "{html}");
        assert!(!html.contains("Forged synthetic words"), "{html}");
        assert!(!html.contains("notation-document__body"), "{html}");
    }
}

#[tokio::test]
async fn a_lawyer_previews_a_draft_with_no_notation_and_a_client_sees_nothing() {
    let f = fixture().await;
    let marker = Uuid::now_v7().to_string();
    draft(&f, &body(&marker)).await;

    let (status, html) = get(&f, &f.lawyer_cookie).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("data-state=\"draft-preview\""), "{html}");
    assert!(html.contains(&marker), "{html}");

    let (status, html) = get(&f, &f.client_cookie).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("data-state=\"empty\""), "{html}");
    assert!(!html.contains(&marker), "{html}");
}

#[tokio::test]
async fn the_client_status_page_is_notations_first_with_the_one_next_step() {
    let f = fixture().await;
    let template = draft(&f, &body(&Uuid::now_v7().to_string())).await;
    notation(&f, &template, "client_review").await;

    let (status, html) = get_page(&f, "status", &f.client_cookie).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    let decision = html.find("data-section=\"decision_queue\"").unwrap();
    let list = html.find("data-section=\"notation_list\"").unwrap();
    assert!(decision < list, "declared order: {html}");
    assert!(
        html.contains("Read them, then approve or tell the firm what to change."),
        "the client's next step: {html}"
    );
    assert!(html.contains("Your turn to review the drafts"), "{html}");
    assert!(html.contains("Sample Engagement Letter"), "{html}");
    assert!(
        !html.contains("Workflow state"),
        "the raw state stays firm-side"
    );
    assert!(
        html.contains("Confirm the flat fee before sending."),
        "{html}"
    );
    assert!(
        !html.contains("href=\"#section-i-p2\""),
        "no document on this page, so a step does not link: {html}"
    );

    let (status, html) = get_page(&f, "status", &f.lawyer_cookie).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(
        html.contains("Workflow state") && html.contains("client_review"),
        "{html}"
    );
    assert!(html.contains("No decision is waiting on you."), "{html}");
}

#[tokio::test]
async fn a_tampered_body_builds_no_checklist() {
    let f = fixture().await;
    let template = draft(&f, &body(&Uuid::now_v7().to_string())).await;
    notation(&f, &template, "client_review").await;
    tamper(&f, &template).await;

    let (status, html) = get_page(&f, "status", &f.client_cookie).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("This checklist could not be built"), "{html}");
    assert!(!html.contains("Confirm the flat fee"), "{html}");
}
