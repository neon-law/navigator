//! Reconcile one open Project under the same PostgreSQL write lock as the
//! on-demand mirror and scheduled membership repair.

use cloud::CalendarService;
use store::{application_store::ApplicationStore, deadline_calendar, project_calendar};
use uuid::Uuid;

use super::TagOutcome;
use crate::dbos_nightly_project_update::NightlyError;

pub(crate) async fn reconcile(
    application_store: &ApplicationStore,
    project_id: Uuid,
    portal_origin: Option<&str>,
) -> Result<TagOutcome, NightlyError> {
    let failed = |phase: &str| NightlyError::Calendar(phase.to_owned());
    let _guard = project_calendar::lock_writes(project_id)
        .await
        .map_err(|_| failed("locking the Project"))?
        .ok_or_else(|| failed("locking the Project"))?;
    let project = store::projects::find_by_id(application_store, project_id)
        .await
        .map_err(|_| failed("reading the Project"))?
        .ok_or_else(|| failed("reading the Project"))?;
    if project.status != "open" {
        return Ok(TagOutcome::Calendar { synced: false });
    }
    let firm_id = project
        .firm_id
        .ok_or_else(|| failed("resolving the owning Firm"))?;
    let Some(owner) = store::firms::calendar_owner(firm_id)
        .await
        .map_err(|_| failed("resolving the Firm calendar owner"))?
    else {
        return Ok(TagOutcome::Calendar { synced: false });
    };
    let calendar = cloud::GoogleCalendar::for_owner(&owner)
        .await
        .map_err(|_| failed("opening the Firm calendar"))?;
    let ensured = project_calendar::ensure(application_store, project_id, &calendar)
        .await
        .map_err(|_| failed("provisioning the calendar"))?;
    match ensured.outcome {
        project_calendar::CalendarOutcome::RecordedResourceMissing => {
            return Err(failed("locating the recorded calendar"))
        }
        project_calendar::CalendarOutcome::ReadOnly => {
            return Ok(TagOutcome::Calendar { synced: false })
        }
        _ => {}
    }
    let calendar_id = store::projects::find_by_id(application_store, project_id)
        .await
        .map_err(|_| failed("reading the recorded calendar"))?
        .and_then(|project| project.calendar_id)
        .ok_or_else(|| failed("reading the recorded calendar"))?;
    if !calendar
        .recorded_calendar_matches(&project.code, &calendar_id)
        .await
        .map_err(|_| failed("checking calendar ownership"))?
    {
        return Err(failed("checking calendar ownership"));
    }
    calendar
        .verify_private_acl(&calendar_id)
        .await
        .map_err(|_| failed("checking calendar privacy"))?;
    let mirror = deadline_calendar::reconcile(
        application_store,
        &calendar,
        project_id,
        portal_origin,
        false,
    )
    .await
    .map_err(|_| failed("mirroring Deadlines"))?;
    if mirror.outcome == deadline_calendar::MirrorOutcome::SkippedClosed {
        return Ok(TagOutcome::Calendar { synced: false });
    }
    if mirror.outcome != deadline_calendar::MirrorOutcome::Mirrored {
        return Err(failed("locating the recorded calendar"));
    }
    project_calendar::sync_membership(application_store, project_id, &calendar)
        .await
        .map_err(|_| failed("verifying calendar membership"))?;
    if !mirror.failures.is_empty() {
        return Err(failed("mirroring individual Deadlines"));
    }
    Ok(TagOutcome::Calendar { synced: true })
}
