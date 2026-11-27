//! Durable execution for Project Slack notices.
//!
//! DBOS deduplicates each event and serializes delivery per Project:
//!
//! - **Per-event workflow identity.** A main-push notice runs as
//!   `ProjectSlack:{project id}:{short sha}:{run attempt}` and a client
//!   view as `ProjectSlack:{project id}:client-view`. A repeated start of
//!   the same id joins the first run, so a relayed event posts once.
//! - **A PostgreSQL session advisory lock** ([`ProjectSlackLane`]) held for
//!   the run. Distinct events for one Project wait for the lock instead of
//!   interleaving channel creation and posts.
//!
//! Project channel metadata lives in PostgreSQL. Retained posted-event history
//! and client-view posting evidence govern delivery admission.
//! Native ownership and delivery admission are checked before provider access;
//! the provider's existing durable checkpoint precedes the PG-only receipt step.

use std::sync::Arc;

use crate::firm_slack::ProjectSlackBots;
use dbos::WorkflowRef;
use serde::{Deserialize, Serialize};
use sqlx::Connection;
use store::application_store::ApplicationStore;

use crate::project_slack::{
    main_push_destination_on, render_main_push, LaneError, MainPushRequest,
    CLIENT_PROJECT_VIEW_MESSAGE,
};

/// The registered workflow's name. Distinct from every other DBOS workflow
/// and from the Restate service (`project-slack`), so the two engines can
/// run side by side during the migration.
pub const WORKFLOW: &str = "ProjectSlack";
/// The lane key of the client-view workflow for a Project.
pub const CLIENT_VIEW_LANE: &str = "client-view";

/// Slack takes no idempotency key on `chat.postMessage`, and the store and
/// bot calls are the retriable class; three attempts bound a lane's retry
/// window the way the Restate object's journaled steps were retried.
const LANE_ATTEMPTS: u32 = 3;

/// The Project Slack workflow handle: a [`LaneRequest`] in, no result beyond
/// success or failure.
pub type ProjectSlackWorkflow = WorkflowRef<LaneRequest, (), LaneError>;

/// The two lanes the Restate object served, as one workflow request. The
/// workflow id carries the Project and the event key; this carries the work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaneRequest {
    /// A client opened the Project in the portal: ensure the channel, post
    /// the view notice.
    ClientView,
    /// A push to the Project repository's `main`, relayed by the GitHub gate.
    MainPush(MainPushRequest),
}

/// The DBOS workflow id of one main-push event: one notice per
/// `(sha, run attempt)` per Project, exactly the Restate idempotency key.
#[must_use]
pub fn main_push_workflow_id(project_id: uuid::Uuid, event: &MainPushRequest) -> String {
    workflows::trigger::project_slack_main_push_workflow_id(
        project_id,
        &event.short_sha,
        event.run_attempt,
    )
}

/// The DBOS workflow id of a Project's client-view lane. One stable id per
/// Project: repeated views join the first run, so the notice posts once.
#[must_use]
pub fn client_view_workflow_id(project_id: uuid::Uuid) -> String {
    workflows::trigger::project_slack_client_view_workflow_id(project_id)
}

/// The lane key a registered workflow id names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneKey {
    /// The client-view lane for a Project.
    ClientView,
    /// A main-push event for a Project, keyed by `(sha, run attempt)`.
    MainPush {
        /// The commit's short SHA.
        short_sha: String,
        /// The GitHub run attempt number.
        run_attempt: u32,
    },
}

/// Parse the Project id and lane key a workflow id names. `None` when the id
/// does not name a Project Slack lane.
#[must_use]
pub fn parse_workflow_id(workflow_id: &str) -> Option<(uuid::Uuid, LaneKey)> {
    let rest = workflow_id.strip_prefix(&format!("{WORKFLOW}:"))?;
    let (project, lane) = rest.split_once(':')?;
    let project_id = project.parse::<uuid::Uuid>().ok()?;
    if lane == CLIENT_VIEW_LANE {
        return Some((project_id, LaneKey::ClientView));
    }
    // The event key is `{sha}:{attempt}`; the sha never carries a colon.
    let (short_sha, run_attempt) = lane.rsplit_once(':')?;
    Some((
        project_id,
        LaneKey::MainPush {
            short_sha: short_sha.to_owned(),
            run_attempt: run_attempt.parse().ok()?,
        },
    ))
}

/// The advisory-lock key for one Project's lane. Deterministic on every
/// executor; the `dbos` system schema never takes application advisory
/// locks, so this keyspace is the lane's alone.
///
/// Advisory-lock keys are 63 bits, so folding a UUID into one is
/// deliberately lossy: the key namespace here holds only lane keys, and a
/// collision would need two live Projects whose UUIDs agree in the low 64
/// bits.
#[must_use]
pub fn lane_lock_key(project_id: uuid::Uuid) -> i64 {
    store::project_slack_deliveries::lane_key(project_id)
}

