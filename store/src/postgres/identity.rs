//! PostgreSQL authority for what a Person or an Entity holds beside its own
//! row: Address, Credential, Email Token, Entity, Person Delegate, Person
//! External Identity, and Person Phone rows.
//!
//! `NAVIGATOR_POSTGRES_PEOPLE_AUTHORITY` selects these tables together with
//! the People directory ([`crate::persons`]). Under it each accessor reads and
//! writes PostgreSQL and returns a configured PostgreSQL failure rather than
//! falling back. A committed write is then projected into SurrealDB, so
//! records that still live there keep resolving their links; [`project`]
//! copies the committed row, or removes the copy of a deleted one.

use chrono::{DateTime, Utc};
use sqlx::{postgres::PgRow, AssertSqlSafe, Row};
use surrealdb::types::{Datetime, RecordId};
use uuid::Uuid;

use crate::postgres::PostgresDb;
use crate::surreal::{record_id, SurrealDb};

/// Why a PostgreSQL identity operation failed.
#[derive(Debug, thiserror::Error)]
pub enum AuthorityError {
    /// The authority setting is invalid or PostgreSQL is unavailable.
    #[error("PostgreSQL identity authority: {0}")]
    Unavailable(String),
    /// A PostgreSQL query failed.
    #[error("PostgreSQL identity query failed: {0}")]
    Query(#[from] sqlx::Error),
    /// PostgreSQL committed the change, but its SurrealDB compatibility
    /// projection failed. Repeating the write repairs it.
    #[error(
        "PostgreSQL committed the identity change, but the compatibility projection failed: {0}"
    )]
    Projection(String),
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

/// How one PostgreSQL column is spelled in its SurrealDB projection.
#[derive(Clone, Copy)]
pub(crate) enum Field {
    /// A UUID that SurrealDB holds as a record link into the named table.
    Link(&'static str),
    Text,
    Bool,
    Time,
    /// A timestamp SurrealDB stores as RFC 3339 text.
    TimeText,
    Json,
}

/// One table's projection: its columns, and SurrealQL that keeps a claim
/// table beside the row in step.
pub(crate) struct Projection {
    pub table: &'static str,
    pub fields: &'static [(&'static str, Field)],
    /// Runs before the row is written or removed, bound to `$id`.
    pub release: &'static str,
    /// Runs after the row is written, bound to `$id` and every field.
    pub claim: &'static str,
}

const STAMPS: [(&str, Field); 2] = [("inserted_at", Field::Time), ("updated_at", Field::Time)];

macro_rules! fields {
    ($($name:literal => $field:expr),* $(,)?) => {
        &[$(($name, $field)),*, STAMPS[0], STAMPS[1]]
    };
}

pub(crate) const ADDRESS: Projection = Projection {
    table: "address",
    fields: fields![
        "person_id" => Field::Link("person"),
        "entity_id" => Field::Link("entity"),
        "line1" => Field::Text,
        "line2" => Field::Text,
        "city" => Field::Text,
        "region" => Field::Text,
        "postal_code" => Field::Text,
        "country" => Field::Text,
    ],
    release: "",
    claim: "",
};

pub(crate) const CREDENTIAL: Projection = Projection {
    table: "credential",
    fields: fields![
        "person_id" => Field::Link("person"),
        "jurisdiction_id" => Field::Link("jurisdiction"),
        "license_number" => Field::Text,
    ],
    release: "",
    claim: "",
};

pub(crate) const EMAIL_TOKEN: Projection = Projection {
    table: "email_token",
    fields: fields![
        "person_id" => Field::Link("person"),
        "email" => Field::Text,
        "purpose" => Field::Text,
        "token_hash" => Field::Text,
        "expires_at" => Field::Time,
        "used_at" => Field::Time,
    ],
    release: "",
    claim: "",
};

pub(crate) const ENTITY: Projection = Projection {
    table: "entity",
    fields: fields![
        "name" => Field::Text,
        "entity_type_id" => Field::Link("entity_type"),
        "jurisdiction_id" => Field::Link("jurisdiction"),
        "phone" => Field::Text,
        "url" => Field::Text,
        "avatar_url" => Field::Text,
        "xero_id" => Field::Text,
        "metadata" => Field::Json,
        "firm_anchor_key" => Field::Text,
    ],
    release: "DELETE firm_anchor WHERE entity_id = $id;",
    claim: "IF $firm_anchor_key != NONE { \
            UPSERT type::record('firm_anchor', $firm_anchor_key) SET entity_id = $id; };",
};

