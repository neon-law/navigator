use std::{path::Path, process::ExitCode, time::Duration};

use anyhow::{Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{projects, remote};

#[derive(Subcommand)]
pub(super) enum Command {
    /// Append an observation about a Person or Entity.
    Add(Add),
}

#[derive(Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
enum Scope {
    Project,
    Attorney,
    Firm,
}

#[derive(Args)]
#[command(group(clap::ArgGroup::new("subject").args(["person", "entity"]).required(true)))]
pub(super) struct Add {
    /// Where the relationship was observed.
    #[arg(long)]
    scope: Scope,
    /// Project code. Defaults to the current checkout's Project for project scope.
    #[arg(long)]
    project: Option<String>,
    /// Firm ID, required for attorney and firm scope.
    #[arg(long, conflicts_with = "project")]
    firm: Option<Uuid>,
    /// ID of an existing Person.
    #[arg(long)]
    person: Option<Uuid>,
    /// ID of an existing Entity.
    #[arg(long)]
    entity: Option<Uuid>,
    /// Relationship type in lower snake_case, such as witness or co_counsel.
    #[arg(long = "type")]
    action: String,
    /// Audit detail.
    #[arg(long)]
    detail: String,
    /// Reuse this ID when retrying the same observation after a connection failure.
    #[arg(long)]
    request_id: Option<Uuid>,
    #[command(flatten)]
    host: super::HostOpt,
}

#[derive(Serialize)]
struct Request<'a> {
    request_id: Uuid,
    scope: Scope,
    firm_id: Option<Uuid>,
    project_id: Option<Uuid>,
    subject: &'static str,
    subject_id: Uuid,
    action: &'a str,
    detail: &'a str,
}

#[derive(Deserialize)]
struct Receipt {
    id: Uuid,
}

pub(super) async fn run(Command::Add(args): Command) -> ExitCode {
    match add(args).await {
        Ok(receipt) => {
            println!("Recorded Relationship Log {}", receipt.id);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator relationship add: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn add(args: Add) -> Result<Receipt> {
    let manifest = projects::manifest::read(Path::new("."));
    let host = args
        .host
        .host
        .or_else(|| manifest.as_ref().and_then(|m| m.host.clone()));
    let project = match (args.scope, args.project, args.firm) {
        (Scope::Project, project, None) => Some(
            project
                .or_else(|| manifest.and_then(|m| m.project))
                .context("pass --project or run from a Project checkout")?,
        ),
        (Scope::Attorney | Scope::Firm, None, Some(_)) => None,
        _ => {
            anyhow::bail!("use --project with project scope or --firm with attorney and firm scope")
        }
    };
    let (subject, subject_id) = match (args.person, args.entity) {
        (Some(id), None) => ("person", id),
        (None, Some(id)) => ("entity", id),
        _ => anyhow::bail!("pass exactly one of --person or --entity"),
    };
    let (base, token) = remote::resolve(host.as_deref())?;
    let project_id = match project {
        Some(code) => Some(projects::surfaces::resolve_project_id(&base, &token, &code).await?),
        None => None,
    };
    let request = Request {
        request_id: args.request_id.unwrap_or_else(Uuid::now_v7),
        scope: args.scope,
        firm_id: args.firm,
        project_id,
        subject,
        subject_id,
        action: &args.action,
        detail: &args.detail,
    };
    send(&base, &token, &request).await
}

async fn send(base: &str, token: &str, request: &Request<'_>) -> Result<Receipt> {
    let response = reqwest::Client::new()
        .post(format!("{base}/app/api/graph/relationship-log"))
        .timeout(Duration::from_secs(30))
        .bearer_auth(token)
        .json(request)
        .send()
        .await
        .with_context(|| {
            format!(
                "connection failed; retry with --request-id {}",
                request.request_id
            )
        })?;
    let status = response.status();
    if status != reqwest::StatusCode::CREATED {
        anyhow::bail!("site refused the observation (HTTP {status})");
    }
    response
        .json()
        .await
        .context("read the Relationship Log receipt")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn add_posts_scope_and_subject_without_an_actor() {
        let server = MockServer::start().await;
        let request = Request {
            request_id: Uuid::now_v7(),
            scope: Scope::Project,
            firm_id: None,
            project_id: Some(Uuid::now_v7()),
            subject: "person",
            subject_id: Uuid::now_v7(),
            action: "witness",
            detail: "Observed the signing",
        };
        Mock::given(method("POST"))
            .and(path("/app/api/graph/relationship-log"))
            .and(body_json(serde_json::json!({
                "request_id": request.request_id, "scope": "project", "firm_id": null,
                "project_id": request.project_id, "subject": "person", "subject_id": request.subject_id,
                "action": "witness", "detail": "Observed the signing",
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": request.request_id})))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            send(&server.uri(), "token", &request).await.unwrap().id,
            request.request_id
        );
    }
}
