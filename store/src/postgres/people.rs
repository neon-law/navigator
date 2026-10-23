//! PostgreSQL operations for the People directory.
//!
//! The opt-in People authority path uses the same UUIDs and role vocabulary
//! as the source store. Mailbox claims and their Person row are committed in
//! one PostgreSQL transaction; successful writes are then projected to
//! SurrealDB for callers whose non-People records still use its links.

use sqlx::{AssertSqlSafe, Postgres, QueryBuilder, Row, Transaction};
use uuid::Uuid;

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
        "SELECT {COLUMNS} FROM person WHERE email_lower = lower($1)"
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

pub(crate) async fn directory(
    pg: &PostgresDb,
    filter_name: &str,
    filter_email: &str,
    sort: &[(String, bool)],
) -> Result<Vec<Person>, PersonError> {
    let order = sort
        .iter()
        .filter_map(|(key, desc)| match key.as_str() {
            "name" => Some(format!("name {}", if *desc { "DESC" } else { "ASC" })),
            "email" => Some(format!("email {}", if *desc { "DESC" } else { "ASC" })),
            _ => None,
        })
        .collect::<Vec<_>>();
    let order = if order.is_empty() {
        "name ASC".to_string()
    } else {
        order.join(", ")
    };
    let sql = format!("SELECT {COLUMNS} FROM person WHERE ($1 = '' OR strpos(lower(name), $1) > 0) AND ($2 = '' OR strpos(email_lower, $2) > 0) ORDER BY {order}");
    let rows = sqlx::query(AssertSqlSafe(sql))
        .bind(filter_name.to_lowercase())
        .bind(filter_email.to_lowercase())
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
    let rows = sqlx::query(AssertSqlSafe(format!("SELECT {COLUMNS} FROM person WHERE ($1 = '' OR strpos(lower(name), $1) > 0) AND ($2 = '' OR strpos(email_lower, $2) > 0) ORDER BY name ASC LIMIT $3")))
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
        sync_surreal(db, &current, admitted).await?;
    } else {
        db.query("DELETE $id")
            .bind(("id", record_id("person", id)))
            .await
            .and_then(surrealdb::IndexedResults::check)
            .map_err(|_| PersonError::PostgresProjection("SurrealDB delete failed".into()))?;
        db.query("DELETE person_mailbox WHERE person_id=$id")
            .bind(("id", record_id("person", id)))
            .await
            .and_then(surrealdb::IndexedResults::check)
            .map_err(|_| {
                PersonError::PostgresProjection("SurrealDB mailbox cleanup failed".into())
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

#[cfg(test)]
#[path = "people_tests.rs"]
mod tests;

fn classify(error: sqlx::Error) -> PersonError {
    if let sqlx::Error::Database(ref database) = error {
        match database.constraint() {
            Some("person_email_lower_key" | "person_mailbox_pkey") => {
                return PersonError::EmailTaken
            }
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

async fn sync_surreal(db: &SurrealDb, person: &Person, admitted: bool) -> Result<(), PersonError> {
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
        .map_err(|_| PersonError::PostgresProjection("SurrealDB write failed".into()))?;
    let email_lower = person.email.trim().to_lowercase();
    db.query("DELETE person_mailbox WHERE person_id = $id; UPSERT type::record('person_mailbox', $email_lower) SET person_id = $id")
        .bind(("id", record_id("person", person.id))).bind(("email_lower", email_lower))
        .await.and_then(surrealdb::IndexedResults::check)
        .map_err(|_| PersonError::PostgresProjection("SurrealDB mailbox sync failed".into()))?;
    Ok(())
}

pub(crate) async fn create(
    pg: &PostgresDb,
    db: &SurrealDb,
    input: &NewPerson,
) -> Result<Person, PersonError> {
    let id = Uuid::now_v7();
    let mut tx = person_transaction(pg, id).await?;
    // The mailbox claim has a Person foreign key, so write the Person first.
    // Both rows still commit atomically, and `email_lower` remains the
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
