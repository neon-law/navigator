//! The PostgreSQL `assistant_turn` table, the transcript's system of record
//! whenever PostgreSQL is configured. Ids are minted UUIDv7s, so ordering by
//! id is the chat's append order.

use std::str::FromStr;

use sqlx::{postgres::PgRow, Row};
use uuid::Uuid;

use super::{
    AssistantTurn, AssistantTurnError, AssistantTurnRow, NewTurn, TurnKind, HISTORY_LIMIT, SELECT,
    TABLE,
};
use crate::postgres::PostgresDb;
use crate::postgres_backfill::Copied;
use crate::surreal::SurrealDb;

/// `None` for a kind outside the glossary's four; the table's check refuses
/// one, so only a hand-edited row reaches here.
fn turn(row: &PgRow) -> Result<Option<AssistantTurn>, sqlx::Error> {
    let kind: String = row.try_get("kind")?;
    let Ok(kind) = TurnKind::from_str(&kind) else {
        return Ok(None);
    };
    Ok(Some(AssistantTurn {
        id: row.try_get("id")?,
        person_id: row.try_get("person_id")?,
        kind,
        content: row.try_get("content")?,
        tool_name: row.try_get("tool_name")?,
        input_tokens: row.try_get("input_tokens")?,
        output_tokens: row.try_get("output_tokens")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    }))
}

/// See [`super::append`].
pub(super) async fn append(
    pg: &PostgresDb,
    turn_row: &NewTurn,
) -> Result<AssistantTurn, AssistantTurnError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO {TABLE} (id, person_id, kind, content, tool_name, input_tokens, \
         output_tokens) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {SELECT}"
    )))
    .bind(Uuid::now_v7())
    .bind(turn_row.person_id)
    .bind(turn_row.kind.to_string())
    .bind(&turn_row.content)
    .bind(&turn_row.tool_name)
    .bind(turn_row.input_tokens)
    .bind(turn_row.output_tokens)
    .fetch_one(&pg.0)
    .await?;
    turn(&row)?.ok_or(AssistantTurnError::WriteReturnedNothing)
}

/// See [`super::history`].
pub(super) async fn history(
    pg: &PostgresDb,
    person_id: Uuid,
) -> Result<Vec<AssistantTurn>, AssistantTurnError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {SELECT} FROM {TABLE} WHERE person_id = $1 ORDER BY id DESC LIMIT $2"
    )))
    .bind(person_id)
    .bind(HISTORY_LIMIT)
    .fetch_all(&pg.0)
    .await?;
    let mut turns = Vec::with_capacity(rows.len());
    for row in rows.iter().rev() {
        turns.extend(turn(row)?);
    }
    Ok(turns)
}

/// See [`super::purge`].
pub(super) async fn purge(pg: &PostgresDb, person_id: Uuid) -> Result<u64, AssistantTurnError> {
    Ok(sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE FROM {TABLE} WHERE person_id = $1"
    )))
    .bind(person_id)
    .execute(&pg.0)
    .await?
    .rows_affected())
}

/// Copy every SurrealDB turn PostgreSQL does not hold, keeping its id and
/// timestamps.
pub(crate) async fn copy_from_surreal(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<Copied, surrealdb::Error> {
    let rows: Vec<AssistantTurnRow> = surreal
        .query(format!("SELECT {SELECT} FROM {TABLE}"))
        .await
        .and_then(surrealdb::IndexedResults::check)?
        .take(0)?;
    let mut copied = Copied::default();
    for row in rows {
        let Some(turn) = row.into_turn() else {
            copied.unrepresentable(TABLE);
            continue;
        };
        let result = sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {TABLE} (id, person_id, kind, content, tool_name, input_tokens, \
             output_tokens, inserted_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT (id) DO NOTHING"
        )))
        .bind(turn.id)
        .bind(turn.person_id)
        .bind(turn.kind.to_string())
        .bind(&turn.content)
        .bind(&turn.tool_name)
        .bind(turn.input_tokens)
        .bind(turn.output_tokens)
        .bind(turn.inserted_at)
        .bind(turn.updated_at)
        .execute(&pg.0)
        .await
        .map(|done| done.rows_affected());
        copied.record(TABLE, Some(turn.id), result);
    }
    Ok(copied)
}
