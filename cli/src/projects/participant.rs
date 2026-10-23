//! CLI adapter for the audited participant DRI designation and removal doors.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use uuid::Uuid;

use crate::{
    projects::{manifest, surfaces},
    remote,
};

pub async fn dri(host: Option<&str>, project: Option<&str>, row: Uuid, clear: bool) -> ExitCode {
    match change(host, project, row, clear).await {
        Ok(()) => {
            println!(
                "participant DRI {}",
                if clear { "cleared" } else { "designated" }
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator project participant dri: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn coordinates(host: Option<&str>, project: Option<&str>) -> Result<(String, String, Uuid)> {
    let local = manifest::read(Path::new("."));
    let project = project
        .or_else(|| local.as_ref().and_then(|m| m.project.as_deref()))
        .ok_or_else(|| anyhow!("pass --project or run from a Project checkout"))?;
    let host = host.or_else(|| local.as_ref().and_then(|m| m.host.as_deref()));
    let (base, token) = remote::resolve(host)?;
    let id = surfaces::resolve_project_id(&base, &token, project).await?;
    Ok((base, token, id))
}

pub async fn list(host: Option<&str>, project: Option<&str>) -> ExitCode {
    let result = async {
        let (base, token, id) = coordinates(host, project).await?;
        let response = reqwest::Client::new()
            .get(format!("{base}/app/api/projects/{id}/participants"))
            .timeout(Duration::from_secs(30))
            .bearer_auth(token)
            .send()
            .await
            .context("list participants")?;
        if !response.status().is_success() {
            return Err(anyhow!("participant list refused: {}", response.status()));
        }
        let rows: serde_json::Value = response.json().await.context("parse participants")?;
        println!("{rows}");
        Ok(())
    }
    .await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator project participant list: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn change(host: Option<&str>, project: Option<&str>, row: Uuid, clear: bool) -> Result<()> {
    let (base, token, id) = coordinates(host, project).await?;
    let method = if clear {
        reqwest::Method::DELETE
    } else {
        reqwest::Method::PUT
    };
    let response = reqwest::Client::new()
        .request(
            method,
            format!("{base}/app/api/projects/{id}/participants/{row}/dri"),
        )
        .timeout(Duration::from_secs(30))
        .bearer_auth(token)
        .send()
        .await
        .context("change participant DRI")?;
    if response.status() != reqwest::StatusCode::NO_CONTENT {
        return Err(anyhow!(
            "participant DRI change refused: {}",
            response.status()
        ));
    }
    Ok(())
}
