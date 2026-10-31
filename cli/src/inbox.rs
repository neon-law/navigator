//! `navigator inbox import` — import a lawyer's own correspondence into the
//! Firm Inbox through the server door `POST /app/api/inbox/import`: an RFC
//! 5322 `.eml` (or a directory of them) with `--source=email`, or a `LinkedIn`
//! data export's `messages.csv` with `--source=linkedin`.
//!
//! Ingest-only: the verb has no send counterpart, matching the documented
//! `navigator mail` posture. Idempotency is the door's own — the door's
//! dedup is the store's — so replaying the same payload prints the existing
//! conversation and message ids as deduped and exits 0 (AC3). An interrupted
//! run never silently retries a possibly-committed write: the error names the
//! reconcile step, which for imports is the idempotent re-run itself (AC11).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use clap::Subcommand;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{remote, HostOpt};

/// The sources the import door's adapters parse. Client-side validation
/// fails fast; the door re-derives the adapter from `source` and remains the
/// authority on unknown values.
const SOURCES: [&str; 2] = ["email", "linkedin"];

/// What `navigator inbox import` imports.
#[derive(Debug, Clone, Default)]
pub struct NewInboxImport {
    pub source: String,
    pub mailbox: String,
    pub files: Vec<PathBuf>,
    pub dir: Option<PathBuf>,
    pub project: Option<String>,
    pub dry_run: bool,
    pub host: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum InboxCmd {
    /// Import a lawyer's correspondence payload into the Firm Inbox: an
    /// RFC 5322 `.eml` with `--source=email`, or a `LinkedIn` data export's
    /// `messages.csv` with `--source=linkedin`. Idempotent — replaying the
    /// same payload reports the existing ids as deduped. Ingest-only: there
    /// is no send verb.
    Import {
        /// Which adapter parses each payload: `email` or `linkedin`.
        #[arg(long)]
        source: String,
        /// The importing account: the lawyer's mailbox address (email) or
        /// their `LinkedIn` profile URL. Becomes the Inbox `provider_account`.
        #[arg(long)]
        mailbox: String,
        /// One payload file; repeat to import several in one run.
        #[arg(long)]
        file: Vec<PathBuf>,
        /// A directory whose payload files (dotfiles skipped) import together.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Matter code to attribute the imported conversations to at import.
        #[arg(long)]
        project: Option<String>,
        /// Print each request JSON and send nothing.
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        host: HostOpt,
    },
}

/// The payloads one run imports: every explicit `--file`, plus — when
/// `--dir` is given — that directory's regular files (dotfiles skipped),
/// sorted, so a bulk run is a stable loop over the door.
fn collect_payloads(files: &[PathBuf], dir: Option<&Path>) -> Result<Vec<PathBuf>> {
    let mut payloads = Vec::new();
    // Canonical keys for the explicit files, so a --dir that contains one of
    // them (directly or through a symlink) cannot submit it twice in one run.
    let explicit = files
        .iter()
        .map(|file| file.canonicalize().unwrap_or_else(|_| file.clone()))
        .collect::<std::collections::HashSet<_>>();
    for file in files {
        if !file.is_file() {
            return Err(anyhow!("{} is not a readable file", file.display()));
        }
        payloads.push(file.clone());
    }
    if let Some(dir) = dir {
        let entries = std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))?;
        let mut dir_payloads = entries
            .map(|entry| {
                entry
                    .with_context(|| format!("read {}", dir.display()))
                    .map(|entry| entry.path())
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        dir_payloads.retain(|path| {
            path.is_file()
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        });
        dir_payloads.retain(|path| {
            !explicit.contains(&path.canonicalize().unwrap_or_else(|_| path.clone()))
        });
        dir_payloads.sort();
        payloads.extend(dir_payloads);
    }
    if payloads.is_empty() {
        return Err(anyhow!(
            "pass --file=<payload> (repeatable) or --dir=<directory> naming what to import"
        ));
    }
    Ok(payloads)
}

/// The door's request for one payload. `project` is the matter code the door
/// attributes the conversations to; the door's `dry_run` field is never sent
/// — this verb's dry run prints the request and sends nothing.
fn request_body(source: &str, mailbox: &str, project: Option<&str>, bytes: &[u8]) -> Value {
    json!({
        "source": source,
        "mailbox": mailbox,
        "payload_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
        "project": project,
    })
}

/// One ingested message as the door reports it.
#[derive(Debug, Deserialize)]
struct ImportedMessage {
    message_id: uuid::Uuid,
    conversation_id: uuid::Uuid,
    deduped: bool,
    direction: String,
    #[serde(default)]
    attachments: Vec<ImportedAttachment>,
}

#[derive(Debug, Deserialize)]
struct ImportedAttachment {
    filename: String,
    asset_id: uuid::Uuid,
}

#[derive(Debug, Deserialize)]
struct InboxImportResponse {
    archived_key: String,
    imported: Vec<ImportedMessage>,
}

/// The response text's first line for error bodies — house convention
/// (`remote.rs::first_line`), copied because that helper is private.
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().to_string()
}

