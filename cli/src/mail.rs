//! Gmail access for `navigator mail`.

use std::fmt::Write as _;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use clap::Subcommand;
use reqwest::{Method, Response};
use serde::Deserialize;
use serde_json::json;
use url::Url;
use uuid::Uuid;

const GMAIL_API: &str = "https://gmail.googleapis.com/gmail/v1";
const GOOGLE_TOKEN_API: &str = "https://oauth2.googleapis.com/token";
const MAX_SEARCH_RESULTS: u8 = 25;
/// Gmail's 25 MB message limit, measured on the encoded MIME message.
const MAX_MESSAGE_BYTES: u64 = 25_000_000;

const DRAFT_ATTACH_HELP: &str = "\
Attachments:
  --attach is repeatable. Each file is read from disk, keeps its file name, and
  takes its MIME type from the extension (.pdf is application/pdf; an unknown
  extension is application/octet-stream). The encoded draft must fit Gmail's
  25 MB message limit, checked before any network call.

  With --draft-id, the update replaces the whole draft: the --attach list
  replaces the draft's previous attachments, and an update without --attach
  leaves the draft with none. The draft stays unsent.";

#[derive(Subcommand)]
pub enum MailAction {
    /// Search Gmail threads, scoped to the current Project when possible.
    Search { query: String },
    /// Read a Gmail thread as plain text.
    Read { thread: String },
    /// Create or update an unsent draft. Use --thread to draft a reply.
    #[command(after_long_help = DRAFT_ATTACH_HELP)]
    Draft {
        /// Recipient. Repeat or comma-separate for several. Defaults to the latest sender when
        /// replying with --thread.
        #[arg(long, value_delimiter = ',')]
        to: Vec<String>,
        /// Cc recipient. Repeat or comma-separate for several.
        #[arg(long, value_delimiter = ',')]
        cc: Vec<String>,
        /// Bcc recipient. Repeat or comma-separate for several.
        #[arg(long, value_delimiter = ',')]
        bcc: Vec<String>,
        /// Subject. Defaults to the latest thread subject when replying.
        #[arg(long)]
        subject: Option<String>,
        /// Plain-text draft body.
        #[arg(long)]
        body_file: PathBuf,
        /// Gmail thread ID to reply within.
        #[arg(long)]
        thread: Option<String>,
        /// Existing Gmail draft ID to update.
        #[arg(long)]
        draft_id: Option<String>,
        /// Attach a file read from disk. Repeat for more files.
        #[arg(long = "attach", value_name = "PATH")]
        attach: Vec<PathBuf>,
    },
    /// File every message in a Gmail thread as a matter Communication.
    File {
        thread: String,
        /// Matter code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        /// Navigator host. Defaults to host in navigator.yaml.
        #[arg(long)]
        host: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    SearchThreads,
    ReadThread,
    ReadProfile,
    CreateDraft,
    UpdateDraft,
}

fn endpoint(
    operation: Operation,
    id: Option<&str>,
) -> (&'static str, Vec<&'static str>, Option<String>) {
    match operation {
        Operation::SearchThreads => ("GET", vec!["users", "me", "threads"], None),
        Operation::ReadThread => ("GET", vec!["users", "me", "threads"], id.map(str::to_owned)),
        Operation::ReadProfile => ("GET", vec!["users", "me", "profile"], None),
        Operation::CreateDraft => ("POST", vec!["users", "me", "drafts"], None),
        Operation::UpdateDraft => ("PUT", vec!["users", "me", "drafts"], id.map(str::to_owned)),
    }
}

struct GmailClient {
    http: reqwest::Client,
    access_token: String,
    base_url: Url,
}

impl GmailClient {
    fn new(access_token: String) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::new(),
            access_token,
            base_url: Url::parse(GMAIL_API).context("parse Gmail API base URL")?,
        })
    }

    #[cfg(test)]
    fn with_base_url(access_token: String, base_url: Url) -> Self {
        Self {
            http: reqwest::Client::new(),
            access_token,
            base_url,
        }
    }

    fn url(&self, operation: Operation, id: Option<&str>) -> Result<Url> {
        let (_, segments, item_id) = endpoint(operation, id);
        let mut url = self.base_url.clone();
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|()| anyhow!("Gmail API base URL cannot accept path segments"))?;
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
            if let Some(item_id) = item_id {
                path.push(&item_id);
            }
        }
        Ok(url)
    }

    async fn call<T: for<'de> Deserialize<'de>>(
        &self,
        operation: Operation,
        id: Option<&str>,
        query: &[(&str, &str)],
        body: Option<serde_json::Value>,
    ) -> Result<T> {
        let (method, _, _) = endpoint(operation, id);
        let mut request = self
            .http
            .request(
                Method::from_bytes(method.as_bytes())?,
                self.url(operation, id)?,
            )
            .bearer_auth(&self.access_token)
            .query(query)
            .timeout(std::time::Duration::from_secs(30));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.context("call Gmail API")?;
        parse_response(response, "Gmail API").await
    }

    async fn search(&self, query: &str) -> Result<Vec<ThreadSummary>> {
        let response: ThreadList = self
            .call(
                Operation::SearchThreads,
                None,
                &[
                    ("q", query),
                    ("maxResults", &MAX_SEARCH_RESULTS.to_string()),
                ],
                None,
            )
            .await?;
        Ok(response.threads.unwrap_or_default())
    }

    async fn thread(&self, id: &str) -> Result<GmailThread> {
        self.call(Operation::ReadThread, Some(id), &[("format", "full")], None)
            .await
    }

    async fn profile(&self) -> Result<GmailProfile> {
        self.call(Operation::ReadProfile, None, &[], None).await
    }

    async fn save_draft(&self, draft: &DraftMessage) -> Result<DraftResponse> {
        let (operation, id) = match draft.id.as_deref() {
            Some(id) => (Operation::UpdateDraft, Some(id)),
            None => (Operation::CreateDraft, None),
        };
        let body = json!({
            "message": {
                "raw": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(draft.raw.as_bytes()),
                "threadId": draft.thread_id,
            }
        });
        self.call(operation, id, &[], Some(body)).await
    }
}

async fn parse_response<T: for<'de> Deserialize<'de>>(
    response: Response,
    service: &str,
) -> Result<T> {
    let status = response.status();
    if !status.is_success() {
        // Provider error bodies may echo request values. Never copy them into
        // agent-visible diagnostics.
        return Err(anyhow!("{service} request failed: {status}"));
    }
    response
        .json()
        .await
        .with_context(|| format!("parse {service} response"))
}

#[derive(Debug, Deserialize)]
struct ThreadList {
    threads: Option<Vec<ThreadSummary>>,
}

