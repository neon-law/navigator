//! Read-only cross-Project deadline brief for the signed-in lawyer.
//!
//! The batch read uses the same Firm-scoped open matter set as the lawyer
//! dashboard. A failed host read remains an incomplete brief.

use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use comfy_table::{presets::UTF8_FULL, Cell, ContentArrangement, Table};
use model::project_deadlines::{Appearance, Hearing, SchedulingPolicy};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::remote;

pub(super) const INVENTORY_PATH: &str = "/app/api/lawyer/deadline-projects";
pub(super) const READ_CONCURRENCY: usize = 8;
const LOCAL_FORMAT: &str = "%Y-%m-%d %H:%M %:z";
const BANDS: [&str; 4] = ["Overdue", "Today", "Next 7 days", "Next 30 days"];

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Project {
    id: Uuid,
    code: String,
    name: String,
    status: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Deadline {
    id: Uuid,
    project_id: Uuid,
    due_at: String,
    due_on: Option<model::project_deadlines::DueOn>,
    status: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    authority: String,
    #[serde(default)]
    authority_kind: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    source_document_id: Option<Uuid>,
    #[serde(default)]
    owner_person_id: Option<Uuid>,
    #[serde(default)]
    calendar_role: String,
    #[serde(default)]
    category: Option<String>,
    hearing: Option<Hearing>,
    replay_key: Option<String>,
    #[serde(default)]
    effort_minutes: i32,
    #[serde(default)]
    scheduling_policy: SchedulingPolicy,
}

#[derive(Clone, Debug)]
struct ReadFailure {
    project: Project,
    message: String,
}

#[derive(Default, Deserialize)]
struct DeadlineBatch {
    projects: Vec<Project>,
    deadlines: Vec<Deadline>,
    #[serde(default)]
    scheduling_policy: SchedulingPolicy,
    #[serde(default)]
    work_schedule: Value,
}

fn validate_deadlines(rows: Vec<Deadline>) -> Result<Vec<Deadline>> {
    for row in &rows {
        DateTime::parse_from_rfc3339(&row.due_at)
            .with_context(|| format!("deadline {} has an unreadable due_at value", row.id))?;
    }
    Ok(rows)
}

fn partition_batch(batch: DeadlineBatch) -> Result<Vec<(Project, Vec<Deadline>)>> {
    let mut by_project = batch
        .projects
        .into_iter()
        .map(|project| (project.id, (project, Vec::new())))
        .collect::<std::collections::HashMap<_, _>>();
    for mut row in validate_deadlines(batch.deadlines)? {
        row.scheduling_policy = batch.scheduling_policy;
        let project_id = row.project_id;
        by_project
            .get_mut(&project_id)
            .ok_or_else(|| anyhow!("deadline batch contains a Project outside its inventory"))?
            .1
            .push(row);
    }
    let mut rows = by_project.into_values().collect::<Vec<_>>();
    rows.sort_by(|a, b| a.0.code.cmp(&b.0.code));
    Ok(rows)
}

#[derive(Clone, Debug, Default)]
struct Filters {
    due_by: Option<NaiveDate>,
    today: bool,
    overdue: bool,
    status: String,
    project: Option<String>,
    horizon: Option<i64>,
}

pub(super) fn local_zone(explicit: Option<&str>) -> Result<Tz> {
    let name = explicit
        .map(str::to_owned)
        .or_else(|| std::env::var("TZ").ok())
        .or_else(|| {
            ["/etc/localtime", "/var/db/timezone/localtime"]
                .iter()
                .filter_map(|path| std::fs::read_link(path).ok())
                .find_map(|path| {
                    path.to_string_lossy()
                        .split("zoneinfo/")
                        .nth(1)
                        .map(str::to_owned)
                })
        })
        .unwrap_or_else(|| "UTC".into());
    Tz::from_str(&name).with_context(|| format!("unknown IANA time zone `{name}`"))
}

fn selected(deadline: &Deadline, filters: &Filters, now: DateTime<Utc>, zone: Tz) -> bool {
    let status = if filters.overdue {
        "open"
    } else {
        filters.status.as_str()
    };
    if deadline.status != status {
        return false;
    }
    let Ok(due) = DateTime::parse_from_rfc3339(&deadline.due_at) else {
        return false;
    };
    let local_due = deadline
        .due_on
        .as_ref()
        .map_or_else(|| due.with_timezone(&zone).date_naive(), |on| on.date);
    let today = now.with_timezone(&zone).date_naive();
    // A civil deadline can still be running after midnight in the viewer's zone.
    if filters.today && (local_due > today || (local_due < today && due.with_timezone(&Utc) < now))
    {
        return false;
    }
    if filters.due_by.is_some_and(|date| local_due > date) {
        return false;
    }
    if filters.horizon.is_some_and(|days| {
        matches!(
            deadline.category.as_deref(),
            Some("trial_day" | "deposition")
        ) && local_due.signed_duration_since(today).num_days() > days
    }) {
        return false;
    }
    !filters.overdue || due.with_timezone(&Utc) < now
}

fn band(deadline: &Deadline, now: DateTime<Utc>, zone: Tz) -> Option<&'static str> {
    let due = DateTime::parse_from_rfc3339(&deadline.due_at).ok()?;
    if due.with_timezone(&Utc) < now {
        return Some("Overdue");
    }
    let today = now.with_timezone(&zone).date_naive();
    let day = deadline
        .due_on
        .as_ref()
        .map_or_else(|| due.with_timezone(&zone).date_naive(), |on| on.date);
    let distance = day.signed_duration_since(today).num_days();
    Some(match distance {
        ..=0 => "Today",
        1..=7 => "Next 7 days",
        8..=30 => "Next 30 days",
        _ => "Later",
    })
}

fn title(row: &Deadline) -> &str {
    row.title
        .as_deref()
        .filter(|title| !title.is_empty())
        .unwrap_or("Deadline")
}

fn due_utc(row: &Deadline) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&row.due_at)
        .ok()
        .map(|due| due.with_timezone(&Utc))
}

fn source_href(base: &str, project: &Project, deadline: &Deadline) -> String {
    format!(
        "{base}/app/projects/{}/deadlines#deadline-{}",
        project.code, deadline.id
    )
}

