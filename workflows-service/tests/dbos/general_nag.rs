//! `GeneralNag` on DBOS: daily-slot deduplication across live executor replicas.

use std::sync::atomic::AtomicU32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

use chrono::{TimeZone, Utc};
use workflows::dbos_engine;
use workflows::dbos_engine::general_nag::{
    self, Context, CountSource, GeneralNagError, GeneralNagReport,
};
use workflows::{CapturingSlackBot, SlackBot, SlackBotError, SlackChannel, SlackMessageReceipt};

use crate::support::{database, APP_VERSION, DEADLINE};

#[derive(Clone)]
struct FixedCounts((usize, usize));

#[async_trait::async_trait]
impl CountSource for FixedCounts {
    async fn read(&self) -> Result<(usize, usize), GeneralNagError> {
        Ok(self.0)
    }
}

#[derive(Clone, Default)]
struct HeldCounts {
    calls: Arc<AtomicUsize>,
    reached: Arc<Notify>,
}

#[derive(Clone, Default)]
struct FailOnceBot {
    attempts: Arc<AtomicU32>,
    capture: CapturingSlackBot,
}

#[async_trait::async_trait]
impl SlackBot for FailOnceBot {
    async fn create_private_channel(&self, name: &str) -> Result<SlackChannel, SlackBotError> {
        self.capture.create_private_channel(name).await
    }

    async fn post_message(&self, channel: &str, text: &str) -> Result<(), SlackBotError> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(SlackBotError::Transport(
                "synthetic first-attempt failure".into(),
            ));
        }
        self.capture.post_message(channel, text).await
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
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(SlackBotError::Transport(
                "synthetic first-attempt failure".into(),
            ));
        }
        self.capture
            .post_message_with_receipt(channel, text, correlation_id)
            .await
    }
    async fn message_receipt_by_correlation_since(
        &self,
        channel: &str,
        key: &str,
        _oldest: chrono::DateTime<Utc>,
    ) -> Result<Option<SlackMessageReceipt>, SlackBotError> {
        self.capture
            .message_receipt_by_correlation(channel, key)
            .await
    }
}

#[async_trait::async_trait]
impl CountSource for HeldCounts {
    async fn read(&self) -> Result<(usize, usize), GeneralNagError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            self.reached.notify_one();
            std::future::pending::<()>().await;
        }
        Ok((3, 1))
    }
}

fn slot() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 13, 1, 11, 0).unwrap()
}

fn context(slack: Arc<dyn SlackBot>) -> Context {
    Context {
        slack,
        channel_id: "C_GENERAL".to_owned(),
        simulated: true,
        counts: Arc::new(FixedCounts((3, 1))),
    }
}

async fn report_of(dbos: &dbos::DBOS, id: &str) -> GeneralNagReport {
    tokio::time::timeout(
        DEADLINE,
        dbos.retrieve_workflow::<GeneralNagReport, GeneralNagError>(id)
            .expect("retrieve GeneralNag")
            .result(),
    )
    .await
    .expect("GeneralNag did not finish")
    .expect("GeneralNag succeeds")
}

#[tokio::test]
async fn two_executors_start_one_daily_notice_and_duplicates_join_it() {
    let Some(db) = database().await else {
        return;
    };
    let first_bot = CapturingSlackBot::new();
    let second_bot = CapturingSlackBot::new();
    let first = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let second = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let first_workflow = general_nag::register(&first, context(Arc::new(first_bot.clone())))
        .expect("register on first executor");
    let second_workflow = general_nag::register(&second, context(Arc::new(second_bot.clone())))
        .expect("register on second executor");
    first.launch().await.expect("launch first executor");
    second.launch().await.expect("launch second executor");

    let (left, right) = tokio::join!(
        general_nag::fire(&first_workflow, slot()),
        general_nag::fire(&second_workflow, slot()),
    );
    left.expect("first start");
    right.expect("second start");

    let id = general_nag::workflow_id(slot());
    let report = report_of(&first, &id).await;
    assert!(
        !report.posted_followup,
        "simulated matters never post counts"
    );
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    first.shutdown().await;
    second.shutdown().await;

    let mut posted = first_bot.posted_messages();
    posted.extend(second_bot.posted_messages());
    assert_eq!(posted.len(), 1, "one daily notice across replicas");
    assert_eq!(posted[0].1, "Nobody Cares, Work Harder");
}

#[tokio::test]
async fn a_real_deployment_posts_the_counts_followup_as_a_second_message() {
    let Some(db) = database().await else {
        return;
    };
    let bot = CapturingSlackBot::new();
    let workflow_dbos = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let mut ctx = context(Arc::new(bot.clone()));
    ctx.simulated = false;
    let workflow = general_nag::register(&workflow_dbos, ctx).expect("register GeneralNag");
    workflow_dbos.launch().await.expect("launch");

    let slot = slot();
    let id = general_nag::workflow_id(slot);
    general_nag::fire(&workflow, slot).await.expect("start");
    let report = report_of(&workflow_dbos, &id).await;
    assert!(report.posted_followup);
    general_nag::fire(&workflow, slot)
        .await
        .expect("same-day duplicate start");
    assert_eq!(report_of(&workflow_dbos, &id).await, report);
    workflow_dbos.shutdown().await;

    assert_eq!(
        bot.posted_messages(),
        [
            (
                "C_GENERAL".to_owned(),
                "Nobody Cares, Work Harder".to_owned()
            ),
            (
                "C_GENERAL".to_owned(),
                "Total open matters: 3, pitches: 1".to_owned(),
            ),
        ]
    );
}

