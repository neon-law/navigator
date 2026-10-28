//! The `slack` tag: one open Project's internal channel canvas and the
//! daily notice with repository activity and open stored deadlines.
//!
//! The canvas carries the synopsis, the Client DRI directory emails, the Lawyer
//! DRIs, the portal link, and the Project's recorded handles. It is the
//! channel's one canvas, edited in place on every run. Both write only to the
//! Project's internal channel; the customer channel is never read or written, and the directory
//! emails stay inside the journaled step that renders the canvas.

use std::fmt::Write;
use std::sync::Arc;

use cloud::forge::ForgeService;
use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use store::projects::ProjectCanvas;
use store::surreal::SurrealDb;
use uuid::Uuid;
use workflows::{upsert_channel_canvas, CanvasWrite, SlackBot};

use super::{slack_error, tag_retry_policy, ProjectTagRequest};

/// What one Project's `slack` child did. Counts only.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SlackOutcome {
    pub canvases_created: usize,
    pub canvases_edited: usize,
    pub notices_posted: usize,
    #[serde(default)]
    pub canvas_failures: usize,
}

/// The journaled result of the canvas step: what was done, never what it said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum CanvasStep {
    /// A closed Project, or one with no internal channel yet.
    Skipped,
    Created,
    Edited,
}

#[derive(Clone)]
pub struct SlackTagService {
    surreal: SurrealDb,
    slack: Arc<dyn SlackBot>,
    portal_origin: Option<String>,
    forge: Option<Arc<dyn ForgeService>>,
}

impl SlackTagService {
    /// `portal_origin` is the deployment's public origin, such as
    /// `https://www.example.com`; without it the canvas links the portal by
    /// its path.
    #[must_use]
    pub fn new(
        surreal: SurrealDb,
        slack: Arc<dyn SlackBot>,
        portal_origin: Option<String>,
        forge: Option<Arc<dyn ForgeService>>,
    ) -> Self {
        Self {
            surreal,
            slack,
            portal_origin,
            forge,
        }
    }
}

#[restate_sdk::workflow(name = "NightlyProjectUpdateSlack")]
impl SlackTagService {
    #[restate_sdk::handler]
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        request: Json<ProjectTagRequest>,
    ) -> Result<Json<SlackOutcome>, HandlerError> {
        let ProjectTagRequest { ran_on, project_id } = request.into_inner();

        let (surreal, slack) = (self.surreal.clone(), Arc::clone(&self.slack));
        let origin = self.portal_origin.clone();
        let day = ran_on.clone();
        // The directory emails stay inside this journaled operation.
        let canvas = ctx
            .run(move || async move {
                upsert_canvas(
                    &surreal,
                    slack.as_ref(),
                    origin.as_deref(),
                    &day,
                    project_id,
                )
                .await
                .map(Json)
            })
            .name("upsert-canvas")
            .retry_policy(tag_retry_policy())
            .await;

        let (surreal, slack) = (self.surreal.clone(), Arc::clone(&self.slack));
        let forge = self.forge.clone();
        let posted = ctx
            .run(move || async move {
                post_notice(
                    &surreal,
                    slack.as_ref(),
                    forge.as_deref(),
                    &ran_on,
                    project_id,
                )
                .await
                .map(Json)
            })
            .name("post-notice")
            .retry_policy(tag_retry_policy())
            .await?
            .into_inner();
        Ok(Json(canvas_outcome(
            &canvas.map(Json::into_inner).map_err(HandlerError::from),
            posted,
        )))
    }
}

fn outcome(canvas: CanvasStep, notice_posted: bool) -> SlackOutcome {
    SlackOutcome {
        canvases_created: usize::from(canvas == CanvasStep::Created),
        canvases_edited: usize::from(canvas == CanvasStep::Edited),
        notices_posted: usize::from(notice_posted),
        canvas_failures: 0,
    }
}

fn canvas_outcome(canvas: &Result<CanvasStep, HandlerError>, posted: bool) -> SlackOutcome {
    match canvas {
        Ok(canvas) => outcome(*canvas, posted),
        Err(_) => SlackOutcome {
            notices_posted: usize::from(posted),
            canvas_failures: 1,
            ..Default::default()
        },
    }
}

