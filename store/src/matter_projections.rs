//! Per-matter profit projection: what a matter is expected to bring in, net
//! of the costs the firm advanced and the time it spent.
//!
//! A pure, read-only derivation in integer cents, beside
//! [`crate::matter_financials`]. It writes nothing.
//!
//! - A **contingent** matter (one with a [Contingency
//!   Term](crate::contingency_terms)) projects each [`Outcome`] — win at
//!   trial, settle, lose — from its odds:
//!   - **fee(o)** applies the term's tier for the stage `o` resolves at to the
//!     gross recovery, honoring [`CostsDeducted`]. Winning at trial resolves
//!     at the last tier's stage; settling resolves at the matter's current
//!     stage, paying the last tier whose stage is at or before it (the first
//!     tier when the matter is earlier, or unstaged). Losing pays no fee.
//!   - **costs recovered(o)** is the confirmed Project Costs advanced when the
//!     outcome recovers anything, capped at the recovery, and 0 otherwise.
//!   - **net(o)** = fee(o) + costs recovered(o) − costs advanced − time cost,
//!     where time cost is the matter's billable Time Entries at their
//!     snapshotted `rate_cents_per_hour`. Profit means cash minus time.
//!
//!   The expected value is Σ p·net(o), shown with the range of nets the
//!   possible outcomes span. The odds come from the matter's current Outcome
//!   Estimate ([`OddsSource::Lawyer`]); without one, from its current stage's
//!   Firm defaults ([`OddsSource::StageDefault`]), which carry odds but no
//!   recovery and so no expected fee; and once a Matter Outcome exists, from
//!   the actual figures ([`OddsSource::Actual`]).
//! - An **hourly or flat-fee** matter projects its Work in Progress plus its
//!   Outstanding Balance with no odds, so firm totals add up across fee kinds.
//!
//! A hybrid term's hourly billing is not projected as income here; its time
//! still counts as time cost. Expected values divide once, after summing, and
//! truncate toward zero, the cents rule the rest of billing uses.
//!
//! The projection is firm work product: [`for_project`] refuses any reader
//! [`crate::firm_side::is_firm_side`] refuses.
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::case_stages::StageDefaultOdds;
use crate::contingency_terms::{CostsDeducted, BASIS_POINTS};
use crate::matter_financials::MatterFinancials;
use crate::matter_outcomes::MatterResult;
use crate::outcome_estimates::{Outcome, OutcomeSet};
use crate::postgres::PostgresDb;
use crate::surreal::SurrealDb;

