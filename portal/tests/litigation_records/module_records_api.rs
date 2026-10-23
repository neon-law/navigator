//! The estate and cap-table reads through the assembled router and the
//! embedded policy: `GET /app/api/projects/{id}/fiduciary-roles`,
//! `.../estate-assets`, and `.../cap-table-holdings`. The firm and the
//! matter's own client read every row; a client reads an empty list while the
//! owning Module is disabled; a client on another matter gets the same 404 as
//! an unknown Project; and a write verb is refused.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use store::persons::Role;
use store::project_modules::Module;
use store::project_references::Party;
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

const RESOURCES: [&str; 3] = ["fiduciary-roles", "estate-assets", "cap-table-holdings"];

struct Fixture {
    app: axum::Router,
    surreal: store::surreal::SurrealDb,
    project_id: Uuid,
    client_cookie: String,
    lawyer_cookie: String,
    other_client_cookie: String,
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

async fn project(surreal: &store::surreal::SurrealDb, code: &str) -> Uuid {
    store::projects::create(
        surreal,
        &store::projects::NewProject {
            code: code.into(),
            name: "Sample matter".into(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(surreal).await,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .id
}

/// A Person participating on `project_id` as `participation`.
async fn participant(
    surreal: &store::surreal::SurrealDb,
    project_id: Uuid,
    email: &str,
    role: Role,
    participation: &str,
) -> Uuid {
    let id = person(surreal, "Sample Participant", email, role).await;
    store::projects::add_participation(surreal, project_id, id, participation)
        .await
        .unwrap();
    id
}

/// One row of each kind on the matter, the Modules that own them enabled, and
/// a row on the other matter so a leak would show up as a second row.
async fn seed_rows(
    surreal: &store::surreal::SurrealDb,
    project_id: Uuid,
    other_project: Uuid,
    client: Uuid,
    lawyer: Uuid,
) {
    store::fiduciary_roles::insert(
        surreal,
        project_id,
        &store::fiduciary_roles::FiduciaryRoleFields {
            role: "trustee".into(),
            fiduciary: Party::person(client),
            successor_order: 1,
            notation_id: None,
        },
    )
    .await
    .unwrap();
    store::estate_assets::insert(
        surreal,
        project_id,
        &store::estate_assets::EstateAssetFields {
            category: "real_property".into(),
            description: "Primary residence".into(),
            passes_by: "trust".into(),
            funding_state: "in_progress".into(),
            note: None,
        },
    )
    .await
    .unwrap();
    store::cap_table_holdings::insert(
        surreal,
        project_id,
        &store::cap_table_holdings::CapTableHoldingFields {
            holder: Party::person(client),
            share_class: "Common".into(),
            shares: 1_000,
            vesting_note: None,
        },
    )
    .await
    .unwrap();
    for module in [Module::Estate, Module::CapTable] {
        store::project_modules::enable(surreal, project_id, module, Some(lawyer))
            .await
            .unwrap();
    }
    store::estate_assets::insert(
        surreal,
        other_project,
        &store::estate_assets::EstateAssetFields {
            category: "account".into(),
            description: "Operating account".into(),
            passes_by: "other".into(),
            funding_state: "funded".into(),
            note: None,
        },
    )
    .await
    .unwrap();
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let project_id = project(&surreal, "sample-estate").await;
    let other_project = project(&surreal, "sample-transactional").await;
    let client = participant(
        &surreal,
        project_id,
        "records-grantor@example.com",
        Role::Client,
        "client",
    )
    .await;
    let lawyer = participant(
        &surreal,
        project_id,
        "records-counsel@example.com",
        Role::Lawyer,
        "lawyer",
    )
    .await;
    let other_client = participant(
        &surreal,
        other_project,
        "records-other@example.com",
        Role::Client,
        "client",
    )
    .await;

    seed_rows(&surreal, project_id, other_project, client, lawyer).await;

    let sessions = portal::SessionStore::new(portal::test_support::TEST_SESSION_KEY);
    let cookie_for = |sub: &str, person_id: Uuid, role: Role| {
        let mut session = portal::SessionData::fresh(sub, role);
        session.person_id = Some(person_id);
        format!(
            "{}={}",
            portal::session::SESSION_COOKIE_NAME,
            sessions.encode(&session)
        )
    };
    let mut state = portal::test_support::app_state(surreal.clone()).await;
    state.policy = portal::policy::PolicyClient::embedded().expect("embedded policy compiles");
    Fixture {
        app: portal::router(state),
        surreal,
        project_id,
        client_cookie: cookie_for("records-grantor", client, Role::Client),
        lawyer_cookie: cookie_for("records-counsel", lawyer, Role::Lawyer),
        other_client_cookie: cookie_for("records-other", other_client, Role::Client),
    }
}

async fn send(f: &Fixture, method: &str, path: &str, cookie: &str) -> (StatusCode, String) {
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
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

fn path(f: &Fixture, resource: &str) -> String {
    format!("/app/api/projects/{}/{resource}", f.project_id)
}

fn rows(body: &str) -> Vec<serde_json::Value> {
    serde_json::from_str::<Vec<serde_json::Value>>(body).expect("a JSON array")
}

#[tokio::test]
async fn the_lawyer_reads_every_row_on_the_matter() {
    let f = fixture().await;
    for resource in RESOURCES {
        let (status, body) = send(&f, "GET", &path(&f, resource), &f.lawyer_cookie).await;
        assert_eq!(status, StatusCode::OK, "{resource}: {body}");
        assert_eq!(rows(&body).len(), 1, "{resource}: {body}");
    }
}

#[tokio::test]
async fn the_client_reads_their_own_matters_rows() {
    let f = fixture().await;
    for resource in RESOURCES {
        let (status, body) = send(&f, "GET", &path(&f, resource), &f.client_cookie).await;
        assert_eq!(status, StatusCode::OK, "{resource}: {body}");
        let rows = rows(&body);
        assert_eq!(rows.len(), 1, "{resource}: {body}");
        assert_eq!(
            rows[0]["project_id"],
            serde_json::json!(f.project_id),
            "{resource}: {body}"
        );
    }
    let (_, body) = send(&f, "GET", &path(&f, "estate-assets"), &f.client_cookie).await;
    assert!(body.contains("Primary residence"), "{body}");
    assert!(!body.contains("Operating account"), "{body}");
}

#[tokio::test]
async fn a_client_on_another_matter_gets_404() {
    let f = fixture().await;
    for resource in RESOURCES {
        let (status, body) = send(&f, "GET", &path(&f, resource), &f.other_client_cookie).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{resource}: {body}");
        assert!(!body.contains("Primary residence"), "{resource}: {body}");
    }
}

#[tokio::test]
async fn an_unknown_project_gets_404() {
    let f = fixture().await;
    let (status, _) = send(
        &f,
        "GET",
        &format!("/app/api/projects/{}/estate-assets", Uuid::now_v7()),
        &f.lawyer_cookie,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_doors_are_read_only() {
    let f = fixture().await;
    for resource in RESOURCES {
        let (status, _) = send(&f, "POST", &path(&f, resource), &f.lawyer_cookie).await;
        assert_ne!(status, StatusCode::OK, "{resource}");
        assert_ne!(status, StatusCode::CREATED, "{resource}");
    }
}

#[tokio::test]
async fn a_client_reads_an_empty_list_while_the_module_is_disabled() {
    let f = fixture().await;
    for module in [Module::Estate, Module::CapTable] {
        store::project_modules::disable(&f.surreal, f.project_id, module, None)
            .await
            .unwrap();
    }
    for resource in RESOURCES {
        let (status, body) = send(&f, "GET", &path(&f, resource), &f.client_cookie).await;
        assert_eq!(status, StatusCode::OK, "{resource}: {body}");
        assert!(rows(&body).is_empty(), "{resource}: {body}");
        let (status, body) = send(&f, "GET", &path(&f, resource), &f.lawyer_cookie).await;
        assert_eq!(status, StatusCode::OK, "{resource}: {body}");
        assert_eq!(rows(&body).len(), 1, "{resource}: {body}");
    }
}
