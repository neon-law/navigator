//! Browser sign-in uses the one registered callback origin, and a brand host
//! gets the sign-in handed back for its own session.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use portal::{OAuthConfig, SessionStore};
use tower::ServiceExt;

fn request(host: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("host", host)
        .body(Body::empty())
        .unwrap()
}

async fn app() -> axum::Router {
    app_with_config("https://primary.example/auth/callback", false, None).await
}

async fn app_with_config(callback: &str, chooser: bool, canonical: Option<&str>) -> axum::Router {
    let surreal = store::test_support::mem_surreal().await;
    let mut state = portal::test_support::app_state(surreal).await;
    state.sessions = SessionStore::new("synthetic-host-test-signing-key");
    state.oauth = Some(OAuthConfig::new(
        "client",
        "secret",
        callback,
        "https://idp.example/authorize",
        "https://idp.example/token",
    ));
    if chooser {
        state.oauth_microsoft = state.oauth.clone();
        state.oauth_apple = state.oauth.clone();
    }
    // A secondary local listener resolves a brand without relying on a live
    // brand hostname. Cookies still belong to the hostname, not the port.
    state.canonical_host = portal::canonical_host::CanonicalHost::new(canonical.map(str::to_owned))
        .with_local_ports([(20430, views::brand::BrandKey::DeleteYourData)].into());
    server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR))
}

#[tokio::test]
async fn chooser_cookie_and_provider_selection_begin_only_on_primary() {
    use http_body_util::BodyExt;

    let app = app_with_config("https://primary.example/auth/callback", true, None).await;
    let path = "/auth/login?return_to=%2Fapp%2Fprojects%3Fstatus%3Dclosed%26search%3Dsample";
    let secondary = app
        .clone()
        .oneshot(request("secondary.example:20430", path))
        .await
        .unwrap();
    assert_eq!(secondary.status(), StatusCode::SEE_OTHER);
    assert!(!secondary.headers().contains_key("set-cookie"));
    let primary = app
        .clone()
        .oneshot(request("primary.example", path))
        .await
        .unwrap();
    assert_eq!(primary.status(), StatusCode::OK);
    assert!(primary.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .contains(portal::oauth::LOGIN_CSRF_COOKIE_NAME));
    let body = primary.into_body().collect().await.unwrap().to_bytes();
    let html = std::str::from_utf8(&body).unwrap();
    for provider in ["oidc", "microsoft", "apple"] {
        assert!(
            html.contains(&format!(
            "/auth/login/{provider}?return_to=/app/projects%3Fstatus%3Dclosed%26search%3Dsample"
        )),
            "{html}"
        );
        let response = app
            .clone()
            .oneshot(request(
                "primary.example",
                &format!("/auth/login/{provider}"),
            ))
            .await
            .unwrap();
        let pre = pre_auth(&response);
        assert_eq!(pre.provider.slug(), provider);
        assert!(!pre.state.is_empty());
        assert!(!pre.verifier.is_empty());
    }
}

fn pre_auth(response: &axum::response::Response) -> portal::oauth::PreAuth {
    let cookie = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|value| value.to_str().unwrap())
        .find_map(|value| value.strip_prefix("navigator_pre_auth="))
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let bytes = SessionStore::new("synthetic-host-test-signing-key")
        .decode_signed_bytes(cookie)
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn unsafe_return_destinations_are_not_signed_into_pre_auth() {
    let app = app().await;
    for target in [
        "https://outside.example/",
        "//outside.example/",
        "/\\outside.example/",
        "/app\r\nLocation: https://outside.example/",
    ] {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("return_to", target)
            .finish();
        let response = app
            .clone()
            .oneshot(request("primary.example", &format!("/auth/login?{query}")))
            .await
            .unwrap();
        assert_eq!(pre_auth(&response).return_to, "", "{target}");
    }
    let response = app
        .oneshot(request(
            "primary.example",
            "/auth/login?return_to=%2Fapp%2Fprojects%3Fstatus%3Dclosed%26search%3Dsample",
        ))
        .await
        .unwrap();
    assert_eq!(
        pre_auth(&response).return_to,
        "/app/projects?status=closed&search=sample"
    );
}

