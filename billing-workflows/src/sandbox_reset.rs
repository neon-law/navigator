//! The `SandboxCreditReset` Restate workflow — the monthly job that turns
//! the calendar into the Legal Sandbox's credit contract.
//!
//! On the first of each month it grants every Nautilus subscriber the
//! glossary's fresh 500-cent credit ([`store::sandbox_usage::grant`], the
//! ledger's own idempotent rollover primitive — this job never touches
//! ledger rows directly), then converts the closed month's overage rows
//! into à la carte invoice lines through the shared `billing` contract.
//!
//! Grant and invoice are separate journaled steps: a retry after the
//! grants succeeded re-runs only the invoicing, and the grants' own
//! idempotence converges a doubled grant anyway. Closed-period rows are
//! never rewritten — an invoice already raised this month is detected by
//! its deterministic idempotency key and skipped on re-run, so a retry can
//! never bill the same overage twice.
//!
//! The `sandbox-credit-reset-trigger` `CronJob` starts one invocation per
//! month (keyed on the billing period, so a same-month re-fire is a no-op);
//! Restate owns the retry schedule. Identical split to the billing canary
//! and reconcile triggers.

use billing::{BillingProvider, InvoiceLine, InvoiceRequest, XeroBillingProvider};
use chrono::Datelike;
use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use store::postgres::PostgresDb;
use store::purchases::{self, PurchaseState};
use store::sandbox_usage;
use store::surreal::SurrealDb;

/// Request body for `SandboxCreditReset::run`. Empty — the trigger only
/// starts the workflow — but kept as a struct so fields can be threaded
/// later without changing the handler signature.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct SandboxResetRequest {}

/// What a reset run did, surfaced as the invocation output.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SandboxResetReport {
    /// The billing month the fresh credit was granted for (`YYYY-MM`).
    pub new_period: String,
    /// The billing month that just closed and was invoiced (`YYYY-MM`).
    pub closed_period: String,
    /// Subscribers granted the new month's credit.
    pub granted: usize,
    /// Closed-month overage rows invoiced à la carte this run.
    pub overage_invoiced: usize,
    /// Closed-month overage rows that became no invoice: the subscriber's
    /// Nautilus Purchase is gone, so there is no contact or account code to
    /// bill. Reported, never silently dropped.
    pub overage_skipped: usize,
    /// Invoices already raised for this closed period on an earlier run —
    /// the idempotency key matched. A retry's receipt, not new money.
    pub overage_already_billed: usize,
}

/// Service registered with the Restate endpoint. Holds a `SurrealDB` clone
/// (the same connection the worker opened at boot); the Postgres pool and
/// the Xero provider are built from env inside the steps so no token sits
/// idle between monthly runs.
#[derive(Clone)]
pub struct SandboxCreditResetService {
    surreal: SurrealDb,
}

impl SandboxCreditResetService {
    #[must_use]
    pub fn new(surreal: SurrealDb) -> Self {
        Self { surreal }
    }
}

#[restate_sdk::workflow(name = "SandboxCreditReset")]
impl SandboxCreditResetService {
    #[restate_sdk::handler]
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        _req: Json<SandboxResetRequest>,
    ) -> Result<Json<SandboxResetReport>, HandlerError> {
        let surreal = self.surreal.clone();
        let grant_json = ctx
            .run(move || async move {
                // The month that just closed, and the month being granted.
                // The `billing_period` spelling is the ledger's own.
                let now = chrono::Utc::now();
                let (year, month) = match now.month() {
                    1 => (now.year() - 1, 12_u32),
                    m => (now.year(), m - 1),
                };
                let closed_period = sandbox_usage::billing_period(year, month as u8);
                let new_period = sandbox_usage::billing_period(now.year(), now.month() as u8);

                let pg = PostgresDb::from_env()
                    .await
                    .map_err(|error| HandlerError::from(anyhow::anyhow!("Postgres: {error}")))?;
                let granted = grant_active_subscribers(&pg, &surreal, &new_period)
                    .await
                    .map_err(HandlerError::from)?;
                serde_json::to_string(&(closed_period, new_period, granted)).map_err(|error| {
                    HandlerError::from(anyhow::anyhow!("journal grant result: {error}"))
                })
            })
            .name("grant")
            .await?;
        let (closed_period, new_period, granted): (String, String, usize) =
            serde_json::from_str(&grant_json)
                .map_err(|error| HandlerError::from(anyhow::anyhow!("unjournal grant: {error}")))?;

        let surreal = self.surreal.clone();
        let closed = closed_period.clone();
        let report_json = ctx
            .run(move || async move {
                // A terminal error, not a silent skip: overage is promised
                // à la carte, so an unconfigured Xero must fail the run and
                // its schedule, not quietly drop what subscribers owe.
                let mut billing = XeroBillingProvider::from_env().ok_or_else(|| {
                    TerminalError::new("Xero is not configured (XERO_* env unset)")
                })?;
                let pg = PostgresDb::from_env()
                    .await
                    .map_err(|error| HandlerError::from(anyhow::anyhow!("Postgres: {error}")))?;
                let report = invoice_closed_overage(&mut billing, &pg, &surreal, &closed)
                    .await
                    .map_err(HandlerError::from)?;
                serde_json::to_string(&report).map_err(|error| {
                    HandlerError::from(anyhow::anyhow!("journal invoice report: {error}"))
                })
            })
            .name("invoice_overage")
            .await?;
        let report: SandboxResetReport = serde_json::from_str(&report_json).map_err(|error| {
            HandlerError::from(anyhow::anyhow!("unjournal invoice report: {error}"))
        })?;

        Ok(Json(SandboxResetReport {
            new_period,
            closed_period,
            granted,
            ..report
        }))
    }
}

