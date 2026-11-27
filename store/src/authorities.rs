//! `store::authorities` — the citation apparatus' write and read
//! commands (#890).
//!
//! The module exists to hold one distinction: an **Authority** is global
//! reference data, while a **matter's use** of it is scoped. Every
//! command here keeps that split, because collapsing it is what leaks one
//! matter's litigation posture into another matter's view of the same
//! case.
//!
//! Reads come in two lenses, and the client lens is not merely a subset —
//! it excludes dispositions that record firm reasoning. See
//! [`client_visible_uses`].
//!
//! PostgreSQL holds the global Authority catalog and, whenever its host is configured,
//! each Project's uses and citations. Matter scoping follows `authority_use.project_id`;
//! a citation names its Project through `authority_use_id`.
//!
//! "One posture per matter per authority" is a convention here rather than
//! an index: the Surreal schema's
//! `authority_use_project` index is not marked `UNIQUE`, so
//! [`cite_in_matter`] reads the existing row back before writing rather
//! than relying on the engine to refuse a duplicate — the same shape
//! `store::attestations::record` uses for the same reason.
//!
//! PostgreSQL holds the global `authority` rows wherever it is configured
//! ([`crate::postgres::catalog`]); a matter's uses and
//! citations stay in SurrealDB and link to them by id.
//!
//! A matter delete reaches
//! `authority_uses → citations → verifications`, and
//! Surreal cascades none of it, so [`delete_for_project`] walks
//! the chain explicitly.

use rules::citation::{AuthorityClass, Disposition};
use serde_json::Value as Json;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::{catalog, matter};
use serde_json::json;

const AUTHORITY_USE_TABLE: &str = "authority_use";
const CITATION_TABLE: &str = "citation";