#[tokio::test]
async fn callback_origin_wins_over_canonical_host_without_looping() {
    let app = app_with_config(
        "http://localhost:20400/auth/callback",
        false,
        Some("canonical.example"),
    )
    .await;
    let secondary = app
        .clone()
        .oneshot(request("localhost:20430", "/auth/login"))
        .await
        .unwrap();
    // A loopback brand listener detours with its handoff parameters.
    assert!(secondary.headers()["location"]
        .to_str()
        .unwrap()
        .starts_with(
            "http://localhost:20400/auth/login?return_host=localhost%3A20430&handoff_nonce="
        ));
    let primary = app
        .clone()
        .oneshot(request("localhost:20400", "/auth/login"))
        .await
        .unwrap();
    assert!(primary.headers()["location"]
        .to_str()
        .unwrap()
        .starts_with("https://idp.example/authorize?"));
    let callback = app
        .oneshot(request("localhost:20400", "/auth/callback?code=x&state=x"))
        .await
        .unwrap();
    assert_eq!(callback.status(), StatusCode::BAD_REQUEST);
}

async fn round_trip_app() -> (axum::Router, wiremock::MockServer) {
    let idp = wiremock::MockServer::start().await;
    let surreal = store::test_support::mem_surreal().await;
    store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Synthetic Person",
            "person@example.com",
            store::persons::Role::Client,
        ),
    )
    .await
    .unwrap();
    let mut state = portal::test_support::app_state(surreal).await;
    state.sessions = SessionStore::new("synthetic-host-test-signing-key");
    state.oauth = Some(portal::test_support::oauth_config_with_verifier(
        OAuthConfig::new(
            "client",
            "secret",
            "https://primary.example/auth/callback",
            format!("{}/authorize", idp.uri()),
            format!("{}/token", idp.uri()),
        ),
        "client",
    ));
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    (app, idp)
}

#[tokio::test]
async fn two_host_round_trip_keeps_state_pkce_and_safe_return_path() {
    use base64::Engine;
    use http_body_util::BodyExt;
    use sha2::{Digest, Sha256};
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, ResponseTemplate};

    let (app, idp) = round_trip_app().await;
    let login_path = "/auth/login?return_to=%2Fapp%2Fprojects%3Fstatus%3Dclosed%26search%3Dsample";
    let secondary = app
        .clone()
        .oneshot(request("secondary.example", login_path))
        .await
        .unwrap();
    assert_eq!(
        secondary.headers()["location"],
        format!("https://primary.example{login_path}")
    );
    assert!(!secondary.headers().contains_key("set-cookie"));

    let login = app
        .clone()
        .oneshot(request("primary.example", login_path))
        .await
        .unwrap();
    let pre = pre_auth(&login);
    let cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let authorize = url::Url::parse(login.headers()["location"].to_str().unwrap()).unwrap();
    let params: std::collections::HashMap<_, _> = authorize.query_pairs().collect();
    assert_eq!(params["state"], pre.state);
    assert_eq!(params["nonce"], pre.nonce);
    assert_eq!(
        params["redirect_uri"],
        "https://primary.example/auth/callback"
    );
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(
        params["code_challenge"],
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(pre.verifier.as_bytes()))
    );

    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains(format!(
            "code_verifier={}",
            pre.verifier
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id_token": portal::test_support::sign_id_token(
                "client", &pre.nonce, "synthetic-subject", "person@example.com", "Synthetic Person",
            ),
            "token_type": "Bearer",
        })))
        .expect(1)
        .mount(&idp)
        .await;

    // A callback without the primary host's cookie still fails closed.
    let callback_path = format!("/auth/callback?code=code&state={}", pre.state);
    let missing = app
        .clone()
        .oneshot(request("primary.example", &callback_path))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
    let body = missing.into_body().collect().await.unwrap().to_bytes();
    assert!(std::str::from_utf8(&body)
        .unwrap()
        .contains("https://primary.example/auth/login"));
    let mut wrong_state = request("primary.example", "/auth/callback?code=code&state=wrong");
    wrong_state
        .headers_mut()
        .insert("cookie", cookie.parse().unwrap());
    let rejected = app.clone().oneshot(wrong_state).await.unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    assert!(idp.received_requests().await.unwrap().is_empty());

    let mut callback = request("primary.example", &callback_path);
    callback
        .headers_mut()
        .insert("cookie", cookie.parse().unwrap());
    let response = app.oneshot(callback).await.unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        "/app/projects?status=closed&search=sample"
    );
    assert!(response
        .headers()
        .get_all("set-cookie")
        .iter()
        .any(|value| value.to_str().unwrap().starts_with("navigator_session=")));
}

