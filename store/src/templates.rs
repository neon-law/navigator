//! Template resolution, content pins, and immutable versions.
//!
//! PostgreSQL serves application reads and writes after the Template history
//! is enrolled. Each scope has one current key; a version change atomically
//! retires that pointer and appends a row, preserving every pinned version.
//! Project, then owning Firm, then shared resolution keeps Firm Libraries
//! private. The explicit importer Source context retains compatibility reads.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use cloud::StorageService;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::{matter, templates as native};

/// The table these rows live in.
pub(crate) const TABLE: &str = "template";

/// One template version.
///
/// The application-facing shape: plain Rust types, no engine handles.
/// PostgreSQL and the frozen Source decoder produce the same model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    #[error(transparent)]
    Authority(#[from] matter::MatterAuthorityError),
    #[error("PostgreSQL Template operation failed")]
    Postgres(#[from] sqlx::Error),
    /// Another writer owns the unique current key for this version line.
    #[error("that template code already has a current version")]
    CurrentVersionTaken,
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

    pub(crate) fn project_id(self) -> Option<Uuid> {
        match self {
            Self::Project(id) => Some(id),
            Self::Shared | Self::Firm(_) => None,
        }
    }

    pub(crate) fn firm_id(self) -> Option<Uuid> {
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

/// Resolve a template by id — what a Notation does with the
/// `template_id` it pinned.
///
/// # Errors
///
/// [`TemplateError`] if storage admission or the lookup fails.
pub async fn find_by_id(
    _db: &ApplicationStore,
    id: Uuid,
) -> Result<Option<Template>, TemplateError> {
    let pg = matter::authority_for(TABLE).await?;
    native::find_by_id(&pg, id).await
}

/// Resolve many templates by id in one round trip — what a listing does
/// after reading a page of notations, rather than one lookup per row.
///
/// # Errors
///
/// [`TemplateError`] if storage admission or the lookup fails.
pub async fn find_by_ids(
    _db: &ApplicationStore,
    ids: &[Uuid],
) -> Result<Vec<Template>, TemplateError> {
    let pg = matter::authority_for(TABLE).await?;
    native::find_by_ids(&pg, ids).await
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
/// [`TemplateError`] if storage admission or the lookup fails.
pub async fn resolve(
    db: &ApplicationStore,
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
/// [`TemplateError`] if storage admission or the lookup fails.
pub async fn resolve_for_firm(
    db: &ApplicationStore,
    firm_id: Uuid,
    code: &str,
) -> Result<Option<Template>, TemplateError> {
    if let Some(firm) = resolve_in(db, Scope::Firm(firm_id), code).await? {
        return Ok(Some(firm));
    }
    resolve_in(db, Scope::Shared, code).await
}

/// The Firm that owns `project_id`, if any.
async fn owning_firm(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<Option<Uuid>, TemplateError> {
    let pg = matter::authority_for(TABLE).await?;
    native::owning_firm(&pg, project_id).await
}

/// The current row for an exact `(project_id, code)` — no shared
/// fallback, unlike [`resolve`]. [`save_version`]'s pointer flip must act
/// on the same scope it writes to.
///
/// # Errors
///
/// [`TemplateError`] if storage admission or the lookup fails.
pub async fn resolve_exact(
    db: &ApplicationStore,
    project_id: Option<Uuid>,
    code: &str,
) -> Result<Option<Template>, TemplateError> {
    resolve_in(db, Scope::of_project(project_id), code).await
}

/// The current row for an exact `(scope, code)`, with no fallback.
///
/// # Errors
///
/// [`TemplateError`] if storage admission or the lookup fails.
pub async fn resolve_in(
    _db: &ApplicationStore,
    scope: Scope,
    code: &str,
) -> Result<Option<Template>, TemplateError> {
    let pg = matter::authority_for(TABLE).await?;
    native::resolve(&pg, scope, code).await
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
/// [`TemplateError`] if storage admission or the lookup fails.
pub async fn versions_of(
    _db: &ApplicationStore,
    project_id: Option<Uuid>,
    code: &str,
) -> Result<Vec<Template>, TemplateError> {
    let pg = matter::authority_for(TABLE).await?;
    native::versions(&pg, project_id, code).await
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
/// [`TemplateError`] if storage admission or the lookup fails.
pub async fn list_current(_db: &ApplicationStore) -> Result<Vec<Template>, TemplateError> {
    let pg = matter::authority_for(TABLE).await?;
    native::list(&pg, None).await
}

/// Every current template in one Firm's Library, ordered by code.
///
/// # Errors
///
/// [`TemplateError`] if storage admission or the lookup fails.
pub async fn list_for_firm(
    _db: &ApplicationStore,
    firm_id: Uuid,
) -> Result<Vec<Template>, TemplateError> {
    let pg = matter::authority_for(TABLE).await?;
    native::list(&pg, Some(Scope::Firm(firm_id))).await
}

/// Every current template scoped to one Project, ordered by code.
///
/// # Errors
///
/// [`TemplateError`] if storage admission or the lookup fails.
pub async fn list_for_project(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<Template>, TemplateError> {
    let pg = matter::authority_for(TABLE).await?;
    native::list(&pg, Some(Scope::Project(project_id))).await
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
/// [`TemplateError`] if storage admission or the lookup fails.
pub async fn exists_for_project(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<bool, TemplateError> {
    let pg = matter::authority_for(TABLE).await?;
    native::exists_for_project(&pg, project_id).await
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
/// PostgreSQL holds the version-line lock while it compares the current row,
/// retires its pointer, and appends the replacement in one transaction. A
/// same-spec contender reuses the winner and its original provenance.
///
/// # Errors
///
/// [`TemplateError`] when storage admission, required pins, or the atomic
/// version write fails.
pub async fn save_version(
    db: &ApplicationStore,
    project_id: Option<Uuid>,
    code: &str,
    version: Version,
) -> Result<Saved, TemplateError> {
    save_version_in(db, Scope::of_project(project_id), code, version).await
}

pub(crate) async fn write_authority() -> Result<crate::postgres::PostgresDb, TemplateError> {
    Ok(matter::authority_for(TABLE).await?)
}

/// [`save_version`] for any [`Scope`] — how a Firm Library version is
/// written. A Firm version, like a Project one, is synced from a
/// repository and must name its commit and carry a body.
///
/// # Errors
///
/// As [`save_version`].
pub async fn save_version_in(
    db: &ApplicationStore,
    scope: Scope,
    code: &str,
    mut version: Version,
) -> Result<Saved, TemplateError> {
    let pg = write_authority().await?;
    version.kind = normalized_kind(version.kind);
    let digest = pin_body(db, scope, &version).await?;
    native::save_version(&pg, scope, code, &version, digest.as_deref()).await
}

/// Whether `row` already carries exactly the spec `version` describes.
/// `code` and `project_id` locate the version line; `source_commit_sha` is
/// provenance and deliberately absent, so byte-identical bytes re-read at a
/// newer commit stay [`Saved::Unchanged`].
///
/// A row that is missing a pin the current write path would record — a
/// digest for its body, or a commit for a Project or Firm version — never
/// matches, so the next sync appends a fully pinned version instead of
/// keeping the unpinned one current.
pub(crate) fn matches_spec(row: &Template, version: &Version) -> bool {
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

/// The digest a new version pins, read from its body asset's row.
///
/// The caller never supplies the digest, so it cannot disagree with the
/// asset. A Project or Firm version is a Draft synced from a repository:
/// it must carry a body and name the commit it was read from. A
/// shared-catalog version may omit the commit.
async fn pin_body(
    db: &ApplicationStore,
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
/// Reads the canonical Asset row before reaching storage.
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
    db: &ApplicationStore,
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
    db: &ApplicationStore,
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

/// Whether an open run pinned at `pinned_sha` has been overtaken by the
/// template's current definition on the repository's main (LAW-201).
///
/// `None` on either side means staleness cannot be proven, and a run is
/// never flagged on what cannot be proven: a run pinned before digests
/// existed, or a template removed from main (a removal is its own
/// lifecycle, not a hash change). Conservative by design — the board
/// flags only a provably stale run.
#[must_use]
pub fn run_is_stale(pinned_sha: Option<&str>, current_sha: Option<&str>) -> bool {
    match (pinned_sha, current_sha) {
        (Some(pinned), Some(current)) => pinned != current,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_run_is_stale_only_when_both_digests_are_known_and_differ() {
        assert!(super::run_is_stale(Some("aaa"), Some("bbb")));
        assert!(!super::run_is_stale(Some("aaa"), Some("aaa")));
        assert!(!super::run_is_stale(None, Some("bbb")));
        assert!(!super::run_is_stale(Some("aaa"), None));
        assert!(!super::run_is_stale(None, None));
    }
}
