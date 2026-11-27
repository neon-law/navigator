//! The actual combined host boots from native storage with no configured Source.
//! This checks unavailable Source, not a globally instrumented SDK prohibition:
//! store's test-only Deref fence is not compiled into this integration dependency.

use anyhow::{ensure, Context as _};
use std::{path::Path, process::Stdio, sync::Arc, time::Duration};
use store::{postgres::PostgresDb, test_support::PostgresFixture};

const CHILD: &str = "NAVIGATOR_NATIVE_STARTUP_CHILD";
const TEST: &str = "full_native_profile_replays_staged_data_and_manual_workflows_without_source";
const TOKEN: &str = "synthetic-native-startup-ingress-token-0123456789";
const BODY: &str = include_str!("../../templates/notations/neon_law/onboarding__letter.md");

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_native_profile_replays_staged_data_and_manual_workflows_without_source() {
    if std::env::var(CHILD).as_deref() == Ok("1") {
        portal::hosting::run(neon::brand()).await.unwrap();
        return;
    }
    let port = store::test_support::postgres_test_port();
    let options = sqlx::postgres::PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(sqlx::postgres::PgSslMode::Disable)
        .options([("session_preload_libraries", "age")]);
    let fixture = PostgresFixture::create(options).await.unwrap();
    let result = exercise(&fixture, &port.to_string()).await;
    let closed = fixture.close().await;
    result.unwrap();
    closed.unwrap();
}

async fn exercise(fixture: &PostgresFixture, postgres_port: &str) -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    stage(root.path())?;
    let mut identity = None;
    let manual_key = format!("manual-{}", uuid::Uuid::new_v4());
    for boot in 0..2 {
        let web_port = free_port()?;
        let worker_port = free_port()?;
        let log = root.path().join(format!("boot-{boot}.log"));
        let mut child = child_command(
            fixture,
            postgres_port,
            root.path(),
            web_port,
            worker_port,
            &log,
        )?
        .spawn()
        .context("start the real combined host")?;
        let observed = async {
            wait_ready(&mut child, web_port, worker_port).await?;
            verify_native_workflows(fixture.postgres(), worker_port, &manual_key).await?;
            PostgresDb::with_test_db(fixture.postgres().clone(), verify_native(root.path())).await
        }
        .await;
        let stopped = stop(&mut child).await;
        if let Err(error) = observed {
            return Err(error.context(format!(
                "combined host output: {}",
                std::fs::read_to_string(&log)?
            )));
        }
        let current = observed?;
        if let Some(previous) = identity {
            ensure!(
                current == previous,
                "staged replay changed immutable native identities"
            );
        }
        identity = Some(current);
        stopped?;
    }
    Ok(())
}

fn stage(root: &Path) -> anyhow::Result<()> {
    let project = root.join("staged/sample-litigation");
    std::fs::create_dir_all(project.join("drafts"))?;
    std::fs::create_dir_all(root.join("public"))?;
    std::fs::create_dir_all(root.join("repos"))?;
    std::fs::write(project.join("navigator.yaml"), "project:\n  host: h.example\n  name: sample-litigation\npages:\n  - slug: timeline\n    kind: docket_deadline_board\n    title: Case timeline\n    lenses:\n      lawyer: [docket_timeline, deadline_board, boundary_note, provenance_statement]\n      client: [docket_timeline, deadline_board, boundary_note, provenance_statement]\n")?;
    std::fs::write(
        project.join("drafts/sample_litigation__engagement.md"),
        BODY,
    )?;
    std::fs::write(project.join("drafts/unlisted_memo.md"), BODY)?;
    std::fs::write(project.join("source-commit"), "abc123\n")?;
    Ok(())
}

fn free_port() -> anyhow::Result<u16> {
    Ok(std::net::TcpListener::bind(("127.0.0.1", 0))?
        .local_addr()?
        .port())
}

