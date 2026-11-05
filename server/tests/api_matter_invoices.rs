#![allow(clippy::doc_markdown)]
//! Integration tests for the matter-invoice door: `POST
//! /app/api/projects/{id}/invoices`, the command that raises one invoice
//! across the billing provider seam from the matter's unbilled work.
//!
//! Like the task doors (`api_project_tasks.rs`), the store under test is
//! PostgreSQL-native, so these tests run on the PostgreSQL lane
//! (`NAVIGATOR_TEST_POSTGRES_PORT`): each test process builds one
//! canonical-schema database, points the app's shared `PostgresDb::from_env()`
//! pool at it, and drives the real handlers through the full router —
//! embedded Rego policy included. Without the lane they skip, unless
//! `NAV_REQUIRE_POSTGRES=1` turns the skip into a failure.
//!
//! The provider is the billing crate's stub, held by the fixture so the
//! assertions can count actual `create_invoice` calls — the idempotency
//! contract is about how many times the seam is crossed, not only what the
//! mirror shows. A dedicated failing provider covers the refusal path.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use billing::{
    BillingError, BillingProvider, ContactId, ContactRequest, InvoiceId, InvoiceRequest,
};
use chrono::{NaiveDate, Utc};
use http_body_util::BodyExt;
use portal::{AppState, SessionStore};
use store::persons::Role;
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

use portal::test_support::TEST_SESSION_KEY as KEY;

#[path = "common/bearer.rs"]
mod bearer_common;

use bearer_common::bearer;
const RATE_CENTS_PER_HOUR: i32 = 25_000;

static DATABASE: tokio::sync::OnceCell<Option<String>> = tokio::sync::OnceCell::const_new();

/// Create one canonical-schema database on the lane and point the app's
/// `PostgresDb::from_env()` pool at it. `None` when the lane is absent —
/// the test then skips.
async fn invoice_store() -> Option<&'static str> {
    DATABASE
        .get_or_init(|| async {
            let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
                assert_ne!(
                    std::env::var("NAV_REQUIRE_POSTGRES").as_deref(),
                    Ok("1"),
                    "the PostgreSQL integration lane requires NAVIGATOR_TEST_POSTGRES_PORT"
                );
                return None;
            };
            let admin_database = std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE")
                .unwrap_or_else(|_| "smoke".into());
            let options = sqlx::postgres::PgConnectOptions::new()
                .host("127.0.0.1")
                .port(port.parse().expect("the lane port is a number"))
                .username("postgres")
                .password("synthetic")
                .ssl_mode(sqlx::postgres::PgSslMode::Disable);
            let admin = sqlx::PgPool::connect_with(options.clone().database(&admin_database))
                .await
                .unwrap();
            let database = format!("invoice_api_test_{}", Uuid::now_v7().simple());
            sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
                .execute(&admin)
                .await
                .unwrap();
            admin.close().await;
            let setup = sqlx::PgPool::connect_with(options.database(&database))
                .await
                .unwrap();
            // The graph migration needs the AGE extension, which only a
            // superuser creates: the lane's superuser init path stands in
            // for the application's `roles.sql`.
            sqlx::raw_sql("CREATE EXTENSION IF NOT EXISTS age; LOAD 'age';")
                .execute(&setup)
                .await
                .unwrap();
            sqlx::raw_sql(store::postgres_schema::canonical_schema())
                .execute(&setup)
                .await
                .unwrap();
            setup.close().await;
            std::env::set_var("NAVIGATOR_POSTGRES_HOST", "127.0.0.1");
            std::env::set_var("NAVIGATOR_POSTGRES_PORT", &port);
            std::env::set_var("NAVIGATOR_POSTGRES_USER", "postgres");
            std::env::set_var("NAVIGATOR_POSTGRES_PASSWORD", "synthetic");
            std::env::set_var("NAVIGATOR_POSTGRES_DATABASE", &database);
            std::env::set_var("NAVIGATOR_POSTGRES_SSLMODE", "disable");
            Some(database)
        })
        .await
        .as_deref()
}