#[derive(Debug, Deserialize)]
struct ThreadSummary {
    id: String,
    snippet: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GmailThread {
    id: String,
    messages: Vec<GmailMessage>,
}

#[derive(Debug, Deserialize)]
struct GmailMessage {
    id: String,
    #[serde(rename = "internalDate")]
    internal_date: Option<String>,
    payload: Option<MessagePart>,
}

#[derive(Debug, Deserialize)]
struct MessagePart {
    #[serde(rename = "mimeType")]
    mime_type: Option<String>,
    headers: Option<Vec<MessageHeader>>,
    body: Option<MessageBody>,
    parts: Option<Vec<MessagePart>>,
    filename: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MessageHeader {
    name: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct MessageBody {
    data: Option<String>,
    #[serde(rename = "attachmentId")]
    attachment_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GmailProfile {
    #[serde(rename = "emailAddress")]
    email_address: String,
}

#[derive(Debug, Deserialize)]
struct DraftResponse {
    id: String,
}

struct DraftMessage {
    id: Option<String>,
    thread_id: Option<String>,
    raw: String,
}

fn header<'a>(message: &'a GmailMessage, name: &str) -> Option<&'a str> {
    message
        .payload
        .as_ref()?
        .headers
        .as_ref()?
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str())
}

fn decoded_body(part: &MessagePart) -> Option<String> {
    let data = part.body.as_ref()?.data.as_deref()?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(data)
        .ok()?;
    String::from_utf8(bytes).ok()
}

fn text_parts(part: &MessagePart, mime_type: &str, output: &mut Vec<String>) {
    if part.mime_type.as_deref() == Some(mime_type) {
        if let Some(body) = decoded_body(part) {
            if !body.trim().is_empty() {
                output.push(body);
            }
        }
    }
    if let Some(parts) = &part.parts {
        for child in parts {
            text_parts(child, mime_type, output);
        }
    }
}

fn plain_text(part: &MessagePart) -> Option<String> {
    let mut text = Vec::new();
    text_parts(part, "text/plain", &mut text);
    if !text.is_empty() {
        return Some(text.join("\n\n"));
    }
    let mut html = Vec::new();
    text_parts(part, "text/html", &mut html);
    (!html.is_empty()).then(|| {
        html.into_iter()
            .map(|body| mail_parser::decoders::html::html_to_text(&body))
            .collect::<Vec<_>>()
            .join("\n\n")
    })
}

fn attachments(part: &MessagePart, output: &mut Vec<String>) {
    let has_payload = part
        .body
        .as_ref()
        .is_some_and(|body| body.attachment_id.is_some());
    if has_payload || part.filename.is_some() {
        output.push(
            part.filename
                .clone()
                .unwrap_or_else(|| "(unnamed attachment)".into()),
        );
    }
    if let Some(parts) = &part.parts {
        for child in parts {
            attachments(child, output);
        }
    }
}

fn without_signature<'a>(body: &'a str, configured_signature: Option<&str>) -> &'a str {
    if let Some(signature) = configured_signature
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if let Some(start) = body.trim_end().strip_suffix(signature) {
            return start.trim_end();
        }
    }
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        if content == "-- " || content == "--" {
            return body[..offset].trim_end();
        }
        offset += line.len();
    }
    body.trim_end()
}

fn safe_header<'a>(value: &'a str, name: &str) -> Result<&'a str> {
    if value.contains(['\r', '\n']) {
        return Err(anyhow!("{name} cannot contain a line break"));
    }
    Ok(value.trim())
}

/// Joins recipients into one header value, refusing line breaks and blanks.
fn address_list(values: &[String], name: &str) -> Result<String> {
    let mut cleaned = Vec::new();
    for value in values {
        let value = safe_header(value, name)?;
        if !value.is_empty() {
            cleaned.push(value);
        }
    }
    Ok(cleaned.join(", "))
}

struct Attachment {
    filename: String,
    mime_type: &'static str,
    bytes: Vec<u8>,
}

fn mime_type_for(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("pdf") => "application/pdf",
        Some("doc") => "application/msword",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("xls") => "application/vnd.ms-excel",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("pptx") => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        Some("odt") => "application/vnd.oasis.opendocument.text",
        Some("rtf") => "application/rtf",
        Some("txt") => "text/plain",
        Some("md") => "text/markdown",
        Some("csv") => "text/csv",
        Some("html" | "htm") => "text/html",
        Some("ics") => "text/calendar",
        Some("eml") => "message/rfc822",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        Some("zip") => "application/zip",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("heic") => "image/heic",
        Some("tif" | "tiff") => "image/tiff",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

/// Size of `bytes` once base64-encoded in 76-character CRLF lines.
fn base64_encoded_len(bytes: u64) -> u64 {
    let encoded = bytes.div_ceil(3) * 4;
    encoded + encoded.div_ceil(76) * 2
}

fn attachment_filename(path: &Path) -> Result<String> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("attachment {} has no UTF-8 file name", path.display()))?;
    if filename.chars().any(char::is_control) {
        return Err(anyhow!(
            "attachment file name cannot contain a line break or control character"
        ));
    }
    Ok(filename.to_owned())
}

fn size_limit_error(what: &str, encoded: u64) -> anyhow::Error {
    anyhow!(
        "{what} is {encoded} bytes once encoded, over Gmail's {MAX_MESSAGE_BYTES}-byte message limit; nothing was sent to Gmail"
    )
}

/// Reads every attachment after checking, from file metadata alone, that each
/// file and the whole draft fit Gmail's message limit.
fn load_attachments(paths: &[PathBuf], body: &str) -> Result<Vec<Attachment>> {
    let mut total = body.len() as u64;
    for path in paths {
        let filename = attachment_filename(path)?;
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("read attachment {}", path.display()))?;
        if !metadata.is_file() {
            return Err(anyhow!("attachment {} is not a file", path.display()));
        }
        let encoded = base64_encoded_len(metadata.len());
        if encoded > MAX_MESSAGE_BYTES {
            return Err(size_limit_error(&format!("attachment {filename}"), encoded));
        }
        total += encoded;
    }
    if total > MAX_MESSAGE_BYTES {
        return Err(size_limit_error(
            &format!("the draft with {} attachments", paths.len()),
            total,
        ));
    }
    paths
        .iter()
        .map(|path| {
            Ok(Attachment {
                filename: attachment_filename(path)?,
                mime_type: mime_type_for(path),
                bytes: std::fs::read(path)
                    .with_context(|| format!("read attachment {}", path.display()))?,
            })
        })
        .collect()
}

fn encoded_word(value: &str) -> String {
    format!(
        "=?UTF-8?B?{}?=",
        base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
    )
}

/// `name="…"; filename="…"` parameters, RFC 2231-encoded when not ASCII.
fn filename_parameters(filename: &str) -> (String, String) {
    if filename.is_ascii() {
        let quoted = filename.replace('\\', "\\\\").replace('"', "\\\"");
        return (
            format!("name=\"{quoted}\""),
            format!("filename=\"{quoted}\""),
        );
    }
    let mut percent = String::new();
    for byte in filename.bytes() {
        if byte.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&byte) {
            percent.push(byte as char);
        } else {
            let _ = write!(percent, "%{byte:02X}");
        }
    }
    (
        format!("name=\"{}\"", encoded_word(filename)),
        format!("filename*=UTF-8''{percent}"),
    )
}

