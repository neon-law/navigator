//! Anonymous-access contract for the mountable portal entry point.
//!
//! Every first-tranche path is classified exactly once in [`CONTRACT`], and
//! the table is the specification: a route may be a host-owned public page,
//! a protected human browser surface, a protected protocol/API surface, or
//! an explicit anonymous operational/protocol ingress — never two of those,
//! and never unclassified.
//!
//! The state these tests build carries `PolicyClient::passthrough`, so embedded Rego policy
//! allows every request. That is deliberate: it proves the boundary is a
//! property of router composition rather than of a policy bundle that has to
//! redeploy in lockstep with the binary.

use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::get,
    Router,
};
use store::test_support::mem_surreal;
use tower::ServiceExt;

/// How one path must answer an anonymous request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    /// A brand host owns this page. The mounted portal does not serve it, so
    /// an anonymous request falls through to the host — 404 in a bare mount.
    HostPublic,
    /// A shared human surface. An anonymous browser is sent through the
    /// existing login door with a `303` to `/auth/login?return_to=…`.
    ProtectedHuman,
    /// A protocol/API surface. An anonymous caller gets a parseable
    /// unauthenticated document, never an HTML login redirect.
    ProtectedProtocol,
    /// An explicit anonymous exception: operational probes, the credential
    /// handshakes, and verified webhook ingress. Protocol *discovery* is no
    /// longer in this class — the A2A agent card moved under the private
    /// `/app/api` prefix with the rest of the API surface.
    PublicIngress,
    /// A page the mounted portal serves to anyone. Distinct from
    /// [`Access::HostPublic`], which a brand host owns and the portal answers
    /// `404` for, and from [`Access::PublicIngress`], which is machine ingress
    /// rather than a page: this one must render its own `200`.
    PortalPublic,
    /// A `/drafts` twin of a `/templates` page: a permanent redirect for
    /// anyone, to the named canonical path, which then applies its own
    /// access (an anonymous browser is sent to the login door).
    AliasRedirect(&'static str),
    /// A retired path kept alive as a permanent redirect to its successor,
    /// for anyone: the old `/docs` and `/app/docs` pages all land on
    /// `/glossary`.
    RetiredRedirect,
}

