//! Typed identity-detail imports and content-free mailbox reconciliation.
//!
//! Captured rows insert only missing identities. Existing PostgreSQL rows and
//! mailbox owners remain unchanged on drift or replay. Mailbox comparisons count
//! individual claims while reporting only their Person UUIDs. The separate
//! Entity Role adapter retains its graph-authority contract.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use sqlx::postgres::{PgArguments, PgRow};
use sqlx::Row;
use surrealdb::types::{Datetime, RecordId, RecordIdKey, SurrealValue};
use uuid::Uuid;

use super::tenancy::{compare, remirror, select_sql, source, timestamp, upsert_sql};
use super::{changed, identity, micros, MirrorError, MirrorKey, Mirrored, ParityReport};
use crate::postgres::PostgresDb;
use crate::surreal::{record_id, record_uuid, SurrealDb};

type PgQuery<'q> = sqlx::query::Query<'q, sqlx::Postgres, PgArguments>;

/// One UUID-keyed table: how SurrealDB's row reads, how PostgreSQL's row
/// reads, and which identity rows it names.
trait Detail: Sized + PartialEq {
    /// The row as SurrealDB stores it.
    type Source: SurrealValue;
    const TABLE: &'static str;
    /// PostgreSQL columns, `id` first, in bind order.
    const COLUMNS: &'static [&'static str];
    /// The Surreal projection [`Self::Source`] reads.
    const SOURCE_FIELDS: &'static str;

    fn key(id: Uuid) -> MirrorKey;
    /// Decode the UUID identity required by a captured row.
    fn record(source: &Self::Source) -> Option<Uuid>;
    fn from_source(source: Self::Source) -> Result<Self, MirrorError>;
    fn from_row(row: &PgRow) -> Result<Self, sqlx::Error>;
    fn bind<'q>(&self, query: PgQuery<'q>) -> PgQuery<'q>;
    /// The Person, Entity, and Jurisdiction rows this row names.
    fn parents(&self) -> Vec<MirrorKey>;
}

fn link(record: &RecordId) -> Result<Uuid, MirrorError> {
    record_uuid(record).ok_or(MirrorError::Source)
}

fn optional_link(record: Option<&RecordId>) -> Result<Option<Uuid>, MirrorError> {
    record.map(link).transpose()
}

fn at(datetime: Datetime) -> DateTime<Utc> {
    micros(datetime.into())
}

fn optional_timestamp(text: Option<&str>) -> Result<Option<DateTime<Utc>>, MirrorError> {
    text.map(timestamp).transpose()
}

#[derive(SurrealValue)]
struct AddressSource {
    id: RecordId,
    person_id: Option<RecordId>,
    entity_id: Option<RecordId>,
    line1: String,
    line2: Option<String>,
    city: String,
    region: String,
    postal_code: String,
    country: String,
    inserted_at: Datetime,
    updated_at: Datetime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AddressImage {
    id: Uuid,
    person_id: Option<Uuid>,
    entity_id: Option<Uuid>,
    line1: String,
    line2: Option<String>,
    city: String,
    region: String,
    postal_code: String,
    country: String,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl Detail for AddressImage {
    type Source = AddressSource;
    const TABLE: &'static str = "address";
    const COLUMNS: &'static [&'static str] = &[
        "id",
        "person_id",
        "entity_id",
        "line1",
        "line2",
        "city",
        "region",
        "postal_code",
        "country",
        "inserted_at",
        "updated_at",
    ];
    const SOURCE_FIELDS: &'static str = "id, person_id, entity_id, line1, line2, city, region, \
        postal_code, country, inserted_at, updated_at";

    fn key(id: Uuid) -> MirrorKey {
        MirrorKey::Address(id)
    }

    fn record(source: &AddressSource) -> Option<Uuid> {
        record_uuid(&source.id)
    }

    fn from_source(row: AddressSource) -> Result<Self, MirrorError> {
        Ok(Self {
            id: link(&row.id)?,
            person_id: optional_link(row.person_id.as_ref())?,
            entity_id: optional_link(row.entity_id.as_ref())?,
            line1: row.line1,
            line2: row.line2,
            city: row.city,
            region: row.region,
            postal_code: row.postal_code,
            country: row.country,
            inserted_at: at(row.inserted_at),
            updated_at: at(row.updated_at),
        })
    }

    fn from_row(row: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            person_id: row.try_get("person_id")?,
            entity_id: row.try_get("entity_id")?,
            line1: row.try_get("line1")?,
            line2: row.try_get("line2")?,
            city: row.try_get("city")?,
            region: row.try_get("region")?,
            postal_code: row.try_get("postal_code")?,
            country: row.try_get("country")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }

    fn bind<'q>(&self, query: PgQuery<'q>) -> PgQuery<'q> {
        query
            .bind(self.id)
            .bind(self.person_id)
            .bind(self.entity_id)
            .bind(self.line1.clone())
            .bind(self.line2.clone())
            .bind(self.city.clone())
            .bind(self.region.clone())
            .bind(self.postal_code.clone())
            .bind(self.country.clone())
            .bind(self.inserted_at)
            .bind(self.updated_at)
    }

    fn parents(&self) -> Vec<MirrorKey> {
        self.person_id
            .map(MirrorKey::Person)
            .into_iter()
            .chain(self.entity_id.map(MirrorKey::Entity))
            .collect()
    }
}

