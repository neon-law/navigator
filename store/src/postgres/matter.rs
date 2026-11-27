//! PostgreSQL authority for matter records: the Project-scoped rows the
//! generic records mirror carries, read and written in PostgreSQL by their
//! owning `store` modules.
//!
//! Wherever PostgreSQL is configured (`NAVIGATOR_POSTGRES_HOST`), each
//! module in [`TABLES`] dispatches its reads and writes here instead of
//! SurrealDB and writes no SurrealDB row, and the records mirror refuses to
//! overwrite or delete these tables from SurrealDB (a missing SurrealDB row
//! is not deletion evidence) outside the first Mirror Backfill (see
//! [`crate::postgres_mirror::initial_copy`]). SurrealDB serves a process
//! without PostgreSQL, which today means only the SurrealDB test harness.
//! Parent rows the modules check — the Project, Person, Entity, and Notation — stay on their
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

/// The tables whose owning module dispatches here.
pub(crate) const TABLES: &[&str] = &[
    "deployment_secret_change",
    "mailroom",
    "letter",
    "lead",
    "lead_consent",
    "project_deadlines",
    "project_service",
    "summary_sender_approval",
    "summary_sender_approval_event",
    "inbox_triage",
    "asset",
    "notation",
    "answer",
    "notation_event",
    "notation_clause",
    "notation_source",
    "notation_document",
    "notation_document_version",
    "cap_table_holding",
    "estate_asset",
    "expunge_record",
    "expunge_request",
    "playbook",
    "contract_review",
    "attestation",
    "authority_use",
    "citation",
    "project_module",
    "efiling_transactions",
    "social_oauth_attempt",
    "testimonial",
    "git_access_token",
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
    #[error("PostgreSQL is unavailable for matter authority")]
    Unavailable,
    /// The family historical import has not been activated.
    #[error(transparent)]
    Family(#[from] crate::postgres::history::FamilyError),
    /// The write collided with the named unique index.
    #[error("the matter record collides with {0}")]
    Conflict(String),
    #[error("PostgreSQL refused the matter record operation")]
    Query(#[source] sqlx::Error),
    #[error("PostgreSQL authority schema is invalid")]
    Schema,
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

/// A matter-record operation. Boxed so every owning module that awaits one
/// keeps a small future instead of embedding the PostgreSQL client's.
pub(crate) type Op<'a, T> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<T, MatterAuthorityError>> + Send + 'a>,
>;

/// The pool to dispatch to, or `None` in a process without PostgreSQL.
pub(crate) fn authority() -> Op<'static, PostgresDb> {
    Box::pin(authority_now())
}

/// Select a table's PostgreSQL authority only after its exact import receipt.
pub(crate) fn authority_for(table: &'static str) -> Op<'static, PostgresDb> {
    Box::pin(async move {
        let pg = authority_now().await?;
        let family = crate::postgres::history::Family::for_table(table)
            .ok_or(crate::postgres::history::FamilyError::Receipt)?;
        crate::postgres::history::require_scope(&pg, family).await?;
        Ok(pg)
    })
}

async fn authority_now() -> Result<PostgresDb, MatterAuthorityError> {
    #[cfg(test)]
    if let Ok(pg) = TEST_MATTER_DB.try_with(Clone::clone) {
        return Ok(pg);
    }
    PostgresDb::application()
        .await
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

/// Remove one Project's citation apparatus atomically, deepest first.
pub(crate) async fn delete_authority_uses(
    pg: &PostgresDb,
    project: Uuid,
) -> Result<(), MatterAuthorityError> {
    let mut tx = pg.pool().begin().await?;
    sqlx::query("DELETE FROM verification WHERE citation_id IN (SELECT c.id FROM citation c JOIN authority_use u ON u.id = c.authority_use_id WHERE u.project_id = $1)")
        .bind(project).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM citation WHERE authority_use_id IN (SELECT id FROM authority_use WHERE project_id = $1)")
        .bind(project).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM authority_use WHERE project_id = $1")
        .bind(project)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Change the capability ledger and its audit trail in the same transaction.
pub(crate) async fn toggle_module(
    pg: &PostgresDb,
    project: Uuid,
    module: crate::project_modules::Module,
    actor: Option<Uuid>,
    enable: bool,
) -> Result<Option<crate::project_modules::ProjectModule>, crate::project_modules::ModuleError> {
    use crate::project_modules::ModuleError;
    async fn run(
        pg: &PostgresDb,
        project: Uuid,
        module: crate::project_modules::Module,
        actor: Option<Uuid>,
        enable: bool,
    ) -> Result<Option<crate::project_modules::ProjectModule>, MatterAuthorityError> {
        let mut tx = pg.pool().begin().await?;
        sqlx::query("SELECT id FROM project WHERE id = $1 FOR UPDATE")
            .bind(project)
            .fetch_optional(&mut *tx)
            .await?;
        let row = if enable {
            sqlx::query("INSERT INTO project_module(id, project_id, module, enabled_at, enabled_by_person_id, inserted_at, updated_at) VALUES ($1, $2, $3, now(), $4, now(), now()) ON CONFLICT (project_id, module) DO NOTHING RETURNING to_jsonb(project_module) AS row")
                .bind(Uuid::now_v7()).bind(project).bind(module.as_str()).bind(actor).fetch_optional(&mut *tx).await?
        } else {
            sqlx::query("DELETE FROM project_module WHERE project_id = $1 AND module = $2 RETURNING to_jsonb(project_module) AS row")
                .bind(project).bind(module.as_str()).fetch_optional(&mut *tx).await?
        };
        if row.is_some() {
            crate::relationship_logs::record_on(
                &mut *tx,
                Uuid::now_v7(),
                &crate::relationship_logs::NewRelationshipLog {
                    actor_person_id: actor,
                    subject_type: "project".to_owned(),
                    subject_id: project,
                    action: if enable {
                        "module_enabled"
                    } else {
                        "module_disabled"
                    }
                    .to_owned(),
                    detail: module.as_str().to_owned(),
                    project_id: Some(project),
                },
            )
            .await?;
        }
        let row = if enable && row.is_none() {
            sqlx::query("SELECT to_jsonb(project_module) AS row FROM project_module WHERE project_id = $1 AND module = $2")
                .bind(project).bind(module.as_str()).fetch_optional(&mut *tx).await?
        } else {
            row
        };
        let result = row.as_ref().map(decode).transpose()?;
        tx.commit().await?;
        Ok(result)
    }
    if enable {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM project WHERE id = $1)")
                .bind(project)
                .fetch_one(pg.pool())
                .await
                .map_err(MatterAuthorityError::from)?;
        if !exists {
            return Err(crate::projects::ProjectStoreError::NoSuchProject(project).into());
        }
    }
    run(pg, project, module, actor, enable)
        .await
        .map_err(ModuleError::from)
}

/// Claim one unexpired OAuth capability and erase its verifier atomically.
pub(crate) async fn consume_oauth(
    pg: &PostgresDb,
    hash: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<Value>, MatterAuthorityError> {
    Ok(sqlx::query_scalar("WITH claimed AS MATERIALIZED (SELECT * FROM social_oauth_attempt WHERE state_hash = $1 AND consumed_at IS NULL AND expires_at > $2 FOR UPDATE) UPDATE social_oauth_attempt AS s SET consumed_at = $2, pkce_verifier = '', provider_account_id = NULL, instagram_account_id = NULL FROM claimed WHERE s.id = claimed.id RETURNING to_jsonb(claimed)")
        .bind(hash).bind(now).fetch_optional(pg.pool()).await?)
}

/// Public consented testimonials, scoped through their Project's Brand.
pub(crate) async fn published_testimonials(
    pg: &PostgresDb,
    brand: Option<&str>,
    limit: Option<u64>,
) -> Result<Vec<crate::testimonials::PublishedTestimonial>, MatterAuthorityError> {
    let limit = limit.map(|limit| i64::try_from(limit).unwrap_or(i64::MAX));
    let rows = sqlx::query("SELECT t.id, t.quote, t.attribution_label FROM testimonial t JOIN project p ON p.id = t.project_id WHERE t.consented_at IS NOT NULL AND t.published_at IS NOT NULL AND ($1::text IS NULL OR p.brand = $1) ORDER BY t.display_order, t.published_at DESC LIMIT $2")
        .bind(brand).bind(limit).fetch_all(pg.pool()).await?;
    rows.into_iter()
        .map(|row| {
            Ok(crate::testimonials::PublishedTestimonial {
                id: row.try_get("id")?,
                quote: row.try_get("quote")?,
                attribution_label: row.try_get("attribution_label")?,
            })
        })
        .collect()
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
        .and_then(|name| crate::postgres::record_metadata::columns(name).map(|cols| (*name, cols)))
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

fn canonical_columns(
    table: &str,
) -> Result<&'static std::collections::BTreeMap<String, String>, MatterAuthorityError> {
    static SCHEMA: std::sync::LazyLock<
        Result<
            std::collections::BTreeMap<String, crate::postgres_schema::Table>,
            crate::postgres_schema::Error,
        >,
    > = std::sync::LazyLock::new(|| {
        crate::postgres_schema::parse_schema(crate::postgres_schema::canonical_schema())
    });
    SCHEMA
        .as_ref()
        .map_err(|_| MatterAuthorityError::Schema)?
        .get(table)
        .map(|table| &table.columns)
        .ok_or(MatterAuthorityError::Schema)
}

pub(crate) fn json_column(table: &str, column: &str) -> Result<bool, MatterAuthorityError> {
    Ok(canonical_columns(table)?
        .get(column)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("jsonb")))
}

/// Project row alias `r` without conflating SQL NULL and explicit JSONB null.
pub(crate) fn row_json(table: &str) -> Result<String, MatterAuthorityError> {
    let mut projection = "to_jsonb(r)".to_owned();
    for (column, kind) in canonical_columns(table)? {
        if kind.eq_ignore_ascii_case("jsonb") {
            let quoted = column.replace('"', "\"\"");
            let key = column.replace('\'', "''");
            projection = format!(
                "({projection} - CASE WHEN r.\"{quoted}\" IS NULL THEN '{key}' ELSE '' END)"
            );
        }
    }
    Ok(projection)
}

/// A present JSON field remains present even when its value is JSON null.
pub(crate) fn present_json<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    <Value as serde::Deserialize>::deserialize(deserializer).map(Some)
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
        clauses.push(if json_column(table, name)? {
            format!("r.{quoted} = $1::jsonb -> '{name}'")
        } else if value.is_null() {
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
    Box::pin(select_now(pg, table, filter, None, order))
}

/// [`select`] narrowed to rows whose UUID `column` is any of `ids`: one
/// statement for many parents instead of one per parent.
pub(crate) fn select_any<'a, T: DeserializeOwned + Send + 'a>(
    pg: &'a PostgresDb,
    table: &'a str,
    filter: Value,
    any: (&'a str, &'a [Uuid]),
    order: &'a [&'a str],
) -> Op<'a, Vec<T>> {
    Box::pin(select_now(pg, table, filter, Some(any), order))
}

async fn select_now<T: DeserializeOwned>(
    pg: &PostgresDb,
    table: &str,
    filter: Value,
    any: Option<(&str, &[Uuid])>,
    order: &[&str],
) -> Result<Vec<T>, MatterAuthorityError> {
    let (table, columns) = registered(table)?;
    let filter = object(filter);
    let projection = row_json(table)?;
    let mut clauses = conditions(table, columns, &filter)?;
    if let Some((name, _)) = any {
        clauses = format!(
            "{clauses} AND r.{} = ANY($2::uuid[])",
            column(table, columns, name)?
        );
    }
    let mut sql = format!(
        "SELECT {projection} AS row FROM \"{table}\" AS r, \
         jsonb_populate_record(NULL::\"{table}\", $1::jsonb) AS f WHERE {clauses}"
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
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(Value::Object(filter));
    if let Some((_, ids)) = any {
        query = query.bind(ids.to_vec());
    }
    let rows = query.fetch_all(pg.pool()).await?;
    rows.iter().map(decode).collect()
}

/// The first row [`select`] would return.
pub(crate) fn find<'a, T: DeserializeOwned + Send + 'a>(
    pg: &'a PostgresDb,
    table: &'a str,
    filter: Value,
) -> Op<'a, Option<T>> {
    Box::pin(async move {
        Ok(select_now(pg, table, filter, None, &[])
            .await?
            .into_iter()
            .next())
    })
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
    upsert_on(pg.pool(), table, row).await
}

/// Write a whole record in the caller's PostgreSQL transaction.
pub(crate) async fn upsert_on<'e, T: DeserializeOwned>(
    executor: impl sqlx::PgExecutor<'e>,
    table: &str,
    row: Value,
) -> Result<T, MatterAuthorityError> {
    write_record_on(executor, table, row, false)
        .await?
        .ok_or_else(|| sqlx::Error::RowNotFound.into())
}

/// Insert an immutable identity without replacing a concurrent existing row.
pub(crate) async fn insert_on<'e, T: DeserializeOwned>(
    executor: impl sqlx::PgExecutor<'e>,
    table: &str,
    row: Value,
) -> Result<Option<T>, MatterAuthorityError> {
    write_record_on(executor, table, row, true).await
}

async fn write_record_on<'e, T: DeserializeOwned>(
    executor: impl sqlx::PgExecutor<'e>,
    table: &str,
    row: Value,
    preserve_existing: bool,
) -> Result<Option<T>, MatterAuthorityError> {
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
    let values = columns
        .iter()
        .map(|name| {
            let quoted = column(table, columns, name)?;
            Ok(if json_column(table, name)? {
                format!("$1::jsonb -> '{name}'")
            } else {
                format!("f.{quoted}")
            })
        })
        .collect::<Result<Vec<_>, MatterAuthorityError>>()?
        .join(", ");
    let names = columns
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let projection = row_json(table)?;
    let conflict = if preserve_existing {
        "DO NOTHING".to_owned()
    } else {
        format!("DO UPDATE SET {updates}")
    };
    let sql = format!(
        "INSERT INTO \"{table}\" AS r ({names}) SELECT {values} FROM jsonb_populate_record(NULL::\"{table}\", $1::jsonb) AS f \
         ON CONFLICT (\"id\") {conflict} RETURNING {projection} AS row"
    );
    let stored = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(Value::Object(row))
        .fetch_optional(executor)
        .await?;
    stored.as_ref().map(decode).transpose()
}

