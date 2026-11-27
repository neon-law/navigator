//! Helpers for the `email_conversations` + `email_conversation_messages`
//! tables — the threaded support inbox behind `support@neonlaw.com`.
//!
//! `web` reaches these so it can open a thread when a new message lands,
//! look one up by its `Reply-To` token when a reply comes back, append
//! each hop to the transcript, and project the conversation's `status`.
//! The thread token is **caller-supplied** — `web` mints an unguessable
//! value; `store` stays free of randomness so its behavior is
//! deterministic under test.
//!
//! # These tables live in SurrealDB
//!
//! Both moved with wave six of #1093 (ENG-160), in the communications
//! slice alongside [`crate::communications`].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::communications::{self as authority, CommunicationsAuthorityError};

/// Freshly opened; not yet acted on.
pub const STATUS_OPEN: &str = "open";
/// Lawyers have been notified; waiting on the attorney to reply.
pub const STATUS_AWAITING_LAWYER: &str = "awaiting_lawyer";
/// A reply was relayed out; waiting on the external party.
pub const STATUS_AWAITING_CLIENT: &str = "awaiting_client";
/// Closed — no further relay.
pub const STATUS_CLOSED: &str = "closed";

/// Inbound from the external party (client / prospective client).
pub const DIRECTION_FROM_EXTERNAL: &str = "from_external";
/// The notification we send the attorney's cockpit inbox.
pub const DIRECTION_TO_LAWYER: &str = "to_lawyer";
/// The attorney's reply back into the thread.
pub const DIRECTION_FROM_LAWYER: &str = "from_lawyer";
/// The relay we send the external party as `support@`.
pub const DIRECTION_TO_EXTERNAL: &str = "to_external";
/// A system note (e.g. conflict-check result); not an email hop.
pub const DIRECTION_SYSTEM: &str = "system";

/// What can go wrong reading or writing a support thread.
#[derive(Debug, Error)]
pub enum EmailConversationError {
    /// A database operation failed.
    /// PostgreSQL, under the communications authority, failed.
    #[error(transparent)]
    Postgres(#[from] CommunicationsAuthorityError),
    /// The token already belongs to another thread. Its own variant
    /// because a token collision is the one failure a caller can act on:
    /// mint another and retry.
    #[error("that thread token is already in use")]
    TokenTaken,
}

/// One threaded support exchange.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailConversation {
    pub id: Uuid,
    /// Opaque, unguessable thread token; the VERP key in `Reply-To`.
    pub token: String,
    /// The external party's address (client or prospective client).
    pub external_email: String,
    /// The external party's display name, if the inbound carried one.
    pub external_name: Option<String>,
    /// The matched [`crate::persons`] row; `None` until conflict-checked.
    pub person_id: Option<Uuid>,
    /// Subject of the originating message.
    pub subject: String,
    /// One of the `STATUS_*` constants.
    pub status: String,
    /// The matter this thread drives, if any.
    pub notation_id: Option<Uuid>,
    /// Firm intake scope. `None` on a row written before assignment existed.
    pub firm_id: Option<Uuid>,
    /// Project the Owner assigned. `None` while the thread is still intake.
    pub project_id: Option<Uuid>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A PostgreSQL conversation as `row`.
macro_rules! pg_conversation {
    () => {
        "SELECT to_jsonb(c) AS row FROM email_conversation c"
    };
}

/// PostgreSQL's unique index on the thread token.
const PG_TOKEN_INDEX: &str = "email_conversation_token_key";

/// One hop in a thread's append-only transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailConversationMessage {
    pub id: Uuid,
    pub conversation_id: Uuid,
    /// One of the `DIRECTION_*` constants.
    pub direction: String,
    pub from_addr: String,
    pub to_addr: String,
    pub subject: String,
    /// Cleaned body — quoted history and signature stripped on lawyer
    /// replies, so a relayed message carries only the new prose.
    pub body_text: String,
    /// Object-storage key of the raw `.eml` for inbound hops; `None` for
    /// messages we generated.
    pub raw_storage_key: Option<String>,
    /// The provider message id — a join key to the delivery stream. A
    /// provider id may recur across distinct transcript hops.
    pub provider_message_id: Option<String>,
    /// RFC 5322 `In-Reply-To` of this hop, when present.
    pub in_reply_to: Option<String>,
    /// Parsed lawyer-reply directives (`@approve`/`@deny`/…) as JSON;
    /// `None` when the reply carried no command.
    pub command_payload: Option<String>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// What to record when opening a new support thread. `status` defaults to
/// [`STATUS_OPEN`] via [`open`].
#[derive(Debug, Clone)]
pub struct NewConversation<'a> {
    /// Unguessable thread token; the VERP key carried in `Reply-To`.
    pub token: &'a str,
    pub external_email: &'a str,
    pub external_name: Option<&'a str>,
    pub subject: &'a str,
    /// Matched `persons.id`, if the sender is already known.
    pub person_id: Option<Uuid>,
    /// Linked matter, if this thread drives one.
    pub notation_id: Option<Uuid>,
}

