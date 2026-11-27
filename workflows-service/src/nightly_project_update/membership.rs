//! The `membership` tag: one Project's internal Slack channel and the
//! firm-side people in it.
//!
//! The channel is created, adopted when one of the expected name already
//! exists, or replaced when the recorded channel has been deleted. The
//! Project's Firm-side participants are invited using their Slack identities
//! or directory lookup. Channels are named `navigator-<code>`, or
//! `staging-<code>` on a simulated-matters deployment, and every call goes
//! through the owning Firm's own Slack bot (ENG-392).
//!
//! A closed or archived Project is not updated: nobody is invited and no
//! channel is created; its recorded channel is archived instead. Directory addresses stay inside the journaled step; only
//! counts leave it.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use store::application_store::ApplicationStore;
use uuid::Uuid;
use workflows::{SlackBot, SlackBotError};

use super::slack_error;

use crate::project_slack::{ensure_channel, ChannelOutcome};
use crate::workflow_step_error::WorkflowStepError;

/// What one Project's `membership` child did. Counts only.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct MembershipOutcome {
    pub channels: usize,
    pub projects_without_channel: usize,
    pub invited: usize,
    /// People removed because the Project no longer names them.
    #[serde(default)]
    pub removed: usize,
    pub already_members: usize,
    pub unmatched_assignments: usize,
    pub channels_created: usize,
    pub channels_repaired: usize,
    /// `1` when a closed or archived Project's channel was archived this run.
    pub channels_archived: usize,
    /// `1` when an open Project's archived channel was unarchived this run.
    #[serde(default)]
    pub channels_unarchived: usize,
}

/// Checkpoint-safe result of resolving a Project's channel. It deliberately
/// contains no membership email addresses or Slack member IDs.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ChannelResolution {
    pub open: bool,
    pub channel_id: Option<String>,
    pub outcome: MembershipOutcome,
}

/// Reconcile one Project's internal channel and its firm-side members.
pub async fn reconcile_project(
    application_store: &ApplicationStore,
    slack: &Arc<dyn SlackBot>,
    simulated: bool,
    project_id: Uuid,
) -> Result<MembershipOutcome, WorkflowStepError> {
    let resolution = resolve_channel(application_store, slack, simulated, project_id).await?;
    let mut report = resolution.outcome;
    if resolution.open {
        if let Some(channel_id) = resolution.channel_id {
            let invited = invite_members(application_store, slack, project_id, &channel_id).await?;
            report.invited += invited.invited;
            report.removed += invited.removed;
            report.already_members += invited.already_members;
            report.unmatched_assignments += invited.unmatched_assignments;
            report.channels += invited.channels;
        } else {
            report.projects_without_channel += 1;
        }
    }
    Ok(report)
}

