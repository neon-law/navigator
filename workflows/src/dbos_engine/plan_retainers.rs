//! Daily reconciliation of onboarding for open Legal Plan Projects.
use std::sync::Arc;

use chrono::{NaiveTime, Utc};
use dbos::DBOS;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const WORKFLOW: &str = "LegalPlanRetainers";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Outcome {
    ClosedOrUnlinked,
    Executed,
    ExistingEnvelope,
    NeedsClient,
    NeedsDocuSign,
    NeedsDraft,
    NeedsReview,
    QuestionnaireOpened,
    EnvelopePrepared,
}

#[async_trait::async_trait]
pub trait Source: Send + Sync {
    async fn projects(&self) -> anyhow::Result<Vec<Uuid>>;
    async fn reconcile(&self, project: Uuid) -> anyhow::Result<Outcome>;
}

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
#[error("Legal Plan retainer reconciliation failed")]
struct Failure;

type Report = Vec<(Uuid, Outcome)>;
type Workflow = dbos::WorkflowRef<(), Report, Failure>;

fn register_workflow(dbos: &DBOS, source: Arc<dyn Source>) -> dbos::Result<Workflow> {
    dbos.register_workflow(WORKFLOW, move |()| {
        let source = source.clone();
        async move {
            let inventory = source.clone();
            let projects = dbos::step("open-plan-projects", move || {
                let inventory = inventory.clone();
                async move {
                    inventory
                        .projects()
                        .await
                        .map_err(|_| dbos::Error::Application(Failure))
                }
            })
            .await?;
            let mut report = Vec::new();
            let mut failed = false;
            for project in projects {
                let source = source.clone();
                let outcome = dbos::step(&format!("reconcile-{project}"), move || {
                    let source = source.clone();
                    async move {
                        source
                            .reconcile(project)
                            .await
                            .map_err(|_| dbos::Error::Application(Failure))
                    }
                })
                .await;
                if let Ok(outcome) = outcome {
                    report.push((project, outcome));
                } else {
                    tracing::warn!(%project, "Legal Plan retainer requires reconciliation");
                    failed = true;
                }
            }
            if failed {
                return Err(dbos::Error::Application(Failure));
            }
            Ok(report)
        }
    })
}

pub(super) fn register(
    dbos: &DBOS,
    source: Arc<dyn Source>,
) -> dbos::Result<super::ScheduleStarter> {
    let workflow = register_workflow(dbos, source)?;
    Ok(Box::new(move |stop| {
        tokio::spawn(super::daily::run(
            WORKFLOW,
            NaiveTime::MIN,
            super::daily::LateStart::CatchUp,
            Utc::now,
            stop,
            move |slot| {
                let workflow = workflow.clone();
                async move {
                    let id = format!("{WORKFLOW}-{}", slot.date_naive());
                    workflow
                        .start_with(
                            (),
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
    use super::{register_workflow, Outcome, Source};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use uuid::Uuid;

    struct SyntheticSource {
        project: Uuid,
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl Source for SyntheticSource {
        async fn projects(&self) -> anyhow::Result<Vec<Uuid>> {
            Ok(vec![self.project])
        }

        async fn reconcile(&self, project: Uuid) -> anyhow::Result<Outcome> {
            assert_eq!(project, self.project);
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Outcome::EnvelopePrepared)
        }
    }

    #[tokio::test]
    async fn retrying_a_daily_slot_does_not_prepare_another_envelope() {
        use sqlx::ConnectOptions;

        store::test_support::with_postgres(async {
            let pg = store::postgres::PostgresDb::application().await.unwrap();
            let url = pg.pool().connect_options().to_url_lossy();
            let dbos = dbos::DBOS::new(super::super::config(url.as_str(), "retainers-test"));
            let project = Uuid::new_v4();
            let source = Arc::new(SyntheticSource {
                project,
                calls: AtomicUsize::new(0),
            });
            let workflow = register_workflow(&dbos, source.clone()).unwrap();
            dbos.launch().await.unwrap();
            let id = format!("test-retainers-{project}");
            for _ in 0..2 {
                let report = workflow
                    .run_with(
                        (),
                        dbos::RunOptions {
                            workflow_id: Some(&id),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                assert_eq!(report, [(project, Outcome::EnvelopePrepared)]);
            }
            dbos.shutdown().await;
            assert_eq!(source.calls.load(Ordering::SeqCst), 1);
        })
        .await;
    }
}
