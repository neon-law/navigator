//! The `relationship_log` table — the append-only audit trail of
//! relationship changes (`person joined entity`, `project closed`).
//!
//! # Why it moved with the graph, though it is not part of it
//!
//! A Relationship Log entry is **one-sided**: an actor took an action
//! against a subject. That is a different question from the two-sided
//! edges `store::relationships` holds, and the two are deliberately not
//! merged — the log *feeds* the graph (an LLM may later parse an edge
//! out of `detail`, and a manual adversity edge cites its log row as
//! `source_id`), but the conflict traversal never reads it.
//!
//! So it lives where the graph lives
//! ([`crate::postgres_graph::graph_authority`]): PostgreSQL wherever it is
//! configured, else SurrealDB. In PostgreSQL the
//! row's actor and Project are foreign keys, so a writer appends the entry
//! only once PostgreSQL holds the Person and the Project it names —
//! `store::projects::open_matter` records its attestation after the
//! matter's PostgreSQL copy lands.
//!
//! # Append-only
//!
//! Never updated in place, never deleted. So there is no unique index —
//! two identical entries a second apart are two real events — and no
//! ASSERT on `action`: the trail records what happened, including
//! actions this code has no closed set for.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::PostgresDb;
use crate::postgres_graph::graph_authority;

#[path = "scoped_relationship_logs.rs"]
pub mod scoped;

/// The `subject_type` of an entry about a Person.
pub const PERSON_SUBJECT: &str = "person";

/// One audit entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelationshipLog {
    pub id: Uuid,
    /// Who acted. `None` for a system action — a scheduled workflow
    /// closing a matter has no person behind it.
    pub actor_person_id: Option<Uuid>,
    /// The table the subject lives in, and its key. Polymorphic across
    /// tables on both sides of the port, so the pair stays an
    /// unenforced id rather than becoming a link.
    pub subject_type: String,
    pub subject_id: Uuid,
    pub action: String,
    pub detail: String,
    /// The Project whose work produced the entry — the filing a service
    /// list came from, the matter a module was enabled on. `None` for an
    /// entry no one Project owns. It scopes the entry; it grants nothing.
    pub project_id: Option<Uuid>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The projection every read shares, in both engines.
const SELECT: &str = "id, actor_person_id, subject_type, subject_id, action, detail, \
                      project_id, inserted_at, updated_at";

fn from_pg(row: &sqlx::postgres::PgRow) -> Result<RelationshipLog, sqlx::Error> {
    Ok(RelationshipLog {
        id: row.try_get("id")?,
        actor_person_id: row.try_get("actor_person_id")?,
        subject_type: row.try_get("subject_type")?,
        subject_id: row.try_get("subject_id")?,
        action: row.try_get("action")?,
        detail: row.try_get("detail")?,
        project_id: row.try_get("project_id")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn many_pg(rows: &[sqlx::postgres::PgRow]) -> Result<Vec<RelationshipLog>, RelationshipLogError> {
    rows.iter()
        .map(|row| from_pg(row).map_err(RelationshipLogError::from))
        .collect()
}

async fn authority() -> Result<PostgresDb, RelationshipLogError> {
    graph_authority()
        .await
        .map_err(RelationshipLogError::Engine)
}

/// What a write stores.
#[derive(Debug, Clone)]
pub struct NewRelationshipLog {
    pub actor_person_id: Option<Uuid>,
    pub subject_type: String,
    pub subject_id: Uuid,
    pub action: String,
    pub detail: String,
    /// The Project the entry belongs to, when one does.
    pub project_id: Option<Uuid>,
}

#[derive(Debug, thiserror::Error)]
pub enum RelationshipLogError {
    #[error("writing a relationship log entry returned no usable row")]
    WriteReturnedNothing,
    /// The authoritative PostgreSQL store refused the read or write.
    #[error("PostgreSQL: {0}")]
    Postgres(#[from] sqlx::Error),
    /// PostgreSQL is configured but unreachable.
    #[error("{0}")]
    Engine(String),
}

/// Append an audit entry in the caller's PostgreSQL transaction.
pub(crate) async fn record_on<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: Uuid,
    input: &NewRelationshipLog,
) -> Result<RelationshipLog, sqlx::Error> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO relationship_log (id, actor_person_id, subject_type, subject_id, \
         action, detail, project_id) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {SELECT}"
    )))
    .bind(id)
    .bind(input.actor_person_id)
    .bind(&input.subject_type)
    .bind(input.subject_id)
    .bind(&input.action)
    .bind(&input.detail)
    .bind(input.project_id)
    .fetch_one(executor)
    .await?;
    from_pg(&row)
}