/// Resolve/adopt/create the channel or archive a closed Project's channel.
/// Safe to repeat if the provider accepted its operation before the worker
/// checkpoint was written: Slack channel lookup and archive are idempotent.
pub async fn resolve_channel(
    application_store: &ApplicationStore,
    slack: &Arc<dyn SlackBot>,
    simulated: bool,
    project_id: Uuid,
) -> Result<ChannelResolution, WorkflowStepError> {
    let Some(target) = store::projects::slack_membership_target(application_store, project_id)
        .await
        .map_err(WorkflowStepError::retryable)?
    else {
        return Ok(ChannelResolution::default());
    };
    let mut report = MembershipOutcome::default();
    if target.status != "open" {
        if let Some(channel_id) = &target.channel_id {
            if slack
                .archive_channel(channel_id)
                .await
                .map_err(slack_error)?
            {
                report.channels_archived += 1;
                log_channel_archived(application_store, target.project_id, &target.status).await;
            }
        }
        return Ok(ChannelResolution {
            open: false,
            channel_id: None,
            outcome: report,
        });
    }
    let mut channel_id = target.channel_id.clone();

    // A recorded channel Slack no longer knows is replaced, not trusted.
    let mut stale = false;
    if let Some(id) = channel_id.as_deref() {
        match slack.channel_member_ids(id).await {
            Ok(_) => {
                // A reopened Project's channel comes back with its history.
                report.channels_unarchived += unarchive_open_channel(slack, id).await?;
                if report.channels_unarchived > 0 {
                    log_unarchived(application_store, target.project_id).await;
                }
            }
            Err(SlackBotError::Api(code)) if code == "channel_not_found" => stale = true,
            Err(error) => return Err(slack_error(error)),
        }
    }
    if stale || channel_id.is_none() {
        let mut project = store::projects::find_by_id(application_store, target.project_id)
            .await
            .map_err(WorkflowStepError::retryable)?
            .ok_or_else(|| {
                WorkflowStepError::terminal("Project disappeared during Slack membership sync")
            })?;
        project.internal_slack_channel_id = None;
        match ensure_channel(application_store, slack, &project, simulated)
            .await
            .map_err(WorkflowStepError::retryable)?
        {
            ChannelOutcome::Created(id) => {
                if stale {
                    report.channels_repaired += 1;
                } else {
                    report.channels_created += 1;
                }
                channel_id = Some(id);
            }
            ChannelOutcome::Adopted(id) => {
                report.channels_repaired += 1;
                channel_id = Some(id);
            }
            ChannelOutcome::Existing(id) => channel_id = Some(id),
        }
    }
    Ok(ChannelResolution {
        open: true,
        channel_id,
        outcome: report,
    })
}

/// Unarchive an open Project's recorded channel: `1` when this call did,
/// `0` when it was already active. Slack answers an active channel with a
/// steady-state refusal the bot adapter reads as `false`, so a nightly re-run
/// changes nothing.
async fn unarchive_open_channel(
    slack: &Arc<dyn SlackBot>,
    channel_id: &str,
) -> Result<usize, WorkflowStepError> {
    slack
        .unarchive_channel(channel_id)
        .await
        .map(usize::from)
        .map_err(slack_error)
}

/// Record the unarchive on the Project's Relationship Log. A log write that
/// fails is reported in the worker log and never undoes the change.
async fn log_unarchived(application_store: &ApplicationStore, project_id: Uuid) {
    let entry = store::relationship_logs::NewRelationshipLog {
        actor_person_id: None,
        subject_type: "project".to_string(),
        subject_id: project_id,
        action: "slack_channel_unarchived".to_string(),
        detail: "Project status open".to_string(),
        project_id: Some(project_id),
    };
    if let Err(error) = store::relationship_logs::record(application_store, &entry).await {
        tracing::warn!(%project_id, %error, "slack channel change not logged");
    }
}

/// Record the archive on the Project's Relationship Log, like
/// [`log_unarchived`].
async fn log_channel_archived(
    application_store: &ApplicationStore,
    project_id: Uuid,
    status: &str,
) {
    let entry = store::relationship_logs::NewRelationshipLog {
        actor_person_id: None,
        subject_type: "project".to_string(),
        subject_id: project_id,
        action: "slack_channel_archived".to_string(),
        detail: format!("Project status {status}"),
        project_id: Some(project_id),
    };
    if let Err(error) = store::relationship_logs::record(application_store, &entry).await {
        tracing::warn!(%project_id, %error, "slack channel change not logged");
    }
}

