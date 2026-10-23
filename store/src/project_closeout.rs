//! Closing out a Project: required handoff acknowledgments and the gate in
//! front of final archival and repository deletion.
//!
//! Closing a matter, preserving its [Project Recovery
//! Archive](../../docs/glossary/project-recovery-archive.md), handing files to a
//! recipient, and deleting its source repository are separate actions. This
//! module records the one a recipient performs — acknowledging the exact
//! manifest they received — and answers whether the irreversible ones may
//! proceed. [`readiness`] names every blocker at once:
//!
//! - the Project must be closed;
//! - a committed recovery generation must have been cut after the close, with
//!   no document changed since its cutoff;
//! - every handoff marked required must be acknowledged against its manifest;
//! - and, before a repository is deleted, that generation's verified Git bundle
//!   must hold the live HEAD and no Legal Hold may stand on the matter.
//!
//! Safety copies are never blocked: requesting a generation or a handoff works
//! at any time. A repository-less Project needs no source backup. The 180-day
//! document window and Project retention stay separate clocks; nothing here
//! starts or resets either.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use crate::postgres::PostgresDb;
use crate::surreal::SurrealDb;

/// One recorded acknowledgment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Acknowledgment {
    pub handoff_id: Uuid,
    pub manifest_sha256: String,
    pub acknowledged_by_person_id: Uuid,
    pub acknowledged_at: DateTime<Utc>,
}

/// Why an acknowledgment was refused.
#[derive(Debug, thiserror::Error)]
pub enum AcknowledgmentError {
    #[error("handoff not found")]
    NotFound,
    #[error("only the handoff's recipient may acknowledge it")]
    NotRecipient,
    #[error("the acknowledged manifest is not the handoff's manifest")]
    ManifestMismatch,
    #[error("database: {0}")]
    Database(String),
}

impl From<sqlx::Error> for AcknowledgmentError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

/// Record that the recipient received exactly the manifest `manifest_sha256`
/// names. A retry with the same digest returns the original acknowledgment.
///
/// # Errors
/// [`AcknowledgmentError`] for a missing or foreign handoff, another person,
/// or a different manifest.
pub async fn acknowledge(
    pg: &PostgresDb,
    project_id: Uuid,
    handoff_id: Uuid,
    person_id: Uuid,
    manifest_sha256: &str,
    now: DateTime<Utc>,
) -> Result<Acknowledgment, AcknowledgmentError> {
    let handoff = crate::project_handoffs::find(pg, project_id, handoff_id)
        .await
        .map_err(|_| AcknowledgmentError::NotFound)?;
    if handoff.recipient_person_id != person_id {
        return Err(AcknowledgmentError::NotRecipient);
    }
    if handoff.manifest_sha256 != manifest_sha256 {
        return Err(AcknowledgmentError::ManifestMismatch);
    }
    sqlx::query(
        "INSERT INTO project_handoff_acknowledgment \
         (handoff_id, manifest_sha256, acknowledged_by_person_id, acknowledged_at) \
         VALUES ($1, $2, $3, $4) ON CONFLICT (handoff_id) DO NOTHING",
    )
    .bind(handoff_id)
    .bind(manifest_sha256)
    .bind(person_id)
    .bind(now)
    .execute(pg.pool())
    .await?;
    let row = sqlx::query(
        "SELECT handoff_id, manifest_sha256, acknowledged_by_person_id, acknowledged_at \
         FROM project_handoff_acknowledgment WHERE handoff_id = $1",
    )
    .bind(handoff_id)
    .fetch_one(pg.pool())
    .await?;
    Ok(Acknowledgment {
        handoff_id: row.try_get("handoff_id")?,
        manifest_sha256: row.try_get("manifest_sha256")?,
        acknowledged_by_person_id: row.try_get("acknowledged_by_person_id")?,
        acknowledged_at: row.try_get("acknowledged_at")?,
    })
}

/// Which irreversible step a readiness check is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closeout<'a> {
    /// Moving a closed Project to `archived`.
    Archive,
    /// Deleting the Project's source repository whose live head is this SHA.
    DeleteRepository { live_head: &'a str },
}

/// Every reason the step may not proceed yet; empty means ready.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Readiness {
    pub blockers: Vec<String>,
}

impl Readiness {
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.blockers.is_empty()
    }
}

/// Name every blocker in front of `step` for `project_id`. Without
/// PostgreSQL the archive and handoff records cannot be checked, so the step
/// is blocked rather than assumed safe.
///
/// # Errors
/// A store read failure.
pub async fn readiness(
    surreal: &SurrealDb,
    pg: Option<&PostgresDb>,
    project_id: Uuid,
    step: Closeout<'_>,
) -> Result<Readiness, String> {
    let mut blockers = Vec::new();
    let project = crate::projects::find_by_id(surreal, project_id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "project not found".to_string())?;
    let closed_at = project
        .closed_at
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc));
    if !matches!(project.status.as_str(), "closed" | "archived") || closed_at.is_none() {
        blockers.push("the Project is not closed".into());
    }
    let assets = crate::assets::for_project_including_deleted(surreal, project_id)
        .await
        .map_err(|error| error.to_string())?;
    if matches!(step, Closeout::DeleteRepository { .. })
        && assets.iter().any(|asset| asset.legal_hold_at.is_some())
    {
        blockers.push("a Legal Hold stands on the Project".into());
    }
    let Some(pg) = pg else {
        blockers.push("the recovery archive store is unavailable".into());
        return Ok(Readiness { blockers });
    };
    match crate::project_archives::latest_committed(pg, project_id)
        .await
        .map_err(|error| error.to_string())?
    {
        None => blockers.push("no committed recovery generation exists".into()),
        Some(generation) => {
            let cutoff = generation.cutoff_at.unwrap_or(generation.requested_at);
            if closed_at.is_some_and(|closed| cutoff < closed) {
                blockers.push("the latest recovery generation predates the close".into());
            }
            if assets.iter().any(|asset| asset.updated_at > cutoff) {
                blockers.push("documents changed after the latest recovery generation".into());
            }
            if let Closeout::DeleteRepository { live_head: head } = step {
                let covered = generation.source_backup().is_some_and(|backup| {
                    backup
                        .refs
                        .lines()
                        .any(|line| line.split_whitespace().any(|word| word == head))
                });
                if !covered {
                    blockers.push(
                        "the recovery generation's verified source backup does not hold the live \
                         HEAD"
                            .into(),
                    );
                }
            }
        }
    }
    let rows = sqlx::query(
        "SELECT h.id FROM project_handoff h \
         LEFT JOIN project_handoff_acknowledgment a \
           ON a.handoff_id = h.id AND a.manifest_sha256 = h.manifest_sha256 \
         WHERE h.project_id = $1 AND h.required AND a.handoff_id IS NULL ORDER BY h.created_at",
    )
    .bind(project_id)
    .fetch_all(pg.pool())
    .await
    .map_err(|error| error.to_string())?;
    for row in rows {
        let id: Uuid = row.try_get("id").map_err(|error| error.to_string())?;
        blockers.push(format!("required handoff {id} is not acknowledged"));
    }
    Ok(Readiness { blockers })
}
