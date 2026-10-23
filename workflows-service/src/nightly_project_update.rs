//! The Nightly Project Update: the parent DAG that walks every Project each
//! night and runs one child DAG per tag.
//!
//! The parent is a Restate workflow keyed on the UTC date, so a second fire on
//! the same day attaches to the first instead of running again. Every child is
//! a Restate workflow keyed `{date}:{project id}`, one per (Project, tag), so a
//! failing tag is counted against that Project and tag and the sweep moves on.
//! The tag set is closed ([`Tag`]); see the glossary term for what each child
//! owns.
//!
//! The clock, the Project roster, and every side effect run inside journaled
//! steps. The journal carries Project ids and aggregate counts only; directory
//! emails and Slack identities never leave the child step that uses them.

use std::fmt::Write as _;
use std::future::Future;
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use store::surreal::SurrealDb;
use uuid::Uuid;
use workflows::{SlackBot, SlackBotError};

pub mod membership;
pub mod repository;
pub mod slack;

use cloud::forgejo_governance::UpdateProposal;
use membership::MembershipOutcome;
use repository::RepositoryOutcome;
use slack::SlackOutcome;

/// A child DAG. The set is closed: a new tag is a glossary change in the same
/// pull request as its child.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tag {
    Membership,
    Slack,
    Repository,
}

impl Tag {
    /// Every tag the parent runs, in the order it runs them for one Project.
    pub const ALL: &'static [Tag] = &[Tag::Membership, Tag::Slack, Tag::Repository];

    /// The tag as the glossary names it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Tag::Membership => "membership",
            Tag::Slack => "slack",
            Tag::Repository => "repository",
        }
    }

    /// The Restate service that runs this tag's child DAG.
    #[must_use]
    pub const fn service(self) -> &'static str {
        match self {
            Tag::Membership => "NightlyProjectUpdateMembership",
            Tag::Slack => "NightlyProjectUpdateSlack",
            Tag::Repository => "NightlyProjectUpdateRepository",
        }
    }

    /// Whether the tag runs for a Project in this state. A closed or archived
    /// Project is not updated: `membership` only archives its channel, and it
    /// is never given a repository, canvas, or notice.
    #[must_use]
    pub const fn applies_to(self, open: bool) -> bool {
        match self {
            Tag::Membership => true,
            Tag::Slack | Tag::Repository => open,
        }
    }
}

/// The parent's idempotency key for the UTC day containing `now`.
#[must_use]
pub fn run_key(now: DateTime<Utc>) -> String {
    now.format("%Y-%m-%d").to_string()
}

/// The key of one (Project, tag) child invocation within a run.
#[must_use]
pub fn child_key(ran_on: &str, project_id: Uuid) -> String {
    format!("{ran_on}:{project_id}")
}

fn parse_run_key(key: &str) -> Result<NaiveDate, HandlerError> {
    NaiveDate::parse_from_str(key, "%Y-%m-%d").map_err(|_| {
        TerminalError::new("Nightly Project Update is keyed on its UTC date, YYYY-MM-DD").into()
    })
}

/// What a child needs to know. Nothing about the Project beyond its id.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ProjectTagRequest {
    /// The parent's UTC date key.
    pub ran_on: String,
    pub project_id: Uuid,
}

/// Retries a child's side-effecting step gets before the failure is counted
/// against its (Project, tag) and the sweep moves on.
pub(crate) fn tag_retry_policy() -> RunRetryPolicy {
    RunRetryPolicy::new()
        .initial_delay(std::time::Duration::from_secs(1))
        .exponentiation_factor(2.0)
        .max_delay(std::time::Duration::from_secs(30))
        .max_attempts(4)
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct RunRequest {}

/// One Project the parent walks. The journal holds this and nothing more.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct RosterEntry {
    pub project_id: Uuid,
    pub open: bool,
}

/// A child's result, by tag.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub enum TagOutcome {
    Membership(MembershipOutcome),
    Slack(SlackOutcome),
    Repository(RepositoryOutcome),
}

/// Aggregate counts for one run. Safe for the Restate journal and for
/// `#messages` or `#staging-messages`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct NightlyProjectUpdateReport {
    pub projects: usize,
    pub open_projects: usize,
    pub channels: usize,
    pub projects_without_channel: usize,
    pub invited: usize,
    pub already_members: usize,
    pub unmatched_assignments: usize,
    /// Channels created for an open Project that had none.
    pub channels_created: usize,
    /// Channels adopted, or swapped in for a deleted one.
    pub channels_repaired: usize,
    /// Channels of a closed or archived Project archived this run.
    #[serde(default)]
    pub channels_archived: usize,
    /// Internal channel canvases created for an open Project that had none.
    pub canvases_created: usize,
    /// Internal channel canvases edited in place.
    pub canvases_edited: usize,
    /// Daily updates posted to an internal channel.
    pub notices_posted: usize,
    /// Repositories created or adopted for an open Project.
    pub repositories_synced: usize,
    /// Open Projects whose `navigator.yaml` pins the running release or a
    /// newer one.
    pub pins_current: usize,
    /// Open Projects whose `navigator.yaml` pins an older release than the
    /// one running. On Forgejo each also gets an update pull request.
    pub pins_stale: usize,
    /// Open Projects whose `navigator.yaml` is missing, unparsable, or pins
    /// something that is not a release version.
    pub pins_unreadable: usize,
    /// Forgejo repositories whose governance had drifted and was repaired.
    #[serde(default)]
    pub governance_repaired: usize,
    /// Version update pull requests opened this run, with squash auto-merge.
    #[serde(default)]
    pub update_prs_opened: usize,
    /// Version update pull requests from an earlier run still open.
    #[serde(default)]
    pub update_prs_pending: usize,
    /// Stale pins whose release's update pull request was closed unmerged
    /// or already merged; none is reopened.
    #[serde(default)]
    pub update_prs_declined: usize,
    /// Projects whose `membership` child failed this run.
    pub membership_failures: usize,
    /// Projects whose `slack` child failed this run.
    pub slack_failures: usize,
    /// Projects whose `repository` child failed this run.
    pub repository_failures: usize,
    /// Projects with at least one failed child (or canvas) this run.
    #[serde(default)]
    pub failed_projects: usize,
    /// Failures grouped by tag and bounded cause, in first-seen order, so one
    /// cause repeated for every Project (a Slack token missing a scope) reads
    /// as one line with a count instead of only a tally.
    #[serde(default)]
    pub failure_causes: Vec<FailureCause>,
}

