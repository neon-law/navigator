//! Projected contingency cash, summed across matters: the lawyer
//! workbench's [Contingency Pipeline](lawyer_pipeline) and the Firm show
//! page's [rollup](firm_rollup).
//!
//! Both fold [`crate::matter_projections`] and never re-derive its math. A
//! matter counts while it is open and its odds are still a judgment — the
//! lawyer's estimate, a stage default, or none yet. A recorded Matter
//! Outcome is realized cash, not pipeline, so it stays out. Expected cash is
//! Σ p·(fee + costs recovered); a matter whose odds carry no recoveries (a
//! stage default) is counted apart, never as 0.
//!
//! Cash lands in the quarter of the estimate's expected resolution date; an
//! overdue date lands in the current quarter, and anything past
//! [`HORIZON_QUARTERS`] lands in one `Later` bucket.
//!
//! **Privacy of the Firm rollup.** [`firm_rollup`] is the one place that
//! builds its labels, and they are only a brand's display name, a lead
//! lawyer's display name, or `"Unassigned"`, the rule
//! [`crate::xero_invoices::firm_thirty_day_rollup`] set for the invoice
//! graphs. No Project code, matter name, client name, Project id, or
//! invoice id reaches its output; the concentration warning is a share, not
//! a matter.
use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate, TimeZone, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::matter_projections::{
    ContingentProjection, MatterProjection, MatterProjectionError, OddsSource,
};
use crate::postgres::PostgresDb;
use crate::projects::Project;
use crate::surreal::SurrealDb;

/// How many quarters past the current one get their own bucket.
pub const HORIZON_QUARTERS: i32 = 7;

/// One matter holding more than this share of the Firm's expected cash, in
/// basis points, raises the concentration warning.
pub const CONCENTRATION_THRESHOLD_BPS: i64 = 2_500;

const UNASSIGNED: &str = "Unassigned";

#[derive(Debug, thiserror::Error)]
pub enum ProjectedCashError {
    #[error(transparent)]
    Projection(Box<MatterProjectionError>),
    #[error(transparent)]
    Projects(#[from] crate::projects::ProjectStoreError),
    #[error(transparent)]
    Invoices(Box<crate::xero_invoices::XeroInvoiceError>),
    #[error(transparent)]
    Brands(#[from] crate::brands::BrandError),
}

impl From<MatterProjectionError> for ProjectedCashError {
    fn from(error: MatterProjectionError) -> Self {
        Self::Projection(Box::new(error))
    }
}

impl From<crate::xero_invoices::XeroInvoiceError> for ProjectedCashError {
    fn from(error: crate::xero_invoices::XeroInvoiceError) -> Self {
        Self::Invoices(Box::new(error))
    }
}

/// A calendar quarter, counted from year 0 so quarters order and subtract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Quarter(i32);

impl Quarter {
    fn of(date: NaiveDate) -> Self {
        let zero_based = i32::try_from(date.month0() / 3).unwrap_or(0);
        Self(date.year() * 4 + zero_based)
    }

    fn label(self) -> String {
        format!("{} Q{}", self.0.div_euclid(4), self.0.rem_euclid(4) + 1)
    }

    fn start(self) -> NaiveDate {
        let month = u32::try_from(self.0.rem_euclid(4) * 3 + 1).unwrap_or(1);
        NaiveDate::from_ymd_opt(self.0.div_euclid(4), month, 1).unwrap_or_default()
    }
}

/// Where a matter's cash lands, relative to `today`'s quarter.
fn bucket(resolves_on: Option<NaiveDate>, today: Quarter) -> Option<Quarter> {
    resolves_on.map(|on| Quarter::of(on).clamp(today, Quarter(today.0 + HORIZON_QUARTERS + 1)))
}

fn bucket_label(quarter: Quarter, today: Quarter) -> String {
    if quarter.0 > today.0 + HORIZON_QUARTERS {
        "Later".to_string()
    } else {
        quarter.label()
    }
}

/// An open matter's contingent projection while its odds are a judgment.
fn pending(project: &Project, projection: MatterProjection) -> Option<ContingentProjection> {
    match projection {
        MatterProjection::Contingent(projection)
            if project.status == "open" && projection.source != Some(OddsSource::Actual) =>
        {
            Some(projection)
        }
        _ => None,
    }
}

/// `projects`' projections as `viewer`. A matter the PostgreSQL guard does
/// not count `viewer` firm-side on is left out, never shown.
async fn pending_projections<'a>(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    viewer: Uuid,
    projects: impl IntoIterator<Item = &'a Project>,
    on: NaiveDate,
) -> Result<Vec<(&'a Project, ContingentProjection)>, MatterProjectionError> {
    let mut pending_matters = Vec::new();
    for project in projects {
        if project.status != "open" {
            continue;
        }
        match crate::matter_projections::for_project(surreal, pg, viewer, project.id, on).await {
            Ok(projection) => {
                if let Some(projection) = pending(project, projection) {
                    pending_matters.push((project, projection));
                }
            }
            Err(MatterProjectionError::NotFirmSide) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(pending_matters)
}

/// Expected cash in one quarter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuarterCash {
    /// `2027 Q2`, or `Later`.
    pub label: String,
    pub expected_cash_cents: i64,
}

/// One matter on the workbench pipeline. The caller already sees the
/// matter, so it carries its code and name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PipelineMatter {
    pub project_id: Uuid,
    pub code: String,
    pub name: String,
    pub source: Option<OddsSource>,
    pub expected_net_cents: Option<i64>,
    pub net_range_cents: Option<(i64, i64)>,
    pub costs_at_risk_cents: i64,
    pub resolves_on: Option<NaiveDate>,
    pub stale: bool,
}

/// The lawyer workbench's Contingency Pipeline over the caller's matters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct LawyerPipeline {
    /// Σ expected fee over matters whose odds carry recoveries.
    pub expected_fee_cents: i64,
    /// Σ expected net over the same matters.
    pub expected_net_cents: i64,
    /// Σ of each matter's lowest and highest net.
    pub net_range_cents: (i64, i64),
    /// Σ costs advanced over every pending contingent matter.
    pub costs_at_risk_cents: i64,
    /// Expected cash by quarter, in order.
    pub by_quarter: Vec<QuarterCash>,
    /// Every pending contingent matter, highest expected net first; matters
    /// with no expected net yet follow, by name.
    pub matters: Vec<PipelineMatter>,
}