/// Look up the eligible identities only inside this operation and invite the
/// missing delta. The return value carries counts, never directory data.
pub async fn invite_members(
    application_store: &ApplicationStore,
    slack: &Arc<dyn SlackBot>,
    project_id: Uuid,
    channel_id: &str,
) -> Result<MembershipOutcome, WorkflowStepError> {
    let Some(target) = store::projects::slack_membership_target(application_store, project_id)
        .await
        .map_err(WorkflowStepError::retryable)?
    else {
        return Ok(MembershipOutcome::default());
    };
    if target.status != "open" || target.channel_id.as_deref() != Some(channel_id) {
        return Ok(MembershipOutcome::default());
    }
    let mut report = MembershipOutcome {
        channels: 1,
        ..Default::default()
    };
    let (user_ids, unmatched) = resolve_slack_ids(slack.as_ref(), &target).await?;
    report.unmatched_assignments += unmatched;
    let members: BTreeSet<String> = slack
        .channel_member_ids(channel_id)
        .await
        .map_err(slack_error)?
        .into_iter()
        .collect();
    let (present, missing): (Vec<String>, Vec<String>) = user_ids
        .iter()
        .cloned()
        .partition(|user_id| members.contains(user_id));
    report.already_members += present.len();
    slack
        .invite_to_channel(channel_id, &missing)
        .await
        .map_err(slack_error)?;
    report.invited += missing.len();
    // Removal only runs against a complete roster: one unresolved email (or an
    // empty roster) would otherwise read as "everyone else left" and empty the
    // channel. The next run retries once Slack matches every identity.
    if unmatched == 0 && !user_ids.is_empty() {
        for stale in members.difference(&user_ids) {
            if slack
                .remove_from_channel(channel_id, stale)
                .await
                .map_err(slack_error)?
            {
                report.removed += 1;
            }
        }
    }
    Ok(report)
}

/// The Slack ids a Project's channel should hold, and how many assigned
/// emails Slack could not match.
async fn resolve_slack_ids(
    slack: &dyn SlackBot,
    target: &store::projects::ProjectSlackMembershipTarget,
) -> Result<(BTreeSet<String>, usize), WorkflowStepError> {
    let mut user_ids: BTreeSet<String> = target.member_slack_ids.iter().cloned().collect();
    let mut unmatched = 0;
    for email in &target.member_emails {
        match slack.user_id_by_email(email).await.map_err(slack_error)? {
            Some(user_id) => {
                user_ids.insert(user_id);
            }
            None => unmatched += 1,
        }
    }
    Ok((user_ids, unmatched))
}

/// `<@U…>` mentions for the people a Project's channel should hold, so a post
/// notifies them even where they have muted the channel. Empty when the
/// Project has no membership target.
pub async fn member_mentions(
    application_store: &ApplicationStore,
    slack: &dyn SlackBot,
    project_id: Uuid,
) -> Result<String, WorkflowStepError> {
    let Some(target) = store::projects::slack_membership_target(application_store, project_id)
        .await
        .map_err(WorkflowStepError::retryable)?
    else {
        return Ok(String::new());
    };
    let (user_ids, _) = resolve_slack_ids(slack, &target).await?;
    Ok(user_ids
        .iter()
        .map(|user_id| format!("<@{user_id}>"))
        .collect::<Vec<_>>()
        .join(" "))
}

#[cfg(test)]
mod tests {
    use super::unarchive_open_channel;

    use async_trait::async_trait;

    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use workflows::{SlackBot, SlackBotError, SlackChannel, SlackMessageReceipt};

    #[derive(Default)]
    struct ReconciliationSlack {
        users: BTreeMap<String, Option<String>>,
        members: BTreeMap<String, Vec<String>>,
        lookups: Mutex<Vec<String>>,
        invitations: Mutex<Vec<(String, Vec<String>)>>,
        /// `(channel, user)` pairs removal was asked for, in order.
        removals: Mutex<Vec<(String, String)>>,
        /// Channel names created, in order.
        created: Mutex<Vec<String>>,
        /// Private channels that already exist, by name.
        existing: BTreeMap<String, String>,
        /// Channel ids Slack no longer knows.
        gone: Vec<String>,
        /// Channel ids archived, in order.
        archived: Mutex<Vec<String>>,
        /// Channel ids Slack holds archived until unarchive is asked for.
        archived_on_slack: Vec<String>,
        /// Channel ids unarchive was asked for, in order.
        unarchived: Mutex<Vec<String>>,
        lookup_error: bool,
        member_error: bool,
        invite_error: bool,
    }

    impl ReconciliationSlack {}

    #[async_trait]
    impl SlackBot for ReconciliationSlack {
        async fn create_private_channel(&self, name: &str) -> Result<SlackChannel, SlackBotError> {
            let mut created = self.created.lock().expect("created lock");
            created.push(name.to_string());
            Ok(SlackChannel {
                id: format!("CNEW{}", created.len()),
                name: name.to_string(),
            })
        }