#[derive(SurrealValue)]
struct CredentialSource {
    id: RecordId,
    person_id: RecordId,
    jurisdiction_id: RecordId,
    license_number: String,
    inserted_at: Datetime,
    updated_at: Datetime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CredentialImage {
    id: Uuid,
    person_id: Uuid,
    jurisdiction_id: Uuid,
    license_number: String,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl Detail for CredentialImage {
    type Source = CredentialSource;
    const TABLE: &'static str = "credential";
    const COLUMNS: &'static [&'static str] = &[
        "id",
        "person_id",
        "jurisdiction_id",
        "license_number",
        "inserted_at",
        "updated_at",
    ];
    const SOURCE_FIELDS: &'static str =
        "id, person_id, jurisdiction_id, license_number, inserted_at, updated_at";

    fn key(id: Uuid) -> MirrorKey {
        MirrorKey::Credential(id)
    }

    fn record(source: &CredentialSource) -> Option<Uuid> {
        record_uuid(&source.id)
    }

    fn from_source(row: CredentialSource) -> Result<Self, MirrorError> {
        Ok(Self {
            id: link(&row.id)?,
            person_id: link(&row.person_id)?,
            jurisdiction_id: link(&row.jurisdiction_id)?,
            license_number: row.license_number,
            inserted_at: at(row.inserted_at),
            updated_at: at(row.updated_at),
        })
    }

    fn from_row(row: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            person_id: row.try_get("person_id")?,
            jurisdiction_id: row.try_get("jurisdiction_id")?,
            license_number: row.try_get("license_number")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }

    fn bind<'q>(&self, query: PgQuery<'q>) -> PgQuery<'q> {
        query
            .bind(self.id)
            .bind(self.person_id)
            .bind(self.jurisdiction_id)
            .bind(self.license_number.clone())
            .bind(self.inserted_at)
            .bind(self.updated_at)
    }

    fn parents(&self) -> Vec<MirrorKey> {
        vec![
            MirrorKey::Person(self.person_id),
            MirrorKey::Jurisdiction(self.jurisdiction_id),
        ]
    }
}

