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
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};
use crate::attachment_scanner::{AttachmentScanner, ScanError, ScanVerdict};

/// One request = one payload. `project` is a matter *code* (`sample-litigation`);
/// attribution at import rides `store::inbox::assign` after ingest. `firm`
/// names the landing Firm for the ingest-only doors when the caller belongs
/// to more than one and no matter names the spot (LAW-347's staging e2e
/// imports unfiled threads for Jev to propose against).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InboxImportRequest {
    /// Which adapter parses the payload (`email` | `gmail` | `linkedin`).
    pub source: String,
    /// Gmail thread identity read by the verified mailbox import command.
    pub gmail_thread_id: Option<String>,
    /// The importing account — the lawyer's mailbox address, or their
    /// LinkedIn profile URL. Becomes the Inbox `provider_account`.
    pub mailbox: String,
    /// The verbatim payload bytes, base64-encoded.
    pub payload_base64: String,
    /// Optional matter code; conversations attribute (or move) there.
    pub project: Option<String>,
    /// Optional Firm id for the ingest-only doors when the caller belongs to
    /// more than one. The caller must hold a membership in it — naming a
    /// Firm is not an authorization grant, exactly as a matter code is not
    /// one. Rejected on the Gmail path, whose Firm comes from the canonical
    /// intake mailbox.
    pub firm: Option<Uuid>,
    /// Parse and preview without writing. The CLI's local `--dry-run`
    /// prints the request instead of contacting this endpoint.
    #[serde(default)]
    pub dry_run: bool,
}

/// One ingested message as the door reports it — the ids a re-run compares
/// for idempotency (AC3), plus what was filed from it.
#[derive(Debug, Serialize)]
struct ImportedMessage {
    message_id: Uuid,
    conversation_id: Uuid,
    /// True when the Message already exists. A retry can still finish
    /// interrupted attachment filing or attribution.
    deduped: bool,
    direction: String,
    /// Email attachments staged in the Firm Inbox or filed into a Project.
    attachments: Vec<ImportedAttachment>,
}

#[derive(Debug, Serialize)]
struct ImportedAttachment {
    filename: String,
    content_type: String,
    size_bytes: usize,
    /// The staged or filed asset containing these bytes.
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
    /// The Firm the import landed in, when the request named one.
    firm: Option<Uuid>,
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
    /// The Firm the import would land in, when the request named one.
    firm: Option<Uuid>,
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
    let memberships = store::firms::memberships_for_person(&state.application_store, person_id)
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

/// The caller's named Firm: accepted only when the caller holds a membership
/// in it. Naming a Firm picks a landing spot; it grants nothing.
async fn resolve_named_firm(
    state: &ApiState,
    person_id: Uuid,
    firm_id: Uuid,
) -> Result<Uuid, Response> {
    let memberships = store::firms::memberships_for_person(&state.application_store, person_id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()).into_response())?;
    if memberships
        .iter()
        .any(|membership| membership.firm_id == firm_id)
    {
        Ok(firm_id)
    } else {
        Err(bad_request(
            "firm_not_member",
            "This account does not belong to the named Firm, so imports cannot land there.",
        ))
    }
}

/// The payload's bytes: `payload_base64` under the strict standard alphabet.
/// A bad encoding is a 400 — the caller picked the wrong file or truncated it.
fn decode_payload(encoded: &str) -> Result<Vec<u8>, Response> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .map_err(|_| bad_request("invalid_base64", "payload_base64 is not valid base64."))
}

