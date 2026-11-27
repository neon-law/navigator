//! The `project-slack` lanes against a disposable PostgreSQL executor and
//! native Project fixtures: one serialized lane per Project, one Slack post
//! per relayed event, and different Projects posting without waiting on
//! each other.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use workflows::{dbos_engine, SlackBot, SlackBotError, SlackChannel};
use workflows_service::dbos_ingress::{ProjectSlackIngress, WorkerIngress};
use workflows_service::dbos_project_slack::{
    self, lane_lock_key, LaneRequest, ProjectSlackLane, ProjectSlackWorkflow,
};
use workflows_service::project_slack::{CiOutcome, LaneError, MainPushRequest};

#[path = "support/native_database.rs"]
mod support;
use support::{database, APP_VERSION};

#[path = "support/native_project.rs"]
mod native_project;

#[path = "support/lost_response_slack.rs"]
mod lost_response_slack;
use lost_response_slack::LostResponseSlackBot;

async fn firm_matter_on(
    pg: &store::postgres::PostgresDb,
    db: &store::application_store::ApplicationStore,
) -> store::projects::Project {
    store::postgres::PostgresDb::with_test_db(pg.clone(), async {
        native_project::matter(db).await.0
    })
    .await
}

fn push(short_sha: &str, run_attempt: u32) -> MainPushRequest {
    MainPushRequest {
        short_sha: short_sha.to_string(),
        run_url: "https://github.com/an-organization/sample-push/actions/runs/1".to_string(),
        author_login: "octocat".to_string(),
        gate: CiOutcome::Pass,
        publish: Some(CiOutcome::Skipped),
        run_attempt,
        subject: None,
    }
}

/// A `SlackBot` that records how many posts are in flight at once. `parties`
/// above one holds each post at a barrier until every party has arrived, so
/// an overlap test proves concurrency instead of racing for it — a wrongly
/// serialized run then fails as a timeout rather than deadlocking forever.
struct LaneSlackBot {
    hold: Duration,
    parties: usize,
    barrier: tokio::sync::Barrier,
    active: AtomicUsize,
    max_active: AtomicUsize,
    posted: Mutex<Vec<String>>,
}

impl LaneSlackBot {
    fn serial(hold: Duration) -> Self {
        Self::with_parties(hold, 1)
    }

    fn synchronized(hold: Duration, parties: usize) -> Self {
        Self::with_parties(hold, parties)
    }

    fn with_parties(hold: Duration, parties: usize) -> Self {
        Self {
            hold,
            parties,
            barrier: tokio::sync::Barrier::new(parties),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            posted: Mutex::new(Vec::new()),
        }
    }

    fn posted(&self) -> Vec<String> {
        self.posted.lock().expect("posted lock").clone()
    }
}

#[async_trait::async_trait]
impl SlackBot for LaneSlackBot {
    async fn find_private_channel_by_name(
        &self,
        _name: &str,
    ) -> Result<Option<SlackChannel>, SlackBotError> {
        Ok(None)
    }

    async fn create_private_channel(&self, name: &str) -> Result<SlackChannel, SlackBotError> {
        Ok(SlackChannel {
            id: format!("C{}", name.to_uppercase().replace('-', "")),
            name: name.to_string(),
        })
    }

    async fn post_message(&self, channel: &str, message: &str) -> Result<(), SlackBotError> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        if self.parties > 1 {
            let _arrived = tokio::time::timeout(self.hold * 4, self.barrier.wait()).await;
        }
        tokio::time::sleep(self.hold).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        self.posted
            .lock()
            .expect("posted lock")
            .push(format!("{channel}:{message}"));
        Ok(())
    }

    async fn user_id_by_email(&self, _email: &str) -> Result<Option<String>, SlackBotError> {
        Ok(None)
    }

    async fn channel_member_ids(&self, _channel: &str) -> Result<Vec<String>, SlackBotError> {
        Ok(Vec::new())
    }

    async fn invite_to_channel(
        &self,
        _channel: &str,
        _user_ids: &[String],
    ) -> Result<(), SlackBotError> {
        Ok(())
    }
}

/// A launched lane: the executor stays alive until the test shuts it down.
struct Lane {
    executor: dbos::DBOS,
    workflow: ProjectSlackWorkflow,
    postgres: store::postgres::PostgresDb,
    application_store: store::application_store::ApplicationStore,
    bot: Arc<LaneSlackBot>,
}

