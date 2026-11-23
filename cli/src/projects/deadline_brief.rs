//! Read-only cross-Project deadline brief for the signed-in lawyer.
//!
//! The inventory door uses the same Firm-scoped open matter set as the lawyer
//! dashboard. Each matter's existing deadline door is then read independently
//! so coverage and partial failures remain visible.

use std::str::FromStr;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use comfy_table::{presets::UTF8_FULL, Cell, ContentArrangement, Table};
use futures::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::remote;

const INVENTORY_PATH: &str = "/app/api/lawyer/deadline-projects";
const READ_CONCURRENCY: usize = 8;

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
}

#[derive(Clone, Debug)]
struct ReadFailure {
    project: Project,
    message: String,
}

fn validate_deadlines(rows: Vec<Deadline>) -> Result<Vec<Deadline>> {
    for row in &rows {
        DateTime::parse_from_rfc3339(&row.due_at)
            .with_context(|| format!("deadline {} has an unreadable due_at value", row.id))?;
    }
    Ok(rows)
}

fn partition_reads(
    reads: Vec<(Project, Result<Vec<Deadline>>)>,
) -> (Vec<(Project, Vec<Deadline>)>, Vec<ReadFailure>) {
    let mut good = Vec::new();
    let mut failed = Vec::new();
    for (project, result) in reads {
        match result {
            Ok(rows) => good.push((project, rows)),
            Err(error) => failed.push(ReadFailure {
                project,
                message: format!("{error:#}"),
            }),
        }
    }
    good.sort_by(|a, b| a.0.code.cmp(&b.0.code));
    failed.sort_by(|a, b| a.project.code.cmp(&b.project.code));
    (good, failed)
}

#[derive(Clone, Debug, Default)]
struct Filters {
    due_by: Option<NaiveDate>,
    today: bool,
    overdue: bool,
    status: String,
    project: Option<String>,
}