/// Upsert one open Project's internal channel canvas.
pub async fn upsert_canvas(
    surreal: &SurrealDb,
    slack: &dyn SlackBot,
    portal_origin: Option<&str>,
    ran_on: &str,
    project_id: Uuid,
) -> Result<CanvasStep, HandlerError> {
    let Some(canvas) = store::projects::project_canvas(surreal, project_id)
        .await
        .map_err(HandlerError::from)?
    else {
        return Ok(CanvasStep::Skipped);
    };
    let (true, Some(channel_id)) = (
        canvas.status == "open",
        canvas.internal_channel_id.as_deref(),
    ) else {
        return Ok(CanvasStep::Skipped);
    };
    let markdown = render_canvas(&canvas, portal_origin, ran_on);
    match upsert_channel_canvas(slack, channel_id, &markdown)
        .await
        .map_err(slack_error)?
    {
        CanvasWrite::Created(_) => Ok(CanvasStep::Created),
        CanvasWrite::Edited(_) => Ok(CanvasStep::Edited),
    }
}

/// The mechanism-only notice: no Project, client, matter, email, or mention.
pub(crate) fn notice(ran_on: &str) -> String {
    format!("The Nightly Project Update ran on {ran_on}.")
}

/// Post stored deadlines and the latest default-branch commit date to one
/// open Project's internal channel. Reads and content remain inside this step.
/// `false` when the
/// Project is closed or has no internal channel.
pub async fn post_notice(
    surreal: &SurrealDb,
    slack: &dyn SlackBot,
    forge: Option<&dyn ForgeService>,
    ran_on: &str,
    project_id: Uuid,
) -> Result<bool, HandlerError> {
    let Some(project) = store::projects::find_by_id(surreal, project_id)
        .await
        .map_err(HandlerError::from)?
    else {
        return Ok(false);
    };
    let (true, Some(channel_id)) = (
        project.status == "open",
        project.internal_slack_channel_id.as_deref(),
    ) else {
        return Ok(false);
    };
    let mut text = notice(ran_on);
    if let Some(url) = project.repository_url.as_deref() {
        let github = match forge {
            Some(forge) => match forge.find_repository(&project.code).await {
                Ok(Some(repository)) if repository.url == url => {
                    match forge.head_commit_committed_at(&project.code).await {
                        Ok(Some(date)) => format!("Latest default-branch commit: {date}."),
                        Ok(None) => "No default-branch commit date available.".into(),
                        Err(_) => "GitHub activity unavailable; retry on the next update.".into(),
                    }
                }
                _ => "GitHub activity unavailable for the recorded repository.".into(),
            },
            None => "GitHub activity unavailable: no forge provider configured.".into(),
        };
        let _ = write!(
            text,
            "\n{github} <{}/commits|View commits>",
            url.trim_end_matches('/')
        );
    }
    let deadlines = store::project_deadlines::list(surreal, project_id)
        .await
        .map_err(HandlerError::from)?;
    let open: Vec<_> = deadlines
        .iter()
        .filter(|row| row.status == store::project_deadlines::STATUS_OPEN)
        .collect();
    if !open.is_empty() {
        let _ = write!(text, "\nOpen deadlines: {}.", open.len());
    }
    // Keep a daily post bounded even for a heavily docketed Project.
    for deadline in open.iter().take(20) {
        let _ = write!(
            text,
            "\nDeadline ({}): {} — {}: {}",
            deadline.calendar_role,
            deadline.due_at,
            deadline.authority_kind,
            slack_plain_text(&deadline.authority.chars().take(300).collect::<String>())
        );
    }
    if open.len() > 20 {
        text.push_str("\nShowing the first 20 open deadlines; use navigator project deadline list for all records.");
    }
    let correlation = format!("nightly-project-update-{ran_on}-{project_id}");
    if slack
        .message_receipt_by_correlation(channel_id, &correlation)
        .await
        .map_err(slack_error)?
        .is_none()
    {
        slack
            .post_message_with_receipt(channel_id, &text, Some(&correlation))
            .await
            .map_err(slack_error)?;
    }
    Ok(true)
}

