//! Filing a raw `.eml` as a matter Communication — the `POST
//! /app/api/projects/{id}/mail/eml` door's parsing half.
//!
//! The caller supplies bytes it already has (a saved copy, a downloaded
//! export), so no per-machine Gmail OAuth grant is involved: the door parses
//! the MIME here and files the message through the same
//! `(channel, source_ref)` idempotent ingest every email Communication uses.
//! An optional `mailbox` — the firm address the filing lawyer files from —
//! decides direction by comparing it to the message's `From`; without it the
//! message files as inbound.

/// Reasons a raw `.eml` cannot file.
#[derive(Debug, thiserror::Error)]
pub enum EmlParseError {
    #[error("the bytes are not a MIME email message")]
    NotMime,
    #[error("the .eml has no Date header")]
    NoDate,
    #[error("the .eml Date header is out of range")]
    BadDate,
    #[error("the .eml has no supported text body")]
    NoBody,
}

/// One raw `.eml` parsed into everything filing needs. Pure, so the door's
/// tests and the handler agree on shape by construction.
pub struct ParsedEml {
    /// The message's own reference — Message-ID without the angle brackets,
    /// else a content digest — that the source reference and attachment
    /// slugs derive from.
    pub message_ref: String,
    /// External id the communication dedupes on: `eml:` plus the Message-ID
    /// (angle brackets dropped), or a content digest for messages that never
    /// carried one.
    pub source_ref: String,
    /// `inbound`, unless the optional mailbox matches `From`.
    pub direction: &'static str,
    /// `From` header text, `Name <a@b>` when a display name is present.
    pub from: String,
    /// `To` header text, same shape.
    pub to: String,
    pub subject: Option<String>,
    /// The Date header as an RFC 3339 instant (read as UTC).
    pub at: String,
    /// The message's trimmed plain-text body.
    pub body: String,
    /// Every attachment part, names and bytes included.
    pub attachments: Vec<crate::inbound_email::InboundAttachment>,
}

/// Parse raw `.eml` bytes for filing. `mailbox`, when given, marks a message
/// from that address as outbound.
///
/// # Errors
///
/// [`EmlParseError`] when the bytes are not MIME, carry no Date header (or an
/// out-of-range one), or have no supported text body.
pub fn parse_eml(raw: &[u8], mailbox: Option<&str>) -> Result<ParsedEml, EmlParseError> {
    let message = mail_parser::MessageParser::default()
        .parse(raw)
        .ok_or(EmlParseError::NotMime)?;
    // A filing-able message always carries a From; bytes without one are not
    // an email at all (mail_parser coerces arbitrary text into a header-less
    // plain-text message).
    if message.from().is_none() {
        return Err(EmlParseError::NotMime);
    }
    let at = eml_sent_at(message.date())?;
    let body = message
        .body_text(0)
        .map(|body| body.trim().to_string())
        .filter(|body| !body.is_empty())
        .ok_or(EmlParseError::NoBody)?;
    let from = address_text(message.from());
    let to = address_text(message.to());
    let direction = match mailbox {
        Some(mailbox) if from_address(message.from()) == mailbox.trim().to_ascii_lowercase() => {
            "outbound"
        }
        _ => "inbound",
    };
    let message_ref = eml_message_ref(message.message_id(), raw);
    Ok(ParsedEml {
        message_ref: message_ref.clone(),
        source_ref: format!("eml:{message_ref}"),
        direction,
        from,
        to,
        subject: message.subject().map(str::to_string),
        at,
        body,
        attachments: crate::inbound_email::attachments_from_raw(raw),
    })
}

/// The Date header as an RFC 3339 instant, read as UTC: the timestamp the
/// Communication log shows alongside the message.
fn eml_sent_at(date: Option<&mail_parser::DateTime>) -> Result<String, EmlParseError> {
    let date = date.ok_or(EmlParseError::NoDate)?;
    chrono::NaiveDate::from_ymd_opt(
        i32::from(date.year),
        u32::from(date.month),
        u32::from(date.day),
    )
    .and_then(|sent| {
        sent.and_hms_opt(
            u32::from(date.hour),
            u32::from(date.minute),
            u32::from(date.second),
        )
    })
    .map(|sent| sent.and_utc().to_rfc3339())
    .ok_or(EmlParseError::BadDate)
}

/// The message reference inside a filed `.eml`'s source reference: its
/// Message-ID without the angle brackets, else a content digest for messages
/// that never carried one.
fn eml_message_ref(message_id: Option<&str>, bytes: &[u8]) -> String {
    use sha2::Digest as _;
    if let Some(id) = message_id.map(str::trim).filter(|id| !id.is_empty()) {
        return id.trim_matches(['<', '>']).to_string();
    }
    let digest = sha2::Sha256::digest(bytes);
    format!(
        "sha256:{:02x}",
        digest
            .iter()
            .take(8)
            .fold(0u128, |acc, b| (acc << 8) | u128::from(*b))
    )
}

