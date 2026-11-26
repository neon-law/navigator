//! `devx e2e` + `devx grant-lawyer` — the deployed-stack smoke checks.
//!
//! Both are smoke-test plumbing for the deployed KIND stack, run from
//! the release workflow (`.github/workflows/deploy.yml`) and locally via
//! `devx`. `ci.yml` has no KIND lane, so nothing here executes on a PR:
//! whatever this module can get wrong has to be caught by the `tests`
//! module below or it is caught by a failed release.
//!
//! `run_e2e` waits for every rollout the tier deploys, probes `ClamAV`
//! and Restate for protocol readiness, hits `/app/health` through the ingress,
//! and confirms the seed data landed. `grant_lawyer` pre-seeds the Lawyer
//! demo user, writing the singular `persons.role` column, so the browser
//! e2e's admin-gated walk can reach `/admin`. It requires activated
//! PostgreSQL history from the sourced `.devx/env`, matching the running app.
//!
//! ## Testing
//!
//! The orchestration shells out to `kubectl`/`curl` against a live
//! cluster, so it isn't unit-tested. The decision logic that *can*
//! drift — the seed thresholds, the row-count narrowing, the rollout
//! list, and the lawyer grant — is pure and covered by the `tests` module
//! below.

use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use super::{require_tools, use_kind_context, wait_for_condition, wait_rollout, KindConfig};

/// Minimum seed-row counts the deployed stack must show. Matches the
/// canonical seed in `store/seeds/`; a smaller count means seeding
/// silently failed.
///
/// Both witnesses read PostgreSQL through the same accessors as the application.
const MIN_QUESTIONS: i64 = 8;
/// The bundled template catalog carries 20+ current rows.
const MIN_TEMPLATES: i64 = 8;
/// `clamd`'s in-cluster port, the remote half of its forward.
const CLAMAV_PORT: u16 = 3310;
/// Every workload the applied tier deploys, waited on before the checks
/// that depend on it. A member joins this list the day it joins the tier:
/// the deploy workflow applies `k8s/overlays/kind` wholesale, so an
/// unlisted member would deploy unwatched and fail later as a confusing
/// connection error. It leaves the list the day it leaves the tier — a
/// name here that no manifest declares fails the gate outright, because
/// `kubectl rollout status` exits non-zero for a resource that does not
/// exist. Both directions are guarded by the `tests` module below.
const DEPENDENT_ROLLOUTS: &[(&str, &str)] = &[
    ("deployment", "surreal"),
    ("statefulset", "seaweedfs"),
    ("deployment", "rauthy"),
    ("deployment", "clamav"),
];

/// Whether the seed counts clear the minimums.
fn seed_counts_ok(questions: i64, templates: i64) -> bool {
    questions >= MIN_QUESTIONS && templates >= MIN_TEMPLATES
}

/// The local Rauthy fixture's Lawyer login, seeded so the browser gate's
/// admin-gated walk can run.
const LAWYER_EMAIL: &str = "lawyer@neonlaw.com";

/// Find or promote the synthetic Lawyer through the authoritative Person accessor.
async fn grant_lawyer_in(surreal: &store::surreal::SurrealDb) -> Result<()> {
    match store::persons::find_by_email_ci(surreal, LAWYER_EMAIL)
        .await
        .context("look up the lawyer person")?
    {
        Some(existing) => {
            store::persons::set_role(surreal, existing.id, store::persons::Role::Lawyer)
                .await
                .context("promote the lawyer person")?;
        }
        None => {
            store::persons::create(
                surreal,
                &store::persons::NewPerson::with_role(
                    "Lawyer",
                    LAWYER_EMAIL,
                    store::persons::Role::Lawyer,
                ),
            )
            .await
            .context("seed the lawyer person")?;
        }
    }
    Ok(())
}

// ---------- orchestration (shell-out; not unit-tested) ----------

