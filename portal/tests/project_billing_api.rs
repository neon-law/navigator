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
    available: Arc<Mutex<bool>>,
    fail_next: Arc<Mutex<bool>>,
    create_calls: Arc<Mutex<Vec<Uuid>>>,
    create_requests: Arc<Mutex<Vec<InvoiceRequest>>>,
    contact_override: Arc<Mutex<Option<String>>>,
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
    fn invoices_available(&self) -> bool {
        *self.available.lock().unwrap()
    }
    async fn ensure_contact(&self, request: &ContactRequest) -> Result<ContactId, BillingError> {
        let resolved = self.inner.ensure_contact(request).await?;
        Ok(self
            .contact_override
            .lock()
            .unwrap()
            .clone()
            .map_or(resolved, ContactId))
    }

    async fn create_invoice(
        &self,
        matter_id: Uuid,
        request: &InvoiceRequest,
    ) -> Result<InvoiceId, BillingError> {
        self.create_calls.lock().unwrap().push(matter_id);
        self.create_requests.lock().unwrap().push(request.clone());
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
            format!("billing-client-{}@example.com", Uuid::new_v4()),
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
        &NewPerson::with_role(
            "Billing Lawyer",
            format!("billing-lawyer-{}@example.com", Uuid::new_v4()),
            Role::Lawyer,
        ),
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
    *provider.available.lock().unwrap() = true;
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
            ..portal::SessionData::fresh(&lawyer.email, Role::Lawyer)
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
            format!("Synthetic billing jurisdiction {}", Uuid::new_v4()),
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
    assert_amended_fee_bills_only_the_balance(&fixture).await;
    Box::pin(assert_later_costs_leave_covered_time_unbilled(&fixture)).await;
}

