#![allow(clippy::doc_markdown)]
//! Router tests for the web crate.
//!
//! Drives the router via `tower::ServiceExt::oneshot` — no socket,
//! no port binding, no flakiness around chosen ephemeral ports. Each
//! test gets its own embedded, memory-backed store via
//! `store::surreal::test_support::mem` so they don't share state.

#[path = "common/intake_stack.rs"]
mod intake_stack;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use portal::{AppState, CanonicalHost};

use tower::ServiceExt;

/// One repo root for this test binary so `/app/readyz` can construct
/// [`repos::RepoStore`]. `NAVIGATOR_GIT_REPO_ROOT` is process-global; a
/// single stable directory avoids racing parallel tests that only need
/// the env to be set, not a unique tree.
static REPO_ROOT: std::sync::LazyLock<tempfile::TempDir> = std::sync::LazyLock::new(|| {
    let dir = tempfile::tempdir().expect("routes test repo root");
    std::env::set_var(repos::REPO_ROOT_ENV, dir.path());
    dir
});

fn ensure_repo_root() {
    let _ = &*REPO_ROOT;
}

#[path = "common/bearer.rs"]
mod bearer_common;

async fn body_string(resp: axum::http::Response<Body>) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

// ---- PATCH /app/api/projects/{id} (descriptive update) ----

// ---- DELETE /app/api/projects/{id} (matter delete) ----

// ---- POST /app/api/projects/{id}/lifecycle (direct transition, distinct from /close) ----

// ---- POST /app/api/projects (matter open) ----

// QUARANTINED, not retired. This test failed once in CI's `cargo test
// --workspace` run (336 passed, 1 failed) on the LAST assertion —
// `/app/projects/{project_code}` returned 500 where 200 was expected. The three client
// assertions above it, which are the ones carrying #782's confidentiality
// promise, passed on that same run: the client did not see the memo. The
// observed failure is lawyer losing their view, not privileged material
// leaking.
//
// Why it is ignored rather than fixed: every read on that handler is
// `.map_err(server_error)?`, and `server_error` commits a 500 by design —
// "a read failure is a 500, never a silently-blank DRI on an accountability
// surface". Making this green would mean swallowing a failed read and
// rendering a blank DRI, which is the exact behaviour that comment forbids.
// There is no application fix here until the underlying `String` is known, and
// it has not been reproduced.
//
// What still guards #782 while this sleeps: `server/tests/project_documents_acl.rs`
// — `client_download_of_an_internal_document_is_404`,
// `client_detail_of_an_internal_document_does_not_leak_it`, and
// `lawyer_download_of_an_internal_document_succeeds`. Those live in a separate
// test binary, so they run in their own process and are not exposed to the
// in-process contention in this file (337 tests, one process, one shared
// store) that is the leading suspect here.
//
// To retire the quarantine: reproduce the 500, identify the `String`, fix that,
// and delete this attribute — do not delete the test.

