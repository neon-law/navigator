//! Read a synthetic Project Calendar through the Navigator site API.

use std::process::ExitCode;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, FixedOffset};
use serde_json::{json, Value};

use crate::remote;

pub(crate) async fn events(
    host: &str,
    project_code: &str,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
) -> ExitCode {
    let result: Result<Vec<Value>> = async {
        let (base, token) = remote::resolve(Some(host))?;
        let response = reqwest::Client::new()
            .post(format!("{base}/app/api/integrations/calendar/events"))
            .timeout(Duration::from_secs(30))
            .bearer_auth(token)
            .json(&json!({"project_code": project_code, "start": start, "end": end}))
            .send()
            .await
            .context("request Project Calendar events")?;
        ensure!(
            response.status().is_success(),
            "Project Calendar events refused ({})",
            response.status()
        );
        response
            .json()
            .await
            .context("read Project Calendar events")
    }
    .await;
    match result {
        Ok(events) => {
            println!("{}", Value::Array(events));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator calendar events: {error}");
            ExitCode::FAILURE
        }
    }
}