/// Every first-tranche path and the single class it belongs to.
///
/// `/app/lawyer` and `/app/admin` are listed as bare roots on purpose. Their
/// descendants were already gated; the roots render their own dashboard
/// handlers, so protection there cannot be inferred from a child route. The
/// retired `/lawyer` and `/admin` roots are asserted gone further down.
///
const CONTRACT: &[(&str, Access)] = &[
    // Human-facing Navigator application surfaces.
    ("/app/projects", Access::ProtectedHuman),
    // The retired Portal mount. An anonymous browser goes through the same
    // login door as every other `/app` page; an authenticated caller is
    // redirected to the matter show page, asserted in
    // `portal/tests/project_portal_route.rs`.
    ("/app/projects/some-code/portal", Access::ProtectedHuman),
    ("/app/lawyer", Access::ProtectedHuman),
    ("/app/admin", Access::ProtectedHuman),
    // The Owner/Admin matter directory (ENG-221). Listed beside the desk root
    // rather than inferred from it: which authenticated tiers reach it is the
    // Rego suite's assertion, and this one is that an anonymous browser never
    // gets that far.
    ("/app/admin/projects", Access::ProtectedHuman),
    // The blank government-forms index, migrated off the retired `/portal`
    // subtree onto `/app/forms`. Any authenticated person may browse it; an
    // anonymous browser goes through the login door like every `/app` page.
    ("/app/forms", Access::ProtectedHuman),
    // The documentation is the glossary: one anonymous page. The source is
    // public, so its ontology is the manual for software anyone can clone.
    ("/glossary", Access::PortalPublic),
    // Every retired documentation path — the public catalog, its per-guide
    // pages, and the signed-in copy — is a permanent redirect to it.
    ("/docs", Access::RetiredRedirect),
    ("/docs/glossary", Access::RetiredRedirect),
    ("/docs/notation", Access::RetiredRedirect),
    ("/app/docs", Access::RetiredRedirect),
    ("/app/docs/glossary", Access::RetiredRedirect),
    // The operational probes: anonymous, with no session requirement. They
    // are the only paths Kubernetes dials, and they answer on `/app` alone.
    ("/app/health", Access::PublicIngress),
    (
        "/webhook/journal/xero/00000000-0000-0000-0000-000000000001",
        Access::PublicIngress,
    ),
    ("/app/readyz", Access::PublicIngress),
    // The living design system reads anonymously: it is a contributor
    // reference, so it renders for a reader who has no account rather than
    // sending them through the login door.
    ("/design", Access::PortalPublic),
    ("/templates", Access::ProtectedHuman),
    ("/drafts", Access::AliasRedirect("/templates")),
    (
        "/drafts/notations/forms/united-states/federal/irs/us--form-990",
        Access::AliasRedirect("/templates/notations/forms/united-states/federal/irs/us--form-990"),
    ),
    ("/app/admin/drafts", Access::ProtectedHuman),
    (
        "/templates/notations/forms/united-states/federal/irs/us--form-990",
        Access::ProtectedHuman,
    ),
    // The Swagger UI shell at the `/app/api` root, its shorter public-footer
    // alias `/api`, and the OpenAPI document beside them are the API's
    // documentation rather than the API: they carry no session boundary and
    // no policy at all, so an anonymous reader gets the real page and the
    // real document, not a login door or a refusal.
    ("/app/api", Access::PortalPublic),
    ("/api", Access::PortalPublic),
    ("/app/api/openapi.json", Access::PortalPublic),
    // Machine surfaces answer machines, even when refusing. Unlike the
    // documentation above, these are the operations it describes, and they
    // stay gated.
    ("/app/api/people", Access::ProtectedProtocol),
    (
        "/app/api/templates/neon-law/shared/retainer",
        Access::ProtectedProtocol,
    ),
    (
        "/app/api/drafts/neon-law/shared/retainer",
        Access::ProtectedProtocol,
    ),
    // The A2A agent card. It moved off the anonymous allowlist when the API
    // surface consolidated under the private `/app/api` prefix: a client
    // now needs a session to read the card, so A2A discovery is not
    // self-service. See `portal::a2a` for why that is the accepted trade.
    ("/app/api/mcp.json", Access::ProtectedProtocol),
    // `/app/mcp` is deliberately absent from this table: it carries a
    // Bearer-only `require_auth` stack (no session cookie), which
    // answers a bare `401` rather than the structured
    // `{"error":"unauthenticated"}` body `Access::ProtectedProtocol` asserts
    // for the session-boundary surfaces above. See
    // `server/tests/mcp_embedded.rs` for its own anonymous-access coverage.
    // Host-owned public pages. Legal and crawler documents belong to the
    // brand host that publishes them, not to the shared application.
    ("/privacy", Access::HostPublic),
    ("/terms", Access::HostPublic),
    ("/robots.txt", Access::HostPublic),
    ("/sitemap.xml", Access::HostPublic),
    ("/llms.txt", Access::HostPublic),
    ("/blog", Access::HostPublic),
    ("/notations", Access::HostPublic),
    ("/start/llc-file", Access::HostPublic),
    // The explicit anonymous allowlist.
    ("/version", Access::PublicIngress),
    ("/auth/login", Access::PublicIngress),
    ("/auth/callback", Access::PublicIngress),
    ("/auth/logout", Access::PublicIngress),
    ("/auth/cli/start", Access::PublicIngress),
    ("/auth/ci/seed-token", Access::PublicIngress),
    ("/docusign/consent-callback", Access::PublicIngress),
    ("/social/oauth/callback", Access::PublicIngress),
    ("/webhook/meta", Access::PublicIngress),
    ("/webhook/tiktok", Access::PublicIngress),
    ("/assets/img/router-contract.svg", Access::PublicIngress),
];

/// An [`AppState`](portal::AppState) with a configured
/// OAuth door, so the login handshakes this contract pins actually mount.
async fn contract_state() -> portal::AppState {
    let mut state = portal::test_support::app_state(mem_surreal().await).await;
    state.oauth = Some(portal::OAuthConfig::new(
        "navigator",
        "secret",
        "http://localhost:3001/auth/callback",
        "https://rauthy.example/auth/v1/oidc/authorize",
        "https://rauthy.example/auth/v1/oidc/token",
    ));
    state
        .assets_storage
        .put(
            "img/router-contract.svg",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
            "image/svg+xml",
        )
        .await
        .unwrap();
    state
}

