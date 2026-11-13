//! Gmail access for `hudson mail`.

use std::fmt::Write as _;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use clap::Subcommand;
use reqwest::{Method, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;
use uuid::Uuid;

pub(crate) mod attachments;
mod login;
mod packet;
mod signature;

const GMAIL_API: &str = "https://gmail.googleapis.com/gmail/v1";
const GOOGLE_TOKEN_API: &str = "https://oauth2.googleapis.com/token";
const MAX_SEARCH_RESULTS: u8 = 25;
/// Keychain service holding the Gmail refresh token; the account is the
/// configured Gmail address.
const REFRESH_TOKEN_SERVICE: &str = "com.neonlaw.navigator.gmail.refresh-token";
/// Keychain service holding the OAuth client secret; the account is the
/// configured client id.
const CLIENT_SECRET_SERVICE: &str = "com.neonlaw.navigator.gmail.client-secret";
/// The operating doc for the Firm's Gmail OAuth client and a lawyer machine's
/// sign-in. Errors about a missing client or sign-in name it.
const PROVISIONING_DOC: &str = "docs/mail-cli.md#provision-gmail-for-hudson-mail";
/// Gmail's 25 MB message limit, measured on the encoded MIME message.
const MAX_MESSAGE_BYTES: u64 = 25_000_000;
/// Where `mail save` writes by default: the Project checkout's git-ignored
/// working folder for email bodies, drafts, and saved copies of sent mail
/// (`cli/src/projects/repository.rs`'s `MAIL_DIRECTORY`).
const MAIL_DIRECTORY: &str = "mail";
/// The asset-lane kind a `mail parse` file-path upload takes when `--kind`
/// is not passed: mail correspondence is served correspondence.
const DEFAULT_PARSE_UPLOAD_KIND: &str = "letter";

const DRAFT_HELP: &str = "\
Signature:
  When the deployment's stores are reachable, the draft wears the attorney's
  stored signature row (resolved by the sending Gmail account), and stderr
  prints the provenance line naming its source and revision. Offline, the
  command falls back to NAVIGATOR_GMAIL_SIGNATURE — announced on stderr,
  never silent. With neither set, the draft signs with the sending Gmail
  account's default signature, and refuses only when that is blank too. A
  matching signature embedded before other text requires review before
  saving. The signature is the close: a sign-off line (\"Thanks!\", \"Best,\")
  directly above it is warned about on stderr.

Body:
  --body-file is plain text. --html-body-file takes HTML instead (bold
  topic headings and the like); the plain-text part is derived from it.
  With --thread, the replied-to message follows the signature as quoted
  history, as the Gmail client quotes it.

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
    /// Search Gmail threads, scoped to the current Project when possible:
    /// mail from or to the client contact, or naming one of the manifest's
    /// `dockets:` case numbers, so the court's notices on the matter appear.
    Search {
        query: String,
        /// Search the whole mailbox, without the Project scope — for
        /// counterparty or claims-agent mail that names neither.
        #[arg(long)]
        unscoped: bool,
    },
    /// Read a Gmail thread as plain text.
    Read { thread: String },
    /// Create or update an unsent draft. Use --thread to draft a reply.
    #[command(after_long_help = DRAFT_HELP)]
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
        /// Plain-text message body. The CLI adds the configured attorney signature.
        #[arg(
            long,
            required_unless_present = "html_body_file",
            conflicts_with = "html_body_file"
        )]
        body_file: Option<PathBuf>,
        /// HTML message body, for bold headings and the like; the plain-text
        /// part is derived from it. The CLI adds the configured attorney signature.
        #[arg(long)]
        html_body_file: Option<PathBuf>,
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
    /// Hand a signed packet to the Firm's mail-center vendor for printing and
    /// physical mail: an unsent Gmail draft to the vendor with the PDFs
    /// attached and the job sheet in the body, logged on the matter as an
    /// outbound Communication with the role `other`. Nothing is sent; the
    /// lawyer reviews and sends the draft. A preview, a `/s/ DRAFT`
    /// signature, or unsigned signing lines are refused.
    Packet {
        /// The packet's PDFs, in mailing order. Each must be the signed revision.
        #[arg(required = true)]
        pdfs: Vec<PathBuf>,
        /// The vendor's email address, from its Person or Entity row.
        #[arg(long)]
        vendor: String,
        /// One line of the recipient's address block. Repeat per line.
        #[arg(long = "recipient", required = true)]
        recipient: Vec<String>,
        /// How the vendor mails it.
        #[arg(long, value_enum, default_value = "certified")]
        service: packet::Service,
        /// Ask for a return receipt (green card).
        #[arg(long)]
        return_receipt: bool,
        /// The date the packet must be mailed by, `YYYY-MM-DD`.
        #[arg(long)]
        mail_by: chrono::NaiveDate,
        /// Matter code the Communication is logged on. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        /// Navigator host. Defaults to host in navigator.yaml.
        #[arg(long)]
        host: Option<String>,
        /// Print the job sheet; draft nothing and log nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Authorize Gmail in the browser and store the refresh token in the
    /// macOS Keychain. Requests gmail.readonly and gmail.compose only.
    Login {
        /// Read the client secret from Google's Desktop-app client JSON.
        ///
        /// Takes `installed.client_secret` from a `client_secret_*.json`,
        /// stores it in the Keychain, and never prints it. Otherwise saved GCP
        /// settings select Secret Manager, or login uses Keychain/hidden input.
        #[arg(long, value_name = "PATH", conflicts_with_all = ["client_secret_from_gcp", "project", "secret"])]
        client_secret_file: Option<PathBuf>,
        /// Fetch the client secret using the signed-in gcloud account.
        #[arg(long)]
        client_secret_from_gcp: bool,
        /// Secret Manager project. Overrides saved mail setup.
        #[arg(long)]
        project: Option<String>,
        /// Secret Manager secret name. Overrides saved mail setup.
        #[arg(long)]
        secret: Option<String>,
    },
    /// Save Gmail account/client settings or check draft-only setup.
    Setup {
        /// Gmail address whose refresh token is stored in the macOS Keychain.
        #[arg(long, requires = "client_id")]
        account: Option<String>,
        /// Google OAuth client id. Saved with --account; never a client secret.
        #[arg(long, requires = "account")]
        client_id: Option<String>,
        /// Save the Secret Manager project used by mail login.
        #[arg(long)]
        project: Option<String>,
        /// Save the secret name; defaults to navigator-mail-oauth-client-secret.
        #[arg(long, requires = "project")]
        secret: Option<String>,
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
        /// Asset-lane kind the messages' attachments file under, as internal
        /// documents. Defaults to `letter`.
        #[arg(long, value_parser = crate::parse_asset_kind)]
        kind: Option<String>,
    },
    /// File one message as a matter Communication: a Gmail web-URL message, a
    /// local .eml, or a PDF/file path through the document-upload lane.
    Parse {
        /// Gmail web URL (copied from the browser), a local .eml path, or a
        /// local PDF/file path to upload as a document.
        reference: String,
        /// Matter code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        /// Navigator host. Defaults to host in navigator.yaml.
        #[arg(long)]
        host: Option<String>,
        /// Asset-lane kind for a file-path upload, or for the attachments of
        /// a Gmail message or local .eml, filed internal. Defaults to `letter`.
        #[arg(long, value_parser = crate::parse_asset_kind)]
        kind: Option<String>,
    },
    /// Save a Gmail message's .eml copy under the git-ignored mail/ folder.
    Save {
        /// Gmail web URL or Gmail message id.
        reference: String,
        /// Directory to save into. Defaults to mail/.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Output filename. Defaults to the convention: YYYY.MM.DD_<subject>.eml.
        #[arg(long)]
        name: Option<String>,
    },
    /// Write each attachment of a Gmail message into a directory.
    ///
    /// Never clobbers; prints name, byte count, and sha256 per file. Nothing
    /// is uploaded.
    Attachments {
        /// Gmail web URL or Gmail message id.
        reference: String,
        /// Directory to write the attachments into.
        #[arg(long)]
        dir: PathBuf,
        /// Only write filenames matching this case-insensitive glob, such as '*.pdf'.
        #[arg(long, value_name = "GLOB")]
        only: Option<String>,
        /// Also write inline images, which are skipped by default.
        #[arg(long)]
        include_inline: bool,
    },
    /// File a saved RFC 5322 message (.eml) into the matter inbox through
    /// the Navigator ingest door. Ingest-only: nothing is sent, and no
    /// Gmail OAuth setup is required. Re-running is safe — the door
    /// dedupes on the provider Message-ID (falling back to a raw-content
    /// hash). A reply joins the Conversation of the message it answers in
    /// the same Project (In-Reply-To, References, or Gmail's X-GM-THRID).
    Ingest {
        /// Path to the raw RFC 5322 message to file.
        #[arg(value_name = "PATH")]
        path: PathBuf,
        /// Matter code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        /// Navigator host. Defaults to host in navigator.yaml.
        #[arg(long)]
        host: Option<String>,
        /// Provider account the message was read from (for example the
        /// Gmail address). Ingress dedup keys on it. Defaults to
        /// cli-ingest.
        #[arg(long)]
        provider_account: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    SearchThreads,
    ReadThread,
    ReadMessage,
    ReadProfile,
    /// `GET users/me/settings/sendAs/{address}` — the sending account's
    /// default signature, the draft's last signature source (LAW-316).
    ReadSendAs,
    CreateDraft,
    UpdateDraft,
    /// `POST users/me/messages/{id}/modify` — the label write behind
    /// `inbox process --apply`'s archive. Never trash, never send.
    ModifyMessage,
}

fn endpoint(
    operation: Operation,
    id: Option<&str>,
) -> (&'static str, Vec<&'static str>, Option<String>) {
    match operation {
        Operation::SearchThreads => ("GET", vec!["users", "me", "threads"], None),
        Operation::ReadThread => ("GET", vec!["users", "me", "threads"], id.map(str::to_owned)),
        Operation::ReadMessage => (
            "GET",
            vec!["users", "me", "messages"],
            id.map(str::to_owned),
        ),
        Operation::ReadProfile => ("GET", vec!["users", "me", "profile"], None),
        Operation::ReadSendAs => (
            "GET",
            vec!["users", "me", "settings", "sendAs"],
            id.map(str::to_owned),
        ),
        Operation::CreateDraft => ("POST", vec!["users", "me", "drafts"], None),
        Operation::UpdateDraft => ("PUT", vec!["users", "me", "drafts"], id.map(str::to_owned)),
        Operation::ModifyMessage => (
            "POST",
            vec!["users", "me", "messages"],
            id.map(str::to_owned),
        ),
    }
}

/// A path segment that rides after the message id — `modify` — for the
/// label-write endpoint whose id is never the last segment.
fn endpoint_suffix(operation: Operation) -> Option<&'static str> {
    match operation {
        Operation::ModifyMessage => Some("modify"),
        Operation::SearchThreads
        | Operation::ReadThread
        | Operation::ReadMessage
        | Operation::ReadProfile
        | Operation::ReadSendAs
        | Operation::CreateDraft
        | Operation::UpdateDraft => None,
    }
}

struct GmailClient {
    http: reqwest::Client,
    access_token: String,
    base_url: Url,
}