/// Failures of one tag that share a bounded cause.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct FailureCause {
    pub tag: String,
    /// A bounded code from [`failure_reason`]; also the metric's `error.type`.
    pub reason: String,
    pub count: usize,
    /// The first message seen for this cause, capped at 300 characters.
    /// Slack and Restate errors carry no addresses.
    pub example: String,
}

/// Classify a child failure into a bounded cause code. Metric labels take the
/// code, never the message, so a new message cannot add a series.
#[must_use]
pub fn failure_reason(message: &str) -> &'static str {
    if message.contains("channel name is taken") {
        "slack_name_taken"
    } else if message.contains("missing_scope") {
        "slack_missing_scope"
    } else if message.contains("channel_not_found") || message.contains("not_in_channel") {
        "slack_channel_not_found"
    } else if message.contains("rate limited") || message.contains("ratelimited") {
        "slack_rate_limited"
    } else if message.contains("Slack") {
        "slack_other"
    } else {
        "other"
    }
}

/// The bounded cause for a canvas that failed inside an otherwise good
/// `slack` child.
const CANVAS_FAILED: &str = "slack_canvas_failed";

impl NightlyProjectUpdateReport {
    /// Child failures across every tag, canvases included.
    #[must_use]
    pub fn failures(&self) -> usize {
        self.membership_failures + self.slack_failures + self.repository_failures
    }

    /// The metric shape: `(stage, error_type, count)` per cause.
    #[must_use]
    pub fn failure_counts(&self) -> Vec<(&str, &str, u64)> {
        self.failure_causes
            .iter()
            .map(|cause| {
                (
                    cause.tag.as_str(),
                    cause.reason.as_str(),
                    u64::try_from(cause.count).unwrap_or(u64::MAX),
                )
            })
            .collect()
    }

    fn note_cause(&mut self, tag: Tag, reason: &str, count: usize, example: &str) {
        if let Some(cause) = self
            .failure_causes
            .iter_mut()
            .find(|cause| cause.tag == tag.name() && cause.reason == reason)
        {
            cause.count += count;
            return;
        }
        self.failure_causes.push(FailureCause {
            tag: tag.name().to_string(),
            reason: reason.to_string(),
            count,
            example: example.chars().take(300).collect(),
        });
    }

    fn absorb(&mut self, outcome: TagOutcome) {
        match outcome {
            TagOutcome::Membership(m) => {
                self.channels += m.channels;
                self.projects_without_channel += m.projects_without_channel;
                self.invited += m.invited;
                self.already_members += m.already_members;
                self.unmatched_assignments += m.unmatched_assignments;
                self.channels_created += m.channels_created;
                self.channels_repaired += m.channels_repaired;
                self.channels_archived += m.channels_archived;
            }
            TagOutcome::Slack(o) => {
                self.canvases_created += o.canvases_created;
                self.canvases_edited += o.canvases_edited;
                self.notices_posted += o.notices_posted;
                self.slack_failures += o.canvas_failures;
                if o.canvas_failures > 0 {
                    self.note_cause(
                        Tag::Slack,
                        CANVAS_FAILED,
                        o.canvas_failures,
                        "the internal channel canvas could not be written",
                    );
                }
            }
            TagOutcome::Repository(r) => {
                self.repositories_synced += r.synced;
                match r.pin {
                    Some(rules::project_manifest::PinStatus::Current) => self.pins_current += 1,
                    Some(rules::project_manifest::PinStatus::Stale) => self.pins_stale += 1,
                    Some(rules::project_manifest::PinStatus::Unreadable) => {
                        self.pins_unreadable += 1;
                    }
                    None => {}
                }
                self.governance_repaired += usize::from(r.governance_repaired > 0);
                match r.update {
                    Some(UpdateProposal::Opened) => self.update_prs_opened += 1,
                    Some(UpdateProposal::Pending) => self.update_prs_pending += 1,
                    Some(UpdateProposal::Declined) => self.update_prs_declined += 1,
                    None => {}
                }
            }
        }
    }

    fn fail(&mut self, tag: Tag, error: &TerminalError) {
        let message = error.message();
        self.note_cause(tag, failure_reason(message), 1, message);
        match tag {
            Tag::Membership => self.membership_failures += 1,
            Tag::Slack => self.slack_failures += 1,
            Tag::Repository => self.repository_failures += 1,
        }
    }
}

