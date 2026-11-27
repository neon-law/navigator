//! Post a receipt-backed Project Cost through the authenticated matter API.

use std::{process::ExitCode, time::Duration};

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use uuid::Uuid;

use crate::{
    commands::{parse_calendar_date, HostOpt},
    remote,
};

#[derive(clap::Subcommand)]
pub enum CostCmd {
    /// Post the filed receipt as a confirmed, unbilled expense.
    Create {
        #[command(flatten)]
        host: HostOpt,
        /// Project code. Defaults to the Project checkout's code.
        #[arg(long)]
        project: Option<String>,
        /// UUID of the filed Receipt Document.
        #[arg(long)]
        receipt: Uuid,
        /// Date of the charge (YYYY-MM-DD).
        #[arg(long, value_parser = parse_calendar_date)]
        occurred_on: NaiveDate,
        /// Confirm the receipt amount and post the cost.
        #[arg(long, required = true)]
        confirm: bool,
    },
}

#[derive(serde::Deserialize)]
struct PostedCost {
    id: Uuid,
}

pub async fn run(action: CostCmd) -> ExitCode {
    let CostCmd::Create {
        host,
        project,
        receipt,
        occurred_on,
        confirm: _,
    } = action;
    match create(
        host.host.as_deref(),
        project.as_deref(),
        receipt,
        occurred_on,
    )
    .await
    {
        Ok((id, true)) => {
            println!("Posted Project Cost {id}");
            ExitCode::SUCCESS
        }
        Ok((id, false)) => {
            println!("Skipped existing Project Cost {id}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator project cost create: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn create(
    host: Option<&str>,
    project: Option<&str>,
    receipt_document_id: Uuid,
    occurred_on: NaiveDate,
) -> Result<(Uuid, bool)> {
    let (host, project) = super::project_target(host, project)?;
    let (base, token) = remote::resolve(host.as_deref())?;
    let id = super::surfaces::resolve_project_id(&base, &token, &project).await?;
    let response = remote::authenticated_client_builder(&base)?
        .build()?
        .post(format!("{base}/app/api/projects/{id}/costs/from-receipt"))
        .timeout(Duration::from_secs(30))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "receipt_document_id": receipt_document_id,
            "occurred_on": occurred_on,
        }))
        .send()
        .await
        .context("post receipt as a Project Cost")?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("read Project Cost response")?;
    let created = match status {
        reqwest::StatusCode::CREATED => true,
        reqwest::StatusCode::OK => false,
        _ => bail!("{}", remote::server_error(status, &body)),
    };
    let cost: PostedCost = serde_json::from_str(&body).context("parse Project Cost response")?;
    Ok((cost.id, created))
}
