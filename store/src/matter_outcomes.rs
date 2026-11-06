//! A Matter Outcome: what actually happened when a matter resolved, stored
//! relationally in PostgreSQL.
//!
//! A matter has one outcome. [`record`] writes it once; [`correct`] adds a
//! row that supersedes the current one with a reason, never an edit, so the
//! chain of rows is the audit trail and its newest link is the outcome. The
//! database rejects updates, deletes, a second original, and a correction
//! of another matter's outcome. Once an outcome exists its actual figures
//! supersede any estimate, and calibration compares them against the
//! matter's Outcome Estimates.
//!
//! A Matter Outcome is firm work product: every read and write takes the
//! acting Person and refuses anyone [`crate::firm_side::is_firm_side`]
//! refuses. Amounts are integer cents. See `docs/glossary/matter-outcome.md`.
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::postgres::PostgresDb;

#[derive(Debug, thiserror::Error)]
pub enum MatterOutcomeError {
    #[error("invalid Matter Outcome: {0}")]
    Invalid(&'static str),
    /// The actor is not a firm-side participant on the matter. Callers answer
    /// with their non-disclosing refusal.
    #[error("only a firm-side participant may handle a Matter Outcome")]
    NotFirmSide,
    /// The matter already has an outcome; change it with [`correct`].
    #[error("the matter already has an outcome; record a correction")]
    AlreadyRecorded,
    /// There is no outcome to correct yet.
    #[error("the matter has no outcome to correct")]
    NotRecorded,
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
}

/// How the matter resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatterResult {
    Won,
    Settled,
    Lost,
    Dismissed,
}

impl MatterResult {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Won => "won",
            Self::Settled => "settled",
            Self::Lost => "lost",
            Self::Dismissed => "dismissed",
        }
    }

    fn parse(raw: &str) -> Result<Self, MatterOutcomeError> {
        match raw {
            "won" => Ok(Self::Won),
            "settled" => Ok(Self::Settled),
            "lost" => Ok(Self::Lost),
            "dismissed" => Ok(Self::Dismissed),
            _ => Err(MatterOutcomeError::Invalid("result")),
        }
    }
}

/// One row of a matter's outcome chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatterOutcome {
    pub id: Uuid,
    pub project_id: Uuid,
    pub result: MatterResult,
    pub resolved_on: NaiveDate,
    pub gross_recovery_cents: i64,
    pub fee_received_cents: i64,
    pub costs_recovered_cents: i64,
    pub iolta_allocation_id: Option<String>,
    pub author_person_id: Uuid,
    /// The row this one corrects; `None` on the original.
    pub supersedes_id: Option<Uuid>,
    pub correction_reason: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

/// The figures a [`record`] or [`correct`] writes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewMatterOutcome {
    pub result: MatterResult,
    pub resolved_on: NaiveDate,
    pub gross_recovery_cents: i64,
    pub fee_received_cents: i64,
    pub costs_recovered_cents: i64,
    #[serde(default)]
    pub iolta_allocation_id: Option<String>,
}

impl NewMatterOutcome {
    fn validate(&self) -> Result<(), MatterOutcomeError> {
        if self.gross_recovery_cents < 0
            || self.fee_received_cents < 0
            || self.costs_recovered_cents < 0
        {
            return Err(MatterOutcomeError::Invalid("amounts are never negative"));
        }
        if self
            .iolta_allocation_id
            .as_deref()
            .is_some_and(|id| id.trim().is_empty() || id.trim().len() > 200)
        {
            return Err(MatterOutcomeError::Invalid("iolta_allocation_id"));
        }
        Ok(())
    }
}

/// Record the matter's outcome. Refused once one exists: a change is a
/// [`correct`]ion.
///
/// # Errors
///
/// [`MatterOutcomeError`] for invalid figures, a non-firm-side author, an
/// existing outcome, or a database failure.
pub async fn record(
    pg: &PostgresDb,
    project_id: Uuid,
    author_person_id: Uuid,
    input: &NewMatterOutcome,
) -> Result<MatterOutcome, MatterOutcomeError> {
    input.validate()?;
    ensure_firm_side(pg, author_person_id, project_id).await?;
    let mut tx = pg.0.begin().await?;
    lock_matter(&mut tx, project_id).await?;
    if current_id(&mut tx, project_id).await?.is_some() {
        return Err(MatterOutcomeError::AlreadyRecorded);
    }
    let id = insert(&mut tx, project_id, author_person_id, input, None).await?;
    tx.commit().await?;
    by_id(pg, id).await
}

