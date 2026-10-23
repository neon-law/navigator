//! Durable internal Slack notices for one Project.
//!
//! The service is a virtual object keyed by the Project id. That gives each
//! Project one serialized lane: the first client view creates its private
//! channel and later views reuse the channel ID persisted on the Project row.
//! Both the channel-creation and message-post calls are journaled Restate
//! steps, so a transient Slack outage is retried without holding an HTTP page
//! request open.

use std::sync::Arc;

use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use tracing::Instrument;
use workflows::{project_channel_name, SlackBot, SlackBotError, SlackChannel};

/// The outcome of one half of a Project repository's CD run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CiOutcome {
    Pass,
    Fail,
    Skipped,
}

impl CiOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Skipped => "skipped",
        }
    }
}

/// One push to a Project repository's `main`, as `POST
/// /app/api/projects/{id}/events/main-push` relays it from the shared
/// `project-gate.yml`. Mechanism only: a short SHA, the run, the pusher's
/// login, and the two outcomes. No commit subject, so none can reach the
/// channel. `publish` is absent when the gate reports before the publish
/// half has run.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct MainPushRequest {
    pub short_sha: String,
    pub run_url: String,
    pub author_login: String,
    pub gate: CiOutcome,
    #[serde(default)]
    pub publish: Option<CiOutcome>,
    pub run_attempt: u32,
}

impl MainPushRequest {
    /// The idempotency key: one notice per `(sha, run attempt)`.
    #[must_use]
    pub fn event_key(&self) -> String {
        format!("{}:{}", self.short_sha, self.run_attempt)
    }
}

/// What `main_push` did with one event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MainPushOutcome {
    Posted,
    /// The same `(sha, run attempt)` was already posted; nothing new.
    Duplicate,
}

/// The object-state key holding the `(sha, run attempt)` keys this Project
/// has already posted, newest last.
const MAIN_PUSH_SEEN_KEY: &str = "main_push_seen";
/// How many posted keys one Project remembers. A rerun of a run attempt
/// arrives within hours; a thousand pushes later the key is new again.
const MAIN_PUSH_SEEN_LIMIT: usize = 200;

/// Empty request for the client-view event. The server derives the event from
/// its authenticated session and authorization result; callers cannot choose
/// an arbitrary Slack message or recipient.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct ClientProjectViewRequest {}

/// The only message currently emitted by this service. It deliberately carries
/// no client name, email, matter name, or portal content; the private channel
/// itself is the Project-specific destination.
pub const CLIENT_PROJECT_VIEW_MESSAGE: &str = "A client viewed this Project in the portal.";

#[derive(Clone)]
pub struct ProjectSlackService {
    surreal: store::surreal::SurrealDb,
    slack: Arc<dyn SlackBot>,
    simulated: bool,
}

impl ProjectSlackService {
    /// `simulated` selects the channel prefix: `staging-` on a
    /// simulated-matters deployment, `project-` otherwise.
    #[must_use]
    pub fn new(
        surreal: store::surreal::SurrealDb,
        slack: Arc<dyn SlackBot>,
        simulated: bool,
    ) -> Self {
        Self {
            surreal,
            slack,
            simulated,
        }
    }
}

fn project_id(key: &str) -> Result<uuid::Uuid, HandlerError> {
    key.parse::<uuid::Uuid>().map_err(|error| {
        HandlerError::from(TerminalError::new(format!(
            "Project Slack object key `{key}` is not a valid project id: {error}"
        )))
    })
}

fn project_store_error(error: store::projects::ProjectStoreError) -> HandlerError {
    if matches!(error, store::projects::ProjectStoreError::Db(_)) {
        HandlerError::from(error)
    } else {
        HandlerError::from(TerminalError::new(error.to_string()))
    }
}

fn project_command_error(error: store::projects::ProjectCommandError) -> HandlerError {
    if matches!(error, store::projects::ProjectCommandError::Db(_)) {
        HandlerError::from(error)
    } else {
        HandlerError::from(TerminalError::new(error.to_string()))
    }
}

