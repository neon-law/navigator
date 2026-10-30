//! Source adapters for imported correspondence (LAW-238 D1).
//!
//! Each adapter parses one native format — an RFC 5322 `.eml`, or the
//! `messages.csv` inside a member's LinkedIn data export — and produces
//! channel-neutral [`ImportMessage`]s for the import door. The adapters own
//! only parsing and identity: dedup, threading, and attribution stay with
//! the store's ingest machinery, which the door calls per message.
//!
//! Identity follows the house pattern: a native provider id when the format
//! has one (an email's `Message-ID`), otherwise a scoped SHA-256 digest so
//! re-importing the same payload dedups to no-ops (AC5).

use crate::inbox;

/// `source` values the import door accepts.
pub const SOURCE_EMAIL: &str = "email";
pub const SOURCE_LINKEDIN: &str = "linkedin";

/// Why an adapter refuses a payload. Errors name the offending shape so a
/// mis-selected file or a LinkedIn schema change fails loudly, not silently
/// (AC9).
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("unknown import source: {0}")]
    UnknownSource(String),
    #[error("payload is not a parseable RFC 5322 message")]
    MalformedEmail,
    #[error("unrecognized LinkedIn export shape — headers were: {0:?}")]
    UnrecognizedExport(Vec<String>),
    #[error("LinkedIn export row {row} is missing {field}")]
    ExportRowMissingField { row: usize, field: &'static str },
    #[error("LinkedIn export row {row}: unparseable DATE value {value:?}")]
    ExportRowUnparseableDate { row: usize, value: String },
    #[error("payload is empty")]
    EmptyPayload,
    #[error("payload is not valid UTF-8 text")]
    NotUtf8,
}

/// Which side of the exchange authored the message, decided by the adapter
/// from the author and the importing mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Inbound,
    Outbound,
}

impl Direction {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Inbound => inbox::direction::INBOUND,
            Direction::Outbound => inbox::direction::OUTBOUND,
        }
    }
}

/// One attachment carried inside an imported message. The LinkedIn export
/// has none (its format carries no attachments); email parts land here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportAttachment {
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// One channel-neutral message an adapter produced. The import door maps it
/// onto [`inbox::IngestInbound`] and walks it through the direction-aware
/// ingest; `author_person_id` is resolved there from `author_address`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportMessage {
    pub channel: &'static str,
    pub direction: Direction,
    /// Display name of the message's author, when the format carries one.
    pub author_name: Option<String>,
    /// Author identity — an email address, or a LinkedIn profile URL.
    pub author_address: Option<String>,
    /// The other party's address, the Conversation's `external_address`.
    pub counterparty_address: String,
    pub counterparty_name: Option<String>,
    pub subject: Option<String>,
    pub body: String,
    /// Native provider id when the format has one; a scoped digest fallback
    /// otherwise. Both adapters always produce one — the ingest dedup key
    /// requires it.
    pub provider_message_id: String,
    pub parent_provider_message_id: Option<String>,
    /// Groups a thread's messages into one Conversation.
    pub provider_thread_ref: Option<String>,
    /// RFC 3339.
    pub occurred_at: String,
    pub attachments: Vec<ImportAttachment>,
    /// Set by the door after archiving the verbatim payload — the `.eml`
    /// key on each email message, or the CSV key on every LinkedIn message
    /// of the import (AC6).
    pub raw_storage_key: Option<String>,
}

/// A source the import door can parse. One impl per `source` value; a new
/// import source is one more impl, plus channel plumbing if it is a channel
/// the store does not model yet.
pub trait ImportSource: Send + Sync {
    /// The Inbox channel the parsed messages land on.
    fn channel(&self) -> &'static str;
    /// Parse the verbatim payload into channel-neutral messages.
    ///
    /// # Errors
    ///
    /// Fails loudly on a payload the format cannot carry — never guesses.
    fn parse(&self, mailbox: &str, bytes: &[u8]) -> Result<Vec<ImportMessage>, ImportError>;
}

