//! DBOS adapter for the `project-slack` virtual object's two lanes.
//!
//! Restate keyed the service by the Project id and serialized every event
//! through one virtual object. DBOS has no virtual objects, so the port
//! replaces that single-writer guarantee with two mechanisms:
//!
//! - **Per-event workflow identity.** A main-push notice runs as
//!   `ProjectSlack:{project id}:{short sha}:{run attempt}` and a client
//!   view as `ProjectSlack:{project id}:client-view`. A repeated start of
//!   the same id joins the first run, so a relayed event posts once.
//! - **A PostgreSQL session advisory lock** ([`ProjectSlackLane`]) held for
//!   the run. Distinct events for one Project wait for the lock instead of
//!   interleaving channel creation and posts.
//!
//! Channel ids live on the Project row in `SurrealDB`, so no K/V state
//! migrates: the DBOS path reads and writes the same row the Restate object
//! does. The Restate object remains registered until cutover.

use std::sync::Arc;

use dbos::WorkflowRef;
use serde::{Deserialize, Serialize};
use sqlx::Connection;
use store::surreal::SurrealDb;
use workflows::SlackBot;

use crate::project_slack::{
    ensure_internal_channel, main_push_destination, render_main_push, LaneError, MainPushRequest,
    CLIENT_PROJECT_VIEW_MESSAGE,
};

/// The registered workflow's name. Distinct from every other DBOS workflow
/// and from the Restate service (`project-slack`), so the two engines can
/// run side by side during the migration.
pub const WORKFLOW: &str = "ProjectSlack";
/// The rollout flag. Default off; the Restate object keeps serving until
/// cutover flips callers.
pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_PROJECT_SLACK_ENABLED";
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
    format!("{WORKFLOW}:{}:{}", project_id.simple(), event.event_key())
}

/// The DBOS workflow id of a Project's client-view lane. One stable id per
/// Project: repeated views join the first run, so the notice posts once.
#[must_use]
pub fn client_view_workflow_id(project_id: uuid::Uuid) -> String {
    format!("{WORKFLOW}:{}:{CLIENT_VIEW_LANE}", project_id.simple())
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
#[allow(clippy::cast_possible_truncation)]
pub fn lane_lock_key(project_id: uuid::Uuid) -> i64 {
    project_id.as_u128() as i64
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

/// The stores and bot one Project Slack lane runs against. Cloned per
/// execution; the `SurrealDB` handle and the bot are shared with the Restate
/// object, the PostgreSQL handle is the same pool the engine runs on.
#[derive(Clone)]
pub struct Context {
    surreal: SurrealDb,
    slack: Arc<dyn SlackBot>,
    postgres: store::postgres::PostgresDb,
    simulated: bool,
}

impl Context {
    /// Assemble the lane's collaborators.
    #[must_use]
    pub fn new(
        surreal: SurrealDb,
        slack: Arc<dyn SlackBot>,
        postgres: store::postgres::PostgresDb,
        simulated: bool,
    ) -> Self {
        Self {
            surreal,
            slack,
            postgres,
            simulated,
        }
    }
}

/// Whether the DBOS Project Slack lane is switched on for this deployment.
pub fn enabled(get: impl Fn(&str) -> Option<String>) -> Result<bool, &'static str> {
    match get(ENABLED_ENV).as_deref().map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err("NAVIGATOR_DBOS_PROJECT_SLACK_ENABLED must be true or false"),
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
    match request {
        LaneRequest::ClientView => {
            let surreal = context.surreal.clone();
            let slack = Arc::clone(&context.slack);
            let simulated = context.simulated;
            let channel_id: String =
                dbos::step_with("ensure-channel", lane_step_options(), move || {
                    let surreal = surreal.clone();
                    let slack = Arc::clone(&slack);
                    async move {
                        ensure_internal_channel(&surreal, &slack, project_id, simulated)
                            .await
                            .map_err(dbos::Error::Application)
                    }
                })
                .await?;
            let slack = Arc::clone(&context.slack);
            dbos::step_with("post-client-view", lane_step_options(), move || {
                let slack = Arc::clone(&slack);
                let channel_id = channel_id.clone();
                async move {
                    slack
                        .post_message(&channel_id, CLIENT_PROJECT_VIEW_MESSAGE)
                        .await
                        .map_err(|error| {
                            dbos::Error::Application(LaneError::Slack(error.to_string()))
                        })
                }
            })
            .await?;
            Ok(())
        }
        LaneRequest::MainPush(event) => {
            let surreal = context.surreal.clone();
            let slack = Arc::clone(&context.slack);
            let simulated = context.simulated;
            let destination =
                dbos::step_with("resolve-destination", lane_step_options(), move || {
                    let surreal = surreal.clone();
                    let slack = Arc::clone(&slack);
                    async move {
                        main_push_destination(&surreal, &slack, project_id, simulated)
                            .await
                            .map_err(dbos::Error::Application)
                    }
                })
                .await?;
            let message = render_main_push(&event, destination.repository_url.as_deref());
            let slack = Arc::clone(&context.slack);
            let channel_id = destination.channel_id;
            dbos::step_with("post-main-push", lane_step_options(), move || {
                let slack = Arc::clone(&slack);
                let channel_id = channel_id.clone();
                let message = message.clone();
                async move {
                    slack
                        .post_message(&channel_id, &message)
                        .await
                        .map_err(|error| {
                            dbos::Error::Application(LaneError::Slack(error.to_string()))
                        })
                }
            })
            .await?;
            Ok(())
        }
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
        client_view_workflow_id, enabled, lane_lock_key, main_push_workflow_id, parse_workflow_id,
        LaneKey, LaneRequest, CLIENT_VIEW_LANE, ENABLED_ENV, WORKFLOW,
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
        }
    }

    #[test]
    fn the_rollout_flag_defaults_off_and_refuses_junk() {
        assert!(!enabled(|_| None).expect("unset"));
        assert!(!enabled(|name| (name == ENABLED_ENV).then(String::new)).expect("empty"));
        assert!(
            !enabled(|name| (name == ENABLED_ENV).then(|| "false".to_string())).expect("false"),
        );
        assert!(enabled(|name| (name == ENABLED_ENV).then(|| "true".to_string())).expect("true"));
        assert!(enabled(|name| (name == ENABLED_ENV).then(|| "yes".to_string())).is_err());
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
