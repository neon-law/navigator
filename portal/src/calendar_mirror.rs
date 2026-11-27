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
    pub application_store: store::application_store::ApplicationStore,
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
        store::deadline_calendar::open_projects()
            .await
            .map_err(|_| MirrorQueueError::Queue)
    }

    async fn reconcile(
        &self,
        project_id: Uuid,
        dry_run: bool,
        _now: DateTime<Utc>,
    ) -> Result<store::deadline_calendar::MirrorReport, String> {
        let calendar = self
            .providers
            .calendar(&self.application_store, project_id)
            .await
            .map_err(|error| error.to_string())?;
        let _guard = crate::integrations_api::calendar_lock(project_id)
            .await
            .map_err(|()| "Calendar reconcile lock unavailable")?;
        store::deadline_calendar::reconcile(
            &self.application_store,
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
    pub application_store: store::application_store::ApplicationStore,
    pub providers: Arc<dyn crate::integrations::IntegrationProviders>,
    pub base_url: Option<String>,
}

#[async_trait::async_trait]
impl Source for MembershipSource {
    async fn candidates(&self) -> Result<Vec<Uuid>, SyncError> {
        store::project_calendar::membership_candidates()
            .await
            .map_err(|_| sync_error("reading Project Calendar candidates"))
    }

    async fn booking_candidates(&self) -> Result<Vec<Uuid>, SyncError> {
        let db = store::attorney_calendars::postgres()
            .await
            .map_err(|_| sync_error("reading Attorney Calendar registrations"))?;
        store::attorney_calendars::booking_candidates(&db)
            .await
            .map_err(|_| sync_error("reading booking calendar registrations"))
    }

    async fn sync_booking(&self, person_id: Uuid) -> Result<u64, SyncError> {
        let _guard = crate::integrations_api::calendar_lock(person_id)
            .await
            .map_err(|()| sync_error("locking booking availability"))?;
        let db = store::attorney_calendars::postgres()
            .await
            .map_err(|_| sync_error("reading Attorney Calendar registrations"))?;
        let Some(target) = store::attorney_calendars::booking_calendar(&db, person_id)
            .await
            .map_err(|_| sync_error("reading the booking calendar registration"))?
        else {
            return Ok(0);
        };
        let person = store::persons::find_by_id(&self.application_store, person_id)
            .await
            .map_err(|_| sync_error("reading the booking calendar owner"))?
            .filter(|person| person.email_confirmed)
            .ok_or_else(|| sync_error("verifying the booking calendar owner"))?;
        let writer = cloud::calendar::GoogleCalendar::for_owner(&person.email)
            .await
            .map_err(provider_error)?;
        writer
            .verify_booking_calendar(&target)
            .await
            .map_err(provider_error)?;

        let memberships = store::firms::memberships_for_person(&self.application_store, person_id)
            .await
            .map_err(|_| sync_error("reading the attorney's Firm memberships"))?;
        let firms: Vec<_> = memberships
            .iter()
            .map(|membership| membership.firm_id)
            .collect();
        let assigned =
            store::project_calendar::shared_freebusy_calendars(&db, &firms, &[person_id])
                .await
                .map_err(|_| sync_error("reading current Project Calendar assignments"))?;
        let first_project = assigned
            .values()
            .flatten()
            .next()
            .map(|project| project.project_id);
        let start = Utc::now().fixed_offset();
        let end = start + chrono::TimeDelta::days(90);
        let mut events = Vec::new();
        let mut unavailable = None;
        for projects in assigned.values() {
            let Some(first) = projects.first() else {
                continue;
            };
            let reader = match self
                .providers
                .calendar(&self.application_store, first.project_id)
                .await
            {
                Ok(reader) => reader,
                Err(error) => {
                    unavailable.get_or_insert_with(|| sync_error(error.slug()));
                    continue;
                }
            };
            let verified = match reader.owned_calendar_codes().await {
                Ok(verified) => verified,
                Err(error) => {
                    unavailable.get_or_insert_with(|| provider_error(error));
                    continue;
                }
            };
            for project in projects {
                let Some(id) = project.calendar_id.as_deref().filter(|id| {
                    *id != target.as_str() && verified.get(*id) == Some(&project.code)
                }) else {
                    unavailable.get_or_insert_with(|| {
                        sync_error("verifying a Project Calendar for booking availability")
                    });
                    continue;
                };
                match reader.events_in_window(id, start, end).await {
                    Ok(source) => events.extend(source),
                    Err(error) => {
                        unavailable.get_or_insert_with(|| provider_error(error));
                    }
                }
            }
        }
        let count = store::attorney_calendars::booking::reconcile(
            &writer,
            &target,
            events,
            unavailable.is_none(),
        )
        .await
        .map_err(provider_error)?;
        if let Some(error) = unavailable {
            return Err(error);
        }
        drop(_guard);
        if let Some(project_id) = first_project {
            let project = store::projects::find_by_id(&self.application_store, project_id)
                .await
                .map_err(|_| sync_error("reading the work schedule time zone"))?
                .ok_or_else(|| sync_error("reading the work schedule time zone"))?;
            let zone = store::projects::calendar_timezone(&self.application_store, &project)
                .await
                .map_err(|_| sync_error("reading the work schedule time zone"))?;
            crate::deadline_scheduling::sync(
                &self.application_store,
                self.providers.as_ref(),
                person_id,
                zone,
                self.base_url.as_deref(),
            )
            .await
            .map_err(|_| sync_error("reconciling Deadline work reservations"))?;
        }
        Ok(count)
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
            .calendar(&self.application_store, project_id)
            .await
            .map_err(|error| sync_error(error.slug()))?;
        store::project_calendar::sync_membership(
            &self.application_store,
            project_id,
            calendar.as_ref(),
        )
        .await
        .map_err(|error| match error {
            store::project_calendar::ProjectCalendarError::Participant { person_id, source } => {
                let mut error = provider_error(source);
                error.person_id = Some(person_id);
                error
            }
            store::project_calendar::ProjectCalendarError::Calendar(error) => provider_error(error),
            _ => sync_error("reading calendar assignment or grant records"),
        })
    }

    async fn backup(&self, project_id: Uuid) -> Result<u64, SyncError> {
        let _guard = crate::integrations_api::calendar_lock(project_id)
            .await
            .map_err(|()| sync_error("locking Project Calendar"))?;
        let calendar = self
            .providers
            .calendar(&self.application_store, project_id)
            .await
            .map_err(|error| sync_error(error.slug()))?;
        store::project_calendar::backup_events(
            &self.application_store,
            project_id,
            calendar.as_ref(),
        )
        .await
        .map_err(|error| match error {
            store::project_calendar::ProjectCalendarError::Calendar(source) => {
                provider_error(source)
            }
            _ => sync_error("saving Project Calendar event backups"),
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