/// Resolve a `source` value to its adapter. The door dispatches on this.
///
/// # Errors
///
/// [`ImportError::UnknownSource`] for a `source` the store does not model.
pub fn source_adapter(source: &str) -> Result<Box<dyn ImportSource>, ImportError> {
    match source {
        SOURCE_EMAIL => Ok(Box::new(EmailAdapter)),
        SOURCE_LINKEDIN => Ok(Box::new(LinkedInAdapter)),
        other => Err(ImportError::UnknownSource(other.to_string())),
    }
}

/// The identity scope for digest fallbacks: the same `"inbox"` scope the
/// SendGrid path uses, so an id-less message dedups within its mailbox.
/// Mirrors `workflows::scoped_email_digest` (store cannot depend on
/// workflows); changing the framing re-keys id-less identity.
#[must_use]
pub fn scoped_digest(mailbox: &str, bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(b"inbox");
    hasher.update([0]);
    hasher.update(mailbox.to_lowercase().as_bytes());
    hasher.update([0]);
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{byte:02x}"));
    }
    hex
}

/// Length-framed SHA-256 over the fields that make an id-less row unique —
/// the framing `portal/src/inbox.rs::provider_message_id` uses for
/// token-less mail, so the same mail delivered by either path dedups.
fn digest_fields(mailbox: &str, fields: [&[u8]; 4]) -> String {
    use sha2::{Digest, Sha256};

    let mut digest = Sha256::new();
    for field in fields {
        digest.update(field.len().to_string().as_bytes());
        digest.update(b":");
        digest.update(field);
    }
    let finalized = digest.finalize();
    scoped_digest(mailbox, &finalized)
}

// ---------------------------------------------------------------------------
// Email adapter
// ---------------------------------------------------------------------------

/// RFC 5322 `.eml` adapter (spec D1, R2 §5): `channel::EMAIL` with the
/// importing mailbox as `provider_account`. Direction: a message authored by
/// the mailbox owner is `outbound` — the path nothing else provides
/// (R1 §4) — anything else `inbound`. Identity: normalized `Message-ID`,
/// else the scoped digest over the parsed message. Attachments ride the
/// message and are filed by the door with full provenance.
pub struct EmailAdapter;

impl ImportSource for EmailAdapter {
    fn channel(&self) -> &'static str {
        inbox::channel::EMAIL
    }

    fn parse(&self, mailbox: &str, bytes: &[u8]) -> Result<Vec<ImportMessage>, ImportError> {
        if bytes.is_empty() {
            return Err(ImportError::EmptyPayload);
        }
        let message = mail_parser::MessageParser::default()
            .parse(bytes)
            .ok_or(ImportError::MalformedEmail)?;

        // mail-parser tolerates non-MIME garbage by treating the whole file
        // as one malformed header — but either way, a payload with no From
        // does not parse into an addressable email, so it fails loudly
        // instead of importing a mislabeled text file (AC9).
        let (author_address, author_name) =
            email_author(&message).ok_or(ImportError::MalformedEmail)?;

        // Equal-address ambiguity resolves outbound, with a comment: a
        // message the mailbox sent itself is lawyer-sent, never inbound.
        let direction = if author_address == mailbox.trim().to_lowercase() {
            Direction::Outbound
        } else {
            Direction::Inbound
        };
        let to_address = message
            .to()
            .and_then(|to| to.first())
            .and_then(|addr| addr.address())
            .map(|address| address.trim().to_string());
        // The counterparty is the other side of the exchange: who the
        // mailbox wrote to, or who wrote to the mailbox.
        let counterparty_address = match &direction {
            Direction::Outbound => to_address.clone().unwrap_or_else(|| mailbox.to_string()),
            Direction::Inbound => author_address.clone(),
        };

        let message_id = message
            .message_id()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string);
        let provider_message_id = message_id.unwrap_or_else(|| {
            digest_fields(
                mailbox,
                [
                    author_address.as_bytes(),
                    message.subject().unwrap_or_default().as_bytes(),
                    message
                        .body_text(0)
                        .map(std::borrow::Cow::into_owned)
                        .unwrap_or_default()
                        .as_bytes(),
                    bytes,
                ],
            )
        });
        let provider_thread_ref = message
            .in_reply_to()
            .as_text()
            .map(str::trim)
            .filter(|reply_to| !reply_to.is_empty())
            .map(str::to_string);

        let attachments = email_attachments(&message);

        let occurred_at = message
            .date()
            .map(rfc3339_from_mail_date)
            .filter(|stamp| !stamp.is_empty())
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

        Ok(vec![ImportMessage {
            channel: inbox::channel::EMAIL,
            direction,
            author_name,
            author_address: Some(author_address),
            counterparty_address,
            counterparty_name: None,
            subject: message
                .subject()
                .map(str::to_string)
                .filter(|subject| !subject.is_empty()),
            body: message
                .body_text(0)
                .map(std::borrow::Cow::into_owned)
                .unwrap_or_default(),
            provider_message_id,
            parent_provider_message_id: provider_thread_ref.clone(),
            provider_thread_ref,
            occurred_at,
            attachments,
            raw_storage_key: None,
        }])
    }
}