fn scheduling_times(row: &Deadline) -> (Option<String>, Option<(String, String)>) {
    if row.status != "open" {
        return (None, None);
    }
    let Ok(due) = DateTime::parse_from_rfc3339(&row.due_at) else {
        return (None, None);
    };
    let category = row
        .category
        .as_deref()
        .and_then(|category| category.parse().ok());
    if model::project_deadlines::is_appearance(&row.calendar_role, category) {
        (
            None,
            row.scheduling_policy
                .appearance(due, row.hearing.as_ref().map_or(60, Hearing::minutes))
                .map(|(start, end)| (start.to_rfc3339(), end.to_rfc3339())),
        )
    } else {
        (
            row.scheduling_policy
                .submit_by(due, row.due_on.as_ref())
                .map(|at| at.to_rfc3339()),
            None,
        )
    }
}

fn deadline_value(base: &str, project: &Project, row: &Deadline, zone: Tz) -> Value {
    let (submit_by, busy_window) = scheduling_times(row);
    let trial_day = match row
        .hearing
        .as_ref()
        .and_then(|hearing| hearing.appearance.as_ref())
    {
        Some(Appearance::TrialDay { day, .. }) => *day,
        _ => None,
    };
    json!({
        "id": row.id,
        "project_code": project.code,
        "project_name": project.name,
        "project_href": format!("{base}/app/projects/{}", project.code),
        "deadline_href": source_href(base, project, row),
        "title": title(row),
        "due_at": row.due_at,
        "due_on": row.due_on,
        "due_local": local_due(row, zone),
        "status": row.status,
        "category": row.category,
        "calendar_role": row.calendar_role,
        "authority": row.authority,
        "authority_kind": row.authority_kind,
        "source": row.source,
        "source_document_id": row.source_document_id,
        "source_document_href": row.source_document_id.map(|asset| format!(
            "{base}/app/projects/{}/documents/{asset}/download", project.code
        )),
        "owner_person_id": row.owner_person_id,
        "description": row.description,
        "effort_minutes": row.effort_minutes,
        "submit_by": submit_by,
        "busy_window": busy_window,
        "trial_day": trial_day,
        "appearance": row.hearing,
        "replay_key": row.replay_key,
    })
}

pub(super) async fn list<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    url: String,
    token: &str,
) -> Result<T> {
    let response = client
        .get(url)
        .timeout(Duration::from_secs(30))
        .bearer_auth(token)
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(anyhow!(
            "HTTP {status}: {}",
            body.trim().chars().take(200).collect::<String>()
        ));
    }
    serde_json::from_str(&body).context("parse Navigator read response")
}

async fn project_deadlines(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    id: Uuid,
) -> Result<Vec<Deadline>> {
    list(
        client,
        format!("{base}/app/api/projects/{id}/deadlines"),
        token,
    )
    .await
    .and_then(validate_deadlines)
}

pub(super) async fn open_for_project(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    id: Uuid,
    code: &str,
    name: &str,
    now: DateTime<Utc>,
) -> Result<Vec<Value>> {
    let mut rows = project_deadlines(client, base, token, id).await?;
    rows.retain(|row| row.status == "open");
    rows.sort_by_key(|row| (due_utc(row), row.id));
    let project = Project {
        id,
        code: code.into(),
        name: name.into(),
        status: "open".into(),
    };
    Ok(rows
        .iter()
        .map(|row| {
            let mut value = deadline_value(base, &project, row, chrono_tz::UTC);
            value["overdue"] = json!(due_utc(row).is_some_and(|due| due < now));
            value
        })
        .collect())
}

struct ReportRows {
    rows: Vec<(Project, Deadline)>,
    next_by_project: Vec<(Project, Deadline)>,
    empty_reads: usize,
    no_filter_matches: usize,
    selected_projects: usize,
    matched_project_codes: Vec<String>,
}

fn project_is_selected(project: &Project, filters: &Filters) -> bool {
    filters
        .project
        .as_deref()
        .is_none_or(|code| code == project.code)
}

fn collect_rows(
    successful: &[(Project, Vec<Deadline>)],
    failures: &[ReadFailure],
    filters: &Filters,
    now: DateTime<Utc>,
    zone: Tz,
) -> ReportRows {
    let selected_projects = successful
        .iter()
        .filter(|(project, _)| project_is_selected(project, filters))
        .count()
        + failures
            .iter()
            .filter(|failure| project_is_selected(&failure.project, filters))
            .count();
    let mut rows = Vec::<(Project, Deadline)>::new();
    let mut empty_reads = 0usize;
    let mut no_filter_matches = 0usize;
    let mut next_by_project = Vec::<(Project, Deadline)>::new();
    for (project, deadlines) in successful {
        if !project_is_selected(project, filters) {
            continue;
        }
        let mut open = deadlines
            .iter()
            .filter(|row| row.status == "open")
            .cloned()
            .collect::<Vec<_>>();
        open.sort_by_key(due_utc);
        if let Some(next) = open.first() {
            next_by_project.push((project.clone(), next.clone()));
        }
        let matching = deadlines
            .iter()
            .filter(|row| selected(row, filters, now, zone))
            .cloned()
            .collect::<Vec<_>>();
        if deadlines.is_empty() {
            empty_reads += 1;
        } else if matching.is_empty() {
            no_filter_matches += 1;
        }
        rows.extend(matching.into_iter().map(|row| (project.clone(), row)));
    }
    rows.sort_by_key(|(_, row)| (due_utc(row), row.id));
    let matched_project_codes = successful
        .iter()
        .filter(|(project, _)| project_is_selected(project, filters))
        .map(|(project, _)| project.code.clone())
        .collect::<Vec<_>>();
    ReportRows {
        rows,
        next_by_project,
        empty_reads,
        no_filter_matches,
        selected_projects,
        matched_project_codes,
    }
}

