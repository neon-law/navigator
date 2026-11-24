//! Lawyer-facing commands for a Jujutsu checkout, independent of its forge.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

use super::{
    forge_forgejo::ForgejoForge, jj, jj_billing, jj_log, lifecycle, push, repository, workspace,
};

#[derive(Subcommand)]
pub enum JjCommand {
    /// Set up Jujutsu in this Git checkout.
    Setup {
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Show changes, authors and conflicts.
    Log {
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// List open pull requests, their authors and requested reviewers on this Project's forge.
    Reviews {
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Stage annotated time for lawyer review.
    StageTime {
        #[arg(long, default_value = "@")]
        revision: String,
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Stage annotated time, land the change and verify deployment.
    Push {
        #[arg(long, default_value = "@")]
        revision: String,
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Reconcile the checkout's repository review policy.
    Policy {
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Report drift without changing repository policy.
        #[arg(long)]
        dry_run: bool,
        /// Require this forge team's approval for changes to the Project's main branch.
        #[arg(long)]
        review_team: Option<String>,
    },
}

pub async fn run(action: JjCommand) -> ExitCode {
    let result = match action {
        JjCommand::Setup { dir } => jj::run(&dir),
        JjCommand::Log { dir } => jj_log::run(&dir),
        JjCommand::Reviews { dir } => super::reviews::list(&dir).await,
        JjCommand::StageTime { revision, dir } => {
            jj_billing::stage_in(&dir, &revision).await.map(|()| {
                println!("Time staged for review in Project Billing.");
            })
        }
        JjCommand::Push { revision, dir } => push::dispatch_jj(&dir, &revision).await,
        JjCommand::Policy {
            dir,
            dry_run,
            review_team,
        } => policy(&dir, dry_run, review_team.as_deref()).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator jj: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn policy(dir: &Path, dry_run: bool, review_team: Option<&str>) -> Result<()> {
    let origin = lifecycle::git_text(dir, &["remote", "get-url", "origin"])?;
    if let Some(team) = review_team {
        anyhow::ensure!(
            model::workspace::is_valid_slug(team),
            "use the review team's slug"
        );
        workspace::verify_checkout_binding(dir, &origin).await?;
        if let Some(target) = crate::github_setup::RepositoryTarget::from_origin(&origin)? {
            return crate::github_setup::require_review_team(&target, team, dry_run).await;
        }
        return ForgejoForge::from_checkout(dir)?
            .require_review_team(team, dry_run)
            .await;
    }
    if let Some(target) = crate::github_setup::RepositoryTarget::from_origin(&origin)? {
        crate::github_setup::run_async(
            &target,
            dry_run,
            crate::published_cli_version(),
            repository::DEFAULT_WORKFLOW_HOME,
        )
        .await
    } else {
        let forge = ForgejoForge::from_checkout(dir)?;
        workspace::verify_checkout_binding(dir, &origin).await?;
        forge.reconcile_policy(dry_run).await
    }
}
