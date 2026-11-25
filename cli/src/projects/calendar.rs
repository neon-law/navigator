//! Read personal and Project Calendars through the Navigator site API.

use std::process::ExitCode;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, FixedOffset, TimeDelta, Utc};
use clap::{Args, Subcommand};
use serde_json::{json, Value};

use crate::remote;

type Instant = DateTime<FixedOffset>;

mod dated;

#[derive(Args)]
pub struct PersonalCalendarArgs {
    /// Navigator host.
    #[arg(long, global = true)]
    host: Option<String>,
    /// Combine calendar events and Deadlines for this local date (YYYY-MM-DD or today).
    #[arg(long, conflicts_with_all = ["start", "end"])]
    date: Option<String>,
    /// IANA time zone for the dated agenda.
    #[arg(long, requires = "date")]
    timezone: Option<String>,
    /// Print the dated agenda as JSON.
    #[arg(long, requires = "date")]
    json: bool,
    #[command(flatten)]
    agenda: Agenda,
    #[command(subcommand)]
    action: Option<PersonalAction>,
}

#[derive(Args)]
pub struct ProjectCalendarArgs {
    /// Navigator host (defaults to the checkout's host).
    #[arg(long, global = true)]
    host: Option<String>,
    /// Project code (defaults to the current checkout).
    #[arg(long, global = true)]
    project: Option<String>,
    #[command(flatten)]
    agenda: Agenda,
    #[command(subcommand)]
    action: Option<ProjectAction>,
}

#[derive(Args)]
struct Agenda {
    /// Agenda start in RFC 3339 (defaults to now).
    #[arg(long)]
    start: Option<Instant>,
    /// Agenda end in RFC 3339 (defaults to 24 hours after start).
    #[arg(long)]
    end: Option<Instant>,
}

impl Agenda {
    fn window(self) -> Result<(Instant, Instant)> {
        let start = self.start.unwrap_or_else(|| Utc::now().fixed_offset());
        let end = match self.end {
            Some(end) => end,
            None => start
                .checked_add_signed(TimeDelta::days(1))
                .context("agenda start is too late to default the end")?,
        };
        Ok((start, end))
    }
}

#[derive(Args)]
struct Availability {
    /// Start of the window in RFC 3339 format.
    #[arg(long)]
    start: Instant,
    /// End of the window in RFC 3339 format.
    #[arg(long)]
    end: Instant,
    /// Limit the read to these attorney addresses.
    #[arg(long = "attorney")]
    attorneys: Vec<String>,
}

#[derive(Subcommand)]
enum PersonalAction {
    /// List events on the signed-in attorney's primary calendar.
    Events {
        #[command(flatten)]
        agenda: Agenda,
        #[arg(long, hide = true)]
        project: Option<String>,
    },
    /// List the signed-in attorney's busy blocks.
    Freebusy {
        #[command(flatten)]
        window: Availability,
        #[arg(long, hide = true)]
        project: Option<String>,
    },
    /// Check whether the signed-in attorney is available.
    Check {
        #[command(flatten)]
        window: Availability,
        #[arg(long, hide = true)]
        project: Option<String>,
    },
    /// Report whether the signed-in attorney's calendars can be read.
    Access {
        #[arg(long, hide = true)]
        project: Option<String>,
    },
    /// Record one of your calendars.
    Register {
        /// Calendar ID. Defaults to the signed-in address.
        #[arg(long)]
        calendar: Option<String>,
        /// Record an extra free/busy calendar.
        #[arg(long, conflicts_with = "booking")]
        freebusy: bool,
        /// Register a private secondary Google Calendar for guest-free booking holds.
        #[arg(long, requires = "calendar")]
        booking: bool,
    },
}

#[derive(Subcommand)]
enum ProjectAction {
    /// List events on the Project Calendar.
    #[command(hide = true)]
    Events(Agenda),
    /// List busy blocks for the Project and its attorneys.
    Freebusy(Availability),
    /// Check availability for the Project and its attorneys.
    Check(Availability),
    /// Check access to the Project and its attorney calendars.
    Access,
}