async fn anonymous_get(app: &Router, path: &str) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// The whole table, asserted against the real mountable router.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn every_first_tranche_path_answers_its_declared_anonymous_contract() {
    let app = portal::router(contract_state().await);

    for (path, access) in CONTRACT {
        let response = anonymous_get(&app, path).await;
        let status = response.status();
        match access {
            Access::HostPublic => assert_eq!(
                status,
                StatusCode::NOT_FOUND,
                "{path} is host-owned; the mounted portal must not serve it"
            ),
            Access::ProtectedHuman => {
                assert_eq!(
                    status,
                    StatusCode::SEE_OTHER,
                    "{path} must send an anonymous browser to the login door"
                );
                let location = response
                    .headers()
                    .get(axum::http::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    location.starts_with("/auth/login?return_to="),
                    "{path} must redirect through /auth/login, got {location}"
                );
            }
            Access::ProtectedProtocol => {
                assert_eq!(
                    status,
                    StatusCode::UNAUTHORIZED,
                    "{path} must refuse a machine caller with a status, not a redirect"
                );
                let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap();
                let document: serde_json::Value = serde_json::from_slice(&body)
                    .unwrap_or_else(|e| panic!("{path} must answer with JSON: {e}"));
                assert_eq!(
                    document.get("error").and_then(serde_json::Value::as_str),
                    Some("unauthenticated"),
                    "{path} must keep the structured unauthenticated shape"
                );
            }
            Access::PublicIngress => {
                assert_ne!(
                    status,
                    StatusCode::NOT_FOUND,
                    "{path} is an explicit anonymous exception and must stay mounted"
                );
                assert_ne!(
                    status,
                    StatusCode::UNAUTHORIZED,
                    "{path} must stay reachable without a session"
                );
                let location = response
                    .headers()
                    .get(axum::http::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default();
                assert!(
                    !location.starts_with("/auth/login"),
                    "{path} must not be bounced to the login door"
                );
            }
            Access::AliasRedirect(target) => {
                assert_eq!(
                    status,
                    StatusCode::PERMANENT_REDIRECT,
                    "{path} is an alias and must redirect permanently"
                );
                let location = response
                    .headers()
                    .get(axum::http::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default();
                assert_eq!(location, *target, "{path} must land on its canonical route");
            }
            Access::RetiredRedirect => {
                assert_eq!(
                    status,
                    StatusCode::PERMANENT_REDIRECT,
                    "{path} is retired and must redirect permanently"
                );
                let location = response
                    .headers()
                    .get(axum::http::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default();
                assert_eq!(location, "/glossary", "{path} must land on the glossary");
            }
            Access::PortalPublic => {
                assert_eq!(
                    status,
                    StatusCode::OK,
                    "{path} is a public page and must render for an anonymous reader"
                );
                let location = response
                    .headers()
                    .get(axum::http::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default();
                assert!(
                    !location.starts_with("/auth/login"),
                    "{path} must not be bounced to the login door"
                );
            }
        }
    }
}

/// `/github-stars` is not routed, and no host reserves a prefix for it.
///
/// The endpoint proxied a star count from
/// `api.github.com/repos/neon-law-source-code/navigator`. It was deleted while
/// that repository was private and the upstream answered `404`; the repository
/// is public again, so the lane could be made to work, and that is exactly why
/// the absence is pinned rather than assumed. Re-adding the route puts an
/// anonymous ingress back on the boundary this file specifies, which is a
/// decision to argue for and not a repair.
#[tokio::test]
async fn github_stars_is_not_routed() {
    let app = portal::router(contract_state().await);

    assert_eq!(
        anonymous_get(&app, "/github-stars").await.status(),
        StatusCode::NOT_FOUND,
        "/github-stars was deleted; the portal must not serve it"
    );
    assert!(
        !portal::RESERVED_PATH_PREFIXES.contains(&"/github-stars"),
        "a deleted route must not keep a reserved host prefix"
    );
}

/// The dedicated `/lawyer` prefix is gone. Workbench pages live under
/// `/app/lawyer`, and a leftover reserved prefix would keep a brand host
/// from publishing that name after nothing mounts there.
#[tokio::test]
async fn the_retired_lawyer_prefix_is_not_served() {
    let app = portal::router(contract_state().await);

    for path in ["/lawyer", "/lawyer/notations", "/lawyer/retainers/new"] {
        assert_eq!(
            anonymous_get(&app, path).await.status(),
            StatusCode::NOT_FOUND,
            "{path} must not keep a mount after the prefix retired"
        );
    }
    assert!(
        !portal::RESERVED_PATH_PREFIXES.contains(&"/lawyer"),
        "a retired prefix must not stay reserved against a host"
    );
}

/// The bundled Harvard-outline recording stage is deprecated. It read no
/// store and served no matter — narrating a notation now lives only bound to
/// a real matter, at `/app/projects/{code}/{id}/outline`
/// ([`portal::dioxus_app::NOTATION_OUTLINE_PATH`]).
#[tokio::test]
async fn the_bundled_outline_stage_is_not_routed() {
    let app = portal::router(contract_state().await);

    assert_eq!(
        anonymous_get(&app, "/app/outline").await.status(),
        StatusCode::NOT_FOUND,
        "/app/outline was deprecated; the portal must not serve it"
    );
}

/// The probes and the MCP JSON-RPC endpoint answer under `/app` and nowhere
/// else. Each was mounted twice while infrastructure moved onto the `/app`
/// path; the bare mount is gone, and a prefix nothing serves must not stay
/// reserved against a brand host that wants to publish that name.
#[tokio::test]
async fn the_pre_app_probe_and_mcp_paths_are_not_served() {
    let app = portal::router(contract_state().await);

    for path in ["/health", "/readyz", "/mcp"] {
        assert_eq!(
            anonymous_get(&app, path).await.status(),
            StatusCode::NOT_FOUND,
            "{path} must not keep a mount once the surface moved under /app"
        );
        assert!(
            !portal::RESERVED_PATH_PREFIXES.contains(&path),
            "{path} must not stay reserved against a host once nothing mounts there"
        );
    }
}

#[tokio::test]
async fn private_bucket_assets_are_served_anonymously_with_bounded_caching() {
    let app = portal::router(contract_state().await);

    let response = anonymous_get(&app, "/assets/img/router-contract.svg").await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(axum::http::header::CONTENT_TYPE),
        Some(&axum::http::HeaderValue::from_static("image/svg+xml"))
    );
    assert_eq!(
        response.headers().get(axum::http::header::CACHE_CONTROL),
        Some(&axum::http::HeaderValue::from_static(
            "public, max-age=3600"
        ))
    );
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::X_CONTENT_TYPE_OPTIONS),
        Some(&axum::http::HeaderValue::from_static("nosniff"))
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(body, &b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"[..]);
}

#[tokio::test]
async fn private_bucket_asset_proxy_hides_missing_and_unsafe_keys() {
    let app = portal::router(contract_state().await);

    for path in [
        "/assets/img/missing.svg",
        "/assets/%2e%2e/docs/private.pdf",
        "/assets/img%5cprivate.pdf",
    ] {
        assert_eq!(
            anonymous_get(&app, path).await.status(),
            StatusCode::NOT_FOUND,
            "{path} must not expose an object"
        );
    }
}

/// Signature- and secret-verified webhook ingress stays anonymous: the
/// provider posting to it holds a credential, not a session.
#[tokio::test]
async fn verified_webhook_ingress_stays_anonymous() {
    let app = portal::router(contract_state().await);

    for path in [
        "/webhook/sms/inbound/dev-secret",
        "/webhook/sms/status/dev-secret",
        "/webhook/sendgrid/inbound/dev-secret",
        "/webhook/email-events/dev-secret",
        "/webhook/esignature/dev-secret",
        "/webhook/meta",
        "/webhook/tiktok",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{path} must stay mounted for its verified sender"
        );
        assert_ne!(
            response.status(),
            StatusCode::SEE_OTHER,
            "{path} must never bounce a webhook sender to a login page"
        );
    }
}

/// The GitHub webhook receiver lives in `workflows-service` on the public
/// `workflows` host because `www` is behind the tailnet perimeter, and GitHub —
/// which cannot join the tailnet — reaches it there. `web`
/// must 404 the path rather than accept a signed delivery meant for the worker.
#[tokio::test]
async fn web_does_not_serve_the_github_webhook_receiver() {
    let app = portal::router(contract_state().await);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/github/dev-secret")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "web must not mount the GitHub webhook receiver"
    );
}

