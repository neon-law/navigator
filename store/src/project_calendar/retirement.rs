//! A closed Project keeps its calendar until the same event set is covered by a published Iceberg generation.
use super::{sync_db, verify_private_calendar, ProjectCalendarError};
use crate::{postgres::PostgresDb, projects::Project};
use cloud::calendar::{CalendarEventBackup, CalendarService};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Confirm captures visible to the Navigator transaction that exported both calendar tables.
pub async fn confirm_archived_calendars(
    db: &PostgresDb,
    snapshot: &str,
    generation: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE project_calendar_retirement SET archive_generation=$2
        WHERE archive_generation IS NULL AND pg_visible_in_snapshot(capture_xid::xid8, $1::pg_snapshot)")
        .bind(snapshot)
        .bind(generation)
        .execute(db.pool())
        .await?;
    Ok(())
}

pub(super) struct Inventory {
    digest: String,
    versions: serde_json::Value,
}

impl Inventory {
    pub(super) fn capture(events: &mut [CalendarEventBackup]) -> Self {
        let digest = digest(events);
        let versions = events
            .iter()
            .map(|e| (e.href.clone(), serde_json::Value::String(e.etag.clone())))
            .collect();
        Self {
            digest,
            versions: serde_json::Value::Object(versions),
        }
    }
}

fn digest(events: &mut [CalendarEventBackup]) -> String {
    events.sort_unstable_by(|a, b| a.href.cmp(&b.href));
    let mut hash = Sha256::new();
    for event in events {
        for field in [&event.href, &event.etag, &event.ics] {
            hash.update((field.len() as u64).to_be_bytes());
            hash.update(field.as_bytes());
        }
    }
    hex::encode(hash.finalize())
}

// A crash can leave a committed Google DELETE and the old Project link. Only the durable
// deletion intent for this exact resource and owner can finish such a pass.
pub(super) async fn finish_missing(
    db: &PostgresDb,
    project: Uuid,
    firm: Uuid,
    id: &str,
    owner: &str,
    calendar: &dyn CalendarService,
) -> Result<bool, ProjectCalendarError> {
    let mut tx = db.pool().begin().await.map_err(sync_db)?;
    let pending: bool = sqlx::query_scalar("SELECT EXISTS(
        SELECT 1 FROM project p JOIN project_calendar_retirement r
          ON (r.project_id,r.firm_id,r.calendar_id)=(p.id,p.firm_id,p.calendar_id)
        WHERE p.id=$1 AND p.firm_id=$2 AND p.calendar_id=$3 AND r.owner_mailbox=$4
          AND r.delete_etag IS NOT NULL AND r.archive_generation IS NOT NULL AND r.deleted_at IS NULL
        FOR UPDATE OF p,r)")
        .bind(project)
        .bind(firm)
        .bind(id)
        .bind(owner)
        .fetch_one(&mut *tx)
        .await.map_err(sync_db)?;
    if !pending || calendar.calendar_etag(id).await?.is_some() {
        return Ok(false);
    }
    unlink(&mut tx, project, firm, id).await?;
    tx.commit().await.map_err(sync_db)?;
    Ok(true)
}

