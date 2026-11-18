//! PostgreSQL authority for the Notation record.
//!
//! Whenever `NAVIGATOR_POSTGRES_HOST` is set ([`PostgresDb::configured`]),
//! `notations` reads and writes PostgreSQL only, a PostgreSQL failure is
//! returned rather than falling back, and the mirror stops copying
//! SurrealDB rows over the authoritative ones. Without a PostgreSQL host
//! the SurrealDB path is the fallback. The parent rows the table names —
//! the Template, Notation Package, Person, Entity, and Project — stay on
//! their own authorities.
//!
//! Rows come back as the JSON object `to_jsonb` makes of them, so every
//! statement selects `to_jsonb(<alias>) AS row` and the owning module's
//! own serde type decodes it.

use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::Row;

use crate::notations::Notation;
use crate::postgres::PostgresDb;

/// The table PostgreSQL holds authoritatively under this module. The mirror
/// refuses to repair it from SurrealDB outside the first Mirror Backfill.
pub(crate) const TABLES: &[&str] = &["notation"];

#[cfg(test)]
tokio::task_local! {
    static TEST_NOTATION_DB: PostgresDb;
}

/// Why PostgreSQL could not serve a notation call. Carries no row values.
#[derive(Debug, thiserror::Error)]
pub enum NotationAuthorityError {
    #[error("PostgreSQL is unavailable for the notation table")]
    Unavailable,
    /// The `sqlx::Error` is boxed because this error nests inside the
    /// domain enums (`NotationError::Authority`, then callers of that),
    /// and an unboxed payload pushes some call sites past clippy's
    /// `result_large_err` threshold.
    #[error("PostgreSQL refused the notation operation: {0}")]
    Query(Box<sqlx::Error>),
    #[error("PostgreSQL returned an unreadable notation row")]
    Row(#[from] serde_json::Error),
}

impl From<sqlx::Error> for NotationAuthorityError {
    fn from(err: sqlx::Error) -> Self {
        Self::Query(Box::new(err))
    }
}

/// The pool to serve from, or `None` when no PostgreSQL host is configured.
pub(crate) async fn authority() -> Result<Option<PostgresDb>, NotationAuthorityError> {
    #[cfg(test)]
    if let Ok(pg) = TEST_NOTATION_DB.try_with(Clone::clone) {
        return Ok(Some(pg));
    }
    PostgresDb::configured()
        .await
        .map_err(|_| NotationAuthorityError::Unavailable)
}

/// The row `query` selects as `row`.
fn decode<T: DeserializeOwned>(row: &sqlx::postgres::PgRow) -> Result<T, NotationAuthorityError> {
    let value: Value = row.try_get("row")?;
    Ok(serde_json::from_value(value)?)
}

/// The pinned template version fields [`create`] copies onto the row.
#[derive(Debug, Clone)]
pub(crate) struct TemplatePins {
    pub source_commit_sha: Option<String>,
    pub body_sha256: Option<String>,
}

/// The pins a Notation copies at [`create`] — from the resolved template
/// when it reads back, or none when it does not, the same fallback the
/// SurrealQL `$template_id.field` evaluation gives the SurrealDB path.
pub(crate) fn template_pins(template: Option<&crate::templates::Template>) -> TemplatePins {
    TemplatePins {
        source_commit_sha: template.and_then(|t| t.source_commit_sha.clone()),
        body_sha256: template.and_then(|t| t.body_sha256.clone()),
    }
}

/// Run `future` with `pg` as the notation authority's test pool.
#[cfg(test)]
pub(crate) async fn with_test_db<T>(
    pg: PostgresDb,
    future: impl std::future::Future<Output = T>,
) -> T {
    TEST_NOTATION_DB.scope(pg, future).await
}

/// Open a Notation under a fresh id. No unique index guards this insert —
/// the caller owns the id, and the module header explains why no retry
/// wraps it.
pub(crate) async fn create(
    pg: &PostgresDb,
    id: uuid::Uuid,
    new: &crate::notations::NewNotation,
    pins: TemplatePins,
) -> Result<Notation, NotationAuthorityError> {
    let row = sqlx::query(
        "INSERT INTO notation (id, template_id, package_id, person_id, entity_id, \
         project_id, state, delivery, questionnaire_snapshot, \
         template_source_commit_sha, template_body_sha256) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
         RETURNING to_jsonb(notation) AS row",
    )
    .bind(id)
    .bind(new.template_id)
    .bind(new.package_id)
    .bind(new.person_id)
    .bind(new.entity_id)
    .bind(new.project_id)
    .bind(&new.state)
    .bind(&new.delivery)
    .bind(&new.questionnaire_snapshot)
    .bind(pins.source_commit_sha)
    .bind(pins.body_sha256)
    .fetch_one(pg.pool())
    .await?;
    decode(&row)
}

/// Write one archived Notation back under its original id with its frozen
/// questionnaire, template pins, and timestamps. The caller has already
/// established the id is free.
pub(crate) async fn restore_archived(
    pg: &PostgresDb,
    notation: &Notation,
) -> Result<Notation, NotationAuthorityError> {
    let row = sqlx::query(
        "INSERT INTO notation (id, template_id, package_id, person_id, entity_id, \
         project_id, state, delivery, questionnaire_snapshot, git_commit_sha, \
         template_source_commit_sha, template_body_sha256, inserted_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
         RETURNING to_jsonb(notation) AS row",
    )
    .bind(notation.id)
    .bind(notation.template_id)
    .bind(notation.package_id)
    .bind(notation.person_id)
    .bind(notation.entity_id)
    .bind(notation.project_id)
    .bind(&notation.state)
    .bind(&notation.delivery)
    .bind(&notation.questionnaire_snapshot)
    .bind(&notation.git_commit_sha)
    .bind(&notation.template_source_commit_sha)
    .bind(&notation.template_body_sha256)
    .bind(notation.inserted_at)
    .bind(notation.updated_at)
    .fetch_one(pg.pool())
    .await?;
    decode(&row)
}

/// Undo a Notation this same request just created and is aborting before
/// it was ever shown to anyone.
pub(crate) async fn delete_uncommitted(
    pg: &PostgresDb,
    id: uuid::Uuid,
) -> Result<(), NotationAuthorityError> {
    sqlx::query("DELETE FROM notation WHERE id = $1")
        .bind(id)
        .execute(pg.pool())
        .await?;
    Ok(())
}

/// Resolve a notation by id.
pub(crate) async fn find_by_id(
    pg: &PostgresDb,
    id: uuid::Uuid,
) -> Result<Option<Notation>, NotationAuthorityError> {
    let row = sqlx::query("SELECT to_jsonb(n) AS row FROM notation n WHERE id = $1")
        .bind(id)
        .fetch_optional(pg.pool())
        .await?;
    row.as_ref().map(decode).transpose()
}

/// Every notation on a Project, newest first.
pub(crate) async fn list_by_project(
    pg: &PostgresDb,
    project_id: uuid::Uuid,
) -> Result<Vec<Notation>, NotationAuthorityError> {
    let rows = sqlx::query(
        "SELECT to_jsonb(n) AS row FROM notation n WHERE project_id = $1 \
         ORDER BY inserted_at DESC",
    )
    .bind(project_id)
    .fetch_all(pg.pool())
    .await?;
    rows.iter().map(decode).collect()
}

/// Every notation across several Projects, newest first — the batched
/// resolver behind "which of these matters have an engagement/closing
/// notation" checks.
pub(crate) async fn list_by_projects(
    pg: &PostgresDb,
    project_ids: &[uuid::Uuid],
) -> Result<Vec<Notation>, NotationAuthorityError> {
    let rows = sqlx::query(
        "SELECT to_jsonb(n) AS row FROM notation n WHERE project_id = ANY($1) \
         ORDER BY inserted_at DESC",
    )
    .bind(project_ids)
    .fetch_all(pg.pool())
    .await?;
    rows.iter().map(decode).collect()
}

/// Every notation a Person is the respondent on, newest first.
pub(crate) async fn list_by_person(
    pg: &PostgresDb,
    person_id: uuid::Uuid,
) -> Result<Vec<Notation>, NotationAuthorityError> {
    let rows = sqlx::query(
        "SELECT to_jsonb(n) AS row FROM notation n WHERE person_id = $1 \
         ORDER BY inserted_at DESC",
    )
    .bind(person_id)
    .fetch_all(pg.pool())
    .await?;
    rows.iter().map(decode).collect()
}

/// Every notation in the workspace, newest first — the lawyer "Notations"
/// directory listing and the data-lake export.
pub(crate) async fn list_all(pg: &PostgresDb) -> Result<Vec<Notation>, NotationAuthorityError> {
    let rows = sqlx::query("SELECT to_jsonb(n) AS row FROM notation n ORDER BY inserted_at DESC")
        .fetch_all(pg.pool())
        .await?;
    rows.iter().map(decode).collect()
}

/// The one notation (if any) already open for this exact
/// `(project_id, template_id, person_id)` triple — the idempotent seed's
/// find-or-create probe.
pub(crate) async fn find_by_project_template_person(
    pg: &PostgresDb,
    project_id: uuid::Uuid,
    template_id: uuid::Uuid,
    person_id: uuid::Uuid,
) -> Result<Option<Notation>, NotationAuthorityError> {
    let row = sqlx::query(
        "SELECT to_jsonb(n) AS row FROM notation n \
         WHERE project_id = $1 AND template_id = $2 AND person_id = $3 \
         ORDER BY inserted_at DESC LIMIT 1",
    )
    .bind(project_id)
    .bind(template_id)
    .bind(person_id)
    .fetch_optional(pg.pool())
    .await?;
    row.as_ref().map(decode).transpose()
}

/// Whether any notation exists on this Project — the matter-delete guard.
pub(crate) async fn exists_for_project(
    pg: &PostgresDb,
    project_id: uuid::Uuid,
) -> Result<bool, NotationAuthorityError> {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM notation WHERE project_id = $1)")
            .bind(project_id)
            .fetch_one(pg.pool())
            .await?;
    Ok(exists)
}