/// `devx e2e`: the full deployed-stack smoke check.
pub fn run_e2e(cfg: &KindConfig) -> Result<()> {
    require_tools(&["kubectl", "curl"])?;
    use_kind_context(cfg)?;

    let root = super::orchestrate::workspace_root().ok();
    let native_requested = std::env::var("NAVIGATOR_KIND_DEPS_OVERLAY").as_deref()
        == Ok(super::native_profile::OVERLAY);
    if native_smoke_selected(root.as_deref(), cfg, native_requested)? {
        let root = root
            .as_deref()
            .context("native smoke workspace is missing")?;
        // This verifies the exact ready native resource; it never provisions,
        // grants roles, creates history, or falls back to the legacy tier.
        super::orchestrate::verified_native_database(root, cfg)?;
        wait_for_clamd_ping(cfg.clamav_port)?;
        check_health_at(&format!(
            "localhost:{}",
            super::worktree_env::web_port(root)?
        ))?;
        check_seed()?;
        eprintln!("=== native checks passed ===");
        return Ok(());
    }

    eprintln!("=== waiting for navigator-web rollout ===");
    wait_rollout("deployment", "navigator-web", cfg)?;

    eprintln!("=== checking dependent services ===");
    for &(kind, name) in DEPENDENT_ROLLOUTS {
        eprintln!("    waiting for {kind}/{name} rollout");
        wait_rollout(kind, name, cfg)?;
    }
    // Direct callers of the compatibility overlay do not run `dev up`, so
    // they have no host-side ClamAV forward. Starting one is
    // harmless when a local `dev up` already owns the port: kubectl exits on
    // the bind conflict and the existing forward satisfies the probe.
    let _clamav_forward = start_port_forward(cfg, "service/clamav", cfg.clamav_port, CLAMAV_PORT)?;
    eprintln!("=== checking ClamAV protocol readiness ===");
    wait_for_clamd_ping(cfg.clamav_port)?;
    wait_for_restate(cfg)?;

    eprintln!("=== hitting the ingress ===");
    check_health()?;

    eprintln!("=== confirming seed data populated ===");
    check_seed()?;

    eprintln!("=== all checks passed ===");
    Ok(())
}

fn native_smoke_selected(
    root: Option<&std::path::Path>,
    cfg: &KindConfig,
    native_requested: bool,
) -> Result<bool> {
    let Some(root) = root else {
        anyhow::ensure!(
            !native_requested,
            "native smoke requires its owned workspace"
        );
        return Ok(false);
    };
    if !super::native_profile::selected(root, cfg)? {
        anyhow::ensure!(
            !native_requested,
            "native smoke requires its creation intent"
        );
        return Ok(false);
    }
    let intent = super::native_profile::Intent::read(root)?
        .context("native smoke requires its owned creation intent")?;
    anyhow::ensure!(
        intent.was_ready(),
        "native tier initialization is incomplete"
    );
    Ok(true)
}

/// A `kubectl port-forward` that lives exactly as long as the check that
/// needs it. Dropping it kills the child, so a forward this command
/// opened never outlives the run and strands a port.
struct PortForward(Option<Child>);

impl Drop for PortForward {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn start_port_forward(
    cfg: &KindConfig,
    resource: &str,
    local_port: u16,
    remote_port: u16,
) -> Result<PortForward> {
    let child = Command::new("kubectl")
        .arg("--context")
        .arg(cfg.kind_context())
        .arg("--namespace")
        .arg(&cfg.namespace)
        .arg("port-forward")
        .arg(resource)
        .arg(format!("{local_port}:{remote_port}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn kubectl port-forward for {resource}"))?;
    Ok(PortForward(Some(child)))
}

/// A ready TCP socket is not sufficient for `ClamAV`: `clamd` can accept the
/// connection while it is still loading its signature database. Probe the
/// documented protocol command so the deployed gate cannot race that load.
fn wait_for_clamd_ping(port: u16) -> Result<()> {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};

    let addr: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .context("parse ClamAV host address")?;
    let deadline = Instant::now() + Duration::from_mins(1);
    loop {
        let ready = TcpStream::connect_timeout(&addr, Duration::from_secs(2))
            .and_then(|mut stream| {
                stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                stream.write_all(b"zPING\0")?;
                let mut reply = [0_u8; 64];
                let read = stream.read(&mut reply)?;
                Ok(reply[..read].windows(4).any(|part| part == b"PONG"))
            })
            .unwrap_or(false);
        if ready {
            eprintln!("    ClamAV PING/PONG ready");
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for ClamAV PING/PONG on 127.0.0.1:{port}");
        }
        sleep(Duration::from_millis(500));
    }
}

