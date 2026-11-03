//! Portal-side CM/ECF notice filing (LAW-171): when a support-inbox message
//! classifies as a Notice of Electronic Filing, [`process_nef`] files the
//! email original and the message's attachment assets onto the attributed
//! matter and records the docket entry. It reads `store::nef` — the same
//! parser, entry-kind matcher, and suggested document kind the CLI's
//! `docket ingest` runs — so both lanes classify one notice identically.
//!
//! The pipeline never guesses, per the issue's rule: a message with no
//! matter, an unparseable notice, or a notice that names no case number
//! stays in the triage backlog unprocessed, and a notice with no document
//! number records nothing on the docket (the CLI files nothing for one
//! either — there is no ordinal to record).

use uuid::Uuid;

use crate::api::ApiError;
use crate::inbound_email::InboundAttachment;

/// What one [`process_nef`] run did. Closed-vocabulary and mechanism-only —
/// a docket number, a court, or a subject never crosses this shape; the
/// caller renders the verdict beside the recorded route.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub(crate) enum NefOutcome {
    /// Filed the original, every attachment asset, and the docket entry.
    Filed {
        case_id: Uuid,
        entry_id: Uuid,
        entry_number: String,
        /// How many documents (original plus attachments) were filed.
        filed_documents: usize,
        /// The asset the entry links as its DKT document — the first
        /// filed attachment when one exists, else the raw original.
        document_asset_id: Option<Uuid>,
    },
    /// The docket entry already existed — same case, same entry number —
    /// so nothing new was written. The caller still records the processing
    /// time; a re-run converges.
    AlreadyRecorded {
        case_id: Uuid,
        entry_id: Uuid,
        entry_number: String,
    },
    /// The notice carries no document number, so there is no docket
    /// ordinal to record. Available attachment assets and the email original
    /// are filed, with no docket entry.
    TextOnly { filed_documents: usize },
    /// The message belongs to no matter. Nothing was filed; the message
    /// stays in the triage backlog.
    NoProject,
    /// The message is not a parseable CM/ECF notice. Nothing was filed;
    /// the message stays in the triage backlog.
    NotANotice,
    /// The notice names no case number. Nothing was filed — matching the
    /// matter by anything else would be a guess — and the message stays
    /// in the triage backlog.
    NoCaseNumber,
}

impl NefOutcome {
    /// The storage spelling recorded beside the route (`nef_outcome::*`).
    #[must_use]
    pub(crate) fn spelling(&self) -> &'static str {
        match self {
            Self::Filed { .. } => store::inbox::nef_outcome::FILED,
            Self::AlreadyRecorded { .. } => store::inbox::nef_outcome::ALREADY_RECORDED,
            Self::TextOnly { .. } => store::inbox::nef_outcome::TEXT_ONLY,
            Self::NoProject => store::inbox::nef_outcome::NO_PROJECT,
            Self::NotANotice => store::inbox::nef_outcome::NOT_A_NOTICE,
            Self::NoCaseNumber => store::inbox::nef_outcome::NO_CASE_NUMBER,
        }
    }
}

