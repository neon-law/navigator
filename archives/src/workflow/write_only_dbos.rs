//! Request-bound create-only archive phases on the existing Archives workflow.
use std::sync::Arc;

use super::{ArchivesDbosError, RunReport};
use crate::write_only::operations::{
    self, DestinationOutcome, DestinationReport, LocalReceipt, Report, Request,
};

fn failed() -> dbos::Error<ArchivesDbosError> {
    dbos::Error::Application(ArchivesDbosError::Report(
        "sealed write-only archive generation unavailable".into(),
    ))
}

pub(super) async fn run(
    request: Request,
    slack: Arc<dyn workflows::SlackBot>,
    channel: String,
    slack_enabled: bool,
    phases: Arc<dyn super::ArchivesDbosPhases>,
) -> dbos::Result<RunReport, ArchivesDbosError> {
    request.validate().map_err(|_| failed())?;
    let prepare = request.clone();
    let source = Arc::clone(&phases);
    let local = dbos::step_with(
        "write_only.local.prepare",
        dbos::StepOptions {
            max_attempts: 1,
            ..Default::default()
        },
        move || {
            let prepare = prepare.clone();
            let source = Arc::clone(&source);
            async move {
                let pg = if prepare.reuse_local.is_none() {
                    Some(source.write_only_postgres().await.map_err(|_| failed())?)
                } else {
                    None
                };
                operations::prepare(pg.as_ref(), &prepare.config, prepare.reuse_local.as_ref())
                    .await
                    .map_err(|_| failed())
            }
        },
    )
    .await?;
    let mut destinations = Vec::new();
    for index in 0..request.config.destinations.len() {
        let config = request.config.clone();
        let source = local.clone();
        let prepared: Option<LocalReceipt> = dbos::step_with(
            &format!("write_only.destination.{index}.prepare"),
            dbos::StepOptions {
                max_attempts: 1,
                ..Default::default()
            },
            move || {
                let config = config.clone();
                let source = source.clone();
                async move {
                    Ok::<_, dbos::Error<ArchivesDbosError>>(
                        operations::prepare_destination(&source, &config, index)
                            .await
                            .ok(),
                    )
                }
            },
        )
        .await?;
        let Some(prepared) = prepared else {
            destinations.push(DestinationReport {
                destination: index,
                prepared: None,
                outcome: None,
                preparation_failed: true,
            });
            continue;
        };
        let upload_config = request.config.clone();
        let upload_receipt = prepared.clone();
        let source_inventory_sha256 = local.inventory_sha256.clone();
        let outcome = dbos::step_with(
            &format!("write_only.destination.{index}.upload"),
            dbos::StepOptions {
                max_attempts: 1,
                ..Default::default()
            },
            move || {
                let config = upload_config.clone();
                let receipt = upload_receipt.clone();
                let source_inventory_sha256 = source_inventory_sha256.clone();
                async move {
                    Ok::<_, dbos::Error<ArchivesDbosError>>(
                        operations::upload(&receipt, &config, index, &source_inventory_sha256)
                            .await
                            .unwrap_or_else(|_| DestinationOutcome::Refused {
                                inventory_uri: format!(
                                    "{}/{}",
                                    receipt.root_uri, receipt.inventory_key
                                ),
                            }),
                    )
                }
            },
        )
        .await?;
        destinations.push(DestinationReport {
            destination: index,
            prepared: Some(prepared),
            outcome: Some(outcome),
            preparation_failed: false,
        });
    }
    finish(&request, local, destinations, slack, channel, slack_enabled).await
}

async fn finish(
    request: &Request,
    local: LocalReceipt,
    destinations: Vec<DestinationReport>,
    slack: Arc<dyn workflows::SlackBot>,
    channel: String,
    slack_enabled: bool,
) -> dbos::Result<RunReport, ArchivesDbosError> {
    let acknowledged = destinations
        .iter()
        .filter(|entry| {
            matches!(
                &entry.outcome,
                Some(DestinationOutcome::Acknowledged { .. })
            )
        })
        .count();
    let failures = destinations.len() - acknowledged;
    let report = Report {
        config_sha256: request.config_sha256.clone(),
        local,
        all_destinations_acknowledged: failures == 0,
        destinations,
        catalog_promoted: false,
    };
    let checkpoint = report.clone();
    let report = dbos::step("write_only.report", move || {
        let checkpoint = checkpoint.clone();
        async move { Ok::<_, dbos::Error<ArchivesDbosError>>(checkpoint) }
    })
    .await?;
    let tables_written = report.local.tables as u64;
    let correlation = format!(
        "archives-{}-write-only-digest",
        dbos::workflow_id().unwrap_or_default()
    );
    dbos::step_with("write_only.notify", dbos::StepOptions {
        max_attempts: super::DBOS_NOTIFY_ATTEMPTS, ..Default::default()
    }, move || {
        let slack = Arc::clone(&slack); let channel = channel.clone(); let correlation = correlation.clone();
        async move {
            let message = format!("Iceberg structured backup: local copy sealed; {acknowledged} destination upload(s) acknowledged; {failures} refused/unconfirmed. Remote verification, writer IAM acceptance, catalog promotion and full restore remain separate. Operational PITR retained.");
            super::post_digest_idempotent(slack.as_ref(), &channel, slack_enabled, &message, &correlation)
                .await.map_err(|_| dbos::Error::Application(ArchivesDbosError::Notify))?;
            telemetry::record_archives_run(tables_written, failures > 0);
            Ok(())
        }
    }).await?;
    if failures > 0 {
        return Err(dbos::Error::Application(ArchivesDbosError::Report(
            "write-only destinations refused/unconfirmed; immutable report is checkpointed".into(),
        )));
    }
    Ok(RunReport {
        run_date: report.local.cutoff_at.date_naive(),
        invocation_id: dbos::workflow_id().unwrap_or_default(),
        tables: report.local.tables,
        rows: report.local.rows,
        failures,
        write_only: Some(report),
    })
}
