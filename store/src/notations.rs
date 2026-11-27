//! A Template filled in for a Person inside a Project.
//!
//! A completed scoped Mirror Backfill enrolls the Notation family before its
//! configured PostgreSQL reads and writes are admitted. Answers, events, clauses,
//! and ordered sources share that authority.
//!
//! Creation copies the Template pins and questionnaire snapshot. State
//! reconciliation changes only the state the caller observed. Uncommitted-create
//! compensation and the daily draft reset are the deletion seams; the reset
//! rechecks document and signature protection under the PostgreSQL parent lock.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::matter;

/// The table these rows live in.
pub(crate) const TABLE: &str = "notation";

/// Captive client recipient: reaches the signing link through Navigator.
pub const DELIVERY_EMBEDDED: &str = "embedded";

/// Non-captive client recipient: DocuSign emails a signing link the client
/// opens from their own inbox.
pub const DELIVERY_EMAILED: &str = "emailed";

/// How the client receives the signing link when a notation is sent.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SigningDelivery {
    #[default]
    Embedded,
    Emailed,
}

impl SigningDelivery {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Embedded => DELIVERY_EMBEDDED,
            Self::Emailed => DELIVERY_EMAILED,
        }
    }
}

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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::postgres::matter::present_json"
    )]
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
    #[error("notation {0} is protected from deletion")]
    ProtectedFromDeletion(Uuid),
    #[error("PostgreSQL notation operation failed")]
    Postgres(#[from] sqlx::Error),
    #[error("PostgreSQL notation configuration is unavailable")]
    Configuration(#[from] crate::postgres::PostgresError),
    #[error(transparent)]
    Authority(#[from] matter::MatterAuthorityError),
    #[error("notation {0} not found")]
    NotFound(Uuid),
    /// A write reported success but returned no row, or returned one this
    /// module could not read back.
    #[error("writing a notation returned no usable row")]
    WriteReturnedNothing,
}

pub(crate) fn postgres_notation(row: &sqlx::postgres::PgRow) -> Result<Notation, NotationError> {
    use sqlx::Row;
    serde_json::from_value(row.try_get("row")?).map_err(|_| NotationError::WriteReturnedNothing)
}

/// Write one archived Notation back under its original id with its frozen
/// questionnaire, template pins, and timestamps. An identical row is left
/// alone; a different row with the same id is a conflict.
///
/// # Errors
/// [`NotationError`] on a database failure.
pub async fn restore_archived(
    _db: &ApplicationStore,
    notation: &Notation,
) -> Result<crate::assets::RowRestore, NotationError> {
    let pg = matter::authority_for(TABLE).await?;
    crate::postgres::notations::restore_archived(&pg, notation).await
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
pub async fn create(_db: &ApplicationStore, new: &NewNotation) -> Result<Notation, NotationError> {
    let pg = matter::authority_for(TABLE).await?;
    crate::postgres::notations::create(&pg, new).await
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
    db: &ApplicationStore,
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
pub async fn delete_uncommitted(_db: &ApplicationStore, id: Uuid) -> Result<(), NotationError> {
    let pg = matter::authority_for(TABLE).await?;
    match crate::postgres::notations::delete_draft(&pg, id).await? {
        crate::notation_reset::ResetOutcome::Deleted
        | crate::notation_reset::ResetOutcome::AlreadyGone => Ok(()),
        crate::notation_reset::ResetOutcome::Kept => Err(NotationError::ProtectedFromDeletion(id)),
    }
}

/// Resolve a notation by id.
///
/// # Errors
///
/// [`NotationError::Db`] if the lookup fails.
pub async fn find_by_id(
    _db: &ApplicationStore,
    id: Uuid,
) -> Result<Option<Notation>, NotationError> {
    let pg = matter::authority_for(TABLE).await?;
    sqlx::query("SELECT to_jsonb(n) - CASE WHEN n.questionnaire_snapshot IS NULL THEN ARRAY['questionnaire_snapshot'] ELSE ARRAY[]::text[] END AS row FROM notation n WHERE id=$1")
        .bind(id)
        .fetch_optional(pg.pool())
        .await?
        .as_ref()
        .map(postgres_notation)
        .transpose()
}

/// Every notation on a Project, newest first.
///
/// # Errors
///
/// [`NotationError::Db`] if the lookup fails.
pub async fn list_by_project(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<Notation>, NotationError> {
    let pg = matter::authority_for(TABLE).await?;
    sqlx::query(
        "SELECT to_jsonb(n) - CASE WHEN n.questionnaire_snapshot IS NULL THEN ARRAY['questionnaire_snapshot'] ELSE ARRAY[]::text[] END AS row FROM notation n WHERE project_id=$1 ORDER BY inserted_at,id",
    )
    .bind(project_id)
    .fetch_all(pg.pool())
    .await?
    .iter()
    .map(postgres_notation)
    .collect()
}

/// Every notation across several Projects, newest first — the batched
/// resolver behind "which of these matters have an engagement/closing
/// notation" checks.
///
/// # Errors
///
/// [`NotationError::Db`] if the lookup fails.
pub async fn list_by_projects(
    _db: &ApplicationStore,
    project_ids: &[Uuid],
) -> Result<Vec<Notation>, NotationError> {
    let pg = matter::authority_for(TABLE).await?;
    sqlx::query("SELECT to_jsonb(n) - CASE WHEN n.questionnaire_snapshot IS NULL THEN ARRAY['questionnaire_snapshot'] ELSE ARRAY[]::text[] END AS row FROM notation n WHERE project_id=ANY($1) ORDER BY inserted_at DESC,id DESC")
        .bind(project_ids).fetch_all(pg.pool()).await?.iter().map(postgres_notation).collect()
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
    _db: &ApplicationStore,
    person_id: Uuid,
) -> Result<Vec<Notation>, NotationError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(matter::select(
        &pg,
        TABLE,
        serde_json::json!({"person_id": person_id}),
        &["inserted_at DESC", "id DESC"],
    )
    .await?)
}

/// Every notation in the workspace, newest first — the lawyer "Notations"
/// directory listing and the data-lake export.
///
/// # Errors
///
/// [`NotationError::Db`] if the lookup fails.
pub async fn list_all(_db: &ApplicationStore) -> Result<Vec<Notation>, NotationError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(matter::select(
        &pg,
        TABLE,
        serde_json::json!({}),
        &["inserted_at DESC", "id DESC"],
    )
    .await?)
}

/// The one notation (if any) already open for this exact
/// `(project_id, template_id, person_id)` triple — the idempotent seed's
/// find-or-create probe.
///
/// # Errors
///
/// [`NotationError::Db`] if the lookup fails.
pub async fn find_by_project_template_person(
    _db: &ApplicationStore,
    project_id: Uuid,
    template_id: Uuid,
    person_id: Uuid,
) -> Result<Option<Notation>, NotationError> {
    let pg = matter::authority_for(TABLE).await?;
    sqlx::query("SELECT to_jsonb(n) - CASE WHEN n.questionnaire_snapshot IS NULL THEN ARRAY['questionnaire_snapshot'] ELSE ARRAY[]::text[] END AS row FROM notation n WHERE project_id=$1 AND template_id=$2 AND person_id=$3 ORDER BY inserted_at DESC LIMIT 1").bind(project_id).bind(template_id).bind(person_id).fetch_optional(pg.pool()).await?.as_ref().map(postgres_notation).transpose()
}

/// Whether any notation exists on this Project — the matter-delete guard.
///
/// # Errors
///
/// [`NotationError::Db`] if the lookup fails.
pub async fn exists_for_project(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<bool, NotationError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM notation WHERE project_id=$1)")
            .bind(project_id)
            .fetch_one(pg.pool())
            .await?,
    )
}

/// Transition a notation's workflow `state`. Returns the updated row.
///
/// # Errors
///
/// [`NotationError::NotFound`] if the notation does not exist, or
/// [`NotationError::Db`] for anything else.
pub async fn update_state(
    _db: &ApplicationStore,
    id: Uuid,
    new_state: &str,
) -> Result<Notation, NotationError> {
    let pg = matter::authority_for(TABLE).await?;
    matter::update(
        &pg,
        TABLE,
        serde_json::json!({"id": id}),
        serde_json::json!({"state": new_state, "updated_at": Utc::now()}),
    )
    .await?
    .into_iter()
    .next()
    .ok_or(NotationError::NotFound(id))
}

/// Reconcile a Notation projection if it still has the observed state.
pub async fn reconcile_state(
    _db: &ApplicationStore,
    id: Uuid,
    observed_state: &str,
    current_state: &str,
) -> Result<(), NotationError> {
    let pg = matter::authority_for(TABLE).await?;
    let _: Vec<Notation> = matter::update(
        &pg,
        TABLE,
        serde_json::json!({"id": id, "state": observed_state}),
        serde_json::json!({"state": current_state, "updated_at": Utc::now()}),
    )
    .await?;
    Ok(())
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
    _db: &ApplicationStore,
    id: Uuid,
    snapshot: Json,
) -> Result<Notation, NotationError> {
    let pg = matter::authority_for(TABLE).await?;
    matter::update(
        &pg,
        TABLE,
        serde_json::json!({"id": id}),
        serde_json::json!({"questionnaire_snapshot": snapshot, "updated_at": Utc::now()}),
    )
    .await?
    .into_iter()
    .next()
    .ok_or(NotationError::NotFound(id))
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
        certificate_of_completion_storage_key, document_pdf_storage_key, furthest_along_state,
        signed_document_storage_key,
    };
    use uuid::Uuid;

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