        async fn find_private_channel_by_name(
            &self,
            name: &str,
        ) -> Result<Option<SlackChannel>, SlackBotError> {
            Ok(self.existing.get(name).map(|id| SlackChannel {
                id: id.clone(),
                name: name.to_string(),
            }))
        }

        async fn post_message(&self, _channel_id: &str, _text: &str) -> Result<(), SlackBotError> {
            Ok(())
        }

        async fn user_id_by_email(&self, email: &str) -> Result<Option<String>, SlackBotError> {
            self.lookups
                .lock()
                .expect("lookups lock")
                .push(email.to_string());
            if self.lookup_error {
                return Err(SlackBotError::RetryableHttpStatus(503));
            }
            Ok(self.users.get(email).cloned().flatten())
        }

        async fn channel_member_ids(&self, channel_id: &str) -> Result<Vec<String>, SlackBotError> {
            if self.gone.iter().any(|gone| gone == channel_id) {
                return Err(SlackBotError::Api("channel_not_found".to_string()));
            }
            if self.member_error {
                return Err(SlackBotError::RateLimited {
                    retry_after_seconds: Some(30),
                });
            }
            Ok(self.members.get(channel_id).cloned().unwrap_or_default())
        }

        async fn invite_to_channel(
            &self,
            channel_id: &str,
            user_ids: &[String],
        ) -> Result<(), SlackBotError> {
            if self.invite_error {
                return Err(SlackBotError::Api("invite_failed".to_string()));
            }
            self.invitations
                .lock()
                .expect("invitations lock")
                .push((channel_id.to_string(), user_ids.to_vec()));
            Ok(())
        }

        async fn remove_from_channel(
            &self,
            channel_id: &str,
            user_id: &str,
        ) -> Result<bool, SlackBotError> {
            self.removals
                .lock()
                .expect("removals lock")
                .push((channel_id.to_string(), user_id.to_string()));
            Ok(true)
        }

        async fn channel_canvas_id(&self, _: &str) -> Result<Option<String>, SlackBotError> {
            Ok(None)
        }

        async fn archive_channel(&self, channel_id: &str) -> Result<bool, SlackBotError> {
            self.archived
                .lock()
                .expect("archived lock")
                .push(channel_id.to_string());
            Ok(true)
        }

        async fn unarchive_channel(&self, channel_id: &str) -> Result<bool, SlackBotError> {
            let mut unarchived = self.unarchived.lock().expect("unarchived lock");
            // Only a channel Slack holds archived changes, and only once; a
            // re-run finds it active, the steady state.
            let changes = self.archived_on_slack.iter().any(|id| id == channel_id)
                && !unarchived.iter().any(|id| id == channel_id);
            unarchived.push(channel_id.to_string());
            Ok(changes)
        }

        async fn create_channel_canvas(&self, _: &str, _: &str) -> Result<String, SlackBotError> {
            Ok("FCANVAS".to_string())
        }

        async fn post_message_with_receipt(
            &self,
            channel_id: &str,
            _text: &str,
            correlation_id: Option<&str>,
        ) -> Result<SlackMessageReceipt, SlackBotError> {
            Ok(SlackMessageReceipt {
                channel_id: channel_id.to_string(),
                timestamp: String::new(),
                correlation_id: correlation_id.map(str::to_string),
            })
        }
    }

    /// A reopened Project's archived channel is unarchived once, and the
    /// nightly re-run finds it active and changes nothing.
    #[tokio::test]
    async fn an_open_projects_archived_channel_is_unarchived_once() {
        let slack = Arc::new(ReconciliationSlack {
            archived_on_slack: vec!["C1".to_string()],
            ..ReconciliationSlack::default()
        });
        let bot: Arc<dyn SlackBot> = slack.clone();
        assert_eq!(unarchive_open_channel(&bot, "C1").await.unwrap(), 1);
        assert_eq!(unarchive_open_channel(&bot, "C1").await.unwrap(), 0);
        assert_eq!(
            *slack.unarchived.lock().unwrap(),
            vec!["C1".to_string(), "C1".to_string()]
        );
    }
}