#[derive(Debug, thiserror::Error)]
pub enum MatterProjectionError {
    /// The reader is not a firm-side participant on the matter. Callers
    /// answer with their non-disclosing refusal.
    #[error("only a firm-side participant may read a matter's projection")]
    NotFirmSide,
    #[error("Contingency Term lookup failed")]
    Term(#[from] crate::contingency_terms::ContingencyTermError),
    #[error("Outcome Estimate lookup failed")]
    Estimate(#[from] crate::outcome_estimates::OutcomeEstimateError),
    #[error("Matter Outcome lookup failed")]
    Outcome(#[from] crate::matter_outcomes::MatterOutcomeError),
    #[error("Case Stage lookup failed")]
    Stage(#[from] crate::case_stages::CaseStageError),
    #[error("Time Entry lookup failed")]
    Time(#[from] crate::time_entries::TimeEntryError),
    #[error("Project cost lookup failed")]
    Cost(#[from] crate::project_costs::ProjectCostError),
    #[error("matter financials lookup failed")]
    Financials(#[from] crate::matter_financials::MatterFinancialsError),
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
}

/// Where a contingent projection's odds came from. Every surface labels
/// it, so nobody reads a Firm default as a lawyer's judgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OddsSource {
    /// The matter's current Outcome Estimate.
    Lawyer,
    /// The current Case Stage's Firm default odds.
    StageDefault,
    /// The recorded Matter Outcome.
    Actual,
}

/// The odds a contingent projection is computed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Odds {
    /// The lawyer's estimate: odds and recoveries.
    Lawyer(OutcomeSet),
    /// The stage's Firm defaults: odds only.
    StageDefault(StageDefaultOdds),
    /// What actually happened.
    Actual {
        result: MatterResult,
        gross_recovery_cents: i64,
        fee_received_cents: i64,
        costs_recovered_cents: i64,
    },
    /// No estimate, no stage default, no outcome.
    Unset,
}

/// One term tier, placed by its stage's position in the Firm's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionedTier {
    pub stage_position: i32,
    pub fee_bps: i32,
}

/// Everything a contingent projection is derived from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContingentInputs {
    pub costs_deducted: CostsDeducted,
    /// The term's tiers, in the Firm's stage order.
    pub tiers: Vec<PositionedTier>,
    /// The matter's current stage position, `None` before it is staged.
    pub current_stage_position: Option<i32>,
    pub odds: Odds,
    /// Confirmed Project Costs the firm advanced on the matter.
    pub costs_advanced_cents: i64,
    /// Billable Time Entries at their snapshotted rates.
    pub time_cost_cents: i64,
}

/// One outcome's projection. Amounts are `None` when the odds carry no
/// recovery (a stage default) or the outcome did not happen (actual).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeProjection {
    pub outcome: Outcome,
    pub probability_bps: i32,
    pub gross_recovery_cents: Option<i64>,
    /// The tier applied, in basis points.
    pub fee_bps: i32,
    pub fee_cents: Option<i64>,
    pub costs_recovered_cents: Option<i64>,
    pub net_cents: Option<i64>,
}

/// A contingent matter's projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContingentProjection {
    /// `None` when the matter has no estimate, no stage default, and no
    /// outcome: there are no odds to show.
    pub source: Option<OddsSource>,
    /// Win at trial, settle, and lose, in that order; empty without odds.
    pub outcomes: Vec<OutcomeProjection>,
    /// Σ p·fee(o); `None` without recoveries.
    pub expected_fee_cents: Option<i64>,
    /// Σ p·(costs advanced − costs recovered(o)); `None` without recoveries.
    pub expected_costs_lost_cents: Option<i64>,
    /// Σ p·net(o), the expected profit; `None` without recoveries.
    pub expected_net_cents: Option<i64>,
    /// The lowest and highest net among outcomes that can happen.
    pub net_range_cents: Option<(i64, i64)>,
    /// What the firm loses if the matter recovers nothing: the confirmed
    /// costs it advanced.
    pub costs_at_risk_cents: i64,
    pub costs_advanced_cents: i64,
    pub time_cost_cents: i64,
    /// When the current Outcome Estimate was recorded ([`OddsSource::Lawyer`]).
    pub estimate_recorded_at: Option<DateTime<Utc>>,
    /// The matter changed stage after the current estimate was recorded.
    pub stale: bool,
    /// When the matter is expected to resolve (the current estimate's
    /// expected resolution date), or resolved (the Matter Outcome's date).
    pub resolves_on: Option<NaiveDate>,
}

impl ContingentProjection {
    /// The outcomes that can happen.
    fn possible(&self) -> impl Iterator<Item = &OutcomeProjection> + Clone {
        self.outcomes.iter().filter(|o| o.probability_bps > 0)
    }

    /// Σ p·(fee(o) + costs recovered(o)): the cash the matter is expected
    /// to bring in. `None` without recoveries.
    #[must_use]
    pub fn expected_cash_cents(&self) -> Option<i64> {
        let advanced = self.costs_advanced_cents;
        self.expected_fee_cents
            .zip(self.expected_costs_lost_cents)
            .map(|(fee, lost)| fee + advanced - lost)
    }

    /// The lowest and highest cash among outcomes that can happen; `None`
    /// without recoveries.
    #[must_use]
    pub fn cash_range_cents(&self) -> Option<(i64, i64)> {
        self.expected_cash_cents()?;
        let cash = self
            .possible()
            .map(|o| o.fee_cents.unwrap_or_default() + o.costs_recovered_cents.unwrap_or_default());
        cash.clone().min().zip(cash.max())
    }
}

/// An hourly or flat-fee matter's projection: money already certain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertainProjection {
    pub wip_cents: i64,
    pub outstanding_cents: i64,
}

