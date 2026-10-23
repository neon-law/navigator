//! The `membership` tag: one Project's internal Slack channel and the
//! firm-side people in it.
//!
//! The channel is created, adopted when one of the expected name already
//! exists, or replaced when the recorded channel has been deleted; the Lawyer
//! DRI, participating Clerks, and owning-Firm Lawyers and Clerks with recorded
//! Slack identities are invited. Channels are named `project-<code>`, or
//! `staging-<code>` on a simulated-matters deployment.
//!
//! A closed or archived Project is not updated: nobody is invited and no
//! channel is created; its recorded channel is archived instead. Directory addresses stay inside the journaled step; only
//! counts leave it.

use std::collections::BTreeSet;
use std::sync::Arc;

use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use store::surreal::SurrealDb;
use uuid::Uuid;
use workflows::{SlackBot, SlackBotError};

use super::{slack_error, tag_retry_policy, ProjectTagRequest};
use crate::project_slack::{ensure_channel, ChannelOutcome};

/// What one Project's `membership` child did. Counts only.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct MembershipOutcome {
    pub channels: usize,
    pub projects_without_channel: usize,
    pub invited: usize,
    pub already_members: usize,
    pub unmatched_assignments: usize,
    pub channels_created: usize,
    pub channels_repaired: usize,
    /// `1` when a closed or archived Project's channel was archived this run.
    pub channels_archived: usize,
}

/// Checkpoint-safe result of resolving a Project's channel. It deliberately
/// contains no membership email addresses or Slack member IDs.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ChannelResolution {
    pub open: bool,
    pub channel_id: Option<String>,
    pub outcome: MembershipOutcome,
}

#[derive(Clone)]
pub struct MembershipTagService {
    surreal: SurrealDb,
    slack: Arc<dyn SlackBot>,
    simulated: bool,
}

impl MembershipTagService {
    #[must_use]
    pub fn new(surreal: SurrealDb, slack: Arc<dyn SlackBot>, simulated: bool) -> Self {
        Self {
            surreal,
            slack,
            simulated,
        }
    }
}

#[restate_sdk::workflow(name = "NightlyProjectUpdateMembership")]
impl MembershipTagService {
    #[restate_sdk::handler]
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        request: Json<ProjectTagRequest>,
    ) -> Result<Json<MembershipOutcome>, HandlerError> {
        let project_id = request.into_inner().project_id;
        let surreal = self.surreal.clone();
        let slack = Arc::clone(&self.slack);
        let simulated = self.simulated;
        // The directory addresses stay inside this journaled operation. Only
        // counts enter the Restate journal.
        let outcome = ctx
            .run(move || async move {
                reconcile_project(&surreal, &slack, simulated, project_id)
                    .await
                    .map(Json)
            })
            .name("reconcile-membership")
            .retry_policy(tag_retry_policy())
            .await?;
        Ok(outcome)
    }
}

