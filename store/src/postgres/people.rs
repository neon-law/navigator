//! PostgreSQL operations for the People directory.
//!
//! The opt-in People authority path uses the same UUIDs and role vocabulary
//! as the source store. Mailbox claims and their Person row are committed in
//! one PostgreSQL transaction; successful writes are then projected to
//! SurrealDB for callers whose non-People records still use its links.

use sqlx::{AssertSqlSafe, Postgres, QueryBuilder, Row, Transaction};
use uuid::Uuid;

use crate::listing::{self, ListParams, Page};
use crate::persons::{NewPerson, Person, PersonEdit, PersonError, Role};
use crate::postgres::PostgresDb;
use crate::surreal::{record_id, SurrealDb};

const COLUMNS: &str = "id, name, given_name, family_name, middle_name, email, \
    oidc_subject, microsoft_subject, apple_subject, role, title, phone, \
    xero_contact_id, profile_image_url, linkedin_url, email_confirmed, \
    is_admitted, inserted_at, updated_at";

fn person(row: &sqlx::postgres::PgRow) -> Result<Person, sqlx::Error> {
    let role: String = row.try_get("role")?;
    let role = Role::parse(&role).ok_or_else(|| sqlx::Error::ColumnDecode {
        index: "role".into(),
        source: Box::new(std::io::Error::other("unknown persisted Person role")),
    })?;
    Ok(Person {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        given_name: row.try_get("given_name")?,
        family_name: row.try_get("family_name")?,
        middle_name: row.try_get("middle_name")?,
        email: row.try_get("email")?,
        oidc_subject: row.try_get("oidc_subject")?,
        microsoft_subject: row.try_get("microsoft_subject")?,
        apple_subject: row.try_get("apple_subject")?,
        role,
        title: row.try_get("title")?,
        phone: row.try_get("phone")?,
        xero_contact_id: row.try_get("xero_contact_id")?,
        profile_image_url: row.try_get("profile_image_url")?,
        linkedin_url: row.try_get("linkedin_url")?,
        email_confirmed: row.try_get("email_confirmed")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn database(_error: sqlx::Error) -> PersonError {
    PersonError::Postgres("database operation failed".into())
}

pub(crate) async fn by_id(pg: &PostgresDb, id: Uuid) -> Result<Option<Person>, PersonError> {
    sqlx::query(AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM person WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(&pg.0)
    .await
    .map_err(database)?
    .as_ref()
    .map(person)
    .transpose()
    .map_err(database)
}

pub(crate) async fn by_email(pg: &PostgresDb, email: &str) -> Result<Option<Person>, PersonError> {
    sqlx::query(AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM person WHERE email = $1::citext"
    )))
    .bind(email.trim())
    .fetch_optional(&pg.0)
    .await
    .map_err(database)?
    .as_ref()
    .map(person)
    .transpose()
    .map_err(database)
}

pub(crate) async fn by_subject(
    pg: &PostgresDb,
    column: &'static str,
    subject: &str,
) -> Result<Option<Person>, PersonError> {
    let sql = format!("SELECT {COLUMNS} FROM person WHERE {column} = $1");
    sqlx::query(AssertSqlSafe(sql))
        .bind(subject)
        .fetch_optional(&pg.0)
        .await
        .map_err(database)?
        .as_ref()
        .map(person)
        .transpose()
        .map_err(database)
}

pub(crate) async fn many_by_ids(pg: &PostgresDb, ids: &[Uuid]) -> Result<Vec<Person>, PersonError> {
    let rows = sqlx::query(AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM person WHERE id = ANY($1)"
    )))
    .bind(ids)
    .fetch_all(&pg.0)
    .await
    .map_err(database)?;
    rows.iter()
        .map(person)
        .collect::<Result<Vec<_>, _>>()
        .map_err(database)
}

pub(crate) async fn all_with_phone(pg: &PostgresDb) -> Result<Vec<Person>, PersonError> {
    let rows = sqlx::query(AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM person WHERE phone IS NOT NULL"
    )))
    .fetch_all(&pg.0)
    .await
    .map_err(database)?;
    rows.iter()
        .map(person)
        .collect::<Result<Vec<_>, _>>()
        .map_err(database)
}

