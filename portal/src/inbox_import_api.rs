//! The inbox import door (LAW-238 D1): `POST /app/api/inbox/import` walks one
//! verbatim correspondence payload — an RFC 5322 `.eml`, or the
//! `messages.csv` of a LinkedIn data export — through a
//! [`store::inbox_import::ImportSource`] adapter and the direction-aware
//! postgres ingest (`store::inbox::ingest_imported`). Dedup, threading, and
//! attribution are the store's own machinery; this door only authenticates
//! the caller, resolves the Firm, archives the raw payload, and files email
//! attachments with provenance.
//!
//! Imports are ingest-only: there is no send verb anywhere on this surface.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};

/// One request = one payload. `project` is a matter *code* (`24-1043`);
/// attribution at import rides `store::inbox::assign` after ingest.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InboxImportRequest {
    /// Which adapter parses the payload (`email` | `linkedin`).
    source: String,
    /// The importing account — the lawyer's mailbox address, or their
    /// LinkedIn profile URL. Becomes the Inbox `provider_account`.
    mailbox: String,
    /// The verbatim payload bytes, base64-encoded.
    payload_base64: String,
    /// Optional matter code; conversations attribute (or move) there.
    project: Option<String>,
    /// Report what the import would do and write nothing — the CLI's
    /// `--dry-run` rides this field (AC11).
    #[serde(default)]
    dry_run: bool,
}

/// One ingested message as the door reports it — the ids a re-run compares
/// for idempotency (AC3), plus what was filed from it.
#[derive(Debug, Serialize)]
struct ImportedMessage {
    message_id: Uuid,
    conversation_id: Uuid,
    /// True when this payload had already been imported — the existing ids
    /// are echoed back and nothing was written.
    deduped: bool,
    direction: String,
    /// Email attachments filed from this message (LinkedIn carries none).
    attachments: Vec<ImportedAttachment>,
}

#[derive(Debug, Serialize)]
struct ImportedAttachment {
    filename: String,
    content_type: String,
    size_bytes: usize,
    /// The asset the bytes filed as — `mail/{message_id}/{index}`.
    asset_id: Uuid,
}

#[derive(Debug, Serialize)]
struct InboxImportResponse {
    source: String,
    mailbox: String,
    /// Object-storage key of the archived verbatim payload (AC6).
    archived_key: String,
    /// The matter code attribution ran against, when the request carried one.
    project: Option<String>,
    imported: Vec<ImportedMessage>,
}

/// `dry_run: true` — what the door would do, with nothing written anywhere
/// (no archive, no rows, no filings).
#[derive(Debug, Serialize)]
struct InboxImportPreview {
    source: String,
    mailbox: String,
    dry_run: bool,
    would_import: usize,
    project: Option<String>,
}

/// A `400 Bad Request` JSON body in the shared `{error, message}` shape —
/// its own copy of `api::bad_request`, which is private to that module.
fn bad_request(error: &'static str, message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": error, "message": message })),
    )
        .into_response()
}

/// Map a store-level ingest fault onto an API error. A denied or missing
/// record is the caller's problem (4xx); everything else is a deployment
/// fault the operator reads in the logs (500) — never matched by message
/// text.
fn inbox_error(error: &store::inbox::InboxError) -> ApiError {
    use store::inbox::InboxError as E;
    match error {
        E::Denied => ApiError::Forbidden,
        E::NotFound | E::CrossFirm => ApiError::NotFound,
        _ => ApiError::Db(error.to_string()),
    }
}

/// The caller's practice Firm: exactly one membership, or name the
/// ambiguity and ask for a matter code.
async fn resolve_firm(state: &ApiState, person_id: Uuid) -> Result<Uuid, Response> {
    let memberships = store::firms::memberships_for_person(&state.surreal, person_id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()).into_response())?;
    let mut firms = memberships
        .iter()
        .map(|membership| membership.firm_id)
        .collect::<Vec<_>>();
    firms.sort_unstable();
    firms.dedup();
    match firms.as_slice() {
        [firm_id] => Ok(*firm_id),
        [] => Err(bad_request(
            "no_firm",
            "This account belongs to no Firm, so imports have nowhere to land.",
        )),
        _ => Err(bad_request(
            "firm_ambiguous",
            "This account belongs to more than one Firm; pass `project` to name the matter.",
        )),
    }
}

/// `POST /app/api/inbox/import` — import one correspondence payload into the
/// Firm Inbox.
///
/// Lawyer-tier only (the [`LawyerSession`] extractor answers 401/403 before
/// the body is parsed). `400` when the source is unknown, the payload is not
/// valid base64 or not parseable by its adapter (AC9 — a mis-selected file
/// or a changed LinkedIn export shape fails loudly, naming the offending
/// headers), or the Firm/matter resolution is ambiguous. `201` carries one
/// summary per ingested message; replaying the same payload reports the
/// existing ids with `deduped: true` and writes nothing (AC3).
/// The payload's bytes: `payload_base64` under the strict standard alphabet.
/// A bad encoding is a 400 — the caller picked the wrong file or truncated it.
fn decode_payload(encoded: &str) -> Result<Vec<u8>, Response> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .map_err(|_| bad_request("invalid_base64", "payload_base64 is not valid base64."))
}