fn draft_raw(
    to: &[String],
    cc: &[String],
    bcc: &[String],
    subject: &str,
    body: &str,
    in_reply_to: Option<&str>,
    attachments: &[Attachment],
) -> Result<String> {
    let to = address_list(to, "recipient")?;
    let cc = address_list(cc, "cc recipient")?;
    let bcc = address_list(bcc, "bcc recipient")?;
    let subject = safe_header(subject, "subject")?;
    if to.is_empty() || subject.is_empty() {
        return Err(anyhow!("recipient and subject are required"));
    }
    let subject = if subject.is_ascii() {
        subject.to_owned()
    } else {
        encoded_word(subject)
    };
    let mut raw = format!("To: {to}\r\n");
    if !cc.is_empty() {
        let _ = write!(raw, "Cc: {cc}\r\n");
    }
    if !bcc.is_empty() {
        let _ = write!(raw, "Bcc: {bcc}\r\n");
    }
    let _ = write!(raw, "Subject: {subject}\r\nMIME-Version: 1.0\r\n");
    if let Some(message_id) = in_reply_to {
        let message_id = safe_header(message_id, "reply message id")?;
        if !message_id.is_empty() {
            raw.push_str("In-Reply-To: ");
            raw.push_str(message_id);
            raw.push_str("\r\nReferences: ");
            raw.push_str(message_id);
            raw.push_str("\r\n");
        }
    }
    let text = without_signature(
        body,
        std::env::var("NAVIGATOR_GMAIL_SIGNATURE").ok().as_deref(),
    )
    .replace("\r\n", "\n")
    .replace('\n', "\r\n");
    if attachments.is_empty() {
        raw.push_str("Content-Type: text/plain; charset=UTF-8\r\n\r\n");
        raw.push_str(&text);
        return Ok(raw);
    }
    let boundary = format!("navigator-mixed-{}", Uuid::new_v4().simple());
    if text.contains(&boundary) {
        return Err(anyhow!("draft body collides with the MIME boundary"));
    }
    let _ = write!(
            raw,
        "Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n--{boundary}\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\n{text}\r\n"
    );
    for attachment in attachments {
        let (name, filename) = filename_parameters(&attachment.filename);
        let _ = write!(
            raw,
            "--{boundary}\r\nContent-Type: {}; {name}\r\nContent-Disposition: attachment; {filename}\r\nContent-Transfer-Encoding: base64\r\n\r\n",
            attachment.mime_type
        );
        let encoded = base64::engine::general_purpose::STANDARD.encode(&attachment.bytes);
        for line in encoded.as_bytes().chunks(76) {
            raw.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
            raw.push_str("\r\n");
        }
    }
    let _ = write!(raw, "--{boundary}--\r\n");
    if raw.len() as u64 > MAX_MESSAGE_BYTES {
        return Err(size_limit_error("the draft", raw.len() as u64));
    }
    Ok(raw)
}

fn now_or_message_date(message: &GmailMessage) -> Result<String> {
    let millis = message
        .internal_date
        .as_deref()
        .ok_or_else(|| anyhow!("message has no timestamp"))?
        .parse::<i64>()
        .context("parse Gmail message timestamp")?;
    chrono::DateTime::from_timestamp_millis(millis)
        .map(|date| date.to_rfc3339())
        .ok_or_else(|| anyhow!("Gmail message timestamp is out of range"))
}

fn client_contact_from_manifest() -> Option<String> {
    let manifest = crate::projects::manifest::read(Path::new("."))?;
    manifest_project_contact(
        manifest.project.as_deref(),
        manifest.project.as_deref(),
        manifest.client_dri.as_deref(),
    )
}

fn current_project_context() -> (Option<String>, Option<String>) {
    let manifest = crate::projects::manifest::read(Path::new("."));
    let project = manifest
        .as_ref()
        .and_then(|manifest| manifest.project.clone());
    let contact = manifest
        .as_ref()
        .and_then(|manifest| manifest.client_dri.as_deref())
        .and_then(email_from_address);
    (project, contact)
}

fn manifest_project_contact(
    requested_project: Option<&str>,
    manifest_project: Option<&str>,
    client_dri: Option<&str>,
) -> Option<String> {
    if requested_project.is_none() || requested_project != manifest_project {
        return None;
    }
    email_from_address(client_dri?)
}

fn addresses(value: &str) -> impl Iterator<Item = String> + '_ {
    value.split([',', ';']).filter_map(|entry| {
        let address = entry
            .split_once('<')
            .and_then(|(_, rest)| rest.split_once('>').map(|(address, _)| address))
            .unwrap_or(entry)
            .trim()
            .trim_matches(['"', '\'']);
        let mut pieces = address.split('@');
        let local = pieces.next()?;
        let domain = pieces.next()?;
        if pieces.next().is_some()
            || local.is_empty()
            || domain.is_empty()
            || local.chars().any(char::is_whitespace)
            || domain.chars().any(char::is_whitespace)
        {
            return None;
        }
        Some(format!("{local}@{domain}").to_ascii_lowercase())
    })
}

fn email_from_address(value: &str) -> Option<String> {
    addresses(value).next()
}

fn message_mentions_contact(message: &GmailMessage, contact: &str) -> bool {
    ["From", "To", "Cc", "Reply-To"]
        .iter()
        .filter_map(|name| header(message, name))
        .flat_map(addresses)
        .any(|address| address == contact.to_ascii_lowercase())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProjectThreadFit {
    OutsideProject,
    Unverifiable,
    Match,
    Mismatch,
}

fn project_thread_fit(
    thread: &GmailThread,
    project: Option<&str>,
    contact: Option<&str>,
) -> ProjectThreadFit {
    if project.is_none() {
        return ProjectThreadFit::OutsideProject;
    }
    let Some(contact) = contact else {
        return ProjectThreadFit::Unverifiable;
    };
    if thread
        .messages
        .iter()
        .any(|message| message_mentions_contact(message, contact))
    {
        ProjectThreadFit::Match
    } else {
        ProjectThreadFit::Mismatch
    }
}

fn project_thread_warning(
    thread: &GmailThread,
    project: Option<&str>,
    contact: Option<&str>,
) -> Option<&'static str> {
    match project_thread_fit(thread, project, contact) {
        ProjectThreadFit::OutsideProject | ProjectThreadFit::Match => None,
        ProjectThreadFit::Unverifiable => {
            Some("Project client contact is unavailable; thread association cannot be verified")
        }
        ProjectThreadFit::Mismatch => {
            Some("thread does not appear to belong to this Project's client contact")
        }
    }
}

fn warn_if_project_thread_unverified(
    thread: &GmailThread,
    project: Option<&str>,
    contact: Option<&str>,
) {
    if let Some(warning) = project_thread_warning(thread, project, contact) {
        eprintln!("warning: {warning} (thread {})", thread.id);
    }
}

async fn read_project_thread(
    gmail: &GmailClient,
    thread_id: &str,
    project: Option<&str>,
    contact: Option<&str>,
) -> Result<GmailThread> {
    let thread = gmail.thread(thread_id).await?;
    warn_if_project_thread_unverified(&thread, project, contact);
    Ok(thread)
}

