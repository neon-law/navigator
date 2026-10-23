//! The retired Portal mount, `/app/projects/{code}/portal`, on the assembled
//! router.
//!
//! Project repositories no longer carry a `portal/` bundle, but a bundle a
//! Project published before that can still sit in object storage. These tests
//! seed exactly that — a stale `index.html` and hashed asset under the
//! Project's `<code>/portal/` prefix — and prove none of it is ever served:
//! every path under the old mount redirects to the matter show page, for a
//! participant and a nonparticipant alike, and the show page itself still
//! renders.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use store::persons::Role;
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

/// Bytes only the stale bundle carries, so a response that contains them was
/// served from the bucket.
const STALE_MARKER: &str = "Libra stale portal bundle";

struct Fixture {
    app: axum::Router,
    project_code: String,
    participant_cookie: String,
    outsider_cookie: String,
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: "libra-formation".into(),
            name: "Libra formation".into(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&surreal).await,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let participant = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role("Libra", "libra@example.com", Role::Client),
    )
    .await
    .unwrap();
    store::projects::add_participation(&surreal, project.id, participant.id, "client")
        .await
        .unwrap();
    let outsider = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role("Aries", "aries@example.com", Role::Client),
    )
    .await
    .unwrap();

    let state = portal::test_support::app_state(surreal).await;
    // The bundle a Project published before it deleted `portal/`. Dev and KIND
    // keep every storage lane on this one root, so this is where it would sit.
    for (path, content_type, bytes) in [
        (
            "index.html",
            "text/html; charset=utf-8",
            format!("<!doctype html><title>{STALE_MARKER}</title>"),
        ),
        (
            "assets/app-4f9c2e1b.js",
            "text/javascript; charset=utf-8",
            format!("console.log('{STALE_MARKER}');"),
        ),
    ] {
        state
            .storage
            .put(
                &format!("{}/portal/{path}", project.code),
                bytes.as_bytes(),
                content_type,
            )
            .await
            .expect("seed a stale bundle object");
    }

    let sessions = portal::SessionStore::new(portal::test_support::TEST_SESSION_KEY);
    let cookie_for = |sub: &str, person_id: Uuid| {
        let mut session = portal::SessionData::fresh(sub, Role::Client);
        session.person_id = Some(person_id);
        format!(
            "{}={}",
            portal::session::SESSION_COOKIE_NAME,
            sessions.encode(&session)
        )
    };

    Fixture {
        app: portal::router(state),
        project_code: project.code,
        participant_cookie: cookie_for("libra-sub", participant.id),
        outsider_cookie: cookie_for("aries-sub", outsider.id),
    }
}

async fn send(app: &axum::Router, uri: &str, cookie: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

fn location(response: &axum::response::Response) -> String {
    response
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Every path a stale bundle was once served at redirects, in one hop, to the
/// matter show page — never to the bundle, and never through a second slash
/// redirect.
#[tokio::test]
async fn every_retired_portal_path_redirects_to_the_matter_show_page() {
    let f = fixture().await;
    let show_page = format!("/app/projects/{}", f.project_code);

    for suffix in [
        "portal",
        "portal/",
        "portal/index.html",
        "portal/assets/app-4f9c2e1b.js",
        "portal/engagement",
        "portal/engagement/",
    ] {
        let uri = format!("/app/projects/{}/{suffix}", f.project_code);
        for cookie in [&f.participant_cookie, &f.outsider_cookie] {
            let response = send(&f.app, &uri, cookie).await;
            assert_eq!(
                response.status(),
                StatusCode::MOVED_PERMANENTLY,
                "{uri} must redirect"
            );
            assert_eq!(location(&response), show_page, "{uri}");
            let body = body_string(response).await;
            assert!(
                !body.contains(STALE_MARKER),
                "{uri} served the stale bundle: {body}"
            );
        }
    }
}

/// The page the redirect lands on still renders for a participant, so a link a
/// client was sent to the old Portal ends on their matter.
#[tokio::test]
async fn the_redirect_lands_on_a_rendering_matter_show_page() {
    let f = fixture().await;
    let response = send(
        &f.app,
        &format!("/app/projects/{}/portal/", f.project_code),
        &f.participant_cookie,
    )
    .await;
    let target = location(&response);

    let matter = send(&f.app, &target, &f.participant_cookie).await;
    assert_eq!(matter.status(), StatusCode::OK);
    let body = body_string(matter).await;
    assert!(body.contains("class=\"app-footer"), "{body}");
    assert!(!body.contains(STALE_MARKER));
    assert!(
        !body.contains(&format!("/app/projects/{}/portal", f.project_code)),
        "the matter show page must not link the retired Portal"
    );
}

/// A code that cannot name a Project is not echoed into a `Location`.
#[tokio::test]
async fn a_malformed_code_is_not_found_rather_than_redirected() {
    let f = fixture().await;
    for uri in [
        "/app/projects/Not_A_Code/portal/",
        "/app/projects/new/portal",
    ] {
        let response = send(&f.app, uri, &f.participant_cookie).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
        assert!(location(&response).is_empty(), "{uri}");
    }
}