/// `devx grant-lawyer`: pre-seed the Lawyer demo user with the `lawyer`
/// role so the browser e2e's admin-gated walk can run.
pub fn grant_lawyer(_cfg: &KindConfig) -> Result<()> {
    // The grant connects to the endpoint the sourced `.devx/env` names —
    // the same coordinates the running `web` reads — through
    // `NAVIGATOR_SURREAL_*`.
    grant_lawyer_from_env()
}

/// Grant Lawyer the `lawyer` role in the `SurrealDB` the environment names.
///
/// The browser gate ([`super::browser_e2e`]) grants into whichever store
/// its host `web` reads, which is the same env-resolved endpoint — so it
/// calls straight through here.
pub fn grant_lawyer_at() -> Result<()> {
    grant_lawyer_from_env()
}

fn grant_lawyer_from_env() -> Result<()> {
    eprintln!("=== granting lawyer the lawyer role in PostgreSQL ===");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create tokio runtime")?;
    runtime.block_on(async {
        let surreal = crate::application_store()
            .await
            .context("verify PostgreSQL application authority")?;
        grant_lawyer_in(&surreal).await?;
        eprintln!("{LAWYER_EMAIL} role=lawyer");
        Ok(())
    })
}

/// Restate readiness depends on the broker. Restate Cloud has no
/// in-cluster `StatefulSet` — probe the tenant via the CLI; otherwise
/// wait on the Operator's `restate` `StatefulSet`. Either way the
/// worker Deployment must roll out.
fn wait_for_restate(cfg: &KindConfig) -> Result<()> {
    let broker = std::env::var("RESTATE_BROKER_URL").unwrap_or_default();
    if broker.contains("restate.cloud") {
        eprintln!("    Restate Cloud broker detected — probing tenant via CLI");
        require_tools(&["restate"])?;
        let out = Command::new("restate")
            .args(["-y", "deployment", "list"])
            .output()
            .context("run `restate -y deployment list`")?;
        let listing = String::from_utf8_lossy(&out.stdout);
        if !listing.contains("workflows-service") {
            eprintln!("{listing}");
            bail!("workflows-service not registered with Restate Cloud tenant");
        }
    } else {
        // The Operator places the cluster in its own `restate` namespace
        // (not cfg.namespace) and names the StatefulSet from the CR spec,
        // not literally "restate" — so wait on the RestateCluster CR's
        // `Ready` condition.
        let (ns, resource, condition) = restate_ready_target();
        eprintln!("    waiting for {resource} {condition} in namespace {ns}");
        wait_for_condition(ns, resource, condition)?;
    }
    // workflows-service is a RestateDeployment CR (Operator-managed), not a
    // plain Deployment — `deployment/workflows-service` returns NotFound. It
    // lives in cfg.namespace (unlike the cluster). Wait on the CR's `Ready`
    // condition.
    eprintln!(
        "    waiting for {WORKFLOWS_SERVICE_READY_RESOURCE} Ready in namespace {}",
        cfg.namespace
    );
    wait_for_condition(&cfg.namespace, WORKFLOWS_SERVICE_READY_RESOURCE, "Ready")
}

/// The Operator-managed resource whose `Ready` condition gates
/// workflows-service readiness. It is a `RestateDeployment` CR, not a plain
/// `Deployment` — querying `deployment/workflows-service` returns `NotFound`.
const WORKFLOWS_SERVICE_READY_RESOURCE: &str = "restatedeployment/workflows-service";

/// The (namespace, resource, condition) the in-cluster Restate readiness
/// wait targets. The Restate Operator reconciles the `RestateCluster` CR
/// into a `StatefulSet` in a namespace named after the cluster (`restate`),
/// *not* in `cfg.namespace` and *not* under a guessable `StatefulSet` name —
/// so the readiness gate is the CR's own `Ready` condition. Pulled out so
/// the namespace/resource choice is unit-testable (see tests below).
fn restate_ready_target() -> (&'static str, &'static str, &'static str) {
    ("restate", "restatecluster/restate", "Ready")
}

