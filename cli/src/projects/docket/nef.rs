//! Read a CM/ECF Notice of Electronic Filing (`docs/glossary/notice-of-electronic-filing.md`).
//!
//! A court's CM/ECF system mails one notice per docket entry. Almost all of
//! its body — often hundreds of thousands of characters — is the
//! [Service List](../../../../docs/glossary/service-list.md); the docket entry itself is a
//! handful of labelled lines and one single-use free-look link.
//!
//! This module is pure: bytes in, a [`Notice`] out. It opens no connection
//! and touches no file, so `navigator project docket ingest` can parse a whole
//! mailbox export before it spends a single free look. Every pass over the
//! body is linear, because the service list is the bulk of the input.

use std::borrow::Cow;

use chrono::NaiveDate;
use serde::Serialize;

/// One docket entry as its notice describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Notice {
    /// The court's own heading lines, joined: `U.S. District Court, District of …`.
    pub court: String,
    /// The case number as the notice prints it, judge initials included.
    pub case_no: String,
    pub case_name: Option<String>,
    /// The docket entry number. `None` for a text-only entry, whose notice
    /// says "No document attached".
    pub doc_no: Option<String>,
    /// The documents after the main one, numbered from 1 in notice order.
    pub attachments: Vec<Attachment>,
    /// The docket text, whitespace collapsed.
    pub docket_text: String,
    /// The party the entry was filed for, when the notice names one.
    pub filed_by: Option<String>,
    /// The CM/ECF user who entered the transaction, as `First Last`.
    pub filer_attorney: Option<String>,
    pub filed_at: Option<NaiveDate>,
    /// The main document's single-use free-look link.
    pub free_look_url: Option<String>,
    /// The main document's filename as the filer uploaded it.
    pub original_filename: Option<String>,
    /// Everyone the notice was electronically mailed to.
    pub service_list: Vec<ServiceListEntry>,
}

/// One document attached to the main document of a docket entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Attachment {
    pub number: u32,
    pub description: String,
    pub original_filename: Option<String>,
    /// Present only when the notice links the attachment on its own.
    pub free_look_url: Option<String>,
}

/// One person a notice was electronically mailed to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceListEntry {
    pub name: String,
    /// `Creditor Acme Lending Co` from `… on behalf of Creditor Acme Lending Co`.
    pub party: Option<String>,
    /// Lowercased, in notice order; the first is the primary email.
    pub emails: Vec<String>,
}

impl ServiceListEntry {
    /// The address a Person is keyed on.
    #[must_use]
    pub fn primary_email(&self) -> Option<&str> {
        self.emails.first().map(String::as_str)
    }
}

/// Why a message is not a notice this module can read.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NefError {
    #[error("not a CM/ECF Notice of Electronic Filing")]
    NotANotice,
    #[error("the notice has no `{0}` line")]
    Missing(&'static str),
}

const NOTICE_HEADING: &str = "Notice of Electronic Filing";

/// Parse one `.eml` message. A file that is not MIME is read as the notice
/// text itself.
///
/// # Errors
///
/// [`NefError`] when the message is not a notice or lacks its case number.
pub fn parse_eml(bytes: &[u8]) -> Result<Notice, NefError> {
    let (text, links) = message_text(bytes);
    parse_text(&text, &links)
}

/// The readable text of a message and every link its HTML carried. The HTML
/// part wins when there is one: it is the only part that is sure to carry
/// the free-look link.
fn message_text(bytes: &[u8]) -> (String, Vec<String>) {
    if let Some(message) = mail_parser::MessageParser::default().parse(bytes) {
        if let Some(html) = message
            .html_body()
            .next()
            .filter(|part| matches!(part.kind(), mail_parser::PartKind::Html))
            .and_then(|part| part.text())
        {
            return html_to_text(&html);
        }
        if let Some(text) = message.body_text(0) {
            return (text.into_owned(), Vec::new());
        }
    }
    (String::from_utf8_lossy(bytes).into_owned(), Vec::new())
}