/// A matter's projection, by fee kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MatterProjection {
    /// The matter has a Contingency Term.
    Contingent(ContingentProjection),
    /// Hourly or flat fee: WIP plus Outstanding Balance, no odds.
    Certain(CertainProjection),
}

impl MatterProjection {
    /// The figure a firm total sums: the expected net of a contingent
    /// matter, or WIP plus Outstanding Balance of any other. `None` for a
    /// contingent matter whose odds carry no recoveries yet.
    #[must_use]
    pub fn expected_value_cents(&self) -> Option<i64> {
        match self {
            Self::Contingent(projection) => projection.expected_net_cents,
            Self::Certain(projection) => Some(
                projection
                    .wip_cents
                    .saturating_add(projection.outstanding_cents),
            ),
        }
    }

    /// The span of outcomes a firm range sums: a contingent matter's net
    /// range, or the single certain figure.
    #[must_use]
    pub fn range_cents(&self) -> Option<(i64, i64)> {
        match self {
            Self::Contingent(projection) => projection.net_range_cents,
            Self::Certain(_) => self.expected_value_cents().map(|value| (value, value)),
        }
    }
}

impl From<&MatterFinancials> for CertainProjection {
    fn from(financials: &MatterFinancials) -> Self {
        Self {
            wip_cents: financials.wip_cents,
            outstanding_cents: financials.outstanding_cents,
        }
    }
}

/// The fee tier `outcome` resolves at.
fn fee_bps(inputs: &ContingentInputs, outcome: Outcome) -> i32 {
    let first = inputs.tiers.first().map_or(0, |tier| tier.fee_bps);
    match outcome {
        Outcome::Lose => 0,
        Outcome::WinAtTrial => inputs.tiers.last().map_or(0, |tier| tier.fee_bps),
        Outcome::Settle => inputs
            .current_stage_position
            .and_then(|current| {
                inputs
                    .tiers
                    .iter()
                    .rev()
                    .find(|tier| tier.stage_position <= current)
            })
            .map_or(first, |tier| tier.fee_bps),
    }
}

/// `amount × bps / 10000`, truncated toward zero, without overflow.
fn share(amount_cents: i64, bps: i32) -> i64 {
    i64::try_from(i128::from(amount_cents) * i128::from(bps) / i128::from(BASIS_POINTS))
        .unwrap_or(i64::MAX)
}

/// Σ p·value / 10000 over `(probability_bps, value)` pairs, divided once.
fn expected(pairs: impl Iterator<Item = (i32, i64)>) -> i64 {
    let total: i128 = pairs
        .map(|(bps, value)| i128::from(bps) * i128::from(value))
        .sum();
    i64::try_from(total / i128::from(BASIS_POINTS)).unwrap_or(i64::MAX)
}

/// One outcome's figures from its gross recovery.
fn from_recovery(
    inputs: &ContingentInputs,
    outcome: Outcome,
    probability_bps: i32,
    gross: i64,
) -> OutcomeProjection {
    let fee_bps = fee_bps(inputs, outcome);
    let base = match inputs.costs_deducted {
        CostsDeducted::AfterFee => gross,
        CostsDeducted::BeforeFee => (gross - inputs.costs_advanced_cents).max(0),
    };
    let fee = share(base, fee_bps);
    let recovered = if gross > 0 {
        inputs.costs_advanced_cents.min(gross)
    } else {
        0
    };
    OutcomeProjection {
        outcome,
        probability_bps,
        gross_recovery_cents: Some(gross),
        fee_bps,
        fee_cents: Some(fee),
        costs_recovered_cents: Some(recovered),
        net_cents: Some(fee + recovered - inputs.costs_advanced_cents - inputs.time_cost_cents),
    }
}

fn without_recovery(
    inputs: &ContingentInputs,
    outcome: Outcome,
    probability_bps: i32,
) -> OutcomeProjection {
    OutcomeProjection {
        outcome,
        probability_bps,
        gross_recovery_cents: None,
        fee_bps: fee_bps(inputs, outcome),
        fee_cents: None,
        costs_recovered_cents: None,
        net_cents: None,
    }
}

