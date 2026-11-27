//! `navigator project deadline` — list, create, update, complete, cancel, and reopen
//! a Project Deadline through the portal API. Each write prints the row the
//! store read back. `ics` prints the whole Project as one calendar.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    projects::{ics, manifest, surfaces},
    remote,
};

pub(super) async fn coordinates(
    host: Option<&str>,
    project: Option<&str>,
) -> Result<(String, String, Uuid)> {
    let (base, token, id, _) = coordinates_with_code(host, project).await?;
    Ok((base, token, id))
}

async fn coordinates_with_code(
    host: Option<&str>,
    project: Option<&str>,
) -> Result<(String, String, Uuid, String)> {
    let local = manifest::read(Path::new("."));
    let project = project
        .or_else(|| local.as_ref().and_then(|m| m.project.as_deref()))
        .ok_or_else(|| anyhow!("pass --project or run from a Project checkout"))?;
    let host = host.or_else(|| local.as_ref().and_then(|m| m.host.as_deref()));
    let (base, token) = remote::resolve(host)?;
    let id = surfaces::resolve_project_id(&base, &token, project).await?;
    Ok((base, token, id, project.to_string()))
}

pub(super) async fn call(request: reqwest::RequestBuilder, action: &str) -> Result<Value> {
    let response = request
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .with_context(|| format!("{action} deadline"))?;
    let status = response.status();
    let body = response.text().await.context("read deadline response")?;
    if !status.is_success() {
        return Err(anyhow!("deadline {action} refused: {status} {body}"));
    }
    serde_json::from_str(&body).context("parse deadline response")
}

fn annotate_row(row: &mut Value) {
    if let Some(rows) = row.as_array_mut() {
        for row in rows {
            annotate_row(row);
        }
    } else if let Some(object) = row.as_object_mut() {
        if let Some(category) = object.get("category") {
            object.insert("category_review_required".into(), json!(category.is_null()));
        }
        if let Some(lead) = object.get("client_lead_days") {
            let visibility = if lead.is_null() { "internal" } else { "client" };
            object.insert("visibility".into(), json!(visibility));
        }
    }
}

pub(super) fn finish(result: Result<Value>) -> ExitCode {
    match result {
        Ok(mut row) => {
            annotate_row(&mut row);
            println!("{row}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator project deadline: {error:#}");
            ExitCode::FAILURE
        }
    }
}

pub async fn list(host: Option<&str>, project: Option<&str>) -> ExitCode {
    finish(
        async {
            let (base, token, id) = coordinates(host, project).await?;
            let mut rows = call(
                reqwest::Client::new()
                    .get(format!("{base}/app/api/projects/{id}/deadlines"))
                    .bearer_auth(token),
                "list",
            )
            .await?;
            group_trial_series(&mut rows);
            Ok(rows)
        }
        .await,
    )
}

fn group_trial_series(rows: &mut Value) {
    let Some(rows) = rows.as_array_mut() else {
        return;
    };
    let mut series = std::collections::BTreeMap::<String, Vec<Value>>::new();
    for row in rows.iter() {
        if let Some(id) = row["hearing"]["appearance"]["day"]["series_id"].as_str() {
            series.entry(id.to_owned()).or_default().push(row.clone());
        }
    }
    rows.retain_mut(|row| {
        let Some(id) = row["hearing"]["appearance"]["day"]["series_id"].as_str() else {
            return true;
        };
        let Some(mut days) = series.remove(id) else {
            return false;
        };
        days.sort_by_key(|day| day["hearing"]["appearance"]["day"]["number"].as_u64());
        row["trial_days"] = json!(days);
        true
    });
}