/// A Project's serialization lane: a PostgreSQL session advisory lock held
/// for the duration of one workflow execution.
///
/// Restate serialized a virtual object's events on one journal; DBOS runs
/// distinct workflow ids concurrently, so a Project's lane takes
/// `pg_advisory_lock` on a key derived from the Project id. The lock is
/// session-scoped: it lives on this guard's own connection, released when
/// the connection closes — so the guard closes its connection on drop, and
/// recovery on another executor re-acquires from a fresh session.
///
/// The lock is deliberately not a journaled step: a checkpoint could not
/// carry another executor's session, and replaying the body re-acquires.
pub struct ProjectSlackLane {
    connection: Option<sqlx::PgConnection>,
    lock_key: i64,
}

impl ProjectSlackLane {
    /// Block until this execution holds the Project's lane. The connection
    /// is separate from the engine pool so the lock outlives no query but
    /// the workflow itself.
    pub async fn acquire(
        postgres: &store::postgres::PostgresDb,
        project_id: uuid::Uuid,
    ) -> Result<Self, sqlx::Error> {
        let options = postgres.pool().connect_options().as_ref().clone();
        let mut connection = sqlx::PgConnection::connect_with(&options).await?;
        let lock_key = lane_lock_key(project_id);
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(lock_key)
            .execute(&mut connection)
            .await?;
        Ok(Self {
            connection: Some(connection),
            lock_key,
        })
    }

    /// Release the lane and close the session. The ordered path; the drop
    /// is the safety net.
    pub async fn release(mut self) {
        if let Some(mut connection) = self.connection.take() {
            let _unlocked = sqlx::query("SELECT pg_advisory_unlock($1)")
                .bind(self.lock_key)
                .execute(&mut connection)
                .await;
            let _closed = connection.close().await;
        }
    }
}

impl Drop for ProjectSlackLane {
    fn drop(&mut self) {
        // A session-level advisory lock dies with its session, so closing
        // the socket releases the lane even when `release` never ran; the
        // spawned unlock only makes it immediate.
        if let Some(mut connection) = self.connection.take() {
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    let lock_key = self.lock_key;
                    handle.spawn(async move {
                        let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
                            .bind(lock_key)
                            .execute(&mut connection)
                            .await;
                        let _closed = connection.close().await;
                    });
                }
                Err(_) => drop(connection),
            }
        }
    }
}

/// The stores and bot resolver one Project Slack lane runs against. Cloned
/// per execution; the `SurrealDB` handle and the resolver are shared with
/// the Restate object, the PostgreSQL handle is the same pool the engine
/// runs on. Each step resolves the owning Firm's bot (ENG-392).
#[derive(Clone)]
pub struct Context {
    application_store: ApplicationStore,
    bots: Arc<dyn ProjectSlackBots>,
    postgres: store::postgres::PostgresDb,
    simulated: bool,
}

impl Context {
    /// Assemble the lane's collaborators.
    #[must_use]
    pub fn new(
        application_store: ApplicationStore,
        bots: Arc<dyn ProjectSlackBots>,
        postgres: store::postgres::PostgresDb,
        simulated: bool,
    ) -> Self {
        Self {
            application_store,
            bots,
            postgres,
            simulated,
        }
    }
}

/// Register the workflow. Call once per executor, before any lane starts.
pub fn register(dbos: &dbos::DBOS, context: Context) -> dbos::Result<ProjectSlackWorkflow> {
    dbos.register_workflow(WORKFLOW, move |request: LaneRequest| {
        let context = context.clone();
        async move {
            // The workflow id is the identity the route validated: one
            // Project's lane plus one event key.
            let id = dbos::workflow_id().unwrap_or_default();
            let Some((project_id, _lane)) = parse_workflow_id(&id) else {
                return Err(dbos::Error::Application(LaneError::InvalidIdentity(id)));
            };
            // Reject an unpostable event before taking the lane, so a
            // malformed event never holds it.
            if let LaneRequest::MainPush(event) = &request {
                if let Err(error) = event.validate() {
                    return Err(dbos::Error::Application(error));
                }
            }
            let lane = ProjectSlackLane::acquire(&context.postgres, project_id)
                .await
                .map_err(|error| dbos::Error::Application(LaneError::Store(error.to_string())))?;
            let outcome = run_lane(&context, project_id, request).await;
            lane.release().await;
            outcome
        }
    })
}

