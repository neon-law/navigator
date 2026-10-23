//! The `NotationReset` Restate workflow: the daily reset that deletes every
//! Notation draft on a deployment and says so in Slack.
//!
//! Keyed on the UTC date, so a second fire on the same day attaches to the
//! first; the Cron schedules console starts it on demand under a
//! `manual-<uuid>` key. The draft predicate is [`store::notation_reset`]'s
//! and is never restated here: this workflow discovers ids, deletes each one
//! in its own journaled step, and reports counts.
//!
//! Every run posts exactly one message to the deployment's internal
//! messages channel — `#messages` on a real deployment, `#staging-messages`
//! on a simulated-matters one — including a run that deleted nothing and a
//! run that failed before it could delete anything. A missing post therefore
//! means the job did not run. The post names the environment, the counts,
//! the duration, and the ids that failed to delete; never a client, a
//! matter, or a Project code.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use store::surreal::SurrealDb;
use uuid::Uuid;
use workflows::SlackBot;

pub use workflows::notation_reset::{
    environment_label, render_failure, render_report, run_key, DraftRoster, NotationResetReport,
};

use crate::nightly_project_update::{slack_error, tag_retry_policy};

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct RunRequest {}

#[derive(Clone)]
pub struct NotationResetService {
    surreal: SurrealDb,
    slack: Arc<dyn SlackBot>,
    messages_channel_id: String,
    staging_messages_channel_id: String,
    environment: &'static str,
    simulated: bool,
    slack_enabled: bool,
}

impl NotationResetService {
    #[must_use]
    pub fn new(
        surreal: SurrealDb,
        slack: Arc<dyn SlackBot>,
        messages_channel_id: String,
        staging_messages_channel_id: String,
        environment: &'static str,
        simulated: bool,
        slack_enabled: bool,
    ) -> Self {
        Self {
            surreal,
            slack,
            messages_channel_id,
            staging_messages_channel_id,
            environment,
            simulated,
            slack_enabled,
        }
    }

    fn channel_id(&self) -> &str {
        workflows::notation_reset::channel_id(
            self.simulated,
            &self.messages_channel_id,
            &self.staging_messages_channel_id,
        )
    }
}

#[restate_sdk::workflow(name = "NotationReset")]
impl NotationResetService {
    #[restate_sdk::handler]
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        _request: Json<RunRequest>,
    ) -> Result<Json<NotationResetReport>, HandlerError> {
        let channel_id = self.channel_id().to_string();
        validate_configuration(&channel_id, self.slack_enabled)?;

        let started_at: String = ctx
            .run(|| async { Ok(Json(Utc::now().to_rfc3339())) })
            .name("read-clock")
            .await?
            .into_inner();
        let started = parse_clock(&started_at)?;
        tracing::info!(
            run_key = ctx.key(),
            environment = self.environment,
            "notation reset started"
        );

        let surreal = self.surreal.clone();
        let roster = ctx
            .run(move || async move { discover_drafts(&surreal).await.map(Json) })
            .name("discover-drafts")
            .retry_policy(tag_retry_policy())
            .await;
        let roster = match roster {
            Ok(Json(roster)) => roster,
            Err(error) => {
                let elapsed = workflows::notation_reset::elapsed_ms(
                    started,
                    self.clock(&ctx, "read-clock-failed").await?,
                );
                let message = render_failure(self.environment, elapsed);
                self.post(&ctx, "post-failure", channel_id, message).await?;
                return Err(error.into());
            }
        };

        let mut report = NotationResetReport::default();
        self.delete_notations(&ctx, &roster.notations, &mut report)
            .await;
        self.delete_previews(&ctx, &roster.previews, &mut report)
            .await;

        report.duration_ms = workflows::notation_reset::elapsed_ms(
            started,
            self.clock(&ctx, "read-clock-end").await?,
        );
        let message = render_report(self.environment, &report);
        self.post(&ctx, "post-outcome", channel_id, message).await?;
        tracing::info!(
            run_key = ctx.key(),
            notations_deleted = report.notations_deleted,
            previews_deleted = report.previews_deleted,
            kept = report.kept,
            failures = report.failures.len(),
            duration_ms = report.duration_ms,
            "notation reset complete"
        );
        Ok(Json(report))
    }
}

