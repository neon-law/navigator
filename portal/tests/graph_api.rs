//! The read-only relationship-graph doors, through the full router and the
//! embedded Rego policy. The AGE-backed neighborhood content itself is proven
//! against a real graph in `store/src/postgres_graph_views_tests.rs`; here the
//! router contract is what's under test: lawyer tier at admission, the
//! server-side depth cap, the non-disclosing 404 for anything out of scope,
//! and the 503 contract when no PostgreSQL graph is configured.

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use portal::session::SessionStore;
use store::firms::FirmMembership;
use store::persons::{NewPerson, Role};
use tower::ServiceExt;
use uuid::Uuid;

/// A router with the embedded policy, as every `/app/api` door sees it.
async fn graph_app(db: store::application_store::ApplicationStore) -> axum::Router {
    let mut state = portal::test_support::app_state(db).await;
    state.policy = portal::policy::PolicyClient::embedded().expect("embedded policy compiles");
    portal::router(state)
}

/// A lawyer-tier bearer token for `person_id`.
fn bearer(person_id: Uuid, role: Role) -> String {
    let mut session = portal::SessionData::fresh("graph-actor", role);
    session.person_id = Some(person_id);
    format!(
        "Bearer {}",
        SessionStore::new(portal::test_support::TEST_SESSION_KEY).encode(&session)
    )
}

async fn get(app: &axum::Router, path: &str, bearer: &str) -> (StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap_or_default())
}

async fn lawyer(
    db: &store::application_store::ApplicationStore,
    name: &str,
) -> store::persons::Person {
    let email = format!("{}@example.com", name.to_lowercase().replace(' ', "."));
    store::persons::create(db, &NewPerson::with_role(name, &email, Role::Lawyer))
        .await
        .unwrap()
}

async fn admin(
    db: &store::application_store::ApplicationStore,
    name: &str,
) -> store::persons::Person {
    let email = format!("{}@example.com", name.to_lowercase().replace(' ', "."));
    store::persons::create(db, &NewPerson::with_role(name, &email, Role::Admin))
        .await
        .unwrap()
}

async fn seed_firm(
    db: &store::application_store::ApplicationStore,
    name: &str,
    admin_dri: Uuid,
) -> store::firms::Firm {
    store::firms::create(
        db,
        &store::firms::NewFirm {
            name: name.to_string(),
            status: "active".into(),
            entity_id: store::test_support::seed_entity(db).await,
            admin_dri_person_id: admin_dri,
        },
    )
    .await
    .unwrap()
}

