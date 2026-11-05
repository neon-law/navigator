//! The PostgreSQL `sent_email` table — the trail's system of record — and its
//! Sent Email Export ledger.
//!
//! Rows are append-only in the database: a trigger refuses UPDATE and
//! TRUNCATE, and refuses DELETE unless the session sets
//! `navigator.sent_email_purge`, the row is over a year old, and its UTC day is
//! in `sent_email_export`. [`purge`] is the only caller that sets it.
//!
//! The export (`archives::sent_email_export`) copies each closed UTC day to an
//! Iceberg table and records it here with [`record_export`]. [`pending_days`]
//! names the days with no ledger row, or with more rows than the ledger
//! counted (a late insert); a re-run over an exported, unchanged day finds
//! nothing to do. [`purge`] deletes a day only when the ledger confirms it and
//! the day still holds exactly the rows the export counted, then stamps the
//! ledger so the purged day is never exported again.

use chrono::{DateTime, Months, NaiveDate, Utc};
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgRow, Row};
use uuid::Uuid;

use super::{NewSentEmail, Page, SentEmail, SentEmailError};
use crate::postgres::PostgresDb;

/// How long a row stays in PostgreSQL before the purge may remove it.
pub const RETENTION_MONTHS: u32 = 12;

/// The session setting the append-only trigger reads to admit a purge.
const PURGE_SETTING: &str = "navigator.sent_email_purge";

const SELECT: &str = "id, recipient, subject, sender, project_id, person_id, template_slug, \
                      body, outcome, sg_message_id, sent_at, inserted_at, updated_at";

fn from_row(row: &PgRow) -> Result<SentEmail, sqlx::Error> {
    Ok(SentEmail {
        id: row.try_get("id")?,
        recipient: row.try_get("recipient")?,
        subject: row.try_get("subject")?,
        sender: row.try_get("sender")?,
        project_id: row.try_get("project_id")?,
        person_id: row.try_get("person_id")?,
        template_slug: row.try_get("template_slug")?,
        body: row.try_get("body")?,
        outcome: row.try_get("outcome")?,
        sg_message_id: row.try_get("sg_message_id")?,
        sent_at: row.try_get("sent_at")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Journal one outbound attempt.
///
/// # Errors
///
/// [`SentEmailError::Sql`] if the insert fails — including a `project_id`
/// that names no PostgreSQL Project.
pub async fn record(pg: &PostgresDb, new: &NewSentEmail) -> Result<SentEmail, SentEmailError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO sent_email (id, recipient, subject, sender, project_id, person_id, \
         template_slug, body, outcome, sg_message_id, sent_at, inserted_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, now(), now()) RETURNING {SELECT}"
    )))
    .bind(Uuid::now_v7())
    .bind(&new.recipient)
    .bind(&new.subject)
    .bind(&new.sender)
    .bind(new.project_id)
    .bind(new.person_id)
    .bind(&new.template_slug)
    .bind(&new.body)
    .bind(&new.outcome)
    .bind(&new.sg_message_id)
    .bind(new.sent_at)
    .fetch_one(&pg.0)
    .await?;
    Ok(from_row(&row)?)
}

/// One page of the trail, newest first, with `requested` clamped into range.
/// The count and the fetch share one `REPEATABLE READ` snapshot, and
/// `project_ids` scopes both, exactly as the SurrealDB pager does.
///
/// # Errors
///
/// [`SentEmailError::Sql`] if a query fails.
pub async fn page(
    pg: &PostgresDb,
    requested: u64,
    per_page: u64,
    project_ids: Option<&[Uuid]>,
) -> Result<Page, SentEmailError> {
    let per_page = per_page.max(1);
    let scoped = project_ids.is_some();
    let projects: Vec<Uuid> = project_ids.unwrap_or_default().to_vec();
    let mut tx = pg.0.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sent_email WHERE (NOT $1 OR project_id = ANY($2))",
    )
    .bind(scoped)
    .bind(&projects)
    .fetch_one(&mut *tx)
    .await?;
    let total = u64::try_from(total).unwrap_or(0);
    let total_pages = total.div_ceil(per_page).max(1);
    let page = requested.clamp(1, total_pages);
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {SELECT} FROM sent_email WHERE (NOT $1 OR project_id = ANY($2)) \
         ORDER BY sent_at DESC, id DESC LIMIT $3 OFFSET $4"
    )))
    .bind(scoped)
    .bind(&projects)
    .bind(i64::try_from(per_page).unwrap_or(i64::MAX))
    .bind(i64::try_from((page - 1).saturating_mul(per_page)).unwrap_or(i64::MAX))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Page {
        rows: rows.iter().map(from_row).collect::<Result<_, _>>()?,
        total_pages,
        page,
    })
}

