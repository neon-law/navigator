//! Outcome Calibration: how well the Firm's Outcome Estimates predicted its
//! Matter Outcomes, for the Firm show page.
//!
//! Each resolved matter is scored on the odds in force [`LOOKBACK_DAYS`]
//! before it resolved: the newest Outcome Estimate recorded on or before
//! `resolved_on − LOOKBACK_DAYS`. A resolved matter with no estimate that
//! old is counted apart, never scored. A corrected Matter Outcome counts only
//! through its current link. `Lost` and `Dismissed` both score as losing.
//!
//! The report holds, in integer basis points and cents:
//!
//! - predicted against actual win and settle rates, with each matter placed
//!   in an odds band of [`BAND_WIDTH_BPS`] by the probability it was given;
//! - the mean three-outcome Brier score, Σ (p − y)², from 0 (perfect) to
//!   20000 (certain and wrong);
//! - the mean recovery error, expected gross recovery minus the actual one;
//! - the same score and error by lead lawyer and by the Case Stage the
//!   matter held when the scored estimate was recorded.
//!
//! Below [`MINIMUM_RESOLVED_MATTERS`] scored matters there is no report.
//!
//! **Privacy.** [`firm_calibration`] is the one place that builds labels,
//! and they are only a lead lawyer's display name, a Case Stage's name, or
//! `"Unassigned"`, the rule [`crate::projected_cash::firm_rollup`] follows.
//! No Project code, matter name, client name, Project id, or invoice id
//! reaches its output.
use std::collections::BTreeMap;

use chrono::{Days, NaiveDate};
use serde::Serialize;
use uuid::Uuid;

use crate::matter_outcomes::{MatterOutcomeError, MatterResult};
use crate::outcome_estimates::{Outcome, OutcomeEstimate, OutcomeEstimateError, OutcomeSet};
use crate::postgres::PostgresDb;
use crate::projects::Project;
use crate::surreal::SurrealDb;

/// How many days before resolution the scored odds were in force.
pub const LOOKBACK_DAYS: u64 = 30;

/// Fewer scored matters than this, and the panel shows no report.
pub const MINIMUM_RESOLVED_MATTERS: usize = 10;

/// The width of one odds band, in basis points: five bands from 0% to 100%.
pub const BAND_WIDTH_BPS: i32 = 2_000;

const WHOLE_BPS: i64 = 10_000;
const UNASSIGNED: &str = "Unassigned";

#[derive(Debug, thiserror::Error)]
pub enum CalibrationError {
    #[error(transparent)]
    Outcomes(#[from] MatterOutcomeError),
    #[error(transparent)]
    Estimates(#[from] OutcomeEstimateError),
    #[error(transparent)]
    Stages(Box<crate::case_stages::CaseStageError>),
    #[error(transparent)]
    Projects(#[from] crate::projects::ProjectStoreError),
}

impl From<crate::case_stages::CaseStageError> for CalibrationError {
    fn from(error: crate::case_stages::CaseStageError) -> Self {
        Self::Stages(Box::new(error))
    }
}

/// One outcome's predicted and actual rate in one band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct BandRate {
    /// Matters whose odds for this outcome fell in the band.
    pub matters: usize,
    /// Their mean predicted probability; `None` with no matters.
    pub predicted_bps: Option<i64>,
    /// The share of them that ended this way; `None` with no matters.
    pub actual_bps: Option<i64>,
}

/// One odds band, `[low_bps, high_bps)`; the last band includes 10000.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OddsBand {
    pub low_bps: i32,
    pub high_bps: i32,
    pub win_at_trial: BandRate,
    pub settle: BandRate,
}

/// One lead lawyer's or Case Stage's score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CalibrationGroup {
    /// A lead lawyer's display name, a Case Stage's name, or `"Unassigned"`.
    pub label: String,
    pub matters: usize,
    pub brier_bps: i64,
    pub mean_recovery_error_cents: i64,
}

/// The scores over every scored matter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CalibrationReport {
    pub brier_bps: i64,
    /// Mean of expected minus actual gross recovery: positive means the
    /// odds were optimistic.
    pub mean_recovery_error_cents: i64,
    pub mean_absolute_recovery_error_cents: i64,
    /// Every band, lowest first.
    pub bands: Vec<OddsBand>,
    /// Sorted by label. A matter with two lead lawyers counts under each.
    pub by_lead_lawyer: Vec<CalibrationGroup>,
    /// Sorted by label.
    pub by_case_stage: Vec<CalibrationGroup>,
}

