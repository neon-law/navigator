//! The shared Clause library's PostgreSQL read model.
//!
//! `rules/clauses/library.yaml` is the source of truth; the `clause` table is
//! the read model built from it, versioned by append. A pinned `code@version`
//! is never rewritten: a change adds a version, and `is_current` marks the
//! newest active version of each code. Retired versions remain readable. The table is a shared catalog, owned by no
//! Firm or Project. `navigator-migrate` seeds it after applying migrations;
//! without `NAVIGATOR_POSTGRES_HOST` the seed is skipped and callers read the
//! embedded library through `rules::clauses`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use rules::clauses::LibraryClause;
use sqlx::Row;
use uuid::Uuid;

use crate::postgres::{PostgresDb, PostgresError};

/// One stored version of a library Clause.
#[derive(Debug, Clone, PartialEq)]
pub struct Clause {
    pub id: Uuid,
    pub code: String,
    pub version: i32,
    pub kind: String,
    pub inputs: serde_json::Value,
    pub body: String,
    pub body_sha256: String,
    pub is_current: bool,
    pub retired_at: Option<DateTime<Utc>>,
    pub inserted_at: DateTime<Utc>,
}

/// What one seed wrote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeedReport {
    /// Library versions added as new rows.
    pub inserted: usize,
    /// Codes whose current version moved.
    pub current_changed: usize,
    /// Existing versions retired without changing their content.
    pub retired: usize,
}

impl SeedReport {
    /// True when the library already matched the table and nothing was written.
    #[must_use]
    pub const fn is_unchanged(&self) -> bool {
        self.inserted == 0 && self.current_changed == 0 && self.retired == 0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClauseError {
    #[error(transparent)]
    Postgres(#[from] PostgresError),
    #[error("the bundled Clause library is invalid: {0}")]
    Library(String),
    /// A stored version's kind, inputs or body differs from the library.
    #[error(
        "Clause `{code}@{version}` differs from its stored version; add a new version instead"
    )]
    ChangedVersion { code: String, version: i32 },
    #[error("PostgreSQL Clause query failed")]
    Query,
}

const COLUMNS: &str =
    "id, code, version, kind, inputs, body, body_sha256, is_current, retired_at, inserted_at";

fn query_failed(_: sqlx::Error) -> ClauseError {
    ClauseError::Query
}

fn clause(row: &sqlx::postgres::PgRow) -> Result<Clause, sqlx::Error> {
    Ok(Clause {
        id: row.try_get("id")?,
        code: row.try_get("code")?,
        version: row.try_get("version")?,
        kind: row.try_get("kind")?,
        inputs: row.try_get("inputs")?,
        body: row.try_get("body")?,
        body_sha256: row.try_get("body_sha256")?,
        is_current: row.try_get("is_current")?,
        retired_at: row.try_get("retired_at")?,
        inserted_at: row.try_get("inserted_at")?,
    })
}

/// Every stored version, ordered by code and version.
///
/// # Errors
/// Returns a query error.
pub async fn list(db: &PostgresDb) -> Result<Vec<Clause>, ClauseError> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM clause ORDER BY code, version"
    )))
    .fetch_all(&db.0)
    .await
    .map_err(query_failed)?
    .iter()
    .map(clause)
    .collect::<Result<_, _>>()
    .map_err(query_failed)
}

/// One pinned version, `code@version`.
///
/// # Errors
/// Returns a query error.
pub async fn get(db: &PostgresDb, code: &str, version: i32) -> Result<Option<Clause>, ClauseError> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM clause WHERE code = $1 AND version = $2"
    )))
    .bind(code)
    .bind(version)
    .fetch_optional(&db.0)
    .await
    .map_err(query_failed)?
    .as_ref()
    .map(clause)
    .transpose()
    .map_err(query_failed)
}