/// The Gmail adapter may import only the actor's verified mailbox and must use
/// the Firm's canonical lawyer-intake account so re-delivery keeps its identity.
async fn gmail_import_firm(
    state: &ApiState,
    actor_id: Uuid,
    mailbox: &str,
) -> Result<(Uuid, String), ApiError> {
    let config = crate::lawyer_mailbox::Config::from_env().ok_or(ApiError::Forbidden)?;
    let person = store::persons::find_by_any_email(&state.application_store, mailbox)
        .await
        .map_err(ApiError::Person)?
        .filter(|person| {
            person.id == actor_id && person.email_confirmed && person.role.is_lawyer_tier()
        })
        .ok_or(ApiError::Forbidden)?;
    let memberships = store::firms::memberships_for_person(&state.application_store, person.id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    if !config.provider_account().eq_ignore_ascii_case(mailbox)
        || !memberships
            .iter()
            .any(|membership| membership.firm_id == config.firm_id)
    {
        return Err(ApiError::Forbidden);
    }
    Ok((config.firm_id, config.provider_account().to_owned()))
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
    let Some(project) = store::projects::find_by_code(&state.application_store, code)
        .await
        .map_err(|error| ApiError::Db(error.to_string()).into_response())?
    else {
        return Err(bad_request(
            "project_not_found",
            "No Project carries that matter code.",
        ));
    };
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
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

pub(crate) async fn resolve_import_target(
    state: &ApiState,
    lawyer: &LawyerSession,
    person_id: Uuid,
    input: &InboxImportRequest,
    mailbox: String,
) -> Result<(Uuid, Option<Uuid>, String), Response> {
    if input.source.trim() == "gmail" {
        if input.firm.is_some() {
            return Err(bad_request(
                "firm_not_supported",
                "The Gmail import resolves its Firm from the canonical intake mailbox; the ingest-only doors name a Firm.",
            ));
        }
        if input.project.is_some() {
            return Err(bad_request(
                "project_confirmation_required",
                "Import Gmail into the Inbox and confirm proposed Project filings individually.",
            ));
        }
        return gmail_import_firm(state, person_id, &mailbox)
            .await
            .map(|(firm, canonical)| (firm, None, canonical))
            .map_err(IntoResponse::into_response);
    }
    // No matter named: the landing Firm is the caller's named membership, or
    // their sole practice Firm by default (run 43930109007's ambiguity).
    if input.project.is_none() {
        if let Some(firm_id) = input.firm {
            return resolve_named_firm(state, person_id, firm_id)
                .await
                .map(|firm| (firm, None, mailbox));
        }
    }
    let (firm, project) =
        resolve_firm_and_project(state, lawyer, person_id, input.project.as_deref()).await?;
    // A Firm named alongside a matter must agree with it, or the request is
    // internally contradictory: refuse loudly rather than guess one meaning.
    if input.firm.is_some_and(|named| named != firm) {
        return Err(bad_request(
            "firm_mismatch",
            "The named Firm does not match the named matter's Firm.",
        ));
    }
    Ok((firm, project, mailbox))
}

/// Archive the verbatim payload (AC6): one key for the .eml, or the CSV
/// once for the whole export — direction-neutral `mail/` prefix, recorded
/// on every Message this import creates.
async fn archive_payload(
    state: &ApiState,
    channel: &str,
    firm_id: Uuid,
    mailbox: &str,
    bytes: &[u8],
) -> Result<String, ApiError> {
    let (content_type, extension) = if channel == store::inbox::channel::EMAIL {
        ("message/rfc822", "eml")
    } else {
        ("text/csv", "csv")
    };
    let digest = store::inbox_import::scoped_digest(&format!("{firm_id}:{mailbox}"), bytes);
    let archived_key = format!("mail/{digest}.{extension}");
    match state.storage.get(&archived_key).await {
        Ok(existing) if existing.bytes == bytes => {}
        Ok(_) => return Err(ApiError::Db("import archive identity conflict".into())),
        Err(cloud::StorageError::NotFound(_)) => {
            state
                .storage
                .put(&archived_key, bytes, content_type)
                .await
                .map_err(|error| ApiError::Db(error.to_string()))?;
        }
        Err(error) => return Err(ApiError::Db(error.to_string())),
    }
    Ok(archived_key)
}

struct ImportMailbox<'a> {
    account: &'a str,
    gmail: bool,
}

/// Stage each imported Gmail Message for individual lawyer review.
async fn ensure_gmail_review(
    state: &ApiState,
    firm_id: Uuid,
    message_id: Uuid,
) -> Result<(), ApiError> {
    use store::inbox_triage::{self, NewInboxTriage, PrivilegeLabel, TriageDisposition};

    let existing = || inbox_triage::find_for_message(&state.application_store, message_id);
    if existing()
        .await
        .map_err(|error| inbox_error(&error))?
        .is_some()
    {
        return Ok(());
    }
    let review = NewInboxTriage {
        message_id,
        firm_id,
        suggested_project_id: None,
        project_confidence: None,
        privilege: PrivilegeLabel::Uncertain,
        privilege_confidence: None,
        rationale: None,
        model_id: None,
        prompt_version: inbox_triage::RULES_ONLY_PROMPT.into(),
        raw_output: None,
        disposition: TriageDisposition::NeedsHuman,
        labeled_example: false,
    };
    if let Err(error) = inbox_triage::record(&state.application_store, &review).await {
        // The Message index settles concurrent imports. Only a committed
        // review makes a failed create equivalent to success.
        if existing()
            .await
            .map_err(|error| inbox_error(&error))?
            .is_none()
        {
            return Err(inbox_error(&error));
        }
    }
    Ok(())
}

fn gmail_thread_ref(gmail: bool, id: Option<&str>) -> Result<Option<String>, Response> {
    let Some(id) = id else {
        return Ok(None);
    };
    if !gmail
        || id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(bad_request(
            "invalid_gmail_thread_id",
            "Gmail thread identity requires the Gmail source and a valid provider ID.",
        ));
    }
    Ok(Some(format!("gmail:{id}")))
}