/// Run every applicable tag for every Project. `call` runs one child; its
/// failure is counted against the tag and never stops another tag or Project.
pub async fn sweep<F, Fut>(roster: &[RosterEntry], mut call: F) -> NightlyProjectUpdateReport
where
    F: FnMut(Tag, Uuid) -> Fut,
    Fut: Future<Output = Result<TagOutcome, TerminalError>>,
{
    let mut report = NightlyProjectUpdateReport {
        projects: roster.len(),
        open_projects: roster.iter().filter(|entry| entry.open).count(),
        ..Default::default()
    };
    for entry in roster {
        let failures_before = report.failures();
        for tag in Tag::ALL
            .iter()
            .copied()
            .filter(|tag| tag.applies_to(entry.open))
        {
            match call(tag, entry.project_id).await {
                Ok(outcome) => report.absorb(outcome),
                Err(error) => {
                    tracing::error!(
                        project_id = %entry.project_id,
                        tag = tag.name(),
                        reason = failure_reason(error.message()),
                        %error,
                        "Nightly Project Update child failed"
                    );
                    report.fail(tag, &error);
                }
            }
        }
        report.failed_projects += usize::from(report.failures() > failures_before);
    }
    report
}

#[derive(Clone)]
pub struct NightlyProjectUpdateService {
    surreal: SurrealDb,
    slack: Arc<dyn SlackBot>,
    messages_channel_id: String,
    staging_messages_channel_id: String,
    simulated: bool,
    slack_enabled: bool,
}

impl NightlyProjectUpdateService {
    #[must_use]
    pub fn new(
        surreal: SurrealDb,
        slack: Arc<dyn SlackBot>,
        messages_channel_id: String,
        staging_messages_channel_id: String,
        simulated: bool,
        slack_enabled: bool,
    ) -> Self {
        Self {
            surreal,
            slack,
            messages_channel_id,
            staging_messages_channel_id,
            simulated,
            slack_enabled,
        }
    }
}

#[restate_sdk::workflow(name = "NightlyProjectUpdate")]
impl NightlyProjectUpdateService {
    #[restate_sdk::handler]
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        _request: Json<RunRequest>,
    ) -> Result<Json<NightlyProjectUpdateReport>, HandlerError> {
        let ran_on = ctx.key().to_string();
        parse_run_key(&ran_on)?;
        let outcome_channel = outcome_channel_id(
            self.simulated,
            &self.messages_channel_id,
            &self.staging_messages_channel_id,
        );
        validate_configuration(outcome_channel, self.slack_enabled)?;

        let started_at = ctx
            .run(|| async { Ok(Json(Utc::now().to_rfc3339())) })
            .name("read-clock")
            .await?
            .into_inner();
        tracing::info!(%ran_on, %started_at, "Nightly Project Update started");

        let slack = Arc::clone(&self.slack);
        let channel_id = outcome_channel.to_string();
        ctx.run(move || async move { post_start(slack.as_ref(), &channel_id).await })
            .name("post-start")
            .await?;

        let surreal = self.surreal.clone();
        let roster = ctx
            .run(move || async move { discover_roster(&surreal).await.map(Json) })
            .name("discover-projects")
            .await?
            .into_inner();

        let context = &ctx;
        let ran_on = &ran_on;
        let report = sweep(&roster, |tag, project_id| async move {
            let key = child_key(ran_on, project_id);
            let request = Json(ProjectTagRequest {
                ran_on: ran_on.clone(),
                project_id,
            });
            match tag {
                Tag::Membership => context
                    .workflow_client::<membership::MembershipTagServiceClient>(key)
                    .run(request)
                    .call()
                    .await
                    .map(|Json(outcome)| TagOutcome::Membership(outcome)),
                Tag::Slack => context
                    .workflow_client::<slack::SlackTagServiceClient>(key)
                    .run(request)
                    .call()
                    .await
                    .map(|Json(outcome)| TagOutcome::Slack(outcome)),
                Tag::Repository => context
                    .workflow_client::<repository::RepositoryTagServiceClient>(key)
                    .run(request)
                    .call()
                    .await
                    .map(|Json(outcome)| TagOutcome::Repository(outcome)),
            }
        })
        .await;

        // In its own step so a journal replay does not count the run twice.
        let recorded = report.clone();
        ctx.run(move || {
            record_outcome(&recorded);
            async { Ok::<(), HandlerError>(()) }
        })
        .name("record-outcome")
        .await?;

        let message = render_report(&report);
        let slack = Arc::clone(&self.slack);
        let channel_id = outcome_channel.to_string();
        ctx.run(move || async move {
            slack
                .post_message(&channel_id, &message)
                .await
                .map_err(slack_error)
        })
        .name("post-outcome")
        .await?;

        Ok(Json(report))
    }
}

pub(crate) async fn discover_roster(surreal: &SurrealDb) -> Result<Vec<RosterEntry>, HandlerError> {
    Ok(store::projects::all(surreal)
        .await
        .map_err(HandlerError::from)?
        .into_iter()
        .map(|project| RosterEntry {
            project_id: project.id,
            open: project.status == "open",
        })
        .collect())
}