/// The current version of `code`.
///
/// # Errors
/// Returns a query error.
pub async fn current(db: &PostgresDb, code: &str) -> Result<Option<Clause>, ClauseError> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM clause WHERE code = $1 AND is_current"
    )))
    .bind(code)
    .fetch_optional(&db.0)
    .await
    .map_err(query_failed)?
    .as_ref()
    .map(clause)
    .transpose()
    .map_err(query_failed)
}

/// One Clause version as the catalog readers see it, whichever source
/// supplied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogClause {
    pub retired_at: Option<DateTime<Utc>>,
    pub code: String,
    pub version: i32,
    pub kind: String,
    pub inputs: BTreeMap<String, rules::clauses::Input>,
    pub body: String,
    /// The newest active version of its code.
    pub is_current: bool,
}

/// Every Clause version, ordered by code and version: the PostgreSQL read
/// model when `NAVIGATOR_POSTGRES_HOST` names a reachable server, otherwise the
/// embedded library. The single read path for the CLI and the lawyer-tier Clauses
/// page.
///
/// # Errors
/// Returns a PostgreSQL configuration or query error, or an invalid embedded
/// library. An unreachable server falls back to the embedded library.
pub async fn catalog() -> Result<Vec<CatalogClause>, ClauseError> {
    if std::env::var_os("NAVIGATOR_POSTGRES_HOST").is_some() {
        match PostgresDb::from_env().await {
            Ok(db) => {
                return list(&db)
                    .await?
                    .into_iter()
                    .map(|clause| {
                        Ok(CatalogClause {
                            retired_at: clause.retired_at,
                            inputs: serde_json::from_value(clause.inputs)
                                .map_err(|error| ClauseError::Library(error.to_string()))?,
                            code: clause.code,
                            version: clause.version,
                            kind: clause.kind,
                            body: clause.body,
                            is_current: clause.is_current,
                        })
                    })
                    .collect();
            }
            Err(PostgresError::Connection) => {}
            Err(error) => return Err(error.into()),
        }
    }
    let library = rules::clauses::library_clauses().map_err(ClauseError::Library)?;
    let mut newest = BTreeMap::<String, i32>::new();
    for clause in library.iter().filter(|clause| clause.retired_at.is_none()) {
        let entry = newest.entry(clause.code.clone()).or_insert(clause.version);
        *entry = (*entry).max(clause.version);
    }
    Ok(library
        .into_iter()
        .map(|clause| CatalogClause {
            retired_at: clause.retired_at,
            is_current: newest.get(&clause.code) == Some(&clause.version),
            code: clause.code,
            version: clause.version,
            kind: clause.kind,
            inputs: clause.inputs,
            body: clause.body,
        })
        .collect())
}