/// `From`/`To` as the filed Communication shows them: `Name <a@b>` when the
/// message carries a display name, the bare address otherwise.
fn address_text(address: Option<&mail_parser::Address<'_>>) -> String {
    match address {
        None => String::new(),
        Some(mail_parser::Address::List(addrs)) => {
            addrs.iter().map(addr_text).collect::<Vec<_>>().join(", ")
        }
        Some(mail_parser::Address::Group(groups)) => groups
            .iter()
            .flat_map(|group| group.addresses.iter().map(addr_text))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

fn addr_text(addr: &mail_parser::Addr<'_>) -> String {
    match (addr.name(), addr.address()) {
        (Some(name), Some(address)) => format!("{name} <{address}>"),
        (Some(name), None) => name.to_string(),
        (None, Some(address)) => address.to_string(),
        (None, None) => String::new(),
    }
}

/// The bare lowercase address of a header's first recipient — what a
/// `mailbox` match compares against.
fn from_address(address: Option<&mail_parser::Address<'_>>) -> String {
    let Some(mail_parser::Address::List(addrs)) = address else {
        return String::new();
    };
    addrs
        .iter()
        .find_map(|addr| addr.address().map(str::to_ascii_lowercase))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: &str = "Message-ID: <abc-123@example.test>\r\n\
From: Client <client@example.test>\r\n\
To: lawyer@neonlaw.test\r\n\
Subject: Retainer questions\r\n\
Date: Fri, 30 Oct 2026 14:03:00 +0000\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Can I sign electronically?\r\n";

    #[test]
    fn a_plain_eml_parses_with_its_headers_and_eml_scoped_source_ref() {
        let parsed = parse_eml(PLAIN.as_bytes(), None).expect("a plain .eml parses");
        assert_eq!(parsed.direction, "inbound");
        assert_eq!(parsed.from, "Client <client@example.test>");
        assert_eq!(parsed.to, "lawyer@neonlaw.test");
        assert_eq!(parsed.subject.as_deref(), Some("Retainer questions"));
        assert_eq!(parsed.at, "2026-10-30T14:03:00+00:00");
        assert_eq!(parsed.body, "Can I sign electronically?");
        assert_eq!(parsed.source_ref, "eml:abc-123@example.test");
        assert!(parsed.attachments.is_empty());
    }

    #[test]
    fn the_firm_mailbox_marks_a_message_outbound() {
        let parsed =
            parse_eml(PLAIN.as_bytes(), Some("counsel@neonlaw.test")).expect("a plain .eml parses");
        assert_eq!(parsed.direction, "inbound", "the sender is not the mailbox");
        let parsed =
            parse_eml(PLAIN.as_bytes(), Some("CLIENT@example.test")).expect("a plain .eml parses");
        assert_eq!(
            parsed.direction, "outbound",
            "the match is case-insensitive"
        );
    }

    #[test]
    fn a_message_without_a_message_id_dedupes_on_a_content_digest() {
        const NO_ID: &str = "From: client@example.test\r\n\
To: lawyer@neonlaw.test\r\n\
Date: Fri, 30 Oct 2026 14:03:00 +0000\r\n\
Content-Type: text/plain\r\n\
\r\n\
Hello\r\n";
        let parsed = parse_eml(NO_ID.as_bytes(), None).expect("parses without a Message-ID");
        let expected = {
            use sha2::Digest as _;
            let digest = sha2::Sha256::digest(NO_ID.as_bytes());
            format!(
                "eml:sha256:{:02x}",
                digest
                    .iter()
                    .take(8)
                    .fold(0u128, |acc, b| (acc << 8) | u128::from(*b))
            )
        };
        assert_eq!(parsed.source_ref, expected);
    }

    #[test]
    fn non_mime_bytes_no_body_and_no_date_are_named_failures() {
        const NO_BODY: &str = "From: client@example.test\r\n\
To: lawyer@neonlaw.test\r\n\
Subject: Empty\r\n\
Date: Fri, 30 Oct 2026 14:03:00 +0000\r\n\
Content-Type: text/plain\r\n\
\r\n\
   \r\n";
        const NO_DATE: &str = "From: client@example.test\r\n\
To: lawyer@neonlaw.test\r\n\
Content-Type: text/plain\r\n\
\r\n\
Hello\r\n";
        assert!(matches!(
            parse_eml(b"not a MIME message", None),
            Err(EmlParseError::NotMime)
        ));
        assert!(matches!(
            parse_eml(NO_BODY.as_bytes(), None),
            Err(EmlParseError::NoBody)
        ));
        assert!(matches!(
            parse_eml(NO_DATE.as_bytes(), None),
            Err(EmlParseError::NoDate)
        ));
    }

    #[test]
    fn attachment_parts_carry_names_and_bytes() {
        const WITH_ATTACHMENT: &str = "From: client@example.test\r\n\
To: lawyer@neonlaw.test\r\n\
Subject: The signed order\r\n\
Date: Fri, 30 Oct 2026 14:03:00 +0000\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"BOUND\"\r\n\
\r\n\
--BOUND\r\n\
Content-Type: text/plain\r\n\
\r\n\
See the attached order.\r\n\
--BOUND\r\n\
Content-Type: application/pdf\r\n\
Content-Disposition: attachment; filename=\"order.pdf\"\r\n\
\r\n\
%PDF-1.4\r\n\
--BOUND--\r\n";
        let parsed = parse_eml(WITH_ATTACHMENT.as_bytes(), None).expect("a multipart .eml parses");
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(parsed.attachments[0].filename, "order.pdf");
        assert_eq!(parsed.attachments[0].content_type, "application/pdf");
        assert_eq!(parsed.attachments[0].bytes, b"%PDF-1.4");
    }
}