/// Walk every parsed message through the direction-aware postgres ingest,
/// then idempotently file email attachments (AC4,
/// AC8). The importing actor is the mailbox owner: their own rows (From
/// = the mailbox) arrive as `outbound` with them as author. Returns one
/// summary per message, in payload order.
async fn import_all(
    state: &ApiState,
    person_id: Uuid,
    firm_id: Uuid,
    project_id: Option<Uuid>,
    messages: &mut [store::inbox_import::ImportMessage],
    mailbox: &ImportMailbox<'_>,
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
        let mut ingest =
            store::inbox_import::import_message_to_ingest(message, firm_id, mailbox.account);
        if mailbox.gmail {
            ingest.source_channel = Some(store::inbox::channel::EMAIL);
        }
        let ingested = store::inbox::ingest_imported(direction, author, &ingest)
            .await
            .map_err(|error| inbox_error(&error))?;

        let mut attachments = Vec::new();
        let mut links = Vec::new();
        for (index, attachment) in message.attachments.iter().enumerate() {
            let link = if mailbox.gmail {
                store::inbox::ingest_attachment(
                    &state.application_store,
                    &state.storage,
                    firm_id,
                    None,
                    &attachment.filename,
                    &attachment.content_type,
                    &attachment.bytes,
                )
                .await
                .map_err(|error| inbox_error(&error))?
            } else if let Some(project_id) = project_id {
                let asset_id = file_import_attachment(
                    state,
                    project_id,
                    ingested.message_id,
                    index,
                    message,
                    attachment,
                )
                .await?;
                store::inbox::IngestAttachment {
                    asset_id,
                    filename: attachment.filename.clone(),
                    sha256: store::documents::sha256_hex(&attachment.bytes),
                }
            } else {
                break;
            };
            attachments.push(ImportedAttachment {
                filename: attachment.filename.clone(),
                content_type: attachment.content_type.clone(),
                size_bytes: attachment.bytes.len(),
                asset_id: link.asset_id,
            });
            links.push(link);
        }
        store::inbox::link_message_assets(
            &state.application_store,
            firm_id,
            ingested.message_id,
            &links,
        )
        .await
        .map_err(|error| inbox_error(&error))?;

        if mailbox.gmail {
            ensure_gmail_review(state, firm_id, ingested.message_id).await?;
        }

        imported.push(ImportedMessage {
            message_id: ingested.message_id,
            conversation_id: ingested.conversation_id,
            deduped: ingested.deduped,
            direction: direction.to_string(),
            attachments,
        });
    }
    Ok(imported)
}