fn warn_if_project_draft_thread_unverified(project: Option<&str>, draft_id: Option<&str>) {
    if let Some(warning) = project_draft_thread_warning(project, draft_id) {
        eprintln!("warning: {warning}");
    }
}

fn project_draft_thread_warning(
    project: Option<&str>,
    draft_id: Option<&str>,
) -> Option<&'static str> {
    (project.is_some() && draft_id.is_some())
        .then_some("updated draft's thread cannot be checked against this Project's client contact")
}

fn source_reference(mailbox: &str, message_id: &str) -> String {
    format!("gmail:{}:{message_id}", mailbox.trim().to_ascii_lowercase())
}

fn scoped_query(query: &str, project: Option<&str>) -> String {
    let Some(email) = project.and_then(|_| client_contact_from_manifest()) else {
        return query.to_owned();
    };
    format!("({query}) (from:{email} OR to:{email})")
}

/// Names every missing Gmail setting at once, with the fix for each.
fn setup_from(account: Option<String>, client_id: Option<String>) -> Result<(String, String)> {
    let account = account.filter(|value| !value.trim().is_empty());
    let client_id = client_id.filter(|value| !value.trim().is_empty());
    let mut steps = Vec::new();
    if account.is_none() {
        steps.push(
            "export NAVIGATOR_GMAIL_ACCOUNT=<gmail address whose refresh token is in the macOS Keychain, service com.neonlaw.navigator.gmail.refresh-token>",
        );
    }
    if client_id.is_none() {
        steps.push("export NAVIGATOR_GMAIL_CLIENT_ID=<Google OAuth client id>");
    }
    match (account, client_id) {
        (Some(account), Some(client_id)) => Ok((account, client_id)),
        _ => Err(anyhow!(
            "Gmail is not set up for navigator mail. Set both before retrying (see docs/mail-cli.md): {}",
            steps.join("; ")
        )),
    }
}

async fn client() -> Result<GmailClient> {
    let (account, client_id) = setup_from(
        std::env::var("NAVIGATOR_GMAIL_ACCOUNT").ok(),
        std::env::var("NAVIGATOR_GMAIL_CLIENT_ID").ok(),
    )?;
    let refresh_token = keychain_refresh_token(&account)?;
    let response = reqwest::Client::new()
        .post(GOOGLE_TOKEN_API)
        .form(&[
            ("client_id", client_id.as_str()),
            ("refresh_token", refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .context("refresh Gmail access token")?;
    let token: TokenResponse = parse_response(response, "Google token endpoint").await?;
    GmailClient::new(token.access_token)
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
}

fn keychain_refresh_token(account: &str) -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        const REFRESH_TOKEN_SERVICE: &str = "com.neonlaw.navigator.gmail.refresh-token";
        let bytes = security_framework::passwords::generic_password(
            security_framework::passwords::PasswordOptions::new_generic_password(
                REFRESH_TOKEN_SERVICE,
                account,
            ),
        )
        .context("read Gmail refresh token from macOS Keychain")?;
        let token = String::from_utf8(bytes).context("Gmail refresh token is not UTF-8")?;
        if token.trim().is_empty() {
            return Err(anyhow!("Gmail refresh token is empty in Keychain"));
        }
        Ok(token)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = account;
        Err(anyhow!("Gmail Keychain access is supported only on macOS"))
    }
}

fn finish(result: Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator mail: {error:#}");
            ExitCode::FAILURE
        }
    }
}

pub async fn run(action: MailAction) -> ExitCode {
    finish(run_inner(action).await)
}

async fn run_inner(action: MailAction) -> Result<()> {
    match action {
        MailAction::Search { query } => {
            let project = crate::projects::manifest::read(Path::new("."))
                .and_then(|manifest| manifest.project);
            let query = scoped_query(&query, project.as_deref());
            let threads = client().await?.search(&query).await?;
            for thread in threads {
                println!("{}\t{}", thread.id, thread.snippet.unwrap_or_default());
            }
            Ok(())
        }
        MailAction::Read { thread } => {
            let (project, contact) = current_project_context();
            let thread = read_project_thread(
                &client().await?,
                &thread,
                project.as_deref(),
                contact.as_deref(),
            )
            .await?;
            print_thread(&thread);
            Ok(())
        }
        MailAction::Draft {
            to,
            cc,
            bcc,
            subject,
            body_file,
            thread,
            draft_id,
            attach,
        } => {
            let request = DraftRequest {
                to,
                cc,
                bcc,
                subject,
                body_file,
                thread,
                draft_id,
                attach,
            };
            let saved = save_draft_command(request, client).await?;
            println!(
                "Draft saved unsent with {} attachment(s). Gmail Drafts mailbox: https://mail.google.com/mail/#drafts (choose account {}). Gmail API draft ID: {}.",
                saved.attachments, saved.account, saved.id
            );
            Ok(())
        }
        MailAction::File {
            thread,
            project,
            host,
        } => file_thread(&thread, project.as_deref(), host.as_deref()).await,
    }
}

struct DraftRequest {
    to: Vec<String>,
    cc: Vec<String>,
    bcc: Vec<String>,
    subject: Option<String>,
    body_file: PathBuf,
    thread: Option<String>,
    draft_id: Option<String>,
    attach: Vec<PathBuf>,
}

struct SavedDraft {
    id: String,
    account: String,
    attachments: usize,
}

/// Reads the body and every attachment from disk, then connects to Gmail and
/// saves one unsent draft. Local refusals happen before `connect` runs.
async fn save_draft_command<F, Fut>(request: DraftRequest, connect: F) -> Result<SavedDraft>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<GmailClient>>,
{
    let DraftRequest {
        to,
        cc,
        bcc,
        subject,
        body_file,
        thread,
        draft_id,
        attach,
    } = request;
    let body = std::fs::read_to_string(&body_file)
        .with_context(|| format!("read draft body {}", body_file.display()))?;
    if body.trim().is_empty() {
        return Err(anyhow!("draft body is empty"));
    }
    let attachments = load_attachments(&attach, &body)?;
    let gmail = connect().await?;
    let profile = gmail.profile().await?;
    let (project, contact) = current_project_context();
    let (to, subject, in_reply_to) = if let Some(thread_id) = thread.as_deref() {
        let thread_data =
            read_project_thread(&gmail, thread_id, project.as_deref(), contact.as_deref()).await?;
        let latest = thread_data
            .messages
            .last()
            .ok_or_else(|| anyhow!("thread has no messages"))?;
        let to = if to.is_empty() {
            vec![header(latest, "Reply-To")
                .or_else(|| header(latest, "From"))
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("reply recipient is unavailable; pass --to"))?]
        } else {
            to
        };
        let subject = subject
            .or_else(|| header(latest, "Subject").map(reply_subject))
            .ok_or_else(|| anyhow!("reply subject is unavailable; pass --subject"))?;
        let message_id = header(latest, "Message-ID").map(str::to_owned);
        (to, subject, message_id)
    } else {
        warn_if_project_draft_thread_unverified(project.as_deref(), draft_id.as_deref());
        if to.is_empty() {
            return Err(anyhow!("pass --to or --thread"));
        }
        (
            to,
            subject.ok_or_else(|| anyhow!("pass --subject or --thread"))?,
            None,
        )
    };
    let raw = draft_raw(
        &to,
        &cc,
        &bcc,
        &subject,
        &body,
        in_reply_to.as_deref(),
        &attachments,
    )?;
    let draft = DraftMessage {
        id: draft_id,
        thread_id: thread,
        raw,
    };
    let result = gmail.save_draft(&draft).await?;
    Ok(SavedDraft {
        id: result.id,
        account: profile.email_address,
        attachments: attachments.len(),
    })
}