/// Correct the matter's outcome: a new row superseding the current one, with
/// the reason. The superseded row stays as the audit trail.
///
/// # Errors
///
/// [`MatterOutcomeError`] for invalid figures or reason, a non-firm-side
/// author, no outcome to correct, or a database failure.
pub async fn correct(
    pg: &PostgresDb,
    project_id: Uuid,
    author_person_id: Uuid,
    input: &NewMatterOutcome,
    reason: &str,
) -> Result<MatterOutcome, MatterOutcomeError> {
    input.validate()?;
    let reason = reason.trim();
    if reason.is_empty() || reason.len() > 500 {
        return Err(MatterOutcomeError::Invalid("correction_reason"));
    }
    ensure_firm_side(pg, author_person_id, project_id).await?;
    let mut tx = pg.0.begin().await?;
    lock_matter(&mut tx, project_id).await?;
    let Some(current) = current_id(&mut tx, project_id).await? else {
        return Err(MatterOutcomeError::NotRecorded);
    };
    let id = insert(
        &mut tx,
        project_id,
        author_person_id,
        input,
        Some((current, reason)),
    )
    .await?;
    tx.commit().await?;
    by_id(pg, id).await
}

/// The matter's outcome: the newest link of its chain, or `None` while the
/// matter is unresolved.
///
/// # Errors
///
/// [`MatterOutcomeError::NotFirmSide`] for any other reader, or a database
/// failure.
pub async fn current(
    pg: &PostgresDb,
    viewer_person_id: Uuid,
    project_id: Uuid,
) -> Result<Option<MatterOutcome>, MatterOutcomeError> {
    ensure_firm_side(pg, viewer_person_id, project_id).await?;
    current_unchecked(pg, project_id).await
}

/// [`current`] after the caller has established the reader is firm-side.
pub(crate) async fn current_unchecked(
    pg: &PostgresDb,
    project_id: Uuid,
) -> Result<Option<MatterOutcome>, MatterOutcomeError> {
    let mut conn = pg.0.acquire().await?;
    let Some(id) = current_id(&mut conn, project_id).await? else {
        return Ok(None);
    };
    by_id(pg, id).await.map(Some)
}

/// The matter's whole outcome chain, original first.
///
/// # Errors
///
/// [`MatterOutcomeError::NotFirmSide`] for any other reader, or a database
/// failure.
pub async fn history(
    pg: &PostgresDb,
    viewer_person_id: Uuid,
    project_id: Uuid,
) -> Result<Vec<MatterOutcome>, MatterOutcomeError> {
    ensure_firm_side(pg, viewer_person_id, project_id).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM matter_outcome WHERE project_id=$1 ORDER BY recorded_at, id"
    )))
    .bind(project_id)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(from_row)
    .collect()
}

const COLUMNS: &str = "id,project_id,result,resolved_on,gross_recovery_cents,fee_received_cents,\
    costs_recovered_cents,iolta_allocation_id,author_person_id,supersedes_id,correction_reason,\
    recorded_at";

async fn ensure_firm_side(
    pg: &PostgresDb,
    person_id: Uuid,
    project_id: Uuid,
) -> Result<(), MatterOutcomeError> {
    if crate::firm_side::is_firm_side(pg, person_id, project_id).await? {
        Ok(())
    } else {
        Err(MatterOutcomeError::NotFirmSide)
    }
}

/// Serialize outcome writes on one matter, so two writers cannot both read
/// the same current link.
async fn lock_matter(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('matter_outcome:' || $1::text))")
        .bind(project_id)
        .execute(&mut **tx)
        .await
        .map(|_| ())
}

