//! PostgreSQL operations for the People directory.
//!
//! Mailbox claims and their Person row commit in one PostgreSQL transaction.

use sqlx::{AssertSqlSafe, Postgres, QueryBuilder, Row, Transaction};
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::listing::{self, ListParams, Page};
use crate::persons::{NewPerson, Person, PersonEdit, PersonError, Role};
use crate::postgres::PostgresDb;

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

pub(crate) async fn set_text(
    pg: &PostgresDb,
    _db: &ApplicationStore,
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
    Ok(Some(row))
}

pub(crate) async fn set_flag(
    pg: &PostgresDb,
    _db: &ApplicationStore,
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
    Ok(Some(row))
}

pub(crate) async fn set_role(
    pg: &PostgresDb,
    _db: &ApplicationStore,
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
    Ok(Some(row))
}

pub(crate) async fn link_subject(
    pg: &PostgresDb,
    _db: &ApplicationStore,
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
        format!(
            "UPDATE person SET {column}=$2, oidc_subject=NULL, updated_at=now() WHERE id=$1 AND oidc_subject=$2 RETURNING {COLUMNS}"
        )
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
    Ok(Some(row))
}

pub(crate) async fn edit(
    pg: &PostgresDb,
    _db: &ApplicationStore,
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
    if moving_email.is_some() {
        builder.push(", email_confirmed=false");
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
    Ok(Some(row))
}

pub(crate) async fn update_contact(
    pg: &PostgresDb,
    _db: &ApplicationStore,
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
    Ok(Some(row))
}

pub(crate) async fn delete(
    pg: &PostgresDb,
    _db: &ApplicationStore,
    id: Uuid,
) -> Result<(), PersonError> {
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
    Ok(())
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
    ("person_calendar", &["calendar_id"]),
];

/// Preserve the retained Person's primary and booking calendar registrations.
async fn prepare_merged_calendars(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    keep_id: Uuid,
    drop_id: Uuid,
) -> Result<(), PersonError> {
    sqlx::query(
        "UPDATE person_calendar SET kind = 'freebusy' WHERE person_id = $2 AND kind = 'primary' \
         AND EXISTS (SELECT 1 FROM person_calendar WHERE person_id = $1 AND kind = 'primary')",
    )
    .bind(keep_id)
    .bind(drop_id)
    .execute(&mut **tx)
    .await
    .map_err(database)?;
    sqlx::query(
        "DELETE FROM person_calendar WHERE person_id = $2 AND kind = 'booking'
         AND EXISTS (SELECT 1 FROM person_calendar WHERE person_id = $1 AND kind = 'booking')",
    )
    .bind(keep_id)
    .bind(drop_id)
    .execute(&mut **tx)
    .await
    .map_err(database)?;
    Ok(())
}

async fn ensure_no_retained_identity_evidence(
    connection: &mut sqlx::PgConnection,
    dropped: Uuid,
) -> Result<(), crate::person_merge::PersonMergeError> {
    let retained: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM billing_annotation WHERE attorney_id=$1 OR reviewed_by=$1)",
    )
    .bind(dropped)
    .fetch_one(&mut *connection)
    .await
    .map_err(database)?;
    if retained {
        return Err(crate::person_merge::PersonMergeError::RetainedBillingHistory);
    }
    let retained_claim: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM legal_plan_claim_event WHERE actor_id=$1)")
            .bind(dropped)
            .fetch_one(connection)
            .await
            .map_err(database)?;
    if retained_claim {
        return Err(crate::person_merge::PersonMergeError::RetainedClaimHistory);
    }
    Ok(())
}

/// Lock both Persons and validate merge eligibility for the caller-owned transaction.
async fn lock_merge_people(
    tx: &mut Transaction<'_, Postgres>,
    keep_id: Uuid,
    drop_id: Uuid,
    actor: Option<Role>,
) -> Result<Role, crate::person_merge::PersonMergeError> {
    let mut locked = [keep_id, drop_id];
    locked.sort();
    for id in locked {
        sqlx::query("SELECT pg_advisory_xact_lock(1312902736, hashtext($1::text))")
            .bind(id)
            .execute(&mut **tx)
            .await
            .map_err(database)?;
    }
    let people: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id,role,email::text FROM person WHERE id=ANY($1::uuid[]) ORDER BY id FOR UPDATE",
    )
    .bind(locked.to_vec())
    .fetch_all(&mut **tx)
    .await
    .map_err(database)?;
    let keep = people
        .iter()
        .find(|row| row.0 == keep_id)
        .ok_or_else(|| crate::person_merge::PersonMergeError::NotFound(keep_id.to_string()))?;
    let dropped = people
        .iter()
        .find(|row| row.0 == drop_id)
        .ok_or_else(|| crate::person_merge::PersonMergeError::NotFound(drop_id.to_string()))?;
    if dropped.1 != Role::Client.as_str() {
        return Err(crate::person_merge::PersonMergeError::Blocked);
    }
    let kept_role = Role::parse(&keep.1)
        .ok_or_else(|| PersonError::Postgres("the kept Person has an invalid role".into()))?;
    if let Some(actor) = actor {
        crate::person_merge::require_identity_governance(actor, kept_role)?;
    }
    // Imported rows can predate mailbox claims. Preserve both primaries as
    // claims before moving links, without taking another Person's capability.
    for (email, owner) in [(&keep.2, keep_id), (&dropped.2, drop_id)] {
        if email.trim().is_empty() {
            continue;
        }
        let holder: Option<Uuid> = sqlx::query_scalar("WITH claimed AS (INSERT INTO person_mailbox(id,person_id) VALUES(lower($1),$2) ON CONFLICT(id) DO NOTHING RETURNING person_id) SELECT person_id FROM claimed UNION ALL SELECT person_id FROM person_mailbox WHERE id=lower($1) LIMIT 1")
            .bind(email.trim()).bind(owner).fetch_optional(&mut **tx).await.map_err(database)?;
        if holder != Some(owner) {
            return Err(PersonError::EmailTaken.into());
        }
    }
    Ok(kept_role)
}