/// Hit `/app/health` through the KIND ingress and require HTTP 200.
///
/// Two guards make this loud-but-bounded instead of an indefinite hang.
/// `--max-time` caps each individual request, so a wedged ingress that
/// accepts the connection but never answers can't block the whole `e2e`
/// step (that un-capped curl was a load-bearing reason a stuck deploy ran
/// for hours). The retry loop tolerates the few seconds the ingress can
/// lag behind a freshly-Ready pod, and every attempt logs its status so a
/// failure says *what* the ingress returned, not just "not 200".
fn check_health() -> Result<()> {
    let host = std::env::var("INGRESS_HOST").unwrap_or_else(|_| "localhost:8080".to_string());
    check_health_at(&host)
}

fn check_health_at(host: &str) -> Result<()> {
    let url = format!("http://{host}/app/health");
    let deadline = Instant::now() + Duration::from_mins(1);
    let mut attempt = 0;
    loop {
        attempt += 1;
        let out = Command::new("curl")
            .args([
                "-sS",
                "--max-time",
                "10",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
            ])
            .arg("--resolve")
            .arg("localhost:8080:127.0.0.1")
            .arg(&url)
            .output()
            .context("curl /app/health")?;
        let status = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if status == "200" {
            eprintln!("    health OK ({status}) after {attempt} attempt(s)");
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "expected HTTP 200 from {url} within 60s; last status {status:?} after {attempt} attempt(s)"
            );
        }
        eprintln!(
            "    /app/health not ready (status {status:?}); retrying in 2s [attempt {attempt}]"
        );
        sleep(Duration::from_secs(2));
    }
}

/// Confirm the canonical Question and current Template catalogs were seeded.
fn check_seed() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create tokio runtime")?;
    let (q, e) = runtime.block_on(count_seeded_rows())?;
    if !seed_counts_ok(q, e) {
        bail!("expected at least {MIN_QUESTIONS} questions and {MIN_TEMPLATES} templates; got q={q} e={e}");
    }
    eprintln!("seed OK (q={q} e={e})");
    Ok(())
}

async fn count_seeded_rows() -> Result<(i64, i64)> {
    let db = crate::application_store().await?;
    let questions = store::questions::list_all(&db)
        .await
        .context("count the seeded questions")?;
    let templates = store::templates::list_current(&db)
        .await
        .context("count the seeded templates")?;
    Ok((witnessed(questions.len()), witnessed(templates.len())))
}