/// The journaled work of one lane. Each step's result is checkpointed, so a
/// recovered execution skips the calls that already succeeded.
async fn run_lane(
    context: &Context,
    project_id: uuid::Uuid,
    request: LaneRequest,
) -> dbos::Result<(), LaneError> {
    let (kind, key) = delivery_identity(&request);
    if store::project_slack_deliveries::admission(&context.postgres, project_id, kind, &key)
        .await
        .map_err(|error| dbos::Error::Application(delivery_error(&error)))?
        == store::project_slack_deliveries::Admission::Posted
    {
        return Ok(());
    }
    match request {
        LaneRequest::ClientView => post_client_view(context, project_id).await?,
        LaneRequest::MainPush(event) => {
            let application_store = context.application_store.clone();
            let bots = Arc::clone(&context.bots);
            let simulated = context.simulated;
            let postgres = context.postgres.clone();
            let destination =
                dbos::step_with("resolve-destination", lane_step_options(), move || {
                    let application_store = application_store.clone();
                    let bots = Arc::clone(&bots);
                    let postgres = postgres.clone();
                    async move {
                        let slack = bots
                            .for_project_on(&postgres, &application_store, project_id)
                            .await
                            .map_err(dbos::Error::Application)?;
                        main_push_destination_on(&postgres, &slack, project_id, simulated)
                            .await
                            .map_err(dbos::Error::Application)
                    }
                })
                .await?;
            let message = render_main_push(&event, destination.repository_url.as_deref());
            let application_store = context.application_store.clone();
            let bots = Arc::clone(&context.bots);
            let channel_id = destination.channel_id;
            let postgres = context.postgres.clone();
            dbos::step_with("post-main-push", lane_step_options(), move || {
                let application_store = application_store.clone();
                let bots = Arc::clone(&bots);
                let channel_id = channel_id.clone();
                let message = message.clone();
                let postgres = postgres.clone();
                async move {
                    let slack = bots
                        .for_project_on(&postgres, &application_store, project_id)
                        .await
                        .map_err(dbos::Error::Application)?;
                    post_correlated(slack.as_ref(), &channel_id, &message).await
                }
            })
            .await?;
        }
    }
    let postgres = context.postgres.clone();
    dbos::step_with("record-posted", lane_step_options(), move || {
        let postgres = postgres.clone();
        let key = key.clone();
        async move {
            store::project_slack_deliveries::record_posted(&postgres, project_id, kind, &key)
                .await
                .map_err(|error| dbos::Error::Application(delivery_error(&error)))
        }
    })
    .await
}

async fn post_client_view(
    context: &Context,
    project_id: uuid::Uuid,
) -> dbos::Result<(), LaneError> {
    let application_store = context.application_store.clone();
    let bots = Arc::clone(&context.bots);
    let simulated = context.simulated;
    let postgres = context.postgres.clone();
    let channel_id: String = dbos::step_with("ensure-channel", lane_step_options(), move || {
        let application_store = application_store.clone();
        let bots = Arc::clone(&bots);
        let postgres = postgres.clone();
        async move {
            let slack = bots
                .for_project_on(&postgres, &application_store, project_id)
                .await
                .map_err(dbos::Error::Application)?;
            main_push_destination_on(&postgres, &slack, project_id, simulated)
                .await
                .map(|destination| destination.channel_id)
                .map_err(dbos::Error::Application)
        }
    })
    .await?;
    let application_store = context.application_store.clone();
    let bots = Arc::clone(&context.bots);
    let postgres = context.postgres.clone();
    dbos::step_with("post-client-view", lane_step_options(), move || {
        let application_store = application_store.clone();
        let bots = Arc::clone(&bots);
        let channel_id = channel_id.clone();
        let postgres = postgres.clone();
        async move {
            let slack = bots
                .for_project_on(&postgres, &application_store, project_id)
                .await
                .map_err(dbos::Error::Application)?;
            post_correlated(slack.as_ref(), &channel_id, CLIENT_PROJECT_VIEW_MESSAGE).await
        }
    })
    .await?;
    Ok(())
}

/// Post once per step: the step's effect key is the Slack correlation, so a
/// re-run after an accepted post that never checkpointed does not repeat it.
async fn post_correlated(
    slack: &dyn workflows::SlackBot,
    channel_id: &str,
    text: &str,
) -> dbos::Result<(), LaneError> {
    let correlation = workflows::dbos_engine::effect_key().ok_or_else(|| {
        dbos::Error::Application(LaneError::Store("posted outside a DBOS step".to_owned()))
    })?;
    crate::firm_slack::post_once(slack, channel_id, &correlation, text)
        .await
        .map_err(|error| dbos::Error::Application(LaneError::Slack(error.to_string())))
}

/// The canonical delivery identity shared with pre-start ingress admission.
pub(crate) fn delivery_identity(request: &LaneRequest) -> (&'static str, String) {
    match request {
        LaneRequest::ClientView => ("client_view", CLIENT_VIEW_LANE.to_owned()),
        LaneRequest::MainPush(event) => (
            "main_push",
            format!("{}:{}", event.short_sha, event.run_attempt),
        ),
    }
}