impl GmailClient {
    fn new(access_token: String) -> Result<Self> {
        Ok(Self::with_base_url(
            access_token,
            Url::parse(GMAIL_API).context("parse Gmail API base URL")?,
        ))
    }

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
            if let Some(suffix) = endpoint_suffix(operation) {
                path.push(suffix);
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

    /// One message in `format=full`, the shape `mail file` and `mail parse`
    /// read headers, bodies, and attachments from.
    async fn message(&self, id: &str) -> Result<GmailMessage> {
        self.call(
            Operation::ReadMessage,
            Some(id),
            &[("format", "full")],
            None,
        )
        .await
    }

    /// One message in `format=raw`: the complete RFC 822 copy `mail save`
    /// writes to disk.
    async fn raw_message(&self, id: &str) -> Result<RawGmailMessage> {
        self.call(Operation::ReadMessage, Some(id), &[("format", "raw")], None)
            .await
    }

    async fn profile(&self) -> Result<GmailProfile> {
        self.call(Operation::ReadProfile, None, &[], None).await
    }

    /// The account's default signature as plain text — blank when Gmail
    /// holds none.
    async fn send_as_signature(&self, address: &str) -> Result<String> {
        let send_as: SendAs = self
            .call(Operation::ReadSendAs, Some(address), &[], None)
            .await
            .context("read the Gmail account's default signature")?;
        Ok(mail_parser::decoders::html::html_to_text(
            send_as.signature.as_deref().unwrap_or_default(),
        ))
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

    /// Remove the INBOX label from one message — the archive. Never a
    /// trash delete: the copy stays readable and searchable.
    async fn archive_message(&self, id: &str) -> Result<()> {
        let _: ArchivedMessage = self
            .call(
                Operation::ModifyMessage,
                Some(id),
                &[],
                Some(json!({ "removeLabelIds": [INBOX_LABEL] })),
            )
            .await?;
        Ok(())
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
struct RawGmailMessage {
    /// URL-safe base64 of the complete RFC 822 message.
    raw: Option<String>,
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
}

#[derive(Debug, Deserialize)]
struct GmailProfile {
    #[serde(rename = "emailAddress")]
    email_address: String,
}

#[derive(Debug, Deserialize)]
struct SendAs {
    signature: Option<String>,
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

/// URL-safe base64 that accepts Gmail's payloads with or without `=`
/// padding: the API omits it on some messages and keeps it on others.
const GMAIL_BASE64URL: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
    &base64::alphabet::URL_SAFE,
    base64::engine::GeneralPurposeConfig::new()
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
);

fn decoded_body(part: &MessagePart) -> Option<String> {
    let data = part.body.as_ref()?.data.as_deref()?;
    let bytes = GMAIL_BASE64URL.decode(data.trim()).ok()?;
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

/// The names of the parts a lawyer would call attachments: a part with a
/// non-empty filename, or one marked `Content-Disposition: attachment`.
/// Gmail reports `filename: ""` on every body part and multipart container,
/// so presence of the field alone means nothing.
fn attachments(part: &MessagePart, output: &mut Vec<String>) {
    let filename = part
        .filename
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let disposition_attachment = part.headers.iter().flatten().any(|header| {
        header.name.eq_ignore_ascii_case("Content-Disposition")
            && header
                .value
                .trim_start()
                .get(..10)
                .is_some_and(|kind| kind.eq_ignore_ascii_case("attachment"))
    });
    if filename.is_some() || disposition_attachment {
        output.push(filename.map_or_else(|| "(unnamed attachment)".into(), str::to_string));
    }
    if let Some(parts) = &part.parts {
        for child in parts {
            attachments(child, output);
        }
    }
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

/// Base64 in 76-character CRLF-terminated lines, the MIME body encoding.
fn base64_lines(bytes: &[u8]) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mut lines = String::with_capacity(encoded.len() + encoded.len() / 38 + 2);
    for line in encoded.as_bytes().chunks(76) {
        lines.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
        lines.push_str("\r\n");
    }
    lines
}

/// The draft's body entity: plain text alone, or, with an HTML body,
/// `multipart/alternative` carrying the plain text and the base64 HTML part.
fn body_entity(body: &str, html: Option<&str>) -> Result<String> {
    let text = body.replace("\r\n", "\n").replace('\n', "\r\n");
    let Some(html) = html else {
        return Ok(format!(
            "Content-Type: text/plain; charset=UTF-8\r\n\r\n{text}"
        ));
    };
    let boundary = format!("navigator-alternative-{}", Uuid::new_v4().simple());
    if text.contains(&boundary) {
        return Err(anyhow!("draft body collides with the MIME boundary"));
    }
    Ok(format!(
        "Content-Type: multipart/alternative; boundary=\"{boundary}\"\r\n\r\n\
         --{boundary}\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\n{text}\r\n\
         --{boundary}\r\nContent-Type: text/html; charset=UTF-8\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n{}--{boundary}--\r\n",
        base64_lines(html.as_bytes())
    ))
}

#[allow(clippy::too_many_arguments)] // each is a distinct header or MIME part
fn draft_raw(
    to: &[String],
    cc: &[String],
    bcc: &[String],
    subject: &str,
    body: &str,
    html: Option<&str>,
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
    let entity = body_entity(body, html)?;
    if attachments.is_empty() {
        raw.push_str(&entity);
        return Ok(raw);
    }
    let boundary = format!("navigator-mixed-{}", Uuid::new_v4().simple());
    if entity.contains(&boundary) {
        return Err(anyhow!("draft body collides with the MIME boundary"));
    }
    let _ = write!(
        raw,
        "Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n--{boundary}\r\n{entity}\r\n"
    );
    for attachment in attachments {
        let (name, filename) = filename_parameters(&attachment.filename);
        let _ = write!(
            raw,
            "--{boundary}\r\nContent-Type: {}; {name}\r\nContent-Disposition: attachment; {filename}\r\nContent-Transfer-Encoding: base64\r\n\r\n",
            attachment.mime_type
        );
        raw.push_str(&base64_lines(&attachment.bytes));
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

/// The Gmail message id a reference names: a Gmail web URL copied from the
/// browser (`https://mail.google.com/mail/u/0/#inbox/<id>`, any
/// `#<label>/…/<id>` or `#search/<query>/<id>` form, and the legacy
/// `?th=<id>` form included), a bare Gmail message id, or the browser's
/// `FMfcgz…` web id, decoded to its hex id. None when the reference is
/// none of these, so a misspelled file path falls through to the caller's
/// file handling.
///
/// The browser's id names the conversation: for a one-message thread it is
/// the message id, and for a longer one Gmail resolves it to the thread's
/// first message — the same reading the legacy hex URLs always had.
fn gmail_message_id(reference: &str) -> Option<String> {
    let candidate = reference_id_segment(reference)?;
    if let Some(GmailWebId::Decoded(id)) = gmail_web_id(&candidate) {
        return Some(id);
    }
    Some(candidate).filter(|id| is_gmail_id(id))
}

/// The id-shaped segment of a reference: the `th` query value or last
/// fragment segment of a Gmail web URL, or the reference itself. None for
/// a URL on another host or with no id segment.
fn reference_id_segment(reference: &str) -> Option<String> {
    let Ok(url) = Url::parse(reference) else {
        return Some(reference.to_string());
    };
    if !url.host_str()?.ends_with("mail.google.com") {
        return None;
    }
    if let Some((_, id)) = url.query_pairs().find(|(name, _)| name == "th") {
        return Some(id.into_owned());
    }
    let fragment = url.fragment()?;
    // `#inbox/<id>?projector=1` opens an attachment preview over the message.
    let fragment = fragment.split_once('?').map_or(fragment, |(path, _)| path);
    fragment
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .map(str::to_string)
}

/// What a browser web id (`FMfcgz…`) decodes to.
#[derive(Debug, PartialEq, Eq)]
enum GmailWebId {
    /// A thread or message id in the API's hex form.
    Decoded(String),
    /// A well-formed web id that carries no API id (`thread-a:r…`, the form
    /// Gmail uses for drafts and some sent mail).
    Opaque,
}

/// Gmail's web ids re-encode `base64("<kind>:<decimal id>")` in a
/// forty-consonant alphabet. Numeric kinds (`f:`, `thread-f:`, `msg-f:`)
/// decode offline to the API's hex id. None when `token` is not a web id
/// at all.
fn gmail_web_id(token: &str) -> Option<GmailWebId> {
    let (kind, value) = decode_web_id(token)?;
    match value.parse::<u64>() {
        Ok(number) if kind.ends_with('f') => Some(GmailWebId::Decoded(format!("{number:x}"))),
        _ => Some(GmailWebId::Opaque),
    }
}

/// A browser web id's `(kind, value)` pair, both still text.
fn decode_web_id(token: &str) -> Option<(String, String)> {
    const WEB: &[u8] = b"BCDFGHJKLMNPQRSTVWXZbcdfghjklmnpqrstvwxz";
    const BASE64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    const STANDARD_ANY_PAD: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    );
    if !(16..=128).contains(&token.len()) {
        return None;
    }
    // Base conversion, 40 to 64, over little-endian digit vectors.
    let mut digits: Vec<u32> = Vec::new();
    for byte in token.bytes() {
        let mut carry = u32::try_from(WEB.iter().position(|&c| c == byte)?).ok()?;
        for digit in &mut digits {
            let value = *digit * 40 + carry;
            *digit = value % 64;
            carry = value / 64;
        }
        while carry > 0 {
            digits.push(carry % 64);
            carry /= 64;
        }
    }
    let encoded: String = digits
        .iter()
        .rev()
        .map(|&digit| char::from(BASE64[digit as usize]))
        .collect();
    let decoded = String::from_utf8(STANDARD_ANY_PAD.decode(encoded).ok()?).ok()?;
    let (kind, value) = decoded.split_once(':')?;
    if kind.is_empty() || !kind.bytes().all(|b| b.is_ascii_lowercase() || b == b'-') {
        return None;
    }
    Some((kind.to_string(), value.to_string()))
}

/// The error for a reference that names no message: a web id that cannot
/// be decoded offline gets its own explanation, anything else `fallback`.
fn unresolved_reference_or(reference: &str, fallback: &str) -> anyhow::Error {
    if reference_id_segment(reference)
        .and_then(|segment| gmail_web_id(&segment))
        .is_some_and(|id| id == GmailWebId::Opaque)
    {
        return anyhow!(
            "the reference carries a Gmail web id that names no API message id (Gmail uses \
             this form for drafts and some sent mail): {reference}. Find its Gmail message id \
             with `hudson mail search` and pass that, or copy the URL from a conversation \
             view"
        );
    }
    anyhow!("{fallback}: {reference}")
}

/// [`unresolved_reference_or`] with the Gmail-only fallback message.
fn unresolved_reference(reference: &str) -> anyhow::Error {
    unresolved_reference_or(
        reference,
        "the reference is neither a Gmail web URL nor a Gmail message id",
    )
}

/// Gmail message ids are base64url strings (commonly 16 characters) with
/// digits in them; the digit requirement keeps a search term or misspelled
/// path from looking like an id.
fn is_gmail_id(id: &str) -> bool {
    (8..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && id.bytes().any(|b| b.is_ascii_digit())
}

/// The Communication request body for one filed message. Pure, so the Gmail
/// and .eml paths cannot drift apart.
fn communication_request(
    mailbox: &str,
    from: &str,
    to: &str,
    subject: Option<&str>,
    at: &str,
    body: &str,
    message_id: &str,
) -> serde_json::Value {
    let direction =
        if email_from_address(from).as_deref() == Some(mailbox.to_ascii_lowercase().as_str()) {
            "outbound"
        } else {
            "inbound"
        };
    let counterparty = if direction == "inbound" { from } else { to };
    json!({
        "channel": if direction == "inbound" { "email_inbound" } else { "email_outbound" },
        "direction": direction,
        "counterparty": counterparty,
        "at": at,
        "body": body,
        "subject": subject,
        "source_ref": source_reference(mailbox, message_id),
    })
}

/// Gmail returns `format=raw` messages URL-safe base64 encoded.
/// Whether the `=` padding is present varies by caller and message, so
/// decoding accepts both forms.
///
/// # Errors
///
/// [`anyhow::Error`] when the response carries no raw field or it does not
/// decode.
fn decode_gmail_raw(raw: &str) -> Result<Vec<u8>> {
    GMAIL_BASE64URL
        .decode(raw.trim())
        .context("decode Gmail raw message")
}

/// The conventional saved-mail filename: the send date, the subject as a
/// slug, and the .eml extension — `2026.10.30_Retainer questions.eml` shape.
fn conventional_mail_name(sent_on: &str, subject: &str) -> String {
    let subject = subject.trim();
    let slug = subject_slug(subject).unwrap_or_else(|| "no-subject".to_string());
    format!("{sent_on}_{slug}.eml")
}

/// The subject as a path-safe slug: alphanumeric characters, `-`, `.`, and
/// `_` survive; whitespace becomes `_`; everything else drops. None when
/// nothing survives.
fn subject_slug(subject: &str) -> Option<String> {
    let mut slug = String::new();
    let mut trailing_separator = true;
    for character in subject.chars() {
        if slug.len() >= 64 {
            break;
        }
        if character.is_alphanumeric() || character == '-' || character == '.' {
            slug.push(character);
            trailing_separator = false;
        } else if (character.is_whitespace() || character == '_') && !trailing_separator {
            slug.push('_');
            trailing_separator = true;
        }
    }
    let slug = slug.trim_matches(['_', '.', '-']).to_string();
    (!slug.is_empty()).then_some(slug)
}

/// A saved-mail filename must start with its send date and keep the .eml
/// extension: `YYYY.MM.DD_<name>.eml`.
///
/// # Errors
///
/// [`anyhow::Error`] naming the convention when the filename does not follow
/// it.
fn check_mail_save_name(name: &str) -> Result<()> {
    let bytes = name.as_bytes();
    let dated = bytes.len() >= 15
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'.'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'.'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
        && bytes[10] == b'_';
    let eml = std::path::Path::new(name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("eml"));
    if dated && eml {
        return Ok(());
    }
    Err(anyhow!(
        "the filename must follow the convention YYYY.MM.DD_<name>.eml, the send date first"
    ))
}

/// What `write_saved_mail` did.
#[derive(Debug, Eq, PartialEq)]
enum SavedMail {
    Written,
    AlreadySaved,
}

/// Write the .eml copy without clobbering: an identical file is already
/// saved (a re-run is idempotent); a different one is refused.
///
/// # Errors
///
/// [`anyhow::Error`] when an existing file's contents differ or the write
/// fails.
fn write_saved_mail(path: &Path, bytes: &[u8]) -> Result<SavedMail> {
    if path.exists() {
        let existing = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        if existing == bytes {
            return Ok(SavedMail::AlreadySaved);
        }
        return Err(anyhow!(
            "{} already exists with different contents; remove it or pass --name",
            path.display()
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))?;
    Ok(SavedMail::Written)
}

/// What a Project-scoped `mail search` admits: the client contact's mail and
/// any message naming one of the Project's case numbers — a court notice
/// comes from the court, never the client, so the contact alone hid them.
#[derive(Debug, Default)]
struct SearchScope {
    contact: Option<String>,
    dockets: Vec<String>,
}

impl SearchScope {
    /// The scope the Project checkout in the working directory declares.
    fn from_manifest() -> Self {
        let Some(manifest) = crate::projects::manifest::read(Path::new(".")) else {
            return Self::default();
        };
        if manifest.project.is_none() {
            return Self::default();
        }
        Self {
            contact: client_contact_from_manifest(),
            dockets: manifest.dockets,
        }
    }
}

fn scoped_query(query: &str, scope: &SearchScope) -> String {
    let mut terms = Vec::new();
    if let Some(email) = &scope.contact {
        terms.push(format!("from:{email} OR to:{email}"));
    }
    for docket in &scope.dockets {
        let docket = docket.replace('"', "");
        let docket = docket.trim();
        if !docket.is_empty() {
            terms.push(format!("\"{docket}\""));
        }
    }
    if terms.is_empty() {
        return query.to_owned();
    }
    format!("({query}) ({})", terms.join(" OR "))
}

#[derive(Default, Deserialize, Serialize)]
struct StoredSetup {
    account: Option<String>,
    client_id: Option<String>,
    gcp_project: Option<String>,
    gcp_secret: Option<String>,
}

fn setup_path() -> PathBuf {
    crate::credentials::default_credentials_path().with_extension("mail.json")
}

fn load_setup(path: &Path) -> Result<StoredSetup> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("parse saved Gmail setup; run hudson mail setup --account <gmail-address> --client-id <oauth-client-id> to replace it"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(StoredSetup::default()),
        Err(error) => Err(error).context("read saved Gmail setup"),
    }
}

fn store_setup(path: &Path, account: &str, client_id: &str) -> Result<()> {
    let (account, client_id) = setup_from(
        Some(account.trim().to_owned()),
        Some(client_id.trim().to_owned()),
    )?;
    // An explicit account/client pair can repair a malformed setup file.
    let mut settings = load_setup(path).unwrap_or_default();
    settings.account = Some(account);
    settings.client_id = Some(client_id);
    write_setup(path, &settings)
}

fn write_setup(path: &Path, settings: &StoredSetup) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).context("create Gmail setup directory")?;
    // NamedTempFile is owner-only on Unix. Rename replaces a permissive destination atomically.
    let mut file =
        tempfile::NamedTempFile::new_in(parent).context("create private Gmail setup file")?;
    serde_json::to_writer_pretty(file.as_file_mut(), settings).context("write Gmail setup")?;
    file.persist(path)
        .map_err(|error| error.error)
        .context("save Gmail setup")?;
    Ok(())
}

fn effective_setup(
    account: Option<String>,
    client_id: Option<String>,
    stored: StoredSetup,
) -> StoredSetup {
    StoredSetup {
        account: account
            .filter(|value| !value.trim().is_empty())
            .or(stored.account),
        client_id: client_id
            .filter(|value| !value.trim().is_empty())
            .or(stored.client_id),
        gcp_project: stored.gcp_project,
        gcp_secret: stored.gcp_secret,
    }
}

fn resolve_setup(
    account: Option<String>,
    client_id: Option<String>,
    stored: StoredSetup,
) -> Result<(String, String)> {
    let settings = effective_setup(account, client_id, stored);
    setup_from(settings.account, settings.client_id)
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
            "Gmail is not set up. Run hudson mail setup --account <gmail-address> --client-id <oauth-client-id> to save settings, or set environment overrides. The Firm's OAuth client and this machine's sign-in are in {PROVISIONING_DOC}: {}",
            steps.join("; ")
        )),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KeychainState {
    AccountUnknown,
    Present,
    Missing,
}

struct SetupReport {
    text: String,
    ready: bool,
}

/// Checklist for the draft-only Gmail path. Built from booleans only, so it never carries a secret
/// or a value read from the environment. The client secret is optional: a token minted by
/// `hudson mail login` with a Desktop client needs it to refresh, and the refresh exchange
/// sends it whenever one is stored.
fn setup_report(
    account_set: bool,
    client_id_set: bool,
    client_secret_stored: bool,
    keychain: KeychainState,
) -> SetupReport {
    let mark = |ok: bool| if ok { "ok      " } else { "MISSING " };
    let keychain_ok = keychain == KeychainState::Present;
    let mut text = String::from("Gmail draft-only setup for hudson mail\n");
    let _ = writeln!(
        text,
        "  [{}] Gmail account (saved or NAVIGATOR_GMAIL_ACCOUNT)",
        mark(account_set)
    );
    let _ = writeln!(
        text,
        "  [{}] OAuth client id (saved or NAVIGATOR_GMAIL_CLIENT_ID)",
        mark(client_id_set)
    );
    let _ = writeln!(
        text,
        "  [{}] OAuth client secret in the macOS Keychain, service {CLIENT_SECRET_SERVICE} (hudson mail login stores it)",
        if client_secret_stored { "ok      " } else { "optional" }
    );
    let _ = writeln!(
        text,
        "  [{}] refresh token in the macOS Keychain, service {REFRESH_TOKEN_SERVICE}",
        mark(keychain_ok)
    );
    if !account_set || !client_id_set {
        text.push_str(
            "\nnavigator mail setup --account <gmail-address> --client-id <oauth-client-id>\n",
        );
    }
    if !client_id_set {
        let _ = writeln!(
            text,
            "No OAuth client id yet? The Firm's operator creates one Desktop-app client per Firm; see {PROVISIONING_DOC}."
        );
    }
    if keychain == KeychainState::Missing {
        let _ = writeln!(
            text,
            "\nAuthorize Gmail in the browser and store the refresh token (uses the selected client secret source):\n  hudson mail login\nSign-in steps for a new machine: {PROVISIONING_DOC}."
        );
    }
    text.push_str(
        "\nScopes: hudson mail login requests gmail.readonly and gmail.compose only, and refuses a token \
carrying any other scope. Gmail has no narrower scope that creates drafts. hudson mail never sends, replies, or \
forwards: it has no send verb and calls no send endpoint. Never print, paste, or log the token or client secret. See \
docs/mail-cli.md.\n",
    );
    let ready = account_set && client_id_set && keychain_ok;
    if ready {
        text.push_str("\nReady: hudson mail draft can create unsent drafts.\n");
    }
    SetupReport { text, ready }
}

fn setup_command(
    account: Option<String>,
    client_id: Option<String>,
    project: Option<&str>,
    secret: Option<&str>,
) -> Result<()> {
    let gcp = login::gcp_source(project, secret, false, &StoredSetup::default())?;
    if let (Some(account), Some(client_id)) = (account, client_id) {
        store_setup(&setup_path(), &account, &client_id)?;
    }
    if let Some(gcp) = gcp {
        let mut settings = load_setup(&setup_path())?;
        settings.gcp_project = Some(gcp.project);
        settings.gcp_secret = Some(gcp.secret);
        write_setup(&setup_path(), &settings)?;
    }
    let settings = effective_setup(
        std::env::var("NAVIGATOR_GMAIL_ACCOUNT").ok(),
        std::env::var("NAVIGATOR_GMAIL_CLIENT_ID").ok(),
        load_setup(&setup_path())?,
    );
    let account = settings.account.filter(|value| !value.trim().is_empty());
    let client_id = settings.client_id.filter(|value| !value.trim().is_empty());
    let client_secret_stored = client_id.as_deref().is_some_and(|client_id| {
        keychain_read(CLIENT_SECRET_SERVICE, client_id).is_ok_and(|secret| secret.is_some())
    });
    let keychain = match &account {
        None => KeychainState::AccountUnknown,
        Some(account) if keychain_refresh_token(account).is_ok() => KeychainState::Present,
        Some(_) => KeychainState::Missing,
    };
    let report = setup_report(
        account.is_some(),
        client_id.is_some(),
        client_secret_stored,
        keychain,
    );
    print!("{}", report.text);
    if report.ready {
        Ok(())
    } else {
        Err(anyhow!("Gmail draft-only setup is incomplete"))
    }
}

/// The configured Gmail account and OAuth client id: environment overrides
/// first, then the saved settings.
fn configured_setup() -> Result<(String, String)> {
    resolve_setup(
        std::env::var("NAVIGATOR_GMAIL_ACCOUNT").ok(),
        std::env::var("NAVIGATOR_GMAIL_CLIENT_ID").ok(),
        load_setup(&setup_path())?,
    )
}

/// The refresh-token form. `client_secret` rides along only when one is
/// stored: a Desktop-app client requires it, and a client that issues
/// tokens without one keeps working.
fn refresh_form<'a>(
    client_id: &'a str,
    client_secret: Option<&'a str>,
    refresh_token: &'a str,
) -> Vec<(&'static str, &'a str)> {
    let mut form = vec![("client_id", client_id)];
    if let Some(secret) = client_secret {
        form.push(("client_secret", secret));
    }
    form.push(("refresh_token", refresh_token));
    form.push(("grant_type", "refresh_token"));
    form
}

async fn client() -> Result<GmailClient> {
    let (account, client_id) = configured_setup()?;
    let refresh_token = keychain_refresh_token(&account).with_context(|| {
        format!("Gmail refresh token unavailable; run hudson mail login (see {PROVISIONING_DOC})")
    })?;
    let client_secret = keychain_read(CLIENT_SECRET_SERVICE, &client_id)?;
    let response = reqwest::Client::new()
        .post(GOOGLE_TOKEN_API)
        .form(&refresh_form(
            &client_id,
            client_secret.as_deref(),
            &refresh_token,
        ))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .context("refresh Gmail access token")?;
    let token = login::token_response(response).await?;
    if let Err(error) = login::check_granted_scopes(token.scope.as_deref()) {
        return Err(login::refuse_stored_token(&account, error));
    }
    GmailClient::new(token.access_token)
}

fn keychain_refresh_token(account: &str) -> Result<String> {
    let token = keychain_read(REFRESH_TOKEN_SERVICE, account)
        .context("read Gmail refresh token from macOS Keychain")?
        .ok_or_else(|| anyhow!("no Gmail refresh token in the Keychain"))?;
    if token.trim().is_empty() {
        return Err(anyhow!("Gmail refresh token is empty in Keychain"));
    }
    Ok(token)
}

/// Reads one generic-password item. `None` when the item does not exist;
/// any other Keychain failure is an error, never a silent absence.
#[cfg(target_os = "macos")]
fn keychain_read(service: &str, account: &str) -> Result<Option<String>> {
    /// `errSecItemNotFound`.
    const ITEM_NOT_FOUND: i32 = -25300;
    match security_framework::passwords::generic_password(
        security_framework::passwords::PasswordOptions::new_generic_password(service, account),
    ) {
        Ok(bytes) => {
            let value = String::from_utf8(bytes).context("Keychain item is not UTF-8")?;
            Ok(Some(value).filter(|value| !value.trim().is_empty()))
        }
        Err(error) if error.code() == ITEM_NOT_FOUND => Ok(None),
        Err(error) => Err(anyhow!("read the macOS Keychain item {service}: {error}")),
    }
}

/// Creates or updates one generic-password item.
#[cfg(target_os = "macos")]
fn keychain_write(service: &str, account: &str, value: &str) -> Result<()> {
    security_framework::passwords::set_generic_password(service, account, value.as_bytes())
        .map_err(|error| anyhow!("write the macOS Keychain item {service}: {error}"))
}

/// Deletes one generic-password item.
#[cfg(target_os = "macos")]
fn keychain_delete(service: &str, account: &str) -> Result<()> {
    security_framework::passwords::delete_generic_password(service, account)
        .map_err(|error| anyhow!("delete the macOS Keychain item {service}: {error}"))
}

#[cfg(not(target_os = "macos"))]
fn keychain_read(_service: &str, _account: &str) -> Result<Option<String>> {
    Err(anyhow!("Gmail Keychain access is supported only on macOS"))
}

#[cfg(not(target_os = "macos"))]
fn keychain_write(_service: &str, _account: &str, _value: &str) -> Result<()> {
    Err(anyhow!("Gmail Keychain access is supported only on macOS"))
}

#[cfg(not(target_os = "macos"))]
fn keychain_delete(_service: &str, _account: &str) -> Result<()> {
    Err(anyhow!("Gmail Keychain access is supported only on macOS"))
}

fn finish(result: Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("hudson mail: {error:#}");
            ExitCode::FAILURE
        }
    }
}

pub async fn run(action: MailAction) -> ExitCode {
    finish(run_inner(action).await)
}

/// The label `inbox process --apply` removes to archive a Gmail copy.
const INBOX_LABEL: &str = "INBOX";

/// Gmail's modify response is the whole message resource; the archive only
/// needs the status, so nothing is read out of it.
#[derive(Debug, Deserialize)]
struct ArchivedMessage {}

/// Archive one Gmail message — a web URL or message id — by removing its
/// INBOX label. Never a trash delete, and never a send. This is the only
/// Gmail write `inbox process` performs.
pub async fn archive(reference: &str) -> Result<()> {
    let id = gmail_message_id(reference).ok_or_else(|| {
        unresolved_reference_or(
            reference,
            "cannot resolve a Gmail message id from the reference",
        )
    })?;
    let client = client().await?;
    client.archive_message(&id).await
}

#[allow(clippy::too_many_lines)] // one dispatch arm per verb; each arm delegates
async fn run_inner(action: MailAction) -> Result<()> {
    match action {
        MailAction::Search { query, unscoped } => {
            let scope = if unscoped {
                SearchScope::default()
            } else {
                SearchScope::from_manifest()
            };
            let scoped = scoped_query(&query, &scope);
            let threads = client().await?.search(&scoped).await?;
            if threads.is_empty() && scoped != query {
                eprintln!(
                    "hudson mail: no threads matched within this Project's scope (its client \
                     contact and dockets: case numbers); pass --unscoped to search the whole mailbox"
                );
            }
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
            html_body_file,
            thread,
            draft_id,
            attach,
        } => {
            let (body_file, html) = match (body_file, html_body_file) {
                (Some(path), _) => (path, false),
                (None, Some(path)) => (path, true),
                (None, None) => return Err(anyhow!("pass --body-file or --html-body-file")),
            };
            // The signature resolves before any Gmail contact: the runtime
            // source is the deployment's stored row for the sending
            // account's Person, the env var is the announced offline
            // fallback, and neither yielding a block refuses below.
            let env_signature = std::env::var("NAVIGATOR_GMAIL_SIGNATURE").ok();
            let sending_account = configured_setup().ok().map(|(account, _)| account);
            let resolved =
                resolve_draft_signature(env_signature.as_deref(), sending_account.as_deref()).await;
            announce_signature_resolution(&resolved);
            let request = DraftRequest {
                to,
                cc,
                bcc,
                subject,
                body_file,
                html,
                thread,
                draft_id,
                attach,
                configured_signature: resolved.signature_body(),
            };
            let saved = save_draft_command(request, client).await?;
            println!(
                "Draft saved unsent with {} attachment(s). Gmail Drafts mailbox: https://mail.google.com/mail/#drafts (choose account {}). Gmail API draft ID: {}.",
                saved.attachments, saved.account, saved.id
            );
            Ok(())
        }
        MailAction::Packet {
            pdfs,
            vendor,
            recipient,
            service,
            return_receipt,
            mail_by,
            project,
            host,
            dry_run,
        } => {
            let job = packet::MailJob {
                pdfs,
                vendor,
                recipient,
                service,
                return_receipt,
                mail_by,
            };
            packet::check(&job)?;
            if dry_run {
                println!(
                    "To: {}\nSubject: {}\n\n{}",
                    job.vendor,
                    job.subject(),
                    job.sheet()
                );
                eprintln!("dry run: no draft saved and no Communication logged");
                return Ok(());
            }
            let body = tempfile::NamedTempFile::new().context("create the job sheet file")?;
            std::fs::write(body.path(), job.sheet()).context("write the job sheet")?;
            let env_signature = std::env::var("NAVIGATOR_GMAIL_SIGNATURE").ok();
            let sending_account = configured_setup().ok().map(|(account, _)| account);
            let resolved =
                resolve_draft_signature(env_signature.as_deref(), sending_account.as_deref()).await;
            announce_signature_resolution(&resolved);
            let saved = save_draft_command(
                DraftRequest {
                    to: vec![job.vendor.clone()],
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject: Some(job.subject()),
                    body_file: body.path().to_path_buf(),
                    html: false,
                    thread: None,
                    draft_id: None,
                    attach: job.pdfs.clone(),
                    configured_signature: resolved.signature_body(),
                },
                client,
            )
            .await?;
            println!(
                "Mail job drafted unsent to {} with {} attachment(s); review and send it from Gmail Drafts. Gmail API draft ID: {}.",
                job.vendor, saved.attachments, saved.id
            );
            let logged = cli::projects::communication::create_row(
                host.as_deref(),
                project.as_deref(),
                &cli::projects::communication::NewCommunication {
                    channel: "email_outbound".into(),
                    direction: "outbound".into(),
                    counterparty: job.vendor.clone(),
                    role: Some("other".into()),
                    at: chrono::Utc::now().to_rfc3339(),
                    duration: None,
                    body_file: body.path().to_path_buf(),
                    recorded: "none".into(),
                    meet_and_confer: None,
                    agreements: Vec::new(),
                    confirms: None,
                    acknowledges: None,
                    source_ref: Some(format!("gmail-draft:{}", saved.id)),
                    client_visible: false,
                    subject: Some(job.subject()),
                },
            )
            .await
            .context("the draft is saved, but logging its Communication failed")?;
            println!("{logged}");
            Ok(())
        }
        MailAction::Setup {
            account,
            client_id,
            project,
            secret,
        } => setup_command(account, client_id, project.as_deref(), secret.as_deref()),
        MailAction::Login {
            client_secret_file,
            client_secret_from_gcp,
            project,
            secret,
        } => {
            let gcp = if client_secret_file.is_some() {
                None
            } else {
                login::gcp_source(
                    project.as_deref(),
                    secret.as_deref(),
                    client_secret_from_gcp,
                    &load_setup(&setup_path())?,
                )?
            };
            login::login_command(client_secret_file.as_deref(), gcp.as_ref()).await
        }
        MailAction::Attachments {
            reference,
            dir,
            only,
            include_inline,
        } => attachments::run(&reference, &dir, only.as_deref(), include_inline).await,
        MailAction::File {
            thread,
            project,
            host,
            kind,
        } => {
            file_thread(
                &thread,
                project.as_deref(),
                host.as_deref(),
                kind.as_deref(),
            )
            .await
        }
        MailAction::Parse {
            reference,
            project,
            host,
            kind,
        } => {
            parse_command(
                &reference,
                project.as_deref(),
                host.as_deref(),
                kind.as_deref(),
            )
            .await
        }
        MailAction::Save {
            reference,
            dir,
            name,
        } => save_message_command(&reference, dir.as_deref(), name.as_deref()).await,
        MailAction::Ingest {
            path,
            project,
            host,
            provider_account,
        } => {
            ingest_provider_message(
                &path,
                project.as_deref(),
                host.as_deref(),
                provider_account.as_deref(),
            )
            .await
        }
    }
}

struct DraftRequest {
    to: Vec<String>,
    cc: Vec<String>,
    bcc: Vec<String>,
    subject: Option<String>,
    body_file: PathBuf,
    /// The body file is HTML; the plain-text part is derived from it.
    html: bool,
    thread: Option<String>,
    draft_id: Option<String>,
    attach: Vec<PathBuf>,
    /// `None` signs from the Gmail account's default signature.
    configured_signature: Option<String>,
}

struct SavedDraft {
    id: String,
    account: String,
    attachments: usize,
}

/// The signature a draft wears, with the provenance naming its source: the
/// line PR #472's letterhead provenance prints, for the mail path (LAW-280).
struct DraftSignature {
    body: String,
    provenance: String,
}

/// What signature resolution decided. The resolution runs before any Gmail
/// contact, so a missing-configuration refusal stays a local refusal — the
/// shape the draft tests pin.
enum ResolvedSignature {
    /// The deployment's stored row for the sending account's Person.
    Stored(DraftSignature),
    /// The `NAVIGATOR_GMAIL_SIGNATURE` env var, announced on stderr — the
    /// offline fallback, never silent, never a refusal.
    Env {
        signature: DraftSignature,
        announcement: String,
    },
    /// Neither source yielded a usable block: the draft signs with the
    /// Gmail account's default signature, and refuses only when that is
    /// blank too. The announcement says why the runtime source did not
    /// answer.
    Neither(String),
}

impl ResolvedSignature {
    /// The block that signs the draft, `None` when neither source yielded
    /// one.
    fn signature_body(self) -> Option<String> {
        match self {
            Self::Stored(signature) | Self::Env { signature, .. } => Some(signature.body),
            Self::Neither(_) => None,
        }
    }
}

/// Print what resolution decided, on stderr: the fallback announcement when
/// the runtime source did not answer, then the provenance line a stored or
/// env-signed draft carries.
fn announce_signature_resolution(resolved: &ResolvedSignature) {
    match resolved {
        ResolvedSignature::Stored(signature) => eprintln!("{}", signature.provenance),
        ResolvedSignature::Env {
            signature,
            announcement,
        } => {
            eprintln!("{announcement}");
            eprintln!("{}", signature.provenance);
        }
        ResolvedSignature::Neither(reason) => eprintln!("{reason}"),
    }
}

/// Resolve the signature a draft wears: the deployment's stored row for the
/// sending account's Person when the stores answer within a bounded budget,
/// else the `NAVIGATOR_GMAIL_SIGNATURE` env var — announced, never silent
/// (LAW-280). A lawyer drafting from a plane without the deployment keeps
/// drafting; only when neither source yields a block does the draft refuse,
/// still before any Gmail contact.
async fn resolve_draft_signature(
    env_signature: Option<&str>,
    sending_account: Option<&str>,
) -> ResolvedSignature {
    let budget = std::time::Duration::from_secs(3);
    match tokio::time::timeout(budget, runtime_signature(sending_account)).await {
        Ok(Ok(signature)) => ResolvedSignature::Stored(DraftSignature {
            body: signature.body,
            provenance: format!(
                "signature: source=runtime person={} revision={}",
                signature.person_id,
                signature.updated_at.as_deref().unwrap_or("n/a"),
            ),
        }),
        Ok(Err(reason)) => env_fallback(env_signature, &reason),
        Err(elapsed) => env_fallback(
            env_signature,
            &format!("the signature store did not answer within {budget:?}: {elapsed:?}"),
        ),
    }
}

/// The stored signature for the sending account's Person, or `Err` naming
/// why the deployment's stores could not answer — the caller falls back to
/// the env var and announces it. The account resolves against `Person.email`
/// exactly: a sending account that is no Person has no row to fetch, and the
/// fallback says so rather than signing with another attorney's block.
async fn runtime_signature(
    sending_account: Option<&str>,
) -> Result<store::mail_signatures::MailSignature, String> {
    let account = sending_account.ok_or("no configured Gmail account; run hudson mail setup")?;
    let surreal = store::surreal::connect_from_env()
        .await
        .map_err(|error| format!("the deployment's document store is unreachable: {error}"))?;
    let pg = store::postgres::PostgresDb::from_env()
        .await
        .map_err(|error| format!("the deployment's relational store is unreachable: {error}"))?;
    let person = store::persons::find_by_email_ci(&surreal, account)
        .await
        .map_err(|error| format!("the sending account's Person lookup failed: {error}"))?
        .ok_or_else(|| format!("no Person matches the sending account {account}"))?;
    store::mail_signatures::for_person(&surreal, &pg, person.id)
        .await
        .map_err(|error| format!("the stored signature read failed: {error}"))?
        .ok_or_else(|| format!("no stored signature row for the sending account {account}"))
}

/// The env fallback for a runtime reason: announced, never silent. A blank
/// env value is not a usable block either — `with_signature` would refuse
/// it, so it leaves `Neither` and the refusal names the env var itself.
fn env_fallback(env_signature: Option<&str>, reason: &str) -> ResolvedSignature {
    let unavailable =
        format!("hudson mail: the deployment's stored signature is unavailable ({reason})");
    match env_signature.filter(|value| !value.trim().is_empty()) {
        Some(body) => ResolvedSignature::Env {
            signature: DraftSignature {
                body: body.to_owned(),
                provenance: "signature: source=env person=n/a revision=n/a".to_owned(),
            },
            announcement: format!(
                "{unavailable}; signing from the NAVIGATOR_GMAIL_SIGNATURE fallback"
            ),
        },
        None => ResolvedSignature::Neither(format!(
            "{unavailable} and NAVIGATOR_GMAIL_SIGNATURE is unset; \
             signing with the Gmail account's default signature"
        )),
    }
}

/// Reads the body and every attachment from disk, then connects to Gmail and
/// saves one unsent draft. Local refusals happen before `connect` runs; a
/// draft with no configured signature signs with the Gmail account's default
/// signature and refuses only when that is blank too (LAW-316).
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
        html,
        thread,
        draft_id,
        attach,
        configured_signature,
    } = request;
    let source = std::fs::read_to_string(&body_file)
        .with_context(|| format!("read draft body {}", body_file.display()))?;
    if source.trim().is_empty() {
        return Err(anyhow!("draft body is empty"));
    }
    let (text, html) = if html {
        (
            mail_parser::decoders::html::html_to_text(&source),
            Some(source),
        )
    } else {
        (source, None)
    };
    let signed = configured_signature
        .map(|configured| signature::with_signature(&text, Some(&configured)))
        .transpose()?;
    let attachments = load_attachments(&attach, html.as_deref().unwrap_or(&text))?;
    let gmail = connect().await?;
    let profile = gmail.profile().await?;
    let signed = account_signed(&gmail, &profile.email_address, &text, signed).await?;
    let (project, contact) = current_project_context();
    let (to, subject, in_reply_to, quoted) = if let Some(thread_id) = thread.as_deref() {
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
        (to, subject, message_id, quoted_history(latest))
    } else {
        warn_if_project_draft_thread_unverified(project.as_deref(), draft_id.as_deref());
        if to.is_empty() {
            return Err(anyhow!("pass --to or --thread"));
        }
        (
            to,
            subject.ok_or_else(|| anyhow!("pass --subject or --thread"))?,
            None,
            None,
        )
    };
    let (body, html) = with_history(&signed, html.as_deref(), quoted.as_ref());
    let raw = draft_raw(
        &to,
        &cc,
        &bcc,
        &subject,
        &body,
        html.as_deref(),
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

/// The signed body: `signed` when a configured signature already signed it,
/// else `text` signed with the Gmail account's default signature. Warns when
/// a sign-off line sits directly above the signature.
async fn account_signed(
    gmail: &GmailClient,
    account: &str,
    text: &str,
    signed: Option<String>,
) -> Result<String> {
    let signed = if let Some(signed) = signed {
        signed
    } else {
        let block = gmail.send_as_signature(account).await?;
        let signed = signature::with_signature(text, Some(&block))?;
        eprintln!("signature: source=gmail person=n/a revision=n/a");
        signed
    };
    if let Some(line) = signature::sign_off(&signed) {
        eprintln!(
            "hudson mail: the body ends with the sign-off `{line}` above the signature; \
             the signature is the close, so remove it"
        );
    }
    Ok(signed)
}

/// The replied-to message a reply quotes: its attribution line and its
/// plain text.
struct QuotedHistory {
    attribution: String,
    text: String,
}

/// The latest thread message as quoted history, the way the Gmail client
/// quotes it; `None` when it carries no readable text.
fn quoted_history(message: &GmailMessage) -> Option<QuotedHistory> {
    let text = plain_text(message.payload.as_ref()?)?;
    let from = header(message, "From").unwrap_or("the sender");
    let attribution = match header(message, "Date") {
        Some(date) => format!("On {date}, {from} wrote:"),
        None => format!("{from} wrote:"),
    };
    Some(QuotedHistory {
        attribution,
        text: text.replace("\r\n", "\n").trim_end().to_owned(),
    })
}

/// Escapes text for an HTML part, keeping its line breaks.
fn html_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\n', "<br>")
}

