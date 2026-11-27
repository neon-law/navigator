//! PostgreSQL-backed legal organizations and the Entity conflict-graph nodes.
//!
//! `entity_commands` validates referenced Entity Types and Jurisdictions and
//! decides which names identify the Firm's own Entity. Namesakes remain valid;
//! only the normalized `firm_anchor_key` has exclusive ownership.
//!
//! PostgreSQL's `entity_firm_anchor_key` unique index serializes competing
//! ownership writes. A collision is [`EntityError::FirmAnchorTaken`]. Protected
//! update and delete statements preserve ownership across competing mutations.
//! The native Entity integration tests exercise the accessors and HTTP doors.
//!
//! Explicit Source compatibility writes use the keyed [`CLAIM_TABLE`] claim
//! and shared retry policy. Historical capture/import retains those records;
//! ordinary native access uses the admitted PostgreSQL pool.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use sqlx::{postgres::PgRow, AssertSqlSafe, Row};

use crate::application_store::ApplicationStore;
use crate::postgres::identity::{self as authority, AuthorityError};
use crate::postgres::PostgresDb;

/// One legal organization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entity {
    pub id: Uuid,
    /// Display name. Deliberately **not** unique — namesakes are real —
    /// except for the firm anchor, which [`Entity::is_firm_anchor`]
    /// reports and PostgreSQL's partial unique index enforces.
    pub name: String,
    /// The kind of organization. A `record<entity_type>` link in the
    /// engine, surfaced here as the id every caller already held.
    pub entity_type_id: Uuid,
    /// Where the organization is domiciled. A `record<jurisdiction>`
    /// link in the engine.
    pub jurisdiction_id: Uuid,
    /// Main phone line. `None` until set by the bulk-contact importer
    /// or an admin edit.
    pub phone: Option<String>,
    /// Canonical website URL (https), canonicalized by the importer.
    pub url: Option<String>,
    /// The admin-only avatar route for this entity's photo in the private
    /// documents bucket (`entities/{id}/avatars/…`), or `None` when it has
    /// none. Set only by [`set_avatar_url`], never by [`create`] or
    /// [`update`].
    pub avatar_url: Option<String>,
    /// The Xero `ContactID` for this entity, when linked — an external
    /// identity, not a credential, mirroring `person.xero_contact_id`. Unlike
    /// that column, this one is editable straight from the admin form: an
    /// entity's Xero contact is entered directly rather than synced by the
    /// billing workflow.
    pub xero_id: Option<String>,
    /// Private supplemental attributes, such as `ein`, stored on this Entity.
    pub metadata: Option<serde_json::Value>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The lowercased name when this row is the firm anchor, `None`
    /// otherwise. Read-only to callers: `entity_commands` computes it.
    pub firm_anchor_key: Option<String>,
}

impl Entity {
    /// Whether this row carries the firm-anchor key — the protected
    /// row that may not be forked, renamed, or deleted.
    #[must_use]
    pub fn is_firm_anchor(&self) -> bool {
        self.firm_anchor_key.is_some()
    }
}

/// The projection every read shares, so one field list describes the
/// row and a new column cannot reach [`EntityRow`] from only one query.
const SELECT: &str = "id, name, entity_type_id, jurisdiction_id, phone, url, avatar_url, \
                      xero_id, metadata, firm_anchor_key, inserted_at, updated_at";

/// What a write stores. `firm_anchor_key` is computed by
/// `entity_commands`, never supplied by a request body.
#[derive(Debug, Clone)]
pub struct NewEntity {
    pub name: String,
    pub entity_type_id: Uuid,
    pub jurisdiction_id: Uuid,
    pub phone: Option<String>,
    pub url: Option<String>,
    pub xero_id: Option<String>,
    pub firm_anchor_key: Option<String>,
}

