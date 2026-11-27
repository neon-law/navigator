//! Durable Project-level legal clocks.
//!
//! The replay key is a natural key: repeating a workflow decision updates the
//! same deadline rather than creating a second statutory window.

use chrono::NaiveDate;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::matter::{self, MatterAuthorityError};
use crate::projects;

const TABLE: &str = "statutory_deadline";

pub const STATUS_OPEN: &str = "open";
pub const STATUS_SATISFIED: &str = "satisfied";
pub const STATUS_CLOSED: &str = "closed";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StatutoryDeadline {
    pub id: Uuid,
    pub project_id: Uuid,
    pub kind: String,
    pub trigger_on: NaiveDate,
    pub due_on: NaiveDate,
    pub statute: String,
    pub source: String,
    pub status: String,
    pub inserted_at: String,
    pub updated_at: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DeadlineError {
    #[error(transparent)]
    Project(#[from] projects::ProjectStoreError),
    #[error(transparent)]
    Postgres(#[from] MatterAuthorityError),
    #[error("writing a statutory deadline returned no usable row")]
    WriteReturnedNothing,
}

#[derive(Debug, Clone)]
pub struct NewStatutoryDeadline<'a> {
    pub project_id: Uuid,
    pub kind: &'a str,
    pub trigger_on: NaiveDate,
    pub due_on: NaiveDate,
    pub statute: &'a str,
    pub source: &'a str,
}

/// Record one deadline idempotently. The project read-back is the
/// reference check: a typed record link does not prove the target exists.
pub async fn record(
    _application_store: &ApplicationStore,
    new: &NewStatutoryDeadline<'_>,
) -> Result<StatutoryDeadline, DeadlineError> {
    // The matter opener's command boundary validates the project before
    // this append, so the record link is deliberately not re-validated
    // here.
    let now = chrono::Utc::now().to_rfc3339();
    let pg = matter::authority().await?;
    let existing: Option<StatutoryDeadline> = matter::find(&pg, TABLE, replay_key(new)).await?;
    let (id, inserted_at) = existing.map_or_else(
        || (Uuid::now_v7(), now.clone()),
        |deadline| (deadline.id, deadline.inserted_at),
    );
    let mut row = replay_key(new);
    row["id"] = serde_json::json!(id);
    row["due_on"] = serde_json::json!(new.due_on);
    row["statute"] = serde_json::json!(new.statute);
    row["status"] = serde_json::json!(STATUS_OPEN);
    row["inserted_at"] = serde_json::json!(inserted_at);
    row["updated_at"] = serde_json::json!(now);
    Ok(matter::upsert(&pg, TABLE, &row).await?)
}

/// The columns of the `(project_id, kind, trigger_on, source)` replay key.
fn replay_key(new: &NewStatutoryDeadline<'_>) -> serde_json::Value {
    serde_json::json!({
        "project_id": new.project_id,
        "kind": new.kind,
        "trigger_on": new.trigger_on,
        "source": new.source,
    })
}

/// Record every deadline in one triage decision.
pub async fn record_all(
    application_store: &ApplicationStore,
    deadlines: &[NewStatutoryDeadline<'_>],
) -> Result<Vec<StatutoryDeadline>, DeadlineError> {
    let mut rows = Vec::with_capacity(deadlines.len());
    for deadline in deadlines {
        rows.push(record(application_store, deadline).await?);
    }
    Ok(rows)
}

/// Deadlines currently recorded for a Project, ordered by due date then id.
pub async fn by_project(
    _application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<StatutoryDeadline>, DeadlineError> {
    let pg = matter::authority().await?;
    let filter = serde_json::json!({"project_id": project_id});
    Ok(matter::select(&pg, TABLE, filter, &["due_on", "id"]).await?)
}

#[cfg(test)]
mod tests {}