fn reply_subject(subject: &str) -> String {
    if subject.to_ascii_lowercase().starts_with("re:") {
        subject.to_owned()
    } else {
        format!("Re: {subject}")
    }
}

fn print_thread(thread: &GmailThread) {
    for message in &thread.messages {
        let from = header(message, "From").unwrap_or("unknown sender");
        let subject = header(message, "Subject").unwrap_or("(no subject)");
        println!("From: {from}\nSubject: {subject}\n");
        if let Some(part) = &message.payload {
            if let Some(body) = plain_text(part).filter(|body| !body.trim().is_empty()) {
                println!("{body}\n");
            } else {
                println!("[No supported plain-text or HTML body was extracted.]\n");
            }
            let mut names = Vec::new();
            attachments(part, &mut names);
            for name in names {
                println!("[Attachment not downloaded: {name}]");
            }
        } else {
            println!("[No supported message body was returned.]\n");
        }
    }
}

async fn file_thread(
    thread_id: &str,
    project_code: Option<&str>,
    host: Option<&str>,
) -> Result<()> {
    let local = crate::projects::manifest::read(Path::new("."));
    let project_code = project_code
        .or_else(|| {
            local
                .as_ref()
                .and_then(|manifest| manifest.project.as_deref())
        })
        .ok_or_else(|| anyhow!("pass --project or run from a Project checkout"))?;
    let host = host.or_else(|| local.as_ref().and_then(|manifest| manifest.host.as_deref()));
    let (base, navigator_token) = crate::remote::resolve(host)?;
    let project_id =
        crate::projects::surfaces::resolve_project_id(&base, &navigator_token, project_code)
            .await?;
    let gmail = client().await?;
    let profile = gmail.profile().await?;
    let expected_contact = local.as_ref().and_then(|manifest| {
        manifest_project_contact(
            Some(project_code),
            manifest.project.as_deref(),
            manifest.client_dri.as_deref(),
        )
    });
    let thread = read_project_thread(
        &gmail,
        thread_id,
        Some(project_code),
        expected_contact.as_deref(),
    )
    .await?;
    for message in &thread.messages {
        if let Some(part) = &message.payload {
            let mut names = Vec::new();
            attachments(part, &mut names);
            if !names.is_empty() {
                return Err(anyhow!(
                    "thread contains attachments ({}); no messages were filed because this CLI cannot archive attachments yet",
                    names.join(", ")
                ));
            }
        }
    }
    let bodies = thread
        .messages
        .iter()
        .map(|message| {
            message
                .payload
                .as_ref()
                .and_then(plain_text)
                .filter(|body| !body.trim().is_empty())
                .ok_or_else(|| {
                    anyhow!(
                        "message {} has no supported text body; no messages were filed",
                        message.id
                    )
                })
        })
        .collect::<Result<Vec<_>>>()?;
    let target = FileTarget {
        base: &base,
        navigator_token: &navigator_token,
        project_id,
        mailbox: &profile.email_address,
    };
    for (message, body) in thread.messages.iter().zip(bodies) {
        file_message(&target, message, &body).await?;
    }
    println!(
        "Filed {} message(s) from thread {} as Communications",
        thread.messages.len(),
        thread.id
    );
    Ok(())
}

struct FileTarget<'a> {
    base: &'a str,
    navigator_token: &'a str,
    project_id: Uuid,
    mailbox: &'a str,
}