/// The Firm show page's Outcome Calibration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutcomeCalibration {
    pub lookback_days: u64,
    pub minimum_matters: usize,
    /// Resolved matters scored on an estimate in force.
    pub scored_matters: usize,
    /// Resolved matters with no estimate [`LOOKBACK_DAYS`] before resolving.
    pub unscored_matters: usize,
    /// `None` below [`MINIMUM_RESOLVED_MATTERS`].
    pub report: Option<CalibrationReport>,
}

/// One resolved matter as calibration reads it: its labels, the odds in
/// force, and what happened. Nothing else about the matter.
#[derive(Debug, Clone)]
struct Scored {
    lead_lawyers: Vec<String>,
    case_stage: Option<String>,
    odds: OutcomeSet,
    result: MatterResult,
    gross_recovery_cents: i64,
}

/// The newest estimate recorded on or before `resolved_on − LOOKBACK_DAYS`.
/// `history` is oldest first.
fn in_force(history: &[OutcomeEstimate], resolved_on: NaiveDate) -> Option<&OutcomeEstimate> {
    let cutoff = resolved_on.checked_sub_days(Days::new(LOOKBACK_DAYS))?;
    history
        .iter()
        .rev()
        .find(|estimate| estimate.recorded_at.date_naive() <= cutoff)
}

/// `sum / n`, rounded half away from zero.
fn mean(sum: i128, n: usize) -> i64 {
    let n = i128::try_from(n.max(1)).unwrap_or(i128::MAX);
    let rounded = if sum >= 0 {
        (sum + n / 2) / n
    } else {
        (sum - n / 2) / n
    };
    i64::try_from(rounded).unwrap_or(if sum >= 0 { i64::MAX } else { i64::MIN })
}

impl Scored {
    fn happened(&self, outcome: Outcome) -> bool {
        crate::matter_projections::result_outcome(self.result) == outcome
    }

    /// Σ (p − y)² over the three outcomes, in bps² (divide by 10000 for bps).
    fn brier_bps_squared(&self) -> i128 {
        Outcome::ALL
            .iter()
            .map(|&outcome| {
                let p = i128::from(self.odds.get(outcome).probability_bps);
                let y = if self.happened(outcome) {
                    i128::from(WHOLE_BPS)
                } else {
                    0
                };
                (p - y) * (p - y)
            })
            .sum()
    }

    /// Expected gross recovery minus the actual one.
    fn recovery_error_cents(&self) -> i128 {
        let expected: i128 = Outcome::ALL
            .iter()
            .map(|&outcome| {
                let odds = self.odds.get(outcome);
                i128::from(odds.probability_bps) * i128::from(odds.gross_recovery_cents)
            })
            .sum::<i128>()
            / i128::from(WHOLE_BPS);
        expected - i128::from(self.gross_recovery_cents)
    }
}

/// Running sums for one group of scored matters.
#[derive(Default)]
struct Tally {
    matters: usize,
    brier: i128,
    error: i128,
    absolute_error: i128,
}

impl Tally {
    fn add(&mut self, matter: &Scored) {
        let error = matter.recovery_error_cents();
        self.matters += 1;
        self.brier += matter.brier_bps_squared();
        self.error += error;
        self.absolute_error += error.abs();
    }

    fn brier_bps(&self) -> i64 {
        mean(self.brier / i128::from(WHOLE_BPS), self.matters)
    }
}

fn into_groups(tallies: BTreeMap<String, Tally>) -> Vec<CalibrationGroup> {
    tallies
        .into_iter()
        .map(|(label, tally)| CalibrationGroup {
            label,
            matters: tally.matters,
            brier_bps: tally.brier_bps(),
            mean_recovery_error_cents: mean(tally.error, tally.matters),
        })
        .collect()
}

/// Predicted and actual sums for one outcome in one band.
#[derive(Default, Clone, Copy)]
struct RateTally {
    matters: usize,
    predicted: i128,
    happened: usize,
}

impl RateTally {
    fn rate(self) -> BandRate {
        let any = self.matters > 0;
        BandRate {
            matters: self.matters,
            predicted_bps: any.then(|| mean(self.predicted, self.matters)),
            actual_bps: any.then(|| {
                mean(
                    i128::try_from(self.happened).unwrap_or(0) * i128::from(WHOLE_BPS),
                    self.matters,
                )
            }),
        }
    }
}

