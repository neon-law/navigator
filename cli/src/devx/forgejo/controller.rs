//! `navigator ops forgejo runner-controller`: start one disposable,
//! repository-scoped runner for every queued Forgejo Actions job, with no
//! operator (ENG-1236).
//!
//! Each tick the controller:
//!
//! 1. recovers leftovers — finished runner Jobs no live launch owns, and
//!    `forgejo-job-*` registrations with no Job for two ticks in a row;
//! 2. lists waiting jobs installation-wide through the admin API, mirrors the
//!    release each one pins into Forgejo's package registry, and
//! 3. launches exactly enough runners: per repository, the waiting jobs minus
//!    this process's runners that have not picked a job up yet, under one
//!    global cap.
//!
//! Every launch is the same code `runner-once` runs ([`runner::execute`]):
//! a per-Job service account without an API token, a read-only root
//! filesystem, no host or Docker socket, `/32` egress to the Forgejo and
//! Navigator hosts only, a deadline and a TTL. The operator token stays in
//! this process; a job pod only ever holds its own one-job credential.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::Value;

use super::release::{release_version, Registry};
use super::runner::{
    self, is_runner_job_name, Cluster, RunnerApi, REGISTRATION_ANNOTATION, REPOSITORY_ANNOTATION,
};

/// The label generated workflows ask for (`runs-on: self-hosted`).
const RUNNER_LABEL: &str = "self-hosted";
/// A finished, unowned Job is left alone this long, so an operator's own
/// `runner-once` can clean up after itself first.
const ORPHAN_GRACE: Duration = Duration::from_mins(2);

/// How the controller is started.
pub(in crate::devx) struct Options {
    pub(in crate::devx) url: String,
    pub(in crate::devx) runner_namespace: String,
    /// `None` in-cluster: the pod's own service account.
    pub(in crate::devx) context: Option<String>,
    pub(in crate::devx) egress_host: Vec<String>,
    pub(in crate::devx) egress_ip: Vec<String>,
    pub(in crate::devx) dns_ip: String,
    pub(in crate::devx) interval: Duration,
    pub(in crate::devx) max_runners: usize,
    pub(in crate::devx) once: bool,
}

#[derive(Debug, Deserialize)]
struct WaitingJob {
    repo_id: u64,
    #[serde(default)]
    status: String,
}

#[derive(Debug, Deserialize)]
struct AdminRunner {
    id: u64,
    #[serde(default)]
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(in crate::devx) struct Run {
    #[serde(default)]
    pub(in crate::devx) commit_sha: String,
}

/// The installation-wide reads the controller makes with the operator token.
pub(in crate::devx) struct Forgejo {
    client: reqwest::Client,
    origin: String,
    token: String,
}

impl Forgejo {
    pub(in crate::devx) fn new(origin: &str, token: String) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            origin: origin.trim_end_matches('/').to_owned(),
            token,
        })
    }

    pub(in crate::devx) async fn get(&self, path: &str) -> Result<Option<Value>> {
        let response = self
            .client
            .get(format!("{}/api/v1{path}", self.origin))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("reach the Forgejo API")?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure!(
            response.status().is_success(),
            "Forgejo API read refused ({})",
            response.status()
        );
        Ok(Some(response.json().await?))
    }

    /// Repository id → count of jobs waiting for a `self-hosted` runner.
    async fn waiting(&self) -> Result<BTreeMap<u64, usize>> {
        let jobs: Vec<WaitingJob> = serde_json::from_value(
            self.get(&format!(
                "/admin/actions/runners/jobs?labels={RUNNER_LABEL}"
            ))
            .await?
            .unwrap_or(Value::Array(Vec::new())),
        )
        .context("read waiting Actions jobs")?;
        let mut waiting = BTreeMap::new();
        for job in jobs
            .into_iter()
            .filter(|job| job.status.is_empty() || job.status == "waiting")
        {
            *waiting.entry(job.repo_id).or_default() += 1;
        }
        Ok(waiting)
    }

    /// `owner/name` of repository `id`.
    async fn repository(&self, id: u64) -> Result<String> {
        let repository = self
            .get(&format!("/repositories/{id}"))
            .await?
            .context("a waiting job's repository no longer exists")?;
        let name = repository["full_name"]
            .as_str()
            .context("repository has no full name")?
            .to_owned();
        runner::repository_scope(&name)?;
        Ok(name)
    }

    /// The most recent runs of `repository`, optionally only in `status`.
    pub(in crate::devx) async fn runs(
        &self,
        repository: &str,
        status: Option<&str>,
    ) -> Result<Vec<Value>> {
        let filter = status
            .map(|status| format!("&status={status}"))
            .unwrap_or_default();
        let listed = self
            .get(&format!(
                "/repos/{repository}/actions/runs?limit=20{filter}"
            ))
            .await?
            .unwrap_or(Value::Null);
        Ok(listed["workflow_runs"]
            .as_array()
            .cloned()
            .unwrap_or_default())
    }

    /// The release `navigator.yaml` pins at `sha`, when it pins an exact one.
    async fn pinned_version(&self, repository: &str, sha: &str) -> Result<Option<String>> {
        ensure!(
            sha.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "run commit is not a SHA"
        );
        let response = self
            .client
            .get(format!(
                "{}/api/v1/repos/{repository}/raw/navigator.yaml?ref={sha}",
                self.origin
            ))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let manifest: serde_yaml::Value =
            serde_yaml::from_str(&response.text().await?).unwrap_or_default();
        Ok(manifest["version"]
            .as_str()
            .map(str::trim)
            .filter(|version| release_version(version).is_ok())
            .map(str::to_owned))
    }

    async fn runner_registrations(&self) -> Result<Vec<AdminRunner>> {
        Ok(serde_json::from_value(
            self.get("/admin/actions/runners?limit=50")
                .await?
                .unwrap_or(Value::Array(Vec::new())),
        )?)
    }

    async fn remove_registration(&self, id: u64) -> Result<()> {
        let response = self
            .client
            .delete(format!("{}/api/v1/admin/actions/runners/{id}", self.origin))
            .bearer_auth(&self.token)
            .send()
            .await?;
        ensure!(
            matches!(
                response.status(),
                StatusCode::NO_CONTENT | StatusCode::NOT_FOUND
            ),
            "orphaned runner registration cleanup refused ({})",
            response.status()
        );
        Ok(())
    }
}

