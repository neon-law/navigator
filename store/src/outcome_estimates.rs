//! A lawyer's Outcome Estimate for a matter, stored relationally in
//! PostgreSQL.
//!
//! An estimate names exactly three [`Outcome`]s — win at trial, settle, and
//! lose — each with a probability in basis points and a gross recovery in
//! integer cents. The three probabilities sum to 10000 and losing recovers
//! nothing. Like `stage_event`, rows are only ever added to and the database
//! rejects updates and deletes: the newest row is the current estimate and
//! the older ones record how the lawyer's view moved.
//!
//! An estimate is firm work product: [`record`] and every read take the
//! acting Person and refuse anyone [`crate::firm_side::is_firm_side`]
//! refuses. See `docs/glossary/outcome-estimate.md`.
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::contingency_terms::BASIS_POINTS;
use crate::postgres::PostgresDb;

#[derive(Debug, thiserror::Error)]
pub enum OutcomeEstimateError {
    #[error("invalid Outcome Estimate: {0}")]
    Invalid(&'static str),
    /// The actor is not a firm-side participant on the matter. Callers answer
    /// with their non-disclosing refusal.
    #[error("only a firm-side participant may handle an Outcome Estimate")]
    NotFirmSide,
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
}

/// One of the three ways a contingent matter ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    WinAtTrial,
    Settle,
    Lose,
}

impl Outcome {
    /// Every outcome, in the order an estimate lists them.
    pub const ALL: [Outcome; 3] = [Outcome::WinAtTrial, Outcome::Settle, Outcome::Lose];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WinAtTrial => "win_at_trial",
            Self::Settle => "settle",
            Self::Lose => "lose",
        }
    }
}

/// One outcome's odds and what the client recovers if it happens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutcomeOdds {
    pub probability_bps: i32,
    pub gross_recovery_cents: i64,
}

/// The three outcomes together. The probabilities sum to 10000.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutcomeSet {
    pub win_at_trial: OutcomeOdds,
    pub settle: OutcomeOdds,
    pub lose: OutcomeOdds,
}

impl OutcomeSet {
    /// The odds of `outcome`.
    #[must_use]
    pub fn get(&self, outcome: Outcome) -> OutcomeOdds {
        match outcome {
            Outcome::WinAtTrial => self.win_at_trial,
            Outcome::Settle => self.settle,
            Outcome::Lose => self.lose,
        }
    }

    /// Check the set: each probability within the whole, the three summing
    /// to it, recoveries never negative, and losing recovering nothing.
    ///
    /// # Errors
    ///
    /// [`OutcomeEstimateError::Invalid`] naming the first failed rule.
    pub fn validate(&self) -> Result<(), OutcomeEstimateError> {
        let odds = Outcome::ALL.map(|outcome| self.get(outcome));
        if odds
            .iter()
            .any(|o| !(0..=BASIS_POINTS).contains(&o.probability_bps))
        {
            return Err(OutcomeEstimateError::Invalid("probability_bps"));
        }
        if odds.iter().map(|o| o.probability_bps).sum::<i32>() != BASIS_POINTS {
            return Err(OutcomeEstimateError::Invalid(
                "the three probabilities must sum to 10000",
            ));
        }
        if odds.iter().any(|o| o.gross_recovery_cents < 0) {
            return Err(OutcomeEstimateError::Invalid("gross_recovery_cents"));
        }
        if self.lose.gross_recovery_cents != 0 {
            return Err(OutcomeEstimateError::Invalid("lose recovers nothing"));
        }
        Ok(())
    }
}

/// One recorded estimate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeEstimate {
    pub id: Uuid,
    pub project_id: Uuid,
    pub author_person_id: Uuid,
    pub recorded_at: DateTime<Utc>,
    pub expected_resolution_on: NaiveDate,
    pub note: Option<String>,
    pub outcomes: OutcomeSet,
}

/// The inputs to [`record`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewOutcomeEstimate {
    pub expected_resolution_on: NaiveDate,
    #[serde(default)]
    pub note: Option<String>,
    pub outcomes: OutcomeSet,
}

impl NewOutcomeEstimate {
    fn validate(&self) -> Result<(), OutcomeEstimateError> {
        if self
            .note
            .as_deref()
            .is_some_and(|note| note.trim().is_empty() || note.trim().len() > 2_000)
        {
            return Err(OutcomeEstimateError::Invalid("note"));
        }
        self.outcomes.validate()
    }
}