pub(crate) async fn by_role(pg: &PostgresDb, role: Role) -> Result<Vec<Person>, PersonError> {
    let rows = sqlx::query(AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM person WHERE role = $1"
    )))
    .bind(role.as_str())
    .fetch_all(&pg.0)
    .await
    .map_err(database)?;
    rows.iter()
        .map(person)
        .collect::<Result<Vec<_>, _>>()
        .map_err(database)
}

/// Every person holding `role` at one Firm — the firm-filtered lookup the
/// deployment-wide [`by_role`] cannot answer. Membership is the
/// `person_firm_role` join; the person's own role stays the system-wide
/// tier, so "the Firm's Owner" is an Owner-tier person with a membership
/// row, and a Firm with none returns an empty list.
pub(crate) async fn by_role_and_firm(
    pg: &PostgresDb,
    role: Role,
    firm_id: Uuid,
) -> Result<Vec<Person>, PersonError> {
    let rows = sqlx::query(AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM person \
         WHERE role = $1 \
           AND id IN (SELECT person_id FROM person_firm_role WHERE firm_id = $2)"
    )))
    .bind(role.as_str())
    .bind(firm_id)
    .fetch_all(&pg.0)
    .await
    .map_err(database)?;
    rows.iter()
        .map(person)
        .collect::<Result<Vec<_>, _>>()
        .map_err(database)
}

/// The directory's `ORDER BY`: only `name` and `email` are honored, never
/// the caller's string, and an empty sort falls back to name.
/// One keyset page of the people directory over the `person_directory` view:
/// each Person with their Firm names. `visible_to_firms` narrows the rows to
/// those the `firm_visible_person` view admits for any of those Firms; `None`
/// lists everyone.
///
/// Only the clauses the request uses are in the SQL, so each filter reaches
/// its trigram index and each sort its `(field, id)` index
/// (`20261107120000_person_directory_list_indexes.sql`); the view's Firm-name
/// lookup runs for the page's rows alone.
pub(crate) async fn directory_page(
    pg: &PostgresDb,
    params: &ListParams,
    visible_to_firms: Option<&[Uuid]>,
) -> Result<Page<(Person, Vec<String>)>, PersonError> {
    let mut columns: Vec<(&str, &str)> = params
        .sort
        .iter()
        .map(|(field, _)| match field.as_str() {
            "email" => ("email", "citext"),
            _ => ("name", "text"),
        })
        .collect();
    columns.push(("id", "uuid"));
    let mut clauses = Vec::new();
    let mut texts = Vec::new();
    for (member, column) in [("name", "name"), ("email", "email::text")] {
        let needle = params.filter(member);
        if !needle.is_empty() {
            texts.push(listing::like_contains(needle));
            clauses.push(format!("{column} ILIKE ${}", texts.len()));
        }
    }
    let firms_bind = texts.len() + 1;
    if visible_to_firms.is_some() {
        clauses.push(format!(
            "id IN (SELECT person_id FROM firm_visible_person WHERE firm_id = ANY(${firms_bind}))"
        ));
    }
    let limit_bind = firms_bind + usize::from(visible_to_firms.is_some());
    let cursor_values = params
        .cursor
        .as_ref()
        .map(|c| c.values.as_slice())
        .unwrap_or_default();
    if let Some(keyset) = listing::pg_keyset(params, &columns, limit_bind + 1) {
        clauses.push(keyset);
    }
    let filter = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    let sql = format!(
        "SELECT {COLUMNS}, firm_names FROM person_directory {filter} \
         ORDER BY {} LIMIT ${limit_bind}",
        listing::pg_order(params, &columns)
    );
    let mut query = sqlx::query(AssertSqlSafe(sql));
    for text in texts {
        query = query.bind(text);
    }
    if let Some(firms) = visible_to_firms {
        query = query.bind(firms);
    }
    query = query.bind(params.fetch_limit());
    for value in cursor_values {
        query = query.bind(value);
    }
    let rows = query.fetch_all(&pg.0).await.map_err(database)?;
    let rows = rows
        .iter()
        .map(|row| Ok((person(row)?, row.try_get("firm_names")?)))
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(database)?;
    Ok(Page::from_rows(
        rows,
        params,
        |(person, _), field| match field {
            "email" => person.email.clone(),
            "name" => person.name.clone(),
            _ => person.id.to_string(),
        },
    ))
}

