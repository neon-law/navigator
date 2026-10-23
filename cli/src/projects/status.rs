//! Read a Project's live lifecycle through its authenticated, matter-scoped API.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::{
    projects::{manifest, surfaces},
    remote,
};

pub async fn run(host: Option<&str>, project: Option<&str>, json: bool) -> ExitCode {
    match read(host, project).await {
        Ok(value) => {
            if json {
                println!("{value}");
            } else {
                print_status(&value);
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator project status: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn read(host: Option<&str>, project: Option<&str>) -> Result<serde_json::Value> {
    let local = manifest::read(Path::new("."));
    let project = project
        .or_else(|| local.as_ref().and_then(|m| m.project.as_deref()))
        .ok_or_else(|| anyhow!("pass a Project code or run from a Project checkout"))?;
    let host = host.or_else(|| local.as_ref().and_then(|m| m.host.as_deref()));
    let (base, token) = remote::resolve(host)?;
    let id = surfaces::resolve_project_id(&base, &token, project).await?;
    let response = reqwest::Client::new()
        .get(format!("{base}/app/api/projects/{id}/status"))
        .timeout(Duration::from_secs(30))
        .bearer_auth(token)
        .send()
        .await
        .context("read Project status")?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!("Project status read failed: {status}"));
    }
    response.json().await.context("parse Project status")
}

fn print_status(value: &serde_json::Value) {
    for field in [
        "code",
        "status",
        "project_status",
        "opened_at",
        "closed_at",
        "source_state",
    ] {
        println!("{field}: {}", value[field].as_str().unwrap_or("none"));
    }
    let onboarding = &value["onboarding"];
    println!("onboarding filed: {}", onboarding["filed"]);
    if let Some(documents) = onboarding["documents"].as_array() {
        for doc in documents {
            println!(
                "  {} (visibility: {})",
                doc["id"],
                doc["visibility"].as_str().unwrap_or("unknown")
            );
        }
    }
    println!("Onboarding presence confirms filing; execution is not verified.");
    if let Some(missing) = value["missing"].as_array() {
        for item in missing {
            println!("missing: {}", item.as_str().unwrap_or_default());
        }
    }
}
