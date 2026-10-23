//! `navigator ops forgejo setup|auto-merge|schedule-open`: the operator's
//! door onto the shared Forgejo governance in [`cloud::forgejo_governance`].

use anyhow::{Context, Result};
pub(in crate::devx) use cloud::forgejo_governance::Forgejo;

fn client(root: &str) -> Result<(Forgejo, tokio::runtime::Runtime)> {
    let token = std::env::var("FORGEJO_TOKEN").context("FORGEJO_TOKEN must be configured")?;
    let forgejo = Forgejo::new(root, token)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    Ok((forgejo, runtime))
}

pub(in crate::devx) fn run(
    root: &str,
    repository: &str,
    context: &str,
    dry_run: bool,
    pull_request: Option<u64>,
) -> Result<()> {
    let (forgejo, runtime) = client(root)?;
    if let Some(number) = pull_request {
        runtime.block_on(forgejo.schedule(repository, context, number))?;
        println!("Governed squash auto-merge accepted for PR {number}");
    } else {
        let drift = runtime.block_on(forgejo.reconcile(repository, context, dry_run))?;
        if drift.is_empty() {
            println!("Readable merge controls already match");
        } else {
            println!(
                "{}: {}",
                if dry_run {
                    "Would reconcile"
                } else {
                    "Reconciled and verified readable controls"
                },
                drift.join(", ")
            );
        }
        if !dry_run {
            println!("Manual-merge controls written off; the API cannot verify their state");
        }
    }
    Ok(())
}

pub(in crate::devx) mod scheduler {
    use anyhow::Result;
    pub(in crate::devx) use cloud::forgejo_governance::scheduler::{
        require_no_actionable_outcomes, schedule_open, PullRequestOutcome,
    };

    pub(in crate::devx) fn run(root: &str, repository: &str, context: &str) -> Result<()> {
        let (forgejo, runtime) = super::client(root)?;
        let outcomes = runtime.block_on(schedule_open(&forgejo, repository, context))?;
        for outcome in &outcomes {
            println!("{}", serde_json::to_string(&outcome)?);
        }
        require_no_actionable_outcomes(&outcomes)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_shared_required_context_is_what_the_gate_workflow_posts_on_a_pull_request() {
        assert_eq!(
            super::super::actions_context(super::super::GATE_WORKFLOW, "pull_request").unwrap(),
            cloud::forgejo_governance::REQUIRED_CHECK_CONTEXT
        );
    }
}