pub(crate) async fn directory(pg: &PostgresDb) -> Result<Vec<Person>, PersonError> {
    let rows = sqlx::query(AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM person ORDER BY name ASC, id ASC"
    )))
    .fetch_all(&pg.0)
    .await
    .map_err(database)?;
    rows.iter()
        .map(person)
        .collect::<Result<Vec<_>, _>>()
        .map_err(database)
}

pub(crate) async fn search(
    pg: &PostgresDb,
    name: Option<&str>,
    email: Option<&str>,
    limit: u64,
) -> Result<Vec<Person>, PersonError> {
    let name = name.map(str::trim).filter(|s| !s.is_empty()).unwrap_or("");
    let email = email.map(str::trim).filter(|s| !s.is_empty()).unwrap_or("");
    let rows = sqlx::query(AssertSqlSafe(format!("SELECT {COLUMNS} FROM person WHERE ($1 = '' OR strpos(lower(name), $1) > 0) AND ($2 = '' OR strpos(lower(email::text), $2) > 0) ORDER BY name ASC LIMIT $3")))
        .bind(name.to_lowercase())
        .bind(email.to_lowercase())
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&pg.0).await.map_err(database)?;
    rows.iter()
        .map(person)
        .collect::<Result<Vec<_>, _>>()
        .map_err(database)
}

pub(crate) async fn default_firm_dri(pg: &PostgresDb) -> Result<Option<Uuid>, PersonError> {
    let rows = by_role(pg, Role::Owner)
        .await?
        .into_iter()
        .chain(by_role(pg, Role::Admin).await?)
        .chain(by_role(pg, Role::Lawyer).await?);
    Ok(rows
        .min_by_key(|person| (std::cmp::Reverse(person.role.authority_rank()), person.id))
        .map(|person| person.id))
}

pub(crate) async fn mailbox_holder(
    pg: &PostgresDb,
    email_lower: &str,
) -> Result<Option<Uuid>, PersonError> {
    sqlx::query_scalar("SELECT person_id FROM person_mailbox WHERE id = lower($1)")
        .bind(email_lower)
        .fetch_optional(&pg.0)
        .await
        .map_err(|_| PersonError::Postgres("database operation failed".into()))
}

pub(crate) async fn is_admitted(pg: &PostgresDb, id: Uuid) -> Result<bool, PersonError> {
    Ok(
        sqlx::query_scalar("SELECT is_admitted FROM person WHERE id = $1")
            .bind(id)
            .fetch_optional(&pg.0)
            .await
            .map_err(database)?
            .unwrap_or(true),
    )
}

async fn person_transaction(
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Transaction<'_, Postgres>, PersonError> {
    let mut tx = pg.0.begin().await.map_err(database)?;
    sqlx::query_scalar::<_, ()>("SELECT pg_advisory_xact_lock(1312902736, hashtext($1::text))")
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(database)?;
    Ok(tx)
}

/// Project the current committed PostgreSQL state while serializing both
/// authoritative writes and compatibility projections for this Person.
async fn sync_current_surreal(
    pg: &PostgresDb,
    db: &SurrealDb,
    id: Uuid,
) -> Result<(), PersonError> {
    let mut tx = person_transaction(pg, id).await?;
    let row = sqlx::query(AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM person WHERE id=$1 FOR UPDATE"
    )))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(database)?;
    if let Some(row) = row {
        let admitted: bool = row.try_get("is_admitted").map_err(database)?;
        let current = person(&row).map_err(database)?;
        let claims: Vec<String> =
            sqlx::query_scalar("SELECT id FROM person_mailbox WHERE person_id=$1 ORDER BY id")
                .bind(id)
                .fetch_all(&mut *tx)
                .await
                .map_err(database)?;
        sync_surreal(db, &current, admitted, &claims).await?;
    } else {
        db.query("DELETE $id")
            .bind(("id", record_id("person", id)))
            .await
            .and_then(surrealdb::IndexedResults::check)
            .map_err(|error| {
                PersonError::PostgresProjection(format!("SurrealDB delete failed: {error}"))
            })?;
        db.query("DELETE person_mailbox WHERE person_id=$id")
            .bind(("id", record_id("person", id)))
            .await
            .and_then(surrealdb::IndexedResults::check)
            .map_err(|error| {
                PersonError::PostgresProjection(format!(
                    "SurrealDB mailbox cleanup failed: {error}"
                ))
            })?;
    }
    tx.commit().await.map_err(database)
}