/// Transition a notation's workflow `state`. `None` when the notation does
/// not exist; the caller turns that into its `NotFound`.
pub(crate) async fn update_state(
    pg: &PostgresDb,
    id: uuid::Uuid,
    new_state: &str,
) -> Result<Option<Notation>, NotationAuthorityError> {
    let row = sqlx::query(
        "UPDATE notation SET state = $2, updated_at = now() WHERE id = $1 \
         RETURNING to_jsonb(notation) AS row",
    )
    .bind(id)
    .bind(new_state)
    .fetch_optional(pg.pool())
    .await?;
    row.as_ref().map(decode).transpose()
}

/// Reconcile a Notation projection if it still has the observed state.
/// `None` when the notation is gone or no longer holds the observed state.
pub(crate) async fn reconcile_state(
    pg: &PostgresDb,
    id: uuid::Uuid,
    observed_state: &str,
    current_state: &str,
) -> Result<Option<Notation>, NotationAuthorityError> {
    let row = sqlx::query(
        "UPDATE notation SET state = $3, updated_at = now() \
         WHERE id = $1 AND state = $2 \
         RETURNING to_jsonb(notation) AS row",
    )
    .bind(id)
    .bind(observed_state)
    .bind(current_state)
    .fetch_optional(pg.pool())
    .await?;
    row.as_ref().map(decode).transpose()
}

/// Overwrite the frozen questionnaire snapshot. Production never calls
/// this after creation — it exists for the test that proves resolution
/// reads the snapshot, not the template.
pub(crate) async fn update_questionnaire_snapshot(
    pg: &PostgresDb,
    id: uuid::Uuid,
    snapshot: &serde_json::Value,
) -> Result<Option<Notation>, NotationAuthorityError> {
    let row = sqlx::query(
        "UPDATE notation SET questionnaire_snapshot = $2, updated_at = now() \
         WHERE id = $1 \
         RETURNING to_jsonb(notation) AS row",
    )
    .bind(id)
    .bind(snapshot)
    .fetch_optional(pg.pool())
    .await?;
    row.as_ref().map(decode).transpose()
}

#[cfg(test)]
#[path = "notation_tests.rs"]
mod tests;
