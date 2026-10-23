use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;

use chrono::NaiveDate;
use workflows::{CapturingSlackBot, SlackBot, SlackBotError, SlackChannel, SlackMessageReceipt};

use crate::support::{database, TestDatabase, APP_VERSION, DEADLINE};
use archives::runner::SnapshotSummary;
use archives::workflow::{self, ArchivesDbosPhaseError, ArchivesDbosPhases, RunRequest};

#[derive(Default)]
struct Phases {
    snapshots: AtomicUsize,
    costs: AtomicUsize,
    promotions: AtomicUsize,
}

#[async_trait::async_trait]
impl ArchivesDbosPhases for Phases {
    async fn snapshot(
        &self,
        scheduled_for: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<SnapshotSummary, ArchivesDbosPhaseError> {
        self.snapshots.fetch_add(1, Ordering::SeqCst);
        Ok(SnapshotSummary {
            run_date: NaiveDate::from_ymd_opt(2026, 10, 22).unwrap(),
            scheduled_for,
            entries: Vec::new(),
            failures: Vec::new(),
            postgres: None,
        })
    }

    async fn cost(&self) -> Result<(), ArchivesDbosPhaseError> {
        self.costs.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn promote(&self, _summary: &SnapshotSummary) -> Vec<String> {
        self.promotions.fetch_add(1, Ordering::SeqCst);
        Vec::new()
    }

    fn render_digest(&self, _summary: &SnapshotSummary) -> Result<String, ArchivesDbosPhaseError> {
        Ok("synthetic Archives digest".into())
    }
}

#[derive(Default)]
struct AcceptThenHold {
    inner: CapturingSlackBot,
    first: AtomicU32,
}

#[async_trait::async_trait]
impl SlackBot for AcceptThenHold {
    async fn create_private_channel(&self, name: &str) -> Result<SlackChannel, SlackBotError> {
        self.inner.create_private_channel(name).await
    }

    async fn post_message(&self, channel: &str, text: &str) -> Result<(), SlackBotError> {
        self.inner.post_message(channel, text).await
    }

    async fn user_id_by_email(&self, email: &str) -> Result<Option<String>, SlackBotError> {
        self.inner.user_id_by_email(email).await
    }

    async fn channel_member_ids(&self, channel: &str) -> Result<Vec<String>, SlackBotError> {
        self.inner.channel_member_ids(channel).await
    }

    async fn invite_to_channel(
        &self,
        channel: &str,
        users: &[String],
    ) -> Result<(), SlackBotError> {
        self.inner.invite_to_channel(channel, users).await
    }

    async fn post_message_with_receipt(
        &self,
        channel: &str,
        text: &str,
        correlation: Option<&str>,
    ) -> Result<SlackMessageReceipt, SlackBotError> {
        let receipt = self
            .inner
            .post_message_with_receipt(channel, text, correlation)
            .await?;
        if self.first.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(SlackBotError::Transport(
                "synthetic process loss after Slack accepted the message".into(),
            ));
        }
        Ok(receipt)
    }

    async fn message_receipt_by_correlation(
        &self,
        channel: &str,
        correlation: &str,
    ) -> Result<Option<SlackMessageReceipt>, SlackBotError> {
        self.inner
            .message_receipt_by_correlation(channel, correlation)
            .await
    }
}

fn settings(db: &TestDatabase, id: &'static str) -> workflows::dbos_engine::Settings {
    let lookup = db.lookup();
    workflows::dbos_engine::Settings::from_lookup(move |key| {
        if key == workflows::dbos_engine::RELEASE_ENV {
            Some(APP_VERSION.to_owned())
        } else if key == workflows::dbos_engine::EXECUTOR_ID_ENV {
            Some(id.to_owned())
        } else {
            lookup(key)
        }
    })
    .unwrap()
}

#[tokio::test]
async fn archives_registered_workflow_retries_accepted_notice_without_duplicate_side_effects() {
    let Some(db) = database().await else { return };
    let phases = Arc::new(Phases::default());
    let slack = Arc::new(AcceptThenHold::default());
    let slot = "2026-10-22T05:55:00Z".parse().unwrap();
    let id = workflow::dbos_workflow_id(slot);

    let executor = dbos::DBOS::new(settings(&db, "archives-pod-a").config());
    let archives = workflow::register_dbos_with(
        &executor,
        slack.clone(),
        "synthetic-messages".into(),
        true,
        phases.clone(),
    )
    .unwrap();
    executor.launch().await.unwrap();
    archives
        .start_with(
            RunRequest {
                scheduled_for: Some(slot),
            },
            dbos::StartOptions {
                workflow_id: Some(&id),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let report = tokio::time::timeout(
        DEADLINE,
        executor
            .retrieve_workflow::<workflow::RunReport, workflow::ArchivesDbosError>(&id)
            .unwrap()
            .result(),
    )
    .await
    .expect("Archives workflow did not recover")
    .unwrap();
    executor.shutdown().await;

    assert_eq!(report.invocation_id, id);
    assert_eq!(phases.snapshots.load(Ordering::SeqCst), 1);
    assert_eq!(phases.costs.load(Ordering::SeqCst), 1);
    assert_eq!(phases.promotions.load(Ordering::SeqCst), 1);
    assert_eq!(slack.inner.posted_messages().len(), 1);
}
