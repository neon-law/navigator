//! PostgreSQL Notation creation and atomic draft reset.

use uuid::Uuid;

use crate::notation_reset::{is_draft, ResetOutcome};
use crate::notations::{NewNotation, Notation, NotationError};
use crate::postgres::matter::MatterAuthorityError;
use crate::postgres::PostgresDb;

pub(crate) const DRAFT_FACTS_SQL: &str = "SELECT
    (EXISTS(SELECT 1 FROM notation_document WHERE notation_id=$1)
     OR EXISTS(SELECT 1 FROM asset WHERE starts_with(storage_key,$2)
               OR starts_with(secondary_storage_key,$2))),
    EXISTS(SELECT 1 FROM signature WHERE notation_id=$1)";

/// Hold a live parent while committing canonical document or asset facts.
/// Importers keep their historical-orphan contract and do not use this writer seam.
pub(crate) async fn lock_parent(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
) -> Result<(), MatterAuthorityError> {
    sqlx::query("SELECT id FROM notation WHERE id=$1 FOR KEY SHARE")
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    Ok(())
}

/// Copy the Template pins in the insert that opens the Notation.
pub(crate) async fn create(pg: &PostgresDb, new: &NewNotation) -> Result<Notation, NotationError> {
    insert(pg.pool(), new).await
}

pub(crate) async fn insert<'a>(
    executor: impl sqlx::Executor<'a, Database = sqlx::Postgres>,
    new: &NewNotation,
) -> Result<Notation, NotationError> {
    let row = sqlx::query(
        "INSERT INTO notation AS n
         (id,template_id,package_id,person_id,entity_id,project_id,state,delivery,
          questionnaire_snapshot,template_source_commit_sha,template_body_sha256)
         SELECT $1,t.id,$3,$4,$5,$6,$7,$8,$9,t.source_commit_sha,t.body_sha256
         FROM template t WHERE t.id=$2 RETURNING to_jsonb(n) - CASE WHEN n.questionnaire_snapshot IS NULL THEN ARRAY['questionnaire_snapshot'] ELSE ARRAY[]::text[] END AS row",
    )
    .bind(Uuid::now_v7())
    .bind(new.template_id)
    .bind(new.package_id)
    .bind(new.person_id)
    .bind(new.entity_id)
    .bind(new.project_id)
    .bind(&new.state)
    .bind(&new.delivery)
    .bind(new.questionnaire_snapshot.as_ref().map(sqlx::types::Json))
    .fetch_optional(executor)
    .await?
    .ok_or(NotationError::WriteReturnedNothing)?;
    crate::notations::postgres_notation(&row)
}

