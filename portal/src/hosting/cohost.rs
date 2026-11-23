//! Coordinate the application and the complete workflow host in one process.

use anyhow::Context;
use dbos::CancellationToken;
use std::{future::Future, sync::Arc};
use tokio::sync::oneshot;
use workflows::dbos_engine;

/// Select the complete workflow host without changing individual workflow ownership.
pub(super) fn enabled(
    host_dbos: bool,
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<bool> {
    let enabled = workflows::flags::flag_enabled(
        "NAVIGATOR_WORKFLOWS_IN_WEB",
        lookup("NAVIGATOR_WORKFLOWS_IN_WEB").as_deref(),
    )?;
    if enabled {
        anyhow::ensure!(
            host_dbos && dbos_engine::enabled(&lookup)?,
            "NAVIGATOR_WORKFLOWS_IN_WEB requires NAVIGATOR_DBOS_ENABLED=true"
        );
        anyhow::ensure!(
            workflows_service::engine::Engine::from_lookup(&lookup)?
                == workflows_service::engine::Engine::Dbos,
            "NAVIGATOR_WORKFLOWS_IN_WEB requires NAVIGATOR_WORKFLOW_ENGINE=dbos"
        );
    }
    Ok(enabled)
}

#[cfg(test)]
mod combined_workflow_tests {
    use super::enabled;
    #[test]
    fn combined_host_requires_both_explicit_dbos_selections() {
        let lookup = |values: &[(&str, &str)]| {
            let values: Vec<_> = values
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect();
            move |name: &str| {
                values
                    .iter()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value.clone())
            }
        };
        assert!(!enabled(true, |_| None).unwrap());
        assert!(enabled(true, lookup(&[("NAVIGATOR_WORKFLOWS_IN_WEB", "typo")])).is_err());
        let configured = [
            ("NAVIGATOR_WORKFLOWS_IN_WEB", "true"),
            ("NAVIGATOR_DBOS_ENABLED", "true"),
            ("NAVIGATOR_WORKFLOW_ENGINE", "dbos"),
        ];
        assert!(enabled(true, lookup(&configured)).unwrap());
        assert!(enabled(false, lookup(&configured)).is_err());
        assert!(enabled(true, lookup(&configured[..2])).is_err());
    }
}

#[allow(clippy::too_many_arguments)] // One boot's hosting choices and transports.
pub(super) async fn run(
    host_dbos: bool,
    combined: bool,
    ops_notifier: Arc<dyn workflows::Notifier>,
    dbos_schedules: dbos_engine::Schedules,
    sent_email_export: Option<Arc<dyn archives::sent_email_export::ExportSource>>,
    archives: Option<archives::workflow::DbosContext>,
    stop: CancellationToken,
    serve: impl Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    if combined {
        telemetry::register_nightly_project_update();
        self::serve(stop, super::shutdown_signal(), serve, move |stop, ready| {
            workflows_service::host::run_embedded(
                async move {
                    stop.cancelled().await;
                    Ok(())
                },
                dbos_schedules,
                move |dbos| super::register_archive_schedules(dbos, sent_email_export, archives),
                ready,
            )
        })
        .await
    } else if host_dbos {
        let lookup = |key: &str| std::env::var(key).ok();
        super::with_dbos_hosting_and_external(
            lookup,
            ops_notifier,
            dbos_schedules,
            move |dbos| super::register_archive_schedules(dbos, sent_email_export, archives),
            self::serve_http(stop, serve, super::shutdown_signal()),
        )
        .await
    } else {
        self::serve_http(stop, serve, super::shutdown_signal()).await
    }
}

pub(super) async fn serve_http(
    stop: CancellationToken,
    serve: impl Future<Output = anyhow::Result<()>>,
    signal: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    tokio::pin!(serve);
    tokio::select! {
        result = &mut serve => result,
        () = signal => {
            stop.cancel();
            serve.await
        }
    }
}

