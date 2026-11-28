//! `navigator project efiling` — record what an e-filing service provider
//! (EFSP) reported about a submission, and file the court-stamped copy it
//! returned.
//!
//! - `record <email.eml>` reads an EFSP status email, takes the transaction id
//!   from it, and records the status. A conformed PDF attached to an
//!   acceptance email is filed as `--kind filing` and linked to the
//!   transaction.
//! - `pair <pdf> --transaction <id>` files a stamped copy the attorney
//!   downloaded by hand and links it to the same transaction, instead of
//!   leaving it an unrelated upload.
//! - `list` prints the transactions recorded on the Project.
//!
//! A notification-only email — one that names the transaction but not its
//! status — records `unknown`, never `accepted`. Every write goes through the
//! portal API and prints the row the store read back.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::NaiveDate;
use mail_parser::MessageParser;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    projects::{manifest, surfaces},
    remote,
};

/// What an EFSP status email says about one transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EfspEmail {
    pub transaction_id: String,
    /// `submitted`, `accepted`, `rejected`, or `unknown`.
    pub status: &'static str,
    pub efsp: Option<String>,
    pub court: Option<String>,
    pub case_number: Option<String>,
    pub filed_on: Option<String>,
    pub accepted_on: Option<String>,
    pub note: Option<String>,
    /// The first PDF attachment: the conformed copy on an acceptance email.
    pub attachment: Option<Attachment>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Attachment {
    pub filename: String,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for Attachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attachment")
            .field("filename", &self.filename)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// Parse an EFSP status email.
///
/// # Errors
/// The bytes are not a MIME message, or the message names no transaction id.
pub fn parse_email(raw: &[u8]) -> Result<EfspEmail> {
    let message = MessageParser::default()
        .parse(raw)
        .ok_or_else(|| anyhow!("not a MIME email message"))?;
    let mut text = String::new();
    if let Some(subject) = message.subject() {
        text.push_str(subject);
        text.push('\n');
    }
    if let Some(body) = message.body_text(0) {
        text.push_str(&body);
    }
    let transaction_id = transaction_id(&text)
        .or_else(|| envelope_number(&text))
        .ok_or_else(|| anyhow!("the email names no e-filing envelope or transaction id"))?;
    let fields = labelled_fields(&text);
    let field = |names: &[&str]| {
        fields
            .iter()
            .find(|(label, _)| names.contains(&label.as_str()))
            .map(|(_, value)| value.clone())
    };
    let status = field(&["status", "filing status", "transaction status"])
        .and_then(|value| status_word(&value))
        .or_else(|| message.subject().and_then(status_subject))
        .or_else(|| status_phrase(&text))
        .unwrap_or(model::efiling_transactions::STATUS_UNKNOWN);
    let decided = status == model::efiling_transactions::STATUS_ACCEPTED;
    let filed_on = decided
        .then(|| field(&["date filed", "filed date", "filing date", "filed on"]))
        .flatten()
        .and_then(|value| parse_date(&value));
    let accepted_on = decided
        .then(|| {
            field(&["date accepted", "accepted date", "accepted on"])
                .and_then(|value| parse_date(&value))
                .or_else(|| {
                    message.date().and_then(|date| {
                        NaiveDate::from_ymd_opt(
                            i32::from(date.year),
                            u32::from(date.month),
                            u32::from(date.day),
                        )
                        .map(|date| date.format("%Y-%m-%d").to_string())
                    })
                })
        })
        .flatten();
    let note = (status == model::efiling_transactions::STATUS_REJECTED)
        .then(|| {
            field(&[
                "rejection reason",
                "reason for rejection",
                "reason",
                "rejection comments",
            ])
        })
        .flatten();
    let efsp = message
        .from()
        .and_then(|from| from.first())
        .and_then(|from| {
            from.name()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    from.address()
                        .and_then(|address| address.rsplit_once('@'))
                        .map(|(_, domain)| domain.to_string())
                })
        });
    let attachment = message.attachments().find_map(|part| {
        let filename = part
            .attachment_name()
            .unwrap_or("conformed.pdf")
            .to_string();
        let is_pdf = part
            .content_type()
            .is_some_and(|ct| ct.ctype() == "application" && ct.subtype() == Some("pdf"))
            || filename.to_ascii_lowercase().ends_with(".pdf");
        is_pdf.then(|| Attachment {
            filename,
            bytes: part.decoded().into_owned(),
        })
    });
    Ok(EfspEmail {
        transaction_id,
        status,
        efsp,
        court: field(&["court", "court name", "court location"]),
        case_number: field(&["case number", "case no.", "case no", "case #"]),
        filed_on,
        accepted_on,
        note,
        attachment,
    })
}

