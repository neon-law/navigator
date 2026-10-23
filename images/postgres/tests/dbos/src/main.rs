use dbos::{Config, DBOS, step};
use std::time::Duration;

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[error("{0}")]
struct SmokeError(String);

async fn write_effect(key: &str) -> dbos::Result<(), SmokeError> {
    let url = std::env::var("DBOS_DATABASE_URL").map_err(|e| SmokeError(e.to_string()))?;
    let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
        .await
        .map_err(|e| SmokeError(e.to_string()))?;
    let connection_task = tokio::spawn(connection);
    client
        .execute("INSERT INTO public.attempts(key) VALUES($1)", &[&key])
        .await
        .map_err(|e| SmokeError(e.to_string()))?;
    let result = client
        .execute(
            "INSERT INTO public.effects(key) VALUES($1) ON CONFLICT DO NOTHING",
            &[&key],
        )
        .await
        .map_err(|e| SmokeError(e.to_string()));
    drop(client);
    connection_task
        .await
        .map_err(|e| SmokeError(e.to_string()))?
        .map_err(|e| SmokeError(e.to_string()))?;
    result?;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut config = Config::from_env("image-smoke");
    config.app_version = Some("smoke-v1".into());
    config.executor_id = Some("smoke-worker".into());
    let dbos = DBOS::new(config);
    let workflow = dbos.register_workflow("checkpoint-smoke", |_: ()| async {
        step("checkpoint-one", || write_effect("checkpoint-one")).await?;
        step("business-write", || async {
            write_effect("business-write").await?;
            // Test controller kills the process after this commit and before DBOS checkpoint.
            if std::env::var("INJECT_CRASH_WINDOW").is_ok() {
                tokio::time::sleep(Duration::from_secs(300)).await;
            }
            Ok::<(), dbos::Error<SmokeError>>(())
        })
        .await?;
        dbos::sleep(Duration::from_secs(2)).await?;
        Ok::<(), dbos::Error<SmokeError>>(())
    })?;
    dbos.launch().await?;
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().route("/ready", axum::routing::get(|| async { "ready" })),
        )
        .await
    });
    let handle = workflow
        .start_with(
            (),
            dbos::StartOptions {
                workflow_id: Some("stable-smoke"),
                ..Default::default()
            },
        )
        .await?;
    handle.result().await?;
    dbos.shutdown().await;
    server.abort();
    Ok(())
}