/// A signed session crosses the boundary and reaches the handler — the
/// boundary refuses anonymity, it does not refuse everyone.
#[tokio::test]
async fn a_signed_session_passes_the_shared_boundary() {
    let app = portal::router(contract_state().await);
    let app_for_gallery = app.clone();
    let sessions = portal::SessionStore::new(portal::test_support::TEST_SESSION_KEY);
    let cookie = format!(
        "{}={}",
        portal::session::SESSION_COOKIE_NAME,
        sessions.encode(&portal::SessionData::fresh(
            "contract-lawyer",
            store::persons::Role::Lawyer,
        ))
    );
    let gallery_cookie = cookie.clone();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/glossary")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "an authenticated reader still gets the shared glossary"
    );
    // `/glossary` is anonymous, so it cannot prove the boundary passes a
    // signed session; a gated surface does. `/templates` is behind the same
    // boundary and renders for any authenticated person.
    let gallery = app_for_gallery
        .oneshot(
            Request::builder()
                .uri("/templates")
                .header("cookie", gallery_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        gallery.status(),
        StatusCode::OK,
        "a signed session passes the boundary onto a gated shared surface"
    );
}

#[tokio::test]
async fn mount_rejects_a_host_route_that_would_shadow_the_app() {
    let state = portal::test_support::app_state(mem_surreal().await).await;
    let host = Router::new().route("/app", get(|| async { StatusCode::IM_A_TEAPOT }));

    let error = portal::mount(state, host, &["/app"]).unwrap_err();

    assert_eq!(
        error.to_string(),
        "host route /app is reserved for the Navigator portal"
    );
}