fn make_report(
    base: &str,
    rows: &ReportRows,
    failures: &[ReadFailure],
    filters: &Filters,
    now: DateTime<Utc>,
    zone: Tz,
    inventory_projects: usize,
) -> Value {
    let failure_json = failures
        .iter()
        .filter(|failure| project_is_selected(&failure.project, filters))
        .map(|failure| json!({"project_code": failure.project.code, "error": failure.message}))
        .collect::<Vec<_>>();
    let row_values = rows
        .rows
        .iter()
        .map(|(project, row)| deadline_value(base, project, row, zone))
        .collect::<Vec<_>>();
    let views = BANDS
        .into_iter()
        .map(|label| {
            let items = rows
                .rows
                .iter()
                .filter(|(_, row)| row.status == "open" && band(row, now, zone) == Some(label))
                .map(|(project, row)| deadline_value(base, project, row, zone))
                .collect::<Vec<_>>();
            (label.to_owned(), json!(items))
        })
        .collect::<serde_json::Map<_, _>>();
    let next_values = rows
        .next_by_project
        .iter()
        .map(|(project, row)| deadline_value(base, project, row, zone))
        .collect::<Vec<_>>();
    json!({
        "as_of": now.to_rfc3339(),
        "timezone": zone.name(),
        "complete": failure_json.is_empty() && rows.selected_projects == rows.matched_project_codes.len(),
        "coverage": {
            "projects_expected": rows.selected_projects,
            "projects_read": rows.matched_project_codes.len(),
            "successful_empty_reads": rows.empty_reads,
            "successful_reads_with_no_filter_matches": rows.no_filter_matches,
            "failed_reads": failure_json.len(),
            "failed_projects": failure_json,
            "project_codes_read": rows.matched_project_codes,
            "inventory_projects": inventory_projects
        },
        "filters": {
            "appearance_horizon_days": filters.horizon,
            "project": filters.project,
            "due_by": filters.due_by.map(|date| date.to_string()),
            "today": filters.today,
            "overdue": filters.overdue,
            "status": if filters.overdue { "open" } else { filters.status.as_str() }
        },
        "views": views,
        "next_recorded_deadline_per_project": next_values,
        "deadlines": row_values
    })
}

#[cfg(test)]
fn render(
    base: &str,
    successful: &[(Project, Vec<Deadline>)],
    failures: &[ReadFailure],
    filters: &Filters,
    now: DateTime<Utc>,
    zone: Tz,
    as_json: bool,
) -> String {
    let inventory_projects = successful.len() + failures.len();
    let rows = collect_rows(successful, failures, filters, now, zone);
    let report = make_report(
        base,
        &rows,
        failures,
        filters,
        now,
        zone,
        inventory_projects,
    );
    if as_json {
        return report.to_string();
    }
    render_console(base, &rows, failures, filters, now, zone, &report)
}

fn render_console(
    base: &str,
    data: &ReportRows,
    failures: &[ReadFailure],
    filters: &Filters,
    now: DateTime<Utc>,
    zone: Tz,
    report: &Value,
) -> String {
    let mut lines = vec![
        format!(
            "Deadline brief · {} · as of {}",
            zone.name(),
            now.with_timezone(&zone).format(LOCAL_FORMAT)
        ),
        format!(
            "Coverage: {}/{} Projects read; {} empty source reads; {} reads with no filter matches; {} failed reads.",
            data.matched_project_codes.len(),
            data.selected_projects,
            data.empty_reads,
            data.no_filter_matches,
            failures.len()
        ),
    ];
    lines.extend(
        failures
            .iter()
            .filter(|failure| project_is_selected(&failure.project, filters))
            .map(|failure| format!("  FAILED {}: {}", failure.project.code, failure.message)),
    );
    if failures.is_empty() && data.rows.is_empty() {
        lines.push("No deadlines match the selected filters.".into());
    }
    lines.push(render_table(base, &data.rows, zone));
    for label in BANDS {
        lines.push(format!("{label}:"));
        let before = lines.len();
        lines.extend(
            data.rows
                .iter()
                .filter(|(_, row)| row.status == "open" && band(row, now, zone) == Some(label))
                .map(|(project, row)| {
                    format!(
                        "  {} · {} · {} · {}",
                        project.code,
                        format_due(row, zone),
                        row.category.as_deref().unwrap_or("uncategorized"),
                        title(row)
                    )
                }),
        );
        if lines.len() == before {
            lines.push("  none".into());
        }
    }
    lines.push("Next recorded operative deadline per Project:".into());
    lines.extend(data.next_by_project.iter().map(|(project, row)| {
        format!(
            "  {} · {} · {}",
            project.code,
            format_due(row, zone),
            row.title.as_deref().unwrap_or("Deadline")
        )
    }));
    let count = |label: &str| report["views"][label].as_array().map_or(0, Vec::len);
    lines.push(format!(
        "Views: overdue {}, today {}, next 7 days {}, next 30 days {}.",
        count(BANDS[0]),
        count(BANDS[1]),
        count(BANDS[2]),
        count(BANDS[3])
    ));
    lines.join("\n")
}

fn format_due(row: &Deadline, zone: Tz) -> String {
    local_due(row, zone).unwrap_or_else(|| row.due_at.clone())
}

fn local_due(row: &Deadline, zone: Tz) -> Option<String> {
    row.due_on
        .as_ref()
        .map(model::project_deadlines::DueOn::display_date)
        .or_else(|| {
            due_utc(row).map(|date| date.with_timezone(&zone).format(LOCAL_FORMAT).to_string())
        })
}

