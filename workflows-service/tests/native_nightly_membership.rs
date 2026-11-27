//! Native PostgreSQL acceptance for roster reconciliation and actual notice delivery.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use store::application_store::ApplicationStore;
use uuid::Uuid;
use workflows::{CapturingSlackBot, SlackBot, SlackBotError, SlackChannel, SlackMessageReceipt};
use workflows_service::nightly_project_update::{membership, repository::RepositoryForge, slack};

#[path = "support/native_project.rs"]
mod native_project;

/// The existing capture transport plus the roster state its API does not model.
#[derive(Default)]
struct RosterSlack {
    capture: CapturingSlackBot,
    members: Mutex<BTreeMap<String, BTreeSet<String>>>,
    removed: Mutex<Vec<(String, String)>>,
    looked_up: Mutex<Vec<String>>,
}

#[async_trait]
impl SlackBot for RosterSlack {
    async fn create_private_channel(&self, name: &str) -> Result<SlackChannel, SlackBotError> {
        self.capture.create_private_channel(name).await
    }

    async fn post_message(&self, channel: &str, text: &str) -> Result<(), SlackBotError> {
        self.capture.post_message(channel, text).await
    }

    async fn post_message_with_receipt(
        &self,
        channel: &str,
        text: &str,
        correlation: Option<&str>,
    ) -> Result<SlackMessageReceipt, SlackBotError> {
        self.capture
            .post_message_with_receipt(channel, text, correlation)
            .await
    }

    async fn message_receipt_by_correlation(
        &self,
        channel: &str,
        correlation: &str,
    ) -> Result<Option<SlackMessageReceipt>, SlackBotError> {
        self.capture
            .message_receipt_by_correlation(channel, correlation)
            .await
    }

    async fn user_id_by_email(&self, email: &str) -> Result<Option<String>, SlackBotError> {
        self.looked_up.lock().unwrap().push(email.to_owned());
        Ok(None)
    }

    async fn channel_member_ids(&self, channel: &str) -> Result<Vec<String>, SlackBotError> {
        Ok(self
            .members
            .lock()
            .unwrap()
            .get(channel)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect())
    }

    async fn invite_to_channel(
        &self,
        channel: &str,
        users: &[String],
    ) -> Result<(), SlackBotError> {
        self.members
            .lock()
            .unwrap()
            .entry(channel.to_owned())
            .or_default()
            .extend(users.iter().cloned());
        Ok(())
    }

    async fn remove_from_channel(&self, channel: &str, user: &str) -> Result<bool, SlackBotError> {
        let removed = self
            .members
            .lock()
            .unwrap()
            .entry(channel.to_owned())
            .or_default()
            .remove(user);
        if removed {
            self.removed
                .lock()
                .unwrap()
                .push((channel.to_owned(), user.to_owned()));
        }
        Ok(removed)
    }
}

