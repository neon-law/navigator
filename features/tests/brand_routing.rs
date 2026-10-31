//! Cucumber runner for `features/brand_routing.feature`.
//!
//! Boots the real composed router against an in-memory `SurrealDB`
//! and grep-asserts the per-brand `og:site_name` on every public
//! route. Brand resolution runs as request middleware —
//! `portal::canonical_host::resolve_brand_and_enforce_host` resolves the
//! request's `views::brand::BrandKey` from its `Host:` header, and
//! `scope_branding` scopes the resolved `Branding` for the rest of the
//! request — so every scenario here (all against the firm's own host)
//! exercises the same seam a second brand host resolves through; the
//! exhaustive per-host matrix lives in
//! `server/tests/routes.rs::host_brand_path_matrix_resolves_every_combination`.
//! The footer is unified site-wide and is no longer a brand marker.

// Cucumber's step-attribute macros require `async fn`, so assertion
// steps that don't await anything still have to be declared async.
#![allow(clippy::unused_async)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cucumber::{given, then, when, World};
use features::{app_state, body_string, fs_storage};
use portal::{policy::PolicyClient, SessionStore};
use tower::ServiceExt;
use workflows::InMemoryRuntime;

#[derive(Default, World)]
#[world(init = Self::default)]
struct BrandWorld {
    app: Option<axum::Router>,
    last_status: Option<StatusCode>,
    last_location: Option<String>,
    last_body: String,
}

impl std::fmt::Debug for BrandWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrandWorld")
            .field("last_status", &self.last_status)
            .finish_non_exhaustive()
    }
}

#[given("the Neon Law Navigator public site is running")]
async fn build(world: &mut BrandWorld) {
    let runtime = Arc::new(InMemoryRuntime::new());
    let storage = fs_storage("brand").await;
    let state = app_state(
        runtime,
        storage,
        PolicyClient::passthrough(),
        None,
        SessionStore::new("test-session-key-not-for-production"),
    )
    .await;
    world.app = Some(features::neon_router(
        state,
        std::path::Path::new(portal::DEFAULT_PUBLIC_DIR),
    ));
}

#[when(regex = r"^a visitor opens (\S+)$")]
async fn visit(world: &mut BrandWorld, path: String) {
    visit_host(world, path, None).await;
}

#[when(regex = r#"^a visitor opens (.+) on host "([^"]+)"$"#)]
async fn visit_on_host(world: &mut BrandWorld, path: String, host: String) {
    visit_host(world, path, Some(host)).await;
}

