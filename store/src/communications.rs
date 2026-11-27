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
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::communications::{self as authority, CommunicationsAuthorityError};

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
    pub const X_INBOUND: &str = "x_inbound";
    pub const X_OUTBOUND: &str = "x_outbound";
    pub const LINKEDIN_INBOUND: &str = "linkedin_inbound";
    pub const LINKEDIN_OUTBOUND: &str = "linkedin_outbound";
    /// A phone call or other off-system contact, logged after the fact as
    /// an internal note (LAW-140). The row's `direction` is its visibility
    /// (`internal` unless the firm chose to share it); the call's own
    /// direction lives in [`super::CallDetails`].
    pub const CALL: &str = "call";
    /// A lawyer's matter note (LAW-230): an observation filed on the matter
    /// by the matter-note verb, with no counterparty and no external source.
    /// Internal by definition — `direction` is always
    /// [`super::direction::INTERNAL`] — so it renders on the workbench Notes
    /// tab and never on a client view.
    pub const NOTE: &str = "note";
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct CallDetails {
    /// `inbound` or `outbound`: who placed the call.
    pub direction: String,
    /// One of [`counterparty_role`].
    pub counterparty_role: String,
    pub duration_minutes: Option<i64>,
    /// One of [`recorded`].
    pub recorded: String,
    /// The firm chose to share this call with the client (LAW-424). The
    /// client lens reads this flag, not `direction`: a call row's
    /// top-level direction is the call's own, so sharing carries its own
    /// signal. Rows written before the field lack the key, and the lens
    /// still reads their share state off `direction != internal`.
    #[serde(default)]
    pub client_visible: bool,
    /// The issue a meet-and-confer call addressed, when it was one.
    pub meet_and_confer: Option<String>,
}

/// Agreement status: unconfirmed until a confirming email is linked.
pub mod agreement_status {
    pub const UNCONFIRMED: &str = "unconfirmed";
    pub const CONFIRMED: &str = "confirmed";
}

/// One thing agreed on a call, tracked until an email confirms it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
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
    #[serde(default)]
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

/// A PostgreSQL row as `row`, `occurred_at` rendered as RFC 3339 text the
/// way the SurrealDB rows carry it.
macro_rules! pg_communication {
    () => {
        "SELECT to_jsonb(c) || jsonb_build_object('occurred_at', \
         to_char(c.occurred_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.USZ')) AS row \
         FROM communication c"
    };
}

/// The `(channel, source_ref)` unique index, named alike in both stores.
const SOURCE_REF_INDEX: &str = "communication_channel_source_ref";

