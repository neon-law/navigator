//! Template resolution and body access.
//!
//! # This table lives in SurrealDB
//!
//! `templates` moved with wave five of #1093 (ENG-121), together with
//! [`crate::assets`]. They had to move as one slice: [`save_version`] folds
//! `asset_id` into the tuple that decides whether a version changed, so
//! split across engines the lane would hold its version identity in one and
//! the bytes it names in the other.
//!
//! Two responsibilities this module owns (see `docs/notation.md`):
//!
//! - **Body access.** A Template's markdown body is a content-addressed
//!   [`crate::assets`] asset referenced by `templates.asset_id`; [`body`]
//!   fetches it from storage. `fk_templates_asset` is gone
//!   (`m20260911_drop_notation_group_foreign_keys`), and what replaced it
//!   is [`body`]'s read-back of the asset before it reaches storage — not
//!   the link's type. The engine does not validate a `record<asset>` link.
//! - **Scoping.** A Template is workspace-shared (`project_id` and
//!   `firm_id` unset), scoped to one Firm's Firm Library (`firm_id`
//!   set), or scoped to one Project (`project_id` set) — see [`Scope`].
//!   [`resolve`] looks a code up preferring the caller's Project, then
//!   the Firm that owns that Project, then the shared row — so a Firm or
//!   a Project can override a shared `code` or define its own without
//!   colliding with another's. A Firm's rows are never read for another
//!   Firm's Project, nor listed as the shared catalog
//!   (`docs/glossary/firm-library.md`).
//!
//! # One current row per code, without a partial index
//!
//! The rule is one current row per shared `code`, one per
//! `(firm_id, code)`, and one per `(project_id, code)`. Surreal has no partial
//! index, so [`current_key`] computes a key that exists only while a row is
//! current, and the schema's UNIQUE index refuses the second one. Multiple
//! NONEs do not collide there, so retired versions never take part. That
//! matters more than it looks: [`save_version`] retires the old row and
//! inserts the new one in two statements, and a multi-statement Surreal
//! query is not one transaction — the index is what makes the pair safe
//! under a race rather than merely usually-right.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use cloud::StorageService;
use serde::Serialize;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::surreal::{record_id, record_uuid, SurrealDb};

/// The table these rows live in.
pub(crate) const TABLE: &str = "template";

/// One template version.
///
/// The application-facing shape: plain Rust types, no engine handles.
/// [`TemplateRow`] is the seam that turns it into (and back out of) what
/// the SDK reads and writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Template {
    pub id: Uuid,
    /// Stable code. Unique among *current* shared templates; unique per
    /// Project among current project-scoped ones.
    pub code: String,
    pub title: String,
    /// `entity`, `person`, or `person_and_entity`.
    pub respondent_type: String,
    /// The Project this template is scoped to; `None` for the
    /// workspace-shared public catalog and for a Firm Library version.
    pub project_id: Option<Uuid>,
    /// The Firm whose Firm Library this version was synced from; `None`
    /// for the shared catalog and for a Project-scoped version.
    pub firm_id: Option<Uuid>,
    /// Whether this row is the live version of its `code`.
    pub is_current: bool,
    /// The [`crate::assets`] row holding the markdown body (with
    /// `{{question_code}}` placeholders). `None` only transiently before
    /// the body is ingested. Read via [`body`].
    pub asset_id: Option<Uuid>,
    /// forms-registry code of the government form this template fills
    /// (e.g. `nv__llc_formation`), from the `form:` frontmatter key;
    /// `None` for Typst-rendered templates.
    pub form_code: Option<String>,
    /// The declared notation `kind` from the template's `kind:`
    /// frontmatter — one of the values in `rules::kind::Kind`. Lets
    /// callers gate on kind (e.g. "the first notation on a Project must be
    /// a `retainer`") without re-parsing the body.
    pub kind: Option<String>,
    /// Commit SHA of `refs/heads/main` the version was cut from when a
    /// Project repo fed it through [`crate::template_source`]. `None` for
    /// the workspace catalog, which is seeded from bundled files rather
    /// than a repo. Provenance, not identity — not part of the version
    /// tuple. Required for a project-scoped version: [`save_version`]
    /// refuses one without it.
    pub source_commit_sha: Option<String>,
    /// Lowercase hex SHA-256 of the body asset's bytes, copied from the
    /// asset row by [`save_version`] and checked against the stored bytes by
    /// [`body`]. `None` only while `asset_id` is, or on a row written before
    /// the digest existed whose asset could not be read at backfill.
    pub body_sha256: Option<String>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The row as the engine reads and writes it.
