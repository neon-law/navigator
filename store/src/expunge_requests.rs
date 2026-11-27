//! Helpers for the `expunge_requests` table — a client's request to
//! delete one of their matter documents, awaiting attorney
//! authorization.
//!
//! A client can only *ask*: [`create`] inserts a `pending` row. A
//! lawyer/admin then resolves it — [`authorize`] (after running the
//! admin-gated expunge, passing the resulting audit-row id) or
//! [`deny`]. The executed expunge is always category `client_request`.
//! See [`crate::expunge_records`] and the design §9.
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

/// What can go wrong reading or writing a deletion request.
///
/// An error in the request’s PostgreSQL operation.
#[derive(Debug, Error)]
pub enum ExpungeRequestError {
    #[error(transparent)]
    Postgres(#[from] matter::MatterAuthorityError),
}

/// The table these rows live in.
const TABLE: &str = "expunge_request";

/// Awaiting lawyer/admin review — the default for a new request.
pub const STATUS_PENDING: &str = "pending";
/// Authorized — the admin-gated expunge has run; `expunge_record_id` is
/// set.
pub const STATUS_AUTHORIZED: &str = "authorized";
/// Denied by a lawyer/admin; nothing was deleted.
pub const STATUS_DENIED: &str = "denied";

/// One client deletion request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpungeRequest {
    pub id: Uuid,
    /// The matter the document belongs to.
    pub project_id: Uuid,
    /// The document the client wants deleted.
    pub asset_id: Uuid,
    /// The client who requested deletion.
    pub requested_by_person_id: Uuid,
    /// One of the `STATUS_*` constants.
    pub status: String,
    /// Optional non-content note from the client (their stated reason).
    pub note: Option<String>,
    /// The lawyer/admin who resolved it. `None` while pending.
    pub resolved_by_person_id: Option<Uuid>,
    /// The audit row from the executed expunge. `None` unless authorized.
    pub expunge_record_id: Option<Uuid>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// What to record for one client deletion request.
#[derive(Debug, Clone)]
pub struct NewExpungeRequest<'a> {
    pub project_id: Uuid,
    pub asset_id: Uuid,
    /// The client asking for deletion.
    pub requested_by_person_id: Uuid,
    /// Optional non-content note (the client's stated reason).
    pub note: Option<&'a str>,
}

/// Insert one `expunge_requests` row at `status = pending`, returning its
/// id. The request never deletes anything on its own — a lawyer/admin must
/// authorize it.
///
/// # Errors
/// Propagates any database error.
pub async fn create(
    _db: &ApplicationStore,
    new: &NewExpungeRequest<'_>,
) -> Result<Uuid, ExpungeRequestError> {
    let pg = matter::authority_for(TABLE).await?;
    let now = Utc::now();
    let row = ExpungeRequest {
        id: Uuid::now_v7(),
        project_id: new.project_id,
        asset_id: new.asset_id,
        requested_by_person_id: new.requested_by_person_id,
        status: STATUS_PENDING.to_owned(),
        note: new.note.map(str::to_owned),
        resolved_by_person_id: None,
        expunge_record_id: None,
        inserted_at: now,
        updated_at: now,
    };
    let stored: ExpungeRequest = matter::upsert(&pg, TABLE, &row).await?;
    Ok(stored.id)
}

/// Load one request by id.
///
/// # Errors
/// Propagates any database error.
pub async fn by_id(
    _db: &ApplicationStore,
    id: Uuid,
) -> Result<Option<ExpungeRequest>, ExpungeRequestError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(matter::find(&pg, TABLE, json!({"id": id})).await?)
}

/// The pending request for a document, if any. Used to show the client
/// "deletion requested" instead of offering the control again.
///
/// # Errors
/// Propagates any database error.
pub async fn pending_for_document(
    _db: &ApplicationStore,
    asset_id: Uuid,
) -> Result<Option<ExpungeRequest>, ExpungeRequestError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(matter::find(
        &pg,
        TABLE,
        json!({"asset_id": asset_id, "status": STATUS_PENDING}),
    )
    .await?)
}

/// Every pending request across all matters, oldest first — the lawyer
/// authorization queue.
///
/// # Errors
/// Propagates any database error.
pub async fn list_pending(
    _db: &ApplicationStore,
) -> Result<Vec<ExpungeRequest>, ExpungeRequestError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(matter::select(&pg, TABLE, json!({"status": STATUS_PENDING}), &["id"]).await?)
}

/// Whether any request — of any `status`, not only pending — is scoped to
/// this Project. The matter-delete guard.
///
/// A resolved request is as load-bearing as an open one: it records that
/// somebody asked for material to be expunged and what the firm decided,
/// so it must outlive nothing quietly.
///
/// # Errors
/// Propagates any database error.
pub async fn exists_for_project(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<bool, ExpungeRequestError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(
        matter::find::<ExpungeRequest>(&pg, TABLE, json!({"project_id": project_id}))
            .await?
            .is_some(),
    )
}

/// Mark a request `authorized`, recording who resolved it and the audit
/// row id from the executed expunge. Returns the updated row, or
/// `Ok(None)` if no row matched.
///
/// # Errors
/// Propagates any database error.
pub async fn authorize(
    db: &ApplicationStore,
    id: Uuid,
    resolved_by_person_id: Uuid,
    expunge_record_id: Uuid,
) -> Result<Option<ExpungeRequest>, ExpungeRequestError> {
    resolve(
        db,
        id,
        STATUS_AUTHORIZED,
        resolved_by_person_id,
        Some(expunge_record_id),
    )
    .await
}

/// Mark a request `denied`, recording who resolved it. Nothing is
/// deleted. Returns the updated row, or `Ok(None)` if no row matched.
///
/// # Errors
/// Propagates any database error.
pub async fn deny(
    db: &ApplicationStore,
    id: Uuid,
    resolved_by_person_id: Uuid,
) -> Result<Option<ExpungeRequest>, ExpungeRequestError> {
    resolve(db, id, STATUS_DENIED, resolved_by_person_id, None).await
}

/// The shared body of [`authorize`] and [`deny`] — the two differ only in
/// the status they land on and whether an audit row is linked.
///
/// `UPDATE $id ... WHERE` (rather than a read-then-write) is what keeps a
/// concurrent second resolution from clobbering the first: the statement
/// matches nothing when the row is already resolved, so the loser gets
/// `Ok(None)` instead of overwriting the winner's `resolved_by`.
async fn resolve(
    _db: &ApplicationStore,
    id: Uuid,
    status: &str,
    resolved_by_person_id: Uuid,
    expunge_record_id: Option<Uuid>,
) -> Result<Option<ExpungeRequest>, ExpungeRequestError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(matter::update(
        &pg,
        TABLE,
        json!({"id": id, "status": STATUS_PENDING}),
        json!({"status": status, "resolved_by_person_id": resolved_by_person_id,
            "expunge_record_id": expunge_record_id, "updated_at": Utc::now()}),
    )
    .await?
    .into_iter()
    .next())
}

#[cfg(test)]
mod tests {}