async fn assert_amended_fee_bills_only_the_balance(fixture: &Fixture) {
    store::attorney_rates::set_flat_fee(&fixture.pg, fixture.project_id, 750_000)
        .await
        .unwrap();
    let amended = generate(&fixture.app, fixture).await;
    assert_eq!(amended.status(), StatusCode::OK);
    let receipt: Value = serde_json::from_slice(
        &axum::body::to_bytes(amended.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        receipt["total_cents"], 250_000,
        "an amendment charges only the remaining agreed fee"
    );
    assert_eq!(receipt["line_items"][0]["unit_amount_cents"], 250_000);
    assert_eq!(
        store::billing_batches::flat_fee_billed_cents(&fixture.pg, fixture.project_id)
            .await
            .unwrap(),
        750_000
    );
    assert_eq!(
        generate(&fixture.app, fixture).await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(fixture.provider.create_keys().len(), 2);
}

async fn assert_later_costs_leave_covered_time_unbilled(fixture: &Fixture) {
    let entry = seed_entry(fixture, 30, "Synthetic covered review").await;
    assert_eq!(
        generate(&fixture.app, fixture).await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(fixture.provider.create_keys().len(), 2);
    let cost_id = seed_confirmed_cost(fixture, 5_000).await;
    fixture.provider.fail_next();
    assert_eq!(
        generate(&fixture.app, fixture).await.status(),
        StatusCode::BAD_GATEWAY
    );
    let batch = pending_batch(&fixture.pg, fixture.project_id)
        .await
        .unwrap();
    assert_eq!(batch.total_cents, 5_000);
    assert_eq!(batch.flat_fee_cents, None);
    assert!(batch.entry_ids.is_empty());
    assert_eq!(batch.cost_ids, vec![cost_id]);
    assert_eq!(time_entry_status(&fixture.pg, entry.id).await, "unbilled");
    assert_eq!(
        cost_status(&fixture.pg, fixture.project_id, cost_id).await,
        "reserved"
    );

    let response = generate(&fixture.app, fixture).await;
    assert_eq!(response.status(), StatusCode::OK);
    let receipt: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["total_cents"], 5_000);
    assert_eq!(receipt["consumed_time_entries"], 0);
    assert_eq!(receipt["consumed_costs"], 1);
    assert_eq!(receipt["flat_fee"], false);
    assert_eq!(receipt["line_items"].as_array().unwrap().len(), 1);
    assert_eq!(time_entry_status(&fixture.pg, entry.id).await, "unbilled");
    assert_eq!(
        cost_status(&fixture.pg, fixture.project_id, cost_id).await,
        "invoiced"
    );
    assert_eq!(
        store::billing_batches::flat_fee_billed_cents(&fixture.pg, fixture.project_id)
            .await
            .unwrap(),
        750_000
    );
    let keys = fixture.provider.create_keys();
    assert_eq!(keys.len(), 4);
    assert_eq!(keys[2], keys[3]);
    assert_eq!(
        generate(&fixture.app, fixture).await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(fixture.provider.create_keys().len(), 4);
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
    let contacts = fixture.provider.inner.contact_calls().len();
    *fixture.provider.contact_override.lock().unwrap() = Some("synthetic-changed-contact".into());

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
    assert_eq!(write_off.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(time_entry_status(&fixture.pg, entry.id).await, "reserved");

    // The batch's debtor remains fixed even after participation changes.
    remove_client_participation(&fixture).await;
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
    assert_frozen_debtor_request(&fixture, &batch, contacts);
    assert_eq!(
        fixture.provider.inner.calls()[0].request.contact_email,
        batch.contact_email
    );
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

fn assert_frozen_debtor_request(
    fixture: &Fixture,
    batch: &store::billing_batches::MatterBillingBatch,
    contacts: usize,
) {
    assert_eq!(
        fixture.provider.inner.contact_calls().len(),
        contacts,
        "retry never resolves a new contact"
    );
    let requests = fixture.provider.create_requests.lock().unwrap().clone();
    assert_eq!(
        requests[0], requests[1],
        "the same key carries the exact frozen request"
    );
    assert_eq!(
        requests[0].contact_id.as_ref().unwrap().0,
        batch.xero_contact_id
    );
    assert_eq!(
        requests[0].issued_date,
        Some(batch.inserted_at.date_naive())
    );
    assert_eq!(requests[0].due_date, Some(batch.due_date));
    assert_eq!(
        batch.due_date,
        batch.inserted_at.date_naive() + chrono::Days::new(14)
    );
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

    // Simulate a response lost beyond Xero's six-minute retention window.
    sqlx::query(
        "UPDATE matter_billing_batch SET first_attempted_at=now()-interval '7 minutes' WHERE id=$1",
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

    // Model a first attachment just after UTC midnight and a provider invoice
    // whose date was corrected to a previous calendar day.
    // Reconciliation found the real invoice. A mirrored but unsettled batch
    // contributes outstanding balance, never that same work to WIP as well.
    store::billing_batches::attach_invoice(&fixture.pg, batch.id, "synthetic-recovered")
        .await
        .unwrap();
    simulate_attachment_after_utc_midnight(&fixture, batch.id).await;
    store::xero_invoices::upsert(
        &fixture.surreal,
        &store::xero_invoices::UpsertXeroInvoice {
            project_id: fixture.project_id,
            xero_invoice_id: "synthetic-recovered".into(),
            reference: batch.reference.clone(),
            status: "AUTHORISED".into(),
            amount_cents: batch.total_cents,
            currency: "USD".into(),
            issued_at: NaiveDate::from_ymd_opt(2026, 10, 21)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap()
                .and_utc(),
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
    settle_with_corrected_provider_financials(&fixture).await;
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
        store::billing_batches::insert_with_reservations(&fixture.pg, &stale, Some(0)).await,
        Err(store::billing_batches::BillingBatchError::FlatFeeAlreadyBilled)
    ));
    assert!(pending_batch(&fixture.pg, fixture.project_id)
        .await
        .is_none());
    assert_voided_invoice_remains_reserved().await;
}

async fn simulate_attachment_after_utc_midnight(fixture: &Fixture, batch_id: Uuid) {
    sqlx::query("UPDATE matter_billing_batch SET updated_at=$2 WHERE id=$1")
        .bind(batch_id)
        .bind(provider_date(22) + chrono::Duration::minutes(5))
        .execute(fixture.pg.pool())
        .await
        .unwrap();
}

fn provider_date(day: u32) -> chrono::DateTime<chrono::Utc> {
    NaiveDate::from_ymd_opt(2026, 10, day)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
}

async fn assert_missing_provider_date_preserves_mirror(fixture: &Fixture) {
    fixture.provider.inner.set_invoice_status(
        "synthetic-recovered",
        InvoiceStatus {
            status: "AUTHORISED".into(),
            amount_paid_cents: 20_000,
            amount_cents: Some(90_000),
            currency: Some("USD".into()),
            issued_at: None,
            due_at: None,
        },
    );
    let before = mirror_rows(&fixture.surreal, fixture.project_id).await;
    assert_eq!(
        generate(&fixture.app, fixture).await.status(),
        StatusCode::BAD_GATEWAY
    );
    let after = mirror_rows(&fixture.surreal, fixture.project_id).await;
    assert_eq!(after[0].amount_cents, before[0].amount_cents);
    assert_eq!(after[0].issued_at, before[0].issued_at);
    assert_eq!(after[0].due_at, before[0].due_at);
}

async fn assert_earlier_provider_date_preserves_work(fixture: &Fixture) {
    fixture.provider.inner.set_invoice_status(
        "synthetic-recovered",
        InvoiceStatus {
            status: "AUTHORISED".into(),
            amount_paid_cents: 20_000,
            amount_cents: Some(90_000),
            currency: Some("USD".into()),
            issued_at: Some(provider_date(17)),
            due_at: None,
        },
    );
    let batch = pending_batch(&fixture.pg, fixture.project_id)
        .await
        .unwrap();
    assert_eq!(
        generate(&fixture.app, fixture).await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        time_entry_status(&fixture.pg, batch.entry_ids[0]).await,
        "reserved"
    );
    assert_eq!(
        cost_status(&fixture.pg, fixture.project_id, batch.cost_ids[0]).await,
        "reserved"
    );
    assert_eq!(
        mirror_rows(&fixture.surreal, fixture.project_id).await[0].issued_at,
        provider_date(17)
    );
}

async fn settle_with_corrected_provider_financials(fixture: &Fixture) {
    // A missing financial snapshot cannot silently restore the request total.
    assert_eq!(
        generate(&fixture.app, fixture).await.status(),
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(
        mirror_rows(&fixture.surreal, fixture.project_id).await[0].amount_cents,
        105_000
    );
    assert_missing_provider_date_preserves_mirror(fixture).await;
    assert_earlier_provider_date_preserves_work(fixture).await;
    // A currency correction is mirrored, but cannot consume USD work.
    fixture.provider.inner.set_invoice_status(
        "synthetic-recovered",
        InvoiceStatus {
            status: "AUTHORISED".into(),
            amount_paid_cents: 20_000,
            amount_cents: Some(90_000),
            currency: Some("EUR".into()),
            issued_at: Some(provider_date(20)),
            due_at: Some(provider_date(31)),
        },
    );
    assert_eq!(
        generate(&fixture.app, fixture).await.status(),
        StatusCode::CONFLICT
    );
    let corrected = mirror_rows(&fixture.surreal, fixture.project_id).await;
    assert_eq!(corrected[0].amount_cents, 90_000);
    assert_eq!(corrected[0].currency, "EUR");
    assert_eq!(corrected[0].issued_at, provider_date(20));
    assert_eq!(corrected[0].due_at, Some(provider_date(31)));
    assert!(pending_batch(&fixture.pg, fixture.project_id)
        .await
        .is_some());
    fixture.provider.inner.set_invoice_status(
        "synthetic-recovered",
        InvoiceStatus {
            status: "AUTHORISED".into(),
            amount_paid_cents: 20_000,
            amount_cents: Some(90_000),
            currency: Some("USD".into()),
            issued_at: Some(provider_date(21)),
            due_at: None,
        },
    );
    settle_without_current_billed_party(fixture, provider_date(21)).await;
    let corrected = mirror_rows(&fixture.surreal, fixture.project_id).await;
    assert_eq!(corrected[0].amount_cents, 90_000);
    assert_eq!(corrected[0].amount_paid_cents, 20_000);
    assert_eq!(corrected[0].currency, "USD");
    assert_eq!(corrected[0].issued_at, provider_date(21));
    assert_eq!(
        corrected[0].due_at, None,
        "cleared provider due date stays cleared"
    );
    assert_eq!(
        store::matter_financials::outstanding_cents(&corrected),
        70_000
    );
}

async fn remove_client_participation(fixture: &Fixture) {
    let rows = store::projects::participations_for_project(&fixture.surreal, fixture.project_id)
        .await
        .unwrap();
    for row in rows.into_iter().filter(|row| row.participation == "client") {
        store::projects::remove_participation(&fixture.surreal, row.id)
            .await
            .unwrap();
    }
}

async fn settle_without_current_billed_party(
    fixture: &Fixture,
    issued_at: chrono::DateTime<chrono::Utc>,
) {
    remove_client_participation(fixture).await;
    let contacts = fixture.provider.inner.contact_calls().len();
    let before = pending_batch(&fixture.pg, fixture.project_id)
        .await
        .unwrap();
    let invoice_id = before.xero_invoice_id.as_deref().unwrap();
    let repeated = store::billing_batches::attach_invoice(&fixture.pg, before.id, invoice_id)
        .await
        .unwrap();
    assert_eq!(
        repeated.updated_at, before.updated_at,
        "re-attach preserves issue time"
    );
    assert!(
        store::billing_batches::attach_invoice(&fixture.pg, before.id, "different-invoice")
            .await
            .is_err(),
        "a saved provider invoice cannot be replaced"
    );
    assert_eq!(
        generate(&fixture.app, fixture).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        fixture.provider.inner.contact_calls().len(),
        contacts,
        "saved invoice settlement needs no contact request"
    );
    assert_eq!(
        mirror_rows(&fixture.surreal, fixture.project_id).await[0].issued_at,
        issued_at,
        "settlement mirrors the provider date instead of the UTC attachment or retry date"
    );
}

async fn assert_first_attempt_is_shared(fixture: &Fixture, batch_id: Uuid) {
    // Model the durable intent before any POST (for example, a process
    // stopped after reservation). No provider invoice exists in this fixture.
    sqlx::query("UPDATE matter_billing_batch SET first_attempted_at=NULL WHERE id=$1")
        .bind(batch_id)
        .execute(fixture.pg.pool())
        .await
        .unwrap();
    let now = chrono::Utc::now();
    let (left, right) = tokio::join!(
        store::billing_batches::start_attempt(&fixture.pg, batch_id, now),
        store::billing_batches::start_attempt(
            &fixture.pg,
            batch_id,
            now + chrono::Duration::microseconds(1)
        ),
    );
    let first = left.unwrap().first_attempted_at;
    assert!(first.is_some());
    assert_eq!(
        right.unwrap().first_attempted_at,
        first,
        "overlapping creates share one persisted first-attempt time"
    );
    let replay = store::billing_batches::start_attempt(
        &fixture.pg,
        batch_id,
        now + chrono::Duration::minutes(7),
    )
    .await
    .unwrap();
    assert_eq!(
        replay.first_attempted_at, first,
        "a retry cannot restart retention"
    );
}

async fn assert_voided_invoice_remains_reserved() {
    let fixture = fixture().await;
    let entry = seed_entry(&fixture, 60, "Synthetic voided invoice recovery").await;
    fixture.provider.fail_next();
    assert_eq!(
        generate(&fixture.app, &fixture).await.status(),
        StatusCode::BAD_GATEWAY
    );
    let batch = pending_batch(&fixture.pg, fixture.project_id)
        .await
        .unwrap();
    assert_first_attempt_is_shared(&fixture, batch.id).await;
    store::billing_batches::attach_invoice(&fixture.pg, batch.id, "synthetic-voided")
        .await
        .unwrap();
    fixture.provider.inner.set_invoice_status(
        "synthetic-voided",
        InvoiceStatus {
            status: "VOIDED".into(),
            amount_paid_cents: 0,
            amount_cents: Some(batch.total_cents),
            currency: Some(batch.currency.clone()),
            issued_at: Some(chrono::Utc::now()),
            due_at: None,
        },
    );
    let retry = generate(&fixture.app, &fixture).await;
    assert_eq!(retry.status(), StatusCode::CONFLICT);
    assert_eq!(time_entry_status(&fixture.pg, entry.id).await, "reserved");
    assert!(pending_batch(&fixture.pg, fixture.project_id)
        .await
        .is_some());
    let invoices = mirror_rows(&fixture.surreal, fixture.project_id).await;
    assert_eq!(invoices.len(), 1);
    assert_eq!(invoices[0].status, "VOIDED");
    assert_eq!(store::matter_financials::outstanding_cents(&invoices), 0);
    let financials =
        store::matter_financials::for_project(&fixture.surreal, &fixture.pg, fixture.project_id)
            .await
            .unwrap();
    assert_eq!(
        financials.wip_cents, 15_000,
        "voided pending work is still unpaid"
    );
    assert_eq!(fixture.provider.create_keys(), vec![batch.id]);
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

async fn assert_zero_rate_work_is_a_noop(fixture: &Fixture) {
    let entry = store::time_entries::create(
        &fixture.pg,
        fixture.project_id,
        fixture.lawyer_id,
        &store::time_entries::NewTimeEntry {
            attorney_id: fixture.lawyer_id,
            performed_on: NaiveDate::from_ymd_opt(2026, 10, 20).unwrap(),
            minutes: 60,
            description: "Synthetic pro bono work".into(),
            billable: true,
            rate_cents_per_hour: 0,
            source: store::time_entries::TimeEntrySource::Manual,
        },
    )
    .await
    .unwrap();
    let response = generate(&fixture.app, fixture).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["reason"], "nothing_to_invoice");
    assert_eq!(time_entry_status(&fixture.pg, entry.id).await, "unbilled");
    assert!(fixture.provider.inner.contact_calls().is_empty());
    assert!(fixture.provider.create_keys().is_empty());
    assert!(pending_batch(&fixture.pg, fixture.project_id)
        .await
        .is_none());
    assert!(mirror_rows(&fixture.surreal, fixture.project_id)
        .await
        .is_empty());
}

async fn assert_unavailable_billing_preserves_work() {
    let fixture = fixture().await;
    let entry = seed_entry(&fixture, 60, "Synthetic unavailable billing").await;
    let cost = seed_confirmed_cost(&fixture, 5_000).await;
    *fixture.provider.available.lock().unwrap() = false;
    let response = generate(&fixture.app, &fixture).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["reason"], "billing_unavailable");
    assert_eq!(time_entry_status(&fixture.pg, entry.id).await, "unbilled");
    assert_eq!(
        cost_status(&fixture.pg, fixture.project_id, cost).await,
        "unbilled"
    );
    assert!(pending_batch(&fixture.pg, fixture.project_id)
        .await
        .is_none());
    assert!(mirror_rows(&fixture.surreal, fixture.project_id)
        .await
        .is_empty());
    assert!(fixture.provider.inner.contact_calls().is_empty());
    assert!(fixture.provider.create_keys().is_empty());
}

async fn run_noop() {
    Box::pin(assert_unavailable_billing_preserves_work()).await;
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
    assert_zero_rate_work_is_a_noop(&fixture).await;
    let unresolved = seed_entry(&fixture, 60, "Synthetic unresolved contact").await;
    *fixture.provider.contact_override.lock().unwrap() = Some("  ".into());
    assert_eq!(
        generate(&fixture.app, &fixture).await.status(),
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(
        time_entry_status(&fixture.pg, unresolved.id).await,
        "unbilled"
    );
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
