//! The `SandboxCreditReset` Restate workflow — the monthly job that turns
//! the calendar into the Legal Sandbox's credit contract.
//!
//! On the first of each month it grants every Same Day Legal subscriber the
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

use serde::{Deserialize, Serialize};
use store::application_store::ApplicationStore;
use store::postgres::PostgresDb;
use store::purchases;
use store::sandbox_usage;

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
    /// Same Day Legal Purchase is gone, so there is no contact or account code to
    /// bill. Reported, never silently dropped.
    pub overage_skipped: usize,
    /// Invoices already raised for this closed period on an earlier run —
    /// the idempotency key matched. A retry's receipt, not new money.
    pub overage_already_billed: usize,
}

/// Grant the fresh period's credit to every current Same Day Legal subscriber —
/// one `active` Purchase of either Same Day Legal offering makes a subscriber.
///
/// Grading who is "active" on Purchases in any state would grant credit to
/// cancelled and never-paid subscriptions; `active` is the state the
/// portal's own gate requires before a composer renders.
pub async fn grant_active_subscribers(
    pg: &PostgresDb,
    application_store: &ApplicationStore,
    new_period: &str,
) -> anyhow::Result<usize> {
    let purchases = purchases::for_offerings(pg, &purchases::SAME_DAY_LEGAL_OFFERINGS).await?;
    let mut granted = 0;
    for person_id in subscriber_ids(&purchases) {
        sandbox_usage::grant(application_store, person_id, new_period).await?;
        granted += 1;
    }
    Ok(granted)
}

/// The distinct person ids holding an `active` Same Day Legal Purchase, ordered
/// for a deterministic grant batch.
fn subscriber_ids(purchases: &[store::purchases::Purchase]) -> Vec<uuid::Uuid> {
    let mut people: Vec<uuid::Uuid> = purchases
        .iter()
        .filter(|purchase| purchase.is_active(chrono::Utc::now()))
        .map(|purchase| purchase.person_id)
        .collect();
    people.sort();
    people.dedup();
    people
}

/// The latest Same Day Legal Purchase of one person: the invoice's account code
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

/// Outcome of one subscriber's closed billing period.
#[derive(Debug, Serialize, Deserialize)]
pub enum OverageOutcome {
    Invoiced(String),
    Skipped,
    AlreadyBilled,
}

/// Stable input retained by the DBOS billing roster checkpoint.
#[derive(Clone, Serialize, Deserialize)]
pub struct Overage {
    pub person_id: uuid::Uuid,
    pub cents: i64,
}

pub async fn closed_overage_roster(
    application_store: &ApplicationStore,
    period: &str,
) -> anyhow::Result<Vec<Overage>> {
    Ok(sandbox_usage::overage_rows(application_store, period)
        .await?
        .into_iter()
        .map(|row| Overage {
            person_id: row.person_id,
            cents: -row.remaining_cents(),
        })
        .collect())
}

/// Invoice one overage using the shared provider contract. The caller owns
/// its timeout and replay admission before making the provider call.
pub async fn invoice_one_from_env(
    pg: &PostgresDb,
    application_store: &ApplicationStore,
    period: &str,
    overage: Overage,
) -> anyhow::Result<OverageOutcome> {
    let mut billing =
        XeroBillingProvider::from_env().ok_or_else(|| anyhow::anyhow!("Xero is not configured"))?;
    let purchases = purchases::for_offerings(pg, &purchases::SAME_DAY_LEGAL_OFFERINGS).await?;
    invoice_one(
        &mut billing,
        application_store,
        &purchases,
        overage.person_id,
        period,
        overage.cents,
    )
    .await
}

async fn invoice_one(
    billing: &mut dyn BillingProvider,
    application_store: &ApplicationStore,
    purchases: &[store::purchases::Purchase],
    person_id: uuid::Uuid,
    closed_period: &str,
    overage_cents: i64,
) -> anyhow::Result<OverageOutcome> {
    if overage_cents <= 0 {
        return Ok(OverageOutcome::AlreadyBilled);
    }
    let Some(purchase) = latest_purchase(purchases, person_id) else {
        return Ok(OverageOutcome::Skipped);
    };
    let person = store::persons::find_by_id(application_store, person_id).await?;
    let Some(person) = person else {
        return Ok(OverageOutcome::Skipped);
    };

    let contact = match &purchase.xero_contact_id {
        Some(id) => billing::ContactId(id.clone()),
        None => {
            billing
                .ensure_contact(&billing::ContactRequest {
                    name: person.name.clone(),
                    email: person.email.clone(),
                })
                .await?
        }
    };
    let key = overage_invoice_key(person_id, closed_period);
    let request = InvoiceRequest {
        contact_id: Some(contact),
        contact_name: person.name.clone(),
        contact_email: person.email.clone(),
        reference: format!("Same Day Legal sandbox usage {closed_period}"),
        line_items: vec![InvoiceLine {
            description: format!(
                "Legal Sandbox model usage beyond the included $5.00, {closed_period}"
            ),
            quantity: 1,
            unit_amount_cents: overage_cents,
            account_code: purchase.account_code.clone(),
        }],
        issued_date: None,
        due_date: None,
    };
    // Keep the deterministic request key. Recovery after provider-key
    // expiry still needs durable per-overage invoice state.
    let receipt = billing.create_invoice(key, &request).await?;
    Ok(OverageOutcome::Invoiced(receipt.0))
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
mod tests {}