pub(crate) fn result_outcome(result: MatterResult) -> Outcome {
    match result {
        MatterResult::Won => Outcome::WinAtTrial,
        MatterResult::Settled => Outcome::Settle,
        MatterResult::Lost | MatterResult::Dismissed => Outcome::Lose,
    }
}

/// Project a contingent matter from its inputs. Pure, so the math is
/// testable without a database.
#[must_use]
pub fn project_contingent(inputs: &ContingentInputs) -> ContingentProjection {
    let (source, outcomes) = match inputs.odds {
        Odds::Unset => (None, Vec::new()),
        Odds::Lawyer(set) => (
            Some(OddsSource::Lawyer),
            Outcome::ALL
                .iter()
                .map(|&outcome| {
                    let odds = set.get(outcome);
                    from_recovery(
                        inputs,
                        outcome,
                        odds.probability_bps,
                        odds.gross_recovery_cents,
                    )
                })
                .collect(),
        ),
        Odds::StageDefault(defaults) => (
            Some(OddsSource::StageDefault),
            [
                (Outcome::WinAtTrial, defaults.win_at_trial_bps),
                (Outcome::Settle, defaults.settle_bps),
                (Outcome::Lose, defaults.lose_bps),
            ]
            .into_iter()
            .map(|(outcome, bps)| {
                if outcome == Outcome::Lose {
                    // Losing recovers nothing whoever sets the odds.
                    from_recovery(inputs, outcome, bps, 0)
                } else {
                    without_recovery(inputs, outcome, bps)
                }
            })
            .collect(),
        ),
        Odds::Actual {
            result,
            gross_recovery_cents,
            fee_received_cents,
            costs_recovered_cents,
        } => {
            let happened = result_outcome(result);
            (
                Some(OddsSource::Actual),
                Outcome::ALL
                    .iter()
                    .map(|&outcome| {
                        if outcome != happened {
                            return without_recovery(inputs, outcome, 0);
                        }
                        OutcomeProjection {
                            outcome,
                            probability_bps: BASIS_POINTS,
                            gross_recovery_cents: Some(gross_recovery_cents),
                            fee_bps: fee_bps(inputs, outcome),
                            fee_cents: Some(fee_received_cents),
                            costs_recovered_cents: Some(costs_recovered_cents),
                            net_cents: Some(
                                fee_received_cents + costs_recovered_cents
                                    - inputs.costs_advanced_cents
                                    - inputs.time_cost_cents,
                            ),
                        }
                    })
                    .collect(),
            )
        }
    };
    // An outcome with no chance contributes nothing, known or not.
    let possible = || outcomes.iter().filter(|o| o.probability_bps > 0);
    let known = !outcomes.is_empty() && possible().all(|o| o.net_cents.is_some());
    let sum = |value: fn(&OutcomeProjection) -> i64| {
        known.then(|| expected(possible().map(|o| (o.probability_bps, value(o)))))
    };
    let costs_advanced = inputs.costs_advanced_cents;
    let nets = possible().filter_map(|o| o.net_cents);
    let net_range_cents = if known {
        nets.clone().min().zip(nets.max())
    } else {
        None
    };
    ContingentProjection {
        source,
        expected_fee_cents: sum(|o| o.fee_cents.unwrap_or_default()),
        expected_costs_lost_cents: known.then(|| {
            expected(possible().map(|o| {
                (
                    o.probability_bps,
                    costs_advanced - o.costs_recovered_cents.unwrap_or_default(),
                )
            }))
        }),
        expected_net_cents: sum(|o| o.net_cents.unwrap_or_default()),
        net_range_cents,
        costs_at_risk_cents: costs_advanced,
        costs_advanced_cents: costs_advanced,
        time_cost_cents: inputs.time_cost_cents,
        estimate_recorded_at: None,
        stale: false,
        resolves_on: None,
        outcomes,
    }
}

/// The time part of a projection: every billable Time Entry on the matter
/// at its snapshotted rate, billed or not. The time was spent either way.
#[must_use]
pub fn time_cost_cents(entries: &[crate::time_entries::TimeEntry]) -> i64 {
    entries
        .iter()
        .filter(|entry| entry.billable)
        .map(|entry| {
            crate::time_entries::entry_amount_cents(entry.minutes, entry.rate_cents_per_hour)
        })
        .sum()
}