/// Why a citation-apparatus command refused.
#[derive(Debug, thiserror::Error)]
pub enum AuthorityError {
    #[error(transparent)]
    Matter(#[from] matter::MatterAuthorityError),
    /// The class is outside [`AuthorityClass`].
    #[error("`{0}` is not a recognized authority class")]
    UnknownClass(String),
    /// The disposition is outside [`Disposition`].
    #[error("`{0}` is not a recognized citation disposition")]
    UnknownDisposition(String),
    /// The position is outside the closed `ours` / `adverse` / `neutral`
    /// vocabulary.
    #[error("`{0}` is not a recognized position")]
    UnknownPosition(String),
    /// A write reported success but returned no row, or returned one this
    /// module could not read back.
    #[error("writing to the citation apparatus returned no usable row")]
    WriteReturnedNothing,
    /// [`update`] found no Authority matching the given id or citation.
    #[error("no authority matches that id or citation")]
    NotFound,
    /// The PostgreSQL catalog authority could not serve the call.
    #[error(transparent)]
    Postgres(#[from] catalog::CatalogError),
}

impl From<sqlx::Error> for AuthorityError {
    fn from(error: sqlx::Error) -> Self {
        Self::Postgres(error.into())
    }
}

// ---------------------------------------------------------------------
// authority — global reference data
// ---------------------------------------------------------------------

/// One globally-shared piece of authority: a case, a statute, a
/// regulation, an administrative order, or a secondary source.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Authority {
    pub id: Uuid,
    pub class: String,
    pub citation: String,
    pub short_cite: Option<String>,
    pub title: String,
    pub publisher: Option<String>,
    pub issued_on: Option<String>,
    pub canonical_url: Option<String>,
    pub checked_on: Option<String>,
    pub archived_asset_id: Option<Uuid>,
    pub inserted_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

const PG_BY_ID: &str = "SELECT id, class, citation, short_cite, title, publisher, issued_on, \
     canonical_url, checked_on, archived_asset_id, inserted_at, updated_at \
     FROM authority WHERE id = $1";
const PG_BY_CITATION: &str = "SELECT id, class, citation, short_cite, title, publisher, \
     issued_on, canonical_url, checked_on, archived_asset_id, inserted_at, updated_at \
     FROM authority WHERE citation = $1";
const PG_UPDATE: &str = "UPDATE authority SET title = $2, short_cite = $3, publisher = $4, \
     issued_on = $5, canonical_url = $6, checked_on = $7, archived_asset_id = $8, \
     updated_at = now() WHERE id = $1 \
     RETURNING id, class, citation, short_cite, title, publisher, issued_on, canonical_url, \
     checked_on, archived_asset_id, inserted_at, updated_at";
/// A lost race on `citation` inserts nothing; the caller re-reads the winner.
const PG_INSERT: &str = "INSERT INTO authority (id, class, citation, short_cite, title, \
     publisher, issued_on, canonical_url, checked_on, archived_asset_id) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) ON CONFLICT (citation) DO NOTHING \
     RETURNING id, class, citation, short_cite, title, publisher, issued_on, canonical_url, \
     checked_on, archived_asset_id, inserted_at, updated_at";

fn from_pg(row: &sqlx::postgres::PgRow) -> Result<Authority, sqlx::Error> {
    use sqlx::Row;
    Ok(Authority {
        id: row.try_get("id")?,
        class: row.try_get("class")?,
        citation: row.try_get("citation")?,
        short_cite: row.try_get("short_cite")?,
        title: row.try_get("title")?,
        publisher: row.try_get("publisher")?,
        issued_on: row.try_get("issued_on")?,
        canonical_url: row.try_get("canonical_url")?,
        checked_on: row.try_get("checked_on")?,
        archived_asset_id: row.try_get("archived_asset_id")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// How [`find`] and [`update`] locate an existing Authority. `citation` and
/// `class` are the row's identity (see the module docs on [`record`]) —
/// there is deliberately no variant that changes either; a caller wanting a
/// different citation or class wants a new Authority, not an update of this
/// one.
#[derive(Debug, Clone, Copy)]
pub enum AuthorityLookup<'a> {
    Id(Uuid),
    Citation(&'a str),
}

/// Find an existing Authority by id or by citation.
///
/// # Errors
/// Propagates any database error.
pub async fn find(
    _db: &ApplicationStore,
    lookup: AuthorityLookup<'_>,
) -> Result<Option<Authority>, AuthorityError> {
    let pg = catalog::authority().await?;
    Ok(match lookup {
        AuthorityLookup::Id(id) => catalog::one(&pg, PG_BY_ID, id, from_pg).await?,
        AuthorityLookup::Citation(citation) => {
            catalog::one(&pg, PG_BY_CITATION, citation.to_string(), from_pg).await?
        }
    })
}

/// What [`update`] may change on an existing Authority. Every field is
/// "leave unchanged when absent" — there is no way to blank a field back to
/// `None` through this patch, since nothing in the issue that motivated it
/// (LAW-61) asked for that, and `citation`/`class` are not here at all:
/// they are immutable (see [`AuthorityLookup`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct AuthorityPatch<'a> {
    pub title: Option<&'a str>,
    pub short_cite: Option<&'a str>,
    pub publisher: Option<&'a str>,
    pub issued_on: Option<&'a str>,
    pub canonical_url: Option<&'a str>,
    pub checked_on: Option<&'a str>,
    /// A new artifact version to archive in place of the current one.
    /// `None` leaves the existing `archived_asset_id` untouched.
    pub archived_asset_id: Option<Uuid>,
}

/// Update the Authority found by `lookup`, leaving every field `patch`
/// leaves as `None` unchanged.
///
/// # Errors
/// [`AuthorityError::NotFound`] when `lookup` matches no row, or on a
/// database failure.
pub async fn update(
    db: &ApplicationStore,
    lookup: AuthorityLookup<'_>,
    patch: &AuthorityPatch<'_>,
) -> Result<Authority, AuthorityError> {
    let existing = find(db, lookup).await?.ok_or(AuthorityError::NotFound)?;
    let title = patch.title.map_or(existing.title.clone(), str::to_string);
    let short_cite = patch
        .short_cite
        .map(str::to_string)
        .or_else(|| existing.short_cite.clone());
    let publisher = patch
        .publisher
        .map(str::to_string)
        .or_else(|| existing.publisher.clone());
    let issued_on = patch
        .issued_on
        .map(str::to_string)
        .or_else(|| existing.issued_on.clone());
    let canonical_url = patch
        .canonical_url
        .map(str::to_string)
        .or_else(|| existing.canonical_url.clone());
    let checked_on = patch
        .checked_on
        .map(str::to_string)
        .or_else(|| existing.checked_on.clone());
    let archived_asset_id = patch.archived_asset_id.or(existing.archived_asset_id);

    let pg = catalog::authority().await?;
    let row = catalog::maybe_row(
        sqlx::query(PG_UPDATE)
            .bind(existing.id)
            .bind(title)
            .bind(short_cite)
            .bind(publisher)
            .bind(issued_on)
            .bind(canonical_url)
            .bind(checked_on)
            .bind(archived_asset_id),
        &pg,
    )
    .await?;
    row.as_ref()
        .map(from_pg)
        .transpose()?
        .ok_or(AuthorityError::WriteReturnedNothing)
}

/// What a new [`Authority`] needs.
#[derive(Debug, Clone)]
pub struct NewAuthority<'a> {
    pub class: AuthorityClass,
    pub citation: &'a str,
    pub short_cite: Option<&'a str>,
    pub title: &'a str,
    pub publisher: Option<&'a str>,
    pub issued_on: Option<&'a str>,
    pub canonical_url: Option<&'a str>,
    pub checked_on: Option<&'a str>,
    pub archived_asset_id: Option<Uuid>,
}

/// Find-or-create the global Authority for `new.citation`.
///
/// Find-or-create rather than insert, because the whole point of a global
/// table is that the second matter to cite a case reuses the first
/// matter's row. A blind insert would rebuild the five private lists this
/// table replaces, one unique-violation at a time. A race on `citation`
/// resolves by re-reading the winner's row, exactly like
/// `store::signatures::record_request`.
///
/// # Errors
/// [`AuthorityError`] on a database failure.
pub async fn record(
    _db: &ApplicationStore,
    new: &NewAuthority<'_>,
) -> Result<Authority, AuthorityError> {
    let pg = catalog::authority().await?;
    record_on(&pg, new).await
}

async fn record_on(
    pg: &crate::postgres::PostgresDb,
    new: &NewAuthority<'_>,
) -> Result<Authority, AuthorityError> {
    let citation = new.citation.to_string();
    if let Some(existing) = catalog::one(pg, PG_BY_CITATION, citation.clone(), from_pg).await? {
        return Ok(existing);
    }
    let inserted = catalog::maybe_row(
        sqlx::query(PG_INSERT)
            .bind(Uuid::now_v7())
            .bind(new.class.as_str().to_string())
            .bind(citation.clone())
            .bind(new.short_cite.map(str::to_string))
            .bind(new.title.to_string())
            .bind(new.publisher.map(str::to_string))
            .bind(new.issued_on.map(str::to_string))
            .bind(new.canonical_url.map(str::to_string))
            .bind(new.checked_on.map(str::to_string))
            .bind(new.archived_asset_id),
        pg,
    )
    .await?;
    match inserted {
        Some(row) => Ok(from_pg(&row)?),
        None => catalog::one(pg, PG_BY_CITATION, citation, from_pg)
            .await?
            .ok_or(AuthorityError::WriteReturnedNothing),
    }
}

/// Parse a stored `authority.class`.
///
/// # Errors
/// [`AuthorityError::UnknownClass`] when the value is outside the closed
/// vocabulary — which means a row was written around the intended
/// taxonomy.
pub fn class_of(row: &Authority) -> Result<AuthorityClass, AuthorityError> {
    AuthorityClass::parse(&row.class).ok_or_else(|| AuthorityError::UnknownClass(row.class.clone()))
}

// ---------------------------------------------------------------------
// authority_use — a matter's scoped use of a global authority
// ---------------------------------------------------------------------

/// Which side of the matter relies on an authority.
pub const POSITIONS: &[&str] = &["ours", "adverse", "neutral"];

/// One matter's use of a global [`Authority`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AuthorityUse {
    pub id: Uuid,
    pub project_id: Uuid,
    pub authority_id: Uuid,
    pub position: String,
    pub disposition: String,
    pub role: Option<String>,
    pub inserted_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Record that `project_id` uses `authority_id`, taking `position` with
/// `disposition`.
///
/// Idempotent on `(project_id, authority_id)`: a matter takes one posture
/// on a given authority, so a second call updates that posture rather
/// than adding a contradictory second row. The Surreal schema carries no
/// unique index on the pair (see the module header), so this reads the
/// existing row back first and upserts it by id — the same shape
/// `store::attestations::record` uses.
///
/// # Errors
/// [`AuthorityError`] when the position is outside its closed vocabulary,
/// or on a database failure.
pub async fn cite_in_matter(
    _db: &ApplicationStore,
    project_id: Uuid,
    authority_id: Uuid,
    position: &str,
    disposition: Disposition,
    role: Option<&str>,
) -> Result<AuthorityUse, AuthorityError> {
    if !POSITIONS.contains(&position) {
        return Err(AuthorityError::UnknownPosition(position.to_string()));
    }

    let pg = matter::authority_for(AUTHORITY_USE_TABLE).await?;
    let existing: Option<AuthorityUse> = matter::find(
        &pg,
        AUTHORITY_USE_TABLE,
        json!({"project_id": project_id, "authority_id": authority_id}),
    )
    .await?;
    let now = chrono::Utc::now();
    let row = AuthorityUse {
        id: existing.as_ref().map_or_else(Uuid::now_v7, |row| row.id),
        project_id,
        authority_id,
        position: position.to_owned(),
        disposition: disposition.as_str().to_owned(),
        role: role.map(str::to_owned),
        inserted_at: existing.map_or(now, |row| row.inserted_at),
        updated_at: now,
    };
    Ok(matter::upsert(&pg, AUTHORITY_USE_TABLE, &row).await?)
}

/// Every use recorded on `project_id`, newest first — the **lawyer lens**.
///
/// Scoped by `project_id` and nothing else, which is what makes one
/// matter's use of an authority invisible from another matter.
///
/// # Errors
/// Propagates any database error.
pub async fn uses_for_project(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<AuthorityUse>, AuthorityError> {
    let pg = matter::authority_for(AUTHORITY_USE_TABLE).await?;
    Ok(matter::select(
        &pg,
        AUTHORITY_USE_TABLE,
        json!({"project_id": project_id}),
        &["id DESC"],
    )
    .await?)
}

/// The uses on `project_id` a **client** may see.
///
/// Not merely a subset of [`uses_for_project`]. Several dispositions
/// record *firm reasoning* — what the firm considered and chose not to
/// rely on — and a client who sees "reviewed, not used" learns the firm's
/// strategic assessment of their own matter. That is a disclosure of work
/// product, a different and worse failure than an ordinary visibility
/// bug.
///
/// The allowlist is derived from
/// [`Disposition::is_firm_reasoning`](rules::citation::Disposition::is_firm_reasoning)
/// rather than written out here, so adding a disposition cannot leave a
/// stale literal behind.
///
/// # Errors
/// Propagates any database error.
pub async fn client_visible_uses(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<AuthorityUse>, AuthorityError> {
    let pg = matter::authority_for(AUTHORITY_USE_TABLE).await?;
    let allowed = Disposition::client_visible();
    let rows: Vec<AuthorityUse> = matter::select(
        &pg,
        AUTHORITY_USE_TABLE,
        json!({"project_id": project_id}),
        &["id DESC"],
    )
    .await?;
    Ok(rows
        .into_iter()
        .filter(|row| {
            allowed
                .iter()
                .any(|value| value.as_str() == row.disposition)
        })
        .collect())
}

/// Parse a stored `authority_use.disposition`.
///
/// # Errors
/// [`AuthorityError::UnknownDisposition`] when the value is outside the
/// closed vocabulary.
pub fn disposition_of(row: &AuthorityUse) -> Result<Disposition, AuthorityError> {
    Disposition::parse(&row.disposition)
        .ok_or_else(|| AuthorityError::UnknownDisposition(row.disposition.clone()))
}

// ---------------------------------------------------------------------
// citation — the Locator
// ---------------------------------------------------------------------

/// One Locator: an assertion citing an authority at a specific place,
/// pinned to both the source and the firm's own draft.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Citation {
    pub id: Uuid,
    pub authority_use_id: Uuid,
    pub quote: String,
    pub why: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::postgres::matter::present_json"
    )]
    pub source_pin: Option<Json>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::postgres::matter::present_json"
    )]
    pub draft_pin: Option<Json>,
    pub inserted_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// What a new [`Citation`] needs.
#[derive(Debug, Clone)]
pub struct NewCitation<'a> {
    pub authority_use_id: Uuid,
    /// The exact relied-on text.
    pub quote: &'a str,
    /// Why the source supports the assertion. Required — see the module
    /// docs on [`Citation`].
    pub why: &'a str,
    /// Paragraph, page, or normalised rect within the source.
    pub source_pin: Option<Json>,
    /// Where in the firm's own draft the assertion sits.
    pub draft_pin: Option<Json>,
}