fn child_command(
    fixture: &PostgresFixture,
    postgres_port: &str,
    root: &Path,
    web_port: u16,
    worker_port: u16,
    log: &Path,
) -> anyhow::Result<tokio::process::Command> {
    let output = std::fs::File::create(log)?;
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    command.env_clear();
    // The canonical deployment registry owns required coordinates. Synthetic
    // non-integration values are overridden below where a real fixture exists.
    for requirement in store::deployment::applicable_web_requirements(
        store::DeploymentEnvironment::Dev,
        &native_startup_profile,
    )? {
        for coordinate in requirement.any_of[0] {
            command.env(coordinate, "synthetic-native-startup");
        }
    }
    command
        .args(["--exact", TEST, "--nocapture", "--test-threads", "1"])
        .current_dir(root)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output)
        .env(CHILD, "1")
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("RUST_LOG", "info")
        .env("RUST_MIN_STACK", "33554432")
        .env("PORT", web_port.to_string())
        .env("NAVIGATOR_ENVIRONMENT", "dev")
        .env("NAVIGATOR_CI_HARNESS", "1")
        .env("NAVIGATOR_SIMULATED_MATTERS", "true")
        .env("NAVIGATOR_STAGING", "false")
        .env(
            workflows::flags::BILLING_CANARY_SCHEDULE_ENABLED_ENV,
            "false",
        )
        .env(
            workflows::flags::RECONCILE_INVOICES_SCHEDULE_ENABLED_ENV,
            "false",
        )
        .env(
            "SESSION_SECRET",
            "synthetic-native-startup-session-secret-0123456789",
        )
        .env("NAVIGATOR_STORAGE_BACKEND", "fs")
        .env("NAVIGATOR_STORAGE_FS_ROOT", root.join("blobs"))
        .env("NAVIGATOR_PUBLIC_DIR", root.join("public"))
        .env("NAVIGATOR_GIT_REPO_ROOT", root.join("repos"))
        .env("NAVIGATOR_SAMPLE_PROJECTS_DIR", root.join("staged"))
        .env("NAVIGATOR_CLAMD_ADDR", "127.0.0.1:1")
        .env("NAVIGATOR_FAKE_SIGNATURE_DIR", root.join("signatures"))
        .env("NAVIGATOR_SIGNATURE_PROVIDER", "fake")
        .env("NAVIGATOR_EMAIL_BACKEND", "capturing")
        .env("NAVIGATOR_RELEASE_TAG", "synthetic-native-startup")
        .env(
            "NAVIGATOR_DBOS_EXECUTOR_ID",
            format!("startup-{}", uuid::Uuid::new_v4()),
        )
        .env("NAVIGATOR_DBOS_INGRESS_TOKEN", TOKEN)
        .env(
            "NAVIGATOR_DBOS_INGRESS_URL",
            format!("http://127.0.0.1:{worker_port}"),
        )
        .env(
            "WORKFLOWS_SERVICE_HEALTH_LISTEN",
            format!("127.0.0.1:{worker_port}"),
        );
    for key in [
        "RESTATE_BROKER_URL",
        "RESTATE_AUTH_TOKEN",
        "RESTATE_ADMIN_URL",
        "RESTATE_IDENTITY_KEY",
    ] {
        command.env_remove(key);
    }
    for prefix in ["NAVIGATOR_POSTGRES", "NAVIGATOR_DBOS"] {
        command
            .env(format!("{prefix}_HOST"), "127.0.0.1")
            .env(format!("{prefix}_PORT"), postgres_port)
            .env(format!("{prefix}_USER"), "postgres")
            .env(format!("{prefix}_PASSWORD"), "synthetic")
            .env(format!("{prefix}_DATABASE"), fixture.database())
            .env(format!("{prefix}_SSLMODE"), "disable");
    }
    Ok(command)
}

fn native_startup_profile(key: &str) -> Option<String> {
    match key {
        "NAVIGATOR_CI_HARNESS" => Some("1".into()),
        "NAVIGATOR_ENVIRONMENT" => Some("dev".into()),
        "NAVIGATOR_SIMULATED_MATTERS" => Some("true".into()),
        "NAVIGATOR_STAGING" => Some("false".into()),
        _ => None,
    }
}