/// Every row, newest first.
///
/// # Errors
///
/// [`SentEmailError::Sql`] if the query fails.
pub async fn all(pg: &PostgresDb) -> Result<Vec<SentEmail>, SentEmailError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {SELECT} FROM sent_email ORDER BY sent_at DESC, id DESC"
    )))
    .fetch_all(&pg.0)
    .await?;
    Ok(rows.iter().map(from_row).collect::<Result<_, _>>()?)
}

/// One UTC day's rows, oldest first — the order the export writes and
/// [`content_sha256`] digests.
///
/// # Errors
///
/// [`SentEmailError::Sql`] if the query fails.
pub async fn day_rows(pg: &PostgresDb, day: NaiveDate) -> Result<Vec<SentEmail>, SentEmailError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {SELECT} FROM sent_email WHERE (sent_at AT TIME ZONE 'UTC')::date = $1 \
         ORDER BY sent_at, id"
    )))
    .bind(day)
    .fetch_all(&pg.0)
    .await?;
    Ok(rows.iter().map(from_row).collect::<Result<_, _>>()?)
}

/// The SHA-256 of a day's row ids, in order. Rows never change, so the ids
/// name the content; the digest says whether an exported day is still the
/// day PostgreSQL holds.
#[must_use]
pub fn content_sha256(ids: impl IntoIterator<Item = Uuid>) -> String {
    let mut hash = Sha256::new();
    for id in ids {
        hash.update(id.as_bytes());
    }
    hash.finalize()
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// What the export wrote for one day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayExport {
    pub day: NaiveDate,
    pub row_count: u64,
    pub content_sha256: String,
    /// The data file's absolute object URI, as the Iceberg manifest records it.
    pub data_file: String,
    pub snapshot_id: i64,
    pub metadata_version: u64,
    pub exported_at: DateTime<Utc>,
    pub purged_at: Option<DateTime<Utc>>,
}

fn export_from_row(row: &PgRow) -> Result<DayExport, sqlx::Error> {
    let count: i64 = row.try_get("row_count")?;
    let version: i64 = row.try_get("metadata_version")?;
    Ok(DayExport {
        day: row.try_get("day")?,
        row_count: u64::try_from(count).unwrap_or(0),
        content_sha256: row.try_get("content_sha256")?,
        data_file: row.try_get("data_file")?,
        snapshot_id: row.try_get("snapshot_id")?,
        metadata_version: u64::try_from(version).unwrap_or(0),
        exported_at: row.try_get("exported_at")?,
        purged_at: row.try_get("purged_at")?,
    })
}

/// The ledger row for `day`, if the export has written it.
///
/// # Errors
///
/// [`SentEmailError::Sql`] if the query fails.
pub async fn export_for(
    pg: &PostgresDb,
    day: NaiveDate,
) -> Result<Option<DayExport>, SentEmailError> {
    let row = sqlx::query("SELECT * FROM sent_email_export WHERE day = $1")
        .bind(day)
        .fetch_optional(&pg.0)
        .await?;
    Ok(row.as_ref().map(export_from_row).transpose()?)
}

/// Days before `before` that need exporting: rows and no ledger row, or more
/// rows than an unpurged ledger row counted. Oldest first.
///
/// # Errors
///
/// [`SentEmailError::Sql`] if the query fails.
pub async fn pending_days(
    pg: &PostgresDb,
    before: NaiveDate,
) -> Result<Vec<NaiveDate>, SentEmailError> {
    Ok(sqlx::query_scalar(
        "WITH days AS ( \
             SELECT (sent_at AT TIME ZONE 'UTC')::date AS day, count(*) AS n \
             FROM sent_email WHERE sent_at < ($1::date)::timestamp AT TIME ZONE 'UTC' \
             GROUP BY 1) \
         SELECT days.day FROM days LEFT JOIN sent_email_export e ON e.day = days.day \
         WHERE e.day IS NULL OR (e.purged_at IS NULL AND days.n > e.row_count) \
         ORDER BY days.day",
    )
    .bind(before)
    .fetch_all(&pg.0)
    .await?)
}