/// The Firm the import lands in — the named matter's Firm when `project`
/// is given, else the caller's own practice Firm. A named matter must be
/// one the caller can see: a matter code is not an authorization grant.
async fn resolve_firm_and_project(
    state: &ApiState,
    lawyer: &LawyerSession,
    person_id: Uuid,
    project: Option<&str>,
) -> Result<(Uuid, Option<Uuid>), Response> {
    let Some(code) = project.map(str::trim).filter(|code| !code.is_empty()) else {
        let firm_id = match resolve_firm(state, person_id).await {
            Ok(firm) => firm,
            // A firm-resolution 400 — already in the shared error shape.
            Err(response) => return Err(response),
        };
        return Ok((firm_id, None));
    };
    let Some(project) = store::projects::find_by_code(&state.surreal, code)
        .await
        .map_err(|error| ApiError::Db(error.to_string()).into_response())?
    else {
        return Err(bad_request(
            "project_not_found",
            "No Project carries that matter code.",
        ));
    };
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.surreal,
        Some(person_id),
        lawyer.0.role,
        project.id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound.into_response());
    }
    let project_firm = project.firm_id.ok_or_else(|| {
        bad_request(
            "project_has_no_firm",
            "That matter belongs to no Firm, so imports cannot land on it.",
        )
    })?;
    Ok((project_firm, Some(project.id)))
}

/// Archive the verbatim payload (AC6): one key for the .eml, or the CSV
/// once for the whole export — direction-neutral `mail/` prefix, recorded
/// on every Message this import creates.
async fn archive_payload(
    state: &ApiState,
    channel: &str,
    mailbox: &str,
    bytes: &[u8],
) -> Result<String, ApiError> {
    let (content_type, extension) = if channel == store::inbox::channel::EMAIL {
        ("message/rfc822", "eml")
    } else {
        ("text/csv", "csv")
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    let slug: String = mailbox
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .to_lowercase();
    let archived_key = format!("mail/{now_ms}-{slug}.{extension}");
    state
        .storage
        .put(&archived_key, bytes, content_type)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    Ok(archived_key)
}

/// Walk every parsed message through the direction-aware postgres ingest,
/// then file email attachments on the messages that landed fresh (AC4,
/// AC8). The importing actor is the mailbox owner: their own rows (From
/// = the mailbox) arrive as `outbound` with them as author. Returns one
/// summary per message, in payload order.
async fn import_all(
    state: &ApiState,
    person_id: Uuid,
    firm_id: Uuid,
    project_id: Option<Uuid>,
    messages: &mut [store::inbox_import::ImportMessage],
    mailbox: &str,
    archived_key: &str,
) -> Result<Vec<ImportedMessage>, ApiError> {
    let mut imported = Vec::with_capacity(messages.len());
    for message in messages.iter_mut() {
        message.raw_storage_key = Some(archived_key.to_string());
        let (direction, author) = match message.direction {
            store::inbox_import::Direction::Outbound => {
                (store::inbox::direction::OUTBOUND, Some(person_id))
            }
            store::inbox_import::Direction::Inbound => (store::inbox::direction::INBOUND, None),
        };
        let ingest = store::inbox_import::import_message_to_ingest(message, firm_id, mailbox);
        let ingested = store::inbox::ingest_imported(direction, author, &ingest)
            .await
            .map_err(|error| inbox_error(&error))?;

        // Email attachments file as slugged revisions with provenance
        // (AC8). A deduped message was already filed by the import that
        // created it, and an unattributed import has no matter to file
        // into — its attachment bytes stay in the archived payload.
        let mut filed_attachments = Vec::new();
        if !ingested.deduped {
            if let Some(project_id) = project_id {
                for (index, attachment) in message.attachments.iter().enumerate() {
                    let asset_id = file_import_attachment(
                        state,
                        project_id,
                        ingested.message_id,
                        index,
                        message,
                        attachment,
                    )
                    .await?;
                    filed_attachments.push(ImportedAttachment {
                        filename: attachment.filename.clone(),
                        content_type: attachment.content_type.clone(),
                        size_bytes: attachment.bytes.len(),
                        asset_id,
                    });
                }
            }
        }

        imported.push(ImportedMessage {
            message_id: ingested.message_id,
            conversation_id: ingested.conversation_id,
            deduped: ingested.deduped,
            direction: direction.to_string(),
            attachments: filed_attachments,
        });
    }
    Ok(imported)
}

/// `POST /app/api/inbox/import` — import one correspondence payload into the
/// Firm Inbox.
///
/// Lawyer-tier only (the [`LawyerSession`] extractor answers 401/403 before
/// the body is parsed). `400` when the source is unknown, the payload is not
/// valid base64 or not parseable by its adapter (AC9 — a mis-selected file
/// or a changed LinkedIn export shape fails loudly, naming the offending
/// headers), or the Firm/matter resolution is ambiguous. `201` carries one
/// summary per ingested message; replaying the same payload reports the
/// existing ids with `deduped: true` and writes nothing (AC3).
pub(crate) async fn inbox_import_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Json(input): Json<InboxImportRequest>,
) -> Result<Response, ApiError> {
    let person_id = lawyer.0.person_id.ok_or(ApiError::NotFound)?;
    let mailbox = input.mailbox.trim().to_string();
    if mailbox.is_empty() {
        return Ok(bad_request(
            "mailbox_required",
            "The importing account (`mailbox`) is required.",
        ));
    }
    let adapter = match store::inbox_import::source_adapter(input.source.trim()) {
        Ok(adapter) => adapter,
        Err(error) => {
            return Ok(bad_request("unknown_source", &error.to_string()));
        }
    };
    let bytes = match decode_payload(&input.payload_base64) {
        Ok(bytes) => bytes,
        Err(response) => return Ok(response),
    };

    // The Firm the import lands in, and the matter it is attributed to.
    let (firm_id, project_id) = match resolve_firm_and_project(
        &state,
        &lawyer,
        person_id,
        input.project.as_deref(),
    )
    .await
    {
        Ok(resolved) => resolved,
        Err(response) => return Ok(response),
    };

    // Parse before anything is written: a mis-selected file never archives.
    let mut messages = match adapter.parse(&mailbox, &bytes) {
        Ok(messages) => messages,
        Err(error) => {
            return Ok(bad_request("unparseable_payload", &error.to_string()));
        }
    };
    if messages.is_empty() {
        return Ok(bad_request(
            "empty_payload",
            "The payload parses but carries no messages.",
        ));
    }

    // Dry runs need the parse result only — no archive, no rows, no filings.
    if input.dry_run {
        let preview = InboxImportPreview {
            source: input.source.trim().to_string(),
            mailbox: mailbox.clone(),
            dry_run: true,
            would_import: messages.len(),
            project: input.project.clone(),
        };
        return Ok((StatusCode::OK, Json(preview)).into_response());
    }

    // Archive the verbatim payload first (AC6).
    let archived_key = archive_payload(&state, adapter.channel(), &mailbox, &bytes).await?;

    // Ingest every message, then file the attachments of the fresh ones.
    let imported = import_all(
        &state,
        person_id,
        firm_id,
        project_id,
        &mut messages,
        &mailbox,
        &archived_key,
    )
    .await?;

    // Attribution last, once per conversation: `assign` projects (or moves)
    // every message on the conversation in one idempotent step (AC7).
    let mut conversations = imported
        .iter()
        .map(|message| message.conversation_id)
        .collect::<Vec<_>>();
    conversations.sort_unstable();
    conversations.dedup();
    if let Some(project_id) = project_id {
        for conversation_id in conversations {
            store::inbox::assign(&state.surreal, person_id, conversation_id, project_id)
                .await
                .map_err(|error| inbox_error(&error))?;
        }
    }

    let response = InboxImportResponse {
        source: input.source.trim().to_string(),
        mailbox,
        archived_key,
        project: input.project,
        imported,
    };
    Ok((StatusCode::CREATED, Json(response)).into_response())
}