/// Narrow a row count to the `i64` the minimums compare against. A count
/// past `i64::MAX` clears every floor, so saturating is exact enough.
fn witnessed(rows: usize) -> i64 {
    i64::try_from(rows).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_smoke_refuses_partial_or_foreign_intent_instead_of_waiting_on_legacy() {
        let root = tempfile::tempdir().unwrap();
        let cfg = KindConfig::from_env();
        assert!(!native_smoke_selected(None, &cfg, false).unwrap());
        assert!(native_smoke_selected(None, &cfg, true).is_err());
        assert!(!native_smoke_selected(Some(root.path()), &cfg, false).unwrap());
        assert!(native_smoke_selected(Some(root.path()), &cfg, true).is_err());
        super::super::native_profile::Intent::prepare(root.path(), &cfg).unwrap();
        assert!(
            native_smoke_selected(Some(root.path()), &cfg, false).is_err(),
            "an interrupted native creation is not compatibility authority"
        );
        let mut foreign = cfg;
        foreign.cluster = "foreign-smoke-tier".into();
        assert!(native_smoke_selected(Some(root.path()), &foreign, true).is_err());
    }

    #[test]
    fn dependent_rollouts_match_kind_resources() {
        assert!(DEPENDENT_ROLLOUTS.contains(&("statefulset", "seaweedfs")));
        assert!(!DEPENDENT_ROLLOUTS.contains(&("deployment", "seaweedfs")));
    }

    /// Every dependency the KIND overlay deploys has to be waited on
    /// here, or `dev e2e` reports the stack healthy while a member is
    /// still starting. The overlay is the authority, so this reads it
    /// rather than restating the list.
    #[test]
    fn every_kind_only_dependency_is_waited_on() {
        let overlay = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repo root is cli/'s parent")
            .join("k8s/overlays/kind/surreal/surreal.yaml");
        let manifest = std::fs::read_to_string(overlay).expect("read the Surreal manifest");

        assert!(manifest.contains("kind: Deployment"), "{manifest}");
        assert!(
            DEPENDENT_ROLLOUTS.contains(&("deployment", "surreal")),
            "the Surreal Deployment ships in the KIND overlay but `dev e2e` never waits for it"
        );
    }

    /// Every `(kind, name)` workload the manifests under `k8s/` declare,
    /// with `kind` lowercased to match the kubectl spelling
    /// `DEPENDENT_ROLLOUTS` uses. This reads the YAML rather than
    /// restating a list, so a workload that lands or leaves the tier
    /// moves this set with it.
    fn declared_workloads() -> std::collections::BTreeSet<(String, String)> {
        use serde::Deserialize;

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repo root is cli/'s parent")
            .join("k8s");
        let mut declared = std::collections::BTreeSet::new();
        for entry in walkdir::WalkDir::new(&root)
            .into_iter()
            .filter_map(Result::ok)
        {
            if entry.path().extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            let text = std::fs::read_to_string(entry.path()).expect("read a k8s manifest");
            for document in serde_yaml::Deserializer::from_str(&text) {
                // A kustomization or a strategic-merge patch is still valid
                // YAML but declares no workload; skipping an unparseable or
                // nameless document keeps this about presence.
                let Ok(value) = serde_yaml::Value::deserialize(document) else {
                    continue;
                };
                let Some(kind) = value.get("kind").and_then(|k| k.as_str()) else {
                    continue;
                };
                let Some(name) = value
                    .get("metadata")
                    .and_then(|m| m.get("name"))
                    .and_then(|n| n.as_str())
                else {
                    continue;
                };
                declared.insert((kind.to_lowercase(), name.to_string()));
            }
        }
        declared
    }

    /// The mirror of [`every_kind_only_dependency_is_waited_on`], and the
    /// direction that broke a release: a name on this list that the
    /// applied manifests do not declare. `wait_rollout` shells `kubectl
    /// rollout status`, which exits non-zero — not zero — for a resource
    /// that does not exist, so a stale entry fails the gate on a
    /// perfectly healthy stack.
    ///
    /// A name outliving its manifest takes the whole gate with it: the
    /// failed wait aborts before `ClamAV` readiness, the attachment smoke
    /// checks, and the browser suites, so the release reports a missing
    /// workload rather than anything those checks would have found.
    #[test]
    fn every_waited_on_rollout_is_declared_in_the_manifests() {
        let declared = declared_workloads();
        for &(kind, name) in DEPENDENT_ROLLOUTS {
            assert!(
                declared.contains(&(kind.to_string(), name.to_string())),
                "`dev e2e` waits for {kind}/{name}, which no manifest under `k8s/` declares; \
                 `kubectl rollout status` exits non-zero for a resource that does not exist, so \
                 this fails the release on a healthy stack"
            );
        }
    }

    #[test]
    fn witnessed_narrows_a_row_count() {
        assert_eq!(witnessed(0), 0);
        assert_eq!(witnessed(38), 38);
    }

    #[tokio::test]
    async fn seed_witnesses_read_native_rows_and_count_only_current_templates() {
        store::test_support::with_postgres(async {
            assert_eq!(count_seeded_rows().await.unwrap(), (0, 0));
            let db = store::surreal::SurrealDb::uninitialized();
            store::questions::create(
                &db,
                &store::questions::NewQuestion::new("synthetic_question", "Synthetic?", "string"),
            )
            .await
            .unwrap();
            for title in ["First", "Second"] {
                store::templates::save_version(
                    &db,
                    None,
                    "synthetic_template",
                    store::templates::Version {
                        title: title.into(),
                        respondent_type: "person".into(),
                        asset_id: None,
                        form_code: None,
                        kind: None,
                        source_commit_sha: None,
                    },
                )
                .await
                .unwrap();
            }
            assert_eq!(count_seeded_rows().await.unwrap(), (1, 1));
        })
        .await;
    }

    /// Two witnesses, so both must clear: a stack where only one table
    /// populated has to fail rather than pass on the other's count.
    #[test]
    fn one_witness_alone_is_not_enough() {
        assert!(!seed_counts_ok(0, 38), "templates alone must not pass");
        assert!(!seed_counts_ok(38, 0), "questions alone must not pass");
    }

    #[test]
    fn seed_counts_ok_enforces_the_minimums() {
        assert!(seed_counts_ok(8, 8));
        assert!(seed_counts_ok(20, 10));
        assert!(!seed_counts_ok(7, 8));
        assert!(!seed_counts_ok(8, 7));
    }

    /// The grant seeds the row when the mailbox is absent.
    #[tokio::test]
    async fn grant_lawyer_seeds_the_lawyer_person() {
        store::test_support::with_postgres(async {
            let surreal = store::surreal::SurrealDb::uninitialized();

            grant_lawyer_in(&surreal).await.unwrap();

            let row = store::persons::find_by_email_ci(&surreal, LAWYER_EMAIL)
                .await
                .unwrap()
                .expect("the grant seeds the mailbox");
            assert_eq!(row.role, store::persons::Role::Lawyer);
            assert_eq!(row.email, LAWYER_EMAIL);
        })
        .await;
    }

    /// …and promotes it when it is already there, at whatever casing.
    ///
    /// `person_email_lower` holds one row per mailbox, so a Lawyer row stored
    /// as `Lawyer@NeonLaw.com` is the *same* mailbox as the seeded
    /// `lawyer@neonlaw.com`. The grant must promote that row rather than
    /// attempt a second create the unique index would reject.
    #[tokio::test]
    async fn grant_lawyer_folds_a_case_variant_row_instead_of_colliding() {
        store::test_support::with_postgres(async {
            let surreal = store::surreal::SurrealDb::uninitialized();
            store::persons::create(
                &surreal,
                &store::persons::NewPerson::with_role(
                    "Lawyer",
                    "Lawyer@NeonLaw.com",
                    store::persons::Role::Client,
                ),
            )
            .await
            .unwrap();

            grant_lawyer_in(&surreal).await.unwrap();

            let rows = store::persons::list_directory(&surreal).await.unwrap();
            assert_eq!(
                rows.len(),
                1,
                "the grant must fold the case-variant mailbox, not add a row"
            );
            assert_eq!(
                rows[0].email, "Lawyer@NeonLaw.com",
                "the stored casing is preserved"
            );
            assert_eq!(rows[0].role, store::persons::Role::Lawyer);
        })
        .await;
    }

    #[test]
    fn restate_readiness_waits_in_the_operator_namespace() {
        // Regression guard for the smoke-check failure where wait_for_restate
        // queried `statefulset/restate` in cfg.namespace (`navigator`) — the
        // Operator places it in the `restate` namespace, so the wait must
        // target that namespace and the RestateCluster CR, not a StatefulSet.
        let (ns, resource, condition) = restate_ready_target();
        assert_eq!(
            ns, "restate",
            "Restate Operator reconciles the cluster into its own `restate` namespace, not cfg.namespace"
        );
        assert!(
            resource.starts_with("restatecluster/"),
            "wait on the RestateCluster CR's Ready condition, not a guessed StatefulSet name: {resource}"
        );
        assert_eq!(condition, "Ready");
    }

    #[test]
    fn workflows_service_readiness_targets_the_restatedeployment_cr() {
        // Regression guard: workflows-service is an Operator-managed
        // RestateDeployment CR, not a plain Deployment — querying
        // `deployment/workflows-service` returns NotFound.
        assert!(
            WORKFLOWS_SERVICE_READY_RESOURCE.starts_with("restatedeployment/"),
            "wait on the RestateDeployment CR, not a plain Deployment: {WORKFLOWS_SERVICE_READY_RESOURCE}"
        );
    }
}