/// Every reserved prefix — not just `/app` — is refused, and both the
/// bare prefix and a descendant of it collide.
#[tokio::test]
async fn mount_rejects_every_portal_owned_prefix() {
    // One migrated schema for the whole table: `mount` rejects before it
    // touches the database, and building a state is cheap once the schema
    // exists.
    let surreal = mem_surreal().await;
    for prefix in portal::RESERVED_PATH_PREFIXES {
        for host_path in [(*prefix).to_string(), format!("{prefix}/anything")] {
            let state = portal::test_support::app_state(surreal.clone()).await;
            let host = Router::new().route(&host_path, get(|| async { StatusCode::IM_A_TEAPOT }));
            let error = portal::mount(state, host, &[&host_path])
                .expect_err("a host may not claim a portal-owned path");
            assert_eq!(
                error.to_string(),
                format!("host route {host_path} is reserved for the Navigator portal")
            );
        }
    }
}

#[tokio::test]
async fn mount_keeps_host_public_routes_and_protects_portal_routes() {
    let host = Router::new().route("/host-page", get(|| async { StatusCode::IM_A_TEAPOT }));
    let app = portal::mount(contract_state().await, host, &["/host-page"])
        .expect("host route is not reserved");

    let host_response = anonymous_get(&app, "/host-page").await;
    assert_eq!(
        host_response.status(),
        StatusCode::IM_A_TEAPOT,
        "the host keeps serving its own public page"
    );

    // `/templates`, not `/docs`: the documentation reads anonymously, so it
    // cannot stand for "the boundary still closes under a host mount".
    // The template gallery is the nearest shared surface that is still gated.
    let gallery_response = anonymous_get(&app, "/templates").await;
    assert_eq!(
        gallery_response.status(),
        StatusCode::SEE_OTHER,
        "a shared Navigator tool stays behind the session boundary under a host mount"
    );
}