fn quarters(cash: BTreeMap<Quarter, i64>, today: Quarter) -> Vec<QuarterCash> {
    cash.into_iter()
        .map(|(quarter, expected_cash_cents)| QuarterCash {
            label: bucket_label(quarter, today),
            expected_cash_cents,
        })
        .collect()
}

/// The pipeline over projections already read. Pure.
fn fold_pipeline(matters: &[(&Project, ContingentProjection)], today: NaiveDate) -> LawyerPipeline {
    let today = Quarter::of(today);
    let mut pipeline = LawyerPipeline::default();
    let mut cash = BTreeMap::new();
    for (project, projection) in matters {
        pipeline.costs_at_risk_cents += projection.costs_at_risk_cents;
        if let (Some(fee), Some(net), Some((low, high))) = (
            projection.expected_fee_cents,
            projection.expected_net_cents,
            projection.net_range_cents,
        ) {
            pipeline.expected_fee_cents += fee;
            pipeline.expected_net_cents += net;
            pipeline.net_range_cents.0 += low;
            pipeline.net_range_cents.1 += high;
        }
        if let (Some(quarter), Some(expected)) = (
            bucket(projection.resolves_on, today),
            projection.expected_cash_cents(),
        ) {
            *cash.entry(quarter).or_insert(0) += expected;
        }
        pipeline.matters.push(PipelineMatter {
            project_id: project.id,
            code: project.code.clone(),
            name: project.name.clone(),
            source: projection.source,
            expected_net_cents: projection.expected_net_cents,
            net_range_cents: projection.net_range_cents,
            costs_at_risk_cents: projection.costs_at_risk_cents,
            resolves_on: projection.resolves_on,
            stale: projection.stale,
        });
    }
    pipeline.matters.sort_by(|a, b| {
        b.expected_net_cents
            .is_some()
            .cmp(&a.expected_net_cents.is_some())
            .then(b.expected_net_cents.cmp(&a.expected_net_cents))
            .then_with(|| a.name.cmp(&b.name))
    });
    pipeline.by_quarter = quarters(cash, today);
    pipeline
}

