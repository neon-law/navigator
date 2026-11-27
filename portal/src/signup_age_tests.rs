//! The signup age gate, driven through the mounted `/auth` routes with a
//! disconnected store handle: nothing here reads or writes a Person.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use tower::ServiceExt;

use crate::oauth::{AuthState, OAuthConfig};
use crate::session::SessionStore;
use crate::test_support::TEST_SESSION_KEY;

fn sessions() -> SessionStore {
    SessionStore::new(TEST_SESSION_KEY)
}

fn app(secure_cookies: bool) -> Router {
    crate::oauth::routes(AuthState {
        oauth: OAuthConfig::new(
            "client",
            "secret",
            "https://app.example.com/auth/callback",
            "https://idp.example.com/authorize",
            "https://idp.example.com/token",
        ),
        oauth_microsoft: None,
        oauth_apple: None,
        sessions: sessions(),
        application_store: store::application_store::ApplicationStore::uninitialized(),
        email: Arc::new(crate::email::CapturingEmail::new()),
        workflow_runtime: Arc::new(workflows::InMemoryRuntime::new()),
        bootstrap_owner_email: None,
        identity_password: None,
        identity_admin: None,
        secure_cookies,
        canonical_host: crate::CanonicalHost::new(None),
    })
    .layer(tower_cookies::CookieManagerLayer::new())
}

struct Reply {
    status: StatusCode,
    location: Option<String>,
    set_cookies: Vec<String>,
    body: String,
}

impl Reply {
    /// The `name=value` pair a response set, ready to send back.
    fn cookie(&self, name: &str) -> Option<String> {
        self.set_cookies
            .iter()
            .find(|cookie| cookie.starts_with(&format!("{name}=")))
            .and_then(|cookie| cookie.split(';').next())
            .map(str::to_owned)
    }

    fn csrf(&self) -> String {
        self.cookie("navigator_account_csrf")
            .expect("the age screen mints a CSRF cookie")
    }

    /// The token the signed CSRF cookie carries, as the form's hidden field.
    fn csrf_token(&self) -> String {
        let value = self.csrf();
        let signed = value.split_once('=').unwrap().1;
        String::from_utf8(sessions().decode_signed_bytes(signed).unwrap()).unwrap()
    }
}

async fn send(app: &Router, request: Request<Body>) -> Reply {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    Reply {
        status,
        location: headers
            .get(header::LOCATION)
            .map(|value| value.to_str().unwrap().to_owned()),
        set_cookies: headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().unwrap().to_owned())
            .collect(),
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

async fn screen(app: &Router, cookies: &str) -> Reply {
    send(
        app,
        Request::get("/auth/signup")
            .header(header::COOKIE, cookies)
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn submit(app: &Router, cookies: &str, form: &str) -> Reply {
    send(
        app,
        Request::post("/auth/signup/age")
            .header(header::COOKIE, cookies)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form.to_owned()))
            .unwrap(),
    )
    .await
}

/// Submit `age` behind a freshly minted CSRF pair, plus any `extra` cookie.
async fn answer(app: &Router, age: &str, extra: &str) -> Reply {
    let first = screen(app, extra).await;
    submit(
        app,
        &format!("{}; {extra}", first.csrf()),
        &format!("age={age}&csrf_token={}", first.csrf_token()),
    )
    .await
}

fn signed(payload: &str) -> String {
    format!(
        "navigator_signup_age={}",
        sessions().encode_signed_bytes(payload.as_bytes())
    )
}

#[tokio::test]
async fn the_age_screen_comes_first_and_an_answer_needs_its_csrf_token() {
    let app = app(false);
    let first = screen(&app, "").await;
    assert_eq!(first.status, StatusCode::OK);
    assert!(first.body.contains("/auth/signup/age"), "{}", first.body);

    for form in ["age=30", "age=30&csrf_token=forged"] {
        let refused = submit(&app, &first.csrf(), form).await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{form}");
        assert!(refused.cookie("navigator_signup_age").is_none());
    }
    assert_eq!(
        answer(&app, "121", "").await.status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn an_adult_reaches_account_creation_and_the_decision_is_only_a_signed_cookie() {
    let app = app(false);
    let accepted = answer(&app, "13", "").await;
    assert_eq!(accepted.status, StatusCode::SEE_OTHER);
    assert_eq!(accepted.location.as_deref(), Some("/auth/signup"));
    let decision = accepted.cookie("navigator_signup_age").unwrap();
    assert!(!decision.contains("13"), "the age itself is not kept");
    let stored = accepted
        .set_cookies
        .iter()
        .find(|cookie| cookie.starts_with("navigator_signup_age="))
        .unwrap();
    assert!(stored.contains("HttpOnly") && stored.contains("SameSite=Lax"));

    // No password door on this deployment: an eligible visitor goes to sign-in.
    let next = screen(&app, &decision).await;
    assert_eq!(next.status, StatusCode::SEE_OTHER);
    assert_eq!(next.location.as_deref(), Some("/auth/login"));
}

#[tokio::test]
async fn a_refusal_survives_reload_and_a_second_answer() {
    let app = app(false);
    let refused = answer(&app, "12", "").await;
    assert_eq!(refused.status, StatusCode::SEE_OTHER);
    let blocked = refused.cookie("navigator_signup_age").unwrap();

    assert_eq!(screen(&app, &blocked).await.status, StatusCode::FORBIDDEN);
    let retry = answer(&app, "40", &blocked).await;
    assert_eq!(retry.status, StatusCode::FORBIDDEN);
    assert!(
        retry.cookie("navigator_signup_age").is_none(),
        "a blocked browser cannot replace its decision"
    );
}

#[tokio::test]
async fn a_forged_expired_or_unknown_decision_is_no_decision() {
    let app = app(false);
    let future = chrono::Utc::now().timestamp() + 600;
    for cookie in [
        "navigator_signup_age=forged".to_owned(),
        signed("eligible:1"),
        signed(&format!("maybe:{future}")),
        signed("eligible"),
        signed("eligible:soon"),
    ] {
        let reply = screen(&app, &cookie).await;
        assert_eq!(reply.status, StatusCode::OK, "{cookie}");
        assert!(reply.location.is_none(), "{cookie}");
    }
    assert_eq!(
        screen(&app, &signed(&format!("eligible:{future}")))
            .await
            .status,
        StatusCode::SEE_OTHER
    );
}

#[tokio::test]
async fn an_https_deployment_lets_the_decision_ride_a_cross_site_callback() {
    let accepted = answer(&app(true), "30", "").await;
    let stored = accepted
        .set_cookies
        .iter()
        .find(|cookie| cookie.starts_with("navigator_signup_age="))
        .unwrap();
    assert!(
        stored.contains("SameSite=None") && stored.contains("Secure"),
        "{stored}"
    );
}