async fn file_message(target: &FileTarget<'_>, message: &GmailMessage, body: &str) -> Result<()> {
    let from = header(message, "From").unwrap_or_default();
    let to = header(message, "To").unwrap_or_default();
    let direction = if email_from_address(from).as_deref()
        == Some(target.mailbox.to_ascii_lowercase().as_str())
    {
        "outbound"
    } else {
        "inbound"
    };
    let counterparty = if direction == "inbound" { from } else { to };
    let request = json!({
        "channel": if direction == "inbound" { "email_inbound" } else { "email_outbound" },
        "direction": direction,
        "counterparty": counterparty,
        "at": now_or_message_date(message)?,
        "body": body,
        "subject": header(message, "Subject"),
        "source_ref": source_reference(target.mailbox, &message.id),
    });
    let response = reqwest::Client::new()
        .post(format!(
            "{}/app/api/projects/{}/communications",
            target.base, target.project_id
        ))
        .bearer_auth(target.navigator_token)
        .json(&request)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .context("file email Communication")?;
    if !response.status().is_success() {
        return Err(anyhow!(
            "file email Communication refused: {}",
            response.status()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_message(headers: &[(&str, &str)]) -> GmailMessage {
        GmailMessage {
            id: "message-1".into(),
            internal_date: None,
            payload: Some(MessagePart {
                mime_type: None,
                headers: Some(
                    headers
                        .iter()
                        .map(|(name, value)| MessageHeader {
                            name: (*name).into(),
                            value: (*value).into(),
                        })
                        .collect(),
                ),
                body: None,
                parts: None,
                filename: None,
            }),
        }
    }

    #[test]
    fn gmail_endpoints_expose_reads_and_draft_writes_only() {
        assert_eq!(endpoint(Operation::SearchThreads, None).0, "GET");
        assert_eq!(endpoint(Operation::ReadThread, Some("thread-id")).0, "GET");
        assert_eq!(endpoint(Operation::ReadProfile, None).0, "GET");
        assert_eq!(endpoint(Operation::CreateDraft, None).0, "POST");
        assert_eq!(endpoint(Operation::UpdateDraft, Some("draft-id")).0, "PUT");
    }

    #[test]
    fn draft_text_ends_at_the_signoff_and_omits_a_trailing_signature() {
        assert_eq!(
            without_signature("Thanks!\n-- \nName\nBar admission", None),
            "Thanks!"
        );
        assert_eq!(without_signature("Thanks!\n--", None), "Thanks!");
        assert_eq!(without_signature("Thanks!", None), "Thanks!");
        assert_eq!(
            without_signature("Please review.\n\nBest regards,\nName", None),
            "Please review.\n\nBest regards,\nName"
        );
        assert_eq!(
            without_signature("Thanks!\n\nI also need the attached order checked.", None),
            "Thanks!\n\nI also need the attached order checked."
        );
    }

    #[test]
    fn draft_text_removes_only_an_exact_configured_signature_block() {
        assert_eq!(
            without_signature(
                "Please review.\n\nJane Doe\nAttorney at Law",
                Some("Jane Doe\nAttorney at Law")
            ),
            "Please review."
        );
    }

    #[test]
    fn agent_contract_requires_body_only_drafts_and_attorney_review() {
        let contract = include_str!("projects/agent_contract.md");
        assert!(contract.contains("navigator mail draft --body-file"));
        assert!(contract.contains("do not add a complimentary close"));
        assert!(contract.contains("The attorney reviews the complete draft in Gmail"));
    }

    #[test]
    fn draft_headers_cannot_inject_a_send_path() {
        assert!(draft_raw(
            &["attacker@example.test\r\nBcc: victim@example.test".into()],
            &[],
            &[],
            "Hello",
            "Thanks!",
            None,
            &[]
        )
        .is_err());
        assert!(draft_raw(
            &["client@example.test".into()],
            &[],
            &[],
            "Subject\nBcc: victim@example.test",
            "Thanks!",
            None,
            &[]
        )
        .is_err());
    }

    #[test]
    fn gmail_ids_are_encoded_as_single_path_segments() {
        let client = GmailClient::new("token".into()).unwrap();
        let url = client.url(Operation::ReadThread, Some("one/two")).unwrap();
        assert!(url.path().ends_with("/threads/one%2Ftwo"));
    }

    #[test]
    fn multipart_body_aggregates_plain_text_and_uses_html_fallback() {
        let encode =
            |body: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(body.as_bytes());
        let plain = MessagePart {
            mime_type: Some("text/plain".into()),
            headers: None,
            body: Some(MessageBody {
                data: Some(encode("Part one")),
                attachment_id: None,
            }),
            parts: None,
            filename: None,
        };
        let plain_two = MessagePart {
            mime_type: Some("text/plain".into()),
            headers: None,
            body: Some(MessageBody {
                data: Some(encode("Part two")),
                attachment_id: None,
            }),
            parts: None,
            filename: None,
        };
        let multipart = MessagePart {
            mime_type: Some("multipart/alternative".into()),
            headers: None,
            body: None,
            parts: Some(vec![plain, plain_two]),
            filename: None,
        };
        assert_eq!(
            plain_text(&multipart).as_deref(),
            Some("Part one\n\nPart two")
        );

        let html = MessagePart {
            mime_type: Some("text/html".into()),
            headers: None,
            body: Some(MessageBody {
                data: Some(encode("<p>Hello &amp; goodbye</p>")),
                attachment_id: None,
            }),
            parts: None,
            filename: None,
        };
        assert!(plain_text(&html).unwrap().contains("Hello & goodbye"));
    }

    #[tokio::test]
    async fn gmail_requests_are_limited_to_reads_and_unsent_draft_writes() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/users/me/threads"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"threads": []})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/users/me/threads/thread-1"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"id":"thread-1","messages":[]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/users/me/profile"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"emailAddress":"lawyer@example.test"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/users/me/drafts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"draft-1"})))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/users/me/drafts/draft-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"draft-1"})))
            .mount(&server)
            .await;

        let client =
            GmailClient::with_base_url("test-token".into(), Url::parse(&server.uri()).unwrap());
        client.search("from:client@example.test").await.unwrap();
        client.thread("thread-1").await.unwrap();
        client.profile().await.unwrap();
        client
            .save_draft(&DraftMessage {
                id: None,
                thread_id: None,
                raw: "To: client@example.test\r\nSubject: Hello\r\n\r\nBody".into(),
            })
            .await
            .unwrap();
        client
            .save_draft(&DraftMessage {
                id: Some("draft-1".into()),
                thread_id: None,
                raw: "To: client@example.test\r\nSubject: Hello\r\n\r\nBody".into(),
            })
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 5);
        assert!(requests.iter().all(|request| {
            !request.url.path().contains("send")
                && matches!(request.method.as_str(), "GET" | "POST" | "PUT")
                && request
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    == Some("Bearer test-token")
        }));
    }

    #[test]
    fn reply_subject_is_not_prefixed_twice() {
        assert_eq!(reply_subject("re: A question"), "re: A question");
        assert_eq!(reply_subject("A question"), "Re: A question");
    }

    #[test]
    fn source_reference_is_account_scoped_and_normalized() {
        assert_eq!(
            source_reference(" Lawyer@Example.Test ", "provider-id"),
            "gmail:lawyer@example.test:provider-id"
        );
    }

    #[test]
    fn project_match_requires_an_exact_contact_address_in_any_thread_message() {
        let unrelated = synthetic_message(&[
            ("From", "Other <other@example.test>"),
            ("To", "client@example.test.attacker.invalid"),
        ]);
        let matching = synthetic_message(&[
            ("From", "Other <other@example.test>"),
            (
                "Cc",
                "Client Team <CLIENT@example.test>, Another <next@example.test>",
            ),
        ]);
        let thread = GmailThread {
            id: "thread-1".into(),
            messages: vec![unrelated, matching],
        };

        assert_eq!(
            project_thread_fit(&thread, Some("sample-project"), Some("client@example.test")),
            ProjectThreadFit::Match
        );
        assert_eq!(
            project_thread_fit(
                &GmailThread {
                    id: "thread-2".into(),
                    messages: vec![synthetic_message(&[(
                        "To",
                        "client@example.test.attacker.invalid"
                    )])],
                },
                Some("sample-project"),
                Some("client@example.test")
            ),
            ProjectThreadFit::Mismatch
        );
    }

    #[test]
    fn no_project_or_usable_contact_does_not_claim_a_thread_mismatch() {
        let thread = GmailThread {
            id: "thread-1".into(),
            messages: vec![synthetic_message(&[("From", "other@example.test")])],
        };
        assert_eq!(
            project_thread_fit(&thread, None, Some("client@example.test")),
            ProjectThreadFit::OutsideProject
        );
        assert_eq!(
            project_thread_fit(&thread, Some("sample-project"), None),
            ProjectThreadFit::Unverifiable
        );
        assert_eq!(email_from_address("unusable contact"), None);
        assert_eq!(project_thread_warning(&thread, None, None), None);
        assert_eq!(
            project_thread_warning(&thread, Some("sample-project"), None),
            Some("Project client contact is unavailable; thread association cannot be verified")
        );
        assert_eq!(
            project_thread_warning(&thread, Some("sample-project"), Some("client@example.test")),
            Some("thread does not appear to belong to this Project's client contact")
        );
        assert_eq!(
            project_draft_thread_warning(Some("sample-project"), Some("draft-1")),
            Some("updated draft's thread cannot be checked against this Project's client contact")
        );
        assert_eq!(project_draft_thread_warning(None, Some("draft-1")), None);
    }

    #[test]
    fn explicit_project_contact_is_used_only_when_manifest_names_that_project() {
        assert_eq!(
            manifest_project_contact(
                Some("sample-project"),
                Some("sample-project"),
                Some("Client <client@example.test>"),
            ),
            Some("client@example.test".into())
        );
        assert_eq!(
            manifest_project_contact(
                Some("another-project"),
                Some("sample-project"),
                Some("client@example.test"),
            ),
            None
        );
    }

    #[tokio::test]
    async fn project_reply_update_reads_and_warns_before_saving_unsent_draft() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/users/me/threads/thread-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id":"thread-1",
                "messages":[{"id":"message-1","payload":{"headers":[
                    {"name":"From","value":"Other <other@example.test>"},
                    {"name":"Subject","value":"A question"}
                ]}}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/users/me/drafts/draft-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"draft-1"})))
            .mount(&server)
            .await;

        let gmail =
            GmailClient::with_base_url("test-token".into(), Url::parse(&server.uri()).unwrap());
        let thread = read_project_thread(
            &gmail,
            "thread-1",
            Some("sample-project"),
            Some("client@example.test"),
        )
        .await
        .unwrap();
        assert_eq!(
            project_thread_warning(&thread, Some("sample-project"), Some("client@example.test")),
            Some("thread does not appear to belong to this Project's client contact")
        );
        let raw = draft_raw(
            &["explicit@example.test".into()],
            &[],
            &[],
            "Re: A question",
            "Synthetic reply",
            Some("<message-1>"),
            &[],
        )
        .unwrap();
        gmail
            .save_draft(&DraftMessage {
                id: Some("draft-1".into()),
                thread_id: Some(thread.id),
                raw,
            })
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method.as_str(), "GET");
        assert_eq!(requests[1].method.as_str(), "PUT");
        assert!(!requests
            .iter()
            .any(|request| request.url.path().contains("send")));
        let body: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert_eq!(body["message"]["threadId"], "thread-1");
        let encoded = body["message"]["raw"].as_str().unwrap();
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .unwrap();
        let decoded = String::from_utf8(decoded).unwrap();
        assert!(decoded.starts_with("To: explicit@example.test\r\n"));
        assert!(decoded.contains("Synthetic reply"));
    }

    fn parsed(raw: &str) -> mail_parser::Message<'_> {
        mail_parser::MessageParser::default()
            .parse(raw.as_bytes())
            .expect("draft parses as MIME")
    }

    fn attachment_summary(message: &mail_parser::Message<'_>) -> Vec<(String, String, Vec<u8>)> {
        use mail_parser::MimeHeaders;
        message
            .attachments()
            .map(|part| {
                let content_type = part.content_type().expect("attachment has a type");
                (
                    part.attachment_name().unwrap_or_default().to_owned(),
                    format!(
                        "{}/{}",
                        content_type.ctype(),
                        content_type.subtype().unwrap_or_default()
                    ),
                    part.contents().to_vec(),
                )
            })
            .collect()
    }

    fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn decoded_draft(request: &wiremock::Request) -> (serde_json::Value, String) {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(body["message"]["raw"].as_str().unwrap())
            .unwrap();
        (body, String::from_utf8(raw).unwrap())
    }

    #[test]
    fn draft_mime_carries_each_attachment_with_its_bytes_name_and_type() {
        let dir = tempfile::tempdir().unwrap();
        let pdf: Vec<u8> = (0..=255u8).cycle().take(70_001).collect();
        let png = b"\x89PNG\r\n\x1a\nsynthetic".to_vec();
        let docx = b"PK\x03\x04synthetic".to_vec();
        let paths = vec![
            write_file(dir.path(), "letter.pdf", &pdf),
            write_file(dir.path(), "Exhibit \"A\".PNG", &png),
            write_file(dir.path(), "Résumé final.docx", &docx),
            write_file(dir.path(), "notes", b"plain"),
        ];
        let attachments = load_attachments(&paths, "Synthetic body").unwrap();
        let raw = draft_raw(
            &["client@example.test".into()],
            &[],
            &[],
            "Letter",
            "Synthetic body",
            Some("<message-1@example.test>"),
            &attachments,
        )
        .unwrap();

        let message = parsed(&raw);
        assert_eq!(message.body_text(0).unwrap().trim(), "Synthetic body");
        assert_eq!(
            message.in_reply_to().as_text(),
            Some("message-1@example.test")
        );
        assert_eq!(
            attachment_summary(&message),
            vec![
                ("letter.pdf".into(), "application/pdf".into(), pdf),
                ("Exhibit \"A\".PNG".into(), "image/png".into(), png),
                (
                    "Résumé final.docx".into(),
                    "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                        .into(),
                    docx
                ),
                (
                    "notes".into(),
                    "application/octet-stream".into(),
                    b"plain".to_vec()
                ),
            ]
        );
    }

    #[test]
    fn draft_carries_every_to_cc_and_bcc_recipient() {
        let raw = draft_raw(
            &["a@example.test".into(), "b@example.test".into()],
            &["c@example.test".into()],
            &["d@example.test".into()],
            "Hello",
            "Body",
            None,
            &[],
        )
        .unwrap();
        assert!(raw.starts_with(
            "To: a@example.test, b@example.test\r\nCc: c@example.test\r\nBcc: d@example.test\r\nSubject: Hello"
        ));
        assert!(draft_raw(
            &["a@example.test".into()],
            &["x\nBcc: y".into()],
            &[],
            "S",
            "B",
            None,
            &[]
        )
        .is_err());
    }

    #[test]
    fn setup_errors_name_every_missing_setting_at_once() {
        let error = setup_from(None, Some(" ".into())).unwrap_err().to_string();
        assert!(
            error.contains("NAVIGATOR_GMAIL_ACCOUNT")
                && error.contains("NAVIGATOR_GMAIL_CLIENT_ID")
        );
        assert_eq!(
            setup_from(Some("a@example.test".into()), Some("id".into())).unwrap(),
            ("a@example.test".to_owned(), "id".to_owned())
        );
    }

    #[test]
    fn a_draft_without_attachments_stays_single_part_plain_text() {
        let raw = draft_raw(
            &["client@example.test".into()],
            &[],
            &[],
            "Hello",
            "Body",
            None,
            &[],
        )
        .unwrap();
        assert!(raw.contains("Content-Type: text/plain; charset=UTF-8\r\n\r\nBody"));
        assert!(!raw.contains("multipart"));
    }

    #[test]
    fn attachment_file_names_cannot_inject_headers() {
        for name in [
            "evil\r\nBcc: victim@example.test\r\n.pdf",
            "evil\nX.pdf",
            "tab\tname.pdf",
        ] {
            let error = load_attachments(&[PathBuf::from(name)], "Body")
                .err()
                .expect("control characters are refused");
            assert!(error.to_string().contains("control character"), "{error:#}");
        }
    }

    #[test]
    fn a_missing_attachment_is_refused_naming_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent.pdf");
        let error = load_attachments(std::slice::from_ref(&missing), "Body")
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains(&missing.display().to_string()));
        let error = load_attachments(&[dir.path().to_path_buf()], "Body")
            .err()
            .unwrap();
        assert!(error.to_string().contains("is not a file"));
    }

    #[tokio::test]
    async fn oversized_attachments_are_refused_before_gmail_is_contacted() {
        let dir = tempfile::tempdir().unwrap();
        let body_file = write_file(dir.path(), "body.txt", b"Synthetic body");
        let sparse = |name: &str, len: u64| {
            let path = dir.path().join(name);
            std::fs::File::create(&path).unwrap().set_len(len).unwrap();
            path
        };
        let single = sparse("single.pdf", 19_000_000);
        let first = sparse("first.pdf", 10_000_000);
        let second = sparse("second.pdf", 10_000_000);
        assert!(base64_encoded_len(10_000_000) < MAX_MESSAGE_BYTES);

        for (attach, expected) in [
            (vec![single], "attachment single.pdf is"),
            (vec![first, second], "the draft with 2 attachments is"),
        ] {
            let connected = std::cell::Cell::new(false);
            let error = save_draft_command(
                DraftRequest {
                    to: vec!["client@example.test".into()],
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject: Some("Large".into()),
                    body_file: body_file.clone(),
                    thread: None,
                    draft_id: None,
                    attach,
                },
                || async {
                    connected.set(true);
                    Err(anyhow!("network reached"))
                },
            )
            .await
            .err()
            .unwrap();
            assert!(
                !connected.get(),
                "Gmail was contacted before the size check"
            );
            let message = error.to_string();
            assert!(message.contains(expected), "{message}");
            assert!(message.contains("nothing was sent to Gmail"), "{message}");
        }
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // one create, update, clear sequence against one mock
    async fn draft_command_creates_and_replaces_attachments_on_a_threaded_update() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/users/me/profile"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"emailAddress":"lawyer@example.test"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/users/me/threads/thread-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id":"thread-1",
                "messages":[{"id":"message-1","payload":{"headers":[
                    {"name":"From","value":"Client <client@example.test>"},
                    {"name":"Subject","value":"A question"},
                    {"name":"Message-ID","value":"<message-1@example.test>"}
                ]}}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/users/me/drafts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"draft-1"})))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/users/me/drafts/draft-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"draft-1"})))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let body_file = write_file(dir.path(), "body.txt", b"Synthetic body");
        let letter = write_file(dir.path(), "letter.pdf", b"%PDF-1.7 synthetic");
        let exhibit = write_file(dir.path(), "exhibit.png", b"\x89PNG synthetic");
        let revised = write_file(dir.path(), "revised.pdf", b"%PDF-1.7 revised");
        let connect = || async {
            Ok(GmailClient::with_base_url(
                "test-token".into(),
                Url::parse(&server.uri()).unwrap(),
            ))
        };

        let created = save_draft_command(
            DraftRequest {
                to: vec!["client@example.test".into()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: Some("Letter".into()),
                body_file: body_file.clone(),
                thread: None,
                draft_id: None,
                attach: vec![letter, exhibit],
            },
            connect,
        )
        .await
        .unwrap();
        assert_eq!(
            (
                created.id.as_str(),
                created.account.as_str(),
                created.attachments
            ),
            ("draft-1", "lawyer@example.test", 2)
        );

        let updated = save_draft_command(
            DraftRequest {
                to: Vec::new(),
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: None,
                body_file: body_file.clone(),
                thread: Some("thread-1".into()),
                draft_id: Some("draft-1".into()),
                attach: vec![revised],
            },
            connect,
        )
        .await
        .unwrap();
        assert_eq!(updated.attachments, 1);

        let cleared = save_draft_command(
            DraftRequest {
                to: Vec::new(),
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: None,
                body_file,
                thread: Some("thread-1".into()),
                draft_id: Some("draft-1".into()),
                attach: Vec::new(),
            },
            connect,
        )
        .await
        .unwrap();
        assert_eq!(cleared.attachments, 0);

        let requests = server.received_requests().await.unwrap();
        assert!(requests.iter().all(|request| {
            !request.url.path().contains("send")
                && matches!(request.method.as_str(), "GET" | "POST" | "PUT")
        }));
        let saves: Vec<_> = requests
            .iter()
            .filter(|request| request.method.as_str() != "GET")
            .collect();
        assert_eq!(
            saves
                .iter()
                .map(|request| (request.method.as_str(), request.url.path()))
                .collect::<Vec<_>>(),
            vec![
                ("POST", "/users/me/drafts"),
                ("PUT", "/users/me/drafts/draft-1"),
                ("PUT", "/users/me/drafts/draft-1"),
            ]
        );

        let (body, raw) = decoded_draft(saves[0]);
        assert!(body["message"]["threadId"].is_null());
        assert_eq!(
            attachment_summary(&parsed(&raw)),
            vec![
                (
                    "letter.pdf".into(),
                    "application/pdf".into(),
                    b"%PDF-1.7 synthetic".to_vec()
                ),
                (
                    "exhibit.png".into(),
                    "image/png".into(),
                    b"\x89PNG synthetic".to_vec()
                ),
            ]
        );

        let (body, raw) = decoded_draft(saves[1]);
        assert_eq!(body["message"]["threadId"], "thread-1");
        let message = parsed(&raw);
        assert_eq!(message.subject(), Some("Re: A question"));
        assert_eq!(
            message.in_reply_to().as_text(),
            Some("message-1@example.test")
        );
        assert_eq!(
            message.references().as_text(),
            Some("message-1@example.test")
        );
        assert_eq!(
            attachment_summary(&message),
            vec![(
                "revised.pdf".into(),
                "application/pdf".into(),
                b"%PDF-1.7 revised".to_vec()
            )]
        );

        let (body, raw) = decoded_draft(saves[2]);
        assert_eq!(body["message"]["threadId"], "thread-1");
        let message = parsed(&raw);
        assert_eq!(message.attachments().count(), 0);
        assert_eq!(
            message.in_reply_to().as_text(),
            Some("message-1@example.test")
        );
    }

    #[test]
    fn draft_help_documents_repeatable_attach_and_replacement() {
        use clap::Parser;

        #[derive(Parser)]
        struct Mail {
            #[command(subcommand)]
            action: MailAction,
        }

        let mut command = crate::concise_help(crate::navigator_command());
        let help = command
            .find_subcommand_mut("mail")
            .and_then(|mail| mail.find_subcommand_mut("draft"))
            .unwrap()
            .render_long_help()
            .to_string();
        let help = help.split_whitespace().collect::<Vec<_>>().join(" ");
        for expected in [
            "--attach <PATH> Attach a file read from disk.",
            "--attach is repeatable.",
            ".pdf is application/pdf",
            "25 MB message limit, checked before any network call",
            "the --attach list replaces the draft's previous attachments",
            "an update without --attach leaves the draft with none",
        ] {
            assert!(help.contains(expected), "missing `{expected}`: {help}");
        }

        let parsed = Mail::try_parse_from([
            "mail",
            "draft",
            "--body-file",
            "body.txt",
            "--attach",
            "a.pdf",
            "--attach",
            "b.png",
        ])
        .unwrap();
        let MailAction::Draft { attach, .. } = parsed.action else {
            panic!("draft subcommand");
        };
        assert_eq!(attach, vec![PathBuf::from("a.pdf"), PathBuf::from("b.png")]);
    }
}
