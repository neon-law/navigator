//! DBOS consumer for an isolated Project maintenance runner.
use anyhow::{Context, Result};
use dbos::{EnqueueOptions, QueueConflict, QueueOptions, DBOS};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::{io::AsyncWriteExt, process::Command};
use workflows::dbos_engine::{
    release_maintenance::{Job, APP, RELEASE_QUEUE, REPAIR_QUEUE},
    Settings,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Repair {
    job: Job,
    diagnostic: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, thiserror::Error)]
#[error("maintenance runner did not complete: {0}")]
struct Failure(String);

async fn run(tool: &Path, mode: &str, input: &impl Serialize) -> Result<bool, String> {
    let bytes = serde_json::to_vec(input).map_err(|_| "invalid job")?;
    let mut child = Command::new(tool)
        .arg(mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "runner unavailable")?;
    let mut stdin = child.stdin.take().ok_or("runner stdin unavailable")?;
    stdin
        .write_all(&bytes)
        .await
        .map_err(|_| "runner stdin closed")?;
    drop(stdin);
    let status = tokio::time::timeout(Duration::from_secs(7200), child.wait())
        .await
        .map_err(|_| "runner timeout")?
        .map_err(|_| "runner exited unexpectedly")?;
    if status.success() {
        Ok(true)
    } else if mode == "update" && status.code() == Some(1) {
        Ok(false)
    } else {
        Err(format!(
            "{} exit {}",
            mode,
            status
                .code()
                .map_or_else(|| "signal".into(), |v| v.to_string())
        ))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let runner = PathBuf::from(
        std::env::var_os("NAVIGATOR_MAINTENANCE_RUNNER")
            .context("set the absolute path of the installed maintenance runner")?,
    );
    anyhow::ensure!(
        runner.is_absolute() && runner.is_file(),
        "maintenance runner must be an installed executable"
    );
    let mut config = Settings::from_lookup(|k| std::env::var(k).ok())?.config();
    config.app_name = APP.into();
    config.listen_queues = Some(vec![RELEASE_QUEUE.into(), REPAIR_QUEUE.into()]);
    let dbos = DBOS::new(config);
    let fallback = runner.clone();
    dbos.register_workflow("RepairProjectRelease", move |input: Repair| {
        let tool = fallback.clone();
        async move {
            dbos::step("repair-and-verify-release", || async {
                run(&tool, "repair", &input)
                    .await
                    .map(|_| ())
                    .map_err(|e| dbos::Error::Application(Failure(e)))
            })
            .await
        }
    })?;
    let engine = dbos.clone();
    dbos.register_workflow("ApplyProjectRelease", move |job: Job| {
        let engine = engine.clone();
        let tool = runner.clone();
        async move {
            let complete = dbos::step("apply-and-verify-release", || async {
                run(&tool, "update", &job)
                    .await
                    .map_err(|e| dbos::Error::Application(Failure(e)))
            })
            .await?;
            if !complete {
                let key = job.key("repair");
                let handle = engine
                    .enqueue_with::<_, (), Failure>(
                        "RepairProjectRelease",
                        Repair {
                            job,
                            diagnostic: "release checks failed".into(),
                        },
                        EnqueueOptions {
                            workflow_id: Some(&key),
                            app_name: Some(APP),
                            ..EnqueueOptions::new(REPAIR_QUEUE)
                        },
                    )
                    .await?;
                handle.result().await?;
            }
            Ok::<(), dbos::Error<Failure>>(())
        }
    })?;
    dbos.launch().await?;
    for name in [RELEASE_QUEUE, REPAIR_QUEUE] {
        dbos.register_queue(
            name,
            QueueOptions {
                concurrency: Some(1),
                ..Default::default()
            },
            QueueConflict::UpdateIfLatestVersion,
        )
        .await?;
    }
    tokio::signal::ctrl_c().await?;
    dbos.shutdown().await;
    Ok(())
}
