//! Controlled authenticated-source wire -> atomic PG handover -> actual native adapters.
use std::collections::BTreeSet;
use std::future::Future;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use store::durable_object_handover::{self, Capture, HandoverError};
use store::postgres::PostgresDb;
use store::surreal::SurrealDb;
use tower::ServiceExt;
use uuid::Uuid;
use workflows::object_handover::{self, Readback};
use workflows::SlackBot;
use workflows_service::dbos_ingress::{NotationIngress, ProjectSlackIngress, WorkerIngress};
use workflows_service::dbos_notation::{self, Operation, TransitionRequest};
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

async fn matter(db: &SurrealDb) -> Matter {
    let (project, admin) = native_project::matter(db).await;
    Matter {
        project: project.id,
        firm: project.firm_id.expect("native fixture Project has a Firm"),
        admin,
        brand: project.brand,
        entity: project.entity_id,
    }
}

async fn new_project(db: &SurrealDb, firm: Uuid, brand: &str, entity: Uuid) -> Uuid {
    native_project::new_project(db, firm, brand, entity)
        .await
        .id
}

const SPEC: &str = "BEGIN:\n  _: END\nEND: {}\n";

async fn with_fixture<F, Fut>(capture_target: bool, test: F)
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
    let fixture = if capture_target {
        store::test_support::PostgresFixture::create_capture_target(options)
            .await
            .unwrap()
    } else {
        store::test_support::PostgresFixture::create(options)
            .await
            .unwrap()
    };
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

#[derive(Clone)]
struct Source {
    services: serde_json::Value,
    state: serde_json::Value,
    invocations: serde_json::Value,
    start_journal: serde_json::Value,
}

impl Source {
    fn controlled(notation: Uuid, posted_project: Uuid) -> Self {
        let mut state = Vec::new();
        let mut invocations = Vec::new();
        let mut start_journal = Vec::new();
        for kind in ["questionnaire", "workflow"] {
            for (key, value) in [
                (
                    format!("{kind}_spec_yaml"),
                    String::from_utf8(serde_json::to_vec(&SPEC.to_owned()).unwrap()).unwrap(),
                ),
                (
                    format!("{kind}_state"),
                    String::from_utf8(serde_json::to_vec(&"BEGIN".to_owned()).unwrap()).unwrap(),
                ),
            ] {
                state.push(serde_json::json!({"service_name":"notation","service_key":notation.to_string(),"key":key,"value_utf8":value,"scope":null}));
            }
            for (index, (key, value)) in [
                (format!("{kind}_spec_yaml"), SPEC),
                (format!("{kind}_state"), "BEGIN"),
            ]
            .into_iter()
            .enumerate()
            {
                let bytes = serde_json::to_vec(&value.to_owned()).unwrap();
                start_journal.push(serde_json::json!({"id":format!("synthetic-{kind}"),"index":index+2,"version":2,"entry_json":serde_json::json!({"Command":{"SetState":{"key":key,"value":bytes,"name":""}}}).to_string()}));
            }
            let payload = serde_json::to_vec(&workflows_service::StartBody {
                spec_yaml: SPEC.to_owned(),
                ephemeral: true,
            })
            .unwrap();
            invocations.push(serde_json::json!({"id":format!("synthetic-{kind}"),"target_service_name":"notation","target_service_key":notation.to_string(),"target_handler_name":format!("{kind}_start"),"status":"completed","completion_result":"success","scope":null,"pinned_deployment_id":"synthetic-deployment","version":2,"entry_json":serde_json::json!({"Command":{"Input":{"headers":[],"payload":payload,"name":""}}}).to_string()}));
        }
        state.push(serde_json::json!({"service_name":"project-slack","service_key":posted_project.to_string(),"key":"main_push_seen","value_utf8":String::from_utf8(serde_json::to_vec(&vec!["abc1234:1"]).unwrap()).unwrap(),"scope":null}));
        invocations.push(serde_json::json!({"id":"synthetic-client-view","target_service_name":"project-slack","target_service_key":posted_project.to_string(),"target_handler_name":"client_project_view","status":"completed","completion_result":"success","scope":null,"pinned_deployment_id":"synthetic-deployment","version":null,"entry_json":null}));
        for invocation in &invocations {
            if let Some(entry_json) = invocation["entry_json"].as_str() {
                start_journal.push(serde_json::json!({"id":invocation["id"],"index":0,"version":2,"entry_json":entry_json}));
            }
        }
        Self {
            services: serde_json::json!([{"name":"notation","revision":1,"deployment_id":"synthetic-deployment"},{"name":"project-slack","revision":1,"deployment_id":"synthetic-deployment"}]),
            state: state.into(),
            invocations: invocations.into(),
            start_journal: start_journal.into(),
        }
    }