pub(crate) fn slack_error(error: SlackBotError) -> HandlerError {
    match error {
        SlackBotError::Api(code) => HandlerError::from(TerminalError::new(format!(
            "Slack rejected the Nightly Project Update: {code}"
        ))),
        error @ SlackBotError::MissingScope { .. } => HandlerError::from(TerminalError::new(
            format!("Slack rejected the Nightly Project Update: {error}"),
        )),
        SlackBotError::HttpStatus(status) => HandlerError::from(TerminalError::new(format!(
            "Slack rejected the Nightly Project Update with HTTP {status}"
        ))),
        SlackBotError::Transport(_) => {
            HandlerError::from(SlackBotError::Transport("Slack request failed".to_string()))
        }
        SlackBotError::RetryableHttpStatus(status) => {
            HandlerError::from(SlackBotError::RetryableHttpStatus(status))
        }
        SlackBotError::RateLimited {
            retry_after_seconds,
        } => HandlerError::from(SlackBotError::RateLimited {
            retry_after_seconds,
        }),
        SlackBotError::IncompleteResponse => HandlerError::from(SlackBotError::IncompleteResponse),
    }
}

const START_MESSAGE: &str = "*Nightly Project Update beginning*\nRepairing Slack channels, reconciling internal membership, and checking repositories.";

async fn post_start(slack: &dyn SlackBot, channel_id: &str) -> Result<(), HandlerError> {
    slack
        .post_message(channel_id, START_MESSAGE)
        .await
        .map_err(slack_error)
}

/// Export the run's outcome metric and, when degraded, one error-level
/// summary so a log alert sees the run and not only its children.
fn record_outcome(report: &NightlyProjectUpdateReport) {
    if report.failures() > 0 {
        tracing::error!(
            failures = report.failures(),
            failed_projects = report.failed_projects,
            projects = report.projects,
            "Nightly Project Update degraded"
        );
    }
    telemetry::record_nightly_project_update(&report.failure_counts());
}

/// The most causes the Slack update lists before summarizing the rest.
const CAUSES_SHOWN: usize = 5;

fn render_headline(report: &NightlyProjectUpdateReport) -> String {
    if report.failures() == 0 {
        return "*Nightly Project Update complete*".to_string();
    }
    let mut headline = format!(
        ":rotating_light: *Nightly Project Update DEGRADED* — {} failures across {} of {} Projects",
        report.failures(),
        report.failed_projects,
        report.projects
    );
    for cause in report.failure_causes.iter().take(CAUSES_SHOWN) {
        let _ = write!(
            headline,
            "\n• {} × {} — {}: {}",
            cause.tag, cause.count, cause.reason, cause.example
        );
    }
    let hidden = report.failure_causes.len().saturating_sub(CAUSES_SHOWN);
    if hidden > 0 {
        let _ = write!(headline, "\n• …and {hidden} more causes");
    }
    headline
}

pub(crate) fn render_report(report: &NightlyProjectUpdateReport) -> String {
    format!(
        "{}\nProjects: {} ({} open).\nChannels checked: {}; channels created: {}; channels repaired: {}; channels archived: {}; Projects without a channel: {}; people invited: {}; already members: {}; unmatched assignments: {}.\nCanvases created: {}; canvases edited: {}; notices posted: {}.\nRepositories created or adopted: {}. Navigator pins — current: {}; stale: {}; unreadable: {}.\nUpdate pull requests — opened: {}; awaiting merge: {}; declined: {}. Governance repaired: {}.\nFailures by tag — membership: {}; slack: {}; repository: {}.",
        render_headline(report),
        report.projects,
        report.open_projects,
        report.channels,
        report.channels_created,
        report.channels_repaired,
        report.channels_archived,
        report.projects_without_channel,
        report.invited,
        report.already_members,
        report.unmatched_assignments,
        report.canvases_created,
        report.canvases_edited,
        report.notices_posted,
        report.repositories_synced,
        report.pins_current,
        report.pins_stale,
        report.pins_unreadable,
        report.update_prs_opened,
        report.update_prs_pending,
        report.update_prs_declined,
        report.governance_repaired,
        report.membership_failures,
        report.slack_failures,
        report.repository_failures,
    )
}

fn outcome_channel_id<'a>(
    simulated: bool,
    messages: &'a str,
    staging_messages: &'a str,
) -> &'a str {
    if simulated {
        staging_messages
    } else {
        messages
    }
}

fn validate_configuration(channel_id: &str, slack_enabled: bool) -> Result<(), HandlerError> {
    if channel_id.is_empty() {
        return Err(TerminalError::new(
            "messages channel id for this deployment is not configured",
        )
        .into());
    }
    if !slack_enabled {
        return Err(TerminalError::new(
            "SLACK_BOT_TOKEN must be configured for the Nightly Project Update",
        )
        .into());
    }
    Ok(())
}

