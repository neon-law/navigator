//! People commands use the same stored Navigator session as Project commands.
use anyhow::{anyhow, Context, Result};
use clap::Subcommand;
use std::process::ExitCode;

#[derive(Subcommand)]
pub enum PersonCmd {
    /// Create a contact, or administer a Firm's Firm roles.
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        email: String,
        /// Firm name or UUID. Requires Admin for that Firm, or Owner.
        #[arg(long, conflicts_with = "project")]
        firm: Option<String>,
        /// Optional Project context; creation does not grant Project access.
        #[arg(long, conflicts_with = "firm")]
        project: Option<String>,
        #[arg(long, default_value = "client", value_parser = ["client", "clerk", "lawyer", "admin", "owner"])]
        role: String,
        #[command(flatten)]
        host: crate::HostOpt,
    },
    /// Merge a duplicate client Person through the authorized site boundary.
    Merge {
        keep: String,
        drop: String,
        #[command(flatten)]
        host: crate::HostOpt,
    },
}

pub async fn run(action: PersonCmd) -> ExitCode {
    match action {
        PersonCmd::Create {
            name,
            email,
            firm: Some(firm),
            project: None,
            role,
            host,
        } => crate::remote::create_person(host.host, &firm, &name, &email, &role).await,
        PersonCmd::Create {
            name,
            email,
            firm: None,
            project,
            role,
            host,
        } => match create_contact(
            host.host.as_deref(),
            project.as_deref(),
            &name,
            &email,
            &role,
        )
        .await
        {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("navigator: {error:#}");
                ExitCode::FAILURE
            }
        },
        PersonCmd::Create { .. } => {
            eprintln!("navigator: --firm and --project cannot be combined");
            ExitCode::FAILURE
        }
        PersonCmd::Merge { keep, drop, host } => {
            crate::remote::merge_people(host.host, &keep, &drop).await
        }
    }
}

async fn create_contact(
    host: Option<&str>,
    code: Option<&str>,
    name: &str,
    email: &str,
    role: &str,
) -> Result<()> {
    let (base, token) = crate::remote::resolve(host)?;
    let project = if let Some(code) = code {
        Some(
            crate::remote::visible_projects_on(&base, &token)
                .await?
                .into_iter()
                .find(|(_, project_code)| project_code == code)
                .map(|(id, _)| id)
                .ok_or_else(|| anyhow!("Project is not available to this session"))?,
        )
    } else {
        None
    };
    let response = reqwest::Client::new()
        .post(format!("{base}/app/api/people"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name":name,"email":email,"role":role,"project_id":project}))
        .send()
        .await
        .context("create Project Person")?
        .error_for_status()
        .context("Person creation refused")?;
    let person: serde_json::Value = response.json().await.context("read created Person")?;
    let id = person["id"]
        .as_str()
        .ok_or_else(|| anyhow!("Person response omitted its id"))?;
    println!("{id}");
    Ok(())
}
