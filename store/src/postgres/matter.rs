//! PostgreSQL authority for matter records: the Project-scoped rows the
//! generic records mirror carries, read and written in PostgreSQL by their
//! owning `store` modules.
//!
//! Opt-in per deployment through `NAVIGATOR_POSTGRES_MATTER_AUTHORITY`. When
//! enabled, each module in [`TABLES`] dispatches its reads and writes here
//! instead of SurrealDB, writes no SurrealDB row, and the records mirror
//! refuses to overwrite or delete these tables from SurrealDB (a missing
//! SurrealDB row is not deletion evidence after cutover). Parent rows the
//! modules check — the Project, Person, Entity, and Notation — stay on their
//! own authorities.
//!
//! The accessor is generic over the records mirror's table registry: a row
//! goes in and comes out as the JSON object `to_jsonb` and
//! `jsonb_populate_record` agree on, so each module states only its filters,
//! order, and the columns it writes. Table and column names are checked
//! against the registry before any statement is built.

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::postgres::PostgresDb;

/// Opt-in switch for the PostgreSQL-authoritative matter records.
pub const AUTHORITY_ENV: &str = "NAVIGATOR_POSTGRES_MATTER_AUTHORITY";

/// The tables whose owning module dispatches here when the switch is on.
pub(crate) const TABLES: &[&str] = &[
    "cap_table_holding",
    "estate_asset",
    "fiduciary_role",
    "git_repository",
    "notation_draft",
    "statutory_deadline",
];

#[cfg(test)]
tokio::task_local! {
    static TEST_MATTER_DB: PostgresDb;
}

/// Why a matter-record operation on PostgreSQL failed. Carries no row
/// values.
#[derive(Debug, thiserror::Error)]
pub enum MatterAuthorityError {
    #[error("PostgreSQL matter authority setting must be one of 0, 1, false, or true")]
    Configuration,
    #[error("PostgreSQL is unavailable for matter authority")]
    Unavailable,
    /// The write collided with the named unique index.
    #[error("the matter record collides with {0}")]
    Conflict(String),
    #[error("PostgreSQL refused the matter record operation")]
    Query(#[source] sqlx::Error),
    #[error("PostgreSQL returned an unreadable matter record")]
    Row(#[from] serde_json::Error),
    #[error("{table}.{column} is not a registered matter record column")]
    Unregistered { table: &'static str, column: String },
}

impl From<sqlx::Error> for MatterAuthorityError {
    fn from(error: sqlx::Error) -> Self {
        if let Some(database) = error.as_database_error() {
            if database.is_unique_violation() {
                if let Some(constraint) = database.constraint() {
                    return Self::Conflict(constraint.to_owned());
                }
            }
        }
        Self::Query(error)
    }
}

/// Strict parse, matching the other authority switches' accepted values.
pub(crate) fn enabled_from(setting: Option<&str>) -> Result<bool, MatterAuthorityError> {
    match setting {
        None | Some("0" | "false") => Ok(false),
        Some("1" | "true") => Ok(true),
        Some(_) => Err(MatterAuthorityError::Configuration),
    }
}

/// Whether this process selected matter authority. A misconfigured switch
/// is an error, so a mirror guard fails closed.
pub(crate) fn selected() -> Result<bool, MatterAuthorityError> {
    #[cfg(test)]
    if TEST_MATTER_DB.try_with(|_| ()).is_ok() {
        return Ok(true);
    }
    let setting = match std::env::var(AUTHORITY_ENV) {
        Ok(setting) => Some(setting),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => return Err(MatterAuthorityError::Configuration),
    };
    enabled_from(setting.as_deref())
}

/// A matter-record operation. Boxed so every owning module that awaits one
/// keeps a small future instead of embedding the PostgreSQL client's.
pub(crate) type Op<'a, T> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<T, MatterAuthorityError>> + Send + 'a>,
>;

/// The pool to dispatch to, or `None` when the deployment keeps SurrealDB.
pub(crate) fn authority() -> Op<'static, Option<PostgresDb>> {
    Box::pin(authority_now())
}

async fn authority_now() -> Result<Option<PostgresDb>, MatterAuthorityError> {
    #[cfg(test)]
    if let Ok(pg) = TEST_MATTER_DB.try_with(Clone::clone) {
        return Ok(Some(pg));
    }
    if !selected()? {
        return Ok(None);
    }
    PostgresDb::from_env()
        .await
        .map(Some)
        .map_err(|_| MatterAuthorityError::Unavailable)
}

/// Run `future` with matter authority selected on `pg`.
#[cfg(test)]
pub(crate) async fn with_test_db<T>(
    pg: PostgresDb,
    future: impl std::future::Future<Output = T>,
) -> T {
    TEST_MATTER_DB.scope(pg, future).await
}

/// The stored form of a union record link (`record<person|entity>`): the
/// tagged JSON the records mirror keeps in a `JSONB` column.
pub(crate) fn tagged(table: &str, id: Uuid) -> Value {
    serde_json::json!({"tb": table, "id": {"Uuid": id.to_string()}})
}

/// The table and UUID a [`tagged`] link names.
pub(crate) fn untagged(value: &Value) -> Option<(&str, Uuid)> {
    let table = value.get("tb")?.as_str()?;
    let id = value.get("id")?.get("Uuid")?.as_str()?.parse().ok()?;
    Some((table, id))
}

fn registered(
    table: &str,
) -> Result<(&'static str, &'static [&'static str]), MatterAuthorityError> {
    TABLES
        .iter()
        .find(|name| **name == table)
        .and_then(|name| crate::postgres_mirror::records::columns(name).map(|cols| (*name, cols)))
        .ok_or(MatterAuthorityError::Unregistered {
            table: "matter",
            column: table.to_owned(),
        })
}

fn column(
    table: &'static str,
    columns: &[&str],
    name: &str,
) -> Result<String, MatterAuthorityError> {
    if columns.contains(&name) {
        Ok(format!("\"{name}\""))
    } else {
        Err(MatterAuthorityError::Unregistered {
            table,
            column: name.to_owned(),
        })
    }
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(fields) => fields,
        _ => Map::new(),
    }
}

/// `r` matched against the typed record `f`: equality for a value, `IS NULL`
/// for an explicit null.
fn conditions(
    table: &'static str,
    columns: &[&str],
    filter: &Map<String, Value>,
) -> Result<String, MatterAuthorityError> {
    let mut clauses = Vec::with_capacity(filter.len());
    for (name, value) in filter {
        let quoted = column(table, columns, name)?;
        clauses.push(if value.is_null() {
            format!("r.{quoted} IS NULL")
        } else {
            format!("r.{quoted} = f.{quoted}")
        });
    }
    Ok(if clauses.is_empty() {
        "TRUE".to_owned()
    } else {
        clauses.join(" AND ")
    })
}

fn decode<T: DeserializeOwned>(row: &sqlx::postgres::PgRow) -> Result<T, MatterAuthorityError> {
    let value: Value = row.try_get("row")?;
    Ok(serde_json::from_value(value)?)
}

/// Rows of `table` matching `filter` (an object of column → value), ordered
/// by `order` (columns, each optionally suffixed ` DESC`).
pub(crate) fn select<'a, T: DeserializeOwned + Send + 'a>(
    pg: &'a PostgresDb,
    table: &'a str,
    filter: Value,
    order: &'a [&'a str],
) -> Op<'a, Vec<T>> {
    Box::pin(select_now(pg, table, filter, order))
}