/// How [`ensure_channel`] resolved a Project's channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelOutcome {
    /// The channel recorded on the Project row was kept.
    Existing(String),
    /// A private channel of the expected name already existed and was recorded.
    Adopted(String),
    /// A new private channel was created and recorded.
    Created(String),
}

impl ChannelOutcome {
    #[must_use]
    pub fn channel_id(&self) -> &str {
        match self {
            Self::Existing(id) | Self::Adopted(id) | Self::Created(id) => id,
        }
    }
}

/// Find or create the Project's private Slack channel. Extracted from the
/// Restate handler so the store/Slack work is unit-testable without a broker.
async fn ensure_internal_channel(
    surreal: &store::surreal::SurrealDb,
    slack: &Arc<dyn SlackBot>,
    project_id: uuid::Uuid,
    simulated: bool,
) -> Result<String, HandlerError> {
    let project = store::projects::find_by_id(surreal, project_id)
        .await
        .map_err(project_store_error)?
        .ok_or_else(|| {
            HandlerError::from(TerminalError::new(
                "Project no longer exists for Slack notice",
            ))
        })?;
    ensure_channel(surreal, slack, &project, simulated)
        .await
        .map(|outcome| outcome.channel_id().to_string())
}

/// Resolve the Project's channel: keep the recorded one, else adopt the
/// private channel already carrying the expected name, else create it. A
/// `name_taken` race is resolved by adopting the winner.
pub(crate) async fn ensure_channel(
    surreal: &store::surreal::SurrealDb,
    slack: &Arc<dyn SlackBot>,
    project: &store::projects::Project,
    simulated: bool,
) -> Result<ChannelOutcome, HandlerError> {
    if let Some(channel_id) = &project.internal_slack_channel_id {
        return Ok(ChannelOutcome::Existing(channel_id.clone()));
    }
    let name = project_channel_name(simulated, &project.code);
    let (channel, adopted): (SlackChannel, bool) = if let Some(found) = slack
        .find_private_channel_by_name(&name)
        .await
        .map_err(HandlerError::from)?
    {
        (found, true)
    } else {
        match slack.create_private_channel(&name).await {
            Ok(created) => (created, false),
            Err(SlackBotError::Api(code)) if code == "name_taken" => {
                let found = slack
                    .find_private_channel_by_name(&name)
                    .await
                    .map_err(HandlerError::from)?
                    .ok_or_else(|| {
                        HandlerError::from(TerminalError::new(
                            "Slack channel name is taken by a channel this bot cannot see; invite the bot to that private channel or archive it",
                        ))
                    })?;
                (found, true)
            }
            Err(error) => return Err(HandlerError::from(error)),
        }
    };
    store::projects::set_internal_slack_channel_id(surreal, project.id, &channel.id)
        .await
        .map_err(project_command_error)?
        .ok_or_else(|| {
            HandlerError::from(TerminalError::new(
                "Project disappeared while saving Slack channel",
            ))
        })?;
    Ok(if adopted {
        ChannelOutcome::Adopted(channel.id)
    } else {
        ChannelOutcome::Created(channel.id)
    })
}

async fn post_client_view_notice(
    slack: &Arc<dyn SlackBot>,
    channel_id: &str,
) -> Result<(), HandlerError> {
    slack
        .post_message(channel_id, CLIENT_PROJECT_VIEW_MESSAGE)
        .await
        .map_err(HandlerError::from)
}

/// The root span of a `client_project_view` invocation. Identifiers only.
fn client_project_view_span(project_id: &str) -> tracing::Span {
    tracing::info_span!(
        "project_slack.client_project_view",
        project_id = %project_id,
        navigator.keep = true,
    )
}

/// The root span of a `main_push` invocation. Identifiers only.
fn main_push_span(project_id: &str, event_key: &str) -> tracing::Span {
    tracing::info_span!(
        "project_slack.main_push",
        project_id = %project_id,
        event_key = %event_key,
        navigator.keep = true,
    )
}