/// Errors reading or writing an entity.
#[derive(Debug, thiserror::Error)]
pub enum EntityError {
    /// The write collided with `entity_firm_anchor` — a row already
    /// holds the firm anchor's identity, and a second would be born
    /// protected. See this module's header.
    #[error("the firm entity already exists")]
    FirmAnchorTaken,
    /// A write reported success but returned no row, or one this
    /// module could not read back.
    #[error("writing an entity returned no usable row")]
    WriteReturnedNothing,
    /// PostgreSQL identity authority is configured and failed.
    #[error(transparent)]
    Authority(#[from] AuthorityError),
}

fn from_pg(row: &PgRow) -> Result<Entity, sqlx::Error> {
    Ok(Entity {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        entity_type_id: row.try_get("entity_type_id")?,
        jurisdiction_id: row.try_get("jurisdiction_id")?,
        phone: row.try_get("phone")?,
        url: row.try_get("url")?,
        avatar_url: row.try_get("avatar_url")?,
        xero_id: row.try_get("xero_id")?,
        metadata: row.try_get("metadata")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
        firm_anchor_key: row.try_get("firm_anchor_key")?,
    })
}

fn pg_error(error: sqlx::Error) -> EntityError {
    if authority::violates(&error, "entity_firm_anchor_key") {
        EntityError::FirmAnchorTaken
    } else {
        EntityError::Authority(error.into())
    }
}

/// Entities from PostgreSQL, `sql` being the statement after `SELECT
/// <columns>`, each row read through [`from_pg`].
async fn pg_rows(
    pg: &PostgresDb,
    sql: &str,
    binds: Vec<PgBind>,
) -> Result<Vec<Entity>, EntityError> {
    let mut query = sqlx::query(AssertSqlSafe(format!("SELECT {SELECT} {sql}")));
    for bind in binds {
        query = match bind {
            PgBind::Id(id) => query.bind(id),
            PgBind::Ids(ids) => query.bind(ids),
            PgBind::Text(text) => query.bind(text),
        };
    }
    let rows = query.fetch_all(&pg.0).await.map_err(pg_error)?;
    rows.iter()
        .map(from_pg)
        .collect::<Result<_, _>>()
        .map_err(pg_error)
}

/// A value [`pg_rows`] binds, in order.
enum PgBind {
    Id(Uuid),
    Ids(Vec<Uuid>),
    Text(String),
}

/// Run one entity write in PostgreSQL and project each row it returns.
async fn pg_write(
    pg: &PostgresDb,
    _db: &ApplicationStore,
    query: sqlx::query::Query<'_, sqlx::Postgres, sqlx::postgres::PgArguments>,
) -> Result<Option<Entity>, EntityError> {
    let row = query.fetch_optional(&pg.0).await.map_err(pg_error)?;
    let entity = row.as_ref().map(from_pg).transpose().map_err(pg_error)?;
    Ok(entity)
}

/// The entity currently holding `key`, or `None` when the anchor is free.
///
/// This reads the claim rather than `entity.firm_anchor_key`, so it
/// answers "who owns the claim" rather than "which row looks like the
/// anchor" — and it is a direct record read, not the scan
/// [`firm_anchor_exists`] performs.
///
/// # Errors
///
/// [`EntityError::Db`] if the lookup fails.
pub async fn firm_anchor_holder(
    _db: &ApplicationStore,
    key: &str,
) -> Result<Option<Uuid>, EntityError> {
    let pg = authority::authority().await?;
    let rows = Box::pin(pg_rows(
        &pg,
        "FROM entity WHERE firm_anchor_key = $1",
        vec![PgBind::Text(key.to_string())],
    ))
    .await?;
    Ok(rows.first().map(|entity| entity.id))
}

/// Resolve one entity by id.
///
/// # Errors
///
/// [`EntityError::Db`] if the lookup fails.
pub async fn find_by_id(_db: &ApplicationStore, id: Uuid) -> Result<Option<Entity>, EntityError> {
    let pg = authority::authority().await?;
    let rows = Box::pin(pg_rows(
        &pg,
        "FROM entity WHERE id = $1",
        vec![PgBind::Id(id)],
    ))
    .await?;
    Ok(rows.into_iter().next())
}

/// Resolve many entities by id in one round trip — the bulk read a
/// conflict finding's labels need, so a report does not issue one query
/// per reached node.
///
/// # Errors
///
/// [`EntityError::Db`] if the lookup fails.
pub async fn find_by_ids(_db: &ApplicationStore, ids: &[Uuid]) -> Result<Vec<Entity>, EntityError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let pg = authority::authority().await?;
    Box::pin(pg_rows(
        &pg,
        "FROM entity WHERE id = ANY($1)",
        vec![PgBind::Ids(ids.to_vec())],
    ))
    .await
}

/// Resolve an entity by exact `name`.
///
/// Names are deliberately non-unique, so this returns the first match
/// the engine reports and is only meaningful for names the caller knows
/// to be singular — the firm anchor (`store::seed`) or a fixture.
///
/// # Errors
///
/// [`EntityError::Db`] if the lookup fails.
pub async fn find_by_name(
    _db: &ApplicationStore,
    name: &str,
) -> Result<Option<Entity>, EntityError> {
    let pg = authority::authority().await?;
    let rows = Box::pin(pg_rows(
        &pg,
        "FROM entity WHERE name = $1 ORDER BY id LIMIT 1",
        vec![PgBind::Text(name.trim().to_string())],
    ))
    .await?;
    Ok(rows.into_iter().next())
}

/// Resolve a name-only reference only when it identifies exactly one Entity.
pub(crate) async fn find_unique_by_name(
    _db: &ApplicationStore,
    name: &str,
) -> Result<Option<Entity>, EntityError> {
    let pg = authority::authority().await?;
    let mut rows = Box::pin(pg_rows(
        &pg,
        "FROM entity WHERE name = $1 LIMIT 2",
        vec![PgBind::Text(name.trim().to_string())],
    ))
    .await?;
    Ok(if rows.len() == 1 { rows.pop() } else { None })
}

