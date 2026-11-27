//! PostgreSQL authority for Brand and social content: `post`, `blog_video`,
//! `social_handle`, `social_publication`, `social_account`, and
//! `social_account_selection`.
//!
//! Whenever `NAVIGATOR_POSTGRES_HOST` is set ([`PostgresDb::configured`]),
//! each owning module (`posts`, `social_handles`, `social_publications`,
//! `social_accounts`, `social_account_selections`) reads and writes
//! PostgreSQL only, and a PostgreSQL failure is returned rather than falling
//! back. The mirror copies SurrealDB rows into these tables only during the
//! first Mirror Backfill (see [`crate::postgres_mirror::initial_copy`]).
//! Without a PostgreSQL host the SurrealDB path is the fallback. The Brand,
//! Firm, and Person rows the tables name must exist in PostgreSQL.
//!
//! Rows come back as the JSON object `to_jsonb` makes of them, so every
//! statement selects its row as `row` and the owning module's own serde type
//! decodes it.

use std::future::Future;
use std::pin::Pin;

use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::Row;

use crate::postgres::PostgresDb;

#[cfg(test)]
tokio::task_local! {
    static TEST_SOCIAL_DB: PostgresDb;
}

/// Why PostgreSQL could not serve a social content call. Carries no row
/// values.
#[derive(Debug, thiserror::Error)]
pub enum SocialAuthorityError {
    #[error("PostgreSQL is unavailable for the social content tables")]
    Unavailable,
    #[error("PostgreSQL refused the social content operation: {0}")]
    Query(#[from] sqlx::Error),
    #[error("PostgreSQL returned an unreadable social content row")]
    Row(#[from] serde_json::Error),
}

/// A social content operation. Boxed so every owning module that awaits one
/// keeps a small future instead of embedding the PostgreSQL client's.
pub(crate) type Op<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SocialAuthorityError>> + Send + 'a>>;

pub(crate) type Query<'q> = sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>;

/// The pool to serve from, or `None` when no PostgreSQL host is configured
/// and for the mirror reading its SurrealDB source.
pub(crate) fn authority() -> Op<'static, PostgresDb> {
    Box::pin(async {
        #[cfg(test)]
        if let Ok(pg) = TEST_SOCIAL_DB.try_with(Clone::clone) {
            return Ok(pg);
        }
        PostgresDb::application()
            .await
            .map_err(|_| SocialAuthorityError::Unavailable)
    })
}

fn decode<T: DeserializeOwned>(row: &sqlx::postgres::PgRow) -> Result<T, SocialAuthorityError> {
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
#[path = "social_tests.rs"]
mod tests;