impl NotationResetService {
    /// One journaled delete per Notation draft, each with a bounded retry; a
    /// failure is recorded by id and the run moves on.
    async fn delete_notations(
        &self,
        ctx: &WorkflowContext<'_>,
        ids: &[Uuid],
        report: &mut NotationResetReport,
    ) {
        for id in ids {
            let surreal = self.surreal.clone();
            let notation_id = *id;
            let outcome = ctx
                .run(move || async move {
                    store::notation_reset::delete_draft(&surreal, notation_id)
                        .await
                        .map(Json)
                        .map_err(|error| HandlerError::from(TerminalError::new(error.to_string())))
                })
                .name(format!("delete-notation-{id}"))
                .retry_policy(tag_retry_policy())
                .await;
            match outcome {
                Ok(Json(outcome)) => report.absorb(outcome),
                Err(error) => {
                    tracing::warn!(notation_id = %id, %error, "notation draft delete failed");
                    report.failures.push(*id);
                }
            }
        }
    }

    /// One journaled delete per stored preview draft.
    async fn delete_previews(
        &self,
        ctx: &WorkflowContext<'_>,
        ids: &[Uuid],
        report: &mut NotationResetReport,
    ) {
        for id in ids {
            let surreal = self.surreal.clone();
            let preview_id = *id;
            let outcome = ctx
                .run(move || async move {
                    store::notation_reset::delete_preview_draft(&surreal, preview_id)
                        .await
                        .map_err(|error| HandlerError::from(TerminalError::new(error.to_string())))
                })
                .name(format!("delete-preview-{id}"))
                .retry_policy(tag_retry_policy())
                .await;
            match outcome {
                Ok(()) => report.previews_deleted += 1,
                Err(error) => {
                    tracing::warn!(draft_id = %id, %error, "preview draft delete failed");
                    report.failures.push(*id);
                }
            }
        }
    }

    /// The run's one Slack post, journaled under `name`.
    async fn post(
        &self,
        ctx: &WorkflowContext<'_>,
        name: &str,
        channel_id: String,
        message: String,
    ) -> Result<(), HandlerError> {
        let slack = Arc::clone(&self.slack);
        ctx.run(move || async move {
            slack
                .post_message(&channel_id, &message)
                .await
                .map_err(slack_error)
        })
        .name(name)
        .await?;
        Ok(())
    }

    async fn clock(
        &self,
        ctx: &WorkflowContext<'_>,
        name: &str,
    ) -> Result<DateTime<Utc>, HandlerError> {
        let at: String = ctx
            .run(|| async { Ok(Json(Utc::now().to_rfc3339())) })
            .name(name)
            .await?
            .into_inner();
        parse_clock(&at)
    }
}

fn parse_clock(value: &str) -> Result<DateTime<Utc>, HandlerError> {
    DateTime::parse_from_rfc3339(value)
        .map(|at| at.with_timezone(&Utc))
        .map_err(|_| TerminalError::new("the journaled clock is not RFC 3339").into())
}

fn validate_configuration(channel_id: &str, slack_enabled: bool) -> Result<(), HandlerError> {
    if channel_id.is_empty() {
        return Err(TerminalError::new(
            "messages channel id for this deployment is not configured",
        )
        .into());
    }
    if !slack_enabled {
        return Err(TerminalError::new(
            "SLACK_BOT_TOKEN must be configured for the notation reset",
        )
        .into());
    }
    Ok(())
}

/// Every draft id on the deployment, Notations and stored previews.
pub async fn discover_drafts(surreal: &SurrealDb) -> Result<DraftRoster, HandlerError> {
    workflows::notation_reset::discover_drafts(surreal)
        .await
        .map_err(|error| HandlerError::from(TerminalError::new(error.to_string())))
}