pub(crate) async fn set_text(
    pg: &PostgresDb,
    db: &SurrealDb,
    id: Uuid,
    column: &str,
    value: Option<&str>,
) -> Result<Option<Person>, PersonError> {
    if !matches!(column, "phone" | "profile_image_url" | "xero_contact_id") {
        return Err(PersonError::Postgres(
            "unsupported Person text field".into(),
        ));
    }
    let sql =
        format!("UPDATE person SET {column}=$2, updated_at=now() WHERE id=$1 RETURNING {COLUMNS}");
    let mut tx = person_transaction(pg, id).await?;
    let row = sqlx::query(AssertSqlSafe(sql))
        .bind(id)
        .bind(value.map(str::trim))
        .fetch_optional(&mut *tx)
        .await
        .map_err(database)?;
    let Some(row) = row else {
        tx.rollback().await.map_err(database)?;
        return Ok(None);
    };
    let row = person(&row).map_err(database)?;
    tx.commit().await.map_err(database)?;
    sync_current_surreal(pg, db, id).await?;
    Ok(Some(row))
}

pub(crate) async fn set_flag(
    pg: &PostgresDb,
    db: &SurrealDb,
    id: Uuid,
    column: &str,
    value: bool,
) -> Result<Option<Person>, PersonError> {
    if !matches!(column, "email_confirmed" | "is_admitted") {
        return Err(PersonError::Postgres("unsupported Person flag".into()));
    }
    let sql =
        format!("UPDATE person SET {column}=$2, updated_at=now() WHERE id=$1 RETURNING {COLUMNS}");
    let mut tx = person_transaction(pg, id).await?;
    let row = sqlx::query(AssertSqlSafe(sql))
        .bind(id)
        .bind(value)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database)?;
    let Some(row) = row else {
        tx.rollback().await.map_err(database)?;
        return Ok(None);
    };
    let row = person(&row).map_err(database)?;
    tx.commit().await.map_err(database)?;
    sync_current_surreal(pg, db, id).await?;
    Ok(Some(row))
}

pub(crate) async fn set_role(
    pg: &PostgresDb,
    db: &SurrealDb,
    id: Uuid,
    role: Role,
) -> Result<Option<Person>, PersonError> {
    let mut tx = person_transaction(pg, id).await?;
    let row = sqlx::query(AssertSqlSafe(format!(
        "UPDATE person SET role=$2, updated_at=now() WHERE id=$1 RETURNING {COLUMNS}"
    )))
    .bind(id)
    .bind(role.as_str())
    .fetch_optional(&mut *tx)
    .await
    .map_err(classify)?;
    let Some(row) = row else {
        tx.rollback().await.map_err(database)?;
        return Ok(None);
    };
    let row = person(&row).map_err(database)?;
    tx.commit().await.map_err(database)?;
    sync_current_surreal(pg, db, id).await?;
    Ok(Some(row))
}

pub(crate) async fn link_subject(
    pg: &PostgresDb,
    db: &SurrealDb,
    id: Uuid,
    column: &str,
    subject: &str,
    adopt_legacy: bool,
) -> Result<Option<Person>, PersonError> {
    if !matches!(
        column,
        "oidc_subject" | "microsoft_subject" | "apple_subject"
    ) {
        return Err(PersonError::Postgres("unsupported provider subject".into()));
    }
    let sql = if adopt_legacy {
        format!("UPDATE person SET {column}=$2, oidc_subject=NULL, updated_at=now() WHERE id=$1 AND oidc_subject=$2 RETURNING {COLUMNS}")
    } else {
        format!("UPDATE person SET {column}=$2, updated_at=now() WHERE id=$1 RETURNING {COLUMNS}")
    };
    let mut tx = person_transaction(pg, id).await?;
    let row = sqlx::query(AssertSqlSafe(sql))
        .bind(id)
        .bind(subject)
        .fetch_optional(&mut *tx)
        .await
        .map_err(classify)?;
    let Some(row) = row else {
        tx.rollback().await.map_err(database)?;
        return Ok(None);
    };
    let row = person(&row).map_err(database)?;
    tx.commit().await.map_err(database)?;
    sync_current_surreal(pg, db, id).await?;
    Ok(Some(row))
}