/// One live launch this process owns.
#[derive(Debug, Clone)]
struct Launch {
    repository: String,
    registration: Option<u64>,
}

/// Every launch this process owns, keyed by Job name.
#[derive(Default)]
struct InFlight(HashMap<String, Launch>);

/// How many runners to start for each repository this tick: its waiting jobs
/// minus this process's runners for it that have not picked a job up yet,
/// with the total held under `max_runners`. Repositories are served in name
/// order, one runner each per round, so one busy repository cannot starve
/// the rest.
fn plan(
    waiting: &BTreeMap<String, usize>,
    idle: &BTreeMap<String, usize>,
    in_flight: usize,
    max_runners: usize,
) -> Vec<String> {
    let mut wanted: BTreeMap<&String, usize> = waiting
        .iter()
        .map(|(repository, count)| {
            (
                repository,
                count.saturating_sub(idle.get(repository).copied().unwrap_or(0)),
            )
        })
        .filter(|(_, count)| *count > 0)
        .collect();
    let mut capacity = max_runners.saturating_sub(in_flight);
    let mut launches = Vec::new();
    while capacity > 0 && !wanted.is_empty() {
        for (repository, count) in &mut wanted {
            if capacity == 0 {
                break;
            }
            if *count > 0 {
                launches.push((*repository).clone());
                *count -= 1;
                capacity -= 1;
            }
        }
        wanted.retain(|_, count| *count > 0);
    }
    launches
}

/// Whether a runner Job has finished, and when it finished.
fn finished_at(job: &Value) -> Option<chrono::DateTime<chrono::Utc>> {
    job["status"]["conditions"]
        .as_array()?
        .iter()
        .find(|condition| {
            matches!(condition["type"].as_str(), Some("Complete" | "Failed"))
                && condition["status"] == "True"
        })
        .and_then(|condition| condition["lastTransitionTime"].as_str())
        .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
        .map(|time| time.with_timezone(&chrono::Utc))
}

pub(in crate::devx) struct Controller {
    cluster: Arc<dyn Cluster>,
    forgejo: Forgejo,
    registry: Registry,
    origin: String,
    token: String,
    egress_host: Vec<String>,
    egress_ip: Vec<String>,
    dns: Ipv4Addr,
    max_runners: usize,
    in_flight: Arc<Mutex<InFlight>>,
    repositories: HashMap<u64, String>,
    mirrored: HashSet<String>,
    unowned_registrations: BTreeSet<u64>,
    observer: Observer,
    runtime: tokio::runtime::Runtime,
}

/// What the controller remembers between ticks to report each run once.
pub(in crate::devx) struct Observer {
    started: chrono::DateTime<chrono::Utc>,
    /// Every repository seen, so a repository whose queue empties reports 0.
    watched: BTreeSet<String>,
    /// Repositories that carry `.forgejo/workflows/cd.yml` on `main`.
    with_cd: HashMap<String, bool>,
    last_refresh: Option<Instant>,
    queued: HashSet<(String, u64)>,
    running: HashSet<(String, u64)>,
    finished: HashSet<(String, u64)>,
}

impl Observer {
    pub(in crate::devx) fn new() -> Self {
        Self {
            started: chrono::Utc::now(),
            watched: BTreeSet::new(),
            with_cd: HashMap::new(),
            last_refresh: None,
            queued: HashSet::new(),
            running: HashSet::new(),
            finished: HashSet::new(),
        }
    }
}

/// A Forgejo timestamp, or `None` for its zero time (`0001-01-01…`).
fn forgejo_time(value: &Value) -> Option<chrono::DateTime<chrono::Utc>> {
    let time = chrono::DateTime::parse_from_rfc3339(value.as_str()?).ok()?;
    (time.timestamp() > 0).then(|| time.with_timezone(&chrono::Utc))
}