/// File a classified NEF message onto its matter.
///
/// # Errors
///
/// Propagates store and storage faults. A triage outcome — no matter, an
/// unparseable notice, no case number — is not an error; it is the
/// pipeline refusing to guess, reported as an outcome.
pub(crate) async fn process_nef(
    state: &crate::api::ApiState,
    row: &store::inbox::UnprocessedRow,
) -> Result<NefOutcome, ApiError> {
    let Some(project_id) = row.project_id else {
        return Ok(NefOutcome::NoProject);
    };

    // The raw RFC 822 source, when the inbox conversation kept it. Its
    // absence is not fatal: the stored body text still parses as a
    // text-only notice, though nothing can then file the original's bytes.
    let raw = raw_source(state, row).await?;

    let notice = raw
        .as_deref()
        .and_then(|bytes| store::nef::parse_eml(bytes).ok())
        .or_else(|| store::nef::parse_text(&row.body, &[]).ok());
    let Some(notice) = notice else {
        return Ok(NefOutcome::NotANotice);
    };
    let case_no = notice.case_no.trim().to_owned();
    if case_no.is_empty() {
        return Ok(NefOutcome::NoCaseNumber);
    }

    // Resolve the case by the docket number the notice prints; an
    // unrecorded case is opened from the notice's own masthead. Many
    // cases per matter is the point, so this is the same resolution the
    // CLI's ingest performs against its manifest's dockets.
    let case = match store::cases::find_case_by_docket_number(&state.surreal, project_id, &case_no)
        .await
        .map_err(|error| case_error(&error))?
    {
        Some(case) => case,
        None => store::cases::open_case(
            &state.surreal,
            &store::cases::NewCase {
                project_id,
                caption: notice.case_name.as_deref().unwrap_or(&case_no),
                forum: Some(notice.court.trim()),
                jurisdiction: None,
                docket_number: Some(&case_no),
                judge: None,
                posture: "unknown",
            },
        )
        .await
        .map_err(|error| case_error(&error))?,
    };

    // Idempotency first: an entry already on the case under this number
    // means a previous run filed this notice. Re-running files nothing.
    if let Some(doc_no) = notice.doc_no.as_deref() {
        let existing = store::cases::docket(&state.surreal, case.id)
            .await
            .map_err(|error| case_error(&error))?
            .into_iter()
            .find(|entry| entry.entry_number == doc_no);
        if let Some(entry) = existing {
            return Ok(NefOutcome::AlreadyRecorded {
                case_id: case.id,
                entry_id: entry.id,
                entry_number: entry.entry_number,
            });
        }
    }

    let kind = store::nef::suggested_kind(&notice.docket_text, None);
    let (filed_documents, document_asset_id) =
        file_notice_documents(state, row, project_id, kind, raw.as_deref()).await?;

    // A notice with no document number names no ordinal — record nothing
    // on the docket rather than guess one. The filed original is the
    // matter's record that the notice arrived.
    let Some(doc_no) = notice.doc_no.clone() else {
        return Ok(NefOutcome::TextOnly { filed_documents });
    };

    // A hearing or trial is never recorded from a notice, because the
    // notice does not carry the date the court set — the CLI's rule,
    // kept here by construction: `store::nef::entry_kind` maps no
    // docket text to an appearance kind.
    let kind_spelling = store::nef::entry_kind(&notice.docket_text);
    let kind = store::cases::EntryKind::parse(kind_spelling).ok_or_else(|| {
        ApiError::DocketInvalid(format!(
            "a notice's docket text produced kind `{kind_spelling}`"
        ))
    })?;
    let filed_or_served_on = notice
        .filed_at
        .map(|date| date.format("%Y-%m-%dT00:00:00Z").to_string());
    let entry = store::cases::record_entry(
        &state.surreal,
        &store::cases::NewDocketEntry {
            case_id: case.id,
            entry_number: &doc_no,
            kind,
            title: &notice.docket_text,
            party: notice.filed_by.as_deref(),
            filed_or_served_on: filed_or_served_on.as_deref(),
            scheduled_on: None,
            supersedes: None,
            document_asset_id,
            notation_id: None,
        },
    )
    .await
    .map_err(|error| case_error(&error))?;

    Ok(NefOutcome::Filed {
        case_id: case.id,
        entry_id: entry.id,
        entry_number: entry.entry_number,
        filed_documents,
        document_asset_id,
    })
}

/// File the notice's attachment assets and, when its raw source survived
/// the ingest, the original. Returns how many documents were filed and
/// the asset the docket entry links — the first filed attachment when
/// one exists, else the original.
async fn file_notice_documents(
    state: &crate::api::ApiState,
    row: &store::inbox::UnprocessedRow,
    project_id: Uuid,
    kind: &'static str,
    raw: Option<&[u8]>,
) -> Result<(usize, Option<Uuid>), ApiError> {
    let mut filed_documents = 0usize;
    let mut document_asset_id: Option<Uuid> = None;

    // The DKT documents: every attachment asset the message carries,
    // filed in message-part order as slugged revisions of
    // `mail/<message id>/<index>` — the inbox lane's own provenance path.
    let attachments = attachment_bytes(state, row).await?;
    if !attachments.is_empty() {
        if let Some(first) = crate::api::file_mail_attachments(
            state,
            project_id,
            kind,
            store::documents::visibility::INTERNAL,
            &crate::api::MailAttachmentSource {
                message_id: &row.id.to_string(),
                sender: &row.external_address,
                received_at: &row.occurred_at,
                subject: row.subject.as_deref().unwrap_or_default(),
            },
            &attachments,
        )
        .await?
        .first()
        .map(|pointer| pointer.current_version.asset_id)
        {
            document_asset_id = Some(first);
        }
        filed_documents += attachments.len();
    }

    // The original itself, when its bytes survived the ingest — the
    // unaltered record of what the court sent, headers included.
    if let Some(bytes) = raw {
        let slug = format!("mail/{}/original", row.id);
        let identity = store::documents::DocumentIdentity {
            slug: Some(&slug),
            published_at: None,
            metadata: None,
            derived_from: None,
            transcript_quality: None,
            signing: None,
            notarization: Vec::new(),
            evidence: None,
        };
        let args = store::documents::IngestArgs {
            project_id,
            source: store::documents::source::EMAIL,
            filename: "original.eml",
            kind,
            content_type: "message/rfc822",
            description: None,
            secondary_storage_key: None,
            visibility: store::documents::visibility::INTERNAL,
        };
        let provenance = store::documents::MailProvenance {
            message_id: &row.id.to_string(),
            sender: &row.external_address,
            received_at: &row.occurred_at,
            subject: row.subject.as_deref().unwrap_or_default(),
        };
        let filed = store::assets::file_revision_with_provenance(
            &state.surreal,
            &state.storage,
            &args,
            &identity,
            &provenance,
            bytes,
        )
        .await
        .map_err(ApiError::Revision)?;
        document_asset_id = document_asset_id.or(match filed {
            store::assets::Filed::Revision(revision) => Some(revision.asset_id),
            store::assets::Filed::Unchanged { asset_id } => Some(asset_id),
        });
        filed_documents += 1;
    }

    Ok((filed_documents, document_asset_id))
}

