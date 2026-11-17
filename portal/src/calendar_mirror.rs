//! The nightly Deadline mirror's source: the store, the deployment's
//! Workspace calendar, and the canonical host for matter-page links.
//!
//! The schedule trait lives in [`workflows::dbos_engine::deadline_mirror`];
//! this is the host side that resolves each matter's credential-bearing
//! calendar and runs the store-level reconcile against it. A deployment
//! with no Workspace calendar configured reports every pass as unmet
//! rather than failing the night.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use uuid::Uuid;
use workflows::dbos_engine::deadline_mirror::{MirrorQueueError, MirrorSource};

pub(crate) struct ScheduledMirror {
    pub surreal: store::surreal::SurrealDb,
    pub providers: Arc<dyn crate::integrations::IntegrationProviders>,
    /// The canonical host the matter-page link names, when configured.
    pub base_url: Option<String>,
}

#[async_trait::async_trait]
impl MirrorSource for ScheduledMirror {
    async fn open_projects(
        &self,
        _now: DateTime<Utc>,
    ) -> Result<Vec<(Uuid, String)>, MirrorQueueError> {
        let all = store::projects::all(&self.surreal)
            .await
            .map_err(|_| MirrorQueueError::Queue)?;
        Ok(all
            .into_iter()
            .filter(|project| project.status == "open")
            .map(|project| (project.id, project.code))
            .collect())
    }

    async fn reconcile(
        &self,
        project_id: Uuid,
        dry_run: bool,
        _now: DateTime<Utc>,
    ) -> Result<store::deadline_calendar::MirrorReport, String> {
        let calendar = self
            .providers
            .calendar(&self.surreal, project_id)
            .await
            .map_err(|error| error.to_string())?;
        let _guard = crate::integrations_api::calendar_lock(project_id)
            .await
            .map_err(|()| "Calendar reconcile lock unavailable")?;
        store::deadline_calendar::reconcile(
            &self.surreal,
            calendar.as_ref(),
            project_id,
            self.base_url.as_deref(),
            dry_run,
        )
        .await
        .map_err(|error| error.to_string())
    }
}

use workflows::dbos_engine::project_calendar::{Source, SyncError};

pub(crate) struct MembershipSource {
    pub surreal: store::surreal::SurrealDb,
    pub providers: Arc<dyn crate::integrations::IntegrationProviders>,
}

#[async_trait::async_trait]
impl Source for MembershipSource {
    async fn candidates(&self) -> Result<Vec<Uuid>, SyncError> {
        Ok(store::projects::all(&self.surreal)
            .await
            .map_err(|_| sync_error("reading Project Calendar assignments"))?
            .into_iter()
            .filter(|project| {
                project.firm_id.is_some()
                    && project
                        .calendar_id
                        .as_ref()
                        .is_some_and(|id| !id.trim().is_empty())
            })
            .map(|project| project.id)
            .collect())
    }

    async fn sync(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<store::project_calendar::ParticipantSync>, SyncError> {
        let _guard = crate::integrations_api::calendar_lock(project_id)
            .await
            .map_err(|()| sync_error("locking Project Calendar"))?;
        let calendar = self
            .providers
            .calendar(&self.surreal, project_id)
            .await
            .map_err(|error| sync_error(error.slug()))?;
        store::project_calendar::sync_membership(&self.surreal, project_id, calendar.as_ref())
            .await
            .map_err(|error| match error {
                store::project_calendar::ProjectCalendarError::Participant {
                    person_id,
                    source,
                } => {
                    let mut error = provider_error(source);
                    error.person_id = Some(person_id);
                    error
                }
                store::project_calendar::ProjectCalendarError::Calendar(error) => {
                    provider_error(error)
                }
                _ => sync_error("reading calendar assignment or grant records"),
            })
    }
}

fn provider_error(error: cloud::CalendarError) -> SyncError {
    match error {
        cloud::CalendarError::Api { action, status } => {
            let mut error = sync_error(action);
            error.status = Some(status);
            error
        }
        cloud::CalendarError::Request { action, .. }
        | cloud::CalendarError::Response { action, .. } => sync_error(action),
        error => sync_error(&error.to_string()),
    }
}

fn sync_error(phase: &str) -> SyncError {
    SyncError {
        phase: phase.into(),
        status: None,
        person_id: None,
        attempts: 0,
    }
}