// Escape Slack control characters so a citation cannot mention a channel or person.
fn slack_plain_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Test composition using no forge client.
#[cfg(test)]
pub(crate) async fn reconcile_slack(
    surreal: &SurrealDb,
    slack: &dyn SlackBot,
    portal_origin: Option<&str>,
    ran_on: &str,
    project_id: Uuid,
) -> Result<SlackOutcome, HandlerError> {
    let canvas = upsert_canvas(surreal, slack, portal_origin, ran_on, project_id).await;
    let posted = post_notice(surreal, slack, None, ran_on, project_id).await?;
    Ok(canvas_outcome(&canvas, posted))
}

/// The matter show page, which renders the Project's declared Matter Pages.
fn matter_link(origin: Option<&str>, code: &str) -> String {
    let path = format!("/app/projects/{code}");
    match origin {
        Some(origin) => format!("{}{path}", origin.trim_end_matches('/')),
        None => path,
    }
}

/// The canvas body. The same Project on the same day renders the same bytes.
pub(crate) fn render_canvas(
    canvas: &ProjectCanvas,
    portal_origin: Option<&str>,
    ran_on: &str,
) -> String {
    let synopsis = match canvas.description.as_deref().map(str::trim) {
        Some(description) if !description.is_empty() => description.to_string(),
        _ => format!("{} ({}), {}", canvas.name, canvas.code, canvas.status),
    };
    let client_dris = if canvas.client_dri_emails.is_empty() {
        "none designated".to_string()
    } else {
        canvas.client_dri_emails.join(", ")
    };
    let lawyer_dris = if canvas.lawyer_dris.is_empty() {
        "none designated".to_string()
    } else {
        canvas
            .lawyer_dris
            .iter()
            .map(|dri| format!("{} ({})", dri.name, dri.email))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut lines = vec![
        format!("# {}", canvas.name),
        String::new(),
        synopsis,
        String::new(),
        "## Handles".to_string(),
        format!("- Code: `{}`", canvas.code),
        format!("- Status: {}", canvas.status),
    ];
    if let Some(repository) = &canvas.repository_url {
        lines.push(format!("- Repository: {repository}"));
    }
    lines.push(format!(
        "- Matter: {}",
        matter_link(portal_origin, &canvas.code)
    ));
    lines.extend([
        String::new(),
        "## Directly responsible individuals".to_string(),
        format!("- Client DRI: {client_dris}"),
        format!("- Lawyer DRI: {lawyer_dris}"),
        String::new(),
        format!("Refreshed by the Nightly Project Update on {ran_on}."),
    ]);
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use store::persons::{self, NewPerson, Role};
    use store::projects::{self, create, designate_dri_in_surreal, DriSide, NewProject};
    use store::test_support::{mem_surreal, seed_entity};
    use workflows::{CapturingSlackBot, SlackBotError};

    const INTERNAL: &str = "CINTERNAL1";
    const EXTERNAL: &str = "CEXTERNAL1";

    #[tokio::test]
    async fn daily_notice_includes_github_and_only_open_stored_deadlines() {
        use cloud::forge::{FakeForge, ForgeService};
        use store::project_deadlines::{self, NewDeadline};

        let surreal = mem_surreal().await;
        let project = seeded(&surreal, "sample-update", "open").await;
        let forge = FakeForge::new();
        let repository = forge.ensure_repository(&project.code).await.unwrap();
        projects::update_project(
            &surreal,
            project.id,
            &projects::UpdateProjectCommand {
                repository_url: Some(repository.url.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        forge.set_head_commit_committed_at(&project.code, "2026-10-05T12:00:00Z");
        let input = NewDeadline {
            title: None,
            description: None,
            project_id: project.id,
            due_at: "2026-10-09T17:00:00-07:00".into(),
            authority_kind: "court_rule".into(),
            authority: "Synthetic Rule 12".into(),
            source: "lawyer".into(),
            source_document_id: None,
            owner_person_id: None,
            calendar_role: "firm".into(),
            internal_lead_days: Some(7),
            client_lead_days: None,
            replay_key: None,
        };
        let open = project_deadlines::create(&surreal, &input).await.unwrap();
        let completed = project_deadlines::create(
            &surreal,
            &NewDeadline {
                authority: "Completed synthetic date".into(),
                ..input.clone()
            },
        )
        .await
        .unwrap();
        project_deadlines::complete(&surreal, project.id, completed.id)
            .await
            .unwrap();
        let cancelled = project_deadlines::create(
            &surreal,
            &NewDeadline {
                authority: "Cancelled synthetic date".into(),
                ..input
            },
        )
        .await
        .unwrap();
        project_deadlines::cancel(&surreal, project.id, cancelled.id)
            .await
            .unwrap();
        let slack = CapturingSlackBot::new();
        post_notice(&surreal, &slack, Some(&forge), "2026-10-06", project.id)
            .await
            .unwrap();
        let posted = slack.posted_messages();
        assert_eq!(posted[0].0, INTERNAL);
        let text = &posted[0].1;
        assert!(text.contains("2026-10-05T12:00:00Z"));
        assert!(text.contains(&format!("{}/commits", repository.url)));
        assert!(text.contains(&open.due_at));
        assert!(text.contains("Synthetic Rule 12"));
        assert!(!text.contains("Completed synthetic date"));
        assert!(!text.contains("Cancelled synthetic date"));
        assert!(!text.contains(EXTERNAL));
    }

    #[tokio::test]
    async fn an_unconfigured_forge_is_visible_in_the_internal_update() {
        let surreal = mem_surreal().await;
        let project = seeded(&surreal, "sample-no-forge", "open").await;
        projects::update_project(
            &surreal,
            project.id,
            &projects::UpdateProjectCommand {
                repository_url: Some("https://forge.example/example/sample-no-forge".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let slack = CapturingSlackBot::new();
        post_notice(&surreal, &slack, None, "2026-10-06", project.id)
            .await
            .unwrap();
        assert!(slack.posted_messages()[0]
            .1
            .contains("no forge provider configured"));
    }

    #[tokio::test]
    async fn retry_after_notice_acceptance_uses_the_same_daily_receipt() {
        let surreal = mem_surreal().await;
        let project = seeded(&surreal, "sample-notice-receipt", "open").await;
        let slack = CapturingSlackBot::new();
        post_notice(&surreal, &slack, None, "2026-10-06", project.id)
            .await
            .unwrap();
        post_notice(&surreal, &slack, None, "2026-10-06", project.id)
            .await
            .unwrap();
        assert_eq!(slack.posted_messages().len(), 1);
    }

    #[tokio::test]
    async fn a_recorded_repository_on_another_forge_is_not_replaced_with_provider_activity() {
        use cloud::forge::{FakeForge, ForgeService};
        let surreal = mem_surreal().await;
        let project = seeded(&surreal, "sample-other-forge", "open").await;
        let forge = FakeForge::new();
        forge.ensure_repository(&project.code).await.unwrap();
        forge.set_head_commit_committed_at(&project.code, "2026-10-05T12:00:00Z");
        projects::update_project(
            &surreal,
            project.id,
            &projects::UpdateProjectCommand {
                repository_url: Some("https://other.example/example/sample-other-forge".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let slack = CapturingSlackBot::new();
        post_notice(&surreal, &slack, Some(&forge), "2026-10-06", project.id)
            .await
            .unwrap();
        let text = &slack.posted_messages()[0].1;
        assert!(text.contains("unavailable for the recorded repository"));
        assert!(!text.contains("2026-10-05T12:00:00Z"));
    }

    #[tokio::test]
    async fn an_unknown_project_gets_neither_canvas_nor_notice() {
        let surreal = mem_surreal().await;
        let slack = CapturingSlackBot::new();
        let missing = uuid::Uuid::now_v7();
        assert!(matches!(
            upsert_canvas(&surreal, &slack, None, "2026-10-06", missing)
                .await
                .unwrap(),
            CanvasStep::Skipped
        ));
        assert!(!post_notice(&surreal, &slack, None, "2026-10-06", missing)
            .await
            .unwrap());
        assert!(slack.posted_messages().is_empty());
        assert!(slack.canvas_writes().is_empty());
    }

    #[tokio::test]
    async fn a_repository_without_a_commit_date_says_so() {
        use cloud::forge::{FakeForge, ForgeService};
        let surreal = mem_surreal().await;
        let project = seeded(&surreal, "sample-empty-repository", "open").await;
        let forge = FakeForge::new();
        let repository = forge.ensure_repository(&project.code).await.unwrap();
        projects::update_project(
            &surreal,
            project.id,
            &projects::UpdateProjectCommand {
                repository_url: Some(repository.url.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let slack = CapturingSlackBot::new();
        post_notice(&surreal, &slack, Some(&forge), "2026-10-06", project.id)
            .await
            .unwrap();
        let text = &slack.posted_messages()[0].1;
        assert!(
            text.contains("No default-branch commit date available."),
            "{text}"
        );
    }

    #[test]
    fn citations_cannot_inject_slack_mentions() {
        assert_eq!(
            slack_plain_text("Rule <@U123> & <!channel>"),
            "Rule &lt;@U123&gt; &amp; &lt;!channel&gt;"
        );
    }

    /// A Project with both DRI sides, an internal channel, and a customer
    /// channel recorded on its other column.
    async fn seeded(surreal: &SurrealDb, code: &str, status: &str) -> projects::Project {
        let project = create(
            surreal,
            &NewProject {
                code: code.to_string(),
                name: "Cruller v. Prine".to_string(),
                status: status.to_string(),
                description: Some("A synthetic litigation matter.".to_string()),
                entity_id: seed_entity(surreal).await,
                ..Default::default()
            },
        )
        .await
        .expect("create Project");
        projects::set_internal_slack_channel_id(surreal, project.id, INTERNAL)
            .await
            .expect("set internal channel")
            .expect("Project exists");
        let external = format!("https://example.slack.com/archives/{EXTERNAL}");
        projects::update_project(
            surreal,
            project.id,
            &projects::UpdateProjectCommand {
                external_slack_channel_url: Some(external),
                ..Default::default()
            },
        )
        .await
        .expect("record the customer channel");
        for (name, email, role, participation, side) in [
            (
                "Lawyer DRI",
                "lawyer@example.com",
                Role::Lawyer,
                "lawyer",
                DriSide::Lawyer,
            ),
            (
                "Client DRI",
                "client@example.com",
                Role::Client,
                "client",
                DriSide::Client,
            ),
        ] {
            let person = persons::create(surreal, &NewPerson::with_role(name, email, role))
                .await
                .expect("create Person");
            projects::add_participation(surreal, project.id, person.id, participation)
                .await
                .expect("add participation");
            designate_dri_in_surreal(surreal, project.id, person.id, side)
                .await
                .expect("designate DRI");
        }
        project
    }

    #[tokio::test]
    async fn the_canvas_holds_the_synopsis_dris_portal_and_handles() {
        let surreal = mem_surreal().await;
        let project = seeded(&surreal, "sample-litigation", "open").await;
        let slack = CapturingSlackBot::new();

        let outcome = reconcile_slack(&surreal, &slack, None, "2026-10-05", project.id)
            .await
            .expect("canvas");
        assert_eq!(outcome.canvases_created, 1);
        let body = slack.canvas_markdown(INTERNAL).expect("canvas written");
        for expected in [
            "A synthetic litigation matter.",
            "client@example.com",
            "Lawyer DRI (lawyer@example.com)",
            "Matter: /app/projects/sample-litigation\n",
            "`sample-litigation`",
            "Status: open",
            "2026-10-05",
        ] {
            assert!(
                body.contains(expected),
                "{expected:?} missing from:\n{body}"
            );
        }
    }

    #[tokio::test]
    async fn a_second_run_edits_the_same_canvas() {
        let surreal = mem_surreal().await;
        let project = seeded(&surreal, "sample-litigation", "open").await;
        let slack = CapturingSlackBot::new();

        let first = reconcile_slack(&surreal, &slack, None, "2026-10-05", project.id)
            .await
            .expect("first");
        let second = reconcile_slack(&surreal, &slack, None, "2026-10-06", project.id)
            .await
            .expect("second");
        assert_eq!((first.canvases_created, first.canvases_edited), (1, 0));
        assert_eq!((second.canvases_created, second.canvases_edited), (0, 1));
        assert_eq!(slack.canvas_channels(), [INTERNAL], "one canvas, not two");
        assert!(slack
            .canvas_markdown(INTERNAL)
            .unwrap()
            .contains("2026-10-06"));
    }

    #[tokio::test]
    async fn a_description_less_project_is_synopsized_by_name_code_and_status() {
        let canvas = ProjectCanvas {
            code: "sample-estate".into(),
            name: "Estate of Cornelius Montgomery".into(),
            status: "open".into(),
            description: Some("  ".into()),
            repository_url: Some("https://github.com/example/sample-estate".into()),
            internal_channel_id: Some("C1".into()),
            client_dri_emails: Vec::new(),
            lawyer_dris: Vec::new(),
        };
        let body = render_canvas(&canvas, Some("https://www.example.com/"), "2026-10-05");
        assert!(body.contains("Estate of Cornelius Montgomery (sample-estate), open"));
        assert!(body.contains("Repository: https://github.com/example/sample-estate"));
        assert!(body.contains("Matter: https://www.example.com/app/projects/sample-estate\n"));
        assert!(
            !body.contains("/portal"),
            "the retired Portal is never linked"
        );
        assert!(body.contains("Client DRI: none designated"));
        assert_eq!(
            body,
            render_canvas(&canvas, Some("https://www.example.com"), "2026-10-05")
        );
    }

    #[tokio::test]
    async fn the_customer_channel_is_never_passed_to_slack() {
        let surreal = mem_surreal().await;
        let project = seeded(&surreal, "sample-litigation", "open").await;
        let slack = CapturingSlackBot::new();
        reconcile_slack(&surreal, &slack, None, "2026-10-05", project.id)
            .await
            .expect("canvas and notice");

        let touched: Vec<String> = slack
            .canvas_channels()
            .into_iter()
            .chain(
                slack
                    .posted_messages()
                    .into_iter()
                    .map(|(channel, _)| channel),
            )
            .collect();
        assert!(!touched.is_empty());
        assert!(
            touched.iter().all(|channel| channel == INTERNAL),
            "only the internal channel is written: {touched:?}"
        );
        assert!(!touched.iter().any(|channel| channel == EXTERNAL));
        let canvas = projects::project_canvas(&surreal, project.id)
            .await
            .unwrap()
            .unwrap();
        assert!(!format!("{canvas:?}").contains(EXTERNAL));
    }

    #[tokio::test]
    async fn the_notice_is_mechanism_only() {
        let surreal = mem_surreal().await;
        let project = seeded(&surreal, "sample-litigation", "open").await;
        let slack = CapturingSlackBot::new();
        let outcome = reconcile_slack(&surreal, &slack, None, "2026-10-05", project.id)
            .await
            .expect("canvas and notice");

        assert_eq!(outcome.notices_posted, 1);
        let posted = slack.posted_messages();
        assert_eq!(posted.len(), 1);
        let (channel, text) = &posted[0];
        assert_eq!(channel, INTERNAL);
        assert_eq!(text, "The Nightly Project Update ran on 2026-10-05.");
        for forbidden in [
            "@",
            "example.com",
            "Cruller",
            "Prine",
            "sample-litigation",
            "synthetic",
            "Lawyer DRI",
        ] {
            assert!(!text.contains(forbidden), "{forbidden:?} in {text:?}");
        }
    }

    #[tokio::test]
    async fn directory_emails_reach_the_canvas_but_not_the_journal_value() {
        let surreal = mem_surreal().await;
        let project = seeded(&surreal, "sample-litigation", "open").await;
        let slack = CapturingSlackBot::new();

        let step = upsert_canvas(&surreal, &slack, None, "2026-10-05", project.id)
            .await
            .expect("canvas step");
        let journal_value = serde_json::to_string(&step).unwrap();
        assert!(slack
            .canvas_markdown(INTERNAL)
            .unwrap()
            .contains("client@example.com"));
        assert!(!journal_value.contains('@') && !journal_value.contains("example.com"));

        let outcome = reconcile_slack(&surreal, &slack, None, "2026-10-05", project.id)
            .await
            .unwrap();
        assert!(!serde_json::to_string(&outcome).unwrap().contains('@'));
        let report = crate::nightly_project_update::render_report(
            &crate::nightly_project_update::NightlyProjectUpdateReport::default(),
        );
        assert!(!report.contains('@'));
    }

    #[tokio::test]
    async fn a_closed_project_is_given_no_canvas_and_no_notice() {
        let surreal = mem_surreal().await;
        let project = seeded(&surreal, "sample-closed", "closed").await;
        let slack = CapturingSlackBot::new();
        let outcome = reconcile_slack(&surreal, &slack, None, "2026-10-05", project.id)
            .await
            .expect("skipped");
        assert_eq!(outcome, SlackOutcome::default());
        assert!(slack.canvas_writes().is_empty());
        assert!(slack.posted_messages().is_empty());
    }

    #[tokio::test]
    async fn a_project_without_an_internal_channel_is_skipped() {
        let surreal = mem_surreal().await;
        let project = create(
            &surreal,
            &NewProject {
                code: "sample-unchanneled".to_string(),
                name: "Sample".to_string(),
                status: "open".to_string(),
                entity_id: seed_entity(&surreal).await,
                ..Default::default()
            },
        )
        .await
        .expect("create Project");
        let slack = CapturingSlackBot::new();
        let step = upsert_canvas(&surreal, &slack, None, "2026-10-05", project.id)
            .await
            .expect("skipped");
        assert_eq!(step, CanvasStep::Skipped);
        assert!(slack.canvas_writes().is_empty());
    }

    #[tokio::test]
    async fn a_canvas_failure_still_posts_the_update_and_is_counted_by_the_sweep() {
        let surreal = mem_surreal().await;
        let failing = seeded(&surreal, "sample-failing", "open").await;
        let capture = Arc::new(CapturingSlackBot::new().failing_canvases("missing_scope"));
        let slack: Arc<dyn SlackBot> = capture.clone();
        let outcome = reconcile_slack(&surreal, slack.as_ref(), None, "2026-10-05", failing.id)
            .await
            .expect("the daily notice succeeds independently");
        assert_eq!(outcome.canvas_failures, 1);
        assert_eq!(outcome.notices_posted, 1);
        assert_eq!(capture.posted_messages().len(), 1);
        assert_eq!(capture.posted_messages()[0].0, INTERNAL);

        let report =
            crate::nightly_project_update::sweep_in_process(&surreal, &slack, None, false).await;
        assert_eq!(report.slack_failures, 1);
        assert_eq!(report.notices_posted, 1);
        assert_eq!(
            report.membership_failures, 0,
            "the other tags still ran for this Project"
        );
    }

    /// Records every channel a call names, to prove nothing else is touched.
    #[derive(Default)]
    struct Spy {
        channels: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl SlackBot for Spy {
        async fn create_private_channel(
            &self,
            _: &str,
        ) -> Result<workflows::SlackChannel, SlackBotError> {
            unreachable!("the slack tag never creates a channel")
        }
        async fn post_message(&self, channel_id: &str, _: &str) -> Result<(), SlackBotError> {
            self.channels.lock().unwrap().push(channel_id.to_string());
            Ok(())
        }
        async fn user_id_by_email(&self, _: &str) -> Result<Option<String>, SlackBotError> {
            unreachable!("the slack tag never looks anyone up")
        }
        async fn channel_member_ids(&self, _: &str) -> Result<Vec<String>, SlackBotError> {
            unreachable!("the slack tag never reads membership")
        }
        async fn invite_to_channel(&self, _: &str, _: &[String]) -> Result<(), SlackBotError> {
            unreachable!("the slack tag never invites")
        }
        async fn channel_canvas_id(
            &self,
            channel_id: &str,
        ) -> Result<Option<String>, SlackBotError> {
            self.channels.lock().unwrap().push(channel_id.to_string());
            Ok(None)
        }
        async fn create_channel_canvas(
            &self,
            channel_id: &str,
            _: &str,
        ) -> Result<String, SlackBotError> {
            self.channels.lock().unwrap().push(channel_id.to_string());
            Ok("F1".to_string())
        }
    }

    #[tokio::test]
    async fn the_slack_tag_only_ever_names_the_internal_channel() {
        let surreal = mem_surreal().await;
        let project = seeded(&surreal, "sample-litigation", "open").await;
        let spy = Spy::default();
        reconcile_slack(&surreal, &spy, None, "2026-10-05", project.id)
            .await
            .expect("canvas and notice");
        let channels = spy.channels.lock().unwrap();
        assert!(!channels.is_empty());
        assert!(channels.iter().all(|channel| channel == INTERNAL));
    }
}