/// The message's raw RFC 822 source, when the inbox conversation's email
/// lane stored one — the raw `.eml` of the thread's inbound hop.
async fn raw_source(
    state: &crate::api::ApiState,
    row: &store::inbox::UnprocessedRow,
) -> Result<Option<Vec<u8>>, ApiError> {
    // An import can keep the raw source directly on the normalized message.
    if let Some(message) = store::inbox::message_by_id(&state.surreal, row.id)
        .await
        .map_err(map_inbox)?
    {
        if let Some(key) = message.raw_storage_key {
            return read_raw_source(state, &key).await.map(Some);
        }
    }
    let Some(provider_message_id) = row.provider_message_id.as_deref() else {
        return Ok(None);
    };
    let Some(conversation) = store::inbox::conversation_by_id(&state.surreal, row.conversation_id)
        .await
        .map_err(map_inbox)?
    else {
        return Ok(None);
    };
    let Some(email_conversation_id) = conversation.email_conversation_id else {
        return Ok(None);
    };
    let raw_key = store::email_conversations::messages(&state.surreal, email_conversation_id)
        .await
        .map_err(|error| map_email(&error))?
        .into_iter()
        .find(|hop| {
            hop.direction == store::email_conversations::DIRECTION_FROM_EXTERNAL
                && hop.provider_message_id.as_deref() == Some(provider_message_id)
        })
        .and_then(|hop| hop.raw_storage_key);
    let Some(key) = raw_key else {
        return Ok(None);
    };
    read_raw_source(state, &key).await.map(Some)
}

async fn read_raw_source(state: &crate::api::ApiState, key: &str) -> Result<Vec<u8>, ApiError> {
    Ok(state
        .storage
        .get(key)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?
        .bytes)
}

/// Every attachment asset the message links, with their bytes read back
/// from storage in message-part order.
async fn attachment_bytes(
    state: &crate::api::ApiState,
    row: &store::inbox::UnprocessedRow,
) -> Result<Vec<InboundAttachment>, ApiError> {
    let assets = store::inbox::message_assets(&state.surreal, row.firm_id, row.id)
        .await
        .map_err(map_inbox)?;
    let mut attachments = Vec::with_capacity(assets.len());
    for asset_link in assets {
        let asset = store::assets::find_by_id(&state.surreal, asset_link.asset_id)
            .await
            .map_err(ApiError::Asset)?
            .ok_or_else(|| {
                ApiError::Db("a message's attachment asset could not be read back".to_string())
            })?;
        let object = state
            .storage
            .get(&asset.storage_key)
            .await
            .map_err(|error| ApiError::Db(error.to_string()))?;
        attachments.push(InboundAttachment {
            filename: asset_link.filename,
            content_type: asset.content_type,
            bytes: object.bytes,
        });
    }
    Ok(attachments)
}

fn map_inbox(error: store::inbox::InboxError) -> ApiError {
    match error {
        store::inbox::InboxError::NotFound => ApiError::NotFound,
        store::inbox::InboxError::Denied => ApiError::Forbidden,
        other => ApiError::Db(other.to_string()),
    }
}

fn map_email(error: &store::email_conversations::EmailConversationError) -> ApiError {
    ApiError::Db(error.to_string())
}

fn case_error(error: &store::cases::CaseError) -> ApiError {
    ApiError::Db(error.to_string())
}