/// Record a lawyer's new estimate. It becomes the matter's current estimate;
/// earlier rows stay as history.
///
/// # Errors
///
/// [`OutcomeEstimateError`] for an invalid set, a non-firm-side author, or a
/// database failure.
pub async fn record(
    pg: &PostgresDb,
    project_id: Uuid,
    author_person_id: Uuid,
    input: &NewOutcomeEstimate,
) -> Result<OutcomeEstimate, OutcomeEstimateError> {
    input.validate()?;
    ensure_firm_side(pg, author_person_id, project_id).await?;
    let o = &input.outcomes;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO outcome_estimate (project_id,author_person_id,expected_resolution_on,note,\
         win_at_trial_probability_bps,win_at_trial_gross_recovery_cents,\
         settle_probability_bps,settle_gross_recovery_cents,\
         lose_probability_bps,lose_gross_recovery_cents) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING id",
    )
    .bind(project_id)
    .bind(author_person_id)
    .bind(input.expected_resolution_on)
    .bind(input.note.as_deref().map(str::trim))
    .bind(o.win_at_trial.probability_bps)
    .bind(o.win_at_trial.gross_recovery_cents)
    .bind(o.settle.probability_bps)
    .bind(o.settle.gross_recovery_cents)
    .bind(o.lose.probability_bps)
    .bind(o.lose.gross_recovery_cents)
    .fetch_one(&pg.0)
    .await?;
    load(pg, "id=$1", id)
        .await?
        .pop()
        .ok_or(OutcomeEstimateError::Invalid("estimate"))
}

/// The matter's current estimate: its newest row, or `None` before any.
///
/// # Errors
///
/// [`OutcomeEstimateError::NotFirmSide`] for any other reader, or a database
/// failure.
pub async fn current(
    pg: &PostgresDb,
    viewer_person_id: Uuid,
    project_id: Uuid,
) -> Result<Option<OutcomeEstimate>, OutcomeEstimateError> {
    ensure_firm_side(pg, viewer_person_id, project_id).await?;
    current_unchecked(pg, project_id).await
}

/// [`current`] after the caller has established the reader is firm-side.
pub(crate) async fn current_unchecked(
    pg: &PostgresDb,
    project_id: Uuid,
) -> Result<Option<OutcomeEstimate>, OutcomeEstimateError> {
    Ok(load(pg, "project_id=$1", project_id).await?.pop())
}

/// Every estimate on the matter, oldest first.
///
/// # Errors
///
/// [`OutcomeEstimateError::NotFirmSide`] for any other reader, or a database
/// failure.
pub async fn history(
    pg: &PostgresDb,
    viewer_person_id: Uuid,
    project_id: Uuid,
) -> Result<Vec<OutcomeEstimate>, OutcomeEstimateError> {
    ensure_firm_side(pg, viewer_person_id, project_id).await?;
    load(pg, "project_id=$1", project_id).await
}

async fn ensure_firm_side(
    pg: &PostgresDb,
    person_id: Uuid,
    project_id: Uuid,
) -> Result<(), OutcomeEstimateError> {
    if crate::firm_side::is_firm_side(pg, person_id, project_id).await? {
        Ok(())
    } else {
        Err(OutcomeEstimateError::NotFirmSide)
    }
}

