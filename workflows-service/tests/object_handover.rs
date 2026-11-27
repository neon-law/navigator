//! PostgreSQL-backed workflow isolation and recovery.

use std::future::Future;
use std::sync::Arc;

use sqlx::postgres::{PgConnectOptions, PgSslMode};
use store::application_store::ApplicationStore;
use store::postgres::PostgresDb;
use uuid::Uuid;
use workflows::SlackBot;
use workflows_service::dbos_project_slack::{self, LaneRequest};
use workflows_service::firm_slack::{FirmSlackBots, ProjectSlackBots, SharedSlackBot};
use workflows_service::project_slack::{CiOutcome, LaneError, MainPushRequest};

#[path = "support/native_project.rs"]
mod native_project;

struct Matter {
    project: Uuid,
    firm: Uuid,
    admin: Uuid,
    brand: String,
    entity: Uuid,
}

async fn matter(db: &ApplicationStore) -> Matter {
    let (project, admin) = native_project::matter(db).await;
    Matter {
        project: project.id,
        firm: project.firm_id.expect("native fixture Project has a Firm"),
        admin,
        brand: project.brand,
        entity: project.entity_id,
    }
}

async fn new_project(db: &ApplicationStore, firm: Uuid, brand: &str, entity: Uuid) -> Uuid {
    native_project::new_project(db, firm, brand, entity)
        .await
        .id
}

async fn with_fixture<F, Fut>(test: F)
where
    F: FnOnce(PostgresDb, String) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let port = store::test_support::postgres_test_port();
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE")
                .unwrap_or_else(|_| "smoke".to_owned()),
        )
        .ssl_mode(PgSslMode::Disable)
        .options([("session_preload_libraries", "age")]);
    let fixture = store::test_support::PostgresFixture::create(options)
        .await
        .unwrap();
    let pg = fixture.postgres().clone();
    let url = format!(
        "postgres://postgres:synthetic@127.0.0.1:{port}/{}?sslmode=disable",
        fixture.database()
    );
    let result = tokio::spawn(PostgresDb::with_test_db(
        pg.clone(),
        Box::pin(test(pg, url)),
    ))
    .await;
    fixture.close().await.unwrap();
    result.unwrap();
}

fn push(sha: &str) -> MainPushRequest {
    MainPushRequest {
        short_sha: sha.to_owned(),
        run_url: "https://github.com/synthetic/example/actions/runs/1".to_owned(),
        author_login: "synthetic".to_owned(),
        gate: CiOutcome::Pass,
        publish: None,
        run_attempt: 1,
        subject: None,
    }
}