#[derive(SurrealValue)]
struct TemplateRow {
    id: surrealdb::types::RecordId,
    code: String,
    title: String,
    respondent_type: String,
    project_id: Option<surrealdb::types::RecordId>,
    firm_id: Option<surrealdb::types::RecordId>,
    is_current: bool,
    asset_id: Option<surrealdb::types::RecordId>,
    form_code: Option<String>,
    kind: Option<String>,
    source_commit_sha: Option<String>,
    body_sha256: Option<String>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl TemplateRow {
    /// `None` when the record id is not a native UUID key — a row written
    /// by something that bypassed [`crate::surreal::record_id`].
    fn into_template(self) -> Option<Template> {
        Some(Template {
            id: record_uuid(&self.id)?,
            code: self.code,
            title: self.title,
            respondent_type: self.respondent_type,
            project_id: self.project_id.as_ref().and_then(record_uuid),
            firm_id: self.firm_id.as_ref().and_then(record_uuid),
            is_current: self.is_current,
            asset_id: self.asset_id.as_ref().and_then(record_uuid),
            form_code: self.form_code,
            kind: self.kind,
            source_commit_sha: self.source_commit_sha,
            body_sha256: self.body_sha256,
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

/// The projection every read shares, so one field list describes the row
/// and a new field cannot reach [`TemplateRow`] from only one query.
const SELECT: &str = "id, code, title, respondent_type, project_id, firm_id, is_current, \
     asset_id, form_code, kind, source_commit_sha, body_sha256, inserted_at, updated_at";

/// Errors from [`body`].
#[derive(Debug, thiserror::Error)]
pub enum TemplateBodyError {
    #[error("template `{0}` has no stored body (asset_id is null)")]
    MissingBody(Uuid),
    #[error("template `{0}` names an asset that does not exist")]
    DanglingAsset(Uuid),
    #[error("asset: {0}")]
    Asset(#[from] crate::assets::AssetError),
    #[error("template body is not valid UTF-8")]
    NotUtf8,
    /// The stored bytes do not hash to the digest the template version (or
    /// the Notation that pinned it) recorded. The body is not returned.
    #[error("template `{0}` body does not match its pinned SHA-256")]
    DigestMismatch(Uuid),
}

/// Errors reading or writing a template.
#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    /// Another writer already holds the current row for this
    /// `(project_id, code)` — the `template_current_key` index refused a
    /// second one.
    #[error("that template code already has a current version")]
    CurrentVersionTaken,
    /// A write reported success but returned no row, or returned one this
    /// module could not read back.
    #[error("writing a template returned no usable row")]
    WriteReturnedNothing,
    /// A project-scoped version is synced from the Project repository, so
    /// it must name the commit it was read from.
    #[error("a Draft synced from a Project repository must name its source commit")]
    MissingSourceCommit,
    /// A project-scoped version must carry a body to pin.
    #[error("a Draft synced from a Project repository must carry a body")]
    MissingBody,
    /// `asset_id` names no asset row, so there is no digest to pin.
    #[error("template body asset `{0}` does not exist")]
    DanglingAsset(Uuid),
    #[error("asset: {0}")]
    Asset(#[from] crate::assets::AssetError),
}

/// Turn a write failure into the caller-correctable conflict it names, or
/// leave it as a database fault. A unique violation carries **no typed
/// detail** — the index name in the message is the only discriminator, so
/// this matches on `template_current_key`, a `DEFINE INDEX` identifier this
/// workspace chose in `store/src/schema/navigator.surql`, not prose.
fn classify_write(error: surrealdb::Error) -> TemplateError {
    if crate::surreal::retry::unique_violation(&error) == Some("template_current_key") {
        TemplateError::CurrentVersionTaken
    } else {
        TemplateError::Db(error)
    }
}

/// Which version line a Template belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The workspace-shared catalog every Firm reads.
    Shared,
    /// One Firm's Firm Library, read only for that Firm and the Projects it
    /// owns.
    Firm(Uuid),
    /// One Project's own repository.
    Project(Uuid),
}

impl Scope {
    /// The scope the `project_id: Option<Uuid>` arguments name.
    #[must_use]
    pub fn of_project(project_id: Option<Uuid>) -> Self {
        project_id.map_or(Self::Shared, Self::Project)
    }

    fn project_id(self) -> Option<Uuid> {
        match self {
            Self::Project(id) => Some(id),
            Self::Shared | Self::Firm(_) => None,
        }
    }

    fn firm_id(self) -> Option<Uuid> {
        match self {
            Self::Firm(id) => Some(id),
            Self::Shared | Self::Project(_) => None,
        }
    }

    /// Whether a version in this scope is synced from a repository, and so
    /// must name its commit and carry a body.
    fn is_synced(self) -> bool {
        !matches!(self, Self::Shared)
    }
}

/// The value stored in `current_key` for a row that is current.
///
/// The unit separator (`U+001F`) joins the scope to the code because it
/// cannot occur in either: a Project id is a UUID and a template code is a
/// frontmatter identifier. A plain separator like `:` could be produced by
/// a code containing it, which would let two distinct `(project, code)`
/// pairs collide on one key and silently retire each other's version.
///
/// The shared line uses an empty scope, so a shared `amendment` and a
/// project-scoped `amendment` get different keys and coexist. A Firm line
/// prefixes its UUID with `firm:`, which no bare Project UUID can equal.
#[cfg(feature = "test-support")]
pub(crate) fn current_key(project_id: Option<Uuid>, code: &str) -> String {
    current_key_in(Scope::of_project(project_id), code)
}

/// The current key for any [`Scope`].
pub(crate) fn current_key_in(scope: Scope, code: &str) -> String {
    let scope = match scope {
        Scope::Shared => String::new(),
        Scope::Project(id) => id.to_string(),
        Scope::Firm(id) => format!("firm:{id}"),
    };
    format!("{scope}\u{1f}{code}")
}

fn one(mut response: surrealdb::IndexedResults) -> Result<Option<Template>, TemplateError> {
    let row: Option<TemplateRow> = response.take(0)?;
    Ok(row.and_then(TemplateRow::into_template))
}

fn many(mut response: surrealdb::IndexedResults) -> Result<Vec<Template>, TemplateError> {
    let rows: Vec<TemplateRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(TemplateRow::into_template)
        .collect())
}

/// Resolve a template by id — what a Notation does with the
/// `template_id` it pinned.
///
/// # Errors
///
/// [`TemplateError::Db`] if the lookup fails.
pub async fn find_by_id(db: &SurrealDb, id: Uuid) -> Result<Option<Template>, TemplateError> {
    let response = db
        .query(format!("SELECT {SELECT} FROM ONLY $id LIMIT 1"))
        .bind(("id", record_id(TABLE, id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    one(response)
}

/// Resolve many templates by id in one round trip — what a listing does
/// after reading a page of notations, rather than one lookup per row.
///
/// # Errors
///
/// [`TemplateError::Db`] if the lookup fails.
pub async fn find_by_ids(db: &SurrealDb, ids: &[Uuid]) -> Result<Vec<Template>, TemplateError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let keys: Vec<surrealdb::types::RecordId> =
        ids.iter().map(|id| record_id(TABLE, *id)).collect();
    let response = db
        .query(format!("SELECT {SELECT} FROM $ids ORDER BY code ASC"))
        .bind(("ids", keys))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}

/// Resolve a template by `code` for a given Project context. Prefers a
/// Project-scoped row (`project_id = project_id`), then the Firm Library
/// row of the Firm that owns the Project (`project.firm_id`), then the
/// workspace-shared row. Returns `None` when none exists.
///
/// Pass `project_id = None` to look up only the shared row (the public
/// catalog).
///
/// # Errors
///
/// [`TemplateError::Db`] if the lookup fails.
pub async fn resolve(
    db: &SurrealDb,
    project_id: Option<Uuid>,
    code: &str,
) -> Result<Option<Template>, TemplateError> {
    let Some(project_id) = project_id else {
        return resolve_in(db, Scope::Shared, code).await;
    };
    if let Some(scoped) = resolve_in(db, Scope::Project(project_id), code).await? {
        return Ok(Some(scoped));
    }
    match owning_firm(db, project_id).await? {
        Some(firm_id) => resolve_for_firm(db, firm_id, code).await,
        None => resolve_in(db, Scope::Shared, code).await,
    }
}

/// Resolve a template by `code` for a Firm with no Project yet — the
/// matter-opening doors. Prefers the Firm's own Library row, then the
/// workspace-shared row.
///
/// # Errors
///
/// [`TemplateError::Db`] if the lookup fails.
pub async fn resolve_for_firm(
    db: &SurrealDb,
    firm_id: Uuid,
    code: &str,
) -> Result<Option<Template>, TemplateError> {
    if let Some(firm) = resolve_in(db, Scope::Firm(firm_id), code).await? {
        return Ok(Some(firm));
    }
    resolve_in(db, Scope::Shared, code).await
}

/// The Firm that owns `project_id`, if any.
async fn owning_firm(db: &SurrealDb, project_id: Uuid) -> Result<Option<Uuid>, TemplateError> {
    let mut response = db
        .query("SELECT VALUE firm_id FROM $project")
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let firms: Vec<Option<surrealdb::types::RecordId>> = response.take(0)?;
    Ok(firms
        .into_iter()
        .flatten()
        .next()
        .as_ref()
        .and_then(record_uuid))
}

/// The current row for an exact `(project_id, code)` — no shared
/// fallback, unlike [`resolve`]. [`save_version`]'s pointer flip must act
/// on the same scope it writes to.
///
/// # Errors
///
/// [`TemplateError::Db`] if the lookup fails.
pub async fn resolve_exact(
    db: &SurrealDb,
    project_id: Option<Uuid>,
    code: &str,
) -> Result<Option<Template>, TemplateError> {
    resolve_in(db, Scope::of_project(project_id), code).await
}

/// The current row for an exact `(scope, code)`, with no fallback.
///
/// # Errors
///
/// [`TemplateError::Db`] if the lookup fails.
pub async fn resolve_in(
    db: &SurrealDb,
    scope: Scope,
    code: &str,
) -> Result<Option<Template>, TemplateError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE current_key = $key LIMIT 1"
        ))
        .bind(("key", current_key_in(scope, code)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    one(response)
}

/// Every version ever written for `(project_id, code)`, newest first —
/// the current row plus every retired one.
///
/// The immutability property made observable: a spec change appends a row
/// and flips `is_current`, so the retired versions are still here for the
/// Notations that pinned them through `notation.template_id`.
///
/// # Errors
///
/// [`TemplateError::Db`] if the lookup fails.
pub async fn versions_of(
    db: &SurrealDb,
    project_id: Option<Uuid>,
    code: &str,
) -> Result<Vec<Template>, TemplateError> {
    // Matched on `(code, project_id)` rather than `current_key`, which only
    // a current row carries — that is the whole point of this read.
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE code = $code AND project_id = $project \
             AND firm_id = NONE ORDER BY id DESC"
        ))
        .bind(("code", code.to_string()))
        .bind((
            "project",
            project_id.map(|p| record_id(crate::projects::PROJECT_TABLE, p)),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}

/// Every current shared and Project-scoped template, ordered by code —
/// the lawyer listing and the importer's "what is already catalogued"
/// read. Callers wanting only the shared catalog filter on `project_id`.
///
/// Firm Library versions are never returned here, so a listing of the
/// shared catalog cannot show one Firm's templates to another. Read a
/// Firm's own with [`list_for_firm`].
///
/// # Errors
///
/// [`TemplateError::Db`] if the lookup fails.
pub async fn list_current(db: &SurrealDb) -> Result<Vec<Template>, TemplateError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE is_current = true AND firm_id = NONE \
             ORDER BY code ASC"
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}

/// Every current template in one Firm's Library, ordered by code.
///
/// # Errors
///
/// [`TemplateError::Db`] if the lookup fails.
pub async fn list_for_firm(db: &SurrealDb, firm_id: Uuid) -> Result<Vec<Template>, TemplateError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} \
             WHERE is_current = true AND firm_id = $firm ORDER BY code ASC"
        ))
        .bind(("firm", record_id(crate::firms::TABLE, firm_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}

/// Every current template scoped to one Project, ordered by code.
///
/// # Errors
///
/// [`TemplateError::Db`] if the lookup fails.
pub async fn list_for_project(
    db: &SurrealDb,
    project_id: Uuid,
) -> Result<Vec<Template>, TemplateError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} \
             WHERE is_current = true AND project_id = $project ORDER BY code ASC"
        ))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}