struct Fixture {
    app: axum::Router,
    surreal: store::surreal::SurrealDb,
    provider: Arc<billing::StubBillingProvider>,
    project_id: Uuid,
    other_project_id: Uuid,
    lawyer: String,
    lawyer_id: Uuid,
    client: String,
    outsider: String,
    clerk: String,
}

async fn seed_matter(surreal: &store::surreal::SurrealDb, code: &str) -> Uuid {
    // `seed_entity` deliberately points its `entity_type_id` and
    // `jurisdiction_id` at rows that exist nowhere: SurrealDB does not
    // validate a link. PostgreSQL does, so the pre-command mirror would
    // refuse the matter's entity — finding no source row for either
    // reference — and every invoice door would answer 503. The fixture
    // therefore resolves real identity rows, exactly as a production
    // matter carries.
    let jurisdiction = store::jurisdictions::create(
        surreal,
        &store::jurisdictions::NewJurisdiction::new(
            format!("Test Jurisdiction {code}"),
            format!("tj-{}", Uuid::now_v7().simple()),
            "state",
        ),
    )
    .await
    .unwrap();
    let entity_type = store::entity_types::create(surreal, &format!("Test Entity Type {code}"))
        .await
        .unwrap();
    let entity = store::entities::create(
        surreal,
        &store::entities::NewEntity {
            name: format!("Matter Entity {code}"),
            entity_type_id: entity_type.id,
            jurisdiction_id: jurisdiction.id,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .unwrap();
    let project = store::projects::create(
        surreal,
        &store::projects::NewProject {
            code: code.to_string(),
            name: "Matter".into(),
            status: "open".into(),
            entity_id: entity.id,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    project.id
}

/// A live project-scoped Document for a cost receipt — the row the cost
/// command validates against (`store::assets`, the same shape
/// `seed_body_asset` seeds, plus the matter link and kind).
async fn seed_receipt_asset(surreal: &store::surreal::SurrealDb, project_id: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    surreal
        .query(
            "CREATE $id SET project_id = $project, kind = 'receipt', \
             filename = 'receipt.pdf', storage_key = $key, \
             content_type = 'application/pdf', byte_size = 128, sha256_hex = $sha, \
             source = 'upload', visibility = 'internal', metadata = NONE",
        )
        .bind(("id", store::surreal::record_id("asset", id)))
        .bind(("project", store::surreal::record_id("project", project_id)))
        .bind(("key", format!("blobs/{}", Uuid::now_v7().simple())))
        .bind(("sha", store::assets::sha256_hex(b"receipt-bytes")))
        .await
        .unwrap()
        .check()
        .unwrap();
    id
}

/// One unbilled billable Time Entry on the matter, at the fixture's snapshot
/// rate — the input the hourly path composes.
async fn seed_entry(
    pg: &store::postgres::PostgresDb,
    project_id: Uuid,
    attorney_id: Uuid,
    minutes: i32,
    billable: bool,
) -> Uuid {
    store::time_entries::create(
        pg,
        project_id,
        attorney_id,
        &store::time_entries::NewTimeEntry {
            attorney_id,
            performed_on: NaiveDate::from_ymd_opt(2026, 10, 30).unwrap(),
            minutes,
            description: "Drafted the motion to compel".into(),
            billable,
            rate_cents_per_hour: RATE_CENTS_PER_HOUR,
            source: store::time_entries::TimeEntrySource::Manual,
        },
    )
    .await
    .unwrap()
    .id
}

/// One receipt-backed Project cost; `confirm` controls whether it is
/// confirmed (invoiced composition) or left estimated (excluded).
async fn seed_cost(
    surreal: &store::surreal::SurrealDb,
    pg: &store::postgres::PostgresDb,
    project_id: Uuid,
    actor_id: Uuid,
    amount_cents: i64,
    confirm: bool,
) -> store::project_costs::ProjectCost {
    let receipt_document_id = seed_receipt_asset(surreal, project_id).await;
    let cost = store::project_costs::create(
        surreal,
        pg,
        project_id,
        actor_id,
        &store::project_costs::NewProjectCost {
            occurred_on: NaiveDate::from_ymd_opt(2026, 10, 29).unwrap(),
            payee: "Synthetic Vendor".into(),
            description: "Filing copy".into(),
            amount_cents,
            currency: "USD".into(),
            amount_status: "estimated".into(),
            receipt_document_id,
            idempotency_key: format!("receipt:{}", Uuid::now_v7()),
            source_reference: store::project_costs::mercury_source_reference(Uuid::now_v7()),
        },
    )
    .await
    .unwrap();
    if confirm {
        return store::project_costs::confirm_amount(pg, cost.id, actor_id, amount_cents)
            .await
            .unwrap();
    }
    cost
}

async fn build_fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let storage: Arc<dyn cloud::StorageService> = Arc::new(
        cloud::FsStorage::new(
            std::env::temp_dir().join(format!("nav-api-invoices-{}", Uuid::now_v7())),
        )
        .await
        .unwrap(),
    );
    let project_id = seed_matter(&surreal, &format!("matter-{}", Uuid::now_v7())).await;
    let other_project_id = seed_matter(&surreal, &format!("other-{}", Uuid::now_v7())).await;
    let run = Uuid::now_v7().simple();
    let lawyer = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Lawyer",
            format!("lawyer-{run}@example.com"),
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    store::projects::add_participation(&surreal, project_id, lawyer.id, "lawyer")
        .await
        .unwrap();
    store::projects::add_participation(&surreal, other_project_id, lawyer.id, "lawyer")
        .await
        .unwrap();
    // Designate the lawyer DRI: the fixture's Postgres-native writes (time
    // entries, flat fees) demand a mirrored project.
    store::projects::designate_dri_in_surreal(
        &surreal,
        project_id,
        lawyer.id,
        store::projects::DriSide::Lawyer,
    )
    .await
    .unwrap();
    let client = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Client",
            format!("client-{run}@example.com"),
            Role::Client,
        ),
    )
    .await
    .unwrap();
    store::projects::add_participation(&surreal, project_id, client.id, "client")
        .await
        .unwrap();
    let outsider = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Outsider",
            format!("outsider-{run}@example.com"),
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    let clerk = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Clerk",
            format!("clerk-{run}@example.com"),
            Role::Clerk,
        ),
    )
    .await
    .unwrap();

    // The fixture holds the stub so the tests can count actual seam calls.
    let provider = Arc::new(billing::StubBillingProvider::for_simulated_matters());
    mirror_matters(&surreal, &[project_id, other_project_id]).await;
    let base = portal::test_support::app_state(surreal.clone()).await;
    let state = AppState {
        sessions: SessionStore::new(KEY),
        billing_provider: provider.clone(),
        storage,
        ..base
    };
    Fixture {
        app: server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR)),
        surreal,
        provider,
        project_id,
        other_project_id,
        lawyer: bearer(lawyer.id, Role::Lawyer),
        lawyer_id: lawyer.id,
        client: bearer(client.id, Role::Client),
        outsider: bearer(outsider.id, Role::Lawyer),
        clerk: bearer(clerk.id, Role::Clerk),
    }
}