fn render_table(base: &str, rows: &[(Project, Deadline)], zone: Tz) -> String {
    let mut table = Table::new();
    table
        .load_style(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header([
        "Project",
        "Due (local)",
        "Category",
        "Title",
        "Source context",
    ]);
    for (project, row) in rows {
        let context = format!(
            "{}: {} · source {} · note {} · owner {} · source document {} · {}",
            row.authority_kind,
            if row.authority.is_empty() {
                "unspecified"
            } else {
                &row.authority
            },
            row.source,
            row.description
                .as_deref()
                .filter(|description| !description.is_empty())
                .unwrap_or("none"),
            row.owner_person_id
                .map_or_else(|| "unassigned".into(), |id| id.to_string()),
            row.source_document_id.map_or_else(
                || "none".into(),
                |id| format!(
                    "{id} ({base}/app/projects/{}/documents/{id}/download)",
                    project.code
                )
            ),
            source_href(base, project, row)
        );
        let (submit_by, busy_window) = scheduling_times(row);
        let context = match (submit_by, busy_window) {
            (Some(at), _) => format!("{context} · Submit by {at}"),
            (_, Some((start, end))) => format!("{context} · Busy {start} – {end}"),
            _ => context,
        };
        table.add_row([
            Cell::new(&project.code),
            Cell::new(format_due(row, zone)),
            Cell::new(row.category.as_deref().unwrap_or("uncategorized")),
            Cell::new(title(row)),
            Cell::new(context),
        ]);
    }
    table.to_string()
}

pub enum BriefHosts {
    Selected(Option<String>),
    AllStored,
}

pub struct BriefOptions {
    pub hosts: BriefHosts,
    pub project: Option<String>,
    pub due_by: Option<String>,
    pub today: bool,
    pub overdue: bool,
    pub status: String,
    pub timezone: Option<String>,
    pub as_json: bool,
    pub horizon: i64,
}

fn host_report(reports: Vec<(String, Result<Value>)>) -> Value {
    let hosts: Vec<_> = reports
        .into_iter()
        .map(|(host, result)| match result {
            Ok(report) => json!({"host": host, "report": report}),
            Err(error) => json!({"host": host, "error": format!("{error:#}")}),
        })
        .collect();
    let complete = hosts.iter().all(|host| host["report"]["complete"] == true);
    json!({"complete": complete, "hosts": hosts})
}

pub async fn brief(options: BriefOptions) -> std::process::ExitCode {
    match run(options).await {
        Ok(complete) if complete => std::process::ExitCode::SUCCESS,
        Ok(_) => std::process::ExitCode::from(1),
        Err(error) => {
            eprintln!("navigator project deadline brief: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(options: BriefOptions) -> Result<bool> {
    let BriefOptions {
        hosts,
        project,
        due_by,
        today,
        overdue,
        status,
        timezone,
        as_json,
        horizon,
    } = options;
    let due_by = due_by
        .map(|value| {
            NaiveDate::parse_from_str(&value, "%Y-%m-%d")
                .with_context(|| format!("--due-by must be YYYY-MM-DD, got `{value}`"))
        })
        .transpose()?;
    let zone = local_zone(timezone.as_deref())?;
    let now = Utc::now();
    let filters = Filters {
        due_by,
        today,
        overdue,
        status,
        project: project.clone(),
        horizon: Some(horizon),
    };
    let BriefHosts::Selected(host) = hosts else {
        let credentials =
            crate::credentials::load(&crate::credentials::default_credentials_path())?;
        anyhow::ensure!(
            !credentials.hosts.is_empty(),
            "no stored Navigator sessions"
        );
        let mut reports = Vec::new();
        for host in credentials.hosts.into_keys() {
            let result = async {
                let (base, token) = remote::resolve(Some(&host))?;
                let (rendered, complete) =
                    render_host(&base, &token, &filters, now, zone, as_json).await?;
                if as_json {
                    return serde_json::from_str(&rendered).context("decode deadline report");
                }
                Ok(json!({"text": rendered, "complete": complete}))
            }
            .await;
            reports.push((host, result));
        }
        let report = host_report(reports);
        if as_json {
            println!("{report}");
        } else if let Some(hosts) = report["hosts"].as_array() {
            for host in hosts {
                println!("{}", host["host"].as_str().unwrap_or_default());
                if let Some(body) = host["report"]["text"]
                    .as_str()
                    .or_else(|| host["error"].as_str())
                {
                    println!("{body}");
                }
            }
        }
        return Ok(report["complete"] == true);
    };
    let (base, token) = remote::resolve(host.as_deref())?;
    let (rendered, complete) = render_host(&base, &token, &filters, now, zone, as_json).await?;
    println!("{rendered}");
    Ok(complete)
}

async fn render_host(
    base: &str,
    token: &str,
    filters: &Filters,
    now: DateTime<Utc>,
    zone: Tz,
    as_json: bool,
) -> Result<(String, bool)> {
    let (successful, failures, schedule) =
        read_host(base, token, filters.project.as_deref(), zone).await?;
    let rows = collect_rows(&successful, &failures, filters, now, zone);
    let mut report = make_report(
        base,
        &rows,
        &failures,
        filters,
        now,
        zone,
        successful.len() + failures.len(),
    );
    add_schedule(&mut report, schedule, zone);
    let rendered = if as_json {
        report.to_string()
    } else {
        let text = render_console(base, &rows, &failures, filters, now, zone, &report);
        let extra = scheduling_console(&report);
        if extra.is_empty() {
            text
        } else {
            format!("{extra}\n{text}")
        }
    };
    Ok((rendered, failures.is_empty()))
}

async fn read_host(
    base: &str,
    token: &str,
    project: Option<&str>,
    zone: Tz,
) -> Result<(Vec<(Project, Vec<Deadline>)>, Vec<ReadFailure>, Value)> {
    let client = reqwest::Client::new();
    let mut batch: DeadlineBatch = list(
        &client,
        format!("{base}{INVENTORY_PATH}?include_deadlines=true&deadline_schedule_timezone={zone}"),
        token,
    )
    .await
    .context("read the dashboard-scoped open Project deadline batch")?;
    let schedule = std::mem::take(&mut batch.work_schedule);
    let mut projects = partition_batch(batch)?;
    if let Some(code) = project {
        projects.retain(|(row, _)| row.code == code);
        anyhow::ensure!(
            !projects.is_empty(),
            "no visible open Project has code `{code}`"
        );
    }
    Ok((projects, Vec::new(), schedule))
}

pub(super) async fn agenda_rows(
    base: &str,
    token: &str,
    day: NaiveDate,
    now: DateTime<Utc>,
    zone: Tz,
    horizon: i64,
) -> Result<Value> {
    let filters = Filters {
        status: "open".to_owned(),
        due_by: Some(day),
        ..Filters::default()
    };
    let (successful, failures, schedule) = read_host(base, token, None, zone).await?;
    let rows = collect_rows(&successful, &failures, &filters, now, zone);
    let upcoming = Filters {
        status: "open".to_owned(),
        horizon: Some(horizon),
        ..Filters::default()
    };
    let upcoming = collect_rows(&successful, &failures, &upcoming, now, zone);
    let mut report = make_report(
        base,
        &rows,
        &failures,
        &filters,
        now,
        zone,
        successful.len() + failures.len(),
    );
    report["appearances"] = upcoming_appearances(
        &upcoming
            .rows
            .iter()
            .map(|(project, row)| deadline_value(base, project, row, zone))
            .collect::<Vec<_>>(),
        now,
    );
    add_schedule(&mut report, schedule, zone);
    Ok(report)
}

fn upcoming_appearances(rows: &[Value], now: DateTime<Utc>) -> Value {
    use std::collections::BTreeMap;
    let mut groups = BTreeMap::<String, (Vec<&Value>, Vec<&Value>)>::new();
    for row in rows {
        if row["status"] != "open"
            || !matches!(row["category"].as_str(), Some("trial_day" | "deposition"))
            || row["due_at"]
                .as_str()
                .and_then(|at| at.parse::<DateTime<chrono::FixedOffset>>().ok())
                .is_none_or(|at| at < now)
        {
            continue;
        }
        if let Some(id) = row["trial_day"]["series_id"]
            .as_str()
            .or_else(|| row["id"].as_str())
        {
            groups.entry(id.to_owned()).or_default().0.push(row);
        }
    }
    for row in rows {
        if let Some(id) = row["replay_key"]
            .as_str()
            .and_then(|key| key.strip_prefix("preparation:"))
            .and_then(|key| key.split_once(':'))
            .map(|(id, _)| id)
        {
            if let Some((_, prep)) = groups.get_mut(id) {
                if row["status"] == "open" {
                    prep.push(row);
                }
            }
        }
    }
    let mut groups: Vec<_> = groups.into_values().collect();
    groups.sort_by_key(|(days, _)| days.first().and_then(|row| row["due_at"].as_str()));
    json!(groups
        .into_iter()
        .map(|(days, preparation)| json!({
            "days": days, "preparation": preparation,
        }))
        .collect::<Vec<_>>())
}

fn add_schedule(report: &mut Value, mut plan: Value, zone: Tz) {
    use model::deadline_schedule::{capacity, Window};
    let parse =
        |value: &Value| serde_json::from_value::<Vec<Window>>(value.clone()).unwrap_or_default();
    let mut busy = parse(&plan["busy"]);
    if let Some(blocks) = plan["schedule"]["blocks"].as_object() {
        busy.extend(blocks.values().flat_map(parse));
    }
    let days = parse(&plan["working_hours"])
        .into_iter()
        .map(|window| {
            let (booked, available) = capacity(&[window], &busy);
            (
                window.start.with_timezone(&zone).date_naive().to_string(),
                json!({
                    "booked_minutes": booked, "available_minutes": available,
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    if plan.is_object() {
        plan["capacity"] = days.into();
    }
    let risk = &plan["schedule"]["at_risk"];
    let include_appearances = report.get("appearances").is_none();
    let now = report["as_of"]
        .as_str()
        .and_then(|now| now.parse().ok())
        .unwrap_or_else(Utc::now);
    if let Some(rows) = report["deadlines"].as_array_mut() {
        for row in &mut *rows {
            let conflicts = row["id"].as_str().and_then(|id| risk.get(id)).cloned();
            if let Some(conflicts) = conflicts {
                row["at_risk"] = json!(true);
                row["competing_deadlines"] = conflicts;
            }
        }
        rows.sort_by_key(|row| row["at_risk"] != true);
        if include_appearances {
            report["appearances"] = upcoming_appearances(rows, now);
        }
    }
    report["work_schedule"] = plan;
}

pub(super) fn scheduling_console(report: &Value) -> String {
    let rows = report["deadlines"].as_array().into_iter().flatten();
    let titles: std::collections::BTreeMap<_, _> = rows
        .clone()
        .filter_map(|row| Some((row["id"].as_str()?, row["title"].as_str()?)))
        .collect();
    let mut lines = Vec::new();
    for row in rows.filter(|row| row["at_risk"] == true) {
        let competing = row["competing_deadlines"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|id| titles.get(id).copied().unwrap_or("another Deadline"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "At risk · {} · Competing: {}",
            row["title"].as_str().unwrap_or("Deadline"),
            if competing.is_empty() {
                "calendar commitments"
            } else {
                &competing
            }
        ));
    }
    if let Some(days) = report["work_schedule"]["capacity"].as_object() {
        for (day, capacity) in days {
            let hours = |key: &str| {
                f64::from(
                    i32::try_from(capacity[key].as_i64().unwrap_or_default()).unwrap_or_default(),
                ) / 60.0
            };
            lines.push(format!(
                "{day} · {:.2} hours booked / {:.2} hours available",
                hours("booked_minutes"),
                hours("available_minutes")
            ));
        }
    } else if report["work_schedule"]["availability"] == "unverified" {
        lines.push("Calendar availability could not be verified.".into());
    }
    for group in report["appearances"].as_array().into_iter().flatten() {
        for row in group["days"].as_array().into_iter().flatten() {
            lines.push(format!(
                "{} · {}",
                row["due_local"].as_str().unwrap_or_default(),
                row["title"].as_str().unwrap_or("Appearance")
            ));
        }
        for row in group["preparation"].as_array().into_iter().flatten() {
            lines.push(format!(
                "  Prepare by {} · {}",
                row["due_local"].as_str().unwrap_or_default(),
                row["title"].as_str().unwrap_or("Preparation")
            ));
        }
    }
    lines.join("\n")
}

pub(crate) fn horizon_days(value: &str) -> std::result::Result<i64, String> {
    value
        .strip_suffix('d')
        .and_then(|days| days.parse::<i64>().ok())
        .filter(|days| (1..=366).contains(days))
        .ok_or_else(|| "horizon must be between 1d and 366d".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_host_keeps_successful_host_coverage_in_the_report() {
        let reports = vec![
            (
                "https://one.example".to_owned(),
                Ok(json!({"complete": true})),
            ),
            (
                "https://two.example".to_owned(),
                Err(anyhow!("session expired")),
            ),
        ];
        let report = host_report(reports);
        assert_eq!(report["complete"], false);
        assert_eq!(report["hosts"][0]["report"]["complete"], true);
        assert_eq!(report["hosts"][1]["error"], "session expired");
    }

    fn project(code: &str) -> Project {
        Project {
            id: Uuid::new_v4(),
            code: code.into(),
            name: format!("Synthetic {code}"),
            status: "open".into(),
        }
    }

    fn deadline(due_at: &str, status: &str) -> Deadline {
        Deadline {
            id: Uuid::new_v4(),
            project_id: Uuid::nil(),
            due_on: None,
            due_at: due_at.into(),
            status: status.into(),
            title: Some("Synthetic filing".into()),
            description: Some("Conditional on entry of an order".into()),
            authority: "Synthetic order".into(),
            authority_kind: "court_order".into(),
            source: "lawyer".into(),
            source_document_id: Some(Uuid::new_v4()),
            owner_person_id: Some(Uuid::new_v4()),
            calendar_role: "firm".into(),
            category: Some("court".into()),
            hearing: None,
            replay_key: None,
            effort_minutes: 360,
            scheduling_policy: SchedulingPolicy::default(),
        }
    }

    fn pacific_cutoff() -> Deadline {
        let mut row = deadline("2028-07-13T06:59:00Z", "open");
        row.due_on = Some(model::project_deadlines::DueOn {
            date: "2028-07-12".parse().unwrap(),
            timezone: chrono_tz::America::Los_Angeles,
            cutoff: Some("23:59:00".parse().unwrap()),
        });
        row
    }

    #[test]
    fn the_brief_shows_the_civil_due_date_when_the_cutoff_is_tomorrow_in_the_viewer_zone() {
        let now = "2028-07-12T12:00:00Z".parse().unwrap();
        let zone = chrono_tz::America::New_York;
        let row = pacific_cutoff();
        let projects = [(project("sample"), vec![row])];
        let filters = Filters {
            today: true,
            status: "open".into(),
            ..Default::default()
        };
        let text = render("", &projects, &[], &filters, now, zone, false);
        assert!(text.contains("Wednesday, 12 Jul 2028"));
        assert!(!text.contains("2028-07-13"));
        let report: Value =
            serde_json::from_str(&render("", &projects, &[], &filters, now, zone, true)).unwrap();
        assert_eq!(
            report["views"]["Today"][0]["due_local"],
            "Wednesday, 12 Jul 2028 · 23:59:00 America/Los_Angeles"
        );
    }

    #[test]
    fn a_still_open_pacific_cutoff_stays_urgent_after_eastern_midnight() {
        let row = pacific_cutoff();
        let zone = chrono_tz::America::New_York;
        let filters = Filters {
            today: true,
            status: "open".into(),
            ..Default::default()
        };
        let midnight = "2028-07-13T04:00:00Z".parse().unwrap();
        assert!(selected(&row, &filters, midnight, zone));
        assert_eq!(band(&row, midnight, zone), Some("Today"));
        let after_cutoff = "2028-07-13T07:00:00Z".parse().unwrap();
        assert!(!selected(&row, &filters, after_cutoff, zone));
        assert_eq!(band(&row, after_cutoff, zone), Some("Overdue"));
    }

    #[test]
    fn a_deposition_at_day_100_shows_its_open_preparation() {
        let now = "2030-03-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let project = project("synthetic");
        let zone = chrono_tz::UTC;
        let mut appearance = deadline("2030-06-09T10:00:00Z", "open");
        appearance.category = Some("deposition".into());
        let mut prep = deadline("2030-06-06T10:00:00Z", "open");
        prep.replay_key = Some(format!("preparation:{}:0", appearance.id));
        let filters = Filters {
            horizon: Some(120),
            status: "open".into(),
            ..Filters::default()
        };
        assert!(selected(&appearance, &filters, now, zone));
        assert!(!selected(
            &appearance,
            &Filters {
                horizon: Some(99),
                ..filters
            },
            now,
            zone
        ));
        let result = upcoming_appearances(
            &[
                deadline_value("", &project, &prep, zone),
                deadline_value("", &project, &appearance, zone),
            ],
            now,
        );
        assert_eq!(result[0]["days"][0]["id"], json!(appearance.id));
        assert_eq!(result[0]["preparation"][0]["id"], json!(prep.id));
    }

    #[test]
    fn local_date_filters_and_urgency_bands_follow_the_selected_zone() {
        let now: DateTime<Utc> = "2028-05-02T06:30:00Z".parse().unwrap();
        let filters = Filters {
            today: true,
            status: "open".into(),
            ..Filters::default()
        };
        let row = deadline("2028-05-01T23:15:00-07:00", "open");
        assert!(selected(
            &row,
            &filters,
            now,
            chrono_tz::America::Los_Angeles
        ));
        assert_eq!(
            band(&row, now, chrono_tz::America::Los_Angeles),
            Some("Overdue")
        );
        assert_eq!(
            band(
                &deadline("2028-05-08T17:00:00-07:00", "open"),
                now,
                chrono_tz::America::Los_Angeles
            ),
            Some("Next 7 days")
        );
        let through_may_first = Filters {
            due_by: Some(NaiveDate::from_ymd_opt(2028, 5, 1).unwrap()),
            status: "open".into(),
            ..Filters::default()
        };
        assert!(selected(
            &deadline("2028-05-01T23:59:00-07:00", "open"),
            &through_may_first,
            now,
            chrono_tz::America::Los_Angeles
        ));
        assert!(!selected(
            &deadline("2028-05-02T00:00:00-07:00", "open"),
            &through_may_first,
            now,
            chrono_tz::America::Los_Angeles
        ));
    }

    #[test]
    fn overdue_filter_is_open_only_and_source_context_survives_json_projection() {
        let now = "2028-05-02T12:00:00Z".parse().unwrap();
        let filters = Filters {
            overdue: true,
            status: "complete".into(),
            ..Filters::default()
        };
        assert!(!selected(
            &deadline("2028-05-01T12:00:00Z", "complete"),
            &filters,
            now,
            chrono_tz::UTC
        ));
        assert!(selected(
            &deadline("2028-05-01T12:00:00Z", "open"),
            &filters,
            now,
            chrono_tz::UTC
        ));
        let row = deadline("2028-05-03T12:00:00Z", "open");
        let value = deadline_value(
            "https://navigator.example.test",
            &project("synthetic"),
            &row,
            chrono_tz::UTC,
        );
        assert_eq!(
            value["source_document_id"],
            row.source_document_id.unwrap().to_string()
        );
        assert_eq!(
            value["owner_person_id"],
            row.owner_person_id.unwrap().to_string()
        );
        assert!(value["deadline_href"]
            .as_str()
            .unwrap()
            .contains("#deadline-"));
        assert_eq!(value["description"], "Conditional on entry of an order");
    }

    #[test]
    fn batch_keeps_empty_projects_and_rejects_out_of_scope_deadlines() {
        let good = project("synthetic-good");
        let bad = project("synthetic-bad");
        let mut row = deadline("2028-05-03T12:00:00Z", "open");
        row.project_id = good.id;
        let rows = partition_batch(DeadlineBatch {
            projects: vec![good.clone(), bad],
            deadlines: vec![row.clone()],
            ..DeadlineBatch::default()
        })
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].1.len(), 0);
        assert_eq!(rows[1].1.len(), 1);
        row.project_id = Uuid::new_v4();
        assert!(partition_batch(DeadlineBatch {
            projects: vec![good],
            deadlines: vec![row],
            ..DeadlineBatch::default()
        })
        .is_err());
    }

    #[test]
    fn malformed_due_instants_fail_the_project_read() {
        let error = validate_deadlines(vec![deadline("not-an-instant", "open")]).unwrap_err();
        assert!(format!("{error:#}").contains("unreadable due_at"));
    }

    fn options(host: &str) -> BriefOptions {
        BriefOptions {
            hosts: BriefHosts::Selected(Some(host.into())),
            project: None,
            due_by: None,
            today: false,
            overdue: false,
            status: "open".into(),
            timezone: Some("America/Los_Angeles".into()),
            as_json: false,
            horizon: 120,
        }
    }

    #[test]
    fn the_console_brief_reports_coverage_bands_and_source_context() {
        let now: DateTime<Utc> = "2028-05-02T18:00:00Z".parse().unwrap();
        let zone = chrono_tz::America::Los_Angeles;
        let base = "https://navigator.example.test";
        let mut unsourced = deadline("2028-05-20T09:00:00-07:00", "open");
        unsourced.title = Some(String::new());
        unsourced.description = None;
        unsourced.authority = String::new();
        unsourced.source_document_id = None;
        unsourced.owner_person_id = None;
        unsourced.category = None;
        let sourced = deadline("2028-05-01T09:00:00-07:00", "open");
        let asset = sourced.source_document_id.unwrap();
        let successful = vec![
            (
                project("synthetic-matter"),
                vec![
                    unsourced,
                    sourced,
                    deadline("2028-05-02T16:00:00-07:00", "open"),
                    deadline("2028-05-05T09:00:00-07:00", "open"),
                    deadline("2028-09-01T09:00:00-07:00", "open"),
                    deadline("2028-05-03T09:00:00-07:00", "complete"),
                ],
            ),
            (project("synthetic-quiet"), Vec::new()),
            (
                project("synthetic-done"),
                vec![deadline("2028-05-03T09:00:00-07:00", "complete")],
            ),
        ];
        let failures = vec![ReadFailure {
            project: project("synthetic-failed"),
            message: "HTTP 503 Service Unavailable: ".into(),
        }];
        let filters = Filters {
            status: "open".into(),
            ..Filters::default()
        };
        let text = render(base, &successful, &failures, &filters, now, zone, false);
        for expected in [
            "Deadline brief · America/Los_Angeles · as of 2028-05-02 11:00 -07:00",
            "Coverage: 3/4 Projects read; 1 empty source reads; 1 reads with no filter matches; 1 failed reads.",
            "  FAILED synthetic-failed: HTTP 503 Service Unavailable: ",
            "Overdue:\n  synthetic-matter · 2028-05-01 09:00 -07:00 · court · Synthetic filing",
            "Today:\n  synthetic-matter · 2028-05-02 16:00 -07:00 · court · Synthetic filing",
            "Next 7 days:\n  synthetic-matter · 2028-05-05 09:00 -07:00",
            "Next 30 days:\n  synthetic-matter · 2028-05-20 09:00 -07:00 · uncategorized · Deadline",
            "Next recorded operative deadline per Project:\n  synthetic-matter · 2028-05-01 09:00 -07:00 · Synthetic filing",
            "Views: overdue 1, today 1, next 7 days 1, next 30 days 1.",
            "unspecified",
            "unassigned",
        ] {
            assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
        }
        assert!(!text.contains("No deadlines match"));
        assert!(text.contains(&asset.to_string()));

        let json = render(base, &successful, &failures, &filters, now, zone, true);
        let report: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(report["complete"], false);
        assert_eq!(report["coverage"]["projects_expected"], 4);
        assert_eq!(report["coverage"]["inventory_projects"], 4);
        assert_eq!(
            report["coverage"]["failed_projects"][0]["project_code"],
            "synthetic-failed"
        );
        assert_eq!(report["deadlines"].as_array().unwrap().len(), 5);
        assert_eq!(
            report["deadlines"][0]["due_local"],
            "2028-05-01 09:00 -07:00"
        );
        assert_eq!(
            report["deadlines"][0]["source_document_href"],
            format!("{base}/app/projects/synthetic-matter/documents/{asset}/download")
        );
        assert_eq!(report["filters"]["status"], "open");
    }

    #[test]
    fn a_project_filter_narrows_coverage_and_an_empty_brief_says_so() {
        let now: DateTime<Utc> = "2028-05-02T18:00:00Z".parse().unwrap();
        let successful = vec![
            (project("synthetic-quiet"), Vec::new()),
            (
                project("synthetic-other"),
                vec![deadline("2028-05-03T09:00:00Z", "open")],
            ),
        ];
        let filters = Filters {
            overdue: true,
            project: Some("synthetic-quiet".into()),
            ..Filters::default()
        };
        let zone = chrono_tz::UTC;
        let text = render("", &successful, &[], &filters, now, zone, false);
        assert!(text.contains("Coverage: 1/1 Projects read; 1 empty source reads"));
        assert!(text.contains("No deadlines match the selected filters."));
        assert!(text.contains("Overdue:\n  none"));
        assert!(!text.contains("synthetic-other"));
        let json = render("", &successful, &[], &filters, now, zone, true);
        let report: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(report["complete"], true);
        assert_eq!(report["filters"]["status"], "open");
        assert_eq!(report["filters"]["project"], "synthetic-quiet");
    }

    #[test]
    fn later_deadlines_fall_outside_every_band_and_unreadable_dates_never_match() {
        let now: DateTime<Utc> = "2028-05-02T12:00:00Z".parse().unwrap();
        let zone = chrono_tz::UTC;
        let later = deadline("2028-09-01T12:00:00Z", "open");
        for (due_at, expected) in [
            ("2028-09-01T12:00:00Z", Some("Later")),
            ("2028-05-02T23:00:00Z", Some("Today")),
            ("2028-05-20T12:00:00Z", Some("Next 30 days")),
            ("soon", None),
        ] {
            assert_eq!(band(&deadline(due_at, "open"), now, zone), expected);
        }
        let open = Filters {
            status: "open".into(),
            ..Filters::default()
        };
        assert!(!selected(&deadline("soon", "open"), &open, now, zone));
        assert_eq!(format_due(&deadline("soon", "open"), zone), "soon");
        let today = Filters {
            today: true,
            ..open
        };
        assert!(!selected(&later, &today, now, zone));
    }

    #[test]
    fn the_zone_is_the_named_one_and_an_unknown_name_is_refused() {
        assert_eq!(
            local_zone(Some("America/Chicago")).unwrap(),
            chrono_tz::America::Chicago
        );
        let error = local_zone(Some("Mars/Olympus")).unwrap_err();
        assert!(error
            .to_string()
            .contains("unknown IANA time zone `Mars/Olympus`"));
    }

    async fn site(inventory: Value) -> wiremock::MockServer {
        use wiremock::matchers::{header, method, path};
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(method("GET"))
            .and(path(INVENTORY_PATH))
            .and(wiremock::matchers::query_param("include_deadlines", "true"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(inventory))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn a_brief_reads_one_batch_and_filters_to_one_project() {
        let (good, bad) = (Uuid::new_v4(), Uuid::new_v4());
        let row = json!({"id": Uuid::new_v4(), "project_id": good, "due_at": "2028-05-03T12:00:00Z", "status": "open"});
        let server = site(json!({"projects": [
            {"id": good, "code": "synthetic-good", "name": "Good", "status": "open"},
            {"id": bad, "code": "synthetic-bad", "name": "Bad", "status": "open"},
        ], "deadlines": [row]}))
        .await;
        let _login = crate::projects::test_login::signed_in(&server.uri()).await;

        assert!(run(options(&server.uri())).await.unwrap());

        let only_good = || BriefOptions {
            project: Some("synthetic-good".into()),
            due_by: Some("2028-06-01".into()),
            as_json: true,
            ..options(&server.uri())
        };
        assert!(run(only_good()).await.unwrap());
        assert_eq!(brief(only_good()).await, std::process::ExitCode::SUCCESS);
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn a_failed_batch_cannot_report_an_all_clear() {
        use wiremock::matchers::{method, path};
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(method("GET"))
            .and(path(INVENTORY_PATH))
            .respond_with(wiremock::ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let _login = crate::projects::test_login::signed_in(&server.uri()).await;
        assert!(run(options(&server.uri())).await.is_err());
        assert_eq!(
            brief(options(&server.uri())).await,
            std::process::ExitCode::FAILURE
        );
    }

    #[tokio::test]
    async fn all_hosts_reports_expired_sessions_and_still_reads_the_active_host() {
        let server = site(json!({"projects": [], "deadlines": []})).await;
        let _login = crate::projects::test_login::signed_in(&server.uri()).await;
        let path = crate::credentials::default_credentials_path();
        let mut credentials = crate::credentials::load(&path).unwrap();
        credentials.set(
            "https://expired.example",
            crate::credentials::HostCredential {
                token: "synthetic".to_owned(),
                person_email: None,
                role: None,
                expires_at: 1,
            },
        );
        crate::credentials::save(&path, &credentials).unwrap();
        for as_json in [false, true] {
            let options = BriefOptions {
                hosts: BriefHosts::AllStored,
                as_json,
                ..options(&server.uri())
            };
            assert!(!run(options).await.unwrap());
        }
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_brief_refuses_bad_input_before_and_after_the_inventory_read() {
        let server = site(json!({"projects": [], "deadlines": []})).await;
        let _login = crate::projects::test_login::signed_in(&server.uri()).await;
        let error = run(BriefOptions {
            due_by: Some("June".into()),
            ..options(&server.uri())
        })
        .await
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("--due-by must be YYYY-MM-DD, got `June`"));
        let error = run(BriefOptions {
            project: Some("synthetic-absent".into()),
            ..options(&server.uri())
        })
        .await
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("no visible open Project has code `synthetic-absent`"));
        let unknown_zone = BriefOptions {
            timezone: Some("Mars/Olympus".into()),
            ..options(&server.uri())
        };
        assert_eq!(brief(unknown_zone).await, std::process::ExitCode::FAILURE);
    }

    #[tokio::test]
    async fn an_unreadable_inventory_or_deadline_body_is_an_error() {
        let server = wiremock::MockServer::start().await;
        let client = reqwest::Client::new();
        let error = list::<Vec<Project>>(&client, format!("{}/absent", server.uri()), "t")
            .await
            .unwrap_err();
        assert!(error.to_string().starts_with("HTTP 404"));
        let malformed = project("synthetic-unreadable");
        let error = partition_batch(DeadlineBatch {
            projects: vec![malformed],
            deadlines: vec![deadline("not-an-instant", "open")],
            ..DeadlineBatch::default()
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("unreadable due_at"));
    }
}
