//! The `slack` tag: one open Project's internal channel canvas and the
//! daily notice with repository activity and open stored deadlines.
//!
//! Repository activity reads through the same forge the `repository` tag
//! uses — on Forgejo the Project's own Firm client — and the recorded
//! repository is linked whether or not that read succeeds.
//!
//! The canvas carries the synopsis, the Client DRI directory emails, the Lawyer
//! DRIs, the portal link, and the Project's recorded handles. It is the
//! channel's one canvas, edited in place on every run. Both write only to the
//! Project's internal channel; the customer channel is never read or written, and the directory
//! emails stay inside the journaled step that renders the canvas.

use std::fmt::Write;

use cloud::forge::ForgeService;

use serde::{Deserialize, Serialize};
use store::application_store::ApplicationStore;
use store::projects::ProjectCanvas;
use uuid::Uuid;
use workflows::{upsert_channel_canvas, CanvasWrite, SlackBot};

use super::repository::{matches_recorded_repository, RepositoryForge};
use super::slack_error;

use crate::workflow_step_error::WorkflowStepError;

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

/// Upsert one open Project's internal channel canvas.
pub async fn upsert_canvas(
    application_store: &ApplicationStore,
    slack: &dyn SlackBot,
    portal_origin: Option<&str>,
    ran_on: &str,
    project_id: Uuid,
) -> Result<CanvasStep, WorkflowStepError> {
    let Some(canvas) = store::projects::project_canvas(application_store, project_id)
        .await
        .map_err(WorkflowStepError::retryable)?
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
    application_store: &ApplicationStore,
    slack: &dyn SlackBot,
    forge: &RepositoryForge,
    ran_on: &str,
    project_id: Uuid,
) -> Result<bool, WorkflowStepError> {
    let Some(project) = store::projects::find_by_id(application_store, project_id)
        .await
        .map_err(WorkflowStepError::retryable)?
    else {
        return Ok(false);
    };
    let (true, Some(channel_id)) = (
        project.status == "open",
        project.internal_slack_channel_id.as_deref(),
    ) else {
        return Ok(false);
    };
    let mentions = super::membership::member_mentions(application_store, slack, project_id).await?;
    let mut text = notice(ran_on);
    if !mentions.is_empty() {
        // The channel is muted, so a mention is what notifies each person.
        text = format!("{mentions} {text}");
    }
    if let Some(url) = project.repository_url.as_deref() {
        let activity = repository_activity(application_store, forge, &project, url).await;
        let _ = write!(
            text,
            "\n{activity} <{}|View repository>",
            url.trim_end_matches('/')
        );
    }
    let deadlines = store::project_deadlines::list(application_store, project_id)
        .await
        .map_err(WorkflowStepError::retryable)?;
    let open: Vec<_> = deadlines
        .iter()
        .filter(|row| {
            !row.calendar_review_pending && row.status == store::project_deadlines::STATUS_OPEN
        })
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
            deadline.due_on.as_ref().map_or_else(
                || deadline.due_at.clone(),
                store::project_deadlines::DueOn::display_date
            ),
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

/// One line on the recorded repository's default branch. A forge that cannot
/// be reached degrades to a line saying so; the notice still posts.
async fn repository_activity(
    application_store: &ApplicationStore,
    forge: &RepositoryForge,
    project: &store::projects::Project,
    url: &str,
) -> String {
    let firm_forge;
    let selected = forge.for_repository(Some(url));
    let forge: &dyn ForgeService = match &selected {
        RepositoryForge::None | RepositoryForge::ByProject { .. } => {
            return "Repository activity unavailable: no forge provider for the recorded repository."
                .into()
        }
        RepositoryForge::GitHub(forge) => forge.as_ref(),
        RepositoryForge::Forgejo(firms) => match firms.for_project(application_store, project.id).await {
            Ok(forge) => {
                firm_forge = forge;
                firm_forge.as_ref()
            }
            Err(_) => {
                return "Repository activity unavailable: the Firm's forge could not be opened."
                    .into()
            }
        },
    };
    match forge.find_repository(&project.code).await {
        Ok(Some(repository)) if matches_recorded_repository(&repository.url, url) => {
            match forge.head_commit_committed_at(&project.code).await {
                Ok(Some(date)) => format!("Latest default-branch commit: {date}."),
                Ok(None) => "No default-branch commit date available.".into(),
                Err(_) => "Repository activity unavailable; retry on the next update.".into(),
            }
        }
        _ => "Repository activity unavailable for the recorded repository.".into(),
    }
}

// Escape Slack control characters so a citation cannot mention a channel or person.
pub(crate) fn slack_plain_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The matter show page, which renders the Project's declared Matter Pages.
pub(crate) fn matter_link(origin: Option<&str>, code: &str) -> String {
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

    #[test]
    fn citations_cannot_inject_slack_mentions() {
        assert_eq!(
            slack_plain_text("Rule <@U123> & <!channel>"),
            "Rule &lt;@U123&gt; &amp; &lt;!channel&gt;"
        );
    }

    #[tokio::test]
    async fn a_description_less_project_is_synopsized_by_name_code_and_status() {
        let canvas = ProjectCanvas {
            code: "sample-estate".into(),
            name: "Estate of Bigfoot".into(),
            status: "open".into(),
            description: Some("  ".into()),
            repository_url: Some("https://github.com/example/sample-estate".into()),
            internal_channel_id: Some("C1".into()),
            client_dri_emails: Vec::new(),
            lawyer_dris: Vec::new(),
        };
        let body = render_canvas(&canvas, Some("https://www.example.com/"), "2026-10-05");
        assert!(body.contains("Estate of Bigfoot (sample-estate), open"));
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
}