fn seconds_between(
    from: Option<chrono::DateTime<chrono::Utc>>,
    to: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<f64> {
    #[allow(clippy::cast_precision_loss)]
    let seconds = (to? - from?).num_milliseconds() as f64 / 1000.0;
    (seconds >= 0.0).then_some(seconds)
}

/// The run's workflow file name: Forgejo's `workflow_id` (`cd.yml`).
fn workflow_of(run: &Value) -> &str {
    run["workflow_id"].as_str().unwrap_or("unknown")
}

const FINISHED: &[&str] = &["success", "failure", "cancelled", "skipped"];
/// A merge whose CD run has not started after this long is a missed CD.
const CD_GRACE: chrono::TimeDelta = chrono::TimeDelta::minutes(15);
/// Heads older than this are history, not a missed CD.
const CD_HORIZON: chrono::TimeDelta = chrono::TimeDelta::hours(24);

/// The pure part of observing one repository's recent runs: which runs are
/// newly queued, started or finished, and how long the oldest has waited.
#[derive(Debug, Default, PartialEq)]
struct RunChanges {
    oldest_waiting_seconds: f64,
    queued: Vec<u64>,
    started: Vec<u64>,
    finished: Vec<u64>,
}

fn run_changes(
    repository: &str,
    runs: &[Value],
    observer: &Observer,
    now: chrono::DateTime<chrono::Utc>,
) -> RunChanges {
    let mut changes = RunChanges::default();
    for run in runs {
        let Some(id) = run["id"].as_u64() else {
            continue;
        };
        let key = (repository.to_owned(), id);
        let status = run["status"].as_str().unwrap_or_default();
        if status == "waiting" {
            if let Some(waited) = seconds_between(forgejo_time(&run["created"]), Some(now)) {
                changes.oldest_waiting_seconds = changes.oldest_waiting_seconds.max(waited);
            }
            if !observer.queued.contains(&key) {
                changes.queued.push(id);
            }
        } else if status == "running" && !observer.running.contains(&key) {
            changes.started.push(id);
        } else if FINISHED.contains(&status)
            && !observer.finished.contains(&key)
            // A restart reports only what finished after it started.
            && forgejo_time(&run["stopped"]).is_some_and(|stopped| stopped >= observer.started)
        {
            changes.finished.push(id);
        }
    }
    changes
}

/// Whether `main`'s head has gone `CD_GRACE` without a CD run.
fn cd_missed(
    head_sha: &str,
    head_time: Option<chrono::DateTime<chrono::Utc>>,
    runs: &[Value],
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let Some(head_time) = head_time else {
        return false;
    };
    let age = now - head_time;
    age > CD_GRACE
        && age < CD_HORIZON
        && !runs
            .iter()
            .any(|run| run["commit_sha"].as_str() == Some(head_sha) && workflow_of(run) == "cd.yml")
}

impl Controller {
    fn in_flight(&self) -> std::sync::MutexGuard<'_, InFlight> {
        self.in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Remove what an earlier process or an interrupted launch left behind.
    /// Returns how many leftovers were removed.
    fn recover(&mut self) -> Result<usize> {
        let owned: HashSet<String> = self.in_flight().0.keys().cloned().collect();
        let jobs = self.cluster.runner_jobs()?;
        let job_names: HashSet<String> = jobs
            .iter()
            .filter_map(|job| job["metadata"]["name"].as_str().map(str::to_owned))
            .collect();
        let mut removed = 0;
        for job in &jobs {
            let name = job["metadata"]["name"].as_str().unwrap_or_default();
            if owned.contains(name) || !is_runner_job_name(name) {
                continue;
            }
            let Some(done) = finished_at(job) else {
                continue;
            };
            if chrono::Utc::now()
                .signed_duration_since(done)
                .to_std()
                .unwrap_or_default()
                < ORPHAN_GRACE
            {
                continue;
            }
            let annotations = &job["metadata"]["annotations"];
            let registration = annotations[REGISTRATION_ANNOTATION]
                .as_str()
                .and_then(|id| id.parse::<u64>().ok());
            let repository = annotations[REPOSITORY_ANNOTATION].as_str();
            if let (Some(id), Some(repository)) = (registration, repository) {
                let api = RunnerApi::new(&self.origin, repository, self.token.clone())?;
                runner::cleanup(self.cluster.as_ref(), &api, &self.runtime, name, id)?;
            } else {
                self.cluster.delete_job(name)?;
            }
            tracing::warn!(target: "forgejo", event = "forgejo.runner.orphan_cleaned", job = name, "removed a finished runner Job no launch owned");
            removed += 1;
        }
        // A registration without a Job: the launch stopped between
        // registering and creating its Job. Seen twice, so a launch in
        // progress elsewhere is never raced.
        let registrations = self.runtime.block_on(self.forgejo.runner_registrations())?;
        let mut unowned = BTreeSet::new();
        for registration in registrations {
            if is_runner_job_name(&registration.name)
                && !job_names.contains(&registration.name)
                && !owned.contains(&registration.name)
            {
                if self.unowned_registrations.contains(&registration.id) {
                    self.runtime
                        .block_on(self.forgejo.remove_registration(registration.id))?;
                    tracing::warn!(target: "forgejo", event = "forgejo.runner.orphan_cleaned", registration = registration.id, "removed a runner registration with no Job");
                    removed += 1;
                } else {
                    unowned.insert(registration.id);
                }
            }
        }
        self.unowned_registrations = unowned;
        Ok(removed)
    }

    /// Per repository, this process's runners that have not started a job.
    fn idle(&self) -> BTreeMap<String, usize> {
        let launches: Vec<Launch> = self.in_flight().0.values().cloned().collect();
        let mut idle = BTreeMap::new();
        for launch in launches {
            let busy = match launch.registration {
                // Still registering: it has not picked anything up.
                None => false,
                Some(id) => RunnerApi::new(&self.origin, &launch.repository, self.token.clone())
                    .ok()
                    .and_then(|api| self.runtime.block_on(api.busy(id)).ok().flatten())
                    .unwrap_or(true),
            };
            if !busy {
                *idle.entry(launch.repository).or_default() += 1;
            }
        }
        idle
    }

    /// Mirror the release each waiting run of `repository` pins.
    fn mirror_pinned(&mut self, repository: &str) {
        let runs = self
            .runtime
            .block_on(self.forgejo.runs(repository, Some("waiting")))
            .unwrap_or_default();
        for run in runs {
            let Ok(run) = serde_json::from_value::<Run>(run) else {
                continue;
            };
            let Ok(Some(version)) = self
                .runtime
                .block_on(self.forgejo.pinned_version(repository, &run.commit_sha))
            else {
                continue;
            };
            if self.mirrored.contains(&version) {
                continue;
            }
            match self.runtime.block_on(self.registry.ensure(&version)) {
                Ok(_) => {
                    self.mirrored.insert(version);
                }
                Err(error) => {
                    tracing::warn!(target: "forgejo", event = "forgejo.release.mirror_failed", version = %version, error = %error, "release mirror failed; the job will report it");
                }
            }
        }
    }

    /// The installation's repositories, refreshed every ten minutes, and
    /// whether each carries the CD workflow on `main`.
    fn refresh_repositories(&mut self) {
        if self
            .observer
            .last_refresh
            .is_some_and(|last| last.elapsed() < Duration::from_mins(10))
        {
            return;
        }
        self.observer.last_refresh = Some(Instant::now());
        for page in 1..=5 {
            let Ok(Some(found)) = self.runtime.block_on(
                self.forgejo
                    .get(&format!("/repos/search?limit=50&page={page}")),
            ) else {
                break;
            };
            let names: Vec<String> = found["data"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|repository| repository["full_name"].as_str().map(str::to_owned))
                .filter(|name| runner::repository_scope(name).is_ok())
                .collect();
            let done = names.len() < 50;
            self.observer.watched.extend(names);
            if done {
                break;
            }
        }
        let watched: Vec<String> = self.observer.watched.iter().cloned().collect();
        for repository in watched {
            let has_cd = self
                .runtime
                .block_on(self.forgejo.get(&format!(
                    "/repos/{repository}/contents/.forgejo/workflows/cd.yml?ref=main"
                )))
                .is_ok_and(|found| found.is_some());
            self.observer.with_cd.insert(repository, has_cd);
        }
    }

    /// Report every watched repository's queue, finished runs and CD
    /// coverage. Errors on one repository never stop the others.
    fn observe(&mut self) {
        self.refresh_repositories();
        let now = chrono::Utc::now();
        let watched: Vec<String> = self.observer.watched.iter().cloned().collect();
        for repository in watched {
            let Ok(runs) = self.runtime.block_on(self.forgejo.runs(&repository, None)) else {
                continue;
            };
            let changes = run_changes(&repository, &runs, &self.observer, now);
            for run in &runs {
                let Some(id) = run["id"].as_u64() else {
                    continue;
                };
                let number = run["index_in_repo"].as_u64().unwrap_or_default();
                let event = run["event"].as_str().unwrap_or("unknown");
                let workflow = workflow_of(run);
                let key = (repository.clone(), id);
                if changes.queued.contains(&id) {
                    tracing::info!(target: "forgejo", event = "forgejo.job.queued", forgejo.run_number = number, forgejo.repository = %repository, forgejo.workflow = workflow, forgejo.event = event, "Forgejo run queued");
                    self.observer.queued.insert(key.clone());
                }
                if changes.started.contains(&id) {
                    let queue = seconds_between(
                        forgejo_time(&run["created"]),
                        forgejo_time(&run["started"]),
                    );
                    tracing::info!(target: "forgejo", event = "forgejo.job.started", forgejo.run_number = number, forgejo.repository = %repository, forgejo.workflow = workflow, forgejo.event = event, queue_seconds = queue.unwrap_or_default(), "Forgejo run started");
                    self.observer.running.insert(key.clone());
                }
                if changes.finished.contains(&id) {
                    let outcome = run["status"].as_str().unwrap_or("unknown");
                    let started = forgejo_time(&run["started"]);
                    let queue = seconds_between(forgejo_time(&run["created"]), started);
                    let duration = seconds_between(started, forgejo_time(&run["stopped"]));
                    telemetry::record_forgejo_run_finished(&telemetry::ForgejoRunFinished {
                        repository: &repository,
                        workflow,
                        event,
                        outcome,
                        queue_seconds: queue,
                        duration_seconds: duration,
                    });
                    let failed_on_main = outcome == "failure" && event != "pull_request";
                    if failed_on_main {
                        tracing::error!(target: "forgejo", event = "forgejo.job.finished", forgejo.run_number = number, forgejo.repository = %repository, forgejo.workflow = workflow, forgejo.event = event, outcome = outcome, duration_seconds = duration.unwrap_or_default(), "Forgejo run failed on main");
                    } else {
                        tracing::info!(target: "forgejo", event = "forgejo.job.finished", forgejo.run_number = number, forgejo.repository = %repository, forgejo.workflow = workflow, forgejo.event = event, outcome = outcome, duration_seconds = duration.unwrap_or_default(), "Forgejo run finished");
                    }
                    self.observer.finished.insert(key.clone());
                    self.observer.queued.remove(&key);
                    self.observer.running.remove(&key);
                }
            }
            let missed = self
                .observer
                .with_cd
                .get(&repository)
                .copied()
                .unwrap_or(false)
                && {
                    let branch = self
                        .runtime
                        .block_on(
                            self.forgejo
                                .get(&format!("/repos/{repository}/branches/main")),
                        )
                        .ok()
                        .flatten()
                        .unwrap_or(Value::Null);
                    branch["commit"]["id"].as_str().is_some_and(|sha| {
                        cd_missed(
                            sha,
                            forgejo_time(&branch["commit"]["timestamp"]),
                            &runs,
                            now,
                        )
                    })
                };
            telemetry::record_forgejo_repository(
                &repository,
                changes.oldest_waiting_seconds,
                missed,
            );
        }
    }

    /// One pass: recover, read the queue, mirror, launch.
    pub(in crate::devx) fn tick(&mut self) -> Result<TickReport> {
        // Recovery never blocks launching: a leftover costs a slot, a missed
        // launch costs a pull request.
        let orphans_cleaned = self.recover().unwrap_or_else(|error| {
            tracing::warn!(target: "forgejo", event = "forgejo.runner.recovery_failed", error = %error, "runner recovery failed this tick");
            0
        });
        let by_id = self.runtime.block_on(self.forgejo.waiting())?;
        let mut waiting = BTreeMap::new();
        for (id, count) in by_id {
            let repository = if let Some(name) = self.repositories.get(&id) {
                name.clone()
            } else {
                let name = self.runtime.block_on(self.forgejo.repository(id))?;
                self.repositories.insert(id, name.clone());
                name
            };
            waiting.insert(repository, count);
        }
        for repository in waiting.keys() {
            self.mirror_pinned(repository);
            self.observer.watched.insert(repository.clone());
        }
        let ips = runner::egress_destinations(&self.egress_ip, &self.egress_host)?;
        let in_flight = self.in_flight().0.len();
        let launches = plan(&waiting, &self.idle(), in_flight, self.max_runners);
        for repository in &launches {
            self.launch(repository.clone(), ips.clone());
        }
        self.observe();
        Ok(TickReport {
            waiting: waiting.values().sum(),
            launched: launches.len(),
            orphans_cleaned,
        })
    }

    /// Start one runner for `repository` on its own thread. Errors are
    /// reported, never fatal to the controller.
    fn launch(&self, repository: String, ips: Vec<Ipv4Addr>) {
        let name = format!("forgejo-job-{}", uuid::Uuid::new_v4().simple());
        self.in_flight().0.insert(
            name.clone(),
            Launch {
                repository: repository.clone(),
                registration: None,
            },
        );
        let cluster = Arc::clone(&self.cluster);
        let in_flight = Arc::clone(&self.in_flight);
        let origin = self.origin.clone();
        let token = self.token.clone();
        let dns = self.dns;
        std::thread::spawn(move || {
            let outcome = launch_one(
                cluster.as_ref(),
                &in_flight,
                &name,
                &repository,
                &origin,
                token,
                &ips,
                dns,
            );
            if let Err(error) = &outcome {
                tracing::warn!(target: "forgejo", event = "forgejo.runner.launch_failed", job = %name, error = %error, "runner launch failed");
            }
            in_flight
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0
                .remove(&name);
        });
    }
}

/// What one tick saw and did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(in crate::devx) struct TickReport {
    pub(in crate::devx) waiting: usize,
    pub(in crate::devx) launched: usize,
    pub(in crate::devx) orphans_cleaned: usize,
}