/// Mirror the matters and their participations the way `prepared_store`
/// does before the app's own doors write PostgreSQL-native rows: the
/// tests below seed those rows directly (time entries, fees, costs), so
/// the stores must already be mirrored — no command runs first. Every
/// test that builds a fixture has run `invoice_store()`, so the pool
/// resolves.
async fn mirror_matters(surreal: &store::surreal::SurrealDb, projects: &[Uuid]) {
    let pg = store::postgres::PostgresDb::from_env().await.unwrap();
    for project in projects {
        store::postgres_mirror::mirror(
            surreal,
            &pg,
            &store::postgres_mirror::MirrorKey::Project(*project),
        )
        .await
        .unwrap();
        for role in store::projects::participations_for_project(surreal, *project)
            .await
            .unwrap()
        {
            store::postgres_mirror::mirror(
                surreal,
                &pg,
                &store::postgres_mirror::MirrorKey::ProjectParticipation(role.id),
            )
            .await
            .unwrap();
        }
    }
}

async fn post(fx: &Fixture, path: &str, auth: Option<&str>) -> axum::http::Response<Body> {
    let mut req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(auth) = auth {
        req = req.header("authorization", auth);
    }
    fx.app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn body_json(resp: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// The invoice-generation door, on the fixture's matter.
async fn generate(fx: &Fixture, auth: &str, project_id: Uuid) -> axum::http::Response<Body> {
    post(
        fx,
        &format!("/app/api/projects/{project_id}/invoices"),
        Some(auth),
    )
    .await
}

/// The matter's mirror rows.
async fn mirror_rows(
    surreal: &store::surreal::SurrealDb,
    project_id: Uuid,
) -> Vec<store::xero_invoices::XeroInvoice> {
    store::xero_invoices::for_projects(surreal, &[project_id])
        .await
        .unwrap()
}

/// The create call the stub recorded, if any — the count of actual seam
/// crossings.
fn create_calls(fx: &Fixture) -> Vec<(Uuid, InvoiceRequest)> {
    fx.provider
        .calls()
        .into_iter()
        .map(|call| (call.matter_id, call.request))
        .collect()
}

#[tokio::test]
async fn the_hourly_path_bills_time_and_confirmed_costs_once() {
    let Some(_) = invoice_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let pg = store::postgres::PostgresDb::from_env().await.unwrap();
    let e1 = seed_entry(&pg, fx.project_id, fx.lawyer_id, 90, true).await;
    let e2 = seed_entry(&pg, fx.project_id, fx.lawyer_id, 30, true).await;
    let _non_billable = seed_entry(&pg, fx.project_id, fx.lawyer_id, 60, false).await;
    let confirmed = seed_cost(&fx.surreal, &pg, fx.project_id, fx.lawyer_id, 1_250, true).await;
    let _estimated = seed_cost(&fx.surreal, &pg, fx.project_id, fx.lawyer_id, 9_999, false).await;

    let resp = generate(&fx, &fx.lawyer, fx.project_id).await;
    assert_eq!(resp.status(), StatusCode::OK, "the generation raises");
    let receipt = body_json(resp).await;

    // Two entries at their snapshotted rate (90 and 30 minutes — two
    // hours) plus the confirmed cost; the estimated cost and the
    // non-billable entry are nobody's invoice.
    let expected = 2 * i64::from(RATE_CENTS_PER_HOUR) + 1_250;
    assert_eq!(receipt["total_cents"], expected);
    assert_eq!(receipt["consumed_time_entries"], 2);
    assert_eq!(receipt["consumed_costs"], 1);
    assert_eq!(receipt["flat_fee"], false);

    // The Xero Reference names the matter — the ingest convention.
    let project = store::projects::find_by_id(&fx.surreal, fx.project_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt["reference"], format!("Matter {}", project.code));

    // One seam crossing, mirrored once.
    assert_eq!(create_calls(&fx).len(), 1);
    assert_eq!(mirror_rows(&fx.surreal, fx.project_id).await.len(), 1);

    // The consumed entries flipped to invoiced under the raised id — both of
    // them, never the non-billable one.
    for id in [e1, e2] {
        let entry = store::time_entries::find_by_id(&pg, id).await.unwrap();
        assert_eq!(entry.billing_status.as_str(), "invoiced");
    }
    let costs = store::project_costs::list_for_project(&pg, fx.project_id)
        .await
        .unwrap();
    let confirmed_after = costs.iter().find(|cost| cost.id == confirmed.id).unwrap();
    assert_eq!(confirmed_after.billing_status, "invoiced");
    assert_eq!(
        confirmed_after.xero_invoice_id.as_deref(),
        Some(receipt["xero_invoice_id"].as_str().unwrap())
    );

    // A repeat finds nothing unbilled: 409, and the seam was crossed once.
    let resp = generate(&fx, &fx.lawyer, fx.project_id).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(create_calls(&fx).len(), 1);
    assert_eq!(mirror_rows(&fx.surreal, fx.project_id).await.len(), 1);
}

#[tokio::test]
async fn a_flat_fee_bills_the_fee_and_consumes_the_entries() {
    let Some(_) = invoice_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let pg = store::postgres::PostgresDb::from_env().await.unwrap();
    store::attorney_rates::set_flat_fee(&pg, fx.project_id, 150_000)
        .await
        .unwrap();
    let entry = seed_entry(&pg, fx.project_id, fx.lawyer_id, 90, true).await;
    let cost = seed_cost(&fx.surreal, &pg, fx.project_id, fx.lawyer_id, 1_250, true).await;

    let resp = generate(&fx, &fx.lawyer, fx.project_id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let receipt = body_json(resp).await;
    assert_eq!(receipt["flat_fee"], true);
    assert_eq!(receipt["total_cents"], 151_250);
    assert_eq!(
        receipt["line_items"][0]["unit_amount_cents"], 150_000,
        "the fee is the first line, covering the entries"
    );

    // The entries are consumed under the fee invoice — never billed again —
    // and the cost rides along.
    let entry = store::time_entries::find_by_id(&pg, entry).await.unwrap();
    assert_eq!(entry.billing_status.as_str(), "invoiced");
    let costs = store::project_costs::list_for_project(&pg, fx.project_id)
        .await
        .unwrap();
    assert_eq!(
        costs
            .iter()
            .find(|c| c.id == cost.id)
            .unwrap()
            .billing_status,
        "invoiced"
    );

    // A repeat is the no-op: the fee is consumed and nothing is unbilled —
    // 409, and the seam was crossed exactly once, so the mirror still holds
    // one row under the first invoice's id.
    let resp = generate(&fx, &fx.lawyer, fx.project_id).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let again = body_json(resp).await;
    assert_eq!(again["reason"], "nothing_to_invoice");
    assert_eq!(mirror_rows(&fx.surreal, fx.project_id).await.len(), 1);
}

#[tokio::test]
async fn hourly_billing_refuses_under_a_consumed_flat_fee() {
    let Some(_) = invoice_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let pg = store::postgres::PostgresDb::from_env().await.unwrap();
    store::attorney_rates::set_flat_fee(&pg, fx.project_id, 150_000)
        .await
        .unwrap();
    seed_entry(&pg, fx.project_id, fx.lawyer_id, 90, true).await;
    let resp = generate(&fx, &fx.lawyer, fx.project_id).await;
    assert_eq!(resp.status(), StatusCode::OK, "the fee bills itself");

    // Later work is covered by the fee agreement: billing it at hourly
    // rates would charge the client twice for the same agreement.
    seed_entry(&pg, fx.project_id, fx.lawyer_id, 30, true).await;
    let resp = generate(&fx, &fx.lawyer, fx.project_id).await;
    let status = resp.status();
    let again = body_json(resp).await;
    assert_eq!(status, StatusCode::CONFLICT, "{again}");
    assert_eq!(again["reason"], "flat_fee_already_billed");
}

#[tokio::test]
async fn already_invoiced_entries_are_skipped() {
    let Some(_) = invoice_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let pg = store::postgres::PostgresDb::from_env().await.unwrap();
    let already = seed_entry(&pg, fx.project_id, fx.lawyer_id, 90, true).await;
    seed_entry(&pg, fx.project_id, fx.lawyer_id, 30, true).await;

    // Partial state: the first entry is already under an earlier invoice —
    // the way a lawyer-raised Xero invoice reconciles onto the mirror.
    let earlier = format!("xero-earlier-{}", Uuid::now_v7().simple());
    store::xero_invoices::upsert(
        &fx.surreal,
        &store::xero_invoices::UpsertXeroInvoice {
            project_id: fx.project_id,
            xero_invoice_id: earlier.clone(),
            reference: format!("Matter earlier-{}", Uuid::now_v7().simple()),
            status: "AUTHORISED".into(),
            amount_cents: 3 * i64::from(RATE_CENTS_PER_HOUR) / 2,
            currency: "USD".into(),
            issued_at: Utc::now(),
            due_at: None,
        },
    )
    .await
    .unwrap();
    store::time_entries::mark_invoiced(&pg, &[already], fx.lawyer_id, &earlier, false)
        .await
        .unwrap();

    let resp = generate(&fx, &fx.lawyer, fx.project_id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let receipt = body_json(resp).await;
    assert_eq!(receipt["consumed_time_entries"], 1, "only the unbilled one");
    assert_eq!(
        receipt["total_cents"],
        i64::from(RATE_CENTS_PER_HOUR) / 2,
        "the composition covers the fresh entry alone"
    );

    // The already-invoiced entry stays exactly where it was.
    let entry = store::time_entries::find_by_id(&pg, already).await.unwrap();
    assert_eq!(entry.billing_status.as_str(), "invoiced");
}

#[tokio::test]
async fn nothing_unbilled_is_a_no_op() {
    let Some(_) = invoice_store().await else {
        return;
    };
    let fx = build_fixture().await;

    // A quiet matter: the guard refuses before the seam is ever crossed —
    // the zero-unbilled no-op.
    let resp = generate(&fx, &fx.lawyer, fx.project_id).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert!(
        create_calls(&fx).is_empty(),
        "the provider was never called"
    );
    assert!(mirror_rows(&fx.surreal, fx.project_id).await.is_empty());
}

#[tokio::test]
async fn a_matter_with_no_client_to_bill_is_refused() {
    let Some(_) = invoice_store().await else {
        return;
    };
    // The refusal test's fixture, minus the client-side participation: the
    // composition resolves the billed party from the matter's client
    // participation, so a matter with unbilled work but nobody to bill
    // refuses before the seam is ever crossed.
    let surreal = mem_surreal().await;
    let storage: Arc<dyn cloud::StorageService> = Arc::new(
        cloud::FsStorage::new(
            std::env::temp_dir().join(format!("nav-invoice-no-party-{}", Uuid::now_v7())),
        )
        .await
        .unwrap(),
    );
    let project_id = seed_matter(&surreal, &format!("matter-{}", Uuid::now_v7())).await;
    let lawyer = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Lawyer",
            format!("lawyer-{}@example.com", Uuid::now_v7().simple()),
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    store::projects::add_participation(&surreal, project_id, lawyer.id, "lawyer")
        .await
        .unwrap();
    mirror_matters(&surreal, &[project_id]).await;
    let base = portal::test_support::app_state(surreal.clone()).await;
    // One shared stub: the app's provider IS the fixture's, so the
    // create-call assertions below verify what the application did.
    let provider = Arc::new(billing::StubBillingProvider::for_simulated_matters());
    let state = AppState {
        sessions: SessionStore::new(KEY),
        billing_provider: provider.clone(),
        storage,
        ..base
    };
    let fx = Fixture {
        app: server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR)),
        surreal,
        provider,
        project_id,
        other_project_id: project_id,
        lawyer: bearer(lawyer.id, Role::Lawyer),
        lawyer_id: lawyer.id,
        client: bearer(lawyer.id, Role::Client),
        outsider: bearer(lawyer.id, Role::Lawyer),
        clerk: bearer(lawyer.id, Role::Clerk),
    };
    let pg = store::postgres::PostgresDb::from_env().await.unwrap();
    seed_entry(&pg, project_id, lawyer.id, 90, true).await;

    let resp = generate(&fx, &fx.lawyer, project_id).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(resp).await;
    assert_eq!(body["reason"], "no_billed_party");
    assert!(
        create_calls(&fx).is_empty(),
        "the provider was never called"
    );
    assert!(mirror_rows(&fx.surreal, project_id).await.is_empty());
}

#[tokio::test]
async fn a_cost_in_another_currency_is_refused() {
    let Some(_) = invoice_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let pg = store::postgres::PostgresDb::from_env().await.unwrap();
    // A confirmed, unbilled cost in euros: the matter bills in USD, so the
    // composition refuses the raise rather than silently converting.
    let receipt_document_id = seed_receipt_asset(&fx.surreal, fx.project_id).await;
    let cost = store::project_costs::create(
        &fx.surreal,
        &pg,
        fx.project_id,
        fx.lawyer_id,
        &store::project_costs::NewProjectCost {
            occurred_on: NaiveDate::from_ymd_opt(2026, 10, 29).unwrap(),
            payee: "Synthetic Vendor".into(),
            description: "Filing copy".into(),
            amount_cents: 41_00,
            currency: "EUR".into(),
            amount_status: "estimated".into(),
            receipt_document_id,
            idempotency_key: format!("receipt:{}", Uuid::now_v7()),
            source_reference: store::project_costs::mercury_source_reference(Uuid::now_v7()),
        },
    )
    .await
    .unwrap();
    store::project_costs::confirm_amount(&pg, cost.id, fx.lawyer_id, 41_00)
        .await
        .unwrap();

    let resp = generate(&fx, &fx.lawyer, fx.project_id).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(resp).await;
    assert_eq!(body["reason"], "currency_mismatch");
    assert!(
        create_calls(&fx).is_empty(),
        "the provider was never called"
    );

    // The euro cost is exactly where it started: confirmed and unbilled.
    let cost = store::project_costs::find_by_id(&pg, cost.id)
        .await
        .unwrap();
    assert_eq!(cost.billing_status.as_str(), "unbilled");
}

#[tokio::test]
async fn a_provider_refusal_marks_nothing() {
    let Some(_) = invoice_store().await else {
        return;
    };
    let surreal = mem_surreal().await;
    let storage: Arc<dyn cloud::StorageService> = Arc::new(
        cloud::FsStorage::new(
            std::env::temp_dir().join(format!("nav-invoice-fail-{}", Uuid::now_v7())),
        )
        .await
        .unwrap(),
    );
    let project_id = seed_matter(&surreal, &format!("matter-{}", Uuid::now_v7())).await;
    let lawyer = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Lawyer",
            format!("lawyer-{}@example.com", Uuid::now_v7().simple()),
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    store::projects::add_participation(&surreal, project_id, lawyer.id, "lawyer")
        .await
        .unwrap();
    // Someone to bill: the composition resolves the billed party from the
    // matter's client-side participation before the seam is crossed.
    let client = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Client",
            format!("client-{}@example.com", Uuid::now_v7().simple()),
            Role::Client,
        ),
    )
    .await
    .unwrap();
    store::projects::add_participation(&surreal, project_id, client.id, "client")
        .await
        .unwrap();
    // Prepare the PostgreSQL side the way `prepared_store` does for the
    // app's own doors: this test seeds Postgres-native rows directly, so
    // the matter and its participations must already be mirrored.
    mirror_matters(&surreal, &[project_id]).await;

    // A provider that refuses at the raise: the seam is crossed, Xero
    // (synthetically) rejects, and the retry must be safe. The same
    // instance is the app's provider — its raise counter is what the
    // assertions below read, not a separate stub's log.
    let raises = Arc::new(Mutex::new(0_usize));
    let base = portal::test_support::app_state(surreal.clone()).await;
    let state = AppState {
        sessions: SessionStore::new(KEY),
        billing_provider: Arc::new(FailingProvider {
            raises: Arc::clone(&raises),
        }),
        storage,
        ..base
    };
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    let fx = Fixture {
        app,
        surreal,
        provider: Arc::new(billing::StubBillingProvider::for_simulated_matters()),
        project_id,
        other_project_id: project_id,
        lawyer: bearer(lawyer.id, Role::Lawyer),
        lawyer_id: lawyer.id,
        client: bearer(lawyer.id, Role::Client),
        outsider: bearer(lawyer.id, Role::Lawyer),
        clerk: bearer(lawyer.id, Role::Clerk),
    };
    let pg = store::postgres::PostgresDb::from_env().await.unwrap();
    let entry = seed_entry(&pg, project_id, lawyer.id, 90, true).await;
    // A confirmed cost rides along and remains reserved for the retry.
    seed_cost(&fx.surreal, &pg, project_id, lawyer.id, 1_250, true).await;

    let resp = generate(&fx, &fx.lawyer, project_id).await;
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        *raises.lock().unwrap(),
        1,
        "the refused invoice creation crossed the app's provider exactly once"
    );
    assert!(
        mirror_rows(&fx.surreal, project_id).await.is_empty(),
        "nothing is mirrored from a refused raise"
    );

    let batch_id = assert_reserved_batch(&fx, &pg, entry).await;

    // A retry crosses the seam again — the refusal is not sticky — and
    // still marks nothing.
    let retry = generate(&fx, &fx.lawyer, project_id).await;
    assert_eq!(retry.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(*raises.lock().unwrap(), 2, "a retry crosses the seam again");
    assert_eq!(assert_reserved_batch(&fx, &pg, entry).await, batch_id);
}