#[derive(SurrealValue)]
struct EmailTokenSource {
    id: RecordId,
    person_id: RecordId,
    email: String,
    purpose: String,
    token_hash: String,
    expires_at: Datetime,
    used_at: Option<Datetime>,
    inserted_at: Datetime,
    updated_at: Datetime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EmailTokenImage {
    id: Uuid,
    person_id: Uuid,
    email: String,
    purpose: String,
    token_hash: String,
    expires_at: DateTime<Utc>,
    used_at: Option<DateTime<Utc>>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl Detail for EmailTokenImage {
    type Source = EmailTokenSource;
    const TABLE: &'static str = "email_token";
    const COLUMNS: &'static [&'static str] = &[
        "id",
        "person_id",
        "email",
        "purpose",
        "token_hash",
        "expires_at",
        "used_at",
        "inserted_at",
        "updated_at",
    ];
    const SOURCE_FIELDS: &'static str = "id, person_id, email, purpose, token_hash, expires_at, \
        used_at, inserted_at, updated_at";

    fn key(id: Uuid) -> MirrorKey {
        MirrorKey::EmailToken(id)
    }

    fn record(source: &EmailTokenSource) -> Option<Uuid> {
        record_uuid(&source.id)
    }

    fn from_source(row: EmailTokenSource) -> Result<Self, MirrorError> {
        Ok(Self {
            id: link(&row.id)?,
            person_id: link(&row.person_id)?,
            email: row.email,
            purpose: row.purpose,
            token_hash: row.token_hash,
            expires_at: at(row.expires_at),
            used_at: row.used_at.map(at),
            inserted_at: at(row.inserted_at),
            updated_at: at(row.updated_at),
        })
    }

    fn from_row(row: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            person_id: row.try_get("person_id")?,
            email: row.try_get("email")?,
            purpose: row.try_get("purpose")?,
            token_hash: row.try_get("token_hash")?,
            expires_at: row.try_get("expires_at")?,
            used_at: row.try_get("used_at")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }

    fn bind<'q>(&self, query: PgQuery<'q>) -> PgQuery<'q> {
        query
            .bind(self.id)
            .bind(self.person_id)
            .bind(self.email.clone())
            .bind(self.purpose.clone())
            .bind(self.token_hash.clone())
            .bind(self.expires_at)
            .bind(self.used_at)
            .bind(self.inserted_at)
            .bind(self.updated_at)
    }

    fn parents(&self) -> Vec<MirrorKey> {
        vec![MirrorKey::Person(self.person_id)]
    }
}

/// Every instant is RFC 3339 text in SurrealDB.
#[derive(SurrealValue)]
struct DelegateSource {
    id: RecordId,
    delegate_person_id: RecordId,
    subject_person_id: RecordId,
    basis: String,
    instrument_reference: Option<String>,
    granted_by_person_id: RecordId,
    granted_at: String,
    subject_notified_at: Option<String>,
    subject_notified_channel: Option<String>,
    revoked_at: Option<String>,
    revoked_by_person_id: Option<RecordId>,
    inserted_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DelegateImage {
    id: Uuid,
    delegate_person_id: Uuid,
    subject_person_id: Uuid,
    basis: String,
    instrument_reference: Option<String>,
    granted_by_person_id: Uuid,
    granted_at: DateTime<Utc>,
    subject_notified_at: Option<DateTime<Utc>>,
    subject_notified_channel: Option<String>,
    revoked_at: Option<DateTime<Utc>>,
    revoked_by_person_id: Option<Uuid>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl Detail for DelegateImage {
    type Source = DelegateSource;
    const TABLE: &'static str = "person_delegate";
    const COLUMNS: &'static [&'static str] = &[
        "id",
        "delegate_person_id",
        "subject_person_id",
        "basis",
        "instrument_reference",
        "granted_by_person_id",
        "granted_at",
        "subject_notified_at",
        "subject_notified_channel",
        "revoked_at",
        "revoked_by_person_id",
        "inserted_at",
        "updated_at",
    ];
    const SOURCE_FIELDS: &'static str = "id, delegate_person_id, subject_person_id, basis, \
        instrument_reference, granted_by_person_id, granted_at, subject_notified_at, \
        subject_notified_channel, revoked_at, revoked_by_person_id, inserted_at, updated_at";

    fn key(id: Uuid) -> MirrorKey {
        MirrorKey::PersonDelegate(id)
    }

    fn record(source: &DelegateSource) -> Option<Uuid> {
        record_uuid(&source.id)
    }

    fn from_source(row: DelegateSource) -> Result<Self, MirrorError> {
        Ok(Self {
            id: link(&row.id)?,
            delegate_person_id: link(&row.delegate_person_id)?,
            subject_person_id: link(&row.subject_person_id)?,
            basis: row.basis,
            instrument_reference: row.instrument_reference,
            granted_by_person_id: link(&row.granted_by_person_id)?,
            granted_at: timestamp(&row.granted_at)?,
            subject_notified_at: optional_timestamp(row.subject_notified_at.as_deref())?,
            subject_notified_channel: row.subject_notified_channel,
            revoked_at: optional_timestamp(row.revoked_at.as_deref())?,
            revoked_by_person_id: optional_link(row.revoked_by_person_id.as_ref())?,
            inserted_at: timestamp(&row.inserted_at)?,
            updated_at: timestamp(&row.updated_at)?,
        })
    }

    fn from_row(row: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            delegate_person_id: row.try_get("delegate_person_id")?,
            subject_person_id: row.try_get("subject_person_id")?,
            basis: row.try_get("basis")?,
            instrument_reference: row.try_get("instrument_reference")?,
            granted_by_person_id: row.try_get("granted_by_person_id")?,
            granted_at: row.try_get("granted_at")?,
            subject_notified_at: row.try_get("subject_notified_at")?,
            subject_notified_channel: row.try_get("subject_notified_channel")?,
            revoked_at: row.try_get("revoked_at")?,
            revoked_by_person_id: row.try_get("revoked_by_person_id")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }

    fn bind<'q>(&self, query: PgQuery<'q>) -> PgQuery<'q> {
        query
            .bind(self.id)
            .bind(self.delegate_person_id)
            .bind(self.subject_person_id)
            .bind(self.basis.clone())
            .bind(self.instrument_reference.clone())
            .bind(self.granted_by_person_id)
            .bind(self.granted_at)
            .bind(self.subject_notified_at)
            .bind(self.subject_notified_channel.clone())
            .bind(self.revoked_at)
            .bind(self.revoked_by_person_id)
            .bind(self.inserted_at)
            .bind(self.updated_at)
    }

    fn parents(&self) -> Vec<MirrorKey> {
        let mut people = vec![
            self.delegate_person_id,
            self.subject_person_id,
            self.granted_by_person_id,
        ];
        people.extend(self.revoked_by_person_id);
        people.dedup();
        people.into_iter().map(MirrorKey::Person).collect()
    }
}