/// A family can send its intake before the site asks it to register or book.
#[tokio::test]
async fn iep_intake_collects_answers_before_offering_signup() {
    ensure_repo_root();
    let state = AppState {
        canonical_host: CanonicalHost::new(None),
        ..portal::test_support::app_state(
            store::application_store::ApplicationStore::uninitialized(),
        )
        .await
    };
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    let contact = get_on_host(&app, "/contact", "iep.neonlaw.com").await;
    assert_eq!(contact.status(), StatusCode::OK);
    let html = body_string(contact).await;
    for field in [
        "district",
        "relationship",
        "concern",
        "next_date",
        "email",
        "_csrf",
    ] {
        assert!(html.contains(&format!(r#"name="{field}""#)), "{field}");
    }
    assert!(html.contains(r#"action="/leads""#));
    assert!(!html.contains("or15n4yy7"));
    let next = get_on_host(&app, "/contact/sent", "iep.neonlaw.com").await;
    assert_eq!(next.status(), StatusCode::OK);
    let html = body_string(next).await;
    assert!(html.contains(r#"href="/auth/signup""#));
    assert!(html.contains("calendar.notion.com/meet/nick-shook/or15n4yy7"));
}

/// Issue `GET path` against `app` with the given `Host:` header.
async fn get_on_host(app: &axum::Router, path: &str, host: &str) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header(header::HOST, host)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

/// ENG-303: every listing in `webapp::admin_listings` is classified exactly
/// once in `webapp::admin_listing::LAWYER_LISTINGS`.
///
/// This is what keeps the seam a seam. A new `listing_router!` mount starts
/// with a new `#[server] pub async fn list_*` in that module, and this test
/// fails until someone has decided whether it discloses matter content — so
/// another unscoped matter-content listing cannot arrive by omission.
#[test]
fn every_admin_listing_is_classified_exactly_once() {
    let source = include_str!("../../webapp/src/admin_listings.rs");
    let declared: Vec<&str> = source
        .lines()
        .filter_map(|line| line.trim().strip_prefix("pub async fn list_"))
        .filter_map(|rest| rest.split('(').next())
        .map(str::trim)
        .collect();
    assert!(
        declared.len() >= 15,
        "expected to find the listing server functions by source scan, found {declared:?}",
    );

    let classified: std::collections::HashSet<&str> = webapp::admin_listing::LAWYER_LISTINGS
        .iter()
        .map(|(name, _, _)| *name)
        .collect();
    assert_eq!(
        classified.len(),
        webapp::admin_listing::LAWYER_LISTINGS.len(),
        "a listing is classified more than once",
    );
    for name in &declared {
        let full = format!("list_{name}");
        assert!(
            classified.contains(full.as_str()),
            "`{full}` is a lawyer listing with no entry in \
             `webapp::admin_listing::LAWYER_LISTINGS`. Decide what it discloses: \
             `Reference`, `MatterContent` (scope it through \
             `require_lawyer_in_matters`), or `ConflictGraph` (firm-wide, Model \
             Rule 1.10).",
        );
    }
    for (name, _, _) in webapp::admin_listing::LAWYER_LISTINGS {
        let bare = name.strip_prefix("list_").unwrap_or(name);
        assert!(
            declared.contains(&bare),
            "`{name}` is classified but no longer exists in `webapp::admin_listings`",
        );
    }
}

// ---------- Error pages: HTML for browsers, JSON for /app/api & /app/mcp ----------

#[tokio::test]
async fn wants_json_path_classifier() {
    // The classifier is the single source of truth for HTML-vs-JSON
    // routing in error responses — lock its behavior down so a future
    // route addition can't silently start handing HTML to a JSON
    // client.
    assert!(portal::wants_json("/app/api/people"));
    assert!(portal::wants_json("/app/api/people/123"));
    assert!(portal::wants_json("/app/mcp"));
    assert!(portal::wants_json("/app/mcp/foo"));
    assert!(portal::wants_json("/app/api/openapi.json"));
    assert!(!portal::wants_json("/"));
    // The pre-`/app` MCP path is not served, so it is a page 404 like any
    // other unrouted path rather than a JSON surface.
    assert!(!portal::wants_json("/mcp"));
    assert!(!portal::wants_json("/mcp/foo"));
    assert!(!portal::wants_json("/app/lawyer"));
    assert!(!portal::wants_json("/app/lawyer/people"));
    assert!(!portal::wants_json("/blog/anything"));
    // `/api-something` (no trailing slash, no exact match) is NOT
    // an api route — leading-substring matches would catch real
    // page paths like `/apidocs` if someone added one.
    assert!(!portal::wants_json("/apidocs"));
}

// ---------- Admin role editing ----------

// ---------- Uniqueness conflicts → 409 + delete guard ----------

// ---------------------------------------------------------------------------
// The glossary at /glossary (portal::glossary), and the retired /docs paths.
// ---------------------------------------------------------------------------

/// True if `s` contains a digit-group run matching `groups` separated
/// by `-` (e.g. `[3, 2, 4]` matches an SSN `123-45-6789`), bounded so a
/// longer digit run on either side doesn't count. Hand-rolled so the
/// guardrail needs no regex dependency.
fn contains_dash_digit_pattern(s: &str, groups: &[usize]) -> bool {
    let bytes = s.as_bytes();
    let total: usize = groups.iter().sum::<usize>() + groups.len() - 1;
    let is_digit = |b: u8| b.is_ascii_digit();
    for start in 0..=bytes.len().saturating_sub(total) {
        // Left boundary: not preceded by a digit.
        if start > 0 && is_digit(bytes[start - 1]) {
            continue;
        }
        let mut pos = start;
        let mut ok = true;
        for (gi, &len) in groups.iter().enumerate() {
            if gi > 0 {
                if bytes.get(pos) != Some(&b'-') {
                    ok = false;
                    break;
                }
                pos += 1;
            }
            for _ in 0..len {
                match bytes.get(pos) {
                    Some(&b) if is_digit(b) => pos += 1,
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                break;
            }
        }
        // Right boundary: not followed by a digit.
        if ok && bytes.get(pos).is_none_or(|&b| !is_digit(b)) {
            return true;
        }
    }
    false
}

#[test]
fn the_glossary_carries_no_client_confidences() {
    // The published-docs guardrail (RPC 1.6): the confidentiality
    // boundary is portal auth on the database, but as a belt-and-braces
    // check no published doc may contain an obvious client identifier —
    // an SSN- (ddd-dd-dddd) or EIN-shaped (dd-ddddddd) number. Docs use
    // placeholders today; this keeps it that way. (A real client name
    // can't be matched mechanically; the DB/portal boundary is what
    // actually protects it.)
    for entry in &portal::glossary::content().entries {
        assert!(
            !contains_dash_digit_pattern(&entry.body_html, &[3, 2, 4]),
            "/glossary#{} contains an SSN-shaped string — the published \
             glossary must carry no client confidence",
            entry.slug
        );
        assert!(
            !contains_dash_digit_pattern(&entry.body_html, &[2, 7]),
            "/glossary#{} contains an EIN-shaped string — the published \
             glossary must carry no client confidence",
            entry.slug
        );
    }
}

#[test]
fn dash_digit_pattern_detects_ssn_and_respects_boundaries() {
    assert!(contains_dash_digit_pattern(
        "ssn 123-45-6789 here",
        &[3, 2, 4]
    ));
    assert!(contains_dash_digit_pattern("ein 12-3456789.", &[2, 7]));
    // A longer digit run is not an SSN.
    assert!(!contains_dash_digit_pattern("v1234-45-6789", &[3, 2, 4]));
    assert!(!contains_dash_digit_pattern("port 8080", &[3, 2, 4]));
}

// ---- Lawyer playbooks (#956 Phase 4) ----
//
// The `/app/admin/playbooks` cluster shipped with no route-level coverage at
// all — only in-file unit tests on the pure parsers, which never touch a
// handler. These tests pin the behaviour the surface actually has (the sort
// `400`, the unknown-id `404`, and each refusal) so the Dioxus port is proved
// against the real routes rather than against the parsers.

// ---------------------------------------------------------------------------
// ENG-81 — one matter surface at `/app/projects`.
//
// The lens used to come from the URL prefix, which the requester chooses. It
// now comes from the caller's tier plus their `person_project_roles` row. These
// tests pin that on the collapsed path: one path per resource, one assertion
// per tier, and the denials that the collapse could quietly turn into grants.
// ---------------------------------------------------------------------------
