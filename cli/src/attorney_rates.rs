//! Effective-dated Attorney Rates through the authenticated Navigator site.

use anyhow::{anyhow, Context, Result};
use chrono::NaiveDate;
use clap::Subcommand;
use serde_json::{json, Value};
use std::process::ExitCode;
use uuid::Uuid;

#[derive(Subcommand)]
pub enum RateAction {
    /// Add an hourly rate effective on the given date.
    Set {
        #[arg(long)]
        attorney: String,
        #[arg(long)]
        cents_per_hour: i32,
        #[arg(long)]
        effective_on: NaiveDate,
    },
    /// Show the attorney's effective-dated rate history.
    List {
        #[arg(long)]
        attorney: String,
    },
}

#[derive(Clone, Copy)]
enum Scope {
    Firm,
    Project(Uuid),
}

/// Set or list one matter's rate history.
pub async fn project(host: Option<&str>, project: Option<&str>, action: RateAction) -> ExitCode {
    let result = async {
        let (host, code) = crate::projects::project_target(host, project)?;
        let (base, token) = crate::remote::resolve(host.as_deref())?;
        let project_id = crate::remote::visible_projects_on(&base, &token)
            .await?
            .into_iter()
            .find_map(|(id, candidate)| (candidate == code).then_some(id))
            .ok_or_else(|| anyhow!("Project is not available to this session"))?;
        operate(&base, &token, Scope::Project(project_id), action).await
    }
    .await;
    finish("project rate", result)
}

/// Set or list a Firm-wide default rate.
pub async fn firm(host: Option<&str>, action: RateAction) -> ExitCode {
    let result = async {
        let (base, token) = crate::remote::resolve(host)?;
        operate(&base, &token, Scope::Firm, action).await
    }
    .await;
    finish("firm rate", result)
}

async fn operate(base: &str, token: &str, scope: Scope, action: RateAction) -> Result<Value> {
    let attorney = match &action {
        RateAction::Set { attorney, .. } | RateAction::List { attorney } => attorney,
    };
    let person_id = person_id(base, token, attorney).await?;
    let client = reqwest::Client::new();
    match action {
        RateAction::Set {
            cents_per_hour,
            effective_on,
            ..
        } => request(
            client
                .post(format!("{base}/app/api/attorney-rates"))
                .bearer_auth(token)
                .json(&json!({
                    "person_id": person_id,
                    "project_id": match scope { Scope::Firm => None, Scope::Project(id) => Some(id) },
                    "hourly_rate_cents": cents_per_hour,
                    "effective_on": effective_on,
                })),
            "set Attorney Rate",
        )
        .await,
        RateAction::List { .. } => {
            let mut url = reqwest::Url::parse(&format!("{base}/app/api/attorney-rates"))?;
            url.query_pairs_mut()
                .append_pair("person_id", &person_id.to_string());
            if let Scope::Project(project_id) = scope {
                url.query_pairs_mut()
                    .append_pair("project_id", &project_id.to_string());
            }
            let response = request(
                client.get(url).bearer_auth(token),
                "list Attorney Rates",
            )
            .await?;
            Ok(response)
        }
    }
}

async fn person_id(base: &str, token: &str, email: &str) -> Result<Uuid> {
    let mut url = reqwest::Url::parse(&format!("{base}/app/api/people"))?;
    url.query_pairs_mut()
        .append_pair("filter[email]", email)
        .append_pair("page[size]", "10");
    let directory = request(
        reqwest::Client::new().get(url).bearer_auth(token),
        "find Attorney",
    )
    .await?;
    directory
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|person| {
            let matches = person
                .pointer("/attributes/email")
                .and_then(Value::as_str)
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(email));
            matches
                .then(|| person.get("id").and_then(Value::as_str))
                .flatten()
                .and_then(|id| Uuid::parse_str(id).ok())
        })
        .ok_or_else(|| anyhow!("No Attorney matches the requested email"))
}

async fn request(request: reqwest::RequestBuilder, action: &str) -> Result<Value> {
    let response = request
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .with_context(|| format!("{action} through Navigator"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .with_context(|| format!("read {action} response"))?;
    if !status.is_success() {
        return Err(anyhow!("{action} refused: {status} {}", first_line(&body)));
    }
    serde_json::from_str(&body).with_context(|| format!("parse {action} response"))
}

fn first_line(body: &str) -> &str {
    body.lines().next().unwrap_or_default()
}

fn finish(command: &str, result: Result<Value>) -> ExitCode {
    let result = result.map(|value| serde_json::to_string_pretty(&value).unwrap_or_default());
    crate::billing::finish(command, result)
}