/// Parse a notice already reduced to text. `links` are the HTML hrefs in
/// document order; a plain-text notice carries its links inline instead.
///
/// # Errors
///
/// [`NefError`] when `text` is not a notice or lacks its case number.
pub fn parse_text(text: &str, links: &[String]) -> Result<Notice, NefError> {
    let rows: Vec<&str> = text.lines().map(str::trim).collect();
    let heading = rows
        .iter()
        .position(|line| line.eq_ignore_ascii_case(NOTICE_HEADING))
        .ok_or(NefError::NotANotice)?;

    let mut notice = Notice {
        court: court_heading(&rows[..heading]),
        case_no: String::new(),
        case_name: None,
        doc_no: None,
        attachments: Vec::new(),
        docket_text: String::new(),
        filed_by: None,
        filer_attorney: None,
        filed_at: None,
        free_look_url: None,
        original_filename: None,
        service_list: Vec::new(),
    };
    let mut section = Section::Header;
    let mut docket_lines: Vec<&str> = Vec::new();
    let mut documents: Vec<Document> = Vec::new();
    let mut pending_name: Option<&str> = None;
    let mut inline_links: Vec<String> = Vec::new();

    for line in &rows[heading + 1..] {
        if let Some(next) = section_marker(line) {
            section = next;
            pending_name = None;
            continue;
        }
        match section {
            Section::Header => read_header_line(line, &mut notice, &mut inline_links),
            Section::DocketText => {
                if line.is_empty() {
                    if !docket_lines.is_empty() {
                        section = Section::Other;
                    }
                } else {
                    docket_lines.push(line);
                }
            }
            Section::Documents => read_document_line(line, &mut documents),
            Section::ServiceList => {
                read_service_line(line, &mut pending_name, &mut notice.service_list);
            }
            Section::Other => {}
        }
    }

    if notice.case_no.is_empty() {
        return Err(NefError::Missing("Case Number"));
    }
    notice.docket_text = collapse_whitespace(&docket_lines.join(" "));
    if notice.filed_by.is_none() {
        notice.filed_by = filed_by_from_docket_text(&notice.docket_text);
    }

    let mut document_links = links
        .iter()
        .chain(inline_links.iter())
        .filter(|link| is_free_look(link))
        .cloned();
    notice.free_look_url = document_links.next();
    let mut documents = documents.into_iter();
    if let Some(main) = documents.next() {
        notice.original_filename = main.original_filename;
    }
    notice.attachments = documents
        .zip(1u32..)
        .map(|(document, number)| Attachment {
            number,
            description: document.description,
            original_filename: document.original_filename,
            free_look_url: document_links.next(),
        })
        .collect();
    Ok(notice)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Header,
    DocketText,
    Documents,
    ServiceList,
    Other,
}

struct Document {
    description: String,
    original_filename: Option<String>,
}

/// The section a marker line opens, or `None` for an ordinary line.
fn section_marker(line: &str) -> Option<Section> {
    let lower = line.to_ascii_lowercase();
    if lower == "docket text:" {
        return Some(Section::DocketText);
    }
    if lower.starts_with("the following document(s) are associated with this transaction") {
        return Some(Section::Documents);
    }
    if lower.ends_with("notice has been electronically mailed to:")
        || lower.ends_with("notice will be electronically mailed to:")
    {
        return Some(Section::ServiceList);
    }
    if lower.ends_with("notice has been delivered by other means to:")
        || lower.ends_with("notice will not be electronically mailed to:")
        || lower.ends_with("notice will be delivered by other means to:")
    {
        return Some(Section::Other);
    }
    None
}

fn read_header_line(line: &str, notice: &mut Notice, inline_links: &mut Vec<String>) {
    if let Some(rest) = line.strip_prefix("The following transaction was") {
        notice.filer_attorney = transaction_actor(rest);
        notice.filed_at = rest
            .rsplit_once("filed on ")
            .and_then(|(_, date)| parse_us_date(date.split_whitespace().next().unwrap_or("")));
        return;
    }
    let Some((label, value)) = line.split_once(':') else {
        return;
    };
    let value = value.trim();
    match label.trim().to_ascii_lowercase().as_str() {
        "case name" => notice.case_name = non_empty(value),
        "case number" => {
            if let Some(case_no) = value.split_whitespace().next() {
                notice.case_no = case_no.to_string();
            }
        }
        "filer" => notice.filed_by = non_empty(value),
        "document number" => {
            let (number, href) = split_inline_link(value);
            notice.doc_no = number
                .split_whitespace()
                .next()
                .filter(|token| token.chars().next().is_some_and(|c| c.is_ascii_digit()))
                .map(str::to_string);
            if let Some(href) = href {
                inline_links.push(href);
            }
        }
        _ => {}
    }
}

/// `Document Number: 29 <https://…>` — the number and the link a plain-text
/// rendering prints beside it.
fn split_inline_link(value: &str) -> (&str, Option<String>) {
    match value.find("http") {
        Some(start) => {
            let link = value[start..]
                .split(|c: char| c.is_whitespace() || c == '>')
                .next()
                .unwrap_or("");
            (
                value[..start].trim_end_matches(['<', ' ']),
                Some(decode_entities(link).into_owned()),
            )
        }
        None => (value, None),
    }
}

