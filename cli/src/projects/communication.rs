//! `navigator project communication create|list` — log a call as an
//! internal-note [Communication](../../../docs/glossary/communication.md),
//! or an email with its source and receipt relation, and read the firm view of
//! the log back.
//!
//! Communications stay server-only: no pointer is written under
//! `documents/`, and `project sync` carries none. The Project comes from
//! `navigator.yaml` the way `project deadline` reads it; `create --dry-run`
//! prints the request and sends nothing.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    projects::{manifest, surfaces},
    remote,
};

async fn coordinates(host: Option<&str>, project: Option<&str>) -> Result<(String, String, Uuid)> {
    let local = manifest::read(Path::new("."));
    let project = project
        .or_else(|| local.as_ref().and_then(|m| m.project.as_deref()))
        .ok_or_else(|| anyhow!("pass --project or run from a Project checkout"))?;
    let host = host.or_else(|| local.as_ref().and_then(|m| m.host.as_deref()));
    let (base, token) = remote::resolve(host)?;
    let id = surfaces::resolve_project_id(&base, &token, project).await?;
    Ok((base, token, id))
}

async fn call(request: reqwest::RequestBuilder, action: &str) -> Result<Value> {
    let response = request
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .with_context(|| format!("{action} communication"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("read communication response")?;
    if !status.is_success() {
        return Err(anyhow!("communication {action} refused: {status} {body}"));
    }
    serde_json::from_str(&body).context("parse communication response")
}

fn finish(result: Result<Value>) -> ExitCode {
    match result {
        Ok(row) => {
            println!("{row}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator project communication: {error:#}");
            ExitCode::FAILURE
        }
    }
}

pub async fn list(host: Option<&str>, project: Option<&str>, channel: Option<&str>) -> ExitCode {
    finish(
        async {
            let (base, token, id) = coordinates(host, project).await?;
            let rows = call(
                reqwest::Client::new()
                    .get(format!("{base}/app/api/projects/{id}/communications"))
                    .bearer_auth(token),
                "list",
            )
            .await?;
            Ok(match channel {
                Some(channel) => Value::Array(
                    rows.as_array()
                        .into_iter()
                        .flatten()
                        .filter(|row| row["channel"] == channel)
                        .cloned()
                        .collect(),
                ),
                None => rows,
            })
        }
        .await,
    )
}

/// What `navigator project communication create` logs.
#[derive(Debug, Clone, Default)]
pub struct NewCommunication {
    pub channel: String,
    pub direction: String,
    pub counterparty: String,
    pub role: Option<String>,
    pub at: String,
    pub duration: Option<i64>,
    pub body_file: PathBuf,
    pub recorded: String,
    pub meet_and_confer: Option<String>,
    pub agreements: Vec<String>,
    pub confirms: Option<Uuid>,
    pub acknowledges: Option<Uuid>,
    pub source_ref: Option<String>,
    pub client_visible: bool,
    pub subject: Option<String>,
}

fn create_body(new: &NewCommunication) -> Result<Value> {
    let body = std::fs::read_to_string(&new.body_file)
        .with_context(|| format!("read {}", new.body_file.display()))?;
    if body.trim().is_empty() {
        return Err(anyhow!("{} is empty", new.body_file.display()));
    }
    let mut request = json!({
        "channel": new.channel,
        "direction": new.direction,
        "counterparty": new.counterparty,
        "at": new.at,
        "body": body,
    });
    if let Some(role) = &new.role {
        request["role"] = json!(role);
    }
    if let Some(source_ref) = &new.source_ref {
        request["source_ref"] = json!(source_ref);
    }
    if let Some(acknowledges) = new.acknowledges {
        request["acknowledges"] = json!(acknowledges);
    }
    if new.channel == "call" {
        request["role"] = json!(new.role.clone().unwrap_or_else(|| "other".to_string()));
        request["recorded"] = json!(new.recorded);
        if let Some(minutes) = new.duration {
            request["duration_minutes"] = json!(minutes);
        }
        if let Some(issue) = &new.meet_and_confer {
            request["meet_and_confer"] = json!(issue);
        }
        if !new.agreements.is_empty() {
            request["agreements"] = json!(new.agreements);
        }
        if new.client_visible {
            request["client_visible"] = json!(true);
        }
    } else if let Some(confirms) = new.confirms {
        request["confirms"] = json!(confirms);
    }
    if let Some(subject) = &new.subject {
        request["subject"] = json!(subject);
    }
    Ok(request)
}

pub async fn create(
    host: Option<&str>,
    project: Option<&str>,
    new: NewCommunication,
    dry_run: bool,
) -> ExitCode {
    finish(
        async {
            let body = create_body(&new)?;
            if dry_run {
                eprintln!(
                    "dry run: `navigator project communication create` would send this and write nothing"
                );
                return Ok(body);
            }
            let (base, token, id) = coordinates(host, project).await?;
            call(
                reqwest::Client::new()
                    .post(format!("{base}/app/api/projects/{id}/communications"))
                    .bearer_auth(token)
                    .json(&body),
                "create",
            )
            .await
        }
        .await,
    )
}

/// What `navigator project communication note` logs (LAW-230).
#[derive(Debug, Clone)]
pub struct NewNote {
    pub body_file: PathBuf,
    pub subject: Option<String>,
}

fn note_body(new: &NewNote, at: &str) -> Result<Value> {
    let body = std::fs::read_to_string(&new.body_file)
        .with_context(|| format!("read {}", new.body_file.display()))?;
    if body.trim().is_empty() {
        return Err(anyhow!("{} is empty", new.body_file.display()));
    }
    let mut request = json!({
        "channel": "note",
        "direction": "internal",
        "at": at,
        "body": body,
    });
    if let Some(subject) = &new.subject {
        request["subject"] = json!(subject);
    }
    Ok(request)
}

/// Log a firm-internal matter note (LAW-230). A note has no counterparty
/// and no client visibility — it renders on the workbench's Notes tab and
/// never on a client view.
pub async fn note(
    host: Option<&str>,
    project: Option<&str>,
    new: NewNote,
    dry_run: bool,
) -> ExitCode {
    finish(
        async {
            let at = chrono::Utc::now().to_rfc3339();
            let body = note_body(&new, &at)?;
            if dry_run {
                eprintln!(
                    "dry run: `navigator project communication note` would send this and write nothing"
                );
                return Ok(body);
            }
            let (base, token, id) = coordinates(host, project).await?;
            call(
                reqwest::Client::new()
                    .post(format!("{base}/app/api/projects/{id}/communications"))
                    .bearer_auth(token)
                    .json(&body),
                "create",
            )
            .await
        }
        .await,
    )
}

#[cfg(test)]
mod tests {
    use super::{create_body, note_body, NewCommunication, NewNote};

    #[test]
    fn a_call_carries_its_structure_and_an_email_carries_what_it_confirms() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("call.md");
        std::fs::write(&note, "Discussed the schedule.\n").unwrap();
        let call = create_body(&NewCommunication {
            channel: "call".into(),
            direction: "outbound".into(),
            counterparty: "counsel@example.com".into(),
            role: Some("opposing_counsel".into()),
            at: "2026-10-09T15:00:00-07:00".into(),
            duration: Some(20),
            body_file: note.clone(),
            recorded: "none".into(),
            meet_and_confer: Some("Set One".into()),
            agreements: vec!["Extend to 2026-11-02".into()],
            ..NewCommunication::default()
        })
        .unwrap();
        assert_eq!(call["role"], "opposing_counsel");
        assert_eq!(call["duration_minutes"], 20);
        assert_eq!(call["agreements"][0], "Extend to 2026-11-02");
        assert!(call.get("confirms").is_none());

        let id = uuid::Uuid::now_v7();
        let email = create_body(&NewCommunication {
            channel: "email_outbound".into(),
            direction: "outbound".into(),
            counterparty: "counsel@example.com".into(),
            at: "2026-10-09T16:00:00-07:00".into(),
            body_file: note,
            confirms: Some(id),
            ..NewCommunication::default()
        })
        .unwrap();
        assert_eq!(email["confirms"], id.to_string());
        assert!(email.get("role").is_none());

        std::fs::write(dir.path().join("empty.md"), "  \n").unwrap();
        assert!(create_body(&NewCommunication {
            channel: "call".into(),
            body_file: dir.path().join("empty.md"),
            ..NewCommunication::default()
        })
        .is_err());
    }
    #[test]
    fn a_note_is_internal_without_a_counterparty() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("note.md");
        std::fs::write(&note, "Client confirmed the schedule.\n").unwrap();
        let request = note_body(
            &NewNote {
                body_file: note,
                subject: Some("Call notes".into()),
            },
            "2026-10-30T17:00:00+00:00",
        )
        .unwrap();
        assert_eq!(request["channel"], "note");
        assert_eq!(request["direction"], "internal");
        assert_eq!(request["subject"], "Call notes");
        assert_eq!(request["at"], "2026-10-30T17:00:00+00:00");
        assert!(request.get("counterparty").is_none());
        assert!(request.get("client_visible").is_none());

        std::fs::write(dir.path().join("empty.md"), "  \n").unwrap();
        assert!(note_body(
            &NewNote {
                body_file: dir.path().join("empty.md"),
                subject: None,
            },
            "2026-10-30T17:00:00+00:00",
        )
        .is_err());
    }

    #[test]
    fn email_request_preserves_source_role_and_receipt_link() {
        let dir = tempfile::tempdir().unwrap();
        let body = dir.path().join("receipt.md");
        std::fs::write(&body, "Receipt acknowledged.").unwrap();
        let target = uuid::Uuid::now_v7();
        let email = create_body(&NewCommunication {
            channel: "email_inbound".into(),
            direction: "inbound".into(),
            role: Some("opposing_counsel".into()),
            source_ref: Some("gmail:synthetic/receipt".into()),
            acknowledges: Some(target),
            body_file: body,
            ..NewCommunication::default()
        })
        .unwrap();
        assert_eq!(email["role"], "opposing_counsel");
        assert_eq!(email["source_ref"], "gmail:synthetic/receipt");
        assert_eq!(email["acknowledges"], target.to_string());
    }
}
