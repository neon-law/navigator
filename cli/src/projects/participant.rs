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
    let method = if clear {
        reqwest::Method::DELETE
    } else {
        reqwest::Method::PUT
    };
    match change(host, project, row, method, "/dri").await {
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

pub async fn remove(host: Option<&str>, project: Option<&str>, row: Uuid) -> ExitCode {
    match change(host, project, row, reqwest::Method::DELETE, "").await {
        Ok(()) => {
            println!("Project participant removed");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator project participant remove: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn change(
    host: Option<&str>,
    project: Option<&str>,
    row: Uuid,
    method: reqwest::Method,
    suffix: &str,
) -> Result<()> {
    let (base, token, id) = coordinates(host, project).await?;
    let response = reqwest::Client::new()
        .request(
            method,
            format!("{base}/app/api/projects/{id}/participants/{row}{suffix}"),
        )
        .timeout(Duration::from_secs(30))
        .bearer_auth(token)
        .send()
        .await
        .context("change Project participation")?;
    if response.status() != reqwest::StatusCode::NO_CONTENT {
        let status = response.status();
        if status == reqwest::StatusCode::CONFLICT
            && response
                .json::<serde_json::Value>()
                .await
                .is_ok_and(|body| body["error"] == "dri_lockout")
        {
            return Err(anyhow!(
                "designate another lawyer DRI on this Project first"
            ));
        }
        return Err(anyhow!("participant change refused: {status}"));
    }
    Ok(())
}