/// Re-check the live predicate under the parent lock and delete the graph together.
pub(crate) async fn delete_draft(pg: &PostgresDb, id: Uuid) -> Result<ResetOutcome, sqlx::Error> {
    let mut tx = pg.pool().begin().await?;
    // A writer may hold the parent until its document/asset fact commits.
    // After waiting for that lock, the next statement must see its commit.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .execute(&mut *tx)
        .await?;
    let state: Option<String> =
        sqlx::query_scalar("SELECT state FROM notation WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(state) = state else {
        return Ok(ResetOutcome::AlreadyGone);
    };
    let prefix = format!("notations/{id}/");
    let (document, signature): (bool, bool) = sqlx::query_as(DRAFT_FACTS_SQL)
        .bind(id)
        .bind(prefix)
        .fetch_one(&mut *tx)
        .await?;
    if !is_draft(&state, document, signature) {
        return Ok(ResetOutcome::Kept);
    }
    sqlx::query("DELETE FROM answer WHERE notation_id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM notation_event WHERE notation_id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM notation_clause WHERE notation_id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM notation_source WHERE notation_id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM notation WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(ResetOutcome::Deleted)
}

/// The document and signature half of the draft predicate for discovery.
pub(crate) async fn draft_facts(pg: &PostgresDb, id: Uuid) -> Result<(bool, bool), sqlx::Error> {
    sqlx::query_as(DRAFT_FACTS_SQL)
        .bind(id)
        .bind(format!("notations/{id}/"))
        .fetch_one(pg.pool())
        .await
}

/// Restore one immutable archived row without replacing a concurrent writer.
pub(crate) async fn restore_archived(
    pg: &PostgresDb,
    archived: &Notation,
) -> Result<crate::assets::RowRestore, NotationError> {
    use chrono::SubsecRound;
    let mut expected = archived.clone();
    expected.inserted_at = expected.inserted_at.trunc_subsecs(6);
    expected.updated_at = expected.updated_at.trunc_subsecs(6);
    let encoded = serde_json::to_value(&expected).map_err(MatterAuthorityError::from)?;
    let inserted: Option<Notation> =
        crate::postgres::matter::insert_on(pg.pool(), "notation", encoded).await?;
    if inserted.is_some() {
        return Ok(crate::assets::RowRestore::Created);
    }
    let existing: Option<Notation> =
        crate::postgres::matter::find(pg, "notation", serde_json::json!({"id": archived.id}))
            .await?;
    Ok(if existing.as_ref() == Some(&expected) {
        crate::assets::RowRestore::Identical
    } else {
        crate::assets::RowRestore::Conflict
    })
}

/// The attribution context pinned to an existing Notation.
pub(crate) async fn notation_context(
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Option<(Uuid, Uuid)>, MatterAuthorityError> {
    Ok(
        sqlx::query_as("SELECT person_id, template_id FROM notation WHERE id = $1")
            .bind(id)
            .fetch_optional(pg.pool())
            .await?,
    )
}

/// Append a custom clause under the same notation lock used by reordering.
pub(crate) async fn append_clause(
    pg: &PostgresDb,
    notation: Uuid,
    body: &str,
    author: Option<Uuid>,
) -> Result<Uuid, MatterAuthorityError> {
    let mut tx = pg.pool().begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(1312902737, hashtext($1::text))")
        .bind(notation)
        .execute(&mut *tx)
        .await?;
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO notation_clause(id, notation_id, position, body_markdown, authored_by_person_id, inserted_at, updated_at) SELECT $1, $2, COALESCE(MAX(position) + 1, 0), $3, $4, now(), now() FROM notation_clause WHERE notation_id = $2")
        .bind(id).bind(notation).bind(body).bind(author).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(id)
}

/// Swap neighbours in one transaction; a reader never sees half the swap.
pub(crate) async fn move_clause(
    pg: &PostgresDb,
    id: Uuid,
    up: bool,
) -> Result<bool, MatterAuthorityError> {
    let mut tx = pg.pool().begin().await?;
    let notation: Option<Uuid> =
        sqlx::query_scalar("SELECT notation_id FROM notation_clause WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(notation) = notation else {
        return Ok(false);
    };
    sqlx::query("SELECT pg_advisory_xact_lock(1312902737, hashtext($1::text))")
        .bind(notation)
        .execute(&mut *tx)
        .await?;
    let rows: Vec<(Uuid, i32)> = sqlx::query_as("SELECT id, position FROM notation_clause WHERE notation_id = $1 ORDER BY position, id FOR UPDATE").bind(notation).fetch_all(&mut *tx).await?;
    let Some(index) = rows.iter().position(|row| row.0 == id) else {
        return Ok(false);
    };
    let neighbour = if up {
        index.checked_sub(1)
    } else {
        index.checked_add(1).filter(|index| *index < rows.len())
    };
    let Some(neighbour) = neighbour else {
        return Ok(false);
    };
    sqlx::query("UPDATE notation_clause SET position = CASE WHEN id = $1 THEN $4 ELSE $3 END, updated_at = now() WHERE id IN ($1, $2)")
        .bind(id).bind(rows[neighbour].0).bind(rows[index].1).bind(rows[neighbour].1).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(true)
}
