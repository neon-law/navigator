//! Per-matter financial projections: Work in Progress and Outstanding Balance.
//!
//! Two read-only numbers a matter's summary strip and Billing surface open
//! with, both in integer cents and never floats — the same no-float rule the
//! Firm rollup applies:
//!
//! - **Work in Progress** (`wip_cents`): unbilled billable time priced at each
//!   entry's snapshotted rate plus unbilled confirmed
//!   [`Project Cost`](crate::project_costs) amounts — the work performed on
//!   the matter that no invoice has yet drawn.
//! - **Outstanding Balance** (`outstanding_cents`): invoiced minus paid over
//!   the matter's [`Xero Invoice`](crate::xero_invoices) mirror rows — what
//!   the client still owes. Voided invoices are owed by nobody and are left
//!   out.
//!
//! The module writes nothing: it reads the time-entry and project-cost tables
//! in PostgreSQL and the invoice mirror, and derives the two sums. Xero is
//! the books; the projection never touches it.
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::{postgres::PostgresDb, surreal::SurrealDb};

#[derive(Debug, thiserror::Error)]
pub enum MatterFinancialsError {
    #[error("Time Entry lookup failed")]
    Time(#[from] crate::time_entries::TimeEntryError),
    #[error("Project cost lookup failed")]
    Cost(#[from] crate::project_costs::ProjectCostError),
    #[error("invoice lookup failed")]
    Invoice(#[from] Box<crate::xero_invoices::XeroInvoiceError>),
    #[error("billing batch lookup failed")]
    Database(#[from] sqlx::Error),
}

/// The two projections, plus the parts each is composed from so a surface can
/// show what the Work in Progress figure draws from. All values are integer
/// cents.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatterFinancials {
    /// Unbilled billable time at snapshot rates plus unbilled confirmed
    /// Project costs.
    pub wip_cents: i64,
    /// Invoiced minus paid across the matter's non-voided mirrored invoices.
    pub outstanding_cents: i64,
    /// The time part of the Work in Progress figure.
    pub unbilled_time_cents: i64,
    /// The Project-cost part of the Work in Progress figure.
    pub unbilled_costs_cents: i64,
}

impl MatterFinancials {
    /// Derive both projections from their inputs. Pure, so the math is
    /// testable without a database.
    #[must_use]
    pub fn from_parts(
        entries: &[crate::time_entries::TimeEntry],
        costs: &[crate::project_costs::ProjectCost],
        invoices: &[crate::xero_invoices::XeroInvoice],
    ) -> Self {
        let unbilled_time_cents = unbilled_time_cents(entries);
        let unbilled_costs_cents = unbilled_costs_cents(costs);
        Self {
            unbilled_time_cents,
            unbilled_costs_cents,
            wip_cents: unbilled_time_cents + unbilled_costs_cents,
            outstanding_cents: outstanding_cents(invoices),
        }
    }
}

/// The Work in Progress time component: unbilled billable entries priced at
/// each entry's own snapshotted rate. Work drawn into a pending batch is
/// still unbilled — no invoice holds it — so it counts. Non-billable work,
/// written-off work, and work already under an invoice contribute nothing.
#[must_use]
pub fn unbilled_time_cents(entries: &[crate::time_entries::TimeEntry]) -> i64 {
    entries
        .iter()
        .filter(|entry| {
            entry.billable
                && matches!(
                    entry.billing_status,
                    crate::time_entries::BillingStatus::Unbilled
                        | crate::time_entries::BillingStatus::Reserved
                )
        })
        .map(|entry| {
            crate::time_entries::entry_amount_cents(entry.minutes, entry.rate_cents_per_hour)
        })
        .sum()
}

/// The Work in Progress cost component: confirmed receipts no invoice has
/// linked, including those drawn into a pending batch. An estimated amount
/// is not yet the firm's money and stays out.
#[must_use]
pub fn unbilled_costs_cents(costs: &[crate::project_costs::ProjectCost]) -> i64 {
    costs
        .iter()
        .filter(|cost| {
            cost.amount_status == "confirmed"
                && (cost.billing_status == "unbilled" || cost.billing_status == "reserved")
        })
        .map(|cost| cost.amount_cents)
        .sum()
}

/// What the client still owes: the unpaid remainder of every mirrored
/// invoice on the matter, voided ones excluded. An overpaid invoice
/// contributes its negative remainder rather than being clamped — the sum
/// stays honest about the matter's true position.
#[must_use]
pub fn outstanding_cents(invoices: &[crate::xero_invoices::XeroInvoice]) -> i64 {
    invoices
        .iter()
        .filter(|invoice| invoice.status != "VOIDED")
        .map(|invoice| invoice.amount_cents - invoice.amount_paid_cents)
        .sum()
}

/// Both projections for one matter. Reads only; the caller's authorization
/// seam (for example `store::access::matter_viewer`) governs who may call it.
///
/// # Errors
///
/// [`MatterFinancialsError`] when any of the three underlying reads fails.
pub async fn for_project(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    project_id: Uuid,
) -> Result<MatterFinancials, MatterFinancialsError> {
    let mut entries = crate::time_entries::wip_billable(pg, project_id).await?;
    let mut costs = crate::project_costs::list_for_project(pg, project_id).await?;
    let invoices = crate::xero_invoices::for_projects(surreal, &[project_id])
        .await
        .map_err(Box::new)?;
    // A partially settled batch can already have a mirrored invoice while
    // its work remains reserved. That invoice accounts for the work: keep
    // it out of WIP without hiding reservations from refused creates.
    let invoice_ids = invoices
        .iter()
        .filter(|invoice| invoice.status != "VOIDED")
        .map(|invoice| invoice.xero_invoice_id.as_str())
        .collect::<Vec<_>>();
    let batches = sqlx::query(
        "SELECT entry_ids,cost_ids FROM matter_billing_batch \
         WHERE project_id=$1 AND xero_invoice_id=ANY($2)",
    )
    .bind(project_id)
    .bind(&invoice_ids)
    .fetch_all(&pg.0)
    .await?;
    for batch in batches {
        let entry_ids: Vec<Uuid> = batch.try_get("entry_ids")?;
        let cost_ids: Vec<Uuid> = batch.try_get("cost_ids")?;
        entries.retain(|entry| !entry_ids.contains(&entry.id));
        costs.retain(|cost| !cost_ids.contains(&cost.id));
    }
    Ok(MatterFinancials::from_parts(&entries, &costs, &invoices))
}

#[cfg(test)]
mod tests {
    use super::{outstanding_cents, unbilled_costs_cents, unbilled_time_cents, MatterFinancials};
    use crate::project_costs::ProjectCost;
    use crate::time_entries::{BillingStatus, TimeEntry, TimeEntrySource};
    use crate::xero_invoices::XeroInvoice;
    use chrono::{NaiveDate, Utc};
    use uuid::Uuid;

    fn entry(minutes: i32, billable: bool, status: BillingStatus) -> TimeEntry {
        TimeEntry {
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            attorney_id: Uuid::new_v4(),
            performed_on: NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
            minutes,
            description: "Drafted the motion to compel".into(),
            billable,
            rate_cents_per_hour: 25_000,
            source: TimeEntrySource::Manual,
            billing_status: status,
            inserted_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn cost(amount_cents: i64, amount_status: &str, billing_status: &str) -> ProjectCost {
        ProjectCost {
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            occurred_on: NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
            payee: "Synthetic Court Reporter".into(),
            description: "Deposition transcript".into(),
            amount_cents,
            currency: "USD".into(),
            amount_status: amount_status.into(),
            billing_status: billing_status.into(),
            receipt_document_id: Uuid::new_v4(),
            source_reference: None,
            xero_invoice_id: None,
            inserted_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn invoice(amount_cents: i64, amount_paid_cents: i64, status: &str) -> XeroInvoice {
        XeroInvoice {
            project_id: Uuid::new_v4(),
            xero_invoice_id: format!("inv-{}", Uuid::new_v4().simple()),
            reference: "Matter 0000".into(),
            status: status.into(),
            amount_cents,
            amount_paid_cents,
            currency: "USD".into(),
            issued_at: Utc::now(),
            due_at: None,
            inserted_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn a_quiet_matter_projects_zero() {
        let financials = MatterFinancials::from_parts(&[], &[], &[]);
        assert_eq!(financials.wip_cents, 0);
        assert_eq!(financials.outstanding_cents, 0);
        assert_eq!(financials.unbilled_time_cents, 0);
        assert_eq!(financials.unbilled_costs_cents, 0);
    }

    #[test]
    fn wip_keeps_work_drawn_into_a_pending_batch() {
        // Reserved work carries no invoice yet: it stays part of the
        // matter's WIP, priced at its own snapshot.
        let entries = [
            entry(60, true, BillingStatus::Unbilled),
            entry(60, true, BillingStatus::Reserved),
            entry(
                60,
                true,
                BillingStatus::Invoiced {
                    xero_invoice_id: "inv-9".into(),
                },
            ),
        ];
        assert_eq!(unbilled_time_cents(&entries), 50_000);
        let costs = [
            cost(1_000, "confirmed", "unbilled"),
            cost(2_000, "confirmed", "reserved"),
            cost(4_000, "confirmed", "invoiced"),
        ];
        assert_eq!(unbilled_costs_cents(&costs), 3_000);
    }

    #[test]
    fn wip_prices_time_at_its_snapshotted_rate() {
        // 90 minutes at 25,000 cents/hour is 37,500 cents; a full hour is
        // 25,000.
        let entries = [
            entry(90, true, BillingStatus::Unbilled),
            entry(60, true, BillingStatus::Unbilled),
        ];
        assert_eq!(unbilled_time_cents(&entries), 62_500);
    }

    #[test]
    fn wip_leaves_out_non_billable_resolved_and_estimated_work() {
        let entries = [
            entry(60, true, BillingStatus::Unbilled),
            entry(60, false, BillingStatus::Unbilled),
            entry(
                60,
                true,
                BillingStatus::WrittenOff {
                    reason: "Flat fee".into(),
                },
            ),
            entry(
                60,
                true,
                BillingStatus::Invoiced {
                    xero_invoice_id: "inv-1".into(),
                },
            ),
        ];
        let costs = [
            cost(4_210, "confirmed", "unbilled"),
            cost(1_000, "estimated", "unbilled"),
            cost(2_500, "confirmed", "invoiced"),
        ];
        // Time: only the first entry; costs: only the confirmed unbilled one.
        let financials = MatterFinancials::from_parts(&entries, &costs, &[]);
        assert_eq!(financials.unbilled_time_cents, 25_000);
        assert_eq!(financials.unbilled_costs_cents, 4_210);
        assert_eq!(financials.wip_cents, 29_210);
    }

    #[test]
    fn outstanding_sums_invoiced_minus_paid_across_the_matter() {
        let invoices = [
            invoice(100_000, 40_000, "AUTHORISED"),
            invoice(50_000, 50_000, "PAID"),
            invoice(30_000, 0, "DRAFT"),
        ];
        // Everything except the voided row counts, drafts included: the mirror
        // row is the firm's own receivable, and the voided invoice is the one
        // thing nobody owes. 60_000 + 0 + 30_000.
        assert_eq!(outstanding_cents(&invoices), 90_000);
    }

    #[test]
    fn outstanding_skips_voided_invoices() {
        let invoices = [
            invoice(100_000, 0, "AUTHORISED"),
            invoice(75_000, 0, "VOIDED"),
        ];
        assert_eq!(outstanding_cents(&invoices), 100_000);
    }

    #[test]
    fn a_partially_paid_invoice_leaves_its_remainder_outstanding() {
        let invoices = [invoice(421_000, 50_000, "PARTIALLYPAID")];
        assert_eq!(outstanding_cents(&invoices), 371_000);
    }

    #[test]
    fn an_overpaid_invoice_counts_its_credit_against_the_matter() {
        let invoices = [
            invoice(50_000, 60_000, "PAID"),
            invoice(10_000, 0, "AUTHORISED"),
        ];
        assert_eq!(outstanding_cents(&invoices), 0);
    }

    #[test]
    fn the_projection_stays_on_the_cents_scale_for_odd_minutes() {
        // 1 minute at 25,000 cents/hour truncates to 416 cents — the same
        // per-entry amount the composed invoice sums.
        let entries = [entry(1, true, BillingStatus::Unbilled)];
        assert_eq!(unbilled_time_cents(&entries), 416);
    }
}