/// The reusable host bootstrap mounts a host's public `Router<AppState>` while
/// the shared session boundary still closes every human and protocol surface.
///
/// This is #730's "hosts publish; portal authenticates" invariant expressed as
/// one composition seam rather than a `portal_only` boolean: a host binary hands
/// [`portal::bootstrap`] its own public pages, and the anonymous-access matrix
/// #732 pinned holds regardless of what the host publishes.
#[tokio::test]
async fn bootstrap_mounts_host_public_routes_behind_the_shared_boundary() {
    use axum::extract::State;
    // A host-supplied public page that reads `AppState`, exactly as the firm's
    // marketing handlers do — proving the seam shares the application state,
    // not just stateless routes the way `mount` does.
    async fn host_home(State(_state): State<portal::AppState>) -> &'static str {
        "host home"
    }
    let host_public = Router::new().route("/host-home", get(host_home));
    let app = portal::bootstrap(
        contract_state().await,
        std::path::Path::new(portal::DEFAULT_PUBLIC_DIR),
        host_public,
        &["/host-home"],
        Vec::new(),
    )
    .expect("the host route does not collide with Navigator");

    let home = anonymous_get(&app, "/host-home").await;
    assert_eq!(
        home.status(),
        StatusCode::OK,
        "the host's own public page is served to an anonymous visitor"
    );

    let lawyer = anonymous_get(&app, "/app/lawyer").await;
    assert_eq!(
        lawyer.status(),
        StatusCode::SEE_OTHER,
        "the shared boundary still sends an anonymous browser to the login door"
    );

    let api = anonymous_get(&app, "/app/api/people").await;
    assert_eq!(
        api.status(),
        StatusCode::UNAUTHORIZED,
        "the shared boundary still refuses an anonymous machine caller with a status"
    );
}

/// Appending a trailing slash to a first-tranche path must not orphan an old
/// link: today it falls through to the 404 fallback as if the path had never
/// been registered, so an inbound link with a stray slash simply breaks. The
/// router must instead redirect it back to the exact registered path.
///
/// The retired Portal subtree is exempt, because it redirects every path —
/// slashed or not — straight to the matter show page in one hop, asserted
/// separately in
/// `project_portal_route.rs::every_retired_portal_path_redirects_to_the_matter_show_page`.
#[tokio::test]
async fn a_trailing_slash_redirects_to_the_registered_path() {
    let app = portal::router(contract_state().await);

    for (path, _access) in CONTRACT {
        if path.contains("/portal") {
            continue;
        }
        let slashed = format!("{path}/");
        let response = anonymous_get(&app, &slashed).await;
        assert_eq!(
            response.status(),
            StatusCode::MOVED_PERMANENTLY,
            "{slashed} must redirect to {path}, not fall through to the fallback"
        );
        let location = response
            .headers()
            .get(axum::http::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert_eq!(
            location, *path,
            "{slashed} must redirect to the exact registered path"
        );
    }
}

/// The redirect is a link-safety net for `GET`/`HEAD` navigation, not a
/// general path rewrite: no route in this router registers a `POST` path
/// ending in `/`, so a `POST` with a stray trailing slash must keep 404ing
/// exactly as it did before, rather than being redirected somewhere a client
/// would silently retry as a `GET`.
#[tokio::test]
async fn a_trailing_slash_is_not_redirected_for_a_post() {
    let app = portal::router(contract_state().await);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "a POST with a trailing slash must not be redirected"
    );
}