// A refused create preserves one reserved batch without an invoice; a
// retry must neither release its work nor compose a different intent.
async fn assert_reserved_batch(
    fx: &Fixture,
    pg: &store::postgres::PostgresDb,
    entry_id: Uuid,
) -> Uuid {
    assert!(store::time_entries::unbilled_billable(pg, fx.project_id)
        .await
        .unwrap()
        .is_empty());
    let entry = store::time_entries::find_by_id(pg, entry_id).await.unwrap();
    assert_eq!(entry.billing_status.as_str(), "reserved");
    let costs = store::project_costs::list_for_project(pg, fx.project_id)
        .await
        .unwrap();
    assert_eq!(costs.len(), 1);
    assert_eq!(costs[0].billing_status, "reserved");
    let batch = store::billing_batches::pending_for_project(pg, fx.project_id)
        .await
        .unwrap()
        .unwrap();
    assert!(batch.xero_invoice_id.is_none());
    assert!(mirror_rows(&fx.surreal, fx.project_id).await.is_empty());
    batch.id
}

#[tokio::test]
async fn outsiders_and_clients_cannot_generate() {
    let Some(_) = invoice_store().await else {
        return;
    };
    let fx = build_fixture().await;

    // A lawyer without a participation row: 404, the matter does not exist
    // for them — never a 403 naming it.
    assert_eq!(
        generate(&fx, &fx.outsider, fx.project_id).await.status(),
        StatusCode::NOT_FOUND
    );
    // A client-side participant is not lawyer tier: route admission refuses
    // before the matter question is ever asked.
    assert_eq!(
        generate(&fx, &fx.client, fx.project_id).await.status(),
        StatusCode::FORBIDDEN
    );
    // A clerk is the same refusal; no session at all is unauthorized.
    assert_eq!(
        generate(&fx, &fx.clerk, fx.project_id).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(
            &fx,
            &format!("/app/api/projects/{}/invoices", fx.project_id),
            None
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    // The lawyer is on the other matter too, so that door answers for
    // itself — its own quiet-matter 409, never a 404 for the first matter's
    // sake.
    assert_eq!(
        generate(&fx, &fx.lawyer, fx.other_project_id)
            .await
            .status(),
        StatusCode::CONFLICT,
        "the lawyer is on both matters, so the other matter answers itself"
    );
}

/// A provider whose invoice creation always refuses — the synthetic outage
/// at the Xero write. The generation must surface it as 502 and mark
/// nothing. The shared counter records the actual `create_invoice` calls so
/// the assertions reflect the provider the application actually used.
struct FailingProvider {
    raises: Arc<Mutex<usize>>,
}

#[async_trait]
impl BillingProvider for FailingProvider {
    fn invoices_available(&self) -> bool {
        true
    }
    async fn ensure_contact(&self, _request: &ContactRequest) -> Result<ContactId, BillingError> {
        Ok(ContactId(format!("contact-{}", Uuid::now_v7().simple())))
    }

    async fn create_invoice(
        &self,
        _matter_id: Uuid,
        _request: &InvoiceRequest,
    ) -> Result<InvoiceId, BillingError> {
        *self.raises.lock().unwrap() += 1;
        Err(BillingError::Provider("synthetic outage".into()))
    }

    async fn get_invoice(&self, _invoice_id: &str) -> Result<billing::InvoiceStatus, BillingError> {
        Err(BillingError::Provider("synthetic outage".into()))
    }

    async fn online_invoice_url(&self, _invoice_id: &str) -> Result<Option<String>, BillingError> {
        Err(BillingError::Provider("synthetic outage".into()))
    }

    async fn create_repeating_invoice(
        &self,
        _key: Uuid,
        _request: &billing::RepeatingInvoiceRequest,
    ) -> Result<String, BillingError> {
        Err(BillingError::Provider("synthetic outage".into()))
    }

    async fn list_receivable_invoices(
        &self,
    ) -> Result<Vec<billing::ReceivableInvoice>, BillingError> {
        Err(BillingError::Provider("synthetic outage".into()))
    }

    async fn list_bank_accounts(&self) -> Result<Vec<billing::TrustBankAccount>, BillingError> {
        Err(BillingError::Provider("synthetic outage".into()))
    }

    async fn list_trust_transactions(
        &self,
    ) -> Result<Vec<billing::TrustBankTransaction>, BillingError> {
        Err(BillingError::Provider("synthetic outage".into()))
    }
}