/// Record a Locator against a matter's use of an authority.
///
/// # Errors
/// Propagates any database error.
pub async fn cite(
    _db: &ApplicationStore,
    new: &NewCitation<'_>,
) -> Result<Citation, AuthorityError> {
    let pg = matter::authority_for(CITATION_TABLE).await?;
    let now = chrono::Utc::now();
    let row = Citation {
        id: Uuid::now_v7(),
        authority_use_id: new.authority_use_id,
        quote: new.quote.to_owned(),
        why: new.why.to_owned(),
        source_pin: new.source_pin.clone(),
        draft_pin: new.draft_pin.clone(),
        inserted_at: now,
        updated_at: now,
    };
    Ok(matter::upsert(&pg, CITATION_TABLE, &row).await?)
}

/// Every Locator recorded against `authority_use_id`, newest first.
///
/// # Errors
/// Propagates any database error.
pub async fn citations_for_use(
    _db: &ApplicationStore,
    authority_use_id: Uuid,
) -> Result<Vec<Citation>, AuthorityError> {
    let pg = matter::authority_for(CITATION_TABLE).await?;
    Ok(matter::select(
        &pg,
        CITATION_TABLE,
        json!({"authority_use_id": authority_use_id}),
        &["id DESC"],
    )
    .await?)
}

/// Delete every `authority_use`, `citation`, and `verification` row that
/// belongs to `project_id`, deepest first.
///
/// The chain is `authority_uses → citations → verifications`. The command
/// deletes it deepest first in one PostgreSQL transaction when configured,
/// retaining the compatibility transaction for a process without PostgreSQL.
///
/// # Errors
/// Propagates any database error.
pub async fn delete_for_project(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<(), AuthorityError> {
    let pg = matter::authority_for(CITATION_TABLE).await?;
    matter::delete_authority_uses(&pg, project_id).await?;
    Ok(())
}

#[cfg(test)]
mod tests {}