/// The costs the firm advanced: every confirmed Project Cost on the matter.
/// An estimated amount is not yet the firm's money and stays out.
#[must_use]
pub fn costs_advanced_cents(costs: &[crate::project_costs::ProjectCost]) -> i64 {
    costs
        .iter()
        .filter(|cost| cost.amount_status == "confirmed")
        .map(|cost| cost.amount_cents)
        .sum()
}

/// A matter's projection on `on`, the date its Contingency Term resolves
/// against. Reads only.
///
/// # Errors
///
/// [`MatterProjectionError::NotFirmSide`] for a reader who is not a
/// firm-side participant, or the failed underlying read.
pub async fn for_project(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    viewer_person_id: Uuid,
    project_id: Uuid,
    on: NaiveDate,
) -> Result<MatterProjection, MatterProjectionError> {
    if !crate::firm_side::is_firm_side(pg, viewer_person_id, project_id).await? {
        return Err(MatterProjectionError::NotFirmSide);
    }
    let Some(term) = crate::contingency_terms::resolve_unchecked(pg, project_id, on).await? else {
        let financials = crate::matter_financials::for_project(surreal, pg, project_id).await?;
        return Ok(MatterProjection::Certain((&financials).into()));
    };
    let current_stage = crate::case_stages::current_stage(pg, project_id).await?;
    let mut tiers = Vec::with_capacity(term.tiers.len());
    for tier in &term.tiers {
        tiers.push(PositionedTier {
            stage_position: crate::case_stages::definition(pg, tier.stage_definition_id)
                .await?
                .position,
            fee_bps: tier.fee_bps,
        });
    }
    let estimate = crate::outcome_estimates::current_unchecked(pg, project_id).await?;
    let outcome = crate::matter_outcomes::current_unchecked(pg, project_id).await?;
    let odds = match (&outcome, &estimate, &current_stage) {
        (Some(outcome), _, _) => Odds::Actual {
            result: outcome.result,
            gross_recovery_cents: outcome.gross_recovery_cents,
            fee_received_cents: outcome.fee_received_cents,
            costs_recovered_cents: outcome.costs_recovered_cents,
        },
        (None, Some(estimate), _) => Odds::Lawyer(estimate.outcomes),
        (None, None, Some(stage)) => stage.default_odds.map_or(Odds::Unset, Odds::StageDefault),
        (None, None, None) => Odds::Unset,
    };
    let entries = crate::time_entries::list_for_project(pg, project_id).await?;
    let costs = crate::project_costs::list_for_project(pg, project_id).await?;
    let mut projection = project_contingent(&ContingentInputs {
        costs_deducted: term.costs_deducted,
        tiers,
        current_stage_position: current_stage.as_ref().map(|stage| stage.position),
        odds,
        costs_advanced_cents: costs_advanced_cents(&costs),
        time_cost_cents: time_cost_cents(&entries),
    });
    projection.resolves_on = outcome
        .as_ref()
        .map(|outcome| outcome.resolved_on)
        .or_else(|| {
            estimate
                .as_ref()
                .map(|estimate| estimate.expected_resolution_on)
        });
    if let (Some(estimate), None) = (&estimate, &outcome) {
        projection.estimate_recorded_at = Some(estimate.recorded_at);
        let last_move = crate::case_stages::history(pg, project_id)
            .await?
            .last()
            .map(|event| event.occurred_at);
        projection.stale = last_move.is_some_and(|moved| moved > estimate.recorded_at);
    }
    Ok(MatterProjection::Contingent(projection))
}

#[cfg(test)]
mod tests {
    use super::{
        project_contingent, CertainProjection, ContingentInputs, MatterProjection, Odds,
        OddsSource, PositionedTier,
    };
    use crate::case_stages::StageDefaultOdds;
    use crate::contingency_terms::CostsDeducted;
    use crate::matter_outcomes::MatterResult;
    use crate::outcome_estimates::tests::worked_example;
    use crate::outcome_estimates::Outcome;

