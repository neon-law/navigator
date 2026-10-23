//! `navigator dev email-summary-e2e` — drive the real inbound-email summary
//! pipeline end to end on this worktree's KIND cluster.
//!
//! Real: the host `web` listener (this branch's binary), the in-cluster
//! Restate broker, the branch `workflows-service` image, `SurrealDB`, `SeaweedFS`,
//! and `ClamAV`. Synthetic and task-owned: the Vertex metadata/Gemini endpoints
//! and the Slack Web API ([`fixtures`]), reached by the worker through the
//! harness-gated `NAVIGATOR_SUMMARY_VERTEX_BASE_URL`, `GOOGLE_METADATA_URL`
//! and `SLACK_API_BASE_URL` overrides. Two CLI-owned TCP proxies sit between
//! `web` and the Restate ingress and `ClamAV`, so a scenario can take either
//! away without touching the cluster.
//!
//! Every failure to find `OrbStack`, the cluster, the worker, or the
//! `EmailSummary` registration is an error: nothing here skips.

mod fixtures;
mod harness;
mod scenarios;

use std::path::Path;
use std::process::Command;

use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};

use super::orchestrate::{
    build_and_load_local_image, configure_worktree_kubeconfig, require_tools, run,
    wait_for_condition, wait_for_selected_pod_ready, workspace_root,
};
use super::KindConfig;

const WORKER_CONTAINERFILE: &str = "images/Containerfile.workflows-service";
const WORKER_IMAGE_REPO: &str = "navigator-workflows-service";
const WORKER_SELECTOR: &str = "app=workflows-service";
const WORKER_RESOURCE: &str = "restatedeployment/workflows-service";
/// The hostname pods use to reach a process on the machine running Docker.
const HOST_FROM_CLUSTER: &str = "host.docker.internal";

/// `navigator dev email-summary-e2e`.
pub(super) fn run_e2e(cfg: &KindConfig) -> Result<()> {
    require_tools(&["docker", "kind", "kubectl", "cargo", "git"])?;
    ensure_orbstack(&docker_operating_system()?)?;
    let root = workspace_root()?;
    configure_worktree_kubeconfig(&root, cfg).context(
        "this worktree has no KIND cluster; run `navigator dev worktree-env up --runtime kind`",
    )?;
    ensure_worker_present(cfg)?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    runtime.block_on(async {
        let fixtures = fixtures::Fixtures::start().await?;
        let image = branch_image_tag(&root)?;
        let admin = format!("http://127.0.0.1:{}", cfg.restate_admin_port);
        let mut harness = harness::Harness::start(cfg, &root, fixtures, admin).await?;
        deploy_branch_worker(
            &root,
            cfg,
            &image,
            harness.fx.port(),
            harness.matter_project_id,
        )?;
        harness::assert_discovery(&harness.admin, "EmailSummary").await?;
        assert_running_image(cfg, &image)?;
        let result = scenarios::run_all(&mut harness).await;
        harness.stop();
        let evidence = result?;
        println!("{}", evidence.render());
        Ok(())
    })
}

/// Fail unless Docker's *currently selected* engine is `OrbStack`. The
/// selection is read, never changed: another task may own the global context.
fn ensure_orbstack(operating_system: &str) -> Result<()> {
    ensure!(
        operating_system.to_ascii_lowercase().contains("orbstack"),
        "Docker's selected engine is not OrbStack; select it for this shell with \
         `DOCKER_CONTEXT=orbstack` (do not switch the global context)"
    );
    Ok(())
}

