//! The Legal Sandbox transcript: one subscriber's Assistant Chat as
//! append-only rows the subscriber owns.
//!
//! The [glossary's Assistant
//! Turn](../../docs/glossary/assistant-turn.md) is one ordered entry in
//! an [Assistant Chat](../../docs/glossary/assistant-chat.md): a prompt
//! the subscriber typed, a reply the assistant gave, or the tool call /
//! tool result pair from the session loop's read-only search detours.
//! This module is the only writer and the only reader — the table has
//! `PERMISSIONS NONE`, so the SurrealDB layer never serves a row to a
//! scope that did not come through here — and every read is scoped to
//! the owning Person. Another caller asking for a chat gets the same
//! answer as for a chat that does not exist, which is the glossary's
//! ownership rule.
//!
//! Order is the record id: ids are minted UUIDv7s, time-ordered, so
//! `history` orders by id and the parallel tool turns of one exchange
//! keep their append order without a `max(seq) + 1` race.
//!
//! Retention is a policy question the spec leaves open. The purge seam
//! ([`purge`]) removes one subscriber's whole chat in one statement, so
//! whatever retention rule the product settles on is a caller of that
//! seam — not a schema migration.
//!
//! PostgreSQL holds the transcript whenever `NAVIGATOR_POSTGRES_HOST` is set
//! ([`PostgresDb::configured`]); without a PostgreSQL host the same calls use
//! the SurrealDB table. Turns SurrealDB still holds reach PostgreSQL through
//! the Mirror Backfill's born-in-PostgreSQL arm.

use crate::application_store::ApplicationStore;
use crate::postgres::PostgresDb;
use strum::EnumString;
use uuid::Uuid;

/// The SurrealDB table holding Assistant Turns.
pub(crate) const TABLE: &str = "assistant_turn";
const SELECT: &str = "id, person_id, kind, content, tool_name, input_tokens, output_tokens, \
                      inserted_at, updated_at";

pub(crate) mod postgres;
#[cfg(test)]
pub(crate) mod postgres_tests;

/// The kind of one [Assistant Turn](../../docs/glossary/assistant-turn.md),
/// exactly the glossary's four.
#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::Display, EnumString)]
#[strum(serialize_all = "snake_case")]
pub enum TurnKind {
    /// What the Person typed.
    Prompt,
    /// The assistant's answer.
    Reply,
    /// A read-only Navigator MCP tool the assistant chose, with its arguments.
    ToolCall,
    /// What that tool returned, paired to its call.
    ToolResult,
}

/// One entry in the transcript, as the public surface reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssistantTurn {
    /// The row's minted UUIDv7 id; ascending ids are the chat's order.
    pub id: Uuid,
    /// The one Person who owns this chat.
    pub person_id: Uuid,
    /// Which of the glossary's four kinds the turn is.
    pub kind: TurnKind,
    /// The turn's text: the prompt typed, the reply given, or the
    /// tool call's arguments / result payload rendered as a string.
    pub content: String,
    /// For tool turns, the Navigator MCP tool name.
    pub tool_name: Option<String>,
    /// Tokens the completion consumed, reported by the provider —
    /// the usage ledger's metering input.
    pub input_tokens: Option<i64>,
    /// Tokens the completion produced, reported by the provider.
    pub output_tokens: Option<i64>,
    /// When the turn entered the transcript.
    pub inserted_at: chrono::DateTime<chrono::Utc>,
    /// When the row last changed. Append-only, so this is the
    /// insertion time in practice.
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// The fields of a turn to append. The id is minted by [`append`], so a
/// caller cannot collide with an existing row by carrying an id.
#[derive(Clone, Debug)]
pub struct NewTurn {
    /// The one Person who owns this chat.
    pub person_id: Uuid,
    /// Which of the glossary's four kinds the turn is.
    pub kind: TurnKind,
    /// The turn's text.
    pub content: String,
    /// For tool turns, the Navigator MCP tool name.
    pub tool_name: Option<String>,
    /// Tokens the completion consumed, reported by the provider.
    pub input_tokens: Option<i64>,
    /// Tokens the completion produced, reported by the provider.
    pub output_tokens: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum AssistantTurnError {
    /// A database operation failed.
    /// The configured PostgreSQL server could not be reached.
    #[error("PostgreSQL: {0}")]
    Postgres(#[from] crate::postgres::PostgresError),
    /// A PostgreSQL statement failed.
    #[error("PostgreSQL query: {0}")]
    Sql(#[from] sqlx::Error),
    /// A row came back that this module cannot faithfully represent —
    /// an unknown `kind`, or a record id that is not a native UUID.
    /// Possible only through a writer that bypassed this module.
    #[error("assistant turn row is not representable")]
    UnrepresentableRow,
    /// A write claimed success but returned no usable row.
    #[error("writing the assistant turn returned no usable row")]
    WriteReturnedNothing,
}

/// Append one turn to a subscriber's chat and return the stored row.
///
/// # Errors
///
/// [`AssistantTurnError::Db`] when SurrealDB refuses the write, or
/// [`AssistantTurnError::WriteReturnedNothing`] when a write that claimed
/// success returns no usable row.
pub async fn append(
    _db: &ApplicationStore,
    turn: NewTurn,
) -> Result<AssistantTurn, AssistantTurnError> {
    let pg = PostgresDb::application().await?;
    Box::pin(postgres::append(&pg, &turn)).await
}

/// The most recent turns in a subscriber's chat, in chronological order.
///
/// # Ownership
///
/// Scoped to the owning Person. Another caller asking for the chat gets
/// an empty vec — the same answer as for a chat that does not exist —
/// never an error that reveals someone else's rows.
///
/// # Errors
///
/// [`AssistantTurnError::Db`] when SurrealDB cannot read the transcript;
/// unrepresentable rows are skipped, mirroring the sibling ledger reads.
pub async fn history(
    _db: &ApplicationStore,
    person_id: Uuid,
) -> Result<Vec<AssistantTurn>, AssistantTurnError> {
    let pg = PostgresDb::application().await?;
    Box::pin(postgres::history(&pg, person_id)).await
}

/// The most turns one `history` read returns — the transcript a session
/// page renders, not a full export.
const HISTORY_LIMIT: i64 = 200;

/// Remove a subscriber's whole chat in one statement — the seam the
/// retention policy will call once the product settles it.
///
/// # Errors
///
/// [`AssistantTurnError::Db`] when SurrealDB cannot run the delete.
pub async fn purge(_db: &ApplicationStore, person_id: Uuid) -> Result<u64, AssistantTurnError> {
    let pg = PostgresDb::application().await?;
    Box::pin(postgres::purge(&pg, person_id)).await
}

#[cfg(test)]
mod tests {}