/// Fallback for provider notices that name an envelope without a transaction id.
fn envelope_number(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    [
        "envelope number",
        "envelope no.",
        "envelope no",
        "envelope #",
    ]
    .iter()
    .find_map(|label| {
        lower.match_indices(label).find_map(|(start, _)| {
            if start > 0 && lower.as_bytes()[start - 1].is_ascii_alphanumeric() {
                return None;
            }
            let rest = text[start + label.len()..].trim_start();
            let value = if label.ends_with('#') {
                rest.trim_start_matches(':').trim_start()
            } else {
                rest.strip_prefix([':', '#'])?.trim_start()
            };
            let end = value
                .find(|ch: char| !ch.is_ascii_alphanumeric() && !matches!(ch, '-' | '_'))
                .unwrap_or(value.len());
            let id = value[..end].trim_end_matches(['-', '_']);
            id.bytes()
                .any(|byte| byte.is_ascii_digit())
                .then(|| id.to_owned())
        })
    })
}

/// The token after the first `transaction [id|number|no.|#] :` in `text`.
fn transaction_id(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find("transaction") {
        let start = from + found + "transaction".len();
        from = start;
        let mut rest = text[start..].trim_start();
        for qualifier in ["id", "number", "no.", "no", "#"] {
            if rest
                .get(..qualifier.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(qualifier))
            {
                rest = rest[qualifier.len()..].trim_start();
                break;
            }
        }
        let Some(rest) = rest.strip_prefix([':', '#']) else {
            continue;
        };
        let token: String = rest
            .trim_start()
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
            .collect();
        let token = token.trim_end_matches(['-', '_']);
        if token.chars().any(|ch| ch.is_ascii_digit()) {
            return Some(token.to_string());
        }
    }
    None
}

/// Every `Label: value` line, label lowercased and trimmed.
fn labelled_fields(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (label, value) = line.split_once(':')?;
            let label = label.trim().trim_start_matches(['*', '-']).trim();
            let value = value.trim();
            (!label.is_empty() && label.len() <= 40 && !value.is_empty())
                .then(|| (label.to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}

fn status_word(value: &str) -> Option<&'static str> {
    let value = value.trim().to_ascii_lowercase();
    let first = value
        .split(|ch: char| !ch.is_ascii_alphabetic())
        .find(|word| !word.is_empty())?;
    match first {
        "accepted" | "filed" => Some(model::efiling_transactions::STATUS_ACCEPTED),
        "rejected" | "returned" => Some(model::efiling_transactions::STATUS_REJECTED),
        "submitted" | "pending" | "received" | "processing" => {
            Some(model::efiling_transactions::STATUS_SUBMITTED)
        }
        _ => None,
    }
}

fn status_subject(subject: &str) -> Option<&'static str> {
    let subject = subject.trim().to_ascii_lowercase();
    [
        (
            "filing accepted",
            model::efiling_transactions::STATUS_ACCEPTED,
        ),
        (
            "filing rejected",
            model::efiling_transactions::STATUS_REJECTED,
        ),
        (
            "filing submitted",
            model::efiling_transactions::STATUS_SUBMITTED,
        ),
    ]
    .into_iter()
    .find_map(|(label, status)| {
        subject
            .strip_prefix(label)
            .filter(|rest| {
                rest.is_empty() || rest.starts_with(|ch: char| !ch.is_ascii_alphanumeric())
            })
            .map(|_| status)
    })
}

