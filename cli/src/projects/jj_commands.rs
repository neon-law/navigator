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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::test_login::{answer, checkout, site};
    use serde_json::json;

    #[tokio::test]
    async fn every_command_fails_outside_a_project_checkout() {
        let empty = tempfile::tempdir().unwrap();
        let dir = || empty.path().to_path_buf();
        let revision = || "@".to_string();
        for action in [
            JjCommand::Setup { dir: dir() },
            JjCommand::Log { dir: dir() },
            JjCommand::StageTime {
                revision: revision(),
                dir: dir(),
            },
            JjCommand::Push {
                revision: revision(),
                dir: dir(),
            },
            JjCommand::Policy {
                dir: dir(),
                dry_run: true,
                review_team: None,
            },
        ] {
            assert_eq!(run(action).await, ExitCode::FAILURE);
        }
    }

    #[tokio::test]
    async fn the_log_and_staging_read_a_tracked_checkout() {
        let _jj = crate::projects::test_login::jj_stand_in();
        let tracked = tempfile::tempdir().unwrap();
        let store = tracked.path().join(".jj");
        std::fs::create_dir(&store).unwrap();
        let dir = || tracked.path().to_path_buf();
        assert_eq!(run(JjCommand::Log { dir: dir() }).await, ExitCode::SUCCESS);
        // A selected change whose checkout has no manifest cannot stage time.
        let record = json!({
            "commit_id": "commit-one", "change_id": "change-one", "author": "a@example.com",
            "description": "Subject\n\nBilling-Time: 0.2 2026-11-20 00000000-0000-4000-8000-000000000001",
            "signed": true, "conflict": false, "divergent": false,
        });
        std::fs::write(store.join("log"), format!("{record}\n")).unwrap();
        let stage = JjCommand::StageTime {
            revision: "@".into(),
            dir: dir(),
        };
        assert_eq!(run(stage).await, ExitCode::FAILURE);
    }

    #[tokio::test]
    async fn forgejo_policy_is_reconciled_only_for_the_projects_recorded_source() {
        let id = uuid::Uuid::new_v4();
        let (server, _login) = site(json!([{"id": id, "code": "sample-matter"}])).await;
        let root = server.uri();
        let origin = format!("{root}/sample-firm/sample-matter.git");
        let manifest = format!("project:\n  name: sample-matter\n  host: {root}\n");
        let dir = checkout(&manifest, Some(&origin));
        // With no token in the environment and no credential held by git for
        // this host, the checkout refuses to bind.
        std::env::remove_var("FORGEJO_TOKEN");
        std::env::remove_var("NAVIGATOR_FORGEJO_ROOT_URL");
        let error = policy(dir.path(), true, None).await.unwrap_err();
        assert!(error.to_string().contains("FORGEJO_TOKEN"), "{error:#}");

        std::env::set_var("NAVIGATOR_FORGEJO_ROOT_URL", &root);
        std::env::set_var("FORGEJO_TOKEN", "forge-token");
        let project = |origin: &str| {
            json!({
                "code": "sample-matter", "status": "open", "firm_id": null, "brand": "sample",
                "repository_remotes": {"origin": origin, "forgejo": null, "forgejo_status": "missing"},
            })
        };
        let door = format!("/app/api/projects/{id}");
        answer(
            &server,
            "GET",
            &door,
            200,
            project("https://forge.example.test/else"),
        )
        .await;
        let error = policy(dir.path(), true, None).await.unwrap_err();
        assert!(error
            .to_string()
            .contains("checkout origin must match the Project's recorded source URL"));
    }
}