/// `entered by Filer, Fern on …` or `received from Quill, Quentin entered on …`.
fn transaction_actor(rest: &str) -> Option<String> {
    let after = rest
        .split_once("entered by ")
        .or_else(|| rest.split_once("received from "))
        .map(|(_, after)| after)?;
    let end = after
        .find(" entered on ")
        .or_else(|| after.find(" on "))
        .unwrap_or(after.len());
    non_empty(&first_last(after[..end].trim()))
}

/// `Filer, Fern` → `Fern Filer`; anything else unchanged.
fn first_last(name: &str) -> String {
    match name.split_once(", ") {
        Some((last, first)) if !first.contains(',') => format!("{first} {last}"),
        _ => name.to_string(),
    }
}

fn read_document_line(line: &str, documents: &mut Vec<Document>) {
    if let Some(description) = strip_label(line, "Document description:") {
        documents.push(Document {
            description: description.to_string(),
            original_filename: None,
        });
    } else if let Some(filename) = strip_label(line, "Original filename:") {
        if let Some(document) = documents.last_mut() {
            let base = filename.rsplit(['\\', '/']).next().unwrap_or(filename);
            document.original_filename = non_empty(base);
        }
    }
}

fn strip_label<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    line.get(..label.len())
        .filter(|head| head.eq_ignore_ascii_case(label))
        .map(|_| line[label.len()..].trim())
}

/// One service-list line. A line with a name and addresses is a whole entry;
/// a name alone waits for the address line beneath it.
fn read_service_line<'a>(
    line: &'a str,
    pending_name: &mut Option<&'a str>,
    entries: &mut Vec<ServiceListEntry>,
) {
    if line.is_empty() {
        *pending_name = None;
        return;
    }
    let (name_part, emails) = split_emails(line);
    let name_part = name_part.trim().trim_end_matches([',', ';']).trim();
    if emails.is_empty() {
        if !name_part.is_empty() {
            *pending_name = Some(line);
        }
        return;
    }
    let name = if name_part.is_empty() {
        pending_name.take().unwrap_or("")
    } else {
        name_part
    };
    *pending_name = None;
    let (name, party) = match name.split_once(" on behalf of ") {
        Some((name, party)) => (name.trim(), non_empty(party)),
        None => (name.trim(), None),
    };
    if name.is_empty() {
        return;
    }
    entries.push(ServiceListEntry {
        name: name.to_string(),
        party,
        emails,
    });
}

/// The line with its email addresses removed, and those addresses lowercased.
fn split_emails(line: &str) -> (String, Vec<String>) {
    let mut rest = String::new();
    let mut emails = Vec::new();
    for token in line.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        let cleaned = token.trim_matches(|c: char| matches!(c, '<' | '>' | '(' | ')' | '"'));
        if is_email(cleaned) {
            let email = cleaned.to_ascii_lowercase();
            if !emails.contains(&email) {
                emails.push(email);
            }
        } else if !token.is_empty() {
            if !rest.is_empty() {
                rest.push(' ');
            }
            rest.push_str(token);
        }
    }
    (rest, emails)
}

fn is_email(token: &str) -> bool {
    let Some((local, domain)) = token.split_once('@') else {
        return false;
    };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// The non-empty heading lines just above `Notice of Electronic Filing`,
/// skipping the CM/ECF boilerplate paragraphs.
fn court_heading(lines: &[&str]) -> String {
    let mut parts: Vec<&str> = lines
        .iter()
        .rev()
        .filter(|line| !line.is_empty())
        .take_while(|line| !line.starts_with("***") && !line.ends_with('.'))
        .take(2)
        .copied()
        .collect();
    parts.reverse();
    parts.join(", ")
}

/// `Filed by Harbor Recovery Fund.` from the docket text.
fn filed_by_from_docket_text(text: &str) -> Option<String> {
    let start = text.find("Filed by ")? + "Filed by ".len();
    let rest = &text[start..];
    let end = rest.find(" (").unwrap_or(rest.len());
    non_empty(rest[..end].trim().trim_end_matches('.'))
}

fn is_free_look(link: &str) -> bool {
    link.contains("/doc1/")
}

fn parse_us_date(raw: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(raw, "%m/%d/%Y").ok()
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Reduce HTML to text lines in one pass: block tags (`<br>`, `<p>`, `<div>`,
/// `<tr>`) end a line, a table cell becomes a space, every other tag
/// disappears, and each `href` is collected in document order.
#[must_use]
pub fn html_to_text(html: &str) -> (String, Vec<String>) {
    let mut text = String::with_capacity(html.len() / 2);
    let mut links = Vec::new();
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        text.push_str(&decode_entities(&rest[..open]));
        let Some(close) = rest[open..].find('>') else {
            rest = "";
            break;
        };
        let tag = &rest[open + 1..open + close];
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase();
        match name.as_str() {
            "br" | "p" | "div" | "tr" | "table" | "li" | "h1" | "h2" | "h3" => text.push('\n'),
            "td" | "th" => text.push(' '),
            "a" => {
                if let Some(href) = attribute(tag, "href") {
                    links.push(decode_entities(href).into_owned());
                }
            }
            _ => {}
        }
        rest = &rest[open + close + 1..];
    }
    text.push_str(&decode_entities(rest));
    (text, links)
}

fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase();
    let at = lower.find(&format!("{name}="))? + name.len() + 1;
    let value = &tag[at..];
    let quote = value.chars().next()?;
    if quote == '"' || quote == '\'' {
        let inner = &value[1..];
        inner.find(quote).map(|end| &inner[..end])
    } else {
        value.split_whitespace().next()
    }
}

fn decode_entities(text: &str) -> Cow<'_, str> {
    if !text.contains('&') {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.replace("&nbsp;", " ")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&"),
    )
}