#[tokio::test]
async fn a_transient_slack_failure_retries_the_same_notice_step() {
    let Some(db) = database().await else {
        return;
    };
    let bot = FailOnceBot::default();
    let workflow_dbos = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = general_nag::register(&workflow_dbos, context(Arc::new(bot.clone())))
        .expect("register GeneralNag");
    workflow_dbos.launch().await.expect("launch");
    let slot = slot();
    let id = general_nag::workflow_id(slot);
    general_nag::fire(&workflow, slot).await.expect("start");
    let report = report_of(&workflow_dbos, &id).await;
    workflow_dbos.shutdown().await;

    assert!(!report.posted_followup);
    assert_eq!(
        bot.attempts.load(Ordering::SeqCst),
        2,
        "notify retried once"
    );
    assert_eq!(
        bot.capture.posted_messages(),
        [(
            "C_GENERAL".to_owned(),
            "Nobody Cares, Work Harder".to_owned()
        )]
    );
}

#[tokio::test]
async fn recovery_reuses_the_first_notice_checkpoint_and_finishes_the_counts_post() {
    let Some(db) = database().await else {
        return;
    };
    let bot = CapturingSlackBot::new();
    let held_counts = HeldCounts::default();
    let slot = slot();
    let id = general_nag::workflow_id(slot);
    let first = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let mut ctx = context(Arc::new(bot.clone()));
    ctx.simulated = false;
    ctx.counts = Arc::new(held_counts.clone());
    let workflow = general_nag::register(&first, ctx).expect("register GeneralNag");
    first.launch().await.expect("launch first executor");
    general_nag::fire(&workflow, slot).await.expect("start");
    tokio::time::timeout(DEADLINE, held_counts.reached.notified())
        .await
        .expect("GeneralNag never reached its counts step");
    first.shutdown().await;

    let steps = sqlx::query_scalar::<_, String>(
        "SELECT function_name FROM dbos.operation_outputs WHERE workflow_uuid = $1 \
         ORDER BY function_id",
    )
    .bind(&id)
    .fetch_all(&db.pool)
    .await
    .expect("read GeneralNag checkpoints");
    assert_eq!(
        steps,
        ["notify"],
        "the first notice checkpointed before shutdown"
    );

    let recovered = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let mut ctx = context(Arc::new(bot.clone()));
    ctx.simulated = false;
    ctx.counts = Arc::new(FixedCounts((3, 1)));
    let _workflow = general_nag::register(&recovered, ctx).expect("register recovery");
    recovered.launch().await.expect("relaunch same version");
    let report = report_of(&recovered, &id).await;
    recovered.shutdown().await;

    assert!(report.posted_followup);
    assert_eq!(
        bot.posted_messages(),
        [
            (
                "C_GENERAL".to_owned(),
                "Nobody Cares, Work Harder".to_owned()
            ),
            (
                "C_GENERAL".to_owned(),
                "Total open matters: 3, pitches: 1".to_owned(),
            ),
        ],
        "recovery must reuse the first step checkpoint",
    );
}

#[derive(Clone, Default)]
struct AcceptedBot {
    capture: CapturingSlackBot,
    hold_counts: bool,
    accepted: Arc<Notify>,
    accepts: Arc<AtomicUsize>,
    incomplete: Arc<std::sync::atomic::AtomicBool>,
    keys: Arc<std::sync::Mutex<Vec<String>>>,
}