async fn connect_slack(
    db: &ApplicationStore,
    matter: &Matter,
    kms: &dyn cloud::RuntimeKms,
    value: &str,
) {
    store::firm_secrets::put(
        db,
        store::firm_secrets::SecretPutRequest {
            actor_role: store::persons::Role::Admin,
            actor_person_id: Some(matter.admin),
            firm_id: matter.firm,
            provider: store::firm_secrets::IntegrationProvider::Slack,
            kind: store::firm_secrets::IntegrationSecretKind::SlackBotToken,
            value,
        },
        kms,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn explicit_context_pool_keeps_real_firm_credentials_separate_from_ambient_database() {
    with_fixture(|pg, _| async move {
        let db = ApplicationStore::uninitialized();
        let first = matter(&db).await;
        let kms = Arc::new(cloud::FakeKms::new("synthetic-key-version"));
        connect_slack(&db, &first, kms.as_ref(), "synthetic-first-token").await;
        let first_pg = pg.clone();
        with_fixture(move |second_pg, _| async move {
            let db = ApplicationStore::uninitialized();
            let second = matter(&db).await;
            connect_slack(&db, &second, kms.as_ref(), "synthetic-second-token").await;
            let bots = FirmSlackBots::new(kms.clone());
            assert!(
                bots.for_project_on(&first_pg, &db, first.project)
                    .await
                    .is_ok(),
                "explicit first pool wins over ambient second pool"
            );
            assert!(matches!(
                bots.for_project_on(&second_pg, &db, first.project).await,
                Err(LaneError::ProjectGone(_))
            ));
            let credential = store::firm_secrets::resolve_project_credential_on(
                &first_pg,
                first.project,
                store::firm_secrets::IntegrationProvider::Slack,
                store::firm_secrets::IntegrationSecretKind::SlackBotToken,
                kms.as_ref(),
            )
            .await
            .unwrap();
            assert_eq!(
                credential.expose_for_provider_call(),
                "synthetic-first-token"
            );
            store::projects::set_internal_slack_channel_on(
                &first_pg,
                first.project,
                "CSYNTHETIC",
                "https://slack.com/archives/CSYNTHETIC",
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(
                store::postgres::projects::find_by_id_on(&first_pg, first.project)
                    .await
                    .unwrap()
                    .unwrap()
                    .internal_slack_channel_id
                    .as_deref(),
                Some("CSYNTHETIC")
            );
            assert!(
                store::postgres::projects::find_by_id_on(&second_pg, first.project)
                    .await
                    .unwrap()
                    .is_none()
            );
        })
        .await;
    })
    .await;
}

struct CheckpointBot {
    inner: workflows::CapturingSlackBot,
    first: std::sync::atomic::AtomicBool,
    posted: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl SlackBot for CheckpointBot {
    async fn find_private_channel_by_name(
        &self,
        name: &str,
    ) -> Result<Option<workflows::SlackChannel>, workflows::SlackBotError> {
        self.inner.find_private_channel_by_name(name).await
    }
    async fn create_private_channel(
        &self,
        name: &str,
    ) -> Result<workflows::SlackChannel, workflows::SlackBotError> {
        self.inner.create_private_channel(name).await
    }
    async fn post_message(
        &self,
        channel: &str,
        message: &str,
    ) -> Result<(), workflows::SlackBotError> {
        self.inner.post_message(channel, message).await?;
        if self.first.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.posted.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
    async fn user_id_by_email(
        &self,
        email: &str,
    ) -> Result<Option<String>, workflows::SlackBotError> {
        self.inner.user_id_by_email(email).await
    }
    async fn channel_member_ids(
        &self,
        channel: &str,
    ) -> Result<Vec<String>, workflows::SlackBotError> {
        self.inner.channel_member_ids(channel).await
    }
    async fn invite_to_channel(
        &self,
        channel: &str,
        users: &[String],
    ) -> Result<(), workflows::SlackBotError> {
        self.inner.invite_to_channel(channel, users).await
    }
}

#[tokio::test]
async fn executor_restart_after_provider_checkpoint_recovers_only_the_missing_receipt() {
    with_fixture(|pg,url|async move {
        let db=ApplicationStore::uninitialized();let first=matter(&db).await;
        let bot=Arc::new(CheckpointBot {inner:workflows::CapturingSlackBot::new(),first:std::sync::atomic::AtomicBool::new(true),posted:tokio::sync::Notify::new(),release:tokio::sync::Notify::new()});
        let worker=dbos::DBOS::new(workflows::dbos_engine::config(&url,"test"));
        let workflow=dbos_project_slack::register(&worker,dbos_project_slack::Context::new(db.clone(),Arc::new(SharedSlackBot(bot.clone())),pg.clone(),false)).unwrap();
        worker.launch().await.unwrap();
        let event=push("abc1234");let id=dbos_project_slack::main_push_workflow_id(first.project,&event);
        let started=workflow.start_with(LaneRequest::MainPush(event),dbos::StartOptions {workflow_id:Some(&id),..Default::default()}).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5),bot.posted.notified()).await.unwrap();
        // Block only after the provider has run but before it returns. Admission
        // already finished, and its existing provider step can still checkpoint.
        let mut blocker=pg.pool().begin().await.unwrap();
        sqlx::query("LOCK TABLE project_slack_delivery IN ACCESS EXCLUSIVE MODE").execute(&mut *blocker).await.unwrap();
        bot.release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(5),async {
            loop {
                let steps=worker.list_workflow_steps(&id).await.unwrap();
                if steps.iter().any(|step|step.step_name=="post-main-push" && step.completed_at.is_some() && step.error.is_none()) {break;}
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }).await.unwrap();
        let absent:i64=sqlx::query_scalar("SELECT count(*) FROM project_slack_delivery WHERE project_id=$1")
            .bind(first.project).fetch_one(&mut *blocker).await.unwrap();
        assert_eq!(absent,0);
        assert_eq!(bot.inner.posted_messages().len(),1);
        // Actual SDK shutdown cancels the body and leaves its run PENDING. No
        // test writes to DBOS tables or manufactures a completed workflow.
        worker.shutdown().await;
        drop(started);drop(workflow);
        let status:String=sqlx::query_scalar("SELECT status FROM dbos.workflow_status WHERE workflow_uuid=$1")
            .bind(&id).fetch_one(pg.pool()).await.unwrap();assert_eq!(status,"PENDING");
        blocker.rollback().await.unwrap();
        let replacement=dbos::DBOS::new(workflows::dbos_engine::config(&url,"test"));
        let _registered=dbos_project_slack::register(&replacement,dbos_project_slack::Context::new(db,Arc::new(SharedSlackBot(bot.clone())),pg.clone(),false)).unwrap();
        replacement.launch().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10),replacement.retrieve_workflow::<(),LaneError>(&id).unwrap().result()).await.unwrap().unwrap();
        assert_eq!(bot.inner.posted_messages().len(),1,"recovery must reuse the provider checkpoint");
        let receipts:i64=sqlx::query_scalar("SELECT count(*) FROM project_slack_delivery WHERE project_id=$1 AND event_kind='main_push' AND event_key='abc1234:1'")
            .bind(first.project).fetch_one(pg.pool()).await.unwrap();assert_eq!(receipts,1);
        let steps=replacement.list_workflow_steps(&id).await.unwrap();
        assert!(steps.iter().any(|step|step.step_name=="record-posted" && step.completed_at.is_some() && step.error.is_none()));
        replacement.shutdown().await;
    }).await;
}

#[tokio::test]
async fn workflow_metadata_preserves_canonical_project_delete_guards_and_ownership_receipt() {
    with_fixture(|pg,_|async move {
        let db=ApplicationStore::uninitialized();let first=matter(&db).await;
        let audit:i64=sqlx::query_scalar("SELECT count(*) FROM durable_object_handover").fetch_one(pg.pool()).await.unwrap();assert_eq!(audit,1);
        store::projects::delete_project(&db,first.project).await.unwrap();
        assert!(store::projects::find_by_id(&db,first.project).await.unwrap().is_none());
        let metadata:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM project_slack_handover WHERE project_id=$1)").bind(first.project).fetch_one(pg.pool()).await.unwrap();assert!(!metadata);
        let remaining:i64=sqlx::query_scalar("SELECT count(*) FROM durable_object_handover").fetch_one(pg.pool()).await.unwrap();assert_eq!(remaining,audit);
        let protected=new_project(&db,first.firm,&first.brand,first.entity).await;
        store::projects::add_participation(&db,protected,first.admin,"lawyer").await.unwrap();
        assert!(matches!(store::projects::delete_project(&db,protected).await,Err(store::projects::ProjectCommandError::Referenced(table)) if table=="person_project_roles"));
        assert!(store::projects::find_by_id(&db,protected).await.unwrap().is_some());
        let metadata:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM project_slack_handover WHERE project_id=$1)").bind(protected).fetch_one(pg.pool()).await.unwrap();assert!(metadata);
    }).await;
}
