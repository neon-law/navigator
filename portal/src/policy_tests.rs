//! Route admission through the mounted middleware: who reaches a handler,
//! who is turned away, and how each refusal is answered.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::{Extension, Router};
use store::persons::Role;
use tower::ServiceExt;

use super::{require_policy, PolicyClient};
use crate::session::{DriView, SessionData, SessionStore, SESSION_COOKIE_NAME};
use crate::test_support::TEST_SESSION_KEY;

const COSTS: &str = "/app/api/projects/00000000-0000-0000-0000-000000000001/costs";

fn sessions() -> SessionStore {
    SessionStore::new(TEST_SESSION_KEY)
}

/// A handler that answers with the subject the middleware admitted.
fn app(client: PolicyClient) -> Router {
    Router::new()
        .fallback(|session: Option<Extension<SessionData>>| async move {
            session.map_or_else(|| "nobody".to_owned(), |session| session.0.sub.clone())
        })
        .layer(axum::middleware::from_fn_with_state(
            (sessions(), client),
            require_policy,
        ))
        .layer(tower_cookies::CookieManagerLayer::new())
}

fn embedded() -> Router {
    app(PolicyClient::embedded().expect("production policy compiles"))
}

fn cookie(session: &SessionData) -> String {
    format!("{SESSION_COOKIE_NAME}={}", sessions().encode(session))
}

fn viewing(role: Role) -> SessionData {
    SessionData {
        viewing_as_dri: Some(DriView {
            actor_sub: "lawyer-sub".into(),
            actor_email: None,
            actor_person_id: None,
            target_name: "Synthetic Client".into(),
            target_email: "client@example.com".into(),
        }),
        ..SessionData::fresh("client-sub", role)
    }
}

async fn send(app: Router, request: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let response = app.oneshot(request).await.unwrap();
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    (
        parts.status,
        parts.headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn post(path: &str) -> axum::http::request::Builder {
    Request::post(path)
}

#[tokio::test]
async fn a_lawyer_adds_a_cost_and_a_client_or_clerk_is_forbidden() {
    let lawyer = SessionData::fresh("lawyer-sub", Role::Lawyer);
    let (status, _, body) = send(
        embedded(),
        post(COSTS)
            .header(header::COOKIE, cookie(&lawyer))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "lawyer-sub"));

    for role in [Role::Client, Role::Clerk] {
        let (status, _, body) = send(
            embedded(),
            post(COSTS)
                .header(header::COOKIE, cookie(&SessionData::fresh("sub", role)))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{role:?}");
        assert_eq!(body, r#"{"error":"forbidden"}"#, "{role:?}");
    }
}

#[tokio::test]
async fn an_anonymous_or_forged_caller_is_sent_to_sign_in() {
    let expired = SessionData {
        exp: 1,
        ..SessionData::fresh("lawyer-sub", Role::Lawyer)
    };
    for cookies in [
        None,
        Some(format!("{SESSION_COOKIE_NAME}=forged")),
        Some(cookie(&expired)),
    ] {
        let mut request = post(&format!("{COSTS}?page=2"));
        if let Some(cookies) = &cookies {
            request = request.header(header::COOKIE, cookies);
        }
        let (status, headers, _) = send(embedded(), request.body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{cookies:?}");
        assert_eq!(
            headers[header::LOCATION],
            format!("/auth/login?return_to={COSTS}"),
            "the query is not carried into the return path"
        );
    }

    // Swagger UI's "Try it out" gets a body that names the fix instead.
    let (status, headers, body) = send(
        embedded(),
        post(COSTS)
            .header("x-navigator-swagger-ui", "1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(headers.contains_key(header::WWW_AUTHENTICATE));
    assert!(body.contains(r#""login":"/auth/login?return_to=/app/api""#));
}

#[tokio::test]
async fn an_upstream_identity_decides_instead_of_the_browser_cookie() {
    let lawyer_cookie = cookie(&SessionData::fresh("cookie-lawyer", Role::Lawyer));

    // A bearer identity without a session is evaluated by its own role.
    for (role, expected) in [
        (Role::Lawyer, StatusCode::OK),
        (Role::Client, StatusCode::FORBIDDEN),
    ] {
        let (status, _, body) = send(
            embedded(),
            post(COSTS)
                .extension(crate::auth::AuthClaims {
                    sub: "bearer@example.com".into(),
                    exp: -5,
                    role,
                })
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, expected, "{role:?}");
        if status == StatusCode::OK {
            assert_eq!(body, "bearer@example.com");
        }
    }

    // A decoded session already on the request wins over any cookie.
    let (status, _, _) = send(
        embedded(),
        post(COSTS)
            .extension(SessionData::fresh("client-sub", Role::Client))
            .header(header::COOKIE, &lawyer_cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // An MCP bearer identity never borrows a browser cookie's role.
    let (status, _, _) = send(
        embedded(),
        post(COSTS)
            .extension(mcp::Principal::new("agent@example.com"))
            .header(header::COOKIE, &lawyer_cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn a_read_only_client_view_can_read_and_stop_but_never_write() {
    // Even a policy that admits everything cannot let the view write.
    let open = || app(PolicyClient::passthrough());
    let viewer = cookie(&viewing(Role::Client));
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let (status, _, body) = send(
            open(),
            Request::builder()
                .method(method)
                .uri(COSTS)
                .header(header::COOKIE, &viewer)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method}");
        assert_eq!(body, r#"{"error":"forbidden"}"#);
    }
    let (status, _, body) = send(
        open(),
        Request::get(COSTS)
            .header(header::COOKIE, &viewer)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "client-sub"));

    // Leaving the view is the one write it keeps, whatever the policy says.
    let deny_all =
        PolicyClient::new("package navigator.authz\nimport rego.v1\ndefault allow := false")
            .expect("policy compiles");
    let (status, _, body) = send(
        app(deny_all),
        post("/app/view-as-client/stop")
            .header(header::COOKIE, &viewer)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "client-sub"));
}

#[tokio::test]
async fn a_policy_that_cannot_be_evaluated_admits_nobody() {
    let broken = || {
        app(PolicyClient::new(
            "package navigator.authz\nimport rego.v1\ndefault allow := false\nallow := 1 / 0",
        )
        .expect("policy compiles"))
    };
    let owner = cookie(&SessionData::fresh("owner-sub", Role::Owner));
    let (status, headers, body) = send(
        broken(),
        Request::get("/app/admin/people")
            .header(header::COOKIE, &owner)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/html"),
        "a browser page gets the styled refusal, not {body}"
    );

    let (status, _, _) = send(
        broken(),
        Request::get("/app/admin/people")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
}