/// Run against the generated worktree environment and the browser harness.
/// Distinct loopback names exercise actual host-only cookie isolation: the
/// sign-in passes through the primary callback and ends on the brand listener
/// with that listener's own session.
#[tokio::test]
async fn browser_login_from_secondary_loopback_host_is_handed_back() {
    use features::webdriver::{base_url, new_client_or_skip, require_harness, wait_for_path};
    use std::time::Duration;

    let Ok(port) = std::env::var("NAVIGATOR_LOCAL_DELETE_YOUR_DATA_PORT") else {
        assert!(
            !require_harness(),
            "source the worktree environment for the secondary listener"
        );
        return;
    };
    let Some(browser) = new_client_or_skip().await else {
        return;
    };
    let primary = url::Url::parse(&base_url()).unwrap();
    assert_eq!(primary.host_str(), Some("localhost"));
    let secondary = format!("http://127.0.0.1:{port}");
    let secondary_origin = url::Url::parse(&secondary).unwrap().origin();
    login_with_optional_capture(&browser, &secondary).await;
    wait_for_path(&browser, "/app/projects", Duration::from_secs(20)).await;
    assert_eq!(
        browser.current_url().await.unwrap().origin(),
        secondary_origin
    );
    let cookies = browser.get_all_cookies().await.unwrap();
    assert!(cookies
        .iter()
        .any(|cookie| cookie.name() == "navigator_session"));
    assert!(cookies
        .iter()
        .all(|cookie| cookie.name() != "navigator_handoff_nonce"));
    browser
        .goto(&format!("{secondary}/app/projects"))
        .await
        .unwrap();
    wait_for_path(&browser, "/app/projects", Duration::from_secs(20)).await;
    assert_eq!(
        browser.current_url().await.unwrap().origin(),
        secondary_origin
    );
    assert!(!browser
        .source()
        .await
        .unwrap()
        .contains("could not be completed"));
    browser.close().await.unwrap();
}

async fn login_with_optional_capture(browser: &fantoccini::Client, secondary: &str) {
    let password = ["pass", "word"].concat();
    let login = features::webdriver::login_as_at(
        browser,
        secondary,
        "lawyer@neonlaw.com",
        &password,
        "/app/projects",
    );
    if std::env::var("NAV_OAUTH_SHOTS").as_deref() != Ok("1") {
        login.await;
        return;
    }
    let directory = std::path::Path::new("/tmp/navigator-screenshots/oauth-host");
    std::fs::create_dir_all(directory).unwrap();
    tokio::pin!(login);
    let mut frame = 0;
    loop {
        let complete = tokio::select! {
            () = &mut login => true,
            () = tokio::time::sleep(std::time::Duration::from_millis(400)) => false,
        };
        std::fs::write(
            directory.join(format!("frame-{frame:04}.png")),
            browser.screenshot().await.unwrap(),
        )
        .unwrap();
        frame += 1;
        if complete {
            break;
        }
    }
}

#[tokio::test]
async fn secondary_login_redirects_before_minting_host_only_pre_auth_cookie() {
    let response = app()
        .await
        .oneshot(
            Request::builder()
                .uri("/auth/login?return_to=%2Fapp%2Fprojects%3Fstatus%3Dclosed")
                .header("host", "secondary.example:20430")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        "https://primary.example/auth/login?return_to=%2Fapp%2Fprojects%3Fstatus%3Dclosed"
    );
    assert!(!response.headers().contains_key("set-cookie"));
}

#[tokio::test]
async fn secondary_app_and_direct_provider_entries_keep_path_and_query() {
    let app = app().await;
    for path in [
        "/app",
        "/app/projects?status=closed&search=sample%20matter",
        "/auth/login/oidc?return_to=%2Fapp%2Fprojects%3Fstatus%3Dclosed",
        "/auth/login/microsoft?return_to=%2Fapp%2Fteam",
        "/auth/login/apple?return_to=%2Fapp%2Fteam",
    ] {
        let response = app
            .clone()
            .oneshot(request("secondary.example:20430", path))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER, "{path}");
        assert_eq!(
            response.headers()["location"],
            format!("https://primary.example{path}")
        );
        assert!(!response.headers().contains_key("set-cookie"), "{path}");
    }
}

