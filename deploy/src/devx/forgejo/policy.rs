//! `hudson ops forgejo setup|auto-merge|schedule-open`: the operator's
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
        println!("{}", reconciliation_report(&drift, dry_run));
    }
    Ok(())
}

/// What a reconcile pass tells the operator about the controls it found drifted.
fn reconciliation_report(drift: &[String], dry_run: bool) -> String {
    let mut report = match (drift.is_empty(), dry_run) {
        (true, _) => "Readable merge controls already match".to_owned(),
        (false, true) => format!("Would reconcile: {}", drift.join(", ")),
        (false, false) => format!(
            "Reconciled and verified readable controls: {}",
            drift.join(", ")
        ),
    };
    if !dry_run {
        report += "\nManual-merge controls written off; the API cannot verify their state";
    }
    report
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
    fn a_reconcile_names_its_drift_and_only_a_real_run_claims_a_write() {
        let drift = ["required checks".to_owned(), "merge style".to_owned()];
        assert_eq!(
            super::reconciliation_report(&drift, true),
            "Would reconcile: required checks, merge style"
        );
        assert_eq!(
            super::reconciliation_report(&drift, false),
            "Reconciled and verified readable controls: required checks, merge style\n\
             Manual-merge controls written off; the API cannot verify their state"
        );
        assert_eq!(
            super::reconciliation_report(&[], true),
            "Readable merge controls already match"
        );
        assert!(super::reconciliation_report(&[], false)
            .starts_with("Readable merge controls already match\nManual-merge"));
    }

    #[test]
    fn the_shared_required_context_is_what_the_gate_workflow_posts_on_a_pull_request() {
        assert_eq!(
            super::super::actions_context(super::super::GATE_WORKFLOW, "pull_request").unwrap(),
            cloud::forgejo_governance::REQUIRED_CHECK_CONTEXT
        );
    }
}