/// The production composition, minus Restate: discover, delete each draft,
/// and post — so a test proves the rerun and the post end to end.
#[cfg(test)]
pub(crate) async fn reset_in_process(
    surreal: &SurrealDb,
    slack: &Arc<dyn SlackBot>,
    channel_id: &str,
    environment: &str,
) -> NotationResetReport {
    let roster = discover_drafts(surreal).await.expect("discover drafts");
    let mut report = NotationResetReport::default();
    for id in roster.notations {
        match store::notation_reset::delete_draft(surreal, id).await {
            Ok(outcome) => report.absorb(outcome),
            Err(_) => report.failures.push(id),
        }
    }
    for id in roster.previews {
        match store::notation_reset::delete_preview_draft(surreal, id).await {
            Ok(()) => report.previews_deleted += 1,
            Err(_) => report.failures.push(id),
        }
    }
    slack
        .post_message(channel_id, &render_report(environment, &report))
        .await
        .expect("post outcome");
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::test_support::{mem_surreal, seed_notation};

    #[test]
    fn configuration_requires_a_channel_and_an_enabled_bot() {
        assert!(validate_configuration("C-MESSAGES", true).is_ok());
        assert!(validate_configuration("", true).is_err());
        assert!(validate_configuration("C-MESSAGES", false).is_err());
    }

    #[tokio::test]
    async fn a_rerun_deletes_nothing_and_still_posts() {
        let surreal = mem_surreal().await;
        let draft = seed_notation(&surreal).await;
        let slack = Arc::new(workflows::CapturingSlackBot::new());
        let bot: Arc<dyn SlackBot> = slack.clone();

        let first = reset_in_process(&surreal, &bot, "C-STAGING", "staging").await;
        assert_eq!(first.notations_deleted, 1);
        assert!(first.failures.is_empty());
        assert!(store::notations::find_by_id(&surreal, draft)
            .await
            .unwrap()
            .is_none());

        let second = reset_in_process(&surreal, &bot, "C-STAGING", "staging").await;
        assert_eq!(
            (
                second.notations_deleted,
                second.previews_deleted,
                second.kept
            ),
            (0, 0, 0),
            "nothing is left to delete"
        );
        let posted = slack.posted_messages();
        assert_eq!(posted.len(), 2, "every run posts, the zero-count one too");
        assert!(posted.iter().all(|(channel, _)| channel == "C-STAGING"));
        assert!(
            posted[1].1.contains("Drafts deleted: 0 notations"),
            "{}",
            posted[1].1
        );
    }

    #[tokio::test]
    async fn a_lawyer_review_notation_survives_the_reset() {
        let surreal = mem_surreal().await;
        let reviewed = seed_notation(&surreal).await;
        store::notations::update_state(&surreal, reviewed, "lawyer_review__pending")
            .await
            .unwrap();
        let draft = seed_notation(&surreal).await;
        let bot: Arc<dyn SlackBot> = Arc::new(workflows::CapturingSlackBot::new());

        let report = reset_in_process(&surreal, &bot, "C-MESSAGES", "production").await;
        assert_eq!(report.notations_deleted, 1);
        assert!(store::notations::find_by_id(&surreal, reviewed)
            .await
            .unwrap()
            .is_some());
        assert!(store::notations::find_by_id(&surreal, draft)
            .await
            .unwrap()
            .is_none());
    }

    #[test]
    fn the_daily_trigger_runs_at_0420_utc_in_every_manifest() {
        for source in [
            include_str!("../../examples/deploy/k8s/exports/cron-notation-reset-trigger.yaml"),
            include_str!(
                "../../k8s/overlays/kind/notation-reset-trigger/notation-reset-trigger.yaml"
            ),
        ] {
            let manifest: serde_yaml::Value = serde_yaml::from_str(source).unwrap();
            assert_eq!(manifest["spec"]["schedule"].as_str(), Some("20 4 * * *"));
            assert_eq!(manifest["spec"]["timeZone"].as_str(), Some("Etc/UTC"));
            assert_eq!(
                manifest["metadata"]["name"].as_str(),
                Some("notation-reset-trigger")
            );
        }
    }

    #[test]
    fn the_trigger_manifest_is_listed_in_every_kustomization() {
        let listed = |value: &serde_yaml::Value, entry: &str| {
            value["resources"]
                .as_sequence()
                .unwrap()
                .iter()
                .any(|resource| resource.as_str() == Some(entry))
        };
        let exports: serde_yaml::Value = serde_yaml::from_str(include_str!(
            "../../examples/deploy/k8s/exports/kustomization.yaml"
        ))
        .unwrap();
        assert!(listed(&exports, "cron-notation-reset-trigger.yaml"));
        let kind: serde_yaml::Value =
            serde_yaml::from_str(include_str!("../../k8s/overlays/kind/kustomization.yaml"))
                .unwrap();
        assert!(listed(&kind, "notation-reset-trigger"));
        let overlay: serde_yaml::Value = serde_yaml::from_str(include_str!(
            "../../k8s/overlays/kind/notation-reset-trigger/kustomization.yaml"
        ))
        .unwrap();
        assert!(listed(&overlay, "notation-reset-trigger.yaml"));
    }

    #[test]
    fn the_trigger_starts_the_workflow_keyed_on_the_utc_date() {
        let source = include_str!("bin/notation-reset-trigger.rs");
        assert!(source.contains("\"NotationReset\""));
        assert!(source.contains("notation_reset::run_key(chrono::Utc::now())"));
    }
}