/// Resolve an entity by the `(name, entity_type_id)` pair the canonical
/// seed keys on.
///
/// Names alone are not unique, and the seed's fixtures are distinguished
/// by their type — a `Human` and an `LLC` may legitimately share a name.
/// This is the seed's find half of find-or-create, not a general lookup.
///
/// # Errors
///
/// [`EntityError::Db`] if the lookup fails.
pub async fn find_by_name_and_type(
    _db: &ApplicationStore,
    name: &str,
    entity_type_id: Uuid,
) -> Result<Option<Entity>, EntityError> {
    let pg = authority::authority().await?;
    let rows = Box::pin(pg_rows(
        &pg,
        "FROM entity WHERE name = $1 AND entity_type_id = $2 ORDER BY id LIMIT 1",
        vec![
            PgBind::Text(name.trim().to_string()),
            PgBind::Id(entity_type_id),
        ],
    ))
    .await?;
    Ok(rows.into_iter().next())
}

/// Resolve an entity by the full `(name, entity_type_id,
/// jurisdiction_id)` triple.
///
/// The bulk-contact importer's identity: two organizations may share a
/// name, and even a name and a type, but the same name as the same kind
/// of entity in the same jurisdiction is one organization. Distinct from
/// [`find_by_name_and_type`], which the seed uses precisely *because* it
/// ignores the jurisdiction — it exists to repair one that drifted.
///
/// # Errors
///
/// [`EntityError::Db`] if the lookup fails.
pub async fn find_by_identity(
    _db: &ApplicationStore,
    name: &str,
    entity_type_id: Uuid,
    jurisdiction_id: Uuid,
) -> Result<Option<Entity>, EntityError> {
    let pg = authority::authority().await?;
    let rows = Box::pin(pg_rows(
        &pg,
        "FROM entity WHERE name = $1 AND entity_type_id = $2 AND jurisdiction_id = $3 \
         ORDER BY id LIMIT 1",
        vec![
            PgBind::Text(name.trim().to_string()),
            PgBind::Id(entity_type_id),
            PgBind::Id(jurisdiction_id),
        ],
    ))
    .await?;
    Ok(rows.into_iter().next())
}

/// Point an existing entity at a different jurisdiction, leaving every
/// other field alone.
///
/// The canonical seed's repair path: a persisted database can outlive a
/// memory-backed local engine whose re-seeded jurisdictions carry fresh
/// ids, and a link the engine never validated then dangles. Narrow on
/// purpose — a full [`update`] here would need the caller to restate
/// fields it has no opinion about.
///
/// # Errors
///
/// [`EntityError::Db`] if the write fails.
pub async fn repoint_jurisdiction(
    db: &ApplicationStore,
    id: Uuid,
    jurisdiction_id: Uuid,
) -> Result<Option<Entity>, EntityError> {
    let pg = authority::authority().await?;
    let sql = format!(
        "UPDATE entity SET jurisdiction_id = $2, updated_at = now() \
         WHERE id = $1 RETURNING {SELECT}"
    );
    let query = sqlx::query(AssertSqlSafe(sql))
        .bind(id)
        .bind(jurisdiction_id);
    Box::pin(pg_write(&pg, db, query)).await
}

/// Set — or clear — the entity's avatar route, leaving every other field
/// alone. Narrow like [`repoint_jurisdiction`]: the avatar upload handler has
/// no opinion about the entity's name, type, or jurisdiction, so it should
/// not have to restate them through [`update`].
///
/// # Errors
///
/// [`EntityError::Db`] if the write fails.
pub async fn set_avatar_url(
    db: &ApplicationStore,
    id: Uuid,
    avatar_url: Option<String>,
) -> Result<Option<Entity>, EntityError> {
    let pg = authority::authority().await?;
    let sql = format!(
        "UPDATE entity SET avatar_url = $2, updated_at = now() WHERE id = $1 RETURNING {SELECT}"
    );
    let query = sqlx::query(AssertSqlSafe(sql)).bind(id).bind(avatar_url);
    Box::pin(pg_write(&pg, db, query)).await
}

/// Fill a missing EIN from an approved public firm seed, preserving existing attributes.
pub(crate) async fn seed_ein(
    db: &ApplicationStore,
    id: Uuid,
    ein: &str,
) -> Result<(), EntityError> {
    let pg = authority::authority().await?;
    let sql = format!(
        "UPDATE entity SET metadata = COALESCE(metadata, '{{}}'::jsonb) \
         || jsonb_build_object('ein', $2::text), updated_at = now() \
         WHERE id = $1 AND metadata->'ein' IS NULL RETURNING {SELECT}"
    );
    let query = sqlx::query(AssertSqlSafe(sql)).bind(id).bind(ein);
    Box::pin(pg_write(&pg, db, query)).await?;
    Ok(())
}