pub(crate) async fn edit(
    pg: &PostgresDb,
    db: &SurrealDb,
    id: Uuid,
    input: &PersonEdit,
) -> Result<Option<Person>, PersonError> {
    let mut tx = person_transaction(pg, id).await?;
    // Serialize edits to a Person before deciding which mailbox claim to move.
    // Reading before the transaction can leave a stale claim if two email edits
    // race and each deletes the other edit's previous address.
    let current = sqlx::query(AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM person WHERE id=$1 FOR UPDATE"
    )))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(database)?;
    let Some(current) = current.as_ref().map(person).transpose().map_err(database)? else {
        tx.rollback().await.map_err(database)?;
        return Ok(None);
    };
    let moving_email = input
        .email
        .as_ref()
        .map(|email| email.trim().to_string())
        .filter(|email| email.to_lowercase() != current.email.to_lowercase());
    if let Some(email) = moving_email.as_deref() {
        sqlx::query("INSERT INTO person_mailbox(id, person_id) VALUES (lower($1), $2)")
            .bind(email)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(classify)?;
    }
    let mut builder = QueryBuilder::<Postgres>::new("UPDATE person SET updated_at=now()");
    if let Some(value) = input.name.as_ref() {
        builder.push(", name=").push_bind(value.trim());
    }
    if let Some(value) = input.email.as_deref() {
        builder.push(", email=").push_bind(value.trim());
    }
    if let Some(value) = input.role {
        builder.push(", role=").push_bind(value.as_str());
    }
    if let Some(value) = input.given_name.as_ref() {
        builder.push(", given_name=").push_bind(value.as_deref());
    }
    if let Some(value) = input.family_name.as_ref() {
        builder.push(", family_name=").push_bind(value.as_deref());
    }
    if let Some(value) = input.middle_name.as_ref() {
        builder.push(", middle_name=").push_bind(value.as_deref());
    }
    if let Some(value) = input.profile_image_url.as_ref() {
        builder
            .push(", profile_image_url=")
            .push_bind(value.as_deref());
    }
    if let Some(value) = input.linkedin_url.as_ref() {
        builder.push(", linkedin_url=").push_bind(value.as_deref());
    }
    builder
        .push(" WHERE id=")
        .push_bind(id)
        .push(" RETURNING ")
        .push(COLUMNS);
    let row = builder
        .build()
        .fetch_optional(&mut *tx)
        .await
        .map_err(classify)?;
    let Some(row) = row else {
        tx.rollback().await.map_err(database)?;
        return Ok(None);
    };
    let row = person(&row).map_err(database)?;
    if moving_email.is_some() {
        sqlx::query("DELETE FROM person_mailbox WHERE id=lower($1) AND person_id=$2")
            .bind(&current.email)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(database)?;
    }
    tx.commit().await.map_err(database)?;
    sync_current_surreal(pg, db, id).await?;
    Ok(Some(row))
}

pub(crate) async fn update_contact(
    pg: &PostgresDb,
    db: &SurrealDb,
    id: Uuid,
    input: &crate::persons::ContactUpdate,
) -> Result<Option<Person>, PersonError> {
    let mut tx = person_transaction(pg, id).await?;
    let row = sqlx::query(AssertSqlSafe(format!("UPDATE person SET name=$2, title=$3, phone=$4, updated_at=now() WHERE id=$1 RETURNING {COLUMNS}")))
        .bind(id).bind(input.name.trim()).bind(&input.title).bind(&input.phone)
        .fetch_optional(&mut *tx).await.map_err(database)?;
    let Some(row) = row else {
        tx.rollback().await.map_err(database)?;
        return Ok(None);
    };
    let row = person(&row).map_err(database)?;
    tx.commit().await.map_err(database)?;
    sync_current_surreal(pg, db, id).await?;
    Ok(Some(row))
}