/// A billing request cannot execute, while Heartbeat proves durable native execution.
async fn verify_native_workflows(pg: &PostgresDb, worker: u16, key: &str) -> anyhow::Result<()> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let response = http
        .post(format!("http://127.0.0.1:{worker}/internal/dbos/schedules"))
        .bearer_auth(TOKEN)
        .json(&serde_json::json!({"service":"BillingCanary", "run_key":key}))
        .send()
        .await?;
    ensure!(
        response.status() == reqwest::StatusCode::GONE,
        "disabled billing was not rejected: {}",
        response.status()
    );
    let billing_id = format!("BillingCanary:{key}");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM dbos.workflow_status WHERE workflow_uuid=$1")
            .bind(billing_id)
            .fetch_one(pg.pool())
            .await?;
    ensure!(count == 0, "disabled billing created a workflow");
    let id = workflows::dbos_engine::heartbeat::workflow_id(
        workflows::dbos_engine::heartbeat::slot(chrono::Utc::now()),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM dbos.workflow_status WHERE workflow_uuid=$1")
                .bind(&id)
                .fetch_optional(pg.pool())
                .await?;
        if status.as_deref() == Some("SUCCESS") {
            break;
        }
        ensure!(
            status.as_deref() != Some("ERROR"),
            "native Heartbeat failed"
        );
        ensure!(
            tokio::time::Instant::now() < deadline,
            "native Heartbeat did not finish"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let steps: Vec<String> = sqlx::query_scalar("SELECT function_name FROM dbos.operation_outputs WHERE workflow_uuid=$1 ORDER BY function_id")
        .bind(&id).fetch_all(pg.pool()).await?;
    ensure!(
        steps == ["beat", "notify"],
        "native Heartbeat checkpoints changed: {steps:?}"
    );
    let automatic: i64 = sqlx::query_scalar("SELECT count(*) FROM dbos.workflow_status WHERE name='BillingCanary' AND workflow_uuid LIKE 'BillingCanary-%'")
        .fetch_one(pg.pool()).await?;
    ensure!(
        automatic == 0,
        "automatic canary ran despite its disabled timer"
    );
    Ok(())
}

async fn wait_ready(
    child: &mut tokio::process::Child,
    web: u16,
    worker: u16,
) -> anyhow::Result<()> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut last = String::new();
    loop {
        ensure!(
            child.try_wait()?.is_none(),
            "combined host exited before readiness"
        );
        let web = http
            .get(format!("http://127.0.0.1:{web}/app/readyz"))
            .send()
            .await;
        let worker = http
            .get(format!("http://127.0.0.1:{worker}/readyz"))
            .send()
            .await;
        if let (Ok(web), Ok(worker)) = (web, worker) {
            last = format!("web={} worker={}", web.status(), worker.status());
            ensure!(
                !web.status().is_client_error() && !worker.status().is_client_error(),
                "readiness route refused request: {last}"
            );
            if web.status().is_success() && worker.status().is_success() {
                let body: serde_json::Value = worker.json().await?;
                ensure!(
                    body["dbos"] == "ok",
                    "worker is not serving a live DBOS executor: {body}"
                );
                return Ok(());
            }
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "combined host did not become ready: {last}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn verify_native(root: &Path) -> anyhow::Result<(uuid::Uuid, uuid::Uuid, uuid::Uuid)> {
    let pg = PostgresDb::application().await?;
    let project = store::postgres::projects::find_by_code_on(&pg, "sample-litigation")
        .await?
        .context("native staged Project")?;
    ensure!(
        project.firm_id.is_some(),
        "staged Project lacks native Firm ownership"
    );
    let pages: Vec<String> = sqlx::query_scalar("SELECT slug FROM matter_page WHERE project_id=$1")
        .bind(project.id)
        .fetch_all(pg.pool())
        .await?;
    ensure!(pages == ["timeline"], "native staged page missing");
    let notes: Vec<(uuid::Uuid, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id,state,template_source_commit_sha,template_body_sha256 FROM notation WHERE project_id=$1")
        .bind(project.id).fetch_all(pg.pool()).await?;
    ensure!(
        notes.len() == 1 && notes[0].1 == "END",
        "signed staged engagement not activated"
    );
    ensure!(
        notes[0].2.as_deref() == Some("abc123"),
        "staged commit pin changed"
    );
    let compiled = rules::clauses::compile(BODY).map_err(anyhow::Error::msg)?;
    let digest = store::assets::sha256_hex(compiled.as_bytes());
    ensure!(
        notes[0].3.as_deref() == Some(digest.as_str()),
        "Notation body pin disagrees with staged bytes"
    );
    let (template, key, body_digest, asset_digest): (uuid::Uuid, String, String, String) = sqlx::query_as(
        "SELECT t.id,a.storage_key,t.body_sha256,a.sha256_hex FROM template t JOIN asset a ON a.id=t.asset_id WHERE t.project_id=$1 AND t.code='unlisted_memo' AND t.is_current")
        .bind(project.id).fetch_one(pg.pool()).await?;
    ensure!(
        body_digest == digest && asset_digest == digest,
        "native Template or Asset digest differs"
    );
    let storage: Arc<dyn cloud::StorageService> =
        Arc::new(cloud::FsStorage::new(root.join("blobs")).await?);
    ensure!(
        storage.get(&key).await?.bytes == compiled.as_bytes(),
        "native Template blob disagrees with stage"
    );
    Ok((project.id, notes[0].0, template))
}

async fn stop(child: &mut tokio::process::Child) -> anyhow::Result<()> {
    if let Some(exit) = child.try_wait()? {
        ensure!(exit.success(), "combined host exited unexpectedly: {exit}");
        return Ok(());
    }
    let id = child.id().context("owned combined host PID")?;
    ensure!(
        tokio::process::Command::new("kill")
            .args(["-TERM", &id.to_string()])
            .status()
            .await?
            .success(),
        "signal owned combined host"
    );
    let exit = match tokio::time::timeout(Duration::from_secs(30), child.wait()).await {
        Ok(result) => result?,
        Err(error) => {
            child
                .kill()
                .await
                .context("clean up owned host after failed graceful drain")?;
            child.wait().await?;
            return Err(error.into());
        }
    };
    ensure!(exit.success(), "combined host failed to drain: {exit}");
    Ok(())
}