#[allow(clippy::too_many_arguments)]
fn launch_one(
    cluster: &dyn Cluster,
    in_flight: &Mutex<InFlight>,
    name: &str,
    repository: &str,
    origin: &str,
    token: String,
    ips: &[Ipv4Addr],
    dns: Ipv4Addr,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let api = RunnerApi::new(origin, repository, token)?;
    runner::require_isolated(cluster, name)?;
    let registration = runtime.block_on(api.register(name))?;
    if let Some(launch) = in_flight
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .0
        .get_mut(name)
    {
        launch.registration = Some(registration.id);
    }
    let started = Instant::now();
    let result = runner::execute(cluster, name, repository, origin, ips, dns, &registration);
    // The pod goes with its Job, so its log is read before cleanup.
    match cluster.job_log(name) {
        Ok(log) => forward_job_log(repository, name, &log),
        Err(error) => {
            tracing::warn!(target: "forgejo", event = "forgejo.runner.log_unavailable", job = name, error = %error, "runner log could not be read");
        }
    }
    tracing::info!(
        target: "forgejo",
        event = "forgejo.runner.finished",
        forgejo.repository = repository,
        job = name,
        outcome = if result.is_ok() { "success" } else { "failure" },
        duration_seconds = started.elapsed().as_secs(),
        "one-job runner finished"
    );
    let cleanup = runner::cleanup(cluster, &api, &runtime, name, registration.id);
    cleanup.and(result)
}