#[tokio::test]
async fn complete_native_roster_removes_only_stale_members_and_notices_mention_recorded_identities()
{
    store::test_support::with_postgres(Box::pin(async {
        let source = ApplicationStore::uninitialized();
        let (project, _) = Box::pin(native_project::matter(&source)).await;
        store::projects::set_internal_slack_channel_id(&source, project.id, "COWNED")
            .await
            .unwrap();
        let person = store::persons::create(
            &source,
            &store::persons::NewPerson::with_role(
                "Synthetic clerk",
                format!("clerk-{}@example.test", Uuid::new_v4()),
                store::persons::Role::Clerk,
            ),
        )
        .await
        .unwrap();
        store::projects::add_participation(&source, project.id, person.id, "clerk")
            .await
            .unwrap();
        store::external_identities::link(
            &source,
            person.id,
            store::external_identities::ExternalSystem::Slack,
            "U-RECORDED",
            None,
        )
        .await
        .unwrap();
        let provider = Arc::new(RosterSlack {
            members: Mutex::new(BTreeMap::from([
                ("COWNED".into(), BTreeSet::from(["U-STALE".into()])),
                ("COTHER".into(), BTreeSet::from(["U-OTHER".to_owned()])),
            ])),
            ..Default::default()
        });
        let bot: Arc<dyn SlackBot> = provider.clone();
        let report = membership::invite_members(&source, &bot, project.id, "COWNED")
            .await
            .unwrap();
        assert_eq!(
            (report.invited, report.removed, report.unmatched_assignments),
            (1, 1, 0)
        );
        assert_eq!(
            *provider.removed.lock().unwrap(),
            [("COWNED".to_owned(), "U-STALE".to_owned())]
        );
        assert_eq!(
            provider.members.lock().unwrap()["COWNED"],
            BTreeSet::from(["U-RECORDED".to_owned()])
        );
        assert_eq!(
            provider.members.lock().unwrap()["COTHER"],
            BTreeSet::from(["U-OTHER".to_owned()])
        );
        let replay = membership::invite_members(&source, &bot, project.id, "COWNED")
            .await
            .unwrap();
        assert_eq!(
            (replay.invited, replay.removed, replay.already_members),
            (0, 0, 1)
        );
        for _ in 0..2 {
            assert!(slack::post_notice(
                &source,
                bot.as_ref(),
                &RepositoryForge::None,
                "2026-11-21",
                project.id
            )
            .await
            .unwrap());
        }
        let posted = provider.capture.posted_messages();
        assert_eq!(
            posted.len(),
            1,
            "the provider receipt joins the repeated notice"
        );
        assert_eq!(posted[0].0, "COWNED");
        assert!(posted[0]
            .1
            .starts_with("<@U-RECORDED> The Nightly Project Update"));
        assert!(!posted[0].1.contains("U-STALE") && !posted[0].1.contains("U-OTHER"));
        assert!(
            provider.looked_up.lock().unwrap().is_empty(),
            "recorded identity needs no email lookup"
        );
    }))
    .await;
}

#[tokio::test]
async fn unmatched_and_empty_native_rosters_cannot_remove_channel_members_or_invent_mentions() {
    store::test_support::with_postgres(Box::pin(async {
        let source = ApplicationStore::uninitialized();
        let (project, _) = Box::pin(native_project::matter(&source)).await;
        store::projects::set_internal_slack_channel_id(&source, project.id, "CINCOMPLETE")
            .await
            .unwrap();
        let person = store::persons::create(
            &source,
            &store::persons::NewPerson::with_role(
                "Synthetic unmatched clerk",
                format!("unmatched-{}@example.test", Uuid::new_v4()),
                store::persons::Role::Clerk,
            ),
        )
        .await
        .unwrap();
        let participation =
            store::projects::add_participation(&source, project.id, person.id, "clerk")
                .await
                .unwrap();
        let initial = BTreeSet::from(["U-RETAIN".to_owned(), "U-UNKNOWN".to_owned()]);
        let provider = Arc::new(RosterSlack {
            members: Mutex::new(BTreeMap::from([("CINCOMPLETE".into(), initial.clone())])),
            ..Default::default()
        });
        let bot: Arc<dyn SlackBot> = provider.clone();
        let incomplete = membership::invite_members(&source, &bot, project.id, "CINCOMPLETE")
            .await
            .unwrap();
        assert_eq!(
            (
                incomplete.unmatched_assignments,
                incomplete.removed,
                incomplete.invited
            ),
            (1, 0, 0)
        );
        assert!(slack::post_notice(
            &source,
            bot.as_ref(),
            &RepositoryForge::None,
            "2026-11-21",
            project.id
        )
        .await
        .unwrap());
        let posted = provider.capture.posted_messages();
        assert_eq!(posted.len(), 1);
        assert!(
            !posted[0].1.contains("<@"),
            "unknown memberships cannot supply Person mentions"
        );
        store::projects::remove_participation(&source, participation.id)
            .await
            .unwrap();
        let empty = membership::invite_members(&source, &bot, project.id, "CINCOMPLETE")
            .await
            .unwrap();
        assert_eq!(
            (empty.unmatched_assignments, empty.removed, empty.invited),
            (0, 0, 0)
        );
        assert!(provider.removed.lock().unwrap().is_empty());
        assert_eq!(provider.members.lock().unwrap()["CINCOMPLETE"], initial);
        assert!(provider
            .looked_up
            .lock()
            .unwrap()
            .iter()
            .all(|email| email == &person.email));
    }))
    .await;
}
