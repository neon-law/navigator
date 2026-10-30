//! `store::communications` — write + read side of the project-scoped,
//! attorney-client privileged conversation log.
//!
//! [`ingest`] is the one write entry point, the analogue of
//! [`crate::documents::ingest_bytes`]: it maps a message from any channel
//! into a [`Communication`] spine row. It is idempotent on
//! `(channel, source_ref)` — a re-delivered email or re-ingested source
//! returns the existing row instead of duplicating, so callers can replay
//! safely.
//!
//! [`for_project`] is the read side: the whole thread for one matter,
//! oldest→newest, the way the conversation view renders it.
//!
//! Channel-specific fidelity (a comment's anchor, an email's headers) lives
//! in satellites that FK back to the row this returns. Privilege is enforced
//! one layer up (`store::access::can_see_project`); this module never widens a
//! query past `project_id`.
//!
//! # This table lives in SurrealDB
//!
//! `communications` moved with wave six of #1093 (ENG-160), in the
//! communications slice alongside [`crate::email_conversations`].

use std::sync::Arc;

use chrono::{DateTime, Months, Utc};
use cloud::{StorageError, StorageService};
use serde::Serialize;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::surreal::{record_id, record_uuid, SurrealDb};

/// The table these rows live in.
const TABLE: &str = "communication";
const PERSON_TABLE: &str = "person";

async fn mirror_communication(db: &SurrealDb, id: Uuid) {
    crate::postgres_mirror::after_write(db, crate::postgres_mirror::MirrorKey::Communication(id))
        .await;
}

/// How long a matter's privileged conversation log is kept after the matter
/// closes, then securely destroyed. Firm policy — the client consents to it
/// in the retainer ("Your file, kept for ten years"). Exceeds the NV RPC
/// file-retention floor, so it is the controlling number.
pub const RETENTION_YEARS: u32 = 10;

/// Channel literals written to `communications.channel`. Centralized so the
/// ingest seams, the thread view, and tests agree on the same strings — a
/// mismatch is a silent "message vanished from the thread" bug. Email and
/// SMS both project onto this spine once a conversation is assigned to a
/// Project.
pub mod channel {
    pub const DOCUMENT_COMMENT: &str = "document_comment";
    pub const EMAIL_INBOUND: &str = "email_inbound";
    pub const EMAIL_OUTBOUND: &str = "email_outbound";
    pub const PORTAL_MESSAGE: &str = "portal_message";
    pub const SMS_INBOUND: &str = "sms_inbound";
    pub const SMS_OUTBOUND: &str = "sms_outbound";
    pub const FACEBOOK_INBOUND: &str = "facebook_inbound";
    pub const FACEBOOK_OUTBOUND: &str = "facebook_outbound";
    pub const INSTAGRAM_INBOUND: &str = "instagram_inbound";
    pub const INSTAGRAM_OUTBOUND: &str = "instagram_outbound";
    pub const THREADS_INBOUND: &str = "threads_inbound";
    pub const THREADS_OUTBOUND: &str = "threads_outbound";
    pub const TIKTOK_INBOUND: &str = "tiktok_inbound";
    pub const TIKTOK_OUTBOUND: &str = "tiktok_outbound";
    pub const YOUTUBE_INBOUND: &str = "youtube_inbound";
    pub const YOUTUBE_OUTBOUND: &str = "youtube_outbound";
    pub const LINKEDIN_INBOUND: &str = "linkedin_inbound";
    pub const LINKEDIN_OUTBOUND: &str = "linkedin_outbound";
    /// A phone call or other off-system contact, logged after the fact as
    /// an internal note (LAW-140). The row's `direction` is its visibility
    /// (`internal` unless the firm chose to share it); the call's own
    /// direction lives in [`super::CallDetails`].
    pub const CALL: &str = "call";
}

/// Who was on the other end of a call.
pub mod counterparty_role {
    pub const OPPOSING_COUNSEL: &str = "opposing_counsel";
    pub const CLIENT: &str = "client";
    pub const COURT: &str = "court";
    pub const OTHER: &str = "other";
    pub const ALL: [&str; 4] = [OPPOSING_COUNSEL, CLIENT, COURT, OTHER];
}

/// Whether a call was recorded, and with whose consent.
pub mod recorded {
    pub const NONE: &str = "none";
    pub const ALL_PARTY_CONSENT: &str = "all_party_consent";
    pub const ALL: [&str; 2] = [NONE, ALL_PARTY_CONSENT];
}

/// The structured half of a `call` row: what a free-form note loses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize, SurrealValue)]
pub struct CallDetails {
    /// `inbound` or `outbound`: who placed the call.
    pub direction: String,
    /// One of [`counterparty_role`].
    pub counterparty_role: String,
    pub duration_minutes: Option<i64>,
    /// One of [`recorded`].
    pub recorded: String,
    /// The issue a meet-and-confer call addressed, when it was one.
    pub meet_and_confer: Option<String>,
}

/// Agreement status: unconfirmed until a confirming email is linked.
pub mod agreement_status {
    pub const UNCONFIRMED: &str = "unconfirmed";
    pub const CONFIRMED: &str = "confirmed";
}

/// One thing agreed on a call, tracked until an email confirms it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize, SurrealValue)]
pub struct Agreement {
    pub text: String,
    /// One of [`agreement_status`].
    pub status: String,
    /// The email Communication that confirmed it, as its id text.
    pub confirmed_by: Option<String>,
}

/// Direction literals written to `communications.direction`.
pub mod direction {
    /// From the client to the firm.
    pub const INBOUND: &str = "inbound";
    /// From the firm to the client.
    pub const OUTBOUND: &str = "outbound";
    /// A firm-internal note — never shown to the client.
    pub const INTERNAL: &str = "internal";
}

