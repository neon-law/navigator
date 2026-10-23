//! One filled-in template — the notation core — and every query against
//! the table.
//!
//! # This table lives in SurrealDB
//!
//! `notations` moved with wave five of #1093 (ENG-121), the heaviest slice
//! of the notation group: `template_id`, `person_id`, `entity_id`, and
//! `project_id` are real `record<…>` links now that every table they name
//! is Surreal-resident, but the engine does not validate a link, so every
//! read-back a caller relied on the (already-dropped) foreign keys for
//! still has to happen in Rust — see [`find_by_id`] and its callers.
//!
//! One thing this module owns beyond CRUD: **rendered-document storage
//! keys** — the per-notation object-store convention every writer and
//! reader of the signing walk shares.
//!
//! # No delete — except undoing our own aborted create
//!
//! A real notation is never deleted, only state-transitioned. The one
//! exception is [`delete_uncommitted`]: a self-serve intake creates its
//! Notation before running the conflict check, and a blocking conflict
//! refuses the matter before the client has ever seen it — the row was
//! never a legal record to begin with, so the caller's own compensation
//! for its own aborted request is not the same operation as deleting a
//! filed notation, and callers must not reach for it for anything else.
//!
//! # No unique index, no write retry
//!
//! Unlike [`crate::templates`] or [`crate::mailrooms`], `notation` carries
//! no unique index — every row is addressed by its own fresh id, never a
//! natural key two writers could race on. [`create`] therefore does not
//! retry a transaction conflict, the same reasoning [`crate::answers`]
//! gives for its own append: there is no row for two writers to contend
//! over. The single-row updates below (`update_state`,
//! `update_questionnaire_snapshot`) act on an id the caller already
//! resolved, and a lost race there is a genuinely concurrent edit of the
//! same row — worth retrying, the way every other store module's `writing`
//! helper does.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::surreal::{record_id, record_uuid, retry, SurrealDb};

/// The table these rows live in.
pub(crate) const TABLE: &str = "notation";
const PERSON_TABLE: &str = "person";
const ENTITY_TABLE: &str = "entity";
const TEMPLATE_TABLE: &str = "template";
const NOTATION_PACKAGE_TABLE: &str = "notation_package";

/// Captive client recipient: signs embedded inside Neon Law Navigator (no
/// email). The historical retainer-walk default.
pub const DELIVERY_EMBEDDED: &str = "embedded";

/// Non-captive client recipient: DocuSign emails a signing link the client
/// opens from their own inbox. The matter-open default — a brand-new client
/// opened from the admin page is not in the room.
pub const DELIVERY_EMAILED: &str = "emailed";

/// One filled-in template, owned by a person (and optionally an entity)
/// inside exactly one Project.
///
/// The application-facing shape: plain Rust types, no engine handles.
/// [`NotationRow`] is the seam that turns it into (and back out of) what
/// the SDK reads and writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notation {
    pub id: Uuid,
    pub template_id: Uuid,
    /// The package this notation belongs to. A package groups several
    /// templates into one client-facing legal deliverable.
    pub package_id: Option<Uuid>,
    pub person_id: Uuid,
    pub entity_id: Option<Uuid>,
    /// Every Notation belongs to exactly one Project — the glossary's
    /// load-bearing rule for the matter-audit-trail story.
    pub project_id: Uuid,
    /// `BEGIN`, `lawyer_review`, `sent_for_signature__pending`, …
    pub state: String,
    /// How the client receives this notation for signature:
    /// [`DELIVERY_EMBEDDED`] or [`DELIVERY_EMAILED`].
    pub delivery: String,
    /// The frozen questionnaire the Notation was opened against — written
    /// once at creation; render/step/fill resolve against it so a later
    /// template edit can't re-route this Notation. `None` for a Notation
    /// created with no snapshot.
    pub questionnaire_snapshot: Option<Json>,
    /// Commit SHA in the Project's repo holding this notation's rendered
    /// document. `None` until the durable commit step lands.
    pub git_commit_sha: Option<String>,
    /// The Draft this Notation was opened from, copied off the pinned
    /// template version by [`create`] and never rewritten: the Project
    /// repository commit, `None` for a shared-catalog Draft.
    pub template_source_commit_sha: Option<String>,
    /// The body digest copied at [`create`]. [`pinned_body`] refuses bytes
    /// that do not hash to it, so a later edit to `drafts/` cannot change
    /// what this Notation says.
    pub template_body_sha256: Option<String>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The row as the engine reads and writes it.
