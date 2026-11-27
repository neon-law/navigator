//! PostgreSQL authority for Communications and the email tables:
//! `communication`, `email_conversation`, `email_conversation_message`,
//! `email_receipt`, `email_delivery`, and `email_delivery_attempt`.
//!
//! Ordinary access requires PostgreSQL application admission. Parent Project,
//! Person, Firm, Notation, and Asset rows must exist in PostgreSQL. Explicit
//! Source reads remain confined to captured-history compatibility operations.
//!
//! Rows come back as the JSON object `to_jsonb` makes of them, so every
//! statement selects `to_jsonb(<alias>) AS row` and the owning module's own
//! serde type decodes it.

use std::future::Future;
use std::pin::Pin;

use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::Row;

use crate::postgres::PostgresDb;

#[cfg(test)]
tokio::task_local! {
    static TEST_COMMUNICATIONS_DB: PostgresDb;
}

/// Why PostgreSQL could not serve a communications call. Carries no row
/// values.
#[derive(Debug, thiserror::Error)]
pub enum CommunicationsAuthorityError {
    #[error("PostgreSQL is unavailable for the communications tables")]
    Unavailable,
    #[error("PostgreSQL refused the communications operation: {0}")]
    Query(#[from] sqlx::Error),
    #[error("PostgreSQL returned an unreadable communications row")]
    Row(#[from] serde_json::Error),
}

impl CommunicationsAuthorityError {
    /// Whether this is a unique violation of the named constraint.
    pub(crate) fn violates(&self, constraint: &str) -> bool {
        matches!(self, Self::Query(sqlx::Error::Database(database))
            if database.is_unique_violation() && database.constraint() == Some(constraint))
    }
}

/// A communications operation. Boxed so every owning module that awaits one
/// keeps a small future instead of embedding the PostgreSQL client's.
pub(crate) type Op<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, CommunicationsAuthorityError>> + Send + 'a>>;

type Query<'q> = sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>;

/// The pool to serve from, or `None` when no PostgreSQL host is configured.
pub(crate) fn authority() -> Op<'static, PostgresDb> {
    Box::pin(async {
        #[cfg(test)]
        if let Ok(pg) = TEST_COMMUNICATIONS_DB.try_with(Clone::clone) {
            return Ok(pg);
        }
        PostgresDb::application()
            .await
            .map_err(|_| CommunicationsAuthorityError::Unavailable)
    })
}

fn decode<T: DeserializeOwned>(
    row: &sqlx::postgres::PgRow,
) -> Result<T, CommunicationsAuthorityError> {
    let value: Value = row.try_get("row")?;
    Ok(serde_json::from_value(value)?)
}

/// Every row `query` selects as `row`.
pub(crate) fn rows<'a, T: DeserializeOwned + Send + 'a>(
    pg: &'a PostgresDb,
    query: Query<'a>,
) -> Op<'a, Vec<T>> {
    Box::pin(async move {
        query
            .fetch_all(pg.pool())
            .await?
            .iter()
            .map(decode)
            .collect()
    })
}

/// The row `query` selects as `row`, if any.
pub(crate) fn maybe<'a, T: DeserializeOwned + Send + 'a>(
    pg: &'a PostgresDb,
    query: Query<'a>,
) -> Op<'a, Option<T>> {
    Box::pin(async move {
        query
            .fetch_optional(pg.pool())
            .await?
            .as_ref()
            .map(decode)
            .transpose()
    })
}

/// Run a statement that returns no row; return how many rows it touched.
pub(crate) fn execute<'a>(pg: &'a PostgresDb, query: Query<'a>) -> Op<'a, u64> {
    Box::pin(async move { Ok(query.execute(pg.pool()).await?.rows_affected()) })
}

#[cfg(test)]
#[path = "communications_tests.rs"]
mod tests;
