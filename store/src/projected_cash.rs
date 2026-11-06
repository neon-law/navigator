//! Projected contingency cash, summed across matters: the lawyer
//! workbench's [Contingency Pipeline](lawyer_pipeline).
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
use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate};
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

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::{fold_pipeline, pending, Quarter};
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
}