fn docker_operating_system() -> Result<String> {
    let output = Command::new("docker")
        .args(["info", "--format", "{{.OperatingSystem}}"])
        .output()
        .context("run `docker info`")?;
    ensure!(
        output.status.success(),
        "`docker info` failed; is the Docker engine (OrbStack) running?"
    );
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn ensure_worker_present(cfg: &KindConfig) -> Result<()> {
    let status = Command::new("kubectl")
        .args(["--namespace", &cfg.namespace, "get", WORKER_RESOURCE])
        .output()
        .context("run kubectl")?;
    ensure!(
        status.status.success(),
        "the {WORKER_RESOURCE} resource is missing from the cluster; run \
         `navigator dev worktree-env up --runtime kind`"
    );
    Ok(())
}

/// An immutable tag derived from the checked-out source: the commit plus every
/// tracked modification and untracked file, so two different trees never share
/// a tag and an unchanged tree always does.
fn branch_image_tag(root: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    for args in [
        vec!["rev-parse", "HEAD"],
        vec!["diff", "HEAD"],
        vec!["ls-files", "--others", "--exclude-standard"],
    ] {
        let output = Command::new("git")
            .current_dir(root)
            .args(&args)
            .output()
            .context("run git")?;
        ensure!(output.status.success(), "`git {}` failed", args.join(" "));
        hasher.update(&output.stdout);
    }
    let untracked = Command::new("git")
        .current_dir(root)
        .args(["ls-files", "--others", "--exclude-standard"])
        .output()
        .context("run git")?;
    for name in String::from_utf8_lossy(&untracked.stdout).lines() {
        if let Ok(bytes) = std::fs::read(root.join(name)) {
            hasher.update(&bytes);
        }
    }
    Ok(format!(
        "{WORKER_IMAGE_REPO}:e2e-{}",
        &hex(&hasher.finalize())[..12]
    ))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Build the branch worker image, load it into this cluster, and point the
/// worker `RestateDeployment` at it with the summary lane enabled against the
/// fixture server.
fn deploy_branch_worker(
    root: &Path,
    cfg: &KindConfig,
    image: &str,
    fixture_port: u16,
    matter_project_id: uuid::Uuid,
) -> Result<()> {
    build_and_load_local_image(root, cfg, WORKER_CONTAINERFILE, image)?;
    let fixtures = format!("http://{HOST_FROM_CLUSTER}:{fixture_port}");
    let env = harness::worker_env(&fixtures, matter_project_id);
    patch_worker(cfg, image, &env)?;
    wait_for_selected_pod_ready(&cfg.namespace, WORKER_SELECTOR)?;
    wait_for_condition(&cfg.namespace, WORKER_RESOURCE, "Ready")
}

fn patch_worker(cfg: &KindConfig, image: &str, env: &[(String, String)]) -> Result<()> {
    let output = Command::new("kubectl")
        .args([
            "--namespace",
            &cfg.namespace,
            "get",
            WORKER_RESOURCE,
            "--output",
            "json",
        ])
        .output()
        .context("read the worker deployment")?;
    ensure!(output.status.success(), "could not read {WORKER_RESOURCE}");
    let resource: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parse the worker deployment")?;
    let patch = worker_patch(&resource, image, env)?;
    run(Command::new("kubectl")
        .args(["--namespace", &cfg.namespace, "patch", WORKER_RESOURCE])
        .args(["--type=json", "--patch", &patch.to_string()]))
}

fn run_kubectl(args: &[&str]) -> Result<()> {
    run(Command::new("kubectl").args(args))
}

fn run_cargo(root: &Path, args: &[&str]) -> Result<()> {
    run(Command::new("cargo").current_dir(root).args(args))
}

/// The JSON patch that sets the worker's image and replaces exactly the
/// variables the harness owns, leaving every other variable as it was.
fn worker_patch(
    resource: &serde_json::Value,
    image: &str,
    env: &[(String, String)],
) -> Result<serde_json::Value> {
    let container = resource
        .pointer("/spec/template/spec/containers/0")
        .context("the worker deployment has no container")?;
    let owned: std::collections::BTreeSet<&str> =
        env.iter().map(|(name, _)| name.as_str()).collect();
    let mut merged: Vec<serde_json::Value> = container["env"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| {
            entry["name"]
                .as_str()
                .is_none_or(|name| !owned.contains(name))
        })
        .collect();
    merged.extend(
        env.iter()
            .map(|(name, value)| serde_json::json!({ "name": name, "value": value })),
    );
    Ok(serde_json::json!([
        { "op": "replace", "path": "/spec/template/spec/containers/0/image", "value": image },
        { "op": "add", "path": "/spec/template/spec/containers/0/env", "value": merged },
    ]))
}

/// The newest running worker pod uses exactly the branch image. The operator
/// keeps the previous version's pod while its invocations drain, and Restate
/// routes new invocations to the latest registered version.
fn assert_running_image(cfg: &KindConfig, image: &str) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    loop {
        let output = Command::new("kubectl")
            .args([
                "--namespace",
                &cfg.namespace,
                "get",
                "pods",
                "--selector",
                WORKER_SELECTOR,
                "--sort-by=.metadata.creationTimestamp",
                "--output",
                "jsonpath={range .items[*]}{.status.phase} {.spec.containers[0].image}{\"\\n\"}{end}",
            ])
            .output()
            .context("read worker pods")?;
        ensure!(output.status.success(), "could not list the worker pods");
        let listing = String::from_utf8_lossy(&output.stdout).to_string();
        let running: Vec<&str> = listing
            .lines()
            .filter(|line| line.starts_with("Running "))
            .collect();
        if running.last().is_some_and(|line| line.ends_with(image)) {
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            bail!("the running worker is not the branch image {image}: {listing}");
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_orbstack_engine_passes() {
        assert!(ensure_orbstack("OrbStack").is_ok());
        let error = ensure_orbstack("Docker Desktop").expect_err("not OrbStack");
        assert!(error.to_string().contains("OrbStack"));
    }

    #[test]
    fn the_worker_patch_replaces_only_the_variables_it_owns() {
        let resource = serde_json::json!({
            "spec": { "template": { "spec": { "containers": [{
                "image": "navigator-workflows-service:dev",
                "env": [
                    { "name": "NAVIGATOR_CI_HARNESS", "value": "1" },
                    { "name": "SLACK_BOT_TOKEN", "value": "stale" },
                    { "name": "UNRELATED", "value": "kept" }
                ]
            }] } } }
        });
        let env = vec![
            (
                "SLACK_BOT_TOKEN".to_string(),
                "synthetic-slack-token".to_string(),
            ),
            ("NEW".to_string(), "1".to_string()),
        ];
        let patch = worker_patch(&resource, "img:e2e-abc", &env).expect("patch");
        assert_eq!(patch[0]["value"], "img:e2e-abc");
        let names: Vec<(&str, &str)> = patch[1]["value"]
            .as_array()
            .expect("env")
            .iter()
            .map(|entry| {
                (
                    entry["name"].as_str().expect("name"),
                    entry["value"].as_str().expect("value"),
                )
            })
            .collect();
        assert_eq!(
            names,
            vec![
                ("NAVIGATOR_CI_HARNESS", "1"),
                ("UNRELATED", "kept"),
                ("SLACK_BOT_TOKEN", "synthetic-slack-token"),
                ("NEW", "1"),
            ]
        );
    }

    #[test]
    fn a_worker_without_a_container_is_an_error() {
        let error = worker_patch(&serde_json::json!({}), "img", &[]).expect_err("no container");
        assert!(error.to_string().contains("no container"));
    }

    #[test]
    fn the_branch_image_tag_is_content_addressed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .current_dir(dir.path())
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .status()
                .expect("git");
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "x",
            "--no-gpg-sign",
        ]);
        let clean = branch_image_tag(dir.path()).expect("tag");
        assert_eq!(clean, branch_image_tag(dir.path()).expect("tag again"));
        assert!(clean.starts_with("navigator-workflows-service:e2e-"));
        std::fs::write(dir.path().join("change.txt"), "one").expect("write");
        let dirty = branch_image_tag(dir.path()).expect("tag");
        assert_ne!(clean, dirty);
        std::fs::write(dir.path().join("change.txt"), "two").expect("write");
        assert_ne!(dirty, branch_image_tag(dir.path()).expect("tag"));
    }
}