fn status_phrase(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("has been accepted") {
        Some(model::efiling_transactions::STATUS_ACCEPTED)
    } else if lower.contains("has been rejected") {
        Some(model::efiling_transactions::STATUS_REJECTED)
    } else {
        None
    }
}

/// `YYYY-MM-DD` from the first date-shaped token in `value`.
fn parse_date(value: &str) -> Option<String> {
    value.split_whitespace().find_map(|token| {
        let token = token.trim_matches(|ch: char| !ch.is_ascii_alphanumeric());
        NaiveDate::parse_from_str(token, "%Y-%m-%d")
            .or_else(|_| NaiveDate::parse_from_str(token, "%m/%d/%Y"))
            .ok()
            .map(|date| date.format("%Y-%m-%d").to_string())
    })
}

/// The slug a transaction's conformed copy is filed under, and its filename.
#[must_use]
pub fn conformed_slug(transaction_id: &str) -> (String, String) {
    let safe: String = transaction_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let filename = format!("{safe}.pdf");
    (format!("efilings/{filename}"), filename)
}

/// Overrides the caller names on the command line. Each wins over the email.
#[derive(Debug, Default, Clone)]
pub struct Overrides {
    pub submitted: Option<Uuid>,
    pub efsp: Option<String>,
    pub court: Option<String>,
    pub case_number: Option<String>,
    pub filed_on: Option<String>,
    pub judge_signed: bool,
    pub fee_receipt: Option<Uuid>,
}

struct Coordinates {
    base: String,
    token: String,
    code: String,
    project_id: Uuid,
}

async fn coordinates(host: Option<&str>, project: Option<&str>) -> Result<Coordinates> {
    let local = manifest::read(Path::new("."));
    let code = project
        .or_else(|| local.as_ref().and_then(|m| m.project.as_deref()))
        .ok_or_else(|| anyhow!("pass --project or run from a Project checkout"))?
        .to_string();
    let host = host.or_else(|| local.as_ref().and_then(|m| m.host.as_deref()));
    let (base, token) = remote::resolve(host)?;
    let project_id = surfaces::resolve_project_id(&base, &token, &code).await?;
    Ok(Coordinates {
        base,
        token,
        code,
        project_id,
    })
}

async fn call(request: reqwest::RequestBuilder, action: &str) -> Result<Value> {
    let response = request
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .with_context(|| format!("{action} e-filing transaction"))?;
    let status = response.status();
    let body = response.text().await.context("read e-filing response")?;
    if !status.is_success() {
        return Err(anyhow!("e-filing {action} refused: {status} {body}"));
    }
    serde_json::from_str(&body).context("parse e-filing response")
}

async fn list_rows(at: &Coordinates) -> Result<Value> {
    call(
        reqwest::Client::new()
            .get(format!(
                "{}/app/api/projects/{}/efilings",
                at.base, at.project_id
            ))
            .bearer_auth(&at.token),
        "list",
    )
    .await
}

async fn find_row(at: &Coordinates, transaction_id: &str) -> Result<Option<Value>> {
    Ok(list_rows(at)
        .await?
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["transaction_id"].as_str() == Some(transaction_id))
        })
        .cloned())
}

async fn post_row(at: &Coordinates, body: &Value) -> Result<Value> {
    call(
        reqwest::Client::new()
            .post(format!(
                "{}/app/api/projects/{}/efilings",
                at.base, at.project_id
            ))
            .bearer_auth(&at.token)
            .json(body),
        "record",
    )
    .await
}

