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
    let store = match &request.config.object_store {
        Some(config) => Some(
            phases
                .write_only_store(config)
                .await
                .map_err(|_| failed())?,
        ),
        None => None,
    };
    let capture_id = request
        .capture_id
        .clone()
        .or_else(dbos::workflow_id)
        .ok_or_else(failed)?;
    let prepare = request.clone();
    let source = Arc::clone(&phases);
    let prepare_store = store.clone();
    let local = dbos::step_with(
        "write_only.local.prepare",
        dbos::StepOptions {
            max_attempts: 1,
            ..Default::default()
        },
        move || {
            let prepare = prepare.clone();
            let source = Arc::clone(&source);
            let store = prepare_store.clone();
            let capture_id = capture_id.clone();
            async move {
                if let Some(store) = store {
                    return operations::object_store::prepare(
                        store.as_ref(),
                        &prepare.config,
                        &capture_id,
                        prepare.reuse_local.as_ref(),
                        || async {
                            let pg = source
                                .write_only_postgres()
                                .await
                                .map_err(|_| anyhow::anyhow!("source unavailable"))?;
                            operations::object_store::capture_native(&pg, &prepare.config).await
                        },
                    )
                    .await
                    .map_err(|_| failed());
                }
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
    let destinations = destinations(&request, &local, store, phases).await?;
    finish(&request, local, destinations, slack, channel, slack_enabled).await
}

async fn destinations(
    request: &Request,
    local: &LocalReceipt,
    store: Option<Arc<dyn operations::object_store::Store>>,
    phases: Arc<dyn super::ArchivesDbosPhases>,
) -> dbos::Result<Vec<DestinationReport>, ArchivesDbosError> {
    let mut destinations = Vec::new();
    for index in 0..request.config.destinations.len() {
        let config = request.config.clone();
        let source = local.clone();
        let destination_store = store.clone();
        let delivery_attempt = request.delivery_attempt;
        let prepared: Option<LocalReceipt> = dbos::step_with(
            &format!("write_only.destination.{index}.prepare"),
            dbos::StepOptions {
                max_attempts: 1,
                ..Default::default()
            },
            move || {
                let config = config.clone();
                let source = source.clone();
                let store = destination_store.clone();
                async move {
                    let result = match store {
                        Some(store) => {
                            operations::object_store::prepare_destination(
                                store.as_ref(),
                                &config,
                                &source,
                                index,
                                delivery_attempt,
                            )
                            .await
                        }
                        None => operations::prepare_destination(&source, &config, index).await,
                    };
                    Ok::<_, dbos::Error<ArchivesDbosError>>(result.ok())
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
        let outcome = upload(
            request,
            local,
            store.clone(),
            phases.clone(),
            &prepared,
            index,
        )
        .await?;
        destinations.push(DestinationReport {
            destination: index,
            prepared: Some(prepared),
            outcome: Some(outcome),
            preparation_failed: false,
        });
    }
    Ok(destinations)
}

async fn upload(
    request: &Request,
    local: &LocalReceipt,
    store: Option<Arc<dyn operations::object_store::Store>>,
    phases: Arc<dyn super::ArchivesDbosPhases>,
    prepared: &LocalReceipt,
    index: usize,
) -> dbos::Result<DestinationOutcome, ArchivesDbosError> {
    let upload_config = request.config.clone();
    let upload_receipt = prepared.clone();
    let source_inventory_sha256 = local.inventory_sha256.clone();
    let upload_store = store.clone();
    let target_factory = phases.clone();
    dbos::step_with(
        &format!("write_only.destination.{index}.upload"),
        dbos::StepOptions {
            max_attempts: 1,
            ..Default::default()
        },
        move || {
            let config = upload_config.clone();
            let receipt = upload_receipt.clone();
            let source_inventory_sha256 = source_inventory_sha256.clone();
            let store = upload_store.clone();
            let target_factory = target_factory.clone();
            async move {
                let result = match store {
                    Some(store) => match target_factory
                        .write_only_destination(config.destinations[index].bucket.clone())
                        .await
                    {
                        Ok(target) => {
                            operations::object_store::upload(
                                store.as_ref(),
                                &config,
                                &receipt,
                                index,
                                &source_inventory_sha256,
                                target.as_ref(),
                            )
                            .await
                        }
                        Err(_) => Err(anyhow::anyhow!("destination unavailable")),
                    },
                    None => {
                        operations::upload(&receipt, &config, index, &source_inventory_sha256).await
                    }
                };
                Ok::<_, dbos::Error<ArchivesDbosError>>(result.unwrap_or_else(|_| {
                    DestinationOutcome::Refused {
                        inventory_uri: format!("{}/{}", receipt.root_uri, receipt.inventory_key),
                    }
                }))
            }
        },
    )
    .await
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
            let message = format!("Iceberg structured backup: source generation sealed; {acknowledged} destination upload(s) acknowledged; {failures} refused/unconfirmed. Remote verification, writer IAM acceptance, catalog promotion and full restore remain separate. Operational PITR retained.");
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
