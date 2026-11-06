//! A matter's Contingency Term: its percentage-of-recovery fee, stored
//! relationally in PostgreSQL.
//!
//! Like an [`AttorneyRate`](crate::attorney_rates::AttorneyRate), a term is
//! **dated and never edited**: a change is a new row with its own
//! `effective_on`, and the database rejects updates and deletes, so the table
//! is its own history. Resolution takes the latest row effective on or before
//! the date asked. Each row holds ordered tiers of (Case Stage, fee in basis
//! points), whether advanced costs come off before or after the percentage,
//! an optional hybrid hourly rate, and the signed fee-agreement Notation the
//! engagement letter requires — the generic retainer carries no amount.
//!
//! A term is firm work product: [`record`] and every read take the acting
//! Person and refuse anyone [`crate::firm_side::is_firm_side`] refuses.
//! Amounts are integer cents and percentages basis points, never floats. See
//! `docs/glossary/contingency-term.md`.
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::postgres::PostgresDb;

/// The whole, in basis points: 10000 is 100%.
pub const BASIS_POINTS: i32 = 10_000;

#[derive(Debug, thiserror::Error)]
pub enum ContingencyTermError {
    #[error("invalid Contingency Term: {0}")]
    Invalid(&'static str),
    /// The actor is not a firm-side participant on the matter. Callers answer
    /// with their non-disclosing refusal.
    #[error("only a firm-side participant may handle a Contingency Term")]
    NotFirmSide,
    #[error("the fee agreement is not a Notation on this matter")]
    FeeAgreementNotOnMatter,
    #[error("a tier's stage is not in the matter Firm's own vocabulary")]
    StageNotInFirmVocabulary,
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
}

/// Whether advanced Project Costs come off the gross recovery before the
/// percentage applies, or the percentage applies to the gross.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostsDeducted {
    BeforeFee,
    AfterFee,
}

impl CostsDeducted {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BeforeFee => "before_fee",
            Self::AfterFee => "after_fee",
        }
    }

    fn parse(raw: &str) -> Result<Self, ContingencyTermError> {
        match raw {
            "before_fee" => Ok(Self::BeforeFee),
            "after_fee" => Ok(Self::AfterFee),
            _ => Err(ContingencyTermError::Invalid("costs_deducted")),
        }
    }
}

/// The fee a resolution at or after `stage_definition_id` pays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContingencyTier {
    pub stage_definition_id: Uuid,
    pub fee_bps: i32,
}

/// One dated Contingency Term row with its tiers in authored order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContingencyTerm {
    pub id: Uuid,
    pub project_id: Uuid,
    pub effective_on: NaiveDate,
    pub tiers: Vec<ContingencyTier>,
    pub costs_deducted: CostsDeducted,
    pub rate_cents_per_hour: Option<i32>,
    pub fee_agreement_notation_id: Uuid,
    pub author_person_id: Uuid,
    pub inserted_at: DateTime<Utc>,
}

/// The inputs to [`record`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewContingencyTerm {
    pub effective_on: NaiveDate,
    pub tiers: Vec<ContingencyTier>,
    pub costs_deducted: CostsDeducted,
    #[serde(default)]
    pub rate_cents_per_hour: Option<i32>,
    pub fee_agreement_notation_id: Uuid,
}

impl NewContingencyTerm {
    fn validate(&self) -> Result<(), ContingencyTermError> {
        if self.tiers.is_empty() || self.tiers.len() > 20 {
            return Err(ContingencyTermError::Invalid("tiers"));
        }
        if self
            .tiers
            .iter()
            .any(|tier| !(0..=BASIS_POINTS).contains(&tier.fee_bps))
        {
            return Err(ContingencyTermError::Invalid("fee_bps"));
        }
        if self.rate_cents_per_hour.is_some_and(|rate| rate < 0) {
            return Err(ContingencyTermError::Invalid("rate_cents_per_hour"));
        }
        Ok(())
    }
}