pub(super) async fn retire(
    db: &PostgresDb,
    project: &Project,
    firm: Uuid,
    id: &str,
    owner: &str,
    inventory: &Inventory,
    calendar: &dyn CalendarService,
) -> Result<(), ProjectCalendarError> {
    // The event rows have committed. A single MVCC snapshot containing this receipt can therefore
    // restore all those rows, including the final empty calendar.
    sqlx::query("INSERT INTO project_calendar_retirement (project_id,firm_id,calendar_id,owner_mailbox,content_sha256,event_versions)
        VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (project_id,firm_id,calendar_id) DO UPDATE
        SET owner_mailbox=EXCLUDED.owner_mailbox,content_sha256=EXCLUDED.content_sha256,event_versions=EXCLUDED.event_versions,
            capture_xid=pg_current_xact_id()::text,archive_generation=NULL,delete_etag=NULL,deleted_at=NULL
        WHERE (project_calendar_retirement.content_sha256,project_calendar_retirement.owner_mailbox)
              IS DISTINCT FROM (EXCLUDED.content_sha256,EXCLUDED.owner_mailbox)")
        .bind(project.id)
        .bind(firm)
        .bind(id)
        .bind(owner)
        .bind(&inventory.digest)
        .bind(&inventory.versions)
        .execute(db.pool())
        .await.map_err(sync_db)?;

    let mut tx = db.pool().begin().await.map_err(sync_db)?;
    // A reopen or relink must serialize with the final provider operation.
    let ready = sqlx::query_scalar::<_, String>("SELECT p.code FROM project p JOIN firm f ON f.id=p.firm_id
        JOIN project_calendar_retirement r ON (r.project_id,r.firm_id,r.calendar_id)=(p.id,p.firm_id,p.calendar_id)
        WHERE p.id=$1 AND p.firm_id=$2 AND p.calendar_id=$3 AND p.status IN ('closed','archived')
          AND f.workspace_provider='google_workspace'
          AND r.owner_mailbox=$4 AND r.content_sha256=$5 AND r.archive_generation IS NOT NULL AND r.deleted_at IS NULL
          AND NOT EXISTS (SELECT 1 FROM project other WHERE other.id<>p.id AND other.calendar_id=p.calendar_id)
        FOR UPDATE OF p,f")
        .bind(project.id)
        .bind(firm)
        .bind(id)
        .bind(owner)
        .bind(&inventory.digest)
        .fetch_optional(&mut *tx)
        .await.map_err(sync_db)?;
    let Some(code) = ready else {
        return Ok(());
    };
    // A saved choice overrides the bootstrap. Lock both the Firm and its optional settings
    // row so clearing or replacing the owner serializes with the provider request.
    let current = sqlx::query_scalar::<_, Option<String>>(
        "SELECT google_workspace_owner FROM firm_calendar_settings WHERE firm_id=$1 FOR SHARE",
    )
    .bind(firm)
    .fetch_optional(&mut *tx)
    .await
    .map_err(sync_db)?;
    if current.is_some_and(|mailbox| mailbox.as_deref() != Some(owner)) {
        return Ok(());
    }
    let version = calendar
        .calendar_etag(id)
        .await?
        .ok_or(ProjectCalendarError::NotFound)?;
    verify_private_calendar(calendar, &code, id).await?;
    // The close operation revokes writer access. Group and domain writers must also be absent.
    calendar.verify_retirement_acl(id, owner).await?;
    if digest(&mut calendar.event_backups(id).await?) != inventory.digest {
        return Err(ProjectCalendarError::Db(
            "Calendar changed during the closing backup".into(),
        ));
    }
    // Commit intent outside the Project-row transaction so it survives an unknown provider outcome.
    sqlx::query("UPDATE project_calendar_retirement SET delete_etag=$4 WHERE project_id=$1 AND firm_id=$2 AND calendar_id=$3")
        .bind(project.id)
        .bind(firm)
        .bind(id)
        .bind(&version)
        .execute(db.pool())
        .await.map_err(sync_db)?;
    calendar.delete_calendar(id, &version).await?;
    unlink(&mut tx, project.id, firm, id).await?;
    tx.commit().await.map_err(sync_db)
}

async fn unlink(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project: Uuid,
    firm: Uuid,
    id: &str,
) -> Result<(), ProjectCalendarError> {
    sqlx::query("UPDATE project SET calendar_id=NULL, updated_at=now() WHERE id=$1 AND firm_id=$2 AND calendar_id=$3")
        .bind(project)
        .bind(firm)
        .bind(id)
        .execute(&mut **tx)
        .await.map_err(sync_db)?;
    sqlx::query("UPDATE project_calendar_retirement SET deleted_at=now() WHERE project_id=$1 AND firm_id=$2 AND calendar_id=$3")
        .bind(project)
        .bind(firm)
        .bind(id)
        .execute(&mut **tx)
        .await.map_err(sync_db)?;
    Ok(())
}