/// Record (or, for a day that gained rows, replace) one day's export. A purged
/// day's row is never replaced.
///
/// # Errors
///
/// [`SentEmailError::Sql`] if the write fails.
pub async fn record_export(pg: &PostgresDb, export: &DayExport) -> Result<(), SentEmailError> {
    sqlx::query(
        "INSERT INTO sent_email_export \
         (day, row_count, content_sha256, data_file, snapshot_id, metadata_version, exported_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (day) DO UPDATE SET row_count = EXCLUDED.row_count, \
         content_sha256 = EXCLUDED.content_sha256, data_file = EXCLUDED.data_file, \
         snapshot_id = EXCLUDED.snapshot_id, metadata_version = EXCLUDED.metadata_version, \
         exported_at = EXCLUDED.exported_at \
         WHERE sent_email_export.purged_at IS NULL",
    )
    .bind(export.day)
    .bind(i64::try_from(export.row_count).unwrap_or(i64::MAX))
    .bind(&export.content_sha256)
    .bind(&export.data_file)
    .bind(export.snapshot_id)
    .bind(i64::try_from(export.metadata_version).unwrap_or(i64::MAX))
    .bind(export.exported_at)
    .execute(&pg.0)
    .await?;
    Ok(())
}

/// What the purge did with one exported day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DayPurge {
    /// Deleted this many rows; rows evidencing a Project Service stay.
    Purged { deleted: u64, retained: u64 },
    /// The day holds rows the export never counted, so nothing was deleted;
    /// the next export run writes them first.
    NotExported,
}

/// The cutoff instant and the last purgeable day for `now`: a day is purged
/// only once every instant in it is past the retention window.
#[must_use]
pub fn retention_cutoff(now: DateTime<Utc>) -> (DateTime<Utc>, NaiveDate) {
    let cutoff = now
        .checked_sub_months(Months::new(RETENTION_MONTHS))
        .unwrap_or(now);
    let last_whole_day = cutoff
        .date_naive()
        .pred_opt()
        .unwrap_or(cutoff.date_naive());
    (cutoff, last_whole_day)
}

/// Delete rows older than one year, one exported day at a time.
///
/// Each day runs in its own transaction: it locks the day's ledger row,
/// recounts and re-digests the day, and deletes only when both match what the
/// export recorded. Rows a `project_service` cites as evidence stay. The day's
/// ledger row is stamped `purged_at` so it is never exported again.
///
/// # Errors
///
/// [`SentEmailError::Sql`] if a query fails; days before it remain purged.
pub async fn purge(
    pg: &PostgresDb,
    now: DateTime<Utc>,
) -> Result<Vec<(NaiveDate, DayPurge)>, SentEmailError> {
    let (_, last_day) = retention_cutoff(now);
    let days: Vec<NaiveDate> = sqlx::query_scalar(
        "SELECT day FROM sent_email_export WHERE purged_at IS NULL AND day <= $1 ORDER BY day",
    )
    .bind(last_day)
    .fetch_all(&pg.0)
    .await?;
    let mut report = Vec::with_capacity(days.len());
    for day in days {
        report.push((day, purge_day(pg, day).await?));
    }
    Ok(report)
}

async fn purge_day(pg: &PostgresDb, day: NaiveDate) -> Result<DayPurge, SentEmailError> {
    let mut tx = pg.0.begin().await?;
    let ledger: Option<(i64, String)> = sqlx::query_as(
        "SELECT row_count, content_sha256 FROM sent_email_export \
         WHERE day = $1 AND purged_at IS NULL FOR UPDATE",
    )
    .bind(day)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((row_count, digest)) = ledger else {
        return Ok(DayPurge::NotExported);
    };
    // `FOR SHARE` holds the day's rows still while they are counted; an
    // insert for the day cannot land between the digest and the delete,
    // because new rows are this year's, not a day this old.
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM sent_email WHERE (sent_at AT TIME ZONE 'UTC')::date = $1 \
         ORDER BY sent_at, id FOR SHARE",
    )
    .bind(day)
    .fetch_all(&mut *tx)
    .await?;
    if i64::try_from(ids.len()).ok() != Some(row_count) || content_sha256(ids) != digest {
        return Ok(DayPurge::NotExported);
    }
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT set_config('{PURGE_SETTING}', 'on', true)"
    )))
    .execute(&mut *tx)
    .await?;
    let deleted = sqlx::query(
        "DELETE FROM sent_email s WHERE (s.sent_at AT TIME ZONE 'UTC')::date = $1 \
         AND NOT EXISTS (SELECT 1 FROM project_service p WHERE p.sent_email_id = s.id)",
    )
    .bind(day)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query("UPDATE sent_email_export SET purged_at = now() WHERE day = $1")
        .bind(day)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let total = u64::try_from(row_count).unwrap_or(0);
    Ok(DayPurge::Purged {
        deleted,
        retained: total.saturating_sub(deleted),
    })
}

#[cfg(test)]
#[path = "postgres_tests.rs"]
mod tests;
