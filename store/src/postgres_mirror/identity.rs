//! Person, Entity, Entity Type, and Jurisdiction rows in the PostgreSQL
//! mirror.
//!
//! The same contract as the tenancy mirror: SurrealDB is read back after its
//! write, and PostgreSQL's copy is upserted or deleted to match. Each row
//! keeps its Surreal record UUID as its PostgreSQL `id`.
//!
//! An Entity names an Entity Type and a Jurisdiction. Those reference rows
//! have no write hook of their own: mirroring an Entity first mirrors each
//! one PostgreSQL does not yet hold, and [`reconcile`] settles the rest. A
//! link SurrealDB cannot resolve is refused with SQLSTATE 23503 and reported
//! by [`parity`].
//!
//! A unique value (an email, a sign-in subject, a reference name) that
//! PostgreSQL holds under another id is never cleared here: that row may be
//! one whose own change has not been mirrored yet. The upsert is refused
//! with SQLSTATE 23505, and [`reconcile`] retries it after the other row
//! converges.
//!
//! Reports name rows by key only, never by name or email.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use sqlx::Row;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use super::tenancy::{compare, images, remirror, select_sql, source, target, upsert_sql};
use super::{changed, micros, MirrorError, MirrorKey, Mirrored, ParityReport};
use crate::postgres::PostgresDb;
use crate::postgres_shadow::{compare_row, Comparison, ShadowKey};
use crate::surreal::{record_id, record_uuid, SurrealDb};

const JURISDICTION_COLUMNS: &[&str] = &[
    "id",
    "name",
    "code",
    "jurisdiction_type",
    "inserted_at",
    "updated_at",
];
const ENTITY_TYPE_COLUMNS: &[&str] = &["id", "name", "inserted_at", "updated_at"];
const ENTITY_COLUMNS: &[&str] = &[
    "id",
    "name",
    "entity_type_id",
    "jurisdiction_id",
    "phone",
    "url",
    "avatar_url",
    "xero_id",
    "firm_anchor_key",
    "inserted_at",
    "updated_at",
];
/// `email_lower` is absent: PostgreSQL generates it from `email`.
const PERSON_COLUMNS: &[&str] = &[
    "id",
    "name",
    "given_name",
    "family_name",
    "middle_name",
    "email",
    "oidc_subject",
    "microsoft_subject",
    "apple_subject",
    "role",
    "title",
    "phone",
    "xero_contact_id",
    "profile_image_url",
    "linkedin_url",
    "email_confirmed",
    "is_admitted",
    "inserted_at",
    "updated_at",
];

static JURISDICTION_UPSERT: LazyLock<String> =
    LazyLock::new(|| upsert_sql("jurisdiction", JURISDICTION_COLUMNS));
static ENTITY_TYPE_UPSERT: LazyLock<String> =
    LazyLock::new(|| upsert_sql("entity_type", ENTITY_TYPE_COLUMNS));
static ENTITY_UPSERT: LazyLock<String> = LazyLock::new(|| upsert_sql("entity", ENTITY_COLUMNS));
static PERSON_UPSERT: LazyLock<String> = LazyLock::new(|| upsert_sql("person", PERSON_COLUMNS));
static JURISDICTION_SELECT: LazyLock<String> =
    LazyLock::new(|| select_sql("jurisdiction", JURISDICTION_COLUMNS));
static ENTITY_TYPE_SELECT: LazyLock<String> =
    LazyLock::new(|| select_sql("entity_type", ENTITY_TYPE_COLUMNS));
static ENTITY_SELECT: LazyLock<String> = LazyLock::new(|| select_sql("entity", ENTITY_COLUMNS));
static PERSON_SELECT: LazyLock<String> = LazyLock::new(|| select_sql("person", PERSON_COLUMNS));
static ENTITY_BY_ID: LazyLock<String> =
    LazyLock::new(|| format!("{} WHERE id = $1", select_sql("entity", ENTITY_COLUMNS)));

