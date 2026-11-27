//! Observe one repository's run states independently of the runner controller.

use std::time::Duration;

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, TimeDelta, Utc};
use serde::Deserialize;
use telemetry::forgejo_queue::{QueueMetrics, Snapshot};

use super::{controller::Forgejo, job_receipts::JobReceipts, runner::RunnerApi};

const INTERVAL: Duration = Duration::from_secs(20);
const PAGE_SIZE: usize = 20;
const FAILURE_WINDOW: TimeDelta = TimeDelta::minutes(15);

#[derive(Deserialize)]
struct RunPage {
    workflow_runs: Vec<Run>,
}

#[derive(Deserialize)]
struct Run {
    index_in_repo: u64,
    status: String,
    event: String,
    workflow_id: String,
    created: Option<DateTime<Utc>>,
    stopped: Option<DateTime<Utc>>,
}

async fn page(forgejo: &Forgejo, repository: &str, filters: &str) -> Result<Vec<Run>> {
    let document = forgejo
        .get(&format!(
            "/repos/{repository}/actions/runs?limit={PAGE_SIZE}&{filters}"
        ))
        .await?
        .context("the selected repository's Actions runs are unavailable")?;
    let page: RunPage =
        serde_json::from_value(document).context("parse Forgejo Actions run list")?;
    Ok(page.workflow_runs)
}

async fn read_snapshot(
    forgejo: &Forgejo,
    repository: &str,
    expected_id: u64,
    now: DateTime<Utc>,
) -> Result<Snapshot> {
    ensure!(
        forgejo.private_repository_id(repository).await? == expected_id,
        "the selected Forgejo repository changed identity"
    );
    let (waiting, failures) = tokio::try_join!(
        page(forgejo, repository, "status=waiting"),
        page(
            forgejo,
            repository,
            "status=failure&event=push&ref=refs%2Fheads%2Fmain"
        ),
    )?;
    Ok(Snapshot {
        waiting: waiting
            .into_iter()
            .take(PAGE_SIZE)
            .filter(|run| run.index_in_repo > 0 && run.status == "waiting")
            .filter_map(|run| {
                let age = (now - run.created?).to_std().ok()?.as_secs_f64();
                Some((run.index_in_repo, age))
            })
            .collect(),
        failures: failures
            .into_iter()
            .take(PAGE_SIZE)
            .filter(|run| run.index_in_repo > 0 && run.status == "failure" && run.event == "push")
            .filter_map(|run| {
                let age = now - run.stopped?;
                let workflow = match run.workflow_id.as_str() {
                    "gate.yml" => "gate.yml",
                    "cd.yml" => "cd.yml",
                    _ => return None,
                };
                (age >= TimeDelta::zero() && age < FAILURE_WINDOW)
                    .then_some((run.index_in_repo, workflow))
            })
            .collect(),
    })
}

pub(in crate::devx) fn run(url: &str, repository: &str) -> Result<()> {
    let token =
        std::env::var("FORGEJO_TOKEN").context("set the queue observer's Forgejo read token")?;
    ensure!(!token.trim().is_empty(), "the Forgejo read token is empty");
    RunnerApi::new(url, repository, token.clone())?;
    let forgejo = Forgejo::new(url, token)?;
    let _telemetry = telemetry::init("navigator-forgejo-queue-observer");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let expected_id = forgejo.private_repository_id(repository).await?;
        let metrics = QueueMetrics::new(repository);
        let mut receipts = JobReceipts::new(Utc::now());
        let mut interval = tokio::time::interval(INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tokio::select! {
            signal = tokio::signal::ctrl_c() => signal.context("listen for Ctrl-C"),
            () = async {
                loop {
                    interval.tick().await;
                    match read_snapshot(&forgejo, repository, expected_id, Utc::now()).await {
                        Ok(snapshot) => {
                            metrics.replace(snapshot);
                            if let Err(error) = receipts.observe(&forgejo, repository).await {
                                tracing::warn!(target: "forgejo", event = "forgejo.task.read_failed", error = %error, "Forgejo job receipt read failed");
                            }
                        }
                        Err(error) => {
                            tracing::warn!(target: "forgejo", event = "forgejo.queue.read_failed", error = %error, "Forgejo queue observation failed");
                            metrics.replace(Snapshot::default());
                        }
                    }
                }
            } => Ok(()),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const REPO: &str = "example/synthetic";
    const RUN_PATH: &str = "/api/v1/repos/example/synthetic/actions/runs";

    async fn api(server: &MockServer, waiting: serde_json::Value, failures: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/example/synthetic"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 42, "full_name": REPO, "private": true
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(RUN_PATH))
            .and(query_param("status", "waiting"))
            .and(query_param("limit", "20"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"workflow_runs": waiting})),
            )
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(RUN_PATH))
            .and(query_param("status", "failure"))
            .and(query_param("ref", "refs/heads/main"))
            .and(query_param("event", "push"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"workflow_runs": failures})),
            )
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn observations_use_only_repository_reads_and_clear_resolved_runs() {
        let server = MockServer::start().await;
        let forgejo = Forgejo::new(&server.uri(), "synthetic-read-token".into()).unwrap();
        let now = "2026-01-01T12:10:00Z".parse().unwrap();
        api(
            &server,
            json!([{
                "index_in_repo": 7, "status": "waiting", "event": "pull_request",
                "workflow_id": "gate.yml", "created": "2026-01-01T12:03:00Z"
            }]),
            json!([
                {"index_in_repo": 6, "status": "failure", "event": "push",
                 "workflow_id": "cd.yml", "stopped": "2026-01-01T12:02:00Z"},
                {"index_in_repo": 5, "status": "failure", "event": "pull_request",
                 "workflow_id": "gate.yml", "stopped": "2026-01-01T12:01:00Z"},
                {"index_in_repo": 4, "status": "failure", "event": "push",
                 "workflow_id": "gate.yml", "stopped": "2026-01-01T11:54:00Z"}
            ]),
        )
        .await;
        assert_eq!(
            read_snapshot(&forgejo, REPO, 42, now).await.unwrap(),
            Snapshot {
                waiting: vec![(7, 420.0)],
                failures: vec![(6, "cd.yml")]
            }
        );
        assert!(read_snapshot(&forgejo, REPO, 11, now).await.is_err());
        let requests = server.received_requests().await.unwrap();
        assert!(requests
            .iter()
            .all(|request| request.method.as_str() == "GET"
                && request
                    .url
                    .path()
                    .starts_with("/api/v1/repos/example/synthetic")));
        server.reset().await;
        api(&server, json!([]), json!([])).await;
        assert_eq!(
            read_snapshot(&forgejo, REPO, 42, now).await.unwrap(),
            Snapshot::default()
        );
    }
}
