//! Matter invoice generation: `POST /app/api/projects/{id}/invoices` — the
//! first production caller of the [`billing::BillingProvider`] seam beyond
//! the purchases flow.
//!
//! The proofs the generation contract rests on: the flat-fee path bills the
//! fee and consumes the work once; the hourly path aggregates time at its
//! snapshotted rates plus confirmed costs, and a second generation bills only
//! the work logged since; a refused provider leaves the batch's work
//! reserved (a write-off cannot reach it) and the retry resumes the same
//! batch under the same idempotency key, so one invoice lands, never two; a
//! matter with nothing unbilled never reaches the provider at all.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use chrono::NaiveDate;
use portal::api::ApiState;
use portal::billing::{
    BillingError, BillingProvider, ContactId, ContactRequest, InvoiceId, InvoiceRequest,
    InvoiceStatus, ReceivableInvoice, RepeatingInvoiceRequest, StubBillingProvider,
};
use serde_json::{json, Value};
use sqlx::{
    postgres::{PgConnectOptions, PgSslMode},
    PgPool,
};
use store::persons::{NewPerson, Role};
use store::postgres_mirror::{self, MirrorKey};
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

/// A provider that delegates everything to the idempotent stub but can fail
/// its next `create_invoice` — the synthetic Xero outage the retry tests run
/// against. The stub underneath re-mints the first attempt's id per key, so
/// the resumed batch's mirror row proves the seam held the line.
#[derive(Clone, Default)]
struct FlakyProvider {
    inner: Arc<StubBillingProvider>,
    fail_next: Arc<Mutex<bool>>,
    create_calls: Arc<Mutex<Vec<Uuid>>>,
}

impl FlakyProvider {
    fn fail_next(&self) {
        *self.fail_next.lock().unwrap() = true;
    }

