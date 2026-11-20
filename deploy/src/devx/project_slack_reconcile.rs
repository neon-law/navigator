//! Privileged, PG-only resolution of retained client-view posting evidence.
//! The Person UUID attributes the operator decision; it is not a login credential.

use anyhow::{Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use store::project_slack_deliveries::reconciliation::{self, Decision, Request};
use uuid::Uuid;

#[derive(Subcommand)]
pub enum Operation {
    /// Resolve verified retained client-view evidence without sending a notice.
    ReconcileClientView(ReconcileArgs),
}

#[derive(Clone, Copy, ValueEnum)]
pub enum DecisionArg {
    Posted,
    VerifiedNotPosted,
}

/// Record an operator-verified decision. This command never sends to Slack.
#[derive(Args)]
pub struct ReconcileArgs {
    /// Stable audit operation; reuse only for the identical decision.
    #[arg(long)]
    operation: Uuid,
    #[arg(long)]
    project: Uuid,
    /// Expected current native ownership epoch, never a Source capture override.
    #[arg(long)]
    ownership_epoch: Uuid,
    /// Attributed Person, verified for current Firm-management authority.
    /// The privileged PostgreSQL connection authorizes this operator command;
    /// supplying a Person UUID does not authenticate that Person.
    #[arg(long)]
    actor_person: Uuid,
    /// Opaque UUID of private evidence the operator independently reviewed.
    /// No evidence content or provider credentials belong on the command line.
    #[arg(long)]
    evidence_reference: Uuid,
    #[arg(long, value_enum)]
    decision: DecisionArg,
}

impl ReconcileArgs {
    fn request(self) -> Request {
        Request {
            operation_id: self.operation,
            project_id: self.project,
            ownership_epoch: self.ownership_epoch,
            actor_person_id: self.actor_person,
            evidence_reference: self.evidence_reference,
            decision: match self.decision {
                DecisionArg::Posted => Decision::Posted,
                DecisionArg::VerifiedNotPosted => Decision::VerifiedNotPosted,
            },
        }
    }
}

pub fn run(operation: Operation) -> Result<()> {
    let Operation::ReconcileClientView(args) = operation;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build operator runtime")?;
    runtime.block_on(async {
        crate::application_store()
            .await
            .context("verify PostgreSQL application authority")?;
        let pg = store::postgres::PostgresDb::application()
            .await
            .context("connect to admitted PostgreSQL")?;
        reconciliation::reconcile(&pg, args.request())
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        println!("Project Slack reconciliation recorded; no provider request made");
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Command {
        #[command(subcommand)]
        operation: Operation,
    }

    fn argv() -> Vec<String> {
        let id = Uuid::from_u128(1).to_string();
        [
            "project-slack",
            "reconcile-client-view",
            "--operation",
            &id,
            "--project",
            &id,
            "--ownership-epoch",
            &id,
            "--actor-person",
            &id,
            "--evidence-reference",
            &id,
            "--decision",
            "verified-not-posted",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    #[test]
    fn reconciliation_cli_requires_typed_evidence_actor_epoch_and_explicit_decision() {
        let parsed = Command::try_parse_from(argv()).unwrap();
        let Operation::ReconcileClientView(args) = parsed.operation;
        assert_eq!(args.request().decision, Decision::VerifiedNotPosted);
        for flag in [
            "--operation",
            "--project",
            "--ownership-epoch",
            "--actor-person",
            "--evidence-reference",
            "--decision",
        ] {
            let mut args = argv();
            let index = args.iter().position(|arg| arg == flag).unwrap();
            args.drain(index..=index + 1);
            assert!(Command::try_parse_from(args).is_err());
        }
        for (flag, value) in [
            ("--actor-person", "operator"),
            ("--evidence-reference", "private body"),
            ("--decision", "retry"),
        ] {
            let mut args = argv();
            let index = args.iter().position(|arg| arg == flag).unwrap();
            args[index + 1] = value.into();
            assert!(Command::try_parse_from(args).is_err());
        }
    }
}