/// Whether any template — current or superseded — is scoped to this
/// Project. The matter-delete guard.
///
/// Deliberately unfiltered by `is_current`, unlike [`list_for_project`]: a
/// superseded version still carries the matter's `project_id`, so deleting
/// the matter out from under it would orphan the whole version line rather
/// than just the row a reader sees.
///
/// # Errors
///
/// [`TemplateError::Db`] if the lookup fails.
pub async fn exists_for_project(db: &SurrealDb, project_id: Uuid) -> Result<bool, TemplateError> {
    let mut response = db
        .query(format!(
            "SELECT VALUE id FROM {TABLE} WHERE project_id = $project LIMIT 1"
        ))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let ids: Vec<surrealdb::types::RecordId> = response.take(0)?;
    Ok(!ids.is_empty())
}

/// The spec fields that make up one Template version. Identity for
/// "did this change?" is the tuple `(title, respondent_type, asset_id,
/// form_code, kind)` — `code` and `project_id` locate the version line.
pub struct Version {
    pub title: String,
    pub respondent_type: String,
    pub asset_id: Option<Uuid>,
    pub form_code: Option<String>,
    /// Declared notation kind — one of the notation-family `kind:` values
    /// (`letter`, `filing`, `will`, `trust`, `directive`, `agreement`,
    /// `onboarding`, `offboarding`, `memo`) from the template's frontmatter;
    /// `None` for a template stored before it declared one. The
    /// engagement-first matter gate keys off the kinds that open a matter
    /// (`rules::kind::Kind::opens_a_matter`).
    pub kind: Option<String>,
    /// Git commit SHA the version was cut from (`crate::template_source`),
    /// or `None` for a non-repo source (the seeded workspace catalog).
    /// Recorded on write as **provenance** — deliberately absent from the
    /// change-detection tuple above, so a byte-identical re-read at a
    /// newer commit is still [`Saved::Unchanged`] and keeps the first
    /// commit that produced these bytes.
    pub source_commit_sha: Option<String>,
}

/// Outcome of [`save_version`].
pub enum Saved {
    /// The current row already matched the spec; nothing was written.
    Unchanged(Template),
    /// A new current row was written — the first version of this code, or
    /// a change that retired the prior version.
    Written(Template),
}

