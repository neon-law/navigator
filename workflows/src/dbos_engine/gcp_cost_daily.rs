//! One read-only billing export report per completed UTC export partition.
use billing::gcp_cost_daily::{DailyCost, ExportTable};
use chrono::{NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};

const WORKFLOW: &str = "GcpDailyCost";

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
#[error("reading the billing export partition failed")]
struct ReadError;

#[derive(Serialize, Deserialize)]
struct Report {
    export_date: NaiveDate,
    totals: Vec<DailyCost>,
}

type Workflow = dbos::WorkflowRef<NaiveDate, Report, ReadError>;

fn register_reader<F, Fut>(dbos: &dbos::DBOS, read: F) -> dbos::Result<Workflow>
where
    F: Fn(NaiveDate) -> Fut + Clone + Send + Sync + 'static,
    Fut: std::future::Future<Output = anyhow::Result<Vec<DailyCost>>> + Send + 'static,
{
    dbos.register_workflow(WORKFLOW, move |date: NaiveDate| {
        let read = read.clone();
        async move {
            let totals = dbos::step_with(
                "read-export-partition",
                dbos::StepOptions {
                    max_attempts: 3,
                    ..Default::default()
                },
                move || {
                    let result = read(date);
                    async move {
                        result
                            .await
                            .map_err(|_| dbos::Error::Application(ReadError))
                    }
                },
            )
            .await?;
            Ok(Report {
                export_date: date,
                totals,
            })
        }
    })
}

pub(super) fn register(
    dbos: &dbos::DBOS,
    table: ExportTable,
) -> dbos::Result<super::ScheduleStarter> {
    let workflow = register_reader(dbos, move |date| {
        let table = table.clone();
        async move { table.aggregate(date).await }
    })?;
    Ok(Box::new(move |stop| {
        tokio::spawn(super::daily::run(
            WORKFLOW,
            NaiveTime::from_hms_opt(13, 0, 0).unwrap_or(NaiveTime::MIN),
            super::daily::LateStart::CatchUp,
            Utc::now,
            stop,
            move |slot| {
                let workflow = workflow.clone();
                async move {
                    let date = slot.date_naive() - chrono::Days::new(1);
                    let id = format!("{WORKFLOW}-{date}");
                    workflow
                        .start_with(
                            date,
                            dbos::StartOptions {
                                workflow_id: Some(&id),
                                ..Default::default()
                            },
                        )
                        .await
                        .map(|_| ())
                }
            },
        ))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::ConnectOptions;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn retry_and_duplicate_start_share_one_daily_cost_report() {
        store::test_support::with_postgres(async {
            let pg = store::postgres::PostgresDb::application().await.unwrap();
            let url = pg.pool().connect_options().to_url_lossy();
            let runtime = dbos::DBOS::new(super::super::config(url.as_str(), "daily-cost-test"));
            let reads = Arc::new(AtomicUsize::new(0));
            let date = NaiveDate::from_ymd_opt(2026, 11, 21).unwrap();
            let workflow = register_reader(&runtime, {
                let reads = Arc::clone(&reads);
                move |day| {
                    let reads = Arc::clone(&reads);
                    async move {
                        assert_eq!(day, date);
                        anyhow::ensure!(
                            reads.fetch_add(1, Ordering::SeqCst) > 0,
                            "retry this synthetic read"
                        );
                        Ok(vec![])
                    }
                }
            })
            .unwrap();
            runtime.launch().await.unwrap();
            let id = format!("{WORKFLOW}-{date}");
            for _ in 0..2 {
                let report = workflow
                    .run_with(
                        date,
                        dbos::RunOptions {
                            workflow_id: Some(&id),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                assert_eq!(report.export_date, date);
                assert!(report.totals.is_empty());
            }
            assert_eq!(reads.load(Ordering::SeqCst), 2);
            runtime.shutdown().await;
        })
        .await;
    }
}
