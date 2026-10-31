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

fn annotate_visibility(row: &mut Value) {
    if let Some(rows) = row.as_array_mut() {
        for row in rows {
            annotate_visibility(row);
        }
    } else if let Some(object) = row.as_object_mut() {
        if let Some(lead) = object.get("client_lead_days") {
            let visibility = if lead.is_null() { "internal" } else { "client" };
            object.insert("visibility".into(), json!(visibility));
        }
    }
}

pub(super) fn finish(result: Result<Value>) -> ExitCode {
    match result {
        Ok(mut row) => {
            annotate_visibility(&mut row);
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
            call(
                reqwest::Client::new()
                    .get(format!("{base}/app/api/projects/{id}/deadlines"))
                    .bearer_auth(token),
                "list",
            )
            .await
        }
        .await,
    )
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
        for result in results {
            let line = match result["detail"].as_str() {
                Some(detail) => format!(" — {detail}"),
                None => String::new(),
            };
            println!(
                "{}: {}{}",
                result["project_code"].as_str().unwrap_or("?"),
                result["outcome"].as_str().unwrap_or("?"),
                line
            );
        }
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
    if category.is_some_and(|value| !store::project_deadlines::CATEGORIES.contains(&value)) {
        return Err(anyhow!(
            "--category must be one of {}",
            store::project_deadlines::CATEGORIES.join(", ")
        ));
    }
    Ok(category.map(str::to_string))
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

#[allow(clippy::too_many_arguments)]
pub async fn create(
    host: Option<&str>,
    project: Option<&str>,
    title: Option<String>,
    description: Option<String>,
    visibility: Option<String>,
    due_at: String,
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
            let (base, token, id, code) = coordinates_with_code(host, project).await?;
            let mut body = json!({
                "due_at": due_at,
                "authority_kind": authority_kind,
                "calendar_role": calendar_role,
            });
            let Some(object) = body.as_object_mut() else {
                return Err(anyhow!("deadline body is an object"));
            };
            if let Some(title) = title {
                object.insert("title".into(), json!(title));
            }
            if let Some(description) = description {
                object.insert("description".into(), json!(description));
            }
            if let Some(category) = category_field(category.as_deref())? {
                object.insert("category".into(), json!(category));
            }
            if let Some(authority) = authority {
                object.insert("authority".into(), json!(authority));
            }
            if let Some(source) = source {
                object.insert("source".into(), json!(source));
            }
            if let Some(document) = source_document {
                object.insert("source_document_id".into(), json!(document));
            }
            if let Some(owner) = owner {
                object.insert("owner_person_id".into(), json!(owner));
            }
            if let Some(days) = internal_lead_days {
                object.insert("internal_lead_days".into(), json!(days));
            }
            if let Some(days) = client_lead_days {
                object.insert("client_lead_days".into(), json!(days));
            }
            if let Some(key) = replay_key {
                object.insert("replay_key".into(), json!(key));
            }
            apply_visibility(object, visibility.as_deref(), client_lead_days, true)?;
            let row = call(
                reqwest::Client::new()
                    .post(format!("{base}/app/api/projects/{id}/deadlines"))
                    .bearer_auth(token)
                    .json(&body),
                "create",
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
            let mut body = json!({});
            let Some(object) = body.as_object_mut() else {
                return Err(anyhow!("deadline body is an object"));
            };
            if let Some(title) = title {
                object.insert("title".into(), json!(title));
            }
            if let Some(due_at) = due_at {
                object.insert("due_at".into(), json!(due_at));
            }
            if let Some(kind) = authority_kind {
                object.insert("authority_kind".into(), json!(kind));
            }
            if let Some(authority) = authority {
                object.insert("authority".into(), json!(authority));
            }
            if let Some(source) = source {
                object.insert("source".into(), json!(source));
            }
            if let Some(document) = source_document {
                object.insert("source_document_id".into(), json!(document));
            }
            if let Some(owner) = owner {
                object.insert("owner_person_id".into(), json!(owner));
            }
            if let Some(role) = calendar_role {
                object.insert("calendar_role".into(), json!(role));
            }
            // Absent flag preserves the stored category; an empty value
            // clears it. The portal's double-Option carries the difference.
            if let Some(raw) = category.as_deref() {
                object.insert(
                    "category".into(),
                    category_field(Some(raw))?
                        .map(|category| json!(category))
                        .unwrap_or(Value::Null),
                );
            }
            if let Some(days) = internal_lead_days {
                object.insert("internal_lead_days".into(), json!(days));
            }
            if let Some(days) = client_lead_days {
                object.insert("client_lead_days".into(), json!(days));
            }
            if let Some(description) = description {
                object.insert("description".into(), json!(description));
            }
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
    use super::{annotate_visibility, apply_visibility, export_row, finish_write, post_transition};
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
        annotate_visibility(&mut body);
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
        annotate_visibility(&mut client);
        assert_eq!(client["visibility"], "client");
        apply_visibility(client.as_object_mut().unwrap(), None, Some(3), true).unwrap();
        assert!(client["client_lead_days"].is_null());
    }
}