/// Reconcile one Project's internal channel and its firm-side members.
pub async fn reconcile_project(
    surreal: &SurrealDb,
    slack: &Arc<dyn SlackBot>,
    simulated: bool,
    project_id: Uuid,
) -> Result<MembershipOutcome, HandlerError> {
    let resolution = resolve_channel(surreal, slack, simulated, project_id).await?;
    let mut report = resolution.outcome;
    if resolution.open {
        if let Some(channel_id) = resolution.channel_id {
            let invited = invite_members(surreal, slack, project_id, &channel_id).await?;
            report.invited += invited.invited;
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
    surreal: &SurrealDb,
    slack: &Arc<dyn SlackBot>,
    simulated: bool,
    project_id: Uuid,
) -> Result<ChannelResolution, HandlerError> {
    let Some(target) = store::projects::slack_membership_target(surreal, project_id)
        .await
        .map_err(HandlerError::from)?
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
            Ok(_) => (),
            Err(SlackBotError::Api(code)) if code == "channel_not_found" => stale = true,
            Err(error) => return Err(slack_error(error)),
        }
    }
    if stale || channel_id.is_none() {
        let mut project = store::projects::find_by_id(surreal, target.project_id)
            .await
            .map_err(HandlerError::from)?
            .ok_or_else(|| {
                HandlerError::from(TerminalError::new(
                    "Project disappeared during Slack membership sync",
                ))
            })?;
        project.internal_slack_channel_id = None;
        match ensure_channel(surreal, slack, &project, simulated).await? {
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

/// Look up the eligible identities only inside this operation and invite the
/// missing delta. The return value carries counts, never directory data.
pub async fn invite_members(
    surreal: &SurrealDb,
    slack: &Arc<dyn SlackBot>,
    project_id: Uuid,
    channel_id: &str,
) -> Result<MembershipOutcome, HandlerError> {
    let Some(target) = store::projects::slack_membership_target(surreal, project_id)
        .await
        .map_err(HandlerError::from)?
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
    let mut user_ids: BTreeSet<String> = target.member_slack_ids.into_iter().collect();
    for email in &target.member_emails {
        match slack.user_id_by_email(email).await.map_err(slack_error)? {
            Some(user_id) => {
                user_ids.insert(user_id);
            }
            None => report.unmatched_assignments += 1,
        }
    }
    let members: BTreeSet<String> = slack
        .channel_member_ids(channel_id)
        .await
        .map_err(slack_error)?
        .into_iter()
        .collect();
    let (present, missing): (Vec<String>, Vec<String>) = user_ids
        .into_iter()
        .partition(|user_id| members.contains(user_id));
    report.already_members += present.len();
    slack
        .invite_to_channel(channel_id, &missing)
        .await
        .map_err(slack_error)?;
    report.invited += missing.len();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::{invite_members, reconcile_project, resolve_channel, MembershipOutcome};
    use crate::nightly_project_update::{sweep_in_process, NightlyProjectUpdateReport};
    use async_trait::async_trait;
    use cloud::forge::{FakeForge, ForgeService};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use store::persons::{self, NewPerson, Role};
    use store::projects::{
        self, create, designate_dri_in_surreal, set_internal_slack_channel_id, DriSide, NewProject,
    };
    use store::test_support::{mem_surreal, seed_entity};
    use workflows::{SlackBot, SlackBotError, SlackChannel, SlackMessageReceipt};

    #[derive(Default)]
    struct ReconciliationSlack {
        users: BTreeMap<String, Option<String>>,
        members: BTreeMap<String, Vec<String>>,
        lookups: Mutex<Vec<String>>,
        invitations: Mutex<Vec<(String, Vec<String>)>>,
        /// Channel names created, in order.
        created: Mutex<Vec<String>>,
        /// Private channels that already exist, by name.
        existing: BTreeMap<String, String>,
        /// Channel ids Slack no longer knows.
        gone: Vec<String>,
        /// Channel ids archived, in order.
        archived: Mutex<Vec<String>>,
        lookup_error: bool,
        member_error: bool,
        invite_error: bool,
    }

    impl ReconciliationSlack {
        fn new(
            users: impl IntoIterator<Item = (String, Option<String>)>,
            members: impl IntoIterator<Item = (String, Vec<String>)>,
        ) -> Self {
            Self {
                users: users.into_iter().collect(),
                members: members.into_iter().collect(),
                ..Self::default()
            }
        }
    }

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

    async fn project(surreal: &store::surreal::SurrealDb, code: &str) -> store::projects::Project {
        create(
            surreal,
            &NewProject {
                code: code.to_string(),
                name: code.to_string(),
                status: "open".to_string(),
                entity_id: seed_entity(surreal).await,
                ..Default::default()
            },
        )
        .await
        .expect("create Project")
    }

    async fn person(
        surreal: &store::surreal::SurrealDb,
        name: &str,
        email: &str,
        role: Role,
    ) -> store::persons::Person {
        persons::create(surreal, &NewPerson::with_role(name, email, role))
            .await
            .expect("create Person")
    }

    #[tokio::test]
    async fn recorded_identity_bypasses_email_lookup() {
        let db = mem_surreal().await;
        let target = project(&db, "recorded-identity").await;
        set_internal_slack_channel_id(&db, target.id, "CIDENTITY")
            .await
            .unwrap();
        let clerk = person(&db, "Clerk", "clerk@example.com", Role::Clerk).await;
        projects::add_participation(&db, target.id, clerk.id, "clerk")
            .await
            .unwrap();
        store::external_identities::link(
            &db,
            clerk.id,
            store::external_identities::ExternalSystem::Slack,
            "U-RECORDED",
            None,
        )
        .await
        .unwrap();
        let slack = Arc::new(ReconciliationSlack {
            lookup_error: true,
            ..Default::default()
        });
        let bot: Arc<dyn SlackBot> = slack.clone();
        let report = sweep_in_process(&db, &bot, None, false).await;
        assert_eq!((report.invited, report.membership_failures), (1, 0));
        assert!(slack.lookups.lock().unwrap().is_empty());
        assert_eq!(
            *slack.invitations.lock().unwrap(),
            [("CIDENTITY".into(), vec!["U-RECORDED".into()])]
        );
    }

    #[tokio::test]
    async fn reconciliation_invites_missing_lawyer_and_clerks_only() {
        let surreal = mem_surreal().await;
        let configured = project(&surreal, "sync-configured").await;
        set_internal_slack_channel_id(&surreal, configured.id, "CTEAM")
            .await
            .expect("set channel")
            .expect("Project exists");
        let empty = project(&surreal, "sync-empty").await;
        set_internal_slack_channel_id(&surreal, empty.id, "CEMPTY")
            .await
            .expect("set channel")
            .expect("Project exists");
        let _without_channel = project(&surreal, "sync-no-channel").await;

        let lawyer = person(&surreal, "Lawyer DRI", "lawyer@example.com", Role::Lawyer).await;
        let clerk_one = person(&surreal, "Clerk One", "clerk-one@example.com", Role::Clerk).await;
        let clerk_two = person(&surreal, "Clerk Two", "clerk-two@example.com", Role::Clerk).await;
        let unmatched = person(
            &surreal,
            "Clerk Three",
            "clerk-three@example.com",
            Role::Clerk,
        )
        .await;
        let client = person(&surreal, "Client", "client@example.com", Role::Client).await;
        for (person, participation) in [
            (&lawyer, "lawyer"),
            (&clerk_one, "clerk"),
            (&clerk_two, "clerk"),
            (&unmatched, "clerk"),
            (&client, "client"),
        ] {
            projects::add_participation(&surreal, configured.id, person.id, participation)
                .await
                .expect("add participation");
        }
        designate_dri_in_surreal(&surreal, configured.id, lawyer.id, DriSide::Lawyer)
            .await
            .expect("designate Lawyer DRI");

        let slack = Arc::new(ReconciliationSlack::new(
            [
                ("lawyer@example.com".to_string(), Some("U-NEW".to_string())),
                (
                    "clerk-one@example.com".to_string(),
                    Some("U-OLD".to_string()),
                ),
                (
                    "clerk-two@example.com".to_string(),
                    Some("U-OLD".to_string()),
                ),
                ("clerk-three@example.com".to_string(), None),
            ],
            [("CTEAM".to_string(), vec!["U-OLD".to_string()])],
        ));
        let slack_bot: Arc<dyn SlackBot> = slack.clone();

        let report = sweep_in_process(&surreal, &slack_bot, None, false).await;
        assert_eq!(
            report,
            NightlyProjectUpdateReport {
                projects: 3,
                open_projects: 3,
                channels: 3,
                projects_without_channel: 0,
                invited: 1,
                already_members: 1,
                unmatched_assignments: 1,
                channels_created: 1,
                channels_repaired: 0,
                canvases_created: 3,
                notices_posted: 3,
                ..Default::default()
            }
        );
        assert_eq!(
            *slack.created.lock().expect("created lock"),
            ["project-sync-no-channel"]
        );
        assert_eq!(
            *slack.lookups.lock().expect("lookups lock"),
            [
                "clerk-one@example.com",
                "clerk-three@example.com",
                "clerk-two@example.com",
                "lawyer@example.com"
            ]
        );
        assert!(!slack
            .lookups
            .lock()
            .expect("lookups lock")
            .contains(&"client@example.com".to_string()));
        assert_eq!(
            *slack.invitations.lock().expect("invitations lock"),
            [
                ("CTEAM".to_string(), vec!["U-NEW".to_string()]),
                ("CEMPTY".to_string(), Vec::new()),
                ("CNEW1".to_string(), Vec::new())
            ]
        );
    }

    #[tokio::test]
    async fn reconciliation_returns_provider_failures_without_email_addresses() {
        let surreal = mem_surreal().await;
        let configured = project(&surreal, "sync-error").await;
        set_internal_slack_channel_id(&surreal, configured.id, "CERROR")
            .await
            .expect("set channel")
            .expect("Project exists");
        let clerk = person(&surreal, "Clerk", "private@example.com", Role::Clerk).await;
        projects::add_participation(&surreal, configured.id, clerk.id, "clerk")
            .await
            .expect("add Clerk");

        let slack = Arc::new(ReconciliationSlack {
            lookup_error: true,
            ..ReconciliationSlack::default()
        });
        let slack_bot: Arc<dyn SlackBot> = slack;
        let error = reconcile_project(&surreal, &slack_bot, false, configured.id)
            .await
            .expect_err("provider lookup failure");
        assert!(format!("{error:?}").contains("503"));
        assert!(!format!("{error:?}").contains("private@example.com"));

        let report = sweep_in_process(&surreal, &slack_bot, None, false).await;
        assert_eq!(report.membership_failures, 1, "counted, not fatal");
    }

    #[tokio::test]
    async fn an_unknown_project_is_left_alone() {
        let surreal = mem_surreal().await;
        let slack = Arc::new(ReconciliationSlack::default());
        let bot: Arc<dyn SlackBot> = slack.clone();
        let missing = uuid::Uuid::now_v7();
        assert_eq!(
            reconcile_project(&surreal, &bot, false, missing)
                .await
                .unwrap(),
            MembershipOutcome::default()
        );
        assert_eq!(
            resolve_channel(&surreal, &bot, false, missing)
                .await
                .unwrap(),
            super::ChannelResolution::default()
        );
        assert_eq!(
            invite_members(&surreal, &bot, missing, "CANY")
                .await
                .unwrap(),
            MembershipOutcome::default()
        );
        assert!(slack.created.lock().unwrap().is_empty());
        assert!(slack.invitations.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn invitations_follow_only_the_channel_recorded_for_the_project() {
        let surreal = mem_surreal().await;
        let configured = project(&surreal, "sync-replayed-channel").await;
        set_internal_slack_channel_id(&surreal, configured.id, "CRECORDED")
            .await
            .expect("set channel")
            .expect("Project exists");
        let clerk = person(&surreal, "Clerk", "clerk@example.com", Role::Clerk).await;
        projects::add_participation(&surreal, configured.id, clerk.id, "clerk")
            .await
            .expect("add Clerk");
        let slack = Arc::new(ReconciliationSlack::new(
            [("clerk@example.com".to_string(), Some("U-CLERK".to_string()))],
            [],
        ));
        let bot: Arc<dyn SlackBot> = slack.clone();
        // A checkpointed channel id that no longer matches the record (the
        // channel was replaced since) invites nobody.
        assert_eq!(
            invite_members(&surreal, &bot, configured.id, "CSUPERSEDED")
                .await
                .unwrap(),
            MembershipOutcome::default()
        );
        assert!(slack.invitations.lock().unwrap().is_empty());
        let invited = invite_members(&surreal, &bot, configured.id, "CRECORDED")
            .await
            .unwrap();
        assert_eq!((invited.channels, invited.invited), (1, 1));
    }

    #[tokio::test]
    async fn reconciliation_stops_on_member_listing_and_invitation_failures() {
        let surreal = mem_surreal().await;
        let configured = project(&surreal, "sync-provider-errors").await;
        set_internal_slack_channel_id(&surreal, configured.id, "CPROVIDERERROR")
            .await
            .expect("set channel")
            .expect("Project exists");
        let clerk = person(&surreal, "Clerk", "clerk@example.com", Role::Clerk).await;
        projects::add_participation(&surreal, configured.id, clerk.id, "clerk")
            .await
            .expect("add Clerk");

        let lookup = [("clerk@example.com".to_string(), Some("U-CLERK".to_string()))];
        for slack in [
            ReconciliationSlack {
                users: lookup.iter().cloned().collect(),
                member_error: true,
                ..ReconciliationSlack::default()
            },
            ReconciliationSlack {
                users: lookup.iter().cloned().collect(),
                invite_error: true,
                ..ReconciliationSlack::default()
            },
        ] {
            let slack = Arc::new(slack);
            let slack_bot: Arc<dyn SlackBot> = slack;
            assert!(
                reconcile_project(&surreal, &slack_bot, false, configured.id)
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn staging_creates_prefixed_channels_and_adopts_existing_ones() {
        let surreal = mem_surreal().await;
        let fresh = project(&surreal, "sample-fresh").await;
        let existing = project(&surreal, "sample-existing").await;

        let slack = Arc::new(ReconciliationSlack {
            existing: [("staging-sample-existing".to_string(), "GEXIST".to_string())]
                .into_iter()
                .collect(),
            ..ReconciliationSlack::default()
        });
        let slack_bot: Arc<dyn SlackBot> = slack.clone();
        let report = sweep_in_process(&surreal, &slack_bot, None, true).await;

        assert_eq!(report.channels_created, 1);
        assert_eq!(report.channels_repaired, 1);
        assert_eq!(report.projects_without_channel, 0);
        assert_eq!(
            *slack.created.lock().expect("created lock"),
            ["staging-sample-fresh"]
        );
        for (id, expected) in [(fresh.id, Some("CNEW1")), (existing.id, Some("GEXIST"))] {
            let stored = projects::find_by_id(&surreal, id)
                .await
                .expect("read")
                .expect("exists");
            assert_eq!(stored.internal_slack_channel_id.as_deref(), expected);
        }
    }

    #[tokio::test]
    async fn a_deleted_channel_is_replaced_and_closed_projects_are_not_given_one() {
        let surreal = mem_surreal().await;
        let stale = project(&surreal, "sample-stale").await;
        set_internal_slack_channel_id(&surreal, stale.id, "CGONE")
            .await
            .expect("set channel")
            .expect("exists");
        let closed = create(
            &surreal,
            &store::projects::NewProject {
                code: "sample-closed".to_string(),
                name: "Closed".to_string(),
                status: "closed".to_string(),
                entity_id: seed_entity(&surreal).await,
                ..Default::default()
            },
        )
        .await
        .expect("create closed Project");

        let slack = Arc::new(ReconciliationSlack {
            gone: vec!["CGONE".to_string()],
            ..ReconciliationSlack::default()
        });
        let slack_bot: Arc<dyn SlackBot> = slack.clone();
        let forge = FakeForge::new();
        let report = sweep_in_process(&surreal, &slack_bot, Some(&forge), false).await;

        assert_eq!(report.channels_repaired, 1);
        assert_eq!(report.channels_created, 0);
        assert_eq!(
            report.projects_without_channel, 0,
            "closed Projects are not counted"
        );
        assert_eq!(report.channels_archived, 0, "no channel to archive");
        assert_eq!(
            *slack.created.lock().expect("created lock"),
            ["project-sample-stale"]
        );
        let stored = projects::find_by_id(&surreal, stale.id)
            .await
            .expect("read")
            .expect("exists");
        assert_eq!(stored.internal_slack_channel_id.as_deref(), Some("CNEW1"));
        let closed = projects::find_by_id(&surreal, closed.id)
            .await
            .expect("read")
            .expect("exists");
        assert_eq!(closed.internal_slack_channel_id, None);
        assert!(forge
            .find_repository("sample-closed")
            .await
            .expect("find")
            .is_none());
    }

    /// A closed Project is not updated: its recorded channel is archived and
    /// nobody is invited to it, even a participant missing from it.
    #[tokio::test]
    async fn a_closed_projects_channel_is_archived_and_nobody_is_invited() {
        let surreal = mem_surreal().await;
        let closed = create(
            &surreal,
            &store::projects::NewProject {
                code: "sample-closed".to_string(),
                name: "Closed".to_string(),
                status: "closed".to_string(),
                entity_id: seed_entity(&surreal).await,
                ..Default::default()
            },
        )
        .await
        .expect("create closed Project");
        set_internal_slack_channel_id(&surreal, closed.id, "CCLOSED")
            .await
            .expect("set channel")
            .expect("exists");
        let lawyer = person(&surreal, "Lawyer", "lawyer@example.com", Role::Lawyer).await;
        projects::add_participation(&surreal, closed.id, lawyer.id, "lawyer")
            .await
            .expect("add participation");
        let slack = Arc::new(ReconciliationSlack::new(
            [(
                "lawyer@example.com".to_string(),
                Some("U-LAWYER".to_string()),
            )],
            [],
        ));
        let slack_bot: Arc<dyn SlackBot> = slack.clone();

        let report = sweep_in_process(&surreal, &slack_bot, None, false).await;

        assert_eq!(report.channels_archived, 1);
        assert_eq!(report.membership_failures, 0);
        assert_eq!(*slack.archived.lock().unwrap(), ["CCLOSED"]);
        assert!(slack.invitations.lock().unwrap().is_empty());
        assert!(slack.lookups.lock().unwrap().is_empty());
        assert!(slack.created.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn open_projects_get_a_repository_once() {
        let surreal = mem_surreal().await;
        let open = project(&surreal, "sample-repo").await;
        let slack_bot: Arc<dyn SlackBot> = Arc::new(ReconciliationSlack::default());
        let forge = FakeForge::new();

        let first = sweep_in_process(&surreal, &slack_bot, Some(&forge), false).await;
        assert_eq!(first.repositories_synced, 1);
        assert_eq!(first.repository_failures, 0);
        let stored = projects::find_by_id(&surreal, open.id)
            .await
            .expect("read")
            .expect("exists");
        assert!(stored.repository_url.is_some());

        let second = sweep_in_process(&surreal, &slack_bot, Some(&forge), false).await;
        assert_eq!(second.repositories_synced, 0, "recorded repository is kept");
        assert!(forge
            .find_repository("sample-repo")
            .await
            .expect("find")
            .is_some());
    }
}