impl AcceptedBot {
    async fn deliver(
        &self,
        channel: &str,
        text: &str,
        key: Option<&str>,
    ) -> Result<SlackMessageReceipt, SlackBotError> {
        let receipt = self
            .capture
            .post_message_with_receipt(channel, text, key)
            .await?;
        if let Some(key) = key {
            self.keys.lock().unwrap().push(key.to_owned());
        }
        if text.starts_with("Total open matters:") == self.hold_counts
            && self.accepts.fetch_add(1, Ordering::SeqCst) == 0
        {
            self.accepted.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(receipt)
    }
}

#[async_trait::async_trait]
impl SlackBot for AcceptedBot {
    async fn create_private_channel(&self, name: &str) -> Result<SlackChannel, SlackBotError> {
        self.capture.create_private_channel(name).await
    }
    async fn post_message(&self, channel: &str, text: &str) -> Result<(), SlackBotError> {
        self.deliver(channel, text, None).await.map(|_| ())
    }
    async fn post_message_with_receipt(
        &self,
        channel: &str,
        text: &str,
        key: Option<&str>,
    ) -> Result<SlackMessageReceipt, SlackBotError> {
        self.deliver(channel, text, key).await
    }
    async fn message_receipt_by_correlation_since(
        &self,
        channel: &str,
        key: &str,
        oldest: chrono::DateTime<Utc>,
    ) -> Result<Option<SlackMessageReceipt>, SlackBotError> {
        assert_eq!(
            oldest,
            slot().date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc(),
            "replay must keep the original slot boundary"
        );
        if self.incomplete.load(Ordering::SeqCst) {
            return Err(SlackBotError::IncompleteResponse);
        }
        self.capture
            .message_receipt_by_correlation(channel, key)
            .await
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
}

#[allow(clippy::too_many_lines)] // The acceptance/checkpoint crash boundary stays in one scenario.
async fn recover_accepted_notice(hold_counts: bool, incomplete: bool) {
    let Some(db) = database().await else { return };
    let bot = AcceptedBot {
        hold_counts,
        ..Default::default()
    };
    let mut ctx = context(Arc::new(bot.clone()));
    ctx.simulated = false;
    let id = general_nag::workflow_id(slot());
    let first = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = general_nag::register(&first, ctx.clone()).unwrap();
    first.launch().await.unwrap();
    general_nag::fire(&workflow, slot()).await.unwrap();
    tokio::time::timeout(DEADLINE, bot.accepted.notified())
        .await
        .expect("provider accepted before checkpoint");
    first.shutdown().await;
    let accepted_before_restart = bot.capture.posted_messages().len();
    bot.incomplete.store(incomplete, Ordering::SeqCst);

    let restarted = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    general_nag::register(&restarted, ctx).unwrap();
    restarted.launch().await.unwrap();
    let result = tokio::time::timeout(
        DEADLINE,
        restarted
            .retrieve_workflow::<GeneralNagReport, GeneralNagError>(&id)
            .unwrap()
            .result(),
    )
    .await
    .expect("recovered workflow settles");
    restarted.shutdown().await;
    if incomplete {
        assert!(
            result.is_err(),
            "ambiguous history must hold the failed notification"
        );
        assert_eq!(
            bot.capture.posted_messages().len(),
            accepted_before_restart,
            "uncertainty must never retransmit"
        );
    } else {
        assert!(result.unwrap().posted_followup);
        assert_eq!(
            bot.capture.posted_messages().len(),
            2,
            "both accepted effects must be reused before their checkpoints"
        );
        let keys = bot.keys.lock().unwrap();
        assert_eq!(keys.len(), 2);
        assert_ne!(
            keys[0], keys[1],
            "the two steps need distinct stable Effect Keys"
        );
        assert!(keys.iter().all(|key| key.starts_with(&format!("{id}/"))));
    }
}

#[tokio::test]
async fn acceptance_before_either_notification_checkpoint_reuses_the_receipt() {
    for hold_counts in [false, true] {
        recover_accepted_notice(hold_counts, false).await;
    }
}

#[tokio::test]
async fn ambiguous_history_after_acceptance_holds_both_notifications_without_retransmission() {
    for hold_counts in [false, true] {
        recover_accepted_notice(hold_counts, true).await;
    }
}

#[tokio::test]
async fn paginated_complete_absence_sends_each_initial_notification_once() {
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let Some(db) = database().await else { return };
    let server = MockServer::start().await;
    let oldest = slot()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp()
        .to_string();
    Mock::given(method("GET"))
        .and(path("/conversations.history"))
        .and(query_param_is_missing("cursor"))
        .and(query_param("oldest", &oldest))
        .and(query_param("inclusive", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok":true,"messages":vec![json!({"ts":"1791849601.000000"});15],"has_more":true,
            "response_metadata":{"next_cursor":"page-two"}
        })))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/conversations.history"))
        .and(query_param("cursor", "page-two"))
        .and(query_param("oldest", &oldest))
        .and(query_param("inclusive", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok":true,"messages":[],"has_more":false,"response_metadata":{"next_cursor":""}
        })))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok":true,"channel":"C_GENERAL","ts":"1791849602.000000"
        })))
        .expect(2)
        .mount(&server)
        .await;
    let bot = workflows::SlackBotClient::with_base_url("synthetic-token", server.uri());
    let mut ctx = context(Arc::new(bot));
    ctx.simulated = false;
    let executor = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = general_nag::register(&executor, ctx).unwrap();
    executor.launch().await.unwrap();
    general_nag::fire(&workflow, slot()).await.unwrap();
    assert!(
        report_of(&executor, &general_nag::workflow_id(slot()))
            .await
            .posted_followup
    );
    executor.shutdown().await;
    let requests = server.received_requests().await.unwrap();
    let keys = requests
        .iter()
        .filter(|request| request.url.path() == "/chat.postMessage")
        .map(|request| {
            serde_json::from_slice::<serde_json::Value>(&request.body).unwrap()["client_msg_id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(keys.len(), 2);
    assert_ne!(keys[0], keys[1]);
}
