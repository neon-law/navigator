//! The closing room at `/app/projects/sample-transactional/closing`: the
//! checklist, signatures, terms, filing requirements, and revisions read the
//! matter's Notations, Signatures, Filings, and Revisions through each lens.
//! The client never reads a hold's words, the clause comparison, or an
//! unpublished revision. The cap table reads the `cap_table` Module's rows,
//! the firm always and a client only while the Module is enabled.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use store::persons::Role;
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

const FUND: &str = "Synthetic Seed Fund LP";

/// A synthetic investors' rights agreement: one open term, one hold, a
/// signature step, and a Secretary of State filing.
const IRA: &str = "---\ntitle: Investors' Rights Agreement\nkind: agreement\nworkflow:\n  BEGIN:\n    _: lawyer_review\n  lawyer_review:\n    _: client_signature\n  client_signature:\n    _: filing__secretary_of_state\n  filing__secretary_of_state:\n    _: END\n  END: {}\n---\n\n# Investors' Rights Agreement\n\n## 1. Pro rata\n\nEach major investor may buy {{pro_rata_threshold}} of new securities.\n\n[Confirm the synthetic option pool timing.]\n";

const HOLD_WORDS: &str = "Confirm the synthetic option pool timing.";
const DRAFT_REDLINE: &str = "ira-v3-internal-redline.docx";

struct Fixture {
    app: axum::Router,
    surreal: store::surreal::SurrealDb,
    project_id: Uuid,
    lawyer_id: Uuid,
    lawyer_cookie: String,
    client_cookie: String,
    outsider_cookie: String,
}

async fn person(surreal: &store::surreal::SurrealDb, name: &str, email: &str, role: Role) -> Uuid {
    store::persons::create(
        surreal,
        &store::persons::NewPerson::with_role(name, email, role),
    )
    .await
    .unwrap()
    .id
}

fn pages() -> Vec<rules::pages::PageEntry> {
    let sections = |extra: &[&str]| -> Vec<String> {
        extra
            .iter()
            .chain(&["cap_table", "boundary_note", "provenance_statement"])
            .map(|s| (*s).to_string())
            .collect()
    };
    let mut lenses = std::collections::BTreeMap::new();
    let shared = [
        "closing_checklist",
        "signature_tracker",
        "term_strip",
        "open_terms",
        "requirement_tracker",
        "redline_view",
    ];
    let mut lawyer: Vec<&str> = shared.to_vec();
    lawyer.push("term_comparison");
    lenses.insert("lawyer".to_string(), sections(&lawyer));
    lenses.insert("client".to_string(), sections(&shared));
    vec![rules::pages::PageEntry {
        slug: "closing".into(),
        kind: "closing_room".into(),
        title: "Closing".into(),
        draft: None,
        lenses,
    }]
}

