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

use crate::surreal::{record_id, retry, SurrealDb};
use std::str::FromStr;
use strum::EnumString;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

/// The SurrealDB table holding Assistant Turns.
pub(crate) const TABLE: &str = "assistant_turn";
const SELECT: &str = "id, person_id, kind, content, tool_name, input_tokens, output_tokens, \
                      inserted_at, updated_at";

const PERSON_TABLE: &str = "person";

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
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    /// A row came back that this module cannot faithfully represent —
    /// an unknown `kind`, or a record id that is not a native UUID.
    /// Possible only through a writer that bypassed this module.
    #[error("assistant turn row is not representable")]
    UnrepresentableRow,
    /// A write claimed success but returned no usable row.
    #[error("writing the assistant turn returned no usable row")]
    WriteReturnedNothing,
}

/// The row as the engine reads and writes it.
#[derive(SurrealValue)]
struct AssistantTurnRow {
    id: surrealdb::types::RecordId,
    person_id: surrealdb::types::RecordId,
    kind: String,
    content: String,
    tool_name: Option<String>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl AssistantTurnRow {
    /// `None` when the id or owner is not a native UUID key, or the kind
    /// is not one the glossary defines. Navigator only writes native UUID
    /// ids through [`crate::surreal::record_id`], so reporting another
    /// shape would invent an id this module cannot faithfully represent.
    fn into_turn(self) -> Option<AssistantTurn> {
        Some(AssistantTurn {
            id: crate::surreal::record_uuid(&self.id)?,
            person_id: crate::surreal::record_uuid(&self.person_id)?,
            kind: TurnKind::from_str(&self.kind).ok()?,
            content: self.content,
            tool_name: self.tool_name,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

fn one(
    mut response: surrealdb::IndexedResults,
) -> Result<Option<AssistantTurn>, AssistantTurnError> {
    let row: Option<AssistantTurnRow> = response.take(0)?;
    Ok(row.and_then(AssistantTurnRow::into_turn))
}

fn many(mut response: surrealdb::IndexedResults) -> Result<Vec<AssistantTurn>, AssistantTurnError> {
    let rows: Vec<AssistantTurnRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(AssistantTurnRow::into_turn)
        .collect())
}

/// Run a write under the shared retry policy — the same one policy the
/// rest of the crate writes under ([`crate::surreal::retry`]). Only the
/// mapping lives here; how long a lost race is re-run is the crate's
/// one policy.
async fn writing<F, Q>(attempt: F) -> Result<surrealdb::IndexedResults, AssistantTurnError>
where
    F: FnMut() -> Q,
    Q: std::future::IntoFuture<Output = Result<surrealdb::IndexedResults, surrealdb::Error>>,
{
    retry::writing(attempt)
        .await
        .map_err(AssistantTurnError::Db)
}

/// Append one turn to a subscriber's chat and return the stored row.
///
/// # Errors
///
/// [`AssistantTurnError::Db`] when SurrealDB refuses the write, or
/// [`AssistantTurnError::WriteReturnedNothing`] when a write that claimed
/// success returns no usable row.
pub async fn append(db: &SurrealDb, turn: NewTurn) -> Result<AssistantTurn, AssistantTurnError> {
    let id = Uuid::now_v7();
    let response = writing(|| {
        db.query(format!(
            "CREATE $id SET person_id = $person_id, kind = $kind, \
             content = $content, tool_name = $tool_name, \
             input_tokens = $input_tokens, output_tokens = $output_tokens \
             RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind(("person_id", record_id(PERSON_TABLE, turn.person_id)))
        .bind(("kind", turn.kind.to_string()))
        .bind(("content", turn.content.clone()))
        .bind(("tool_name", turn.tool_name.clone()))
        .bind(("input_tokens", turn.input_tokens))
        .bind(("output_tokens", turn.output_tokens))
    })
    .await?;
    one(response)?.ok_or(AssistantTurnError::WriteReturnedNothing)
}

/// A subscriber's chat in chronological order.
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
    db: &SurrealDb,
    person_id: Uuid,
) -> Result<Vec<AssistantTurn>, AssistantTurnError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} \
             WHERE person_id = $person_id ORDER BY id ASC LIMIT $limit"
        ))
        .bind(("person_id", record_id(PERSON_TABLE, person_id)))
        .bind(("limit", HISTORY_LIMIT))
        .await?;
    many(response)
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
pub async fn purge(db: &SurrealDb, person_id: Uuid) -> Result<u64, AssistantTurnError> {
    let mut response = db
        .query(format!(
            "DELETE {TABLE} WHERE person_id = $person_id RETURN BEFORE"
        ))
        .bind(("person_id", record_id(PERSON_TABLE, person_id)))
        .await?;
    let rows: Vec<AssistantTurnRow> = response.take(0)?;
    Ok(rows.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::mem_surreal;

    fn prompt(person_id: Uuid, content: &str) -> NewTurn {
        NewTurn {
            person_id,
            kind: TurnKind::Prompt,
            content: content.to_string(),
            tool_name: None,
            input_tokens: None,
            output_tokens: None,
        }
    }

    /// The row that comes back from `append` is the row `history` reads:
    /// same id, same owner, same content, same timestamps.
    #[tokio::test]
    async fn append_returns_the_stored_row_and_history_reads_it_back() {
        let db = mem_surreal().await;
        let person = Uuid::now_v7();
        let stored = append(&db, prompt(person, "Is my lease assignable?"))
            .await
            .unwrap();
        assert_eq!(stored.person_id, person);
        assert_eq!(stored.kind, TurnKind::Prompt);
        assert_eq!(stored.content, "Is my lease assignable?");
        assert_eq!(stored.tool_name, None);

        let history = history(&db, person).await.unwrap();
        assert_eq!(history, vec![stored]);
    }

    /// UUIDv7 ids are time-ordered, so history returns the transcript in
    /// the order turns were appended, and tool rounds keep their order.
    #[tokio::test]
    async fn history_orders_by_the_minted_ids() {
        let db = mem_surreal().await;
        let person = Uuid::now_v7();
        for (kind, content, tool) in [
            (
                TurnKind::Prompt,
                "Is my lease assignable?".to_string(),
                None,
            ),
            (
                TurnKind::ToolCall,
                "{\"query\":\"lease assignment\"}".to_string(),
                Some("search_case_law".to_string()),
            ),
            (
                TurnKind::ToolResult,
                "{\"cases\":[]}".to_string(),
                Some("search_case_law".to_string()),
            ),
            (
                TurnKind::Reply,
                "Generally, yes — with notice.".to_string(),
                None,
            ),
        ] {
            append(
                &db,
                NewTurn {
                    person_id: person,
                    kind,
                    content,
                    tool_name: tool,
                    input_tokens: None,
                    output_tokens: None,
                },
            )
            .await
            .unwrap();
        }
        let history = history(&db, person).await.unwrap();
        let kinds: Vec<TurnKind> = history.iter().map(|turn| turn.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TurnKind::Prompt,
                TurnKind::ToolCall,
                TurnKind::ToolResult,
                TurnKind::Reply
            ]
        );
        assert_eq!(history[1].tool_name.as_deref(), Some("search_case_law"));
    }

    /// Ownership is the glossary's rule: another caller asking for the
    /// chat gets the same answer as for a chat that does not exist.
    #[tokio::test]
    async fn another_person_gets_an_empty_history() {
        let db = mem_surreal().await;
        let owner = Uuid::now_v7();
        let stranger = Uuid::now_v7();
        append(&db, prompt(owner, "private question"))
            .await
            .unwrap();
        assert!(history(&db, stranger).await.unwrap().is_empty());
    }

    /// Token counts are part of the stored row — the ledger's metering
    /// input, recorded where the completion happened.
    #[tokio::test]
    async fn token_counts_round_trip() {
        let db = mem_surreal().await;
        let person = Uuid::now_v7();
        let stored = append(
            &db,
            NewTurn {
                person_id: person,
                kind: TurnKind::Reply,
                content: "Generally, yes.".to_string(),
                tool_name: None,
                input_tokens: Some(12),
                output_tokens: Some(34),
            },
        )
        .await
        .unwrap();
        assert_eq!(stored.input_tokens, Some(12));
        assert_eq!(stored.output_tokens, Some(34));
        let history = history(&db, person).await.unwrap();
        assert_eq!(history[0].output_tokens, Some(34));
    }

    /// `purge` removes exactly the one subscriber's chat, reports the
    /// count, and is safe to run again.
    #[tokio::test]
    async fn purge_removes_one_subscribers_chat_and_repeats_cleanly() {
        let db = mem_surreal().await;
        let owner = Uuid::now_v7();
        let other = Uuid::now_v7();
        append(&db, prompt(owner, "one")).await.unwrap();
        append(&db, prompt(other, "two")).await.unwrap();

        assert_eq!(purge(&db, owner).await.unwrap(), 1);
        assert!(history(&db, owner).await.unwrap().is_empty());
        assert_eq!(history(&db, other).await.unwrap().len(), 1);
        assert_eq!(purge(&db, owner).await.unwrap(), 0);
    }

    /// A row written by a bypassing writer with an undefined kind is
    /// skipped by reads, not reported as something the glossary defines.
    #[tokio::test]
    async fn a_row_with_an_unknown_kind_is_skipped() {
        let db = mem_surreal().await;
        let person = Uuid::now_v7();
        append(&db, prompt(person, "real")).await.unwrap();
        db.query(
            "CREATE assistant_turn SET person_id = $person_id, \
             kind = 'vision', content = 'not one of the four'"
                .to_string(),
        )
        .bind(("person_id", record_id(PERSON_TABLE, person)))
        .await
        .unwrap();

        let history = history(&db, person).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content, "real");
    }
}