async fn launch_lane(
    url: &str,
    postgres: store::postgres::PostgresDb,
    application_store: store::application_store::ApplicationStore,
    bot: Arc<LaneSlackBot>,
) -> Lane {
    let executor = dbos::DBOS::new(dbos_engine::config(url, APP_VERSION));
    let workflow = dbos_project_slack::register(
        &executor,
        dbos_project_slack::Context::new(
            application_store.clone(),
            Arc::new(workflows_service::firm_slack::SharedSlackBot(bot.clone())),
            postgres.clone(),
            false,
        ),
    )
    .expect("register ProjectSlack");
    executor.launch().await.expect("launch DBOS");
    Lane {
        executor,
        workflow,
        postgres,
        application_store,
        bot,
    }
}

/// A session outside the lane guard's own pool, so an exclusion probe
/// cannot be satisfied by the pool's existing session.
async fn foreign_session(url: &str) -> sqlx::PgPool {
    sqlx::PgPool::connect(url).await.expect("foreign session")
}

#[tokio::test]
async fn a_project_lane_lock_excludes_every_other_session() {
    let Some(db) = database().await else { return };
    let postgres = db.store();
    let project = uuid::Uuid::now_v7();
    let lane = ProjectSlackLane::acquire(&postgres, project)
        .await
        .expect("acquire the lane");
    let outsider = foreign_session(&db.url).await;
    let taken: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(lane_lock_key(project))
        .fetch_one(&outsider)
        .await
        .expect("probe the held lane");
    assert!(!taken, "a second session took the Project's lane");
    // The guard is per Project: another Project's key is still free.
    let other_project = uuid::Uuid::now_v7();
    let other: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(lane_lock_key(other_project))
        .fetch_one(&outsider)
        .await
        .expect("probe another Project's lane");
    assert!(other, "another Project's lane was not free");
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(lane_lock_key(other_project))
        .execute(&outsider)
        .await
        .expect("release the probe");
    lane.release().await;
    let free: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(lane_lock_key(project))
        .fetch_one(&outsider)
        .await
        .expect("re-probe the released lane");
    assert!(free, "the lane never became free");
    outsider.close().await;
    db.close().await;
}

#[tokio::test]
async fn the_same_projects_main_pushes_post_one_at_a_time() {
    let Some(db) = database().await else { return };
    let bot = Arc::new(LaneSlackBot::serial(Duration::from_millis(250)));
    let lane = launch_lane(
        &db.url,
        db.store(),
        store::application_store::ApplicationStore::uninitialized(),
        bot.clone(),
    )
    .await;
    let matter = firm_matter_on(&lane.postgres, &lane.application_store).await;
    // Two distinct main pushes for ONE Project: both must post, and the
    // lane must hold them to one post at a time — the single-writer
    // guarantee the replaced virtual object gave the GitHub relay.
    let first = dbos_project_slack::main_push_workflow_id(matter.id, &push("abc1234", 1));
    let second = dbos_project_slack::main_push_workflow_id(matter.id, &push("def5678", 1));
    let (left, right) = tokio::join!(
        lane.workflow.start_with(
            LaneRequest::MainPush(push("abc1234", 1)),
            dbos::StartOptions {
                workflow_id: Some(&first),
                ..Default::default()
            },
        ),
        lane.workflow.start_with(
            LaneRequest::MainPush(push("def5678", 1)),
            dbos::StartOptions {
                workflow_id: Some(&second),
                ..Default::default()
            },
        ),
    );
    left.expect("start the first push")
        .result()
        .await
        .expect("first push completed");
    right
        .expect("start the second push")
        .result()
        .await
        .expect("second push completed");
    assert_eq!(lane.bot.posted().len(), 2, "{:?}", lane.bot.posted());
    assert_eq!(
        lane.bot.max_active.load(Ordering::SeqCst),
        1,
        "one Project's posts never overlap"
    );
    lane.executor.shutdown().await;
    db.close().await;
}