/// Atomically patch the matching rows without replacing unrelated columns.
pub(crate) fn update<'a, T: DeserializeOwned + Send + 'a>(
    pg: &'a PostgresDb,
    table: &'a str,
    filter: Value,
    changes: Value,
) -> Op<'a, Vec<T>> {
    Box::pin(async move {
        let (table, columns) = registered(table)?;
        let filter = object(filter);
        let changes = object(changes);
        let mut assignments = Vec::with_capacity(changes.len());
        for name in changes.keys() {
            let quoted = column(table, columns, name)?;
            if name == "id" {
                return Err(MatterAuthorityError::Unregistered {
                    table,
                    column: name.clone(),
                });
            }
            assignments.push(if json_column(table, name)? {
                format!("{quoted} = $2::jsonb -> '{name}'")
            } else {
                format!("{quoted} = c.{quoted}")
            });
        }
        if assignments.is_empty() {
            return select_now(pg, table, Value::Object(filter), None, &[]).await;
        }
        let projection = row_json(table)?;
        let sql = format!(
            "UPDATE \"{table}\" AS r SET {} FROM \
             jsonb_populate_record(NULL::\"{table}\", $1::jsonb) AS f, \
             jsonb_populate_record(NULL::\"{table}\", $2::jsonb) AS c \
             WHERE {} RETURNING {projection} AS row",
            assignments.join(", "),
            conditions(table, columns, &filter)?
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(Value::Object(filter))
            .bind(Value::Object(changes))
            .fetch_all(pg.pool())
            .await?;
        rows.iter().map(decode).collect()
    })
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
pub(crate) mod tests;

#[cfg(test)]
#[path = "matter_satellite_tests.rs"]
mod satellite_tests;