/// One message in a matter's privileged conversation log.
///
/// The application-facing shape: plain Rust types, no engine handles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Communication {
    pub id: Uuid,
    /// The matter this message belongs to.
    pub project_id: Uuid,
    /// Channel discriminator — one of [`channel`].
    pub channel: String,
    /// `inbound`, `outbound`, or `internal` — one of [`direction`].
    pub direction: String,
    /// The [`crate::persons`] row that authored it; `None` for a system or
    /// unknown sender.
    pub author_person_id: Option<Uuid>,
    /// Email/name of the other party when there is no `persons` row.
    pub counterparty: Option<String>,
    /// Optional subject line (email subject; `None` for comments).
    pub subject: Option<String>,
    /// Normalized message text — the only body representation.
    pub body: String,
    /// External id for idempotent ingest: email `Message-ID`, the comment
    /// id, the SMS provider id.
    pub source_ref: Option<String>,
    /// The email counterparty's role, when recorded.
    pub counterparty_role: Option<String>,
    /// Outbound email whose receipt this inbound email acknowledges.
    pub acknowledges: Option<Uuid>,
    /// Earliest linked inbound receipt timestamp, derived within this Project.
    pub receipt_acknowledged_at: Option<String>,
    /// Raw payload (verbatim `.eml`, …); `None` for comments.
    pub asset_id: Option<Uuid>,
    /// RFC 3339 timestamp of when the message actually happened.
    pub occurred_at: String,
    /// The structured call, on a [`channel::CALL`] row only.
    pub call: Option<CallDetails>,
    /// What a call agreed, each unconfirmed until an email confirms it.
    pub agreements: Vec<Agreement>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Communication {
    /// Whether any agreement on this row still waits for a confirming email.
    #[must_use]
    pub fn has_unconfirmed_agreements(&self) -> bool {
        self.agreements
            .iter()
            .any(|agreement| agreement.status == agreement_status::UNCONFIRMED)
    }
}