/// The draft's plain-text and HTML parts: the signed body, then any quoted
/// history after the signature. The HTML part is the lawyer's HTML with the
/// same signature and history appended.
fn with_history(
    signed: &str,
    html: Option<&str>,
    quoted: Option<&QuotedHistory>,
) -> (String, Option<String>) {
    let mut text = signed.to_owned();
    if let Some(quoted) = quoted {
        let lines: Vec<String> = quoted
            .text
            .lines()
            .map(|line| format!("> {line}").trim_end().to_owned())
            .collect();
        let _ = write!(text, "\n\n{}\n{}", quoted.attribution, lines.join("\n"));
    }
    let html = html.map(|html| {
        let signature = signed
            .rsplit_once("-- \n")
            .map_or(signed, |(_, block)| block);
        let mut tail = format!("<br><br>-- <br>{}", html_text(signature));
        if let Some(quoted) = quoted {
            let _ = write!(
                tail,
                "<br><br><div class=\"gmail_quote\">{}<blockquote class=\"gmail_quote\" \
                 style=\"margin:0 0 0 .8ex;border-left:1px #ccc solid;padding-left:1ex\">{}\
                 </blockquote></div>",
                html_text(&quoted.attribution),
                html_text(&quoted.text)
            );
        }
        match html.to_ascii_lowercase().rfind("</body>") {
            Some(at) => format!("{}{tail}{}", &html[..at], &html[at..]),
            None => format!("{html}{tail}"),
        }
    });
    (text, html)
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

/// Files one saved RFC 5322 message into the matter inbox through the
/// Navigator ingest door (`POST /app/api/projects/{id}/inbox-ingest`).
/// Ingest-only: the door stores the message as an Inbox conversation
/// attributed to the Project and never sends anything. The raw bytes travel
/// as base64 and the server re-parses them, so the CLI cannot claim fields
/// the message does not carry.
async fn ingest_provider_message(
    path: &Path,
    project_code: Option<&str>,
    host: Option<&str>,
    provider_account: Option<&str>,
) -> Result<()> {
    let raw = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
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
    let body = json!({
        "raw": base64::engine::general_purpose::STANDARD.encode(&raw),
        "provider_account": provider_account,
    });
    let response = reqwest::Client::new()
        .post(format!("{base}/app/api/projects/{project_id}/inbox-ingest"))
        .bearer_auth(navigator_token)
        .timeout(std::time::Duration::from_secs(30))
        .json(&body)
        .send()
        .await
        .context("ingest provider message")?;
    if !response.status().is_success() {
        return Err(refused("ingest", response).await);
    }
    let text = response.text().await.context("read ingest response")?;
    println!("{text}");
    Ok(())
}

async fn file_thread(
    thread_id: &str,
    project_code: Option<&str>,
    host: Option<&str>,
    kind: Option<&str>,
) -> Result<()> {
    let kind = kind.unwrap_or(DEFAULT_PARSE_UPLOAD_KIND);
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
    let mut attachments_filed = 0;
    for (message, body) in thread.messages.iter().zip(bodies) {
        if has_attachments(message) {
            let filed = file_raw_message(&gmail, &target, &message.id, kind).await?;
            attachments_filed += filed.attachments_filed;
            filed.print_skipped();
        } else {
            file_message(&target, message, &body).await?;
        }
    }
    println!(
        "Filed {} message(s) from thread {} as Communications",
        thread.messages.len(),
        thread.id
    );
    if attachments_filed > 0 {
        println!("Filed {attachments_filed} attachment(s) as {kind} documents");
    }
    Ok(())
}

/// Whether a message carries a part a lawyer would call an attachment.
fn has_attachments(message: &GmailMessage) -> bool {
    message.payload.as_ref().is_some_and(|part| {
        let mut names = Vec::new();
        attachments(part, &mut names);
        !names.is_empty()
    })
}

/// File a message that carries attachments: fetch its RFC 822 copy through
/// the read-only Gmail path and post it to the `mail/eml` door, which files
/// the Communication and each attachment as a linked, internal document of
/// `kind`. The door dedupes on the message's own Message-ID, so a re-run
/// writes nothing twice.
async fn file_raw_message(
    gmail: &GmailClient,
    target: &FileTarget<'_>,
    message_id: &str,
    kind: &str,
) -> Result<EmlFiled> {
    let raw_field = gmail.raw_message(message_id).await?;
    let raw = decode_gmail_raw(
        raw_field
            .raw
            .as_deref()
            .ok_or_else(|| anyhow!("message {message_id} came back with no raw copy"))?,
    )?;
    let filed = post_eml(
        target.base,
        target.navigator_token,
        target.project_id,
        &raw,
        kind,
        Some(target.mailbox),
    )
    .await?;
    Ok(eml_filing_summary(&filed))
}

struct FileTarget<'a> {
    base: &'a str,
    navigator_token: &'a str,
    project_id: Uuid,
    mailbox: &'a str,
}

