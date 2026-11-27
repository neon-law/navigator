//! PostgreSQL authority for the litigation domain: `case`,
//! `case_docket_entry`, `discovery_request`, `discovery_item`, and `filing`.
//!
//! Whenever `NAVIGATOR_POSTGRES_HOST` is set ([`PostgresDb::configured`]),
//! the owning modules (`cases`, `filings`) read and write PostgreSQL only,
//! a PostgreSQL failure is returned rather than falling back, and the
//! mirror stops copying SurrealDB rows over the authoritative ones. Without
//! a PostgreSQL host the SurrealDB path is the fallback. Parent rows the
//! tables name — the Project, Case, Notation, and Asset — stay on their own
//! authorities.
//!
//! Rows come back as the JSON object `to_jsonb` makes of them, so every
//! statement selects `to_jsonb(<alias>) AS row` and the owning module's own
//! serde type decodes it. `case` is a reserved word and always quoted.

use std::future::Future;
use std::pin::Pin;

use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::Row;

use crate::postgres::PostgresDb;

#[cfg(test)]
tokio::task_local! {
    static TEST_CASES_DB: PostgresDb;
}
/// Why PostgreSQL could not serve a litigation call. Carries no row values.
#[derive(Debug, thiserror::Error)]
pub enum CasesAuthorityError {
    #[error("PostgreSQL is unavailable for the litigation tables")]
    Unavailable,
    #[error("PostgreSQL refused the litigation operation: {0}")]
    Query(#[from] sqlx::Error),
    #[error("PostgreSQL returned an unreadable litigation row")]
    Row(#[from] serde_json::Error),
}

/// A litigation operation. Boxed so every owning module that awaits one
/// keeps a small future instead of embedding the PostgreSQL client's.
pub(crate) type Op<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, CasesAuthorityError>> + Send + 'a>>;

type Query<'q> = sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>;

/// The pool to serve from, or `None` when no PostgreSQL host is configured.
pub(crate) fn authority() -> Op<'static, PostgresDb> {
    Box::pin(async {
        #[cfg(test)]
        if let Ok(pg) = TEST_CASES_DB.try_with(Clone::clone) {
            return Ok(pg);
        }
        PostgresDb::application()
            .await
            .map_err(|_| CasesAuthorityError::Unavailable)
    })
}

pub(crate) fn decode<T: DeserializeOwned>(
    row: &sqlx::postgres::PgRow,
) -> Result<T, CasesAuthorityError> {
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