/// What can go wrong reading or writing the conversation log.
#[derive(Debug, thiserror::Error)]
pub enum CommunicationError {
    /// A database operation failed.
    /// PostgreSQL, under the communications authority, failed.
    #[error(transparent)]
    Postgres(#[from] CommunicationsAuthorityError),
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
    /// Share the call with the client: persisted on the row's
    /// `call.client_visible`, which the client lens reads. Refused for
    /// opposing counsel.
    pub client_visible: bool,
}

/// Refuse a call's direction, role, recording consent, or duration the
/// caller can correct.
fn check_call(call: &CallDetails) -> Result<(), CommunicationError> {
    if !matches!(call.direction.as_str(), "inbound" | "outbound") {
        return Err(CommunicationError::Invalid(
            "direction must be inbound or outbound".into(),
        ));
    }
    if !counterparty_role::ALL.contains(&call.counterparty_role.as_str()) {
        return Err(CommunicationError::Invalid(format!(
            "role must be one of {}",
            counterparty_role::ALL.join(", ")
        )));
    }
    if !recorded::ALL.contains(&call.recorded.as_str()) {
        return Err(CommunicationError::Invalid(format!(
            "recorded must be one of {}",
            recorded::ALL.join(", ")
        )));
    }
    if call.duration_minutes.is_some_and(|minutes| minutes < 0) {
        return Err(CommunicationError::Invalid(
            "duration must be zero or more minutes".into(),
        ));
    }
    Ok(())
}

/// Log a call as a Communication (LAW-140). The row reads as the call
/// that happened (LAW-424): top-level `direction` and `counterparty_role`
/// are the call's own flags, shared or not. A call with opposing counsel
/// is never client-visible: that note is work product.
///
/// # Errors
/// [`CommunicationError::Invalid`] for a value the caller can correct, or
/// any database error.
pub async fn record_call(
    _db: &ApplicationStore,
    new: &NewCall,
) -> Result<Communication, CommunicationError> {
    check_call(&new.call)?;
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
    // LAW-424: the row carries the call's own direction and role at the
    // top level; sharing is the row's `call.client_visible` signal.
    let direction = new.call.direction.clone();
    let mut call = new.call.clone();
    call.client_visible = new.client_visible;
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
    let pg = authority::authority().await?;
    let row = serde_json::json!({
        "id": id, "project_id": new.project_id, "channel": channel::CALL,
        "direction": direction, "author_person_id": new.author_person_id,
        "counterparty": counterparty, "subject": new.subject, "body": body,
        "occurred_at": at, "counterparty_role": new.call.counterparty_role,
        "call": call, "agreements": agreements,
    });
    let query = sqlx::query(
        "INSERT INTO communication SELECT * FROM jsonb_populate_record(NULL::communication, \
         $1 || jsonb_build_object('inserted_at', now(), 'updated_at', now()))",
    )
    .bind(row);
    authority::execute(&pg, query).await?;
    find_pg(&pg, new.project_id, id)
        .await?
        .ok_or_else(|| CommunicationError::Invalid("the call could not be read back".into()))
}

/// One Communication on a Project, or `None`.
///
/// # Errors
/// Propagates any database error.
pub async fn find(
    _db: &ApplicationStore,
    project_id: Uuid,
    id: Uuid,
) -> Result<Option<Communication>, CommunicationError> {
    let pg = authority::authority().await?;
    Ok(find_pg(&pg, project_id, id).await?)
}

/// Mark every unconfirmed agreement on the call `call_id` as confirmed by
/// the email Communication `confirmed_by`, and return the call.
///
/// # Errors
/// [`CommunicationError::NotFound`] when `call_id` is not a call on this
/// Project, or any database error.
pub async fn confirm_agreements(
    db: &ApplicationStore,
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
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "UPDATE communication SET agreements = $3, updated_at = now() \
         WHERE id = $1 AND project_id = $2",
    )
    .bind(call_id)
    .bind(project_id)
    .bind(serde_json::to_value(&agreements).map_err(CommunicationsAuthorityError::from)?);
    authority::execute(&pg, query).await?;
    find_pg(&pg, project_id, call_id)
        .await?
        .ok_or(CommunicationError::NotFound)
}

/// Inputs to [`log_note`].
#[derive(Debug, Clone)]
pub struct NewNote {
    /// Matter the note is filed on.
    pub project_id: Uuid,
    /// The lawyer who wrote it, when there is a `persons` row.
    pub author_person_id: Option<Uuid>,
    /// Optional subject line.
    pub subject: Option<String>,
    /// The note's text.
    pub body: String,
}