/// The Contingency Pipeline over `projects`, read as `viewer`. Pass the
/// collection the workbench KPIs read
/// ([`crate::access::visible_projects_as_lawyer`]); a matter the
/// PostgreSQL guard does not count `viewer` firm-side on is left out too.
///
/// # Errors
///
/// The failed projection read.
pub async fn lawyer_pipeline(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    viewer: Uuid,
    projects: &[Project],
    on: NaiveDate,
) -> Result<LawyerPipeline, MatterProjectionError> {
    let matters = pending_projections(surreal, pg, viewer, projects, on).await?;
    Ok(fold_pipeline(&matters, on))
}

/// One label's expected cash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CashGroup {
    /// A brand's display name, a lead lawyer's display name, or
    /// `"Unassigned"`.
    pub label: String,
    pub expected_cash_cents: i64,
    pub matters: usize,
}

/// One quarter of the Firm rollup: projected cash beside the USD invoiced
/// and paid on invoices issued that quarter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FirmQuarter {
    /// `2027 Q2`, or `Later`.
    pub label: String,
    pub projected_cash_cents: i64,
    pub invoiced_cents: i64,
    pub paid_cents: i64,
}

/// The Firm show page's projected contingency cash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct FirmProjectedCash {
    pub expected_cash_cents: i64,
    /// Σ of each matter's lowest and highest cash.
    pub cash_range_cents: (i64, i64),
    /// The three quarters before this one, this one, and every quarter with
    /// projected cash after it.
    pub quarters: Vec<FirmQuarter>,
    /// Sorted by label.
    pub by_brand: Vec<CashGroup>,
    /// Sorted by label. A matter with two lead lawyers counts under each.
    pub by_lead_lawyer: Vec<CashGroup>,
    /// The largest single matter's share of the expected cash, in basis
    /// points, when it exceeds [`CONCENTRATION_THRESHOLD_BPS`].
    pub concentration_bps: Option<i64>,
    /// Open contingent matters whose odds carry no recoveries yet.
    pub unestimated_matters: usize,
}

/// One pending matter as the Firm rollup reads it.
struct FirmMatter<'a> {
    brand: String,
    lead_lawyers: &'a [String],
    projection: ContingentProjection,
}

fn add(groups: &mut BTreeMap<String, (i64, usize)>, label: &str, cents: i64) {
    let entry = groups.entry(label.to_string()).or_default();
    entry.0 += cents;
    entry.1 += 1;
}

fn into_groups(groups: BTreeMap<String, (i64, usize)>) -> Vec<CashGroup> {
    groups
        .into_iter()
        .map(|(label, (expected_cash_cents, matters))| CashGroup {
            label,
            expected_cash_cents,
            matters,
        })
        .collect()
}