async fn scan_gmail_attachments(
    scanner: Option<&dyn AttachmentScanner>,
    messages: &[store::inbox_import::ImportMessage],
) -> Result<(), Response> {
    for attachment in messages.iter().flat_map(|message| &message.attachments) {
        let (status, error) = match scanner {
            Some(scanner) => match scanner.scan(&attachment.bytes).await {
                Ok(ScanVerdict::Clean) => continue,
                Ok(ScanVerdict::Found { .. }) => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "attachment_malware_detected",
                ),
                Err(ScanError::SizeLimit { .. }) => {
                    (StatusCode::PAYLOAD_TOO_LARGE, "attachment_too_large")
                }
                Err(_) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "attachment_scanner_unavailable",
                ),
            },
            None => (
                StatusCode::SERVICE_UNAVAILABLE,
                "attachment_scanner_unavailable",
            ),
        };
        return Err((status, Json(serde_json::json!({"error": error}))).into_response());
    }
    Ok(())
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
/// existing Message ids with `deduped: true`; interrupted filing can finish (AC3).
pub(crate) async fn inbox_import_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    scanner: Option<Extension<Arc<dyn AttachmentScanner>>>,
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
    let source = input.source.trim();
    let gmail = source == "gmail";
    let provider_thread_ref = match gmail_thread_ref(gmail, input.gmail_thread_id.as_deref()) {
        Ok(thread) => thread,
        Err(response) => return Ok(response),
    };
    let adapter = match store::inbox_import::source_adapter(if gmail { "email" } else { source }) {
        Ok(adapter) => adapter,
        Err(error) => {
            return Ok(bad_request("unknown_source", &error.to_string()));
        }
    };

    let (firm_id, project_id, mailbox) =
        match resolve_import_target(&state, &lawyer, person_id, &input, mailbox).await {
            Ok(target) => target,
            Err(response) => return Ok(response),
        };
    let bytes = match decode_payload(&input.payload_base64) {
        Ok(bytes) => bytes,
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
            firm: input.firm,
        };
        return Ok((StatusCode::OK, Json(preview)).into_response());
    }

    if gmail {
        let scanner = scanner.as_ref().map(|Extension(scanner)| scanner.as_ref());
        if let Err(response) = scan_gmail_attachments(scanner, &messages).await {
            return Ok(response);
        }
    }

    if let Some(thread) = provider_thread_ref {
        for message in &mut messages {
            message.provider_thread_ref = Some(thread.clone());
        }
    }

    // Archive the verbatim payload before ingesting the Message (AC6).
    let archived_key =
        archive_payload(&state, adapter.channel(), firm_id, &mailbox, &bytes).await?;

    // Ingest every message, then finish any attachment filings.
    let imported = import_all(
        &state,
        person_id,
        firm_id,
        project_id,
        &mut messages,
        &ImportMailbox {
            account: &mailbox,
            gmail,
        },
        &archived_key,
    )
    .await?;

    // Attribution last, once per conversation (AC7).
    attribute_imports(&state, person_id, project_id, &imported).await?;

    let response = InboxImportResponse {
        source: input.source.trim().to_string(),
        mailbox,
        archived_key,
        project: input.project,
        firm: input.firm,
        imported,
    };
    Ok((StatusCode::CREATED, Json(response)).into_response())
}

/// Attribution rides last, once per conversation: `assign` projects (or
/// moves) every message on the conversation in one idempotent step (AC7).
/// Ingest-only imports carry no matter and stay unattributed for Jev.
async fn attribute_imports(
    state: &ApiState,
    person_id: Uuid,
    project_id: Option<Uuid>,
    imported: &[ImportedMessage],
) -> Result<(), ApiError> {
    let Some(project_id) = project_id else {
        return Ok(());
    };
    let mut conversations = imported
        .iter()
        .map(|message| message.conversation_id)
        .collect::<Vec<_>>();
    conversations.sort_unstable();
    conversations.dedup();
    for conversation_id in conversations {
        store::inbox::assign(
            &state.application_store,
            person_id,
            conversation_id,
            project_id,
        )
        .await
        .map_err(|error| inbox_error(&error))?;
    }
    Ok(())
}

/// File one parsed attachment as `mail/{message_id}/{index}.<extension>` with full
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
    let mut filename = std::path::Path::new(&attachment.filename)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("attachment")
        .to_owned();
    if std::path::Path::new(&filename)
        .extension()
        .is_none_or(std::ffi::OsStr::is_empty)
    {
        filename.push_str(".bin");
    }
    let extension = std::path::Path::new(&filename)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty())
        .unwrap_or("bin");
    let slug = format!("mail/{message_id_str}/{index}.{extension}");
    let kind = "unclassified";
    let args = store::documents::IngestArgs {
        project_id,
        source: store::documents::source::EMAIL,
        filename: &filename,
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
        evidence: None,
        folder: None,
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
        &state.application_store,
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

fn fi_preview_default() -> bool {
    true
}

/// Explicitly mapped metadata only. The default request is a read-only preview.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FiImportRequest {
    account: String,
    brand: String,
    project: Option<String>,
    mapping: store::fi_import::Mapping,
    payload_base64: String,
    #[serde(default = "fi_preview_default")]
    dry_run: bool,
}