#[tokio::test]
async fn primary_login_accepts_case_and_default_port_without_a_redirect_loop() {
    let app = app().await;
    for host in ["primary.example", "PRIMARY.EXAMPLE:443"] {
        let response = app
            .clone()
            .oneshot(request(host, "/auth/login/oidc"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response.headers()["location"].to_str().unwrap();
        assert!(location.starts_with("https://idp.example/authorize?"));
        assert!(location.contains("code_challenge_method=S256"));
        assert!(location.contains("state="));
        assert!(response.headers().contains_key("set-cookie"));
    }
    for slug in ["unknown", "microsoft", "apple"] {
        let response = app
            .clone()
            .oneshot(request("primary.example", &format!("/auth/login/{slug}")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(!response.headers().contains_key("set-cookie"));
    }
}

#[tokio::test]
async fn secondary_public_pages_and_health_probes_stay_on_their_host() {
    let app = app().await;
    for path in ["/", "/app/health", "/app/readyz"] {
        let response = app
            .clone()
            .oneshot(request("secondary.example:20430", path))
            .await
            .unwrap();
        assert!(!response.status().is_redirection(), "{path}");
    }
}

// --- Per-host sign-in -------------------------------------------------------

const LOCAL_PRIMARY: &str = "localhost:4001";
const LOCAL_BRAND: &str = "127.0.0.1:20430";

async fn handoff_app() -> (axum::Router, wiremock::MockServer) {
    let idp = wiremock::MockServer::start().await;
    let surreal = store::test_support::mem_surreal().await;
    store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Synthetic Person",
            "person@example.com",
            store::persons::Role::Client,
        ),
    )
    .await
    .unwrap();
    let mut state = portal::test_support::app_state(surreal).await;
    state.sessions = SessionStore::new("synthetic-host-test-signing-key");
    state.oauth = Some(portal::test_support::oauth_config_with_verifier(
        OAuthConfig::new(
            "client",
            "secret",
            format!("http://{LOCAL_PRIMARY}/auth/callback"),
            format!("{}/authorize", idp.uri()),
            format!("{}/token", idp.uri()),
        ),
        "client",
    ));
    state.canonical_host = portal::canonical_host::CanonicalHost::new(None)
        .with_local_ports([(20430, views::brand::BrandKey::DeleteYourData)].into());
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    (app, idp)
}

fn set_cookie<'a>(response: &'a axum::response::Response, name: &str) -> Option<&'a str> {
    response
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|value| value.to_str().unwrap())
        .find(|value| value.starts_with(&format!("{name}=")))
        .and_then(|value| value.split(';').next())
}

fn with_cookies(mut req: Request<Body>, cookies: &[&str]) -> Request<Body> {
    req.headers_mut()
        .insert("cookie", cookies.join("; ").parse().unwrap());
    req
}

async fn mount_token(idp: &wiremock::MockServer, nonce: &str) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id_token": portal::test_support::sign_id_token(
                "client", nonce, "synthetic-subject", "person@example.com", "Synthetic Person",
            ),
            "token_type": "Bearer",
        })))
        .mount(idp)
        .await;
}

