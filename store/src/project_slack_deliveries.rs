//! Posted notice admission for a Project's native Slack lane.
//!
//! The caller holds [`lane_key`]'s advisory lock across admission and posting.
//! Captured rows remain inactive until source handover is independently verified.

use sqlx::{Postgres, Row, Transaction};
use thiserror::Error;
use uuid::Uuid;

use crate::postgres::PostgresDb;

pub mod reconciliation;

/// The same serialization key used by the native Project Slack adapter.
#[must_use]
pub fn lane_key(project_id: Uuid) -> i64 {
    let (_, lower) = project_id.as_u64_pair();
    i64::from_be_bytes(lower.to_be_bytes())
}

/// A known event's admission while the caller owns its Project's lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// A successful post is already recorded.
    Posted,
    /// Native ownership and posting evidence permit this event.
    Allowed,
}

/// A delivery cannot proceed without complete native ownership and evidence.
#[derive(Debug, Error)]
pub enum DeliveryError {
    #[error("Project Slack handover is not active")]
    HandoverRequired,
    #[error("Project Slack client-view posting evidence is unresolved")]
    Unresolved,
    #[error("Project Slack main-push history was not imported for this Project")]
    MainPushHistoryUnavailable,
    #[error("invalid Project Slack event identity")]
    InvalidEvent,
    #[error("Project Slack delivery storage unavailable")]
    Storage,
}

impl From<sqlx::Error> for DeliveryError {
    fn from(_: sqlx::Error) -> Self {
        Self::Storage
    }
}

/// Initialize only a Project created by the native owning command in this transaction.
/// An absent complete workflow-ownership epoch leaves it unenrolled. PostgreSQL
/// application authority alone cannot attest that a Restate writer never posted.
/// Never use this to enroll an existing or imported Project.
pub(crate) async fn initialize_fresh(
    transaction: &mut Transaction<'_, Postgres>,
    project_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO project_slack_handover (project_id,ownership_epoch,native_active,client_view_disposition) SELECT $1,ownership_epoch,true,'not_posted' FROM durable_object_ownership WHERE singleton")
        .bind(project_id)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

pub(crate) fn validate_event(kind: &str, key: &str) -> Result<(), DeliveryError> {
    if kind == "client_view" && key == "client-view" {
        return Ok(());
    }
    if kind == "main_push" {
        if let Some((sha, attempt)) = key.split_once(':') {
            if (7..=40).contains(&sha.len())
                && sha
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                && attempt.parse::<u32>().is_ok_and(|attempt| attempt > 0)
            {
                return Ok(());
            }
        }
    }
    Err(DeliveryError::InvalidEvent)
}

/// Check admission before any provider access, while holding the Project lane.
///
/// # Errors
/// Fails closed for missing ownership, unresolved client-view evidence or invalid events.
pub async fn admission(
    postgres: &PostgresDb,
    project_id: Uuid,
    kind: &str,
    key: &str,
) -> Result<Admission, DeliveryError> {
    validate_event(kind, key)?;
    let row = sqlx::query("SELECT h.native_active,h.client_view_disposition,e.origin FROM project_slack_handover h JOIN durable_object_ownership o ON o.ownership_epoch=h.ownership_epoch LEFT JOIN durable_object_handover e ON h.capture_id=e.capture_id WHERE h.project_id=$1")
        .bind(project_id).fetch_optional(postgres.pool()).await?
        .ok_or(DeliveryError::HandoverRequired)?;
    if !row.try_get::<bool, _>("native_active")? {
        return Err(DeliveryError::HandoverRequired);
    }
    let posted:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM project_slack_delivery WHERE project_id=$1 AND event_kind=$2 AND event_key=$3)")
        .bind(project_id).bind(kind).bind(key).fetch_one(postgres.pool()).await?;
    if posted {
        return Ok(Admission::Posted);
    }
    if kind == "main_push"
        && row.try_get::<Option<String>, _>("origin")?.as_deref() == Some("source_discarded")
    {
        return Err(DeliveryError::MainPushHistoryUnavailable);
    }
    if kind == "client_view" {
        match row
            .try_get::<String, _>("client_view_disposition")?
            .as_str()
        {
            "posted" => return Ok(Admission::Posted),
            "not_posted" => {}
            _ => return Err(DeliveryError::Unresolved),
        }
    }
    Ok(Admission::Allowed)
}

/// Record a provider post in a separate PG-only checkpointed step AFTER the
/// existing provider-post step has durably completed. A receipt failure retries
/// only this write and reuses the provider checkpoint on recovery.
///
/// # Errors
/// The native ownership/evidence check or PostgreSQL write may fail.
pub async fn record_posted(
    postgres: &PostgresDb,
    project_id: Uuid,
    kind: &str,
    key: &str,
) -> Result<(), DeliveryError> {
    admission(postgres, project_id, kind, key).await?;
    sqlx::query("INSERT INTO project_slack_delivery (project_id,event_kind,event_key) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING")
        .bind(project_id).bind(kind).bind(key).execute(postgres.pool()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn lane_key_preserves_the_existing_runtime_bits() {
        for id in [
            uuid::Uuid::nil(),
            uuid::Uuid::max(),
            uuid::Uuid::from_u128(1),
            uuid::Uuid::from_u128(1_u128 << 63),
            uuid::Uuid::new_v4(),
            uuid::Uuid::now_v7(),
        ] {
            assert_eq!(super::lane_key(id), id.as_u128() as i64);
        }
    }
}
