//! The three sample matters, headless: the development seed writes the rows
//! each sample's declared sections read, the `pages:` block each sample
//! repository carries is stored as `project sync` would store it, and every
//! declared page then renders through the assembled router in both the
//! client and the lawyer lens, each declared section present. The
//! litigation timeline carries every entry of the sample's case timeline.
//!
//! The fixtures under `fixtures/` are the `pages:` blocks of
//! `neon-law/sample-litigation`, `-estate`, and `-transactional`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rules::dashboard::Lens;
use store::persons::Role;
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

const SAMPLES: &[(&str, &str)] = &[
    (
        store::seed::SAMPLE_LITIGATION_CODE,
        include_str!("fixtures/sample-litigation.pages.yaml"),
    ),
    (
        store::seed::SAMPLE_ESTATE_CODE,
        include_str!("fixtures/sample-estate.pages.yaml"),
    ),
    (
        store::seed::SAMPLE_TRANSACTIONAL_CODE,
        include_str!("fixtures/sample-transactional.pages.yaml"),
    ),
];

/// Entries of the litigation timeline a lawyer reads on the timeline page:
/// filed Docket Entries and the dates still ahead.
const TIMELINE_ENTRIES: &[&str] = &[
    "Complaint — trespass to land",
    "Summons issued and served on Wendell Prine",
    "Notice of rescission served",
    "Answer to Count II and counterclaim",
    "Motion for partial summary judgment on the limitations defense",
    "Answer to counterclaim",
    "Hearing on the motion for partial summary judgment, Department XVII",
    "Trial setting",
];

struct Fixture {
    app: axum::Router,
    client_cookie: String,
    lawyer_cookie: String,
}

async fn person_id(surreal: &store::surreal::SurrealDb, email: &str) -> Uuid {
    store::persons::find_by_email_ci(surreal, email)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("the sample seed creates {email}"))
        .id
}

fn pages(yaml: &str) -> Vec<rules::pages::PageEntry> {
    let document: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
    rules::pages::entries(document.as_mapping().unwrap()).unwrap()
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let mut state = portal::test_support::app_state(surreal.clone()).await;
    store::seed::seed_canonical(&surreal, &state.storage)
        .await
        .unwrap();
    store::seed::seed_sample_portfolio(&surreal, &state.storage)
        .await
        .unwrap();
    for (code, yaml) in SAMPLES {
        let project = store::projects::find_by_code(&surreal, code)
            .await
            .unwrap()
            .unwrap();
        store::matter_pages::replace_for_project(&surreal, project.id, &pages(yaml))
            .await
            .unwrap();
    }
    let client = person_id(&surreal, "client@neonlaw.com").await;
    let lawyer = person_id(&surreal, "lawyer@neonlaw.com").await;

    let sessions = portal::SessionStore::new(portal::test_support::TEST_SESSION_KEY);
    let cookie_for = |sub: &str, person: Uuid, role: Role| {
        let mut session = portal::SessionData::fresh(sub, role);
        session.person_id = Some(person);
        format!(
            "{}={}",
            portal::session::SESSION_COOKIE_NAME,
            sessions.encode(&session)
        )
    };
    state.policy = portal::policy::PolicyClient::embedded().expect("embedded policy compiles");
    Fixture {
        app: portal::router(state),
        client_cookie: cookie_for("sample-client", client, Role::Client),
        lawyer_cookie: cookie_for("sample-lawyer", lawyer, Role::Lawyer),
    }
}

async fn get(f: &Fixture, path: &str, cookie: &str) -> (StatusCode, String) {
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// Each sample's `pages:` block passes the gate's shape rules, so a title
/// that would repeat the show page's own tab never reaches a strip.
#[test]
fn every_sample_pages_block_passes_the_gate_shape_rules() {
    for (code, yaml) in SAMPLES {
        assert_eq!(rules::pages::shape_problems(yaml), Vec::new(), "{code}");
    }
}

#[tokio::test]
async fn every_declared_sample_page_renders_in_both_lenses_from_seeded_records() {
    let f = fixture().await;
    let mut rendered = std::collections::BTreeMap::new();
    for (code, yaml) in SAMPLES {
        for page in pages(yaml) {
            for (lens, cookie) in [
                (Lens::Client, &f.client_cookie),
                (Lens::Lawyer, &f.lawyer_cookie),
            ] {
                let path = format!("/app/projects/{code}/{}", page.slug);
                let (status, html) = get(&f, &path, cookie).await;
                assert_eq!(
                    status,
                    StatusCode::OK,
                    "{path} as {}: {html}",
                    lens.as_str()
                );
                assert!(
                    html.contains(&format!("data-lens=\"{}\"", lens.as_str())),
                    "{path} renders the {} lens",
                    lens.as_str()
                );
                for section in page.sections_for(lens).unwrap_or_default() {
                    assert!(
                        html.contains(&format!("data-section=\"{}\"", section.as_str())),
                        "{path} as {} renders `{}`",
                        lens.as_str(),
                        section.as_str()
                    );
                }
                rendered.insert((code.to_string(), page.slug.clone(), lens.as_str()), html);
            }
        }
    }

    let page = |code: &str, slug: &str, lens: Lens| {
        rendered
            .get(&(code.to_string(), slug.to_string(), lens.as_str()))
            .unwrap_or_else(|| panic!("{code}/{slug} was rendered"))
    };
    let litigation = store::seed::SAMPLE_LITIGATION_CODE;
    let timeline = page(litigation, "timeline", Lens::Lawyer);
    for entry in TIMELINE_ENTRIES {
        assert!(timeline.contains(entry), "the timeline carries `{entry}`");
    }
    assert!(
        page(litigation, "timeline", Lens::Client).contains("Trial setting"),
        "a client reads the dates still ahead"
    );
    assert!(
        page(litigation, "discovery", Lens::Lawyer).contains("I was wearing a sweater."),
        "the discovery page pairs each request with its response"
    );
    assert!(page(litigation, "case-library", Lens::Lawyer).contains("D.R. Horton, Inc. v. Green"));
    assert!(
        page(store::seed::SAMPLE_ESTATE_CODE, "research", Lens::Lawyer)
            .contains("In re Estate of Borghi")
    );
}