/// Rewrite typed Person links without replacing their row or other JSON fields.
async fn relink_tagged_people(
    transaction: &mut Transaction<'_, Postgres>,
    keep_id: Uuid,
    drop_id: Uuid,
) -> Result<(), PersonError> {
    for &(table, column) in crate::postgres::record_metadata::TAGGED_PERSON_LINKS {
        sqlx::query(AssertSqlSafe(format!(
            "UPDATE \"{table}\" SET \"{column}\" = jsonb_set(\"{column}\", '{{id,Uuid}}', to_jsonb($1::text), false) WHERE \"{column}\"->>'tb'='person' AND \"{column}\"->'id'->>'Uuid'=$2::text"
        )))
        .bind(keep_id).bind(drop_id).execute(&mut **transaction).await.map_err(database)?;
    }
    Ok(())
}

pub(crate) async fn merge_rows(
    pg: &PostgresDb,
    keep_id: Uuid,
    drop_id: Uuid,
) -> Result<(), crate::person_merge::PersonMergeError> {
    merge_rows_with_actor(pg, keep_id, drop_id, None).await
}

async fn merge_rows_with_actor(
    pg: &PostgresDb,
    keep_id: Uuid,
    drop_id: Uuid,
    actor: Option<Role>,
) -> Result<(), crate::person_merge::PersonMergeError> {
    let mut tx = pg.0.begin().await.map_err(database)?;
    let kept_role = lock_merge_people(&mut tx, keep_id, drop_id, actor).await?;
    ensure_no_retained_identity_evidence(&mut tx, drop_id).await?;
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
    prepare_merged_calendars(&mut tx, keep_id, drop_id).await?;
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
    let mut links: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.conrelid::regclass::text, a.attname::text FROM pg_constraint c \
         JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = ANY(c.conkey) \
         WHERE c.contype = 'f' AND c.confrelid = 'person'::regclass ORDER BY 1, 2",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(database)?;
    links.extend(
        crate::postgres::record_metadata::PERSON_LINKS
            .iter()
            .copied()
            .map(|(table, column)| (table.to_owned(), column.to_owned())),
    );
    links.sort();
    links.dedup();
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
    sqlx::query(
        "UPDATE person_phone d SET person_id = $1, updated_at = now(), \
         is_primary = d.is_primary AND NOT EXISTS \
         (SELECT 1 FROM person_phone k WHERE k.person_id = $1 AND k.is_primary) \
         WHERE d.person_id = $2",
    )
    .bind(keep_id)
    .bind(drop_id)
    .execute(&mut *tx)
    .await
    .map_err(database)?;
    sqlx::query("UPDATE person_project_role SET participation=$1 WHERE person_id=$2")
        .bind(crate::projects::participation_for_role(kept_role))
        .bind(keep_id)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
    relink_tagged_people(&mut tx, keep_id, drop_id).await?;
    move_subjects(&mut tx, keep_id, drop_id).await?;
    sqlx::query("DELETE FROM person WHERE id = $1")
        .bind(drop_id)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
    sqlx::query("SELECT public.graph_merge_person($1,$2)")
        .bind(keep_id)
        .bind(drop_id)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
    tx.commit().await.map_err(database)?;
    Ok(())
}

pub(crate) async fn merge(
    pg: &PostgresDb,
    _db: &ApplicationStore,
    keep_id: Uuid,
    drop_id: Uuid,
    actor: Option<Role>,
) -> Result<(), crate::person_merge::PersonMergeError> {
    match actor {
        Some(actor) => merge_rows_with_actor(pg, keep_id, drop_id, Some(actor)).await?,
        None => merge_rows(pg, keep_id, drop_id).await?,
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
    _db: &ApplicationStore,
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
    Ok(())
}

pub(crate) async fn create(
    pg: &PostgresDb,
    _db: &ApplicationStore,
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
    Ok(row)
}

pub(crate) async fn find_or_create(
    pg: &PostgresDb,
    db: &ApplicationStore,
    input: &NewPerson,
) -> Result<Person, PersonError> {
    if let Some(row) = by_email(pg, &input.email).await? {
        return Ok(row);
    }
    match create(pg, db, input).await {
        Err(PersonError::EmailTaken) => {
            let row = by_email(pg, &input.email)
                .await?
                .ok_or(PersonError::WriteReturnedNothing)?;
            Ok(row)
        }
        result => result,
    }
}