async fn seed_matter(db: &store::application_store::ApplicationStore, code: &str) -> Uuid {
    store::test_support::seed_compiled_brands_postgres(db).await;
    store::projects::create(
        db,
        &store::projects::NewProject {
            code: code.into(),
            name: code.to_string(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(db).await,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .id
}

#[tokio::test]
async fn graph_depth_above_the_ceiling_is_a_client_error() {
    store::test_support::with_required_postgres(async {
        let db = store::application_store::ApplicationStore::uninitialized();
        let lawyer = lawyer(&db, "Depth Lawyer").await;
        let app = graph_app(db).await;
        let bearer = bearer(lawyer.id, Role::Lawyer);
        let seed = Uuid::now_v7();

        // The cap is enforced before any store read: even an out-of-scope seed
        // gets the validation error, never a tenancy signal.
        for (query, expected) in [
            ("?depth=4", "depth_out_of_range"),
            ("?depth=0", "depth_out_of_range"),
            ("?depth=abc", "invalid_depth"),
            ("?depth=", "invalid_depth"),
        ] {
            let (status, body) = get(
                &app,
                &format!("/app/api/graph/project/{seed}{query}"),
                &bearer,
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {body:?}");
            assert_eq!(body["error"], expected, "{query}");
        }
    })
    .await;
}

#[tokio::test]
async fn a_matter_outside_the_callers_scope_is_a_bare_404() {
    store::test_support::with_required_postgres(async {
        let db = store::application_store::ApplicationStore::uninitialized();
        let outsider = lawyer(&db, "Outsider Lawyer").await;
        let matter = seed_matter(&db, "outgraph").await;
        let app = graph_app(db).await;
        let bearer = bearer(outsider.id, Role::Lawyer);

        let (status, body) = get(&app, &format!("/app/api/graph/project/{matter}"), &bearer).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body:?}");
        assert_eq!(body["error"], "not_found");
    })
    .await;
}

#[tokio::test]
async fn a_firm_view_outside_the_callers_directory_is_a_bare_404() {
    store::test_support::with_required_postgres(async {
        let db = store::application_store::ApplicationStore::uninitialized();
        // The firm's admin DRI holds the membership; the caller holds none.
        let member = admin(&db, "Firm Member").await;
        let firm = seed_firm(&db, "Out of Reach Firm", member.id).await;
        let outsider = lawyer(&db, "Firm Outsider").await;
        let app = graph_app(db).await;
        let bearer = bearer(outsider.id, Role::Lawyer);

        let (status, body) = get(&app, &format!("/app/api/graph/firm/{}", firm.id), &bearer).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body:?}");
        assert_eq!(body["error"], "not_found");
    })
    .await;
}

#[tokio::test]
async fn a_lawyer_view_of_another_firms_person_is_a_bare_404() {
    store::test_support::with_required_postgres(async {
        let db = store::application_store::ApplicationStore::uninitialized();
        let member = admin(&db, "Directed Admin").await;
        let target = lawyer(&db, "Directed Target").await;
        let firm = seed_firm(&db, "Directed Firm", member.id).await;
        store::firms::add_membership(
            &db,
            &store::firms::NewPersonFirmRole {
                person_id: target.id,
                firm_id: firm.id,
                membership: FirmMembership::Lawyer,
                is_dri: false,
            },
        )
        .await
        .unwrap();
        // The caller belongs to a different firm with no directory reach into
        // the target's.
        let other_member = admin(&db, "Other Firm Admin").await;
        let other_firm = seed_firm(&db, "Other Firm", other_member.id).await;
        let caller = lawyer(&db, "Unrelated Caller").await;
        store::firms::add_membership(
            &db,
            &store::firms::NewPersonFirmRole {
                person_id: caller.id,
                firm_id: other_firm.id,
                membership: FirmMembership::Lawyer,
                is_dri: false,
            },
        )
        .await
        .unwrap();
        let app = graph_app(db).await;
        let bearer = bearer(caller.id, Role::Lawyer);

        let (status, body) = get(
            &app,
            &format!("/app/api/graph/lawyer/{}", target.id),
            &bearer,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body:?}");
        assert_eq!(body["error"], "not_found");
    })
    .await;
}

/// The 200 path needs a configured PostgreSQL graph; without one the door
/// reports 503 `graph_requires_postgresql` rather than an empty neighborhood.
async fn assert_view_or_unconfigured(app: &axum::Router, path: &str, bearer: &str) {
    let (status, body) = get(app, path, bearer).await;
    // No PostgreSQL configured: the honest answer is 503, not 404 — the
    // caller was authorized; the store just is not there.
    if store::postgres::PostgresDb::from_env().await.is_err() {
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body:?}");
        assert_eq!(body["error"], "graph_requires_postgresql");
    } else {
        assert!(status.is_success(), "{body:?}");
        assert!(body["nodes"].is_array(), "{body:?}");
        assert!(body["edges"].is_array(), "{body:?}");
    }
}

#[tokio::test]
async fn a_lawyer_reads_their_own_neighborhood() {
    store::test_support::with_required_postgres(async {
        let db = store::application_store::ApplicationStore::uninitialized();
        let lawyer = lawyer(&db, "Self View Lawyer").await;
        let app = graph_app(db).await;
        let bearer = bearer(lawyer.id, Role::Lawyer);

        assert_view_or_unconfigured(
            &app,
            &format!("/app/api/graph/lawyer/{}", lawyer.id),
            &bearer,
        )
        .await;
    })
    .await;
}

#[tokio::test]
async fn a_directory_authorized_caller_reads_a_firm_view() {
    store::test_support::with_required_postgres(async {
        let db = store::application_store::ApplicationStore::uninitialized();
        let admin = admin(&db, "Firm Admin").await;
        let firm = seed_firm(&db, "Readable Firm", admin.id).await;
        let app = graph_app(db).await;
        let bearer = bearer(admin.id, Role::Admin);

        assert_view_or_unconfigured(&app, &format!("/app/api/graph/firm/{}", firm.id), &bearer)
            .await;
    })
    .await;
}

#[tokio::test]
async fn an_unauthenticated_or_client_caller_is_refused_before_scope() {
    store::test_support::with_required_postgres(async {
        let db = store::application_store::ApplicationStore::uninitialized();
        let matter = seed_matter(&db, "nograph").await;
        let client = store::persons::create(
            &db,
            &NewPerson::with_role("Graph Client", "graph-client@example.com", Role::Client),
        )
        .await
        .unwrap();
        let app = graph_app(db.clone()).await;

        let (status, body) = get(&app, &format!("/app/api/graph/project/{matter}"), "").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body:?}");

        let (status, body) = get(
            &app,
            &format!("/app/api/graph/project/{matter}"),
            &bearer(client.id, Role::Client),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body:?}");
    })
    .await;
}

/// A separate process guarantees no application PostgreSQL configuration or
/// inherited task-local pool, even when the parent runs the native fixtures.
#[tokio::test]
async fn unavailable_graph_answers_503_for_an_authorized_self_view() {
    const CHILD: &str = "NAV_GRAPH_UNAVAILABLE_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child.args([
            "--exact",
            "unavailable_graph_answers_503_for_an_authorized_self_view",
            "--nocapture",
        ]);
        child.env(CHILD, "1");
        for key in [
            "NAVIGATOR_POSTGRES_HOST",
            "NAVIGATOR_POSTGRES_PORT",
            "NAVIGATOR_POSTGRES_USER",
            "NAVIGATOR_POSTGRES_PASSWORD",
            "NAVIGATOR_POSTGRES_DATABASE",
            "NAVIGATOR_POSTGRES_SSLMODE",
            "NAVIGATOR_POSTGRES_ROOT_CERT",
        ] {
            child.env_remove(key);
        }
        assert!(child.status().unwrap().success());
        return;
    }
    let actor = Uuid::now_v7();
    let app = graph_app(store::application_store::ApplicationStore::uninitialized()).await;
    let (status, body) = get(
        &app,
        &format!("/app/api/graph/lawyer/{actor}"),
        &bearer(actor, Role::Lawyer),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body:?}");
    assert_eq!(body["error"], "graph_requires_postgresql");
}