async fn load(
    pg: &PostgresDb,
    filter: &'static str,
    key: Uuid,
) -> Result<Vec<OutcomeEstimate>, OutcomeEstimateError> {
    let sql = format!(
        "SELECT id,project_id,author_person_id,recorded_at,expected_resolution_on,note,\
         win_at_trial_probability_bps,win_at_trial_gross_recovery_cents,\
         settle_probability_bps,settle_gross_recovery_cents,\
         lose_probability_bps,lose_gross_recovery_cents \
         FROM outcome_estimate WHERE {filter} ORDER BY recorded_at, id"
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(key)
        .fetch_all(&pg.0)
        .await?
        .iter()
        .map(|row| {
            let odds = |outcome: Outcome| -> Result<OutcomeOdds, sqlx::Error> {
                Ok(OutcomeOdds {
                    probability_bps: row
                        .try_get(format!("{}_probability_bps", outcome.as_str()).as_str())?,
                    gross_recovery_cents: row
                        .try_get(format!("{}_gross_recovery_cents", outcome.as_str()).as_str())?,
                })
            };
            Ok(OutcomeEstimate {
                id: row.try_get("id")?,
                project_id: row.try_get("project_id")?,
                author_person_id: row.try_get("author_person_id")?,
                recorded_at: row.try_get("recorded_at")?,
                expected_resolution_on: row.try_get("expected_resolution_on")?,
                note: row.try_get("note")?,
                outcomes: OutcomeSet {
                    win_at_trial: odds(Outcome::WinAtTrial)?,
                    settle: odds(Outcome::Settle)?,
                    lose: odds(Outcome::Lose)?,
                },
            })
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{OutcomeEstimateError, OutcomeOdds, OutcomeSet};

    /// The worked example: win 15% at $400k, settle 60% at $150k, lose 25%.
    pub(crate) fn worked_example() -> OutcomeSet {
        OutcomeSet {
            win_at_trial: OutcomeOdds {
                probability_bps: 1_500,
                gross_recovery_cents: 40_000_000,
            },
            settle: OutcomeOdds {
                probability_bps: 6_000,
                gross_recovery_cents: 15_000_000,
            },
            lose: OutcomeOdds {
                probability_bps: 2_500,
                gross_recovery_cents: 0,
            },
        }
    }

    #[test]
    fn the_three_probabilities_sum_to_the_whole() {
        assert!(worked_example().validate().is_ok());
        let mut short = worked_example();
        short.lose.probability_bps = 2_400;
        assert!(matches!(
            short.validate(),
            Err(OutcomeEstimateError::Invalid(
                "the three probabilities must sum to 10000"
            ))
        ));
        let mut negative = worked_example();
        negative.settle.probability_bps = -1;
        negative.lose.probability_bps = 2_501;
        assert!(matches!(
            negative.validate(),
            Err(OutcomeEstimateError::Invalid("probability_bps"))
        ));
    }

    #[test]
    fn recoveries_are_never_negative_and_losing_recovers_nothing() {
        let mut negative = worked_example();
        negative.settle.gross_recovery_cents = -1;
        assert!(matches!(
            negative.validate(),
            Err(OutcomeEstimateError::Invalid("gross_recovery_cents"))
        ));
        let mut lose_pays = worked_example();
        lose_pays.lose.gross_recovery_cents = 100;
        assert!(matches!(
            lose_pays.validate(),
            Err(OutcomeEstimateError::Invalid("lose recovers nothing"))
        ));
    }

    // The remaining assertions need a live PostgreSQL.
    mod database {
        use super::super::{current, history, record, NewOutcomeEstimate, OutcomeEstimateError};
        use super::worked_example;
        use crate::firm_side::tests::matter;
        use crate::postgres_mirror::tests::{connect, fresh_schema};
        use chrono::NaiveDate;

        fn estimate(note: &str) -> NewOutcomeEstimate {
            NewOutcomeEstimate {
                expected_resolution_on: NaiveDate::from_ymd_opt(2027, 6, 30).unwrap(),
                note: Some(note.into()),
                outcomes: worked_example(),
            }
        }

        #[tokio::test]
        async fn only_a_firm_side_participant_records_or_reads_an_estimate() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let m = matter(&pg).await;
            for outsider in [m.client, m.clerk, m.outside_lawyer] {
                assert!(matches!(
                    record(&pg, m.project_id, outsider, &estimate("Mediation set")).await,
                    Err(OutcomeEstimateError::NotFirmSide)
                ));
                assert!(matches!(
                    current(&pg, outsider, m.project_id).await,
                    Err(OutcomeEstimateError::NotFirmSide)
                ));
                assert!(matches!(
                    history(&pg, outsider, m.project_id).await,
                    Err(OutcomeEstimateError::NotFirmSide)
                ));
            }
            assert!(history(&pg, m.lawyer, m.project_id)
                .await
                .unwrap()
                .is_empty());
        }

        #[tokio::test]
        async fn the_newest_estimate_wins_and_the_rest_stay_as_history() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let m = matter(&pg).await;
            assert_eq!(current(&pg, m.lawyer, m.project_id).await.unwrap(), None);
            let first = record(&pg, m.project_id, m.lawyer, &estimate("After the answer"))
                .await
                .unwrap();
            let mut later = estimate("After the deposition");
            later.outcomes.settle.probability_bps = 5_000;
            later.outcomes.lose.probability_bps = 3_500;
            let second = record(&pg, m.project_id, m.admin, &later).await.unwrap();
            assert_eq!(
                current(&pg, m.lawyer, m.project_id).await.unwrap(),
                Some(second.clone())
            );
            assert_eq!(
                history(&pg, m.lawyer, m.project_id).await.unwrap(),
                vec![first, second]
            );
        }

        #[tokio::test]
        async fn the_database_holds_the_sum_and_refuses_edits() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let m = matter(&pg).await;
            let recorded = record(&pg, m.project_id, m.lawyer, &estimate("Initial"))
                .await
                .unwrap();
            for statement in [
                "UPDATE outcome_estimate SET note='Rewritten' WHERE id=$1",
                "DELETE FROM outcome_estimate WHERE id=$1",
            ] {
                assert!(
                    sqlx::query(statement)
                        .bind(recorded.id)
                        .execute(pg.pool())
                        .await
                        .is_err(),
                    "{statement}"
                );
            }
            let off_sum = sqlx::query(
                "INSERT INTO outcome_estimate (project_id,author_person_id,expected_resolution_on,\
                 win_at_trial_probability_bps,win_at_trial_gross_recovery_cents,\
                 settle_probability_bps,settle_gross_recovery_cents,lose_probability_bps) \
                 VALUES ($1,$2,'2027-01-01',5000,1,5000,1,1)",
            )
            .bind(m.project_id)
            .bind(m.lawyer)
            .execute(pg.pool())
            .await;
            assert!(off_sum.is_err(), "the database checks the sum too");
        }
    }
}
