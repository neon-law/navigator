//! Read a synthetic Project Calendar through the Navigator site API.

use std::process::ExitCode;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, FixedOffset};
use serde_json::{json, Value};

use crate::remote;

pub(crate) async fn events(
    host: &str,
    project_code: Option<&str>,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
) -> ExitCode {
    let result: Result<Value> = async {
        let (base, token) = remote::resolve(Some(host))?;
        let (path, body) = match project_code {
            Some(project_code) => (
                "/app/api/integrations/calendar/events",
                json!({"project_code": project_code, "start": start, "end": end}),
            ),
            None => (
                "/app/api/calendars/events",
                json!({"start": start, "end": end}),
            ),
        };
        let value = post(&base, &token, path, body).await?;
        if value.get("unreadable").is_some() {
            anyhow::bail!("{value}");
        }
        Ok(value)
    }
    .await;
    finish("navigator calendar events", result, false)
}

pub(crate) async fn freebusy(
    host: &str,
    project_code: Option<&str>,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
    attorneys: &[String],
) -> ExitCode {
    let result = post_window(
        host,
        "/app/api/calendars/freebusy",
        project_code,
        start,
        end,
        attorneys,
    )
    .await;
    finish("navigator calendar freebusy", result, true)
}

pub(crate) async fn check(
    host: &str,
    project_code: Option<&str>,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
    attorneys: &[String],
) -> ExitCode {
    let result = post_window(
        host,
        "/app/api/calendars/check",
        project_code,
        start,
        end,
        attorneys,
    )
    .await;
    finish("navigator calendar check", result, true)
}

pub(crate) async fn access(host: &str, project_code: Option<&str>) -> ExitCode {
    let result: Result<Value> = async {
        let (base, token) = remote::resolve(Some(host))?;
        post(
            &base,
            &token,
            "/app/api/calendars/access",
            json!({ "project_code": project_code }),
        )
        .await
    }
    .await;
    match result {
        Ok(value) => {
            let blocked = access_blocked(&value);
            println!("{value}");
            if blocked {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            eprintln!("navigator calendar access: {error}");
            ExitCode::FAILURE
        }
    }
}

fn access_blocked(value: &Value) -> bool {
    let attorneys = value
        .get("attorneys")
        .and_then(Value::as_array)
        .is_some_and(|rows| {
            rows.iter().any(|row| {
                row.get("missing").is_some()
                    || row
                        .get("calendars")
                        .and_then(Value::as_array)
                        .is_some_and(|calendars| {
                            calendars.iter().any(|calendar| {
                                calendar.get("readable") == Some(&Value::Bool(false))
                            })
                        })
            })
        });
    let project = value
        .pointer("/project_calendar/readable")
        .is_some_and(|readable| readable == &Value::Bool(false));
    attorneys || project
}

pub(crate) async fn register(host: &str, calendar_id: Option<&str>, freebusy: bool) -> ExitCode {
    let result: Result<Value> = async {
        let (base, token) = remote::resolve(Some(host))?;
        post(
            &base,
            &token,
            "/app/api/calendars/register",
            json!({
                "calendar_id": calendar_id,
                "kind": if freebusy { "freebusy" } else { "primary" },
            }),
        )
        .await
    }
    .await;
    finish("navigator calendar register", result, false)
}

async fn post_window(
    host: &str,
    path: &str,
    project_code: Option<&str>,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
    attorneys: &[String],
) -> Result<Value> {
    let (base, token) = remote::resolve(Some(host))?;
    post(
        &base,
        &token,
        path,
        json!({
            "project_code": project_code,
            "start": start,
            "end": end,
            "attorneys": attorneys,
        }),
    )
    .await
}

async fn post(base: &str, token: &str, path: &str, body: Value) -> Result<Value> {
    let response = reqwest::Client::new()
        .post(format!("{base}{path}"))
        .timeout(Duration::from_secs(30))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("request {path}"))?;
    ensure!(
        response.status().is_success(),
        "{path} refused ({})",
        response.status()
    );
    response.json().await.context("read calendar response")
}

fn finish(command: &str, result: Result<Value>, fail_on_gap: bool) -> ExitCode {
    match result {
        Ok(value) => {
            let blocked = fail_on_gap
                && (value.get("clear") == Some(&Value::Bool(false))
                    || value
                        .get("unreadable")
                        .and_then(Value::as_array)
                        .is_some_and(|rows| !rows.is_empty()));
            println!("{value}");
            if blocked {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            eprintln!("{command}: {error}");
            ExitCode::FAILURE
        }
    }
}