/// Log a lawyer's matter note as an internal Communication (LAW-230).
///
/// A note has no counterparty, no external source, and no client
/// visibility: it is firm work product on the conversation spine, which
/// makes the client view's exclusion structural (`for_project_client_visible`
/// filters `direction = 'internal'`) rather than a convention each reader
/// has to remember.
///
/// # Errors
/// [`CommunicationError::Invalid`] when the body is empty; propagates
/// database errors.
pub async fn log_note(
    db: &ApplicationStore,
    note: &NewNote,
) -> Result<Communication, CommunicationError> {
    let body = note.body.trim();
    if body.is_empty() {
        return Err(CommunicationError::Invalid("the note is empty".into()));
    }
    let ingested = ingest(
        db,
        &IngestArgs {
            project_id: note.project_id,
            channel: channel::NOTE,
            direction: direction::INTERNAL,
            author_person_id: note.author_person_id,
            counterparty: None,
            subject: note
                .subject
                .as_deref()
                .map(str::trim)
                .filter(|subject| !subject.is_empty()),
            body,
            source_ref: None,
            asset_id: None,
            occurred_at: &chrono::Utc::now().to_rfc3339(),
        },
    )
    .await?;
    find(db, note.project_id, ingested.communication_id)
        .await?
        .ok_or_else(|| CommunicationError::Invalid("the note could not be read back".into()))
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
    db: &ApplicationStore,
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
pub async fn ingest(
    db: &ApplicationStore,
    args: &IngestArgs<'_>,
) -> Result<Ingested, CommunicationError> {
    ingest_with_details(db, args, EmailDetails::default()).await
}

/// The row a replayed `(channel, source_ref)` already resolves to, checked
/// against this ingest: the same Project and the same email metadata.
async fn replayed(
    db: &ApplicationStore,
    args: &IngestArgs<'_>,
    details: EmailDetails<'_>,
) -> Result<Option<Ingested>, CommunicationError> {
    let Some(source_ref) = args.source_ref else {
        return Ok(None);
    };
    let Some(row) = find_by_source(db, args.channel, source_ref).await? else {
        return Ok(None);
    };
    if row.project_id != args.project_id {
        return Err(CommunicationError::Invalid(
            "source reference already belongs to another Project".into(),
        ));
    }
    validate_replayed_details(&row, details)?;
    Ok(Some(Ingested {
        communication_id: row.id,
        deduped: true,
    }))
}

async fn ingest_with_details(
    db: &ApplicationStore,
    args: &IngestArgs<'_>,
    details: EmailDetails<'_>,
) -> Result<Ingested, CommunicationError> {
    if let Some(ingested) = replayed(db, args, details).await? {
        return Ok(ingested);
    }

    let id = Uuid::now_v7();
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "INSERT INTO communication (id, project_id, channel, direction, author_person_id, \
         counterparty, subject, body, source_ref, asset_id, counterparty_role, acknowledges, \
         occurred_at, inserted_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13::timestamptz, \
         now(), now())",
    )
    .bind(id)
    .bind(args.project_id)
    .bind(args.channel.to_owned())
    .bind(args.direction.to_owned())
    .bind(args.author_person_id)
    .bind(args.counterparty.map(str::to_owned))
    .bind(args.subject.map(str::to_owned))
    .bind(args.body.to_owned())
    .bind(args.source_ref.map(str::to_owned))
    .bind(args.asset_id)
    .bind(details.counterparty_role.map(str::to_owned))
    .bind(details.acknowledges)
    .bind(args.occurred_at.to_owned());
    match authority::execute(&pg, query).await {
        Ok(_) => Ok(Ingested {
            communication_id: id,
            deduped: false,
        }),
        // A racer won between our lookup and this insert.
        Err(error) if error.violates(SOURCE_REF_INDEX) => replayed(db, args, details)
            .await?
            .ok_or_else(|| error.into()),
        Err(error) => Err(error.into()),
    }
}

fn find_pg(
    pg: &crate::postgres::PostgresDb,
    project_id: Uuid,
    id: Uuid,
) -> authority::Op<'_, Option<Communication>> {
    let query = sqlx::query(concat!(
        pg_communication!(),
        " WHERE c.id = $1 AND c.project_id = $2"
    ))
    .bind(id)
    .bind(project_id);
    authority::maybe(pg, query)
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
    _db: &ApplicationStore,
    id: Uuid,
    project_id: Uuid,
) -> Result<(), CommunicationError> {
    let pg = authority::authority().await?;
    let query =
        sqlx::query("UPDATE communication SET project_id = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(project_id);
    authority::execute(&pg, query).await?;
    Ok(())
}

/// The row a `(channel, source_ref)` pair already resolves to, if any.
pub(crate) async fn find_by_source(
    _db: &ApplicationStore,
    channel: &str,
    source_ref: &str,
) -> Result<Option<Communication>, CommunicationError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(concat!(
        pg_communication!(),
        " WHERE c.channel = $1 AND c.source_ref = $2"
    ))
    .bind(channel.to_owned())
    .bind(source_ref.to_owned());
    Ok(authority::maybe(&pg, query).await?)
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
    db: &ApplicationStore,
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
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<bool, CommunicationError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(EXISTS (SELECT 1 FROM communication WHERE project_id = $1)) AS row",
    )
    .bind(project_id);
    Ok(authority::maybe(&pg, query).await?.unwrap_or(false))
}