/// Record a new dated Contingency Term on a matter. Never replaces a row: a
/// term recorded for the same date as an earlier one supersedes it by
/// insertion order. Tiers must name stages from the matter Firm's vocabulary
/// in that vocabulary's order, and the fee agreement must be a Notation on
/// this matter (its PostgreSQL mirror row; the caller mirrors it first).
///
/// # Errors
///
/// [`ContingencyTermError`] for invalid input, a non-firm-side author, a
/// foreign fee agreement or stage, or a database failure.
pub async fn record(
    pg: &PostgresDb,
    project_id: Uuid,
    author_person_id: Uuid,
    input: &NewContingencyTerm,
) -> Result<ContingencyTerm, ContingencyTermError> {
    input.validate()?;
    if !crate::firm_side::is_firm_side(pg, author_person_id, project_id).await? {
        return Err(ContingencyTermError::NotFirmSide);
    }
    let mut tx = pg.0.begin().await?;
    let on_matter: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM notation WHERE id=$1 AND project_id=$2)")
            .bind(input.fee_agreement_notation_id)
            .bind(project_id)
            .fetch_one(&mut *tx)
            .await?;
    if !on_matter {
        return Err(ContingencyTermError::FeeAgreementNotOnMatter);
    }
    let stage_ids: Vec<Uuid> = input
        .tiers
        .iter()
        .map(|tier| tier.stage_definition_id)
        .collect();
    // Each tier's stage position in the matter Firm's vocabulary, in tier
    // order; a stage from elsewhere has no row.
    let positions: Vec<Option<i32>> = sqlx::query_scalar(
        "SELECT d.position FROM unnest($1::uuid[]) WITH ORDINALITY AS t(stage_id, n) \
         LEFT JOIN project p ON p.id=$2 \
         LEFT JOIN stage_definition d ON d.id=t.stage_id AND d.firm_id=p.firm_id \
         ORDER BY t.n",
    )
    .bind(&stage_ids)
    .bind(project_id)
    .fetch_all(&mut *tx)
    .await?;
    let positions: Vec<i32> = positions
        .into_iter()
        .collect::<Option<_>>()
        .ok_or(ContingencyTermError::StageNotInFirmVocabulary)?;
    if positions.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(ContingencyTermError::Invalid(
            "tiers must follow the Firm's stage order",
        ));
    }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO contingency_term \
         (project_id,effective_on,costs_deducted,rate_cents_per_hour,fee_agreement_notation_id,author_person_id) \
         VALUES ($1,$2,$3,$4,$5,$6) RETURNING id",
    )
    .bind(project_id)
    .bind(input.effective_on)
    .bind(input.costs_deducted.as_str())
    .bind(input.rate_cents_per_hour)
    .bind(input.fee_agreement_notation_id)
    .bind(author_person_id)
    .fetch_one(&mut *tx)
    .await?;
    for (ordinal, tier) in (0_i16..).zip(&input.tiers) {
        sqlx::query(
            "INSERT INTO contingency_term_tier (contingency_term_id,ordinal,stage_definition_id,fee_bps) \
             VALUES ($1,$2,$3,$4)",
        )
        .bind(id)
        .bind(ordinal)
        .bind(tier.stage_definition_id)
        .bind(tier.fee_bps)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    let mut terms = load(pg, "t.id=$1", id, None).await?;
    terms.pop().ok_or(ContingencyTermError::Invalid("term"))
}

/// The term that governs `project_id` on `on`: the latest row effective on or
/// before that date, or `None` when the matter has no contingent fee yet.
///
/// # Errors
///
/// [`ContingencyTermError::NotFirmSide`] for any other reader, or a database
/// failure.
pub async fn resolve(
    pg: &PostgresDb,
    viewer_person_id: Uuid,
    project_id: Uuid,
    on: NaiveDate,
) -> Result<Option<ContingencyTerm>, ContingencyTermError> {
    ensure_firm_side(pg, viewer_person_id, project_id).await?;
    resolve_unchecked(pg, project_id, on).await
}