/// A brand host's sign-in uses the one registered callback and comes back to
/// that host with its own session, bound to the browser that started it.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn brand_host_sign_in_is_handed_back_with_its_own_session() {
    let (app, idp) = handoff_app().await;
    let login_path = "/auth/login?return_to=%2Fapp%2Fprojects";

    // 1. The brand host sets its nonce cookie and detours to the callback origin.
    let brand_login = app
        .clone()
        .oneshot(request(LOCAL_BRAND, login_path))
        .await
        .unwrap();
    assert_eq!(brand_login.status(), StatusCode::SEE_OTHER);
    let nonce_cookie = set_cookie(&brand_login, portal::host_handoff::HANDOFF_NONCE_COOKIE)
        .unwrap()
        .to_owned();
    let detour = url::Url::parse(brand_login.headers()["location"].to_str().unwrap()).unwrap();
    assert_eq!(
        detour.origin().ascii_serialization(),
        format!("http://{LOCAL_PRIMARY}")
    );
    let params: std::collections::HashMap<_, _> = detour.query_pairs().into_owned().collect();
    assert_eq!(params["return_to"], "/app/projects");
    assert_eq!(params["return_host"], LOCAL_BRAND);
    assert_eq!(
        format!(
            "{}={}",
            portal::host_handoff::HANDOFF_NONCE_COOKIE,
            params["handoff_nonce"]
        ),
        nonce_cookie
    );

    // 2. The callback origin starts the provider redirect and remembers the host.
    let primary_login = app
        .clone()
        .oneshot(request(
            LOCAL_PRIMARY,
            &format!("{}?{}", detour.path(), detour.query().unwrap()),
        ))
        .await
        .unwrap();
    let pre = pre_auth(&primary_login);
    let pre_cookie = set_cookie(&primary_login, "navigator_pre_auth")
        .unwrap()
        .to_owned();
    let return_cookie = set_cookie(&primary_login, portal::host_handoff::RETURN_HOST_COOKIE)
        .unwrap()
        .to_owned();
    mount_token(&idp, &pre.nonce).await;

    // 3. The callback hands the verified sign-in back to the brand host.
    let callback = app
        .clone()
        .oneshot(with_cookies(
            request(
                LOCAL_PRIMARY,
                &format!("/auth/callback?code=code&state={}", pre.state),
            ),
            &[&pre_cookie, &return_cookie],
        ))
        .await
        .unwrap();
    assert_eq!(callback.status(), StatusCode::SEE_OTHER);
    let handoff = url::Url::parse(callback.headers()["location"].to_str().unwrap()).unwrap();
    assert_eq!(
        handoff.origin().ascii_serialization(),
        format!("http://{LOCAL_BRAND}")
    );
    assert_eq!(handoff.path(), "/auth/handoff");
    let handoff_path = format!("{}?{}", handoff.path(), handoff.query().unwrap());

    // A captured handoff is refused in a browser without the brand host's nonce.
    let stolen = app
        .clone()
        .oneshot(request(LOCAL_BRAND, &handoff_path))
        .await
        .unwrap();
    assert_eq!(stolen.status(), StatusCode::BAD_REQUEST);
    assert!(set_cookie(&stolen, "navigator_session").is_none());
    // Nor is it redeemable on a different host.
    let elsewhere = app
        .clone()
        .oneshot(with_cookies(
            request(LOCAL_PRIMARY, &handoff_path),
            &[&nonce_cookie],
        ))
        .await
        .unwrap();
    assert_eq!(elsewhere.status(), StatusCode::BAD_REQUEST);

    // 4. The brand host sets its own session and lands on the deep link.
    let landed = app
        .clone()
        .oneshot(with_cookies(
            request(LOCAL_BRAND, &handoff_path),
            &[&nonce_cookie],
        ))
        .await
        .unwrap();
    assert_eq!(landed.status(), StatusCode::SEE_OTHER);
    assert_eq!(landed.headers()["location"], "/app/projects");
    assert_eq!(landed.headers()["referrer-policy"], "no-referrer");
    let session = set_cookie(&landed, "navigator_session").unwrap().to_owned();

    // The brand host now serves its own `/app` rather than bouncing it.
    let app_page = app
        .oneshot(with_cookies(
            request(LOCAL_BRAND, "/app/projects"),
            &[&session],
        ))
        .await
        .unwrap();
    assert!(
        !app_page
            .headers()
            .get("location")
            .is_some_and(|location| location.to_str().unwrap().contains(LOCAL_PRIMARY)),
        "a brand host keeps its own /app"
    );
}

/// A `return_host` this deployment does not serve is ignored: the sign-in
/// completes on the callback origin, never on the named host.
#[tokio::test]
async fn an_unserved_return_host_is_never_handed_a_sign_in() {
    let (app, idp) = handoff_app().await;
    for host in ["outside.example", "outside.example:20430", "localhost:9"] {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("return_host", host)
            .append_pair("handoff_nonce", "0123456789abcdef0123456789abcdef")
            .finish();
        let login = app
            .clone()
            .oneshot(request(LOCAL_PRIMARY, &format!("/auth/login?{query}")))
            .await
            .unwrap();
        assert!(
            set_cookie(&login, portal::host_handoff::RETURN_HOST_COOKIE).is_none(),
            "{host}"
        );
        let pre = pre_auth(&login);
        let pre_cookie = set_cookie(&login, "navigator_pre_auth").unwrap().to_owned();
        idp.reset().await;
        mount_token(&idp, &pre.nonce).await;
        let callback = app
            .clone()
            .oneshot(with_cookies(
                request(
                    LOCAL_PRIMARY,
                    &format!("/auth/callback?code=code&state={}", pre.state),
                ),
                &[&pre_cookie],
            ))
            .await
            .unwrap();
        assert_eq!(callback.status(), StatusCode::SEE_OTHER, "{host}");
        assert!(
            callback.headers()["location"]
                .to_str()
                .unwrap()
                .starts_with('/'),
            "{host}"
        );
    }
}