/// Move `firm_anchor_key` onto — or off — the entity carrying `id`,
/// returning the saved row.
///
/// Every other door computes the key from the name at the instant it
/// writes the row, which is right for a create and for an operator
/// edit. It is not enough for a *rename of the configured anchor
/// itself*: the rows already exist, the seed skips them, and the delete
/// guard reads this column rather than the name — so an anchor moved in
/// `seed::FIRM_ENTITY_NAME` would leave the outgoing firm protected and
/// the incoming one deletable. This is the seam the seed reconciles
/// through, and it is deliberately narrow: it writes one column and
/// takes the key already computed by
/// [`crate::entity_commands::firm_anchor_key`] rather than a name it
/// would have to re-derive.
///
/// # Errors
///
/// [`EntityError::FirmAnchorTaken`] when another row already holds
/// `key`, and [`EntityError::Db`] for anything else.
pub async fn set_firm_anchor_key(
    db: &ApplicationStore,
    id: Uuid,
    key: Option<String>,
) -> Result<Option<Entity>, EntityError> {
    let pg = authority::authority().await?;
    // The `entity_firm_anchor_key` unique index is the claim here.
    let sql = format!(
        "UPDATE entity SET firm_anchor_key = $2, updated_at = now() \
         WHERE id = $1 RETURNING {SELECT}"
    );
    let query = sqlx::query(AssertSqlSafe(sql)).bind(id).bind(key);
    Box::pin(pg_write(&pg, db, query)).await
}

/// Whether any row already carries `key` as its firm-anchor key.
///
/// The existence half of the guard `entity_commands` runs before a
/// create. It is **not** what makes the guard safe — the claim
/// [`take_claim`] writes is — but it turns the common case into a clean
/// validation error rather than a write that has to fail first.
///
/// It reads `entity.firm_anchor_key`, so it answers "does a row look like
/// the anchor". [`firm_anchor_holder`] answers the different question of
/// who holds the claim, and that is the one a race turns on.
///
/// # Errors
///
/// [`EntityError::Db`] if the lookup fails.
pub async fn firm_anchor_exists(db: &ApplicationStore, key: &str) -> Result<bool, EntityError> {
    Ok(firm_anchor_holder(db, key).await?.is_some())
}

/// Every entity, ordered by name so a listing is stably ordered before
/// any caller applies its own sort.
///
/// # Errors
///
/// [`EntityError::Db`] if the lookup fails.
pub async fn all(_db: &ApplicationStore) -> Result<Vec<Entity>, EntityError> {
    let pg = authority::authority().await?;
    Box::pin(pg_rows(&pg, "FROM entity ORDER BY name, id", Vec::new())).await
}

/// Write a new entity row under a fresh v7 id.
///
/// # Errors
///
/// [`EntityError::FirmAnchorTaken`] when the row would fork the firm
/// anchor, and [`EntityError::Db`] for anything else.
pub async fn create(db: &ApplicationStore, input: &NewEntity) -> Result<Entity, EntityError> {
    upsert_with_id(db, Uuid::now_v7(), input).await
}

