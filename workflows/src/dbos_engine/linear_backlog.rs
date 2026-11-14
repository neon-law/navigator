//! Daily Linear portfolio snapshot delivered through the DBOS Slack receipt path.

use std::{collections::BTreeMap, fmt::Write as _, sync::Arc, time::Duration};

use chrono::{DateTime, Days, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::America::New_York;
use dbos::{CancellationToken, StepOptions, WorkflowRef, DBOS};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

const WORKFLOW: &str = "LinearBacklog";
const CHANNEL: &str = "SLACK_LINEAR_BACKLOG_CHANNEL_ID";
const API_KEY: &str = "LINEAR_BACKLOG_READ_KEY";
const ENDPOINT: &str = "https://api.linear.app/graphql";
const PROJECTS: &str = r#"query($after: String) {
  projects(first: 100, after: $after, filter: {accessibleTeams: {some: {key: {in: ["ENG", "LAW"]}}}}) {
    nodes {
      id name url description content status { type }
      projectUpdates(first: 1) { nodes { body health } }
    }
    pageInfo { hasNextPage endCursor }
  }
}"#;
const ISSUES: &str = r#"query($after: String) {
  issues(first: 100, after: $after, filter: {team: {key: {in: ["ENG", "LAW"]}}}) {
    nodes { team { key } state { type } priorityLabel project { id } }
    pageInfo { hasNextPage endCursor }
  }
}"#;

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum DigestError {
    #[error("Linear backlog could not be read in full")]
    Read,
    #[error("Linear backlog exceeds Slack's message limit")]
    Length,
    #[error("Linear backlog delivery could not be confirmed")]
    Slack,
}

#[derive(Clone)]
pub struct Context {
    client: reqwest::Client,
    endpoint: String,
    key: reqwest::header::HeaderValue,
    slack: Arc<dyn crate::SlackBot>,
    channel: String,
}

impl Context {
    /// Configure delivery only when this executor has a destination.
    /// # Errors
    /// Returns an error if a destination is configured without its credentials.
    pub fn from_lookup(
        get: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, super::SettingsError> {
        let value = |key| get(key).filter(|value| !value.trim().is_empty());
        let Some(channel) = value(CHANNEL) else {
            return Ok(None);
        };
        if !channel.starts_with(['C', 'G'])
            || !channel
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        {
            return Err(super::SettingsError::Invalid(CHANNEL));
        }
        let key = value(API_KEY).ok_or(super::SettingsError::Missing(API_KEY))?;
        let mut key = reqwest::header::HeaderValue::from_str(&key)
            .map_err(|_| super::SettingsError::Invalid(API_KEY))?;
        key.set_sensitive(true);
        let token =
            value("SLACK_BOT_TOKEN").ok_or(super::SettingsError::Missing("SLACK_BOT_TOKEN"))?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| super::SettingsError::Invalid(API_KEY))?;
        Ok(Some(Self {
            client,
            endpoint: ENDPOINT.to_owned(),
            key,
            slack: Arc::new(crate::SlackBotClient::new(token)),
            channel,
        }))
    }

    async fn read(&self) -> Result<String, DigestError> {
        let projects = self.pages::<Project>(PROJECTS, "projects").await?;
        let issues = self.pages::<Issue>(ISSUES, "issues").await?;
        render(projects, issues)
    }

    async fn pages<T: DeserializeOwned>(
        &self,
        query: &str,
        field: &str,
    ) -> Result<Vec<T>, DigestError> {
        let mut rows = Vec::new();
        let mut cursor = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            let mut result = self
                .client
                .post(&self.endpoint)
                .header(reqwest::header::AUTHORIZATION, self.key.clone())
                .json(&serde_json::json!({"query": query, "variables": {"after": cursor}}))
                .send()
                .await
                .map_err(|_| DigestError::Read)?
                .error_for_status()
                .map_err(|_| DigestError::Read)?
                .json::<Response<T>>()
                .await
                .map_err(|_| DigestError::Read)?;
            if result.errors.is_some_and(|errors| !errors.is_empty()) {
                return Err(DigestError::Read);
            }
            let page = result
                .data
                .as_mut()
                .and_then(|data| data.remove(field))
                .ok_or(DigestError::Read)?;
            rows.extend(page.nodes);
            if !page.page_info.has_next_page {
                return Ok(rows);
            }
            let next = page.page_info.end_cursor.ok_or(DigestError::Read)?;
            if !seen.insert(next.clone()) {
                return Err(DigestError::Read);
            }
            cursor = Some(next);
        }
    }
}

#[derive(Deserialize)]
struct Response<T> {
    data: Option<BTreeMap<String, Page<T>>>,
    errors: Option<Vec<serde_json::Value>>,
}