#[cfg(test)]
mod tests {
    use super::{html_to_text, parse_eml, parse_text, NefError, ServiceListEntry};
    use chrono::NaiveDate;

    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nef")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
    }

    #[test]
    fn a_district_notice_reads_its_entry_documents_and_service_list() {
        let notice = parse_eml(&fixture("district-motion.eml")).unwrap();

        assert_eq!(notice.court, "U.S. District Court, District of Nowhere");
        assert_eq!(notice.case_no, "9:26-cv-00042-XYZ");
        assert_eq!(notice.case_name.as_deref(), Some("Cruller v. Prine"));
        assert_eq!(notice.doc_no.as_deref(), Some("12"));
        assert_eq!(notice.filed_by.as_deref(), Some("Prine Bakery Supply LLC"));
        assert_eq!(notice.filer_attorney.as_deref(), Some("Fern Filer"));
        assert_eq!(notice.filed_at, NaiveDate::from_ymd_opt(2026, 3, 4));
        assert_eq!(
            notice.free_look_url.as_deref(),
            Some("https://ecf.example.com/doc1/0420001?caseid=77&de_seq_num=12&magic_num=4821")
        );
        assert_eq!(
            notice.original_filename.as_deref(),
            Some("motion-to-dismiss.pdf")
        );
        assert!(notice
            .docket_text
            .starts_with("MOTION to Dismiss for Failure to State a Claim by Defendant"));
        assert!(notice.docket_text.ends_with("(Filer, Fern)"));

        let attachments: Vec<_> = notice
            .attachments
            .iter()
            .map(|a| {
                (
                    a.number,
                    a.description.as_str(),
                    a.original_filename.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            attachments,
            vec![
                (1, "Exhibit A - Supply Agreement", Some("exhibit-a.pdf")),
                (2, "Proposed Order", Some("proposed-order.pdf")),
            ]
        );
        assert!(notice.attachments.iter().all(|a| a.free_look_url.is_none()));

        assert_eq!(
            notice.service_list,
            vec![
                ServiceListEntry {
                    name: "Fern Filer".into(),
                    party: None,
                    emails: vec!["fern.filer@example.com".into(), "docket@example.com".into()],
                },
                ServiceListEntry {
                    name: "Gus Gavel".into(),
                    party: None,
                    emails: vec!["gus.gavel@example.com".into()],
                },
                ServiceListEntry {
                    name: "Hana Hearsay".into(),
                    party: None,
                    emails: vec![
                        "hana.hearsay@example.com".into(),
                        "hh-docket@example.com".into(),
                        "paralegal@example.com".into(),
                    ],
                },
            ]
        );
    }

    #[test]
    fn a_bankruptcy_notice_reads_parties_and_an_inline_free_look_link() {
        let notice = parse_eml(&fixture("bankruptcy-transfer.eml")).unwrap();

        assert_eq!(notice.court, "U.S. Bankruptcy Court, District of Elsewhere");
        assert_eq!(notice.case_no, "26-10042-ZZZ");
        assert_eq!(notice.doc_no.as_deref(), Some("29"));
        assert_eq!(notice.filer_attorney.as_deref(), Some("Quentin Quill"));
        assert_eq!(notice.filed_by.as_deref(), Some("Harbor Recovery Fund"));
        assert_eq!(notice.filed_at, NaiveDate::from_ymd_opt(2026, 3, 5));
        assert_eq!(
            notice.free_look_url.as_deref(),
            Some("https://ecf.example.com/doc1/0310009?caseid=88&de_seq_num=301&magic_num=7313")
        );
        assert_eq!(
            notice.original_filename.as_deref(),
            Some("transfer-of-claim-7.pdf")
        );
        assert!(notice.attachments.is_empty());
        assert!(notice.docket_text.starts_with("Transfer of Claim."));

        let people: Vec<_> = notice
            .service_list
            .iter()
            .map(|e| (e.name.as_str(), e.party.as_deref(), e.primary_email()))
            .collect();
        assert_eq!(
            people,
            vec![
                (
                    "Quentin Quill",
                    Some("Creditor Harbor Recovery Fund"),
                    Some("quentin.quill@example.com")
                ),
                (
                    "Uma Upton",
                    Some("U.S. Trustee U.S. Trustee"),
                    Some("uma.upton@example.com")
                ),
                (
                    "Vera Venue",
                    Some("Debtor Widget Works LLC"),
                    Some("vera.venue@example.com")
                ),
            ],
            "the not-mailed block is no part of the service list"
        );
    }

    #[test]
    fn a_text_only_entry_has_no_document_number() {
        let text = "U.S. District Court\n\nDistrict of Nowhere\n\nNotice of Electronic Filing\n\n\
                    The following transaction was entered on 3/6/2026 at 8:00 AM PST and filed on 3/6/2026\n\
                    Case Name: Cruller v. Prine\nCase Number: 9:26-cv-00042-XYZ\n\
                    Document Number: No document attached\n\nDocket Text:\n\
                    MINUTE ORDER setting a hearing.\n\n";
        let notice = parse_text(text, &[]).unwrap();
        assert_eq!(notice.doc_no, None);
        assert_eq!(notice.filer_attorney, None);
        assert_eq!(notice.docket_text, "MINUTE ORDER setting a hearing.");
    }

    #[test]
    fn a_message_that_is_not_a_notice_is_refused() {
        assert_eq!(
            parse_eml(b"Subject: lunch\n\nSee you at noon.").unwrap_err(),
            NefError::NotANotice
        );
        assert_eq!(
            parse_text("Notice of Electronic Filing\nDocket Text:\nx\n", &[]).unwrap_err(),
            NefError::Missing("Case Number")
        );
    }

    #[test]
    fn html_to_text_ends_lines_at_block_tags_and_keeps_hrefs() {
        let (text, links) = html_to_text(
            "<p>A&nbsp;B</p><table><tr><td>x</td><td><a href=\"https://e.example/doc1/1?a=1&amp;b=2\">7</a></td></tr></table>",
        );
        let rows: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        assert_eq!(rows, vec!["A B", "x  7"]);
        assert_eq!(links, vec!["https://e.example/doc1/1?a=1&b=2".to_string()]);
    }

    /// A real service list runs to ~370k characters. The parser must stay
    /// linear over it: this bound is generous for a linear pass and far too
    /// tight for a quadratic one.
    #[test]
    fn a_370k_character_service_list_parses_in_linear_time() {
        use std::fmt::Write as _;
        let mut html = String::from(
            "<p>U.S. Bankruptcy Court</p><p>District of Elsewhere</p>\
             <p>Notice of Electronic Filing</p>\
             <div>The following transaction was received from Quill, Quentin entered on 3/5/2026 at 9:01 AM EST and filed on 3/5/2026</div>\
             <table><tr><td>Case Number:</td><td>26-10042-ZZZ</td></tr>\
             <tr><td>Document Number:</td><td><a href=\"https://ecf.example.com/doc1/1?magic_num=1\">30</a></td></tr></table>\
             <p>Docket Text:<br>Notice of Appearance.</p>\
             <p>26-10042-ZZZ Notice will be electronically mailed to:<br>",
        );
        let mut count = 0usize;
        while html.len() < 370_000 {
            write!(
                html,
                "<br>Person {count} on behalf of Creditor Holder {count}<br>person{count}@example.com, docket{count}@example.com<br>"
            )
            .unwrap();
            count += 1;
        }
        html.push_str("</p>");
        let message =
            format!("MIME-Version: 1.0\r\nContent-Type: text/html; charset=us-ascii\r\n\r\n{html}");

        let started = std::time::Instant::now();
        let notice = parse_eml(message.as_bytes()).unwrap();
        let elapsed = started.elapsed();

        assert_eq!(notice.service_list.len(), count);
        assert_eq!(
            notice.service_list.last().unwrap().primary_email(),
            Some(format!("person{}@example.com", count - 1).as_str())
        );
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "parsing {} characters took {elapsed:?}",
            message.len()
        );
    }
}