/// The From header as (lowercased address, trimmed display name) — `None`
/// when the payload has no addressable sender.
fn email_author(message: &mail_parser::Message<'_>) -> Option<(String, Option<String>)> {
    let author = message.from().and_then(|from| from.first())?;
    let address = author
        .address()
        .map(|address| address.trim().to_lowercase())
        .filter(|address| !address.is_empty())?;
    let name = author
        .name()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string);
    Some((address, name))
}

/// Every attachment part of a parsed email, in header order.
fn email_attachments(message: &mail_parser::Message<'_>) -> Vec<ImportAttachment> {
    use mail_parser::MimeHeaders as _;

    message
        .attachments()
        .map(|part| {
            let content_type = part.content_type().map_or_else(
                || "application/octet-stream".to_owned(),
                |mime| {
                    format!(
                        "{}/{}",
                        mime.c_type,
                        mime.c_subtype.as_deref().unwrap_or("octet-stream")
                    )
                },
            );
            ImportAttachment {
                filename: part.attachment_name().unwrap_or("attachment").to_string(),
                content_type,
                bytes: part.contents().to_vec(),
            }
        })
        .collect()
}

/// An RFC 5322 date as RFC 3339 UTC — `to_timestamp` resolves the header's
/// own timezone offset. Empty when the header is absent or invalid.
#[must_use]
pub fn rfc3339_from_mail_date(date: &mail_parser::DateTime) -> String {
    if !date.is_valid() {
        return String::new();
    }
    chrono::DateTime::from_timestamp(date.to_timestamp(), 0)
        .map(|utc| utc.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// LinkedIn adapter
// ---------------------------------------------------------------------------

/// The export's required columns, lowercased (R5). The schema is not
/// contractual and varies by export version, so parsing reads headers by
/// these names and refuses anything else.
const LINKEDIN_REQUIRED_COLUMNS: &[&str] = &["conversation id", "from", "date", "content"];

/// `messages.csv` from the member's LinkedIn data export (R5). One Inbox
/// Conversation per `CONVERSATION ID`; rows ordered by normalized `DATE`.
/// Identity is a scoped digest over (conversation id, date, sender,
/// content) — the export carries no per-message id, so re-importing the
/// same export dedups to no-ops (AC5). The export carries no attachments.
pub struct LinkedInAdapter;

impl ImportSource for LinkedInAdapter {
    fn channel(&self) -> &'static str {
        inbox::channel::LINKEDIN
    }

    fn parse(&self, mailbox: &str, bytes: &[u8]) -> Result<Vec<ImportMessage>, ImportError> {
        if bytes.is_empty() {
            return Err(ImportError::EmptyPayload);
        }
        let text = std::str::from_utf8(bytes)
            .map_err(|_| ImportError::NotUtf8)?
            .trim_start_matches('\u{feff}');

        let mut reader = csv::ReaderBuilder::new()
            .flexible(true)
            .has_headers(true)
            .from_reader(text.as_bytes());
        let headers = reader
            .headers()
            .map_err(|error| ImportError::UnrecognizedExport(vec![error.to_string()]))?;
        let header_names: Vec<String> = headers
            .iter()
            .map(str::trim)
            .map(str::to_lowercase)
            .collect();

        let column = |name: &str| header_names.iter().position(|header| header == name);
        // Any required column missing — or the shape wholly alien — fails
        // loudly with the offending header list (AC9).
        for name in LINKEDIN_REQUIRED_COLUMNS {
            if column(name).is_none() {
                return Err(ImportError::UnrecognizedExport(
                    headers.iter().map(str::to_string).collect(),
                ));
            }
        }
        let conversation_id_at = column("conversation id");
        let from_at = column("from");
        let date_at = column("date");
        let content_at = column("content");
        let profile_url_at = column("sender profile url");
        let to_at = column("to");
        let subject_at = column("subject");

        let mut messages = Vec::new();
        for (index, record) in reader.records().enumerate() {
            let row = index + 1; // the header row is row 0
            let record = record.map_err(|error| {
                ImportError::UnrecognizedExport(vec![format!("row {row}: {error}")])
            })?;
            let field = |at: Option<usize>| at.and_then(|i| record.get(i)).map(str::trim);
            let conversation_id = field(conversation_id_at)
                .filter(|value| !value.is_empty())
                .ok_or(ImportError::ExportRowMissingField {
                    row,
                    field: "CONVERSATION ID",
                })?;
            let from = field(from_at)
                .filter(|value| !value.is_empty())
                .ok_or(ImportError::ExportRowMissingField { row, field: "FROM" })?;
            let date = field(date_at)
                .filter(|value| !value.is_empty())
                .ok_or(ImportError::ExportRowMissingField { row, field: "DATE" })?;
            let content = field(content_at).unwrap_or_default();
            let profile_url = field(profile_url_at).filter(|value| !value.is_empty());
            let to = field(to_at).filter(|value| !value.is_empty());
            let subject = field(subject_at).filter(|value| !value.is_empty());

            let message = linkedin_row_to_message(
                mailbox,
                row,
                conversation_id,
                from,
                date,
                content,
                profile_url,
                to,
                subject,
            )?;
            messages.push(message);
        }

        // A Conversation reads chronologically; exports may not be ordered.
        messages.sort_by(|a, b| a.occurred_at.cmp(&b.occurred_at));
        Ok(messages)
    }
}

/// Map one export row to a channel-neutral import message. `row` is the
/// 1-based CSV row the values came from — error messages name it.
#[allow(clippy::too_many_arguments)]
fn linkedin_row_to_message<'a>(
    mailbox: &str,
    row: usize,
    conversation_id: &'a str,
    from: &'a str,
    date: &'a str,
    content: &'a str,
    profile_url: Option<&'a str>,
    to: Option<&'a str>,
    subject: Option<&'a str>,
) -> Result<ImportMessage, ImportError> {
    // Direction: the member's own rows are their half of the
    // exchange (AC4). The importing account is the member's LinkedIn
    // identity — profile URL, or the exact display name the export
    // uses.
    let sender = profile_url.unwrap_or(from);
    let direction = if sender.eq_ignore_ascii_case(mailbox.trim())
        || from.eq_ignore_ascii_case(mailbox.trim())
    {
        Direction::Outbound
    } else {
        Direction::Inbound
    };
    // The thread-stable counterparty identity is the display name:
    // an outbound row names the recipient in TO, an inbound row
    // names the sender in FROM — the same person, the same string,
    // so both halves of one conversation thread together. The
    // profile URL identifies the row's SENDER (direction, digest),
    // but the export carries no URL for the person in TO.
    let counterparty = match direction {
        Direction::Outbound => to.unwrap_or(mailbox).to_string(),
        Direction::Inbound => from.to_string(),
    };
    let occurred_at =
        linkedin_date_to_rfc3339(date).ok_or(ImportError::ExportRowUnparseableDate {
            row,
            value: date.to_string(),
        })?;
    // SUBJECT+CONTENT is the body (spec D1); the subject also rides
    // the thread header.
    let body = match subject {
        Some(subject) if !content.is_empty() => format!("{subject}\n\n{content}"),
        Some(subject) => subject.to_string(),
        None => content.to_string(),
    };
    let provider_message_id = digest_fields(
        mailbox,
        [
            conversation_id.as_bytes(),
            date.as_bytes(),
            sender.as_bytes(),
            content.as_bytes(),
        ],
    );
    Ok(ImportMessage {
        channel: inbox::channel::LINKEDIN,
        direction,
        author_name: Some(from.to_string()),
        author_address: profile_url.map(str::to_string),
        counterparty_address: counterparty,
        counterparty_name: match direction {
            Direction::Inbound => Some(from.to_string()),
            Direction::Outbound => to.map(str::to_string),
        },
        subject: subject.map(str::to_string),
        body,
        provider_message_id,
        parent_provider_message_id: None,
        provider_thread_ref: Some(conversation_id.to_string()),
        occurred_at,
        attachments: Vec::new(),
        raw_storage_key: None,
    })
}