async fn visit_host(world: &mut BrandWorld, path: String, host: Option<String>) {
    let mut request = Request::builder().uri(path);
    if let Some(host) = host {
        request = request.header(axum::http::header::HOST, host);
    }
    let resp = world
        .app
        .as_ref()
        .expect("app not built")
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    world.last_status = Some(resp.status());
    world.last_location = resp
        .headers()
        .get(axum::http::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    world.last_body = body_string(resp).await;
}

#[then(regex = r"^the response status is (\d+)$")]
async fn status(world: &mut BrandWorld, code: u16) {
    assert_eq!(world.last_status.expect("no status").as_u16(), code);
}

// The footer is unified site-wide now (firm-anchored copyright on every
// page), so it is no longer a per-brand marker. The reliable per-brand
// signal the layout still emits is the Open Graph `og:site_name`, set
// from the page's `SiteBrand`. The trailing quote in the needle keeps
// "Neon Law" from matching a longer site-name tag as a prefix.
/// Whether `body` declares `brand` as its `og:site_name`.
///
/// HTML attributes may arrive in either order: `property="og:site_name"
/// content="…"` or `content="…" property="og:site_name"`. A Gherkin step
/// matches raw bytes, so both spellings are accepted. Each
/// needle keeps the brand's closing quote, which is what stops "Neon Law" from
/// matching a longer site-name tag as a prefix.
fn declares_brand(body: &str, brand: &str) -> bool {
    html_forms(brand).iter().any(|brand| {
        body.contains(&format!("og:site_name\" content=\"{brand}\""))
            || body.contains(&format!("content=\"{brand}\" property=\"og:site_name\""))
    })
}

/// `text` as it may appear in rendered HTML: a brand name such as
/// "Guieb & Shook" reaches the page with its ampersand escaped, as `&amp;` or
/// Dioxus's `&#38;`.
fn html_forms(text: &str) -> [String; 3] {
    [
        text.to_string(),
        text.replace('&', "&amp;"),
        text.replace('&', "&#38;"),
    ]
}

#[then(regex = r#"^the page is branded "([^"]+)"$"#)]
async fn page_is_branded(world: &mut BrandWorld, brand: String) {
    assert!(
        declares_brand(&world.last_body, &brand),
        "expected page branded {brand:?} (og:site_name); body did not contain it",
    );
}

#[then(regex = r#"^the page is not branded "([^"]+)"$"#)]
async fn page_is_not_branded(world: &mut BrandWorld, brand: String) {
    assert!(
        !declares_brand(&world.last_body, &brand),
        "page unexpectedly branded {brand:?} (og:site_name)",
    );
}

#[then(regex = r#"^the response redirects to "([^"]+)"$"#)]
async fn redirects_to(world: &mut BrandWorld, target: String) {
    assert_eq!(
        world.last_location.as_deref(),
        Some(target.as_str()),
        "expected Location header {target:?}, got {:?}",
        world.last_location,
    );
}

/// A path with no page answers `404` and points nowhere.
///
/// The status alone is not the assertion. A `404` carrying a `Location` would
/// send a browser to a page that supposedly does not exist, since a browser
/// given both follows the header and never surfaces the status at all.
#[then("the response carries no redirect")]
async fn carries_no_redirect(world: &mut BrandWorld) {
    assert_eq!(
        world.last_location, None,
        "an unpublished path must point nowhere, got {:?}",
        world.last_location,
    );
}

#[then(regex = r#"^the response body contains "(.+)"$"#)]
async fn body_contains(world: &mut BrandWorld, needle: String) {
    // Feature files use `\"` to embed a literal double-quote; un-escape
    // so the assertion matches the actual HTML output.
    let needle = needle.replace("\\\"", "\"");
    assert!(
        html_forms(&needle)
            .iter()
            .any(|form| world.last_body.contains(form.as_str())),
        "expected response body to contain {needle:?}",
    );
}

#[then(regex = r#"^the response body does not contain "(.+)"$"#)]
async fn body_does_not_contain(world: &mut BrandWorld, needle: String) {
    assert!(
        !world.last_body.contains(&needle),
        "response body unexpectedly contained {needle:?}",
    );
}

// --- Footer: the three-line legal strip ---------------------------------

#[then(regex = r#"^the footer copyright names "(.+)"$"#)]
async fn footer_copyright_names(world: &mut BrandWorld, entity: String) {
    let body = &world.last_body;
    let start = body
        .find("site-footer__copyright")
        .expect("the page renders the footer's copyright line");
    let paragraph = &body[start..];
    let end = paragraph
        .find("</p>")
        .expect("the copyright paragraph closes");
    let paragraph = &paragraph[..end];
    assert!(
        html_forms(&entity)
            .iter()
            .any(|form| paragraph.contains(form.as_str())),
        "the footer copyright names {entity:?}: {paragraph}",
    );
}

#[then("the footer renders the attorney-advertising disclaimer")]
async fn footer_renders_disclaimer(world: &mut BrandWorld) {
    assert!(
        world.last_body.contains("site-footer__disclaimer"),
        "the footer renders the disclaimer line: {}",
        world.last_body,
    );
}

#[then(regex = r#"^the footer renders the trademark notice: (yes|no)$"#)]
async fn footer_renders_trademark(world: &mut BrandWorld, renders: String) {
    let present = world.last_body.contains("site-footer__trademark");
    assert_eq!(
        present,
        renders == "yes",
        "trademark notice presence mismatch (expected {renders})",
    );
}

/// The surfaces the simplified footer no longer renders, as the compiled
/// branding used to spell them: an office address, an association name,
/// the family-row label, a social handle, and the contact band's email.
const REMOVED_FOOTER_DETAIL: &[&str] = &[
    "site-footer__offices",
    "site-footer__contact",
    "site-footer__memberships",
    "site-footer__family",
    "site-footer__social",
    "site-footer__nav",
    "site-footer__attribution",
    "site-footer__provider",
    "1515 20th Street, Bakersfield",
    "Justice Technology Association",
    "Our Family",
    "x.com/NeonLawUSA",
    "contact@neonlaw.com",
];

#[then("the footer carries no firm detail beyond the legal strip")]
async fn footer_is_the_legal_strip_only(world: &mut BrandWorld) {
    for needle in REMOVED_FOOTER_DETAIL {
        assert!(
            !world.last_body.contains(needle),
            "the footer still renders {needle:?}",
        );
    }
}

/// The live-resolution story, end to end: an Owner moves a brand to a firm
/// whose entity no compiled fallback names, and the brand's public pages
/// resolve the new firm's entity on the next request. Seeds the same shared
/// store the router reads — a first firm wearing the brand, a second firm
/// with a fresh entity — then performs the Owner's `move_brand`. Brand
/// creation is the Admin-DRI-only act (`brands::authorize`); the move is
/// the Owner-only one (`firms::move_brand`), so the two roles both appear.
#[given(regex = r#"^the brand "([^"]+)" is moved to a firm whose entity is "([^"]+)"$"#)]
async fn move_brand_to_firm(world: &mut BrandWorld, brand_key: String, entity_name: String) {
    let surreal = features::shared_surreal().await;

    // Two firms and one brand: the brand starts wearing the first firm
    // and the Owner moves it to the second, whose entity no compiled
    // fallback names.
    let admin = store::test_support::ensure_person(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Mobility Firm Admin",
            "mobility-firm-admin@example.com",
            store::persons::Role::Admin,
        ),
    )
    .await;
    let new_entity = store::entities::create(
        &surreal,
        &store::entities::NewEntity {
            name: entity_name.clone(),
            entity_type_id: store::test_support::SEED_ENTITY_TYPE_ID,
            jurisdiction_id: store::test_support::SEED_ENTITY_JURISDICTION_ID,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .expect("the moved-to firm's entity creates");
    let prior_entity = store::entities::create(
        &surreal,
        &store::entities::NewEntity {
            name: "Prior Holder PC".to_string(),
            entity_type_id: store::test_support::SEED_ENTITY_TYPE_ID,
            jurisdiction_id: store::test_support::SEED_ENTITY_JURISDICTION_ID,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .expect("the prior firm's entity creates");
    let prior = store::firms::create(
        &surreal,
        &store::firms::NewFirm {
            name: "Prior Holder (firm)".to_string(),
            status: "active".to_string(),
            entity_id: prior_entity.id,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .expect("the prior firm creates");
    let target = store::firms::create(
        &surreal,
        &store::firms::NewFirm {
            name: format!("{entity_name} (firm)"),
            status: "active".to_string(),
            entity_id: new_entity.id,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .expect("the moved-to firm creates");

    // The brand row must exist before it can wear a firm. The shared store
    // may already hold the key — the site seed upserts every house brand —
    // so the create runs only for a brand it has never seen, wearing the
    // prior firm. That create needs the firm's own Admin DRI, which `admin`
    // is. Either way the Owner's move below re-points the brand to the
    // target firm, which is the part the footer must follow.
    if store::brands::find_by_key(&surreal, &brand_key)
        .await
        .expect("the brand row is readable")
        .is_none()
    {
        store::brands::create(
            &surreal,
            store::persons::Role::Admin,
            Some(admin.id),
            &store::brands::NewBrand {
                name: "Mobility Test Brand".to_string(),
                key: brand_key.clone(),
                firm_id: Some(prior.id),
                primary_color: None,
                accent_color: None,
                typeface: None,
            },
        )
        .await
        .expect("the brand row creates wearing the prior firm");
    }

    store::firms::move_brand(
        &surreal,
        store::persons::Role::Owner,
        None,
        &brand_key,
        target.id,
    )
    .await
    .expect("the Owner's move succeeds");
}

#[tokio::main]
async fn main() {
    BrandWorld::cucumber()
        // Every scenario composes the Dioxus-backed public router. Serializing
        // them keeps the pinned Tokio worker pools bounded on developer
        // machines, matching the CI gate's constrained execution topology.
        .max_concurrent_scenarios(1)
        .run_and_exit("tests/features/brand_routing.feature")
        .await;
}