    const COSTS: i64 = 1_200_000;

    /// 33.3% from intake (position 1), 40% at trial (position 5); the
    /// matter is in discovery (position 3).
    fn inputs(odds: Odds) -> ContingentInputs {
        ContingentInputs {
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
        }
    }

    #[test]
    fn the_worked_example_expects_53970_in_fees_and_3000_in_lost_costs() {
        // Settle 60% at $150k for 33.3%, win 15% at $400k for 40%, lose
        // 25%, with $12k in costs advanced.
        let projection = project_contingent(&inputs(Odds::Lawyer(worked_example())));
        assert_eq!(projection.source, Some(OddsSource::Lawyer));
        assert_eq!(projection.expected_fee_cents, Some(5_397_000));
        assert_eq!(projection.expected_costs_lost_cents, Some(300_000));
        assert_eq!(projection.expected_net_cents, Some(5_097_000));
        let nets: Vec<_> = projection.outcomes.iter().map(|o| o.net_cents).collect();
        assert_eq!(nets, [Some(16_000_000), Some(4_995_000), Some(-1_200_000)]);
        assert_eq!(projection.net_range_cents, Some((-1_200_000, 16_000_000)));
        assert_eq!(projection.costs_at_risk_cents, COSTS);
        // Cash: 60% × (49,950 + 12,000) + 15% × (160,000 + 12,000).
        assert_eq!(projection.expected_cash_cents(), Some(6_297_000));
        assert_eq!(projection.cash_range_cents(), Some((0, 17_200_000)));
        let fees: Vec<_> = projection.outcomes.iter().map(|o| o.fee_bps).collect();
        assert_eq!(fees, [4_000, 3_330, 0]);
    }

    #[test]
    fn logged_time_comes_off_every_outcome_at_its_snapshot() {
        let mut with_time = inputs(Odds::Lawyer(worked_example()));
        with_time.time_cost_cents = 2_500_000;
        let projection = project_contingent(&with_time);
        assert_eq!(projection.expected_net_cents, Some(5_097_000 - 2_500_000));
        assert_eq!(
            projection.outcomes[2].net_cents,
            Some(-1_200_000 - 2_500_000)
        );
        assert_eq!(
            projection.expected_fee_cents,
            Some(5_397_000),
            "time is a cost, never a fee"
        );
    }

    #[test]
    fn costs_deducted_before_the_fee_shrink_its_base() {
        let mut before = inputs(Odds::Lawyer(worked_example()));
        before.costs_deducted = CostsDeducted::BeforeFee;
        let projection = project_contingent(&before);
        // Settle: (15,000,000 − 1,200,000) × 33.3%; win: (40,000,000 −
        // 1,200,000) × 40%.
        assert_eq!(projection.outcomes[1].fee_cents, Some(4_595_400));
        assert_eq!(projection.outcomes[0].fee_cents, Some(15_520_000));
    }

    #[test]
    fn settling_pays_the_tier_for_the_stage_the_matter_is_in() {
        let mut at_trial = inputs(Odds::Lawyer(worked_example()));
        at_trial.current_stage_position = Some(5);
        assert_eq!(project_contingent(&at_trial).outcomes[1].fee_bps, 4_000);
        let mut unstaged = inputs(Odds::Lawyer(worked_example()));
        unstaged.current_stage_position = None;
        assert_eq!(project_contingent(&unstaged).outcomes[1].fee_bps, 3_330);
        let mut early = inputs(Odds::Lawyer(worked_example()));
        early.current_stage_position = Some(0);
        assert_eq!(
            project_contingent(&early).outcomes[1].fee_bps,
            3_330,
            "before the first tier's stage, the first tier"
        );
    }