/// The production composition, minus Restate: every tag's real child logic
/// run in-process against fakes, so tests exercise the sweep end to end.
#[cfg(test)]
pub(crate) async fn sweep_in_process(
    surreal: &SurrealDb,
    slack: &Arc<dyn SlackBot>,
    forge: Option<&cloud::forge::FakeForge>,
    simulated: bool,
) -> NightlyProjectUpdateReport {
    let roster = discover_roster(surreal).await.expect("discover Projects");
    let forge = &forge.map_or(repository::RepositoryForge::None, |forge| {
        repository::RepositoryForge::GitHub(Arc::new(forge.clone()))
    });
    sweep(&roster, |tag, project_id| async move {
        match tag {
            Tag::Membership => membership::reconcile_project(surreal, slack, simulated, project_id)
                .await
                .map(TagOutcome::Membership),
            Tag::Slack => {
                slack::reconcile_slack(surreal, slack.as_ref(), None, "2026-10-05", project_id)
                    .await
                    .map(TagOutcome::Slack)
            }
            Tag::Repository => repository::reconcile_repository(
                surreal,
                forge,
                project_id,
                repository::RUNTIME_VERSION,
            )
            .await
            .map(TagOutcome::Repository),
        }
        .map_err(|error| TerminalError::new(format!("{error:?}")))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn at(date: &str, time: &str) -> DateTime<Utc> {
        format!("{date}T{time}Z").parse().expect("timestamp")
    }

    #[test]
    fn the_parent_key_is_the_utc_date() {
        assert_eq!(run_key(at("2026-10-05", "05:55:00")), "2026-10-05");
        assert_eq!(
            run_key(at("2026-10-05", "05:55:00")),
            run_key(at("2026-10-05", "23:59:59")),
            "a second fire the same UTC day keys to the same invocation"
        );
        assert_ne!(
            run_key(at("2026-10-05", "23:59:59")),
            run_key(at("2026-10-06", "00:00:00"))
        );
    }

    #[test]
    fn only_a_utc_date_is_a_valid_parent_key() {
        assert!(parse_run_key("2026-10-05").is_ok());
        for bad in ["", "today", "2026-13-01", "2026-10-05T05:55:00Z"] {
            assert!(parse_run_key(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_child_key_names_the_day_and_the_project() {
        let id = Uuid::nil();
        assert_eq!(
            child_key("2026-10-05", id),
            "2026-10-05:00000000-0000-0000-0000-000000000000"
        );
        assert_eq!(child_key("2026-10-05", id), child_key("2026-10-05", id));
    }

    fn entry(open: bool) -> RosterEntry {
        RosterEntry {
            project_id: Uuid::new_v4(),
            open,
        }
    }

    #[tokio::test]
    async fn one_tag_failure_is_counted_and_the_other_tags_still_run() {
        let roster = [entry(true), entry(true)];
        let failing = roster[0].project_id;
        let ran = RefCell::new(Vec::new());
        let report = sweep(&roster, |tag, id| {
            ran.borrow_mut().push((id, tag));
            async move {
                match tag {
                    Tag::Membership if id == failing => Err(TerminalError::new("Slack down")),
                    Tag::Membership => Ok(TagOutcome::Membership(MembershipOutcome {
                        channels: 1,
                        invited: 2,
                        ..Default::default()
                    })),
                    Tag::Slack => Ok(TagOutcome::Slack(SlackOutcome {
                        notices_posted: 1,
                        ..Default::default()
                    })),
                    Tag::Repository => Ok(TagOutcome::Repository(RepositoryOutcome {
                        synced: 1,
                        ..Default::default()
                    })),
                }
            }
        })
        .await;

        assert_eq!(report.membership_failures, 1);
        assert_eq!(report.failed_projects, 1);
        assert_eq!(report.failure_causes.len(), 1);
        assert!(report.failure_causes[0].example.contains("Slack down"));
        assert_eq!((report.slack_failures, report.repository_failures), (0, 0));
        assert_eq!(
            report.notices_posted, 2,
            "the slack tag ran for both Projects"
        );
        assert_eq!(report.channels, 1, "the other Project's membership counted");
        assert_eq!(report.invited, 2);
        assert_eq!(
            report.repositories_synced, 2,
            "the failing Project's repository tag still ran"
        );
        assert_eq!(ran.borrow().len(), 6, "every tag ran for every Project");
    }

    #[tokio::test]
    async fn each_update_proposal_outcome_is_counted_separately() {
        let proposals = [
            Some(UpdateProposal::Opened),
            Some(UpdateProposal::Pending),
            Some(UpdateProposal::Declined),
            None,
        ];
        let roster: Vec<RosterEntry> = proposals.iter().map(|_| entry(true)).collect();
        let by_project: Vec<(Uuid, Option<UpdateProposal>)> = roster
            .iter()
            .map(|entry| entry.project_id)
            .zip(proposals)
            .collect();
        let by_project = &by_project;
        let report = sweep(&roster, |tag, id| async move {
            match tag {
                Tag::Membership => Ok(TagOutcome::Membership(MembershipOutcome::default())),
                Tag::Slack => Ok(TagOutcome::Slack(SlackOutcome::default())),
                Tag::Repository => Ok(TagOutcome::Repository(RepositoryOutcome {
                    pin: Some(rules::project_manifest::PinStatus::Stale),
                    update: by_project
                        .iter()
                        .find(|(project_id, _)| *project_id == id)
                        .and_then(|(_, update)| *update),
                    ..Default::default()
                })),
            }
        })
        .await;
        assert_eq!(
            (
                report.update_prs_opened,
                report.update_prs_pending,
                report.update_prs_declined,
                report.pins_stale,
            ),
            (1, 1, 1, 4)
        );
        assert!(render_report(&report)
            .contains("Update pull requests — opened: 1; awaiting merge: 1; declined: 1."));
    }

    #[tokio::test]
    async fn a_failing_repository_does_not_stop_the_next_project() {
        let roster = [entry(true), entry(true)];
        let failing = roster[0].project_id;
        let report = sweep(&roster, |tag, id| async move {
            match tag {
                Tag::Repository if id == failing => Err(TerminalError::new("forge down")),
                Tag::Membership => Ok(TagOutcome::Membership(MembershipOutcome::default())),
                Tag::Slack => Ok(TagOutcome::Slack(SlackOutcome::default())),
                Tag::Repository => Ok(TagOutcome::Repository(RepositoryOutcome {
                    synced: 1,
                    ..Default::default()
                })),
            }
        })
        .await;
        assert_eq!(report.repository_failures, 1);
        assert_eq!(report.repositories_synced, 1);
        assert_eq!(report.membership_failures, 0);
    }

    #[tokio::test]
    async fn a_failing_canvas_is_counted_and_the_sweep_continues() {
        let roster = [entry(true), entry(true)];
        let failing = roster[0].project_id;
        let report = sweep(&roster, |tag, id| async move {
            match tag {
                Tag::Slack if id == failing => Err(TerminalError::new("missing_scope")),
                Tag::Membership => Ok(TagOutcome::Membership(MembershipOutcome::default())),
                Tag::Slack => Ok(TagOutcome::Slack(SlackOutcome {
                    canvases_edited: 1,
                    notices_posted: 1,
                    ..Default::default()
                })),
                Tag::Repository => Ok(TagOutcome::Repository(RepositoryOutcome {
                    synced: 1,
                    ..Default::default()
                })),
            }
        })
        .await;
        assert_eq!(report.slack_failures, 1);
        assert_eq!((report.canvases_edited, report.notices_posted), (1, 1));
        assert_eq!(report.repositories_synced, 2, "repository still ran");
        assert!(render_report(&report).contains("slack: 1"));
    }

    #[tokio::test]
    async fn a_closed_project_runs_membership_only() {
        let roster = [entry(false)];
        let ran = RefCell::new(Vec::new());
        let report = sweep(&roster, |tag, _| {
            ran.borrow_mut().push(tag);
            async move { Ok(TagOutcome::Membership(MembershipOutcome::default())) }
        })
        .await;
        assert_eq!(*ran.borrow(), [Tag::Membership]);
        assert_eq!((report.projects, report.open_projects), (1, 0));
    }

    #[tokio::test]
    async fn a_second_run_the_same_day_creates_nothing_new() {
        use cloud::forge::FakeForge;
        use store::projects::{create, NewProject};
        use store::test_support::{mem_surreal, seed_entity};

        let surreal = mem_surreal().await;
        create(
            &surreal,
            &NewProject {
                code: "sample-idempotent".to_string(),
                name: "Sample".to_string(),
                status: "open".to_string(),
                entity_id: seed_entity(&surreal).await,
                ..Default::default()
            },
        )
        .await
        .expect("create Project");
        let slack: Arc<dyn SlackBot> = Arc::new(workflows::CapturingSlackBot::new());
        let forge = FakeForge::new();

        let first = sweep_in_process(&surreal, &slack, Some(&forge), false).await;
        assert_eq!((first.channels_created, first.repositories_synced), (1, 1));
        let second = sweep_in_process(&surreal, &slack, Some(&forge), false).await;
        assert_eq!(
            (
                second.channels_created,
                second.channels_repaired,
                second.repositories_synced
            ),
            (0, 0, 0),
            "recorded channel and repository are kept"
        );
        assert_eq!(
            (second.membership_failures, second.repository_failures),
            (0, 0)
        );
    }

    #[tokio::test]
    async fn the_aggregate_counts_pins_by_status_and_carries_no_repository_url() {
        use rules::project_manifest::PinStatus;
        let roster = [entry(true), entry(true), entry(true)];
        let statuses = [PinStatus::Current, PinStatus::Stale, PinStatus::Unreadable];
        let updates = [None, Some(UpdateProposal::Opened), None];
        let report = sweep(&roster, |tag, id| async move {
            match tag {
                Tag::Membership => Ok(TagOutcome::Membership(MembershipOutcome::default())),
                Tag::Slack => Ok(TagOutcome::Slack(SlackOutcome::default())),
                Tag::Repository => {
                    let index = roster
                        .iter()
                        .position(|entry| entry.project_id == id)
                        .unwrap();
                    Ok(TagOutcome::Repository(RepositoryOutcome {
                        synced: 0,
                        pin: Some(statuses[index]),
                        pinned_version: Some("26.9.13".to_string()),
                        runtime_version: Some("26.10.4".to_string()),
                        governance_repaired: index * 3,
                        update: updates[index],
                    }))
                }
            }
        })
        .await;
        assert_eq!(
            (
                report.pins_current,
                report.pins_stale,
                report.pins_unreadable
            ),
            (1, 1, 1)
        );
        assert_eq!(
            (report.update_prs_opened, report.governance_repaired),
            (1, 2),
            "repositories repaired, not controls"
        );
        let message = render_report(&report);
        assert!(
            message.contains("Navigator pins — current: 1; stale: 1; unreadable: 1."),
            "{message}"
        );
        assert!(
            message.contains(
                "Update pull requests — opened: 1; awaiting merge: 0; declined: 0. Governance repaired: 2."
            ),
            "{message}"
        );
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("https://"));
        assert!(
            !json.contains("26.9.13"),
            "the aggregate carries counts, not versions"
        );
    }

    #[test]
    fn every_tag_has_a_distinct_glossary_name_and_service() {
        let mut names: Vec<_> = Tag::ALL.iter().map(|tag| tag.name()).collect();
        let mut services: Vec<_> = Tag::ALL.iter().map(|tag| tag.service()).collect();
        names.sort_unstable();
        services.sort_unstable();
        names.dedup();
        services.dedup();
        assert_eq!(names.len(), Tag::ALL.len());
        assert_eq!(services.len(), Tag::ALL.len());
    }

    #[test]
    fn tag_order_preserves_the_parent_journal_sequence() {
        assert_eq!(Tag::ALL, &[Tag::Membership, Tag::Slack, Tag::Repository]);
    }

    #[test]
    fn the_daily_trigger_runs_at_0555_utc_in_every_manifest() {
        for source in [
            include_str!("../../examples/deploy/k8s/exports/cron-nightly-project-update-trigger.yaml"),
            include_str!("../../k8s/overlays/kind/nightly-project-update-trigger/nightly-project-update-trigger.yaml"),
        ] {
            let manifest: serde_yaml::Value = serde_yaml::from_str(source).unwrap();
            assert_eq!(manifest["spec"]["schedule"].as_str(), Some("55 5 * * *"));
            assert_eq!(manifest["spec"]["timeZone"].as_str(), Some("Etc/UTC"));
            assert_eq!(
                manifest["metadata"]["name"].as_str(),
                Some("nightly-project-update-trigger")
            );
        }
    }

    #[test]
    fn the_trigger_manifest_is_listed_in_every_kustomization() {
        let exports: serde_yaml::Value = serde_yaml::from_str(include_str!(
            "../../examples/deploy/k8s/exports/kustomization.yaml"
        ))
        .unwrap();
        let listed = |value: &serde_yaml::Value, entry: &str| {
            value["resources"]
                .as_sequence()
                .unwrap()
                .iter()
                .any(|resource| resource.as_str() == Some(entry))
        };
        assert!(listed(&exports, "cron-nightly-project-update-trigger.yaml"));
        let kind: serde_yaml::Value =
            serde_yaml::from_str(include_str!("../../k8s/overlays/kind/kustomization.yaml"))
                .unwrap();
        assert!(listed(&kind, "nightly-project-update-trigger"));
        let overlay: serde_yaml::Value = serde_yaml::from_str(include_str!(
            "../../k8s/overlays/kind/nightly-project-update-trigger/kustomization.yaml"
        ))
        .unwrap();
        assert!(listed(&overlay, "nightly-project-update-trigger.yaml"));
    }

    #[test]
    fn the_trigger_starts_the_parent_keyed_on_the_utc_date() {
        let source = include_str!("bin/nightly-project-update-trigger.rs");
        assert!(source.contains("\"NightlyProjectUpdate\""));
        assert!(source.contains("nightly_project_update::run_key(chrono::Utc::now())"));
    }

    #[test]
    fn start_notice_uses_the_environment_report_channel() {
        assert_eq!(
            outcome_channel_id(false, "C-MESSAGES", "C-STAGING"),
            "C-MESSAGES"
        );
        assert_eq!(
            outcome_channel_id(true, "C-MESSAGES", "C-STAGING"),
            "C-STAGING"
        );
        assert!(!START_MESSAGE.contains('@'));
    }

    #[tokio::test]
    async fn the_start_notice_posts_to_the_chosen_channel() {
        let slack = workflows::CapturingSlackBot::new();
        post_start(&slack, "C-STAGING").await.expect("start notice");
        assert_eq!(
            slack.posted_messages().last().unwrap(),
            &("C-STAGING".to_string(), START_MESSAGE.to_string())
        );
    }

    #[test]
    fn configuration_requires_a_report_channel_and_an_enabled_slack_bot() {
        assert!(validate_configuration("C-MESSAGES", true).is_ok());
        assert!(validate_configuration("", true).is_err());
        assert!(validate_configuration("C-MESSAGES", false).is_err());
    }

    #[test]
    fn outcome_contains_only_aggregate_counts() {
        let message = render_report(&NightlyProjectUpdateReport {
            projects: 9,
            open_projects: 8,
            channels: 3,
            projects_without_channel: 2,
            invited: 4,
            already_members: 2,
            unmatched_assignments: 1,
            channels_created: 5,
            channels_repaired: 6,
            channels_archived: 14,
            canvases_created: 10,
            canvases_edited: 11,
            notices_posted: 12,
            repositories_synced: 7,
            pins_current: 5,
            pins_stale: 1,
            pins_unreadable: 2,
            governance_repaired: 3,
            update_prs_opened: 4,
            update_prs_pending: 1,
            update_prs_declined: 0,
            ..Default::default()
        });
        assert_eq!(
            message,
            "*Nightly Project Update complete*\nProjects: 9 (8 open).\nChannels checked: 3; channels created: 5; channels repaired: 6; channels archived: 14; Projects without a channel: 2; people invited: 4; already members: 2; unmatched assignments: 1.\nCanvases created: 10; canvases edited: 11; notices posted: 12.\nRepositories created or adopted: 7. Navigator pins — current: 5; stale: 1; unreadable: 2.\nUpdate pull requests — opened: 4; awaiting merge: 1; declined: 0. Governance repaired: 3.\nFailures by tag — membership: 0; slack: 0; repository: 0."
        );
        assert!(!message.contains('@'));
    }

    #[tokio::test]
    async fn one_cause_repeated_for_every_project_is_one_loud_line() {
        let roster: Vec<_> = (0..22).map(|_| entry(true)).collect();
        let healthy = roster[0].project_id;
        let report = sweep(&roster, |tag, id| async move {
            match tag {
                Tag::Membership if id != healthy => Err(TerminalError::new(
                    "Slack channel name is taken by a channel this bot cannot see; invite the bot to that private channel or archive it",
                )),
                Tag::Membership => Ok(TagOutcome::Membership(MembershipOutcome::default())),
                Tag::Slack if id == healthy => Ok(TagOutcome::Slack(SlackOutcome {
                    canvas_failures: 1,
                    ..Default::default()
                })),
                Tag::Slack => Ok(TagOutcome::Slack(SlackOutcome::default())),
                Tag::Repository => Ok(TagOutcome::Repository(RepositoryOutcome::default())),
            }
        })
        .await;

        assert_eq!((report.failures(), report.failed_projects), (22, 22));
        assert_eq!(
            report.failure_counts(),
            [
                ("slack", "slack_canvas_failed", 1),
                ("membership", "slack_name_taken", 21)
            ]
        );
        let message = render_report(&report);
        assert!(
            message.starts_with(
                ":rotating_light: *Nightly Project Update DEGRADED* — 22 failures across 22 of 22 Projects\n• slack × 1 — slack_canvas_failed: "
            ),
            "{message}"
        );
        assert!(
            message.contains("\n• membership × 21 — slack_name_taken: Slack channel name is taken"),
            "{message}"
        );
        assert!(!message.contains("complete"));
    }

    #[test]
    fn the_update_lists_five_causes_and_summarizes_the_rest() {
        let mut report = NightlyProjectUpdateReport::default();
        for n in 0..7 {
            report.fail(Tag::Repository, &TerminalError::new(format!("forge {n}")));
        }
        assert_eq!(report.failure_causes.len(), 1, "one bounded cause");
        report.failure_causes = (0..7)
            .map(|n| FailureCause {
                tag: "repository".into(),
                reason: format!("cause_{n}"),
                count: 1,
                example: "forge down".into(),
            })
            .collect();
        let message = render_report(&report);
        assert_eq!(message.matches("\n• repository × 1").count(), 5);
        assert!(message.contains("\n• …and 2 more causes"), "{message}");
    }

    #[test]
    fn the_dash0_check_rules_read_the_metrics_the_worker_exports() {
        let rules: serde_yaml::Value = serde_yaml::from_str(include_str!(
            "../../examples/deploy/k8s/observability/dash0-check-rules.yaml"
        ))
        .unwrap();
        let groups = rules["spec"]["groups"].as_sequence().unwrap();
        let expr = |alert: &str| {
            groups
                .iter()
                .flat_map(|group| group["rules"].as_sequence().unwrap())
                .find(|rule| rule["alert"].as_str() == Some(alert))
                .and_then(|rule| rule["expr"].as_str())
                .unwrap_or_else(|| panic!("{alert} is declared"))
                .to_string()
        };
        assert!(
            expr("NightlyProjectUpdateChildrenFailed").contains(&format!(
                "otel_metric_name=\"{}\"",
                telemetry::NIGHTLY_PROJECT_UPDATE_CHILD_FAILED
            ))
        );
        assert!(expr("NightlyProjectUpdateMissed").contains(&format!(
            "otel_metric_name=\"{}\"",
            telemetry::NIGHTLY_PROJECT_UPDATE_RUN
        )));
        assert!(
            groups
                .iter()
                .all(|group| group["rules"].as_sequence().unwrap().len() == 1),
            "one rule per group, so each loads as a Terraform dash0_check_rule"
        );
    }

    #[test]
    fn failure_reasons_are_bounded_codes() {
        for (message, reason) in [
            ("Slack channel name is taken by a channel this bot cannot see", "slack_name_taken"),
            (
                "Slack API rejected conversations.list: missing_scope (needed: groups:read; provided: chat:write)",
                "slack_missing_scope",
            ),
            ("Slack API rejected the request: channel_not_found", "slack_channel_not_found"),
            ("Slack API rejected the request: not_in_channel", "slack_channel_not_found"),
            ("Slack API rate limited the request", "slack_rate_limited"),
            ("Slack rejected the Nightly Project Update with HTTP 500", "slack_other"),
            ("forge down", "other"),
        ] {
            assert_eq!(failure_reason(message), reason, "{message}");
        }
    }

    #[test]
    fn an_empty_sweep_still_reports_a_completed_update() {
        assert!(
            render_report(&NightlyProjectUpdateReport::default()).contains("Channels checked: 0")
        );
    }

    #[test]
    fn slack_errors_preserve_retryability_and_hide_transport_details() {
        for error in [
            SlackBotError::Api("not_in_channel".to_string()),
            SlackBotError::HttpStatus(403),
            SlackBotError::Transport("contains private request detail".to_string()),
            SlackBotError::RetryableHttpStatus(503),
            SlackBotError::RateLimited {
                retry_after_seconds: Some(20),
            },
            SlackBotError::IncompleteResponse,
        ] {
            let rendered = format!("{:?}", slack_error(error));
            assert!(!rendered.contains("private request detail"));
            assert!(!rendered.contains("private@example.com"));
        }
    }

    #[test]
    fn a_missing_scope_reaches_the_log_with_the_scope_slack_needed() {
        let rendered = format!(
            "{:?}",
            slack_error(SlackBotError::MissingScope {
                method: "conversations.members".to_string(),
                needed: "groups:read".to_string(),
                provided: "chat:write".to_string(),
            })
        );
        assert!(rendered.contains("conversations.members"), "{rendered}");
        assert!(rendered.contains("needed: groups:read"), "{rendered}");
        assert!(rendered.contains("provided: chat:write"), "{rendered}");
    }

    #[test]
    fn the_report_is_the_only_journal_payload_and_carries_no_directory_data() {
        let json = serde_json::to_string(&NightlyProjectUpdateReport::default()).unwrap();
        assert!(!json.contains('@'));
        let roster = serde_json::to_string(&entry(true)).unwrap();
        assert!(roster.contains("project_id") && roster.contains("open"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&roster)
                .unwrap()
                .as_object()
                .unwrap()
                .len(),
            2,
            "the roster journals an id and a flag, nothing else"
        );
    }
}