/// What a conformed-copy upload is filed with.
pub struct ConformedUpload<'a> {
    pub transaction_id: &'a str,
    pub bytes: &'a [u8],
    pub filed_on: &'a str,
    pub submitted: Option<&'a str>,
    pub judge_signed: bool,
}

/// File a conformed copy as `--kind filing` through the document upload seam
/// and return its asset id.
async fn upload_conformed(at: &Coordinates, upload: &ConformedUpload<'_>) -> Result<Uuid> {
    let client =
        remote::DocumentClient::with_credential(at.base.clone(), at.token.clone(), &at.code)
            .await?;
    let (slug, filename) = conformed_slug(upload.transaction_id);
    let what = if upload.judge_signed {
        "Received copy (entered order still needed)"
    } else {
        "Conformed copy"
    };
    let description = format!(
        "{what} of e-filing transaction {}, filed {}",
        upload.transaction_id, upload.filed_on
    );
    let metadata = json!({
        "efiling_transaction_id": upload.transaction_id,
        "filed_on": upload.filed_on,
        "submitted_document_id": upload.submitted,
    });
    let pointer = client
        .upload_bytes(
            &filename,
            upload.bytes,
            "filing",
            Some("internal"),
            Some(&description),
            Some("application/pdf"),
            Some(&slug),
            Some(metadata),
        )
        .await?;
    eprintln!("filed {slug}");
    Ok(pointer.current_version.asset_id)
}

/// Refuse a conformed copy that cannot be linked to a submitted version: one
/// for a transaction already rejected, or for a transaction that names no
/// submitted revision when the caller did not name one either.
fn require_linkable(
    existing: Option<&Value>,
    submitted: Option<Uuid>,
    transaction: &str,
) -> Result<()> {
    if existing.and_then(|row| row["status"].as_str())
        == Some(model::efiling_transactions::STATUS_REJECTED)
    {
        return Err(anyhow!(
            "transaction {transaction} is recorded as rejected; a stamped copy cannot be filed against it"
        ));
    }
    let linked =
        submitted.is_some() || existing.is_some_and(|row| row["submitted_document_id"].is_string());
    if !linked {
        return Err(anyhow!(
            "transaction {transaction} names no submitted version; pass --submitted <asset id> so the \
             stamped copy links to it"
        ));
    }
    Ok(())
}

fn insert_some(body: &mut Value, key: &str, value: Option<impl Into<Value>>) {
    if let (Some(object), Some(value)) = (body.as_object_mut(), value) {
        object.insert(key.to_string(), value.into());
    }
}