#[derive(SurrealValue)]
struct ExternalIdentitySource {
    id: RecordId,
    person_id: RecordId,
    system: String,
    external_id: String,
    handle: Option<String>,
    inserted_at: Datetime,
    updated_at: Datetime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExternalIdentityImage {
    id: Uuid,
    person_id: Uuid,
    system: String,
    external_id: String,
    handle: Option<String>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl Detail for ExternalIdentityImage {
    type Source = ExternalIdentitySource;
    const TABLE: &'static str = "person_external_identity";
    const COLUMNS: &'static [&'static str] = &[
        "id",
        "person_id",
        "system",
        "external_id",
        "handle",
        "inserted_at",
        "updated_at",
    ];
    const SOURCE_FIELDS: &'static str =
        "id, person_id, system, external_id, handle, inserted_at, updated_at";

    fn key(id: Uuid) -> MirrorKey {
        MirrorKey::ExternalIdentity(id)
    }

    fn record(source: &ExternalIdentitySource) -> Option<Uuid> {
        record_uuid(&source.id)
    }

    fn from_source(row: ExternalIdentitySource) -> Result<Self, MirrorError> {
        Ok(Self {
            id: link(&row.id)?,
            person_id: link(&row.person_id)?,
            system: row.system,
            external_id: row.external_id,
            handle: row.handle,
            inserted_at: at(row.inserted_at),
            updated_at: at(row.updated_at),
        })
    }

    fn from_row(row: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            person_id: row.try_get("person_id")?,
            system: row.try_get("system")?,
            external_id: row.try_get("external_id")?,
            handle: row.try_get("handle")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }

    fn bind<'q>(&self, query: PgQuery<'q>) -> PgQuery<'q> {
        query
            .bind(self.id)
            .bind(self.person_id)
            .bind(self.system.clone())
            .bind(self.external_id.clone())
            .bind(self.handle.clone())
            .bind(self.inserted_at)
            .bind(self.updated_at)
    }

    fn parents(&self) -> Vec<MirrorKey> {
        vec![MirrorKey::Person(self.person_id)]
    }
}

/// SurrealDB's rows of one table, or the one row `id` names. Each keeps
/// its key; an unreadable image remains a divergent keyed row.
async fn sources<D: Detail>(
    surreal: &SurrealDb,
    id: Option<Uuid>,
) -> Result<Vec<(Uuid, Result<D, MirrorError>)>, MirrorError> {
    let from = if id.is_some() { "$id" } else { D::TABLE };
    let mut query = surreal.query(format!("SELECT {} FROM {from}", D::SOURCE_FIELDS));
    if let Some(id) = id {
        query = query.bind(("id", record_id(D::TABLE, id)));
    }
    let mut response = query
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(source)?;
    let rows: Vec<D::Source> = response.take(0).map_err(source)?;
    let mut images = BTreeMap::new();
    for row in rows {
        let id = D::record(&row).ok_or(MirrorError::Source)?;
        if images.insert(id, D::from_source(row)).is_some() {
            return Err(MirrorError::Source);
        }
    }
    Ok(images.into_iter().collect())
}

