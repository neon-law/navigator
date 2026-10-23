//! The estate plan console at `/app/projects/sample-estate/{slug}`: each
//! estate Notation renders as an instrument card with the formalities its
//! workflow declares, a blocked instrument names its open term, and the
//! client lens never reads the words of a hold. The fiduciary roster and the
//! asset schedule read the `estate` Module's rows: the firm always, a client
//! only while the Module is enabled. Neither a clerk nor a non-participant
//! reads an instrument.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use store::persons::Role;
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

/// A synthetic power of attorney: a two-witness, notarized workflow, one
/// open term, and one hold.
const POA: &str = "---\ntitle: Durable Power of Attorney\nkind: directive\nworkflow:\n  BEGIN:\n    _: lawyer_review\n  lawyer_review:\n    approved: generate_pdf\n    rejected: reask\n  reask:\n    _: lawyer_review\n  generate_pdf:\n    _: client_signature\n  client_signature:\n    _: witnesses\n  witnesses:\n    _: notarization\n  notarization:\n    _: END\n  END: {}\n---\n\n# Durable Power of Attorney\n\n## I. Agent\n\nThe agent serves, then {{person__successor_agent.name}}.\n\n[Confirm the synthetic gifting power.]\n";

/// A synthetic trust with no open term, already at its signing step.
const TRUST: &str = "---\ntitle: Revocable Living Trust\nkind: trust\nworkflow:\n  BEGIN:\n    _: lawyer_review\n  lawyer_review:\n    _: client_signature\n  client_signature:\n    _: notarization\n  notarization:\n    _: END\n  END: {}\n---\n\n# Revocable Living Trust\n\n## I. Trustee\n\nThe settlor serves as trustee.\n";

const HOLD_WORDS: &str = "Confirm the synthetic gifting power.";

const TRUSTEE: &str = "Synthetic Successor Trustee Co.";
const ASSET: &str = "Synthetic brokerage account";

