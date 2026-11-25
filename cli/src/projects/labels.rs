//! Read and change Project Labels through Navigator's authenticated API.

use std::{process::ExitCode, time::Duration};

use anyhow::{bail, Context, Result};
use clap::{Args as ClapArgs, Subcommand};
use serde_json::{json, Value};

use crate::{projects, remote};

#[derive(ClapArgs)]
pub struct Args {
    #[arg(long, global = true)]
    project: Option<String>,
    #[arg(long, global = true)]
    host: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show the Firm catalog and which labels this Project carries.
    List,
    /// Create a Firm label by stable key. Requires Firm administration.
    Create {
        key: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        category: String,
    },
    /// Hide a label from filters and new assignments. Requires Firm administration.
    Retire { key: String },
    /// Make a retired label available again. Requires Firm administration.
    Restore { key: String },
    /// Apply one or more active Firm labels to this Project.
    Add {
        #[arg(required = true)]
        keys: Vec<String>,
    },
    /// Remove one or more label assignments from this Project.
    Remove {
        #[arg(required = true)]
        keys: Vec<String>,
    },
}

pub(crate) async fn run(args: Args) -> ExitCode {
    match request(args).await {
        Ok(labels) => {
            println!("{labels}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator project labels: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn request(args: Args) -> Result<Value> {
    let (host, code) = projects::project_target(args.host.as_deref(), args.project.as_deref())?;
    let (base, token) = remote::resolve(host.as_deref())?;
    let project = projects::surfaces::resolve_project_id(&base, &token, &code).await?;
    let url = format!("{base}/app/api/projects/{project}/labels");

    let body = match args.command {
        Command::List => None,
        Command::Create {
            key,
            name,
            category,
        } => Some(json!({ "action": "create", "key": key, "name": name, "category": category })),
        Command::Retire { key } => Some(json!({ "action": "retire", "key": key })),
        Command::Restore { key } => Some(json!({ "action": "restore", "key": key })),
        Command::Add { keys } => Some(json!({ "action": "add", "keys": keys })),
        Command::Remove { keys } => Some(json!({ "action": "remove", "keys": keys })),
    };
    let client = reqwest::Client::new();
    let request = match body {
        Some(body) => client.post(url).json(&body),
        None => client.get(url),
    };
    let response = request
        .bearer_auth(token)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .context("request Project Labels")?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("read Project Labels response")?;
    if !status.is_success() {
        bail!("label command refused: {status} {body}");
    }
    serde_json::from_str(&body).context("parse Project Labels response")
}