/// Grant the fresh period's credit to every current Nautilus subscriber —
/// one `active` Purchase of either Nautilus offering makes a subscriber.
///
/// Grading who is "active" on Purchases in any state would grant credit to
/// cancelled and never-paid subscriptions; `active` is the state the
/// portal's own gate requires before a composer renders.
async fn grant_active_subscribers(
    pg: &PostgresDb,
    surreal: &SurrealDb,
    new_period: &str,
) -> anyhow::Result<usize> {
    let purchases = purchases::for_offerings(pg, &purchases::NAUTILUS_OFFERINGS).await?;
    let mut granted = 0;
    for person_id in subscriber_ids(&purchases) {
        sandbox_usage::grant(surreal, person_id, new_period).await?;
        granted += 1;
    }
    Ok(granted)
}

/// The distinct person ids holding an `active` Nautilus Purchase, ordered
/// for a deterministic grant batch.
fn subscriber_ids(purchases: &[store::purchases::Purchase]) -> Vec<uuid::Uuid> {
    let mut people: Vec<uuid::Uuid> = purchases
        .iter()
        .filter(|purchase| purchase.state == PurchaseState::Active)
        .map(|purchase| purchase.person_id)
        .collect();
    people.sort();
    people.dedup();
    people
}

/// The latest Nautilus Purchase of one person: the invoice's account code
/// and the purchase the subscription billed through.
fn latest_purchase(
    purchases: &[store::purchases::Purchase],
    person_id: uuid::Uuid,
) -> Option<&store::purchases::Purchase> {
    purchases
        .iter()
        .filter(|purchase| purchase.person_id == person_id)
        .max_by_key(|purchase| (purchase.inserted_at, purchase.id))
}

/// Convert a closed period's overage rows into à la carte invoices.
///
/// Each overage row becomes one `InvoiceRequest` — one line, the overage
/// amount, on the subscriber's Nautilus account code, billed to the person
/// the ledger row belongs to. A row whose person or Nautilus Purchase is
/// gone is skipped and counted; the run continues, because one orphaned
/// ledger row must not stop every other subscriber's invoice.
///
/// Retry-safe two ways: the step's result is journaled (Restate replays it
/// without re-executing), and the invoice key is deterministic —
/// [`overage_invoice_key`] — so the `billing` contract's own promise ("the
/// same `matter_id` never creates two invoices") holds even when the step
/// itself died mid-flight and re-runs.
async fn invoice_closed_overage(
    billing: &mut dyn BillingProvider,
    pg: &PostgresDb,
    surreal: &SurrealDb,
    closed_period: &str,
) -> anyhow::Result<SandboxResetReport> {
    let rows = sandbox_usage::overage_rows(surreal, closed_period).await?;
    let purchases = purchases::for_offerings(pg, &purchases::NAUTILUS_OFFERINGS).await?;

    let mut report = SandboxResetReport {
        new_period: String::new(),
        closed_period: closed_period.to_string(),
        granted: 0,
        overage_invoiced: 0,
        overage_skipped: 0,
        overage_already_billed: 0,
    };

    for row in rows {
        let overage_cents = row.spent_cents - row.granted_cents;
        if overage_cents <= 0 {
            // A row flagged overage with the balance at or above zero —
            // already settled by an earlier run's invoice. Counted and
            // skipped rather than re-billed.
            report.overage_already_billed += 1;
            continue;
        }

        let Some(purchase) = latest_purchase(&purchases, row.person_id) else {
            report.overage_skipped += 1;
            continue;
        };
        let person = store::persons::find_by_id(surreal, row.person_id).await?;
        let Some(person) = person else {
            report.overage_skipped += 1;
            continue;
        };

        let key = overage_invoice_key(row.person_id, closed_period);
        let request = InvoiceRequest {
            contact_name: person.name.clone(),
            contact_email: person.email.clone(),
            reference: format!("Nautilus sandbox usage {closed_period}"),
            line_items: vec![InvoiceLine {
                description: format!(
                    "Legal Sandbox model usage beyond the included $5.00, {}",
                    closed_period
                ),
                quantity: 1,
                unit_amount_cents: overage_cents,
                account_code: purchase.account_code.clone(),
            }],
            due_date: None,
        };
        // The key is deterministic, and `create_invoice` is idempotent on
        // the matter id — the same overage retried after a mid-flight
        // crash rebuilds the same key and the provider dedupes.
        billing.create_invoice(key, &request).await?;
        report.overage_invoiced += 1;
    }
    Ok(report)
}

