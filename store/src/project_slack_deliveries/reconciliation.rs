//! Privileged reconciliation of unresolved client-view evidence. No provider calls.
//!
//! The connection is the operator's infrastructure authority. The Person is verified
//! for current Firm-management authority and recorded as attributed operator identity;
//! passing a UUID does not authenticate that person or prove the private evidence.

use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::postgres::PostgresDb;

/// A verified operator decision about the retained client-view event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Posted,
    VerifiedNotPosted,
}

impl Decision {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Posted => "posted",
            Self::VerifiedNotPosted => "verified_not_posted",
        }
    }
}

/// Stable operation identity, attributed Person and opaque private evidence pointer.
#[derive(Clone, Copy)]
pub struct Request {
    pub operation_id: Uuid,
    pub project_id: Uuid,
    pub ownership_epoch: Uuid,
    pub actor_person_id: Uuid,
    pub evidence_reference: Uuid,
    pub decision: Decision,
}

/// Content-free refusal; no captured values, connection details or evidence are logged.
#[derive(Debug, thiserror::Error)]
pub enum ReconciliationError {
    #[error("invalid Project Slack reconciliation identity")]
    Invalid,
    #[error("Project Slack reconciliation requires active Firm-management authority")]
    Unauthorized,
    #[error("Project Slack reconciliation requires the current active ownership epoch")]
    Inactive,
    #[error("Project Slack reconciliation conflicts with retained delivery evidence")]
    Conflict,
    #[error("Project Slack reconciliation storage unavailable")]
    Storage(#[source] sqlx::Error),
}

impl From<sqlx::Error> for ReconciliationError {
    fn from(error: sqlx::Error) -> Self {
        if error
            .as_database_error()
            .is_some_and(|error| error.is_unique_violation())
        {
            Self::Conflict
        } else {
            Self::Storage(error)
        }
    }
}

/// Resolve one unresolved event while serializing against native posting.
/// Exact operation replay is idempotent; another decision never overwrites it.
///
/// # Errors
/// Invalid identity, stale ownership, insufficient authority, conflicting evidence,
/// or unavailable PostgreSQL refuse without changing disposition or audit rows.
pub async fn reconcile(pg: &PostgresDb, request: Request) -> Result<(), ReconciliationError> {
    if [
        request.operation_id,
        request.project_id,
        request.ownership_epoch,
        request.actor_person_id,
        request.evidence_reference,
    ]
    .iter()
    .any(Uuid::is_nil)
    {
        return Err(ReconciliationError::Invalid);
    }
    let mut tx = pg.pool().begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(super::lane_key(request.project_id))
        .execute(&mut *tx)
        .await?;
    let epoch: Option<Uuid> = sqlx::query_scalar(
        "SELECT ownership_epoch FROM durable_object_ownership WHERE singleton FOR SHARE",
    )
    .fetch_optional(&mut *tx)
    .await?;
    if epoch != Some(request.ownership_epoch) {
        return Err(ReconciliationError::Inactive);
    }
    authorize(&mut tx, request).await?;
    let row = sqlx::query("SELECT ownership_epoch,native_active,client_view_disposition FROM project_slack_handover WHERE project_id=$1 FOR UPDATE")
        .bind(request.project_id).fetch_optional(&mut *tx).await?
        .ok_or(ReconciliationError::Inactive)?;
    if row.try_get::<Uuid, _>("ownership_epoch")? != request.ownership_epoch
        || !row.try_get::<bool, _>("native_active")?
    {
        return Err(ReconciliationError::Inactive);
    }
    if replay(&mut tx, request).await? {
        let disposition: String = row.try_get("client_view_disposition")?;
        let expected = match request.decision {
            Decision::Posted => "posted",
            Decision::VerifiedNotPosted => "not_posted",
        };
        let posted: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM project_slack_delivery WHERE project_id=$1 AND event_kind='client_view' AND event_key='client-view')")
            .bind(request.project_id).fetch_one(&mut *tx).await?;
        if disposition != expected || (request.decision == Decision::Posted && !posted) {
            return Err(ReconciliationError::Conflict);
        }
        tx.commit().await?;
        return Ok(());
    }
    let posted: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM project_slack_delivery WHERE project_id=$1 AND event_kind='client_view' AND event_key='client-view')")
        .bind(request.project_id).fetch_one(&mut *tx).await?;
    if row.try_get::<String, _>("client_view_disposition")? != "unresolved" || posted {
        return Err(ReconciliationError::Conflict);
    }
    sqlx::query("INSERT INTO project_slack_delivery_reconciliation(operation_id,project_id,ownership_epoch,actor_person_id,evidence_reference,decision) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(request.operation_id).bind(request.project_id).bind(request.ownership_epoch)
        .bind(request.actor_person_id).bind(request.evidence_reference).bind(request.decision.as_str())
        .execute(&mut *tx).await?;
    if request.decision == Decision::Posted {
        sqlx::query("INSERT INTO project_slack_delivery(project_id,event_kind,event_key) VALUES($1,'client_view','client-view')")
            .bind(request.project_id).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE project_slack_handover SET client_view_disposition=$2 WHERE project_id=$1")
        .bind(request.project_id)
        .bind(match request.decision {
            Decision::Posted => "posted",
            Decision::VerifiedNotPosted => "not_posted",
        })
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn authorize(
    tx: &mut Transaction<'_, Postgres>,
    request: Request,
) -> Result<(), ReconciliationError> {
    let firm: Option<Uuid> =
        sqlx::query_scalar("SELECT firm_id FROM project WHERE id=$1 FOR SHARE")
            .bind(request.project_id)
            .fetch_optional(&mut **tx)
            .await?
            .flatten();
    let firm = firm.ok_or(ReconciliationError::Unauthorized)?;
    let active: Option<String> =
        sqlx::query_scalar("SELECT status FROM firm WHERE id=$1 FOR SHARE")
            .bind(firm)
            .fetch_optional(&mut **tx)
            .await?;
    let person = sqlx::query("SELECT role,is_admitted FROM person WHERE id=$1 FOR SHARE")
        .bind(request.actor_person_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(ReconciliationError::Unauthorized)?;
    if active.as_deref() != Some("active") || !person.try_get::<bool, _>("is_admitted")? {
        return Err(ReconciliationError::Unauthorized);
    }
    let role: crate::persons::Role = serde_json::from_value(serde_json::Value::String(
        person.try_get::<String, _>("role")?,
    ))
    .map_err(|_| ReconciliationError::Unauthorized)?;
    // Hold membership and Person locks through commit; reuse the established policy
    // rather than inventing an alternate role or accepting a caller-supplied tier.
    sqlx::query("SELECT id FROM person_firm_role WHERE person_id=$1 AND firm_id=$2 FOR SHARE")
        .bind(request.actor_person_id)
        .bind(firm)
        .fetch_optional(&mut **tx)
        .await?;
    crate::postgres::tenancy::authorize_firm_management_on(
        &mut **tx,
        role,
        Some(request.actor_person_id),
        firm,
    )
    .await
    .map_err(|_| ReconciliationError::Unauthorized)
}

async fn replay(
    tx: &mut Transaction<'_, Postgres>,
    request: Request,
) -> Result<bool, ReconciliationError> {
    let row = sqlx::query("SELECT project_id,ownership_epoch,actor_person_id,evidence_reference,decision FROM project_slack_delivery_reconciliation WHERE operation_id=$1")
        .bind(request.operation_id).fetch_optional(&mut **tx).await?;
    let Some(row) = row else { return Ok(false) };
    if row.try_get::<Uuid, _>("project_id")? != request.project_id
        || row.try_get::<Uuid, _>("ownership_epoch")? != request.ownership_epoch
        || row.try_get::<Uuid, _>("actor_person_id")? != request.actor_person_id
        || row.try_get::<Uuid, _>("evidence_reference")? != request.evidence_reference
        || row.try_get::<String, _>("decision")? != request.decision.as_str()
    {
        return Err(ReconciliationError::Conflict);
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