fn delivery_error(error: &store::project_slack_deliveries::DeliveryError) -> LaneError {
    match error {
        store::project_slack_deliveries::DeliveryError::Storage => {
            LaneError::Store("Project Slack delivery storage unavailable".to_owned())
        }
        _ => LaneError::Refused(error.to_string()),
    }
}

/// Bounded retries with the lane's terminal/retriable classification: a
/// terminal failure stops the step at once, a retriable one burns an
/// attempt and then the run fails, exactly as the migration plan records.
fn lane_step_options() -> dbos::StepOptions<LaneError> {
    dbos::StepOptions {
        max_attempts: LANE_ATTEMPTS,
        should_retry: Some(Arc::new(|error: &dbos::Error<LaneError>| match error {
            dbos::Error::Application(lane) => !lane.terminal(),
            _ => true,
        })),
        ..dbos::StepOptions::default()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        client_view_workflow_id, lane_lock_key, main_push_workflow_id, parse_workflow_id, LaneKey,
        LaneRequest, CLIENT_VIEW_LANE, WORKFLOW,
    };
    use crate::project_slack::{CiOutcome, LaneError, MainPushRequest};
    use uuid::Uuid;

    fn event(short_sha: &str, run_attempt: u32) -> MainPushRequest {
        MainPushRequest {
            short_sha: short_sha.to_string(),
            run_url: "https://github.com/an-organization/sample-push/actions/runs/1".to_string(),
            author_login: "octocat".to_string(),
            gate: CiOutcome::Pass,
            publish: Some(CiOutcome::Skipped),
            run_attempt,
            subject: None,
        }
    }

    #[test]
    fn workflow_ids_name_one_lane_of_one_project() {
        let project = Uuid::now_v7();
        assert_eq!(
            client_view_workflow_id(project),
            format!("{WORKFLOW}:{}:{CLIENT_VIEW_LANE}", project.simple()),
        );
        assert_eq!(
            main_push_workflow_id(project, &event("abc1234", 2)),
            format!("{WORKFLOW}:{}:abc1234:2", project.simple()),
        );
    }

    #[test]
    fn workflow_ids_parse_back_to_their_project_and_lane() {
        let project = Uuid::now_v7();
        assert_eq!(
            parse_workflow_id(&client_view_workflow_id(project)),
            Some((project, LaneKey::ClientView)),
        );
        assert_eq!(
            parse_workflow_id(&main_push_workflow_id(project, &event("abc1234", 2))),
            Some((
                project,
                LaneKey::MainPush {
                    short_sha: "abc1234".to_string(),
                    run_attempt: 2
                }
            )),
        );
        // Not one of ours, and not one at all.
        assert_eq!(parse_workflow_id("FinanceAggregate:abc"), None);
        assert_eq!(parse_workflow_id(&format!("{WORKFLOW}:nonsense:x")), None);
    }

    #[test]
    fn workflow_ids_are_namespaced_away_from_other_engines_work() {
        let project = Uuid::now_v7();
        let ids = [
            client_view_workflow_id(project),
            main_push_workflow_id(project, &event("abc1234", 1)),
        ];
        for id in ids {
            assert!(id.starts_with(&format!("{WORKFLOW}:")), "{id}");
            assert!(!id.contains("FinanceAggregate"), "{id}");
        }
        // Two lanes of one Project are distinct ids; two Projects' lanes
        // are distinct too.
        assert_ne!(
            client_view_workflow_id(project),
            main_push_workflow_id(project, &event("abc1234", 1)),
        );
        assert_ne!(
            client_view_workflow_id(project),
            client_view_workflow_id(Uuid::now_v7()),
        );
    }

    #[test]
    fn the_lane_lock_key_is_deterministic_per_project() {
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        assert_eq!(lane_lock_key(first), lane_lock_key(first));
        assert_ne!(lane_lock_key(first), lane_lock_key(second));
    }

    #[test]
    fn a_lane_request_round_trips_through_the_workflow_journal() {
        for request in [
            LaneRequest::ClientView,
            LaneRequest::MainPush(event("abc1234", 1)),
        ] {
            let encoded = serde_json::to_string(&request).expect("serialize");
            let decoded: LaneRequest = serde_json::from_str(&encoded).expect("deserialize");
            assert_eq!(decoded, request);
        }
    }

    #[test]
    fn the_run_refuses_a_malformed_event_without_taking_the_lane() {
        // The workflow validates before acquiring: covered here at the
        // classification level, where the same validator is the source.
        assert!(event("abc1234", 1).validate().is_ok());
        assert!(matches!(
            event("0", 1).validate(),
            Err(LaneError::InvalidEvent(_))
        ));
        assert!(parse_workflow_id("not-a-lane").is_none());
    }
}