/// `POST /app/api/inbox/import/fi`: both historical directions, never a send.
#[expect(
    clippy::too_many_lines,
    reason = "one bounded batch boundary keeps prevalidation before every write and reports partial commits"
)]
pub(crate) async fn fi_import_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Json(input): Json<FiImportRequest>,
) -> Result<Response, ApiError> {
    use store::fi_import::{self, Existing};
    let actor = lawyer.0.person_id.ok_or(ApiError::NotFound)?;
    let (firm, project) =
        match resolve_firm_and_project(&state, &lawyer, actor, input.project.as_deref()).await {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
    if let Some(project_id) = project {
        store::inbox::require_assignment_authorization(
            &state.application_store,
            actor,
            firm,
            project_id,
        )
        .await
        .map_err(|error| inbox_error(&error))?;
    }
    let brand = store::brands::find_by_key(&state.application_store, input.brand.trim())
        .await
        .map_err(|_| ApiError::NotFound)?
        .ok_or(ApiError::NotFound)?;
    if store::firms::firm_id_for_brand_key(&state.application_store, &brand.key)
        .await
        .map_err(|_| ApiError::NotFound)?
        != Some(firm)
    {
        return Err(ApiError::NotFound);
    }
    let account = match fi_import::account(&input.account) {
        Ok(account) => account,
        Err(error) => return Ok(bad_request("fi_mapping", &error.to_string())),
    };
    if input.payload_base64.len() > fi_import::MAX_CSV_BYTES * 4 / 3 + 8 {
        return Ok(bad_request("fi_limit", "Fi CSV exceeds the import limit."));
    }
    let bytes = match decode_payload(&input.payload_base64) {
        Ok(bytes) => bytes,
        Err(response) => return Ok(response),
    };
    let rows = match fi_import::parse(&input.mapping, &input.account, &bytes) {
        Ok(rows) => rows,
        Err(error) => return Ok(bad_request("fi_parse", &error.to_string())),
    };
    let pg = fi_import::native_pool()
        .await
        .map_err(|_| ApiError::Db("native Inbox authority is required for Fi imports".into()))?;
    let scope = fi_import::Scope {
        actor,
        firm,
        brand: brand.key.clone(),
        account,
        project,
    };
    let mut previews = Vec::with_capacity(rows.len());
    let mut new = 0;
    let mut existing = 0;
    let mut conflicts = 0;
    for row in &rows {
        let status = fi_import::preview_on(&pg, &scope, row)
            .await
            .map_err(|_| ApiError::Db("Fi duplicate preflight unavailable".into()))?;
        match status {
            Existing::New => new += 1,
            Existing::Identical => existing += 1,
            Existing::Conflict => conflicts += 1,
        }
        previews.push(
            serde_json::json!({"row":row.number,"status":status,"kind":row.metadata.kind,
            "direction":row.metadata.direction,"occurred_at":row.metadata.occurred_at,
            "duration_seconds":row.metadata.duration_seconds}),
        );
    }
    if input.dry_run || conflicts > 0 {
        let status = if conflicts > 0 {
            StatusCode::CONFLICT
        } else {
            StatusCode::OK
        };
        return Ok((
            status,
            Json(
                serde_json::json!({"dry_run":input.dry_run,"new":new,"existing":existing,
            "conflicts":conflicts,"rows":previews,"imported":[]}),
            ),
        )
            .into_response());
    }
    let mut imported = Vec::new();
    for row in &rows {
        let saved = match fi_import::ingest_on(&pg, &scope, row).await {
            Ok(saved) => saved,
            Err(error) => {
                let status = if error == fi_import::FiImportError::Conflict {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                };
                return Ok((status,Json(serde_json::json!({"dry_run":false,"failed_row":row.number,
                    "error":error.to_string(),"imported":imported,"outcome_may_be_committed":error==fi_import::FiImportError::Storage,
                    "reconcile":"Rerun the unchanged batch to reconcile committed source identities."}))).into_response());
            }
        };
        // Report persistence before the separate source-authoritative Project
        // projection; a failed attribution remains visible and safe to re-run.
        imported.push(
            serde_json::json!({"row":row.number,"message_id":saved.message_id,
            "conversation_id":saved.conversation_id,"deduped":saved.deduped}),
        );
        if let Some(project) = project {
            if store::inbox::assign(
                &state.application_store,
                actor,
                saved.conversation_id,
                project,
            )
            .await
            .is_err()
            {
                return Ok((
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({"dry_run":false,
                    "failed_row":row.number,"error":"Project attribution requires reconciliation",
                    "imported":imported,"reconcile":"Rerun the same batch and Project."})),
                )
                    .into_response());
            }
        }
    }
    Ok((
        StatusCode::CREATED,
        Json(
            serde_json::json!({"dry_run":false,"new":new,"existing":existing,
        "conflicts":0,"imported":imported}),
        ),
    )
        .into_response())
}
