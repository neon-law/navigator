//! `NotationReset` on DBOS: one reset per daily slot across executors, and
//! recovery that keeps completed deletes and posts once.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use store::notation_reset::ResetOutcome;
use store::test_support::{mem_surreal, seed_notation};
use tokio::sync::Notify;
use workflows::dbos_engine::{self, notation_reset};
use workflows::notation_reset::NotationResetReport;
use workflows::{CapturingSlackBot, SlackBot, SlackBotError, SlackChannel, SlackMessageReceipt};

use crate::support::{database, APP_VERSION, DEADLINE};

fn slot() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 21, 4, 20, 0).unwrap()
}

fn context(
    surreal: &store::surreal::SurrealDb,
    slack: Arc<dyn SlackBot>,
) -> notation_reset::Context {
    notation_reset::Context {
        surreal: surreal.clone(),
        slack,
        channel_id: "C-STAGING".to_owned(),
        environment: "staging",
    }
}

async fn report_of(dbos: &dbos::DBOS, id: &str) -> NotationResetReport {
    tokio::time::timeout(
        DEADLINE,
        dbos.retrieve_workflow::<NotationResetReport, notation_reset::NotationResetError>(id)
            .expect("retrieve NotationReset")
            .result(),
    )
    .await
    .expect("NotationReset did not finish")
    .expect("NotationReset succeeds")
}

/// A bot whose first post never returns, so the run is cut off after every
/// delete has checkpointed and before the post has.
#[derive(Clone, Default)]
struct HeldBot {
    posts: Arc<AtomicUsize>,
    reached: Arc<Notify>,
    capture: CapturingSlackBot,
}

#[async_trait::async_trait]
impl SlackBot for HeldBot {
    async fn create_private_channel(&self, name: &str) -> Result<SlackChannel, SlackBotError> {
        self.capture.create_private_channel(name).await
    }

    async fn post_message(&self, _channel: &str, _text: &str) -> Result<(), SlackBotError> {
        self.posts.fetch_add(1, Ordering::SeqCst);
        self.reached.notify_one();
        std::future::pending::<()>().await;
        Ok(())
    }

    async fn user_id_by_email(&self, email: &str) -> Result<Option<String>, SlackBotError> {
        self.capture.user_id_by_email(email).await
    }

    async fn channel_member_ids(&self, channel: &str) -> Result<Vec<String>, SlackBotError> {
        self.capture.channel_member_ids(channel).await
    }

    async fn invite_to_channel(
        &self,
        channel: &str,
        users: &[String],
    ) -> Result<(), SlackBotError> {
        self.capture.invite_to_channel(channel, users).await
    }

    async fn post_message_with_receipt(
        &self,
        channel: &str,
        text: &str,
        correlation_id: Option<&str>,
    ) -> Result<SlackMessageReceipt, SlackBotError> {
        self.capture
            .post_message_with_receipt(channel, text, correlation_id)
            .await
    }
}

#[tokio::test]
async fn two_executors_run_one_reset_per_slot_and_post_once() {
    let Some(db) = database().await else {
        return;
    };
    let surreal = mem_surreal().await;
    let drafts = [seed_notation(&surreal).await, seed_notation(&surreal).await];
    let (first_bot, second_bot) = (CapturingSlackBot::new(), CapturingSlackBot::new());
    let first = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let second = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let first_workflow =
        notation_reset::register(&first, context(&surreal, Arc::new(first_bot.clone())))
            .expect("register on first executor");
    let second_workflow =
        notation_reset::register(&second, context(&surreal, Arc::new(second_bot.clone())))
            .expect("register on second executor");
    first.launch().await.expect("launch first executor");
    second.launch().await.expect("launch second executor");

    let (left, right) = tokio::join!(
        notation_reset::fire(&first_workflow, slot()),
        notation_reset::fire(&second_workflow, slot()),
    );
    left.expect("first start");
    right.expect("second start");
    let id = notation_reset::workflow_id(slot());
    assert_eq!(id, "NotationReset-2026-10-21");
    let report = report_of(&first, &id).await;
    notation_reset::fire(&first_workflow, slot())
        .await
        .expect("a same-slot start joins the finished run");
    assert_eq!(report_of(&first, &id).await, report);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    first.shutdown().await;
    second.shutdown().await;

    assert_eq!(report.notations_deleted, 2);
    assert!(report.failures.is_empty());
    for draft in drafts {
        assert!(store::notations::find_by_id(&surreal, draft)
            .await
            .unwrap()
            .is_none());
    }
    let mut posted = first_bot.posted_messages();
    posted.extend(second_bot.posted_messages());
    assert_eq!(posted.len(), 1, "one reset message across executors");
    assert_eq!(posted[0].0, "C-STAGING");
    assert!(
        posted[0]
            .1
            .starts_with("*Notation reset complete* (staging)\nDrafts deleted: 2 notations"),
        "{}",
        posted[0].1
    );
}

#[tokio::test]
async fn recovery_keeps_checkpointed_deletes_and_posts_once() {
    let Some(db) = database().await else {
        return;
    };
    let surreal = mem_surreal().await;
    let draft = seed_notation(&surreal).await;
    let held = HeldBot::default();
    let id = notation_reset::workflow_id(slot());

    let first = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = notation_reset::register(&first, context(&surreal, Arc::new(held.clone())))
        .expect("register NotationReset");
    first.launch().await.expect("launch first executor");
    notation_reset::fire(&workflow, slot())
        .await
        .expect("start");
    tokio::time::timeout(DEADLINE, held.reached.notified())
        .await
        .expect("NotationReset never reached its post");
    first.shutdown().await;

    let steps = sqlx::query_scalar::<_, String>(
        "SELECT function_name FROM dbos.operation_outputs WHERE workflow_uuid = $1 \
         ORDER BY function_id",
    )
    .bind(&id)
    .fetch_all(&db.pool)
    .await
    .expect("read NotationReset checkpoints");
    assert_eq!(
        steps,
        [
            "started".to_owned(),
            "discover".to_owned(),
            format!("delete-notation-{draft}"),
            "finished".to_owned(),
        ],
        "every delete checkpointed before the post"
    );
    assert_eq!(
        store::notation_reset::delete_draft(&surreal, draft)
            .await
            .unwrap(),
        ResetOutcome::AlreadyGone,
        "a delete repeated by recovery finds nothing left to delete"
    );

    let bot = CapturingSlackBot::new();
    let recovered = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let _workflow = notation_reset::register(&recovered, context(&surreal, Arc::new(bot.clone())))
        .expect("register recovery");
    recovered.launch().await.expect("relaunch same version");
    let report = report_of(&recovered, &id).await;
    recovered.shutdown().await;

    assert_eq!(
        report.notations_deleted, 1,
        "the checkpointed delete counts"
    );
    assert_eq!(held.posts.load(Ordering::SeqCst), 1);
    let posted = bot.posted_messages();
    assert_eq!(posted.len(), 1, "recovery posts once");
    assert!(
        posted[0].1.contains("Drafts deleted: 1 notations"),
        "{}",
        posted[0].1
    );
}