fn finish(verb: &str, result: Result<Value>) -> ExitCode {
    match result {
        Ok(row) => {
            println!("{row}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator project efiling {verb}: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// `navigator project efiling list`
pub async fn list(host: Option<&str>, project: Option<&str>) -> ExitCode {
    finish(
        "list",
        async {
            let at = coordinates(host, project).await?;
            list_rows(&at).await
        }
        .await,
    )
}

#[derive(serde::Serialize)]
pub struct FeeMatch {
    pub envelope: String,
    pub account_id: Uuid,
    pub transaction_id: Uuid,
    pub confirm: bool,
}

/// Confirm a reviewed match and let the server verify the exact bank charge.
pub async fn settle_fee(
    host: Option<&str>,
    project: Option<&str>,
    fee_match: FeeMatch,
) -> ExitCode {
    finish(
        "settle fee",
        async {
            let at = coordinates(host, project).await?;
            call(
                reqwest::Client::new()
                    .post(format!(
                        "{}/app/api/projects/{}/efilings/settle-fee",
                        at.base, at.project_id
                    ))
                    .bearer_auth(&at.token)
                    .json(&fee_match),
                "settle fee",
            )
            .await
        }
        .await,
    )
}

/// `navigator project efiling record <email.eml>`
pub async fn record(
    host: Option<&str>,
    project: Option<&str>,
    email: &Path,
    overrides: Overrides,
) -> ExitCode {
    finish(
        "record",
        record_email(host, project, email, overrides).await,
    )
}

async fn file_email_copy(
    at: &Coordinates,
    transaction_id: &str,
    attachment: &Attachment,
    filed_on: &str,
    overrides: &Overrides,
) -> Result<Uuid> {
    let existing = find_row(at, transaction_id).await?;
    require_linkable(existing.as_ref(), overrides.submitted, transaction_id)?;
    let submitted = overrides.submitted.map(|id| id.to_string()).or_else(|| {
        existing
            .as_ref()
            .and_then(|row| row["submitted_document_id"].as_str())
            .map(str::to_string)
    });
    upload_conformed(
        at,
        &ConformedUpload {
            transaction_id,
            bytes: &attachment.bytes,
            filed_on,
            submitted: submitted.as_deref(),
            judge_signed: overrides.judge_signed,
        },
    )
    .await
}

async fn record_email(
    host: Option<&str>,
    project: Option<&str>,
    email: &Path,
    overrides: Overrides,
) -> Result<Value> {
    let raw = std::fs::read(email).with_context(|| format!("read {}", email.display()))?;
    let parsed = parse_email(&raw)?;
    let accepted = parsed.status == model::efiling_transactions::STATUS_ACCEPTED;
    let filed_on = overrides.filed_on.clone().or(parsed.filed_on.clone());
    if !accepted && overrides.filed_on.is_some() {
        return Err(anyhow!(
            "the email reports {}, so --filed-on would record a pending filing as filed",
            parsed.status
        ));
    }
    if !accepted && overrides.fee_receipt.is_some() {
        return Err(anyhow!("a fee receipt requires an accepted envelope"));
    }
    let conformed = parsed.attachment.as_ref().filter(|_| accepted);
    if let Some(attachment) = &parsed.attachment {
        if !accepted {
            eprintln!(
                "not filing {}: the email reports {}, not accepted",
                attachment.filename, parsed.status
            );
        }
    }
    if conformed.is_some() && filed_on.is_none() {
        return Err(anyhow!(
            "the email attaches a conformed copy but names no court-filed date; pass --filed-on YYYY-MM-DD"
        ));
    }

    let at = coordinates(host, project).await?;
    let conformed_id = match (conformed, filed_on.as_deref()) {
        (Some(attachment), Some(filed_on)) => Some(
            file_email_copy(
                &at,
                &parsed.transaction_id,
                attachment,
                filed_on,
                &overrides,
            )
            .await?,
        ),
        _ => None,
    };

    let mut body = json!({
        "transaction_id": parsed.transaction_id,
        "status": parsed.status,
    });
    insert_some(
        &mut body,
        "fee_receipt_document_id",
        overrides.fee_receipt.map(|id| id.to_string()),
    );
    insert_some(&mut body, "efsp", overrides.efsp.or(parsed.efsp));
    insert_some(&mut body, "court", overrides.court.or(parsed.court));
    insert_some(
        &mut body,
        "case_number",
        overrides.case_number.or(parsed.case_number),
    );
    insert_some(
        &mut body,
        "submitted_document_id",
        overrides.submitted.map(|id| id.to_string()),
    );
    if accepted {
        insert_some(&mut body, "filed_on", filed_on);
        insert_some(&mut body, "accepted_on", parsed.accepted_on);
    }
    insert_some(
        &mut body,
        "conformed_document_id",
        conformed_id.map(|id| id.to_string()),
    );
    insert_some(&mut body, "note", parsed.note);
    if overrides.judge_signed {
        mark_judge_signed(&mut body);
    }
    post_row(&at, &body).await
}

/// A judge-signed document comes back only as a received copy: flag the row
/// until the entered order is obtained.
fn mark_judge_signed(body: &mut Value) {
    body["needs_entered_order"] = json!(true);
    if body.get("note").is_none() {
        body["note"] = json!(JUDGE_SIGNED_NOTE);
    }
}

const JUDGE_SIGNED_NOTE: &str =
    "received copy of a judge-signed document; obtain the entered order";

/// `navigator project efiling pair <pdf> --transaction <id>`
pub async fn pair(
    host: Option<&str>,
    project: Option<&str>,
    file: &Path,
    transaction: &str,
    accepted_on: Option<String>,
    overrides: Overrides,
) -> ExitCode {
    finish(
        "pair",
        pair_file(host, project, file, transaction, accepted_on, overrides).await,
    )
}

async fn pair_file(
    host: Option<&str>,
    project: Option<&str>,
    file: &Path,
    transaction: &str,
    accepted_on: Option<String>,
    overrides: Overrides,
) -> Result<Value> {
    let transaction = model::efiling_transactions::normalize_transaction_id(transaction)
        .map_err(|error| anyhow!("{error}"))?;
    let filed_on = overrides
        .filed_on
        .clone()
        .ok_or_else(|| anyhow!("pass --filed-on YYYY-MM-DD: the date the court deems it filed"))?;
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    if !bytes.starts_with(b"%PDF") {
        return Err(anyhow!("{} is not a PDF", file.display()));
    }

    let at = coordinates(host, project).await?;
    let existing = find_row(&at, &transaction).await?;
    require_linkable(existing.as_ref(), overrides.submitted, &transaction)?;
    let submitted = overrides.submitted.map(|id| id.to_string()).or_else(|| {
        existing
            .as_ref()
            .and_then(|row| row["submitted_document_id"].as_str())
            .map(str::to_string)
    });
    let conformed = upload_conformed(
        &at,
        &ConformedUpload {
            transaction_id: &transaction,
            bytes: &bytes,
            filed_on: &filed_on,
            submitted: submitted.as_deref(),
            judge_signed: overrides.judge_signed,
        },
    )
    .await?;

    let mut body = json!({
        "transaction_id": transaction,
        "status": model::efiling_transactions::STATUS_ACCEPTED,
        "filed_on": filed_on,
        "conformed_document_id": conformed.to_string(),
    });
    insert_some(&mut body, "efsp", overrides.efsp);
    insert_some(&mut body, "court", overrides.court);
    insert_some(&mut body, "case_number", overrides.case_number);
    insert_some(
        &mut body,
        "submitted_document_id",
        overrides.submitted.map(|id| id.to_string()),
    );
    insert_some(&mut body, "accepted_on", accepted_on);
    if overrides.judge_signed {
        mark_judge_signed(&mut body);
    }
    post_row(&at, &body).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCEPTED: &[u8] = include_bytes!("../../tests/fixtures/efiling/accepted.eml");
    const REJECTED: &[u8] = include_bytes!("../../tests/fixtures/efiling/rejected.eml");
    const NOTIFICATION: &[u8] = include_bytes!("../../tests/fixtures/efiling/notification.eml");

    #[test]
    fn envelope_notices_share_id_and_leave_service_notifications_unknown() {
        for (subject, status) in [
            ("Filing Submitted", "submitted"),
            ("Notification of Service", "unknown"),
            ("Filing Accepted", "accepted"),
        ] {
            let eml = format!(
                "From: Filing Service <filing@example.test>\r\nSubject: {subject}\r\n\r\n\
                 Envelope Number: TEST-4242\r\n"
            );
            let parsed = super::parse_email(eml.as_bytes()).unwrap();
            assert_eq!(parsed.transaction_id, "TEST-4242");
            assert_eq!(parsed.status, status);
        }
    }

    #[test]
    fn existing_transaction_label_keeps_its_key_when_an_envelope_is_also_named() {
        let raw = b"From: Filing Service <filing@example.test>\r\nSubject: Filing Accepted\r\n\r\n\
                    Envelope Number: TEST-4242\r\nTransaction Number: TEST-7423\r\n";
        assert_eq!(super::parse_email(raw).unwrap().transaction_id, "TEST-7423");
    }

    #[test]
    fn an_acceptance_email_yields_the_filed_date_and_the_conformed_copy() {
        let email = parse_email(ACCEPTED).unwrap();
        assert_eq!(email.transaction_id, "EX-20261005-0042");
        assert_eq!(email.status, "accepted");
        assert_eq!(email.efsp.as_deref(), Some("Example EFSP"));
        assert_eq!(
            email.court.as_deref(),
            Some("Superior Court of California, County of Example")
        );
        assert_eq!(email.case_number.as_deref(), Some("26EXCV00042"));
        assert_eq!(email.filed_on.as_deref(), Some("2026-10-05"));
        assert_eq!(email.accepted_on.as_deref(), Some("2026-10-06"));
        let attachment = email.attachment.unwrap();
        assert_eq!(attachment.filename, "conformed-motion.pdf");
        assert!(attachment.bytes.starts_with(b"%PDF-1.4"));
    }

    #[test]
    fn a_rejection_email_yields_its_reason_and_no_filed_date() {
        let email = parse_email(REJECTED).unwrap();
        assert_eq!(email.transaction_id, "EX-20261005-0043");
        assert_eq!(email.status, "rejected");
        assert_eq!(email.filed_on, None);
        assert_eq!(email.accepted_on, None);
        assert_eq!(
            email.note.as_deref(),
            Some("Document is missing the attorney signature.")
        );
        assert_eq!(email.attachment, None);
    }

    #[test]
    fn a_notification_only_email_is_unknown_never_accepted() {
        let email = parse_email(NOTIFICATION).unwrap();
        assert_eq!(email.transaction_id, "EX-20261005-0044");
        assert_eq!(email.status, "unknown");
        assert_eq!(email.filed_on, None);
        assert_eq!(email.accepted_on, None);
        assert_eq!(email.attachment, None);
    }

    #[test]
    fn an_email_without_a_transaction_id_is_refused() {
        let raw =
            b"From: Example EFSP <noreply@example.com>\r\nSubject: Hello\r\n\r\nNothing here.\r\n";
        assert!(parse_email(raw).is_err());
    }

    #[test]
    fn the_conformed_slug_is_stable_and_slug_safe() {
        assert_eq!(
            conformed_slug("EX-20261005-0042"),
            (
                "efilings/EX-20261005-0042.pdf".to_string(),
                "EX-20261005-0042.pdf".to_string()
            )
        );
        assert_eq!(conformed_slug("A/B 1").0, "efilings/A-B-1.pdf");
    }

    #[test]
    fn dates_parse_in_both_court_shapes() {
        assert_eq!(
            parse_date("10/05/2026 4:59 PM").as_deref(),
            Some("2026-10-05")
        );
        assert_eq!(parse_date("2026-10-05").as_deref(), Some("2026-10-05"));
        assert_eq!(parse_date("pending"), None);
    }

    use crate::projects::test_login::{site, Login};
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const CODE: &str = "sample-matter";

    /// A signed-in site with one matter, a directory of fixtures, and the
    /// matter's e-filing door.
    async fn matter() -> (MockServer, Login, tempfile::TempDir, String) {
        let id = Uuid::new_v4();
        let (server, login) = site(json!([{"id": id, "code": CODE}])).await;
        let files = tempfile::tempdir().unwrap();
        for (name, bytes) in [
            ("accepted.eml", ACCEPTED),
            ("rejected.eml", REJECTED),
            ("stamped.pdf", b"%PDF-1.4 stamped".as_slice()),
            ("notes.txt", b"not a pdf".as_slice()),
        ] {
            std::fs::write(files.path().join(name), bytes).unwrap();
        }
        (
            server,
            login,
            files,
            format!("/app/api/projects/{id}/efilings"),
        )
    }

    #[tokio::test]
    async fn a_rejection_is_recorded_with_its_reason_and_the_callers_overrides() {
        let (server, _login, files, door) = matter().await;
        let host = server.uri();
        let submitted = Uuid::new_v4();
        Mock::given(method("POST"))
            .and(path(door.as_str()))
            .and(body_json(json!({
                "transaction_id": "EX-20261005-0043",
                "status": "rejected",
                "efsp": "Override EFSP",
                "court": "Override Court",
                "case_number": "26EXCV00099",
                "submitted_document_id": submitted.to_string(),
                "note": "Document is missing the attorney signature.",
                "needs_entered_order": true,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "rejected"})))
            .mount(&server)
            .await;
        let overrides = Overrides {
            submitted: Some(submitted),
            efsp: Some("Override EFSP".into()),
            court: Some("Override Court".into()),
            case_number: Some("26EXCV00099".into()),
            filed_on: None,
            judge_signed: true,
            fee_receipt: None,
        };
        let email = files.path().join("rejected.eml");
        let recorded = record(Some(&host), Some(CODE), &email, overrides.clone()).await;
        assert_eq!(recorded, ExitCode::SUCCESS);

        let dated = Overrides {
            filed_on: Some("2026-10-05".into()),
            ..overrides
        };
        let error = record_email(Some(&host), Some(CODE), &email, dated)
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("--filed-on would record a pending filing as filed"));
        let missing = files.path().join("absent.eml");
        let unread = record(Some(&host), Some(CODE), &missing, Overrides::default()).await;
        assert_eq!(unread, ExitCode::FAILURE);
    }

    #[tokio::test]
    async fn a_stamped_copy_is_filed_only_against_a_linkable_transaction() {
        let (server, _login, files, door) = matter().await;
        let host = Some(server.uri());
        let host = host.as_deref();
        let email = files.path().join("accepted.eml");
        let pdf = files.path().join("stamped.pdf");
        let dated = || Overrides {
            filed_on: Some("2026-10-05".into()),
            ..Overrides::default()
        };
        assert_eq!(list(host, Some(CODE)).await, ExitCode::FAILURE);
        for (rows, expected) in [
            (json!([]), "names no submitted version"),
            (
                json!([{"transaction_id": "EX-20261005-0042", "status": "rejected"}]),
                "is recorded as rejected",
            ),
        ] {
            let _rows = Mock::given(method("GET"))
                .and(path(door.as_str()))
                .respond_with(ResponseTemplate::new(200).set_body_json(rows))
                .mount_as_scoped(&server)
                .await;
            assert_eq!(list(host, Some(CODE)).await, ExitCode::SUCCESS);
            let error = record_email(host, Some(CODE), &email, Overrides::default())
                .await
                .unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
            let error = pair_file(host, Some(CODE), &pdf, "EX-20261005-0042", None, dated())
                .await
                .unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }

        let notes = files.path().join("notes.txt");
        for (file, transaction, overrides, expected) in [
            (&pdf, " ", dated(), ""),
            (
                &pdf,
                "EX-1",
                Overrides::default(),
                "pass --filed-on YYYY-MM-DD",
            ),
            (&notes, "EX-1", dated(), "is not a PDF"),
        ] {
            let error = pair_file(host, Some(CODE), file, transaction, None, overrides)
                .await
                .unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
        let paired = pair(host, None, &pdf, "EX-1", None, dated()).await;
        assert_eq!(paired, ExitCode::FAILURE);
    }

    #[test]
    fn a_judge_signed_row_keeps_an_existing_note() {
        let mut body = json!({"note": "from the clerk"});
        mark_judge_signed(&mut body);
        assert_eq!(
            body,
            json!({"note": "from the clerk", "needs_entered_order": true})
        );
        let mut body = json!({});
        mark_judge_signed(&mut body);
        assert_eq!(body["note"], JUDGE_SIGNED_NOTE);
    }
}
