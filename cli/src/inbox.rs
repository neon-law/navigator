//! `navigator inbox` — the lawyer-mailbox processing backlog.
//!
//! Plan by default: listing and showing a message classify server-side and
//! record nothing. `--apply` records the classified route (idempotent),
//! posts the metadata-only Slack notice, and — with `--gmail` — archives
//! the Gmail copy by removing its INBOX label, never by trashing it. The
//! classification itself lives behind the Navigator doors, so the CLI
//! cannot out-vote it, and nothing here sends anything.

use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use clap::Subcommand;
use reqwest::Method;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Subcommand)]
pub enum InboxAction {
    /// List the unprocessed lawyer-mailbox messages, oldest first.
    List {
        /// Navigator host. Defaults to host in navigator.yaml.
        #[arg(long)]
        host: Option<String>,
    },
    /// Show one message's process plan, or with --apply, record it.
    Process {
        /// Inbox message id from `navigator inbox list`.
        message: Uuid,
        /// Record the classified route, notify the matter channel, and
        /// archive the Gmail copy passed with --gmail.
        #[arg(long)]
        apply: bool,
        /// Gmail web URL or message id to archive after applying.
        #[arg(long)]
        gmail: Option<String>,
        /// Navigator host. Defaults to host in navigator.yaml.
        #[arg(long)]
        host: Option<String>,
    },
}

pub async fn run(action: InboxAction) -> ExitCode {
    finish(run_inner(action).await)
}

fn finish(result: Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator inbox: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run_inner(action: InboxAction) -> Result<()> {
    match action {
        InboxAction::List { host } => list(host.as_deref()).await,
        InboxAction::Process {
            message,
            apply,
            gmail,
            host,
        } => process(message, apply, gmail.as_deref(), host.as_deref()).await,
    }
}

/// The backlog door: `GET /app/api/inbox/unprocessed`.
fn list_request(base: &str) -> (Method, String) {
    (Method::GET, format!("{base}/app/api/inbox/unprocessed"))
}

/// The process door: one constant-path POST. The plan (apply: false) and
/// the apply (apply: true) differ only in the request body — the parsed
/// message id is body data, never URL material.
fn process_request(base: &str) -> (Method, String) {
    (Method::POST, format!("{base}/app/api/inbox/process"))
}

/// The body the process door reads: which message, and whether to record.
fn process_body(message: Uuid, apply: bool) -> serde_json::Value {
    serde_json::json!({ "message": message, "apply": apply })
}

/// One message's plan, as the doors serve it. The body never travels.
#[derive(Debug, Deserialize)]
struct ProcessView {
    id: Uuid,
    #[allow(dead_code)]
    conversation_id: Uuid,
    channel: String,
    provider_message_id: Option<String>,
    external_address: String,
    subject: Option<String>,
    computed_route: String,
    computed_label: String,
    reason: String,
    recorded_route: Option<String>,
    recorded_at: Option<String>,
    project_id: Option<Uuid>,
    #[allow(dead_code)]
    occurred_at: String,
    #[allow(dead_code)]
    inserted_at: String,
}

/// An apply's response: the recorded verdict plus the notice outcome.
#[derive(Debug, Deserialize)]
struct ProcessApplyView {
    result: ProcessView,
    route: String,
    route_label: String,
    #[allow(dead_code)]
    reason: String,
    recorded_at: String,
    already_recorded: bool,
    slack_notice: String,
}

async fn list(host: Option<&str>) -> Result<()> {
    let (base, token) = crate::remote::resolve(host)?;
    let (method, url) = list_request(&base);
    let response = reqwest::Client::new()
        .request(method, url)
        .bearer_auth(token)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .context("read the inbox backlog")?;
    let status = response.status();
    let text = response
        .text()
        .await
        .context("read the inbox backlog response")?;
    if !status.is_success() {
        return Err(anyhow!("inbox list refused: {status} {text}"));
    }
    let views: Vec<ProcessView> = serde_json::from_str(&text).context("parse the inbox backlog")?;
    if views.is_empty() {
        println!("The inbox backlog is empty.");
        return Ok(());
    }
    for view in views {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            view.id,
            view.computed_label,
            view.reason,
            view.external_address,
            view.subject.as_deref().unwrap_or_default()
        );
    }
    Ok(())
}

