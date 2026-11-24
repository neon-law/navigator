//! The session and bearer boundaries, driven through mounted middleware:
//! who is admitted, how an anonymous caller is refused, and that a scoped CI
//! session never leaves its routes.

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::middleware::from_fn_with_state;
use axum::{Extension, Router};
use jsonwebtoken::{encode, EncodingKey, Header};
use store::persons::Role;
use tower::ServiceExt;
use uuid::Uuid;

use super::{
    inject_bearer_session, inject_optional_session, require_auth, require_session, AuthClaims,
    AuthConfig,
};
use crate::session::{
    DocumentScope, SessionData, SessionScope, SessionSource, SessionStore, SESSION_COOKIE_NAME,
};
use crate::test_support::TEST_SESSION_KEY;

const SECRET: &str = "synthetic-bearer-secret";

fn sessions() -> SessionStore {
    SessionStore::new(TEST_SESSION_KEY)
}

/// Answers with whichever identity the boundary under test admitted.
async fn who(
    session: Option<Extension<SessionData>>,
    claims: Option<Extension<AuthClaims>>,
) -> String {
    match (session, claims) {
        (Some(session), _) => format!("session:{}", session.sub),
        (None, Some(claims)) => format!("claims:{}", claims.sub),
        (None, None) => "nobody".to_owned(),
    }
}

fn session_boundary(auth: AuthConfig) -> Router {
    Router::new()
        .fallback(who)
        .layer(from_fn_with_state((sessions(), auth), require_session))
        .layer(tower_cookies::CookieManagerLayer::new())
}

fn open() -> AuthConfig {
    AuthConfig::new(false, None)
}

fn bearer(sub: &str, secret: &str) -> String {
    let claims = AuthClaims {
        sub: sub.into(),
        exp: i64::try_from(jsonwebtoken::get_current_timestamp() + 3600).unwrap(),
        role: Role::Lawyer,
    };
    let key = EncodingKey::from_secret(secret.as_bytes()).unwrap();
    format!(
        "Bearer {}",
        encode(&Header::default(), &claims, &key).unwrap()
    )
}

fn cookie(session: &SessionData) -> String {
    format!("{SESSION_COOKIE_NAME}={}", sessions().encode(session))
}

fn expired() -> SessionData {
    SessionData {
        exp: 1,
        ..SessionData::fresh("expired-sub", Role::Lawyer)
    }
}

fn legacy_ci() -> SessionData {
    SessionData {
        source: SessionSource::Ci,
        ..SessionData::fresh("ci-sub", Role::Lawyer)
    }
}

/// A CI session minted for one Project's gate reads.
fn gate(project_id: Uuid) -> SessionData {
    SessionData {
        scope: Some(SessionScope::Document(DocumentScope {
            project_id,
            project_code: "acme".into(),
        })),
        ..legacy_ci()
    }
}

async fn send(app: Router, request: Request<Body>) -> (StatusCode, HeaderMap, String) {
    let response = app.oneshot(request).await.unwrap();
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    (
        parts.status,
        parts.headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

async fn get(
    app: Router,
    path: &str,
    headers: &[(header::HeaderName, &str)],
) -> (StatusCode, HeaderMap, String) {
    let mut request = Request::get(path);
    for (name, value) in headers {
        request = request.header(name, *value);
    }
    send(app, request.body(Body::empty()).unwrap()).await
}

#[tokio::test]
async fn an_anonymous_caller_is_refused_in_the_shape_its_client_can_use() {
    // A browser page: sign in, then come back to the same address.
    let (status, headers, _) = get(session_boundary(open()), "/app/projects?tab=open", &[]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        headers[header::LOCATION],
        "/auth/login?return_to=/app/projects%3Ftab=open"
    );

    // A JSON route, or any caller that sent a bearer: a parseable 401.
    for (path, extra) in [
        ("/app/api/projects", None),
        (
            "/app/projects",
            Some((header::AUTHORIZATION, "Bearer stale")),
        ),
    ] {
        let extra: Vec<_> = extra.into_iter().collect();
        let (status, headers, body) = get(session_boundary(open()), path, &extra).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        assert!(headers.contains_key(header::WWW_AUTHENTICATE));
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["error"], "unauthenticated");
        assert_eq!(body["login"], format!("/auth/login?return_to={path}"));
    }

    let swagger = header::HeaderName::from_static("x-navigator-swagger-ui");
    let (status, _, body) = get(
        session_boundary(open()),
        "/app/api/projects",
        &[(swagger, "1")],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("Try it out"), "{body}");

    // An expired or forged session is no session, however it arrives.
    for stale in [cookie(&expired()), format!("{SESSION_COOKIE_NAME}=forged")] {
        let (status, _, _) = get(
            session_boundary(open()),
            "/app/projects",
            &[(header::COOKIE, &stale)],
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{stale}");
    }
    let (status, _, _) = send(
        session_boundary(open()),
        Request::get("/app/projects")
            .extension(expired())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn a_signed_session_or_a_verified_bearer_is_admitted() {
    let lawyer = SessionData::fresh("lawyer-sub", Role::Lawyer);
    let (status, _, body) = get(
        session_boundary(open()),
        "/app/projects",
        &[(header::COOKIE, &cookie(&lawyer))],
    )
    .await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::OK, "session:lawyer-sub")
    );

    let enforced = || session_boundary(AuthConfig::new(false, Some(SECRET)));
    let (status, _, body) = get(
        enforced(),
        "/app/api/projects",
        &[(header::AUTHORIZATION, &bearer("agent", SECRET))],
    )
    .await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "claims:agent"));
    let (status, _, _) = get(
        enforced(),
        "/app/api/projects",
        &[(header::AUTHORIZATION, &bearer("agent", "another-secret"))],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Without a configured verifier a well-formed bearer proves nothing.
    let (status, _, _) = get(
        session_boundary(open()),
        "/app/api/projects",
        &[(header::AUTHORIZATION, &bearer("agent", SECRET))],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_ci_session_is_held_to_the_routes_it_was_minted_for() {
    let project = Uuid::new_v4();
    let integrity = format!("/app/api/projects/{project}/documents/integrity");
    let another = format!("/app/api/projects/{}/documents/integrity", Uuid::new_v4());
    let scoped = cookie(&gate(project));
    let refused = |body: &str| {
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["error"], "scope_violation");
    };

    let (status, _, body) = get(
        session_boundary(open()),
        &integrity,
        &[(header::COOKIE, &scoped)],
    )
    .await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "session:ci-sub"));
    for path in [another.as_str(), "/app/api/people"] {
        let (status, _, body) =
            get(session_boundary(open()), path, &[(header::COOKIE, &scoped)]).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        refused(&body);
    }
    // A read route is not a write route.
    let (status, _, body) = send(
        session_boundary(open()),
        Request::post(&integrity)
            .header(header::COOKIE, &scoped)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    refused(&body);
    // A CI session with no scope at all is admitted nowhere.
    let (status, _, body) = get(
        session_boundary(open()),
        &integrity,
        &[(header::COOKIE, &cookie(&legacy_ci()))],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    refused(&body);

    // The same sessions presented as a bearer blob.
    let bearer_boundary = || {
        Router::new()
            .fallback(who)
            .layer(from_fn_with_state(sessions(), inject_bearer_session))
    };
    let blob = |session: &SessionData| format!("Bearer {}", sessions().encode(session));
    let (status, _, body) = get(
        bearer_boundary(),
        &integrity,
        &[(header::AUTHORIZATION, &blob(&gate(project)))],
    )
    .await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "session:ci-sub"));
    let (status, _, body) = get(
        bearer_boundary(),
        &another,
        &[(header::AUTHORIZATION, &blob(&gate(project)))],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    refused(&body);
    let (status, _, body) = get(
        bearer_boundary(),
        &integrity,
        &[(header::AUTHORIZATION, &blob(&legacy_ci()))],
    )
    .await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "nobody"));
}