#[tokio::test]
async fn the_ingress_posts_one_relayed_event_once() {
    let Some(db) = database().await else { return };
    let bot = Arc::new(LaneSlackBot::serial(Duration::from_millis(50)));
    let lane = launch_lane(
        &db.url,
        db.store(),
        store::application_store::ApplicationStore::uninitialized(),
        bot.clone(),
    )
    .await;
    let matter = firm_matter_on(&lane.postgres, &lane.application_store).await;
    let ingress = ProjectSlackIngress::new(
        "synthetic-service-token".into(),
        lane.workflow.clone(),
        lane.executor.clone(),
        lane.application_store.clone(),
        lane.postgres.clone(),
    );
    let router = workflows_service::health::router_with_dbos_ingress(
        WorkerIngress {
            project_slack: Some(ingress),
            ..WorkerIngress::default()
        },
        None,
    );
    let id = dbos_project_slack::main_push_workflow_id(matter.id, &push("abc1234", 1));
    let payload = serde_json::json!({
        "workflow_id": id,
        "event": push("abc1234", 1),
    });
    for _ in 0..2 {
        let delivery = Request::builder()
            .method("POST")
            .uri("/internal/dbos/project-slack")
            .header("authorization", "Bearer synthetic-service-token")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();
        assert_eq!(
            router.clone().oneshot(delivery).await.unwrap().status(),
            StatusCode::ACCEPTED
        );
    }
    lane.executor
        .retrieve_workflow::<(), LaneError>(&id)
        .expect("retrieve the relayed run")
        .result()
        .await
        .expect("relayed run completed");
    assert_eq!(
        lane.bot.posted().len(),
        1,
        "a repeated delivery must not post twice: {:?}",
        lane.bot.posted()
    );
    assert_eq!(lane.bot.max_active.load(Ordering::SeqCst), 1);
    lane.executor.shutdown().await;
    db.close().await;
}

#[tokio::test]
async fn a_repeated_client_view_posts_once() {
    let Some(db) = database().await else { return };
    let bot = Arc::new(LaneSlackBot::serial(Duration::from_millis(50)));
    let lane = launch_lane(
        &db.url,
        db.store(),
        store::application_store::ApplicationStore::uninitialized(),
        bot.clone(),
    )
    .await;
    let matter = firm_matter_on(&lane.postgres, &lane.application_store).await;
    let id = dbos_project_slack::client_view_workflow_id(matter.id);
    for attempt in 0..2 {
        let started = lane
            .workflow
            .start_with(
                LaneRequest::ClientView,
                dbos::StartOptions {
                    workflow_id: Some(&id),
                    ..Default::default()
                },
            )
            .await
            .unwrap_or_else(|error| panic!("client view {attempt} must start: {error}"));
        started.result().await.expect("client view completed");
    }
    assert_eq!(
        lane.bot.posted().len(),
        1,
        "the repeated client view joined the first run: {:?}",
        lane.bot.posted()
    );
    lane.executor.shutdown().await;
    db.close().await;
}

#[tokio::test]
async fn different_projects_post_without_waiting_on_each_other() {
    let Some(db) = database().await else { return };
    let bot = Arc::new(LaneSlackBot::synchronized(Duration::from_millis(200), 2));
    let lane = launch_lane(
        &db.url,
        db.store(),
        store::application_store::ApplicationStore::uninitialized(),
        bot.clone(),
    )
    .await;
    let first = firm_matter_on(&lane.postgres, &lane.application_store).await;
    let second = firm_matter_on(&lane.postgres, &lane.application_store).await;
    let first_id = dbos_project_slack::main_push_workflow_id(first.id, &push("abc1234", 1));
    let second_id = dbos_project_slack::main_push_workflow_id(second.id, &push("def5678", 1));
    let (left, right) = tokio::join!(
        lane.workflow.start_with(
            LaneRequest::MainPush(push("abc1234", 1)),
            dbos::StartOptions {
                workflow_id: Some(&first_id),
                ..Default::default()
            },
        ),
        lane.workflow.start_with(
            LaneRequest::MainPush(push("def5678", 1)),
            dbos::StartOptions {
                workflow_id: Some(&second_id),
                ..Default::default()
            },
        ),
    );
    left.expect("start the first Project's push")
        .result()
        .await
        .expect("first Project's push completed");
    right
        .expect("start the second Project's push")
        .result()
        .await
        .expect("second Project's push completed");
    assert_eq!(lane.bot.posted().len(), 2);
    assert_eq!(
        lane.bot.max_active.load(Ordering::SeqCst),
        2,
        "different Projects' posts must overlap"
    );
    lane.executor.shutdown().await;
    db.close().await;
}

