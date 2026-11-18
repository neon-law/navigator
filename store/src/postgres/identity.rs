//! PostgreSQL authority for what a Person or an Entity holds beside its own
//! row: Address, Credential, Email Token, Entity, Person Delegate, Person
//! External Identity, and Person Phone rows.
//!
//! These accessors share PostgreSQL identity authority and never project writes
//! into a compatibility database.

use crate::postgres::PostgresDb;

/// Why a PostgreSQL identity operation failed.
#[derive(Debug, thiserror::Error)]
pub enum AuthorityError {
    /// The authority setting is invalid or PostgreSQL is unavailable.
    #[error("PostgreSQL identity authority: {0}")]
    Unavailable(String),
    /// A PostgreSQL query failed.
    #[error("PostgreSQL identity query failed: {0}")]
    Query(#[from] sqlx::Error),
}

/// The PostgreSQL pool when People authority is configured, `None` when the
/// SurrealDB path stays in force.
///
/// # Errors
///
/// [`AuthorityError::Unavailable`] when the setting is invalid or PostgreSQL
/// cannot be reached.
pub(crate) async fn authority() -> Result<Option<PostgresDb>, AuthorityError> {
    crate::persons::postgres_authority()
        .await
        .map_err(|error| AuthorityError::Unavailable(error.to_string()))
}

/// Whether `error` is PostgreSQL refusing a write on the named unique
/// constraint or index.
pub(crate) fn violates(error: &sqlx::Error, constraint: &str) -> bool {
    matches!(error, sqlx::Error::Database(database) if database.constraint() == Some(constraint))
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
