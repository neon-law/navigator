//! LAW-124: a Lawyer's service write clears the assembled router's Rego
//! gate, and the stored Service shows in the matter page's document history.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use store::persons::Role;
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

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
            code: "libra-service".into(),
            name: "libra-service".into(),
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

    let mut state = portal::test_support::app_state(surreal.clone()).await;
    store::documents::ingest_bytes_as(
        &state.surreal,
        &state.assets_storage,
        &store::documents::IngestArgs {
            project_id: project.id,
            source: store::documents::source::UPLOAD,
            filename: "motion.pdf",
            kind: "pleading",
            content_type: "application/pdf",
            description: None,
            visibility: store::documents::visibility::INTERNAL,
            secondary_storage_key: None,
        },
        &store::documents::DocumentIdentity {
            slug: Some("pleadings/motion.pdf"),
            ..Default::default()
        },
        b"%PDF-1.7 synthetic",
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
    state.policy = portal::policy::PolicyClient::embedded().expect("embedded policy compiles");
    Fixture {
        app: portal::router(state),
        code: project.code,
        project_id: project.id,
        lawyer_cookie,
        lawyer_csrf: lawyer_session.csrf_token.clone(),
    }
}

#[tokio::test]
async fn a_lawyer_records_service_and_the_document_history_lists_it() {
    let f = fixture().await;
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/app/api/projects/{}/service", f.project_id))
                .header("cookie", &f.lawyer_cookie)
                .header("x-csrf-token", &f.lawyer_csrf)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "slug": "pleadings/motion.pdf",
                        "recipient": "Example Counsel",
                        "counsel_for": "Defendant Example Co.",
                        "method": "certified_mail",
                        "served_on": "2026-03-05",
                        "address": "200 Sample Street, Sampleton, CA 90000",
                        "declarant": "Pat Sample"
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
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );

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
    let html = String::from_utf8_lossy(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .into_owned();
    assert!(
        html.contains("Served by certified mail on 2026-03-05 on Example Counsel, counsel for Defendant Example Co."),
        "{html}"
    );
}