/// Make PostgreSQL's copy of one UUID-keyed row match SurrealDB's.
async fn mirror_detail<D: Detail>(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Mirrored, MirrorError> {
    let Some((_, image)) = sources::<D>(surreal, Some(id)).await?.pop() else {
        return Ok(Mirrored::Unchanged);
    };
    let image = image?;
    for parent in image.parents() {
        identity::ensure(surreal, pg, parent).await?;
    }
    let written = image
        .bind(sqlx::query(sqlx::AssertSqlSafe(upsert_sql(
            D::TABLE,
            D::COLUMNS,
        ))))
        .execute(&pg.0)
        .await?;
    Ok(changed(written.rows_affected(), Mirrored::Written))
}

async fn compare_detail<D: Detail>(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<super::TableParity, MirrorError> {
    let source: BTreeMap<MirrorKey, Option<D>> = sources::<D>(surreal, None)
        .await?
        .into_iter()
        .map(|(id, image)| (D::key(id), image.ok()))
        .collect();
    let target: BTreeMap<MirrorKey, D> =
        sqlx::query(sqlx::AssertSqlSafe(select_sql(D::TABLE, D::COLUMNS)))
            .fetch_all(&pg.0)
            .await?
            .iter()
            .map(|row| {
                row.try_get::<Uuid, _>("id")
                    .map(D::key)
                    .and_then(|key| Ok((key, D::from_row(row)?)))
            })
            .collect::<Result<_, sqlx::Error>>()?;
    Ok(compare(D::TABLE, &source, &target))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EntityRoleImage {
    person_id: Uuid,
    entity_id: Uuid,
    role: String,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl EntityRoleImage {
    fn from_source(row: crate::entity_roles::EntityRole) -> Self {
        Self {
            person_id: row.person_id,
            entity_id: row.entity_id,
            role: row.role,
            inserted_at: micros(row.inserted_at),
            updated_at: micros(row.updated_at),
        }
    }

    fn key(&self) -> MirrorKey {
        MirrorKey::EntityRole {
            person_id: self.person_id,
            entity_id: self.entity_id,
            role: self.role.clone(),
        }
    }
}

/// Keyed on the tie itself: the PostgreSQL `id` is PostgreSQL's own.
const ENTITY_ROLE_UPSERT: &str = "INSERT INTO entity_role \
    (person_id, entity_id, role, inserted_at, updated_at) VALUES ($1, $2, $3, $4, $5) \
    ON CONFLICT (person_id, entity_id, role) DO UPDATE SET \
    inserted_at = EXCLUDED.inserted_at, updated_at = EXCLUDED.updated_at \
    WHERE (entity_role.inserted_at, entity_role.updated_at) IS DISTINCT FROM \
    (EXCLUDED.inserted_at, EXCLUDED.updated_at)";

async fn mirror_entity_role(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    person_id: Uuid,
    entity_id: Uuid,
    role: &str,
) -> Result<Mirrored, MirrorError> {
    let found = crate::entity_roles::find_in_surreal(surreal, person_id, entity_id, role)
        .await
        .map_err(source)?;
    let Some(tie) = found else {
        // A tie SurrealDB lacks is no evidence once PostgreSQL holds the
        // table: copy, never delete.
        if crate::postgres_graph::holds("entity_role") {
            return Err(MirrorError::GraphAuthority);
        }
        let deleted = sqlx::query(
            "DELETE FROM entity_role WHERE person_id = $1 AND entity_id = $2 AND role = $3",
        )
        .bind(person_id)
        .bind(entity_id)
        .bind(role)
        .execute(&pg.0)
        .await?;
        return Ok(changed(deleted.rows_affected(), Mirrored::Deleted));
    };
    let tie = EntityRoleImage::from_source(tie);
    identity::ensure(surreal, pg, MirrorKey::Person(tie.person_id)).await?;
    identity::ensure(surreal, pg, MirrorKey::Entity(tie.entity_id)).await?;
    let written = sqlx::query(ENTITY_ROLE_UPSERT)
        .bind(tie.person_id)
        .bind(tie.entity_id)
        .bind(&tie.role)
        .bind(tie.inserted_at)
        .bind(tie.updated_at)
        .execute(&pg.0)
        .await?;
    Ok(changed(written.rows_affected(), Mirrored::Written))
}

async fn compare_entity_roles(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<super::TableParity, MirrorError> {
    let source: BTreeMap<MirrorKey, Option<EntityRoleImage>> =
        crate::entity_roles::all_in_surreal(surreal)
            .await
            .map_err(source)?
            .into_iter()
            .map(|tie| {
                let image = EntityRoleImage::from_source(tie);
                (image.key(), Some(image))
            })
            .collect();
    let target: BTreeMap<MirrorKey, EntityRoleImage> =
        sqlx::query("SELECT person_id, entity_id, role, inserted_at, updated_at FROM entity_role")
            .fetch_all(&pg.0)
            .await?
            .iter()
            .map(|row| {
                let image = EntityRoleImage {
                    person_id: row.try_get("person_id")?,
                    entity_id: row.try_get("entity_id")?,
                    role: row.try_get("role")?,
                    inserted_at: row.try_get("inserted_at")?,
                    updated_at: row.try_get("updated_at")?,
                };
                Ok((image.key(), image))
            })
            .collect::<Result<_, sqlx::Error>>()?;
    Ok(compare("entity_role", &source, &target))
}

/// A mailbox claim. The record key is the mailbox itself.
#[derive(SurrealValue)]
struct MailboxSource {
    id: RecordId,
    person_id: RecordId,
}

/// Every mailbox claim in SurrealDB, by the Person holding it, or only
/// `person`'s. An unreadable natural key or holder refuses the capture instead
/// of silently lowering its census.
async fn source_claims(
    surreal: &SurrealDb,
    person: Option<Uuid>,
) -> Result<BTreeMap<Uuid, BTreeSet<String>>, MirrorError> {
    let mut query = surreal.query(match person {
        Some(_) => "SELECT id, person_id FROM person_mailbox WHERE person_id = $person",
        None => "SELECT id, person_id FROM person_mailbox",
    });
    if let Some(person) = person {
        query = query.bind(("person", record_id("person", person)));
    }
    let mut response = query
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(source)?;
    let rows: Vec<MailboxSource> = response.take(0).map_err(source)?;
    let mut claims: BTreeMap<Uuid, BTreeSet<String>> = BTreeMap::new();
    for row in rows {
        let (RecordIdKey::String(mailbox), Some(holder)) =
            (&row.id.key, record_uuid(&row.person_id))
        else {
            return Err(MirrorError::Source);
        };
        if mailbox.is_empty() || *mailbox != mailbox.trim().to_lowercase() {
            return Err(MirrorError::Source);
        }
        claims.entry(holder).or_default().insert(mailbox.clone());
    }
    Ok(claims)
}

/// Insert captured mailbox claims without releasing or taking existing claims.
async fn mirror_mailboxes(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    person: Uuid,
) -> Result<Mirrored, MirrorError> {
    let claims: Vec<String> = source_claims(surreal, Some(person))
        .await?
        .remove(&person)
        .unwrap_or_default()
        .into_iter()
        .collect();
    if !claims.is_empty() {
        identity::ensure(surreal, pg, MirrorKey::Person(person)).await?;
    }
    let mut tx = pg.0.begin().await?;
    let mut claimed = 0;
    for mailbox in &claims {
        claimed += sqlx::query(
            "INSERT INTO person_mailbox (id, person_id) VALUES ($1, $2) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(mailbox)
        .bind(person)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    tx.commit().await?;
    Ok(changed(claimed, Mirrored::Written))
}

async fn compare_mailboxes(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<super::TableParity, MirrorError> {
    let source = source_claims(surreal, None).await?;
    let mut target: BTreeMap<Uuid, BTreeSet<String>> = BTreeMap::new();
    for row in sqlx::query("SELECT id, person_id FROM person_mailbox")
        .fetch_all(&pg.0)
        .await?
    {
        target
            .entry(row.try_get("person_id")?)
            .or_default()
            .insert(row.try_get("id")?);
    }
    let mut claims = BTreeMap::<&String, (Option<Uuid>, Option<Uuid>)>::new();
    for (person, mailboxes) in &source {
        for mailbox in mailboxes {
            claims.entry(mailbox).or_default().0 = Some(*person);
        }
    }
    for (person, mailboxes) in &target {
        for mailbox in mailboxes {
            claims.entry(mailbox).or_default().1 = Some(*person);
        }
    }
    let mut divergent: Vec<_> = claims
        .into_values()
        .filter_map(|(source, target)| {
            if source == target {
                None
            } else {
                source.or(target).map(MirrorKey::PersonMailbox)
            }
        })
        .collect();
    // Keep one entry per differing natural claim, without exposing mailbox order.
    divergent.sort();
    Ok(super::TableParity {
        table: "person_mailbox",
        source_rows: source.values().map(BTreeSet::len).sum(),
        target_rows: target.values().map(BTreeSet::len).sum(),
        divergent,
    })
}

/// Make PostgreSQL's copy of one identity-detail row match SurrealDB's.
pub(super) async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    key: &MirrorKey,
) -> Result<Mirrored, MirrorError> {
    if !super::source_import_allowed(pg, key.table()).await? {
        return Ok(Mirrored::Unchanged);
    }

    if key.held_by_people_authority() {
        return Ok(Mirrored::Unchanged);
    }
    match key {
        MirrorKey::Address(id) => mirror_detail::<AddressImage>(surreal, pg, *id).await,
        MirrorKey::Credential(id) => mirror_detail::<CredentialImage>(surreal, pg, *id).await,
        MirrorKey::EmailToken(id) => mirror_detail::<EmailTokenImage>(surreal, pg, *id).await,
        MirrorKey::PersonDelegate(id) => mirror_detail::<DelegateImage>(surreal, pg, *id).await,
        MirrorKey::ExternalIdentity(id) => {
            mirror_detail::<ExternalIdentityImage>(surreal, pg, *id).await
        }
        MirrorKey::EntityRole {
            person_id,
            entity_id,
            role,
        } => mirror_entity_role(surreal, pg, *person_id, *entity_id, role).await,
        MirrorKey::PersonMailbox(person) => mirror_mailboxes(surreal, pg, *person).await,
        // `super::mirror` routes only identity-detail keys here.
        _ => Err(MirrorError::Source),
    }
}

/// Compare only the six detail tables in the pending identity scope.
pub(crate) async fn scoped_parity(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<ParityReport, MirrorError> {
    Ok(ParityReport {
        tables: vec![
            compare_detail::<AddressImage>(surreal, pg).await?,
            compare_detail::<CredentialImage>(surreal, pg).await?,
            compare_detail::<EmailTokenImage>(surreal, pg).await?,
            compare_detail::<DelegateImage>(surreal, pg).await?,
            compare_detail::<ExternalIdentityImage>(surreal, pg).await?,
            compare_mailboxes(surreal, pg).await?,
        ],
    })
}

async fn pending_detail<D: Detail>(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<Option<super::TableParity>, MirrorError> {
    if crate::postgres_backfill::family::active_for_table(pg, D::TABLE).await? {
        return Ok(None);
    }
    compare_detail::<D>(surreal, pg).await.map(Some)
}

/// Compare identity detail history not already held by scoped authority.
///
/// # Errors
/// Either store or the persisted ownership ledger could not be read.
pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let mut tables: Vec<_> = [
        pending_detail::<AddressImage>(surreal, pg).await?,
        pending_detail::<CredentialImage>(surreal, pg).await?,
        pending_detail::<EmailTokenImage>(surreal, pg).await?,
    ]
    .into_iter()
    .flatten()
    .collect();
    let graph_authoritative = crate::postgres_graph::holds("entity_role");
    if !graph_authoritative || super::compares_authoritative(pg).await? {
        let entity_roles = compare_entity_roles(surreal, pg).await?;
        tables.push(if graph_authoritative {
            super::only_missing(pg, entity_roles).await?
        } else {
            entity_roles
        });
    }
    tables.extend(
        [
            pending_detail::<DelegateImage>(surreal, pg).await?,
            pending_detail::<ExternalIdentityImage>(surreal, pg).await?,
        ]
        .into_iter()
        .flatten(),
    );
    if !crate::postgres_backfill::family::active_for_table(pg, "person_mailbox").await? {
        tables.push(compare_mailboxes(surreal, pg).await?);
    }
    Ok(ParityReport { tables })
}

/// Re-mirror every divergent identity-detail row from SurrealDB, then
/// compare again. A row that fails both passes stays in the returned
/// report.
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
#[path = "identity_detail_tests.rs"]
mod tests;