    fn create_keys(&self) -> Vec<Uuid> {
        self.create_calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl BillingProvider for FlakyProvider {
    async fn ensure_contact(&self, request: &ContactRequest) -> Result<ContactId, BillingError> {
        self.inner.ensure_contact(request).await
    }

    async fn create_invoice(
        &self,
        matter_id: Uuid,
        request: &InvoiceRequest,
    ) -> Result<InvoiceId, BillingError> {
        self.create_calls.lock().unwrap().push(matter_id);
        let failing = {
            let mut next = self.fail_next.lock().unwrap();
            std::mem::take(&mut *next)
        };
        if failing {
            return Err(BillingError::Provider("synthetic xero outage".into()));
        }
        self.inner.create_invoice(matter_id, request).await
    }

    async fn get_invoice(&self, invoice_id: &str) -> Result<InvoiceStatus, BillingError> {
        self.inner.get_invoice(invoice_id).await
    }

    async fn online_invoice_url(&self, invoice_id: &str) -> Result<Option<String>, BillingError> {
        self.inner.online_invoice_url(invoice_id).await
    }

    async fn create_repeating_invoice(
        &self,
        key: Uuid,
        request: &RepeatingInvoiceRequest,
    ) -> Result<String, BillingError> {
        self.inner.create_repeating_invoice(key, request).await
    }

    async fn list_receivable_invoices(&self) -> Result<Vec<ReceivableInvoice>, BillingError> {
        self.inner.list_receivable_invoices().await
    }

    async fn list_bank_accounts(
        &self,
    ) -> Result<Vec<portal::billing::TrustBankAccount>, BillingError> {
        self.inner.list_bank_accounts().await
    }

    async fn list_trust_transactions(
        &self,
    ) -> Result<Vec<portal::billing::TrustBankTransaction>, BillingError> {
        self.inner.list_trust_transactions().await
    }
}

struct Fixture {
    app: Router,
    provider: FlakyProvider,
    pg: store::postgres::PostgresDb,
    surreal: store::surreal::SurrealDb,
    assets_storage: std::sync::Arc<dyn cloud::StorageService>,
    project_id: Uuid,
    lawyer: portal::SessionData,
    lawyer_id: Uuid,
}

async fn fixture() -> Fixture {
    let pg = store::postgres::PostgresDb::from_env().await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    let surreal = mem_surreal().await;
    let project_id = source_project(&surreal).await;
    let client = store::persons::create(
        &surreal,
        &NewPerson::with_role(
            "Synthetic Billing Client",
            "billing-client@example.com",
            Role::Client,
        ),
    )
    .await
    .unwrap();
    store::participation::add_participant(
        &surreal,
        &store::participation::AddParticipantCommand {
            project_id,
            person_id: client.id,
            dri: store::participation::DriRequest::Unchanged,
            actor: store::participation::DriActor::System,
        },
    )
    .await
    .unwrap();
    let lawyer = store::persons::create(
        &surreal,
        &NewPerson::with_role("Billing Lawyer", "billing-lawyer@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    store::participation::add_participant(
        &surreal,
        &store::participation::AddParticipantCommand {
            project_id,
            person_id: lawyer.id,
            dri: store::participation::DriRequest::Unchanged,
            actor: store::participation::DriActor::System,
        },
    )
    .await
    .unwrap();
    // The invoice route mirrors the project and its participations before
    // the provider runs; the fixture seeds the same rows so the store-level
    // helpers below can be called directly.
    mirror_project(&surreal, &pg, project_id).await;

    let provider = FlakyProvider::default();
    let state = portal::test_support::app_state(surreal).await;
    let app = portal::api::routes().with_state(ApiState {
        surreal: state.surreal.clone(),
        email: state.email.clone(),
        bootstrap_owner_email: state.bootstrap_owner_email.clone(),
        bootstrap_company: "Synthetic Firm".into(),
        questionnaire_runtime: state.questionnaire_runtime.clone(),
        storage: state.storage.clone(),
        workflow_runtime: state.workflow_runtime.clone(),
        assets_storage: state.assets_storage.clone(),
        forms_registry: state.forms_registry.clone(),
        signature_provider: state.signature_provider.clone(),
        contract_reviewer: state.contract_reviewer.clone(),
        integration_providers: state.integration_providers.clone(),
        billing_provider: Arc::new(provider.clone()),
        canonical_base_url: None,
    });
    Fixture {
        app,
        provider,
        pg,
        surreal: state.surreal.clone(),
        assets_storage: state.assets_storage.clone(),
        project_id,
        lawyer: portal::SessionData {
            person_id: Some(lawyer.id),
            ..portal::SessionData::fresh("billing-lawyer@example.com", Role::Lawyer)
        },
        lawyer_id: lawyer.id,
    }
}

async fn mirror_project(
    surreal: &store::surreal::SurrealDb,
    pg: &store::postgres::PostgresDb,
    project_id: Uuid,
) {
    postgres_mirror::mirror(surreal, pg, &MirrorKey::Project(project_id))
        .await
        .unwrap();
    let participations = store::projects::participations_for_project(surreal, project_id)
        .await
        .unwrap();
    for role in participations {
        postgres_mirror::mirror(surreal, pg, &MirrorKey::ProjectParticipation(role.id))
            .await
            .unwrap();
    }
}

async fn source_project(surreal: &store::surreal::SurrealDb) -> Uuid {
    let jurisdiction = store::jurisdictions::create(
        surreal,
        &store::jurisdictions::NewJurisdiction::new(
            "Synthetic billing jurisdiction",
            format!("BILL-{}", Uuid::new_v4()),
            "state",
        ),
    )
    .await
    .unwrap();
    let entity_type = store::entity_types::create(
        surreal,
        &format!("Synthetic billing entity type {}", Uuid::new_v4()),
    )
    .await
    .unwrap();
    let entity = store::entities::create(
        surreal,
        &store::entities::NewEntity {
            name: "Synthetic Billing Matter Entity".into(),
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
            code: store::projects::code_from_name("Billing API matter", Uuid::new_v4()),
            name: "Billing API matter".into(),
            status: "open".into(),
            entity_id: entity.id,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    project.id
}

async fn post_json(
    app: &Router,
    uri: &str,
    body: Value,
    session: &portal::SessionData,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .extension(session.clone())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn generate(app: &Router, fixture: &Fixture) -> axum::response::Response {
    post_json(
        app,
        &format!("/app/api/projects/{}/invoices", fixture.project_id),
        json!({}),
        &fixture.lawyer,
    )
    .await
}

async fn seed_entry(
    fixture: &Fixture,
    minutes: i32,
    description: &str,
) -> store::time_entries::TimeEntry {
    store::time_entries::create(
        &fixture.pg,
        fixture.project_id,
        fixture.lawyer_id,
        &store::time_entries::NewTimeEntry {
            attorney_id: fixture.lawyer_id,
            performed_on: NaiveDate::from_ymd_opt(2026, 10, 20).unwrap(),
            minutes,
            description: description.to_string(),
            billable: true,
            rate_cents_per_hour: 15_000,
            source: store::time_entries::TimeEntrySource::Manual,
        },
    )
    .await
    .unwrap()
}

async fn seed_confirmed_cost(fixture: &Fixture, amount_cents: i64) -> Uuid {
    let receipt = store::documents::ingest_bytes_as(
        &fixture.surreal,
        &fixture.assets_storage,
        &store::documents::IngestArgs {
            project_id: fixture.project_id,
            source: store::documents::source::UPLOAD,
            filename: "receipt.pdf",
            kind: "exhibit",
            content_type: "application/pdf",
            description: Some("Synthetic filing receipt"),
            visibility: store::documents::visibility::CLIENT,
            secondary_storage_key: None,
        },
        &store::documents::DocumentIdentity::default(),
        b"synthetic receipt bytes",
    )
    .await
    .unwrap();
    let cost = store::project_costs::create(
        &fixture.surreal,
        &fixture.pg,
        fixture.project_id,
        fixture.lawyer_id,
        &store::project_costs::NewProjectCost {
            occurred_on: NaiveDate::from_ymd_opt(2026, 10, 18).unwrap(),
            payee: "Synthetic Vendor".into(),
            description: "Synthetic filing copy".into(),
            amount_cents,
            currency: "USD".into(),
            amount_status: "estimated".into(),
            receipt_document_id: receipt.asset_id,
            idempotency_key: format!("synthetic-{}", Uuid::new_v4()),
            source_reference: format!("mercury::{}", Uuid::new_v4()),
        },
    )
    .await
    .unwrap();
    store::project_costs::confirm_amount(&fixture.pg, cost.id, fixture.lawyer_id, amount_cents)
        .await
        .unwrap();
    cost.id
}

async fn time_entry_status(pg: &store::postgres::PostgresDb, id: Uuid) -> String {
    store::time_entries::find_by_id(pg, id)
        .await
        .unwrap()
        .billing_status
        .as_str()
        .to_string()
}

async fn time_entry_invoice(pg: &store::postgres::PostgresDb, id: Uuid) -> Option<String> {
    match &store::time_entries::find_by_id(pg, id)
        .await
        .unwrap()
        .billing_status
    {
        store::time_entries::BillingStatus::Invoiced { xero_invoice_id } => {
            Some(xero_invoice_id.clone())
        }
        _ => None,
    }
}

async fn entry_audit_kinds(pg: &store::postgres::PostgresDb, id: Uuid) -> Vec<String> {
    store::time_entries::list_events(pg, id)
        .await
        .unwrap()
        .into_iter()
        .map(|event| event.event_kind)
        .collect()
}

async fn cost_status(pg: &store::postgres::PostgresDb, project_id: Uuid, id: Uuid) -> String {
    store::project_costs::list_for_project(pg, project_id)
        .await
        .unwrap()
        .into_iter()
        .find(|cost| cost.id == id)
        .unwrap()
        .billing_status
}

async fn mirror_rows(
    surreal: &store::surreal::SurrealDb,
    project_id: Uuid,
) -> Vec<store::xero_invoices::XeroInvoice> {
    store::xero_invoices::for_projects(surreal, &[project_id])
        .await
        .unwrap()
}

async fn pending_batch(
    pg: &store::postgres::PostgresDb,
    project_id: Uuid,
) -> Option<store::billing_batches::MatterBillingBatch> {
    store::billing_batches::pending_for_project(pg, project_id)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_flat_fee_bills_the_fee_and_consumes_the_work() {
    if std::env::var("NAV_BILLING_API_CHILD").as_deref() == Ok("1") {
        Box::pin(run_flat_fee()).await;
        return;
    }
    let Some((port, database)) = isolated_database().await else {
        return;
    };
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_flat_fee_bills_the_fee_and_consumes_the_work",
            "--nocapture",
        ])
        .env("NAV_BILLING_API_CHILD", "1")
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", &port)
        .env("NAVIGATOR_POSTGRES_USER", "postgres")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", &database)
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .status()
        .unwrap();
    remove_isolated_database(&port, &database).await;
    assert!(status.success());
}

async fn run_flat_fee() {
    let fixture = fixture().await;
    store::attorney_rates::set_flat_fee(&fixture.pg, fixture.project_id, 500_000)
        .await
        .unwrap();
    let entry = seed_entry(&fixture, 60, "Synthetic research").await;
    let cost_id = seed_confirmed_cost(&fixture, 12_340).await;

    // A lawyer with no participation row gets the non-disclosing 404 —
    // the seam's matter-scope question runs before anything else.
    let outsider = post_json(
        &fixture.app,
        &format!("/app/api/projects/{}/invoices", Uuid::new_v4()),
        json!({}),
        &fixture.lawyer,
    )
    .await;
    assert_eq!(outsider.status(), StatusCode::NOT_FOUND);

    let response = generate(&fixture.app, &fixture).await;
    assert_eq!(response.status(), StatusCode::OK);
    let receipt: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["flat_fee"], json!(true));
    assert_eq!(receipt["total_cents"], json!(512_340));
    assert_eq!(receipt["consumed_time_entries"], json!(1));
    assert_eq!(receipt["consumed_costs"], json!(1));
    // The fee is the line; the entries are covered by it, not itemized.
    assert_eq!(receipt["line_items"].as_array().unwrap().len(), 2);

    let invoice_id = receipt["xero_invoice_id"].as_str().unwrap().to_string();
    assert_eq!(time_entry_status(&fixture.pg, entry.id).await, "invoiced");
    assert_eq!(
        time_entry_invoice(&fixture.pg, entry.id).await,
        Some(invoice_id.clone())
    );
    assert_eq!(
        cost_status(&fixture.pg, fixture.project_id, cost_id).await,
        "invoiced"
    );
    let rows = mirror_rows(&fixture.surreal, fixture.project_id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].xero_invoice_id, invoice_id);
    assert_eq!(rows[0].amount_cents, 512_340);
    assert!(pending_batch(&fixture.pg, fixture.project_id)
        .await
        .is_none());
    // Work consumed under the fee is never billed again: the repeat is the
    // zero-unbilled no-op, not a second invoice.
    let repeat = generate(&fixture.app, &fixture).await;
    assert_eq!(repeat.status(), StatusCode::CONFLICT);
    assert_eq!(fixture.provider.create_keys().len(), 1);
}

#[tokio::test]
async fn an_hourly_invoice_aggregates_time_at_snapshot_rates_and_new_work_gets_the_next_invoice() {
    if std::env::var("NAV_BILLING_API_CHILD").as_deref() == Ok("1") {
        Box::pin(run_hourly_split()).await;
        return;
    }
    let Some((port, database)) = isolated_database().await else {
        return;
    };
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "an_hourly_invoice_aggregates_time_at_snapshot_rates_and_new_work_gets_the_next_invoice", "--nocapture"])
        .env("NAV_BILLING_API_CHILD", "1")
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", &port)
        .env("NAVIGATOR_POSTGRES_USER", "postgres")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", &database)
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .status()
        .unwrap();
    remove_isolated_database(&port, &database).await;
    assert!(status.success());
}

async fn run_hourly_split() {
    let fixture = fixture().await;
    let first = seed_entry(&fixture, 90, "Synthetic drafting").await;
    let second = seed_entry(&fixture, 60, "Synthetic research").await;
    let cost_id = seed_confirmed_cost(&fixture, 12_340).await;

    let response = generate(&fixture.app, &fixture).await;
    assert_eq!(response.status(), StatusCode::OK);
    let receipt: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    // 90 min at 150.00/h + 60 min at 150.00/h + the cost, integer cents:
    // 22_500 + 15_000 + 12_340.
    assert_eq!(receipt["flat_fee"], json!(false));
    assert_eq!(receipt["total_cents"], json!(49_840));
    assert_eq!(receipt["line_items"].as_array().unwrap().len(), 3);
    let first_invoice = receipt["xero_invoice_id"].as_str().unwrap().to_string();
    assert_eq!(time_entry_status(&fixture.pg, first.id).await, "invoiced");
    assert_eq!(time_entry_status(&fixture.pg, second.id).await, "invoiced");
    assert_eq!(
        cost_status(&fixture.pg, fixture.project_id, cost_id).await,
        "invoiced"
    );

    // Work logged after the invoice is what the next one draws from — the
    // settled batch never joins a later composition.
    seed_entry(&fixture, 30, "Synthetic review").await;
    let next = generate(&fixture.app, &fixture).await;
    assert_eq!(next.status(), StatusCode::OK);
    let next_receipt: Value = serde_json::from_slice(
        &axum::body::to_bytes(next.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(next_receipt["total_cents"], json!(7_500));
    assert_eq!(next_receipt["consumed_time_entries"], json!(1));
    assert_ne!(
        next_receipt["xero_invoice_id"].as_str().unwrap(),
        first_invoice
    );
    assert_eq!(
        mirror_rows(&fixture.surreal, fixture.project_id)
            .await
            .len(),
        2
    );
    // Two generations, two batch keys — the idempotency key is the batch,
    // not the matter, so successive invoices never collide.
    let keys = fixture.provider.create_keys();
    assert_eq!(keys.len(), 2);
    assert_ne!(keys[0], keys[1]);
}

#[tokio::test]
async fn a_refused_provider_leaves_the_work_reserved_and_a_retry_bills_it_once() {
    if std::env::var("NAV_BILLING_API_CHILD").as_deref() == Ok("1") {
        Box::pin(run_refusal_and_retry()).await;
        return;
    }
    let Some((port, database)) = isolated_database().await else {
        return;
    };
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_refused_provider_leaves_the_work_reserved_and_a_retry_bills_it_once",
            "--nocapture",
        ])
        .env("NAV_BILLING_API_CHILD", "1")
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", &port)
        .env("NAVIGATOR_POSTGRES_USER", "postgres")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", &database)
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .status()
        .unwrap();
    remove_isolated_database(&port, &database).await;
    assert!(status.success());
}

async fn run_refusal_and_retry() {
    let fixture = fixture().await;
    let entry = seed_entry(&fixture, 60, "Synthetic research").await;
    let cost_id = seed_confirmed_cost(&fixture, 5_000).await;

    fixture.provider.fail_next();
    let refused = generate(&fixture.app, &fixture).await;
    assert_eq!(refused.status(), StatusCode::BAD_GATEWAY);
    // Nothing was invoiced and nothing mirrored — and the batch's work is
    // reserved, not returned to the pool the next composition would draw.
    assert_eq!(time_entry_status(&fixture.pg, entry.id).await, "reserved");
    assert_eq!(
        cost_status(&fixture.pg, fixture.project_id, cost_id).await,
        "reserved"
    );
    assert!(mirror_rows(&fixture.surreal, fixture.project_id)
        .await
        .is_empty());
    let batch = pending_batch(&fixture.pg, fixture.project_id)
        .await
        .expect("the refused generation keeps its pending batch");
    assert_eq!(batch.total_cents, 20_000);

    // A reserved entry cannot be written off while its batch is pending:
    // the work the provider will bill cannot be removed behind its back.
    let write_off = post_json(
        &fixture.app,
        &format!(
            "/app/api/projects/{}/time-entries/{}/write-off",
            fixture.project_id, entry.id
        ),
        json!({ "reason": "no longer warranted" }),
        &fixture.lawyer,
    )
    .await;
    assert_eq!(write_off.status(), StatusCode::CONFLICT);
    assert_eq!(time_entry_status(&fixture.pg, entry.id).await, "reserved");

    // The retry resumes the same batch: same key, same request — the seam
    // re-mints the first attempt's invoice and exactly one mirror row lands.
    let retry = generate(&fixture.app, &fixture).await;
    assert_eq!(retry.status(), StatusCode::OK);
    let receipt: Value = serde_json::from_slice(
        &axum::body::to_bytes(retry.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let invoice_id = receipt["xero_invoice_id"].as_str().unwrap().to_string();
    assert_eq!(receipt["total_cents"], json!(20_000));
    let rows = mirror_rows(&fixture.surreal, fixture.project_id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].xero_invoice_id, invoice_id);
    assert_eq!(time_entry_status(&fixture.pg, entry.id).await, "invoiced");
    assert_eq!(
        cost_status(&fixture.pg, fixture.project_id, cost_id).await,
        "invoiced"
    );
    assert!(pending_batch(&fixture.pg, fixture.project_id)
        .await
        .is_none());
    // Both crossings — the refusal and the retry — went to the seam under
    // the same key: the failed attempt is recorded too, so two calls, one
    // matter key.
    let keys = fixture.provider.create_keys();
    assert_eq!(keys, vec![batch.id, batch.id]);
    // The audited transitions tell the whole story: reserved on the failed
    // attempt, invoiced on the retry — nothing was ever unbilled again.
    assert_eq!(
        entry_audit_kinds(&fixture.pg, entry.id).await,
        vec![
            "created".to_string(),
            "reserved".to_string(),
            "invoiced".to_string()
        ]
    );
}

#[tokio::test]
async fn uncertain_invoices_require_reconciliation_and_a_consumed_fee_cannot_race() {
    if std::env::var("NAV_BILLING_API_CHILD").as_deref() == Ok("1") {
        Box::pin(run_recovery_safety()).await;
        return;
    }
    let Some((port, database)) = isolated_database().await else {
        return;
    };
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "uncertain_invoices_require_reconciliation_and_a_consumed_fee_cannot_race",
            "--nocapture",
        ])
        .env("NAV_BILLING_API_CHILD", "1")
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", &port)
        .env("NAVIGATOR_POSTGRES_USER", "postgres")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", &database)
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .status()
        .unwrap();
    remove_isolated_database(&port, &database).await;
    assert!(status.success());
}