/// Append one entry to the trail.
///
/// # Errors
///
/// [`RelationshipLogError::Db`] or [`RelationshipLogError::Postgres`] if
/// the write fails, or [`RelationshipLogError::Engine`] if PostgreSQL is
/// configured but unreachable.
pub async fn record(
    _db: &ApplicationStore,
    input: &NewRelationshipLog,
) -> Result<RelationshipLog, RelationshipLogError> {
    let id = Uuid::now_v7();
    let pg = authority().await?;
    Ok(record_on(&pg.0, id, input).await?)
}

/// Whether an entry identical to `input` — same actor-independent subject,
/// action, and detail — is already on the trail. The seed import uses it to
/// keep a re-run from appending the same event twice.
///
/// # Errors
///
/// [`RelationshipLogError::Db`] or [`RelationshipLogError::Postgres`] if
/// the lookup fails.
pub async fn exists(
    _db: &ApplicationStore,
    input: &NewRelationshipLog,
) -> Result<bool, RelationshipLogError> {
    let pg = authority().await?;
    let found = sqlx::query(
        "SELECT 1 FROM relationship_log WHERE subject_type = $1 \
         AND subject_id = $2 AND action = $3 AND detail = $4 LIMIT 1",
    )
    .bind(&input.subject_type)
    .bind(input.subject_id)
    .bind(&input.action)
    .bind(&input.detail)
    .fetch_optional(&pg.0)
    .await?;
    Ok(found.is_some())
}

/// The whole trail, newest first — the order the lawyer listing reads,
/// which `relationship_log_inserted_at` indexes. Entries recorded in one
/// statement share `inserted_at`; the UUIDv7 id breaks the tie.
///
/// # Errors
///
/// [`RelationshipLogError::Db`] or [`RelationshipLogError::Postgres`] if
/// the lookup fails.
pub async fn all(_db: &ApplicationStore) -> Result<Vec<RelationshipLog>, RelationshipLogError> {
    let pg = authority().await?;
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {SELECT} FROM relationship_log ORDER BY inserted_at DESC, id DESC"
    )))
    .fetch_all(&pg.0)
    .await?;
    many_pg(&rows)
}

/// Every entry about one subject, newest first.
///
/// # Errors
///
/// [`RelationshipLogError::Db`] or [`RelationshipLogError::Postgres`] if
/// the lookup fails.
pub async fn for_subject(
    _db: &ApplicationStore,
    subject_type: &str,
    subject_id: Uuid,
) -> Result<Vec<RelationshipLog>, RelationshipLogError> {
    let pg = authority().await?;
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {SELECT} FROM relationship_log WHERE subject_type = $1 AND subject_id = $2 \
         ORDER BY inserted_at DESC, id DESC"
    )))
    .bind(subject_type)
    .bind(subject_id)
    .fetch_all(&pg.0)
    .await?;
    many_pg(&rows)
}

/// Every entry recorded for one Project, newest first.
///
/// # Errors
///
/// [`RelationshipLogError::Db`] or [`RelationshipLogError::Postgres`] if
/// the lookup fails.
pub async fn for_project(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<RelationshipLog>, RelationshipLogError> {
    let pg = authority().await?;
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {SELECT} FROM relationship_log WHERE project_id = $1 \
         ORDER BY inserted_at DESC, id DESC"
    )))
    .bind(project_id)
    .fetch_all(&pg.0)
    .await?;
    many_pg(&rows)
}

#[cfg(test)]
mod tests {}