pub(crate) async fn create_with_metadata(
    db: &ApplicationStore,
    input: &NewEntity,
    metadata: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Result<Entity, EntityError> {
    upsert_with_metadata(db, Uuid::now_v7(), input, metadata).await
}

/// Create or update the entity carrying `id`.
///
/// [`create`] owns id generation, which is right for a real create. A
/// seed or a fixture that must exist under a known id cannot use it.
/// Idempotent, so re-running a seed reconciles the row instead of
/// duplicating it.
///
/// # Errors
///
/// [`EntityError::FirmAnchorTaken`] when the row would fork the firm
/// anchor, and [`EntityError::Db`] for anything else.
pub async fn upsert_with_id(
    db: &ApplicationStore,
    id: Uuid,
    input: &NewEntity,
) -> Result<Entity, EntityError> {
    upsert_with_metadata(db, id, input, None).await
}

async fn upsert_with_metadata(
    db: &ApplicationStore,
    id: Uuid,
    input: &NewEntity,
    metadata: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Result<Entity, EntityError> {
    let pg = authority::authority().await?;
    let sql = format!(
        "INSERT INTO entity (id, name, entity_type_id, jurisdiction_id, phone, url, \
         xero_id, firm_anchor_key, metadata) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, \
         entity_type_id = EXCLUDED.entity_type_id, jurisdiction_id = EXCLUDED.jurisdiction_id, \
         phone = EXCLUDED.phone, url = EXCLUDED.url, xero_id = EXCLUDED.xero_id, \
         firm_anchor_key = EXCLUDED.firm_anchor_key, \
         metadata = COALESCE(EXCLUDED.metadata, entity.metadata), updated_at = now() \
         RETURNING {SELECT}"
    );
    let query = sqlx::query(AssertSqlSafe(sql))
        .bind(id)
        .bind(input.name.trim())
        .bind(input.entity_type_id)
        .bind(input.jurisdiction_id)
        .bind(input.phone.clone())
        .bind(input.url.clone())
        .bind(input.xero_id.clone())
        .bind(input.firm_anchor_key.clone())
        .bind(metadata.cloned().map(serde_json::Value::Object));
    Box::pin(pg_write(&pg, db, query))
        .await?
        .ok_or(EntityError::WriteReturnedNothing)
}

/// Replace every field of the entity carrying `id`, returning the saved
/// row — or `None` when no such row exists **or the write would rename
/// the firm anchor**.
///
/// The `WHERE` is the rename half of the guard [`delete_unless_firm_anchor`]
/// carries for deletes, and it exists for the same reason: a concurrent
/// write can mint the anchor on this row after the caller read it as
/// ordinary, and a rename that decided on that earlier read would move
/// the freshly-protected firm away. The condition therefore travels with
/// the write rather than being checked before it.
///
/// It admits exactly two cases: the name is unchanged (so the anchor
/// stays editable — an operator can still correct its entity type), or
/// the row is not the anchor. A caller that needs to tell "refused" from
/// "no such row" re-reads; this seam reports both as `None` because from
/// its side they are the same outcome — nothing was written.
///
/// # Errors
///
/// [`EntityError::FirmAnchorTaken`] when the write would fork the firm
/// anchor, and [`EntityError::Db`] for anything else.
pub async fn update(
    db: &ApplicationStore,
    id: Uuid,
    input: &NewEntity,
) -> Result<Option<Entity>, EntityError> {
    update_with_metadata(db, id, input, None).await
}

/// Update Entity fields and optionally replace its metadata in the same write.
/// Omission preserves metadata; an empty object clears it.
///
/// # Errors
///
/// Returns the same database and firm-anchor errors as [`update`].
pub(crate) async fn update_with_metadata(
    db: &ApplicationStore,
    id: Uuid,
    input: &NewEntity,
    metadata: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Result<Option<Entity>, EntityError> {
    update_fields(
        db,
        id,
        input,
        metadata,
        ContactUpdate {
            phone: true,
            url: true,
        },
    )
    .await
}

pub(crate) struct ContactUpdate {
    pub phone: bool,
    pub url: bool,
}

/// Apply requested contact values while keeping omitted columns at their
/// current values at write time.
pub(crate) async fn update_fields(
    db: &ApplicationStore,
    id: Uuid,
    input: &NewEntity,
    metadata: Option<&serde_json::Map<String, serde_json::Value>>,
    contact: ContactUpdate,
) -> Result<Option<Entity>, EntityError> {
    let pg = authority::authority().await?;
    // A Firm anchor keeps its name: the rename is refused as `None`.
    let sql = format!(
        "UPDATE entity SET name = $2, entity_type_id = $3, jurisdiction_id = $4, \
         xero_id = $5, firm_anchor_key = $6, \
         phone = CASE WHEN $7 THEN $8 ELSE phone END, \
         url = CASE WHEN $9 THEN $10 ELSE url END, \
         metadata = COALESCE($11, metadata), updated_at = now() \
         WHERE id = $1 AND (name = $2 OR firm_anchor_key IS NULL) RETURNING {SELECT}"
    );
    let query = sqlx::query(AssertSqlSafe(sql))
        .bind(id)
        .bind(input.name.trim())
        .bind(input.entity_type_id)
        .bind(input.jurisdiction_id)
        .bind(input.xero_id.clone())
        .bind(input.firm_anchor_key.clone())
        .bind(contact.phone)
        .bind(input.phone.clone())
        .bind(contact.url)
        .bind(input.url.clone())
        .bind(metadata.cloned().map(serde_json::Value::Object));
    Box::pin(pg_write(&pg, db, query)).await
}

/// Delete the entity carrying `id` **unless it is the firm anchor**,
/// returning the removed row — or `None` when nothing was removed,
/// because the row was already gone or because it was protected.
///
/// There is deliberately no unguarded delete. The protection has to be
/// part of the delete statement rather than a check the caller ran
/// first: a concurrent rename can mint the anchor in the window between
/// the two, and a delete that decided on the earlier read would then
/// remove the freshly-protected firm. The `WHERE`
/// closes that window by making the refusal a property of the row at the instant
/// of the write.
///
/// `firm_anchor_key` is the discriminator rather than the name, because
/// it is what every door that mints an anchor sets — and what the
/// `entity_firm_anchor` index already trusts.
///
/// Callers must still check [`dependents`] first: nothing in the engine
/// refuses a delete that strands a reference, because a `record<>` link
/// is not a foreign key.
///
/// # Errors
///
/// [`EntityError::Db`] if the delete fails.
pub async fn delete_unless_firm_anchor(
    db: &ApplicationStore,
    id: Uuid,
) -> Result<Option<Entity>, EntityError> {
    let pg = authority::authority().await?;
    let sql =
        format!("DELETE FROM entity WHERE id = $1 AND firm_anchor_key IS NULL RETURNING {SELECT}");
    Box::pin(pg_write(&pg, db, sqlx::query(AssertSqlSafe(sql)).bind(id))).await
}

/// The tables that reference `id`, with how many rows each holds. Empty
/// when the entity is free to delete.
///
/// Counts precede deletion so the operator sees which references to detach,
/// rather than only a foreign-key refusal. PostgreSQL counts come from one
/// committed statement snapshot; explicit Source reads retain their query path.
///
/// # Errors
///
/// [`EntityError::Authority`] for PostgreSQL admission or query failure;
/// [`EntityError::Db`] for a Source query failure.
pub async fn dependents(_db: &ApplicationStore, id: Uuid) -> Result<Vec<Dependent>, EntityError> {
    let counts = {
        let pg = authority::authority().await?;
        // Match the canonical typed graph endpoints, including their table
        // discriminator: a Person with the same UUID is not this Entity.
        let (projects, addresses, roles, relationships, references): (i64, i64, i64, i64, i64) =
            sqlx::query_as(
                "SELECT (SELECT count(*) FROM project WHERE entity_id=$1), \
                 (SELECT count(*) FROM address WHERE entity_id=$1), \
                 (SELECT count(*) FROM entity_role WHERE entity_id=$1), \
                 (SELECT count(*) FROM relationship WHERE \
                    (\"in\"->>'tb'='entity' AND \"in\"->'id'->>'Uuid'=$2) OR \
                    (\"out\"->>'tb'='entity' AND \"out\"->'id'->>'Uuid'=$2)), \
                 (SELECT count(*) FROM project_reference WHERE entity_id=$1)",
            )
            .bind(id)
            .bind(id.to_string())
            .fetch_one(pg.pool())
            .await
            .map_err(pg_error)?;
        [projects, addresses, roles, relationships, references]
    };
    Ok(DEPENDENT_TABLES
        .iter()
        .zip(counts)
        .filter_map(|(&(singular, plural), counted)| {
            let count = usize::try_from(counted).unwrap_or(0);
            (count > 0).then_some(Dependent {
                singular,
                plural,
                count,
            })
        })
        .collect())
}

/// One table still pointing at an entity, and how many of its rows do.
///
/// Both spellings travel because the refusal is shown to a person: "1
/// address" and "2 addresses" are the same fact, and deriving one from
/// the other by trimming an `s` gets "1 addresse".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependent {
    pub singular: &'static str,
    pub plural: &'static str,
    pub count: usize,
}

impl Dependent {
    /// The noun this count should be printed with.
    #[must_use]
    pub fn noun(&self) -> &'static str {
        if self.count == 1 {
            self.singular
        } else {
            self.plural
        }
    }
}

/// The reference categories in the order [`dependents`] counts them,
/// shared by its PostgreSQL and explicit Source queries,
/// so the two cannot drift apart silently.
const DEPENDENT_TABLES: [(&str, &str); 5] = [
    ("project", "projects"),
    ("address", "addresses"),
    ("entity role", "entity roles"),
    ("relationship", "relationships"),
    ("project reference", "project references"),
];

#[cfg(test)]
mod tests {
    use super::{
        all, create, delete_unless_firm_anchor, dependents, find_by_id, find_by_ids, find_by_name,
        firm_anchor_exists, seed_ein, set_avatar_url, update, update_with_metadata, EntityError,
        NewEntity,
    };
    use crate::application_store::ApplicationStore;
    use uuid::Uuid;

    async fn input(db: &ApplicationStore, name: &str) -> NewEntity {
        let (entity_type_id, jurisdiction_id) = crate::test_support::seed_entity_parents(db).await;
        NewEntity {
            name: name.into(),
            entity_type_id,
            jurisdiction_id,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        }
    }

    async fn anchor(db: &ApplicationStore, name: &str) -> NewEntity {
        NewEntity {
            firm_anchor_key: Some(name.to_lowercase()),
            ..input(db, name).await
        }
    }

    #[tokio::test]
    async fn a_created_entity_reads_back_by_id_and_name() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let created = create(&db, &input(&db, "  Beta LLC  ").await)
                .await
                .unwrap();

            assert_eq!(
                created.name, "Beta LLC",
                "the name is trimmed on the way in"
            );
            assert_eq!(
                find_by_id(&db, created.id).await.unwrap(),
                Some(created.clone())
            );
            assert_eq!(find_by_name(&db, "Beta LLC").await.unwrap(), Some(created));
            assert_eq!(find_by_name(&db, "Nothing Co").await.unwrap(), None);
        })
        .await;
    }

    #[tokio::test]
    async fn set_avatar_url_writes_and_clears_leaving_every_other_field_alone() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let created = create(&db, &input(&db, "Delta LLC").await).await.unwrap();
            assert_eq!(created.avatar_url, None);

            let key = format!("entities/{}/avatars/{}.jpg", created.id, created.id);
            let with_avatar = set_avatar_url(&db, created.id, Some(key.clone()))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(with_avatar.avatar_url.as_deref(), Some(key.as_str()));
            assert_eq!(with_avatar.name, "Delta LLC", "no other field moved");

            let cleared = set_avatar_url(&db, created.id, None)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(cleared.avatar_url, None);
        })
        .await;
    }

    #[tokio::test]
    async fn seeded_ein_fills_missing_values_and_preserves_other_metadata_and_existing_ein() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let row = create(&db, &input(&db, "Example LLC").await).await.unwrap();
            seed_ein(&db, row.id, "12-3456789").await.unwrap();
            assert_eq!(
                find_by_id(&db, row.id).await.unwrap().unwrap().metadata,
                Some(serde_json::json!({"ein": "12-3456789"}))
            );
            let metadata = serde_json::json!({"registration": {"state": "NV"}});
            update_with_metadata(
                &db,
                row.id,
                &input(&db, "Example LLC").await,
                metadata.as_object(),
            )
            .await
            .unwrap()
            .unwrap();
            seed_ein(&db, row.id, "12-3456789").await.unwrap();
            let saved = find_by_id(&db, row.id).await.unwrap().unwrap();
            assert_eq!(
                saved.metadata,
                Some(serde_json::json!({
                    "ein": "12-3456789", "registration": {"state": "NV"}
                }))
            );
            seed_ein(&db, row.id, "98-7654321").await.unwrap();
            assert_eq!(find_by_id(&db, row.id).await.unwrap().unwrap(), saved);
        })
        .await;
    }

    #[tokio::test]
    async fn ordinary_updates_preserve_entity_metadata() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let created = create(&db, &input(&db, "Example LLC").await).await.unwrap();
            assert_eq!(created.metadata, None);
            let metadata = serde_json::json!({"ein": "12-3456789", "nested": {"label": "sample"}});
            update_with_metadata(
                &db,
                created.id,
                &input(&db, "Example LLC").await,
                metadata.as_object(),
            )
            .await
            .unwrap()
            .unwrap();
            let updated = update(&db, created.id, &input(&db, "Renamed LLC").await)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(updated.metadata, Some(metadata));
        })
        .await;
    }

    /// Entity Type and Jurisdiction IDs stay UUIDs in the API while
    /// PostgreSQL enforces each parent reference with a foreign key.
    #[tokio::test]
    async fn the_reference_links_round_trip_as_the_ids_the_caller_supplied() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let wanted = input(&db, "Gamma Trust").await;
            let created = create(&db, &wanted).await.unwrap();

            assert_eq!(created.entity_type_id, wanted.entity_type_id);
            assert_eq!(created.jurisdiction_id, wanted.jurisdiction_id);
            let read_back = find_by_id(&db, created.id).await.unwrap().unwrap();
            assert_eq!(read_back.entity_type_id, wanted.entity_type_id);
            assert_eq!(read_back.jurisdiction_id, wanted.jurisdiction_id);
        })
        .await;
    }

    /// PostgreSQL refuses a second row for the normalized firm anchor
    /// without a read-before-write on the part of the caller.
    #[tokio::test]
    async fn a_second_row_under_the_firm_anchor_key_is_refused() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            create(&db, &anchor(&db, "Neon Law").await).await.unwrap();

            let forked = create(&db, &anchor(&db, "Neon Law").await).await;
            assert!(
                matches!(forked, Err(EntityError::FirmAnchorTaken)),
                "the unique `entity_firm_anchor` index is the gate, got {forked:?}"
            );
            assert!(firm_anchor_exists(&db, "neon law").await.unwrap());
        })
        .await;
    }

    /// Concurrency is the whole reason the guard is an index rather than
    /// a read followed by a write: two racers both see a free name, and
    /// only the index can stop them both writing.
    #[tokio::test]
    async fn concurrent_creates_of_the_anchor_yield_exactly_one_row() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let racer = anchor(&db, "Neon Law").await;
            let other = anchor(&db, "Neon Law").await;
            let (first, second) = tokio::join!(create(&db, &racer), create(&db, &other));

            let outcomes = [first, second];
            assert_eq!(
                outcomes.iter().filter(|r| r.is_ok()).count(),
                1,
                "exactly one racer may mint the anchor: {outcomes:?}"
            );
            assert_eq!(all(&db).await.unwrap().len(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn ordinary_namesakes_still_coexist() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            // Only the anchor takes the key; two ordinary namesakes coexist
            // because PostgreSQL's partial unique index excludes NULL keys.
            create(&db, &input(&db, "Beta LLC").await).await.unwrap();
            create(&db, &input(&db, "Beta LLC").await).await.unwrap();
            assert_eq!(all(&db).await.unwrap().len(), 2);
        })
        .await;
    }

    #[tokio::test]
    async fn update_replaces_every_field_and_reports_a_missing_row() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let row = create(&db, &input(&db, "Beta LLC").await).await.unwrap();
            assert_eq!(row.xero_id, None, "a fresh entity has no Xero contact yet");
            let mut edit = input(&db, "Beta Holdings LLC").await;
            edit.phone = Some("+1 702 555 0100".into());
            edit.url = Some("https://example.com".into());
            edit.xero_id = Some("contact-123".into());

            let updated = update(&db, row.id, &edit).await.unwrap().unwrap();
            assert_eq!(updated.id, row.id);
            assert_eq!(updated.name, "Beta Holdings LLC");
            assert_eq!(updated.entity_type_id, edit.entity_type_id);
            assert_eq!(updated.phone.as_deref(), Some("+1 702 555 0100"));
            assert_eq!(updated.url.as_deref(), Some("https://example.com"));
            assert_eq!(updated.xero_id.as_deref(), Some("contact-123"));

            assert_eq!(update(&db, Uuid::now_v7(), &edit).await.unwrap(), None);
        })
        .await;
    }

    #[tokio::test]
    async fn delete_returns_the_removed_row_once_and_then_nothing() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let row = create(&db, &input(&db, "Beta LLC").await).await.unwrap();

            assert_eq!(
                delete_unless_firm_anchor(&db, row.id)
                    .await
                    .unwrap()
                    .map(|e| e.id),
                Some(row.id)
            );
            assert_eq!(find_by_id(&db, row.id).await.unwrap(), None);
            assert_eq!(delete_unless_firm_anchor(&db, row.id).await.unwrap(), None);
        })
        .await;
    }

    /// The window the delete's own `WHERE` closes: a rename mints the
    /// anchor *after* a delete has read its target and judged it
    /// ordinary. A delete that decided on that earlier read would remove
    /// the freshly-protected firm, so the refusal lives in the delete.
    #[tokio::test]
    async fn a_row_that_became_the_anchor_after_the_read_is_not_deleted() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let ordinary = create(&db, &input(&db, "Acme LLC").await).await.unwrap();

            // What a caller would have read: an ordinary, deletable row.
            assert!(!find_by_id(&db, ordinary.id)
                .await
                .unwrap()
                .unwrap()
                .is_firm_anchor());

            // …and then a concurrent rename mints the anchor on it.
            update(
                &db,
                ordinary.id,
                &NewEntity {
                    firm_anchor_key: Some("example law pc".into()),
                    ..input(&db, "Neon Law").await
                },
            )
            .await
            .unwrap();

            assert_eq!(
                delete_unless_firm_anchor(&db, ordinary.id).await.unwrap(),
                None,
                "the delete must refuse a row that became the anchor"
            );
            assert!(
                find_by_id(&db, ordinary.id).await.unwrap().is_some(),
                "the freshly-protected firm must survive the racing delete"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn find_by_ids_reads_many_and_ignores_ids_with_no_row() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let one = create(&db, &input(&db, "One LLC").await).await.unwrap();
            let two = create(&db, &input(&db, "Two LLC").await).await.unwrap();

            let found = find_by_ids(&db, &[one.id, two.id, Uuid::now_v7()])
                .await
                .unwrap();
            let mut names: Vec<String> = found.into_iter().map(|e| e.name).collect();
            names.sort();
            assert_eq!(names, ["One LLC", "Two LLC"]);
            assert!(find_by_ids(&db, &[]).await.unwrap().is_empty());
        })
        .await;
    }

    /// The operator sees dependent Projects before PostgreSQL's foreign key
    /// refuses a deletion that would strand them.
    #[tokio::test]
    async fn dependents_counts_the_rows_a_delete_would_strand() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let entity = create(&db, &input(&db, "Referenced LLC").await)
                .await
                .unwrap();
            assert!(
                dependents(&db, entity.id).await.unwrap().is_empty(),
                "a fresh entity is free to delete"
            );

            crate::test_support::seed_compiled_brands_postgres(&db).await;
            crate::projects::create(
                &db,
                &crate::projects::NewProject {
                    code: format!("dep-{}", Uuid::now_v7()),
                    name: "Dependent matter".into(),
                    status: "open".into(),
                    entity_id: entity.id,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

            let found = dependents(&db, entity.id).await.unwrap();
            assert_eq!(
                found
                    .iter()
                    .map(|d| (d.noun(), d.count))
                    .collect::<Vec<_>>(),
                vec![("project", 1)],
                "the matter opened against this entity has to be named"
            );
        })
        .await;
    }

    /// An Entity Role is another dependent boundary. The count must report
    /// that row as well as Project references before an Entity is deleted.
    #[tokio::test]
    async fn dependents_sees_a_graph_edge_as_well_as_a_matter() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let entity = create(&db, &input(&db, "Edged LLC").await).await.unwrap();
            let person = crate::persons::create(
                &db,
                &crate::persons::NewPerson::new(
                    "Edge Holder",
                    format!("{}@example.com", Uuid::now_v7()),
                ),
            )
            .await
            .unwrap();
            crate::entity_roles::grant(&db, person.id, entity.id, "manages")
                .await
                .unwrap();

            let found = dependents(&db, entity.id).await.unwrap();
            assert_eq!(
                found
                    .iter()
                    .map(|d| (d.noun(), d.count))
                    .collect::<Vec<_>>(),
                vec![("entity role", 1)]
            );
        })
        .await;
    }

    /// Name ordering comes from the PostgreSQL accessor's stable projection.
    #[tokio::test]
    async fn listing_orders_by_name() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            for name in ["Zeta LLC", "Alpha LLC", "Mu LLC"] {
                create(&db, &input(&db, name).await).await.unwrap();
            }
            let names: Vec<String> = all(&db)
                .await
                .unwrap()
                .into_iter()
                .map(|e| e.name)
                .collect();
            assert_eq!(names, ["Alpha LLC", "Mu LLC", "Zeta LLC"]);
        })
        .await;
    }
}