impl PersonalCalendarArgs {
    pub async fn run(self) -> ExitCode {
        let host = self.host.as_deref();
        if let Some(date) = self.date.as_deref() {
            if self.action.is_some() {
                eprintln!("navigator calendar: use --date without a subcommand");
                return ExitCode::FAILURE;
            }
            return dated::run(host, date, self.timezone.as_deref(), self.json).await;
        }
        match self.action {
            None => events(host, None, self.agenda, false).await,
            Some(PersonalAction::Events { agenda, project }) => {
                events(host, project.as_deref(), agenda, true).await
            }
            Some(PersonalAction::Freebusy { window, project }) => {
                availability(host, project.as_deref(), window, false).await
            }
            Some(PersonalAction::Check { window, project }) => {
                availability(host, project.as_deref(), window, true).await
            }
            Some(PersonalAction::Access { project }) => access(host, project.as_deref()).await,
            Some(PersonalAction::Register {
                calendar,
                freebusy,
                booking,
            }) => {
                let kind = if booking {
                    "booking"
                } else if freebusy {
                    "freebusy"
                } else {
                    "primary"
                };
                register(host, calendar.as_deref(), kind).await
            }
        }
    }
}

impl ProjectCalendarArgs {
    pub async fn run(self) -> ExitCode {
        let target = super::project_target(self.host.as_deref(), self.project.as_deref());
        let (host, project) = match target {
            Ok(target) => target,
            Err(error) => return finish("navigator project calendar", Err(error), false),
        };
        let host = host.as_deref();
        let project = Some(project.as_str());
        match self.action.unwrap_or(ProjectAction::Events(self.agenda)) {
            ProjectAction::Events(agenda) => events(host, project, agenda, false).await,
            ProjectAction::Freebusy(window) => availability(host, project, window, false).await,
            ProjectAction::Check(window) => availability(host, project, window, true).await,
            ProjectAction::Access => access(host, project).await,
        }
    }
}

async fn events(
    host: Option<&str>,
    project: Option<&str>,
    agenda: Agenda,
    compatibility: bool,
) -> ExitCode {
    let result: Result<Value> = async {
        let (start, end) = agenda.window()?;
        let (base, token) = remote::resolve(host)?;
        let path = if compatibility && project.is_some() {
            "/app/api/integrations/calendar/events"
        } else {
            "/app/api/calendars/events"
        };
        let mut body = json!({"start": start, "end": end});
        if let Some(project) = project {
            body["project_code"] = json!(project);
        }
        post(&base, &token, path, body).await
    }
    .await;
    finish("navigator calendar events", result, true)
}

async fn availability(
    host: Option<&str>,
    project: Option<&str>,
    window: Availability,
    check: bool,
) -> ExitCode {
    let verb = if check { "check" } else { "freebusy" };
    let result: Result<Value> = async {
        let (base, token) = remote::resolve(host)?;
        let mut body = json!({
            "project_code": project,
            "start": window.start,
            "end": window.end,
            "attorneys": window.attorneys,
        });
        if project.is_none() && window.attorneys.is_empty() {
            body["personal"] = json!(true);
        }
        post(&base, &token, &format!("/app/api/calendars/{verb}"), body).await
    }
    .await;
    finish(&format!("navigator calendar {verb}"), result, true)
}

async fn access(host: Option<&str>, project_code: Option<&str>) -> ExitCode {
    let result: Result<Value> = async {
        let (base, token) = remote::resolve(host)?;
        let body = match project_code {
            Some(code) => json!({ "project_code": code }),
            None => json!({ "personal": true }),
        };
        post(&base, &token, "/app/api/calendars/access", body).await
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

async fn register(host: Option<&str>, calendar_id: Option<&str>, kind: &str) -> ExitCode {
    let result: Result<Value> = async {
        let (base, token) = remote::resolve(host)?;
        post(
            &base,
            &token,
            "/app/api/calendars/register",
            json!({
                "calendar_id": calendar_id,
                "kind": kind,
            }),
        )
        .await
    }
    .await;
    finish("navigator calendar register", result, false)
}

pub(super) async fn post(base: &str, token: &str, path: &str, body: Value) -> Result<Value> {
    let response = remote::authenticated_client_builder(base)?
        .build()?
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