fn local_zone(explicit: Option<&str>) -> Result<Tz> {
    let name = explicit
        .map(str::to_owned)
        .or_else(|| std::env::var("TZ").ok())
        .or_else(|| {
            ["/etc/localtime", "/var/db/timezone/localtime"]
                .iter()
                .filter_map(|path| std::fs::read_link(path).ok())
                .filter_map(|path| {
                    path.to_string_lossy()
                        .split("zoneinfo/")
                        .nth(1)
                        .map(str::to_owned)
                })
                .next()
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
    let local_due = due.with_timezone(&zone);
    let today = now.with_timezone(&zone).date_naive();
    if filters.today && local_due.date_naive() != today {
        return false;
    }
    if filters
        .due_by
        .is_some_and(|date| local_due.date_naive() > date)
    {
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
    let day = due.with_timezone(&zone).date_naive();
    let distance = day.signed_duration_since(today).num_days();
    Some(match distance {
        0 => "Today",
        1..=7 => "Next 7 days",
        8..=30 => "Next 30 days",
        _ => "Later",
    })
}

fn source_href(base: &str, project: &Project, deadline: &Deadline) -> String {
    format!(
        "{base}/app/projects/{}/deadlines#deadline-{}",
        project.code, deadline.id
    )
}

fn deadline_value(base: &str, project: &Project, row: &Deadline, zone: Tz) -> Value {
    let parsed = DateTime::parse_from_rfc3339(&row.due_at).ok();
    json!({
        "id": row.id,
        "project_code": project.code,
        "project_name": project.name,
        "project_href": format!("{base}/app/projects/{}", project.code),
        "deadline_href": source_href(base, project, row),
        "title": row.title.as_deref().filter(|title| !title.is_empty()).unwrap_or("Deadline"),
        "due_at": row.due_at,
        "due_local": parsed.map(|at| at.with_timezone(&zone).format("%Y-%m-%d %H:%M %:z").to_string()),
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
    })
}

async fn list<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    url: String,
    token: &str,
) -> Result<T> {
    let response = client.get(url).bearer_auth(token).send().await?;
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

async fn gather(
    base: &str,
    token: &str,
    projects: Vec<Project>,
) -> (Vec<(Project, Vec<Deadline>)>, Vec<ReadFailure>) {
    let client = reqwest::Client::new();
    let reads = stream::iter(projects.into_iter().map(|project| {
        let client = client.clone();
        let token = token.to_owned();
        let url = format!("{base}/app/api/projects/{}/deadlines", project.id);
        async move {
            let result = list::<Vec<Deadline>>(&client, url, &token)
                .await
                .and_then(validate_deadlines);
            (project, result)
        }
    }))
    .buffer_unordered(READ_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;
    partition_reads(reads)
}

fn render(
    base: &str,
    successful: &[(Project, Vec<Deadline>)],
    failures: &[ReadFailure],
    filters: &Filters,
    now: DateTime<Utc>,
    zone: Tz,
    as_json: bool,
) {
    let total = successful.len() + failures.len();
    let selected_projects = successful
        .iter()
        .filter(|(project, _)| {
            filters
                .project
                .as_deref()
                .is_none_or(|code| code == project.code)
        })
        .count()
        + failures
            .iter()
            .filter(|failure| {
                filters
                    .project
                    .as_deref()
                    .is_none_or(|code| code == failure.project.code)
            })
            .count();
    let mut rows = Vec::<(Project, Deadline)>::new();
    let mut empty_reads = 0usize;
    let mut no_filter_matches = 0usize;
    let mut next_by_project = Vec::<(Project, Deadline)>::new();
    for (project, deadlines) in successful {
        if filters
            .project
            .as_deref()
            .is_some_and(|code| code != project.code)
        {
            continue;
        }
        let mut open = deadlines
            .iter()
            .filter(|row| row.status == "open")
            .cloned()
            .collect::<Vec<_>>();
        open.sort_by_key(|row| {
            DateTime::parse_from_rfc3339(&row.due_at)
                .ok()
                .map(|d| d.with_timezone(&Utc))
        });
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
    rows.sort_by(|a, b| {
        let left = DateTime::parse_from_rfc3339(&a.1.due_at)
            .ok()
            .map(|d| d.with_timezone(&Utc));
        let right = DateTime::parse_from_rfc3339(&b.1.due_at)
            .ok()
            .map(|d| d.with_timezone(&Utc));
        left.cmp(&right).then_with(|| a.1.id.cmp(&b.1.id))
    });
    let failure_json = failures
        .iter()
        .filter(|failure| {
            filters
                .project
                .as_deref()
                .is_none_or(|code| code == failure.project.code)
        })
        .map(|failure| json!({"project_code": failure.project.code, "error": failure.message}))
        .collect::<Vec<_>>();
    let matched_project_codes = successful
        .iter()
        .filter(|(project, _)| {
            filters
                .project
                .as_deref()
                .is_none_or(|code| code == project.code)
        })
        .map(|(project, _)| project.code.clone())
        .collect::<Vec<_>>();
    let row_values = rows
        .iter()
        .map(|(project, row)| deadline_value(base, project, row, zone))
        .collect::<Vec<_>>();
    let views = ["Overdue", "Today", "Next 7 days", "Next 30 days"]
        .into_iter()
        .map(|label| {
            let items = rows
                .iter()
                .filter(|(_, row)| row.status == "open" && band(row, now, zone) == Some(label))
                .map(|(project, row)| deadline_value(base, project, row, zone))
                .collect::<Vec<_>>();
            (label.to_owned(), json!(items))
        })
        .collect::<serde_json::Map<_, _>>();
    let next_values = next_by_project
        .iter()
        .map(|(project, row)| deadline_value(base, project, row, zone))
        .collect::<Vec<_>>();
    let report = json!({
        "as_of": now.to_rfc3339(),
        "timezone": zone.name(),
        "complete": failure_json.is_empty() && selected_projects == matched_project_codes.len(),
        "coverage": {
            "projects_expected": selected_projects,
            "projects_read": matched_project_codes.len(),
            "successful_empty_reads": empty_reads,
            "successful_reads_with_no_filter_matches": no_filter_matches,
            "failed_reads": failure_json.len(),
            "failed_projects": failure_json,
            "project_codes_read": matched_project_codes,
            "inventory_projects": total
        },
        "filters": {
            "project": filters.project,
            "due_by": filters.due_by.map(|date| date.to_string()),
            "today": filters.today,
            "overdue": filters.overdue,
            "status": if filters.overdue { "open" } else { filters.status.as_str() }
        },
        "views": views,
        "next_recorded_deadline_per_project": next_values,
        "deadlines": row_values
    });
    if as_json {
        println!("{report}");
        return;
    }
    println!(
        "Deadline brief · {} · as of {}",
        zone.name(),
        now.with_timezone(&zone).format("%Y-%m-%d %H:%M %:z")
    );
    println!(
        "Coverage: {}/{} Projects read; {empty_reads} empty source reads; {no_filter_matches} reads with no filter matches; {} failed reads.",
        matched_project_codes.len(),
        selected_projects,
        failures.len()
    );
    for failure in failures.iter().filter(|failure| {
        filters
            .project
            .as_deref()
            .is_none_or(|code| code == failure.project.code)
    }) {
        println!("  FAILED {}: {}", failure.project.code, failure.message);
    }
    if failures.is_empty() && rows.is_empty() {
        println!("No deadlines match the selected filters.");
    }
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
    for (project, row) in &rows {
        let due = DateTime::parse_from_rfc3339(&row.due_at)
            .map(|date| {
                date.with_timezone(&zone)
                    .format("%Y-%m-%d %H:%M %:z")
                    .to_string()
            })
            .unwrap_or_else(|_| row.due_at.clone());
        let title = row
            .title
            .as_deref()
            .filter(|title| !title.is_empty())
            .unwrap_or("Deadline");
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
        table.add_row([
            Cell::new(&project.code),
            Cell::new(due),
            Cell::new(row.category.as_deref().unwrap_or("uncategorized")),
            Cell::new(title),
            Cell::new(context),
        ]);
    }
    println!("{table}");
    for label in ["Overdue", "Today", "Next 7 days", "Next 30 days"] {
        println!("{label}:");
        let mut found = false;
        for (project, row) in rows
            .iter()
            .filter(|(_, row)| row.status == "open" && band(row, now, zone) == Some(label))
        {
            found = true;
            let due = DateTime::parse_from_rfc3339(&row.due_at)
                .map(|date| {
                    date.with_timezone(&zone)
                        .format("%Y-%m-%d %H:%M %:z")
                        .to_string()
                })
                .unwrap_or_else(|_| row.due_at.clone());
            println!(
                "  {} · {} · {} · {}",
                project.code,
                due,
                row.category.as_deref().unwrap_or("uncategorized"),
                row.title
                    .as_deref()
                    .filter(|title| !title.is_empty())
                    .unwrap_or("Deadline")
            );
        }
        if !found {
            println!("  none");
        }
    }
    println!("Next recorded operative deadline per Project:");
    for (project, row) in &next_by_project {
        let due = DateTime::parse_from_rfc3339(&row.due_at)
            .map(|date| {
                date.with_timezone(&zone)
                    .format("%Y-%m-%d %H:%M %:z")
                    .to_string()
            })
            .unwrap_or_else(|_| row.due_at.clone());
        println!(
            "  {} · {} · {}",
            project.code,
            due,
            row.title.as_deref().unwrap_or("Deadline")
        );
    }
    println!(
        "Views: overdue {}, today {}, next 7 days {}, next 30 days {}.",
        report["views"]["Overdue"].as_array().map_or(0, Vec::len),
        report["views"]["Today"].as_array().map_or(0, Vec::len),
        report["views"]["Next 7 days"]
            .as_array()
            .map_or(0, Vec::len),
        report["views"]["Next 30 days"]
            .as_array()
            .map_or(0, Vec::len)
    );
}

pub async fn brief(
    host: Option<&str>,
    project: Option<&str>,
    due_by: Option<&str>,
    today: bool,
    overdue: bool,
    status: &str,
    timezone: Option<&str>,
    as_json: bool,
) -> std::process::ExitCode {
    match run(
        host, project, due_by, today, overdue, status, timezone, as_json,
    )
    .await
    {
        Ok(complete) if complete => std::process::ExitCode::SUCCESS,
        Ok(_) => std::process::ExitCode::from(1),
        Err(error) => {
            eprintln!("navigator project deadline brief: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(
    host: Option<&str>,
    project: Option<&str>,
    due_by: Option<&str>,
    today: bool,
    overdue: bool,
    status: &str,
    timezone: Option<&str>,
    as_json: bool,
) -> Result<bool> {
    let due_by = due_by
        .map(|value| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .with_context(|| format!("--due-by must be YYYY-MM-DD, got `{value}`"))
        })
        .transpose()?;
    let zone = local_zone(timezone)?;
    let now = Utc::now();
    let filters = Filters {
        due_by,
        today,
        overdue,
        status: status.to_owned(),
        project: project.map(str::to_owned),
    };
    let (base, token) = remote::resolve(host)?;
    let client = reqwest::Client::new();
    let mut projects: Vec<Project> = list(&client, format!("{base}{INVENTORY_PATH}"), &token)
        .await
        .context("read the dashboard-scoped open Project inventory")?;
    if let Some(code) = project {
        projects.retain(|row| row.code == code);
        anyhow::ensure!(
            !projects.is_empty(),
            "no visible open Project has code `{code}`"
        );
    }
    let (successful, failures) = gather(&base, &token, projects).await;
    render(&base, &successful, &failures, &filters, now, zone, as_json);
    Ok(failures.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        }
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
    fn project_reads_keep_successes_and_report_partial_failures() {
        let good = project("synthetic-good");
        let bad = project("synthetic-failed");
        let (read, failed) = partition_reads(vec![
            (good, Ok(Vec::new())),
            (bad, Err(anyhow!("synthetic read failure: HTTP 503"))),
        ]);
        assert_eq!(read.len(), 1);
        assert!(read[0].1.is_empty());
        assert_eq!(failed.len(), 1);
        assert!(failed[0].message.contains("503"));
    }

    #[test]
    fn malformed_due_instants_fail_the_project_read() {
        let error = validate_deadlines(vec![deadline("not-an-instant", "open")]).unwrap_err();
        assert!(format!("{error:#}").contains("unreadable due_at"));
    }
}
