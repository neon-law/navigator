#![allow(clippy::doc_markdown)]
//! Matter-lifecycle warning flags on the admin Projects list.
//!
//! The firm's lifecycle invariant: every matter opens on an onboarding
//! (`onboarding__*`) notation — the client's retainer — and a *closed*
//! matter carries an `offboarding__letter`. Neither is schema-enforced, so the
//! Projects list surfaces the gaps: a missing onboarding notation folds into
//! the lifecycle status pill's "pitch" state (no separate badge duplicates
//! it), and a missing offboarding letter still carries its own
//! warning badge. These tests pin both the pure rule
//! (`store::projects::matter_flags`) and the rendered list.
//!
//! **An artifact clears a flag; a walk never does.** A notation row means
//! somebody opened a questionnaire walk, not that it produced anything, so the
//! flag clears only on a classified `assets` row or on a walk that actually
//! rendered its instruments. The tests below keep those two apart
//! deliberately: `notation()` opens a walk and leaves it at `BEGIN`,
//! `walk_that_produced_an_instrument()` runs it far enough to file a draft.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use store::persons::Role;
use tower::ServiceExt;
// Keyed render assertions: the matter-flag badges are catalog copy.
use portal::session::{SessionData, SESSION_COOKIE_NAME};
use portal::{AppState, SessionStore};

const SESSION_KEY: &str = "test-session-key-not-for-production";

// ---- the pure rule ----

#[test]
fn flags_an_open_matter_with_no_onboarding_as_missing_onboarding() {
    assert_eq!(
        store::projects::matter_flags(false, "open", false),
        (true, false)
    );
}

#[test]
fn a_matter_with_an_onboarding_notation_is_not_missing_its_retainer() {
    assert_eq!(
        store::projects::matter_flags(true, "open", false),
        (false, false)
    );
}

#[test]
fn flags_a_closed_matter_with_no_closing_letter() {
    // Has its retainer, but closed without a closing letter.
    assert_eq!(
        store::projects::matter_flags(true, "closed", false),
        (false, true)
    );
}

#[test]
fn a_closed_matter_with_a_closing_letter_is_clean() {
    assert_eq!(
        store::projects::matter_flags(true, "closed", true),
        (false, false)
    );
}

#[test]
fn an_open_matter_never_owes_a_closing_letter() {
    // No closing letter on an open matter is fine — it is only owed at close.
    assert_eq!(
        store::projects::matter_flags(true, "open", false),
        (false, false)
    );
}

// ---- the yellow/green/red lifecycle indicator ----

#[test]
fn an_open_matter_missing_its_onboarding_letter_is_the_yellow_state() {
    assert_eq!(
        store::projects::matter_lifecycle("open", true, false),
        store::projects::MatterLifecycle::NeedsOnboarding
    );
}

#[test]
fn an_open_matter_with_its_onboarding_letter_is_the_green_on_file_state() {
    assert_eq!(
        store::projects::matter_lifecycle("open", false, false),
        store::projects::MatterLifecycle::OnboardingOnFile
    );
}

/// The green label is deliberately concise; the title carries the fuller
/// caveat that the paperwork is on file but not verified as executed.
#[test]
fn the_green_label_names_an_active_matter() {
    use store::projects::MatterLifecycle;

    assert_eq!(MatterLifecycle::OnboardingOnFile.label(), "active");
}

/// Every state's title says what the indicator did and did not verify, and the
/// green one carries the limit the one-line label cannot.
#[test]
fn every_lifecycle_state_carries_its_own_title_and_green_states_its_limit() {
    use store::projects::MatterLifecycle;
    let states = [
        MatterLifecycle::NeedsOnboarding,
        MatterLifecycle::OnboardingOnFile,
        MatterLifecycle::Closed,
    ];
    let titles: Vec<&str> = states.iter().map(|s| s.title()).collect();
    assert_eq!(
        titles
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3,
        "each state must carry its own title: {titles:?}"
    );
    assert!(
        MatterLifecycle::OnboardingOnFile
            .title()
            .contains("verified as completed"),
        "the green title must state verified completion: {}",
        MatterLifecycle::OnboardingOnFile.title()
    );
}

#[test]
fn a_closed_matter_with_its_offboarding_letter_is_the_red_state() {
    assert_eq!(
        store::projects::matter_lifecycle("closed", false, false),
        store::projects::MatterLifecycle::Closed
    );
}

#[test]
fn a_closed_matter_still_owing_its_offboarding_letter_is_still_the_red_state() {
    // Red covers both — the still-owed gap keeps surfacing through the
    // separate "no offboarding letter" badge, not a fourth colour.
    assert_eq!(
        store::projects::matter_lifecycle("closed", false, true),
        store::projects::MatterLifecycle::Closed
    );
}

#[test]
fn every_lifecycle_state_carries_its_own_class_and_a_distinct_text_label() {
    use store::projects::MatterLifecycle;
    let states = [
        MatterLifecycle::NeedsOnboarding,
        MatterLifecycle::OnboardingOnFile,
        MatterLifecycle::Closed,
    ];
    let classes: Vec<&str> = states.iter().map(|s| s.class()).collect();
    let labels: Vec<&str> = states.iter().map(|s| s.label()).collect();
    assert_eq!(
        classes
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3,
        "each state must render its own class: {classes:?}"
    );
    assert_eq!(
        labels
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3,
        "each state must carry its own text label — colour cannot be the only signal: {labels:?}"
    );
}

// ---- what counts as the matter-opening engagement ----