pub(crate) async fn delete(pg: &PostgresDb, db: &SurrealDb, id: Uuid) -> Result<(), PersonError> {
    let mut tx = person_transaction(pg, id).await?;
    sqlx::query("DELETE FROM person_mailbox WHERE person_id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
    sqlx::query("DELETE FROM person WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
    tx.commit().await.map_err(database)?;
    sync_current_surreal(pg, db, id).await
}

/// Tables where a unique index spans a Person link, with the other columns
/// of that index. A row the dropped Person holds that the kept Person
/// already holds an equivalent of is deleted, and the kept Person's row stands.
const MERGE_UNIQUE_LINKS: &[(&str, &[&str])] = &[
    ("person_firm_role", &["firm_id"]),
    ("person_project_role", &["project_id"]),
    ("credential", &["jurisdiction_id"]),
    ("entity_role", &["entity_id", "role"]),
    ("person_external_identity", &["system"]),
    ("project_reference", &["project_id", "reason"]),
    ("testimonial", &["project_id", "quote"]),
    ("sandbox_usage", &["period"]),
    ("person_phone", &["e164"]),
];

/// Fold `drop_id` into `keep_id` in one transaction: every PostgreSQL row
/// that links the dropped Person moves to the kept Person, the kept Person
/// takes each provider subject it lacks and every mailbox claim, and the
/// dropped row is deleted last. Links are read from the catalog's foreign
/// keys, so a table added later moves without a list to update.
pub(crate) async fn merge(
    pg: &PostgresDb,
    db: &SurrealDb,
    keep_id: Uuid,
    drop_id: Uuid,
) -> Result<(), PersonError> {
    let mut tx = pg.0.begin().await.map_err(database)?;
    let mut locked = [keep_id, drop_id];
    locked.sort();
    for id in locked {
        sqlx::query("SELECT pg_advisory_xact_lock(1312902736, hashtext($1::text))")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(database)?;
    }
    for flag in ["is_lawyer_dri", "is_client_dri"] {
        sqlx::query(AssertSqlSafe(format!(
            "UPDATE person_project_role SET {flag} = true WHERE person_id = $1 AND project_id IN \
             (SELECT project_id FROM person_project_role WHERE person_id = $2 AND {flag})"
        )))
        .bind(keep_id)
        .bind(drop_id)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
    }
    for (table, others) in MERGE_UNIQUE_LINKS {
        let same = others
            .iter()
            .map(|column| format!("k.{column} = d.{column}"))
            .collect::<Vec<_>>()
            .join(" AND ");
        sqlx::query(AssertSqlSafe(format!(
            "DELETE FROM {table} d WHERE d.person_id = $2 AND EXISTS \
             (SELECT 1 FROM {table} k WHERE k.person_id = $1 AND {same})"
        )))
        .bind(keep_id)
        .bind(drop_id)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
    }
    sqlx::query(
        "UPDATE relationship_log SET subject_id = $1 \
         WHERE subject_type = 'person' AND subject_id = $2",
    )
    .bind(keep_id)
    .bind(drop_id)
    .execute(&mut *tx)
    .await
    .map_err(database)?;
    let links: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.conrelid::regclass::text, a.attname::text FROM pg_constraint c \
         JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = ANY(c.conkey) \
         WHERE c.contype = 'f' AND c.confrelid = 'person'::regclass ORDER BY 1, 2",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(database)?;
    for (table, column) in links {
        sqlx::query(AssertSqlSafe(format!(
            "UPDATE {table} SET {column} = $1 WHERE {column} = $2"
        )))
        .bind(keep_id)
        .bind(drop_id)
        .execute(&mut *tx)
        .await
        .map_err(classify)?;
    }
    // `person_phone` names its Person without a foreign key. The kept
    // Person's primary number stays the only primary.
    let phones: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE person_phone d SET person_id = $1, updated_at = now(), \
         is_primary = d.is_primary AND NOT EXISTS \
         (SELECT 1 FROM person_phone k WHERE k.person_id = $1 AND k.is_primary) \
         WHERE d.person_id = $2 RETURNING d.id",
    )
    .bind(keep_id)
    .bind(drop_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(database)?;
    move_subjects(&mut tx, keep_id, drop_id).await?;
    sqlx::query("DELETE FROM person WHERE id = $1")
        .bind(drop_id)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
    tx.commit().await.map_err(database)?;
    sync_current_surreal(pg, db, drop_id).await?;
    sync_current_surreal(pg, db, keep_id).await?;
    for phone in phones {
        crate::postgres::identity::project(pg, db, &crate::postgres::identity::PERSON_PHONE, phone)
            .await
            .map_err(|error| PersonError::PostgresProjection(error.to_string()))?;
    }
    Ok(())
}

/// Give the kept Person each provider subject it lacks from the dropped one.
/// Each subject column is unique, so the dropped row lets go first.
async fn move_subjects(
    tx: &mut Transaction<'_, Postgres>,
    keep_id: Uuid,
    drop_id: Uuid,
) -> Result<(), PersonError> {
    let subjects: (Option<String>, Option<String>, Option<String>) = sqlx::query_as(
        "UPDATE person d SET oidc_subject = NULL, microsoft_subject = NULL, apple_subject = NULL \
         FROM person o WHERE d.id = $1 AND o.id = d.id \
         RETURNING o.oidc_subject, o.microsoft_subject, o.apple_subject",
    )
    .bind(drop_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(database)?;
    sqlx::query(
        "UPDATE person SET oidc_subject = COALESCE(oidc_subject, $2), \
         microsoft_subject = COALESCE(microsoft_subject, $3), \
         apple_subject = COALESCE(apple_subject, $4), updated_at = now() WHERE id = $1",
    )
    .bind(keep_id)
    .bind(subjects.0)
    .bind(subjects.1)
    .bind(subjects.2)
    .execute(&mut **tx)
    .await
    .map_err(classify)?;
    Ok(())
}

#[cfg(test)]
#[path = "people_tests.rs"]
mod tests;

fn classify(error: sqlx::Error) -> PersonError {
    if let sqlx::Error::Database(ref database) = error {
        match database.constraint() {
            Some("person_email_key" | "person_mailbox_pkey") => return PersonError::EmailTaken,
            Some("person_oidc_subject_key") => return PersonError::OidcSubjectTaken,
            Some("person_microsoft_subject_key") => return PersonError::MicrosoftSubjectTaken,
            Some("person_apple_subject_key") => return PersonError::AppleSubjectTaken,
            _ => {}
        }
    }
    database(error)
}

async fn insert_person(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    input: &NewPerson,
) -> Result<Person, PersonError> {
    let row = sqlx::query(AssertSqlSafe(format!("INSERT INTO person (id, name, email, role, given_name, family_name, middle_name, oidc_subject, microsoft_subject, apple_subject, title, phone, profile_image_url, linkedin_url) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) RETURNING {COLUMNS}")))
        .bind(id).bind(input.name.trim()).bind(input.email.trim()).bind(input.role.as_str())
        .bind(&input.given_name).bind(&input.family_name).bind(&input.middle_name)
        .bind(&input.oidc_subject).bind(&input.microsoft_subject).bind(&input.apple_subject)
        .bind(&input.title).bind(&input.phone).bind(&input.profile_image_url).bind(&input.linkedin_url)
        .fetch_one(&mut **tx).await.map_err(classify)?;
    person(&row).map_err(database)
}

async fn sync_surreal(
    db: &SurrealDb,
    person: &Person,
    admitted: bool,
    claims: &[String],
) -> Result<(), PersonError> {
    let query = db.query("UPSERT $id SET name=$name, given_name=$given_name, family_name=$family_name, middle_name=$middle_name, email=$email, oidc_subject=$oidc_subject, microsoft_subject=$microsoft_subject, apple_subject=$apple_subject, role=$role, title=$title, phone=$phone, xero_contact_id=$xero_contact_id, profile_image_url=$profile_image_url, linkedin_url=$linkedin_url, email_confirmed=$email_confirmed, is_admitted=$is_admitted, inserted_at=$inserted_at, updated_at=$updated_at")
        .bind(("id", record_id("person", person.id))).bind(("name", person.name.clone()))
        .bind(("given_name", person.given_name.clone())).bind(("family_name", person.family_name.clone()))
        .bind(("middle_name", person.middle_name.clone())).bind(("email", person.email.clone()))
        .bind(("oidc_subject", person.oidc_subject.clone())).bind(("microsoft_subject", person.microsoft_subject.clone()))
        .bind(("apple_subject", person.apple_subject.clone())).bind(("role", person.role.as_str().to_string()))
        .bind(("title", person.title.clone())).bind(("phone", person.phone.clone()))
        .bind(("xero_contact_id", person.xero_contact_id.clone())).bind(("profile_image_url", person.profile_image_url.clone()))
        .bind(("linkedin_url", person.linkedin_url.clone())).bind(("email_confirmed", person.email_confirmed))
        .bind(("is_admitted", admitted)).bind(("inserted_at", surrealdb::types::Datetime::from(person.inserted_at)))
        .bind(("updated_at", surrealdb::types::Datetime::from(person.updated_at)));
    query
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|error| {
            PersonError::PostgresProjection(format!("SurrealDB write failed: {error}"))
        })?;
    // Every claim the Person holds in PostgreSQL, primary and aliases alike,
    // replaces the projected set in one statement.
    db.query("DELETE person_mailbox WHERE person_id = $id; FOR $claim IN $claims { UPSERT type::record('person_mailbox', $claim) SET person_id = $id; };")
        .bind(("id", record_id("person", person.id))).bind(("claims", claims.to_vec()))
        .await.and_then(surrealdb::IndexedResults::check)
        .map_err(|error| {
            PersonError::PostgresProjection(format!("SurrealDB mailbox sync failed: {error}"))
        })?;
    Ok(())
}

/// Every address `id` holds besides its primary one, lowercased and sorted.
pub(crate) async fn aliases(pg: &PostgresDb, id: Uuid) -> Result<Vec<String>, PersonError> {
    sqlx::query_scalar(
        "SELECT m.id FROM person_mailbox m JOIN person p ON p.id = m.person_id \
         WHERE m.person_id = $1 AND m.id <> lower(p.email::text) ORDER BY m.id",
    )
    .bind(id)
    .fetch_all(&pg.0)
    .await
    .map_err(database)
}

/// Claim `email` as another address of `id`. A claim `id` already holds is
/// left as it is; one another Person holds is refused, never taken.
pub(crate) async fn add_alias(
    pg: &PostgresDb,
    db: &SurrealDb,
    id: Uuid,
    email: &str,
) -> Result<(), PersonError> {
    let mut tx = person_transaction(pg, id).await?;
    let holder: Option<Uuid> = sqlx::query_scalar(
        "WITH claimed AS (INSERT INTO person_mailbox(id, person_id) VALUES (lower($1), $2) \
         ON CONFLICT (id) DO NOTHING RETURNING person_id) \
         SELECT person_id FROM claimed UNION ALL \
         SELECT person_id FROM person_mailbox WHERE id = lower($1) LIMIT 1",
    )
    .bind(email.trim())
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(database)?;
    if holder != Some(id) {
        tx.rollback().await.map_err(database)?;
        return Err(PersonError::EmailTaken);
    }
    tx.commit().await.map_err(database)?;
    sync_current_surreal(pg, db, id).await
}

pub(crate) async fn create(
    pg: &PostgresDb,
    db: &SurrealDb,
    input: &NewPerson,
) -> Result<Person, PersonError> {
    let id = Uuid::now_v7();
    let mut tx = person_transaction(pg, id).await?;
    // The mailbox claim has a Person foreign key, so write the Person first.
    // Both rows still commit atomically, and `person_email_key` remains the
    // database-enforced race backstop for concurrent creates.
    let row = insert_person(&mut tx, id, input).await?;
    sqlx::query("INSERT INTO person_mailbox(id, person_id) VALUES (lower($1), $2)")
        .bind(input.email.trim())
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(classify)?;
    tx.commit().await.map_err(database)?;
    sync_current_surreal(pg, db, id).await?;
    Ok(row)
}

pub(crate) async fn find_or_create(
    pg: &PostgresDb,
    db: &SurrealDb,
    input: &NewPerson,
) -> Result<Person, PersonError> {
    if let Some(row) = by_email(pg, &input.email).await? {
        sync_current_surreal(pg, db, row.id).await?;
        return Ok(row);
    }
    match create(pg, db, input).await {
        Err(PersonError::EmailTaken) => {
            let row = by_email(pg, &input.email)
                .await?
                .ok_or(PersonError::WriteReturnedNothing)?;
            sync_current_surreal(pg, db, row.id).await?;
            Ok(row)
        }
        result => result,
    }
}