/// Compare one Entity read by id with PostgreSQL's row, through the
/// mirror's Entity image.
pub(crate) async fn shadow_entity(
    pg: &PostgresDb,
    id: Uuid,
    source: Option<&crate::entities::Entity>,
) -> Result<Comparison, MirrorError> {
    compare_row(
        pg,
        ENTITY_BY_ID.as_str(),
        ShadowKey::Id(id),
        source.map(EntityImage::from_source),
        EntityImage::from_row,
        |image| image.id,
    )
    .await
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JurisdictionImage {
    id: Uuid,
    name: String,
    code: Option<String>,
    jurisdiction_type: Option<String>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl JurisdictionImage {
    fn from_source(row: &crate::jurisdictions::Jurisdiction) -> Self {
        Self {
            id: row.id,
            name: row.name.clone(),
            code: Some(row.code.clone()),
            jurisdiction_type: Some(row.jurisdiction_type.clone()),
            inserted_at: micros(row.inserted_at),
            updated_at: micros(row.updated_at),
        }
    }

    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            code: row.try_get("code")?,
            jurisdiction_type: row.try_get("jurisdiction_type")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EntityTypeImage {
    id: Uuid,
    name: String,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl EntityTypeImage {
    fn from_source(row: &crate::entity_types::EntityType) -> Self {
        Self {
            id: row.id,
            name: row.name.clone(),
            inserted_at: micros(row.inserted_at),
            updated_at: micros(row.updated_at),
        }
    }

    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EntityImage {
    id: Uuid,
    name: String,
    entity_type_id: Uuid,
    jurisdiction_id: Uuid,
    phone: Option<String>,
    url: Option<String>,
    avatar_url: Option<String>,
    xero_id: Option<String>,
    firm_anchor_key: Option<String>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl EntityImage {
    fn from_source(row: &crate::entities::Entity) -> Self {
        Self {
            id: row.id,
            name: row.name.clone(),
            entity_type_id: row.entity_type_id,
            jurisdiction_id: row.jurisdiction_id,
            phone: row.phone.clone(),
            url: row.url.clone(),
            avatar_url: row.avatar_url.clone(),
            xero_id: row.xero_id.clone(),
            firm_anchor_key: row.firm_anchor_key.clone(),
            inserted_at: micros(row.inserted_at),
            updated_at: micros(row.updated_at),
        }
    }

    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            entity_type_id: row.try_get("entity_type_id")?,
            jurisdiction_id: row.try_get("jurisdiction_id")?,
            phone: row.try_get("phone")?,
            url: row.try_get("url")?,
            avatar_url: row.try_get("avatar_url")?,
            xero_id: row.try_get("xero_id")?,
            firm_anchor_key: row.try_get("firm_anchor_key")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

/// The Person row as SurrealDB stores it, including `is_admitted`, which
/// [`crate::persons::Person`] does not carry. The role is copied as stored.
#[derive(SurrealValue)]
struct PersonSource {
    id: surrealdb::types::RecordId,
    name: String,
    given_name: Option<String>,
    family_name: Option<String>,
    middle_name: Option<String>,
    email: String,
    oidc_subject: Option<String>,
    microsoft_subject: Option<String>,
    apple_subject: Option<String>,
    role: String,
    title: Option<String>,
    phone: Option<String>,
    xero_contact_id: Option<String>,
    profile_image_url: Option<String>,
    linkedin_url: Option<String>,
    /// Absent on a row older than the field; the schema default is `false`.
    email_confirmed: Option<bool>,
    /// Absent on a row older than the field; the schema default is `true`.
    is_admitted: Option<bool>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

const PERSON_SOURCE_FIELDS: &str = "id, name, given_name, family_name, middle_name, email, \
    oidc_subject, microsoft_subject, apple_subject, role, title, phone, xero_contact_id, \
    profile_image_url, linkedin_url, email_confirmed, is_admitted, inserted_at, updated_at";

#[derive(Debug, Clone, PartialEq, Eq)]
struct PersonImage {
    id: Uuid,
    name: String,
    given_name: Option<String>,
    family_name: Option<String>,
    middle_name: Option<String>,
    email: String,
    oidc_subject: Option<String>,
    microsoft_subject: Option<String>,
    apple_subject: Option<String>,
    role: String,
    title: Option<String>,
    phone: Option<String>,
    xero_contact_id: Option<String>,
    profile_image_url: Option<String>,
    linkedin_url: Option<String>,
    email_confirmed: bool,
    is_admitted: bool,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl PersonImage {
    /// `None` for a record whose key is not a native UUID; no workspace
    /// writer mints one, and it has no PostgreSQL identity.
    fn from_source(row: PersonSource) -> Option<Self> {
        Some(Self {
            id: record_uuid(&row.id)?,
            name: row.name,
            given_name: row.given_name,
            family_name: row.family_name,
            middle_name: row.middle_name,
            email: row.email,
            oidc_subject: row.oidc_subject,
            microsoft_subject: row.microsoft_subject,
            apple_subject: row.apple_subject,
            role: row.role,
            title: row.title,
            phone: row.phone,
            xero_contact_id: row.xero_contact_id,
            profile_image_url: row.profile_image_url,
            linkedin_url: row.linkedin_url,
            email_confirmed: row.email_confirmed.unwrap_or(false),
            is_admitted: row.is_admitted.unwrap_or(true),
            inserted_at: micros(row.inserted_at.into()),
            updated_at: micros(row.updated_at.into()),
        })
    }

    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            given_name: row.try_get("given_name")?,
            family_name: row.try_get("family_name")?,
            middle_name: row.try_get("middle_name")?,
            email: row.try_get("email")?,
            oidc_subject: row.try_get("oidc_subject")?,
            microsoft_subject: row.try_get("microsoft_subject")?,
            apple_subject: row.try_get("apple_subject")?,
            role: row.try_get("role")?,
            title: row.try_get("title")?,
            phone: row.try_get("phone")?,
            xero_contact_id: row.try_get("xero_contact_id")?,
            profile_image_url: row.try_get("profile_image_url")?,
            linkedin_url: row.try_get("linkedin_url")?,
            email_confirmed: row.try_get("email_confirmed")?,
            is_admitted: row.try_get("is_admitted")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

async fn source_person(surreal: &SurrealDb, id: Uuid) -> Result<Option<PersonImage>, MirrorError> {
    let mut response = surreal
        .query(format!(
            "SELECT {PERSON_SOURCE_FIELDS} FROM ONLY $id LIMIT 1"
        ))
        .bind(("id", record_id("person", id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(source)?;
    let row: Option<PersonSource> = response.take(0).map_err(source)?;
    Ok(row.and_then(PersonImage::from_source))
}

async fn source_people(surreal: &SurrealDb) -> Result<Vec<PersonImage>, MirrorError> {
    let mut response = surreal
        .query(format!("SELECT {PERSON_SOURCE_FIELDS} FROM person"))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(source)?;
    let rows: Vec<PersonSource> = response.take(0).map_err(source)?;
    Ok(rows
        .into_iter()
        .filter_map(PersonImage::from_source)
        .collect())
}

/// Make PostgreSQL's copy of one identity row match SurrealDB's.
pub(super) async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    key: &MirrorKey,
) -> Result<Mirrored, MirrorError> {
    match key {
        MirrorKey::Jurisdiction(id) => mirror_jurisdiction(surreal, pg, *id).await,
        MirrorKey::EntityType(id) => mirror_entity_type(surreal, pg, *id).await,
        MirrorKey::Entity(id) => {
            let source = crate::entities::find_by_id(surreal, *id)
                .await
                .map_err(source)?;
            let Some(entity) = source else {
                return delete(pg, "DELETE FROM entity WHERE id = $1", *id).await;
            };
            let image = EntityImage::from_source(&entity);
            if !present(pg, ENTITY_TYPE_PRESENT, image.entity_type_id).await? {
                mirror_entity_type(surreal, pg, image.entity_type_id).await?;
            }
            if !present(pg, JURISDICTION_PRESENT, image.jurisdiction_id).await? {
                mirror_jurisdiction(surreal, pg, image.jurisdiction_id).await?;
            }
            upsert_entity(pg, &image).await
        }
        MirrorKey::Person(id) => {
            if crate::persons::postgres_people_authority_configured() {
                return Ok(Mirrored::Unchanged);
            }
            match source_person(surreal, *id).await? {
                Some(person) => upsert_person(pg, &person).await,
                None => delete(pg, "DELETE FROM person WHERE id = $1", *id).await,
            }
        }
        // `super::mirror` routes only identity keys here.
        _ => Err(MirrorError::Source),
    }
}

/// Mirror a Person, Entity, or Jurisdiction another mirrored row names,
/// only when PostgreSQL does not hold it yet. A held row is left for its
/// own hook.
pub(super) async fn ensure(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    key: MirrorKey,
) -> Result<(), MirrorError> {
    let (sql, id) = match key {
        MirrorKey::Jurisdiction(id) => (JURISDICTION_PRESENT, id),
        MirrorKey::Entity(id) => (ENTITY_PRESENT, id),
        MirrorKey::Person(id) => (PERSON_PRESENT, id),
        _ => return Ok(()),
    };
    if !present(pg, sql, id).await? {
        mirror(surreal, pg, &key).await?;
    }
    Ok(())
}

const JURISDICTION_PRESENT: &str = "SELECT EXISTS (SELECT 1 FROM jurisdiction WHERE id = $1)";
const ENTITY_TYPE_PRESENT: &str = "SELECT EXISTS (SELECT 1 FROM entity_type WHERE id = $1)";
const ENTITY_PRESENT: &str = "SELECT EXISTS (SELECT 1 FROM entity WHERE id = $1)";
const PERSON_PRESENT: &str = "SELECT EXISTS (SELECT 1 FROM person WHERE id = $1)";

/// Whether PostgreSQL holds the row one of the `*_PRESENT` queries names.
async fn present(pg: &PostgresDb, sql: &'static str, id: Uuid) -> Result<bool, MirrorError> {
    Ok(sqlx::query_scalar(sql).bind(id).fetch_one(&pg.0).await?)
}

async fn delete(pg: &PostgresDb, sql: &'static str, id: Uuid) -> Result<Mirrored, MirrorError> {
    let deleted = sqlx::query(sql).bind(id).execute(&pg.0).await?;
    Ok(changed(deleted.rows_affected(), Mirrored::Deleted))
}

async fn mirror_jurisdiction(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Mirrored, MirrorError> {
    let source = crate::jurisdictions::find_by_id(surreal, id)
        .await
        .map_err(source)?;
    let Some(row) = source else {
        return delete(pg, "DELETE FROM jurisdiction WHERE id = $1", id).await;
    };
    let row = JurisdictionImage::from_source(&row);
    let written = sqlx::query(JURISDICTION_UPSERT.as_str())
        .bind(row.id)
        .bind(&row.name)
        .bind(&row.code)
        .bind(&row.jurisdiction_type)
        .bind(row.inserted_at)
        .bind(row.updated_at)
        .execute(&pg.0)
        .await?;
    Ok(changed(written.rows_affected(), Mirrored::Written))
}

async fn mirror_entity_type(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Mirrored, MirrorError> {
    let source = crate::entity_types::find_by_id(surreal, id)
        .await
        .map_err(source)?;
    let Some(row) = source else {
        return delete(pg, "DELETE FROM entity_type WHERE id = $1", id).await;
    };
    let row = EntityTypeImage::from_source(&row);
    let written = sqlx::query(ENTITY_TYPE_UPSERT.as_str())
        .bind(row.id)
        .bind(&row.name)
        .bind(row.inserted_at)
        .bind(row.updated_at)
        .execute(&pg.0)
        .await?;
    Ok(changed(written.rows_affected(), Mirrored::Written))
}

async fn upsert_entity(pg: &PostgresDb, entity: &EntityImage) -> Result<Mirrored, MirrorError> {
    let written = sqlx::query(ENTITY_UPSERT.as_str())
        .bind(entity.id)
        .bind(&entity.name)
        .bind(entity.entity_type_id)
        .bind(entity.jurisdiction_id)
        .bind(&entity.phone)
        .bind(&entity.url)
        .bind(&entity.avatar_url)
        .bind(&entity.xero_id)
        .bind(&entity.firm_anchor_key)
        .bind(entity.inserted_at)
        .bind(entity.updated_at)
        .execute(&pg.0)
        .await?;
    Ok(changed(written.rows_affected(), Mirrored::Written))
}

async fn upsert_person(pg: &PostgresDb, person: &PersonImage) -> Result<Mirrored, MirrorError> {
    let written = sqlx::query(PERSON_UPSERT.as_str())
        .bind(person.id)
        .bind(&person.name)
        .bind(&person.given_name)
        .bind(&person.family_name)
        .bind(&person.middle_name)
        .bind(&person.email)
        .bind(&person.oidc_subject)
        .bind(&person.microsoft_subject)
        .bind(&person.apple_subject)
        .bind(&person.role)
        .bind(&person.title)
        .bind(&person.phone)
        .bind(&person.xero_contact_id)
        .bind(&person.profile_image_url)
        .bind(&person.linkedin_url)
        .bind(person.email_confirmed)
        .bind(person.is_admitted)
        .bind(person.inserted_at)
        .bind(person.updated_at)
        .execute(&pg.0)
        .await?;
    Ok(changed(written.rows_affected(), Mirrored::Written))
}

fn infallible<S, I>(image: impl Fn(&S) -> I) -> impl Fn(&S) -> Result<I, MirrorError> {
    move |row| Ok(image(row))
}

/// Compare every Jurisdiction, Entity Type, Entity, and Person in both
/// stores, references before the rows that name them.
///
/// # Errors
///
/// Either store could not be read.
pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let jurisdictions = crate::jurisdictions::list_all(surreal)
        .await
        .map_err(source)?;
    let entity_types = crate::entity_types::list(surreal, &[])
        .await
        .map_err(source)?;
    let entities = crate::entities::all(surreal).await.map_err(source)?;
    let people = source_people(surreal).await?;
    let people: BTreeMap<MirrorKey, Option<PersonImage>> = people
        .into_iter()
        .map(|person| (MirrorKey::Person(person.id), Some(person)))
        .collect();

    let tables = vec![
        compare(
            "jurisdiction",
            &images(
                &jurisdictions,
                |row| MirrorKey::Jurisdiction(row.id),
                infallible(JurisdictionImage::from_source),
            ),
            &target(
                pg,
                JURISDICTION_SELECT.as_str(),
                JurisdictionImage::from_row,
                |row| MirrorKey::Jurisdiction(row.id),
            )
            .await?,
        ),
        compare(
            "entity_type",
            &images(
                &entity_types,
                |row| MirrorKey::EntityType(row.id),
                infallible(EntityTypeImage::from_source),
            ),
            &target(
                pg,
                ENTITY_TYPE_SELECT.as_str(),
                EntityTypeImage::from_row,
                |row| MirrorKey::EntityType(row.id),
            )
            .await?,
        ),
        compare(
            "entity",
            &images(
                &entities,
                |row| MirrorKey::Entity(row.id),
                infallible(EntityImage::from_source),
            ),
            &target(pg, ENTITY_SELECT.as_str(), EntityImage::from_row, |row| {
                MirrorKey::Entity(row.id)
            })
            .await?,
        ),
        compare(
            "person",
            &people,
            &target(pg, PERSON_SELECT.as_str(), PersonImage::from_row, |row| {
                MirrorKey::Person(row.id)
            })
            .await?,
        ),
    ];
    Ok(ParityReport { tables })
}

/// Re-mirror every divergent identity row from SurrealDB, then compare
/// again. A row that fails both passes stays in the returned report.
///
/// # Errors
///
/// Either store could not be read for the comparison.
pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let before = parity(surreal, pg).await?;
    remirror(surreal, pg, &before).await;
    parity(surreal, pg).await
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
