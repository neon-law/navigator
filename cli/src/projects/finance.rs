//! Start and read a Project's finance aggregate run through its authenticated,
//! matter-scoped API (`POST`/`GET /app/api/projects/{id}/finance`). The run
//! reads `finance/<code>/input.json` already in the deployment's storage; this
//! module never writes it.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::{
    projects::{manifest, surfaces},
    remote,
};

/// Resolve the deployment and the Project's id: the explicit code, else the
/// checkout's `project.name`.
async fn resolve(
    host: Option<&str>,
    project: Option<&str>,
) -> Result<(String, String, uuid::Uuid)> {
    let local = manifest::read(Path::new("."));
    let project = project
        .or_else(|| local.as_ref().and_then(|m| m.project.as_deref()))
        .ok_or_else(|| anyhow!("pass a Project code or run from a Project checkout"))?;
    let host = host.or_else(|| local.as_ref().and_then(|m| m.host.as_deref()));
    let (base, token) = remote::resolve(host)?;
    let id = surfaces::resolve_project_id(&base, &token, project).await?;
    Ok((base, token, id))
}

/// `navigator project finance run`: queue a run, print its `run_id`.
pub async fn run(host: Option<&str>, project: Option<&str>) -> ExitCode {
    match start(host, project).await {
        Ok(run_id) => {
            println!("{run_id}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator project finance run: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn start(host: Option<&str>, project: Option<&str>) -> Result<String> {
    let (base, token, id) = resolve(host, project).await?;
    // The run takes no body, and Google's load balancer answers a POST without
    // a length with 411, so the zero is sent explicitly.
    let response = reqwest::Client::new()
        .post(format!("{base}/app/api/projects/{id}/finance/run"))
        .timeout(Duration::from_secs(30))
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_LENGTH, "0")
        .body(Vec::<u8>::new())
        .send()
        .await
        .context("start finance run")?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!("finance run failed: {status}"));
    }
    let body: serde_json::Value = response.json().await.context("parse finance run")?;
    body["run_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("finance run answered without a run_id"))
}

/// `navigator project finance show`: read the stored report.
pub async fn show(host: Option<&str>, project: Option<&str>, json: bool) -> ExitCode {
    match read(host, project).await {
        Ok(Some(report)) => {
            if json {
                println!("{report}");
            } else {
                print_report(&report);
            }
            ExitCode::SUCCESS
        }
        Ok(None) => {
            eprintln!("navigator project finance show: no run stored yet");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("navigator project finance show: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn read(host: Option<&str>, project: Option<&str>) -> Result<Option<serde_json::Value>> {
    let (base, token, id) = resolve(host, project).await?;
    let response = reqwest::Client::new()
        .get(format!("{base}/app/api/projects/{id}/finance"))
        .timeout(Duration::from_secs(30))
        .bearer_auth(token)
        .send()
        .await
        .context("read finance report")?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(anyhow!("finance read failed: {status}"));
    }
    response
        .json()
        .await
        .map(Some)
        .context("parse finance report")
}

fn print_report(report: &serde_json::Value) {
    for field in ["source", "run_id", "computed_at"] {
        println!("{field}: {}", report[field].as_str().unwrap_or("none"));
    }
    if let Some(totals) = report["totals"].as_object() {
        println!("totals:");
        for (key, value) in totals {
            println!("  {key}: {value}");
        }
    }
}