/// [`resolve`] after the caller has established the reader is firm-side.
pub(crate) async fn resolve_unchecked(
    pg: &PostgresDb,
    project_id: Uuid,
    on: NaiveDate,
) -> Result<Option<ContingencyTerm>, ContingencyTermError> {
    Ok(load(
        pg,
        "t.project_id=$1 AND t.effective_on<=$2",
        project_id,
        Some(on),
    )
    .await?
    .pop())
}

/// The matter's term history, oldest effective date first.
///
/// # Errors
///
/// [`ContingencyTermError::NotFirmSide`] for any other reader, or a database
/// failure.
pub async fn history(
    pg: &PostgresDb,
    viewer_person_id: Uuid,
    project_id: Uuid,
) -> Result<Vec<ContingencyTerm>, ContingencyTermError> {
    ensure_firm_side(pg, viewer_person_id, project_id).await?;
    load(pg, "t.project_id=$1", project_id, None).await
}

async fn ensure_firm_side(
    pg: &PostgresDb,
    person_id: Uuid,
    project_id: Uuid,
) -> Result<(), ContingencyTermError> {
    if crate::firm_side::is_firm_side(pg, person_id, project_id).await? {
        Ok(())
    } else {
        Err(ContingencyTermError::NotFirmSide)
    }
}

