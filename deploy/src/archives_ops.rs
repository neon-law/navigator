//! Explicit archive operations. Writer and verifier are separate invocations/ADC identities.
use std::{path::PathBuf, process::ExitCode};

use anyhow::{ensure, Result};
use archives::write_only::operations::{self, Config, LocalReceipt, Request};
use clap::Subcommand;
use serde::Serialize;

#[derive(Debug, Subcommand)]
pub enum ArchivesCmd {
    /// Bind admitted source identities and reviewed paired coverage into private config.
    Config {
        #[arg(long)]
        local_root: PathBuf,
        #[arg(long)]
        destinations: PathBuf,
        #[arg(long)]
        source_project_number: Option<u64>,
        #[arg(
            long,
            conflicts_with = "registered_navigator_only",
            required_unless_present = "registered_navigator_only"
        )]
        coverage: Option<PathBuf>,
        #[arg(long)]
        registered_navigator_only: bool,
        #[arg(long)]
        out: PathBuf,
    },
    /// Read the admitted source's immutable authority fingerprint for private config review.
    SourceIdentity {
        #[arg(long)]
        out: PathBuf,
    },
    /// Capture registered Navigator tables once, retain raw local Iceberg, create offsite objects.
    /// Acknowledged uploads still require separate verification/catalog registration.
    Capture {
        #[arg(long)]
        config: PathBuf,
        /// Exact prior local receipt; never reads PostgreSQL or re-extracts when supplied.
        #[arg(long)]
        reuse_local: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Check every local object hash and query actual raw-file Iceberg tables, without source reads.
    VerifyLocal {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        receipt: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Separate reader ADC verifies project/UBLA/retention and complete Iceberg closure.
    /// Does not publish a catalog or attest effective writer IAM or PITR/DBOS recovery.
    VerifyCloud {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        destination: usize,
        #[arg(long)]
        report: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Render proposed create-only writer/separate-verifier bucket policy; applies NOTHING.
    ProvisioningPlan {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        writer: String,
        #[arg(long)]
        verifier: String,
        #[arg(long)]
        out: PathBuf,
    },
}

async fn read<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Result<T> {
    Ok(serde_json::from_slice(&tokio::fs::read(path).await?)?)
}

async fn read_local(path: &PathBuf) -> Result<LocalReceipt> {
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Saved {
        Receipt(LocalReceipt),
        Report(operations::Report),
    }
    Ok(match read(path).await? {
        Saved::Receipt(receipt) => receipt,
        Saved::Report(report) => report.local,
    })
}

async fn write(path: &PathBuf, value: &impl Serialize) -> Result<()> {
    use tokio::io::AsyncWriteExt as _;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .await?;
    file.write_all(&serde_json::to_vec_pretty(value)?).await?;
    file.sync_all().await?;
    Ok(())
}

pub async fn run(command: ArchivesCmd) -> ExitCode {
    if let Ok(code) = execute(command).await {
        code
    } else {
        // Storage/database details can include private content. Keep them out of CLI logs.
        eprintln!(
            "hudson: archive operation failed; no verified backup/catalog success was declared"
        );
        ExitCode::FAILURE
    }
}

async fn execute(command: ArchivesCmd) -> Result<ExitCode> {
    match command {
        ArchivesCmd::Config {
            local_root,
            destinations,
            source_project_number,
            coverage,
            registered_navigator_only: _,
            out,
        } => {
            let pg = store::postgres::PostgresDb::application().await?;
            let capture = match coverage {
                Some(path) => operations::CaptureScope::Paired {
                    coverage: read(&path).await?,
                    forgejo_identity_sha256: operations::forgejo_source_identity(&pg).await?,
                },
                None => operations::CaptureScope::RegisteredNavigator,
            };
            let config = Config {
                format_version: 1,
                local_root,
                destinations: read(&destinations).await?,
                source_project_number,
                source_identity_sha256: operations::source_identity(pg.pool()).await?,
                capture,
            };
            config.validate()?;
            write(&out, &config).await?;
            Ok(ExitCode::SUCCESS)
        }
        ArchivesCmd::SourceIdentity { out } => {
            let pg = store::postgres::PostgresDb::application().await?;
            let hash = operations::source_identity(pg.pool()).await?;
            write(&out, &serde_json::json!({"source_identity_sha256":hash})).await?;
            Ok(ExitCode::SUCCESS)
        }
        ArchivesCmd::Capture {
            config,
            reuse_local,
            out,
        } => {
            let config: Config = read(&config).await?;
            let reuse_local = match reuse_local {
                Some(path) => Some(read_local(&path).await?),
                None => None,
            };
            let request = Request {
                config_sha256: config.digest()?,
                config,
                reuse_local,
            };
            let report = operations::execute(&request).await?;
            let code = if report.all_destinations_acknowledged {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
            write(&out, &report).await?;
            eprintln!("Archive transport report written; cloud verification/catalog promotion remain separate");
            Ok(code)
        }
        ArchivesCmd::VerifyLocal {
            config,
            receipt,
            out,
        } => {
            let config: Config = read(&config).await?;
            let receipt = read_local(&receipt).await?;
            config.validate()?;
            write(&out, &operations::verify::local(&receipt, &config).await?).await?;
            Ok(ExitCode::SUCCESS)
        }
        ArchivesCmd::VerifyCloud {
            config,
            destination,
            report,
            out,
        } => {
            let config: Config = read(&config).await?;
            let previous = read(&report).await?;
            let report = verify_cloud_report(&config, &previous, destination).await?;
            write(&out, &report).await?;
            Ok(ExitCode::SUCCESS)
        }
        ArchivesCmd::ProvisioningPlan {
            config,
            writer,
            verifier,
            out,
        } => {
            let config = read(&config).await?;
            write(
                &out,
                &operations::provisioning_plan(&config, &writer, &verifier)?,
            )
            .await?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

async fn verify_cloud_report(
    config: &Config,
    previous: &operations::Report,
    destination: usize,
) -> Result<operations::verify::Verification> {
    ensure!(
        previous.config_sha256 == config.digest()?,
        "verification configuration differs from original request"
    );
    let entry = previous
        .destinations
        .iter()
        .find(|entry| entry.destination == destination)
        .ok_or_else(|| anyhow::anyhow!("destination attempt is missing"))?;
    let prepared = entry
        .prepared
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("destination was never prepared"))?;
    let generation = match &entry.outcome {
        Some(operations::DestinationOutcome::Acknowledged { generation, .. }) => Some(*generation),
        _ => None,
    };
    let report = operations::verify::cloud(
        config,
        destination,
        &prepared.inventory_key,
        generation,
        &prepared.inventory_sha256,
        &previous.local.inventory_sha256,
    )
    .await?;
    ensure!(
        !report.catalog_promoted,
        "verification cannot promote a catalog"
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    #[test]
    fn archive_commands_require_explicit_reviewed_scope_and_report_bound_verification() {
        assert!(crate::Cli::try_parse_from([
            "hudson",
            "ops",
            "archives",
            "config",
            "--local-root",
            "/synthetic-private-archive",
            "--destinations",
            "/synthetic/destinations.json",
            "--out",
            "/synthetic/config.json"
        ])
        .is_err());
        assert!(crate::Cli::try_parse_from([
            "hudson",
            "ops",
            "archives",
            "config",
            "--local-root",
            "/synthetic-private-archive",
            "--destinations",
            "/synthetic/destinations.json",
            "--coverage",
            "/synthetic/coverage.json",
            "--out",
            "/synthetic/config.json"
        ])
        .is_ok());
        assert!(crate::Cli::try_parse_from([
            "hudson",
            "ops",
            "archives",
            "verify-cloud",
            "--config",
            "/synthetic/config.json",
            "--destination",
            "0",
            "--report",
            "/synthetic/report.json",
            "--out",
            "/synthetic/verification.json"
        ])
        .is_ok());
        assert!(crate::Cli::try_parse_from([
            "hudson",
            "ops",
            "archives",
            "verify-cloud",
            "--config",
            "/synthetic/config.json",
            "--destination",
            "0",
            "--out",
            "/synthetic/verification.json"
        ])
        .is_err());
    }
}