pub(super) async fn serve<W, F>(
    stop: CancellationToken,
    signal: impl Future<Output = ()>,
    serve: impl Future<Output = anyhow::Result<()>>,
    worker: F,
) -> anyhow::Result<()>
where
    W: Future<Output = anyhow::Result<()>>,
    F: FnOnce(CancellationToken, oneshot::Sender<()>) -> W,
{
    let (ready, started) = oneshot::channel();
    let worker_stop = CancellationToken::new();
    let worker = worker(worker_stop.clone(), ready);
    tokio::pin!(worker, serve, signal);
    // HTTP never accepts traffic until the sole executor has launched.
    tokio::select! {
        biased;
        result = &mut worker => return result.and_then(|()| Err(anyhow::anyhow!("workflow host stopped before readiness"))),
        result = started => {
            if let Err(error) = result {
                worker_stop.cancel();
                return worker.await.and_then(|()| Err(error.into()));
            }
        },
        () = &mut signal => {
            worker_stop.cancel();
            return worker.await;
        }
    }
    tokio::select! {
        result = &mut worker => {
            stop.cancel();
            let drained = serve.await.context("drain application listener");
            result.and(drained).and_then(|()| Err(anyhow::anyhow!("workflow host stopped")))
        },
        result = &mut serve => {
            stop.cancel();
            worker_stop.cancel();
            let drained = worker.await.context("drain workflow host");
            result.and(drained)
        },
        () = &mut signal => {
            stop.cancel();
            // In-flight app requests may still call authenticated workflow ingress.
            // Keep that listener and the executor alive until the app has drained.
            let app = serve.await;
            worker_stop.cancel();
            app.and(worker.await)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn disabled_workflow_host_serves_without_an_executor() {
        let served = Arc::new(AtomicBool::new(false));
        let observed = served.clone();
        Box::pin(run(
            false,
            false,
            Arc::new(workflows::CapturingNotifier::new()),
            dbos_engine::Schedules::default(),
            None,
            None,
            CancellationToken::new(),
            async move {
                observed.store(true, Ordering::SeqCst);
                Ok(())
            },
        ))
        .await
        .unwrap();
        assert!(served.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn startup_failure_never_polls_the_application() {
        let served = Arc::new(AtomicBool::new(false));
        let observed = served.clone();
        let result = serve(
            CancellationToken::new(),
            std::future::pending(),
            async move {
                observed.store(true, Ordering::SeqCst);
                Ok(())
            },
            |_, _| async { anyhow::bail!("registration failed") },
        )
        .await;
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("registration failed"));
        assert!(!served.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn application_failure_cancels_and_joins_the_worker() {
        let drained = Arc::new(AtomicBool::new(false));
        let observed = drained.clone();
        let result = serve(
            CancellationToken::new(),
            std::future::pending(),
            async { anyhow::bail!("listener failed") },
            move |stop, ready| async move {
                ready.send(()).unwrap();
                stop.cancelled().await;
                observed.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .await;
        assert!(result.unwrap_err().to_string().contains("listener failed"));
        assert!(drained.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn shutdown_drains_both_hosts_after_readiness() {
        let stop = CancellationToken::new();
        let app_stop = stop.clone();
        let (signal, shutdown) = oneshot::channel();
        let app_drained = Arc::new(AtomicBool::new(false));
        let worker_drained = Arc::new(AtomicBool::new(false));
        let app = app_drained.clone();
        let worker = worker_drained.clone();
        let app_before_worker = app_drained.clone();
        serve(
            stop,
            async {
                shutdown.await.unwrap();
            },
            async move {
                signal.send(()).unwrap();
                app_stop.cancelled().await;
                app.store(true, Ordering::SeqCst);
                Ok(())
            },
            move |stop, ready| async move {
                ready.send(()).unwrap();
                stop.cancelled().await;
                assert!(
                    app_before_worker.load(Ordering::SeqCst),
                    "workflow ingress stays up until the app drains"
                );
                worker.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .unwrap();
        assert!(app_drained.load(Ordering::SeqCst));
        assert!(worker_drained.load(Ordering::SeqCst));
    }
}