    #[test]
    fn a_stage_default_shows_odds_but_no_expected_fee() {
        let projection = project_contingent(&inputs(Odds::StageDefault(StageDefaultOdds {
            win_at_trial_bps: 1_000,
            settle_bps: 5_000,
            lose_bps: 4_000,
        })));
        assert_eq!(projection.source, Some(OddsSource::StageDefault));
        let odds: Vec<_> = projection
            .outcomes
            .iter()
            .map(|o| o.probability_bps)
            .collect();
        assert_eq!(odds, [1_000, 5_000, 4_000]);
        assert_eq!(projection.outcomes[0].fee_cents, None);
        assert_eq!(projection.outcomes[2].net_cents, Some(-COSTS));
        assert_eq!(projection.expected_fee_cents, None);
        assert_eq!(projection.expected_net_cents, None);
        assert_eq!(projection.net_range_cents, None);
        assert_eq!(projection.costs_at_risk_cents, COSTS);
        assert_eq!(projection.expected_cash_cents(), None);
        assert_eq!(projection.cash_range_cents(), None);
    }

    #[test]
    fn a_matter_outcome_replaces_the_odds_with_the_actual_figure() {
        let projection = project_contingent(&inputs(Odds::Actual {
            result: MatterResult::Settled,
            gross_recovery_cents: 15_000_000,
            fee_received_cents: 5_000_000,
            costs_recovered_cents: COSTS,
        }));
        assert_eq!(projection.source, Some(OddsSource::Actual));
        let settled = projection.outcomes[1];
        assert_eq!(settled.outcome, Outcome::Settle);
        assert_eq!(settled.probability_bps, 10_000);
        assert_eq!(projection.expected_net_cents, Some(5_000_000));
        assert_eq!(projection.expected_fee_cents, Some(5_000_000));
        assert_eq!(projection.expected_costs_lost_cents, Some(0));
        assert_eq!(projection.net_range_cents, Some((5_000_000, 5_000_000)));
        let dismissed = project_contingent(&inputs(Odds::Actual {
            result: MatterResult::Dismissed,
            gross_recovery_cents: 0,
            fee_received_cents: 0,
            costs_recovered_cents: 0,
        }));
        assert_eq!(dismissed.outcomes[2].probability_bps, 10_000);
        assert_eq!(dismissed.expected_net_cents, Some(-COSTS));
    }

    #[test]
    fn no_odds_projects_nothing_but_the_costs_at_risk() {
        let projection = project_contingent(&inputs(Odds::Unset));
        assert_eq!(projection.source, None);
        assert!(projection.outcomes.is_empty());
        assert_eq!(projection.expected_net_cents, None);
        assert_eq!(projection.costs_at_risk_cents, COSTS);
    }

    #[test]
    fn a_recovery_smaller_than_the_costs_recovers_only_itself() {
        let mut thin = worked_example();
        thin.settle.gross_recovery_cents = 500_000;
        let projection = project_contingent(&inputs(Odds::Lawyer(thin)));
        assert_eq!(projection.outcomes[1].costs_recovered_cents, Some(500_000));
    }

    #[test]
    fn hourly_and_flat_matters_add_up_with_contingent_ones() {
        let certain = MatterProjection::Certain(CertainProjection {
            wip_cents: 250_000,
            outstanding_cents: 100_000,
        });
        let contingent = MatterProjection::Contingent(project_contingent(&inputs(Odds::Lawyer(
            worked_example(),
        ))));
        let total: i64 = [&certain, &contingent]
            .iter()
            .filter_map(|projection| projection.expected_value_cents())
            .sum();
        assert_eq!(total, 350_000 + 5_097_000);
        assert_eq!(certain.range_cents(), Some((350_000, 350_000)));
    }

    // The reads behind the math need a live PostgreSQL.
    mod database {
        use super::super::{for_project, MatterProjection, MatterProjectionError, OddsSource};
        use crate::contingency_terms::tests::database::{fee_agreement, standard};
        use crate::firm_side::tests::matter;
        use crate::outcome_estimates::NewOutcomeEstimate;
        use crate::postgres_mirror::tests::{connect, fresh_schema};
        use chrono::NaiveDate;

        /// The lawyer's view of a matter that has a Contingency Term.
        async fn contingent(
            surreal: &crate::surreal::SurrealDb,
            pg: &crate::postgres::PostgresDb,
            m: &crate::firm_side::tests::Matter,
            on: NaiveDate,
        ) -> super::super::ContingentProjection {
            match for_project(surreal, pg, m.lawyer, m.project_id, on)
                .await
                .unwrap()
            {
                MatterProjection::Contingent(projection) => projection,
                MatterProjection::Certain(_) => panic!("a contingent matter"),
            }
        }