/// The chain's newest link: the matter's row that nothing supersedes.
async fn current_id(
    conn: &mut sqlx::PgConnection,
    project_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT o.id FROM matter_outcome o WHERE o.project_id=$1 \
         AND NOT EXISTS (SELECT 1 FROM matter_outcome n WHERE n.supersedes_id=o.id)",
    )
    .bind(project_id)
    .fetch_optional(conn)
    .await
}

async fn insert(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_id: Uuid,
    author_person_id: Uuid,
    input: &NewMatterOutcome,
    supersedes: Option<(Uuid, &str)>,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO matter_outcome (project_id,result,resolved_on,gross_recovery_cents,\
         fee_received_cents,costs_recovered_cents,iolta_allocation_id,author_person_id,\
         supersedes_id,correction_reason) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING id",
    )
    .bind(project_id)
    .bind(input.result.as_str())
    .bind(input.resolved_on)
    .bind(input.gross_recovery_cents)
    .bind(input.fee_received_cents)
    .bind(input.costs_recovered_cents)
    .bind(input.iolta_allocation_id.as_deref().map(str::trim))
    .bind(author_person_id)
    .bind(supersedes.map(|(id, _)| id))
    .bind(supersedes.map(|(_, reason)| reason))
    .fetch_one(&mut **tx)
    .await
}

async fn by_id(pg: &PostgresDb, id: Uuid) -> Result<MatterOutcome, MatterOutcomeError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM matter_outcome WHERE id=$1"
    )))
    .bind(id)
    .fetch_one(&pg.0)
    .await?;
    from_row(&row)
}

