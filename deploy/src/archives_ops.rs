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
        #[arg(
            long,
            conflicts_with = "object_store",
            required_unless_present = "object_store"
        )]
        local_root: Option<PathBuf>,
        /// Explicit source bucket/prefix, credential path and byte/object limits JSON.
        #[arg(
            long,
            conflicts_with = "local_root",
            required_unless_present = "local_root"
        )]
        object_store: Option<PathBuf>,
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
    /// Seal Navigator rows once in the configured local/object store, then create offsite objects.
    /// Acknowledged uploads still require separate verification/catalog registration.
    Capture {
        #[arg(long)]
        config: PathBuf,
        /// Exact sealed source receipt; never reads PostgreSQL or re-extracts when supplied.
        #[arg(long, alias = "reuse-sealed")]
        reuse_local: Option<PathBuf>,
        /// Stable operator-selected identity for object capture/recovery; changed cutoffs need new identities.
        #[arg(long)]
        capture_id: Option<String>,
        /// Explicit bounded retry from an existing sealed generation.
        #[arg(long, default_value_t=0, value_parser=clap::value_parser!(u8).range(0..=3))]
        delivery_attempt: u8,
        #[arg(long)]
        out: PathBuf,
    },
    /// Verify the sealed source objects and query Iceberg without PostgreSQL reads.
    #[command(alias = "verify-local")]
    VerifySealed {
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
            object_store,
            destinations,
            source_project_number,
            coverage,
            registered_navigator_only: _,
            out,
        } => {
            configure(
                local_root,
                object_store,
                destinations,
                source_project_number,
                coverage,
                out,
            )
            .await
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
            capture_id,
            delivery_attempt,
            out,
        } => {
            let config: Config = read(&config).await?;
            let reuse_local = match reuse_local {
                Some(path) => Some(read_local(&path).await?),
                None => None,
            };
            let request = Request {
                capture_id,
                delivery_attempt,
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
        ArchivesCmd::VerifySealed {
            config,
            receipt,
            out,
        } => {
            let config: Config = read(&config).await?;
            let receipt = read_local(&receipt).await?;
            config.validate()?;
            write(&out, &operations::verify::sealed(&receipt, &config).await?).await?;
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

async fn configure(
    local_root: Option<PathBuf>,
    object_store: Option<PathBuf>,
    destinations: PathBuf,
    source_project_number: Option<u64>,
    coverage: Option<PathBuf>,
    out: PathBuf,
) -> Result<ExitCode> {
    let pg = store::postgres::PostgresDb::application().await?;
    let capture = match coverage {
        Some(path) => operations::CaptureScope::Paired {
            coverage: read(&path).await?,
            forgejo_identity_sha256: operations::forgejo_source_identity(&pg).await?,
        },
        None => operations::CaptureScope::RegisteredNavigator,
    };
    let object_store: Option<operations::object_store::Config> = match object_store {
        Some(path) => Some(read(&path).await?),
        None => None,
    };
    let config = Config {
        format_version: if object_store.is_some() { 2 } else { 1 },
        object_store,
        local_root: local_root.unwrap_or_default(),
        destinations: read(&destinations).await?,
        source_project_number,
        source_identity_sha256: operations::source_identity(pg.pool()).await?,
        capture,
    };
    config.validate()?;
    write(&out, &config).await?;
    Ok(ExitCode::SUCCESS)
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
    use super::*;
    use clap::Parser as _;

    /// A private directory holding a valid local-root configuration with one destination.
    fn configured() -> (tempfile::TempDir, PathBuf, Config) {
        let dir = tempfile::tempdir().unwrap();
        let value = serde_json::json!({
            "format_version": 1,
            "local_root": dir.path().join("sealed"),
            "source_project_number": null,
            "source_identity_sha256": "a".repeat(64),
            "capture": {"scope": "registered_navigator"},
            "destinations": [{
                "project_number": 7,
                "bucket": "example-offsite-archive",
                "location": "us-west1",
                "minimum_retention_seconds":
                    cloud::archives_project::OFFSITE_ARCHIVES_RETENTION_SECONDS,
            }],
        });
        let path = dir.path().join("config.json");
        std::fs::write(&path, value.to_string()).unwrap();
        let config = serde_json::from_value(value).unwrap();
        (dir, path, config)
    }

    fn receipt(root: &std::path::Path) -> serde_json::Value {
        serde_json::json!({
            "root": root,
            "root_uri": "file:///synthetic",
            "inventory_key": "inventory.json",
            "inventory_sha256": "b".repeat(64),
            "cutoff_at": "2026-01-02T03:04:05Z",
            "tables": 1,
            "rows": 2,
        })
    }

    fn report(config: &Config, root: &std::path::Path) -> serde_json::Value {
        serde_json::json!({
            "config_sha256": config.digest().unwrap(),
            "local": receipt(root),
            "destinations": [
                {"destination": 0, "prepared": null, "outcome": null, "preparation_failed": true}
            ],
            "all_destinations_acknowledged": false,
            "catalog_promoted": false,
        })
    }

    #[tokio::test]
    async fn the_provisioning_plan_is_written_once_as_a_private_proposal() {
        use std::os::unix::fs::PermissionsExt as _;
        let (dir, config, _) = configured();
        let out = dir.path().join("plan.json");
        let plan = |writer: &str, out: &PathBuf| ArchivesCmd::ProvisioningPlan {
            config: config.clone(),
            writer: writer.to_owned(),
            verifier: "serviceAccount:example-verifier".to_owned(),
            out: out.clone(),
        };
        assert_eq!(
            run(plan("serviceAccount:example-writer", &out)).await,
            ExitCode::SUCCESS
        );
        assert_eq!(out.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        let written: serde_json::Value = read(&out).await.unwrap();
        let destination = &written["destinations"][0];
        assert_eq!(destination["bucket"]["name"], "example-offsite-archive");
        assert_eq!(
            destination["writer_binding"],
            serde_json::json!({
                "role": "roles/storage.objectCreator",
                "member": "serviceAccount:example-writer"
            })
        );
        assert_eq!(destination["automatic_apply"], false);

        let before = std::fs::read(&out).unwrap();
        assert_eq!(
            run(plan("serviceAccount:example-other", &out)).await,
            ExitCode::FAILURE,
            "an existing output is never replaced"
        );
        assert_eq!(std::fs::read(&out).unwrap(), before);
        let same_identity = dir.path().join("same.json");
        assert_eq!(
            run(plan("serviceAccount:example-verifier", &same_identity)).await,
            ExitCode::FAILURE
        );
        assert!(!same_identity.exists());
    }

    #[tokio::test]
    async fn a_saved_report_or_its_receipt_names_the_same_sealed_source() {
        let (dir, _, config) = configured();
        let (receipt_path, report_path) = (
            dir.path().join("receipt.json"),
            dir.path().join("report.json"),
        );
        std::fs::write(&receipt_path, receipt(dir.path()).to_string()).unwrap();
        std::fs::write(&report_path, report(&config, dir.path()).to_string()).unwrap();
        let from_receipt = read_local(&receipt_path).await.unwrap();
        let from_report = read_local(&report_path).await.unwrap();
        assert_eq!(from_receipt.inventory_sha256, "b".repeat(64));
        assert_eq!(
            serde_json::to_value(from_receipt).unwrap(),
            serde_json::to_value(from_report).unwrap()
        );
        std::fs::write(&receipt_path, "{}").unwrap();
        assert!(read_local(&receipt_path).await.is_err());
    }

    #[tokio::test]
    async fn cloud_verification_is_bound_to_the_original_config_and_a_prepared_destination() {
        let (dir, config_path, config) = configured();
        let previous: operations::Report =
            serde_json::from_value(report(&config, dir.path())).unwrap();
        let refusal = |config: Config, previous: operations::Report, destination| async move {
            verify_cloud_report(&config, &previous, destination)
                .await
                .map(drop)
                .expect_err("refused")
                .to_string()
        };
        assert_eq!(
            refusal(config.clone(), previous.clone(), 0).await,
            "destination was never prepared"
        );
        assert_eq!(
            refusal(config.clone(), previous.clone(), 1).await,
            "destination attempt is missing"
        );
        let mut retargeted = config.clone();
        retargeted.destinations[0].bucket = "example-other-archive".to_owned();
        assert_eq!(
            refusal(retargeted, previous, 0).await,
            "verification configuration differs from original request"
        );

        let report_path = dir.path().join("report.json");
        std::fs::write(&report_path, report(&config, dir.path()).to_string()).unwrap();
        let out = dir.path().join("verification.json");
        assert_eq!(
            run(ArchivesCmd::VerifyCloud {
                config: config_path,
                destination: 0,
                report: report_path,
                out: out.clone(),
            })
            .await,
            ExitCode::FAILURE
        );
        assert!(!out.exists(), "a refusal declares no verification");
    }

    #[tokio::test]
    async fn an_unsealed_source_fails_capture_and_verification_without_writing_a_result() {
        let (dir, config, _) = configured();
        let receipt_path = dir.path().join("receipt.json");
        std::fs::write(
            &receipt_path,
            receipt(&dir.path().join("sealed")).to_string(),
        )
        .unwrap();
        let out = dir.path().join("out.json");
        for command in [
            ArchivesCmd::Capture {
                config: config.clone(),
                reuse_local: Some(receipt_path.clone()),
                capture_id: None,
                delivery_attempt: 0,
                out: out.clone(),
            },
            ArchivesCmd::VerifySealed {
                config: config.clone(),
                receipt: receipt_path.clone(),
                out: out.clone(),
            },
            ArchivesCmd::Capture {
                config: dir.path().join("missing-config.json"),
                reuse_local: None,
                capture_id: None,
                delivery_attempt: 0,
                out: out.clone(),
            },
        ] {
            assert_eq!(run(command).await, ExitCode::FAILURE);
            assert!(!out.exists());
        }
    }

    #[test]
    fn cloud_archive_config_accepts_object_source_and_refuses_ambiguous_backing() {
        let base = [
            "hudson",
            "ops",
            "archives",
            "config",
            "--destinations",
            "/synthetic/destinations.json",
            "--coverage",
            "/synthetic/coverage.json",
            "--out",
            "/synthetic/config.json",
        ];
        assert!(crate::Cli::try_parse_from(base).is_err());
        let object = base
            .into_iter()
            .chain(["--object-store", "/synthetic/source-store.json"]);
        assert!(crate::Cli::try_parse_from(object.clone()).is_ok());
        assert!(
            crate::Cli::try_parse_from(object.chain(["--local-root", "/synthetic/local"])).is_err()
        );
        assert!(crate::Cli::try_parse_from([
            "hudson",
            "ops",
            "archives",
            "verify-sealed",
            "--config",
            "/synthetic/config.json",
            "--receipt",
            "/synthetic/receipt.json",
            "--out",
            "/synthetic/verification.json"
        ])
        .is_ok());
    }

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