struct Fixture {
    app: axum::Router,
    surreal: store::surreal::SurrealDb,
    project_id: Uuid,
    lawyer_id: Uuid,
    lawyer_cookie: String,
    client_cookie: String,
    clerk_cookie: String,
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

/// A project-scoped Draft whose body is in object storage, and a Notation on
/// it at `state`.
async fn notation(
    surreal: &store::surreal::SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    project_id: Uuid,
    respondent: Uuid,
    (code, kind, body, state): (&str, &str, &str, &str),
) -> Uuid {
    let asset = store::assets::ingest_content(surreal, storage, body.as_bytes(), "text/markdown")
        .await
        .unwrap();
    let template = store::templates::save_version(
        surreal,
        Some(project_id),
        code,
        store::templates::Version {
            title: body
                .lines()
                .find_map(|line| line.strip_prefix("title: "))
                .unwrap()
                .to_string(),
            respondent_type: "person".into(),
            asset_id: Some(asset),
            form_code: None,
            kind: Some(kind.into()),
            source_commit_sha: Some(store::test_support::SYNTHETIC_COMMIT_SHA.into()),
        },
    )
    .await
    .unwrap()
    .into_model();
    store::notations::create(
        surreal,
        &store::notations::NewNotation::new(template.id, respondent, project_id, state),
    )
    .await
    .unwrap()
    .id
}

async fn seed_estate_rows(surreal: &store::surreal::SurrealDb, project_id: Uuid, client: Uuid) {
    let trustee = store::entities::create(
        surreal,
        &store::entities::NewEntity {
            name: TRUSTEE.into(),
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
    for (role, fiduciary, order) in [
        (
            "trustee",
            store::project_references::Party::person(client),
            1,
        ),
        (
            "trustee",
            store::project_references::Party {
                kind: store::relationships::Endpoint::Entity,
                id: trustee,
            },
            2,
        ),
    ] {
        store::fiduciary_roles::insert(
            surreal,
            project_id,
            &store::fiduciary_roles::FiduciaryRoleFields {
                role: role.into(),
                fiduciary,
                successor_order: order,
                notation_id: None,
            },
        )
        .await
        .unwrap();
    }
    store::estate_assets::insert(
        surreal,
        project_id,
        &store::estate_assets::EstateAssetFields {
            category: "securities".into(),
            description: ASSET.into(),
            passes_by: "trust".into(),
            funding_state: "consent_needed".into(),
            note: None,
        },
    )
    .await
    .unwrap();
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: "sample-estate".into(),
            name: "Estate of Cornelius Montgomery".into(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&surreal).await,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut ids = Vec::new();
    for (name, email, role, participation) in [
        (
            "Sample Counsel",
            "estate-counsel@example.com",
            Role::Lawyer,
            "lawyer",
        ),
        (
            "Sample Client",
            "estate-client@example.com",
            Role::Client,
            "client",
        ),
        (
            "Sample Clerk",
            "estate-clerk@example.com",
            Role::Clerk,
            "clerk",
        ),
    ] {
        let id = person(&surreal, name, email, role).await;
        store::projects::add_participation(&surreal, project.id, id, participation)
            .await
            .unwrap();
        ids.push((id, role));
    }
    let outsider = person(
        &surreal,
        "Other Client",
        "estate-other@example.com",
        Role::Client,
    )
    .await;

    let mut state = portal::test_support::app_state(surreal.clone()).await;
    state.policy = portal::policy::PolicyClient::embedded().expect("embedded policy compiles");
    let client = ids[1].0;
    notation(
        &surreal,
        &state.storage,
        project.id,
        client,
        ("estate__poa", "directive", POA, "generate_pdf"),
    )
    .await;
    notation(
        &surreal,
        &state.storage,
        project.id,
        client,
        ("estate__trust", "trust", TRUST, "client_signature"),
    )
    .await;
    seed_estate_rows(&surreal, project.id, client).await;
    store::matter_pages::replace_for_project(&surreal, project.id, &pages())
        .await
        .unwrap();

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
        lawyer_id: ids[0].0,
        lawyer_cookie: cookie("estate-counsel", ids[0].0, ids[0].1),
        client_cookie: cookie("estate-client", ids[1].0, ids[1].1),
        clerk_cookie: cookie("estate-clerk", ids[2].0, ids[2].1),
        outsider_cookie: cookie("estate-other", outsider, Role::Client),
    }
}

fn pages() -> Vec<rules::pages::PageEntry> {
    let sections = [
        "instrument_set",
        "signing_ceremony",
        "distribution_plan",
        "administration_steps",
        "fiduciary_roster",
        "asset_schedule",
        "boundary_note",
        "provenance_statement",
    ]
    .map(String::from)
    .to_vec();
    let mut lenses = std::collections::BTreeMap::new();
    for lens in ["lawyer", "client", "clerk"] {
        lenses.insert(lens.to_string(), sections.clone());
    }
    vec![rules::pages::PageEntry {
        slug: "estate-plan".into(),
        kind: "estate_plan_console".into(),
        title: "Estate plan".into(),
        draft: None,
        lenses,
    }]
}

async fn get(f: &Fixture, cookie: &str) -> (StatusCode, String) {
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/app/projects/sample-estate/estate-plan")
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
async fn a_lawyer_sees_each_instrument_with_its_formalities_and_blockers() {
    let f = fixture().await;
    let (status, html) = get(&f, &f.lawyer_cookie).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    for text in [
        "data-section=\"instrument_set\"",
        "Durable Power of Attorney",
        "data-status=\"Blocked\"",
        "Blocked by open term",
        "Successor agent",
        HOLD_WORDS,
        "Revocable Living Trust",
        "data-status=\"At ceremony\"",
        "Witnesses sign",
        "Notarized",
        "data-section=\"signing_ceremony\"",
        "Not scheduled yet",
        "data-section=\"administration_steps\"",
        "data-state=\"Now\"",
    ] {
        assert!(html.contains(text), "{text}: {html}");
    }
    // No trust answer names its residue, so the plan says so.
    assert!(
        html.contains("No residuary shares are recorded yet."),
        "{html}"
    );
}

#[tokio::test]
async fn a_client_sees_the_blocked_term_but_never_the_hold_words() {
    let f = fixture().await;
    let (status, html) = get(&f, &f.client_cookie).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("data-lens=\"client\""), "{html}");
    assert!(html.contains("Successor agent"), "{html}");
    assert!(
        html.contains("a drafting point your lawyer is resolving"),
        "{html}"
    );
    assert!(
        !html.contains(HOLD_WORDS),
        "a hold's words must not reach a client's markup or hydration payload"
    );
}

#[tokio::test]
async fn neither_a_clerk_nor_an_outsider_reads_the_instruments() {
    let f = fixture().await;
    // A clerk never receives a matter's legal work: the page either has no
    // face for them or loads none of the Notations.
    let (_, html) = get(&f, &f.clerk_cookie).await;
    assert!(!html.contains("Durable Power of Attorney"), "{html}");
    assert!(!html.contains(HOLD_WORDS), "{html}");
    let (status, html) = get(&f, &f.outsider_cookie).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!html.contains("Durable Power of Attorney"), "{html}");
}

#[tokio::test]
async fn a_client_reads_the_estate_rows_only_while_the_module_is_enabled() {
    let f = fixture().await;
    let (_, lawyer) = get(&f, &f.lawyer_cookie).await;
    for text in [
        "data-section=\"fiduciary_roster\"",
        "Sample Client",
        TRUSTEE,
        ASSET,
        "data-funding=\"Consent needed\"",
        "0 of 1 funded",
    ] {
        assert!(lawyer.contains(text), "{text}: {lawyer}");
    }
    let (_, client) = get(&f, &f.client_cookie).await;
    assert!(!client.contains(TRUSTEE), "{client}");
    assert!(!client.contains(ASSET), "{client}");
    assert!(
        client.contains("No fiduciary is named on this matter yet."),
        "{client}"
    );
    store::project_modules::enable(
        &f.surreal,
        f.project_id,
        store::project_modules::Module::Estate,
        Some(f.lawyer_id),
    )
    .await
    .unwrap();
    let (_, client) = get(&f, &f.client_cookie).await;
    assert!(client.contains(TRUSTEE), "{client}");
    assert!(client.contains(ASSET), "{client}");
}
