//! Record governed expunges (design §9).
//!
//! The legal council's requirement: when a matter repo's history is
//! rewritten to remove privileged / sealed / lawfully-deleted material,
//! the expunge *itself* is recorded — who authorized it, when, and the
//! category — so the redaction is auditable. This module is the
//! write/read seam for that audit log; the `portal::expunge` orchestrator
//! calls [`record`] after the repo rewrite + LFS deletion.
//!
//! Commands use PostgreSQL whenever its host is configured; see
//! [storage models](../../docs/storage-models.md#matter-records).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::matter;
use serde_json::json;

/// What can go wrong reading or writing the audit trail.
///
/// A typed error rather than a bare [`surrealdb::Error`] because `portal`
/// and `webapp` consume this module and do not depend on the SurrealDB
/// crate — the same shape [`crate::persons::PersonError`] uses.
#[derive(Debug, Error)]
pub enum ExpungeRecordError {
    #[error(transparent)]
    Postgres(#[from] matter::MatterAuthorityError),
    /// A database operation failed.
    /// The write was refused because `category` is outside the
    /// `CATEGORY_*` set — the schema ASSERT rejecting an unclassifiable
    /// audit row.
    #[error("unknown expunge category `{0}` (expected privilege | sealing | client_request | retention)")]
    BadCategory(String),
}

/// The table these rows live in.
pub(crate) const TABLE: &str = "expunge_record";

/// Privilege clawback — material committed in error that is privileged.
pub const CATEGORY_PRIVILEGE: &str = "privilege";
/// A court sealing order.
pub const CATEGORY_SEALING: &str = "sealing";
/// A client's lawful deletion request.
pub const CATEGORY_CLIENT_REQUEST: &str = "client_request";
/// A soft-deleted document purged after its 180-day retention window.
pub const CATEGORY_RETENTION: &str = "retention";

/// One governed-expunge audit row.
///
/// The application-facing shape: plain Rust types, no engine handles.
/// [`ExpungeRecordRow`] is the seam that turns it into (and back out of)
/// what the SDK reads and writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpungeRecord {
    pub id: Uuid,
    /// The matter whose repo was rewritten.
    pub project_id: Uuid,
    /// Repo path removed from all history (metadata, not content).
    pub path: String,
    /// One of the `CATEGORY_*` constants.
    pub category: String,
    /// The [`crate::persons`] row that authorized the expunge.
    pub authorized_by_person_id: Uuid,
    /// `main` oid before the rewrite (`None` if the repo was empty).
    pub head_before: Option<String>,
    /// `main` oid after the rewrite.
    pub head_after: Option<String>,
    /// Optional non-content note (e.g. a docket reference).
    pub note: Option<String>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Inputs to [`record`].
#[derive(Debug, Clone)]
pub struct NewExpunge<'a> {
    /// The matter whose repo was rewritten.
    pub project_id: Uuid,
    /// The repo path removed (metadata, not content).
    pub path: &'a str,
    /// One of the `CATEGORY_*` constants.
    pub category: &'a str,
    /// The admin who authorized the expunge.
    pub authorized_by_person_id: Uuid,
    /// `main` oid before the rewrite.
    pub head_before: Option<&'a str>,
    /// `main` oid after the rewrite.
    pub head_after: Option<&'a str>,
    /// Optional non-content note (e.g. a docket reference).
    pub note: Option<&'a str>,
}

/// Insert one expunge audit row, returning its id. No unique index guards
/// this insert, so no retry wraps it — each call mints its own fresh id,
/// and a second expunge of the same path is a second auditable event.
///
/// # Errors
///
/// [`ExpungeRecordError::BadCategory`] when the caller passes a category
/// outside the `CATEGORY_*` set, or any other database error.
pub async fn record(
    _db: &ApplicationStore,
    new: &NewExpunge<'_>,
) -> Result<Uuid, ExpungeRecordError> {
    let pg = matter::authority_for(TABLE).await?;
    if ![
        CATEGORY_PRIVILEGE,
        CATEGORY_SEALING,
        CATEGORY_CLIENT_REQUEST,
        CATEGORY_RETENTION,
    ]
    .contains(&new.category)
    {
        return Err(ExpungeRecordError::BadCategory(new.category.to_owned()));
    }
    let now = Utc::now();
    let row = ExpungeRecord {
        id: Uuid::now_v7(),
        project_id: new.project_id,
        path: new.path.to_owned(),
        category: new.category.to_owned(),
        authorized_by_person_id: new.authorized_by_person_id,
        head_before: new.head_before.map(str::to_owned),
        head_after: new.head_after.map(str::to_owned),
        note: new.note.map(str::to_owned),
        inserted_at: now,
        updated_at: now,
    };
    let stored: ExpungeRecord = matter::upsert(&pg, TABLE, &row).await?;
    Ok(stored.id)
}

/// Load one audit row by id — the completed-expunge screen reads it back
/// from the id its redirect carries.
///
/// # Errors
///
/// Propagates any database error.
pub async fn by_id(
    _db: &ApplicationStore,
    id: Uuid,
) -> Result<Option<ExpungeRecord>, ExpungeRecordError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(matter::find(&pg, TABLE, json!({"id": id})).await?)
}

/// Every expunge recorded against a matter, oldest first — the matter's
/// redaction history. Rides `expunge_record_project`.
///
/// # Errors
///
/// Propagates any database error.
pub async fn for_project(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<ExpungeRecord>, ExpungeRecordError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(matter::select(&pg, TABLE, json!({"project_id": project_id}), &["id"]).await?)
}

/// Whether any expunge audit row is scoped to this Project — the
/// matter-delete guard.
///
/// # Errors
/// Propagates any database error.
pub async fn exists_for_project(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<bool, ExpungeRecordError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(
        matter::find::<ExpungeRecord>(&pg, TABLE, json!({"project_id": project_id}))
            .await?
            .is_some(),
    )
}

#[cfg(test)]
mod tests {}