/// Where a main-push notice goes: the Project's private channel (created or
/// adopted if missing) and the recorded repository URL the commit link is
/// built from. Resolved from the Project row only, never from the request.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct MainPushDestination {
    pub channel_id: String,
    pub repository_url: Option<String>,
}

async fn main_push_destination(
    surreal: &store::surreal::SurrealDb,
    slack: &Arc<dyn SlackBot>,
    project_id: uuid::Uuid,
    simulated: bool,
) -> Result<MainPushDestination, HandlerError> {
    let project = store::projects::find_by_id(surreal, project_id)
        .await
        .map_err(project_store_error)?
        .ok_or_else(|| {
            HandlerError::from(TerminalError::new(
                "Project no longer exists for Slack notice",
            ))
        })?;
    let channel = ensure_channel(surreal, slack, &project, simulated).await?;
    Ok(MainPushDestination {
        channel_id: channel.channel_id().to_string(),
        repository_url: project.repository_url,
    })
}

/// The main-push notice. Short SHA (linked to the commit when a repository
/// URL is recorded), the pusher's login, the gate and publish outcomes, and
/// the run link. No client name, matter name, Project code, or commit
/// subject: the private channel is already the Project-specific destination.
#[must_use]
pub fn render_main_push(event: &MainPushRequest, repository_url: Option<&str>) -> String {
    let sha = repository_url.map_or_else(
        || format!("`{}`", event.short_sha),
        |url| {
            format!(
                "<{}/commit/{}|`{}`>",
                url.trim_end_matches('/'),
                event.short_sha,
                event.short_sha
            )
        },
    );
    let publish = event.publish.map_or_else(String::new, |publish| {
        format!(", publish {}", publish.as_str())
    });
    format!(
        "Push to main: {sha} by {} — gate {}{publish}. <{}|CI run #{}>",
        event.author_login,
        event.gate.as_str(),
        event.run_url,
        event.run_attempt
    )
}

/// Decide whether `event` is new for a Project that has already posted
/// `seen`, recording it when so. Pure, so the once-per-`(sha, attempt)`
/// rule is testable without a broker.
#[must_use]
pub fn admit_main_push(seen: &mut Vec<String>, event: &MainPushRequest) -> MainPushOutcome {
    let key = event.event_key();
    if seen.contains(&key) {
        return MainPushOutcome::Duplicate;
    }
    seen.push(key);
    if seen.len() > MAIN_PUSH_SEEN_LIMIT {
        let excess = seen.len() - MAIN_PUSH_SEEN_LIMIT;
        seen.drain(..excess);
    }
    MainPushOutcome::Posted
}

#[restate_sdk::object(name = "project-slack")]
impl ProjectSlackService {
    /// Ensure the Project has a private Slack channel, then post the client
    /// portal-view notice into that channel.
    #[restate_sdk::handler]
    async fn client_project_view(
        &self,
        ctx: ObjectContext<'_>,
        _request: Json<ClientProjectViewRequest>,
    ) -> Result<(), HandlerError> {
        let span = client_project_view_span(ctx.key());
        async move {
            let project_id = project_id(ctx.key())?;
            let surreal = self.surreal.clone();
            let slack = Arc::clone(&self.slack);
            let simulated = self.simulated;
            let channel_id: String = ctx
                .run(|| async move {
                    ensure_internal_channel(&surreal, &slack, project_id, simulated)
                        .await
                        .map(Json)
                })
                .name("ensure-channel")
                .await?
                .into_inner();

            let slack = Arc::clone(&self.slack);
            ctx.run(move || async move { post_client_view_notice(&slack, &channel_id).await })
                .name("post-client-view")
                .await?;
            Ok(())
        }
        .instrument(span)
        .await
    }