/// Print the Project's Deadlines as one RFC 5545 `VCALENDAR`. Nothing is
/// written: the calendar is rendered from the rows the portal returns.
pub async fn ics(host: Option<&str>, project: Option<&str>) -> ExitCode {
    let rendered = async {
        let (base, token, id, code) = coordinates_with_code(host, project).await?;
        let rows = call(
            reqwest::Client::new()
                .get(format!("{base}/app/api/projects/{id}/deadlines"))
                .bearer_auth(token),
            "list",
        )
        .await?;
        let rows = rows
            .as_array()
            .ok_or_else(|| anyhow!("deadline list is not an array"))?;
        let uid_host = reqwest::Url::parse(&base)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
            .unwrap_or_else(|| "navigator".into());
        ics::render(
            &ics::Calendar {
                host: &uid_host,
                base_url: &base,
                project_code: &code,
            },
            rows,
        )
        .map_err(|error| anyhow!(error))
    }
    .await;
    match rendered {
        Ok(calendar) => {
            print!("{calendar}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator project deadline: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Reconcile the Project's mirrored Google Calendar events to its Deadline
/// rows through the portal door — the portal owns the Firm's calendar
/// credential. `dry_run` plans the same writes without making them.
pub async fn sync(host: Option<&str>, project: Option<&str>, dry_run: bool) -> ExitCode {
    let mirrored: anyhow::Result<()> = async {
        let (base, token, _, code) = coordinates_with_code(host, project).await?;
        let report = call(
            reqwest::Client::new()
                .post(format!(
                    "{base}/app/api/integrations/calendar/deadline-mirror"
                ))
                .bearer_auth(token)
                .json(&json!({
                    "project_code": code,
                    "all": false,
                    "dry_run": dry_run,
                })),
            "sync",
        )
        .await?;
        let results = report["results"]
            .as_array()
            .ok_or_else(|| anyhow!("mirror report is not an array"))?;
        let mut accepted = !results.is_empty();
        for result in results {
            let outcome = result["outcome"].as_str().unwrap_or("?");
            accepted &= matches!(outcome, "mirrored" | "planned" | "skipped_closed");
            let line = match result["detail"].as_str() {
                Some(detail) => format!(" — {detail}"),
                None => String::new(),
            };
            let code = result["project_code"].as_str().unwrap_or("?");
            println!("{code}: {outcome}{line}");
            if let Some(remedy) = sync_remedy(outcome, code) {
                eprintln!("{code}: {remedy}");
            }
        }
        anyhow::ensure!(
            accepted,
            "calendar mirror did not report a successful outcome"
        );
        Ok(())
    }
    .await;
    match mirrored {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator project deadline: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// The next step for a sync outcome that wrote nothing, or not everything.
/// A retry is always safe: each event carries its Deadline id, so a second
/// pass patches what the first one wrote instead of duplicating it.
fn sync_remedy(outcome: &str, code: &str) -> Option<String> {
    let retry = format!("navigator project deadline sync --project {code}");
    Some(match outcome {
        "calendar_unprovisioned" => format!(
            "the Project has no calendar yet; choose the Firm's Workspace calendar owner and \
             authorize its calendar delegation (docs/third-party-integrations.md), then `{retry}`"
        ),
        "provider_unauthorized" => format!(
            "Google Workspace refused the Firm's calendar owner; check its domain-wide \
             delegation for the calendar scope, then `{retry}`"
        ),
        "provider_unavailable" | "mirror_incomplete" => {
            format!("nothing was duplicated; `{retry}` again to finish the writes")
        }
        "recorded_resource_missing" => format!(
            "preserve the recorded calendar ID and check the delegated owner's access with \
             `{retry} --dry-run`"
        ),
        _ => return None,
    })
}

fn apply_visibility(
    object: &mut serde_json::Map<String, Value>,
    visibility: Option<&str>,
    client_lead: Option<i64>,
    creating: bool,
) -> Result<()> {
    match visibility {
        Some("internal") => {
            object.insert("client_lead_days".into(), Value::Null);
        }
        Some("client") if client_lead.is_none() => {
            return Err(anyhow!(
                "client visibility requires --client-lead-days and --internal-lead-days"
            ))
        }
        None if creating => {
            object.insert("client_lead_days".into(), Value::Null);
        }
        Some("client") | None => {}
        Some(_) => return Err(anyhow!("visibility must be internal or client")),
    }
    Ok(())
}

/// The `category` a create or update body carries. An empty value clears it;
/// the portal settles a cleared category from the calendar role and authority
/// kind on create and refuses when neither does. Validation fails fast here
/// so a typo never reaches the store.
fn category_field(category: Option<&str>) -> Result<Option<String>> {
    let category = category.map(str::trim).filter(|value| !value.is_empty());
    if category.is_some_and(|value| !model::project_deadlines::CATEGORIES.contains(&value)) {
        return Err(anyhow!(
            "--category must be one of {}",
            model::project_deadlines::CATEGORIES.join(", ")
        ));
    }
    Ok(category.map(str::to_string))
}

fn insert<T: serde::Serialize>(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Option<T>,
) {
    if let Some(value) = value {
        object.insert(key.into(), json!(value));
    }
}

fn export_row(base: &str, code: &str, row: &Value, location: Option<&str>) -> Result<String> {
    let url = reqwest::Url::parse(base)?;
    let mut row = row.clone();
    if let Some(location) = location {
        row["location"] = json!(location);
    }
    ics::render(
        &ics::Calendar {
            host: url.host_str().unwrap_or("navigator"),
            base_url: base,
            project_code: code,
        },
        &[row],
    )
    .map_err(|error| anyhow!(error))
}

fn finish_write(
    row: Value,
    base: &str,
    code: &str,
    path: Option<&Path>,
    location: Option<&str>,
) -> Result<Value> {
    if let Some(path) = path {
        let calendar = export_row(base, code, &row, location)?;
        std::fs::write(path, calendar).with_context(|| {
            format!(
                "deadline {} was saved, but calendar export to {} failed",
                row["id"],
                path.display()
            )
        })?;
    }
    Ok(row)
}

/// Replay keys identify a row across edits. Confirm this payload as well as
/// its identity so a lost update cannot be mistaken for an earlier create.
fn matches_create(row: &Value, project: Uuid, key: &str, body: &Value) -> bool {
    let timestamp = |value: &Value| {
        value
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s.trim()).ok())
    };
    row["project_id"] == json!(project)
        && row["replay_key"] == key
        && row["status"] != "cancelled"
        && row["id"].as_str().is_some_and(|id| !id.is_empty())
        && body.as_object().is_some_and(|fields| {
            fields.iter().all(|(field, requested)| {
                let stored = &row[field];
                match (field.as_str(), requested.as_str().map(str::trim)) {
                    ("due_at", _) => {
                        row["due_on"].is_null()
                            && timestamp(requested).is_some()
                            && timestamp(requested) == timestamp(stored)
                    }
                    ("due_on", _) => {
                        let Ok(saved) = serde_json::from_value::<model::project_deadlines::DueOn>(
                            stored.clone(),
                        ) else {
                            return false;
                        };
                        serde_json::from_value::<model::project_deadlines::DueOnInput>(
                            requested.clone(),
                        )
                        .is_ok_and(|input| {
                            input.date == saved.date
                                && input.cutoff == saved.cutoff
                                && input.timezone.is_none_or(|zone| zone == saved.timezone)
                        })
                    }
                    ("description", Some("")) => stored.is_null(),
                    ("source", Some("")) => stored == "lawyer",
                    (_, Some(text)) => stored == text,
                    _ => stored == requested,
                }
            })
        })
}

/// A response can disappear after the transaction commits. Both attempts use
/// the same Project URL and normalized key, including the final read-back.
async fn create_confirmed(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    project: Uuid,
    body: &Value,
    key: &str,
    timeout: Duration,
) -> Result<Value> {
    for attempt in 0..2 {
        let response = client
            .post(url)
            .bearer_auth(token)
            .json(body)
            .timeout(timeout)
            .send()
            .await;
        if let Ok(response) = response {
            let status = response.status();
            if status.is_success() {
                if let Ok(row) = response.json::<Value>().await {
                    if row["id"].as_str().is_some_and(|id| !id.is_empty()) {
                        return Ok(row);
                    }
                }
            } else if !status.is_server_error() && status != reqwest::StatusCode::REQUEST_TIMEOUT {
                let detail = response.text().await.unwrap_or_default();
                if attempt == 0 {
                    anyhow::bail!("deadline create refused: {status} {detail}");
                }
                // An earlier request might still have committed.
            }
        }
        if let Ok(rows) = call(client.get(url).bearer_auth(token), "verify create").await {
            if let Some(row) = rows.as_array().and_then(|rows| {
                rows.iter()
                    .find(|row| matches_create(row, project, key, body))
            }) {
                return Ok(row.clone());
            }
        }
    }
    anyhow::bail!(
        "deadline create unconfirmed for Project {project}; retry with --replay-key {key}"
    )
}

pub(crate) fn effort_minutes(input: &str) -> std::result::Result<i32, String> {
    if input == "0" {
        return Ok(0);
    }
    let units = [("m", 1), ("h", 60), ("d", 480)];
    units
        .into_iter()
        .find_map(|(suffix, minutes)| {
            input.strip_suffix(suffix).and_then(|value| {
                value
                    .parse::<i32>()
                    .ok()
                    .filter(|value| *value >= 0)
                    .and_then(|value| value.checked_mul(minutes))
            })
        })
        .ok_or_else(|| {
            "effort must be a nonnegative whole duration such as 30m, 4h, 3d, or 0".into()
        })
}

pub(crate) fn hearing_details(
    duration_minutes: Option<std::num::NonZeroU32>,
    location: Option<String>,
) -> Option<model::project_deadlines::Hearing> {
    (duration_minutes.is_some() || location.is_some()).then_some(
        model::project_deadlines::Hearing {
            duration_minutes,
            location,
            ..Default::default()
        },
    )
}

#[derive(Default)]
pub struct DepositionOptions {
    pub role: Option<model::project_deadlines::DeponentRole>,
    pub taken_by: Option<model::project_deadlines::TakenBy>,
    pub confirm: bool,
    pub prep_file: Option<std::path::PathBuf>,
}

fn prepare_deposition(
    options: DepositionOptions,
    hearing: &mut Option<model::project_deadlines::Hearing>,
    due_at: Option<&str>,
    category: Option<&str>,
) -> Result<Option<Value>> {
    use model::project_deadlines::{Appearance, Category, PreparationDeadline};
    if category != Some("deposition") {
        anyhow::ensure!(
            options.role.is_none() && !options.confirm && options.prep_file.is_none(),
            "use deposition options with --category deposition"
        );
        return Ok(None);
    }
    let start =
        chrono::DateTime::parse_from_rfc3339(due_at.context("a deposition requires --due-at")?)?;
    let details = hearing
        .as_mut()
        .context("specify --hearing-minutes and --hearing-location")?;
    details.appearance = Some(Appearance::Deposition {
        deponent_role: options.role.context("specify --deponent-role")?,
        taken_by: options.taken_by.context("specify --taken-by")?,
    });
    let prep: Vec<PreparationDeadline> = if let Some(path) = options.prep_file {
        serde_json::from_slice(&std::fs::read(path).context("read preparation Deadlines")?)
            .context("parse preparation Deadlines")?
    } else {
        details.preparation(start)
    };
    details.prep = Some(prep);
    details
        .validate_appearance(Some(Category::Deposition), start)
        .map_err(|error| anyhow!(error))?;
    Ok((!options.confirm).then(|| {
        json!({
            "status": "proposed",
            "due_at": start,
            "hearing": details,
            "next": "Use --confirm to save. Use --prep-file to edit or drop proposed preparation.",
        })
    }))
}

#[allow(clippy::too_many_arguments)]
pub async fn create(
    host: Option<&str>,
    project: Option<&str>,
    title: Option<String>,
    description: Option<String>,
    visibility: Option<String>,
    due_at: Option<String>,
    due_on: Option<model::project_deadlines::DueOnInput>,
    mut hearing: Option<model::project_deadlines::Hearing>,
    deposition: DepositionOptions,
    effort_minutes: Option<i32>,
    authority_kind: String,
    authority: Option<String>,
    source: Option<String>,
    source_document: Option<Uuid>,
    owner: Option<Uuid>,
    calendar_role: String,
    category: Option<String>,
    internal_lead_days: Option<i64>,
    client_lead_days: Option<i64>,
    replay_key: Option<String>,
    ics_path: Option<&Path>,
    location: Option<&str>,
) -> ExitCode {
    finish(
        async {
            if title.as_deref().is_none_or(|title| title.trim().is_empty()) {
                return Err(anyhow!("--title must name what is due"));
            }
            if let Some(preview) = prepare_deposition(
                deposition,
                &mut hearing,
                due_at.as_deref(),
                category.as_deref(),
            )? {
                return Ok(preview);
            }
            let (base, token, id, code) = coordinates_with_code(host, project).await?;
            let mut body = serde_json::Map::new();
            let object = &mut body;
            insert(object, "due_at", due_at);
            insert(object, "due_on", due_on);
            insert(object, "hearing", hearing);
            insert(object, "effort_minutes", effort_minutes);
            insert(object, "authority_kind", Some(authority_kind));
            insert(object, "calendar_role", Some(calendar_role));
            insert(object, "title", title);
            insert(object, "description", description);
            insert(object, "category", category_field(category.as_deref())?);
            insert(object, "authority", authority);
            insert(object, "source", source);
            insert(object, "source_document_id", source_document);
            insert(object, "owner_person_id", owner);
            insert(object, "internal_lead_days", internal_lead_days);
            insert(object, "client_lead_days", client_lead_days);
            let key = replay_key
                .as_deref()
                .map(str::trim)
                .filter(|key| !key.is_empty())
                .map_or_else(|| Uuid::now_v7().to_string(), str::to_owned);
            insert(object, "replay_key", Some(&key));
            apply_visibility(object, visibility.as_deref(), client_lead_days, true)?;
            let row = create_confirmed(
                &reqwest::Client::new(),
                &format!("{base}/app/api/projects/{id}/deadlines"),
                &token,
                id,
                &Value::Object(body),
                &key,
                Duration::from_secs(30),
            )
            .await?;
            finish_write(row, &base, &code, ics_path, location)
        }
        .await,
    )
}

#[allow(clippy::too_many_arguments)]
pub async fn update(
    host: Option<&str>,
    project: Option<&str>,
    id: Uuid,
    title: Option<String>,
    description: Option<String>,
    visibility: Option<String>,
    due_at: Option<String>,
    due_on: Option<model::project_deadlines::DueOnInput>,
    hearing: Option<model::project_deadlines::Hearing>,
    effort_minutes: Option<i32>,
    authority_kind: Option<String>,
    authority: Option<String>,
    source: Option<String>,
    source_document: Option<Uuid>,
    owner: Option<Uuid>,
    calendar_role: Option<String>,
    category: Option<String>,
    internal_lead_days: Option<i64>,
    client_lead_days: Option<i64>,
    ics_path: Option<&Path>,
    location: Option<&str>,
) -> ExitCode {
    finish(
        async {
            let (base, token, project_id, code) = coordinates_with_code(host, project).await?;
            let mut body = serde_json::Map::new();
            let object = &mut body;
            insert(object, "title", title);
            insert(object, "due_at", due_at);
            insert(object, "due_on", due_on);
            insert(object, "hearing", hearing);
            insert(object, "effort_minutes", effort_minutes);
            insert(object, "authority_kind", authority_kind);
            insert(object, "authority", authority);
            insert(object, "source", source);
            insert(object, "source_document_id", source_document);
            insert(object, "owner_person_id", owner);
            insert(object, "calendar_role", calendar_role);
            // Absent flag preserves the stored category; an empty value
            // clears it. The portal's double-Option carries the difference.
            if let Some(raw) = category.as_deref() {
                object.insert("category".into(), json!(category_field(Some(raw))?));
            }
            insert(object, "internal_lead_days", internal_lead_days);
            insert(object, "client_lead_days", client_lead_days);
            insert(object, "description", description);
            apply_visibility(object, visibility.as_deref(), client_lead_days, false)?;
            let row = call(
                reqwest::Client::new()
                    .patch(format!(
                        "{base}/app/api/projects/{project_id}/deadlines/{id}"
                    ))
                    .bearer_auth(token)
                    .json(&body),
                "update",
            )
            .await?;
            finish_write(row, &base, &code, ics_path, location)
        }
        .await,
    )
}

pub async fn complete(host: Option<&str>, project: Option<&str>, id: Uuid) -> ExitCode {
    transition(host, project, id, "complete").await
}

pub async fn cancel(host: Option<&str>, project: Option<&str>, id: Uuid) -> ExitCode {
    transition(host, project, id, "cancel").await
}

pub async fn reopen(host: Option<&str>, project: Option<&str>, id: Uuid) -> ExitCode {
    transition(host, project, id, "reopen").await
}

async fn transition(host: Option<&str>, project: Option<&str>, id: Uuid, verb: &str) -> ExitCode {
    finish(
        async {
            let (base, token, project_id) = coordinates(host, project).await?;
            post_transition(&base, &token, project_id, id, verb).await
        }
        .await,
    )
}

/// `POST …/deadlines/{id}/{complete|cancel|reopen}`. The handler takes no body, but
/// the production load balancer answers a bodyless POST with
/// `411 Length Required`, so the request says `Content-Length: 0` outright.
async fn post_transition(
    base: &str,
    token: &str,
    project_id: Uuid,
    id: Uuid,
    verb: &str,
) -> Result<Value> {
    call(
        reqwest::Client::new()
            .post(format!(
                "{base}/app/api/projects/{project_id}/deadlines/{id}/{verb}"
            ))
            .bearer_auth(token)
            .header(reqwest::header::CONTENT_LENGTH, "0")
            .body(Vec::<u8>::new()),
        verb,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::{annotate_row, apply_visibility, export_row, finish_write, post_transition};
    use uuid::Uuid;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A front end that refuses a POST without `Content-Length`, the way the
    /// production load balancer does, must still let every transition through.
    #[tokio::test]
    async fn transitions_pass_a_front_end_that_requires_content_length() {
        let server = MockServer::start().await;
        let project_id = Uuid::new_v4();
        let id = Uuid::new_v4();
        for verb in ["complete", "cancel", "reopen"] {
            let route = format!("/app/api/projects/{project_id}/deadlines/{id}/{verb}");
            Mock::given(method("POST"))
                .and(path(route.clone()))
                .and(header("content-length", "0"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({ "status": verb })),
                )
                .with_priority(1)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path(route))
                .respond_with(ResponseTemplate::new(411).set_body_string("Length Required"))
                .with_priority(10)
                .mount(&server)
                .await;
        }

        for verb in ["complete", "cancel", "reopen"] {
            let row = post_transition(&server.uri(), "a-token", project_id, id, verb)
                .await
                .unwrap_or_else(|error| panic!("{verb}: {error:#}"));
            assert_eq!(row["status"], verb);
        }
    }
    #[test]
    fn row_export_preserves_the_event_and_ephemeral_location() {
        let row = serde_json::json!({"id":"deadline-1", "due_at":"2026-11-02T17:00:00-08:00", "updated_at":"2026-10-26T00:00:00Z", "status":"open", "title":"Response"});
        let calendar = export_row(
            "https://example.test",
            "sample",
            &row,
            Some("https://court.test/join?a=1,b=2"),
        )
        .unwrap();
        assert!(calendar.contains("UID:deadline-1@example.test\r\n"));
        assert!(calendar.contains("LOCATION:https://court.test/join?a=1\\,b=2\r\n"));
        assert_eq!(calendar.matches("BEGIN:VEVENT").count(), 1);
        assert!(row.get("location").is_none());
    }
    #[test]
    fn failed_export_reports_that_the_deadline_was_saved() {
        let row = serde_json::json!({"id":"saved-row", "due_at":"2026-11-02T17:00:00-08:00"});
        let directory = tempfile::tempdir().unwrap();
        let missing_parent = directory.path().join("missing/event.ics");
        let error = finish_write(
            row,
            "https://example.test",
            "sample",
            Some(&missing_parent),
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("saved-row"));
        assert!(error.to_string().contains("was saved"));
    }
    #[test]
    fn visibility_is_a_projection_of_the_client_lead() {
        let mut body = serde_json::json!({"client_lead_days": 3});
        apply_visibility(
            body.as_object_mut().unwrap(),
            Some("internal"),
            Some(3),
            false,
        )
        .unwrap();
        assert!(body["client_lead_days"].is_null());
        annotate_row(&mut body);
        assert_eq!(body["visibility"], "internal");
        assert!(
            apply_visibility(body.as_object_mut().unwrap(), Some("client"), None, false).is_err()
        );
        let mut client = serde_json::json!({"client_lead_days": 3});
        apply_visibility(
            client.as_object_mut().unwrap(),
            Some("client"),
            Some(3),
            true,
        )
        .unwrap();
        annotate_row(&mut client);
        assert_eq!(client["visibility"], "client");
        apply_visibility(client.as_object_mut().unwrap(), None, Some(3), true).unwrap();
        assert!(client["client_lead_days"].is_null());
        let error = apply_visibility(client.as_object_mut().unwrap(), Some("public"), None, true)
            .unwrap_err();
        assert_eq!(error.to_string(), "visibility must be internal or client");
    }

    use crate::projects::test_login::{answer, site};
    use serde_json::json;
    use std::process::ExitCode;
    use wiremock::matchers::body_json;

    const CODE: &str = "sample-matter";

    #[test]
    fn civil_date_recovery_accepts_the_inherited_zone_and_preserves_the_due_kind() {
        let project = Uuid::now_v7();
        let row = json!({"id":"saved", "project_id":project, "replay_key":"retry",
            "due_at":"2030-07-17T04:00:00Z",
            "due_on":{"date":"2030-07-16","timezone":"America/New_York"}});
        let matches = |body| super::matches_create(&row, project, "retry", &body);
        assert!(matches(json!({"due_on":{"date":"2030-07-16"}})));
        assert!(!matches(
            json!({"due_on":{"date":"2030-07-16","timezone":"America/Chicago"}})
        ));
        assert!(!matches(json!({"due_at":"2030-07-17T04:00:00Z"})));
    }

    async fn matter() -> (MockServer, crate::projects::test_login::Login, String) {
        let id = Uuid::new_v4();
        let (server, login) = site(json!([{"id": id, "code": CODE}])).await;
        (server, login, format!("/app/api/projects/{id}/deadlines"))
    }

    #[tokio::test]
    async fn list_and_ics_read_the_project_rows_and_refuse_a_failed_read() {
        let (server, _login, door) = matter().await;
        let host = server.uri();
        let host = Some(host.as_str());
        assert_eq!(super::list(host, Some(CODE)).await, ExitCode::FAILURE);
        assert_eq!(super::list(host, None).await, ExitCode::FAILURE);
        let rows = json!([{
            "id": "deadline-1", "due_at": "2026-11-02T17:00:00-08:00",
            "updated_at": "2026-10-26T00:00:00Z", "status": "open", "title": "Response",
            "category": null, "client_lead_days": null,
        }]);
        answer(&server, "GET", &door, 200, rows).await;
        assert_eq!(super::list(host, Some(CODE)).await, ExitCode::SUCCESS);
        assert_eq!(super::ics(host, Some(CODE)).await, ExitCode::SUCCESS);
        assert_eq!(super::ics(host, Some("absent")).await, ExitCode::FAILURE);
    }

    #[tokio::test]
    async fn ics_refuses_a_deadline_list_that_is_not_an_array() {
        let (server, _login, door) = matter().await;
        answer(&server, "GET", &door, 200, json!({"rows": []})).await;
        let host = server.uri();
        assert_eq!(super::ics(Some(&host), Some(CODE)).await, ExitCode::FAILURE);
    }

    #[tokio::test]
    async fn ambiguous_create_reconciles_the_project_key_before_retrying() {
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        let id = Uuid::new_v4();
        let project = Uuid::new_v4();
        let client = reqwest::Client::new();
        let row = json!({"id": id, "project_id": project, "replay_key": "retry"});
        let body = json!({"replay_key": "retry"});
        for commit_first in [true, false] {
            let server = MockServer::start().await;
            let url = format!("{}/app/api/projects/{project}/deadlines", server.uri());
            let rows = Arc::new(Mutex::new(Vec::new()));
            let stored = rows.clone();
            let saved = row.clone();
            Mock::given(method("POST"))
                .and(body_json(body.clone()))
                .respond_with(move |_: &wiremock::Request| {
                    if commit_first {
                        stored.lock().unwrap().push(saved.clone());
                    }
                    ResponseTemplate::new(201).set_delay(Duration::from_secs(2))
                })
                .up_to_n_times(1)
                .mount(&server)
                .await;
            let stored = rows.clone();
            let saved = row.clone();
            Mock::given(method("POST"))
                .and(body_json(body.clone()))
                .respond_with(move |_: &wiremock::Request| {
                    let mut rows = stored.lock().unwrap();
                    if rows.is_empty() {
                        rows.push(saved.clone());
                    }
                    ResponseTemplate::new(201).set_body_json(&rows[0])
                })
                .mount(&server)
                .await;
            let stored = rows.clone();
            Mock::given(method("GET"))
                .respond_with(move |_: &wiremock::Request| {
                    ResponseTemplate::new(200).set_body_json(&*stored.lock().unwrap())
                })
                .mount(&server)
                .await;
            let create = || {
                super::create_confirmed(
                    &client,
                    &url,
                    "token",
                    project,
                    &body,
                    "retry",
                    Duration::from_millis(200),
                )
            };
            assert_eq!(create().await.unwrap(), row);
            assert_eq!(rows.lock().unwrap().len(), 1);
            let requests = server.received_requests().await.unwrap();
            assert_eq!(requests.len(), if commit_first { 2 } else { 3 });
            let (first, second) = tokio::join!(create(), create());
            assert_eq!(first.unwrap(), row);
            assert_eq!(second.unwrap(), row);
            assert_eq!(rows.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn unavailable_verification_returns_the_same_key_after_two_attempts() {
        let server = MockServer::start().await;
        let project = Uuid::now_v7();
        let url = format!("{}/app/api/projects/{project}/deadlines", server.uri());
        let body = json!({"replay_key":"retry","title":"Synthetic deadline"});
        Mock::given(method("POST"))
            .and(body_json(&body))
            .respond_with(ResponseTemplate::new(504))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .expect(2)
            .mount(&server)
            .await;
        let error = super::create_confirmed(
            &reqwest::Client::new(),
            &url,
            "token",
            project,
            &body,
            "retry",
            std::time::Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("unconfirmed"));
        assert!(error.to_string().contains("--replay-key retry"));
    }

    #[tokio::test]
    async fn create_sends_only_supplied_fields_and_exports_the_saved_row() {
        let (server, _login, door) = matter().await;
        let (document, owner) = (Uuid::new_v4(), Uuid::new_v4());
        Mock::given(method("POST"))
            .and(path(door))
            .and(body_json(json!({
                "due_at": "2026-11-02T17:00:00-08:00",
                "authority_kind": "contract",
                "calendar_role": "firm",
                "title": "Response",
                "description": "Served by mail",
                "category": "internal",
                "authority": "Synthetic agreement",
                "source": "lawyer",
                "source_document_id": document,
                "owner_person_id": owner,
                "internal_lead_days": 7,
                "client_lead_days": 3,
                "replay_key": "replay-1",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "deadline-1", "due_at": "2026-11-02T17:00:00-08:00",
                "updated_at": "2026-10-26T00:00:00Z", "status": "open", "title": "Response",
            })))
            .mount(&server)
            .await;
        let host = server.uri();
        let directory = tempfile::tempdir().unwrap();
        let calendar = directory.path().join("event.ics");
        let create = |title: Option<&str>, category: &str, visibility: &str| {
            super::create(
                Some(&host),
                Some(CODE),
                title.map(str::to_owned),
                Some("Served by mail".into()),
                Some(visibility.into()),
                Some("2026-11-02T17:00:00-08:00".into()),
                None,
                None,
                super::DepositionOptions::default(),
                None,
                "contract".into(),
                Some("Synthetic agreement".into()),
                Some("lawyer".into()),
                Some(document),
                Some(owner),
                "firm".into(),
                Some(category.into()),
                Some(7),
                Some(3),
                Some("replay-1".into()),
                Some(&calendar),
                Some("Courtroom 4"),
            )
        };
        assert_eq!(
            create(Some("  "), "internal", "client").await,
            ExitCode::FAILURE
        );
        assert_eq!(
            create(Some("Response"), "nonsense", "client").await,
            ExitCode::FAILURE
        );
        assert_eq!(
            create(Some("Response"), "internal", "public").await,
            ExitCode::FAILURE
        );
        assert!(!calendar.exists());
        assert_eq!(
            create(Some("Response"), "internal", "client").await,
            ExitCode::SUCCESS
        );
        let exported = std::fs::read_to_string(&calendar).unwrap();
        assert!(exported.contains("LOCATION:Courtroom 4\r\n"), "{exported}");
    }

    #[tokio::test]
    async fn update_patches_named_fields_and_an_empty_category_clears_it() {
        let (server, _login, door) = matter().await;
        let id = Uuid::new_v4();
        let (document, owner) = (Uuid::new_v4(), Uuid::new_v4());
        Mock::given(method("PATCH"))
            .and(path(format!("{door}/{id}")))
            .and(body_json(json!({
                "title": "Reply",
                "due_at": "2026-11-09T17:00:00-08:00",
                "authority_kind": "rule",
                "authority": "Local rule 7",
                "source": "lawyer",
                "effort_minutes": 45,
                "source_document_id": document,
                "owner_person_id": owner,
                "calendar_role": "client",
                "category": null,
                "internal_lead_days": 5,
                "client_lead_days": null,
                "description": "Moved by stipulation",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": id})))
            .mount(&server)
            .await;
        let host = server.uri();
        let update = |category: &str| {
            super::update(
                Some(&host),
                Some(CODE),
                id,
                Some("Reply".into()),
                Some("Moved by stipulation".into()),
                Some("internal".into()),
                Some("2026-11-09T17:00:00-08:00".into()),
                None,
                None,
                Some(45),
                Some("rule".into()),
                Some("Local rule 7".into()),
                Some("lawyer".into()),
                Some(document),
                Some(owner),
                Some("client".into()),
                Some(category.into()),
                Some(5),
                Some(2),
                None,
                None,
            )
        };
        assert_eq!(update("").await, ExitCode::SUCCESS);
        assert_eq!(update("nonsense").await, ExitCode::FAILURE);
    }

    #[tokio::test]
    async fn each_transition_verb_posts_to_its_own_door() {
        let (server, _login, door) = matter().await;
        let id = Uuid::new_v4();
        let host = server.uri();
        let host = Some(host.as_str());
        for verb in ["complete", "cancel"] {
            let route = format!("{door}/{id}/{verb}");
            answer(&server, "POST", &route, 200, json!({"status": verb})).await;
        }
        assert_eq!(
            super::complete(host, Some(CODE), id).await,
            ExitCode::SUCCESS
        );
        assert_eq!(super::cancel(host, Some(CODE), id).await, ExitCode::SUCCESS);
        assert_eq!(super::reopen(host, Some(CODE), id).await, ExitCode::FAILURE);
    }

    #[tokio::test]
    async fn sync_succeeds_only_when_every_result_is_an_accepted_outcome() {
        let (server, _login, _door) = matter().await;
        let host = server.uri();
        let door = "/app/api/integrations/calendar/deadline-mirror";
        assert_eq!(
            super::sync(Some(&host), Some(CODE), true).await,
            ExitCode::FAILURE
        );
        for (results, expected) in [
            (json!({"results": {}}), ExitCode::FAILURE),
            (json!({"results": []}), ExitCode::FAILURE),
            (
                json!({"results": [
                    {"project_code": CODE, "outcome": "mirrored", "detail": "2 events"},
                    {"project_code": CODE, "outcome": "calendar_unprovisioned"},
                ]}),
                ExitCode::FAILURE,
            ),
            (
                json!({"results": [{"project_code": CODE, "outcome": "planned"}]}),
                ExitCode::SUCCESS,
            ),
        ] {
            server.reset().await;
            answer(
                &server,
                "GET",
                "/app/api/projects",
                200,
                json!([{"id": Uuid::new_v4(), "code": CODE}]),
            )
            .await;
            Mock::given(method("POST"))
                .and(path(door))
                .and(body_json(
                    json!({"project_code": CODE, "all": false, "dry_run": true}),
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(results))
                .mount(&server)
                .await;
            assert_eq!(super::sync(Some(&host), Some(CODE), true).await, expected);
        }
    }

    #[test]
    fn each_incomplete_sync_outcome_names_its_next_step() {
        for (outcome, expected) in [
            ("calendar_unprovisioned", "has no calendar yet"),
            ("provider_unauthorized", "domain-wide"),
            ("provider_unavailable", "nothing was duplicated"),
            ("mirror_incomplete", "nothing was duplicated"),
            (
                "recorded_resource_missing",
                "navigator project deadline sync --project sample --dry-run",
            ),
        ] {
            let remedy = super::sync_remedy(outcome, "sample").unwrap();
            assert!(remedy.contains(expected), "{remedy}");
            assert!(remedy.contains("navigator project deadline sync --project sample"));
        }
        assert_eq!(super::sync_remedy("mirrored", "sample"), None);
    }

    #[test]
    fn a_category_is_trimmed_validated_and_cleared_when_blank() {
        let known = model::project_deadlines::CATEGORIES[0];
        assert_eq!(
            super::category_field(Some(&format!(" {known} ")))
                .unwrap()
                .as_deref(),
            Some(known)
        );
        assert_eq!(super::category_field(Some("  ")).unwrap(), None);
        assert_eq!(super::category_field(None).unwrap(), None);
        let error = super::category_field(Some("nonsense")).unwrap_err();
        assert!(error.to_string().starts_with("--category must be one of "));
    }
}