/// The conversation as a **client** may see it: every row except
/// firm-internal notes (`direction = internal`), and every call whose
/// `call.client_visible` flag is not set (LAW-424). Internal notes are
/// firm work product; a client must never read one, so the exclusion is
/// enforced in the query, not left to the template. The firm sees
/// [`for_project`].
///
/// # Errors
///
/// Propagates any database error.
pub async fn for_project_client_visible(
    db: &ApplicationStore,
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
    _db: &ApplicationStore,
    project_id: Uuid,
    exclude_direction: Option<&str>,
) -> Result<Vec<Communication>, CommunicationError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(concat!(
        pg_communication!(),
        " WHERE c.project_id = $1 AND c.direction IS DISTINCT FROM $2 \
         AND ($2::text IS NULL OR c.call IS NULL OR COALESCE((c.call->>'client_visible')::boolean, TRUE)) \
         ORDER BY c.occurred_at, c.id"
    ))
    .bind(project_id)
    .bind(exclude_direction.map(str::to_owned));
    Ok(with_receipts(authority::rows(&pg, query).await?))
}

/// Stamp each outbound email with the earliest inbound receipt in the
/// thread that acknowledges it.
fn with_receipts(mut rows: Vec<Communication>) -> Vec<Communication> {
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
    rows
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
    application_store: &ApplicationStore,
    storage: &Arc<dyn StorageService>,
    project_id: Uuid,
) -> Result<PurgeReport, PurgeError> {
    let comms = for_project(application_store, project_id).await?;
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
    crate::document_comments::clear_communication_links(application_store, &comm_ids).await?;

    let deleted = u64::try_from(comm_ids.len()).unwrap_or(u64::MAX);
    {
        let pg = authority::authority()
            .await
            .map_err(CommunicationError::from)?;
        let query = sqlx::query("DELETE FROM communication WHERE project_id = $1").bind(project_id);
        authority::execute(&pg, query)
            .await
            .map_err(CommunicationError::from)?;
    }

    // Which assets lost their last communication referent — read after the
    // delete, so the answer reflects it. A raw payload referenced by a
    // communication on *another* matter is retained: this sweep destroys one
    // matter's file, and an asset another matter still points at is not part
    // of it.
    let mut orphaned: Vec<Uuid> = Vec::new();
    for aid in asset_ids {
        if !asset_is_referenced(application_store, aid).await? {
            orphaned.push(aid);
        }
    }

    // Now the other engine. Delete each orphaned asset row, then keep its
    // storage key as a candidate only if no other row still references that
    // content-addressed object — `storage_key` is not unique, because
    // identical bytes are stored once and may be referenced several times.
    let mut storage_keys: Vec<String> = Vec::new();
    for aid in orphaned {
        // A filed Project document can be linked from a manually logged
        // Communication. Its own retention and Legal Hold still govern it.
        if crate::assets::find_by_id(application_store, aid)
            .await?
            .is_some_and(|asset| asset.project_id.is_some())
        {
            continue;
        }
        let Some(deleted) = crate::assets::delete(application_store, aid).await? else {
            continue;
        };
        if !crate::assets::storage_key_is_referenced(application_store, &deleted.storage_key)
            .await?
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
async fn asset_is_referenced(
    _db: &ApplicationStore,
    asset_id: Uuid,
) -> Result<bool, CommunicationError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(EXISTS (SELECT 1 FROM communication WHERE asset_id = $1)) AS row",
    )
    .bind(asset_id);
    Ok(authority::maybe(&pg, query).await?.unwrap_or(false))
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
    application_store: &ApplicationStore,
    storage: &Arc<dyn StorageService>,
    now: DateTime<Utc>,
    retention_years: u32,
) -> Result<PurgeReport, PurgeError> {
    let closed_matters = crate::projects::all(application_store).await?;

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
            let r = purge_for_project(application_store, storage, p.id).await?;
            total.communications_deleted += r.communications_deleted;
            total.assets_deleted += r.assets_deleted;
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod native_tests;
