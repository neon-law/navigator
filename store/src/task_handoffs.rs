//! Reviewed handoff notes use the existing Task's lifecycle and audit trail.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::access::MatterViewer;
use crate::application_store::ApplicationStore;
use crate::postgres::PostgresDb;
use crate::tasks::{self, NewProjectTask, ProjectTask, ProjectTaskError};

#[derive(Debug, Serialize, Deserialize)]
pub struct Handoff {
    pub responsible_role: String,
    pub retry_on_release: Option<String>,
    pub communication_id: Option<Uuid>,
    pub deadline_id: Option<Uuid>,
    pub issue: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SourceLink {
    pub label: String,
    pub href: String,
}

#[derive(Debug, Serialize)]
pub struct PendingHandoff {
    pub title: String,
    pub body: Option<String>,
    pub author: Option<String>,
    pub updated_at: String,
    pub state: &'static str,
    pub responsible_role: String,
    pub due_on: Option<String>,
    pub retry_on_release: Option<String>,
    pub sources: Vec<SourceLink>,
}

pub async fn create(
    application_store: &ApplicationStore,
    pg: &PostgresDb,
    project_id: Uuid,
    actor_id: Uuid,
    task: &NewProjectTask,
    handoff: &Handoff,
) -> Result<ProjectTask, ProjectTaskError> {
    validate(handoff)?;
    if let Some(id) = handoff.communication_id {
        if crate::communications::find(application_store, project_id, id)
            .await
            .map_or(true, |row| {
                row.is_none_or(|row| {
                    row.direction != "internal"
                        || row.channel != crate::communications::channel::NOTE
                })
            })
        {
            return Err(ProjectTaskError::Invalid("internal Communication source"));
        }
    }
    if let Some(id) = handoff.deadline_id {
        if !matches!(
            crate::project_deadlines::find(application_store, project_id, id).await,
            Ok(Some(_))
        ) {
            return Err(ProjectTaskError::Invalid("Deadline source"));
        }
    }
    tasks::create_task(pg, project_id, actor_id, task, Some(handoff)).await
}

fn validate(handoff: &Handoff) -> Result<(), ProjectTaskError> {
    if !matches!(
        handoff.responsible_role.as_str(),
        "owner" | "admin" | "lawyer" | "clerk"
    ) {
        return Err(ProjectTaskError::Invalid("responsible role"));
    }
    if handoff
        .retry_on_release
        .as_deref()
        .is_some_and(|release| release_date(release).is_none())
    {
        return Err(ProjectTaskError::Invalid("release must be YY.M.D"));
    }
    if handoff.issue.as_deref().is_some_and(|issue| {
        !issue.split_once('-').is_some_and(|(team, number)| {
            (2..=10).contains(&team.len())
                && team.bytes().all(|ch| ch.is_ascii_uppercase())
                && (1..=10).contains(&number.len())
                && number.bytes().all(|ch| ch.is_ascii_digit())
        })
    }) {
        return Err(ProjectTaskError::Invalid("engineering issue identifier"));
    }
    Ok(())
}

fn release_date(release: &str) -> Option<NaiveDate> {
    let mut parts = release.split('.');
    let year = parts.next()?;
    if year.len() != 2 || !year.bytes().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let year: i32 = year.parse().ok()?;
    let date = NaiveDate::from_ymd_opt(
        2000 + year,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    )?;
    parts.next().is_none().then_some(date)
}

fn state(task: &str, target: Option<&str>, installed: &str) -> &'static str {
    if task == "done" {
        return "resolved";
    }
    let Some(target) = target.and_then(release_date) else {
        return "pending";
    };
    let (base, suffix) = installed.split_once('-').unwrap_or((installed, ""));
    if release_date(base).is_some_and(|date| {
        date > target || (date == target && (suffix.is_empty() || suffix.starts_with("hotfix.")))
    }) {
        "ready_to_retry"
    } else {
        "pending"
    }
}

/// Read the same pending context for the workbench and the signed-in agent.
/// The caller resolves the viewer for this Project before opening the store.
pub async fn for_project(
    pg: Option<&PostgresDb>,
    viewer: MatterViewer,
    project_id: Uuid,
    code: &str,
    include_resolved: bool,
) -> Result<Vec<PendingHandoff>, ProjectTaskError> {
    let Some(pg) = pg.filter(|_| viewer.is_firm_side()) else {
        return Ok(Vec::new());
    };
    let installed = std::env::var("NAVIGATOR_RELEASE_TAG").unwrap_or_default();
    let rows = sqlx::query(
        "SELECT t.id,t.title,t.body,t.state,t.due_on,t.updated_at,t.handoff,p.name AS author
         FROM project_task t
         JOIN project_task_events e ON e.project_task_id=t.id AND e.event_kind='created'
         JOIN person p ON p.id=e.actor_person_id
         WHERE t.project_id=$1 AND t.handoff IS NOT NULL AND ($2 OR t.state <> 'done')
         ORDER BY t.updated_at DESC,t.id",
    )
    .bind(project_id)
    .bind(include_resolved)
    .fetch_all(pg.pool())
    .await?;
    let root = format!("/app/projects/{code}");
    rows.iter()
        .map(|row| {
            let sqlx::types::Json(handoff) =
                row.try_get::<sqlx::types::Json<Handoff>, _>("handoff")?;
            let id: Uuid = row.try_get("id")?;
            let mut sources = vec![SourceLink {
                label: "Task and status".into(),
                href: format!("{root}/tasks#task-{id}"),
            }];
            if let Some(id) = handoff.communication_id {
                sources.push(SourceLink {
                    label: "Internal Communication".into(),
                    href: format!("{root}/communications#communication-{id}"),
                });
            }
            if let Some(id) = handoff.deadline_id {
                sources.push(SourceLink {
                    label: "Deadline".into(),
                    href: format!("{root}/deadlines#deadline-{id}"),
                });
            }
            if let Some(issue) = handoff.issue {
                sources.push(SourceLink {
                    href: format!("https://linear.app/neon-law/issue/{issue}"),
                    label: issue,
                });
            }
            let task: String = row.try_get("state")?;
            Ok(PendingHandoff {
                title: row.try_get("title")?,
                body: row.try_get("body")?,
                author: row.try_get("author")?,
                updated_at: row
                    .try_get::<chrono::DateTime<chrono::Utc>, _>("updated_at")?
                    .to_rfc3339(),
                state: state(&task, handoff.retry_on_release.as_deref(), &installed),
                responsible_role: handoff.responsible_role,
                due_on: row
                    .try_get::<Option<NaiveDate>, _>("due_on")?
                    .map(|date| date.to_string()),
                retry_on_release: handoff.retry_on_release,
                sources,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::state;

    #[test]
    fn only_an_installed_release_meeting_the_trigger_makes_a_handoff_ready() {
        assert_eq!(state("todo", Some("26.11.17"), ""), "pending");
        assert_eq!(state("todo", Some("26.11.17"), "26.11.17-rc.1"), "pending");
        assert_eq!(
            state("todo", Some("26.11.17"), "26.11.17"),
            "ready_to_retry"
        );
        assert_eq!(
            state("todo", Some("26.11.17"), "26.11.18"),
            "ready_to_retry"
        );
        assert_eq!(state("done", Some("26.11.17"), "26.11.18"), "resolved");
    }
}