/// Lowercase hex SHA-256 of a Clause body, as the table's check computes it.
#[must_use]
pub fn body_sha256(body: &str) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let mut out = String::with_capacity(64);
    for byte in Sha256::digest(body.as_bytes()) {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Upsert `library` into the table in one transaction. New versions are
/// appended, retirement metadata is applied, the newest active version becomes current, and an
/// unchanged library writes nothing. A stored version whose content differs
/// is refused rather than overwritten.
///
/// # Errors
/// Returns a changed pinned version or a query error; nothing is committed.
pub async fn seed(db: &PostgresDb, library: &[LibraryClause]) -> Result<SeedReport, ClauseError> {
    let mut report = SeedReport::default();
    let mut newest = BTreeMap::<&str, i32>::new();
    let mut tx = db.0.begin().await.map_err(query_failed)?;
    for source in library {
        let inputs = serde_json::to_value(&source.inputs)
            .map_err(|error| ClauseError::Library(error.to_string()))?;
        let digest = body_sha256(&source.body);
        let inserted = sqlx::query(
            "INSERT INTO clause (code, version, kind, inputs, body, body_sha256, retired_at) VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (code, version) DO NOTHING",
        )
        .bind(&source.code)
        .bind(source.version)
        .bind(&source.kind)
        .bind(&inputs)
        .bind(&source.body)
        .bind(&digest)
        .bind(source.retired_at)
        .execute(&mut *tx)
        .await
        .map_err(query_failed)?
        .rows_affected();
        if inserted == 0 {
            let same: bool = sqlx::query_scalar(
                "SELECT kind = $3 AND inputs = $4 AND body_sha256 = $5 AND body = $6 FROM clause WHERE code = $1 AND version = $2",
            )
            .bind(&source.code)
            .bind(source.version)
            .bind(&source.kind)
            .bind(&inputs)
            .bind(&digest)
            .bind(&source.body)
            .fetch_one(&mut *tx)
            .await
            .map_err(query_failed)?;
            if !same {
                return Err(ClauseError::ChangedVersion {
                    code: source.code.clone(),
                    version: source.version,
                });
            }
        }
        report.inserted += usize::try_from(inserted).unwrap_or(usize::MAX);
        if let Some(retired_at) = source.retired_at {
            let retired = sqlx::query(
                "UPDATE clause SET retired_at = $3, is_current = false WHERE code = $1 AND version = $2 AND retired_at IS NULL",
            )
            .bind(&source.code)
            .bind(source.version)
            .bind(retired_at)
            .execute(&mut *tx)
            .await
            .map_err(query_failed)?
            .rows_affected();
            report.retired += usize::try_from(retired).unwrap_or(usize::MAX);
        } else {
            let entry = newest.entry(source.code.as_str()).or_insert(source.version);
            *entry = (*entry).max(source.version);
        }
    }
    for (code, version) in newest {
        let cleared = sqlx::query(
            "UPDATE clause SET is_current = false WHERE code = $1 AND is_current AND version <> $2",
        )
        .bind(code)
        .bind(version)
        .execute(&mut *tx)
        .await
        .map_err(query_failed)?
        .rows_affected();
        let marked = sqlx::query(
            "UPDATE clause SET is_current = true WHERE code = $1 AND version = $2 AND NOT is_current AND retired_at IS NULL",
        )
        .bind(code)
        .bind(version)
        .execute(&mut *tx)
        .await
        .map_err(query_failed)?
        .rows_affected();
        if cleared + marked > 0 {
            report.current_changed += 1;
        }
    }
    tx.commit().await.map_err(query_failed)?;
    Ok(report)
}

/// Seed the bundled library through the configured operator connection.
/// Returns `None` without connecting when `NAVIGATOR_POSTGRES_HOST` is unset.
///
/// # Errors
/// Returns a configuration, connection, library or seed error.
pub async fn seed_library_from_env() -> Result<Option<SeedReport>, ClauseError> {
    if std::env::var_os("NAVIGATOR_POSTGRES_HOST").is_none() {
        return Ok(None);
    }
    let library = rules::clauses::library_clauses().map_err(ClauseError::Library)?;
    let db = PostgresDb::from_env().await?;
    seed(&db, &library).await.map(Some)
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn the_embedded_library_answers_when_no_postgres_is_configured() {
        if std::env::var_os("NAVIGATOR_POSTGRES_HOST").is_some() {
            return;
        }
        let catalog = super::catalog().await.unwrap();
        let versions: Vec<_> = catalog
            .iter()
            .filter(|clause| clause.code == "nevada_engagement_arbitration")
            .map(|clause| {
                (
                    clause.version,
                    clause.is_current,
                    clause.retired_at.is_some(),
                )
            })
            .collect();
        assert_eq!(versions, [(1, false, true), (2, true, false)]);
        let governing = catalog
            .iter()
            .find(|clause| clause.code == "governing_law")
            .unwrap();
        assert!(governing.retired_at.is_some());
        assert!(!governing.is_current);
        assert!(catalog
            .iter()
            .all(|clause| !clause.body.is_empty() && !clause.kind.is_empty()));
    }
}
