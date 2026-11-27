//! Retained Cron routes authenticate against the real native executor.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use workflows::dbos_engine;
use workflows_service::dbos_ingress::{NightlyIngress, WorkerIngress, YoutubeIngress};
use workflows_service::dbos_nightly_project_update::{self as nightly, Providers};
use workflows_service::nightly_project_update::repository::RepositoryForge;
use workflows_service::youtube_comments::{self as youtube, YoutubeCommentsPollerService};

#[path = "support/application_process.rs"]
mod application_process;

const DEADLINE: Duration = Duration::from_secs(90);

#[tokio::test]
async fn retained_cron_routes_authenticate_and_replay_on_native_dbos() {
    let Some(database) = application_process::database().await else {
        return;
    };
    exercise_routes(&database).await.unwrap();
}

async fn exercise_routes(database: &application_process::ApplicationDatabase) -> Result<()> {
    let pg = database.store();
    let executor = dbos::DBOS::new(dbos_engine::config_with_executor_id(
        &database.url,
        "native-cron-test",
        "native-cron-test",
    ));
    let slack = Arc::new(workflows::CapturingSlackBot::new());
    let inert = store::application_store::ApplicationStore::uninitialized();
    let nightly = nightly::register(
        &executor,
        nightly::Context::new(
            inert.clone(),
            slack.clone(),
            Arc::new(workflows_service::firm_slack::SharedSlackBot(slack.clone())),
            "native-cron-test".into(),
            true,
            Providers {
                repository_forge: RepositoryForge::None,
                runtime_version: "native-cron-test".into(),
                portal_origin: None,
            },
        ),
    )?;
    let youtube = youtube::register_dbos(
        &executor,
        YoutubeCommentsPollerService::new(inert, Arc::new(cloud::NoRuntimeKms)),
    )?;
    executor.launch().await?;
    let token = uuid::Uuid::new_v4().to_string();
    let router = workflows_service::health::router_with_dbos_ingress(
        WorkerIngress {
            nightly_project_update: Some(NightlyIngress::new(token.clone(), nightly)),
            youtube_comments: Some(YoutubeIngress {
                token: token.clone().into(),
                workflow: youtube,
            }),
            ..Default::default()
        },
        None,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let stop = dbos::CancellationToken::new();
    let stopped = stop.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(stopped.cancelled_owned())
            .await
    });
    let result = verify_routes(&origin, &token, &executor, &slack, pg).await;
    stop.cancel();
    server.await??;
    executor.shutdown().await;
    pg.pool().close().await;
    result
}

async fn verify_routes(
    origin: &str,
    token: &str,
    executor: &dbos::DBOS,
    slack: &workflows::CapturingSlackBot,
    pg: &store::postgres::PostgresDb,
) -> Result<()> {
    let client = reqwest::Client::new();
    let nightly_id = nightly::workflow_id("2026-11-19");
    let youtube_id = "YoutubeCommentsPoller:2026-11-19-05-45";
    let routes = [
        (
            "/internal/dbos/nightly-project-update",
            serde_json::json!({"workflow_id": nightly_id}),
        ),
        (
            "/internal/dbos/youtube-comments-poll",
            serde_json::json!({"tick":"2026-11-19-05-45"}),
        ),
    ];
    for (path, body) in &routes {
        for credential in [None, Some("incorrect")] {
            let mut request = client.post(format!("{origin}{path}")).json(body);
            if let Some(credential) = credential {
                request = request.bearer_auth(credential);
            }
            assert_eq!(
                request.send().await?.status(),
                reqwest::StatusCode::UNAUTHORIZED
            );
        }
    }
    let unauthorized_runs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM dbos.workflow_status WHERE workflow_uuid = ANY($1)",
    )
    .bind([nightly_id.as_str(), youtube_id])
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(unauthorized_runs, 0);
    for replay in [false, true] {
        let notices = slack.posted_messages().len();
        for (path, body) in &routes {
            assert_eq!(
                client
                    .post(format!("{origin}{path}"))
                    .bearer_auth(token)
                    .json(body)
                    .send()
                    .await?
                    .status(),
                reqwest::StatusCode::ACCEPTED
            );
        }
        let nightly = tokio::time::timeout(DEADLINE,
            executor.retrieve_workflow::<workflows_service::nightly_project_update::NightlyProjectUpdateReport,
                nightly::NightlyError>(&nightly_id)?.result()).await??;
        let youtube = tokio::time::timeout(
            DEADLINE,
            executor
                .retrieve_workflow::<youtube::PollReport, youtube::PollError>(youtube_id)?
                .result(),
        )
        .await??;
        assert_eq!(nightly.projects, 0);
        assert_eq!(youtube.videos, 0);
        if replay {
            assert_eq!(slack.posted_messages().len(), notices);
        }
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM dbos.workflow_status WHERE workflow_uuid = ANY($1)",
        )
        .bind([nightly_id.as_str(), youtube_id])
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(count, 2);
    }
    Ok(())
}