const BANDS: usize = 5;

fn band_of(probability_bps: i32) -> usize {
    usize::try_from(probability_bps / BAND_WIDTH_BPS)
        .unwrap_or(0)
        .min(BANDS - 1)
}

/// The calibration over matters already read. Pure.
fn fold(matters: &[Scored], unscored_matters: usize) -> OutcomeCalibration {
    let mut calibration = OutcomeCalibration {
        lookback_days: LOOKBACK_DAYS,
        minimum_matters: MINIMUM_RESOLVED_MATTERS,
        scored_matters: matters.len(),
        unscored_matters,
        report: None,
    };
    if matters.len() < MINIMUM_RESOLVED_MATTERS {
        return calibration;
    }
    let mut firm = Tally::default();
    let (mut by_lead_lawyer, mut by_case_stage) = (BTreeMap::new(), BTreeMap::new());
    let mut bands = [[RateTally::default(); 2]; BANDS];
    for matter in matters {
        firm.add(matter);
        if matter.lead_lawyers.is_empty() {
            by_lead_lawyer
                .entry(UNASSIGNED.to_string())
                .or_insert_with(Tally::default)
                .add(matter);
        }
        for name in &matter.lead_lawyers {
            by_lead_lawyer
                .entry(name.clone())
                .or_insert_with(Tally::default)
                .add(matter);
        }
        by_case_stage
            .entry(
                matter
                    .case_stage
                    .clone()
                    .unwrap_or_else(|| UNASSIGNED.to_string()),
            )
            .or_insert_with(Tally::default)
            .add(matter);
        for (column, outcome) in [Outcome::WinAtTrial, Outcome::Settle]
            .into_iter()
            .enumerate()
        {
            let probability = matter.odds.get(outcome).probability_bps;
            let tally = &mut bands[band_of(probability)][column];
            tally.matters += 1;
            tally.predicted += i128::from(probability);
            tally.happened += usize::from(matter.happened(outcome));
        }
    }
    calibration.report = Some(CalibrationReport {
        brier_bps: firm.brier_bps(),
        mean_recovery_error_cents: mean(firm.error, firm.matters),
        mean_absolute_recovery_error_cents: mean(firm.absolute_error, firm.matters),
        bands: (0_i32..)
            .zip(bands)
            .map(|(index, [win, settle])| OddsBand {
                low_bps: index * BAND_WIDTH_BPS,
                high_bps: (index + 1) * BAND_WIDTH_BPS,
                win_at_trial: win.rate(),
                settle: settle.rate(),
            })
            .collect(),
        by_lead_lawyer: into_groups(by_lead_lawyer),
        by_case_stage: into_groups(by_case_stage),
    });
    calibration
}

/// The Case Stage label `project_id` held at `estimate`'s recording.
async fn stage_at(
    pg: &PostgresDb,
    project_id: Uuid,
    estimate: &OutcomeEstimate,
    labels: &mut BTreeMap<Uuid, String>,
) -> Result<Option<String>, CalibrationError> {
    let Some(event) = crate::case_stages::history(pg, project_id)
        .await?
        .into_iter()
        .rev()
        .find(|event| event.occurred_at <= estimate.recorded_at)
    else {
        return Ok(None);
    };
    if let Some(label) = labels.get(&event.to_stage_id) {
        return Ok(Some(label.clone()));
    }
    let label = crate::case_stages::definition(pg, event.to_stage_id)
        .await?
        .label;
    labels.insert(event.to_stage_id, label.clone());
    Ok(Some(label))
}

/// Score each `(project, lead lawyers)` that has a Matter Outcome, read as
/// `viewer`, and count the resolved ones with no estimate in force. A
/// matter the PostgreSQL guard does not count `viewer` firm-side on is left
/// out, never shown.
async fn score(
    pg: &PostgresDb,
    viewer: Uuid,
    matters: impl IntoIterator<Item = (Uuid, Vec<String>)>,
) -> Result<(Vec<Scored>, usize), CalibrationError> {
    let mut stage_labels = BTreeMap::new();
    let (mut scored, mut unscored) = (Vec::new(), 0);
    for (project_id, lead_lawyers) in matters {
        let outcome = match crate::matter_outcomes::current(pg, viewer, project_id).await {
            Ok(Some(outcome)) => outcome,
            Ok(None) | Err(MatterOutcomeError::NotFirmSide) => continue,
            Err(error) => return Err(error.into()),
        };
        let history = crate::outcome_estimates::history(pg, viewer, project_id).await?;
        let Some(estimate) = in_force(&history, outcome.resolved_on) else {
            unscored += 1;
            continue;
        };
        scored.push(Scored {
            lead_lawyers,
            case_stage: stage_at(pg, project_id, estimate, &mut stage_labels).await?,
            odds: estimate.outcomes,
            result: outcome.result,
            gross_recovery_cents: outcome.gross_recovery_cents,
        });
    }
    Ok((scored, unscored))
}