/// One hop to append to a thread's transcript. See the `DIRECTION_*`
/// constants above.
///
/// `Default` lets callers fill only the fields a given hop needs and
/// elide the optional tail (`raw_storage_key` / `provider_message_id` /
/// `in_reply_to` / `command_payload`) with `..Default::default()` — the
/// required fields are always set explicitly at the call site.
#[derive(Debug, Clone, Default)]
pub struct NewMessage<'a> {
    pub conversation_id: Uuid,
    pub direction: &'a str,
    pub from_addr: &'a str,
    pub to_addr: &'a str,
    pub subject: &'a str,
    pub body_text: &'a str,
    pub raw_storage_key: Option<&'a str>,
    pub provider_message_id: Option<&'a str>,
    pub in_reply_to: Option<&'a str>,
    pub command_payload: Option<&'a str>,
}

/// Open a new conversation at `status = open`, returning its id.
///
/// # Errors
///
/// [`EmailConversationError::TokenTaken`] when the token already threads
/// another conversation, or any other database error.
pub async fn open(
    _db: &ApplicationStore,
    new: &NewConversation<'_>,
) -> Result<Uuid, EmailConversationError> {
    let id = Uuid::now_v7();
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "INSERT INTO email_conversation (id, token, external_email, external_name, \
         person_id, subject, status, notation_id, inserted_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now(), now())",
    )
    .bind(id)
    .bind(new.token.to_owned())
    .bind(new.external_email.to_owned())
    .bind(new.external_name.map(str::to_owned))
    .bind(new.person_id)
    .bind(new.subject.to_owned())
    .bind(STATUS_OPEN)
    .bind(new.notation_id);
    match authority::execute(&pg, query).await {
        Ok(_) => Ok(id),
        Err(error) if error.violates(PG_TOKEN_INDEX) => Err(EmailConversationError::TokenTaken),
        Err(error) => Err(error.into()),
    }
}

/// Look up a conversation by its `Reply-To` token — the threading lookup
/// run on every inbound reply.
///
/// # Errors
///
/// Propagates any database error.
pub async fn by_token(
    _db: &ApplicationStore,
    token: &str,
) -> Result<Option<EmailConversation>, EmailConversationError> {
    let pg = authority::authority().await?;
    let query =
        sqlx::query(concat!(pg_conversation!(), " WHERE c.token = $1")).bind(token.to_owned());
    Ok(authority::maybe(&pg, query).await?)
}

/// The most recent conversation opened by an external address — the
/// "is this person already talking to us?" lookup, which the
/// `email_conversation_external_email` index serves.
///
/// Newest first, because a person who writes in twice is asking about
/// their current exchange, not a closed one.
///
/// # Errors
///
/// Propagates any database error.
pub async fn by_external_email(
    _db: &ApplicationStore,
    external_email: &str,
) -> Result<Option<EmailConversation>, EmailConversationError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(concat!(
        pg_conversation!(),
        " WHERE c.external_email = $1 ORDER BY c.id DESC LIMIT 1"
    ))
    .bind(external_email.to_owned());
    Ok(authority::maybe(&pg, query).await?)
}

/// Load one conversation by id.
///
/// # Errors
///
/// Propagates any database error.
pub async fn by_id(
    _db: &ApplicationStore,
    id: Uuid,
) -> Result<Option<EmailConversation>, EmailConversationError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(concat!(pg_conversation!(), " WHERE c.id = $1")).bind(id);
    Ok(authority::maybe(&pg, query).await?)
}

