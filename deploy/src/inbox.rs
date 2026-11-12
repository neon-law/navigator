//! `hudson inbox` — the lawyer-mailbox processing backlog.
//!
//! Plan by default: listing and showing a message classify server-side and
//! record nothing. `--apply` records the classified route (idempotent),
//! posts the metadata-only Slack notice, and — with `--gmail` — archives
//! the Gmail copy after confirmed filing, removing its INBOX label. The
//! classification itself lives behind the Navigator doors, so the CLI
//! cannot out-vote it, and nothing here sends anything.
//!
//! `inbox attachments` and `inbox process --dir` write a message's linked
//! attachments to a local directory through the same writer as
//! `mail attachments`; each file's digest must match the plan's.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use clap::Subcommand;
use reqwest::Method;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Subcommand)]
pub enum InboxAction {
    /// Preview a metadata-only Google Fi CSV using an explicit column mapping.
    ImportFi {
        #[arg(long)]
        file: std::path::PathBuf,
        #[arg(long)]
        mapping: std::path::PathBuf,
        /// Historical Fi account in E.164 form; it is never a live sender.
        #[arg(long)]
        account: String,
        #[arg(long)]
        brand: String,
        #[arg(long)]
        project: Option<String>,
        /// Write validated historical metadata; omitted means read-only preview.
        #[arg(long)]
        apply: bool,
        #[command(flatten)]
        host: crate::HostOpt,
    },
    /// Propose an assignment for every unattributed backlog thread by
    /// matching its From/To/Cc addresses against the open Projects' Person
    /// emails. A unique match assigns the thread; shared inboxes, no
    /// matches, several matches, and senders with no Person row leave it
    /// in the inbox for the lawyer.
    AssignMatches {
        /// Print the proposed assignment per thread without assigning,
        /// filing, archiving, or recording anything.
        #[arg(long)]
        dry_run: bool,
        /// Navigator host. Defaults to host in navigator.yaml.
        #[arg(long)]
        host: Option<String>,
    },
    #[command(flatten)]
    Import(crate::inbox_import::InboxCmd),
    /// List the unprocessed lawyer-mailbox messages, oldest first.
    List {
        /// Navigator host. Defaults to host in navigator.yaml.
        #[arg(long)]
        host: Option<String>,
    },
    /// Show one message's process plan, or with --apply, record it.
    Process {
        /// Inbox message id from `hudson inbox list`.
        message: Uuid,
        /// Record the classified route, notify the matter channel, and
        /// archive --gmail only after a NEF filing finishes; refusals fail in triage.
        #[arg(long)]
        apply: bool,
        /// Gmail web URL or message id to archive after processing finishes.
        #[arg(long)]
        gmail: Option<String>,
        /// On --apply, file an attributed message's attachments as internal
        /// documents of this kind. Without it, none file and the count shows.
        #[arg(long, value_parser = crate::parse_asset_kind)]
        kind: Option<String>,
        /// Also write the message's attachments into this directory.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Navigator host. Defaults to host in navigator.yaml.
        #[arg(long)]
        host: Option<String>,
    },
    /// Write one message's attachments into a directory; records nothing.
    Attachments {
        /// Inbox message id from `navigator inbox list`.
        message: Uuid,
        /// Directory to write into, one file per attachment.
        #[arg(long)]
        dir: PathBuf,
        /// Only attachments whose filename matches this glob (`*`, `?`).
        #[arg(long)]
        only: Option<String>,
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
            eprintln!("hudson inbox: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run_inner(action: InboxAction) -> Result<()> {
    match action {
        InboxAction::ImportFi {
            file,
            mapping,
            account,
            brand,
            project,
            apply,
            host,
        } => {
            crate::inbox_import::import_fi(crate::inbox_import::NewFiImport {
                file,
                mapping,
                account,
                brand,
                project,
                apply,
                host: host.host,
            })
            .await
        }

        InboxAction::Import(crate::inbox_import::InboxCmd::Import {
            source,
            mailbox,
            file,
            dir,
            project,
            dry_run,
            host,
        }) => {
            crate::inbox_import::import(crate::inbox_import::NewInboxImport {
                source,
                mailbox,
                files: file,
                dir,
                project,
                dry_run,
                host: host.host,
            })
            .await
        }
        InboxAction::AssignMatches { dry_run, host } => {
            assign_matches(dry_run, host.as_deref()).await
        }
        InboxAction::List { host } => list(host.as_deref()).await,
        InboxAction::Process {
            message,
            apply,
            gmail,
            kind,
            dir,
            host,
        } => {
            process(
                message,
                apply,
                gmail.as_deref(),
                kind.as_deref(),
                dir.as_deref(),
                host.as_deref(),
            )
            .await
        }
        InboxAction::Attachments {
            message,
            dir,
            only,
            host,
        } => {
            let (base, token) = crate::remote::resolve(host.as_deref())?;
            let view = plan(&base, &token, message).await?;
            download(&base, &token, &view, &dir, only.as_deref()).await
        }
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

/// The attachment door: one constant-path POST whose body names the
/// message and the asset.
fn attachment_request(base: &str) -> (Method, String) {
    (Method::POST, format!("{base}/app/api/inbox/attachment"))
}

/// The assign-matches door: one constant-path POST. The plan (apply:
/// false) and the apply (apply: true) differ only in the request body.
fn assign_matches_request(base: &str) -> (Method, String) {
    (Method::POST, format!("{base}/app/api/inbox/assign-matches"))
}

/// The body the assign-matches door reads: propose only, or propose and
/// assign. No other knob exists — the door classifies server-side.
fn assign_matches_body(apply: bool) -> serde_json::Value {
    serde_json::json!({ "apply": apply })
}

/// The body the process door reads: which message, and whether to record.
fn process_body(message: Uuid, apply: bool, kind: Option<&str>) -> serde_json::Value {
    let mut body = serde_json::json!({ "message": message, "apply": apply });
    if let Some(kind) = kind {
        body["kind"] = serde_json::json!(kind);
    }
    body
}

/// One message's plan, as the doors serve it. The body never travels.
#[derive(Debug, Deserialize)]
struct ProcessView {
    id: Uuid,
    channel: String,
    provider_message_id: Option<String>,
    external_address: String,
    subject: Option<String>,
    computed_route: String,
    computed_label: String,
    reason: String,
    recorded_route: Option<String>,
    recorded_at: Option<String>,
    #[serde(default)]
    recorded_nef_outcome: Option<String>,
    project_id: Option<Uuid>,
    #[serde(default)]
    attachments: Vec<AttachmentView>,
}

/// One linked attachment, as the plan lists it.
#[derive(Debug, Deserialize)]
struct AttachmentView {
    asset_id: Uuid,
    filename: String,
    sha256: String,
}

/// One thread's address-match proposal, as the door serves it. The
/// decision's `project_id` is set exactly when `decision` is `assigned`;
/// `reason` names why a thread stayed in the inbox.
#[derive(Debug, Deserialize)]
struct AssignMatchView {
    message: Uuid,
    from: String,
    subject: Option<String>,
    decision: MatchDecisionView,
    applied: bool,
}

/// The decision the address-match made for one thread.
#[derive(Debug, Deserialize)]
struct MatchDecisionView {
    decision: String,
    project_id: Option<Uuid>,
    reason: Option<String>,
}

/// An apply's response: the recorded verdict plus the notice outcome.
#[derive(Debug, Deserialize)]
struct ProcessApplyView {
    result: ProcessView,
    route: String,
    route_label: String,
    recorded_at: String,
    already_recorded: bool,
    slack_notice: String,
    #[serde(default)]
    nef: Option<NefFiling>,
    #[serde(default)]
    attachments: AttachmentFiling,
}

/// What filed from a non-NEF message's attachments on apply.
#[derive(Debug, Default, Deserialize)]
struct AttachmentFiling {
    filed: usize,
    unfiled: usize,
}

#[derive(Debug, Deserialize)]
struct NefFiling {
    outcome: String,
}

impl ProcessApplyView {
    fn require_completed_filing(&self) -> Result<()> {
        if self.route != store::inbox::route::NEF {
            return Ok(());
        }
        let outcome = self.nef.as_ref().map(|filing| filing.outcome.as_str());
        match outcome {
            Some(
                store::inbox::nef_outcome::FILED
                | store::inbox::nef_outcome::ALREADY_RECORDED
                | store::inbox::nef_outcome::TEXT_ONLY,
            ) => Ok(()),
            Some(outcome) => Err(anyhow!(
                "NEF filing remains in triage: {outcome}; Gmail copy left in the inbox"
            )),
            None => Err(anyhow!(
                "NEF filing outcome is unconfirmed; Gmail copy left in the inbox"
            )),
        }
    }
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
            "{}\t{}\t{}\t{}\t{}\t{}",
            view.id,
            view.computed_label,
            view.reason,
            view.external_address,
            view.subject.as_deref().unwrap_or_default(),
            view.recorded_nef_outcome.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}

/// Propose — and without `--dry-run`, make — the address-match assignment
/// for every unattributed backlog thread. The door classifies server-side
/// from the same Person CRM the triage surface reads; each unique match
/// goes through the existing `inbox assign` gates. Filing and archiving
/// stay with `inbox process` entirely: this verb never files, never
/// archives, and (dry run) never records.
async fn assign_matches(dry_run: bool, host: Option<&str>) -> Result<()> {
    let (base, token) = crate::remote::resolve(host)?;
    let (method, url) = assign_matches_request(&base);
    let client = reqwest::Client::new();
    let mut body = assign_matches_body(!dry_run);
    let mut printed = false;
    loop {
        let response = client
            .request(method.clone(), &url)
            .bearer_auth(&token)
            .json(&body)
            .timeout(std::time::Duration::from_secs(300))
            .send()
            .await
            .context("call the assign-matches door")?;
        let status = response.status();
        let next: Option<serde_json::Value> = response
            .headers()
            .get("x-navigator-next-cursor")
            .map(|value| serde_json::from_slice(value.as_bytes()))
            .transpose()
            .context("parse the assign-matches continuation")?;
        let text = response
            .text()
            .await
            .context("read the assign-matches response")?;
        if !status.is_success() {
            return Err(anyhow!("inbox assign-matches refused: {status} {text}"));
        }
        let views: Vec<AssignMatchView> =
            serde_json::from_str(&text).context("parse the assign-matches response")?;
        printed |= !views.is_empty();
        print_assignment_matches(&views);
        let Some(next) = next else { break };
        anyhow::ensure!(
            body.get("after") != Some(&next),
            "assign-matches continuation did not advance"
        );
        println!("Continuing with the next inbox batch.");
        body["after"] = next;
    }
    if !printed {
        println!("No unattributed inbox threads to assign.");
    } else if dry_run {
        println!(
            "Dry run — nothing assigned, filed, archived, or recorded. Re-run without --dry-run to make these assignments."
        );
    }
    Ok(())
}

fn print_assignment_matches(views: &[AssignMatchView]) {
    for view in views {
        println!("Message      {}", view.message);
        println!("From         {}", view.from);
        if let Some(subject) = &view.subject {
            println!("Subject      {subject}");
        }
        match view.decision.decision.as_str() {
            "assigned" => println!(
                "Decision     assign to matter {}",
                view.decision
                    .project_id
                    .as_ref()
                    .map_or_else(|| "with no served project id".to_owned(), Uuid::to_string,)
            ),
            "left_in_inbox" => match view.decision.reason.as_deref() {
                Some(reason) => println!("Decision     left in the inbox ({reason})"),
                None => println!("Decision     left in the inbox"),
            },
            other => println!("Decision     {other}"),
        }
        let verb = if view.applied { "assigned" } else { "proposed" };
        println!("Applied      {verb}");
        println!();
    }
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
    for attachment in &view.attachments {
        println!(
            "Attachment   {}\tsha256:{}",
            attachment.filename, attachment.sha256
        );
    }
    if let Some(apply) = apply {
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
        if let Some(filing) = &apply.nef {
            println!("Filing       {}", filing.outcome);
        }
        if apply.attachments.filed > 0 {
            println!(
                "Attachments  {} filed as internal documents",
                apply.attachments.filed
            );
        }
        if apply.attachments.unfiled > 0 {
            println!(
                "Attachments  {} not filed; re-run with --apply --kind <kind> to file them",
                apply.attachments.unfiled
            );
        }
    } else {
        if let (Some(route), Some(recorded_at)) = (&view.recorded_route, &view.recorded_at) {
            println!("Recorded     {route} at {recorded_at}");
        }
        if let Some(outcome) = &view.recorded_nef_outcome {
            println!("Filing       {outcome}");
        }
    }
}

/// Read one message's plan from the process door; records nothing.
async fn plan(base: &str, token: &str, message: Uuid) -> Result<ProcessView> {
    let text = call_process(base, token, &process_body(message, false, None)).await?;
    serde_json::from_str(&text).context("parse the process plan")
}

async fn call_process(base: &str, token: &str, body: &serde_json::Value) -> Result<String> {
    let (method, url) = process_request(base);
    let response = reqwest::Client::new()
        .request(method, url)
        .bearer_auth(token)
        .json(body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .context("call the process door")?;
    let status = response.status();
    let text = response.text().await.context("read the process response")?;
    if !status.is_success() {
        return Err(anyhow!("inbox process refused: {status} {text}"));
    }
    Ok(text)
}

/// Fetch the plan's attachments and write them into `dir`. Every name and
/// digest is checked before a byte is written: bytes that do not hash to
/// the digest the plan lists refuse the whole run.
async fn download(
    base: &str,
    token: &str,
    view: &ProcessView,
    dir: &Path,
    only: Option<&str>,
) -> Result<()> {
    use crate::mail::attachments::{
        checked_filename, disambiguate, format_report, glob_matches, write_reports,
        ExtractedAttachment,
    };
    let client = reqwest::Client::new();
    let mut fetched = Vec::new();
    for attachment in &view.attachments {
        let name = checked_filename(&attachment.filename)?;
        if only.is_some_and(|pattern| !glob_matches(pattern, &name)) {
            continue;
        }
        let (method, url) = attachment_request(base);
        let response = client
            .request(method, url)
            .bearer_auth(token)
            .json(&serde_json::json!({ "message": view.id, "asset": attachment.asset_id }))
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await
            .context("call the attachment door")?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow!("inbox attachments refused: {status} {text}"));
        }
        let bytes = response.bytes().await.context("read the attachment")?;
        if store::assets::sha256_hex(&bytes) != attachment.sha256 {
            return Err(anyhow!(
                "{name} does not match the digest its message records; nothing was written"
            ));
        }
        fetched.push(ExtractedAttachment {
            name,
            bytes: bytes.to_vec(),
        });
    }
    if fetched.is_empty() {
        println!("No attachments matched; nothing was written");
        return Ok(());
    }
    for report in write_reports(dir, &disambiguate(fetched))? {
        println!("{}", format_report(&report));
    }
    Ok(())
}

/// The commands a dry run leaves the lawyer to choose from.
fn next_actions(view: &ProcessView, downloaded: bool) -> Vec<String> {
    let id = view.id;
    let mut actions = Vec::new();
    if view.attachments.is_empty() {
        actions.push(format!("navigator inbox process {id} --apply"));
        return actions;
    }
    if !downloaded {
        actions.push(format!("navigator inbox attachments {id} --dir <dir>"));
    }
    if view.project_id.is_some() {
        actions.push(format!(
            "navigator inbox process {id} --apply --kind <kind>"
        ));
    } else {
        actions.push(format!(
            "navigator inbox process {id} --apply  (attachments file once the conversation has a matter)"
        ));
    }
    actions
}

async fn process(
    message: Uuid,
    apply: bool,
    gmail: Option<&str>,
    kind: Option<&str>,
    dir: Option<&Path>,
    host: Option<&str>,
) -> Result<()> {
    let (base, token) = crate::remote::resolve(host)?;
    if !apply {
        let view = plan(&base, &token, message).await?;
        print_plan(&view, None);
        if let Some(dir) = dir {
            download(&base, &token, &view, dir, None).await?;
        }
        println!("Dry run — nothing recorded. Next:");
        for action in next_actions(&view, dir.is_some()) {
            println!("  {action}");
        }
        return Ok(());
    }
    let text = call_process(&base, &token, &process_body(message, true, kind)).await?;
    let apply_view: ProcessApplyView =
        serde_json::from_str(&text).context("parse the process response")?;
    print_plan(&apply_view.result, Some(&apply_view));
    if let Some(dir) = dir {
        download(&base, &token, &apply_view.result, dir, None).await?;
    }
    apply_view.require_completed_filing()?;
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

        let body = process_body(message, false, None);
        assert_eq!(body["message"], serde_json::json!(message));
        assert_eq!(body["apply"], serde_json::json!(false));
        assert!(body.get("kind").is_none(), "no kind is sent unless named");

        let body = process_body(message, true, Some("exhibit"));
        assert_eq!(body["message"], serde_json::json!(message));
        assert_eq!(body["apply"], serde_json::json!(true));
        assert_eq!(body["kind"], serde_json::json!("exhibit"));
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

    fn applied_response() -> serde_json::Value {
        serde_json::from_str(
            r#"{
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
            "nef": {"outcome": "filed"},
            "slack_notice": "no_project"
        }"#,
        )
        .unwrap()
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
        serve_each(vec![(status_line.to_owned(), body.into_bytes())]).await
    }

    /// Serve the canned responses in order, one connection each.
    async fn serve_each(responses: Vec<(String, Vec<u8>)>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for (status_line, body) in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf).await;
                let head = format!(
                    "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\ncontent-length: \
                     {}\r\nconnection: close\r\n\r\n",
                    body.len(),
                );
                socket.write_all(head.as_bytes()).await.unwrap();
                socket.write_all(&body).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        format!("http://{addr}")
    }

    const ATTACHMENT_BYTES: &[u8] = b"%PDF-1.4 synthetic engagement letter";

    /// A triage plan listing one attachment whose digest is `sha256`.
    fn plan_with_attachment(sha256: &str) -> String {
        let mut plan: serde_json::Value = serde_json::from_str(PLAN_JSON).unwrap();
        plan["attachments"] = serde_json::json!([{
            "asset_id": "00000000-0000-0000-0000-000000000009",
            "filename": "engagement letter.pdf",
            "position": 0,
            "sha256": sha256,
        }]);
        plan.to_string()
    }

    #[tokio::test]
    async fn attachments_writes_each_listed_file_and_a_rerun_leaves_it_alone() {
        let _lock = CREDENTIALS_LOCK.lock().await;
        let previous = credentials_previous();
        let out = tempfile::tempdir().unwrap();
        let plan = plan_with_attachment(&store::assets::sha256_hex(ATTACHMENT_BYTES));
        for _ in 0..2 {
            let base = serve_each(vec![
                ("200 OK".into(), plan.clone().into_bytes()),
                ("200 OK".into(), ATTACHMENT_BYTES.to_vec()),
            ])
            .await;
            stored_login(&base);
            let exit = run(InboxAction::Attachments {
                message: Uuid::from_u128(7),
                dir: out.path().to_path_buf(),
                only: None,
                host: Some(base),
            })
            .await;
            assert_eq!(exit, std::process::ExitCode::SUCCESS);
        }
        assert_eq!(
            std::fs::read(out.path().join("engagement letter.pdf")).unwrap(),
            ATTACHMENT_BYTES
        );
        restore_credentials(previous);
    }

    #[tokio::test]
    async fn bytes_that_miss_the_listed_digest_write_nothing() {
        let _lock = CREDENTIALS_LOCK.lock().await;
        let previous = credentials_previous();
        let out = tempfile::tempdir().unwrap();
        let base = serve_each(vec![
            (
                "200 OK".into(),
                plan_with_attachment(&store::assets::sha256_hex(b"other bytes")).into_bytes(),
            ),
            ("200 OK".into(), ATTACHMENT_BYTES.to_vec()),
        ])
        .await;
        stored_login(&base);

        let error = super::process(
            Uuid::from_u128(7),
            false,
            None,
            None,
            Some(out.path()),
            Some(&base),
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("does not match the digest"), "{error}");
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
        restore_credentials(previous);
    }

    #[tokio::test]
    async fn only_skips_unmatched_attachments_without_fetching_them() {
        let _lock = CREDENTIALS_LOCK.lock().await;
        let previous = credentials_previous();
        let out = tempfile::tempdir().unwrap();
        // One response only: a fetch of the unmatched attachment would hang.
        let base = serve_once(
            "200 OK",
            plan_with_attachment(&store::assets::sha256_hex(ATTACHMENT_BYTES)),
        )
        .await;
        stored_login(&base);

        let exit = run(InboxAction::Attachments {
            message: Uuid::from_u128(7),
            dir: out.path().to_path_buf(),
            only: Some("*.docx".into()),
            host: Some(base),
        })
        .await;

        assert_eq!(exit, std::process::ExitCode::SUCCESS);
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
        restore_credentials(previous);
    }

    #[test]
    fn a_dry_run_names_the_next_commands() {
        let mut view: super::ProcessView = serde_json::from_str(PLAN_JSON).unwrap();
        assert_eq!(
            super::next_actions(&view, false),
            ["navigator inbox process 00000000-0000-0000-0000-000000000007 --apply"]
        );

        view = serde_json::from_str(&plan_with_attachment("00")).unwrap();
        let triage = super::next_actions(&view, false);
        assert!(triage[0].contains("inbox attachments"), "{triage:?}");
        assert!(triage[1].contains("once the conversation has a matter"));

        view.project_id = Some(Uuid::from_u128(3));
        let attributed = super::next_actions(&view, true);
        assert_eq!(attributed.len(), 1, "already downloaded: {attributed:?}");
        assert!(attributed[0].ends_with("--apply --kind <kind>"));
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
            kind: None,
            dir: None,
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
        let apply_json = applied_response().to_string();
        let base = serve_once("200 OK", apply_json.clone()).await;
        stored_login(&base);

        let exit = run(InboxAction::Process {
            message: Uuid::from_u128(7),
            apply: true,
            gmail: None,
            kind: None,
            dir: None,
            host: Some(base),
        })
        .await;

        assert_eq!(exit, std::process::ExitCode::SUCCESS);
        restore_credentials(previous);
    }

    #[tokio::test]
    async fn triage_and_unconfirmed_filings_fail_before_archiving_gmail() {
        let _lock = CREDENTIALS_LOCK.lock().await;
        let previous = credentials_previous();
        for outcome in [
            Some("no_project"),
            Some("not_a_notice"),
            Some("no_case_number"),
            Some("future_unknown"),
            None,
        ] {
            let mut response = applied_response();
            response["nef"] = outcome.map_or(
                serde_json::Value::Null,
                |outcome| serde_json::json!({"outcome": outcome}),
            );
            let base = serve_once("200 OK", response.to_string()).await;
            stored_login(&base);
            let error = super::process(
                Uuid::from_u128(7),
                true,
                Some("invalid-gmail-reference"),
                None,
                None,
                Some(&base),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(
                error.contains("NEF filing"),
                "the filing gate refuses before any Gmail adapter: {error}"
            );
            assert!(error.contains("Gmail copy left in the inbox"));
        }
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
            kind: None,
            dir: None,
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

    #[test]
    fn the_assign_matches_body_and_view_read_as_the_door_serves_them() {
        let body = super::assign_matches_body(true);
        assert_eq!(body["apply"], serde_json::json!(true));
        assert_eq!(
            super::assign_matches_body(false)["apply"],
            serde_json::json!(false)
        );

        let (method, url) = super::assign_matches_request("https://navigator.example.test");
        assert_eq!(method, Method::POST);
        assert_eq!(
            url,
            "https://navigator.example.test/app/api/inbox/assign-matches"
        );

        let views: Vec<super::AssignMatchView> = serde_json::from_str(
            r#"[{
                "message": "00000000-0000-0000-0000-000000000007",
                "conversation_id": "00000000-0000-0000-0000-000000000001",
                "from": "client@example.com",
                "subject": "Retainer",
                "decision": {
                    "decision": "assigned",
                    "project_id": "00000000-0000-0000-0000-000000000003"
                },
                "applied": false
            }, {
                "message": "00000000-0000-0000-0000-000000000008",
                "conversation_id": "00000000-0000-0000-0000-000000000002",
                "from": "efiling-notice@courts.example.test",
                "subject": null,
                "decision": {"decision": "left_in_inbox", "reason": "no_match"},
                "applied": false
            }]"#,
        )
        .unwrap();
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].decision.decision, "assigned");
        assert_eq!(views[0].decision.project_id, Some(Uuid::from_u128(3)));
        assert!(!views[0].applied);
        assert_eq!(views[1].decision.decision, "left_in_inbox");
        assert_eq!(views[1].decision.reason.as_deref(), Some("no_match"));
        assert!(views[1].decision.project_id.is_none());
        assert!(!views[1].applied);
    }

    #[tokio::test]
    async fn assign_matches_follows_the_continuation_cursor() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _lock = CREDENTIALS_LOCK.lock().await;
        let previous = credentials_previous();
        let server = MockServer::start().await;
        let cursor = serde_json::json!(["2026-01-01T00:00:00Z", Uuid::nil()]);
        Mock::given(method("POST"))
            .and(path("/app/api/inbox/assign-matches"))
            .and(body_json(serde_json::json!({"apply": false})))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-navigator-next-cursor", cursor.to_string())
                    .set_body_json(serde_json::json!([])),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/app/api/inbox/assign-matches"))
            .and(body_json(
                serde_json::json!({"apply": false, "after": cursor}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server)
            .await;
        let base = server.uri();
        stored_login(&base);

        let result = super::assign_matches(true, Some(&base)).await;

        restore_credentials(previous);
        result.unwrap();
        server.verify().await;
    }

    #[tokio::test]
    async fn assign_matches_prints_proposals_from_a_mocked_door() {
        let _lock = CREDENTIALS_LOCK.lock().await;
        let previous = credentials_previous();
        let base = serve_once(
            "200 OK",
            r#"[
                {
                    "message": "00000000-0000-0000-0000-000000000007",
                    "conversation_id": "00000000-0000-0000-0000-000000000001",
                    "from": "client@example.com",
                    "subject": "Retainer",
                    "decision": {
                        "decision": "assigned",
                        "project_id": "00000000-0000-0000-0000-000000000003"
                    },
                    "applied": false
                },
                {
                    "message": "00000000-0000-0000-0000-000000000008",
                    "conversation_id": "00000000-0000-0000-0000-000000000002",
                    "from": "stranger@example.com",
                    "subject": "Hello",
                    "decision": {"decision": "left_in_inbox", "reason": "no_sender_person"},
                    "applied": false
                }
            ]"#
            .to_owned(),
        )
        .await;
        stored_login(&base);

        let exit = run(InboxAction::AssignMatches {
            dry_run: true,
            host: Some(base),
        })
        .await;

        assert_eq!(exit, std::process::ExitCode::SUCCESS);
        restore_credentials(previous);
    }
}