async fn select_now<T: DeserializeOwned>(
    pg: &PostgresDb,
    table: &str,
    filter: Value,
    order: &[&str],
) -> Result<Vec<T>, MatterAuthorityError> {
    let (table, columns) = registered(table)?;
    let filter = object(filter);
    let mut sql = format!(
        "SELECT to_jsonb(r) AS row FROM \"{table}\" AS r, \
         jsonb_populate_record(NULL::\"{table}\", $1::jsonb) AS f WHERE {}",
        conditions(table, columns, &filter)?
    );
    if !order.is_empty() {
        let mut terms = Vec::with_capacity(order.len());
        for term in order {
            let (name, direction) = term
                .strip_suffix(" DESC")
                .map_or((*term, ""), |name| (name, " DESC"));
            terms.push(format!("r.{}{direction}", column(table, columns, name)?));
        }
        sql.push_str(" ORDER BY ");
        sql.push_str(&terms.join(", "));
    }
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(Value::Object(filter))
        .fetch_all(pg.pool())
        .await?;
    rows.iter().map(decode).collect()
}

/// The first row [`select`] would return.
pub(crate) fn find<'a, T: DeserializeOwned + Send + 'a>(
    pg: &'a PostgresDb,
    table: &'a str,
    filter: Value,
) -> Op<'a, Option<T>> {
    Box::pin(async move { Ok(select_now(pg, table, filter, &[]).await?.into_iter().next()) })
}

/// Insert one whole row, or replace every column of the row holding its
/// `id`, and return it as stored.
pub(crate) fn upsert<'a, T: DeserializeOwned + Send + 'a>(
    pg: &'a PostgresDb,
    table: &'a str,
    row: &impl serde::Serialize,
) -> Op<'a, T> {
    let row = serde_json::to_value(row);
    Box::pin(async move { upsert_now(pg, table, row?).await })
}

async fn upsert_now<T: DeserializeOwned>(
    pg: &PostgresDb,
    table: &str,
    row: Value,
) -> Result<T, MatterAuthorityError> {
    let (table, columns) = registered(table)?;
    let row = object(row);
    for name in row.keys() {
        column(table, columns, name)?;
    }
    let updates = columns[1..]
        .iter()
        .map(|name| format!("\"{name}\" = EXCLUDED.\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO \"{table}\" AS r SELECT * FROM jsonb_populate_record(NULL::\"{table}\", $1::jsonb) \
         ON CONFLICT (\"id\") DO UPDATE SET {updates} RETURNING to_jsonb(r) AS row"
    );
    let stored = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(Value::Object(row))
        .fetch_one(pg.pool())
        .await?;
    decode(&stored)
}

/// Delete the rows matching `filter`; return how many went.
pub(crate) fn delete<'a>(pg: &'a PostgresDb, table: &'a str, filter: Value) -> Op<'a, u64> {
    Box::pin(delete_now(pg, table, filter))
}

async fn delete_now(
    pg: &PostgresDb,
    table: &str,
    filter: Value,
) -> Result<u64, MatterAuthorityError> {
    let (table, columns) = registered(table)?;
    let filter = object(filter);
    let sql = format!(
        "DELETE FROM \"{table}\" AS r USING jsonb_populate_record(NULL::\"{table}\", $1::jsonb) AS f \
         WHERE {}",
        conditions(table, columns, &filter)?
    );
    Ok(sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(Value::Object(filter))
        .execute(pg.pool())
        .await?
        .rows_affected())
}

#[cfg(test)]
#[path = "matter_tests.rs"]
mod tests;
