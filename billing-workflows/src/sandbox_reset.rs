//! The `SandboxCreditReset` Restate workflow — the monthly job that turns
//! the calendar into the Legal Sandbox's credit contract.
//!
//! On the first of each month it grants every Same Day Legal subscriber the
//! glossary's fresh 500-cent credit ([`store::sandbox_usage::grant`], the
//! ledger's own idempotent rollover primitive — this job never touches
//! ledger rows directly), then bills the closed month's overage as one sent
//! Stripe invoice per subscriber, on the Stripe Operating account and
//! Customer the subscription itself is bound to.
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

use billing::stripe::{InvoiceCustomer, OneOffInvoice, StripeInvoiceClient};
use store::firm_secrets::{IntegrationProvider, IntegrationSecretKind};

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
    /// Closed-month overage rows that became no invoice: the subscriber has
    /// no Stripe-bound Same Day Legal Purchase (it is gone, or predates
    /// Stripe), so there is no account to bill. Reported, never silently
    /// dropped.
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

/// Where one subscriber's overage is billed: the Firm, Stripe account, and
/// Customer of their latest Stripe-bound Same Day Legal subscription. A
/// subscription bought before Stripe has none.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OverageTarget {
    firm: uuid::Uuid,
    account: String,
    customer: Option<String>,
}

fn overage_target(
    purchases: &[store::purchases::Purchase],
    person_id: uuid::Uuid,
) -> Option<OverageTarget> {
    purchases
        .iter()
        .filter(|purchase| purchase.person_id == person_id)
        .filter_map(|purchase| purchase.stripe.as_ref().map(|stripe| (purchase, stripe)))
        .max_by_key(|(purchase, _)| (purchase.inserted_at, purchase.id))
        .map(|(_, stripe)| OverageTarget {
            firm: stripe.firm_id,
            account: stripe.account_id.clone(),
            customer: stripe.customer_id.clone(),
        })
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

/// Invoice one overage as a sent Stripe invoice on the subscriber's own
/// Stripe account. The caller owns its timeout and replay admission before
/// making the provider call.
pub async fn invoice_one_from_env(
    pg: &PostgresDb,
    application_store: &ApplicationStore,
    period: &str,
    overage: Overage,
) -> anyhow::Result<OverageOutcome> {
    if overage.cents <= 0 {
        return Ok(OverageOutcome::AlreadyBilled);
    }
    let purchases = purchases::for_offerings(pg, &purchases::SAME_DAY_LEGAL_OFFERINGS).await?;
    let Some(target) = overage_target(&purchases, overage.person_id) else {
        return Ok(OverageOutcome::Skipped);
    };
    let Some(person) = store::persons::find_by_id(application_store, overage.person_id).await?
    else {
        return Ok(OverageOutcome::Skipped);
    };
    let kms = cloud::runtime_kms_from_env().await;
    let key = store::firm_secrets::resolve_firm_credential(
        application_store,
        target.firm,
        IntegrationProvider::Stripe,
        IntegrationSecretKind::StripeOperatingAppKey,
        kms.as_ref(),
    )
    .await?;
    let live = store::firm_secrets::stripe_mode()? == store::firm_secrets::Mode::Live;
    let stripe = StripeInvoiceClient::new(key.expose_for_provider_call(), live)?;
    if stripe.account_id().await? != target.account {
        anyhow::bail!("the Firm's Stripe account no longer matches the subscription");
    }
    let customer = match target.customer.as_deref() {
        Some(id) => InvoiceCustomer::Existing(id),
        None => InvoiceCustomer::New {
            name: &person.name,
            email: &person.email,
        },
    };
    let description =
        format!("Legal Sandbox model usage beyond the included $5.00, {period} (Same Day Legal)");
    // Keep the deterministic request key. Recovery after Stripe's key
    // retention still needs durable per-overage invoice state.
    let sent = stripe
        .send_one_off_invoice(&OneOffInvoice {
            key: overage_invoice_key(overage.person_id, period),
            customer,
            description: &description,
            amount_cents: overage.cents,
            days_until_due: OVERAGE_DUE_DAYS,
            metadata: ("navigator_sandbox_period", period.to_owned()),
        })
        .await?;
    Ok(OverageOutcome::Invoiced(sent.id))
}

/// Days after it is sent that an overage invoice falls due.
const OVERAGE_DUE_DAYS: u32 = 14;

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
    use super::*;
    use chrono::{TimeDelta, Utc};
    use store::purchases::{Cadence, Purchase, PurchaseState, StripePurchase};

    fn purchase(person: uuid::Uuid, account: Option<&str>, age_days: i64) -> Purchase {
        let at = Utc::now() - TimeDelta::days(age_days);
        Purchase {
            id: uuid::Uuid::now_v7(),
            person_id: person,
            email: "subscriber@example.com".into(),
            offering: "delete-your-data-monthly".into(),
            offering_name: "Same Day Legal".into(),
            cadence: Cadence::Month,
            amount_cents: 2_900,
            currency: "USD".into(),
            account_code: "200".into(),
            state: PurchaseState::Active,
            invoice_url: None,
            inserted_at: at,
            updated_at: at,
            activated_at: Some(at),
            stripe: account.map(|account| StripePurchase {
                firm_id: uuid::Uuid::nil(),
                account_id: account.into(),
                checkout_session_id: Some("cs_test_synthetic".into()),
                customer_id: Some(format!("cus_{}", account.trim_start_matches("acct_"))),
                subscription_id: Some("sub_synthetic".into()),
                period_end: Some(at + TimeDelta::days(30)),
                cancel_at_period_end: false,
                renewal_terms: "Renews monthly.".into(),
                consented_at: at,
            }),
            stripe_invoice: None,
            renewal_cancelled_at: None,
            access_ends_at: None,
        }
    }

    #[test]
    fn overage_bills_the_latest_stripe_subscription_and_skips_one_bought_before_stripe() {
        let person = uuid::Uuid::now_v7();
        let other = uuid::Uuid::now_v7();
        let purchases = [
            purchase(person, Some("acct_Older"), 90),
            purchase(person, Some("acct_Newer"), 10),
            purchase(person, None, 1),
            purchase(other, None, 5),
        ];
        let target = overage_target(&purchases, person).unwrap();
        assert_eq!(target.account, "acct_Newer");
        assert_eq!(target.customer.as_deref(), Some("cus_Newer"));
        assert_eq!(overage_target(&purchases, other), None);
        assert_eq!(overage_target(&purchases, uuid::Uuid::now_v7()), None);
    }

    #[test]
    fn each_subscriber_month_has_one_stable_invoice_key() {
        let person = uuid::Uuid::now_v7();
        assert_eq!(
            overage_invoice_key(person, "2026-10"),
            overage_invoice_key(person, "2026-10")
        );
        assert_ne!(
            overage_invoice_key(person, "2026-10"),
            overage_invoice_key(person, "2026-11")
        );
    }
}