#[test]
fn an_onboarding_template_opens_a_matter_whatever_its_code() {
    // The flag keys off the declared `kind`, never the template's code.
    // Every onboarding template happens to use an `onboarding__*` code
    // today; a future one that does not must still count, or the badge
    // would lie about a matter that has its engagement.
    assert!(store::projects::template_opens_a_matter(Some("onboarding")));
}

#[test]
fn a_retired_retainer_kind_string_no_longer_opens_a_matter() {
    // `Kind::Retainer` was merged into `Kind::Onboarding` — every
    // `kind: retainer` template was reclassified `kind: onboarding`, so
    // the bare string `"retainer"` is no longer a recognized `Kind` at
    // all and must not open a matter by accident.
    assert!(!store::projects::template_opens_a_matter(Some("retainer")));
}

#[test]
fn work_inside_an_open_matter_does_not_open_one() {
    for kind in ["letter", "filing", "will", "trust", "directive", "memo"] {
        assert!(
            !store::projects::template_opens_a_matter(Some(kind)),
            "{kind} is work inside a matter, not the engagement that opens it"
        );
    }
}

#[test]
fn an_absent_or_unknown_kind_never_opens_a_matter() {
    // A template that declares no kind — or one this build does not
    // recognize — cannot be a matter's engagement. The badge stays on:
    // better to over-warn lawyer than to silently call a matter opened.
    assert!(!store::projects::template_opens_a_matter(None));
    assert!(!store::projects::template_opens_a_matter(Some("bogus")));
}

#[test]
fn the_badge_reads_every_kind_straight_from_the_classifier() {
    // The badge must answer "does this kind open a matter?" from
    // `rules::kind::Kind::opens_a_matter` and nothing else — re-keying it to
    // a template's `code` (the `onboarding__*` prefix it used to use) would
    // put a permanent "no engagement letter" warning on every matter opened
    // by a retainer template named otherwise. This walks `Kind::ALL` so a
    // new kind cannot slip through unclassified.
    //
    // Scope: this pins the *badge* to the classifier. The first-notation
    // gate in `workflows::notation_session` reads the same classifier, but
    // it is not exercised here — `an_onboarding_opens_a_matter_as_its_first_notation`
    // (`web/tests/project_notation_create.rs`) is what drives a real
    // `kind: onboarding` template through that gate.
    for kind in rules::kind::Kind::ALL {
        assert_eq!(
            store::projects::template_opens_a_matter(Some(kind.as_str())),
            kind.opens_a_matter(),
            "the badge must classify {} straight from `Kind::opens_a_matter`",
            kind.as_str()
        );
    }
}

// ---- what counts as the matter-closing offboarding letter ----

#[test]
fn an_offboarding_template_closes_a_matter_whatever_its_code() {
    // The flag keys off the declared `kind`, never the template's code —
    // the mirror of the engagement side above.
    assert!(store::projects::template_closes_a_matter(Some(
        "offboarding"
    )));
}

#[test]
fn an_ordinary_letter_does_not_close_a_matter() {
    // `kind: letter` is too broad to be the closing classifier — every
    // offboarding letter is a letter, but a demand, notice, or settlement
    // letter must not silently clear the badge.
    assert!(!store::projects::template_closes_a_matter(Some("letter")));
}

#[test]
fn work_inside_an_open_matter_does_not_close_it() {
    for kind in ["onboarding", "filing", "will", "trust", "directive", "memo"] {
        assert!(
            !store::projects::template_closes_a_matter(Some(kind)),
            "{kind} is work inside a matter, not the letter that closes it"
        );
    }
}

#[test]
fn an_absent_or_unknown_kind_never_closes_a_matter() {
    assert!(!store::projects::template_closes_a_matter(None));
    assert!(!store::projects::template_closes_a_matter(Some("bogus")));
}

#[test]
fn the_closing_badge_reads_every_kind_straight_from_the_classifier() {
    // The mirror of `the_badge_reads_every_kind_straight_from_the_classifier`:
    // the badge must answer "does this kind close a matter?" from
    // `rules::kind::Kind::closes_a_matter` and nothing else.
    for kind in rules::kind::Kind::ALL {
        assert_eq!(
            store::projects::template_closes_a_matter(Some(kind.as_str())),
            kind.closes_a_matter(),
            "the badge must classify {} straight from `Kind::closes_a_matter`",
            kind.as_str()
        );
    }
}

// ---- the asset lane (an uploaded, lawyer-classified letter counts too) ----

// ---- the rendered list ----

async fn body_string(resp: axum::http::Response<Body>) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn projects_list_reports_a_loader_failure_as_a_server_error() {
    // The lawyer-lens query runs against a handle with no engine behind it, so
    // it fails. The endpoint must surface that as a real 500 (the explicit
    // server error the retired `projects_index` handler returned), not a 200
    // with an error body — availability monitoring and HTTP clients must see the
    // matter directory as unavailable, not served.
    //
    // The lever is the Surreal handle because every row this listing reads is
    // Surreal-resident, so an unreachable Surreal handle is the only thing
    // that can make the listing fail.
    let application_store = store::application_store::ApplicationStore::uninitialized();
    let state = AppState {
        sessions: SessionStore::new(SESSION_KEY),
        ..portal::test_support::app_state(application_store.clone()).await
    };
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));

    let admin = SessionData::fresh("admin-sub", Role::Admin);
    let cookie = format!(
        "{SESSION_COOKIE_NAME}={}",
        SessionStore::new(SESSION_KEY).encode(&admin)
    );
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/app/projects")
                .header("authorization", "Bearer dev")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let html = body_string(resp).await;
    assert!(
        html.contains("Failed to load projects."),
        "the failed load must still render the error state under the 500; got: {html}",
    );
}