    fn read(&self) -> Readback {
        object_handover::frozen_readback_with(|sql| {
            Ok(if sql.contains("COUNT(*)") && sql.contains("FROM state") {
                serde_json::json!([{"count":self.state.as_array().unwrap().len()}]).to_string()
            } else if sql.contains("COUNT(*)") && sql.contains("FROM sys_journal") {
                serde_json::json!([{"count":self.start_journal.as_array().unwrap().len()}])
                    .to_string()
            } else if sql.contains("COUNT(*)") {
                serde_json::json!([{"count":self.invocations.as_array().unwrap().len()}])
                    .to_string()
            } else if sql.contains("FROM sys_service") {
                self.services.to_string()
            } else if sql.contains("FROM state") {
                self.state.to_string()
            } else if sql.contains("FROM sys_journal") {
                self.start_journal.to_string()
            } else if sql.contains("FROM sys_invocation") {
                self.invocations.to_string()
            } else {
                return Err(object_handover::CaptureError(
                    "unexpected controlled source query",
                ));
            })
        })
        .unwrap()
    }

    async fn capture(&self, pg: &PostgresDb) -> Capture {
        let inventory: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM project ORDER BY id")
            .fetch_all(pg.pool())
            .await
            .unwrap();
        object_handover::map_capture(
            &self.read(),
            "synthetic",
            Uuid::new_v4(),
            Uuid::new_v4(),
            &inventory.into_iter().collect::<BTreeSet<_>>(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap()
    }

    async fn activate(&self, pg: &PostgresDb, capture: &Capture) -> Result<(), HandoverError> {
        durable_object_handover::activate(pg, capture, || async {
            let mapped = object_handover::map_capture(
                &self.read(),
                "synthetic",
                capture.capture_id,
                capture.ownership_epoch,
                &capture
                    .slack_projects
                    .iter()
                    .map(|row| row.project_id)
                    .collect(),
                capture.captured_at_unix,
            )
            .map_err(|_| HandoverError::InvalidCapture)?;
            if mapped != *capture {
                return Err(HandoverError::Conflict);
            }
            Ok(())
        })
        .await
    }
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

async fn state_status(router: &axum::Router, id: Uuid, door: &str) -> StatusCode {
    router
        .clone()
        .oneshot(
            Request::post(format!("/notation/{id}/{door}"))
                .header("authorization", "Bearer synthetic-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

async fn slack_status(router: &axum::Router, id: &str) -> StatusCode {
    router
        .clone()
        .oneshot(
            Request::post("/internal/dbos/project-slack")
                .header("authorization", "Bearer synthetic-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"workflow_id":id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

async fn missing_slack_workflow(worker: &dbos::DBOS, id: &str) {
    let result = worker
        .retrieve_workflow::<(), LaneError>(id)
        .unwrap()
        .status()
        .await;
    assert!(
        matches!(result, Err(dbos::Error::WorkflowNotFound { .. })),
        "admission refusal must not poison the stable workflow identity"
    );
}

#[tokio::test]
async fn captured_objects_activate_only_after_parity_and_run_native_without_source() {
    with_fixture(true,|pg,url|async move {
        let db=SurrealDb::uninitialized();
        let first=matter(&db).await;let second=new_project(&db,first.firm,&first.brand,first.entity).await;
        let notation=Uuid::new_v4();let source=Source::controlled(notation,first.project);let capture=source.capture(&pg).await;
        assert!(durable_object_handover::require_native_ownership(&pg).await.is_err());
        let preview=durable_object_handover::import(&pg,&capture,false).await.unwrap();
        assert!(!preview.committed && !preview.native_activation);
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM notation_machine_state").fetch_one(pg.pool()).await.unwrap();assert_eq!(count,0);
        let receipt=durable_object_handover::import(&pg,&capture,true).await.unwrap();
        let replay=durable_object_handover::import(&pg,&capture,true).await.unwrap();assert_eq!(receipt,replay);
        assert_eq!((receipt.notation_machines,receipt.slack_projects,receipt.slack_posted_events),(2,2,2));
        assert!(durable_object_handover::require_native_ownership(&pg).await.is_err());
        let worker=dbos::DBOS::new(workflows::dbos_engine::config(&url,"test"));
        let bot=Arc::new(workflows::CapturingSlackBot::new());
        let slack=dbos_project_slack::register(&worker,dbos_project_slack::Context::new(db.clone(),Arc::new(SharedSlackBot(bot.clone())),pg.clone(),false)).unwrap();
        let directory=tempfile::tempdir().unwrap();let storage=Arc::new(cloud::FsStorage::new(directory.path()).await.unwrap());
        let notation_workflow=dbos_notation::register(&worker,dbos_notation::Context::new(db.clone(),pg.clone(),Arc::new(workflows::CapturingEmail::default()),storage)).unwrap();
        worker.launch().await.unwrap();
        let router=workflows_service::health::router_with_dbos_ingress(WorkerIngress {notation:Some(NotationIngress::new("synthetic-token".to_owned(),worker.clone(),pg.clone(),notation_workflow.clone())),project_slack:Some(ProjectSlackIngress::new("synthetic-token".to_owned(),slack.clone(),worker.clone(),db.clone(),pg.clone())),..Default::default()},None);
        for door in ["workflow_current_state","workflow_events"] {assert_eq!(state_status(&router,notation,door).await,StatusCode::SERVICE_UNAVAILABLE);}
        let stable_client=dbos_project_slack::client_view_workflow_id(first.project);
        assert_eq!(slack_status(&router,&stable_client).await,StatusCode::SERVICE_UNAVAILABLE);
        missing_slack_workflow(&worker,&stable_client).await;
        let approval_id=format!("NotationApproval:{notation}:1");
        let approval=router.clone().oneshot(Request::post(format!("/notation/{notation}/workflow_approve"))
            .header("authorization","Bearer synthetic-token").header("content-type","application/json")
            .body(Body::from(serde_json::json!({"review_revision":1,"value":"approve","acting_person_id":null}).to_string())).unwrap()).await.unwrap();
        assert_eq!(approval.status(),StatusCode::SERVICE_UNAVAILABLE);
        assert!(matches!(worker.retrieve_workflow::<dbos_notation::TransitionReport,dbos_notation::NotationTransitionError>(&approval_id).unwrap().status().await,Err(dbos::Error::WorkflowNotFound {..})));
        source.activate(&pg,&capture).await.unwrap();
        durable_object_handover::verify_runtime_ownership(&pg).await.unwrap();
        assert_eq!(slack_status(&router,&stable_client).await,StatusCode::ACCEPTED);
        worker.retrieve_workflow::<(),LaneError>(&stable_client).unwrap().result().await.unwrap();
        assert_eq!(state_status(&router,notation,"workflow_events").await,StatusCode::OK);
        assert_eq!(state_status(&router,notation,"workflow_current_state").await,StatusCode::OK);
        for request in [LaneRequest::MainPush(push("abc1234")),LaneRequest::ClientView] {
            let id=match &request {LaneRequest::MainPush(event)=>dbos_project_slack::main_push_workflow_id(first.project,event),LaneRequest::ClientView=>dbos_project_slack::client_view_workflow_id(first.project)};
            slack.start_with(request,dbos::StartOptions {workflow_id:Some(&id),..Default::default()}).await.unwrap().result().await.unwrap();
        }
        assert!(bot.posted_messages().is_empty());
        let unresolved=dbos_project_slack::client_view_workflow_id(second);
        assert_eq!(slack_status(&router,&unresolved).await,StatusCode::CONFLICT);
        missing_slack_workflow(&worker,&unresolved).await;
        assert!(matches!(store::project_slack_deliveries::admission(&pg,second,"client_view","client-view").await,Err(store::project_slack_deliveries::DeliveryError::Unresolved)));
        assert!(bot.posted_messages().is_empty());
        // Fail exactly the first receipt insert. A sequence survives rollback;
        // the existing provider checkpoint must not run again on PG-only retry.
        sqlx::raw_sql("CREATE SEQUENCE synthetic_receipt_attempt; CREATE FUNCTION synthetic_receipt_failure() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('synthetic_receipt_attempt')=1 THEN RAISE EXCEPTION 'synthetic receipt outage'; END IF; RETURN NEW; END $$; CREATE TRIGGER synthetic_receipt_failure BEFORE INSERT ON project_slack_delivery FOR EACH ROW EXECUTE FUNCTION synthetic_receipt_failure();").execute(pg.pool()).await.unwrap();
        let fresh=push("def5678");let id=dbos_project_slack::main_push_workflow_id(first.project,&fresh);
        slack.start_with(LaneRequest::MainPush(fresh.clone()),dbos::StartOptions {workflow_id:Some(&id),..Default::default()}).await.unwrap().result().await.unwrap();
        slack.start_with(LaneRequest::MainPush(fresh),dbos::StartOptions {workflow_id:Some(&id),..Default::default()}).await.unwrap().result().await.unwrap();
        assert_eq!(bot.posted_messages().len(),1);
        let attempts:i64=sqlx::query_scalar("SELECT last_value FROM synthetic_receipt_attempt").fetch_one(pg.pool()).await.unwrap();assert_eq!(attempts,2);
        let rows:i64=sqlx::query_scalar("SELECT count(*) FROM project_slack_delivery WHERE project_id=$1 AND event_key='def5678:1'").bind(first.project).fetch_one(pg.pool()).await.unwrap();assert_eq!(rows,1);
        let steps=worker.list_workflow_steps(&id).await.unwrap();assert!(steps.len()>=3,"destination, provider and receipt checkpoints are distinct");
        notation_workflow.start(TransitionRequest {kind:workflows::MachineKind::Workflow,notation_id:notation,operation:Operation::Signal {condition:"_".to_owned(),value:None,acting_person_id:None,ephemeral:true}}).await.unwrap().result().await.unwrap();
        let state=store::notation_machine_state::read(&pg,notation,"workflow").await.unwrap().unwrap();assert_eq!(state.current_state,"END");
        assert!(durable_object_handover::import(&pg,&capture,true).await.is_err(),"native progress must not be overwritten by exact capture replay");
        let future=new_project(&db,first.firm,&first.brand,first.entity).await;
        assert!(store::project_slack_deliveries::admission(&pg,future,"client_view","client-view").await.is_ok());
        let future_id=dbos_project_slack::client_view_workflow_id(future);
        slack.start_with(LaneRequest::ClientView,dbos::StartOptions {workflow_id:Some(&future_id),..Default::default()}).await.unwrap().result().await.unwrap();
        assert_eq!(bot.posted_messages().len(),2,"a genuinely new native Project needs no manual handover");
        worker.shutdown().await;
    }).await;
}

async fn counts(pg: &PostgresDb) -> (i64, i64) {
    (
        sqlx::query_scalar("SELECT count(*) FROM durable_object_handover")
            .fetch_one(pg.pool())
            .await
            .unwrap(),
        sqlx::query_scalar("SELECT count(*) FROM notation_machine_state")
            .fetch_one(pg.pool())
            .await
            .unwrap(),
    )
}

#[tokio::test]
async fn pending_native_start_and_partial_interruption_cannot_leave_a_half_import() {
    with_fixture(true,|pg,_|async move {
        let db=SurrealDb::uninitialized();let first=matter(&db).await;
        let notation=Uuid::new_v4();let source=Source::controlled(notation,first.project);let capture=source.capture(&pg).await;
        let operation=Uuid::new_v4();
        store::notation_machine_state::operations::admit(&pg,operation,notation,"synthetic-start-fingerprint").await.unwrap();
        assert!(matches!(durable_object_handover::import(&pg,&capture,true).await,Err(HandoverError::PendingNativeOperation)));
        assert_eq!(counts(&pg).await,(0,0));
        store::notation_machine_state::operations::discard_unstarted(&pg,operation).await.unwrap();
        // The importer writes both machines before reaching this relation. Wait
        // for the actual lock request before interrupting its owning future.
        let mut blocker=pg.pool().begin().await.unwrap();
        sqlx::query("LOCK TABLE project_slack_handover IN ACCESS EXCLUSIVE MODE").execute(&mut *blocker).await.unwrap();
        let target=pg.clone();let captured=capture.clone();
        let importing=tokio::spawn(async move {durable_object_handover::import(&target,&captured,true).await});
        tokio::time::timeout(std::time::Duration::from_secs(5),async {
            loop {
                let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='relation' AND relation='project_slack_handover'::regclass AND database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND NOT granted)")
                    .fetch_one(pg.pool()).await.unwrap();
                if waiting {break;}
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }).await.unwrap();
        importing.abort();assert!(importing.await.unwrap_err().is_cancelled());
        blocker.rollback().await.unwrap();
        // Retrying the exact capture waits for the interrupted transaction's
        // rollback, then creates one coherent result without repairing half rows.
        let receipt=durable_object_handover::import(&pg,&capture,true).await.unwrap();
        assert_eq!(counts(&pg).await,(1,2));
        assert_eq!(durable_object_handover::import(&pg,&capture,true).await.unwrap(),receipt);
        assert!(durable_object_handover::require_native_ownership(&pg).await.is_err());
    }).await;
}

#[tokio::test]
async fn native_drift_unknown_keys_and_inventory_changes_refuse_without_overwrite() {
    with_fixture(true, |pg, _| async move {
        let db = SurrealDb::uninitialized();
        let first = matter(&db).await;
        let notation = Uuid::new_v4();
        let source = Source::controlled(notation, first.project);
        let capture = source.capture(&pg).await;
        store::notation_machine_state::start_machine(&pg, notation, "workflow", SPEC, true)
            .await
            .unwrap();
        store::notation_machine_state::apply_transition(&pg, notation, "workflow", "BEGIN", "END")
            .await
            .unwrap();
        assert!(matches!(
            durable_object_handover::import(&pg, &capture, true).await,
            Err(HandoverError::Conflict)
        ));
        assert_eq!(
            counts(&pg).await,
            (0, 1),
            "the first machine and capture header rolled back"
        );
        assert!(
            store::notation_machine_state::read(&pg, notation, "questionnaire")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store::notation_machine_state::read(&pg, notation, "workflow")
                .await
                .unwrap()
                .unwrap()
                .current_state,
            "END"
        );
        let mut unknown = source.clone();
        unknown.state[0]["key"] = "unknown".into();
        assert!(object_handover::map_capture(
            &unknown.read(),
            "synthetic",
            capture.capture_id,
            capture.ownership_epoch,
            &BTreeSet::from([first.project]),
            capture.captured_at_unix
        )
        .is_err());
        assert_eq!(counts(&pg).await, (0, 1));
    })
    .await;
    with_fixture(true, |pg, _| async move {
        let db = SurrealDb::uninitialized();
        let first = matter(&db).await;
        let source = Source::controlled(Uuid::new_v4(), first.project);
        let capture = source.capture(&pg).await;
        durable_object_handover::import(&pg, &capture, true)
            .await
            .unwrap();
        let mut changed = source.clone();
        changed.services[0]["deployment_id"] = "different-source".into();
        assert!(changed.activate(&pg, &capture).await.is_err());
        assert!(durable_object_handover::require_native_ownership(&pg)
            .await
            .is_err());
        new_project(&db, first.firm, &first.brand, first.entity).await;
        assert!(source.activate(&pg, &capture).await.is_err());
        assert!(durable_object_handover::require_native_ownership(&pg)
            .await
            .is_err());
        assert_eq!(counts(&pg).await, (1, 2));
    })
    .await;
}

async fn connect_slack(db: &SurrealDb, matter: &Matter, kms: &dyn cloud::RuntimeKms, value: &str) {
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
    with_fixture(false, |pg, _| async move {
        let db = SurrealDb::uninitialized();
        let first = matter(&db).await;
        let kms = Arc::new(cloud::FakeKms::new("synthetic-key-version"));
        connect_slack(&db, &first, kms.as_ref(), "synthetic-first-token").await;
        let first_pg = pg.clone();
        with_fixture(false, move |second_pg, _| async move {
            let db = SurrealDb::uninitialized();
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

#[tokio::test]
async fn uncaptured_native_machine_or_pending_start_refuses_global_activation() {
    with_fixture(true, |pg, _| async move {
        let db = SurrealDb::uninitialized();
        let first = matter(&db).await;
        let source = Source::controlled(Uuid::new_v4(), first.project);
        let capture = source.capture(&pg).await;
        durable_object_handover::import(&pg, &capture, true)
            .await
            .unwrap();
        let outside = Uuid::new_v4();
        store::notation_machine_state::start_machine(&pg, outside, "workflow", SPEC, true)
            .await
            .unwrap();
        assert!(matches!(
            source.activate(&pg, &capture).await,
            Err(HandoverError::Conflict)
        ));
        assert_eq!(counts(&pg).await, (1, 3));
        assert!(durable_object_handover::require_native_ownership(&pg)
            .await
            .is_err());
        assert_eq!(
            store::notation_machine_state::read(&pg, outside, "workflow")
                .await
                .unwrap()
                .unwrap()
                .current_state,
            "BEGIN"
        );
    })
    .await;
    with_fixture(true, |pg, _| async move {
        let db = SurrealDb::uninitialized();
        let first = matter(&db).await;
        let source = Source::controlled(Uuid::new_v4(), first.project);
        let capture = source.capture(&pg).await;
        durable_object_handover::import(&pg, &capture, true)
            .await
            .unwrap();
        let outside = Uuid::new_v4();
        let operation = Uuid::new_v4();
        store::notation_machine_state::operations::admit(
            &pg,
            operation,
            outside,
            "synthetic-uncaptured-start",
        )
        .await
        .unwrap();
        assert!(matches!(
            source.activate(&pg, &capture).await,
            Err(HandoverError::PendingNativeOperation)
        ));
        assert!(
            store::notation_machine_state::read(&pg, outside, "workflow")
                .await
                .unwrap()
                .is_none()
        );
        let pending: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM notation_transition WHERE id=$1 AND result IS NULL)",
        )
        .bind(operation)
        .fetch_one(pg.pool())
        .await
        .unwrap();
        assert!(pending);
        assert_eq!(counts(&pg).await, (1, 2));
        assert!(durable_object_handover::require_native_ownership(&pg)
            .await
            .is_err());
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
    with_fixture(false,|pg,url|async move {
        let db=SurrealDb::uninitialized();let first=matter(&db).await;
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
async fn workflow_metadata_preserves_canonical_project_delete_guards_and_capture_audit() {
    with_fixture(false,|pg,_|async move {
        let db=SurrealDb::uninitialized();let first=matter(&db).await;
        let audit:i64=sqlx::query_scalar("SELECT count(*) FROM durable_object_handover").fetch_one(pg.pool()).await.unwrap();assert_eq!(audit,1);
        store::projects::delete_project_with_surreal(&db,first.project).await.unwrap();
        assert!(store::projects::find_by_id(&db,first.project).await.unwrap().is_none());
        let metadata:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM project_slack_handover WHERE project_id=$1)").bind(first.project).fetch_one(pg.pool()).await.unwrap();assert!(!metadata);
        let remaining:i64=sqlx::query_scalar("SELECT count(*) FROM durable_object_handover").fetch_one(pg.pool()).await.unwrap();assert_eq!(remaining,audit);
        let protected=new_project(&db,first.firm,&first.brand,first.entity).await;
        store::projects::add_participation(&db,protected,first.admin,"lawyer").await.unwrap();
        assert!(matches!(store::projects::delete_project_with_surreal(&db,protected).await,Err(store::projects::ProjectCommandError::Referenced(table)) if table=="person_project_roles"));
        assert!(store::projects::find_by_id(&db,protected).await.unwrap().is_some());
        let metadata:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM project_slack_handover WHERE project_id=$1)").bind(protected).fetch_one(pg.pool()).await.unwrap();assert!(metadata);
    }).await;
}

// These are the real pinned SDK serialization boundaries used by captured
// Notation state/start commands and Project Slack keyed state. Native migration
// fixtures stay SDK-free; the compatibility lane checks identical persisted bytes.

/// The controlled wire fixture passes the production capture parser, import,
/// and activation parity checks before two native callers can be selected.
#[tokio::test]
async fn captured_object_epoch_is_admitted_and_invalid_or_unavailable_epochs_refuse() {
    with_fixture(true, |pg, _url| async move {
        let db = SurrealDb::uninitialized();
        let first = matter(&db).await;
        let source = Source::controlled(Uuid::new_v4(), first.project);
        let capture = source.capture(&pg).await;
        let receipt = durable_object_handover::import(&pg, &capture, true).await.unwrap();
        assert!(receipt.committed && !receipt.native_activation);
        source.activate(&pg, &capture).await.unwrap();
        let provenance: (String, bool) = sqlx::query_as(
            "SELECT h.origin,EXISTS(SELECT 1 FROM database_origin) FROM durable_object_ownership o JOIN durable_object_handover h ON h.capture_id=o.capture_id WHERE o.singleton",
        ).fetch_one(pg.pool()).await.unwrap();
        assert_eq!(provenance, ("captured_source".into(), false));
        durable_object_handover::verify_runtime_ownership(&pg).await.unwrap();
        sqlx::query("UPDATE durable_object_ownership SET ownership_epoch=$1")
            .bind(Uuid::nil())
            .execute(pg.pool())
            .await
            .unwrap();
        assert!(
            durable_object_handover::verify_runtime_ownership(&pg)
                .await
                .is_err()
        );
        pg.pool().close().await;
        assert!(
            durable_object_handover::verify_runtime_ownership(&pg)
                .await
                .is_err()
        );
    }).await;
}