/// LinkedIn export timestamps ("2026-10-30 14:23:05 UTC", RFC 3339, and
/// date-only shapes) as RFC 3339 UTC. Returns `None` for a value none of
/// the known shapes parse — the caller fails loudly (AC9).
#[must_use]
pub fn linkedin_date_to_rfc3339(value: &str) -> Option<String> {
    let value = value.trim();
    if let Ok(stamp) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(stamp.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    }
    for format in [
        "%Y-%m-%d %H:%M:%S%.f %Z",
        "%Y-%m-%d %H:%M:%S %Z",
        "%Y-%m-%d %H:%M %Z",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%d",
        "%m/%d/%y %H:%M",
        "%m/%d/%y",
    ] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(value, format) {
            return Some(
                naive
                    .and_utc()
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            );
        }
        if format == "%Y-%m-%d" {
            if let Ok(date) = chrono::NaiveDate::parse_from_str(value, format) {
                return Some(
                    date.and_hms_opt(0, 0, 0)?
                        .and_utc()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                );
            }
        }
        if format.ends_with("%Z") {
            if let Ok(with_tz) = chrono::DateTime::parse_from_str(value, format) {
                return Some(with_tz.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Door-side mapping
// ---------------------------------------------------------------------------

/// Map one parsed message onto the ingest payload the door walks through
/// the direction-aware postgres ingest. `firm_id` and `mailbox` come from
/// the import request; the store's own identity does the rest.
#[must_use]
pub fn import_message_to_ingest<'a>(
    message: &'a ImportMessage,
    firm_id: uuid::Uuid,
    mailbox: &'a str,
) -> inbox::IngestInbound<'a> {
    inbox::IngestInbound {
        firm_id,
        brand_key: None,
        interaction_type: Some("message"),
        provider_post_id: None,
        parent_provider_message_id: message.parent_provider_message_id.as_deref(),
        visibility: None,
        channel: message.channel,
        provider_account: mailbox,
        provider_message_id: message.provider_message_id.as_str(),
        provider_thread_ref: message.provider_thread_ref.as_deref(),
        external_address: message.counterparty_address.as_str(),
        external_name: message.counterparty_name.as_deref(),
        subject: message.subject.as_deref(),
        body: &message.body,
        // The importing mailbox is where the payload was received — the
        // same "source page" sense the support inbox records.
        source_page: Some(mailbox),
        source_channel: None,
        // An import records existing correspondence; it is not itself a
        // consent signal, and imports are never promotional.
        matter_consent: false,
        promotional_opt_in: false,
        email_conversation_id: None,
        attachment_asset_id: None,
        occurred_at: &message.occurred_at,
        raw_storage_key: message.raw_storage_key.as_deref(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAILBOX: &str = "nick@neonlaw.com";

    #[test]
    fn email_from_counterparty_is_inbound_with_message_id_identity() {
        let raw = b"Message-ID: <notice-7@court.example>\r\n\
                    From: Clerk <clerk@court.example>\r\n\
                    To: nick@neonlaw.com\r\n\
                    Subject: Re: discovery schedule\r\n\
                    Date: Tue, 28 Oct 2026 14:03:00 +0000\r\n\
                    \r\n\
                    The schedule is entered.";
        let parsed = EmailAdapter.parse(MAILBOX, raw).expect("parses");
        assert_eq!(parsed.len(), 1);
        let message = &parsed[0];
        assert_eq!(message.channel, "email");
        assert_eq!(message.direction, Direction::Inbound);
        assert_eq!(message.provider_message_id, "notice-7@court.example");
        assert_eq!(
            message.author_address.as_deref(),
            Some("clerk@court.example")
        );
        assert_eq!(message.counterparty_address, "clerk@court.example");
        assert_eq!(message.subject.as_deref(), Some("Re: discovery schedule"));
        assert_eq!(message.occurred_at, "2026-10-28T14:03:00Z");
        assert!(message.attachments.is_empty());
    }

    #[test]
    fn email_from_mailbox_owner_is_outbound() {
        let raw = b"Message-ID: <sent-1@neonlaw.com>\r\n\
                    From: Nick Shook <nick@neonlaw.com>\r\n\
                    To: clerk@court.example\r\n\
                    Subject: Re: discovery schedule\r\n\
                    Date: Tue, 28 Oct 2026 15:00:00 +0000\r\n\
                    \r\n\
                    Filed this morning.";
        let parsed = EmailAdapter.parse(MAILBOX, raw).expect("parses");
        assert_eq!(parsed[0].direction, Direction::Outbound);
        assert_eq!(parsed[0].counterparty_address, "clerk@court.example");
        assert_eq!(parsed[0].author_name.as_deref(), Some("Nick Shook"));
    }

    #[test]
    fn token_less_email_dedups_via_scoped_digest() {
        let raw = b"From: clerk@court.example\r\n\
                    To: nick@neonlaw.com\r\n\
                    Subject: no id\r\n\
                    \r\n\
                    body text";
        let first = EmailAdapter.parse(MAILBOX, raw).expect("parses");
        let second = EmailAdapter.parse(MAILBOX, raw).expect("parses");
        assert!(!first[0].provider_message_id.is_empty());
        assert_eq!(first[0].provider_message_id, second[0].provider_message_id);
        // The digest is scoped to the mailbox, so another firm's import of
        // the same bytes cannot collide.
        let elsewhere = EmailAdapter
            .parse("other@neonlaw.com", raw)
            .expect("parses");
        assert_ne!(
            first[0].provider_message_id,
            elsewhere[0].provider_message_id
        );
    }

    #[test]
    fn email_thread_ref_follows_in_reply_to() {
        let raw = b"Message-ID: <reply-2@neonlaw.com>\r\n\
                    In-Reply-To: <notice-7@court.example>\r\n\
                    From: nick@neonlaw.com\r\n\
                    To: clerk@court.example\r\n\
                    Subject: Re: discovery schedule\r\n\
                    \r\n\
                    Acknowledged.";
        let parsed = EmailAdapter.parse(MAILBOX, raw).expect("parses");
        assert_eq!(
            parsed[0].provider_thread_ref.as_deref(),
            Some("notice-7@court.example")
        );
        assert_eq!(
            parsed[0].parent_provider_message_id.as_deref(),
            Some("notice-7@court.example")
        );
    }

    #[test]
    fn email_attachments_are_parsed_with_metadata() {
        let raw = b"From: clerk@court.example\r\n\
                    To: nick@neonlaw.com\r\n\
                    Subject: exhibit\r\n\
                    Content-Type: multipart/mixed; boundary=nav\r\n\
                    \r\n\
                    --nav\r\n\
                    Content-Type: text/plain\r\n\
                    \r\n\
                    see attached\r\n\
                    --nav\r\n\
                    Content-Type: application/pdf\r\n\
                    Content-Disposition: attachment; filename=exhibit-a.pdf\r\n\
                    Content-Transfer-Encoding: base64\r\n\
                    \r\n\
                    JVBERi0xLjc=\r\n\
                    --nav--\r\n";
        let parsed = EmailAdapter.parse(MAILBOX, raw).expect("parses");
        assert_eq!(parsed[0].attachments.len(), 1);
        let attachment = &parsed[0].attachments[0];
        assert_eq!(attachment.filename, "exhibit-a.pdf");
        assert_eq!(attachment.content_type, "application/pdf");
        assert_eq!(attachment.bytes, b"%PDF-1.7");
    }

    #[test]
    fn garbage_email_fails_loudly() {
        assert!(matches!(
            EmailAdapter.parse(MAILBOX, b"not mime at all"),
            Err(ImportError::MalformedEmail)
        ));
        assert!(matches!(
            EmailAdapter.parse(MAILBOX, b""),
            Err(ImportError::EmptyPayload)
        ));
    }

    const LINKEDIN_HEADERS: &str =
        "CONVERSATION ID,FROM,SENDER PROFILE URL,TO,DATE,SUBJECT,CONTENT";

    fn linkedin_fixture() -> String {
        format!(
            "{LINKEDIN_HEADERS}\r\n\
             conv-1,Jordan Cliente,https://linkedin.com/in/jordan,Me,2026-10-29 10:00:00 UTC,,Can we talk about the retainer?\r\n\
             conv-1,Me,https://linkedin.com/in/me,Jordan Cliente,2026-10-29 11:00:00 UTC,,Of course - Tuesday works.\r\n\
             conv-2,Payee Recruiter,https://linkedin.com/in/recruiter,Me,2026-10-28 09:30:00 UTC,,Role opening\r\n"
        )
    }

    #[test]
    fn linkedin_rows_parse_thread_and_order() {
        let parsed = LinkedInAdapter
            .parse("https://linkedin.com/in/me", linkedin_fixture().as_bytes())
            .expect("parses");
        assert_eq!(parsed.len(), 3);
        // Rows sorted by normalized occurred_at across threads.
        assert_eq!(parsed[0].provider_thread_ref.as_deref(), Some("conv-2"));
        assert_eq!(parsed[1].occurred_at, "2026-10-29T10:00:00Z");
        assert_eq!(parsed[2].occurred_at, "2026-10-29T11:00:00Z");
        // The member's own rows are outbound; everyone else's inbound.
        assert_eq!(parsed[0].direction, Direction::Inbound);
        assert_eq!(parsed[1].direction, Direction::Inbound);
        assert_eq!(parsed[2].direction, Direction::Outbound);
        // Identity is a scoped digest, stable across re-imports.
        let again = LinkedInAdapter
            .parse("https://linkedin.com/in/me", linkedin_fixture().as_bytes())
            .expect("parses");
        assert_eq!(parsed[1].provider_message_id, again[1].provider_message_id);
        // SUBJECT+CONTENT compose the body.
        assert!(parsed[2].body.contains("Of course - Tuesday works."));
    }

    #[test]
    fn linkedin_header_match_is_case_insensitive() {
        let lowered = linkedin_fixture().to_lowercase();
        let parsed = LinkedInAdapter
            .parse("https://linkedin.com/in/me", lowered.as_bytes())
            .expect("parses");
        assert_eq!(parsed.len(), 3);
    }

    #[test]
    fn corrupted_linkedin_headers_fail_loudly_with_header_list() {
        let bytes = "conversation,who,when,what\r\nx,y,z,w\r\n";
        let error = LinkedInAdapter
            .parse(MAILBOX, bytes.as_bytes())
            .expect_err("unrecognized shape fails");
        match error {
            ImportError::UnrecognizedExport(headers) => {
                assert!(headers.iter().any(|header| header == "who"));
            }
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn linkedin_row_missing_required_field_names_the_field() {
        let bytes =
            format!("{LINKEDIN_HEADERS}\r\n,Someone,url,Me,2026-10-29 10:00:00 UTC,,hello\r\n");
        assert!(matches!(
            LinkedInAdapter.parse(MAILBOX, bytes.as_bytes()),
            Err(ImportError::ExportRowMissingField {
                field: "CONVERSATION ID",
                ..
            })
        ));
    }

    #[test]
    fn linkedin_unparseable_date_is_named() {
        let bytes = format!("{LINKEDIN_HEADERS}\r\nconv-1,Jordan,url,Me,not a date,,hello\r\n");
        assert!(matches!(
            LinkedInAdapter.parse(MAILBOX, bytes.as_bytes()),
            Err(ImportError::ExportRowUnparseableDate { .. })
        ));
    }

    #[test]
    fn linkedin_dates_normalize_to_rfc3339() {
        assert_eq!(
            linkedin_date_to_rfc3339("2026-10-29 10:00:00 UTC").as_deref(),
            Some("2026-10-29T10:00:00Z")
        );
        assert_eq!(
            linkedin_date_to_rfc3339("2026-10-29T10:00:00Z").as_deref(),
            Some("2026-10-29T10:00:00Z")
        );
        assert_eq!(linkedin_date_to_rfc3339("yesterday"), None);
    }

    #[test]
    fn source_adapter_dispatches_and_refuses_unknowns() {
        assert!(source_adapter("email").is_ok());
        assert!(source_adapter("linkedin").is_ok());
        assert!(matches!(
            source_adapter("facebook-mail"),
            Err(ImportError::UnknownSource(_))
        ));
    }

    #[test]
    fn import_message_maps_onto_ingest_payload() {
        let message = ImportMessage {
            channel: "email",
            direction: Direction::Outbound,
            author_name: Some("Nick Shook".to_string()),
            author_address: Some(MAILBOX.to_string()),
            counterparty_address: "clerk@court.example".to_string(),
            counterparty_name: Some("Clerk".to_string()),
            subject: Some("re: discovery".to_string()),
            body: "Filed.".to_string(),
            provider_message_id: "sent-1@neonlaw.com".to_string(),
            parent_provider_message_id: None,
            provider_thread_ref: None,
            occurred_at: "2026-10-28T15:00:00Z".to_string(),
            attachments: Vec::new(),
            raw_storage_key: Some("mail/1760000000-sent.eml".to_string()),
        };
        let firm_id = uuid::Uuid::now_v7();
        let inbound = import_message_to_ingest(&message, firm_id, MAILBOX);
        assert_eq!(inbound.firm_id, firm_id);
        assert_eq!(inbound.channel, "email");
        assert_eq!(inbound.provider_account, MAILBOX);
        assert_eq!(inbound.external_address, "clerk@court.example");
        assert_eq!(inbound.body, "Filed.");
        assert_eq!(inbound.raw_storage_key, Some("mail/1760000000-sent.eml"));
        assert_eq!(inbound.provider_message_id, "sent-1@neonlaw.com");
    }
}
