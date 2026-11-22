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
    },
}

pub async fn run(action: JjCommand) -> ExitCode {
    let result = match action {
        JjCommand::Setup { dir } => jj::run(&dir),
        JjCommand::Log { dir } => jj_log::run(&dir),
        JjCommand::StageTime { revision, dir } => {
            jj_billing::stage_in(&dir, &revision).await.map(|()| {
                println!("Time staged for review in Project Billing.");
            })
        }
        JjCommand::Push { revision, dir } => push::dispatch_jj(&dir, &revision).await,
        JjCommand::Policy { dir, dry_run } => policy(&dir, dry_run).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator jj: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn policy(dir: &Path, dry_run: bool) -> Result<()> {
    let origin = lifecycle::git_text(dir, &["remote", "get-url", "origin"])?;
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
        workspace::verify_policy_target(dir, &origin).await?;
        forge.reconcile_policy(dry_run).await
    }
}