impl Saved {
    /// The now-current Template row, either way.
    #[must_use]
    pub fn into_model(self) -> Template {
        match self {
            Saved::Unchanged(m) | Saved::Written(m) => m,
        }
    }

    /// Whether this call wrote a new row.
    #[must_use]
    pub fn was_written(&self) -> bool {
        matches!(self, Saved::Written(_))
    }
}

/// Make `version` the current Template for `(project_id, code)`, appending
/// it as a new row and retiring any existing current row.
///
/// Immutable by policy: a spec change never rewrites a row a Notation
/// pinned via `notation.template_id`. When `version` matches the existing
/// current row, this is a no-op and returns [`Saved::Unchanged`] — so a
/// re-seed of an unchanged template does not churn versions.
///
/// Race-safe without a lock. `cli import` and the canonical seed both run
/// this on every boot, and the cucumber suite runs concurrent scenarios
/// against one engine, so idempotence under contention is the contract
/// rather than a nicety. A concurrent writer that wins the
/// `template_current_key` index turns this call's insert into
/// [`TemplateError::CurrentVersionTaken`], which is re-read once: if the
/// winner wrote the same spec the answer is [`Saved::Unchanged`], and only
/// a genuinely different spec that still lost surfaces the conflict.
///
/// # Errors
///
/// [`TemplateError::CurrentVersionTaken`] when a concurrent writer holds
/// the current row for a *different* spec, and [`TemplateError::Db`] for
/// anything else.
pub async fn save_version(
    db: &SurrealDb,
    project_id: Option<Uuid>,
    code: &str,
    version: Version,
) -> Result<Saved, TemplateError> {
    save_version_in(db, Scope::of_project(project_id), code, version).await
}

/// [`save_version`] for any [`Scope`] — how a Firm Library version is
/// written. A Firm version, like a Project one, is synced from a
/// repository and must name its commit and carry a body.
///
/// # Errors
///
/// As [`save_version`].
pub async fn save_version_in(
    db: &SurrealDb,
    scope: Scope,
    code: &str,
    version: Version,
) -> Result<Saved, TemplateError> {
    let mut backoff = WRITE_BACKOFF;
    for remaining in (0..WRITE_ATTEMPTS).rev() {
        match attempt_save(db, scope, code, &version).await {
            Err(TemplateError::CurrentVersionTaken) => {
                // Someone else holds the slot. If they wrote what we were
                // about to write, this call is simply a no-op — the shape a
                // concurrent re-seed takes.
                if let Some(winner) = resolve_in(db, scope, code).await? {
                    if matches_spec(&winner, &version) {
                        return Ok(Saved::Unchanged(winner));
                    }
                }
                // Otherwise we may have read mid-flip. `attempt_save` retires
                // the incumbent and inserts its replacement in two statements,
                // so between them NO row carries `current_key` — a reader in
                // that window sees no current version at all and must not
                // conclude anything from it. Back off and look again rather
                // than deciding from a half-applied pointer flip.
                if remaining == 0 {
                    return Err(TemplateError::CurrentVersionTaken);
                }
                tokio::time::sleep(rand::random_range(std::time::Duration::ZERO..=backoff)).await;
                backoff *= 2;
            }
            other => return other,
        }
    }
    unreachable!("the last attempt returns rather than falling out of the loop")
}

/// How many times [`save_version`] re-reads after losing the
/// `template_current_key` slot, and the first backoff window.
///
/// Not the transaction-conflict retry: that is one policy for the whole
/// crate, in [`crate::surreal::retry`], and the statements this module
/// issues reach it through [`crate::assets::writing`]. This bound is
/// around a *unique index* violation, where the point of another pass is
/// to re-read the winner rather than to re-run the write.
const WRITE_ATTEMPTS: usize = 5;
const WRITE_BACKOFF: std::time::Duration = std::time::Duration::from_millis(2);

/// Whether `row` already carries exactly the spec `version` describes.
/// `code` and `project_id` locate the version line; `source_commit_sha` is
/// provenance and deliberately absent, so byte-identical bytes re-read at a
/// newer commit stay [`Saved::Unchanged`].
///
/// A row that is missing a pin the current write path would record — a
/// digest for its body, or a commit for a Project or Firm version — never
/// matches, so the next sync appends a fully pinned version instead of
/// keeping the unpinned one current.
fn matches_spec(row: &Template, version: &Version) -> bool {
    let pinned = row.body_sha256.is_some() == row.asset_id.is_some()
        && ((row.project_id.is_none() && row.firm_id.is_none()) || row.source_commit_sha.is_some());
    pinned
        && row.title == version.title
        && row.respondent_type == version.respondent_type
        && row.asset_id == version.asset_id
        && row.form_code == version.form_code
        && row.kind == normalized_kind(version.kind.clone())
}