#[tokio::test]
async fn the_ingress_never_rebinds_a_claimed_event_id() {
    let Some(db) = database().await else { return };
    let bot = Arc::new(LaneSlackBot::serial(Duration::from_millis(50)));
    let lane = launch_lane(
        &db.url,
        db.store(),
        store::application_store::ApplicationStore::uninitialized(),
        bot.clone(),
    )
    .await;
    let matter = firm_matter_on(&lane.postgres, &lane.application_store).await;
    let ingress = ProjectSlackIngress::new(
        "synthetic-service-token".into(),
        lane.workflow.clone(),
        lane.executor.clone(),
        lane.application_store.clone(),
        lane.postgres.clone(),
    );
    let router = workflows_service::health::router_with_dbos_ingress(
        WorkerIngress {
            project_slack: Some(ingress),
            ..WorkerIngress::default()
        },
        None,
    );
    let id = dbos_project_slack::main_push_workflow_id(matter.id, &push("abc1234", 1));
    let deliver = |body: serde_json::Value| {
        Request::builder()
            .method("POST")
            .uri("/internal/dbos/project-slack")
            .header("authorization", "Bearer synthetic-service-token")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    };
    let first = serde_json::json!({"workflow_id": id, "event": push("abc1234", 1)});
    let response = router.clone().oneshot(deliver(first)).await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    lane.executor
        .retrieve_workflow::<(), LaneError>(&id)
        .expect("retrieve the relayed run")
        .result()
        .await
        .expect("relayed run completed");
    // The same (sha, attempt) relayed again with different event fields
    // would silently swap what the lane published: the claim refuses it.
    let rebinding = serde_json::json!({
        "workflow_id": id,
        "event": MainPushRequest {
            gate: CiOutcome::Fail,
            ..push("abc1234", 1)
        },
    });
    let response = router.clone().oneshot(deliver(rebinding)).await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        lane.bot.posted().len(),
        1,
        "the refused rebind must not post: {:?}",
        lane.bot.posted()
    );
    // A well-formed id for a Project that does not exist answers 404 so the
    // relay can tell a typo from a race.
    let missing = serde_json::json!({
        "workflow_id": dbos_project_slack::client_view_workflow_id(uuid::Uuid::now_v7()),
    });
    let response = router.oneshot(deliver(missing)).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    lane.executor.shutdown().await;
    db.close().await;
}

/// Slack accepted the post but the worker stopped before the step's
/// checkpoint. Recovery re-runs the step; the correlated lookup finds the
/// accepted post, so neither lane posts twice and the receipt lands once.
#[tokio::test]
async fn a_post_accepted_before_its_checkpoint_is_not_repeated_on_recovery() {
    let Some(db) = database().await else { return };
    let application_store = store::application_store::ApplicationStore::uninitialized();
    let matter = firm_matter_on(&db.store(), &application_store).await;
    for (request, id) in [
        (
            LaneRequest::MainPush(push("abc1234", 1)),
            dbos_project_slack::main_push_workflow_id(matter.id, &push("abc1234", 1)),
        ),
        (
            LaneRequest::ClientView,
            dbos_project_slack::client_view_workflow_id(matter.id),
        ),
    ] {
        let bot = Arc::new(LostResponseSlackBot::default());
        let executor = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
        let workflow = dbos_project_slack::register(
            &executor,
            dbos_project_slack::Context::new(
                application_store.clone(),
                Arc::new(workflows_service::firm_slack::SharedSlackBot(bot.clone())),
                db.store(),
                false,
            ),
        )
        .expect("register ProjectSlack");
        executor.launch().await.expect("launch DBOS");
        let running = {
            let id = id.clone();
            tokio::spawn(async move {
                workflow
                    .run_with(
                        request,
                        dbos::RunOptions {
                            workflow_id: Some(&id),
                            ..Default::default()
                        },
                    )
                    .await
            })
        };
        tokio::time::timeout(Duration::from_mins(1), bot.reached.notified())
            .await
            .expect("the lane never reached Slack");
        executor.shutdown().await;
        let interrupted = running.await.expect("the lane task panicked");
        assert!(
            matches!(interrupted, Err(dbos::Error::Interrupted { .. })),
            "shutdown interrupts the post: {interrupted:?}"
        );

        executor.launch().await.expect("relaunch DBOS");
        tokio::time::timeout(
            Duration::from_mins(1),
            executor
                .retrieve_workflow::<(), LaneError>(&id)
                .expect("retrieve the recovered lane")
                .result(),
        )
        .await
        .expect("the recovered lane never finished")
        .expect("the recovered lane succeeds");
        executor.shutdown().await;
        assert_eq!(
            bot.posted().len(),
            1,
            "{id} posted twice: {:?}",
            bot.posted()
        );
        assert!(
            bot.posted()[0].1.is_some(),
            "{id} posted without a correlation Slack can find again"
        );
    }
    db.close().await;
}