/// Re-emit a finished runner's stdout, one record per line, so a job's log
/// reaches the collector (and from it Dash0 and Cloud Logging) with the
/// repository and runner Job attached. Each line passes the telemetry
/// crate's content screen like any other record; a line that looks like
/// client content is dropped there, and `navigator ops forgejo logs` still
/// reads the whole log from Forgejo.
fn forward_job_log(repository: &str, job: &str, log: &str) {
    for line in log.lines().filter(|line| !line.trim().is_empty()) {
        tracing::info!(target: "forgejo.job.log", job = job, forgejo.repository = repository, "{line}");
    }
}

/// `navigator ops forgejo runner-controller`.
pub(in crate::devx) fn run(options: &Options) -> Result<()> {
    let _telemetry = telemetry::init("navigator-forgejo-runner-controller");
    ensure!(
        options.max_runners > 0 && options.max_runners <= 32,
        "--max-runners must be between 1 and 32"
    );
    ensure!(
        !options.egress_host.is_empty() || !options.egress_ip.is_empty(),
        "name the Forgejo and Navigator hosts with --egress-host"
    );
    let token = std::env::var("FORGEJO_TOKEN")
        .context("FORGEJO_TOKEN must hold the environment's Forgejo operator token")?;
    let cluster: Arc<dyn Cluster> = if let Some(context) = &options.context {
        runner::validate_namespace(context, &options.runner_namespace)?;
        Arc::new(super::cluster::Kube::selected(
            context,
            &options.runner_namespace,
        ))
    } else {
        runner::validate_namespace("in-cluster", &options.runner_namespace)?;
        Arc::new(super::kubeapi::InCluster::from_pod(
            &options.runner_namespace,
        )?)
    };
    // Validates the origin the same way `runner-once` does.
    RunnerApi::new(&options.url, "validate/origin", token.clone())?;
    let mut controller = Controller {
        cluster,
        forgejo: Forgejo::new(&options.url, token.clone())?,
        registry: Registry::new(&options.url, token.clone())?,
        origin: options.url.clone(),
        token,
        egress_host: options.egress_host.clone(),
        egress_ip: options.egress_ip.clone(),
        dns: runner::dns_address(&options.dns_ip)?,
        max_runners: options.max_runners,
        in_flight: Arc::default(),
        repositories: HashMap::new(),
        mirrored: HashSet::new(),
        unowned_registrations: BTreeSet::new(),
        observer: Observer::new(),
        runtime: tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?,
    };
    telemetry::register_forgejo_controller();
    loop {
        let result = controller.tick();
        telemetry::record_forgejo_tick(
            if result.is_ok() { "ok" } else { "error" },
            result
                .as_ref()
                .map_or(0, |report| report.orphans_cleaned as u64),
        );
        match result {
            Ok(report) => tracing::info!(
                target: "forgejo",
                event = "forgejo.controller.tick",
                outcome = "ok",
                waiting = report.waiting,
                launched = report.launched,
                orphans_cleaned = report.orphans_cleaned,
                "runner controller tick"
            ),
            Err(error) => {
                tracing::error!(target: "forgejo", event = "forgejo.controller.tick", outcome = "error", error = %error, "runner controller tick failed");
                if options.once {
                    return Err(error);
                }
            }
        }
        if options.once {
            // Wait for this tick's launches so `--once` is a whole pass.
            while !controller.in_flight().0.is_empty() {
                std::thread::sleep(Duration::from_secs(2));
            }
            return Ok(());
        }
        std::thread::sleep(options.interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn counts(pairs: &[(&str, usize)]) -> BTreeMap<String, usize> {
        pairs
            .iter()
            .map(|(name, count)| ((*name).to_owned(), *count))
            .collect()
    }

    #[test]
    fn a_waiting_job_gets_exactly_one_runner_even_across_ticks() {
        // First tick: one waiting job, nothing in flight.
        assert_eq!(
            plan(&counts(&[("o/a", 1)]), &counts(&[]), 0, 4),
            vec!["o/a"]
        );
        // Next tick before the runner picks it up: the idle runner covers it.
        assert!(plan(&counts(&[("o/a", 1)]), &counts(&[("o/a", 1)]), 1, 4).is_empty());
    }

    #[test]
    fn the_global_cap_is_shared_fairly_across_repositories() {
        let launches = plan(
            &counts(&[("o/a", 5), ("o/b", 1), ("o/c", 2)]),
            &counts(&[]),
            1,
            4,
        );
        assert_eq!(launches, vec!["o/a", "o/b", "o/c"]);
        assert!(plan(&counts(&[("o/a", 5)]), &counts(&[]), 4, 4).is_empty());
    }

    #[test]
    fn only_finished_jobs_have_a_finish_time() {
        let done = json!({"status": {"conditions": [
            {"type": "Complete", "status": "True", "lastTransitionTime": "2026-11-05T10:00:00Z"}
        ]}});
        assert!(finished_at(&done).is_some());
        let failed = json!({"status": {"conditions": [
            {"type": "Failed", "status": "True", "lastTransitionTime": "2026-11-05T10:00:00Z"}
        ]}});
        assert!(finished_at(&failed).is_some());
        assert!(finished_at(&json!({"status": {"active": 1}})).is_none());
        let suspended = json!({"status": {"conditions": [
            {"type": "Suspended", "status": "True", "lastTransitionTime": "2026-11-05T10:00:00Z"}
        ]}});
        assert!(finished_at(&suspended).is_none());
    }

    fn at(time: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(time)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn each_run_is_reported_queued_started_and_finished_once() {
        let mut observer = Observer::new();
        observer.started = at("2026-11-05T09:00:00Z");
        let now = at("2026-11-05T10:10:00Z");
        let runs = vec![
            json!({"id": 1, "status": "waiting", "created": "2026-11-05T10:00:00Z"}),
            json!({"id": 2, "status": "running", "created": "2026-11-05T09:58:00Z", "started": "2026-11-05T09:59:00Z"}),
            json!({"id": 3, "status": "failure", "stopped": "2026-11-05T10:05:00Z"}),
            // Finished before this process started: never replayed.
            json!({"id": 4, "status": "success", "stopped": "2026-11-05T08:00:00Z"}),
        ];
        let changes = run_changes("o/a", &runs, &observer, now);
        assert!((changes.oldest_waiting_seconds - 600.0).abs() < f64::EPSILON);
        assert_eq!(
            (changes.queued, changes.started, changes.finished),
            (vec![1], vec![2], vec![3])
        );
        observer.queued.insert(("o/a".into(), 1));
        observer.running.insert(("o/a".into(), 2));
        observer.finished.insert(("o/a".into(), 3));
        let again = run_changes("o/a", &runs, &observer, now);
        assert!(again.queued.is_empty() && again.started.is_empty() && again.finished.is_empty());
    }

    #[test]
    fn a_merge_without_a_cd_run_is_missed_only_after_the_grace_period() {
        let head = "a".repeat(40);
        let merged = Some(at("2026-11-05T10:00:00Z"));
        let cd = vec![json!({"commit_sha": head, "workflow_id": "cd.yml"})];
        let gate_only = vec![json!({"commit_sha": head, "workflow_id": "gate.yml"})];
        assert!(!cd_missed(
            &head,
            merged,
            &gate_only,
            at("2026-11-05T10:10:00Z")
        ));
        assert!(cd_missed(
            &head,
            merged,
            &gate_only,
            at("2026-11-05T10:20:00Z")
        ));
        assert!(!cd_missed(&head, merged, &cd, at("2026-11-05T10:20:00Z")));
        assert!(!cd_missed(
            &head,
            merged,
            &gate_only,
            at("2026-11-07T10:20:00Z")
        ));
    }

    /// ENG-1239: every Forgejo check rule reads a series this controller or
    /// the collector's Forgejo scrape exports, one rule per group.
    #[test]
    fn the_forgejo_check_rules_read_the_metrics_the_controller_exports() {
        let rules: serde_yaml::Value = serde_yaml::from_str(include_str!(
            "../../../../examples/deploy/k8s/observability/dash0-forgejo-check-rules.yaml"
        ))
        .unwrap();
        let groups = rules["spec"]["groups"].as_sequence().unwrap();
        assert!(groups
            .iter()
            .all(|group| group["rules"].as_sequence().unwrap().len() == 1));
        let expr = |alert: &str| {
            groups
                .iter()
                .flat_map(|group| group["rules"].as_sequence().unwrap())
                .find(|rule| rule["alert"].as_str() == Some(alert))
                .and_then(|rule| rule["expr"].as_str())
                .unwrap_or_else(|| panic!("{alert} is declared"))
                .to_owned()
        };
        for (alert, metric) in [
            (
                "ForgejoJobWaitingNoRunner",
                telemetry::FORGEJO_RUN_OLDEST_WAITING,
            ),
            (
                "ForgejoRunnerControllerMissed",
                telemetry::FORGEJO_CONTROLLER_TICK,
            ),
            ("ForgejoGateFailedOnMain", telemetry::FORGEJO_RUN_FINISHED),
            ("ForgejoCdFailed", telemetry::FORGEJO_RUN_FINISHED),
            ("ForgejoCdMissed", telemetry::FORGEJO_CD_MISSED),
            (
                "ForgejoRunnerOrphan",
                telemetry::FORGEJO_RUNNER_ORPHAN_CLEANED,
            ),
        ] {
            assert!(
                expr(alert).contains(&format!("otel_metric_name=\"{metric}\"")),
                "{alert}"
            );
        }
        assert!(expr("ForgejoMetricsAbsent").contains("forgejo"));
        let text = serde_yaml::to_string(&rules).unwrap();
        for content in ["title", "commit message", "head_branch"] {
            assert!(!text.contains(content), "{content}");
        }
    }

    struct FakeCluster {
        jobs: Vec<Value>,
        deleted: Mutex<Vec<String>>,
    }

    impl Cluster for FakeCluster {
        fn namespace(&self) -> &'static str {
            "runners"
        }
        fn create(&self, _: &Value) -> Result<()> {
            Ok(())
        }
        fn job(&self, _: &str) -> Result<Option<Value>> {
            Ok(None)
        }
        fn unsuspend(&self, _: &str) -> Result<()> {
            Ok(())
        }
        fn delete_job(&self, name: &str) -> Result<()> {
            self.deleted.lock().unwrap().push(name.to_owned());
            Ok(())
        }
        fn runner_namespace(&self) -> Result<Value> {
            Ok(Value::Null)
        }
        fn network_policies(&self) -> Result<Value> {
            Ok(json!({"items": []}))
        }
        fn runner_jobs(&self) -> Result<Vec<Value>> {
            let deleted = self.deleted.lock().unwrap();
            Ok(self
                .jobs
                .iter()
                .filter(|job| {
                    !deleted
                        .iter()
                        .any(|name| job["metadata"]["name"] == name.as_str())
                })
                .cloned()
                .collect())
        }
        fn job_log(&self, _: &str) -> Result<String> {
            Ok(String::new())
        }
    }

    /// Killing the controller mid-job leaves a finished Job and a live
    /// registration; the next process's first tick removes both.
    #[test]
    fn a_restarted_controller_removes_what_its_predecessor_left() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server_runtime = tokio::runtime::Runtime::new().unwrap();
        let orphan = "forgejo-job-0123456789abcdef0123456789abcdef";
        let stale = "forgejo-job-fedcba9876543210fedcba9876543210";
        let server = server_runtime.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("DELETE"))
                .and(path("/api/v1/repos/o/a/actions/runners/6"))
                .respond_with(ResponseTemplate::new(204))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/v1/admin/actions/runners"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                    {"id": 9, "name": stale},
                    {"id": 10, "name": "someone-elses-runner"}
                ])))
                .mount(&server)
                .await;
            Mock::given(method("DELETE"))
                .and(path("/api/v1/admin/actions/runners/9"))
                .respond_with(ResponseTemplate::new(204))
                .expect(1)
                .mount(&server)
                .await;
            server
        });
        let cluster = Arc::new(FakeCluster {
            jobs: vec![json!({
                "metadata": {"name": orphan, "annotations": {
                    REGISTRATION_ANNOTATION: "6", REPOSITORY_ANNOTATION: "o/a"
                }},
                "status": {"conditions": [{"type": "Complete", "status": "True",
                    "lastTransitionTime": "2026-11-05T10:00:00Z"}]}
            })],
            deleted: Mutex::new(Vec::new()),
        });
        let mut controller = Controller {
            cluster: cluster.clone(),
            forgejo: Forgejo::new(&server.uri(), "operator".into()).unwrap(),
            registry: Registry::new(&server.uri(), "operator".into()).unwrap(),
            origin: server.uri(),
            token: "operator".into(),
            egress_host: Vec::new(),
            egress_ip: Vec::new(),
            dns: Ipv4Addr::new(169, 254, 20, 10),
            max_runners: 4,
            in_flight: Arc::default(),
            repositories: HashMap::new(),
            mirrored: HashSet::new(),
            unowned_registrations: BTreeSet::new(),
            observer: Observer::new(),
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
        };
        // First tick: the Job goes; the registration is only noted.
        assert_eq!(controller.recover().unwrap(), 1);
        assert_eq!(*cluster.deleted.lock().unwrap(), vec![orphan.to_owned()]);
        // Second tick: the registration has had no Job for two ticks.
        assert_eq!(controller.recover().unwrap(), 1);
        drop(server_runtime);
    }

    /// A cluster that accepts every launch: an isolated namespace, a Job that
    /// succeeds at once, and a two-line runner log.
    #[derive(Default)]
    struct LaunchCluster {
        created: Mutex<Vec<String>>,
        deleted: Mutex<Vec<String>>,
    }

    impl Cluster for LaunchCluster {
        fn namespace(&self) -> &'static str {
            "runners"
        }
        fn create(&self, object: &Value) -> Result<()> {
            self.created
                .lock()
                .unwrap()
                .push(object["kind"].as_str().unwrap_or_default().to_owned());
            Ok(())
        }
        fn job(&self, _: &str) -> Result<Option<Value>> {
            Ok(Some(
                json!({"metadata": {"uid": "job-uid"}, "status": {"succeeded": 1}}),
            ))
        }
        fn unsuspend(&self, _: &str) -> Result<()> {
            Ok(())
        }
        fn delete_job(&self, name: &str) -> Result<()> {
            self.deleted.lock().unwrap().push(name.to_owned());
            Ok(())
        }
        fn runner_namespace(&self) -> Result<Value> {
            Ok(json!({"metadata": {"labels": {
                "navigator.neonlaw.org/runner-network-policy": "enforced",
                "pod-security.kubernetes.io/enforce": "restricted"
            }}}))
        }
        fn network_policies(&self) -> Result<Value> {
            Ok(json!({"items": []}))
        }
        fn runner_jobs(&self) -> Result<Vec<Value>> {
            Ok(Vec::new())
        }
        fn job_log(&self, _: &str) -> Result<String> {
            Ok("step one\n\nstep two\n".into())
        }
    }

    /// One queued job in `o/a`: the tick mirrors the release its commit pins,
    /// starts exactly one isolated runner, forwards its log, removes it, and
    /// reports the repository's runs and its missed CD once each.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn a_tick_runs_each_waiting_job_once_and_reports_the_repository() {
        use wiremock::matchers::{method, path, path_regex, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let now = chrono::Utc::now();
        let ago = |minutes: i64| (now - chrono::TimeDelta::minutes(minutes)).to_rfc3339();
        let server_runtime = tokio::runtime::Runtime::new().unwrap();
        let server = server_runtime.block_on(async {
            let server = MockServer::start().await;
            let get = |route: &str, body: Value| {
                Mock::given(method("GET"))
                    .and(path(route.to_owned()))
                    .respond_with(ResponseTemplate::new(200).set_body_json(body))
            };
            get(
                "/api/v1/admin/actions/runners/jobs",
                json!([{"repo_id": 3, "status": "waiting"}, {"repo_id": 3, "status": "running"}]),
            )
            .mount(&server)
            .await;
            get("/api/v1/repositories/3", json!({"full_name": "o/a"}))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/v1/repos/o/a/actions/runs"))
                .and(query_param("status", "waiting"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "workflow_runs": [{"id": 11, "commit_sha": "abc123", "status": "waiting"}]
                })))
                .with_priority(1)
                .mount(&server)
                .await;
            get(
                "/api/v1/repos/o/a/actions/runs",
                json!({"workflow_runs": [
                    {"id": 11, "index_in_repo": 4, "status": "waiting", "event": "pull_request",
                     "workflow_id": "gate.yml", "created": ago(10)},
                    {"id": 12, "index_in_repo": 3, "status": "running", "event": "push",
                     "workflow_id": "cd.yml", "created": ago(12), "started": ago(11)},
                    {"id": 13, "index_in_repo": 2, "status": "failure", "event": "push",
                     "workflow_id": "gate.yml", "commit_sha": "abc123", "created": ago(30),
                     "started": ago(29), "stopped": ago(20)},
                    {"id": 14, "index_in_repo": 1, "status": "success", "event": "pull_request",
                     "workflow_id": "gate.yml", "created": ago(40), "started": ago(39),
                     "stopped": ago(35)},
                ]}),
            )
            .mount(&server)
            .await;
            Mock::given(method("GET"))
                .and(path("/api/v1/repos/o/a/raw/navigator.yaml"))
                .and(query_param("ref", "abc123"))
                .respond_with(ResponseTemplate::new(200).set_body_string("version: 26.11.5\n"))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path_regex(
                    "^/api/packages/navigator/generic/navigator/26.11.5/",
                ))
                .respond_with(ResponseTemplate::new(200))
                .expect(2)
                .mount(&server)
                .await;
            get("/api/v1/admin/actions/runners", json!([]))
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/api/v1/repos/o/a/actions/runners"))
                .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                    "id": 7, "uuid": "d012e532-c826-4e1d-adb8-58207c520f1b",
                    "token": "synthetic-runner-token"
                })))
                .expect(1)
                .mount(&server)
                .await;
            get(
                "/api/v1/repos/o/a/actions/runners/7",
                json!({"id": 7, "uuid": "d012e532-c826-4e1d-adb8-58207c520f1b",
                       "owner_id": 0, "repo_id": 3, "ephemeral": true, "status": "idle"}),
            )
            .mount(&server)
            .await;
            Mock::given(method("DELETE"))
                .and(path("/api/v1/repos/o/a/actions/runners/7"))
                .respond_with(ResponseTemplate::new(204))
                .expect(1)
                .mount(&server)
                .await;
            get(
                "/api/v1/repos/search",
                json!({"data": [{"full_name": "o/a"}, {"full_name": "not-a-scope"}]}),
            )
            .mount(&server)
            .await;
            get(
                "/api/v1/repos/o/a/contents/.forgejo/workflows/cd.yml",
                json!({"name": "cd.yml"}),
            )
            .mount(&server)
            .await;
            get(
                "/api/v1/repos/o/a/branches/main",
                json!({"commit": {"id": "deadbeef", "timestamp": ago(60)}}),
            )
            .mount(&server)
            .await;
            server
        });
        let cluster = Arc::new(LaunchCluster::default());
        let mut observer = Observer::new();
        observer.started = now - chrono::TimeDelta::hours(1);
        let mut controller = Controller {
            cluster: cluster.clone(),
            forgejo: Forgejo::new(&server.uri(), "operator".into()).unwrap(),
            registry: Registry::new(&server.uri(), "operator".into()).unwrap(),
            origin: server.uri(),
            token: "operator".into(),
            egress_host: Vec::new(),
            egress_ip: vec!["8.8.8.8".into()],
            dns: Ipv4Addr::new(169, 254, 20, 10),
            max_runners: 4,
            in_flight: Arc::default(),
            repositories: HashMap::new(),
            mirrored: HashSet::new(),
            unowned_registrations: BTreeSet::new(),
            observer,
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
        };

        let report = controller.tick().unwrap();
        assert_eq!(
            report,
            TickReport {
                waiting: 1,
                launched: 1,
                orphans_cleaned: 0
            }
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        while !controller.in_flight().0.is_empty() {
            assert!(Instant::now() < deadline, "the launch never finished");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            *cluster.created.lock().unwrap(),
            ["Job", "ServiceAccount", "Secret", "NetworkPolicy"]
        );
        let deleted = cluster.deleted.lock().unwrap().clone();
        assert_eq!(deleted.len(), 1);
        assert!(is_runner_job_name(&deleted[0]));
        assert!(controller.mirrored.contains("26.11.5"));
        let key = |id: u64| ("o/a".to_owned(), id);
        assert!(controller.observer.queued.contains(&key(11)));
        assert!(controller.observer.running.contains(&key(12)));
        assert!(controller.observer.finished.contains(&key(13)));
        assert!(controller.observer.finished.contains(&key(14)));
        assert_eq!(controller.observer.with_cd.get("o/a"), Some(&true));
        assert!(!controller.observer.watched.contains("not-a-scope"));

        // A second tick with an idle runner of ours already registered for
        // `o/a` starts nothing more; the repository and release are cached.
        controller.in_flight().0.insert(
            "forgejo-job-0123456789abcdef0123456789abcdef".into(),
            Launch {
                repository: "o/a".into(),
                registration: Some(7),
            },
        );
        let report = controller.tick().unwrap();
        assert_eq!(report.launched, 0);
        assert_eq!(report.waiting, 1);
        drop(controller);
        drop(server_runtime);
    }

    #[test]
    fn a_tick_fails_when_the_waiting_queue_cannot_be_read() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server_runtime = tokio::runtime::Runtime::new().unwrap();
        let server = server_runtime.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v1/admin/actions/runners/jobs"))
                .respond_with(ResponseTemplate::new(403))
                .mount(&server)
                .await;
            server
        });
        let mut controller = Controller {
            cluster: Arc::new(LaunchCluster::default()),
            forgejo: Forgejo::new(&server.uri(), "operator".into()).unwrap(),
            registry: Registry::new(&server.uri(), "operator".into()).unwrap(),
            origin: server.uri(),
            token: "operator".into(),
            egress_host: Vec::new(),
            egress_ip: vec!["8.8.8.8".into()],
            dns: Ipv4Addr::new(169, 254, 20, 10),
            max_runners: 4,
            in_flight: Arc::default(),
            repositories: HashMap::new(),
            mirrored: HashSet::new(),
            unowned_registrations: BTreeSet::new(),
            observer: Observer::new(),
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
        };
        // Recovery's own failure (the registrations read) is logged, not
        // fatal; the unreadable queue is.
        let error = controller.tick().unwrap_err();
        assert!(error.to_string().contains("403"), "{error}");
        drop(controller);
        drop(server_runtime);
    }
}
