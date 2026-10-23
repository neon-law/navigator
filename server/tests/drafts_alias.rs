#![allow(clippy::doc_markdown)]
//! Draft is Template (`docs/glossary/draft.md`), so every `/templates` path
//! has a `/drafts` twin, and each twin answers by the canonical handler:
//!
//! - the public gallery twins redirect (308) to the canonical route, keeping
//!   the suffix and the query string;
//! - the API and admin twins are served by the very same handlers, behind the
//!   same policy, so they answer exactly as the canonical route does;
//! - an alias request is never counted as a visit; the canonical request that
//!   follows is counted once.
//!
//! Every comparison below is between the alias and the canonical route on the
//! same router, never against a second copy of the expectation.

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use http_body_util::BodyExt;
use portal::AppState;
use store::persons::Role;
use tower::ServiceExt;

const FORM_990: &str = "notations/forms/united-states/federal/irs/us--form-990";

async fn app_with_db() -> (axum::Router, store::surreal::SurrealDb) {
    let db = portal::test_support::embedded_surreal().await;
    // The embedded Rego policy, not the passthrough default: these tests
    // are about who may reach each spelling, so the real rules must run.
    let state = AppState {
        policy: portal::policy::PolicyClient::embedded().expect("the embedded policy compiles"),
        ..portal::test_support::app_state(db.clone()).await
    };
    (
        server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR)),
        db,
    )
}

async fn app() -> axum::Router {
    app_with_db().await.0
}

fn sessions() -> portal::SessionStore {
    portal::SessionStore::new(portal::test_support::TEST_SESSION_KEY)
}

fn token(role: Role) -> String {
    sessions().encode(&portal::SessionData::fresh("alias-test-sub", role))
}

fn cookie(role: Role) -> String {
    format!("{}={}", portal::session::SESSION_COOKIE_NAME, token(role))
}