/// File one parsed attachment as `mail/{message_id}/{index}` with full
/// `MailProvenance` — the `mail_file_door` pattern (AC8). Returns the asset
/// the bytes live under; identical bytes re-file as `Unchanged`.
async fn file_import_attachment(
    state: &ApiState,
    project_id: Uuid,
    message_id: Uuid,
    index: usize,
    message: &store::inbox_import::ImportMessage,
    attachment: &store::inbox_import::ImportAttachment,
) -> Result<Uuid, ApiError> {
    let message_id_str = message_id.to_string();
    let slug = format!("mail/{message_id_str}/{index}");
    let kind = "unclassified";
    let args = store::documents::IngestArgs {
        project_id,
        source: store::documents::source::EMAIL,
        filename: &attachment.filename,
        kind,
        content_type: &attachment.content_type,
        description: None,
        secondary_storage_key: None,
        visibility: store::documents::visibility::INTERNAL,
    };
    let identity = store::documents::DocumentIdentity {
        slug: Some(&slug),
        published_at: None,
        metadata: None,
        derived_from: None,
        transcript_quality: None,
        signing: None,
        notarization: Vec::new(),
    };
    let provenance = store::documents::MailProvenance {
        message_id: &message_id_str,
        sender: message
            .author_address
            .as_deref()
            .unwrap_or(&message.counterparty_address),
        received_at: &message.occurred_at,
        subject: message.subject.as_deref().unwrap_or_default(),
    };
    let filed = store::assets::file_revision_with_provenance(
        &state.surreal,
        &state.storage,
        &args,
        &identity,
        &provenance,
        &attachment.bytes,
    )
    .await
    .map_err(ApiError::Revision)?;
    let asset_id = match filed {
        store::assets::Filed::Revision(document) => document.asset_id,
        store::assets::Filed::Unchanged { asset_id } => asset_id,
    };
    // Only the first attachment occupies the Message's slot; the rest
    // resolve through MailProvenance.source_message_id.
    if index == 0 {
        store::inbox::set_attachment_asset(message_id, asset_id)
            .await
            .map_err(|error| inbox_error(&error))?;
    }
    Ok(asset_id)
}
