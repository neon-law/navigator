//! Router-level contract for the matter card's Publish / Unpublish forms:
//! the real CSRF, session and routing layers in front of the handlers.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use store::persons::{NewPerson, Role};
use store::test_support::mem_surreal;
use tower::ServiceExt;

struct Fixture {
    app: Router,
    surreal: store::surreal::SurrealDb,
    code: String,
    client_id: uuid::Uuid,
    project_id: uuid::Uuid,
    cookie: String,
    csrf: String,
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let client = store::persons::create(&surreal, &NewPerson::new("Client", "c@example.com"))
        .await
        .unwrap();
    let admin = store::persons::create(
        &surreal,
        &NewPerson {
            role: Role::Admin,
            ..NewPerson::new("Admin", "a@example.com")
        },
    )
    .await
    .unwrap();
    let project_id = store::test_support::seed_project_surreal(&surreal, "Routes matter").await;
    let project = store::projects::find_by_id(&surreal, project_id)
        .await
        .unwrap()
        .unwrap();
    store::projects::designate_dri_in_surreal(
        &surreal,
        project.id,
        client.id,
        store::projects::DriSide::Client,
    )
    .await
    .unwrap();
    store::projects::add_participation(&surreal, project.id, admin.id, "admin")
        .await
        .unwrap();
    store::testimonials::save_for_client_dri(
        &surreal,
        client.id,
        project.id,
        &store::testimonials::TestimonialSubmission {
            quote: "Client words.",
            attribution_label: None,
            request_public: true,
        },
    )
    .await
    .unwrap();

    let mut session = portal::SessionData::fresh("admin-sub", Role::Admin);
    session.person_id = Some(admin.id);
    let csrf = session.csrf_token.clone();
    let sessions = portal::SessionStore::new(portal::test_support::TEST_SESSION_KEY);
    let cookie = format!(
        "{}={}",
        portal::session::SESSION_COOKIE_NAME,
        sessions.encode(&session)
    );
    let state = portal::test_support::app_state(surreal.clone()).await;
    Fixture {
        app: portal::router(state),
        surreal,
        code: project.code,
        client_id: client.id,
        project_id: project.id,
        cookie,
        csrf,
    }
}

async fn send(
    f: &Fixture,
    method: Method,
    verb: &str,
    cookie: Option<&str>,
    body: String,
) -> StatusCode {
    let mut request = Request::builder()
        .method(method)
        .uri(format!("/app/admin/projects/{}/testimonial/{verb}", f.code))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    f.app
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap()
        .status()
}

async fn is_published(f: &Fixture) -> bool {
    store::testimonials::for_person_project(&f.surreal, f.client_id, f.project_id)
        .await
        .unwrap()
        .unwrap()
        .published_at
        .is_some()
}

#[tokio::test]
async fn a_form_post_with_the_session_token_publishes_then_unpublishes() {
    let f = fixture().await;
    let body = format!("_csrf={}", f.csrf);

    let status = send(&f, Method::POST, "publish", Some(&f.cookie), body.clone()).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(is_published(&f).await);

    let status = send(&f, Method::POST, "unpublish", Some(&f.cookie), body).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(!is_published(&f).await);
}

#[tokio::test]
async fn a_cookie_post_without_the_csrf_token_is_rejected_and_changes_nothing() {
    let f = fixture().await;

    for body in [String::new(), "_csrf=wrong".to_string()] {
        let status = send(&f, Method::POST, "publish", Some(&f.cookie), body).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(!is_published(&f).await);
    }
}

#[tokio::test]
async fn the_publication_routes_accept_post_only() {
    let f = fixture().await;

    for verb in ["publish", "unpublish"] {
        let status = send(&f, Method::GET, verb, Some(&f.cookie), String::new()).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{verb}");
    }
    assert!(!is_published(&f).await);
}

#[tokio::test]
async fn an_anonymous_post_is_refused_and_changes_nothing() {
    let f = fixture().await;

    for verb in ["publish", "unpublish"] {
        let status = send(&f, Method::POST, verb, None, String::new()).await;
        // The session boundary sends a logged-out caller to sign in.
        assert_eq!(status, StatusCode::SEE_OTHER, "{verb}");
    }
    assert!(!is_published(&f).await);
}
