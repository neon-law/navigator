//! A DBOS workflow parks each draft revision until a lawyer records a decision.
//! Notifications only wake the workflow. PostgreSQL authorizes publication.
use dbos::{CancellationToken, StartOptions, StepOptions, WorkflowRef, DBOS};
use std::time::Duration;
use store::{deadline_proposals as proposals, postgres::PostgresDb};

#[derive(Debug, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[error("court-date publication is unavailable")]
pub struct PublicationError;
pub type Workflow = WorkflowRef<proposals::Job, (), PublicationError>;
const TOPIC: &str = "lawyer-reviewed";

pub fn register(dbos: &DBOS, pg: PostgresDb) -> dbos::Result<Workflow> {
    dbos.register_workflow("ReviewCourtDate", move |job| {
        let pg = pg.clone();
        async move {
            loop {
                let phase = dbos::step_with("read-revision-decision", options(), || async {
                    proposals::phase(&pg, job)
                        .await
                        .map_err(|_| dbos::Error::Application(PublicationError))
                })
                .await?;
                match phase {
                    proposals::Status::Pending => {
                        // Timeout only rechecks the SQL decision. It never approves a date.
                        dbos::recv::<(), PublicationError>(Some(TOPIC), Duration::from_hours(24))
                            .await?;
                    }
                    proposals::Status::Approved => {
                        return dbos::step_with("publish-approved-revision", options(), || async {
                            proposals::publish(&pg, job)
                                .await
                                .map_err(|_| dbos::Error::Application(PublicationError))
                        })
                        .await
                    }
                    proposals::Status::Returned | proposals::Status::Published => return Ok(()),
                }
            }
        }
    })
}
fn options() -> StepOptions<PublicationError> {
    StepOptions {
        max_attempts: 3,
        ..Default::default()
    }
}
#[must_use]
pub fn workflow_id(job: proposals::Job) -> String {
    format!(
        "ReviewCourtDate:{}:{}:{}:{}",
        job.project_id, job.id, job.revision, job.attempt
    )
}
pub async fn schedule(dbos: DBOS, pg: PostgresDb, flow: Workflow, stop: CancellationToken) {
    let mut ticks = tokio::time::interval(Duration::from_secs(10));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {()=stop.cancelled()=>break,_=ticks.tick()=>{}}
        let Ok(jobs) = proposals::work(&pg).await else {
            tracing::warn!("court-date approval queue unavailable");
            continue;
        };
        for job in jobs {
            if dispatch(&dbos, &pg, &flow, job).await.is_err() {
                tracing::warn!(proposal_id=%job.id,"court-date workflow dispatch failed");
            }
        }
    }
}
async fn dispatch(
    dbos: &DBOS,
    pg: &PostgresDb,
    flow: &Workflow,
    job: proposals::Job,
) -> Result<(), PublicationError> {
    let id = workflow_id(job);
    let handle = flow
        .start_with(
            job,
            StartOptions {
                workflow_id: Some(&id),
                ..Default::default()
            },
        )
        .await
        .map_err(|_| PublicationError)?;
    let terminal = !handle
        .status()
        .await
        .map_err(|_| PublicationError)?
        .is_active();
    if !terminal
        && proposals::phase(pg, job)
            .await
            .map_err(|_| PublicationError)?
            != proposals::Status::Pending
    {
        let key = format!("{id}:decision");
        dbos.send_with(
            dbos::SendMessage {
                topic: Some(TOPIC),
                idempotency_key: Some(&key),
                ..dbos::SendMessage::new(&id, &())
            },
            dbos::SendOptions::default(),
        )
        .await
        .map_err(|_| PublicationError)?;
    }
    proposals::observe(pg, job, terminal)
        .await
        .map_err(|_| PublicationError)
}