/// The row as the engine reads and writes it.
#[derive(SurrealValue)]
struct CommunicationRow {
    id: surrealdb::types::RecordId,
    project_id: surrealdb::types::RecordId,
    channel: String,
    direction: String,
    author_person_id: Option<surrealdb::types::RecordId>,
    counterparty: Option<String>,
    subject: Option<String>,
    body: String,
    source_ref: Option<String>,
    counterparty_role: Option<String>,
    acknowledges: Option<surrealdb::types::RecordId>,
    asset_id: Option<surrealdb::types::RecordId>,
    occurred_at: String,
    call: Option<CallDetails>,
    agreements: Option<Vec<Agreement>>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl CommunicationRow {
    fn into_communication(self) -> Option<Communication> {
        Some(Communication {
            id: record_uuid(&self.id)?,
            project_id: record_uuid(&self.project_id)?,
            channel: self.channel,
            direction: self.direction,
            author_person_id: self.author_person_id.as_ref().and_then(record_uuid),
            counterparty: self.counterparty,
            subject: self.subject,
            body: self.body,
            source_ref: self.source_ref,
            counterparty_role: self.counterparty_role,
            acknowledges: self.acknowledges.as_ref().and_then(record_uuid),
            receipt_acknowledged_at: None,
            asset_id: self.asset_id.as_ref().and_then(record_uuid),
            occurred_at: self.occurred_at,
            call: self.call,
            agreements: self.agreements.unwrap_or_default(),
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

/// The projection every read shares.
const SELECT: &str = "id, project_id, channel, direction, author_person_id, counterparty, \
                      subject, body, source_ref, counterparty_role, acknowledges, asset_id, occurred_at, call, agreements, \
                      inserted_at, updated_at";

/// What can go wrong reading or writing the conversation log.
#[derive(Debug, thiserror::Error)]
pub enum CommunicationError {
    /// A database operation failed.
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    /// A call the caller can correct: an unknown direction, role, or
    /// recording consent, a non-RFC 3339 time, or a blank body.
    #[error("{0}")]
    Invalid(String),
    /// The call named by [`confirm_agreements`] is not on this Project.
    #[error("no such call on this Project")]
    NotFound,
}

/// What [`record_call`] needs.
#[derive(Debug, Clone, Default)]
pub struct NewCall {
    pub project_id: Uuid,
    pub author_person_id: Option<Uuid>,
    /// The other party, as an email or a name.
    pub counterparty: String,
    pub subject: Option<String>,
    pub body: String,
    /// RFC 3339: when the call started.
    pub at: String,
    pub call: CallDetails,
    /// What was agreed, each stored `unconfirmed`.
    pub agreements: Vec<String>,
    /// Share the note with the client: the row's direction becomes the
    /// call's own, so the client lens reads it. Refused for opposing counsel.
    pub client_visible: bool,
}

/// Log a call as a Communication (LAW-140). The row is an internal note
/// unless `client_visible` is set, and a call with opposing counsel is
/// always internal: that note is work product.
///
/// # Errors
/// [`CommunicationError::Invalid`] for a value the caller can correct, or
/// any database error.
pub async fn record_call(
    db: &SurrealDb,
    new: &NewCall,
) -> Result<Communication, CommunicationError> {
    if !matches!(new.call.direction.as_str(), "inbound" | "outbound") {
        return Err(CommunicationError::Invalid(
            "direction must be inbound or outbound".into(),
        ));
    }
    if !counterparty_role::ALL.contains(&new.call.counterparty_role.as_str()) {
        return Err(CommunicationError::Invalid(format!(
            "role must be one of {}",
            counterparty_role::ALL.join(", ")
        )));
    }
    if !recorded::ALL.contains(&new.call.recorded.as_str()) {
        return Err(CommunicationError::Invalid(format!(
            "recorded must be one of {}",
            recorded::ALL.join(", ")
        )));
    }
    if new.call.duration_minutes.is_some_and(|minutes| minutes < 0) {
        return Err(CommunicationError::Invalid(
            "duration must be zero or more minutes".into(),
        ));
    }
    let at = DateTime::parse_from_rfc3339(new.at.trim())
        .map(|at| at.to_rfc3339())
        .map_err(|_| {
            CommunicationError::Invalid(
                "at must be an RFC 3339 timestamp with an explicit offset".into(),
            )
        })?;
    let body = new.body.trim();
    if body.is_empty() {
        return Err(CommunicationError::Invalid("the call note is empty".into()));
    }
    let counterparty = new.counterparty.trim();
    if counterparty.is_empty() {
        return Err(CommunicationError::Invalid(
            "counterparty must not be blank".into(),
        ));
    }
    if new.client_visible && new.call.counterparty_role == counterparty_role::OPPOSING_COUNSEL {
        return Err(CommunicationError::Invalid(
            "a call with opposing counsel is always internal; it cannot be client-visible".into(),
        ));
    }
    let direction = if new.client_visible {
        new.call.direction.clone()
    } else {
        direction::INTERNAL.to_string()
    };
    let agreements: Vec<Agreement> = new
        .agreements
        .iter()
        .map(|text| text.trim())
        .filter(|text| !text.is_empty())
        .map(|text| Agreement {
            text: text.to_string(),
            status: agreement_status::UNCONFIRMED.to_string(),
            confirmed_by: None,
        })
        .collect();
    let id = Uuid::now_v7();
    let mut response = db
        .query(format!(
            "CREATE $id SET \
             project_id = $project_id, channel = $channel, direction = $direction, \
             author_person_id = $author, counterparty = $counterparty, subject = $subject, \
             body = $body, source_ref = NONE, asset_id = NONE, occurred_at = $occurred_at, \
             call = $call, agreements = $agreements \
             RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind((
            "project_id",
            record_id(crate::projects::PROJECT_TABLE, new.project_id),
        ))
        .bind(("channel", channel::CALL.to_string()))
        .bind(("direction", direction))
        .bind((
            "author",
            new.author_person_id.map(|p| record_id(PERSON_TABLE, p)),
        ))
        .bind(("counterparty", counterparty.to_string()))
        .bind(("subject", new.subject.clone()))
        .bind(("body", body.to_string()))
        .bind(("occurred_at", at))
        .bind(("call", new.call.clone()))
        .bind(("agreements", agreements))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<CommunicationRow> = response.take(0)?;
    let row = row
        .and_then(CommunicationRow::into_communication)
        .ok_or_else(|| CommunicationError::Invalid("the call could not be read back".into()))?;
    mirror_communication(db, row.id).await;
    Ok(row)
}

/// One Communication on a Project, or `None`.
///
/// # Errors
/// Propagates any database error.
pub async fn find(
    db: &SurrealDb,
    project_id: Uuid,
    id: Uuid,
) -> Result<Option<Communication>, CommunicationError> {
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE id = $id AND project_id = $project LIMIT 1"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<CommunicationRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .find_map(CommunicationRow::into_communication))
}

/// Mark every unconfirmed agreement on the call `call_id` as confirmed by
/// the email Communication `confirmed_by`, and return the call.
///
/// # Errors
/// [`CommunicationError::NotFound`] when `call_id` is not a call on this
/// Project, or any database error.
pub async fn confirm_agreements(
    db: &SurrealDb,
    project_id: Uuid,
    call_id: Uuid,
    confirmed_by: Uuid,
) -> Result<Communication, CommunicationError> {
    let call = find(db, project_id, call_id)
        .await?
        .filter(|row| row.channel == channel::CALL)
        .ok_or(CommunicationError::NotFound)?;
    let agreements: Vec<Agreement> = call
        .agreements
        .into_iter()
        .map(|agreement| {
            if agreement.status == agreement_status::UNCONFIRMED {
                Agreement {
                    status: agreement_status::CONFIRMED.to_string(),
                    confirmed_by: Some(confirmed_by.to_string()),
                    ..agreement
                }
            } else {
                agreement
            }
        })
        .collect();
    let mut response = db
        .query(format!(
            "UPDATE $id SET agreements = $agreements, updated_at = time::now() RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, call_id)))
        .bind(("agreements", agreements))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<CommunicationRow> = response.take(0)?;
    let call = rows
        .into_iter()
        .find_map(CommunicationRow::into_communication)
        .ok_or(CommunicationError::NotFound)?;
    mirror_communication(db, call.id).await;
    Ok(call)
}

/// Inputs to [`ingest`]. Held in a struct (not a positional list) so future
/// fields don't break callers, mirroring [`crate::documents::IngestArgs`].
#[derive(Debug, Clone)]
pub struct IngestArgs<'a> {
    /// Matter this message belongs to.
    pub project_id: Uuid,
    /// Channel literal — one of [`channel`].
    pub channel: &'a str,
    /// Direction literal — one of [`direction`].
    pub direction: &'a str,
    /// Author, when we have a `persons` row for them.
    pub author_person_id: Option<Uuid>,
    /// Email/name of the other party when there is no `persons` row.
    pub counterparty: Option<&'a str>,
    /// Optional subject line.
    pub subject: Option<&'a str>,
    /// Normalized message text.
    pub body: &'a str,
    /// External id for idempotent ingest (`Message-ID`, comment id, SMS id).
    /// When `Some`, a second ingest with the same `(channel, source_ref)`
    /// returns the existing row.
    pub source_ref: Option<&'a str>,
    /// Raw payload asset, when one was archived (verbatim `.eml`, …).
    pub asset_id: Option<Uuid>,
    /// When the message actually happened (RFC 3339). Distinct from the
    /// insert time, which the row stamps itself.
    pub occurred_at: &'a str,
}

/// What [`ingest`] resolved to.
#[derive(Debug, Clone, Copy)]
pub struct Ingested {
    pub communication_id: Uuid,
    /// `true` when an existing row with the same `(channel, source_ref)`
    /// was returned instead of inserting — the caller replayed a source.
    pub deduped: bool,
}

fn validate_replayed_details(
    row: &Communication,
    details: EmailDetails<'_>,
) -> Result<(), CommunicationError> {
    if details
        .acknowledges
        .is_some_and(|id| row.acknowledges != Some(id))
        || details
            .counterparty_role
            .is_some_and(|role| row.counterparty_role.as_deref() != Some(role))
    {
        return Err(CommunicationError::Invalid(
            "source reference already has different email metadata".into(),
        ));
    }
    Ok(())
}

/// Optional metadata recorded with an email Communication.
#[derive(Debug, Clone, Copy, Default)]
pub struct EmailDetails<'a> {
    pub counterparty_role: Option<&'a str>,
    pub acknowledges: Option<Uuid>,
}

/// Ingest an email with its counterparty role and receipt relation.
///
/// # Errors
/// Rejects invalid roles, receipt directions, and targets outside the Project.
/// Propagates database errors.
pub async fn ingest_email(
    db: &SurrealDb,
    args: &IngestArgs<'_>,
    details: EmailDetails<'_>,
) -> Result<Ingested, CommunicationError> {
    if !matches!(
        (args.channel, args.direction),
        (channel::EMAIL_INBOUND, direction::INBOUND)
            | (channel::EMAIL_OUTBOUND, direction::OUTBOUND)
    ) {
        return Err(CommunicationError::Invalid(
            "email channel and direction must agree".into(),
        ));
    }
    if details
        .counterparty_role
        .is_some_and(|role| !counterparty_role::ALL.contains(&role))
    {
        return Err(CommunicationError::Invalid(
            "invalid counterparty role".into(),
        ));
    }
    if let Some(id) = details.acknowledges {
        if args.channel != channel::EMAIL_INBOUND {
            return Err(CommunicationError::Invalid(
                "only inbound email can acknowledge receipt".into(),
            ));
        }
        let target = find(db, args.project_id, id)
            .await?
            .ok_or(CommunicationError::NotFound)?;
        if target.channel != channel::EMAIL_OUTBOUND || target.direction != direction::OUTBOUND {
            return Err(CommunicationError::Invalid(
                "receipt must acknowledge an outbound email".into(),
            ));
        }
    }
    ingest_with_details(db, args, details).await
}

/// Ingest one message into the conversation log. Idempotent on
/// `(channel, source_ref)` when `source_ref` is `Some`.
///
/// Two halves, and both are load-bearing. The lookup below is the common
/// path — a replayed source returns the existing row as `deduped` rather
/// than an error. The `communication_channel_source_ref` unique index is
/// the guarantee: two racers past the lookup at once cannot both insert,
/// and the loser re-reads the winner's row instead of duplicating a
/// client's message in their own privileged thread.
///
/// An absent `source_ref` does not collide — many rows may carry `NONE`
/// under one unique index — so a document comment, which has no external
/// id, is unrestricted.
///
/// # Errors
///
/// Propagates any database error.
pub async fn ingest(db: &SurrealDb, args: &IngestArgs<'_>) -> Result<Ingested, CommunicationError> {
    ingest_with_details(db, args, EmailDetails::default()).await
}

async fn ingest_with_details(
    db: &SurrealDb,
    args: &IngestArgs<'_>,
    details: EmailDetails<'_>,
) -> Result<Ingested, CommunicationError> {
    if let Some(source_ref) = args.source_ref {
        if let Some(row) = find_by_source(db, args.channel, source_ref).await? {
            if row.project_id != args.project_id {
                return Err(CommunicationError::Invalid(
                    "source reference already belongs to another Project".into(),
                ));
            }
            validate_replayed_details(&row, details)?;
            return Ok(Ingested {
                communication_id: row.id,
                deduped: true,
            });
        }
    }

    let id = Uuid::now_v7();
    let created = db
        .query(format!(
            "CREATE $id SET \
             project_id = $project_id, channel = $channel, direction = $direction, \
             author_person_id = $author, counterparty = $counterparty, subject = $subject, \
             body = $body, source_ref = $source_ref, asset_id = $asset_id, \
             counterparty_role = $counterparty_role, acknowledges = $acknowledges, \
             occurred_at = $occurred_at \
             RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind((
            "project_id",
            record_id(crate::projects::PROJECT_TABLE, args.project_id),
        ))
        .bind(("channel", args.channel.to_string()))
        .bind(("direction", args.direction.to_string()))
        .bind((
            "author",
            args.author_person_id.map(|p| record_id(PERSON_TABLE, p)),
        ))
        .bind(("counterparty", args.counterparty.map(str::to_string)))
        .bind(("subject", args.subject.map(str::to_string)))
        .bind(("body", args.body.to_string()))
        .bind(("source_ref", args.source_ref.map(str::to_string)))
        .bind((
            "counterparty_role",
            details.counterparty_role.map(str::to_string),
        ))
        .bind((
            "acknowledges",
            details.acknowledges.map(|id| record_id(TABLE, id)),
        ))
        .bind((
            "asset_id",
            args.asset_id.map(|a| record_id(crate::assets::TABLE, a)),
        ))
        .bind(("occurred_at", args.occurred_at.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check);

    let mut response = match created {
        Ok(response) => response,
        Err(error) if is_duplicate_source_ref(&error) => {
            // A racer won between our lookup and this insert. Return their
            // row: the whole point of the key is that one source produces
            // one message.
            let Some(source_ref) = args.source_ref else {
                return Err(CommunicationError::Db(error));
            };
            let existing = find_by_source(db, args.channel, source_ref).await?;
            if existing
                .as_ref()
                .is_some_and(|row| row.project_id != args.project_id)
            {
                return Err(CommunicationError::Invalid(
                    "source reference already belongs to another Project".into(),
                ));
            }
            if let Some(row) = &existing {
                validate_replayed_details(row, details)?;
            }
            return existing
                .map(|row| Ingested {
                    communication_id: row.id,
                    deduped: true,
                })
                .ok_or(CommunicationError::Db(error));
        }
        Err(error) => return Err(CommunicationError::Db(error)),
    };
    let row: Option<CommunicationRow> = response.take(0)?;

    let communication_id = row
        .and_then(CommunicationRow::into_communication)
        .map_or(id, |c| c.id);
    mirror_communication(db, communication_id).await;
    Ok(Ingested {
        communication_id,
        deduped: false,
    })
}

/// Whether this failure is the `(channel, source_ref)` key rejecting a
/// duplicate.
///
/// Discriminated on the index name rather than a typed detail: a unique
/// violation arrives as the untyped `Internal` catch-all every
/// unclassified failure also uses, so the index name — which the workspace
/// owns and no other failure mentions — is the only reliable signal.
fn is_duplicate_source_ref(error: &surrealdb::Error) -> bool {
    crate::surreal::retry::unique_violation(error) == Some("communication_channel_source_ref")
}

/// Move one communication onto another Project.
///
/// Assignment keeps the row — and its channel history — and changes the
/// Project the matter surface reads. The previous Project's query no longer
/// returns it.
///
/// # Errors
///
/// Propagates any database error.
pub async fn move_to_project(
    db: &SurrealDb,
    id: Uuid,
    project_id: Uuid,
) -> Result<(), CommunicationError> {
    db.query("UPDATE $id SET project_id = $project, updated_at = time::now()")
        .bind(("id", record_id(TABLE, id)))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    mirror_communication(db, id).await;
    Ok(())
}

/// The row a `(channel, source_ref)` pair already resolves to, if any.
pub(crate) async fn find_by_source(
    db: &SurrealDb,
    channel: &str,
    source_ref: &str,
) -> Result<Option<Communication>, CommunicationError> {
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} \
             WHERE channel = $channel AND source_ref = $source_ref LIMIT 1"
        ))
        .bind(("channel", channel.to_string()))
        .bind(("source_ref", source_ref.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<CommunicationRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .find_map(CommunicationRow::into_communication))
}

/// The whole conversation for one matter, oldest→newest — the order the
/// thread view renders. This is the **firm** view: every row, internal notes
/// included. Whether the caller may read this `project_id` at all is the
/// access layer's job (`store::access::can_see_project`); a client gets
/// [`for_project_client_visible`] instead.
///
/// # Errors
///
/// Propagates any database error.
pub async fn for_project(
    db: &SurrealDb,
    project_id: Uuid,
) -> Result<Vec<Communication>, CommunicationError> {
    read_thread(db, project_id, None).await
}

/// Whether the matter's thread holds any message at all — including a
/// firm-internal note, which [`for_project_client_visible`] hides but which
/// is still a reference. The matter-delete guard.
///
/// # Errors
///
/// Propagates any database error.
pub async fn exists_for_project(
    db: &SurrealDb,
    project_id: Uuid,
) -> Result<bool, CommunicationError> {
    let mut response = db
        .query(format!(
            "SELECT VALUE id FROM {TABLE} WHERE project_id = $project LIMIT 1"
        ))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let ids: Vec<surrealdb::types::RecordId> = response.take(0)?;
    Ok(!ids.is_empty())
}

/// The conversation as a **client** may see it: every row except firm-internal
/// notes (`direction = internal`). Internal notes are firm work product; a
/// client must never read one, so the exclusion is enforced in the query, not
/// left to the template. The firm sees [`for_project`].
///
/// # Errors
///
/// Propagates any database error.
pub async fn for_project_client_visible(
    db: &SurrealDb,
    project_id: Uuid,
) -> Result<Vec<Communication>, CommunicationError> {
    read_thread(db, project_id, Some(direction::INTERNAL)).await
}

/// The body both thread reads share, so the ordering — and the
/// `project_id` scope neither may widen — is written once.
///
/// `occurred_at` then `id`: the timestamp is when the message happened,
/// and the UUIDv7 id breaks ties in insertion order, so two messages
/// stamped the same second still render in the order they arrived.
async fn read_thread(
    db: &SurrealDb,
    project_id: Uuid,
    exclude_direction: Option<&str>,
) -> Result<Vec<Communication>, CommunicationError> {
    let filter = if exclude_direction.is_some() {
        "AND direction != $excluded "
    } else {
        ""
    };
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} \
             WHERE project_id = $project {filter}ORDER BY occurred_at ASC, id ASC"
        ))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .bind((
            "excluded",
            exclude_direction.unwrap_or_default().to_string(),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<CommunicationRow> = response.take(0)?;
    let mut rows: Vec<_> = rows
        .into_iter()
        .filter_map(CommunicationRow::into_communication)
        .collect();
    let mut receipts = std::collections::HashMap::<Uuid, String>::new();
    for row in &rows {
        if row.channel == channel::EMAIL_INBOUND && row.direction == direction::INBOUND {
            if let Some(id) = row.acknowledges {
                receipts
                    .entry(id)
                    .and_modify(|at| {
                        if chrono::DateTime::parse_from_rfc3339(&row.occurred_at).ok()
                            < chrono::DateTime::parse_from_rfc3339(at).ok()
                        {
                            at.clone_from(&row.occurred_at);
                        }
                    })
                    .or_insert_with(|| row.occurred_at.clone());
            }
        }
    }
    for row in &mut rows {
        if row.channel == channel::EMAIL_OUTBOUND && row.direction == direction::OUTBOUND {
            row.receipt_acknowledged_at = receipts.remove(&row.id);
        }
    }
    Ok(rows)
}

/// Errors from a retention purge — the DB deletes and the storage-object
/// deletes can each fail.
#[derive(Debug, thiserror::Error)]
pub enum PurgeError {
    #[error("database: {0}")]
    Db(#[from] CommunicationError),
    #[error("matters: {0}")]
    Projects(#[from] crate::projects::ProjectStoreError),
    #[error("asset: {0}")]
    Asset(#[from] crate::assets::AssetError),
    #[error("storage: {0}")]
    Storage(#[from] StorageError),
    #[error("document comment: {0}")]
    DocumentComment(#[from] crate::document_comments::DocumentCommentError),
}

/// What a purge removed — returned for the caller to log / report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PurgeReport {
    pub communications_deleted: u64,
    pub assets_deleted: u64,
}

/// Permanently delete one matter's privileged conversation log and the raw
/// payloads only it referenced — the end-of-retention destruction the
/// retainer promises. Matter-scoped and logged.
///
/// The comment satellite's `communication_id` link is cleared first (the
/// Phase A review-surface row itself is governed by the separate expunge
/// machinery, not this sweep): the comment must not outlive the spine row
/// it names.
///
/// The order of the three deletes is the guarantee, and it survived the
/// port unchanged. Communications go first, so a crash part-way leaves an
/// asset row whose last referent is gone — an orphan a later sweep
/// reclaims — rather than a communication pointing at bytes that are
/// already deleted. Storage objects go last for the same reason, and only
/// once no remaining asset row references the content-addressed key.
///
/// # Errors
///
/// [`PurgeError`] on a database or storage failure.
pub async fn purge_for_project(
    surreal: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    project_id: Uuid,
) -> Result<PurgeReport, PurgeError> {
    let comms = for_project(surreal, project_id).await?;
    if comms.is_empty() {
        return Ok(PurgeReport::default());
    }
    let comm_ids: Vec<Uuid> = comms.iter().map(|c| c.id).collect();
    let mut asset_ids: Vec<Uuid> = comms.iter().filter_map(|c| c.asset_id).collect();
    asset_ids.sort();
    asset_ids.dedup();

    // Clear the comment satellite's link first: nothing enforces the
    // reference, so the ordering is what keeps a comment from outliving the
    // spine row it names.
    crate::document_comments::clear_communication_links(surreal, &comm_ids).await?;

    let deleted = u64::try_from(comm_ids.len()).unwrap_or(u64::MAX);
    surreal
        .query(format!("DELETE {TABLE} WHERE project_id = $project"))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(CommunicationError::Db)?;
    crate::postgres_mirror::after_writes(
        surreal,
        comm_ids
            .iter()
            .copied()
            .map(crate::postgres_mirror::MirrorKey::Communication),
    )
    .await;

    // Which assets lost their last communication referent — read after the
    // delete, so the answer reflects it. A raw payload referenced by a
    // communication on *another* matter is retained: this sweep destroys one
    // matter's file, and an asset another matter still points at is not part
    // of it.
    let mut orphaned: Vec<Uuid> = Vec::new();
    for aid in asset_ids {
        if !asset_is_referenced(surreal, aid).await? {
            orphaned.push(aid);
        }
    }

    // Now the other engine. Delete each orphaned asset row, then keep its
    // storage key as a candidate only if no other row still references that
    // content-addressed object — `storage_key` is not unique, because
    // identical bytes are stored once and may be referenced several times.
    let mut storage_keys: Vec<String> = Vec::new();
    for aid in orphaned {
        let Some(deleted) = crate::assets::delete(surreal, aid).await? else {
            continue;
        };
        if !crate::assets::storage_key_is_referenced(surreal, &deleted.storage_key).await?
            && !storage_keys.contains(&deleted.storage_key)
        {
            storage_keys.push(deleted.storage_key);
        }
    }

    // Storage objects last — orphaned bytes are recoverable; a dangling row is
    // not.
    for key in &storage_keys {
        storage.delete(key).await?;
    }

    let report = PurgeReport {
        communications_deleted: deleted,
        assets_deleted: storage_keys.len() as u64,
    };
    tracing::info!(
        %project_id,
        communications = report.communications_deleted,
        assets = report.assets_deleted,
        "purged matter conversation log at end of retention",
    );
    Ok(report)
}

/// Whether any communication still points at this raw-payload asset.
async fn asset_is_referenced(db: &SurrealDb, asset_id: Uuid) -> Result<bool, CommunicationError> {
    let mut response = db
        .query(format!(
            "SELECT VALUE id FROM {TABLE} WHERE asset_id = $asset LIMIT 1"
        ))
        .bind(("asset", record_id(crate::assets::TABLE, asset_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let ids: Vec<surrealdb::types::RecordId> = response.take(0)?;
    Ok(!ids.is_empty())
}

/// Purge every matter whose retention window has elapsed: a closed matter
/// (`projects.closed_at` set) is destroyed once `closed_at + retention_years`
/// has passed `now`. `now` is passed in so the sweep is deterministic in
/// tests and replayable in a durable workflow. `retention_years` is normally
/// [`RETENTION_YEARS`].
///
/// # Errors
///
/// [`PurgeError`] on the first database or storage failure.
pub async fn purge_expired_matters(
    surreal: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    now: DateTime<Utc>,
    retention_years: u32,
) -> Result<PurgeReport, PurgeError> {
    let closed_matters = crate::projects::all(surreal).await?;

    let mut total = PurgeReport::default();
    for p in closed_matters {
        let Some(stamp) = p.closed_at.as_deref() else {
            continue;
        };
        let Ok(closed_time) = DateTime::parse_from_rfc3339(stamp) else {
            tracing::warn!(project_id = %p.id, closed_at = stamp, "unparseable closed_at; skipping retention");
            continue;
        };
        let due = closed_time
            .with_timezone(&Utc)
            .checked_add_months(Months::new(retention_years * 12));
        if due.is_some_and(|due| now >= due) {
            let r = purge_for_project(surreal, storage, p.id).await?;
            total.communications_deleted += r.communications_deleted;
            total.assets_deleted += r.assets_deleted;
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::{
        channel, direction, for_project, for_project_client_visible, ingest, purge_expired_matters,
        purge_for_project, CommunicationError, IngestArgs, RETENTION_YEARS,
    };
    use chrono::{DateTime, Utc};
    use std::sync::Arc;
    use uuid::Uuid;

    async fn seed_project(surreal: &crate::surreal::SurrealDb) -> Uuid {
        let id = Uuid::now_v7();
        let __dri = crate::test_support::dri_person(surreal).await;
        crate::projects::create(
            surreal,
            &crate::projects::NewProject {
                code: format!("communication-{id}"),
                name: "Test Matter".into(),
                status: "open".into(),
                entity_id: crate::test_support::seed_entity(surreal).await,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id
    }

    fn args<'a>(project_id: Uuid, ch: &'a str, dir: &'a str, body: &'a str) -> IngestArgs<'a> {
        IngestArgs {
            project_id,
            channel: ch,
            direction: dir,
            author_person_id: None,
            counterparty: None,
            subject: None,
            body,
            source_ref: None,
            asset_id: None,
            occurred_at: "2026-06-08T10:00:00Z",
        }
    }

    #[tokio::test]
    async fn historical_messages_without_email_metadata_remain_readable() {
        let db = crate::surreal::test_support::mem().await;
        let project = seed_project(&db).await;
        let created = ingest(
            &db,
            &args(
                project,
                channel::EMAIL_OUTBOUND,
                direction::OUTBOUND,
                "Sent",
            ),
        )
        .await
        .unwrap();
        db.query("UPDATE $id UNSET counterparty_role, acknowledges")
            .bind((
                "id",
                crate::surreal::record_id(super::TABLE, created.communication_id),
            ))
            .await
            .unwrap()
            .check()
            .unwrap();
        let row = super::find(&db, project, created.communication_id)
            .await
            .unwrap()
            .unwrap();
        assert!(row.counterparty_role.is_none());
        assert!(row.acknowledges.is_none());
        assert!(row.receipt_acknowledged_at.is_none());
    }

    #[tokio::test]
    async fn receipt_projection_uses_earliest_instant_and_only_the_same_project() {
        let db = crate::surreal::test_support::mem().await;
        let project = seed_project(&db).await;
        let outbound = ingest(
            &db,
            &args(
                project,
                channel::EMAIL_OUTBOUND,
                direction::OUTBOUND,
                "Sent",
            ),
        )
        .await
        .unwrap();
        let mut receipt = args(
            project,
            channel::EMAIL_INBOUND,
            direction::INBOUND,
            "Acknowledged",
        );
        let metadata = super::EmailDetails {
            counterparty_role: Some("court"),
            acknowledges: Some(outbound.communication_id),
        };
        receipt.occurred_at = "2026-06-08T12:00:00+03:00";
        let earliest = super::ingest_email(&db, &receipt, metadata).await.unwrap();
        receipt.occurred_at = "2026-06-08T10:00:00Z";
        super::ingest_email(&db, &receipt, metadata).await.unwrap();
        let rows = super::for_project(&db, project).await.unwrap();
        let sent = rows
            .iter()
            .find(|row| row.id == outbound.communication_id)
            .unwrap();
        assert_eq!(
            sent.receipt_acknowledged_at.as_deref(),
            Some("2026-06-08T12:00:00+03:00")
        );
        let other = seed_project(&db).await;
        super::move_to_project(&db, earliest.communication_id, other)
            .await
            .unwrap();
        let rows = super::for_project(&db, project).await.unwrap();
        let sent = rows
            .iter()
            .find(|row| row.id == outbound.communication_id)
            .unwrap();
        assert_eq!(
            sent.receipt_acknowledged_at.as_deref(),
            Some("2026-06-08T10:00:00Z")
        );
    }

    #[tokio::test]
    async fn ingest_inserts_a_spine_row() {
        let surreal = crate::surreal::test_support::mem().await;
        let project_id = seed_project(&surreal).await;

        let out = ingest(
            &surreal,
            &args(
                project_id,
                channel::DOCUMENT_COMMENT,
                direction::INBOUND,
                "Should this be my full legal name?",
            ),
        )
        .await
        .unwrap();
        assert!(!out.deduped);

        let rows = for_project(&surreal, project_id).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, out.communication_id);
        assert_eq!(rows[0].channel, channel::DOCUMENT_COMMENT);
        assert_eq!(rows[0].direction, direction::INBOUND);
        assert_eq!(rows[0].body, "Should this be my full legal name?");
        assert!(rows[0].source_ref.is_none());
    }

    /// LAW-140: the schema accepts `call` and refuses a channel it does not
    /// list; a call is internal by default, so the client-visible read
    /// excludes it while the firm read carries its structure; agreements
    /// stay unconfirmed until an email Communication confirms them.
    #[tokio::test]
    async fn a_call_is_logged_internal_with_agreements_until_an_email_confirms_them() {
        use super::{confirm_agreements, record_call, CallDetails, NewCall};
        let surreal = crate::surreal::test_support::mem().await;
        let project_id = seed_project(&surreal).await;

        let unknown = ingest(
            &surreal,
            &args(project_id, "carrier_pigeon", "internal", "x"),
        )
        .await;
        assert!(
            unknown.is_err(),
            "an unlisted channel is refused by the schema"
        );

        let call = record_call(
            &surreal,
            &NewCall {
                project_id,
                counterparty: "counsel@example.com".into(),
                body: "Discussed the discovery schedule.".into(),
                at: "2026-10-09T15:00:00-07:00".into(),
                call: CallDetails {
                    direction: "outbound".into(),
                    counterparty_role: "opposing_counsel".into(),
                    duration_minutes: Some(20),
                    recorded: "none".into(),
                    meet_and_confer: Some("Responses to Set One".into()),
                },
                agreements: vec!["Extend responses to 2026-11-02".into()],
                ..NewCall::default()
            },
        )
        .await
        .expect("log the call");
        assert_eq!(call.channel, "call");
        assert_eq!(call.direction, "internal");
        assert_eq!(call.call.as_ref().unwrap().duration_minutes, Some(20));
        assert_eq!(call.agreements[0].status, "unconfirmed");
        assert!(call.has_unconfirmed_agreements());

        let firm = for_project(&surreal, project_id).await.unwrap();
        assert_eq!(firm.len(), 1);
        assert_eq!(firm[0].call, call.call);
        assert!(for_project_client_visible(&surreal, project_id)
            .await
            .unwrap()
            .is_empty());

        // A call with opposing counsel cannot be shared with the client.
        let mut shared = NewCall {
            project_id,
            counterparty: "counsel@example.com".into(),
            body: "note".into(),
            at: "2026-10-09T15:00:00-07:00".into(),
            call: call.call.clone().unwrap(),
            client_visible: true,
            ..NewCall::default()
        };
        assert!(record_call(&surreal, &shared).await.is_err());
        shared.call.counterparty_role = "client".into();
        let visible = record_call(&surreal, &shared)
            .await
            .expect("a client call may be shared");
        assert_eq!(visible.direction, "outbound");
        assert_eq!(
            for_project_client_visible(&surreal, project_id)
                .await
                .unwrap()
                .len(),
            1
        );

        let email = ingest(
            &surreal,
            &args(
                project_id,
                "email_outbound",
                "outbound",
                "Confirming our call.",
            ),
        )
        .await
        .unwrap();
        let confirmed = confirm_agreements(&surreal, project_id, call.id, email.communication_id)
            .await
            .expect("confirm");
        assert_eq!(confirmed.agreements[0].status, "confirmed");
        assert_eq!(
            confirmed.agreements[0].confirmed_by.as_deref(),
            Some(email.communication_id.to_string().as_str())
        );
        assert!(!confirmed.has_unconfirmed_agreements());
        assert!(
            confirm_agreements(&surreal, project_id, email.communication_id, call.id)
                .await
                .is_err(),
            "only a call carries agreements"
        );
    }

    #[tokio::test]
    async fn ingest_dedupes_on_channel_and_source_ref() {
        let surreal = crate::surreal::test_support::mem().await;
        let project_id = seed_project(&surreal).await;

        let mut a = args(project_id, channel::EMAIL_INBOUND, direction::INBOUND, "hi");
        a.source_ref = Some("msg-abc@mail.example.com");

        let first = ingest(&surreal, &a).await.unwrap();
        assert!(!first.deduped);

        // Same Message-ID re-delivered (SendGrid retry): no duplicate row.
        let second = ingest(&surreal, &a).await.unwrap();
        assert!(second.deduped);
        assert_eq!(second.communication_id, first.communication_id);

        assert_eq!(for_project(&surreal, project_id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn ingest_refuses_source_ref_replay_across_projects() {
        let surreal = crate::surreal::test_support::mem().await;
        let first_project = seed_project(&surreal).await;
        let second_project = seed_project(&surreal).await;
        let mut first = args(
            first_project,
            channel::EMAIL_INBOUND,
            direction::INBOUND,
            "private",
        );
        first.source_ref = Some("gmail:lawyer@example.test:message-1");
        ingest(&surreal, &first).await.unwrap();

        let mut replay = args(
            second_project,
            channel::EMAIL_INBOUND,
            direction::INBOUND,
            "replacement",
        );
        replay.source_ref = first.source_ref;
        let error = ingest(&surreal, &replay).await.unwrap_err();
        assert!(matches!(error, CommunicationError::Invalid(_)));
        assert_eq!(for_project(&surreal, first_project).await.unwrap().len(), 1);
        assert!(for_project(&surreal, second_project)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn same_source_ref_different_channel_is_distinct() {
        let surreal = crate::surreal::test_support::mem().await;
        let project_id = seed_project(&surreal).await;

        let mut inbound = args(project_id, channel::EMAIL_INBOUND, direction::INBOUND, "q");
        inbound.source_ref = Some("ref-1");
        let mut outbound = args(
            project_id,
            channel::EMAIL_OUTBOUND,
            direction::OUTBOUND,
            "a",
        );
        outbound.source_ref = Some("ref-1");

        let a = ingest(&surreal, &inbound).await.unwrap();
        let b = ingest(&surreal, &outbound).await.unwrap();
        assert_ne!(a.communication_id, b.communication_id);
        assert_eq!(for_project(&surreal, project_id).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn the_source_ref_key_is_enforced_by_the_database_not_only_the_lookup() {
        // The read-then-write in `ingest` is the common path; the unique
        // index is the guarantee. Written around the command — as a racer
        // that got past the lookup would be — the second insert must still
        // be refused, or one client message can land twice in their own
        // privileged thread.
        let surreal = crate::surreal::test_support::mem().await;
        let project_id = seed_project(&surreal).await;
        let mut first = args(
            project_id,
            channel::EMAIL_INBOUND,
            direction::INBOUND,
            "the only copy",
        );
        first.source_ref = Some("<dupe@mail.example.com>");
        ingest(&surreal, &first).await.unwrap();

        let raw = surreal
            .query(
                "CREATE type::record('communication', rand::uuid::v7()) SET \
                 project_id = $project, channel = $channel, direction = 'inbound', \
                 body = 'a second copy', source_ref = $source_ref, \
                 occurred_at = '2026-06-08T10:00:00Z'",
            )
            .bind((
                "project",
                crate::surreal::record_id(crate::projects::PROJECT_TABLE, project_id),
            ))
            .bind(("channel", channel::EMAIL_INBOUND.to_string()))
            .bind(("source_ref", "<dupe@mail.example.com>".to_string()))
            .await
            .and_then(surrealdb::IndexedResults::check)
            .expect_err("the unique index must refuse a duplicate source_ref");
        assert!(
            super::is_duplicate_source_ref(&raw),
            "the refusal must be recognizable as the source-ref key: {raw}"
        );
        assert_eq!(for_project(&surreal, project_id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn null_source_ref_never_dedupes() {
        let surreal = crate::surreal::test_support::mem().await;
        let project_id = seed_project(&surreal).await;

        // Two comments with no external id are distinct events. This also
        // holds the `(channel, source_ref)` unique index honest: an absent
        // `source_ref` must not collide, or the first comment on a matter
        // would block every later one.
        ingest(
            &surreal,
            &args(
                project_id,
                channel::DOCUMENT_COMMENT,
                direction::INBOUND,
                "one",
            ),
        )
        .await
        .unwrap();
        ingest(
            &surreal,
            &args(
                project_id,
                channel::DOCUMENT_COMMENT,
                direction::INBOUND,
                "two",
            ),
        )
        .await
        .unwrap();

        assert_eq!(for_project(&surreal, project_id).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn client_visible_excludes_internal_firm_notes() {
        let surreal = crate::surreal::test_support::mem().await;
        let project_id = seed_project(&surreal).await;

        // A client-facing inbound message and a firm-internal note.
        ingest(
            &surreal,
            &args(
                project_id,
                channel::EMAIL_INBOUND,
                direction::INBOUND,
                "client question",
            ),
        )
        .await
        .unwrap();
        ingest(
            &surreal,
            &args(
                project_id,
                channel::PORTAL_MESSAGE,
                direction::INTERNAL,
                "FIRM EYES ONLY — strategy note",
            ),
        )
        .await
        .unwrap();

        // The firm sees both; the client sees only the non-internal row, and
        // never the internal note's body.
        assert_eq!(for_project(&surreal, project_id).await.unwrap().len(), 2);
        let client_view = for_project_client_visible(&surreal, project_id)
            .await
            .unwrap();
        assert_eq!(client_view.len(), 1);
        assert_eq!(client_view[0].body, "client question");
        assert!(
            client_view
                .iter()
                .all(|c| c.direction != direction::INTERNAL),
            "a client must never read a firm-internal note"
        );
    }

    #[tokio::test]
    async fn for_project_returns_thread_oldest_first() {
        let surreal = crate::surreal::test_support::mem().await;
        let project_id = seed_project(&surreal).await;

        let mut later = args(
            project_id,
            channel::EMAIL_OUTBOUND,
            direction::OUTBOUND,
            "reply",
        );
        later.occurred_at = "2026-06-08T12:00:00Z";
        let mut earlier = args(
            project_id,
            channel::EMAIL_INBOUND,
            direction::INBOUND,
            "question",
        );
        earlier.occurred_at = "2026-06-08T09:00:00Z";

        ingest(&surreal, &later).await.unwrap();
        ingest(&surreal, &earlier).await.unwrap();

        let rows = for_project(&surreal, project_id).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].body, "question");
        assert_eq!(rows[1].body, "reply");
    }

    async fn fs_storage() -> Arc<dyn cloud::StorageService> {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        Arc::new(cloud::FsStorage::new(root).await.unwrap())
    }

    #[tokio::test]
    async fn purge_for_project_deletes_the_log_and_its_raw_payload() {
        let surreal = crate::surreal::test_support::mem().await;
        let storage = fs_storage().await;
        let project_id = seed_project(&surreal).await;

        // A raw .eml payload stored as a content-addressed asset, referenced by
        // an inbound-email communication.
        let asset_id = crate::assets::ingest_content(
            &surreal,
            &storage,
            b"raw rfc5322 bytes",
            "message/rfc822",
        )
        .await
        .unwrap();
        let storage_key = crate::assets::find_by_id(&surreal, asset_id)
            .await
            .unwrap()
            .unwrap()
            .storage_key;

        let mut with_asset = args(
            project_id,
            channel::EMAIL_INBOUND,
            direction::INBOUND,
            "raw email body",
        );
        with_asset.asset_id = Some(asset_id);
        ingest(&surreal, &with_asset).await.unwrap();
        ingest(
            &surreal,
            &args(
                project_id,
                channel::PORTAL_MESSAGE,
                direction::INTERNAL,
                "a note",
            ),
        )
        .await
        .unwrap();

        let report = purge_for_project(&surreal, &storage, project_id)
            .await
            .unwrap();
        assert_eq!(report.communications_deleted, 2);
        assert_eq!(report.assets_deleted, 1);

        // The conversation log is gone, the asset row is gone, and the raw
        // payload is gone from storage — the row and its bytes share a fate.
        assert!(for_project(&surreal, project_id).await.unwrap().is_empty());
        assert!(crate::assets::find_by_id(&surreal, asset_id)
            .await
            .unwrap()
            .is_none());
        assert!(storage.get(&storage_key).await.is_err());
    }

    #[tokio::test]
    async fn purge_expired_matters_only_purges_past_the_retention_window() {
        let surreal = crate::surreal::test_support::mem().await;
        let storage = fs_storage().await;

        // now, an old close (11 years ago → expired), a recent close (kept).
        let now: DateTime<Utc> = "2026-06-08T00:00:00Z".parse().unwrap();
        let old_project = seed_project(&surreal).await;
        let recent_project = seed_project(&surreal).await;
        set_closed_at(&surreal, old_project, "2015-01-01T00:00:00Z").await;
        set_closed_at(&surreal, recent_project, "2025-12-01T00:00:00Z").await;

        ingest(
            &surreal,
            &args(
                old_project,
                channel::EMAIL_INBOUND,
                direction::INBOUND,
                "old",
            ),
        )
        .await
        .unwrap();
        ingest(
            &surreal,
            &args(
                recent_project,
                channel::EMAIL_INBOUND,
                direction::INBOUND,
                "recent",
            ),
        )
        .await
        .unwrap();

        let report = purge_expired_matters(&surreal, &storage, now, RETENTION_YEARS)
            .await
            .unwrap();
        assert_eq!(report.communications_deleted, 1, "only the expired matter");

        // The 11-year-old matter's log is destroyed; the recent one is kept.
        assert!(for_project(&surreal, old_project).await.unwrap().is_empty());
        assert_eq!(
            for_project(&surreal, recent_project).await.unwrap().len(),
            1
        );
    }

    async fn set_closed_at(surreal: &crate::surreal::SurrealDb, project_id: Uuid, when: &str) {
        surreal
            .query("UPDATE $id SET status = 'closed', closed_at = $closed_at")
            .bind(("id", crate::surreal::record_id("project", project_id)))
            .bind(("closed_at", when.to_string()))
            .await
            .and_then(surrealdb::IndexedResults::check)
            .unwrap();
    }
}