async fn run_recovery_safety() {
    let fixture = fixture().await;
    store::attorney_rates::set_flat_fee(&fixture.pg, fixture.project_id, 100_000)
        .await
        .unwrap();
    seed_entry(&fixture, 60, "Synthetic recovery work").await;
    seed_confirmed_cost(&fixture, 5_000).await;
    fixture.provider.fail_next();
    assert_eq!(
        generate(&fixture.app, &fixture).await.status(),
        StatusCode::BAD_GATEWAY
    );
    let batch = pending_batch(&fixture.pg, fixture.project_id)
        .await
        .unwrap();
    let financials =
        store::matter_financials::for_project(&fixture.surreal, &fixture.pg, fixture.project_id)
            .await
            .unwrap();
    assert_eq!(financials.wip_cents, 20_000, "refused work stays visible");
    assert_eq!(financials.outstanding_cents, 0);

    // Simulate a response lost beyond Xero's idempotency retention window.
    sqlx::query(
        "UPDATE matter_billing_batch SET inserted_at=now()-interval '25 hours' WHERE id=$1",
    )
    .bind(batch.id)
    .execute(fixture.pg.pool())
    .await
    .unwrap();
    let refused = generate(&fixture.app, &fixture).await;
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let reason: Value = serde_json::from_slice(
        &axum::body::to_bytes(refused.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(reason["reason"], "invoice_reconciliation_required");
    assert_eq!(fixture.provider.create_keys(), vec![batch.id]);

    // Reconciliation found the real invoice. A mirrored but unsettled batch
    // contributes outstanding balance, never that same work to WIP as well.
    store::billing_batches::attach_invoice(&fixture.pg, batch.id, "synthetic-recovered")
        .await
        .unwrap();
    store::xero_invoices::upsert(
        &fixture.surreal,
        &store::xero_invoices::UpsertXeroInvoice {
            project_id: fixture.project_id,
            xero_invoice_id: "synthetic-recovered".into(),
            reference: batch.reference.clone(),
            status: "AUTHORISED".into(),
            amount_cents: batch.total_cents,
            currency: "USD".into(),
            issued_at: chrono::Utc::now(),
            due_at: None,
        },
    )
    .await
    .unwrap();
    let financials =
        store::matter_financials::for_project(&fixture.surreal, &fixture.pg, fixture.project_id)
            .await
            .unwrap();
    assert_eq!(financials.wip_cents, 0);
    assert_eq!(financials.outstanding_cents, 105_000);
    assert_eq!(
        generate(&fixture.app, &fixture).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        fixture.provider.create_keys(),
        vec![batch.id],
        "saved ids never create again"
    );

    assert_eq!(
        store::billing_batches::complete(&fixture.pg, batch.id)
            .await
            .unwrap()
            .status,
        "completed",
        "concurrent settlement retries remain idempotent"
    );

    // A slower request composed while fee_billed was false. Its earlier
    // read must be checked again inside the reservation transaction, even
    // if the first batch completed and released the pending unique index.
    let stale = store::billing_batches::MatterBillingBatch {
        id: Uuid::new_v4(),
        entry_ids: vec![],
        cost_ids: vec![],
        ..batch
    };
    assert!(matches!(
        store::billing_batches::insert_with_reservations(&fixture.pg, &stale).await,
        Err(store::billing_batches::BillingBatchError::FlatFeeAlreadyBilled)
    ));
    assert!(pending_batch(&fixture.pg, fixture.project_id)
        .await
        .is_none());
}

#[tokio::test]
async fn an_empty_matter_never_reaches_the_provider() {
    if std::env::var("NAV_BILLING_API_CHILD").as_deref() == Ok("1") {
        Box::pin(run_noop()).await;
        return;
    }
    let Some((port, database)) = isolated_database().await else {
        return;
    };
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "an_empty_matter_never_reaches_the_provider",
            "--nocapture",
        ])
        .env("NAV_BILLING_API_CHILD", "1")
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", &port)
        .env("NAVIGATOR_POSTGRES_USER", "postgres")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", &database)
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .status()
        .unwrap();
    remove_isolated_database(&port, &database).await;
    assert!(status.success());
}

async fn run_noop() {
    let fixture = fixture().await;
    let response = generate(&fixture.app, &fixture).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["reason"], json!("nothing_to_invoice"));
    assert!(fixture.provider.create_keys().is_empty());
    assert!(pending_batch(&fixture.pg, fixture.project_id)
        .await
        .is_none());
}

async fn isolated_database() -> Option<(String, String)> {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref() != Ok("1"),
            "PostgreSQL fixture required"
        );
        return None;
    };
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port.parse().unwrap())
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(PgSslMode::Disable);
    let admin = PgPool::connect_with(options).await.unwrap();
    let database = format!("billing_api_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    Some((port, database))
}

async fn remove_isolated_database(port: &str, database: &str) {
    let admin = PgPool::connect_with(
        PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port.parse().unwrap())
            .username("postgres")
            .password("synthetic")
            .database("smoke")
            .ssl_mode(PgSslMode::Disable),
    )
    .await
    .unwrap();
    sqlx::query("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname=$1")
        .bind(database)
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
