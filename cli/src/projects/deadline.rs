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

async fn coordinates(host: Option<&str>, project: Option<&str>) -> Result<(String, String, Uuid)> {
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

async fn call(request: reqwest::RequestBuilder, action: &str) -> Result<Value> {
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

fn finish(result: Result<Value>) -> ExitCode {
    match result {
        Ok(row) => {
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

#[allow(clippy::too_many_arguments)]
pub async fn create(
    host: Option<&str>,
    project: Option<&str>,
    due_at: String,
    authority_kind: String,
    authority: Option<String>,
    source: Option<String>,
    source_document: Option<Uuid>,
    owner: Option<Uuid>,
    calendar_role: String,
    internal_lead_days: Option<i64>,
    client_lead_days: Option<i64>,
    replay_key: Option<String>,
) -> ExitCode {
    finish(
        async {
            let (base, token, id) = coordinates(host, project).await?;
            let mut body = json!({
                "due_at": due_at,
                "authority_kind": authority_kind,
                "calendar_role": calendar_role,
            });
            let Some(object) = body.as_object_mut() else {
                return Err(anyhow!("deadline body is an object"));
            };
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
            call(
                reqwest::Client::new()
                    .post(format!("{base}/app/api/projects/{id}/deadlines"))
                    .bearer_auth(token)
                    .json(&body),
                "create",
            )
            .await
        }
        .await,
    )
}

#[allow(clippy::too_many_arguments)]
pub async fn update(
    host: Option<&str>,
    project: Option<&str>,
    id: Uuid,
    due_at: Option<String>,
    authority_kind: Option<String>,
    authority: Option<String>,
    source: Option<String>,
    source_document: Option<Uuid>,
    owner: Option<Uuid>,
    calendar_role: Option<String>,
    internal_lead_days: Option<i64>,
    client_lead_days: Option<i64>,
) -> ExitCode {
    finish(
        async {
            let (base, token, project_id) = coordinates(host, project).await?;
            let mut body = json!({});
            let Some(object) = body.as_object_mut() else {
                return Err(anyhow!("deadline body is an object"));
            };
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
            if let Some(days) = internal_lead_days {
                object.insert("internal_lead_days".into(), json!(days));
            }
            if let Some(days) = client_lead_days {
                object.insert("client_lead_days".into(), json!(days));
            }
            call(
                reqwest::Client::new()
                    .patch(format!(
                        "{base}/app/api/projects/{project_id}/deadlines/{id}"
                    ))
                    .bearer_auth(token)
                    .json(&body),
                "update",
            )
            .await
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
    use super::post_transition;
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
}
