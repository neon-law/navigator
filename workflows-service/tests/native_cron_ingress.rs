//! Retained Cron routes authenticate against the real native executor.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use store::test_support::PostgresFixture;
use workflows::dbos_engine;
use workflows_service::dbos_ingress::{NightlyIngress, WorkerIngress, YoutubeIngress};
use workflows_service::dbos_nightly_project_update::{self as nightly, Providers};
use workflows_service::nightly_project_update::repository::RepositoryForge;
use workflows_service::youtube_comments::{self as youtube, YoutubeCommentsPollerService};

const CHILD: &str = "NAVIGATOR_NATIVE_CRON_PROBE";
const CASE: &str = "retained_cron_routes_authenticate_and_replay_on_native_dbos";
const DEADLINE: Duration = Duration::from_secs(90);

#[tokio::test]
async fn retained_cron_routes_authenticate_and_replay_on_native_dbos() {
    if std::env::var_os(CHILD).is_some() {
        exercise_routes().await.unwrap();
    } else {
        isolated_executor().await.unwrap();
    }
}

async fn isolated_executor() -> Result<()> {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert_ne!(std::env::var("NAV_REQUIRE_POSTGRES").as_deref(), Ok("1"));
        return Ok(());
    };
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port.parse()?)
        .username("postgres")
        .password("synthetic")
        .database("postgres")
        .ssl_mode(PgSslMode::Disable)
        .options([("session_preload_libraries", "age")]);
    let fixture = PostgresFixture::create(options).await?;
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    command
        .args(["--exact", CASE, "--nocapture"])
        .env(CHILD, "1");
    for prefix in ["NAVIGATOR_POSTGRES", "NAVIGATOR_DBOS"] {
        for (key, value) in [
            ("HOST", "127.0.0.1"),
            ("PORT", port.as_str()),
            ("USER", "postgres"),
            ("PASSWORD", "synthetic"),
            ("DATABASE", fixture.database()),
            ("SSLMODE", "disable"),
        ] {
            command.env(format!("{prefix}_{key}"), value);
        }
    }
    command
        .env("NAVIGATOR_DBOS_EXECUTOR_ID", "native-cron-test")
        .env("NAVIGATOR_RELEASE_TAG", "native-cron-test")
        .env("NAV_REQUIRE_POSTGRES", "1")
        .env("NAVIGATOR_FORBID_SURREAL", "1")
        .kill_on_drop(true);
    let outcome = run_child(command).await;
    let cleanup = fixture.close().await;
    outcome?;
    cleanup
}

async fn run_child(mut command: tokio::process::Command) -> Result<()> {
    let mut child = command
        .spawn()
        .context("start isolated native Cron executor")?;
    let status = if let Ok(status) = tokio::time::timeout(DEADLINE, child.wait()).await {
        status?
    } else {
        child.kill().await?;
        child.wait().await?;
        anyhow::bail!("isolated native Cron executor exceeded its deadline");
    };
    anyhow::ensure!(status.success(), "isolated native Cron executor failed");
    Ok(())
}

async fn exercise_routes() -> Result<()> {
    let pg = store::postgres::PostgresDb::application().await?;
    let settings = dbos_engine::Settings::from_lookup(|key| std::env::var(key).ok())?;
    let executor = dbos::DBOS::new(settings.config());
    let slack = Arc::new(workflows::CapturingSlackBot::new());
    let inert = store::surreal::SurrealDb::uninitialized();
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
    let result = verify_routes(&origin, &token, &executor, &slack, &pg).await;
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