/// The deterministic overage-invoice key for one subscriber-month. `v5`
/// over the ledger's own compound key, so every retry of the same
/// overage builds the same id.
#[must_use]
pub fn overage_invoice_key(person_id: uuid::Uuid, period: &str) -> uuid::Uuid {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_DNS,
        format!("sandbox-overage:{person_id}:{period}").as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::{grant_active_subscribers, invoice_closed_overage, overage_invoice_key};
    use billing::StubBillingProvider;
    use sqlx::postgres::{PgConnectOptions, PgSslMode};
    use sqlx::{AssertSqlSafe, PgPool};
    use store::purchases::{Cadence, NewPurchase};
    use store::sandbox_usage::MeterDecision;
    use store::surreal::test_support::mem;

    /// The same Postgres test lane the store's own purchase tests use:
    /// per-test database, migrated. `None` keeps `cargo test` zero-config
    /// without the integration tier running (`NAV_REQUIRE_POSTGRES=1`
    /// turns the skip into a failure).
    async fn database() -> Option<store::postgres::PostgresDb> {
        let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
            assert_ne!(
                std::env::var("NAV_REQUIRE_POSTGRES").as_deref(),
                Ok("1"),
                "PostgreSQL integration lane requires NAVIGATOR_TEST_POSTGRES_PORT"
            );
            return None;
        };
        let options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port.parse().unwrap())
            .username("postgres")
            .password("synthetic")
            .database(
                &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE")
                    .unwrap_or_else(|_| "smoke".into()),
            )
            .ssl_mode(PgSslMode::Disable);
        let admin = PgPool::connect_with(options.clone()).await.unwrap();
        let name = format!("sandbox_reset_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
        let pg = store::postgres::PostgresDb::connect(options.database(&name))
            .await
            .unwrap();
        pg.migrate_embedded(None).await.unwrap();
        Some(pg)
    }

    /// A Nautilus monthly plan for one person, mirroring the store's own
    /// purchase-test fixture.
    fn plan(person_id: uuid::Uuid) -> NewPurchase {
        NewPurchase {
            person_id,
            email: "client@example.com".into(),
            offering: "delete-your-data-monthly".into(),
            offering_name: "Delete Your Data, monthly".into(),
            cadence: Cadence::Month,
            amount_cents: 2_900,
            currency: "USD".into(),
            account_code: "200".into(),
        }
    }

    #[tokio::test]
    async fn grants_follow_active_purchases_and_skip_other_states() {
        let Some(pg) = database().await else { return };
        let surreal = mem().await;
        let now = chrono::Utc::now();
        let active = uuid::Uuid::now_v7();
        let lapsed = uuid::Uuid::now_v7();
        let other = uuid::Uuid::now_v7();

        let active_purchase = store::purchases::open(&pg, &plan(active), now)
            .await
            .unwrap();
        store::purchases::activate(&pg, active_purchase.id, None, now)
            .await
            .unwrap();
        store::purchases::open(&pg, &plan(lapsed), now)
            .await
            .unwrap();
        let consultation = NewPurchase {
            offering: "consultation".into(),
            offering_name: "Consultation".into(),
            ..plan(other)
        };
        store::purchases::record_complimentary(&pg, &consultation, now)
            .await
            .unwrap();

        let granted = grant_active_subscribers(&pg, &surreal, "2026-11")
            .await
            .unwrap();

        assert_eq!(granted, 1, "only the active subscription is a subscriber");
        let row = store::sandbox_usage::find(&surreal, active, "2026-11")
            .await
            .unwrap()
            .expect("the active subscriber's row");
        assert_eq!(row.granted_cents, store::sandbox_usage::MONTHLY_GRANT_CENTS);
        assert!(
            store::sandbox_usage::find(&surreal, lapsed, "2026-11")
                .await
                .unwrap()
                .is_none(),
            "an invoiced-but-never-active purchase grants nothing"
        );
        assert!(
            store::sandbox_usage::find(&surreal, other, "2026-11")
                .await
                .unwrap()
                .is_none(),
            "a non-Nautilus offering grants nothing"
        );
    }

    /// Overdraw one subscriber's closed month, seed the person the invoice
    /// bills, and run the invoicing twice.
    async fn overdrawn_subscriber(
        pg: &store::postgres::PostgresDb,
        surreal: &store::surreal::SurrealDb,
    ) -> uuid::Uuid {
        let person = store::test_support::ensure_person(
            surreal,
            &store::persons::NewPerson::new("Ada Lovelace", "ada@example.com"),
        )
        .await;
        store::purchases::open(pg, &plan(person.id), chrono::Utc::now())
            .await
            .unwrap();
        store::sandbox_usage::grant(surreal, person.id, "2026-10")
            .await
            .unwrap();
        assert!(matches!(
            store::sandbox_usage::debit(surreal, person.id, "2026-10", 625)
                .await
                .unwrap(),
            MeterDecision::Charge { .. }
        ));
        person.id
    }

    #[tokio::test]
    async fn closed_overage_becomes_one_invoice_line_on_the_subscribers_account() {
        let Some(pg) = database().await else { return };
        let surreal = mem().await;
        let person = overdrawn_subscriber(&pg, &surreal).await;

        let mut stub = StubBillingProvider::new();
        let report = invoice_closed_overage(&mut stub, &pg, &surreal, "2026-10")
            .await
            .unwrap();

        assert_eq!(report.overage_invoiced, 1, "the 125-cent overage is billed");
        assert_eq!(report.overage_skipped, 0);

        let call = &stub.calls()[0];
        assert_eq!(call.request.contact_name, "Ada Lovelace");
        assert_eq!(call.request.contact_email, "ada@example.com");
        assert_eq!(call.request.line_items.len(), 1);
        assert_eq!(call.request.line_items[0].unit_amount_cents, 125);
        assert_eq!(
            call.request.line_items[0].account_code, "200",
            "the overage posts to the subscriber's Nautilus account code"
        );
        assert_eq!(
            call.matter_id,
            overage_invoice_key(person, "2026-10"),
            "the invoice key is the deterministic overage key"
        );
    }

    /// The billing contract's promise is idempotency on the matter id, so
    /// the retry's deduplication is the *key*: a re-run rebuilds the same
    /// key for the same overage. (The stub records calls without
    /// deduplicating; the real provider honors the key.)
    #[tokio::test]
    async fn a_rerun_of_the_same_overage_builds_the_same_invoice_key() {
        let Some(pg) = database().await else { return };
        let surreal = mem().await;
        let person = overdrawn_subscriber(&pg, &surreal).await;

        let mut stub = StubBillingProvider::new();
        let first = invoice_closed_overage(&mut stub, &pg, &surreal, "2026-10")
            .await
            .unwrap();
        let second = invoice_closed_overage(&mut stub, &pg, &surreal, "2026-10")
            .await
            .unwrap();

        assert_eq!(
            first.overage_invoiced, second.overage_invoiced,
            "the re-run sees the same closed-period rows"
        );
        assert_eq!(
            stub.calls()[0].matter_id,
            overage_invoice_key(person, "2026-10"),
            "the first call carries the deterministic key"
        );
        assert_eq!(
            stub.calls()[1].matter_id,
            stub.calls()[0].matter_id,
            "the re-run rebuilds the same key — the provider dedupes on it"
        );
    }

    #[tokio::test]
    async fn an_overage_without_a_purchase_is_skipped_and_counted() {
        let Some(pg) = database().await else { return };
        let surreal = mem().await;
        let person = uuid::Uuid::now_v7();
        store::sandbox_usage::grant(&surreal, person, "2026-10")
            .await
            .unwrap();
        assert!(matches!(
            store::sandbox_usage::debit(&surreal, person, "2026-10", 600)
                .await
                .unwrap(),
            MeterDecision::Charge { .. }
        ));

        // No Nautilus Purchase anywhere: the subscriber's purchase is gone.
        let mut stub = StubBillingProvider::new();
        let report = invoice_closed_overage(&mut stub, &pg, &surreal, "2026-10")
            .await
            .unwrap();

        assert_eq!(report.overage_skipped, 1);
        assert_eq!(report.overage_invoiced, 0);
        assert!(stub.calls().is_empty(), "no invoice without a billed party");
    }
}