/// The Firm rollup over projections and invoices already read. Pure; it
/// reads nothing about a matter but its brand label, its lead lawyers'
/// names, and its projection.
fn fold_firm(
    matters: &[FirmMatter<'_>],
    invoices: &[crate::xero_invoices::FirmInvoice],
    today: NaiveDate,
) -> FirmProjectedCash {
    let today = Quarter::of(today);
    let mut rollup = FirmProjectedCash::default();
    let mut by_quarter: BTreeMap<Quarter, (i64, i64, i64)> = (today.0 - 3..=today.0)
        .map(|q| (Quarter(q), (0, 0, 0)))
        .collect();
    let (mut by_brand, mut by_lead_lawyer) = (BTreeMap::new(), BTreeMap::new());
    let mut largest = 0;
    for matter in matters {
        let (Some(cash), Some((low, high))) = (
            matter.projection.expected_cash_cents(),
            matter.projection.cash_range_cents(),
        ) else {
            rollup.unestimated_matters += 1;
            continue;
        };
        rollup.expected_cash_cents += cash;
        rollup.cash_range_cents.0 += low;
        rollup.cash_range_cents.1 += high;
        largest = largest.max(cash);
        if let Some(quarter) = bucket(matter.projection.resolves_on, today) {
            by_quarter.entry(quarter).or_default().0 += cash;
        }
        add(&mut by_brand, &matter.brand, cash);
        if matter.lead_lawyers.is_empty() {
            add(&mut by_lead_lawyer, UNASSIGNED, cash);
        }
        for name in matter.lead_lawyers {
            add(&mut by_lead_lawyer, name, cash);
        }
    }
    for invoice in invoices.iter().filter(|invoice| invoice.currency == "USD") {
        let quarter = Quarter::of(invoice.issued_at.date_naive());
        if let Some(totals) = by_quarter.get_mut(&quarter) {
            totals.1 += invoice.amount_cents;
            totals.2 += invoice.amount_paid_cents;
        }
    }
    if rollup.expected_cash_cents > 0 {
        let share = largest * 10_000 / rollup.expected_cash_cents;
        rollup.concentration_bps = (share > CONCENTRATION_THRESHOLD_BPS).then_some(share);
    }
    rollup.quarters = by_quarter
        .into_iter()
        .map(|(quarter, (projected, invoiced, paid))| FirmQuarter {
            label: bucket_label(quarter, today),
            projected_cash_cents: projected,
            invoiced_cents: invoiced,
            paid_cents: paid,
        })
        .collect();
    rollup.by_brand = into_groups(by_brand);
    rollup.by_lead_lawyer = into_groups(by_lead_lawyer);
    rollup
}

/// The Firm's projected contingency cash on `on`, read as `viewer` (an
/// Owner or Admin), over every open matter the Firm owns.
///
/// # Errors
///
/// The failed Project, projection, brand, or invoice read.
pub async fn firm_rollup(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    viewer: Uuid,
    firm_id: Uuid,
    on: NaiveDate,
) -> Result<FirmProjectedCash, ProjectedCashError> {
    let projects: Vec<Project> = crate::projects::all(surreal)
        .await?
        .into_iter()
        .filter(|project| project.firm_id == Some(firm_id))
        .collect();
    let pending_matters = pending_projections(surreal, pg, viewer, &projects, on).await?;
    let lead_lawyers = crate::projects::dri_names_by_project(surreal, "is_lawyer_dri").await?;
    let mut brand_names: BTreeMap<&str, String> = BTreeMap::new();
    for (project, _) in &pending_matters {
        if !brand_names.contains_key(project.brand.as_str()) {
            let name = crate::brands::find_by_key(surreal, &project.brand)
                .await?
                .map_or_else(|| project.brand.clone(), |brand| brand.name);
            brand_names.insert(project.brand.as_str(), name);
        }
    }
    let matters: Vec<FirmMatter<'_>> = pending_matters
        .into_iter()
        .map(|(project, projection)| FirmMatter {
            brand: brand_names[project.brand.as_str()].clone(),
            lead_lawyers: lead_lawyers.get(&project.id).map_or(&[], Vec::as_slice),
            projection,
        })
        .collect();
    let since = Utc.from_utc_datetime(
        &Quarter(Quarter::of(on).0 - 3)
            .start()
            .and_hms_opt(0, 0, 0)
            .unwrap_or_default(),
    );
    let invoices = crate::xero_invoices::for_firm_since(surreal, firm_id, since).await?;
    Ok(fold_firm(&matters, &invoices, on))
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, TimeZone, Utc};

    use super::{fold_firm, fold_pipeline, pending, FirmMatter, Quarter};
    use crate::contingency_terms::CostsDeducted;
    use crate::matter_projections::{
        project_contingent, ContingentInputs, ContingentProjection, MatterProjection, Odds,
        PositionedTier,
    };
    use crate::outcome_estimates::tests::worked_example;

    const COSTS: i64 = 1_200_000;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 6).unwrap()
    }

    /// The worked example: $62,970 expected cash, $50,970 expected net.
    fn projection(odds: Odds, resolves_on: Option<NaiveDate>) -> ContingentProjection {
        let mut projection = project_contingent(&ContingentInputs {
            costs_deducted: CostsDeducted::AfterFee,
            tiers: vec![
                PositionedTier {
                    stage_position: 1,
                    fee_bps: 3_330,
                },
                PositionedTier {
                    stage_position: 5,
                    fee_bps: 4_000,
                },
            ],
            current_stage_position: Some(3),
            odds,
            costs_advanced_cents: COSTS,
            time_cost_cents: 0,
        });
        projection.resolves_on = resolves_on;
        projection
    }

    fn estimated(on: (i32, u32, u32)) -> ContingentProjection {
        projection(
            Odds::Lawyer(worked_example()),
            NaiveDate::from_ymd_opt(on.0, on.1, on.2),
        )
    }

    fn defaulted() -> ContingentProjection {
        projection(
            Odds::StageDefault(crate::case_stages::StageDefaultOdds {
                win_at_trial_bps: 1_000,
                settle_bps: 5_000,
                lose_bps: 4_000,
            }),
            None,
        )
    }

    async fn project(
        surreal: &crate::surreal::SurrealDb,
        code: &str,
        name: &str,
    ) -> crate::projects::Project {
        crate::projects::create(
            surreal,
            &crate::projects::NewProject {
                code: code.to_string(),
                name: name.to_string(),
                status: "open".to_string(),
                entity_id: crate::test_support::seed_entity(surreal).await,
                brand: "neon".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap()
    }

    #[test]
    fn quarters_label_and_order_by_calendar() {
        assert_eq!(Quarter::of(today()).label(), "2026 Q4");
        assert_eq!(Quarter(Quarter::of(today()).0 + 1).label(), "2027 Q1");
        assert_eq!(
            Quarter(Quarter::of(today()).0 - 3).start(),
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()
        );
    }

    #[tokio::test]
    async fn a_resolved_or_closed_matter_is_not_pipeline() {
        let surreal = crate::surreal::test_support::mem().await;
        let mut open = project(&surreal, "open-matter", "Open matter").await;
        let actual = projection(
            Odds::Actual {
                result: crate::matter_outcomes::MatterResult::Won,
                gross_recovery_cents: 1,
                fee_received_cents: 1,
                costs_recovered_cents: 0,
            },
            None,
        );
        assert!(pending(&open, MatterProjection::Contingent(actual)).is_none());
        assert!(pending(&open, MatterProjection::Contingent(defaulted())).is_some());
        open.status = "closed".to_string();
        assert!(pending(&open, MatterProjection::Contingent(defaulted())).is_none());
    }

    #[tokio::test]
    async fn the_pipeline_sums_estimated_matters_and_ranks_by_expected_net() {
        let surreal = crate::surreal::test_support::mem().await;
        let (a, b, c) = (
            project(&surreal, "alpha", "Alpha").await,
            project(&surreal, "bravo", "Bravo").await,
            project(&surreal, "charlie", "Charlie").await,
        );
        let mut cheaper = estimated((2027, 2, 1));
        cheaper.expected_net_cents = Some(1_000);
        cheaper.stale = true;
        let pipeline = fold_pipeline(
            &[
                (&a, defaulted()),
                (&b, cheaper),
                // Overdue: lands in the current quarter.
                (&c, estimated((2026, 3, 1))),
            ],
            today(),
        );
        assert_eq!(pipeline.expected_fee_cents, 2 * 5_397_000);
        assert_eq!(pipeline.expected_net_cents, 1_000 + 5_097_000);
        assert_eq!(pipeline.net_range_cents, (2 * -1_200_000, 2 * 16_000_000));
        assert_eq!(pipeline.costs_at_risk_cents, 3 * COSTS);
        let ranked: Vec<_> = pipeline.matters.iter().map(|m| m.code.as_str()).collect();
        assert_eq!(ranked, ["charlie", "bravo", "alpha"], "no estimate last");
        assert!(pipeline.matters[1].stale);
        let quarters: Vec<_> = pipeline
            .by_quarter
            .iter()
            .map(|q| (q.label.as_str(), q.expected_cash_cents))
            .collect();
        assert_eq!(quarters, [("2026 Q4", 6_297_000), ("2027 Q1", 6_297_000)]);
    }

    #[tokio::test]
    async fn far_off_resolutions_share_one_later_bucket() {
        let surreal = crate::surreal::test_support::mem().await;
        let (a, b) = (
            project(&surreal, "far", "Far").await,
            project(&surreal, "farther", "Farther").await,
        );
        let pipeline = fold_pipeline(
            &[(&a, estimated((2029, 1, 1))), (&b, estimated((2035, 1, 1)))],
            today(),
        );
        let quarters: Vec<_> = pipeline
            .by_quarter
            .iter()
            .map(|q| (q.label.as_str(), q.expected_cash_cents))
            .collect();
        assert_eq!(quarters, [("Later", 2 * 6_297_000)]);
    }

    fn firm_matter<'a>(
        brand: &str,
        lead_lawyers: &'a [String],
        projection: ContingentProjection,
    ) -> FirmMatter<'a> {
        FirmMatter {
            brand: brand.to_string(),
            lead_lawyers,
            projection,
        }
    }

    #[test]
    fn the_firm_rollup_groups_by_brand_and_lead_lawyer_and_flags_concentration() {
        let one = vec!["Lawyer One".to_string()];
        let rollup = fold_firm(
            &[
                firm_matter("Neon Law", &one, estimated((2027, 1, 15))),
                firm_matter("Other Brand", &[], estimated((2026, 12, 1))),
                firm_matter("Neon Law", &one, defaulted()),
            ],
            &[],
            today(),
        );
        assert_eq!(rollup.expected_cash_cents, 2 * 6_297_000);
        assert_eq!(rollup.cash_range_cents, (0, 2 * 17_200_000));
        assert_eq!(rollup.unestimated_matters, 1);
        let brands: Vec<_> = rollup
            .by_brand
            .iter()
            .map(|g| (g.label.as_str(), g.expected_cash_cents, g.matters))
            .collect();
        assert_eq!(
            brands,
            [("Neon Law", 6_297_000, 1), ("Other Brand", 6_297_000, 1)]
        );
        let lawyers: Vec<_> = rollup
            .by_lead_lawyer
            .iter()
            .map(|g| g.label.as_str())
            .collect();
        assert_eq!(lawyers, ["Lawyer One", "Unassigned"]);
        assert_eq!(rollup.concentration_bps, Some(5_000), "each holds half");
        let labels: Vec<_> = rollup.quarters.iter().map(|q| q.label.as_str()).collect();
        assert_eq!(
            labels,
            ["2026 Q1", "2026 Q2", "2026 Q3", "2026 Q4", "2027 Q1"]
        );
        assert_eq!(rollup.quarters[3].projected_cash_cents, 6_297_000);
    }

    #[test]
    fn spread_cash_raises_no_concentration_warning_and_invoices_sit_beside_it() {
        let matters: Vec<_> = (0..5)
            .map(|_| firm_matter("Neon Law", &[], estimated((2027, 1, 15))))
            .collect();
        let issued = |y, m, d| Utc.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap();
        let invoice = |issued_at, currency: &str| crate::xero_invoices::FirmInvoice {
            project_id: uuid::Uuid::nil(),
            xero_invoice_id: "INV-1".to_string(),
            status: "PAID".to_string(),
            amount_cents: 10_000,
            amount_paid_cents: 4_000,
            issued_at,
            currency: currency.to_string(),
            brand: "neon".to_string(),
            lawyer_dris: vec![],
        };
        let rollup = fold_firm(
            &matters,
            &[
                invoice(issued(2026, 8, 1), "USD"),
                invoice(issued(2026, 8, 2), "EUR"),
                invoice(issued(2024, 1, 1), "USD"),
            ],
            today(),
        );
        assert_eq!(rollup.concentration_bps, None, "20% each");
        let q3 = &rollup.quarters[2];
        assert_eq!(q3.label, "2026 Q3");
        assert_eq!(
            (q3.invoiced_cents, q3.paid_cents),
            (10_000, 4_000),
            "USD only"
        );
    }

    /// ENG-1250 privacy: the rollup's output carries only brand names, lead
    /// lawyer names, "Unassigned", quarters, and cents, whatever the matters
    /// and invoices behind it are called.
    #[tokio::test]
    async fn no_project_code_matter_name_client_or_invoice_id_reaches_the_rollup() {
        let surreal = crate::surreal::test_support::mem().await;
        let secret = project(&surreal, "secret-code", "Secret Matter v. Hidden Client").await;
        let lawyers = vec!["Lawyer One".to_string()];
        let rollup = fold_firm(
            &[
                firm_matter("Neon Law", &lawyers, estimated((2027, 1, 15))),
                firm_matter("Neon Law", &[], estimated((2027, 3, 1))),
            ],
            &[crate::xero_invoices::FirmInvoice {
                project_id: secret.id,
                xero_invoice_id: "xero-secret-id".to_string(),
                status: "AUTHORISED".to_string(),
                amount_cents: 1,
                amount_paid_cents: 0,
                issued_at: Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
                currency: "USD".to_string(),
                brand: secret.brand.clone(),
                lawyer_dris: vec!["Lawyer One".to_string()],
            }],
            today(),
        );
        let json = serde_json::to_string(&rollup).unwrap();
        for forbidden in [
            secret.code.as_str(),
            secret.name.as_str(),
            "Hidden Client",
            "xero-secret-id",
            &secret.id.to_string(),
            &secret.entity_id.to_string(),
        ] {
            assert!(!json.contains(forbidden), "{forbidden} leaked into {json}");
        }
        assert!(
            json.contains("Neon Law") && json.contains("Unassigned"),
            "{json}"
        );
    }
}