/// Terms matching `filter` (over alias `t`, binding `$1` and optionally
/// `$2`), oldest effective date first, each with its tiers in order.
async fn load(
    pg: &PostgresDb,
    filter: &'static str,
    key: Uuid,
    on: Option<NaiveDate>,
) -> Result<Vec<ContingencyTerm>, ContingencyTermError> {
    let sql = format!(
        "SELECT t.id,t.project_id,t.effective_on,t.costs_deducted,t.rate_cents_per_hour,\
         t.fee_agreement_notation_id,t.author_person_id,t.inserted_at,\
         COALESCE((SELECT array_agg(r.stage_definition_id ORDER BY r.ordinal) \
                   FROM contingency_term_tier r WHERE r.contingency_term_id=t.id),'{{}}') AS stages,\
         COALESCE((SELECT array_agg(r.fee_bps ORDER BY r.ordinal) \
                   FROM contingency_term_tier r WHERE r.contingency_term_id=t.id),'{{}}') AS fees \
         FROM contingency_term t WHERE {filter} \
         ORDER BY t.effective_on, t.inserted_at, t.id"
    );
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(key);
    if let Some(on) = on {
        query = query.bind(on);
    }
    query
        .fetch_all(&pg.0)
        .await?
        .iter()
        .map(|row| {
            let stages: Vec<Uuid> = row.try_get("stages")?;
            let fees: Vec<i32> = row.try_get("fees")?;
            let costs: String = row.try_get("costs_deducted")?;
            Ok(ContingencyTerm {
                id: row.try_get("id")?,
                project_id: row.try_get("project_id")?,
                effective_on: row.try_get("effective_on")?,
                tiers: stages
                    .into_iter()
                    .zip(fees)
                    .map(|(stage_definition_id, fee_bps)| ContingencyTier {
                        stage_definition_id,
                        fee_bps,
                    })
                    .collect(),
                costs_deducted: CostsDeducted::parse(&costs)?,
                rate_cents_per_hour: row.try_get("rate_cents_per_hour")?,
                fee_agreement_notation_id: row.try_get("fee_agreement_notation_id")?,
                author_person_id: row.try_get("author_person_id")?,
                inserted_at: row.try_get("inserted_at")?,
            })
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{ContingencyTermError, ContingencyTier, CostsDeducted, NewContingencyTerm};
    use chrono::NaiveDate;
    use uuid::Uuid;

    fn term(tiers: Vec<ContingencyTier>) -> NewContingencyTerm {
        NewContingencyTerm {
            effective_on: NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
            tiers,
            costs_deducted: CostsDeducted::AfterFee,
            rate_cents_per_hour: None,
            fee_agreement_notation_id: Uuid::new_v4(),
        }
    }

    fn tier(fee_bps: i32) -> ContingencyTier {
        ContingencyTier {
            stage_definition_id: Uuid::new_v4(),
            fee_bps,
        }
    }

    #[test]
    fn a_term_needs_tiers_within_the_whole() {
        assert!(term(vec![tier(3_330), tier(4_000)]).validate().is_ok());
        assert!(matches!(
            term(Vec::new()).validate(),
            Err(ContingencyTermError::Invalid("tiers"))
        ));
        for fee in [-1, 10_001] {
            assert!(matches!(
                term(vec![tier(fee)]).validate(),
                Err(ContingencyTermError::Invalid("fee_bps"))
            ));
        }
        let mut hybrid = term(vec![tier(2_500)]);
        hybrid.rate_cents_per_hour = Some(-1);
        assert!(matches!(
            hybrid.validate(),
            Err(ContingencyTermError::Invalid("rate_cents_per_hour"))
        ));
    }

    #[test]
    fn costs_deducted_round_trips_its_stored_spelling() {
        for costs in [CostsDeducted::BeforeFee, CostsDeducted::AfterFee] {
            assert_eq!(CostsDeducted::parse(costs.as_str()).unwrap(), costs);
        }
        assert!(CostsDeducted::parse("whenever").is_err());
    }

    // The remaining assertions need a live PostgreSQL.
    pub(crate) mod database {
        use super::super::{
            history, record, resolve, ContingencyTermError, ContingencyTier, CostsDeducted,
            NewContingencyTerm,
        };
        use crate::firm_side::tests::{matter, Matter};
        use crate::postgres::PostgresDb;
        use crate::postgres_mirror::tests::{connect, fresh_schema};
        use chrono::NaiveDate;
        use uuid::Uuid;

        /// A Notation on `project_id`, as its PostgreSQL mirror row.
        pub(crate) async fn fee_agreement(pg: &PostgresDb, m: &Matter) -> Uuid {
            let template: Uuid = sqlx::query_scalar(
                "INSERT INTO template (id,code,title,respondent_type,is_current,inserted_at,updated_at) \
                 VALUES ($1,'synthetic-fee-agreement','Synthetic Fee Agreement','person',false,now(),now()) \
                 RETURNING id",
            )
            .bind(Uuid::new_v4())
            .fetch_one(pg.pool())
            .await
            .unwrap();
            sqlx::query_scalar(
                "INSERT INTO notation (template_id,person_id,project_id,state) \
                 VALUES ($1,$2,$3,'executed') RETURNING id",
            )
            .bind(template)
            .bind(m.lawyer)
            .bind(m.project_id)
            .fetch_one(pg.pool())
            .await
            .unwrap()
        }

        pub(crate) fn standard(m: &Matter, notation: Uuid, on: NaiveDate) -> NewContingencyTerm {
            NewContingencyTerm {
                effective_on: on,
                tiers: vec![
                    ContingencyTier {
                        stage_definition_id: m.intake,
                        fee_bps: 3_330,
                    },
                    ContingencyTier {
                        stage_definition_id: m.trial,
                        fee_bps: 4_000,
                    },
                ],
                costs_deducted: CostsDeducted::AfterFee,
                rate_cents_per_hour: None,
                fee_agreement_notation_id: notation,
            }
        }

        fn day(month: u32, day: u32) -> NaiveDate {
            NaiveDate::from_ymd_opt(2026, month, day).unwrap()
        }

        #[tokio::test]
        async fn only_a_firm_side_participant_records_or_reads_a_term() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let m = matter(&pg).await;
            let notation = fee_agreement(&pg, &m).await;
            let input = standard(&m, notation, day(1, 1));
            for outsider in [m.client, m.clerk, m.outside_lawyer] {
                assert!(matches!(
                    record(&pg, m.project_id, outsider, &input).await,
                    Err(ContingencyTermError::NotFirmSide)
                ));
                assert!(matches!(
                    resolve(&pg, outsider, m.project_id, day(6, 1)).await,
                    Err(ContingencyTermError::NotFirmSide)
                ));
                assert!(matches!(
                    history(&pg, outsider, m.project_id).await,
                    Err(ContingencyTermError::NotFirmSide)
                ));
            }
            let recorded = record(&pg, m.project_id, m.lawyer, &input).await.unwrap();
            assert_eq!(recorded.tiers, input.tiers);
            assert_eq!(recorded.author_person_id, m.lawyer);
            assert_eq!(
                resolve(&pg, m.lawyer, m.project_id, day(6, 1))
                    .await
                    .unwrap(),
                Some(recorded)
            );
        }

        #[tokio::test]
        async fn the_latest_effective_term_governs_and_future_terms_wait() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let m = matter(&pg).await;
            let notation = fee_agreement(&pg, &m).await;
            assert_eq!(
                resolve(&pg, m.lawyer, m.project_id, day(6, 1))
                    .await
                    .unwrap(),
                None,
                "a matter with no term resolves to none"
            );
            let january = record(
                &pg,
                m.project_id,
                m.lawyer,
                &standard(&m, notation, day(1, 1)),
            )
            .await
            .unwrap();
            let mut march = standard(&m, notation, day(3, 1));
            march.tiers[0].fee_bps = 3_000;
            march.costs_deducted = CostsDeducted::BeforeFee;
            let march = record(&pg, m.project_id, m.lawyer, &march).await.unwrap();
            assert_eq!(
                resolve(&pg, m.lawyer, m.project_id, day(2, 15))
                    .await
                    .unwrap(),
                Some(january.clone()),
                "a later term never governs an earlier date"
            );
            assert_eq!(
                resolve(&pg, m.lawyer, m.project_id, day(3, 1))
                    .await
                    .unwrap(),
                Some(march.clone())
            );
            assert_eq!(
                resolve(&pg, m.lawyer, m.project_id, day(12, 31))
                    .await
                    .unwrap()
                    .unwrap()
                    .tiers[0]
                    .fee_bps,
                3_000
            );
            assert_eq!(
                history(&pg, m.lawyer, m.project_id).await.unwrap(),
                vec![january, march],
                "both rows remain as history"
            );
        }

        #[tokio::test]
        async fn the_database_refuses_edits_deletes_and_foreign_references() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let m = matter(&pg).await;
            let notation = fee_agreement(&pg, &m).await;
            let recorded = record(
                &pg,
                m.project_id,
                m.lawyer,
                &standard(&m, notation, day(1, 1)),
            )
            .await
            .unwrap();
            for statement in [
                "UPDATE contingency_term SET costs_deducted='before_fee' WHERE id=$1",
                "DELETE FROM contingency_term WHERE id=$1",
                "UPDATE contingency_term_tier SET fee_bps=1 WHERE contingency_term_id=$1",
                "DELETE FROM contingency_term_tier WHERE contingency_term_id=$1",
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

            // A stage from another Firm, refused by the store and the database.
            let other = matter(&pg).await;
            let mut foreign = standard(&m, notation, day(2, 1));
            foreign.tiers[1].stage_definition_id = other.trial;
            assert!(matches!(
                record(&pg, m.project_id, m.lawyer, &foreign).await,
                Err(ContingencyTermError::StageNotInFirmVocabulary)
            ));
            let raw = sqlx::query(
                "INSERT INTO contingency_term_tier (contingency_term_id,ordinal,stage_definition_id,fee_bps) \
                 VALUES ($1,9,$2,100)",
            )
            .bind(recorded.id)
            .bind(other.trial)
            .execute(pg.pool())
            .await;
            assert!(raw.is_err(), "the trigger refuses a foreign stage");

            // Tiers out of the Firm's stage order, and a fee agreement on
            // another matter.
            let mut reversed = standard(&m, notation, day(2, 1));
            reversed.tiers.reverse();
            assert!(matches!(
                record(&pg, m.project_id, m.lawyer, &reversed).await,
                Err(ContingencyTermError::Invalid(_))
            ));
            let elsewhere = fee_agreement(&pg, &other).await;
            assert!(matches!(
                record(
                    &pg,
                    m.project_id,
                    m.lawyer,
                    &standard(&m, elsewhere, day(2, 1))
                )
                .await,
                Err(ContingencyTermError::FeeAgreementNotOnMatter)
            ));
        }
    }
}