#[tokio::test]
async fn a_public_page_sees_a_session_only_when_it_is_valid_for_that_route() {
    let project = Uuid::new_v4();
    let surface = format!("/app/api/project-surfaces/{project}");
    let optional = || {
        Router::new()
            .fallback(who)
            .layer(from_fn_with_state(sessions(), inject_optional_session))
            .layer(tower_cookies::CookieManagerLayer::new())
    };
    let lawyer = SessionData::fresh("lawyer-sub", Role::Lawyer);
    for (path, cookies, expected) in [
        ("/", Some(cookie(&lawyer)), "session:lawyer-sub"),
        ("/", None, "nobody"),
        ("/", Some(cookie(&expired())), "nobody"),
        ("/", Some(cookie(&legacy_ci())), "nobody"),
        ("/", Some(cookie(&gate(project))), "nobody"),
        (
            surface.as_str(),
            Some(cookie(&gate(project))),
            "session:ci-sub",
        ),
    ] {
        let headers: Vec<_> = cookies
            .as_deref()
            .map(|value| (header::COOKIE, value))
            .into_iter()
            .collect();
        let (status, _, body) = get(optional(), path, &headers).await;
        assert_eq!(
            (status, body.as_str()),
            (StatusCode::OK, expected),
            "{path}"
        );
    }
    // A session an earlier layer decoded is kept as it is.
    let (_, _, body) = send(
        optional(),
        Request::get("/")
            .extension(SessionData::fresh("upstream-sub", Role::Client))
            .header(header::COOKIE, cookie(&lawyer))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(body, "session:upstream-sub");
}

#[tokio::test]
async fn the_bearer_gate_admits_only_a_token_its_verifier_signed() {
    let gate = |auth: AuthConfig| {
        Router::new()
            .fallback(who)
            .layer(from_fn_with_state(auth, require_auth))
    };
    let enforced = || gate(AuthConfig::new(false, Some(SECRET)));

    let (status, _, body) = get(
        enforced(),
        "/x",
        &[(header::AUTHORIZATION, &bearer("agent", SECRET))],
    )
    .await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "claims:agent"));
    for refused in [
        None,
        Some("Basic dXNlcjpwYXNz".to_owned()),
        Some("Bearer not-a-token".to_owned()),
        Some(bearer("agent", "another-secret")),
    ] {
        let headers: Vec<_> = refused
            .as_deref()
            .map(|value| (header::AUTHORIZATION, value))
            .into_iter()
            .collect();
        let (status, _, _) = get(enforced(), "/x", &headers).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{refused:?}");
    }

    // Claims an earlier layer verified are not verified twice.
    let (status, _, body) = send(
        enforced(),
        Request::get("/x")
            .extension(AuthClaims {
                sub: "upstream".into(),
                exp: 0,
                role: Role::Client,
            })
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "claims:upstream"));

    // A deployment with verification switched off, or never configured, passes through.
    for auth in [AuthConfig::new(true, Some(SECRET)), open()] {
        let (status, _, body) = get(gate(auth), "/x", &[]).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "nobody"));
    }
}
