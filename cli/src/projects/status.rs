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
        Ok(mut value) => {
            if matches!(value["project_status"].as_str(), Some("pitch" | "open")) {
                value["workspace"] = super::workspace::inspect(&value).await;
            }
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

/// Refresh the matter's already-linked envelopes through the Firm-scoped provider.
pub async fn refresh_envelopes(host: Option<&str>, project: &str) -> ExitCode {
    let result: Result<()> = async {
        let (base, token) = remote::resolve(host)?;
        let id = surfaces::resolve_project_id(&base, &token, project).await?;
        let value = refresh_known(&base, &token, id).await?;
        println!("{}", serde_json::to_string_pretty(&value)?);
        Ok(())
    }
    .await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator project envelope status: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn refresh_known(base: &str, token: &str, id: uuid::Uuid) -> Result<serde_json::Value> {
    let response = remote::authenticated_client_builder(base)?
        .timeout(Duration::from_secs(120))
        .build()?
        .post(format!("{base}/app/api/projects/{id}/envelopes/refresh"))
        .bearer_auth(token)
        .json(&serde_json::json!({}))
        .send()
        .await?;
    anyhow::ensure!(
        response.status().is_success(),
        "envelope refresh failed: {}",
        response.status()
    );
    let value: serde_json::Value = response.json().await?;
    Ok(value)
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
    match value["drive_folder_id"].as_str() {
        Some(id) => println!("drive folder: https://drive.google.com/drive/folders/{id}"),
        None => println!("drive folder: none linked"),
    }
    if let Some(history) = value["history"].as_array() {
        println!("status history:");
        for entry in history {
            let field = |key: &str| entry[key].as_str().unwrap_or("none");
            println!(
                "  {} -> {} (effective: {}, recorded: {}, reason: {}, attester: {})",
                field("from_status"),
                field("to_status"),
                field("effective_at"),
                field("recorded_at"),
                field("reason"),
                field("attesting_person_id")
            );
        }
    }
    let onboarding = &value["onboarding"];
    println!("onboarding filed: {}", onboarding["filed"]);
    println!(
        "onboarding status: {}",
        onboarding["status"].as_str().unwrap_or("unknown")
    );
    if let Some(documents) = onboarding["documents"].as_array() {
        for doc in documents {
            println!(
                "  {} (visibility: {})",
                doc["id"],
                doc["visibility"].as_str().unwrap_or("unknown")
            );
        }
    }
    if let Some(adoptions) = value["envelopes"]
        .as_array()
        .or_else(|| value["adopted_envelopes"].as_array())
    {
        for adoption in adoptions {
            let envelope = &adoption["envelope"];
            println!(
                "DocuSign envelope {}: {} (Notation {})",
                envelope["envelopeId"].as_str().unwrap_or("unknown"),
                envelope["status"].as_str().unwrap_or("unknown"),
                adoption["notation_id"],
            );
            if let Some(signers) = envelope["signers"].as_array() {
                for signer in signers {
                    println!(
                        "  {}: {}",
                        signer["name"].as_str().unwrap_or("unknown"),
                        signer["status"].as_str().unwrap_or("unknown")
                    );
                }
            }
        }
    }
    if let Some(workspace) = value["workspace"].as_object() {
        for (step, outcome) in workspace {
            println!("workspace {step}: {}", outcome.as_str().unwrap_or_default());
        }
    }
    if let Some(missing) = value["missing"].as_array() {
        for item in missing {
            println!("missing: {}", item.as_str().unwrap_or_default());
        }
    }
}

#[cfg(test)]
mod envelope_tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn matter_envelope_status_refreshes_the_scoped_record_and_reports_refusals() {
        for status in [200, 403] {
            let server = MockServer::start().await;
            let id = uuid::Uuid::nil();
            Mock::given(method("POST"))
                .and(path(format!("/app/api/projects/{id}/envelopes/refresh")))
                .and(header("authorization", "Bearer test-token"))
                .respond_with(
                    ResponseTemplate::new(status)
                        .set_body_json(serde_json::json!({"envelopes": []})),
                )
                .expect(1)
                .mount(&server)
                .await;
            let result = refresh_known(&server.uri(), "test-token", id).await;
            assert_eq!(result.is_ok(), status == 200);
        }
    }
}