#[derive(SurrealValue)]
struct NotationRow {
    id: surrealdb::types::RecordId,
    template_id: surrealdb::types::RecordId,
    package_id: Option<surrealdb::types::RecordId>,
    person_id: surrealdb::types::RecordId,
    entity_id: Option<surrealdb::types::RecordId>,
    project_id: surrealdb::types::RecordId,
    state: String,
    delivery: String,
    questionnaire_snapshot: Option<Json>,
    git_commit_sha: Option<String>,
    template_source_commit_sha: Option<String>,
    template_body_sha256: Option<String>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl NotationRow {
    /// `None` when a record id is not a native UUID key — a row written
    /// by something that bypassed [`crate::surreal::record_id`].
    fn into_notation(self) -> Option<Notation> {
        Some(Notation {
            id: record_uuid(&self.id)?,
            template_id: record_uuid(&self.template_id)?,
            package_id: self.package_id.as_ref().and_then(record_uuid),
            person_id: record_uuid(&self.person_id)?,
            entity_id: self.entity_id.as_ref().and_then(record_uuid),
            project_id: record_uuid(&self.project_id)?,
            state: self.state,
            delivery: self.delivery,
            questionnaire_snapshot: self.questionnaire_snapshot,
            git_commit_sha: self.git_commit_sha,
            template_source_commit_sha: self.template_source_commit_sha,
            template_body_sha256: self.template_body_sha256,
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

/// The projection every read shares, so one field list describes the row
/// and a new column cannot reach [`NotationRow`] from only one query.
const SELECT: &str =
    "id, template_id, package_id, person_id, entity_id, project_id, state, delivery, \
     questionnaire_snapshot, git_commit_sha, template_source_commit_sha, template_body_sha256, \
     inserted_at, updated_at";

/// Everything [`create`] needs to open a Notation.
#[derive(Debug, Clone)]
pub struct NewNotation {
    pub template_id: Uuid,
    pub package_id: Option<Uuid>,
    pub person_id: Uuid,
    pub entity_id: Option<Uuid>,
    pub project_id: Uuid,
    pub state: String,
    pub delivery: String,
    pub questionnaire_snapshot: Option<Json>,
}

impl NewNotation {
    /// A bare Notation at `state`, embedded delivery, no entity and no
    /// snapshot — narrow it with the builders below.
    #[must_use]
    pub fn new(
        template_id: Uuid,
        person_id: Uuid,
        project_id: Uuid,
        state: impl Into<String>,
    ) -> Self {
        Self {
            template_id,
            package_id: None,
            person_id,
            entity_id: None,
            project_id,
            state: state.into(),
            delivery: DELIVERY_EMBEDDED.to_string(),
            questionnaire_snapshot: None,
        }
    }

    #[must_use]
    pub fn with_entity(mut self, entity_id: Uuid) -> Self {
        self.entity_id = Some(entity_id);
        self
    }

    /// Associate this template instance with the package that assembled it.
    #[must_use]
    pub fn with_package(mut self, package_id: Uuid) -> Self {
        self.package_id = Some(package_id);
        self
    }

    #[must_use]
    pub fn with_delivery(mut self, delivery: impl Into<String>) -> Self {
        self.delivery = delivery.into();
        self
    }

    #[must_use]
    pub fn with_questionnaire_snapshot(mut self, snapshot: Json) -> Self {
        self.questionnaire_snapshot = Some(snapshot);
        self
    }
}

/// Errors reading or writing a notation.
#[derive(Debug, thiserror::Error)]
pub enum NotationError {
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    #[error("notation {0} not found")]
    NotFound(Uuid),
    /// A write reported success but returned no row, or returned one this
    /// module could not read back.
    #[error("writing a notation returned no usable row")]
    WriteReturnedNothing,
}

/// Run a write under the shared retry policy
/// ([`crate::surreal::retry`]), mapping whatever finally comes back to
/// this module's error.
///
/// Only the mapping lives here. How long a lost race is re-run, and
/// which engine conditions count as a lost race, are one policy for the
/// whole crate.
async fn writing<F, Q>(attempt: F) -> Result<surrealdb::IndexedResults, NotationError>
where
    F: FnMut() -> Q,
    Q: std::future::IntoFuture<Output = Result<surrealdb::IndexedResults, surrealdb::Error>>,
{
    retry::writing(attempt).await.map_err(NotationError::Db)
}

fn one(mut response: surrealdb::IndexedResults) -> Result<Option<Notation>, NotationError> {
    let row: Option<NotationRow> = response.take(0)?;
    Ok(row.and_then(NotationRow::into_notation))
}

fn many(mut response: surrealdb::IndexedResults) -> Result<Vec<Notation>, NotationError> {
    let rows: Vec<NotationRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(NotationRow::into_notation)
        .collect())
}

/// Write one archived Notation back under its original id with its frozen
/// questionnaire, template pins, and timestamps. An identical row is left
/// alone; a different row with the same id is a conflict.
///
/// # Errors
/// [`NotationError`] on a database failure.
pub async fn restore_archived(
    db: &SurrealDb,
    notation: &Notation,
) -> Result<crate::assets::RowRestore, NotationError> {
    if let Some(existing) = find_by_id(db, notation.id).await? {
        return Ok(if &existing == notation {
            crate::assets::RowRestore::Identical
        } else {
            crate::assets::RowRestore::Conflict
        });
    }
    let response = writing(|| {
        db.query(format!(
            "CREATE $id SET template_id = $template_id, package_id = $package_id, \
             person_id = $person_id, entity_id = $entity_id, project_id = $project_id, \
             state = $state, delivery = $delivery, \
             questionnaire_snapshot = $questionnaire_snapshot, git_commit_sha = $git_commit_sha, \
             template_source_commit_sha = $template_source_commit_sha, \
             template_body_sha256 = $template_body_sha256, inserted_at = $inserted_at, \
             updated_at = $updated_at RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, notation.id)))
        .bind(("template_id", record_id("template", notation.template_id)))
        .bind((
            "package_id",
            notation
                .package_id
                .map(|id| record_id("notation_package", id)),
        ))
        .bind(("person_id", record_id("person", notation.person_id)))
        .bind((
            "entity_id",
            notation.entity_id.map(|id| record_id("entity", id)),
        ))
        .bind(("project_id", record_id("project", notation.project_id)))
        .bind(("state", notation.state.clone()))
        .bind(("delivery", notation.delivery.clone()))
        .bind((
            "questionnaire_snapshot",
            notation.questionnaire_snapshot.clone(),
        ))
        .bind(("git_commit_sha", notation.git_commit_sha.clone()))
        .bind((
            "template_source_commit_sha",
            notation.template_source_commit_sha.clone(),
        ))
        .bind((
            "template_body_sha256",
            notation.template_body_sha256.clone(),
        ))
        .bind((
            "inserted_at",
            surrealdb::types::Datetime::from(notation.inserted_at),
        ))
        .bind((
            "updated_at",
            surrealdb::types::Datetime::from(notation.updated_at),
        ))
    })
    .await?;
    one(response)?.ok_or(NotationError::WriteReturnedNothing)?;
    Ok(crate::assets::RowRestore::Created)
}

/// Open a Notation. No unique index guards this insert — see the module
/// header for why no retry wraps it.
///
/// The same statement copies the pinned template version's
/// `source_commit_sha` and `body_sha256` onto the row, so the Notation
/// carries the exact Draft it was opened from.
///
/// # Errors
///
/// [`NotationError::Db`] if the insert fails, or
/// [`NotationError::WriteReturnedNothing`] if it reports success but
/// returns no row.
pub async fn create(db: &SurrealDb, new: &NewNotation) -> Result<Notation, NotationError> {
    let id = Uuid::now_v7();
    let mut response = db
        .query(format!(
            "CREATE $id SET \
             template_id = $template_id, \
             package_id = $package_id, \
             person_id = $person_id, \
             entity_id = $entity_id, \
             project_id = $project_id, \
             state = $state, \
             delivery = $delivery, \
             questionnaire_snapshot = $questionnaire_snapshot, \
             template_source_commit_sha = $template_id.source_commit_sha, \
             template_body_sha256 = $template_id.body_sha256 \
             RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind(("template_id", record_id(TEMPLATE_TABLE, new.template_id)))
        .bind((
            "package_id",
            new.package_id
                .map(|id| record_id(NOTATION_PACKAGE_TABLE, id)),
        ))
        .bind(("person_id", record_id(PERSON_TABLE, new.person_id)))
        .bind((
            "entity_id",
            new.entity_id.map(|e| record_id(ENTITY_TABLE, e)),
        ))
        .bind((
            "project_id",
            record_id(crate::projects::PROJECT_TABLE, new.project_id),
        ))
        .bind(("state", new.state.clone()))
        .bind(("delivery", new.delivery.clone()))
        .bind(("questionnaire_snapshot", new.questionnaire_snapshot.clone()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;

    let row: Option<NotationRow> = response.take(0)?;
    let notation = row
        .and_then(NotationRow::into_notation)
        .ok_or(NotationError::WriteReturnedNothing)?;
    crate::postgres_mirror::after_write(db, crate::postgres_mirror::MirrorKey::Notation(id)).await;
    Ok(notation)
}

/// Errors from [`pinned_body`].
#[derive(Debug, thiserror::Error)]
pub enum PinnedBodyError {
    #[error("template: {0}")]
    Template(#[from] crate::templates::TemplateError),
    /// The Notation names a template version that no longer reads back.
    #[error("notation {notation} pins template {template}, which does not exist")]
    MissingTemplate { notation: Uuid, template: Uuid },
    #[error("body: {0}")]
    Body(#[from] crate::templates::TemplateBodyError),
}

/// The Draft body this Notation was opened against, verified against the
/// digest it copied at [`create`] before any renderer draws it.
///
/// Fails closed: bytes that do not hash to the pinned digest return
/// [`crate::templates::TemplateBodyError::DigestMismatch`] and no body. A
/// Notation opened before the digest existed carries none and is checked
/// against its template version's digest alone.
///
/// # Errors
///
/// [`PinnedBodyError`] when the template version is missing, its body
/// cannot be read, or the bytes do not match.
pub async fn pinned_body(
    db: &SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    notation: &Notation,
) -> Result<String, PinnedBodyError> {
    let template = crate::templates::find_by_id(db, notation.template_id)
        .await?
        .ok_or(PinnedBodyError::MissingTemplate {
            notation: notation.id,
            template: notation.template_id,
        })?;
    Ok(crate::templates::body_pinned_to(
        db,
        storage,
        &template,
        notation.template_body_sha256.as_deref(),
    )
    .await?)
}

/// Undo a Notation this same request just created and is aborting before
/// it was ever shown to anyone — see the module header's "No delete"
/// section. Not for removing a real notation; there is no other way to do
/// that, on purpose.
///
/// # Errors
///
/// [`NotationError::Db`] if the delete fails.
pub async fn delete_uncommitted(db: &SurrealDb, id: Uuid) -> Result<(), NotationError> {
    db.query("DELETE $id")
        .bind(("id", record_id(TABLE, id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    crate::postgres_mirror::after_write(db, crate::postgres_mirror::MirrorKey::Notation(id)).await;
    Ok(())
}

/// Resolve a notation by id.
///
/// # Errors
///
/// [`NotationError::Db`] if the lookup fails.
pub async fn find_by_id(db: &SurrealDb, id: Uuid) -> Result<Option<Notation>, NotationError> {
    let response = db
        .query(format!("SELECT {SELECT} FROM ONLY $id LIMIT 1"))
        .bind(("id", record_id(TABLE, id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let notation = one(response)?;
    crate::postgres_shadow::notation_read(id, notation.as_ref());
    Ok(notation)
}

/// Every notation on a Project, newest first.
///
/// # Errors
///
/// [`NotationError::Db`] if the lookup fails.
pub async fn list_by_project(
    db: &SurrealDb,
    project_id: Uuid,
) -> Result<Vec<Notation>, NotationError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE project_id = $project \
             ORDER BY inserted_at DESC"
        ))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}

/// Every notation across several Projects, newest first — the batched
/// resolver behind "which of these matters have an engagement/closing
/// notation" checks.
///
/// # Errors
///
/// [`NotationError::Db`] if the lookup fails.
pub async fn list_by_projects(
    db: &SurrealDb,
    project_ids: &[Uuid],
) -> Result<Vec<Notation>, NotationError> {
    if project_ids.is_empty() {
        return Ok(Vec::new());
    }
    let keys: Vec<surrealdb::types::RecordId> = project_ids
        .iter()
        .map(|id| record_id(crate::projects::PROJECT_TABLE, *id))
        .collect();
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE project_id IN $projects \
             ORDER BY inserted_at DESC"
        ))
        .bind(("projects", keys))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}

/// How far a state has progressed through its workflow — used only to pick
/// which of a matter's several Notations decides its row on the lawyer
/// Projects list.
///
/// This is a deliberately separate table from
/// `workflows::client_phrase_for`'s phrase catalog: that one answers "what do
/// we tell the client" from a state name, this one answers "how far along is
/// it" from the same name, and conflating them would make a phrasing change
/// accidentally reorder the fold. `store` does not depend on `workflows` (the
/// dependency runs the other way — `workflows` depends on `store`), so this
/// table is its own, keyed on the same state-name prefixes.
///
/// `BEGIN` ranks lowest — nothing has happened yet. `END` ranks highest, but
/// [`furthest_along_state`] excludes it from candidacy before comparing: a
/// closed Notation is never what a matter is currently waiting on.
pub(crate) fn progress_rank(state: &str) -> u8 {
    let prefix = state.split_once("__").map_or(state, |(p, _)| p);
    match prefix {
        "BEGIN" => 0,
        "intake_persisted" => 1,
        "lawyer_review" => 2,
        "reask" => 3,
        "client_review" => 4,
        "generate_pdf" | "document_drafts" => 5,
        "document_intake" => 6,
        "extract" | "analysis" => 7,
        "sent_for_signature" => 8,
        "notarization" => 10,
        "firm_signature" => 11,
        "mailroom_send" | "certified_mail" => 12,
        "mailroom_receive" => 13,
        "email_send" => 14,
        "e_filing" | "filing" => 15,
        "onchain" => 16,
        "END" => u8::MAX,
        p if p == "witnesses" || p.ends_with("_signature") || p.ends_with("_signatures") => 9,
        // An unrecognized prefix ranks just past intake — further along than
        // "nothing has happened" without asserting an ordering the table
        // cannot back up.
        _ => 1,
    }
}

/// The state of whichever of one matter's Notations has progressed furthest
/// through its workflow, ignoring any Notation parked at the terminal `END`
/// state — a closed Notation is never what the matter is currently waiting
/// on. `None` for an empty set, or when every state given is `END`.
#[must_use]
pub fn furthest_along_state<'a, I>(states: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    states
        .into_iter()
        .filter(|state| *state != "END")
        .max_by_key(|state| progress_rank(state))
}

/// Every notation a Person is the respondent on, newest first.
///
/// # Errors
///
/// [`NotationError::Db`] if the lookup fails.
pub async fn list_by_person(
    db: &SurrealDb,
    person_id: Uuid,
) -> Result<Vec<Notation>, NotationError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE person_id = $person \
             ORDER BY inserted_at DESC"
        ))
        .bind(("person", record_id(PERSON_TABLE, person_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}

/// Every notation in the workspace, newest first — the lawyer "Notations"
/// directory listing and the data-lake export.
///
/// # Errors
///
/// [`NotationError::Db`] if the lookup fails.
pub async fn list_all(db: &SurrealDb) -> Result<Vec<Notation>, NotationError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} ORDER BY inserted_at DESC"
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}

/// The one notation (if any) already open for this exact
/// `(project_id, template_id, person_id)` triple — the idempotent seed's
/// find-or-create probe.
///
/// # Errors
///
/// [`NotationError::Db`] if the lookup fails.
pub async fn find_by_project_template_person(
    db: &SurrealDb,
    project_id: Uuid,
    template_id: Uuid,
    person_id: Uuid,
) -> Result<Option<Notation>, NotationError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} \
             WHERE project_id = $project AND template_id = $template AND person_id = $person \
             LIMIT 1"
        ))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .bind(("template", record_id(TEMPLATE_TABLE, template_id)))
        .bind(("person", record_id(PERSON_TABLE, person_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    one(response)
}

/// Whether any notation exists on this Project — the matter-delete guard.
///
/// # Errors
///
/// [`NotationError::Db`] if the lookup fails.
pub async fn exists_for_project(db: &SurrealDb, project_id: Uuid) -> Result<bool, NotationError> {
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

/// Transition a notation's workflow `state`. Returns the updated row.
///
/// # Errors
///
/// [`NotationError::NotFound`] if the notation does not exist, or
/// [`NotationError::Db`] for anything else.
pub async fn update_state(
    db: &SurrealDb,
    id: Uuid,
    new_state: &str,
) -> Result<Notation, NotationError> {
    let mut response = writing(|| {
        db.query(format!(
            "UPDATE $id SET state = $state, updated_at = time::now() RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind(("state", new_state.to_string()))
    })
    .await?;
    let row: Option<NotationRow> = response.take(0)?;
    let notation = row
        .and_then(NotationRow::into_notation)
        .ok_or(NotationError::NotFound(id))?;
    crate::postgres_mirror::after_write(db, crate::postgres_mirror::MirrorKey::Notation(id)).await;
    Ok(notation)
}

/// Overwrite the frozen questionnaire snapshot. Production never calls
/// this after creation — it exists for the test that proves resolution
/// reads the snapshot, not the template.
///
/// # Errors
///
/// [`NotationError::NotFound`] if the notation does not exist, or
/// [`NotationError::Db`] for anything else.
pub async fn update_questionnaire_snapshot(
    db: &SurrealDb,
    id: Uuid,
    snapshot: Json,
) -> Result<Notation, NotationError> {
    let mut response = writing(|| {
        db.query(format!(
            "UPDATE $id SET questionnaire_snapshot = $snapshot, updated_at = time::now() \
             RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind(("snapshot", snapshot.clone()))
    })
    .await?;
    let row: Option<NotationRow> = response.take(0)?;
    let notation = row
        .and_then(NotationRow::into_notation)
        .ok_or(NotationError::NotFound(id))?;
    crate::postgres_mirror::after_write(db, crate::postgres_mirror::MirrorKey::Notation(id)).await;
    Ok(notation)
}

/// Storage-key convention for the rendered document PDF of a given notation —
/// the version sent out for signature. Per-notation and template-agnostic (the
/// retainer, the trust, and any future signed template share this scheme). It
/// lives here, next to the notation reads, so every reader and writer of the
/// object store — the signing walk, the document-download route, and the
/// matter-detail existence probe — derives the same key from one place.
#[must_use]
pub fn document_pdf_storage_key(notation_id: Uuid) -> String {
    format!("notations/{notation_id}/document.pdf")
}

/// Storage key for the executed (signed) document PDF — the version the
/// provider returns once every party has signed.
#[must_use]
pub fn signed_document_storage_key(notation_id: Uuid) -> String {
    format!("notations/{notation_id}/signed-document.pdf")
}

/// Storage key for the Certificate of Completion — the ESIGN evidentiary record
/// archived alongside the signed retainer.
#[must_use]
pub fn certificate_of_completion_storage_key(notation_id: Uuid) -> String {
    format!("notations/{notation_id}/certificate-of-completion.pdf")
}

#[cfg(test)]
mod tests {
    use super::{
        certificate_of_completion_storage_key, create, document_pdf_storage_key, find_by_id,
        find_by_project_template_person, furthest_along_state, list_all, list_by_person,
        list_by_project, list_by_projects, signed_document_storage_key,
        update_questionnaire_snapshot, update_state, NewNotation, NotationError, DELIVERY_EMBEDDED,
    };
    use crate::surreal::test_support::mem;
    use uuid::Uuid;

    async fn a_template(surreal: &crate::surreal::SurrealDb) -> Uuid {
        crate::templates::save_version(
            surreal,
            None,
            &format!("sitting__transcript-{}", Uuid::now_v7()),
            crate::templates::Version {
                title: "Estate".into(),
                respondent_type: "person".into(),
                asset_id: None,
                form_code: None,
                kind: None,
                source_commit_sha: None,
            },
        )
        .await
        .unwrap()
        .into_model()
        .id
    }

    async fn fs_storage() -> std::sync::Arc<dyn cloud::StorageService> {
        let dir = std::env::temp_dir().join(format!("navigator-notations-{}", Uuid::now_v7()));
        std::sync::Arc::new(cloud::FsStorage::new(dir).await.unwrap())
    }

    /// Sync `body` into the Project's `lease` Draft at `commit`.
    async fn sync_lease(
        surreal: &crate::surreal::SurrealDb,
        storage: &std::sync::Arc<dyn cloud::StorageService>,
        project: Uuid,
        body: &str,
        commit: &str,
    ) -> crate::templates::Template {
        let asset_id =
            crate::assets::ingest_content(surreal, storage, body.as_bytes(), "text/markdown")
                .await
                .unwrap();
        crate::templates::save_version(
            surreal,
            Some(project),
            "lease",
            crate::templates::Version {
                title: "Lease".into(),
                respondent_type: "person".into(),
                asset_id: Some(asset_id),
                form_code: None,
                kind: Some("agreement".into()),
                source_commit_sha: Some(commit.into()),
            },
        )
        .await
        .unwrap()
        .into_model()
    }

    /// Opening copies the Draft's commit and digest; editing `drafts/`
    /// afterwards cuts a new version and leaves the Notation's body
    /// byte-identical.
    #[tokio::test]
    async fn a_notation_keeps_the_draft_it_was_opened_from() {
        let surreal = mem().await;
        let storage = fs_storage().await;
        let project = crate::test_support::seed_project_surreal(&surreal, "matter").await;
        let person = a_person(&surreal, "pin@example.com").await;
        let first_commit = "1111111111111111111111111111111111111111";
        let original = "# Lease\n\nRent is due on the first.\n";
        let v1 = sync_lease(&surreal, &storage, project, original, first_commit).await;

        let opened = create(&surreal, &NewNotation::new(v1.id, person, project, "BEGIN"))
            .await
            .unwrap();
        assert_eq!(
            opened.template_source_commit_sha.as_deref(),
            Some(first_commit)
        );
        assert_eq!(
            opened.template_body_sha256.as_deref(),
            Some(crate::assets::sha256_hex(original.as_bytes()).as_str())
        );
        let before = super::pinned_body(&surreal, &storage, &opened)
            .await
            .unwrap();

        let v2 = sync_lease(
            &surreal,
            &storage,
            project,
            "# Lease\n\nRent is due on the fifth.\n",
            "2222222222222222222222222222222222222222",
        )
        .await;
        assert_ne!(v2.id, v1.id, "the edit cut a new version");

        let reread = find_by_id(&surreal, opened.id).await.unwrap().unwrap();
        assert_eq!(reread, opened, "the Notation row is untouched");
        let after = super::pinned_body(&surreal, &storage, &reread)
            .await
            .unwrap();
        assert_eq!(after.as_bytes(), before.as_bytes());
        assert_eq!(after, original);
    }

    /// Bytes that do not hash to the Notation's pin are never returned.
    #[tokio::test]
    async fn a_tampered_draft_body_fails_the_notation_closed() {
        let surreal = mem().await;
        let storage = fs_storage().await;
        let project = crate::test_support::seed_project_surreal(&surreal, "matter").await;
        let person = a_person(&surreal, "tamper@example.com").await;
        let body = "# Lease\n\nRent is due on the first.\n";
        let template = sync_lease(
            &surreal,
            &storage,
            project,
            body,
            crate::test_support::SYNTHETIC_COMMIT_SHA,
        )
        .await;
        let notation = create(
            &surreal,
            &NewNotation::new(template.id, person, project, "BEGIN"),
        )
        .await
        .unwrap();
        let key = format!("blobs/{}", crate::assets::sha256_hex(body.as_bytes()));
        storage
            .put(&key, b"# Lease\n\nRent is waived.\n", "text/markdown")
            .await
            .unwrap();

        let refused = super::pinned_body(&surreal, &storage, &notation).await;
        assert!(
            matches!(
                refused,
                Err(super::PinnedBodyError::Body(
                    crate::templates::TemplateBodyError::DigestMismatch(_)
                ))
            ),
            "{refused:?}"
        );
    }

    async fn a_person(surreal: &crate::surreal::SurrealDb, email: &str) -> Uuid {
        crate::persons::create(surreal, &crate::persons::NewPerson::new("Libra", email))
            .await
            .unwrap()
            .id
    }

    async fn open_notation(surreal: &crate::surreal::SurrealDb) -> Uuid {
        let project = crate::test_support::seed_project_surreal(surreal, "matter").await;
        let template_id = a_template(surreal).await;
        let person_id = a_person(surreal, &format!("libra-{}@example.com", Uuid::now_v7())).await;
        create(
            surreal,
            &NewNotation::new(template_id, person_id, project, "BEGIN"),
        )
        .await
        .unwrap()
        .id
    }

    #[test]
    fn notation_storage_keys_are_stable_per_notation_paths() {
        let id: Uuid = "00000000-0000-0000-0000-0000000000ab".parse().unwrap();
        assert_eq!(
            document_pdf_storage_key(id),
            "notations/00000000-0000-0000-0000-0000000000ab/document.pdf"
        );
        assert_eq!(
            signed_document_storage_key(id),
            "notations/00000000-0000-0000-0000-0000000000ab/signed-document.pdf"
        );
        assert_eq!(
            certificate_of_completion_storage_key(id),
            "notations/00000000-0000-0000-0000-0000000000ab/certificate-of-completion.pdf"
        );
    }

    #[tokio::test]
    async fn create_writes_a_notation_and_find_by_id_reads_it_back() {
        let surreal = mem().await;
        let project = crate::test_support::seed_project_surreal(&surreal, "matter").await;
        let template_id = a_template(&surreal).await;
        let person_id = a_person(&surreal, "libra@example.com").await;
        let created = create(
            &surreal,
            &NewNotation::new(template_id, person_id, project, "BEGIN"),
        )
        .await
        .unwrap();
        assert_eq!(created.delivery, DELIVERY_EMBEDDED);
        assert_eq!(created.state, "BEGIN");
        assert_eq!(created.package_id, None);

        let found = find_by_id(&surreal, created.id).await.unwrap().unwrap();
        assert_eq!(found, created);
    }

    #[tokio::test]
    async fn a_notation_can_keep_the_package_that_assembled_it() {
        let surreal = mem().await;
        let project = crate::test_support::seed_project_surreal(&surreal, "matter").await;
        let template_id = a_template(&surreal).await;
        let person_id = a_person(&surreal, "libra@example.com").await;
        let package_id = crate::notation_packages::create(
            &surreal,
            &crate::notation_packages::NewNotationPackage::new(
                "estate-package",
                "Estate package",
                "complex",
                10_000,
            ),
        )
        .await
        .unwrap()
        .id;

        let created = create(
            &surreal,
            &NewNotation::new(template_id, person_id, project, "BEGIN").with_package(package_id),
        )
        .await
        .unwrap();
        assert_eq!(created.package_id, Some(package_id));
    }

    #[tokio::test]
    async fn a_missing_notation_reads_back_as_none() {
        let surreal = mem().await;
        assert!(find_by_id(&surreal, Uuid::now_v7())
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn list_by_project_reads_only_that_projects_notations() {
        let surreal = mem().await;
        let project_a = crate::test_support::seed_project_surreal(&surreal, "matter-a").await;
        let project_b = crate::test_support::seed_project_surreal(&surreal, "matter-b").await;
        let template_id = a_template(&surreal).await;
        let person_id = a_person(&surreal, "libra@example.com").await;
        let on_a = create(
            &surreal,
            &NewNotation::new(template_id, person_id, project_a, "BEGIN"),
        )
        .await
        .unwrap()
        .id;
        create(
            &surreal,
            &NewNotation::new(template_id, person_id, project_b, "BEGIN"),
        )
        .await
        .unwrap();

        let rows = list_by_project(&surreal, project_a).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, on_a);
    }

    #[tokio::test]
    async fn list_by_projects_batches_across_several_projects() {
        let surreal = mem().await;
        let project_a = crate::test_support::seed_project_surreal(&surreal, "matter-a").await;
        let project_b = crate::test_support::seed_project_surreal(&surreal, "matter-b").await;
        let project_c = crate::test_support::seed_project_surreal(&surreal, "matter-c").await;
        let template_id = a_template(&surreal).await;
        let person_id = a_person(&surreal, "libra@example.com").await;
        create(
            &surreal,
            &NewNotation::new(template_id, person_id, project_a, "BEGIN"),
        )
        .await
        .unwrap();
        create(
            &surreal,
            &NewNotation::new(template_id, person_id, project_b, "BEGIN"),
        )
        .await
        .unwrap();

        let rows = list_by_projects(&surreal, &[project_a, project_b])
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(list_by_projects(&surreal, &[project_c])
            .await
            .unwrap()
            .is_empty());
        assert!(list_by_projects(&surreal, &[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_by_person_reads_only_that_persons_notations() {
        let surreal = mem().await;
        let project = crate::test_support::seed_project_surreal(&surreal, "matter").await;
        let template_id = a_template(&surreal).await;
        let libra = a_person(&surreal, "libra@example.com").await;
        let aries = a_person(&surreal, "aries@example.com").await;
        create(
            &surreal,
            &NewNotation::new(template_id, libra, project, "BEGIN"),
        )
        .await
        .unwrap();
        create(
            &surreal,
            &NewNotation::new(template_id, aries, project, "BEGIN"),
        )
        .await
        .unwrap();

        let rows = list_by_person(&surreal, libra).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].person_id, libra);
    }

    #[tokio::test]
    async fn list_all_reads_every_notation() {
        let surreal = mem().await;
        open_notation(&surreal).await;
        open_notation(&surreal).await;
        assert_eq!(list_all(&surreal).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn find_by_project_template_person_matches_the_exact_triple() {
        let surreal = mem().await;
        let project = crate::test_support::seed_project_surreal(&surreal, "matter").await;
        let template_id = a_template(&surreal).await;
        let person_id = a_person(&surreal, "libra@example.com").await;
        let created = create(
            &surreal,
            &NewNotation::new(template_id, person_id, project, "BEGIN"),
        )
        .await
        .unwrap();

        let found = find_by_project_template_person(&surreal, project, template_id, person_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.id, created.id);

        let other_project = crate::test_support::seed_project_surreal(&surreal, "other").await;
        assert!(
            find_by_project_template_person(&surreal, other_project, template_id, person_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn exists_for_project_reflects_whether_any_notation_is_open() {
        let surreal = mem().await;
        let project = crate::test_support::seed_project_surreal(&surreal, "matter").await;
        assert!(!super::exists_for_project(&surreal, project).await.unwrap());
        let template_id = a_template(&surreal).await;
        let person_id = a_person(&surreal, "libra@example.com").await;
        create(
            &surreal,
            &NewNotation::new(template_id, person_id, project, "BEGIN"),
        )
        .await
        .unwrap();
        assert!(super::exists_for_project(&surreal, project).await.unwrap());
    }

    #[tokio::test]
    async fn update_state_transitions_the_row() {
        let surreal = mem().await;
        let id = open_notation(&surreal).await;
        let updated = update_state(&surreal, id, "lawyer_review").await.unwrap();
        assert_eq!(updated.state, "lawyer_review");
        assert_eq!(
            find_by_id(&surreal, id).await.unwrap().unwrap().state,
            "lawyer_review"
        );
    }

    #[tokio::test]
    async fn update_state_on_a_missing_notation_reports_not_found() {
        let surreal = mem().await;
        let err = update_state(&surreal, Uuid::now_v7(), "lawyer_review")
            .await
            .unwrap_err();
        assert!(matches!(err, NotationError::NotFound(_)));
    }

    #[tokio::test]
    async fn update_questionnaire_snapshot_overwrites_the_frozen_copy() {
        let surreal = mem().await;
        let id = open_notation(&surreal).await;
        let alt = serde_json::json!({"states": {"replaced": true}});
        let updated = update_questionnaire_snapshot(&surreal, id, alt.clone())
            .await
            .unwrap();
        assert_eq!(updated.questionnaire_snapshot, Some(alt));
    }

    #[test]
    fn furthest_along_state_is_none_for_no_notations() {
        assert_eq!(furthest_along_state(Vec::<&str>::new()), None);
    }

    #[test]
    fn furthest_along_state_is_the_one_notation_given() {
        assert_eq!(
            furthest_along_state(["lawyer_review"]),
            Some("lawyer_review")
        );
    }

    #[test]
    fn furthest_along_state_picks_the_deeper_of_two_notations() {
        // `lawyer_review` (review) sits well behind `firm_signature`
        // (signing) in the matter lifecycle.
        assert_eq!(
            furthest_along_state(["lawyer_review", "firm_signature__closing_letter"]),
            Some("firm_signature__closing_letter")
        );
        // Order in the input must not matter.
        assert_eq!(
            furthest_along_state(["firm_signature__closing_letter", "lawyer_review"]),
            Some("firm_signature__closing_letter")
        );
    }

    #[test]
    fn furthest_along_state_ignores_a_closed_notation_beside_a_live_one() {
        assert_eq!(
            furthest_along_state(["END", "lawyer_review"]),
            Some("lawyer_review")
        );
    }

    #[test]
    fn furthest_along_state_is_none_when_every_notation_is_closed() {
        assert_eq!(furthest_along_state(["END", "END"]), None);
    }
}