/// Append one hop to a thread's transcript, returning its id. Does not
/// touch the conversation's `status` — callers advance that explicitly
/// via [`set_status`] so the projection stays under their control.
///
/// # Errors
///
/// Propagates any database error.
pub async fn append(
    _db: &ApplicationStore,
    new: &NewMessage<'_>,
) -> Result<Uuid, EmailConversationError> {
    let id = Uuid::now_v7();
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "INSERT INTO email_conversation_message (id, conversation_id, direction, from_addr, \
         to_addr, subject, body_text, raw_storage_key, provider_message_id, in_reply_to, \
         command_payload, inserted_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, now(), now())",
    )
    .bind(id)
    .bind(new.conversation_id)
    .bind(new.direction.to_owned())
    .bind(new.from_addr.to_owned())
    .bind(new.to_addr.to_owned())
    .bind(new.subject.to_owned())
    .bind(new.body_text.to_owned())
    .bind(new.raw_storage_key.map(str::to_owned))
    .bind(new.provider_message_id.map(str::to_owned))
    .bind(new.in_reply_to.map(str::to_owned))
    .bind(new.command_payload.map(str::to_owned));
    authority::execute(&pg, query).await?;
    Ok(id)
}

/// The full transcript of a conversation, oldest hop first.
///
/// # Errors
///
/// Propagates any database error.
pub async fn messages(
    _db: &ApplicationStore,
    conversation_id: Uuid,
) -> Result<Vec<EmailConversationMessage>, EmailConversationError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(m) AS row FROM email_conversation_message m \
         WHERE m.conversation_id = $1 ORDER BY m.id",
    )
    .bind(conversation_id);
    Ok(authority::rows(&pg, query).await?)
}

/// Resolve one message by its own row id, independent of which conversation
/// it belongs to — the lookup `navigator site mail file` (ENG-517) needs: an
/// operator names the message they are looking at, not its thread.
///
/// # Errors
///
/// Propagates any database error.
pub async fn message_by_id(
    _db: &ApplicationStore,
    id: Uuid,
) -> Result<Option<EmailConversationMessage>, EmailConversationError> {
    let pg = authority::authority().await?;
    let query =
        sqlx::query("SELECT to_jsonb(m) AS row FROM email_conversation_message m WHERE m.id = $1")
            .bind(id);
    Ok(authority::maybe(&pg, query).await?)
}

/// Move a conversation to a new `status`. Returns the updated row, or
/// `Ok(None)` if no row matched.
///
/// # Errors
///
/// Propagates any database error.
pub async fn set_status(
    _db: &ApplicationStore,
    id: Uuid,
    status: &str,
) -> Result<Option<EmailConversation>, EmailConversationError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "UPDATE email_conversation c SET status = $2, updated_at = now() WHERE c.id = $1 \
         RETURNING to_jsonb(c) AS row",
    )
    .bind(id)
    .bind(status.to_owned());
    Ok(authority::maybe(&pg, query).await?)
}

/// Link a conversation to a running workflow notation (the `@link` lawyer
/// command). Once set, the `@approve`/`@deny`/`@signal` command channel
/// fires on this notation and inbound attachments file onto its matter.
/// Returns `None` when no conversation has `id`.
///
/// # Errors
///
/// Propagates any database error.
pub async fn set_notation(
    _db: &ApplicationStore,
    id: Uuid,
    notation_id: Uuid,
) -> Result<Option<EmailConversation>, EmailConversationError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "UPDATE email_conversation c SET notation_id = $2, updated_at = now() \
         WHERE c.id = $1 RETURNING to_jsonb(c) AS row",
    )
    .bind(id)
    .bind(notation_id);
    Ok(authority::maybe(&pg, query).await?)
}

/// Record the Firm and Project an Owner assigned this thread to.
///
/// # Errors
///
/// Propagates any database error.
pub async fn set_scope(
    _db: &ApplicationStore,
    id: Uuid,
    firm_id: Option<Uuid>,
    project_id: Option<Uuid>,
) -> Result<Option<EmailConversation>, EmailConversationError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "UPDATE email_conversation c SET firm_id = $2, project_id = $3, updated_at = now() \
         WHERE c.id = $1 RETURNING to_jsonb(c) AS row",
    )
    .bind(id)
    .bind(firm_id)
    .bind(project_id);
    Ok(authority::maybe(&pg, query).await?)
}

#[cfg(test)]
mod tests {}