        #[tokio::test]
        async fn the_odds_fall_back_from_outcome_to_estimate_to_stage_default() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let surreal = crate::surreal::test_support::mem().await;
            let m = matter(&pg).await;
            let on = NaiveDate::from_ymd_opt(2026, 11, 6).unwrap();
            for outsider in [m.client, m.clerk, m.outside_lawyer] {
                assert!(matches!(
                    for_project(&surreal, &pg, outsider, m.project_id, on).await,
                    Err(MatterProjectionError::NotFirmSide)
                ));
            }
            // No term: an hourly matter, certain money only.
            assert!(matches!(
                for_project(&surreal, &pg, m.lawyer, m.project_id, on)
                    .await
                    .unwrap(),
                MatterProjection::Certain(_)
            ));

            let notation = fee_agreement(&pg, &m).await;
            crate::contingency_terms::record(
                &pg,
                m.project_id,
                m.lawyer,
                &standard(&m, notation, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()),
            )
            .await
            .unwrap();
            let unset = contingent(&surreal, &pg, &m, on).await;
            assert_eq!(unset.source, None, "no estimate, no stage, no odds");

            crate::case_stages::set_stage(&pg, m.project_id, m.lawyer, m.intake, None)
                .await
                .unwrap();
            crate::case_stages::set_default_odds(
                &pg,
                m.intake,
                Some(crate::case_stages::StageDefaultOdds {
                    win_at_trial_bps: 1_000,
                    settle_bps: 5_000,
                    lose_bps: 4_000,
                }),
            )
            .await
            .unwrap();
            let defaulted = contingent(&surreal, &pg, &m, on).await;
            assert_eq!(defaulted.source, Some(OddsSource::StageDefault));
            assert_eq!(defaulted.expected_net_cents, None);

            let estimate = crate::outcome_estimates::record(
                &pg,
                m.project_id,
                m.lawyer,
                &NewOutcomeEstimate {
                    expected_resolution_on: NaiveDate::from_ymd_opt(2027, 6, 30).unwrap(),
                    note: None,
                    outcomes: crate::outcome_estimates::tests::worked_example(),
                },
            )
            .await
            .unwrap();
            let estimated = contingent(&surreal, &pg, &m, on).await;
            assert_eq!(estimated.source, Some(OddsSource::Lawyer));
            assert_eq!(estimated.estimate_recorded_at, Some(estimate.recorded_at));
            assert!(!estimated.stale);
            assert_eq!(estimated.expected_fee_cents, Some(5_397_000));
            assert_eq!(estimated.resolves_on, NaiveDate::from_ymd_opt(2027, 6, 30));

            // The matter moves on after the estimate: it is now stale, and
            // settling pays the trial tier.
            crate::case_stages::set_stage(&pg, m.project_id, m.lawyer, m.trial, None)
                .await
                .unwrap();
            let moved = contingent(&surreal, &pg, &m, on).await;
            assert!(moved.stale);
            assert_eq!(moved.outcomes[1].fee_bps, 4_000);

            crate::matter_outcomes::record(
                &pg,
                m.project_id,
                m.lawyer,
                &crate::matter_outcomes::NewMatterOutcome {
                    result: crate::matter_outcomes::MatterResult::Won,
                    resolved_on: NaiveDate::from_ymd_opt(2027, 5, 1).unwrap(),
                    gross_recovery_cents: 40_000_000,
                    fee_received_cents: 16_000_000,
                    costs_recovered_cents: 0,
                    iolta_allocation_id: None,
                },
            )
            .await
            .unwrap();
            let actual = contingent(&surreal, &pg, &m, on).await;
            assert_eq!(actual.source, Some(OddsSource::Actual));
            assert_eq!(actual.expected_net_cents, Some(16_000_000));
            assert!(!actual.stale, "an actual figure is never stale");
            assert_eq!(actual.resolves_on, NaiveDate::from_ymd_opt(2027, 5, 1));
        }
    }
}