pub(crate) const PERSON_DELEGATE: Projection = Projection {
    table: "person_delegate",
    fields: &[
        ("delegate_person_id", Field::Link("person")),
        ("subject_person_id", Field::Link("person")),
        ("basis", Field::Text),
        ("instrument_reference", Field::Text),
        ("granted_by_person_id", Field::Link("person")),
        ("granted_at", Field::TimeText),
        ("subject_notified_at", Field::TimeText),
        ("subject_notified_channel", Field::Text),
        ("revoked_at", Field::TimeText),
        ("revoked_by_person_id", Field::Link("person")),
        ("inserted_at", Field::TimeText),
        ("updated_at", Field::TimeText),
    ],
    release: "",
    claim: "",
};

pub(crate) const EXTERNAL_IDENTITY: Projection = Projection {
    table: "person_external_identity",
    fields: fields![
        "person_id" => Field::Link("person"),
        "system" => Field::Text,
        "external_id" => Field::Text,
        "handle" => Field::Text,
    ],
    release: "",
    claim: "",
};

pub(crate) const PERSON_PHONE: Projection = Projection {
    table: "person_phone",
    fields: fields![
        "person_id" => Field::Link("person"),
        "e164" => Field::Text,
        "label" => Field::Text,
        "is_primary" => Field::Bool,
    ],
    release: "",
    claim: "",
};

/// One decoded column, ready to bind into SurrealQL.
enum Value {
    Link(Option<RecordId>),
    Text(Option<String>),
    Bool(Option<bool>),
    Time(Option<Datetime>),
    Json(Option<serde_json::Value>),
}

fn decode(row: &PgRow, name: &str, field: Field) -> Result<Value, sqlx::Error> {
    Ok(match field {
        Field::Link(table) => Value::Link(
            row.try_get::<Option<Uuid>, _>(name)?
                .map(|id| record_id(table, id)),
        ),
        Field::Text => Value::Text(row.try_get(name)?),
        Field::Bool => Value::Bool(row.try_get(name)?),
        Field::Time => Value::Time(
            row.try_get::<Option<DateTime<Utc>>, _>(name)?
                .map(Datetime::from),
        ),
        Field::TimeText => Value::Text(
            row.try_get::<Option<DateTime<Utc>>, _>(name)?
                .map(|at| at.to_rfc3339()),
        ),
        Field::Json => Value::Json(row.try_get(name)?),
    })
}

/// Copy the committed PostgreSQL row `id` of `projection` into SurrealDB,
/// or remove SurrealDB's copy when PostgreSQL holds none.
///
/// A per-row advisory lock serializes projections, and each one reads the
/// row current when it holds the lock, so the last projection to run
/// always writes the latest committed state.
///
/// # Errors
///
/// [`AuthorityError::Query`] when PostgreSQL cannot be read and
/// [`AuthorityError::Projection`] when SurrealDB refuses the copy.
pub(crate) async fn project(
    pg: &PostgresDb,
    db: &SurrealDb,
    projection: &Projection,
    id: Uuid,
) -> Result<(), AuthorityError> {
    let table = projection.table;
    let mut tx = pg.0.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1), hashtext($2::text))")
        .bind(table)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let columns = projection
        .fields
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ");
    let row = sqlx::query(AssertSqlSafe(format!(
        "SELECT {columns} FROM {table} WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let release = projection.release;
    let mut values = Vec::new();
    let statement = if let Some(row) = row {
        for (name, field) in projection.fields {
            values.push((*name, decode(&row, name, *field)?));
        }
        let set = projection
            .fields
            .iter()
            .map(|(name, _)| format!("{name} = ${name}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{release} UPSERT $id SET {set}; {}", projection.claim)
    } else {
        format!("{release} DELETE $id;")
    };
    let mut query = db.query(statement).bind(("id", record_id(table, id)));
    for (name, value) in values {
        query = match value {
            Value::Link(value) => query.bind((name, value)),
            Value::Text(value) => query.bind((name, value)),
            Value::Bool(value) => query.bind((name, value)),
            Value::Time(value) => query.bind((name, value)),
            Value::Json(value) => query.bind((name, value)),
        };
    }
    query
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|error| AuthorityError::Projection(format!("{table}: {error}")))?;
    tx.commit().await?;
    Ok(())
}

/// Whether `error` is PostgreSQL refusing a write on the named unique
/// constraint or index.
pub(crate) fn violates(error: &sqlx::Error, constraint: &str) -> bool {
    matches!(error, sqlx::Error::Database(database) if database.constraint() == Some(constraint))
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
