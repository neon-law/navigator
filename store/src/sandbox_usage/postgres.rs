//! The PostgreSQL `sandbox_usage` table, the ledger's system of record
//! whenever PostgreSQL is configured.
//!
//! `UNIQUE (person_id, period)` is the concurrency boundary here: a racing
//! first use of a month resolves on that constraint, and a debit is one
//! conditional `UPDATE` of the one row, so parallel debits queue on its row
//! lock and each re-reads the balance its predecessor committed. Every `SET`
//! expression reads the pre-update row, so the price is counted once.

use sqlx::{postgres::PgRow, Row};
use uuid::Uuid;

use super::{
    MeterDecision, SandboxUsage, SandboxUsageError, SandboxUsageRow, MONTHLY_GRANT_CENTS, SELECT,
    TABLE,
};
use crate::postgres::PostgresDb;
use crate::postgres_backfill::Copied;
use crate::surreal::SurrealDb;

fn usage(row: &PgRow) -> Result<SandboxUsage, sqlx::Error> {
    Ok(SandboxUsage {
        person_id: row.try_get("person_id")?,
        period: row.try_get("period")?,
        granted_cents: row.try_get("granted_cents")?,
        spent_cents: row.try_get("spent_cents")?,
        overage: row.try_get("overage")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// See [`super::find`].
pub(super) async fn find(
    pg: &PostgresDb,
    person_id: Uuid,
    period: &str,
) -> Result<Option<SandboxUsage>, SandboxUsageError> {
    Ok(sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {SELECT} FROM {TABLE} WHERE person_id = $1 AND period = $2"
    )))
    .bind(person_id)
    .bind(period)
    .fetch_optional(&pg.0)
    .await?
    .as_ref()
    .map(usage)
    .transpose()?)
}

/// Create the period's row with the contract grant unless one exists.
async fn ensure(pg: &PostgresDb, person_id: Uuid, period: &str) -> Result<(), SandboxUsageError> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO {TABLE} (id, person_id, period, granted_cents, spent_cents, overage) \
         VALUES ($1, $2, $3, $4, 0, false) ON CONFLICT (person_id, period) DO NOTHING"
    )))
    .bind(Uuid::now_v7())
    .bind(person_id)
    .bind(period)
    .bind(MONTHLY_GRANT_CENTS)
    .execute(&pg.0)
    .await?;
    Ok(())
}

/// See [`super::grant`]. An existing row is returned untouched.
pub(super) async fn grant(
    pg: &PostgresDb,
    person_id: Uuid,
    period: &str,
) -> Result<SandboxUsage, SandboxUsageError> {
    ensure(pg, person_id, period).await?;
    find(pg, person_id, period)
        .await?
        .ok_or(SandboxUsageError::WriteReturnedNothing)
}

/// See [`super::debit`]. The `WHERE` is the whole decision: a row already
/// flagged `overage` updates nothing and reports [`MeterDecision::Exhausted`].
pub(super) async fn debit(
    pg: &PostgresDb,
    person_id: Uuid,
    period: &str,
    price_cents: i64,
) -> Result<MeterDecision, SandboxUsageError> {
    ensure(pg, person_id, period).await?;
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE {TABLE} SET overage = (spent_cents + $3) > granted_cents, \
         spent_cents = spent_cents + $3, updated_at = now() \
         WHERE person_id = $1 AND period = $2 AND NOT overage RETURNING {SELECT}"
    )))
    .bind(person_id)
    .bind(period)
    .bind(price_cents)
    .fetch_optional(&pg.0)
    .await?;
    Ok(match row.as_ref().map(usage).transpose()? {
        Some(row) => MeterDecision::Charge {
            spent_cents: row.spent_cents,
            remaining_cents: row.remaining_cents(),
        },
        None => MeterDecision::Exhausted,
    })
}

/// See [`super::overage_rows`].
pub(super) async fn overage_rows(
    pg: &PostgresDb,
    period: &str,
) -> Result<Vec<SandboxUsage>, SandboxUsageError> {
    Ok(sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {SELECT} FROM {TABLE} WHERE period = $1 AND overage ORDER BY person_id"
    )))
    .bind(period)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(usage)
    .collect::<Result<_, _>>()?)
}

/// Copy every SurrealDB period row PostgreSQL does not hold. A period
/// PostgreSQL already holds keeps its own balance.
pub(crate) async fn copy_from_surreal(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<Copied, surrealdb::Error> {
    let rows: Vec<SandboxUsageRow> = surreal
        .query(format!("SELECT {SELECT} FROM {TABLE}"))
        .await
        .and_then(surrealdb::IndexedResults::check)?
        .take(0)?;
    let mut copied = Copied::default();
    for row in rows {
        let Some(usage) = row.into_usage() else {
            copied.unrepresentable(TABLE);
            continue;
        };
        let result = sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {TABLE} (id, person_id, period, granted_cents, spent_cents, overage, \
             inserted_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (person_id, period) DO NOTHING"
        )))
        .bind(Uuid::now_v7())
        .bind(usage.person_id)
        .bind(&usage.period)
        .bind(usage.granted_cents)
        .bind(usage.spent_cents)
        .bind(usage.overage)
        .bind(usage.inserted_at)
        .bind(usage.updated_at)
        .execute(&pg.0)
        .await
        .map(|done| done.rows_affected());
        copied.record(TABLE, Some(usage.person_id), result);
    }
    Ok(copied)
}