fn print_plan(view: &ProcessView, apply: Option<&ProcessApplyView>) {
    println!("Message      {}", view.id);
    println!("Channel      {}", view.channel);
    println!("From         {}", view.external_address);
    if let Some(subject) = &view.subject {
        println!("Subject      {subject}");
    }
    if let Some(provider_message_id) = &view.provider_message_id {
        println!("Provider id  {provider_message_id}");
    }
    if let Some(project) = view.project_id {
        println!("Matter       {project}");
    }
    println!(
        "Route        {} ({})",
        view.computed_label, view.computed_route
    );
    println!("Because      {}", view.reason);
    match apply {
        Some(apply) => {
            let note = if apply.already_recorded {
                " (already recorded — overwritten with the same truth)"
            } else {
                ""
            };
            println!(
                "Recorded     {} ({}){}",
                apply.route_label, apply.route, note
            );
            println!("Recorded at  {}", apply.recorded_at);
            println!("Slack notice {}", apply.slack_notice);
        }
        None => {
            if let (Some(route), Some(recorded_at)) = (&view.recorded_route, &view.recorded_at) {
                println!("Recorded     {route} at {recorded_at}");
            }
        }
    }
}

async fn process(
    message: Uuid,
    apply: bool,
    gmail: Option<&str>,
    host: Option<&str>,
) -> Result<()> {
    let (base, token) = crate::remote::resolve(host)?;
    let (method, url) = process_request(&base);
    let response = reqwest::Client::new()
        .request(method, url)
        .bearer_auth(token)
        .json(&process_body(message, apply))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .context("call the process door")?;
    let status = response.status();
    let text = response.text().await.context("read the process response")?;
    if !status.is_success() {
        return Err(anyhow!("inbox process refused: {status} {text}"));
    }
    if !apply {
        let view: ProcessView = serde_json::from_str(&text).context("parse the process plan")?;
        print_plan(&view, None);
        println!("Dry run — nothing recorded. Re-run with --apply to record and archive.");
        return Ok(());
    }
    let apply_view: ProcessApplyView =
        serde_json::from_str(&text).context("parse the process response")?;
    print_plan(&apply_view.result, Some(&apply_view));
    match gmail {
        Some(reference) => match crate::mail::archive(reference).await {
            Ok(()) => println!("Archived the Gmail copy (INBOX label removed)."),
            Err(error) => {
                // The recording succeeded; the archive is the part that
                // failed. Say both, and fail the run so it is retried.
                println!("Recording succeeded; archiving the Gmail copy failed.");
                return Err(error);
            }
        },
        None => println!("No Gmail copy requested; nothing to archive."),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        finish, list_request, process_body, process_request, run, InboxAction, ProcessApplyView,
    };
    use crate::credentials::{Credentials, HostCredential};
    use reqwest::Method;
    use uuid::Uuid;

    #[test]
    fn the_plan_reads_and_the_apply_posts() {
        let base = "https://navigator.example.test";
        let message = Uuid::from_u128(7);
        let (method, url) = list_request(base);
        assert_eq!(method, Method::GET);
        assert_eq!(url, format!("{base}/app/api/inbox/unprocessed"));

        // One constant-path door serves both the plan and the apply: the
        // parsed message id is body data, never URL material.
        let (method, url) = process_request(base);
        assert_eq!(method, Method::POST);
        assert_eq!(url, format!("{base}/app/api/inbox/process"));

        let body = process_body(message, false);
        assert_eq!(body["message"], serde_json::json!(message));
        assert_eq!(body["apply"], serde_json::json!(false));

        let body = process_body(message, true);
        assert_eq!(body["message"], serde_json::json!(message));
        assert_eq!(body["apply"], serde_json::json!(true));
    }

    #[test]
    fn the_apply_response_parses_as_the_door_serves_it() {
        let json = r#"{
            "result": {
                "id": "00000000-0000-0000-0000-000000000007",
                "conversation_id": "00000000-0000-0000-0000-000000000001",
                "channel": "EMAIL",
                "provider_message_id": "<caf@example.test>",
                "external_address": "ecf@example.test",
                "subject": "Notice of Electronic Filing",
                "computed_route": "nef",
                "computed_label": "NEF",
                "reason": "the subject says NOTICE of ELECTRONIC FILING",
                "recorded_route": null,
                "recorded_at": null,
                "project_id": null,
                "occurred_at": "2026-10-31T12:00:00Z",
                "inserted_at": "2026-10-31T12:00:00Z"
            },
            "route": "nef",
            "route_label": "NEF",
            "reason": "the subject says NOTICE of ELECTRONIC FILING",
            "recorded_at": "2026-10-31T12:00:00Z",
            "already_recorded": false,
            "slack_notice": "notified"
        }"#;
        let apply: ProcessApplyView = serde_json::from_str(json).unwrap();
        assert_eq!(apply.route, "nef");
        assert_eq!(apply.route_label, "NEF");
        assert!(!apply.already_recorded);
        assert_eq!(apply.slack_notice, "notified");
        assert_eq!(apply.result.computed_route, "nef");
    }

    /// Env mutation for the credentials file is process-global; serialize the
    /// mocked-door tests so parallel runs cannot race the override. A tokio
    /// mutex guards across the awaits the verbs perform.
    static CREDENTIALS_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn credentials_previous() -> String {
        std::env::var("NAVIGATOR_CREDENTIALS_FILE")
            .ok()
            .unwrap_or_default()
    }

    /// Serve one canned HTTP response on a loopback port and return the base
    /// URL the verb should be pointed at. The verb opens exactly one request
    /// against it, so a single accept is the whole server.
    async fn serve_once(status_line: &str, body: String) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let status_line = status_line.to_owned();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf).await;
            let http = format!(
                "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\ncontent-length: \
                 {}\r\nconnection: close\r\n\r\n{body}",
                body.len(),
            );
            socket.write_all(http.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        });
        format!("http://{addr}")
    }

    /// Store a current login for the base and point the credentials path at
    /// the file, restoring whatever the environment carried before.
    fn stored_login(base: &str) {
        // The directory must outlive the test's reads; leak it deliberately
        // (a handful of synthetic-credential files in /tmp per test run).
        let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        let path = dir.path().join("navigator.json");
        let mut creds = Credentials::default();
        creds.set(
            base,
            HostCredential {
                token: "synthetic-token".into(),
                person_email: Some("lawyer@neonlaw.com".into()),
                role: Some("lawyer".into()),
                expires_at: 1_900_000_000,
            },
        );
        crate::credentials::save(&path, &creds).unwrap();
        std::env::set_var("NAVIGATOR_CREDENTIALS_FILE", path);
    }

    fn restore_credentials(previous: String) {
        if previous.is_empty() {
            std::env::remove_var("NAVIGATOR_CREDENTIALS_FILE");
        } else {
            std::env::set_var("NAVIGATOR_CREDENTIALS_FILE", previous);
        }
    }

    const BACKLOG_JSON: &str = r#"[{
        "id": "00000000-0000-0000-0000-000000000007",
        "conversation_id": "00000000-0000-0000-0000-000000000001",
        "channel": "email",
        "provider_message_id": "<caf@example.test>",
        "external_address": "ecf@example.test",
        "subject": "Notice of Electronic Filing",
        "computed_route": "nef",
        "computed_label": "NEF",
        "reason": "the subject says NOTICE of ELECTRONIC FILING",
        "recorded_route": null,
        "recorded_at": null,
        "project_id": null,
        "occurred_at": "2026-10-31T12:00:00Z",
        "inserted_at": "2026-10-31T12:00:00Z"
    }]"#;

    /// The process door's plan response: one message's view, never an array.
    const PLAN_JSON: &str = r#"{
        "id": "00000000-0000-0000-0000-000000000007",
        "conversation_id": "00000000-0000-0000-0000-000000000001",
        "channel": "email",
        "provider_message_id": "<caf@example.test>",
        "external_address": "ecf@example.test",
        "subject": "Notice of Electronic Filing",
        "computed_route": "nef",
        "computed_label": "NEF",
        "reason": "the subject says NOTICE of ELECTRONIC FILING",
        "recorded_route": null,
        "recorded_at": null,
        "project_id": null,
        "occurred_at": "2026-10-31T12:00:00Z",
        "inserted_at": "2026-10-31T12:00:00Z"
    }"#;

    #[tokio::test]
    async fn list_prints_the_backlog_from_a_mocked_door() {
        let _lock = CREDENTIALS_LOCK.lock().await;
        let previous = credentials_previous();
        let base = serve_once("200 OK", BACKLOG_JSON.to_owned()).await;
        stored_login(&base);

        let exit = run(InboxAction::List { host: Some(base) }).await;

        assert_eq!(exit, std::process::ExitCode::SUCCESS);
        restore_credentials(previous);
    }

    #[tokio::test]
    async fn a_plan_against_a_mocked_door_stops_dry() {
        let _lock = CREDENTIALS_LOCK.lock().await;
        let previous = credentials_previous();
        let base = serve_once("200 OK", PLAN_JSON.to_owned()).await;
        stored_login(&base);

        let exit = run(InboxAction::Process {
            message: Uuid::from_u128(7),
            apply: false,
            gmail: None,
            host: Some(base),
        })
        .await;

        assert_eq!(exit, std::process::ExitCode::SUCCESS);
        restore_credentials(previous);
    }

    #[tokio::test]
    async fn an_apply_against_a_mocked_door_records_and_leaves_gmail_alone() {
        let _lock = CREDENTIALS_LOCK.lock().await;
        let previous = credentials_previous();
        let apply_json = r#"{
            "result": {
                "id": "00000000-0000-0000-0000-000000000007",
                "conversation_id": "00000000-0000-0000-0000-000000000001",
                "channel": "email",
                "provider_message_id": "<caf@example.test>",
                "external_address": "ecf@example.test",
                "subject": "Notice of Electronic Filing",
                "computed_route": "nef",
                "computed_label": "NEF",
                "reason": "the subject says NOTICE of ELECTRONIC FILING",
                "recorded_route": "nef",
                "recorded_at": "2026-10-31T12:00:00Z",
                "project_id": null,
                "occurred_at": "2026-10-31T12:00:00Z",
                "inserted_at": "2026-10-31T12:00:00Z"
            },
            "route": "nef",
            "route_label": "NEF",
            "reason": "the subject says NOTICE of ELECTRONIC FILING",
            "recorded_at": "2026-10-31T12:00:00Z",
            "already_recorded": false,
            "slack_notice": "no_project"
        }"#;
        let base = serve_once("200 OK", apply_json.to_owned()).await;
        stored_login(&base);

        let exit = run(InboxAction::Process {
            message: Uuid::from_u128(7),
            apply: true,
            gmail: None,
            host: Some(base),
        })
        .await;

        assert_eq!(exit, std::process::ExitCode::SUCCESS);
        restore_credentials(previous);
    }

    #[tokio::test]
    async fn a_refusing_door_fails_the_verb() {
        let _lock = CREDENTIALS_LOCK.lock().await;
        let previous = credentials_previous();
        let base = serve_once("403 Forbidden", r#"{"error":"denied"}"#.to_owned()).await;
        stored_login(&base);

        let exit = run(InboxAction::Process {
            message: Uuid::from_u128(7),
            apply: false,
            gmail: None,
            host: Some(base),
        })
        .await;

        assert_eq!(exit, std::process::ExitCode::FAILURE);
        restore_credentials(previous);
    }

    #[test]
    fn finish_reports_a_failed_run() {
        let exit = finish(Err(anyhow::anyhow!("synthetic failure")));
        assert_eq!(exit, std::process::ExitCode::FAILURE);
    }
}