/// The Firm's Outcome Calibration, read as `viewer` (an Owner or Admin),
/// over every resolved matter the Firm owns.
///
/// # Errors
///
/// The failed Project, outcome, estimate, or stage read.
pub async fn firm_calibration(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    viewer: Uuid,
    firm_id: Uuid,
) -> Result<OutcomeCalibration, CalibrationError> {
    let mut lead_lawyers = crate::projects::dri_names_by_project(surreal, "is_lawyer_dri").await?;
    let matters: Vec<(Uuid, Vec<String>)> = crate::projects::all(surreal)
        .await?
        .into_iter()
        .filter(|project: &Project| project.firm_id == Some(firm_id))
        .map(|project| {
            let names = lead_lawyers.remove(&project.id).unwrap_or_default();
            (project.id, names)
        })
        .collect();
    let (scored, unscored) = score(pg, viewer, matters).await?;
    Ok(fold(&scored, unscored))
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, TimeZone, Utc};
    use uuid::Uuid;

    use super::{fold, in_force, mean, Scored, MINIMUM_RESOLVED_MATTERS};
    use crate::matter_outcomes::MatterResult;
    use crate::outcome_estimates::tests::worked_example;
    use crate::outcome_estimates::{OutcomeEstimate, OutcomeOdds, OutcomeSet};

    fn odds(win: i32, settle: i32) -> OutcomeSet {
        OutcomeSet {
            win_at_trial: OutcomeOdds {
                probability_bps: win,
                gross_recovery_cents: 40_000_000,
            },
            settle: OutcomeOdds {
                probability_bps: settle,
                gross_recovery_cents: 15_000_000,
            },
            lose: OutcomeOdds {
                probability_bps: 10_000 - win - settle,
                gross_recovery_cents: 0,
            },
        }
    }

    fn scored(
        lawyers: &[&str],
        stage: Option<&str>,
        odds: OutcomeSet,
        result: MatterResult,
        gross_recovery_cents: i64,
    ) -> Scored {
        Scored {
            lead_lawyers: lawyers.iter().map(ToString::to_string).collect(),
            case_stage: stage.map(str::to_string),
            odds,
            result,
            gross_recovery_cents,
        }
    }

    fn estimate(recorded: (i32, u32, u32)) -> OutcomeEstimate {
        OutcomeEstimate {
            id: Uuid::new_v4(),
            project_id: Uuid::nil(),
            author_person_id: Uuid::nil(),
            recorded_at: Utc
                .with_ymd_and_hms(recorded.0, recorded.1, recorded.2, 23, 0, 0)
                .unwrap(),
            expected_resolution_on: NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(),
            note: None,
            outcomes: worked_example(),
        }
    }

    #[test]
    fn the_odds_in_force_are_the_newest_estimate_thirty_days_before_resolution() {
        let history = [
            estimate((2026, 1, 1)),
            estimate((2026, 5, 2)),
            estimate((2026, 5, 3)),
        ];
        let resolved = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
        assert_eq!(
            in_force(&history, resolved).map(|e| e.id),
            Some(history[1].id),
            "recorded on the cutoff day counts; a day later does not"
        );
        let early = NaiveDate::from_ymd_opt(2026, 1, 30).unwrap();
        assert!(in_force(&history, early).is_none());
    }

    #[test]
    fn means_round_half_away_from_zero() {
        assert_eq!(mean(5, 2), 3);
        assert_eq!(mean(-5, 2), -3);
        assert_eq!(mean(4, 3), 1);
        assert_eq!(mean(0, 0), 0);
    }

    #[test]
    fn below_the_minimum_there_is_no_report() {
        let few: Vec<_> = (1..MINIMUM_RESOLVED_MATTERS)
            .map(|_| scored(&[], None, odds(5_000, 0), MatterResult::Won, 40_000_000))
            .collect();
        let calibration = fold(&few, 3);
        assert_eq!(calibration.scored_matters, MINIMUM_RESOLVED_MATTERS - 1);
        assert_eq!(calibration.unscored_matters, 3);
        assert!(calibration.report.is_none());
    }

    #[test]
    fn a_perfect_forecaster_scores_zero_and_a_coin_flip_is_banded() {
        let perfect: Vec<_> = (0..MINIMUM_RESOLVED_MATTERS)
            .map(|_| scored(&[], None, odds(10_000, 0), MatterResult::Won, 40_000_000))
            .collect();
        let report = fold(&perfect, 0).report.unwrap();
        assert_eq!(report.brier_bps, 0);
        assert_eq!(report.mean_recovery_error_cents, 0);
        let top = report.bands[4];
        assert_eq!((top.low_bps, top.high_bps), (8_000, 10_000));
        assert_eq!(top.win_at_trial.matters, MINIMUM_RESOLVED_MATTERS);
        assert_eq!(top.win_at_trial.actual_bps, Some(10_000));
        assert_eq!(report.bands[0].settle.predicted_bps, Some(0));
        assert_eq!(report.bands[2].win_at_trial.predicted_bps, None);

        // Half won, half lost, each at 50/0/50: Brier (0.25 + 0 + 0.25) = 0.5.
        let coin: Vec<_> = (0..MINIMUM_RESOLVED_MATTERS)
            .map(|i| {
                let won = i % 2 == 0;
                scored(
                    &[],
                    None,
                    odds(5_000, 0),
                    if won {
                        MatterResult::Won
                    } else {
                        MatterResult::Dismissed
                    },
                    if won { 40_000_000 } else { 0 },
                )
            })
            .collect();
        let report = fold(&coin, 0).report.unwrap();
        assert_eq!(report.brier_bps, 5_000);
        let middle = report.bands[2].win_at_trial;
        assert_eq!(
            (middle.matters, middle.predicted_bps, middle.actual_bps),
            (MINIMUM_RESOLVED_MATTERS, Some(5_000), Some(5_000))
        );
        // Expected $200,000 each; actual $400,000 or $0: no bias, $200,000 off.
        assert_eq!(report.mean_recovery_error_cents, 0);
        assert_eq!(report.mean_absolute_recovery_error_cents, 20_000_000);
    }

    #[test]
    fn scores_group_by_lead_lawyer_and_case_stage() {
        let mut matters = vec![
            scored(
                &["Lawyer One", "Lawyer Two"],
                Some("Discovery"),
                odds(0, 10_000),
                MatterResult::Settled,
                10_000_000,
            ),
            scored(&[], None, odds(0, 10_000), MatterResult::Lost, 0),
        ];
        matters.extend((2..MINIMUM_RESOLVED_MATTERS).map(|_| {
            scored(
                &["Lawyer One"],
                Some("Trial"),
                odds(10_000, 0),
                MatterResult::Won,
                40_000_000,
            )
        }));
        let report = fold(&matters, 0).report.unwrap();
        let lawyers: Vec<_> = report
            .by_lead_lawyer
            .iter()
            .map(|g| (g.label.as_str(), g.matters, g.brier_bps))
            .collect();
        assert_eq!(
            lawyers,
            [
                ("Lawyer One", MINIMUM_RESOLVED_MATTERS - 1, 0),
                ("Lawyer Two", 1, 0),
                // Certain to settle, and lost: (1 − 0)² + (0 − 1)² = 2.
                ("Unassigned", 1, 20_000),
            ]
        );
        let stages: Vec<_> = report
            .by_case_stage
            .iter()
            .map(|g| (g.label.as_str(), g.mean_recovery_error_cents))
            .collect();
        assert_eq!(
            stages,
            [
                ("Discovery", 5_000_000),
                ("Trial", 0),
                ("Unassigned", 15_000_000)
            ]
        );
        let settle = report.bands[4].settle;
        assert_eq!((settle.matters, settle.actual_bps), (2, Some(5_000)));
    }

    /// ENG-1251 privacy: whatever the matters behind it are called, the
    /// calibration carries only lead lawyer names, Case Stage names,
    /// "Unassigned", bands, and numbers. `Scored` has no field for a
    /// matter's code, name, client, or id; the serialized output proves the
    /// labels are the only strings.
    #[test]
    fn no_project_code_matter_name_client_or_invoice_id_reaches_the_calibration() {
        let matters: Vec<_> = (0..MINIMUM_RESOLVED_MATTERS)
            .map(|i| {
                scored(
                    if i == 0 { &["Lawyer One"] } else { &[] },
                    (i == 1).then_some("Discovery"),
                    odds(3_000, 3_000),
                    MatterResult::Settled,
                    15_000_000,
                )
            })
            .collect();
        let json = serde_json::to_string(&fold(&matters, 2)).unwrap();
        let strings: Vec<String> = json
            .split('"')
            .skip(1)
            .step_by(2)
            .filter(|s| !s.ends_with("_bps") && !s.ends_with("_cents") && !s.ends_with("_days"))
            .map(str::to_string)
            .collect();
        let structural = [
            "minimum_matters",
            "scored_matters",
            "unscored_matters",
            "report",
            "bands",
            "win_at_trial",
            "settle",
            "matters",
            "by_lead_lawyer",
            "by_case_stage",
            "label",
        ];
        for value in strings {
            assert!(
                structural.contains(&value.as_str())
                    || ["Lawyer One", "Discovery", "Unassigned"].contains(&value.as_str()),
                "unexpected string {value:?} in {json}"
            );
        }
    }

    // The reads behind the fold need a live PostgreSQL.
    mod database {
        use chrono::{Days, NaiveDate, Utc};

        use super::super::{score, LOOKBACK_DAYS};
        use crate::firm_side::tests::matter;
        use crate::matter_outcomes::{MatterResult, NewMatterOutcome};
        use crate::outcome_estimates::tests::worked_example;
        use crate::outcome_estimates::NewOutcomeEstimate;
        use crate::postgres_mirror::tests::{connect, fresh_schema};

        fn resolution(result: MatterResult, resolved_on: NaiveDate) -> NewMatterOutcome {
            NewMatterOutcome {
                result,
                resolved_on,
                gross_recovery_cents: 15_000_000,
                fee_received_cents: 5_000_000,
                costs_recovered_cents: 0,
                iolta_allocation_id: None,
            }
        }

        #[tokio::test]
        async fn a_resolved_matter_is_scored_on_odds_in_force_through_its_current_outcome() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let (m, open, early) = (matter(&pg).await, matter(&pg).await, matter(&pg).await);
            let today = Utc::now().date_naive();
            let in_force = today.checked_add_days(Days::new(LOOKBACK_DAYS)).unwrap();
            crate::case_stages::set_stage(&pg, m.project_id, m.lawyer, m.intake, None)
                .await
                .unwrap();
            for (each, lawyer) in [(&m, m.lawyer), (&early, early.lawyer)] {
                crate::outcome_estimates::record(
                    &pg,
                    each.project_id,
                    lawyer,
                    &NewOutcomeEstimate {
                        expected_resolution_on: in_force,
                        note: None,
                        outcomes: worked_example(),
                    },
                )
                .await
                .unwrap();
            }
            crate::matter_outcomes::record(
                &pg,
                m.project_id,
                m.lawyer,
                &resolution(MatterResult::Won, in_force),
            )
            .await
            .unwrap();
            crate::matter_outcomes::correct(
                &pg,
                m.project_id,
                m.lawyer,
                &resolution(MatterResult::Settled, in_force),
                "settled on the courthouse steps",
            )
            .await
            .unwrap();
            // Resolved today: the estimate recorded today was not in force
            // LOOKBACK_DAYS earlier, so the matter is counted, not scored.
            crate::matter_outcomes::record(
                &pg,
                early.project_id,
                early.lawyer,
                &resolution(MatterResult::Lost, today),
            )
            .await
            .unwrap();

            let matters =
                [&m, &open, &early].map(|each| (each.project_id, vec!["Lawyer One".to_string()]));
            let (scored, unscored) = score(&pg, m.admin, matters.clone()).await.unwrap();
            assert_eq!(unscored, 1, "resolved too soon after its estimate");
            assert_eq!(scored.len(), 1, "the open matter is not resolved");
            assert_eq!(scored[0].result, MatterResult::Settled, "the current link");
            assert_eq!(scored[0].case_stage.as_deref(), Some("Intake"));
            assert_eq!(scored[0].odds, worked_example());

            let (scored, unscored) = score(&pg, m.outside_lawyer, matters).await.unwrap();
            assert_eq!((scored.len(), unscored), (0, 0), "off the matters: nothing");
        }
    }
}