fn from_row(row: &sqlx::postgres::PgRow) -> Result<MatterOutcome, MatterOutcomeError> {
    let result: String = row.try_get("result")?;
    Ok(MatterOutcome {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        result: MatterResult::parse(&result)?,
        resolved_on: row.try_get("resolved_on")?,
        gross_recovery_cents: row.try_get("gross_recovery_cents")?,
        fee_received_cents: row.try_get("fee_received_cents")?,
        costs_recovered_cents: row.try_get("costs_recovered_cents")?,
        iolta_allocation_id: row.try_get("iolta_allocation_id")?,
        author_person_id: row.try_get("author_person_id")?,
        supersedes_id: row.try_get("supersedes_id")?,
        correction_reason: row.try_get("correction_reason")?,
        recorded_at: row.try_get("recorded_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::{MatterOutcomeError, MatterResult, NewMatterOutcome};
    use chrono::NaiveDate;

    pub(crate) fn settled() -> NewMatterOutcome {
        NewMatterOutcome {
            result: MatterResult::Settled,
            resolved_on: NaiveDate::from_ymd_opt(2027, 3, 1).unwrap(),
            gross_recovery_cents: 15_000_000,
            fee_received_cents: 4_995_000,
            costs_recovered_cents: 1_200_000,
            iolta_allocation_id: Some("synthetic-allocation".into()),
        }
    }

    #[test]
    fn amounts_are_never_negative() {
        assert!(settled().validate().is_ok());
        let mut negative = settled();
        negative.fee_received_cents = -1;
        assert!(matches!(
            negative.validate(),
            Err(MatterOutcomeError::Invalid(_))
        ));
        let mut blank = settled();
        blank.iolta_allocation_id = Some("  ".into());
        assert!(matches!(
            blank.validate(),
            Err(MatterOutcomeError::Invalid("iolta_allocation_id"))
        ));
    }

    #[test]
    fn result_round_trips_its_stored_spelling() {
        for result in [
            MatterResult::Won,
            MatterResult::Settled,
            MatterResult::Lost,
            MatterResult::Dismissed,
        ] {
            assert_eq!(MatterResult::parse(result.as_str()).unwrap(), result);
        }
        assert!(MatterResult::parse("withdrawn").is_err());
    }

    // The remaining assertions need a live PostgreSQL.
    mod database {
        use super::super::{correct, current, history, record, MatterOutcomeError, MatterResult};
        use super::settled;
        use crate::firm_side::tests::matter;
        use crate::postgres_mirror::tests::{connect, fresh_schema};

        #[tokio::test]
        async fn only_a_firm_side_participant_records_or_reads_an_outcome() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let m = matter(&pg).await;
            for outsider in [m.client, m.clerk, m.outside_lawyer] {
                assert!(matches!(
                    record(&pg, m.project_id, outsider, &settled()).await,
                    Err(MatterOutcomeError::NotFirmSide)
                ));
                assert!(matches!(
                    current(&pg, outsider, m.project_id).await,
                    Err(MatterOutcomeError::NotFirmSide)
                ));
                assert!(matches!(
                    history(&pg, outsider, m.project_id).await,
                    Err(MatterOutcomeError::NotFirmSide)
                ));
            }
            assert_eq!(current(&pg, m.lawyer, m.project_id).await.unwrap(), None);
        }

        #[tokio::test]
        async fn one_outcome_per_matter_and_a_correction_supersedes_it() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let m = matter(&pg).await;
            assert!(matches!(
                correct(&pg, m.project_id, m.lawyer, &settled(), "Typo").await,
                Err(MatterOutcomeError::NotRecorded)
            ));
            let original = record(&pg, m.project_id, m.lawyer, &settled())
                .await
                .unwrap();
            assert!(matches!(
                record(&pg, m.project_id, m.lawyer, &settled()).await,
                Err(MatterOutcomeError::AlreadyRecorded)
            ));
            let mut fixed = settled();
            fixed.fee_received_cents = 5_000_000;
            let correction = correct(
                &pg,
                m.project_id,
                m.lawyer,
                &fixed,
                "The fee posted net of a wire charge",
            )
            .await
            .unwrap();
            assert_eq!(correction.supersedes_id, Some(original.id));
            assert_eq!(
                current(&pg, m.lawyer, m.project_id).await.unwrap(),
                Some(correction.clone())
            );
            assert_eq!(
                history(&pg, m.lawyer, m.project_id).await.unwrap(),
                vec![original, correction],
                "the superseded row stays as the audit trail"
            );
        }

        #[tokio::test]
        async fn the_database_refuses_a_second_original_a_fork_and_edits() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let m = matter(&pg).await;
            let original = record(&pg, m.project_id, m.lawyer, &settled())
                .await
                .unwrap();
            let raw = |supersedes: Option<uuid::Uuid>, reason: Option<&'static str>| {
                sqlx::query(
                    "INSERT INTO matter_outcome (project_id,result,resolved_on,gross_recovery_cents,\
                     fee_received_cents,costs_recovered_cents,author_person_id,supersedes_id,\
                     correction_reason) VALUES ($1,'won','2027-01-01',0,0,0,$2,$3,$4)",
                )
                .bind(m.project_id)
                .bind(m.lawyer)
                .bind(supersedes)
                .bind(reason)
            };
            assert!(
                raw(None, None).execute(pg.pool()).await.is_err(),
                "a second original"
            );
            raw(Some(original.id), Some("First fix"))
                .execute(pg.pool())
                .await
                .unwrap();
            assert!(
                raw(Some(original.id), Some("A fork"))
                    .execute(pg.pool())
                    .await
                    .is_err(),
                "a row is superseded at most once"
            );
            let other = matter(&pg).await;
            let foreign = sqlx::query(
                "INSERT INTO matter_outcome (project_id,result,resolved_on,gross_recovery_cents,\
                 fee_received_cents,costs_recovered_cents,author_person_id,supersedes_id,\
                 correction_reason) VALUES ($1,'lost','2027-01-01',0,0,0,$2,$3,'Cross-matter')",
            )
            .bind(other.project_id)
            .bind(other.lawyer)
            .bind(original.id)
            .execute(pg.pool())
            .await;
            assert!(foreign.is_err(), "a correction stays on its own matter");
            for statement in [
                "UPDATE matter_outcome SET result='won' WHERE id=$1",
                "DELETE FROM matter_outcome WHERE id=$1",
            ] {
                assert!(
                    sqlx::query(statement)
                        .bind(original.id)
                        .execute(pg.pool())
                        .await
                        .is_err(),
                    "{statement}"
                );
            }
            assert_eq!(original.result, MatterResult::Settled);
        }
    }
}