/// `mail parse`: file one message as a matter Communication. A Gmail web URL
/// (or message id) fetches through the read-only Gmail path `mail file`
/// uses; a local .eml files from its own bytes; any other local file rides
/// the existing `navigator document upload` lane as a document.
async fn parse_command(
    reference: &str,
    project: Option<&str>,
    host: Option<&str>,
    kind: Option<&str>,
) -> Result<()> {
    if Path::new(reference).exists() {
        return parse_local_file(reference, project, host, kind).await;
    }
    let message_id = gmail_message_id(reference).ok_or_else(|| {
        unresolved_reference_or(
            reference,
            "the reference is neither an existing file, a Gmail web URL, nor a Gmail message id",
        )
    })?;
    let names_conversation = reference_names_conversation(reference);
    parse_gmail_message(&message_id, names_conversation, project, host, kind).await
}

/// Whether a reference names a whole conversation rather than one message:
/// a Gmail web URL whose id is a thread (the browser's `#inbox/<id>` view, a
/// `thread-f:` web id, or the legacy hex and `?th=` forms). A bare id and a
/// `msg-f:` web id name one message.
fn reference_names_conversation(reference: &str) -> bool {
    if Url::parse(reference).is_err() {
        return false;
    }
    let Some(segment) = reference_id_segment(reference) else {
        return false;
    };
    !web_id_kind(&segment).is_some_and(|kind| kind.starts_with("msg"))
}

/// The `<kind>` of a browser web id (`thread-f`, `msg-f`, `f`), or None
/// when `token` is not one.
fn web_id_kind(token: &str) -> Option<String> {
    decode_web_id(token).map(|(kind, _)| kind)
}

async fn parse_gmail_message(
    message_id: &str,
    names_conversation: bool,
    project: Option<&str>,
    host: Option<&str>,
    kind: Option<&str>,
) -> Result<()> {
    let kind = kind.unwrap_or(DEFAULT_PARSE_UPLOAD_KIND);
    let local = crate::projects::manifest::read(Path::new("."));
    let project_code = project
        .or_else(|| local.as_ref().and_then(|m| m.project.as_deref()))
        .ok_or_else(|| anyhow!("pass --project or run from a Project checkout"))?;
    let host = host.or_else(|| local.as_ref().and_then(|m| m.host.as_deref()));
    let (base, navigator_token) = crate::remote::resolve(host)?;
    let project_id =
        crate::projects::surfaces::resolve_project_id(&base, &navigator_token, project_code)
            .await?;
    let gmail = client().await?;
    let profile = gmail.profile().await?;
    // A conversation link names the thread, whose id is its first message.
    // The lawyer means the message in front of them: the newest one.
    let message_id = if names_conversation {
        let thread = gmail.thread(message_id).await?;
        let latest = thread
            .messages
            .last()
            .ok_or_else(|| anyhow!("thread {message_id} has no messages"))?;
        if thread.messages.len() > 1 {
            println!(
                "The link names a {}-message conversation; filing its latest message, {} ({})",
                thread.messages.len(),
                latest.id,
                now_or_message_date(latest).unwrap_or_default()
            );
        }
        latest.id.clone()
    } else {
        message_id.to_string()
    };
    let message_id = message_id.as_str();
    let message = gmail.message(message_id).await?;
    let payload = message
        .payload
        .as_ref()
        .ok_or_else(|| anyhow!("message {message_id} has no supported body"))?;
    let mut names = Vec::new();
    attachments(payload, &mut names);
    let body = plain_text(payload)
        .filter(|body| !body.trim().is_empty())
        .ok_or_else(|| {
            anyhow!("message {message_id} has no supported text body; nothing was filed")
        })?;
    let expected_contact = local.as_ref().and_then(|manifest| {
        manifest_project_contact(
            Some(project_code),
            manifest.project.as_deref(),
            manifest.client_dri.as_deref(),
        )
    });
    if let Some(contact) = expected_contact {
        if !message_mentions_contact(&message, &contact) {
            eprintln!(
                "warning: message does not appear to belong to this Project's client contact"
            );
        }
    }
    let target = FileTarget {
        base: &base,
        navigator_token: &navigator_token,
        project_id,
        mailbox: &profile.email_address,
    };
    if names.is_empty() {
        file_message(&target, &message, &body).await?;
        println!("Filed the message as a Communication");
        return Ok(());
    }
    let filed = file_raw_message(&gmail, &target, message_id, kind).await?;
    println!("Filed the message as a Communication");
    if filed.deduped {
        println!("Already filed (deduped on {})", filed.source_ref);
    }
    println!(
        "Filed {} of {} attachment(s) as internal {kind} documents: {}",
        filed.attachments_filed,
        names.len(),
        names.join(", ")
    );
    filed.print_skipped();
    Ok(())
}

async fn parse_local_file(
    reference: &str,
    project: Option<&str>,
    host: Option<&str>,
    kind: Option<&str>,
) -> Result<()> {
    let local = crate::projects::manifest::read(Path::new("."));
    let project_code = project
        .or_else(|| local.as_ref().and_then(|m| m.project.as_deref()))
        .ok_or_else(|| anyhow!("pass --project or run from a Project checkout"))?;
    let path = Path::new(reference);
    let kind = kind.unwrap_or(DEFAULT_PARSE_UPLOAD_KIND);
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("eml"))
    {
        return parse_eml_file(path, project_code, host, kind).await;
    }
    println!("Uploading {} as a {kind} document", path.display());
    let exit = crate::remote::document_upload(
        host,
        project_code,
        path,
        kind,
        Some("internal"),
        None,
        None,
        None,
        None,
        &crate::remote::DocumentExecution::default(),
        None,
    )
    .await;
    if exit == std::process::ExitCode::SUCCESS {
        Ok(())
    } else {
        Err(anyhow!("document upload failed"))
    }
}

async fn parse_eml_file(
    path: &Path,
    project_code: &str,
    host: Option<&str>,
    kind: &str,
) -> Result<()> {
    let local = crate::projects::manifest::read(Path::new("."));
    let host = host.or_else(|| local.as_ref().and_then(|m| m.host.as_deref()));
    let (base, navigator_token) = crate::remote::resolve(host)?;
    let project_id =
        crate::projects::surfaces::resolve_project_id(&base, &navigator_token, project_code)
            .await?;
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let filed = post_eml(&base, &navigator_token, project_id, &bytes, kind, None).await?;
    let summary = eml_filing_summary(&filed);
    println!("Filed {} as a Communication", path.display());
    if summary.deduped {
        println!("Already filed (deduped on {})", summary.source_ref);
    }
    if summary.attachments_filed > 0 {
        println!(
            "Filed {} attachment(s) as documents",
            summary.attachments_filed
        );
    }
    summary.print_skipped();
    Ok(())
}

/// Post a raw .eml to the door, which parses and files it server-side, so a
/// local .eml files without any per-machine Gmail OAuth setup. The door
/// decides direction from the signed-in lawyer's own address; `kind` is the
/// asset-lane kind its attachments file under.
async fn post_eml(
    base: &str,
    navigator_token: &str,
    project_id: Uuid,
    bytes: &[u8],
    kind: &str,
    mailbox: Option<&str>,
) -> Result<serde_json::Value> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mut request = json!({ "raw_base64": encoded, "kind": kind });
    if let Some(mailbox) = mailbox {
        request["mailbox"] = json!(mailbox);
    }
    let response = reqwest::Client::new()
        .post(format!("{base}/app/api/projects/{project_id}/mail/eml"))
        .bearer_auth(navigator_token)
        .json(&request)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .context("file the .eml through the mail/eml door")?;
    if !response.status().is_success() {
        return Err(refused("file the .eml", response).await);
    }
    response.json().await.context("read the door's response")
}

/// Report the status and local guidance for known refusal codes. Server
/// message fields may contain private input, so they never enter diagnostics.
async fn refused(action: &str, mut response: reqwest::Response) -> anyhow::Error {
    let status = response.status();
    let reason = refusal_reason(&mut response).await.unwrap_or_default();
    anyhow!("{action} refused: {status}{reason}")
}