struct Seen {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

async fn send(
    app: &axum::Router,
    method: Method,
    uri: &str,
    role: Option<Role>,
    bearer: bool,
    json: Option<&str>,
) -> Seen {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(role) = role {
        request = if bearer {
            request.header(header::AUTHORIZATION, format!("Bearer {}", token(role)))
        } else {
            request.header(header::COOKIE, cookie(role))
        };
    }
    let body = match json {
        Some(json) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let (parts, body) = response.into_parts();
    Seen {
        status: parts.status,
        headers: parts.headers,
        body: body.collect().await.unwrap().to_bytes().to_vec(),
    }
}

async fn get(app: &axum::Router, uri: &str, role: Option<Role>) -> Seen {
    send(app, Method::GET, uri, role, false, None).await
}

fn location(seen: &Seen) -> &str {
    seen.headers
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
}

/// A response body with what legitimately differs per request removed: the
/// CSP nonce each page mints, the request path a login redirect or the page's
/// own links echo back (the alias names `/drafts`, the canonical route
/// `/templates`), and the tab title, which the portal derives from the URL
/// for every route (`/app/admin/drafts` is "Admin | Drafts"). Nothing else
/// may differ: the listing's heading, columns, rows, and links are
/// canonical.
fn normalized(body: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(body).replace("drafts", "templates");
    if let (Some(open), Some(close)) = (text.find("<title>"), text.find("</title>")) {
        text.replace_range(open..close + "</title>".len(), "");
    }
    let mut out = String::with_capacity(text.len());
    while let Some(start) = text.find("nonce=\"") {
        let value = start + "nonce=\"".len();
        let end = text[value..].find('"').map_or(text.len(), |n| value + n);
        out.push_str(&text[..value]);
        text = text[end..].to_string();
    }
    out.push_str(&text);
    out
}

/// What a client sees of a response, minus per-request noise: the status,
/// the bytes, and the headers that carry meaning here.
fn assert_same(alias: &Seen, canonical: &Seen, what: &str) {
    assert_eq!(alias.status, canonical.status, "{what}: status");
    let (a, c) = (normalized(&alias.body), normalized(&canonical.body));
    if a != c {
        let at = a.chars().zip(c.chars()).take_while(|(x, y)| x == y).count();
        panic!(
            "{what}: body bytes differ at {at}:\n alias:     {:?}\n canonical: {:?}",
            a.chars()
                .skip(at.saturating_sub(60))
                .take(160)
                .collect::<String>(),
            c.chars()
                .skip(at.saturating_sub(60))
                .take(160)
                .collect::<String>(),
        );
    }
    for name in [
        header::CONTENT_TYPE,
        header::CONTENT_DISPOSITION,
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
        header::VARY,
        header::CACHE_CONTROL,
    ] {
        assert_eq!(
            alias.headers.get(&name),
            canonical.headers.get(&name),
            "{what}: {name}"
        );
    }
}

#[tokio::test]
async fn get_and_head_on_the_public_aliases_redirect_with_the_suffix_and_query() {
    let app = app().await;
    let cases = [
        ("/drafts", "/templates"),
        (
            "/drafts?utm_source=newsletter",
            "/templates?utm_source=newsletter",
        ),
        (
            &format!("/drafts/{FORM_990}"),
            &format!("/templates/{FORM_990}"),
        ),
        (
            &format!("/drafts/{FORM_990}/download?utm_campaign=a%20b&x=1"),
            &format!("/templates/{FORM_990}/download?utm_campaign=a%20b&x=1"),
        ),
        // Percent-encoding and an empty query survive byte for byte.
        ("/drafts/a%2Fb/c?", "/templates/a%2Fb/c?"),
        ("/drafts/nonprofit/MadeUp", "/templates/nonprofit/MadeUp"),
    ];
    for method in [Method::GET, Method::HEAD] {
        for (alias, expected) in &cases {
            let seen = send(&app, method.clone(), alias, None, false, None).await;
            assert_eq!(
                seen.status,
                StatusCode::PERMANENT_REDIRECT,
                "{method} {alias}"
            );
            assert_eq!(location(&seen), *expected, "{method} {alias}");
        }
    }
}

#[tokio::test]
async fn an_alias_is_followed_to_the_same_content_download_or_404_as_the_canonical_route() {
    let app = app().await;
    for suffix in [
        String::new(),
        format!("/{FORM_990}"),
        format!("/{FORM_990}/download"),
        // A legacy alias and the kebab redirect are the canonical handler's.
        "/notations/forms/united_states/federal/irs/us__form_990".to_string(),
        "/nonprofit/MadeUp".to_string(),
        // Curated visibility: a confidential template is a 404 either way.
        "/neon-law/shared/retainer".to_string(),
        "/neon-law/shared/retainer/download".to_string(),
    ] {
        let alias = get(&app, &format!("/drafts{suffix}"), Some(Role::Client)).await;
        assert_eq!(alias.status, StatusCode::PERMANENT_REDIRECT, "{suffix}");
        let followed = get(&app, location(&alias), Some(Role::Client)).await;
        let canonical = get(&app, &format!("/templates{suffix}"), Some(Role::Client)).await;
        assert_same(&followed, &canonical, &format!("/drafts{suffix}"));
    }
    // The download really is the attachment, and the confidential one 404s.
    let download = get(
        &app,
        &format!("/templates/{FORM_990}/download"),
        Some(Role::Client),
    )
    .await;
    assert_eq!(download.status, StatusCode::OK);
    assert!(download.headers.contains_key(header::CONTENT_DISPOSITION));
    let retainer = get(
        &app,
        "/templates/neon-law/shared/retainer",
        Some(Role::Client),
    )
    .await;
    assert_eq!(retainer.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn only_the_drafts_path_and_its_descendants_are_aliased() {
    let app = app().await;
    // A path that merely starts with the word is not the alias.
    for path in ["/draftsman", "/drafts-archive", "/draft"] {
        let seen = get(&app, path, None).await;
        assert_ne!(seen.status, StatusCode::PERMANENT_REDIRECT, "{path}");
    }
    // A dot segment would let a client's own normalisation walk out of
    // `/templates`, so it is refused rather than carried over.
    for path in [
        "/drafts/../app/admin",
        "/drafts/%2e%2e/app",
        "/drafts/a/./b",
        "/drafts/%2E/b",
    ] {
        let seen = get(&app, path, None).await;
        assert_eq!(seen.status, StatusCode::NOT_FOUND, "{path}");
        assert!(!seen.headers.contains_key(header::LOCATION), "{path}");
    }
}

#[tokio::test]
async fn the_raw_source_api_twin_answers_exactly_as_the_canonical_route_for_every_caller() {
    let app = app().await;
    for suffix in [
        format!("/{FORM_990}"),
        "/neon-law/shared/retainer".to_string(),
        "/no/such/template".to_string(),
        "/notations/forms/united_states/federal/irs/us__form_990".to_string(),
    ] {
        for role in [None, Some(Role::Client), Some(Role::Lawyer)] {
            let canonical = send(
                &app,
                Method::GET,
                &format!("/app/api/templates{suffix}"),
                role,
                true,
                None,
            )
            .await;
            let alias = send(
                &app,
                Method::GET,
                &format!("/app/api/drafts{suffix}"),
                role,
                true,
                None,
            )
            .await;
            let who = format!("{role:?} {suffix}");
            // The kebab redirect names the canonical path, so compare the
            // redirect's shape rather than a Location the alias never chose.
            if canonical.status.is_redirection() {
                assert_eq!(alias.status, canonical.status, "{who}");
            } else {
                assert_same(&alias, &canonical, &who);
            }
        }
    }
    // The curated raw source really is served, as markdown, to a client.
    let served = get(
        &app,
        &format!("/app/api/drafts/{FORM_990}"),
        Some(Role::Client),
    )
    .await;
    assert_eq!(served.status, StatusCode::OK);
    assert_eq!(
        served.headers.get(header::CONTENT_TYPE).unwrap(),
        "text/markdown; charset=utf-8"
    );
}

#[tokio::test]
async fn both_validation_urls_lint_alike_and_the_exact_route_beats_the_catch_all() {
    let app = app().await;
    let valid = serde_json::json!({ "contents": "---\nfoo: bar\n---\n\nBody.\n" }).to_string();
    let invalid = serde_json::json!({
        "contents": format!("---\nfoo: bar\n---\n\n{}\n", "x".repeat(150)),
        "path": "t.md",
    })
    .to_string();
    for (label, body) in [("valid", &valid), ("invalid", &invalid)] {
        for role in [
            None,
            Some(Role::Client),
            Some(Role::Lawyer),
            Some(Role::Admin),
        ] {
            let canonical = send(
                &app,
                Method::POST,
                "/app/api/templates/validate",
                role,
                true,
                Some(body),
            )
            .await;
            let alias = send(
                &app,
                Method::POST,
                "/app/api/drafts/validate",
                role,
                true,
                Some(body),
            )
            .await;
            assert_same(&alias, &canonical, &format!("{label} {role:?}"));
        }
    }
    // A lawyer's lint is a real JSON report, not the raw-source route's 405
    // or 404: the exact `POST /app/api/drafts/validate` route was chosen.
    let report = send(
        &app,
        Method::POST,
        "/app/api/drafts/validate",
        Some(Role::Lawyer),
        true,
        Some(&invalid),
    )
    .await;
    assert_eq!(report.status, StatusCode::OK);
    let report: serde_json::Value = serde_json::from_slice(&report.body).unwrap();
    assert_eq!(report["clean"], false, "{report}");
    // The authoring command stays lawyer-tier under both names.
    for uri in ["/app/api/templates/validate", "/app/api/drafts/validate"] {
        let client = send(
            &app,
            Method::POST,
            uri,
            Some(Role::Client),
            true,
            Some(&valid),
        )
        .await;
        assert_eq!(client.status, StatusCode::FORBIDDEN, "{uri}");
        let anonymous = send(&app, Method::POST, uri, None, true, Some(&valid)).await;
        assert!(
            anonymous.status == StatusCode::UNAUTHORIZED
                || anonymous.status == StatusCode::SEE_OTHER,
            "{uri}: {}",
            anonymous.status
        );
    }
}

#[tokio::test]
async fn the_admin_alias_admits_and_refuses_exactly_as_the_canonical_listing_does() {
    let app = app().await;
    for query in ["", "?sort=title", "?sort=-code", "?sort=nonsense"] {
        for role in [
            None,
            Some(Role::Client),
            Some(Role::Clerk),
            Some(Role::Lawyer),
            Some(Role::Admin),
        ] {
            let canonical = get(&app, &format!("/app/admin/templates{query}"), role).await;
            let alias = get(&app, &format!("/app/admin/drafts{query}"), role).await;
            let who = format!("{role:?} {query}");
            if canonical.status.is_redirection() {
                // An anonymous caller is sent to sign in, then back here.
                assert_eq!(alias.status, canonical.status, "{who}");
                assert!(location(&alias).starts_with("/auth/login?return_to=/app/admin/drafts"));
            } else {
                assert_eq!(alias.status, canonical.status, "{who}: status");
                assert_same(
                    &alias,
                    &canonical,
                    &format!("{who}: the page stays canonical"),
                );
            }
        }
    }
    let lawyer = get(&app, "/app/admin/drafts", Some(Role::Lawyer)).await;
    assert_eq!(lawyer.status, StatusCode::OK);
    let client = get(&app, "/app/admin/drafts", Some(Role::Client)).await;
    assert_eq!(client.status, StatusCode::FORBIDDEN);
    let bad_sort = get(&app, "/app/admin/drafts?sort=nonsense", Some(Role::Lawyer)).await;
    assert_eq!(bad_sort.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_alias_is_answered_the_same_on_every_brand_host() {
    let app = app().await;
    let mut hosts: Vec<String> = views::brand::live_brand_hosts()
        .into_iter()
        .map(|(_, host)| host.to_string())
        .collect();
    hosts.push("unregistered.example".to_string());
    for host in hosts {
        for (alias, canonical) in [
            ("/drafts", "/templates"),
            ("/drafts/x/y?utm_source=a", "/templates/x/y?utm_source=a"),
        ] {
            let request = |uri: &str| {
                Request::builder()
                    .uri(uri)
                    .header(header::HOST, &host)
                    .body(Body::empty())
                    .unwrap()
            };
            let alias_response = app.clone().oneshot(request(alias)).await.unwrap();
            let canonical_response = app.clone().oneshot(request(canonical)).await.unwrap();
            // Whatever the host does with the canonical route (here, send an
            // anonymous caller to sign in), the alias hands off to it.
            assert_eq!(
                alias_response.status(),
                StatusCode::PERMANENT_REDIRECT,
                "{host} {alias}"
            );
            assert_eq!(
                alias_response.headers().get(header::LOCATION).unwrap(),
                canonical,
                "{host} {alias}"
            );
            assert!(
                canonical_response.status().is_redirection(),
                "{host} {canonical} answers {}",
                canonical_response.status()
            );
        }
    }
}

async fn visit_summary(
    db: &store::surreal::SurrealDb,
) -> store::visitor_analytics::VisitorAnalyticsSummary {
    // The aggregate write is fire-and-forget; give it time to land, then
    // read once more so a late stray write would show.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    store::visitor_analytics::summary(db).await.unwrap()
}

/// The actual middleware recording path, not only `should_count_route`: an
/// alias request alone records nothing, and an alias followed by the
/// canonical request records exactly one visit, on the canonical route.
#[tokio::test]
async fn an_alias_is_never_counted_and_the_canonical_request_after_it_is_counted_once() {
    let (app, db) = app_with_db().await;
    let alias = get(&app, &format!("/drafts/{FORM_990}?utm_source=x"), None).await;
    assert_eq!(alias.status, StatusCode::PERMANENT_REDIRECT);
    let none = visit_summary(&db).await;
    assert_eq!(
        none.total_visits, 0,
        "an alias alone records no visit: {none:?}"
    );

    let followed = get(&app, location(&alias), None).await;
    assert!(followed.status.is_redirection());
    let one = visit_summary(&db).await;
    assert_eq!(one.total_visits, 1, "{one:?}");
    let routes: Vec<&str> = one.routes.iter().map(|row| row.label.as_str()).collect();
    assert_eq!(routes, ["/templates/{*path}"], "{one:?}");
    assert_eq!(
        one.sources[0].label, "x",
        "attribution survives the redirect"
    );

    // The bare index behaves the same way.
    let (app, db) = app_with_db().await;
    get(&app, "/drafts", None).await;
    get(&app, "/templates", None).await;
    let index = visit_summary(&db).await;
    assert_eq!(index.total_visits, 1, "{index:?}");
    assert_eq!(index.routes[0].label, "/templates");
}
