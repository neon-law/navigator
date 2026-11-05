//! Read a Project's onboarding status (`pitch` | `official`) through its
//! authenticated, matter-scoped API.
//!
//! The status is derived server-side from the signature records: `official`
//! only once the matter's onboarding notation has a provider-confirmed
//! completed envelope on a loop Navigator itself sent. Everything else —
//! no envelope yet, one still outstanding, one declined or voided — is
//! `pitch`.

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
            let code = value["code"].as_str().unwrap_or("unknown");
            let status = value["onboarding_status"].as_str().unwrap_or("unknown");
            if json {
                println!("{value}");
            } else {
                println!("{code}: {status}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator project lifecycle: {error:#}");
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
        .context("read Project lifecycle")?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!("Project lifecycle read failed: {status}"));
    }
    let status: serde_json::Value = response.json().await.context("parse Project status")?;
    let onboarding_status = status["onboarding"]["status"]
        .as_str()
        .ok_or_else(|| anyhow!("the Project status read carried no onboarding status"))?;
    Ok(serde_json::json!({
        "code": status["code"],
        "onboarding_status": onboarding_status,
    }))
}