/// The reconcile text every interrupted import names: a replayed payload is
/// deduped to the existing rows, so re-running the same command is safe.
const RECONCILE: &str = "import outcome unknown; re-run `navigator inbox import` — a replayed payload is deduped to the existing rows";

fn print_imported(payload: &Path, imported: &InboxImportResponse) {
    println!(
        "{}: {} message(s), archived {}",
        payload.display(),
        imported.imported.len(),
        imported.archived_key
    );
    for message in &imported.imported {
        let deduped = if message.deduped { " (deduped)" } else { "" };
        println!(
            "  {} {} conversation {}{}",
            message.message_id, message.direction, message.conversation_id, deduped
        );
        for attachment in &message.attachments {
            println!(
                "    attachment {} asset {}",
                attachment.filename, attachment.asset_id
            );
        }
    }
}

async fn import(new: NewInboxImport) -> Result<()> {
    let source = new.source.trim();
    if !SOURCES.contains(&source) {
        return Err(anyhow!(
            "unknown --source `{source}`: expected one of {}",
            SOURCES.join(", ")
        ));
    }
    let mailbox = new.mailbox.trim();
    if mailbox.is_empty() {
        return Err(anyhow!(
            "--mailbox is required: the importing account, e.g. --mailbox=nick@neonlaw.com"
        ));
    }
    let payloads = collect_payloads(&new.files, new.dir.as_deref())?;

    // A dry run prints the request JSON whether or not the caller is logged
    // in — it sends nothing, so it resolves no session (AC11).
    let (base, token, client) = if new.dry_run {
        (String::new(), String::new(), None)
    } else {
        let (base, token) = remote::resolve(new.host.as_deref())?;
        (base, token, Some(reqwest::Client::new()))
    };

    for payload in payloads {
        let bytes =
            std::fs::read(&payload).with_context(|| format!("read {}", payload.display()))?;
        if bytes.is_empty() {
            return Err(anyhow!("{} is empty", payload.display()));
        }
        let body = request_body(source, mailbox, new.project.as_deref(), &bytes);
        if new.dry_run {
            println!("{body}");
            continue;
        }
        let url = format!("{base}/app/api/inbox/import");
        let response = client
            .as_ref()
            .expect("non-dry runs build a client")
            .post(&url)
            .timeout(Duration::from_secs(30))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}: {RECONCILE}"))?;
        let status = response.status();
        let text = response.text().await.context("read import response")?;
        if !status.is_success() {
            return Err(anyhow!("import refused: {status} {}", first_line(&text)));
        }
        let imported: InboxImportResponse = serde_json::from_str(&text)
            .with_context(|| format!("parse import response: {RECONCILE}"))?;
        print_imported(&payload, &imported);
    }
    Ok(())
}

pub(crate) async fn run(new: NewInboxImport) -> ExitCode {
    match import(new).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator inbox import: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{collect_payloads, request_body};
    use base64::Engine as _;

    #[test]
    fn the_request_names_source_mailbox_project_and_the_verbatim_bytes() {
        let body = request_body(
            "email",
            "nick@neonlaw.com",
            Some("24-1043"),
            b"From: counsel@example.com\r\n",
        );
        assert_eq!(body["source"], "email");
        assert_eq!(body["mailbox"], "nick@neonlaw.com");
        assert_eq!(body["project"], "24-1043");
        let encoded = body["payload_base64"].as_str().unwrap();
        assert_eq!(
            String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .unwrap()
            )
            .unwrap(),
            "From: counsel@example.com\r\n"
        );
        // The door's dry-run field is the CLI's --dry-run only; the CLI
        // never sends a dry-run request.
        assert!(body.get("dry_run").is_none());

        let unattributed = request_body("linkedin", "https://linkedin.com/in/nick", None, b"");
        assert_eq!(unattributed["project"], serde_json::Value::Null);
    }

    #[test]
    fn payload_collection_unions_files_with_dir_and_skips_dotfiles() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.eml"), "b").unwrap();
        std::fs::write(dir.path().join("a.eml"), "a").unwrap();
        std::fs::write(dir.path().join(".hidden.eml"), "h").unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();

        // Explicit files keep their order; --dir adds its own files sorted
        // (dotfiles and directories skipped).
        let payloads = collect_payloads(&[dir.path().join("b.eml")], Some(dir.path())).unwrap();
        assert_eq!(
            payloads,
            vec![dir.path().join("b.eml"), dir.path().join("a.eml")]
        );

        // A directory with no files (only the `nested` directory) is an error.
        let nested = dir.path().join("nested");
        assert!(collect_payloads(&[], Some(&nested)).is_err());

        // A named directory as an explicit --file is an error.
        assert!(collect_payloads(&[nested], None).is_err());
    }
}
