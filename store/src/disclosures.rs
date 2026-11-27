//! `disclosures` — the formal disclosures the firm records against an
//! Entity or a matter (conflicts, related-party, and the rest).
//!
//! The conflict graph reads the `conflict` and `related_party` kinds:
//! a recorded disclosure on any entity the traversal reaches always
//! surfaces, regardless of how it was reached. See [`crate::conflicts`].
//!
//! # Where the rows live
//!
//! With the graph: PostgreSQL wherever it is configured, else SurrealDB
//! ([`crate::postgres_graph::graph_authority`]).

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::Row;
use thiserror::Error;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::PostgresDb;
use crate::postgres_graph::graph_authority;

/// A conflict the firm has recorded against an Entity.
pub const KIND_CONFLICT: &str = "conflict";
/// A related-party relationship the firm has recorded.
pub const KIND_RELATED_PARTY: &str = "related_party";

/// The kinds the conflict graph surfaces. Other kinds are recorded and
/// read back, but do not by themselves raise a conflict finding.
pub const CONFLICT_KINDS: &[&str] = &[KIND_CONFLICT, KIND_RELATED_PARTY];

/// What can go wrong reading or writing a disclosure.
#[derive(Debug, Error)]
pub enum DisclosureError {
    /// A database operation failed.
    /// The authoritative PostgreSQL store refused the read or write.
    #[error("PostgreSQL: {0}")]
    Postgres(#[from] sqlx::Error),
    /// PostgreSQL is configured but unreachable.
    #[error("{0}")]
    Engine(String),
}

/// One formal disclosure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Disclosure {
    pub id: Uuid,
    /// The company the disclosure is about, when it is about one.
    pub entity_id: Option<Uuid>,
    /// The matter the disclosure is about, when it is about one.
    pub project_id: Option<Uuid>,
    pub kind: String,
    pub summary: String,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The projection every read shares, in both engines.
const SELECT: &str = "id, entity_id, project_id, kind, summary, inserted_at, updated_at";

fn from_pg(row: &sqlx::postgres::PgRow) -> Result<Disclosure, sqlx::Error> {
    Ok(Disclosure {
        id: row.try_get("id")?,
        entity_id: row.try_get("entity_id")?,
        project_id: row.try_get("project_id")?,
        kind: row.try_get("kind")?,
        summary: row.try_get("summary")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

async fn authority() -> Result<PostgresDb, DisclosureError> {
    graph_authority().await.map_err(DisclosureError::Engine)
}

/// What to record for one disclosure.
#[derive(Debug, Clone)]
pub struct NewDisclosure<'a> {
    pub entity_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub kind: &'a str,
    pub summary: &'a str,
}

/// Record one disclosure, returning its id.
///
/// # Errors
/// Propagates any database error.
pub async fn record(
    _db: &ApplicationStore,
    new: &NewDisclosure<'_>,
) -> Result<Uuid, DisclosureError> {
    let id = Uuid::now_v7();
    let pg = authority().await?;
    sqlx::query(
        "INSERT INTO \"disclosure\" (id, entity_id, project_id, kind, summary, \
         inserted_at, updated_at) VALUES ($1, $2, $3, $4, $5, now(), now())",
    )
    .bind(id)
    .bind(new.entity_id)
    .bind(new.project_id)
    .bind(new.kind)
    .bind(new.summary)
    .execute(&pg.0)
    .await?;
    Ok(id)
}

/// Every disclosure, oldest first — the lawyer directory listing.
///
/// # Errors
/// Propagates any database error.
pub async fn all(_db: &ApplicationStore) -> Result<Vec<Disclosure>, DisclosureError> {
    let pg = authority().await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {SELECT} FROM \"disclosure\" ORDER BY id ASC"
    )))
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(|row| from_pg(row).map_err(DisclosureError::from))
    .collect()
}

/// Every conflict / related-party disclosure that names an Entity,
/// grouped by that Entity — the lookup the conflict graph builds once per
/// check, from PostgreSQL when the check walks the AGE graph (`postgres`)
/// and from SurrealDB otherwise.
///
/// Bulk rather than per-entity: an under-inclusive result here is a
/// **missed conflict**, so the graph loads the whole set up front rather
/// than querying per node reached.
///
/// # Errors
/// Propagates any database error.
pub(crate) async fn conflict_summaries_by_entity(
    pg: &PostgresDb,
) -> Result<std::collections::HashMap<Uuid, Vec<String>>, DisclosureError> {
    let kinds: Vec<String> = CONFLICT_KINDS.iter().map(|k| (*k).to_string()).collect();
    let rows: Vec<Disclosure> = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {SELECT} FROM \"disclosure\" \
             WHERE kind = ANY($1) AND entity_id IS NOT NULL ORDER BY id ASC"
    )))
    .bind(&kinds)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(from_pg)
    .collect::<Result<_, _>>()?;
    let mut by_entity: std::collections::HashMap<Uuid, Vec<String>> =
        std::collections::HashMap::new();
    for row in rows {
        if let Some(entity_id) = row.entity_id {
            by_entity.entry(entity_id).or_default().push(row.summary);
        }
    }
    Ok(by_entity)
}

#[cfg(test)]
mod tests {}
