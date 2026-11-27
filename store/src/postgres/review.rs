//! PostgreSQL authority for the review surface:
//! `review_document` and `document_comment`.
//!
//! Whenever `NAVIGATOR_POSTGRES_HOST` is set ([`PostgresDb::configured`]),
//! the owning modules (`review_documents`, `document_comments`) read and
//! write PostgreSQL only, a PostgreSQL failure is returned rather than
//! falling back, and the mirror stops copying SurrealDB rows over the
//! authoritative ones. Without a PostgreSQL host the SurrealDB path is the
//! fallback. Parent rows the tables name — the Notation and the Person —
//! stay on their own authorities.
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
    static TEST_REVIEW_DB: PostgresDb;
}

/// Why PostgreSQL could not serve a review-surface call. Carries no row
/// values.
#[derive(Debug, thiserror::Error)]
pub enum ReviewAuthorityError {
    #[error("PostgreSQL is unavailable for the review tables")]
    Unavailable,
    #[error("PostgreSQL refused the review operation: {0}")]
    Query(#[from] sqlx::Error),
    #[error("PostgreSQL returned an unreadable review row")]
    Row(#[from] serde_json::Error),
}

impl ReviewAuthorityError {
    /// Whether this is a unique violation of the named constraint.
    pub(crate) fn violates(&self, constraint: &str) -> bool {
        matches!(self, Self::Query(sqlx::Error::Database(database))
            if database.is_unique_violation() && database.constraint() == Some(constraint))
    }
}

/// A review-surface operation. Boxed so every owning module that awaits one
/// keeps a small future instead of embedding the PostgreSQL client's.
pub(crate) type Op<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ReviewAuthorityError>> + Send + 'a>>;

type Query<'q> = sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>;

/// The pool to serve from, or `None` when no PostgreSQL host is configured.
pub(crate) fn authority() -> Op<'static, PostgresDb> {
    Box::pin(async {
        #[cfg(test)]
        if let Ok(pg) = TEST_REVIEW_DB.try_with(Clone::clone) {
            return Ok(pg);
        }
        PostgresDb::application()
            .await
            .map_err(|_| ReviewAuthorityError::Unavailable)
    })
}

fn decode<T: DeserializeOwned>(row: &sqlx::postgres::PgRow) -> Result<T, ReviewAuthorityError> {
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

/// The first column of the row `query` returns, if any.
pub(crate) fn maybe_scalar<'a, T>(pg: &'a PostgresDb, query: Query<'a>) -> Op<'a, Option<T>>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + 'a,
{
    Box::pin(async move {
        let row = query.fetch_optional(pg.pool()).await?;
        match row {
            Some(row) => Ok(Some(row.try_get(0)?)),
            None => Ok(None),
        }
    })
}

/// Every first-column value `query` returns.
pub(crate) fn scalars<'a, T>(pg: &'a PostgresDb, query: Query<'a>) -> Op<'a, Vec<T>>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + 'a,
{
    Box::pin(async move {
        let mut out = Vec::new();
        for row in query.fetch_all(pg.pool()).await? {
            out.push(row.try_get(0)?);
        }
        Ok(out)
    })
}

/// Run `future` with `pg` serving the review tables.
#[cfg(test)]
pub(crate) async fn with_test_db<T>(pg: PostgresDb, future: impl Future<Output = T>) -> T {
    TEST_REVIEW_DB.scope(pg, future).await
}

#[cfg(test)]
#[path = "review_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "review_compatibility_tests.rs"]
mod compatibility_tests;