/// Buffer at most 4 KiB before decoding a refusal. Unknown or oversized
/// responses retain only their HTTP status, never arbitrary response values.
async fn refusal_reason(response: &mut reqwest::Response) -> Option<String> {
    const MAX_REFUSAL_BYTES: usize = 4096;
    #[derive(Deserialize)]
    struct Refusal {
        error: String,
    }
    let mut body = Vec::with_capacity(MAX_REFUSAL_BYTES);
    while let Some(chunk) = response.chunk().await.ok()? {
        if chunk.len() > MAX_REFUSAL_BYTES.saturating_sub(body.len()) {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    let refusal: Refusal = serde_json::from_slice(&body).ok()?;
    let guidance = match refusal.error.as_str() {
        "kind_required" => "A document kind is required for attachments; pass --kind.",
        "invalid_kind" => "Choose a supported --kind value.",
        "not_base64" | "invalid_base64" => "The message could not be decoded.",
        "not_mime" | "unparseable_message" => "The file must contain an email message.",
        "no_date" => "The saved message requires a Date header.",
        "bad_date" => "The saved message Date header is out of range.",
        "no_body" => "The saved message has no supported text body.",
        "message_too_large" => "The message exceeds the inbound size cap.",
        "project_has_no_firm" => "The Project needs an owning Firm before inbox filing.",
        "unauthenticated" => "Sign in to Navigator again.",
        "forbidden" => "The Navigator session does not allow this action.",
        "not_found" => "The requested resource is not available to this session.",
        "invalid_request" => "Check the filing request fields.",
        "internal" => "Navigator could not complete the filing.",
        _ => return None,
    };
    Some(format!(": {}: {guidance}", refusal.error))
}

/// What the .eml door's response means for the operator. Pure, so the
/// response's shape is read in exactly one place.
fn eml_filing_summary(response: &serde_json::Value) -> EmlFiled {
    EmlFiled {
        source_ref: response["source_ref"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        deduped: response["deduped"].as_bool().unwrap_or(false),
        attachments_filed: response["filed"].as_array().map_or(0, std::vec::Vec::len),
        skipped: response["skipped"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|part| {
                let filename = part["filename"].as_str().unwrap_or_default();
                let reason = part["reason"]
                    .as_str()
                    .unwrap_or_default()
                    .replace('_', " ");
                match part["document_slug"].as_str() {
                    Some(slug) => format!("{filename}: {reason} as {slug}"),
                    None => format!("{filename}: {reason}"),
                }
            })
            .collect(),
    }
}

impl EmlFiled {
    /// Name every attachment the door left unfiled, and why.
    fn print_skipped(&self) {
        for line in &self.skipped {
            println!("Skipped {line}");
        }
    }
}

struct EmlFiled {
    source_ref: String,
    deduped: bool,
    attachments_filed: usize,
    /// One `<filename>: <reason>` line per attachment left unfiled.
    skipped: Vec<String>,
}

/// `mail save`: fetch the message's RFC 822 copy through the read-only Gmail
/// path and write it under the Project checkout's git-ignored mail/ folder
/// with the conventional YYYY.MM.DD_<subject>.eml name. Saving never sends.
async fn save_message_command(
    reference: &str,
    dir: Option<&Path>,
    name: Option<&str>,
) -> Result<()> {
    let message_id = gmail_message_id(reference).ok_or_else(|| unresolved_reference(reference))?;
    if let Some(name) = name {
        check_mail_save_name(name)?;
    }
    let gmail = client().await?;
    let raw_field = gmail.raw_message(&message_id).await?;
    let raw = decode_gmail_raw(
        raw_field
            .raw
            .as_deref()
            .ok_or_else(|| anyhow!("message {message_id} came back with no raw copy"))?,
    )?;
    let message = mail_parser::MessageParser::default()
        .parse(&raw)
        .ok_or_else(|| anyhow!("the saved message is not a MIME email"))?;
    let name = if let Some(name) = name {
        name.to_string()
    } else {
        let sent_on = message
            .date()
            .map(|date| format!("{:04}.{:02}.{:02}", date.year, date.month, date.day))
            .ok_or_else(|| anyhow!("the message has no Date header; pass --name"))?;
        conventional_mail_name(&sent_on, message.subject().unwrap_or(""))
    };
    let path = dir.unwrap_or_else(|| Path::new(MAIL_DIRECTORY)).join(&name);
    match write_saved_mail(&path, &raw)? {
        SavedMail::Written => println!("Saved {}", path.display()),
        SavedMail::AlreadySaved => println!("Already saved: {}", path.display()),
    }
    Ok(())
}

async fn file_message(target: &FileTarget<'_>, message: &GmailMessage, body: &str) -> Result<()> {
    let request = communication_request(
        target.mailbox,
        header(message, "From").unwrap_or_default(),
        header(message, "To").unwrap_or_default(),
        header(message, "Subject"),
        &now_or_message_date(message)?,
        body,
        &message.id,
    );
    post_communication(target, &request).await
}

async fn post_communication(target: &FileTarget<'_>, request: &serde_json::Value) -> Result<()> {
    let response = reqwest::Client::new()
        .post(format!(
            "{}/app/api/projects/{}/communications",
            target.base, target.project_id
        ))
        .bearer_auth(target.navigator_token)
        .json(request)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .context("file email Communication")?;
    if !response.status().is_success() {
        return Err(refused("file email Communication", response).await);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIGURED_SIGNATURE: &str = "Example Attorney\nConfidential: intended recipient only.";

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
    fn gmail_endpoints_expose_reads_draft_writes_and_archive_only() {
        assert_eq!(endpoint(Operation::SearchThreads, None).0, "GET");
        assert_eq!(endpoint(Operation::ReadThread, Some("thread-id")).0, "GET");
        assert_eq!(
            endpoint(Operation::ReadMessage, Some("message-id")).0,
            "GET"
        );
        assert_eq!(endpoint(Operation::ReadProfile, None).0, "GET");
        assert_eq!(
            endpoint(Operation::ReadSendAs, Some("lawyer@example.test")).0,
            "GET"
        );
        assert_eq!(endpoint(Operation::CreateDraft, None).0, "POST");
        assert_eq!(endpoint(Operation::UpdateDraft, Some("draft-id")).0, "PUT");
        // The archive is a label write — POST modify — never a delete or a
        // send.
        assert_eq!(
            endpoint(Operation::ModifyMessage, Some("message-id")).0,
            "POST"
        );
        assert_eq!(endpoint_suffix(Operation::ModifyMessage), Some("modify"));
    }

    #[test]
    fn gmail_web_urls_yield_their_message_id() {
        assert_eq!(
            gmail_message_id("https://mail.google.com/mail/u/0/#inbox/18fa3bc2d4e5f6a7").as_deref(),
            Some("18fa3bc2d4e5f6a7")
        );
        assert_eq!(
            gmail_message_id("https://mail.google.com/mail/u/1/#label/Work/AbC_-12345678")
                .as_deref(),
            Some("AbC_-12345678")
        );
        assert_eq!(
            gmail_message_id("https://mail.google.com/mail/u/0/?ui=2&view=om&th=18fa3bc2d4e5f6a7")
                .as_deref(),
            Some("18fa3bc2d4e5f6a7")
        );
        assert_eq!(
            gmail_message_id("18fa3bc2d4e5f6a7").as_deref(),
            Some("18fa3bc2d4e5f6a7")
        );
    }

    #[test]
    fn a_conversation_link_names_the_thread_and_a_message_link_names_one_message() {
        // Synthetic web ids for the decimal id 1234567890123456789.
        let thread =
            "https://mail.google.com/mail/u/0/#inbox/NHgvNZFVxQXXXjLLNMnkPsxfnNsvVMQRDgQWNfRSckg";
        let message =
            "https://mail.google.com/mail/u/0/#inbox/CXKnWZngLGgrlKSJsQCLdJqbQBNrBFBFHmpvwkg";
        assert!(reference_names_conversation(thread));
        assert!(reference_names_conversation(
            "https://mail.google.com/mail/u/0/#inbox/112210f47de98115"
        ));
        assert!(!reference_names_conversation(message));
        assert!(!reference_names_conversation("112210f47de98115"));
        assert_eq!(
            gmail_message_id(thread).as_deref(),
            Some("112210f47de98115")
        );
        assert_eq!(
            gmail_message_id(message).as_deref(),
            Some("112210f47de98115")
        );
        assert_eq!(
            web_id_kind("CXKnWZngLGgrlKSJsQCLdJqbQBNrBFBFHmpvwkg").as_deref(),
            Some("msg-f")
        );
    }

    #[tokio::test]
    async fn a_message_with_attachments_files_its_raw_copy_through_the_eml_door() {
        use wiremock::matchers::{body_partial_json, method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let raw = b"From: client@example.test\r\nTo: lawyer@example.test\r\n\
Date: Tue, 10 Nov 2026 11:36:29 -0500\r\nSubject: Update\r\n\
MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n\
--b\r\nContent-Type: text/plain\r\n\r\nScreenshots attached.\r\n\
--b\r\nContent-Type: image/png\r\nContent-Disposition: attachment; filename=\"a.png\"\r\n\r\nPNG\r\n\
--b--\r\n";
        let gmail = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/users/me/messages/message-1"))
            .and(query_param("format", "raw"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "message-1",
                "raw": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw),
            })))
            .expect(1)
            .mount(&gmail)
            .await;
        let navigator = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/app/api/projects/{}/mail/eml", Uuid::nil())))
            .and(body_partial_json(json!({
                "kind": "exhibit",
                "mailbox": "lawyer@example.test",
                "raw_base64": base64::engine::general_purpose::STANDARD.encode(raw),
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "source_ref": "eml:synthetic",
                "deduped": false,
                "filed": [{ "slug": "mail/synthetic/0.png" }],
            })))
            .expect(1)
            .mount(&navigator)
            .await;
        let client = GmailClient::with_base_url("t".into(), Url::parse(&gmail.uri()).unwrap());
        let base = navigator.uri();
        let target = FileTarget {
            base: &base,
            navigator_token: "navigator-token",
            project_id: Uuid::nil(),
            mailbox: "lawyer@example.test",
        };
        let filed = file_raw_message(&client, &target, "message-1", "exhibit")
            .await
            .unwrap();
        assert_eq!(filed.attachments_filed, 1);
        assert!(!filed.deduped);
        assert_eq!(filed.source_ref, "eml:synthetic");
    }

    #[test]
    fn gmail_web_ids_in_browser_urls_decode_to_their_hex_id() {
        // Synthetic web ids: base64("f:<decimal>") re-encoded in Gmail's
        // forty-consonant alphabet, the `FMfcgz…` shape the browser shows.
        for url in [
            "https://mail.google.com/mail/u/0/#inbox/FMfcgzQcqbThLrGqQsnjtQPDxLcKcpDw",
            "https://mail.google.com/mail/u/0/#all/FMfcgzQcqbThLrGqQsnjtQPDxLcKcpDw",
            "https://mail.google.com/mail/u/1/#label/Court+Notices/FMfcgzQcqbThLrGqQsnjtQPDxLcKcpDw",
            "https://mail.google.com/mail/u/0/#search/from%3Aecf/FMfcgzQcqbThLrGqQsnjtQPDxLcKcpDw",
            "FMfcgzQcqbThLrGqQsnjtQPDxLcKcpDw",
        ] {
            assert_eq!(
                gmail_message_id(url).as_deref(),
                Some("19a24cff5a335f79"),
                "{url}"
            );
        }
        // `msg-f:` and `thread-f:` payloads decode the same way.
        assert_eq!(
            gmail_message_id(
                "https://mail.google.com/mail/u/0/#inbox/CXKnWZngLGgsJngQCFjlMGBrHTdgzqmNNwWpWbB"
            )
            .as_deref(),
            Some("19a24cff5a335f7a")
        );
        assert_eq!(
            gmail_web_id("NHgvNZFVxQXXXjLLlrBqXjgFWLFMMggHDSCgTnzMCbV"),
            Some(GmailWebId::Decoded("19a24cff5a335f7b".into()))
        );
    }

    #[test]
    fn gmail_web_ids_without_a_hex_id_are_named_in_the_error() {
        // `thread-a:r…` web ids (drafts, some sent mail) carry no API id.
        let url =
            "https://mail.google.com/mail/u/0/#sent/hxTnWLsSgZFtzTnCPbnHcvHmljQSrDDfZXkDZLjbFLLzr";
        assert_eq!(gmail_message_id(url), None);
        let message = unresolved_reference(url).to_string();
        assert!(message.contains("Gmail web id"), "{message}");
        assert!(message.contains("hudson mail search"), "{message}");
        let plain = unresolved_reference("notes/draft-body.txt").to_string();
        assert!(plain.contains("neither"), "{plain}");
    }

    #[test]
    fn non_gmail_references_name_no_message() {
        assert_eq!(
            gmail_message_id("https://example.com/mail/u/0/#inbox/18fa3bc2d4e5f6a7"),
            None
        );
        assert_eq!(
            gmail_message_id("https://mail.google.com/mail/u/0/#search/retainer"),
            None
        );
        assert_eq!(gmail_message_id("notes/draft-body.txt"), None);
        assert_eq!(gmail_message_id("short"), None);
    }

    #[test]
    fn communication_requests_carry_direction_counterparty_and_source() {
        let inbound = communication_request(
            "lawyer@neonlaw.test",
            "Client <client@example.test>",
            "lawyer@neonlaw.test",
            Some("Retainer questions"),
            "2026-10-30T14:03:00+00:00",
            "Can I sign electronically?",
            "<abc-123@example.test>",
        );
        assert_eq!(inbound["channel"], "email_inbound");
        assert_eq!(inbound["direction"], "inbound");
        assert_eq!(inbound["counterparty"], "Client <client@example.test>");
        assert_eq!(
            inbound["source_ref"],
            "gmail:lawyer@neonlaw.test:<abc-123@example.test>"
        );

        let outbound = communication_request(
            "lawyer@neonlaw.test",
            "lawyer@neonlaw.test",
            "client@example.test",
            None,
            "2026-10-30T14:03:00+00:00",
            "Draft is ready for your review.",
            "18fa3bc2d4e5f6a7",
        );
        assert_eq!(outbound["channel"], "email_outbound");
        assert_eq!(outbound["direction"], "outbound");
        assert_eq!(outbound["counterparty"], "client@example.test");
    }

    #[test]
    fn an_eml_door_response_summarizes_dedup_and_attachments() {
        let summary = eml_filing_summary(&serde_json::json!({
            "source_ref": "eml:abc-123@example.test",
            "direction": "inbound",
            "deduped": true,
            "communication_id": "0195c9e1-7c2f-7000-8000-000000000000",
            "attachments": [],
            "filed": [
                { "current_version": { "version": 1 } },
                { "current_version": { "version": 1 } }
            ]
        }));
        assert_eq!(summary.source_ref, "eml:abc-123@example.test");
        assert!(summary.deduped);
        assert_eq!(summary.attachments_filed, 2);
        assert!(summary.skipped.is_empty());

        // An attachment the matter already holds is named, with its document.
        let summary = eml_filing_summary(&serde_json::json!({
            "source_ref": "eml:abc-123@example.test",
            "deduped": false,
            "filed": [],
            "skipped": [{
                "filename": "order.pdf",
                "reason": "already_on_file",
                "document_slug": "2026.10.30_order.pdf"
            }]
        }));
        assert_eq!(
            summary.skipped,
            ["order.pdf: already on file as 2026.10.30_order.pdf"]
        );

        // A dry-run response: no communication row, no attachments filed.
        let summary = eml_filing_summary(&serde_json::json!({
            "source_ref": "eml:sha256:9f2b",
            "direction": "inbound",
            "deduped": false,
            "attachments": [],
            "filed": []
        }));
        assert_eq!(summary.source_ref, "eml:sha256:9f2b");
        assert!(!summary.deduped);
        assert_eq!(summary.attachments_filed, 0);
    }

    #[test]
    fn gmail_raw_copies_decode_from_url_safe_base64() {
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(b"From: client@example.test\r\n\r\nHello");
        assert_eq!(
            decode_gmail_raw(&encoded).unwrap(),
            b"From: client@example.test\r\n\r\nHello".to_vec()
        );
        assert!(decode_gmail_raw("not base64!").is_err());
    }

    #[test]
    fn gmail_raw_copies_decode_whether_or_not_they_carry_padding() {
        // A length that is not a multiple of three, so the padded form ends in `=`.
        let message = b"From: court@example.test\r\nSubject: Notice\r\n\r\nHi?>";
        let padded = base64::engine::general_purpose::URL_SAFE.encode(message);
        assert!(padded.ends_with('='), "fixture must exercise padding");
        assert_eq!(decode_gmail_raw(&padded).unwrap(), message.to_vec());
        let unpadded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(message);
        assert_eq!(decode_gmail_raw(&unpadded).unwrap(), message.to_vec());
    }

    #[test]
    fn body_parts_with_empty_filenames_are_not_attachments() {
        // Gmail's API reports `filename: ""` on every body part, the
        // multipart container included.
        let payload: MessagePart = serde_json::from_value(json!({
            "mimeType": "multipart/alternative",
            "filename": "",
            "body": { "size": 0 },
            "parts": [
                { "mimeType": "text/plain", "filename": "",
                  "headers": [{ "name": "Content-Type", "value": "text/plain; charset=UTF-8" }],
                  "body": { "data": "SGVsbG8" } },
                { "mimeType": "text/html", "filename": "",
                  "body": { "data": "PHA-SGVsbG88L3A-" } }
            ]
        }))
        .unwrap();
        let mut names = Vec::new();
        attachments(&payload, &mut names);
        assert!(names.is_empty(), "{names:?}");
    }

    #[test]
    fn named_or_disposition_attachment_parts_are_attachments() {
        let payload: MessagePart = serde_json::from_value(json!({
            "mimeType": "multipart/mixed",
            "filename": "",
            "parts": [
                { "mimeType": "text/plain", "filename": "", "body": { "data": "SGVsbG8" } },
                { "mimeType": "application/pdf", "filename": "order.pdf",
                  "body": { "attachmentId": "att-1" } },
                { "mimeType": "application/octet-stream", "filename": "",
                  "headers": [{ "name": "Content-Disposition", "value": "attachment" }],
                  "body": { "attachmentId": "att-2" } }
            ]
        }))
        .unwrap();
        let mut names = Vec::new();
        attachments(&payload, &mut names);
        assert_eq!(names, vec!["order.pdf", "(unnamed attachment)"]);
    }

    #[test]
    fn gmail_body_parts_decode_whether_or_not_they_carry_padding() {
        let padded = base64::engine::general_purpose::URL_SAFE.encode(b"Hello there");
        assert!(padded.ends_with('='));
        let part: MessagePart = serde_json::from_value(json!({
            "mimeType": "text/plain",
            "body": { "data": padded }
        }))
        .unwrap();
        assert_eq!(decoded_body(&part).as_deref(), Some("Hello there"));
    }

    #[test]
    fn saved_mail_names_carry_the_send_date_and_subject_slug() {
        assert_eq!(
            conventional_mail_name("2026.10.30", "Retainer questions: phase 2!"),
            "2026.10.30_Retainer_questions_phase_2.eml"
        );
        assert_eq!(
            conventional_mail_name("2026.10.30", "   "),
            "2026.10.30_no-subject.eml"
        );
        assert_eq!(
            conventional_mail_name("2026.10.30", "Fwd: the signed order"),
            "2026.10.30_Fwd_the_signed_order.eml"
        );
    }

    #[test]
    fn save_names_must_carry_the_send_date_prefix_and_eml_extension() {
        assert!(check_mail_save_name("2026.10.30_Retainer questions.eml").is_ok());
        assert!(check_mail_save_name("2026.10.30_.eml").is_ok());
        assert!(check_mail_save_name("mail.eml").is_err());
        assert!(check_mail_save_name("2026-10-30_dashes.eml").is_err());
        assert!(check_mail_save_name("2026.10.30_wrong.pdf").is_err());
        assert!(check_mail_save_name("2026.10.30_no_extension").is_err());
    }

    #[test]
    fn saved_mail_never_clobbers_a_changed_file() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("mail").join("2026.10.30_Order.eml");
        assert_eq!(
            write_saved_mail(&path, b"first").unwrap(),
            SavedMail::Written
        );
        assert!(path.exists());
        assert_eq!(
            write_saved_mail(&path, b"first").unwrap(),
            SavedMail::AlreadySaved
        );
        let error = write_saved_mail(&path, b"different")
            .unwrap_err()
            .to_string();
        assert!(error.contains("different contents"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
    }

    #[test]
    fn agent_contract_documents_mail_parse_and_save() {
        let contract = include_str!("../../cli/src/projects/agent_contract.md");
        assert!(contract.contains("hudson mail parse"));
        assert!(contract.contains("hudson mail save"));
        assert!(contract.contains("`mail/` is git-ignored"));
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
            }),
            parts: None,
            filename: None,
        };
        let plain_two = MessagePart {
            mime_type: Some("text/plain".into()),
            headers: None,
            body: Some(MessageBody {
                data: Some(encode("Part two")),
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

    /// The archive removes the INBOX label and nothing else — never a
    /// trash delete, never a send, and no other label mutation.
    #[tokio::test]
    async fn the_archive_removes_only_the_inbox_label() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/users/me/messages/18fa3bc2d4e5f6a7/modify"))
            .and(body_partial_json(json!({ "removeLabelIds": ["INBOX"] })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;

        let client =
            GmailClient::with_base_url("test-token".into(), Url::parse(&server.uri()).unwrap());
        client.archive_message("18fa3bc2d4e5f6a7").await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.method.as_str(), "POST");
        assert!(request.url.path().contains("/modify"));
        assert!(!request.url.path().contains("trash"));
        assert!(!request.url.path().contains("delete"));
        let body: serde_json::Value = serde_json::from_slice(request.body.as_slice()).unwrap();
        assert_eq!(body["removeLabelIds"], json!(["INBOX"]));
        assert!(body.get("delete").is_none());
        assert!(body.get("addLabelIds").is_none());
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
            None,
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
                    part.decoded().into_owned(),
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
            None,
            Some("<message-1@example.test>"),
            &attachments,
        )
        .unwrap();

        let message = parsed(&raw);
        assert_eq!(message.body_text(0).unwrap().trim(), "Synthetic body");
        assert_eq!(
            message.in_reply_to().and_then(|ids| ids.last()),
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
            None,
            &[]
        )
        .is_err());
    }

    #[test]
    fn provisioning_doc_anchor_names_a_heading_in_the_mail_doc() {
        let doc = include_str!("../../docs/mail-cli.md");
        let (path, anchor) = PROVISIONING_DOC.split_once('#').unwrap();
        assert_eq!(path, "docs/mail-cli.md");
        assert_eq!(anchor, "provision-gmail-for-hudson-mail");
        assert!(doc.contains("\n## Provision Gmail for hudson mail\n"));
    }

    #[test]
    fn setup_errors_name_every_missing_setting_at_once() {
        let error = setup_from(None, Some(" ".into())).unwrap_err().to_string();
        assert!(
            error.contains("NAVIGATOR_GMAIL_ACCOUNT")
                && error.contains("NAVIGATOR_GMAIL_CLIENT_ID")
        );
        assert!(
            error.contains("docs/mail-cli.md#provision-gmail-for-hudson-mail"),
            "the error must name the provisioning doc: {error}"
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
                    configured_signature: Some(CONFIGURED_SIGNATURE.into()),
                    to: vec!["client@example.test".into()],
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject: Some("Large".into()),
                    body_file: body_file.clone(),
                    html: false,
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
                configured_signature: Some(CONFIGURED_SIGNATURE.into()),
                to: vec!["client@example.test".into()],
                cc: vec!["copy@example.test".into()],
                bcc: vec!["blind@example.test".into()],
                subject: Some("Letter".into()),
                body_file: body_file.clone(),
                html: false,
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

        std::fs::write(
            &body_file,
            format!("Revised body\n\n-- \n{CONFIGURED_SIGNATURE}\n\n-- \n{CONFIGURED_SIGNATURE}"),
        )
        .unwrap();
        let updated = save_draft_command(
            DraftRequest {
                configured_signature: Some(CONFIGURED_SIGNATURE.into()),
                to: Vec::new(),
                cc: vec!["copy@example.test".into()],
                bcc: vec!["blind@example.test".into()],
                subject: None,
                body_file: body_file.clone(),
                html: false,
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
                configured_signature: Some(CONFIGURED_SIGNATURE.into()),
                to: Vec::new(),
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: None,
                body_file,
                html: false,
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
        assert!(raw.contains("Cc: copy@example.test\r\n"));
        assert!(raw.contains("Bcc: blind@example.test\r\n"));
        let text = parsed(&raw).body_text(0).unwrap().replace("\r\n", "\n");
        assert_eq!(text.matches(CONFIGURED_SIGNATURE).count(), 1);
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
        assert!(raw.contains("Cc: copy@example.test\r\n"));
        assert!(raw.contains("Bcc: blind@example.test\r\n"));
        assert_eq!(body["message"]["threadId"], "thread-1");
        let message = parsed(&raw);
        let text = message.body_text(0).unwrap().replace("\r\n", "\n");
        assert!(text.starts_with("Revised body"));
        assert_eq!(text.matches(CONFIGURED_SIGNATURE).count(), 1);
        assert_eq!(message.subject(), Some("Re: A question"));
        assert_eq!(
            message.in_reply_to().and_then(|ids| ids.last()),
            Some("message-1@example.test")
        );
        assert_eq!(
            message.references().and_then(|ids| ids.last()),
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
            message.in_reply_to().and_then(|ids| ids.last()),
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

    #[test]
    fn setup_saves_settings_and_loads_them_without_environment() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mail.json");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&path, "old settings").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        store_setup(&path, "lawyer@example.test", "test-client-id").unwrap();
        let stored = load_setup(&path).unwrap();
        assert_eq!(
            resolve_setup(None, None, stored).unwrap(),
            ("lawyer@example.test".into(), "test-client-id".into())
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn setup_missing_file_is_unconfigured_and_malformed_file_is_reported() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mail.json");
        assert!(resolve_setup(None, None, load_setup(&path).unwrap()).is_err());
        std::fs::write(&path, "invalid private setting").unwrap();
        let error = load_setup(&path).err().unwrap().to_string();
        assert!(error.contains("Gmail setup"));
        assert!(!error.contains("invalid private setting"));
    }

    #[test]
    fn gcp_setup_survives_account_updates_and_old_settings_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mail.json");
        let settings = StoredSetup {
            gcp_project: Some("example-mail".into()),
            gcp_secret: Some("shared-client".into()),
            ..Default::default()
        };
        write_setup(&path, &settings).unwrap();
        store_setup(&path, "lawyer@example.test", "test-client").unwrap();
        let stored = load_setup(&path).unwrap();
        assert_eq!(stored.gcp_project.as_deref(), Some("example-mail"));
        assert_eq!(stored.gcp_secret.as_deref(), Some("shared-client"));
        std::fs::write(
            &path,
            r#"{"account":"lawyer@example.test","client_id":"test-client"}"#,
        )
        .unwrap();
        assert!(load_setup(&path).unwrap().gcp_project.is_none());
    }

    #[test]
    fn mail_cli_parses_gcp_settings_and_refuses_competing_sources() {
        use clap::Parser;
        #[derive(Parser)]
        struct Mail {
            #[command(subcommand)]
            action: MailAction,
        }
        let parsed = Mail::try_parse_from([
            "mail",
            "login",
            "--client-secret-from-gcp",
            "--project",
            "example-mail",
            "--secret",
            "shared-client",
        ])
        .unwrap();
        assert!(matches!(
            parsed.action,
            MailAction::Login {
                client_secret_from_gcp: true,
                project: Some(_),
                secret: Some(_),
                ..
            }
        ));
        for flag in ["--client-secret-from-gcp", "--project", "--secret"] {
            let mut args = vec!["mail", "login", "--client-secret-file", "client.json", flag];
            if flag != "--client-secret-from-gcp" {
                args.push("example-mail");
            }
            assert!(Mail::try_parse_from(args).is_err());
        }
        assert!(Mail::try_parse_from([
            "mail",
            "setup",
            "--project",
            "example-mail",
            "--secret",
            "shared-client"
        ])
        .is_ok());
    }

    #[test]
    fn setup_flags_require_both_settings() {
        use clap::Parser;
        #[derive(Parser)]
        struct Mail {
            #[command(subcommand)]
            action: MailAction,
        }
        assert!(
            Mail::try_parse_from(["mail", "setup", "--account", "lawyer@example.test"]).is_err()
        );
        assert!(Mail::try_parse_from(["mail", "setup", "--client-id", "test-client-id"]).is_err());
        assert!(Mail::try_parse_from([
            "mail",
            "setup",
            "--account",
            "lawyer@example.test",
            "--client-id",
            "test-client-id"
        ])
        .is_ok());
        assert!(Mail::try_parse_from(["mail", "setup"]).is_ok());
    }

    #[test]
    fn setup_environment_overrides_stored_values() {
        let stored = StoredSetup {
            account: Some("stored@example.test".into()),
            client_id: Some("stored-client".into()),
            ..StoredSetup::default()
        };
        assert_eq!(
            resolve_setup(Some("override@example.test".into()), None, stored).unwrap(),
            ("override@example.test".into(), "stored-client".into())
        );
    }

    #[test]
    fn setup_blank_environment_values_fall_back_to_saved_values() {
        let stored = StoredSetup {
            account: Some("stored@example.test".into()),
            client_id: Some("stored-client".into()),
            ..StoredSetup::default()
        };
        assert_eq!(
            resolve_setup(Some(" ".into()), Some(String::new()), stored).unwrap(),
            ("stored@example.test".into(), "stored-client".into())
        );
    }

    #[test]
    fn setup_rejects_blank_settings_without_overwriting_existing_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mail.json");
        store_setup(&path, "lawyer@example.test", "test-client-id").unwrap();
        assert!(store_setup(&path, " ", "client").is_err());
        assert_eq!(
            load_setup(&path).unwrap().account.as_deref(),
            Some("lawyer@example.test")
        );
    }

    #[test]
    fn setup_report_names_each_gap_and_never_claims_send_access() {
        let report = setup_report(false, false, false, KeychainState::AccountUnknown);
        assert!(!report.ready);
        for expected in [
            "NAVIGATOR_GMAIL_ACCOUNT",
            "NAVIGATOR_GMAIL_CLIENT_ID",
            "com.neonlaw.navigator.gmail.refresh-token",
            "com.neonlaw.navigator.gmail.client-secret",
            "gmail.readonly",
            "gmail.compose",
            "never sends",
        ] {
            assert!(report.text.contains(expected), "missing `{expected}`");
        }
        assert!(!report.text.contains("security add-generic-password"));
        assert!(report.text.contains(
            "No OAuth client id yet? The Firm's operator creates one Desktop-app client per Firm; \
see docs/mail-cli.md#provision-gmail-for-hudson-mail."
        ));

        let missing = setup_report(true, true, false, KeychainState::Missing);
        assert!(!missing.ready);
        assert!(missing.text.contains("hudson mail login"));
        assert!(missing.text.contains(
            "Sign-in steps for a new machine: docs/mail-cli.md#provision-gmail-for-hudson-mail."
        ));
        assert!(!missing.text.contains("No OAuth client id yet"));
        assert!(missing.text.contains("[optional] OAuth client secret"));
        assert!(!missing.text.contains("security add-generic-password"));

        // The client secret is optional: its absence never blocks readiness.
        let ready = setup_report(true, true, false, KeychainState::Present);
        assert!(ready.ready);
        assert!(!ready.text.contains("  hudson mail login\n"));

        let with_secret = setup_report(true, true, true, KeychainState::Present);
        assert!(with_secret.ready);
        assert!(with_secret.text.contains("[ok      ] OAuth client secret"));
    }

    #[test]
    fn refresh_sends_the_client_secret_only_when_one_is_stored() {
        let with = refresh_form("client", Some("secret"), "refresh");
        assert_eq!(
            with,
            [
                ("client_id", "client"),
                ("client_secret", "secret"),
                ("refresh_token", "refresh"),
                ("grant_type", "refresh_token"),
            ]
        );
        let without = refresh_form("client", None, "refresh");
        assert!(without.iter().all(|(name, _)| *name != "client_secret"));
        assert_eq!(without.len(), 3);
    }

    #[test]
    fn setup_command_prints_only_the_fixed_report() {
        let source = include_str!("mail.rs");
        let body = source.split("fn setup_command(").nth(1).unwrap();
        let body = body.split("async fn client()").next().unwrap();
        assert!(body.contains("print!(\"{}\", report.text)"));
        assert!(!body.contains("eprintln!") && !body.contains("println!(\"{account"));
    }

    fn message_with_body(mime_type: &str, body_data: &str) -> GmailMessage {
        GmailMessage {
            id: "message-1".into(),
            internal_date: None,
            payload: Some(MessagePart {
                mime_type: Some(mime_type.into()),
                headers: Some(vec![MessageHeader {
                    name: "From".into(),
                    value: "Client <client@example.test>".into(),
                }]),
                body: Some(MessageBody {
                    data: Some(body_data.into()),
                }),
                parts: None,
                filename: None,
            }),
        }
    }

    fn encoded(data: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
    }

    #[test]
    fn thread_printing_survives_missing_bodies_and_names_attachments() {
        let text = message_with_body("text/plain", &encoded(b"Can I sign electronically?"));
        let html_only = message_with_body("text/html", &encoded(b"<p>Confirmed</p>"));
        let no_payload = GmailMessage {
            id: "message-3".into(),
            internal_date: None,
            payload: None,
        };
        let with_attachment = GmailMessage {
            id: "message-4".into(),
            internal_date: None,
            payload: Some(MessagePart {
                mime_type: Some("multipart/mixed".into()),
                headers: Some(vec![MessageHeader {
                    name: "Subject".into(),
                    value: "Signed retainer".into(),
                }]),
                body: None,
                parts: Some(vec![MessagePart {
                    mime_type: Some("application/pdf".into()),
                    headers: None,
                    body: Some(MessageBody { data: None }),
                    parts: None,
                    filename: Some("contract.pdf".into()),
                }]),
                filename: None,
            }),
        };
        print_thread(&GmailThread {
            id: "thread-1".into(),
            messages: vec![text, html_only, no_payload, with_attachment],
        });
    }

    #[test]
    fn the_thread_timestamp_comes_from_gmails_internal_date() {
        let mut message = synthetic_message(&[("From", "client@example.test")]);
        message.internal_date = Some("1760000000000".into());
        let at = now_or_message_date(&message).unwrap();
        assert!(at.starts_with("2025-10-09T"), "{at}");

        message.internal_date = None;
        assert!(now_or_message_date(&message)
            .unwrap_err()
            .to_string()
            .contains("no timestamp"));

        message.internal_date = Some("not-a-number".into());
        assert!(now_or_message_date(&message).is_err());

        message.internal_date = Some("99999999999999999999".into());
        assert!(now_or_message_date(&message).is_err());
    }

    #[test]
    fn attachment_mime_types_follow_the_extension_caselessly() {
        assert_eq!(mime_type_for(Path::new("report.PDF")), "application/pdf");
        assert_eq!(mime_type_for(Path::new("notes.txt")), "text/plain");
        assert_eq!(mime_type_for(Path::new("calendar.ICS")), "text/calendar");
        assert_eq!(mime_type_for(Path::new("photo.heic")), "image/heic");
        assert_eq!(
            mime_type_for(Path::new("archive.unknownext")),
            "application/octet-stream"
        );
        assert_eq!(
            mime_type_for(Path::new("noextension")),
            "application/octet-stream"
        );
    }

    #[test]
    fn encoded_attachment_size_is_base64_plus_line_breaks() {
        assert_eq!(base64_encoded_len(0), 0);
        assert_eq!(base64_encoded_len(1), 6);
        assert_eq!(base64_encoded_len(3), 6);
        assert_eq!(base64_encoded_len(57), 78);
    }

    #[test]
    fn attachment_names_must_be_utf8_and_printable() {
        assert_eq!(
            attachment_filename(Path::new("signed retainer.pdf")).unwrap(),
            "signed retainer.pdf"
        );

        let control = std::env::temp_dir().join("bad\u{0007}name.pdf");
        assert!(attachment_filename(&control).is_err());

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let non_utf8 = Path::new(std::ffi::OsStr::from_bytes(b"/tmp/attachment-\xff.pdf"));
            assert!(attachment_filename(non_utf8).is_err());
        }
    }

    #[test]
    fn attachments_are_read_whole_and_refused_over_gmails_limit() {
        let directory = tempfile::tempdir().unwrap();

        let Err(missing) = load_attachments(&[directory.path().join("absent.pdf")], "body") else {
            panic!("a missing attachment must be refused");
        };
        assert!(missing.to_string().contains("absent.pdf"));

        let Err(directory_attachment) = load_attachments(&[directory.path().to_path_buf()], "body")
        else {
            panic!("a directory must not load as an attachment");
        };
        assert!(directory_attachment.to_string().contains("is not a file"));

        let oversized = directory.path().join("huge.bin");
        let file = std::fs::File::create(&oversized).unwrap();
        file.set_len(30_000_000).unwrap();
        let Err(error) = load_attachments(&[oversized], "body") else {
            panic!("a 30MB attachment must be refused");
        };
        assert!(error.to_string().contains("once encoded, over Gmail's"));

        let first = directory.path().join("first.bin");
        let second = directory.path().join("second.bin");
        std::fs::File::create(&first)
            .unwrap()
            .set_len(13_000_000)
            .unwrap();
        std::fs::File::create(&second)
            .unwrap()
            .set_len(13_000_000)
            .unwrap();
        let Err(total) = load_attachments(&[first, second], "body") else {
            panic!("two 13MB attachments must be refused together");
        };
        assert!(total.to_string().contains("the draft with 2 attachments"));

        let small = directory.path().join("exhibit.txt");
        std::fs::write(&small, b"exhibit text").unwrap();
        let loaded = load_attachments(std::slice::from_ref(&small), "body").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].filename, "exhibit.txt");
        assert_eq!(loaded[0].mime_type, "text/plain");
        assert_eq!(loaded[0].bytes, b"exhibit text");
    }

    #[test]
    fn encoded_words_and_filename_parameters_stay_rfc_compliant() {
        let encoded_subject = encoded_word("Retainer — questions");
        let (marker, remainder) = encoded_subject.split_once("?B?").unwrap();
        assert_eq!(marker, "=?UTF-8");
        let payload = remainder.strip_suffix("?=").unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "Retainer — questions");

        let (name, filename) = filename_parameters("signed \"final\" copy.pdf");
        assert_eq!(name, "name=\"signed \\\"final\\\" copy.pdf\"");
        assert_eq!(filename, "filename=\"signed \\\"final\\\" copy.pdf\"");

        let (name, filename) = filename_parameters("café.txt");
        assert!(name.starts_with("name=\"=?UTF-8?B?"), "{name}");
        assert_eq!(filename, "filename*=UTF-8''caf%C3%A9.txt");
    }

    #[test]
    fn header_values_refuse_line_breaks_and_blank_recipients_are_dropped() {
        assert!(safe_header("Subject\r\nBcc: x", "subject").is_err());
        assert_eq!(safe_header("  padded  ", "subject").unwrap(), "padded");

        let joined = address_list(
            &["a@x.test".into(), "   ".into(), " b@y.test ".into()],
            "recipient",
        )
        .unwrap();
        assert_eq!(joined, "a@x.test, b@y.test");
        assert!(address_list(&["bad\nvalue".into()], "recipient").is_err());
    }

    #[test]
    fn addresses_are_extracted_from_display_names_and_lowercased() {
        let collected: Vec<String> =
            addresses("Ms. Client <Client@Example.test>; other@y.test, junk").collect();
        assert_eq!(collected, vec!["client@example.test", "other@y.test"]);

        assert_eq!(
            email_from_address("Lawyer <lawyer@neonlaw.test>").as_deref(),
            Some("lawyer@neonlaw.test")
        );
        assert_eq!(email_from_address("not-an-address"), None);
    }

    #[test]
    fn a_scoped_query_only_scopes_when_the_manifest_names_a_contact() {
        assert_eq!(
            scoped_query("retainer", &SearchScope::default()),
            "retainer"
        );
        let client_only = SearchScope {
            contact: Some("client@example.test".into()),
            dockets: vec![],
        };
        assert_eq!(
            scoped_query("retainer", &client_only),
            "(retainer) (from:client@example.test OR to:client@example.test)"
        );
    }

    #[test]
    fn a_scoped_query_reaches_court_notices_naming_the_projects_case_numbers() {
        let scope = SearchScope {
            contact: Some("client@example.test".into()),
            dockets: vec!["2:26-bk-10001".into(), "A-26-900001-C".into()],
        };
        assert_eq!(
            scoped_query("from:uscourts.gov", &scope),
            "(from:uscourts.gov) (from:client@example.test OR to:client@example.test \
             OR \"2:26-bk-10001\" OR \"A-26-900001-C\")"
        );
        // Dockets alone still scope, and a stray quote cannot break the query.
        let dockets_only = SearchScope {
            contact: None,
            dockets: vec!["26-\"10002\"".into(), "  ".into()],
        };
        assert_eq!(
            scoped_query("notice", &dockets_only),
            "(notice) (\"26-10002\")"
        );
    }

    #[test]
    fn mail_search_accepts_an_unscoped_flag() {
        use clap::Parser as _;
        #[derive(clap::Parser)]
        struct Harness {
            #[command(subcommand)]
            action: MailAction,
        }
        let parsed = Harness::try_parse_from(["mail", "search", "--unscoped", "from:ecf"]).unwrap();
        assert!(matches!(
            parsed.action,
            MailAction::Search { unscoped: true, ref query } if query == "from:ecf"
        ));
    }

    #[tokio::test]
    async fn an_embedded_signature_requires_review_before_updating_a_draft() {
        for body in [
            format!("{CONFIGURED_SIGNATURE}\n\n> Quoted history"),
            format!("Please review.\n\n{CONFIGURED_SIGNATURE}\n\n> Quoted history"),
            format!(
                "Please review.\n\n-- \n{CONFIGURED_SIGNATURE}\n\nFollowing text.\n\n-- \n{CONFIGURED_SIGNATURE}"
            ),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let body_file = write_file(directory.path(), "body.txt", body.as_bytes());
            let mut request =
                draft_request(body_file, &["client@example.test"], Some("Review"), None);
            request.draft_id = Some("existing-draft".into());
            let connected = std::cell::Cell::new(false);
            let error = save_draft_command(request, || async {
                connected.set(true);
                Err(anyhow!("provider must remain untouched"))
            })
            .await
            .err()
            .unwrap();
            assert!(
                !connected.get(),
                "ambiguous signatures must refuse before reading or updating the existing draft"
            );
            let message = error.to_string();
            assert!(message.contains("signature needs review"), "{message}");
            assert!(!message.contains("NAVIGATOR_GMAIL_SIGNATURE"), "{message}");
            assert!(!message.contains("Example Attorney"), "{message}");
            assert!(!message.contains("Confidential:"), "{message}");
        }
    }

    #[tokio::test]
    async fn a_draft_without_a_configured_account_falls_back_to_the_env_signature() {
        let resolved = resolve_draft_signature(Some(CONFIGURED_SIGNATURE), None).await;
        let ResolvedSignature::Env {
            signature,
            announcement,
        } = resolved
        else {
            panic!("an account resolution failure must fall back to the env signature");
        };
        assert_eq!(signature.body, CONFIGURED_SIGNATURE);
        assert_eq!(
            signature.provenance,
            "signature: source=env person=n/a revision=n/a"
        );
        assert!(
            announcement.contains("NAVIGATOR_GMAIL_SIGNATURE"),
            "{announcement}"
        );
    }

    #[tokio::test]
    async fn a_blank_env_signature_is_not_a_usable_fallback_block() {
        let resolved = resolve_draft_signature(Some("   "), None).await;
        assert!(
            matches!(resolved, ResolvedSignature::Neither(_)),
            "a blank env value must leave the draft to the Gmail account's signature"
        );
    }

    #[tokio::test]
    async fn neither_resolution_source_yields_no_signature_body() {
        let resolved = resolve_draft_signature(None, None).await;
        assert!(
            resolved.signature_body().is_none(),
            "neither source must leave the draft to the Gmail account's signature"
        );
    }

    /// A Gmail stub for the LAW-316 drafts: the profile, the account's
    /// send-as signature, a one-message thread, and the draft writes.
    async fn signature_stub(account_signature: &str) -> wiremock::MockServer {
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
            .and(path("/users/me/settings/sendAs/lawyer@example.test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "sendAsEmail": "lawyer@example.test",
                "signature": account_signature,
            })))
            .mount(&server)
            .await;
        let earlier = GMAIL_BASE64URL.encode("Can you send the engagement letter?\r\nThank you.");
        Mock::given(method("GET"))
            .and(path("/users/me/threads/thread-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id":"thread-1",
                "messages":[{"id":"message-1","payload":{
                    "mimeType":"text/plain",
                    "headers":[
                        {"name":"From","value":"Client <client@example.test>"},
                        {"name":"Date","value":"Mon, 9 Nov 2026 09:00:00 -0800"},
                        {"name":"Subject","value":"A question"},
                        {"name":"Message-ID","value":"<message-1@example.test>"}
                    ],
                    "body":{"data": earlier}
                }}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/users/me/drafts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"draft-1"})))
            .mount(&server)
            .await;
        server
    }

    fn unconfigured_request(body_file: PathBuf, html: bool, thread: Option<&str>) -> DraftRequest {
        DraftRequest {
            configured_signature: None,
            html,
            ..draft_request(
                body_file,
                &["client@example.test"],
                Some("Engagement letter"),
                thread,
            )
        }
    }

    #[tokio::test]
    async fn an_unconfigured_draft_signs_with_the_gmail_account_signature() {
        let server = signature_stub("<div>Example Attorney<br>Confidential notice</div>").await;
        let dir = tempfile::tempdir().unwrap();
        let body_file = write_file(dir.path(), "body.txt", b"Please review.");
        let connect = || async {
            Ok(GmailClient::with_base_url(
                "test-token".into(),
                Url::parse(&server.uri()).unwrap(),
            ))
        };
        save_draft_command(unconfigured_request(body_file, false, None), connect)
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        let draft = requests
            .iter()
            .find(|request| request.method.as_str() == "POST")
            .expect("one draft write");
        assert_eq!(draft.url.path(), "/users/me/drafts");
        let (_, raw) = decoded_draft(draft);
        let text = parsed(&raw).body_text(0).unwrap().replace("\r\n", "\n");
        assert!(
            text.starts_with("Please review.\n\n-- \nExample Attorney\nConfidential notice"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_blank_gmail_account_signature_refuses_without_a_draft_write() {
        let server = signature_stub("").await;
        let dir = tempfile::tempdir().unwrap();
        let body_file = write_file(dir.path(), "body.txt", b"Please review.");
        let connect = || async {
            Ok(GmailClient::with_base_url(
                "test-token".into(),
                Url::parse(&server.uri()).unwrap(),
            ))
        };
        let error = save_draft_command(unconfigured_request(body_file, false, None), connect)
            .await
            .err()
            .expect("no signature anywhere must refuse");
        let message = error.to_string();
        for source in [
            "stored signature row",
            "NAVIGATOR_GMAIL_SIGNATURE",
            "Gmail account's default signature",
        ] {
            assert!(message.contains(source), "{message}");
        }
        let requests = server.received_requests().await.unwrap();
        assert!(requests
            .iter()
            .all(|request| request.method.as_str() == "GET"));
    }

    #[tokio::test]
    async fn an_html_reply_carries_both_parts_the_signature_and_quoted_history() {
        let server = signature_stub("Example Attorney").await;
        let dir = tempfile::tempdir().unwrap();
        let body_file = write_file(
            dir.path(),
            "body.html",
            b"<p><b>Engagement</b></p><p>The letter is attached.</p>",
        );
        let connect = || async {
            Ok(GmailClient::with_base_url(
                "test-token".into(),
                Url::parse(&server.uri()).unwrap(),
            ))
        };
        save_draft_command(
            unconfigured_request(body_file, true, Some("thread-1")),
            connect,
        )
        .await
        .unwrap();

        let requests = server.received_requests().await.unwrap();
        let draft = requests
            .iter()
            .find(|request| request.method.as_str() == "POST")
            .expect("one draft write");
        let (body, raw) = decoded_draft(draft);
        assert_eq!(body["message"]["threadId"], "thread-1");
        let message = parsed(&raw);
        let text = message.body_text(0).unwrap().replace("\r\n", "\n");
        assert!(text.contains("Engagement"), "{text}");
        assert!(!text.contains("<b>"), "{text}");
        assert!(
            text.contains(
                "-- \nExample Attorney\n\nOn Mon, 9 Nov 2026 09:00:00 -0800, \
                 Client <client@example.test> wrote:\n\
                 > Can you send the engagement letter?\n> Thank you."
            ),
            "{text}"
        );
        let html = message.body_html(0).unwrap();
        assert!(html.contains("<b>Engagement</b>"), "{html}");
        assert!(html.contains("-- <br>Example Attorney"), "{html}");
        assert!(
            html.contains("<blockquote") && html.contains("Can you send the engagement letter?"),
            "{html}"
        );
    }

    #[test]
    fn draft_takes_exactly_one_of_a_plain_or_html_body_file() {
        use clap::Parser;

        #[derive(Parser)]
        struct Mail {
            #[command(subcommand)]
            action: MailAction,
        }

        let parsed =
            Mail::try_parse_from(["mail", "draft", "--html-body-file", "body.html"]).unwrap();
        let MailAction::Draft {
            body_file,
            html_body_file,
            ..
        } = parsed.action
        else {
            panic!("draft subcommand");
        };
        assert_eq!(
            (body_file, html_body_file),
            (None, Some(PathBuf::from("body.html")))
        );
        for args in [
            vec!["mail", "draft"],
            vec![
                "mail",
                "draft",
                "--body-file",
                "body.txt",
                "--html-body-file",
                "body.html",
            ],
        ] {
            assert!(Mail::try_parse_from(args).is_err());
        }
    }

    #[tokio::test]
    async fn an_empty_or_missing_draft_body_is_refused_before_connecting() {
        let directory = tempfile::tempdir().unwrap();

        let missing = save_draft_command(
            DraftRequest {
                configured_signature: Some(CONFIGURED_SIGNATURE.into()),
                to: vec!["a@x.test".into()],
                cc: vec![],
                bcc: vec![],
                subject: Some("s".into()),
                body_file: directory.path().join("absent.txt"),
                html: false,
                thread: None,
                draft_id: None,
                attach: vec![],
            },
            || async { Err(anyhow!("must not connect")) },
        )
        .await;
        let Err(missing) = missing else {
            panic!("a missing draft body must be refused");
        };
        assert!(missing.to_string().contains("read draft body"));

        let body_file = directory.path().join("body.txt");
        std::fs::write(&body_file, "   \n").unwrap();
        let empty = save_draft_command(
            DraftRequest {
                configured_signature: Some(CONFIGURED_SIGNATURE.into()),
                to: vec!["a@x.test".into()],
                cc: vec![],
                bcc: vec![],
                subject: Some("s".into()),
                body_file,
                html: false,
                thread: None,
                draft_id: None,
                attach: vec![],
            },
            || async { Err(anyhow!("must not connect")) },
        )
        .await;
        let Err(empty) = empty else {
            panic!("an empty draft body must be refused");
        };
        assert!(empty.to_string().contains("draft body is empty"));
    }

    #[tokio::test]
    async fn parse_save_and_file_references_are_named_when_unusable() {
        let error = parse_command("definitely-absent-reference.txt", Some("p"), None, None)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("neither an existing file, a Gmail web URL, nor a Gmail message id"),
            "{error}"
        );

        let error = save_message_command("definitely-absent-reference.txt", None, None)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("neither a Gmail web URL nor a Gmail message id"),
            "{error}"
        );

        let error = file_thread("thread-1", None, None, None).await.unwrap_err();
        assert!(error.to_string().contains("pass --project"), "{error}");
    }

    #[tokio::test]
    async fn filing_a_communication_to_a_dead_endpoint_names_the_failure() {
        let target = FileTarget {
            base: "http://127.0.0.1:1",
            navigator_token: "token",
            project_id: uuid::Uuid::nil(),
            mailbox: "lawyer@neonlaw.test",
        };
        let error = post_communication(&target, &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("file email Communication"),
            "{error}"
        );
    }

    #[test]
    fn finish_maps_result_to_exit_code() {
        assert_eq!(finish(Ok(())), ExitCode::SUCCESS);
        assert_eq!(finish(Err(anyhow!("boom"))), ExitCode::FAILURE);
    }

    #[tokio::test]
    async fn a_filed_message_posts_one_communication_with_its_time_and_source() {
        use wiremock::matchers::{header as header_is, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{project_id}/communications"
            )))
            .and(header_is("authorization", "Bearer navigator-token"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&server)
            .await;
        let base = server.uri();
        let target = FileTarget {
            base: &base,
            navigator_token: "navigator-token",
            project_id,
            mailbox: "Lawyer@Example.test",
        };
        let mut message = synthetic_message(&[
            ("From", "Client <client@example.test>"),
            ("To", "lawyer@example.test"),
            ("Subject", "Signing question"),
        ]);
        message.internal_date = Some("1760000000000".into());
        file_message(&target, &message, "Can I sign electronically?")
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["direction"], "inbound");
        assert_eq!(body["channel"], "email_inbound");
        assert_eq!(body["counterparty"], "Client <client@example.test>");
        assert_eq!(body["subject"], "Signing question");
        assert_eq!(body["body"], "Can I sign electronically?");
        assert_eq!(body["source_ref"], "gmail:lawyer@example.test:message-1");
        assert!(body["at"].as_str().unwrap().starts_with("2025-10-09T"));

        // A message without a timestamp is refused before anything is posted.
        message.internal_date = None;
        assert!(file_message(&target, &message, "body").await.is_err());
        server.verify().await;
    }

    #[tokio::test]
    async fn a_refused_communication_names_the_status_only() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(403).set_body_string("secret echo"))
            .mount(&server)
            .await;
        let base = server.uri();
        let target = FileTarget {
            base: &base,
            navigator_token: "navigator-token",
            project_id: Uuid::nil(),
            mailbox: "lawyer@example.test",
        };
        let error = post_communication(&target, &json!({}))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("refused: 403"), "{error}");
        assert!(!error.contains("secret echo"), "{error}");
    }

    #[tokio::test]
    async fn an_eml_refusal_names_the_doors_reason_and_the_kind_is_sent() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/app/api/projects/{}/mail/eml", Uuid::nil())))
            .and(body_partial_json(json!({ "kind": "letter" })))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "kind_required",
                "message": "A document kind is required when the message carries attachments.",
            })))
            .expect(1)
            .mount(&server)
            .await;
        let error = post_eml(
            &server.uri(),
            "navigator-token",
            Uuid::nil(),
            b"From: lawyer@example.test\r\n\r\nbody\r\n",
            "letter",
            None,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("refused: 400 Bad Request"), "{error}");
        assert!(error.contains("kind_required"), "{error}");
        assert!(
            error.contains("A document kind is required"),
            "the door's message reaches the operator: {error}"
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn mail_refusals_suppress_structured_private_values() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        for code in ["kind_required", "SYNTHETIC_PRIVATE_CODE"] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                    "error": code,
                    "message": "SYNTHETIC_PRIVATE_MESSAGE",
                })))
                .mount(&server)
                .await;
            let response = reqwest::Client::new()
                .post(server.uri())
                .send()
                .await
                .unwrap();
            let error = refused("file the .eml", response).await.to_string();
            assert!(!error.contains("SYNTHETIC_PRIVATE"), "{error}");
            if code == "kind_required" {
                assert!(error.contains("kind_required"), "{error}");
                assert!(error.contains("--kind"), "{error}");
            } else {
                assert_eq!(error, "file the .eml refused: 400 Bad Request");
            }
        }
    }

    #[tokio::test]
    async fn mail_refusals_reject_oversized_bodies() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "kind_required",
                "message": "x".repeat(8192),
            })))
            .mount(&server)
            .await;
        let response = reqwest::Client::new()
            .post(server.uri())
            .send()
            .await
            .unwrap();
        let error = refused("file the .eml", response).await.to_string();
        assert_eq!(error, "file the .eml refused: 400 Bad Request");
    }

    #[tokio::test]
    async fn gmail_message_reads_and_errors_never_echo_provider_bodies() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/users/me/messages/18fa3bc2d4e5f6a7"))
            .and(query_param("format", "full"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "18fa3bc2d4e5f6a7",
                "internalDate": "1760000000000",
                "payload": {"mimeType": "text/plain", "body": {"data": encoded(b"Hello")}}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/users/me/messages/18fa3bc2d4e5f6a7"))
            .and(query_param("format", "raw"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"raw": encoded(b"Subject: x\r\n\r\nraw")})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/users/me/messages/denied000001"))
            .respond_with(ResponseTemplate::new(403).set_body_string("token=leaked"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/users/me/messages/garbled00001"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;
        let client =
            GmailClient::with_base_url("test-token".into(), Url::parse(&server.uri()).unwrap());
        let message = client.message("18fa3bc2d4e5f6a7").await.unwrap();
        assert_eq!(message.internal_date.as_deref(), Some("1760000000000"));
        assert_eq!(
            plain_text(message.payload.as_ref().unwrap()).as_deref(),
            Some("Hello")
        );
        let raw = client.raw_message("18fa3bc2d4e5f6a7").await.unwrap();
        assert_eq!(
            decode_gmail_raw(raw.raw.as_deref().unwrap()).unwrap(),
            b"Subject: x\r\n\r\nraw"
        );
        let denied = client.message("denied000001").await.unwrap_err();
        assert_eq!(
            denied.to_string(),
            "Gmail API request failed: 403 Forbidden"
        );
        assert!(!format!("{denied:#}").contains("leaked"));
        let garbled = client.message("garbled00001").await.unwrap_err();
        assert!(
            garbled.to_string().contains("parse Gmail API response"),
            "{garbled}"
        );

        let production = GmailClient::new("token".into()).unwrap();
        assert_eq!(
            production
                .url(Operation::ModifyMessage, Some("18fa3bc2d4e5f6a7"))
                .unwrap()
                .as_str(),
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/18fa3bc2d4e5f6a7/modify"
        );
    }

    async fn draft_server(thread: serde_json::Value) -> wiremock::MockServer {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/users/me/profile"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"emailAddress": "lawyer@example.test"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/users/me/threads/thread-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(thread))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/users/me/drafts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "draft-9"})))
            .mount(&server)
            .await;
        server
    }

    fn draft_request(
        body_file: PathBuf,
        to: &[&str],
        subject: Option<&str>,
        thread: Option<&str>,
    ) -> DraftRequest {
        DraftRequest {
            configured_signature: Some(CONFIGURED_SIGNATURE.into()),
            to: to.iter().map(|value| (*value).to_string()).collect(),
            cc: vec![],
            bcc: vec![],
            subject: subject.map(str::to_string),
            body_file,
            html: false,
            thread: thread.map(str::to_string),
            draft_id: None,
            attach: vec![],
        }
    }

    async fn draft_with(
        server: &wiremock::MockServer,
        request: DraftRequest,
    ) -> Result<SavedDraft> {
        let base = Url::parse(&server.uri()).unwrap();
        save_draft_command(request, || async move {
            Ok(GmailClient::with_base_url("test-token".into(), base))
        })
        .await
    }

    #[tokio::test]
    async fn a_new_draft_needs_recipients_and_a_subject_and_saves_unsent() {
        let directory = tempfile::tempdir().unwrap();
        let body = directory.path().join("body.txt");
        std::fs::write(&body, "Synthetic body").unwrap();
        let server = draft_server(json!({"id": "thread-1", "messages": []})).await;

        let error = draft_with(&server, draft_request(body.clone(), &[], Some("Hi"), None))
            .await
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "pass --to or --thread");
        let error = draft_with(
            &server,
            draft_request(body.clone(), &["c@example.test"], None, None),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.to_string(), "pass --subject or --thread");

        let saved = draft_with(
            &server,
            draft_request(body, &["c@example.test"], Some("Hello"), None),
        )
        .await
        .unwrap();
        assert_eq!(saved.id, "draft-9");
        assert_eq!(saved.account, "lawyer@example.test");
        assert_eq!(saved.attachments, 0);
        let requests = server.received_requests().await.unwrap();
        let draft = requests
            .iter()
            .find(|request| request.method.as_str() == "POST")
            .unwrap();
        let (json_body, raw) = decoded_draft(draft);
        assert!(json_body["message"]["threadId"].is_null());
        assert!(raw.starts_with("To: c@example.test\r\n"), "{raw}");
        assert!(raw.contains("Subject: Hello"));
        assert!(!raw.contains("In-Reply-To"));
    }

    #[tokio::test]
    async fn a_reply_draft_defaults_to_the_latest_reply_to_and_subject() {
        let directory = tempfile::tempdir().unwrap();
        let body = directory.path().join("body.txt");
        std::fs::write(&body, "Synthetic reply").unwrap();
        let server = draft_server(json!({
            "id": "thread-1",
            "messages": [
                {"id": "m-1", "payload": {"headers": [
                    {"name": "From", "value": "Old <old@example.test>"},
                    {"name": "Subject", "value": "Older"}
                ]}},
                {"id": "m-2", "payload": {"headers": [
                    {"name": "From", "value": "Client <client@example.test>"},
                    {"name": "Reply-To", "value": "assistant@example.test"},
                    {"name": "Subject", "value": "Retainer"},
                    {"name": "Message-ID", "value": "<m-2@example.test>"}
                ]}}
            ]
        }))
        .await;
        let saved = draft_with(&server, draft_request(body, &[], None, Some("thread-1")))
            .await
            .unwrap();
        assert_eq!(saved.id, "draft-9");
        let requests = server.received_requests().await.unwrap();
        let draft = requests
            .iter()
            .find(|request| request.method.as_str() == "POST")
            .unwrap();
        let (json_body, raw) = decoded_draft(draft);
        assert_eq!(json_body["message"]["threadId"], "thread-1");
        assert!(raw.starts_with("To: assistant@example.test\r\n"), "{raw}");
        assert!(raw.contains("Subject: Re: Retainer"), "{raw}");
        assert!(raw.contains("In-Reply-To: <m-2@example.test>"), "{raw}");
    }

    #[tokio::test]
    async fn a_reply_draft_without_messages_recipient_or_subject_is_refused_unsaved() {
        let directory = tempfile::tempdir().unwrap();
        let body = directory.path().join("body.txt");
        std::fs::write(&body, "Synthetic reply").unwrap();
        let cases = [
            (
                json!({"id": "thread-1", "messages": []}),
                "thread has no messages",
            ),
            (
                json!({"id": "thread-1", "messages": [{"id": "m-1", "payload": {"headers": [
                    {"name": "Subject", "value": "No sender"}
                ]}}]}),
                "reply recipient is unavailable; pass --to",
            ),
            (
                json!({"id": "thread-1", "messages": [{"id": "m-1", "payload": {"headers": [
                    {"name": "From", "value": "client@example.test"}
                ]}}]}),
                "reply subject is unavailable; pass --subject",
            ),
        ];
        for (thread, want) in cases {
            let server = draft_server(thread).await;
            let error = draft_with(
                &server,
                draft_request(body.clone(), &[], None, Some("thread-1")),
            )
            .await
            .err()
            .unwrap();
            assert_eq!(error.to_string(), want);
            assert!(!server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|request| request.method.as_str() == "POST"));
        }
    }

    #[tokio::test]
    async fn local_commands_refuse_unusable_inputs_before_any_network_call() {
        let directory = tempfile::tempdir().unwrap();
        let error = ingest_provider_message(&directory.path().join("absent.eml"), None, None, None)
            .await
            .unwrap_err();
        assert!(error.to_string().starts_with("read "), "{error}");

        let eml = directory.path().join("2026.10.30_Question.eml");
        std::fs::write(&eml, "Subject: x\r\n\r\nbody").unwrap();
        let error = ingest_provider_message(&eml, None, None, None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("pass --project"), "{error}");

        let pdf = directory.path().join("letter.pdf");
        std::fs::write(&pdf, b"%PDF-1.4").unwrap();
        for path in [&eml, &pdf] {
            let error = parse_command(path.to_str().unwrap(), None, None, None)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("pass --project"), "{error}");
        }
        let error = parse_gmail_message("18fa3bc2d4e5f6a7", false, None, None, None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("pass --project"), "{error}");

        let error = archive("no id here").await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot resolve a Gmail message id"),
            "{error}"
        );
        let error = save_message_command("18fa3bc2d4e5f6a7", None, Some("Question.eml"))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("YYYY.MM.DD_<name>.eml"),
            "{error}"
        );
    }

    #[test]
    fn gmail_urls_build_each_operation_path() {
        let client = GmailClient::with_base_url(
            "token".into(),
            Url::parse("https://gmail.googleapis.com/gmail/v1/").unwrap(),
        );
        assert_eq!(
            client.url(Operation::SearchThreads, None).unwrap().as_str(),
            "https://gmail.googleapis.com/gmail/v1/users/me/threads"
        );
        assert_eq!(
            client
                .url(Operation::ReadThread, Some("th-1"))
                .unwrap()
                .as_str(),
            "https://gmail.googleapis.com/gmail/v1/users/me/threads/th-1"
        );
        assert_eq!(
            client
                .url(Operation::ReadMessage, Some("m-1"))
                .unwrap()
                .as_str(),
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/m-1"
        );
        // The modify suffix rides after the id: POST .../messages/{id}/modify.
        assert_eq!(
            client
                .url(Operation::ModifyMessage, Some("m-1"))
                .unwrap()
                .as_str(),
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/m-1/modify"
        );
        assert_eq!(
            client
                .url(Operation::UpdateDraft, Some("d-1"))
                .unwrap()
                .as_str(),
            "https://gmail.googleapis.com/gmail/v1/users/me/drafts/d-1"
        );
    }

    #[tokio::test]
    async fn search_sends_the_bearer_token_and_parses_thread_summaries() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/users/me/threads"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer token-from-test",
            ))
            .and(wiremock::matchers::query_param("q", "from:client"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "threads": [
                        {"id": "th-1", "snippet": "retainer question"},
                        {"id": "th-2"}
                    ]
                })),
            )
            .mount(&server)
            .await;
        let client = GmailClient::with_base_url(
            "token-from-test".into(),
            Url::parse(&server.uri()).unwrap(),
        );
        let threads = client.search("from:client").await.unwrap();
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0].id, "th-1");
        assert_eq!(threads[0].snippet.as_deref(), Some("retainer question"));
        // A summary with no snippet field deserializes to None, not an error.
        assert_eq!(threads[1].snippet, None);
    }

    #[tokio::test]
    async fn an_empty_thread_list_parses_to_no_threads() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        let client = GmailClient::with_base_url("t".into(), Url::parse(&server.uri()).unwrap());
        assert!(client.search("anything").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_gmail_error_names_the_service_without_echoing_the_body() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(429)
                    .set_body_string("echoed-request-value must never surface"),
            )
            .mount(&server)
            .await;
        let client = GmailClient::with_base_url("t".into(), Url::parse(&server.uri()).unwrap());
        let error = client.search("from:client").await.unwrap_err();
        let rendered = error.to_string();
        assert!(rendered.contains("Gmail API request failed"), "{rendered}");
        assert!(rendered.contains("429"), "{rendered}");
        assert!(
            !rendered.contains("echoed-request-value"),
            "provider error body leaked into the diagnostic: {rendered}"
        );
    }

    #[tokio::test]
    async fn an_unparseable_success_body_is_a_parse_error() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_string("not the json you want"),
            )
            .mount(&server)
            .await;
        let client = GmailClient::with_base_url("t".into(), Url::parse(&server.uri()).unwrap());
        let error = client.profile().await.unwrap_err();
        assert!(
            error.to_string().contains("parse Gmail API response"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn profile_reads_the_signed_in_mailbox() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/users/me/profile"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"emailAddress": "lawyer@neonlaw.test"})),
            )
            .mount(&server)
            .await;
        let client = GmailClient::with_base_url("t".into(), Url::parse(&server.uri()).unwrap());
        assert_eq!(
            client.profile().await.unwrap().email_address,
            "lawyer@neonlaw.test"
        );
    }

    #[tokio::test]
    async fn save_draft_creates_then_updates_by_id() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/users/me/drafts"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": "dr-new-1"})),
            )
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("PUT"))
            .and(wiremock::matchers::path("/users/me/drafts/d-1"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": "d-1"})),
            )
            .mount(&server)
            .await;
        let client = GmailClient::with_base_url("t".into(), Url::parse(&server.uri()).unwrap());

        let raw = "Subject: hi\r\n\r\nbody".to_string();
        let created = client
            .save_draft(&DraftMessage {
                id: None,
                thread_id: Some("th-1".into()),
                raw: raw.clone(),
            })
            .await
            .unwrap();
        assert_eq!(created.id, "dr-new-1");
        let sent = &server.received_requests().await.unwrap()[0];
        let body: serde_json::Value = serde_json::from_slice(&sent.body).unwrap();
        assert_eq!(body["message"]["threadId"], "th-1");
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(body["message"]["raw"].as_str().unwrap())
            .unwrap();
        assert_eq!(decoded, raw.as_bytes());

        let updated = client
            .save_draft(&DraftMessage {
                id: Some("d-1".into()),
                thread_id: None,
                raw,
            })
            .await
            .unwrap();
        assert_eq!(updated.id, "d-1");
    }

    #[tokio::test]
    async fn archive_removes_only_the_inbox_label() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/users/me/messages/m-1/modify"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": "m-1"})),
            )
            .mount(&server)
            .await;
        let client = GmailClient::with_base_url("t".into(), Url::parse(&server.uri()).unwrap());
        client.archive_message("m-1").await.unwrap();
        let sent = &server.received_requests().await.unwrap()[0];
        let body: serde_json::Value = serde_json::from_slice(&sent.body).unwrap();
        assert_eq!(
            body["removeLabelIds"],
            serde_json::json!([INBOX_LABEL]),
            "the archive is the INBOX label write, never a delete or a send"
        );
    }
}
