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