    /// A push to the Project repository's `main`: post one notice to the
    /// Project's private channel, once per `(sha, run attempt)`. The
    /// channel comes from the Project row, never from the request.
    #[restate_sdk::handler]
    async fn main_push(
        &self,
        ctx: ObjectContext<'_>,
        request: Json<MainPushRequest>,
    ) -> Result<Json<MainPushOutcome>, HandlerError> {
        let event = request.into_inner();
        let span = main_push_span(ctx.key(), &event.event_key());
        async move {
            let project_id = project_id(ctx.key())?;
            let mut seen: Vec<String> = ctx
                .get::<Json<Vec<String>>>(MAIN_PUSH_SEEN_KEY)
                .await?
                .map(Json::into_inner)
                .unwrap_or_default();
            if admit_main_push(&mut seen, &event) == MainPushOutcome::Duplicate {
                tracing::info!("main push already posted");
                return Ok(Json(MainPushOutcome::Duplicate));
            }

            let surreal = self.surreal.clone();
            let slack = Arc::clone(&self.slack);
            let simulated = self.simulated;
            let destination: MainPushDestination = ctx
                .run(|| async move {
                    main_push_destination(&surreal, &slack, project_id, simulated)
                        .await
                        .map(Json)
                })
                .name("resolve-destination")
                .await?
                .into_inner();

            let message = render_main_push(&event, destination.repository_url.as_deref());
            let slack = Arc::clone(&self.slack);
            let channel_id = destination.channel_id;
            ctx.run(move || async move {
                slack
                    .post_message(&channel_id, &message)
                    .await
                    .map_err(HandlerError::from)
            })
            .name("post-main-push")
            .await?;
            // Recorded after the post, so a run that never managed to post
            // is not remembered as posted.
            ctx.set(MAIN_PUSH_SEEN_KEY, Json(seen));
            Ok(Json(MainPushOutcome::Posted))
        }
        .instrument(span)
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::{
        admit_main_push, client_project_view_span, ensure_internal_channel, main_push_span,
        post_client_view_notice, project_command_error, project_id, project_store_error,
        render_main_push, CiOutcome, MainPushOutcome, MainPushRequest, ProjectSlackService,
        CLIENT_PROJECT_VIEW_MESSAGE,
    };
    use std::sync::Arc;
    use store::projects::{create, set_internal_slack_channel_id, NewProject, ProjectCommandError};
    use store::test_support::{mem_surreal, seed_entity};
    use uuid::Uuid;
    use workflows::CapturingSlackBot;

    #[test]
    fn the_client_project_view_root_span_is_kept_by_the_collector() {
        let fields = crate::span_capture::fields_of(|| client_project_view_span("p-1"));
        assert_eq!(
            fields.get("navigator.keep").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn object_key_must_be_a_project_uuid() {
        let id = Uuid::now_v7();
        assert_eq!(project_id(&id.to_string()).expect("uuid"), id);
        assert!(project_id("not-a-uuid").is_err());
    }

    #[test]
    fn store_and_command_errors_other_than_db_are_terminal() {
        let store_err = project_store_error(store::projects::ProjectStoreError::CodeTaken);
        assert!(
            format!("{store_err:?}").contains("already in use"),
            "{store_err:?}"
        );
        let command_err = project_command_error(ProjectCommandError::NotFound);
        assert!(
            format!("{command_err:?}").contains("no matter"),
            "{command_err:?}"
        );
        let invalid = project_command_error(ProjectCommandError::Invalid("bad channel"));
        assert!(
            format!("{invalid:?}").contains("bad channel"),
            "{invalid:?}"
        );
    }

    fn push(short_sha: &str, run_attempt: u32) -> MainPushRequest {
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
    fn the_main_push_root_span_is_kept_by_the_collector() {
        let fields = crate::span_capture::fields_of(|| main_push_span("p-1", "abc1234:1"));
        assert_eq!(
            fields.get("navigator.keep").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn a_main_push_is_admitted_once_per_sha_and_run_attempt() {
        let mut seen = Vec::new();
        assert_eq!(
            admit_main_push(&mut seen, &push("abc1234", 1)),
            MainPushOutcome::Posted
        );
        assert_eq!(
            admit_main_push(&mut seen, &push("abc1234", 1)),
            MainPushOutcome::Duplicate,
            "a rerun of the same attempt posts nothing new"
        );
        assert_eq!(
            admit_main_push(&mut seen, &push("abc1234", 2)),
            MainPushOutcome::Posted,
            "a re-run attempt is a new event"
        );
        assert_eq!(
            admit_main_push(&mut seen, &push("def5678", 1)),
            MainPushOutcome::Posted
        );
        for index in 0..super::MAIN_PUSH_SEEN_LIMIT {
            let _ = admit_main_push(&mut seen, &push(&format!("{index:07x}"), 1));
        }
        assert_eq!(
            seen.len(),
            super::MAIN_PUSH_SEEN_LIMIT,
            "the memory is bounded"
        );
        assert!(
            !seen.contains(&"abc1234:1".to_string()),
            "the oldest keys age out"
        );
    }

    #[test]
    fn the_request_is_the_shape_the_portal_door_sends() {
        let event: MainPushRequest = serde_json::from_value(serde_json::json!({
            "short_sha": "abc1234",
            "run_url": "https://github.com/an-organization/sample-push/actions/runs/1",
            "author_login": "octocat",
            "gate": "pass",
            "run_attempt": 1
        }))
        .unwrap();
        assert_eq!(event.publish, None);
        assert_eq!(event.event_key(), "abc1234:1");
        let full: MainPushRequest = serde_json::from_value(serde_json::json!({
            "short_sha": "abc1234",
            "run_url": "https://github.com/an-organization/sample-push/actions/runs/1",
            "author_login": "octocat",
            "gate": "fail",
            "publish": "skipped",
            "run_attempt": 3
        }))
        .unwrap();
        assert_eq!(
            (full.gate, full.publish),
            (CiOutcome::Fail, Some(CiOutcome::Skipped))
        );
    }

    /// The content guard, mirroring `client_view_copy_names_no_client_or_matter`:
    /// the notice is built from the mechanism fields only, so a client name,
    /// matter name, Project code, or commit subject can never reach the
    /// channel.
    #[test]
    fn the_main_push_notice_names_no_client_matter_project_or_subject() {
        let event = push("abc1234", 2);
        let message = render_main_push(
            &event,
            Some("https://github.com/an-organization/sample-push"),
        );
        assert_eq!(
            message,
            "Push to main: <https://github.com/an-organization/sample-push/commit/abc1234|`abc1234`> by octocat — gate pass, publish skipped. <https://github.com/an-organization/sample-push/actions/runs/1|CI run #2>"
        );
        let lower = message.to_ascii_lowercase();
        assert!(!lower.contains('@'));
        assert!(!lower.contains("client"));
        assert!(!lower.contains("matter"));
        assert!(!lower.contains("subject"));
        let fields: serde_json::Value = serde_json::to_value(&event).unwrap();
        let names: Vec<&str> = fields
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            names,
            [
                "author_login",
                "gate",
                "publish",
                "run_attempt",
                "run_url",
                "short_sha"
            ],
            "the event carries mechanism fields only; there is no subject or name to leak"
        );
        let bare = render_main_push(&push("abc1234", 1), None);
        assert!(
            bare.starts_with("Push to main: `abc1234` by octocat — gate pass"),
            "{bare}"
        );
    }

    #[tokio::test]
    async fn the_destination_is_the_project_row_channel_and_repository() {
        let surreal = mem_surreal().await;
        let project = create(
            &surreal,
            &NewProject {
                code: "sample-push".to_string(),
                name: "Sample Push".to_string(),
                status: "open".to_string(),
                entity_id: seed_entity(&surreal).await,
                ..Default::default()
            },
        )
        .await
        .expect("create Project");
        store::projects::set_repository_url(
            &surreal,
            project.id,
            Some("https://github.com/an-organization/sample-push"),
        )
        .await
        .unwrap();
        set_internal_slack_channel_id(&surreal, project.id, "CPUSH")
            .await
            .unwrap()
            .unwrap();
        let slack = Arc::new(CapturingSlackBot::new());
        let bot: Arc<dyn workflows::SlackBot> = slack.clone();
        let destination = super::main_push_destination(&surreal, &bot, project.id, false)
            .await
            .expect("destination");
        assert_eq!(destination.channel_id, "CPUSH");
        assert_eq!(
            destination.repository_url.as_deref(),
            Some("https://github.com/an-organization/sample-push")
        );
        assert!(slack.created_channels().is_empty());
    }

    #[test]
    fn client_view_copy_names_no_client_or_matter() {
        assert_eq!(
            CLIENT_PROJECT_VIEW_MESSAGE,
            "A client viewed this Project in the portal."
        );
        let lower = CLIENT_PROJECT_VIEW_MESSAGE.to_ascii_lowercase();
        assert!(!lower.contains("email"));
        assert!(!lower.contains('@'));
    }

    #[tokio::test]
    async fn ensure_creates_a_channel_once_then_reuses_it() {
        let surreal = mem_surreal().await;
        let project = create(
            &surreal,
            &NewProject {
                code: "sample-slack".to_string(),
                name: "Sample Slack".to_string(),
                status: "open".to_string(),
                entity_id: seed_entity(&surreal).await,
                ..Default::default()
            },
        )
        .await
        .expect("create Project");
        let slack = Arc::new(CapturingSlackBot::new());
        let bot: Arc<dyn workflows::SlackBot> = slack.clone();

        let first = ensure_internal_channel(&surreal, &bot, project.id, false)
            .await
            .expect("create channel");
        let second = ensure_internal_channel(&surreal, &bot, project.id, false)
            .await
            .expect("reuse channel");
        assert_eq!(first, second);
        assert_eq!(slack.created_channels(), vec!["project-sample-slack"]);

        post_client_view_notice(&bot, &first)
            .await
            .expect("post notice");
        assert_eq!(
            slack.posted_messages(),
            vec![(first, CLIENT_PROJECT_VIEW_MESSAGE.to_string())]
        );
    }

    #[tokio::test]
    async fn ensure_reuses_a_channel_id_already_on_the_project() {
        let surreal = mem_surreal().await;
        let project = create(
            &surreal,
            &NewProject {
                code: "sample-existing".to_string(),
                name: "Sample Existing".to_string(),
                status: "open".to_string(),
                entity_id: seed_entity(&surreal).await,
                ..Default::default()
            },
        )
        .await
        .expect("create Project");
        set_internal_slack_channel_id(&surreal, project.id, "CEXISTING")
            .await
            .expect("persist channel")
            .expect("project exists");
        let slack = Arc::new(CapturingSlackBot::new());
        let bot: Arc<dyn workflows::SlackBot> = slack.clone();
        let channel = ensure_internal_channel(&surreal, &bot, project.id, false)
            .await
            .expect("reuse stored channel");
        assert_eq!(channel, "CEXISTING");
        assert!(slack.created_channels().is_empty());
    }

    #[tokio::test]
    async fn ensure_fails_closed_when_the_project_is_gone() {
        let surreal = mem_surreal().await;
        let slack: Arc<dyn workflows::SlackBot> = Arc::new(CapturingSlackBot::new());
        let err = ensure_internal_channel(&surreal, &slack, Uuid::now_v7(), false)
            .await
            .expect_err("missing project");
        assert!(format!("{err:?}").contains("no longer exists"), "{err:?}");
    }

    #[tokio::test]
    async fn service_holds_the_store_and_bot() {
        let surreal = mem_surreal().await;
        let slack: Arc<dyn workflows::SlackBot> = Arc::new(CapturingSlackBot::new());
        let _service = ProjectSlackService::new(surreal, slack, false);
    }

    #[tokio::test]
    async fn simulated_deployments_create_staging_prefixed_channels() {
        let surreal = mem_surreal().await;
        let project = create(
            &surreal,
            &NewProject {
                code: "sample-staging".to_string(),
                name: "Sample Staging".to_string(),
                status: "open".to_string(),
                entity_id: seed_entity(&surreal).await,
                ..Default::default()
            },
        )
        .await
        .expect("create Project");
        let slack = Arc::new(CapturingSlackBot::new());
        let bot: Arc<dyn workflows::SlackBot> = slack.clone();
        ensure_internal_channel(&surreal, &bot, project.id, true)
            .await
            .expect("create channel");
        assert_eq!(slack.created_channels(), vec!["staging-sample-staging"]);
    }

    struct ExistingChannelSlack {
        found: Option<workflows::SlackChannel>,
        create_error: Option<&'static str>,
        created: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl workflows::SlackBot for ExistingChannelSlack {
        async fn find_private_channel_by_name(
            &self,
            _name: &str,
        ) -> Result<Option<workflows::SlackChannel>, workflows::SlackBotError> {
            Ok(self.found.clone())
        }
        async fn create_private_channel(
            &self,
            name: &str,
        ) -> Result<workflows::SlackChannel, workflows::SlackBotError> {
            self.created.lock().expect("lock").push(name.to_string());
            match self.create_error {
                Some(code) => Err(workflows::SlackBotError::Api(code.to_string())),
                None => Err(workflows::SlackBotError::IncompleteResponse),
            }
        }
        async fn post_message(&self, _: &str, _: &str) -> Result<(), workflows::SlackBotError> {
            Ok(())
        }
        async fn user_id_by_email(
            &self,
            _: &str,
        ) -> Result<Option<String>, workflows::SlackBotError> {
            Ok(None)
        }
        async fn channel_member_ids(
            &self,
            _: &str,
        ) -> Result<Vec<String>, workflows::SlackBotError> {
            Ok(Vec::new())
        }
        async fn invite_to_channel(
            &self,
            _: &str,
            _: &[String],
        ) -> Result<(), workflows::SlackBotError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn ensure_adopts_an_existing_channel_instead_of_creating_one() {
        let surreal = mem_surreal().await;
        let project = create(
            &surreal,
            &NewProject {
                code: "sample-adopt".to_string(),
                name: "Sample Adopt".to_string(),
                status: "open".to_string(),
                entity_id: seed_entity(&surreal).await,
                ..Default::default()
            },
        )
        .await
        .expect("create Project");
        let slack = Arc::new(ExistingChannelSlack {
            found: Some(workflows::SlackChannel {
                id: "GADOPTED".to_string(),
                name: "project-sample-adopt".to_string(),
            }),
            create_error: None,
            created: std::sync::Mutex::default(),
        });
        let bot: Arc<dyn workflows::SlackBot> = slack.clone();
        let outcome = super::ensure_channel(&surreal, &bot, &project, false)
            .await
            .expect("adopt");
        assert_eq!(outcome, super::ChannelOutcome::Adopted("GADOPTED".into()));
        assert!(slack.created.lock().expect("lock").is_empty());
        let stored = store::projects::find_by_id(&surreal, project.id)
            .await
            .expect("read")
            .expect("exists");
        assert_eq!(
            stored.internal_slack_channel_id.as_deref(),
            Some("GADOPTED")
        );
    }

    #[tokio::test]
    async fn ensure_fails_closed_when_the_name_is_taken_by_an_invisible_channel() {
        let surreal = mem_surreal().await;
        let project = create(
            &surreal,
            &NewProject {
                code: "sample-taken".to_string(),
                name: "Sample Taken".to_string(),
                status: "open".to_string(),
                entity_id: seed_entity(&surreal).await,
                ..Default::default()
            },
        )
        .await
        .expect("create Project");
        let slack = Arc::new(ExistingChannelSlack {
            found: None,
            create_error: Some("name_taken"),
            created: std::sync::Mutex::default(),
        });
        let bot: Arc<dyn workflows::SlackBot> = slack;
        let error = super::ensure_channel(&surreal, &bot, &project, false)
            .await
            .expect_err("name taken");
        assert!(format!("{error:?}").contains("cannot see"), "{error:?}");
    }
}