#[derive(Deserialize)]
struct Page<T> {
    nodes: Vec<T>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
struct Project {
    id: String,
    name: String,
    url: String,
    description: Option<String>,
    content: Option<String>,
    #[serde(rename = "projectUpdates", default)]
    updates: Option<ProjectUpdates>,
    status: State,
}

#[derive(Deserialize)]
struct ProjectUpdates {
    nodes: Vec<ProjectUpdate>,
}

#[derive(Deserialize)]
struct ProjectUpdate {
    body: String,
    health: String,
}

#[derive(Deserialize)]
struct State {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct Issue {
    team: Team,
    state: State,
    #[serde(rename = "priorityLabel")]
    priority: String,
    project: Option<ProjectId>,
}

#[derive(Deserialize)]
struct Team {
    key: String,
}

#[derive(Deserialize)]
struct ProjectId {
    id: String,
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn excerpt(value: &str, limit: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let mut excerpt: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        excerpt.push('…');
    }
    escape(&excerpt)
}

fn render(projects: Vec<Project>, issues: Vec<Issue>) -> Result<String, DigestError> {
    let projects: BTreeMap<_, _> = projects
        .into_iter()
        .map(|project| (project.id.clone(), project))
        .collect();
    let mut by_status = BTreeMap::<_, usize>::new();
    let mut by_team = BTreeMap::<_, usize>::new();
    let mut by_project = BTreeMap::<_, usize>::new();
    let mut priorities = [0; 5];
    let mut open = 0;
    let mut unprojected = 0;
    for issue in issues {
        if matches!(
            issue.state.kind.as_str(),
            "completed" | "canceled" | "duplicate"
        ) {
            continue;
        }
        open += 1;
        *by_team.entry(issue.team.key).or_default() += 1;
        let priority = match issue.priority.as_str() {
            "No priority" => 0,
            "Urgent" => 1,
            "High" => 2,
            "Medium" => 3,
            "Low" => 4,
            _ => return Err(DigestError::Read),
        };
        priorities[priority] += 1;
        match issue.project {
            Some(project) => *by_project.entry(project.id).or_default() += 1,
            None => unprojected += 1,
        }
    }
    for project in projects.values() {
        *by_status.entry(project.status.kind.as_str()).or_default() += 1;
    }
    let counts = |values: BTreeMap<&str, usize>| {
        values
            .into_iter()
            .map(|(label, count)| format!("{} {count}", escape(label)))
            .collect::<Vec<_>>()
            .join(" · ")
    };
    let mut text = format!(
        "*Linear backlog*\n{} projects ({}) · {open} open issues ({}) · {unprojected} without a project\nUrgent {} · High {} · Medium {} · Low {} · No priority {}",
        projects.len(), counts(by_status), counts(by_team.iter().map(|(k, v)| (k.as_str(), *v)).collect()),
        priorities[1], priorities[2], priorities[3], priorities[4], priorities[0],
    );
    let mut projects: Vec<_> = projects.into_values().collect();
    projects.sort_by(|left, right| left.name.cmp(&right.name));
    for project in projects {
        if project.status.kind != "started" {
            continue;
        }
        let count = by_project.get(&project.id).copied().unwrap_or_default();
        let rationale = [project.description.as_deref(), project.content.as_deref()]
            .into_iter()
            .flatten()
            .find(|text| !text.trim().is_empty())
            .unwrap_or("Purpose missing in Linear");
        let label = escape(&project.name).replace('|', "¦");
        let url = reqwest::Url::parse(&project.url).map_err(|_| DigestError::Read)?;
        if url.scheme() != "https" || url.host_str() != Some("linear.app") {
            return Err(DigestError::Read);
        }
        let _ = write!(
            text,
            "\n• <{}|{label}> — {count} open. {}",
            url.as_str().replace('|', "%7C"),
            excerpt(rationale, 360)
        );
        if let Some(update) = project
            .updates
            .and_then(|updates| updates.nodes.into_iter().next())
        {
            let _ = write!(
                text,
                " · Update ({}): {}",
                escape(&update.health),
                excerpt(&update.body, 160)
            );
        }
    }
    if text.chars().count() > 39_000 {
        return Err(DigestError::Length);
    }
    Ok(text)
}

type Workflow = WorkflowRef<NaiveDate, (), DigestError>;

pub(super) fn register(dbos: &DBOS, context: Context) -> dbos::Result<Workflow> {
    dbos.register_workflow(WORKFLOW, move |day: NaiveDate| {
        let context = context.clone();
        async move {
            let source = context.clone();
            let text =
                dbos::step_with::<String, DigestError, _, _>("read", retry(), move || {
                    let source = source.clone();
                    async move {
                        dbos::Result::<String, DigestError>::Ok(
                            source.read().await.map_err(dbos::Error::Application)?,
                        )
                    }
                })
                .await?;
            dbos::step_with::<(), DigestError, _, _>("post", retry(), move || {
                let (context, text) = (context.clone(), text.clone());
                async move {
                    if super::post_once(context.slack.as_ref(), &context.channel, &text, slot(day))
                        .await
                    {
                        dbos::Result::<(), DigestError>::Ok(())
                    } else {
                        Err(dbos::Error::Application(DigestError::Slack))
                    }
                }
            })
            .await?;
            dbos::Result::<_, DigestError>::Ok(())
        }
    })
}

fn retry() -> StepOptions<DigestError> {
    StepOptions {
        max_attempts: 5,
        ..Default::default()
    }
}

fn slot(day: NaiveDate) -> DateTime<Utc> {
    let nine = NaiveTime::from_hms_opt(9, 0, 0).unwrap_or_default();
    // 09:00 is outside the DST transition window in New York.
    New_York
        .from_local_datetime(&day.and_time(nine))
        .single()
        .map_or_else(
            || day.and_time(nine).and_utc(),
            |local| local.with_timezone(&Utc),
        )
}

pub(super) async fn schedule(workflow: Workflow, stop: CancellationToken) {
    let mut day = Utc::now().with_timezone(&New_York).date_naive();
    loop {
        let delay = (slot(day) - Utc::now()).to_std().unwrap_or_default();
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(delay) => {}
        }
        let id = format!("{WORKFLOW}-{day}");
        match workflow
            .start_with(
                day,
                dbos::StartOptions {
                    workflow_id: Some(&id),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(_) => {
                let Some(next) = day.checked_add_days(Days::new(1)) else {
                    return;
                };
                day = next;
            }
            Err(error) => {
                tracing::error!(%error, %day, "DBOS Linear backlog start failed");
                tokio::select! {
                    () = stop.cancelled() => return,
                    () = tokio::time::sleep(Duration::from_mins(1)) => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn paginates_and_rejects_graphql_partial_data() {
        use wiremock::{matchers::body_partial_json, Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let lookup = |key: &str| match key {
            CHANNEL => Some("C012BACKLOG".to_owned()),
            API_KEY | "SLACK_BOT_TOKEN" => Some("synthetic".to_owned()),
            _ => None,
        };
        let mut context = Context::from_lookup(lookup).unwrap().unwrap();
        context.endpoint = server.uri();
        let response = |id, next, cursor| {
            serde_json::json!({
                "data": {"projects": {"nodes": [{"id": id}],
                    "pageInfo": {"hasNextPage": next, "endCursor": cursor}}}
            })
        };
        Mock::given(body_partial_json(
            serde_json::json!({"variables": {"after": null}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(response(
            "first",
            true,
            Some("next"),
        )))
        .expect(2)
        .mount(&server)
        .await;
        let second = || {
            Mock::given(body_partial_json(
                serde_json::json!({"variables": {"after": "next"}}),
            ))
        };
        let valid = second()
            .respond_with(ResponseTemplate::new(200).set_body_json(response("second", false, None)))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let rows = context
            .pages::<ProjectId>(PROJECTS, "projects")
            .await
            .unwrap();
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["first", "second"]
        );
        drop(valid);
        let mut partial = response("second", false, None);
        partial["errors"] = serde_json::json!([{"message": "synthetic access error"}]);
        second()
            .respond_with(ResponseTemplate::new(200).set_body_json(partial))
            .expect(1)
            .mount(&server)
            .await;
        assert!(matches!(
            context.pages::<ProjectId>(PROJECTS, "projects").await,
            Err(DigestError::Read)
        ));
    }

    #[test]
    fn eastern_slot_tracks_daylight_saving() {
        for (day, hour) in [("2026-03-07", 14), ("2026-03-08", 13), ("2026-11-01", 14)] {
            let day = NaiveDate::parse_from_str(day, "%Y-%m-%d").unwrap();
            assert_eq!(slot(day), day.and_hms_opt(hour, 0, 0).unwrap().and_utc());
        }
    }

    #[test]
    fn digest_counts_open_work_and_explains_shared_projects_once() {
        let project = || {
            serde_json::from_value(serde_json::json!({
                "id": "one", "name": "Example", "url": "https://linear.app/example/project/one",
                "description": "Improve access", "status": {"type":"started"}
            }))
            .unwrap()
        };
        let issue = |kind, project| {
            serde_json::from_value(serde_json::json!({
                "team": {"key":"ENG"}, "state": {"type":kind}, "priorityLabel": "High", "project": project
            }))
            .unwrap()
        };
        let text = render(
            vec![project(), project()],
            vec![
                issue("started", serde_json::json!({"id":"one"})),
                issue("backlog", serde_json::Value::Null),
                issue("duplicate", serde_json::Value::Null),
            ],
        )
        .unwrap();
        assert!(
            text.contains("1 projects (started 1) · 2 open issues (ENG 2) · 1 without a project")
        );
        assert_eq!(text.matches("Improve access").count(), 1);
    }
}