/// The rules layer (S103) validates the *trimmed* scalar, so store it
/// trimmed too — otherwise a padded frontmatter value like
/// `kind: "retainer "` passes validation but is stored untrimmed and then
/// fails to parse at the engagement-first gate. Empty-after-trim collapses
/// to "no kind declared".
fn normalized_kind(kind: Option<String>) -> Option<String> {
    kind.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

async fn attempt_save(
    db: &SurrealDb,
    scope: Scope,
    code: &str,
    spec: &Version,
) -> Result<Saved, TemplateError> {
    let mut version = Version {
        title: spec.title.clone(),
        respondent_type: spec.respondent_type.clone(),
        asset_id: spec.asset_id,
        form_code: spec.form_code.clone(),
        kind: spec.kind.clone(),
        source_commit_sha: spec.source_commit_sha.clone(),
    };
    version.kind = normalized_kind(version.kind);
    let body_sha256 = pin_body(db, scope, &version).await?;

    let current = resolve_in(db, scope, code).await?;
    if let Some(existing) = &current {
        if matches_spec(existing, &version) {
            return Ok(Saved::Unchanged(existing.clone()));
        }
    }

    // Retire the incumbent first: it holds `current_key`, and the UNIQUE
    // index would refuse the new row while it still does. Clearing the key
    // is what makes the slot available, so this is the pointer flip, not
    // bookkeeping after one.
    //
    // Both writes go through the shared conflict retry. The key-value layer
    // is optimistic, so a write that merely *raced* another one comes back
    // as a retryable `TransactionConflict` — nothing about the statement was
    // wrong. The cucumber suite shares one engine across concurrent
    // scenarios, so a writer that assumed exclusivity flakes rather than
    // fails.
    if let Some(existing) = &current {
        crate::assets::writing(|| {
            db.query(
                "UPDATE $id SET is_current = false, current_key = NONE, updated_at = time::now()",
            )
            .bind(("id", record_id(TABLE, existing.id)))
        })
        .await
        .map_err(classify_write)?;
        crate::postgres_mirror::after_write(
            db,
            crate::postgres_mirror::MirrorKey::Template(existing.id),
        )
        .await;
    }

    let id = Uuid::now_v7();
    let mut response = crate::assets::writing(|| {
        db.query(format!(
            "CREATE $id SET \
             code = $code, \
             title = $title, \
             respondent_type = $respondent_type, \
             project_id = $project_id, \
             firm_id = $firm_id, \
             is_current = true, \
             asset_id = $asset_id, \
             form_code = $form_code, \
             kind = $kind, \
             source_commit_sha = $source_commit_sha, \
             body_sha256 = $body_sha256, \
             current_key = $current_key \
             RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind(("code", code.to_string()))
        .bind(("title", version.title.clone()))
        .bind(("respondent_type", version.respondent_type.clone()))
        .bind((
            "project_id",
            scope
                .project_id()
                .map(|p| record_id(crate::projects::PROJECT_TABLE, p)),
        ))
        .bind((
            "firm_id",
            scope.firm_id().map(|f| record_id(crate::firms::TABLE, f)),
        ))
        .bind((
            "asset_id",
            version.asset_id.map(|a| record_id(crate::assets::TABLE, a)),
        ))
        .bind(("form_code", version.form_code.clone()))
        .bind(("kind", version.kind.clone()))
        .bind(("source_commit_sha", version.source_commit_sha.clone()))
        .bind(("body_sha256", body_sha256.clone()))
        .bind(("current_key", current_key_in(scope, code)))
    })
    .await
    .map_err(classify_write)?;

    let row: Option<TemplateRow> = response.take(0)?;
    let template = row
        .and_then(TemplateRow::into_template)
        .ok_or(TemplateError::WriteReturnedNothing)?;
    crate::postgres_mirror::after_write(
        db,
        crate::postgres_mirror::MirrorKey::Template(template.id),
    )
    .await;
    Ok(Saved::Written(template))
}

/// The digest a new version pins, read from its body asset's row.
///
/// The caller never supplies the digest, so it cannot disagree with the
/// asset. A Project or Firm version is a Draft synced from a repository:
/// it must carry a body and name the commit it was read from. A
/// shared-catalog version may omit the commit.
async fn pin_body(
    db: &SurrealDb,
    scope: Scope,
    version: &Version,
) -> Result<Option<String>, TemplateError> {
    if scope.is_synced() {
        if version
            .source_commit_sha
            .as_deref()
            .is_none_or(|sha| sha.trim().is_empty())
        {
            return Err(TemplateError::MissingSourceCommit);
        }
        if version.asset_id.is_none() {
            return Err(TemplateError::MissingBody);
        }
    }
    let Some(asset_id) = version.asset_id else {
        return Ok(None);
    };
    let asset = crate::assets::find_by_id(db, asset_id)
        .await?
        .ok_or(TemplateError::DanglingAsset(asset_id))?;
    Ok(Some(asset.sha256_hex))
}

/// Fetch a Template's markdown body from object storage.
///
/// Reads the asset row back before reaching storage. That read-back is
/// what replaced `fk_templates_asset` when the constraint was dropped: the
/// engine does not validate a `record<asset>` link, so a template naming
/// an asset that was never written is accepted at write time and has to be
/// caught here.
///
/// The bytes are hashed before they are returned. They must match the
/// asset row's digest and, when the version recorded one, its
/// `body_sha256`; anything else fails closed with
/// [`TemplateBodyError::DigestMismatch`].
///
/// # Errors
///
/// [`TemplateBodyError`] when the template has no body, names an asset
/// that does not exist, storage fails, the bytes do not match the pinned
/// digest, or they are not UTF-8.
pub async fn body(
    db: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    template: &Template,
) -> Result<String, TemplateBodyError> {
    body_pinned_to(db, storage, template, None).await
}

/// [`body`], also checked against `pinned_sha256` — the digest a Notation
/// copied when it opened. A Notation renders through this so a body that
/// differs from the one it was opened against is never drawn.
///
/// # Errors
///
/// As [`body`], and [`TemplateBodyError::DigestMismatch`] when the bytes do
/// not hash to `pinned_sha256`.
pub async fn body_pinned_to(
    db: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    template: &Template,
    pinned_sha256: Option<&str>,
) -> Result<String, TemplateBodyError> {
    let asset_id = template
        .asset_id
        .ok_or(TemplateBodyError::MissingBody(template.id))?;
    let asset = crate::assets::find_by_id(db, asset_id)
        .await?
        .ok_or(TemplateBodyError::DanglingAsset(template.id))?;
    let bytes = storage
        .get(&asset.storage_key)
        .await
        .map_err(|e| TemplateBodyError::Asset(crate::assets::AssetError::Storage(e)))?
        .bytes;
    let actual = crate::assets::sha256_hex(&bytes);
    let expected = [
        Some(asset.sha256_hex.as_str()),
        template.body_sha256.as_deref(),
        pinned_sha256,
    ];
    if expected
        .into_iter()
        .flatten()
        .any(|digest| digest != actual)
    {
        return Err(TemplateBodyError::DigestMismatch(template.id));
    }
    String::from_utf8(bytes).map_err(|_| TemplateBodyError::NotUtf8)
}

#[cfg(test)]
mod tests {
    use super::{
        body, resolve, resolve_exact, save_version, Template, TemplateBodyError, TemplateError,
    };
    use crate::surreal::test_support::mem;
    use crate::surreal::SurrealDb;
    use uuid::Uuid;

    async fn fs_storage() -> std::sync::Arc<dyn cloud::StorageService> {
        let dir = std::env::temp_dir().join(format!("navigator-templates-{}", Uuid::now_v7()));
        std::sync::Arc::new(cloud::FsStorage::new(dir).await.unwrap())
    }

    fn version(title: &str, asset: Option<Uuid>) -> super::Version {
        super::Version {
            title: title.into(),
            respondent_type: "entity".into(),
            asset_id: asset,
            form_code: None,
            kind: None,
            source_commit_sha: None,
        }
    }

    /// A project-scoped version is a repository sync, so it carries a body
    /// and a commit; a shared one may carry neither.
    async fn insert_template(db: &SurrealDb, code: &str, project_id: Option<Uuid>) -> Uuid {
        let mut v = version(code, None);
        if project_id.is_some() {
            v.asset_id = Some(crate::test_support::seed_body_asset(db, code.as_bytes()).await);
            v.source_commit_sha = Some(crate::test_support::SYNTHETIC_COMMIT_SHA.into());
        }
        save_version(db, project_id, code, v)
            .await
            .unwrap()
            .into_model()
            .id
    }

    #[tokio::test]
    async fn resolve_prefers_project_then_owning_firm_then_shared() {
        let db = mem().await;
        let firm = crate::test_support::seed_firm_with_brand(&db, "neon").await;
        let project = crate::projects::create(
            &db,
            &crate::projects::NewProject {
                code: format!("firm-scope-{}", Uuid::now_v7()),
                name: "matter".into(),
                status: "open".into(),
                entity_id: crate::test_support::seed_entity(&db).await,
                firm_id: Some(firm),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id;
        let shared = insert_template(&db, "engagement", None).await;
        let mut firm_version = version("engagement", None);
        firm_version.asset_id = Some(crate::test_support::seed_body_asset(&db, b"firm").await);
        firm_version.source_commit_sha = Some(crate::test_support::SYNTHETIC_COMMIT_SHA.into());
        let firm_row =
            super::save_version_in(&db, super::Scope::Firm(firm), "engagement", firm_version)
                .await
                .unwrap()
                .into_model();
        assert_eq!(firm_row.firm_id, Some(firm));
        assert_ne!(
            firm_row.id, shared,
            "the Firm line coexists with the shared one"
        );

        let resolved = resolve(&db, Some(project), "engagement").await.unwrap();
        assert_eq!(resolved.map(|t| t.id), Some(firm_row.id));
        let scoped = insert_template(&db, "engagement", Some(project)).await;
        let resolved = resolve(&db, Some(project), "engagement").await.unwrap();
        assert_eq!(resolved.map(|t| t.id), Some(scoped));
        // A Project with no Firm, and the shared lookup, never see the Firm line.
        let unowned = crate::test_support::seed_project_surreal(&db, "unowned").await;
        let resolved = resolve(&db, Some(unowned), "engagement").await.unwrap();
        assert_eq!(resolved.map(|t| t.id), Some(shared));
        let resolved = resolve(&db, None, "engagement").await.unwrap();
        assert_eq!(resolved.map(|t| t.id), Some(shared));
    }

    #[tokio::test]
    async fn a_firm_version_without_a_commit_is_refused() {
        let db = mem().await;
        let firm = crate::test_support::seed_firm_with_brand(&db, "neon").await;
        let asset = crate::test_support::seed_body_asset(&db, b"firm").await;
        let refused = super::save_version_in(
            &db,
            super::Scope::Firm(firm),
            "engagement",
            version("e", Some(asset)),
        )
        .await;
        assert!(matches!(refused, Err(TemplateError::MissingSourceCommit)));
    }

    #[tokio::test]
    async fn resolve_prefers_project_scoped_then_falls_back_to_shared() {
        let db = mem().await;
        let p = crate::test_support::seed_project_surreal(&db, "matter").await;
        let shared = insert_template(&db, "amendment", None).await;
        let scoped = insert_template(&db, "amendment", Some(p)).await;

        // From the project: the scoped row wins.
        assert_eq!(
            resolve(&db, Some(p), "amendment")
                .await
                .unwrap()
                .unwrap()
                .id,
            scoped
        );
        // No project context: the shared row.
        assert_eq!(
            resolve(&db, None, "amendment").await.unwrap().unwrap().id,
            shared
        );
        // A different project with no scoped row falls back to shared.
        let other = crate::test_support::seed_project_surreal(&db, "other").await;
        assert_eq!(
            resolve(&db, Some(other), "amendment")
                .await
                .unwrap()
                .unwrap()
                .id,
            shared
        );
    }

    /// A shared `code` and a project-scoped one coexist, which is what
    /// `current_key`'s scope carries: empty for the shared line, the
    /// Project id otherwise.
    #[tokio::test]
    async fn shared_and_project_scoped_codes_coexist() {
        let db = mem().await;
        let p = crate::test_support::seed_project_surreal(&db, "matter").await;
        let shared = insert_template(&db, "consent", None).await;
        let scoped = insert_template(&db, "consent", Some(p)).await;

        assert_ne!(shared, scoped);
        assert_eq!(
            resolve_exact(&db, None, "consent")
                .await
                .unwrap()
                .unwrap()
                .id,
            shared
        );
        assert_eq!(
            resolve_exact(&db, Some(p), "consent")
                .await
                .unwrap()
                .unwrap()
                .id,
            scoped
        );
    }

    #[tokio::test]
    async fn save_version_writes_first_version_and_resolve_reads_it() {
        let db = mem().await;
        let saved = save_version(&db, None, "amendment", version("Amendment", None))
            .await
            .unwrap();
        assert!(saved.was_written());
        let current = resolve(&db, None, "amendment").await.unwrap().unwrap();
        assert_eq!(current.id, saved.into_model().id);
        assert!(current.is_current);
    }

    #[tokio::test]
    async fn save_version_persists_and_reads_back_the_kind() {
        let db = mem().await;
        let mut v = version("Retainer", None);
        v.kind = Some("retainer".into());
        save_version(&db, None, "onboarding__letter", v)
            .await
            .unwrap();
        let current = resolve(&db, None, "onboarding__letter")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.kind.as_deref(), Some("retainer"));
    }

    #[tokio::test]
    async fn a_padded_kind_is_stored_trimmed_so_the_retainer_gate_matches() {
        let db = mem().await;
        let mut v = version("Retainer", None);
        v.kind = Some("  retainer  ".into());
        save_version(&db, None, "onboarding__letter", v)
            .await
            .unwrap();
        // Stored trimmed → an exact `== "retainer"` gate matches.
        assert_eq!(
            resolve(&db, None, "onboarding__letter")
                .await
                .unwrap()
                .unwrap()
                .kind
                .as_deref(),
            Some("retainer"),
        );
    }

    #[tokio::test]
    async fn a_whitespace_only_kind_collapses_to_none() {
        let db = mem().await;
        let mut v = version("T", None);
        v.kind = Some("   ".into());
        save_version(&db, None, "amendment", v).await.unwrap();
        assert_eq!(
            resolve(&db, None, "amendment").await.unwrap().unwrap().kind,
            None,
        );
    }

    #[tokio::test]
    async fn changing_only_the_kind_appends_a_new_version() {
        let db = mem().await;
        save_version(&db, None, "offboarding__letter", version("Closing", None))
            .await
            .unwrap();
        let mut with_kind = version("Closing", None);
        with_kind.kind = Some("letter".into());
        let again = save_version(&db, None, "offboarding__letter", with_kind)
            .await
            .unwrap();
        assert!(
            again.was_written(),
            "declaring a kind on an existing template must cut a new version"
        );
        assert_eq!(
            resolve(&db, None, "offboarding__letter")
                .await
                .unwrap()
                .unwrap()
                .kind
                .as_deref(),
            Some("letter"),
        );
    }

    /// `asset_id` is in the change-detection tuple, which is the whole
    /// reason `templates` and `assets` moved in one slice: a new body
    /// means a new version, and that comparison has to see both sides.
    #[tokio::test]
    async fn changing_only_the_asset_appends_a_new_version() {
        let db = mem().await;
        let storage = fs_storage().await;
        let first_body = crate::assets::ingest_content(&db, &storage, b"# v1", "text/markdown")
            .await
            .unwrap();
        let second_body = crate::assets::ingest_content(&db, &storage, b"# v2", "text/markdown")
            .await
            .unwrap();

        let v1 = save_version(&db, None, "deed", version("Deed", Some(first_body)))
            .await
            .unwrap();
        assert!(v1.was_written());
        let v2 = save_version(&db, None, "deed", version("Deed", Some(second_body)))
            .await
            .unwrap();
        assert!(
            v2.was_written(),
            "a new body must cut a new version even when every other field matches"
        );
        assert_eq!(
            resolve(&db, None, "deed").await.unwrap().unwrap().asset_id,
            Some(second_body)
        );
    }

    #[tokio::test]
    async fn unchanged_re_save_is_a_no_op() {
        let db = mem().await;
        let first = save_version(&db, None, "amendment", version("Amendment", None))
            .await
            .unwrap()
            .into_model();
        let again = save_version(&db, None, "amendment", version("Amendment", None))
            .await
            .unwrap();
        assert!(
            !again.was_written(),
            "an identical spec must not churn versions"
        );
        assert_eq!(again.into_model().id, first.id);
    }

    #[tokio::test]
    async fn changing_a_template_appends_a_version_and_pins_the_old_one() {
        let db = mem().await;
        let v1 = save_version(&db, None, "amendment", version("Amendment", None))
            .await
            .unwrap()
            .into_model();
        let v2 = save_version(&db, None, "amendment", version("Amendment v2", None))
            .await
            .unwrap();
        assert!(v2.was_written());
        let v2 = v2.into_model();
        assert_ne!(v1.id, v2.id, "a change appends a new row, never rewrites");

        // resolve returns the new current version.
        assert_eq!(
            resolve(&db, None, "amendment").await.unwrap().unwrap().id,
            v2.id
        );
        // The old row survives, retired — a Notation that pinned
        // `template_id = v1` still finds its exact bytes.
        let pinned = super::find_by_id(&db, v1.id).await.unwrap().unwrap();
        assert!(!pinned.is_current);
        assert_eq!(pinned.title, "Amendment");
    }

    /// The shape the seed and `cli import` actually produce: several
    /// writers racing to save the *same* spec on every boot. Every one of
    /// them must succeed and agree on the row — a loser that surfaced
    /// `CurrentVersionTaken` here would fail a boot over a no-op.
    #[tokio::test]
    async fn concurrent_re_seed_of_one_spec_settles_on_one_row_without_erroring() {
        let db = mem().await;
        let racers: Vec<_> = (0..6)
            .map(|_| {
                let db = db.clone();
                tokio::spawn(async move {
                    save_version(&db, None, "amendment", version("Amendment", None)).await
                })
            })
            .collect();

        let mut ids = Vec::new();
        for racer in racers {
            ids.push(
                racer
                    .await
                    .expect("task must not panic")
                    .expect("an identical re-seed must never surface a conflict")
                    .into_model()
                    .id,
            );
        }
        assert!(
            ids.windows(2).all(|w| w[0] == w[1]),
            "the racers disagreed about which row won: {ids:?}"
        );
        assert_eq!(
            super::versions_of(&db, None, "amendment")
                .await
                .unwrap()
                .len(),
            1,
            "one version, not six"
        );
    }

    /// The seed and `cli import` both run on every boot, and the cucumber
    /// suite runs concurrent scenarios against one engine, so racing
    /// writers must settle on one current row rather than forking the
    /// version line. The `template_current_key` UNIQUE index is what makes
    /// that true — `save_version` retires and inserts in two statements,
    /// and a multi-statement Surreal query is not one transaction.
    #[tokio::test]
    async fn concurrent_save_version_leaves_exactly_one_current_row() {
        let db = mem().await;
        let racers: Vec<_> = (0..6)
            .map(|n| {
                let db = db.clone();
                tokio::spawn(async move {
                    save_version(&db, None, "amendment", version(&format!("v{n}"), None)).await
                })
            })
            .collect();

        for racer in racers {
            // A loser is refused by the index rather than forking the line;
            // either outcome is acceptable, a panic is not.
            let _ = racer.await.expect("task must not panic");
        }

        let current: Vec<Template> = super::list_current(&db).await.unwrap();
        assert_eq!(
            current.iter().filter(|t| t.code == "amendment").count(),
            1,
            "the racers forked the version line: {current:?}"
        );
    }

    #[tokio::test]
    async fn body_reads_the_markdown_back_from_object_storage() {
        let db = mem().await;
        let storage = fs_storage().await;
        let asset_id =
            crate::assets::ingest_content(&db, &storage, b"# Deed\n\n{{buyer}}", "text/markdown")
                .await
                .unwrap();
        let tmpl = save_version(&db, None, "deed", version("Deed", Some(asset_id)))
            .await
            .unwrap()
            .into_model();
        let text = body(&db, &storage, &tmpl).await.unwrap();
        assert_eq!(text, "# Deed\n\n{{buyer}}");
    }

    #[tokio::test]
    async fn body_reports_a_template_with_no_stored_body() {
        let db = mem().await;
        let storage = fs_storage().await;
        let tmpl = save_version(&db, None, "deed", version("Deed", None))
            .await
            .unwrap()
            .into_model();
        assert!(matches!(
            body(&db, &storage, &tmpl).await,
            Err(TemplateBodyError::MissingBody(_))
        ));
    }

    /// The engine does not validate a `record<asset>` link, so the write
    /// path reads the asset back to take its digest and refuses a version
    /// naming an asset that was never written.
    #[tokio::test]
    async fn a_version_naming_an_asset_that_does_not_exist_is_refused() {
        let db = mem().await;
        let missing = Uuid::now_v7();
        let refused = save_version(&db, None, "deed", version("Deed", Some(missing))).await;
        assert!(
            matches!(refused, Err(TemplateError::DanglingAsset(id)) if id == missing),
            "{:?}",
            refused.err()
        );
        assert!(resolve(&db, None, "deed").await.unwrap().is_none());
    }

    /// A row written before the write path checked the link still reads
    /// back as dangling at render time.
    #[tokio::test]
    async fn body_reports_a_legacy_template_naming_an_asset_that_does_not_exist() {
        let db = mem().await;
        let storage = fs_storage().await;
        let tmpl = save_version(&db, None, "deed", version("Deed", None))
            .await
            .unwrap()
            .into_model();
        let dangling = Template {
            asset_id: Some(Uuid::now_v7()),
            ..tmpl
        };
        assert!(matches!(
            body(&db, &storage, &dangling).await,
            Err(TemplateBodyError::DanglingAsset(_))
        ));
    }

    /// A Draft synced from a Project repository names its commit and
    /// carries a body; either missing refuses the write and stores nothing.
    #[tokio::test]
    async fn a_repository_sync_without_a_commit_or_a_body_is_refused() {
        let db = mem().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let asset = crate::test_support::seed_body_asset(&db, b"# Lease").await;

        let no_commit =
            save_version(&db, Some(project), "lease", version("Lease", Some(asset))).await;
        assert!(matches!(no_commit, Err(TemplateError::MissingSourceCommit)));

        let mut blank = version("Lease", Some(asset));
        blank.source_commit_sha = Some("  ".into());
        let blank = save_version(&db, Some(project), "lease", blank).await;
        assert!(matches!(blank, Err(TemplateError::MissingSourceCommit)));

        let mut no_body = version("Lease", None);
        no_body.source_commit_sha = Some(crate::test_support::SYNTHETIC_COMMIT_SHA.into());
        let no_body = save_version(&db, Some(project), "lease", no_body).await;
        assert!(matches!(no_body, Err(TemplateError::MissingBody)));

        assert!(super::versions_of(&db, Some(project), "lease")
            .await
            .unwrap()
            .is_empty());
    }

    /// The write copies the digest off the asset row. A shared-catalog
    /// version may omit the commit but still carries the digest.
    #[tokio::test]
    async fn every_version_with_a_body_pins_its_digest() {
        let db = mem().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let bytes = b"# Lease\n\n{{tenant}}";
        let asset = crate::test_support::seed_body_asset(&db, bytes).await;

        let shared = save_version(&db, None, "lease", version("Lease", Some(asset)))
            .await
            .unwrap()
            .into_model();
        assert_eq!(shared.source_commit_sha, None);
        assert_eq!(
            shared.body_sha256.as_deref(),
            Some(crate::assets::sha256_hex(bytes).as_str())
        );

        let mut synced = version("Lease", Some(asset));
        synced.source_commit_sha = Some(crate::test_support::SYNTHETIC_COMMIT_SHA.into());
        let synced = save_version(&db, Some(project), "lease", synced)
            .await
            .unwrap()
            .into_model();
        assert_eq!(
            synced.source_commit_sha.as_deref(),
            Some(crate::test_support::SYNTHETIC_COMMIT_SHA)
        );
        assert_eq!(synced.body_sha256, shared.body_sha256);
        assert_eq!(
            super::find_by_id(&db, synced.id).await.unwrap().unwrap(),
            synced,
            "the pin reads back"
        );
    }

    /// A row written before the pins existed never counts as unchanged, so
    /// the next sync of the same bytes appends a pinned version.
    #[tokio::test]
    async fn a_sync_over_an_unpinned_legacy_row_appends_a_pinned_version() {
        let db = mem().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let asset = crate::test_support::seed_body_asset(&db, b"# Lease").await;
        let mut synced = version("Lease", Some(asset));
        synced.source_commit_sha = Some(crate::test_support::SYNTHETIC_COMMIT_SHA.into());
        let first = save_version(&db, Some(project), "lease", synced)
            .await
            .unwrap()
            .into_model();
        db.query("UPDATE $id UNSET source_commit_sha, body_sha256")
            .bind(("id", crate::surreal::record_id(super::TABLE, first.id)))
            .await
            .unwrap()
            .check()
            .unwrap();

        let mut again = version("Lease", Some(asset));
        again.source_commit_sha = Some(crate::test_support::SYNTHETIC_COMMIT_SHA.into());
        let again = save_version(&db, Some(project), "lease", again)
            .await
            .unwrap();
        assert!(again.was_written());
        let current = again.into_model();
        assert_ne!(current.id, first.id);
        assert!(current.body_sha256.is_some());
        assert!(current.source_commit_sha.is_some());
    }

    /// Bytes that no longer hash to the pinned digest fail closed.
    #[tokio::test]
    async fn a_tampered_body_fails_the_digest_check_closed() {
        let db = mem().await;
        let storage = fs_storage().await;
        let asset_id = crate::assets::ingest_content(&db, &storage, b"# Deed", "text/markdown")
            .await
            .unwrap();
        let tmpl = save_version(&db, None, "deed", version("Deed", Some(asset_id)))
            .await
            .unwrap()
            .into_model();
        let asset = crate::assets::find_by_id(&db, asset_id)
            .await
            .unwrap()
            .unwrap();
        storage
            .put(&asset.storage_key, b"# Forged deed", "text/markdown")
            .await
            .unwrap();
        assert!(matches!(
            body(&db, &storage, &tmpl).await,
            Err(TemplateBodyError::DigestMismatch(id)) if id == tmpl.id
        ));
        let other = crate::assets::sha256_hex(b"# Something else");
        storage
            .put(&asset.storage_key, b"# Deed", "text/markdown")
            .await
            .unwrap();
        assert_eq!(body(&db, &storage, &tmpl).await.unwrap(), "# Deed");
        assert!(matches!(
            super::body_pinned_to(&db, &storage, &tmpl, Some(&other)).await,
            Err(TemplateBodyError::DigestMismatch(_))
        ));
    }

    #[tokio::test]
    async fn a_missing_template_reads_back_as_none() {
        let db = mem().await;
        assert!(super::find_by_id(&db, Uuid::now_v7())
            .await
            .unwrap()
            .is_none());
        assert!(super::find_by_ids(&db, &[]).await.unwrap().is_empty());
    }
}