/// A query string on the slashed request survives onto the redirect target.
#[tokio::test]
async fn a_trailing_slash_redirect_preserves_the_query_string() {
    let app = portal::router(contract_state().await);

    let response = anonymous_get(&app, "/app/projects/?sort=name").await;

    assert_eq!(response.status(), StatusCode::MOVED_PERMANENTLY);
    let location = response
        .headers()
        .get(axum::http::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert_eq!(location, "/app/projects?sort=name");
}

/// The production brand composition path enforces the same reserved-prefix
/// contract as the standalone [`portal::mount`] seam. A brand declaration is
/// rejected before Axum can merge it, so host order can never shadow a shared
/// application route or turn a collision into a boot-time panic.
#[tokio::test]
async fn bootstrap_rejects_every_navigator_owned_prefix() {
    let surreal = mem_surreal().await;
    for prefix in portal::RESERVED_PATH_PREFIXES {
        for host_path in [(*prefix).to_string(), format!("{prefix}/anything")] {
            let state = portal::test_support::app_state(surreal.clone()).await;
            let host = Router::new().route(&host_path, get(|| async { StatusCode::IM_A_TEAPOT }));
            let error = portal::bootstrap(
                state,
                std::path::Path::new(portal::DEFAULT_PUBLIC_DIR),
                host,
                &[&host_path],
                Vec::new(),
            )
            .expect_err("a brand host may not claim a Navigator-owned path");
            assert_eq!(
                error.to_string(),
                format!("host route {host_path} is reserved for the Navigator portal")
            );
        }
    }
}

/// The lawyer-tier Clauses pages answer by tier through the embedded Rego policy: the
/// lawyer tier reads, a Client and a Clerk are refused with `403`.
mod clauses_page {
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use std::fmt::Write as _;
    use store::persons::Role;
    use store::test_support::mem_surreal;
    use tower::ServiceExt;

    /// A shared Template whose compiled body pins `sources`.
    async fn seed_template(state: &portal::AppState, code: &str, title: &str, sources: &[&str]) {
        let mut pins = String::new();
        for (index, source) in sources.iter().enumerate() {
            let _ = writeln!(pins, "  c{index}:\n    source: {source}");
        }
        let body = format!("---\nkind: agreement\nclause_sources:\n{pins}---\nBody of {code}.\n");
        let asset_id = store::assets::ingest_content(
            &state.surreal,
            &state.storage,
            body.as_bytes(),
            "text/markdown",
        )
        .await
        .unwrap();
        store::templates::save_version(
            &state.surreal,
            None,
            code,
            store::templates::Version {
                title: title.into(),
                respondent_type: "entity".into(),
                asset_id: Some(asset_id),
                form_code: None,
                kind: Some("agreement".into()),
                source_commit_sha: None,
            },
        )
        .await
        .unwrap();
    }

    async fn app() -> axum::Router {
        let mut state = portal::test_support::app_state(mem_surreal().await).await;
        state.policy = portal::policy::PolicyClient::embedded().expect("embedded policy compiles");
        seed_template(
            &state,
            "sample__services",
            "Sample Services Agreement",
            &["governing_law@1", "dispute_resolution@1"],
        )
        .await;
        seed_template(
            &state,
            "sample__license",
            "Sample License Agreement",
            &["governing_law@1"],
        )
        .await;
        portal::router(state)
    }

    async fn get(app: &axum::Router, role: Role, uri: &str) -> (StatusCode, String) {
        let sessions = portal::SessionStore::new(portal::test_support::TEST_SESSION_KEY);
        let cookie = format!(
            "{}={}",
            portal::session::SESSION_COOKIE_NAME,
            sessions.encode(&portal::SessionData::fresh("clauses-sub", role))
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    #[tokio::test]
    async fn the_lawyer_tier_reads_and_everyone_else_is_refused() {
        let app = app().await;
        for uri in ["/app/admin/clauses", "/app/admin/clauses/governing_law"] {
            for role in [Role::Owner, Role::Admin, Role::Lawyer] {
                let (status, _) = get(&app, role, uri).await;
                assert_eq!(status, StatusCode::OK, "{role:?} {uri}");
            }
            for role in [Role::Clerk, Role::Client] {
                let (status, body) = get(&app, role, uri).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{role:?} {uri}");
                assert!(!body.contains("governing_law"), "{role:?} {uri}");
            }
        }
    }

    #[tokio::test]
    async fn the_list_shows_every_clause_code_with_its_usage_and_retirement() {
        let app = app().await;
        let (status, body) = get(&app, Role::Lawyer, "/app/admin/clauses").await;
        assert_eq!(status, StatusCode::OK);
        for current in rules::clauses::library_clauses().unwrap() {
            assert!(body.contains(&current.code), "{} is listed", current.code);
        }
        // The superseded version is not a row of its own.
        assert_eq!(
            body.matches("/app/admin/clauses/nevada_engagement_arbitration\"")
                .count(),
            1
        );
        let row = |code: &str| {
            let start = body.find(&format!("/app/admin/clauses/{code}\"")).unwrap();
            let end = body[start..].find("</tr>").unwrap() + start;
            body[start..end].to_string()
        };
        assert!(row("governing_law").contains("2 Templates"));
        assert!(row("governing_law").contains("(retired)"));
        assert!(!row("nevada_engagement_arbitration").contains("(retired)"));
        assert!(row("dispute_resolution").contains("1 Template<"));
        assert!(row("nda_administration").contains("No Templates"));
        assert!(body.contains(r#"role="search""#));
        // Read-only: no form but the search.
        assert_eq!(body.matches("<form").count(), 1);
    }

    #[tokio::test]
    async fn search_narrows_by_code_kind_and_text() {
        let app = app().await;
        let (_, by_code) = get(&app, Role::Lawyer, "/app/admin/clauses?q=GOVERNING_law").await;
        assert!(by_code.contains("/app/admin/clauses/governing_law\""));
        assert!(!by_code.contains("/app/admin/clauses/dispute_resolution\""));

        let (_, by_kind) = get(&app, Role::Lawyer, "/app/admin/clauses?kind=onboarding").await;
        assert!(by_kind.contains("/app/admin/clauses/engagement_arbitration\""));
        assert!(!by_kind.contains("/app/admin/clauses/governing_law\""));

        let (_, by_text) = get(&app, Role::Lawyer, "/app/admin/clauses?q=entire+agreement").await;
        assert!(by_text.contains("/app/admin/clauses/nda_administration\""));
        assert!(!by_text.contains("/app/admin/clauses/governing_law\""));

        let (status, none) = get(&app, Role::Lawyer, "/app/admin/clauses?q=zzzzzz").await;
        assert_eq!(status, StatusCode::OK);
        assert!(none.contains("No Clauses match."));
    }

    #[tokio::test]
    async fn the_detail_shows_body_inputs_and_the_pinning_templates() {
        let app = app().await;
        let (status, body) = get(&app, Role::Lawyer, "/app/admin/clauses/governing_law").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("governing_law@1"));
        assert!(body.contains("(retired)"));
        assert!(body.contains("new references are refused"));
        assert!(body.contains("Governing law:"));
        assert!(body.contains("jurisdiction"));
        assert!(body.contains("Sample Services Agreement"));
        assert!(body.contains("sample__license"));

        let (_, unpinned) = get(&app, Role::Lawyer, "/app/admin/clauses/nda_administration").await;
        assert!(unpinned.contains("No Template pins this version."));

        let (_, versions) = get(
            &app,
            Role::Lawyer,
            "/app/admin/clauses/nevada_engagement_arbitration",
        )
        .await;
        assert!(versions.contains("nevada_engagement_arbitration@2"));
        assert!(versions.contains("nevada_engagement_arbitration@1"));

        let (status, _) = get(&app, Role::Lawyer, "/app/admin/clauses/no_such_clause").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// The canonical seed stores a shared Template's body without its
    /// frontmatter, so its `clause_sources:` are not in the stored bytes. The
    /// shipped Templates still count as pinning the Clauses they compile in.
    #[tokio::test]
    async fn the_canonically_seeded_templates_count_as_pinning_their_clauses() {
        let mut state = portal::test_support::app_state(mem_surreal().await).await;
        state.policy = portal::policy::PolicyClient::embedded().expect("embedded policy compiles");
        store::seed::seed_canonical(&state.surreal, &state.storage)
            .await
            .expect("canonical seed");
        let app = portal::router(state);

        let (status, list) = get(&app, Role::Lawyer, "/app/admin/clauses").await;
        assert_eq!(status, StatusCode::OK);
        let start = list
            .find("/app/admin/clauses/dispute_resolution\"")
            .unwrap();
        let end = list[start..].find("</tr>").unwrap() + start;
        assert!(
            list[start..end].contains("4 Templates"),
            "{}",
            &list[start..end]
        );

        let (_, detail) = get(&app, Role::Lawyer, "/app/admin/clauses/dispute_resolution").await;
        for code in [
            "advisor_agreement",
            "master_services_agreement",
            "mutual_nda",
            "one_way_nda",
        ] {
            assert!(detail.contains(&format!("({code})")), "{code} pins it");
        }
    }
}