async fn seed_holdings(surreal: &store::surreal::SurrealDb, project_id: Uuid, client: Uuid) {
    let fund = store::entities::create(
        surreal,
        &store::entities::NewEntity {
            name: FUND.into(),
            entity_type_id: store::test_support::SEED_ENTITY_TYPE_ID,
            jurisdiction_id: store::test_support::SEED_ENTITY_JURISDICTION_ID,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .unwrap()
    .id;
    for (holder, class, shares) in [
        (
            store::project_references::Party::person(client),
            "Common",
            7_500,
        ),
        (
            store::project_references::Party {
                kind: store::relationships::Endpoint::Entity,
                id: fund,
            },
            "Series Seed",
            2_500,
        ),
    ] {
        store::cap_table_holdings::insert(
            surreal,
            project_id,
            &store::cap_table_holdings::CapTableHoldingFields {
                holder,
                share_class: class.into(),
                shares,
                vesting_note: None,
            },
        )
        .await
        .unwrap();
    }
}

/// The agreement's Notation with a sent envelope, and three revisions of its
/// document: two published to the client and one internal redline.
async fn seed_deal(
    surreal: &store::surreal::SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    project_id: Uuid,
    client: Uuid,
) {
    let asset = store::assets::ingest_content(surreal, storage, IRA.as_bytes(), "text/markdown")
        .await
        .unwrap();
    let template = store::templates::save_version(
        surreal,
        Some(project_id),
        "closing__ira",
        store::templates::Version {
            title: "Investors' Rights Agreement".into(),
            respondent_type: "person".into(),
            asset_id: Some(asset),
            form_code: None,
            kind: Some("agreement".into()),
            source_commit_sha: Some(store::test_support::SYNTHETIC_COMMIT_SHA.into()),
        },
    )
    .await
    .unwrap()
    .into_model();
    let notation = store::notations::create(
        surreal,
        &store::notations::NewNotation::new(template.id, client, project_id, "client_signature"),
    )
    .await
    .unwrap();
    store::signatures::record_request(
        surreal,
        notation.id,
        store::signatures::SignatureProvider::DocuSign,
        "synthetic-envelope",
    )
    .await
    .unwrap();
    for (filename, visibility, published) in [
        (
            "ira-v1.docx",
            store::documents::visibility::CLIENT,
            Some("2026-10-01T00:00:00Z"),
        ),
        (
            "ira-v2.docx",
            store::documents::visibility::CLIENT,
            Some("2026-10-05T00:00:00Z"),
        ),
        (DRAFT_REDLINE, store::documents::visibility::INTERNAL, None),
    ] {
        store::assets::file_revision(
            surreal,
            storage,
            &store::documents::IngestArgs {
                project_id,
                source: store::documents::source::UPLOAD,
                filename,
                kind: "unclassified",
                content_type: "application/pdf",
                description: None,
                visibility,
                secondary_storage_key: None,
            },
            &store::documents::DocumentIdentity {
                slug: Some("ira"),
                published_at: published,
                ..Default::default()
            },
            filename.as_bytes(),
        )
        .await
        .unwrap();
    }
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: "sample-transactional".into(),
            name: "Widget Works — Outside Counsel".into(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&surreal).await,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let lawyer = person(
        &surreal,
        "Sample Counsel",
        "closing-counsel@example.com",
        Role::Lawyer,
    )
    .await;
    let client = person(
        &surreal,
        "Sample Founder",
        "closing-founder@example.com",
        Role::Client,
    )
    .await;
    for (id, participation) in [(lawyer, "lawyer"), (client, "client")] {
        store::projects::add_participation(&surreal, project.id, id, participation)
            .await
            .unwrap();
    }
    let outsider = person(
        &surreal,
        "Other Client",
        "closing-other@example.com",
        Role::Client,
    )
    .await;
    seed_holdings(&surreal, project.id, client).await;
    store::matter_pages::replace_for_project(&surreal, project.id, &pages())
        .await
        .unwrap();

    let mut state = portal::test_support::app_state(surreal.clone()).await;
    state.policy = portal::policy::PolicyClient::embedded().expect("embedded policy compiles");
    seed_deal(&surreal, &state.storage, project.id, client).await;
    let sessions = portal::SessionStore::new(portal::test_support::TEST_SESSION_KEY);
    let cookie = |sub: &str, person_id: Uuid, role: Role| {
        let mut session = portal::SessionData::fresh(sub, role);
        session.person_id = Some(person_id);
        format!(
            "{}={}",
            portal::session::SESSION_COOKIE_NAME,
            sessions.encode(&session)
        )
    };
    Fixture {
        app: portal::router(state),
        surreal,
        project_id: project.id,
        lawyer_id: lawyer,
        lawyer_cookie: cookie("closing-counsel", lawyer, Role::Lawyer),
        client_cookie: cookie("closing-founder", client, Role::Client),
        outsider_cookie: cookie("closing-other", outsider, Role::Client),
    }
}

async fn get(f: &Fixture, cookie: &str) -> (StatusCode, String) {
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/app/projects/sample-transactional/closing")
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

#[tokio::test]
async fn the_cap_table_reads_the_module_rows_through_each_lens() {
    let f = fixture().await;
    let (status, lawyer) = get(&f, &f.lawyer_cookie).await;
    assert_eq!(status, StatusCode::OK, "{lawyer}");
    for text in [
        "data-section=\"cap_table\"",
        "Sample Founder",
        FUND,
        "75.00%",
        "25.00%",
        "role=\"img\"",
    ] {
        assert!(lawyer.contains(text), "{text}: {lawyer}");
    }
    let (status, client) = get(&f, &f.client_cookie).await;
    assert_eq!(status, StatusCode::OK, "{client}");
    assert!(!client.contains(FUND), "{client}");
    assert!(
        client.contains("No holding is recorded on this matter yet."),
        "{client}"
    );
    store::project_modules::enable(
        &f.surreal,
        f.project_id,
        store::project_modules::Module::CapTable,
        Some(f.lawyer_id),
    )
    .await
    .unwrap();
    let (_, client) = get(&f, &f.client_cookie).await;
    assert!(client.contains(FUND), "{client}");
    let (status, outsider) = get(&f, &f.outsider_cookie).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!outsider.contains(FUND), "{outsider}");
}

#[tokio::test]
async fn a_lawyer_sees_the_closing_room_with_holds_and_every_revision() {
    let f = fixture().await;
    let (status, html) = get(&f, &f.lawyer_cookie).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    for text in [
        "data-section=\"closing_checklist\"",
        "Answer: Pro rata threshold",
        HOLD_WORDS,
        "data-section=\"signature_tracker\"",
        "data-status=\"Sent\"",
        "data-section=\"term_strip\"",
        "data-section=\"open_terms\"",
        "/outline#section-1-p1",
        "data-section=\"requirement_tracker\"",
        "data-status=\"Not started\"",
        "data-section=\"redline_view\"",
        DRAFT_REDLINE,
        "data-section=\"term_comparison\"",
        "No contract review has findings yet.",
    ] {
        assert!(html.contains(text), "{text}: {html}");
    }
}

#[tokio::test]
async fn a_client_never_reads_hold_words_the_comparison_or_an_unpublished_revision() {
    let f = fixture().await;
    let (status, html) = get(&f, &f.client_cookie).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("Answer: Pro rata threshold"), "{html}");
    assert!(html.contains("ira-v2.docx"), "{html}");
    assert!(
        !html.contains(HOLD_WORDS),
        "a hold's words must not reach a client's markup or hydration payload"
    );
    assert!(!html.contains(DRAFT_REDLINE), "{html}");
    assert!(!html.contains("data-section=\"term_comparison\""), "{html}");
}
