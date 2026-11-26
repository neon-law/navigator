//! Helpers for the `notation_clauses` table — per-notation custom prose a
//! lawyer adds to a single matter's assembled document.
//!
//! Commands use PostgreSQL; see
//! [storage models](../../docs/storage-models.md#matter-records).
//!
//! Kept beside the other orchestration helpers so `web` reaches them
//! without re-importing the entity. The render-time splice lives in `web`
//! (it assembles the template body); this module owns the rows.

use crate::postgres::matter;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::surreal::{record_id, record_uuid, SurrealDb};

/// The table these rows live in.
pub(crate) const TABLE: &str = "notation_clause";

/// The marker in a template body where a notation's custom clauses are
/// spliced, in `position` order. A body without the marker renders
/// unchanged — clauses simply don't appear.
pub const CUSTOM_CLAUSES_MARKER: &str = "{{custom_clauses}}";

/// One custom paragraph spliced into a single notation's assembled
/// document at [`CUSTOM_CLAUSES_MARKER`], in `position` order.
///
/// The application-facing shape: plain Rust types, no engine handles.
/// [`NotationClauseRow`] is the seam that turns it into (and back out of)
/// what the SDK reads and writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotationClause {
    pub id: Uuid,
    pub notation_id: Uuid,
    /// Render order within the notation, ascending.
    pub position: i32,
    /// The clause prose (markdown), as the attorney reviews it.
    pub body_markdown: String,
    /// The lawyer who added the clause.
    pub authored_by_person_id: Option<Uuid>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The row as the engine reads and writes it.
#[derive(SurrealValue)]
struct NotationClauseRow {
    id: surrealdb::types::RecordId,
    notation_id: surrealdb::types::RecordId,
    position: i32,
    body_markdown: String,
    authored_by_person_id: Option<surrealdb::types::RecordId>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl NotationClauseRow {
    /// `None` when a record id is not a native UUID key — a row written
    /// by something that bypassed [`crate::surreal::record_id`].
    fn into_clause(self) -> Option<NotationClause> {
        Some(NotationClause {
            id: record_uuid(&self.id)?,
            notation_id: record_uuid(&self.notation_id)?,
            position: self.position,
            body_markdown: self.body_markdown,
            authored_by_person_id: self.authored_by_person_id.as_ref().and_then(record_uuid),
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

/// The projection every read shares, so one field list describes the row
/// and a new column cannot reach [`NotationClauseRow`] from only one
/// query.
const SELECT: &str = "id, notation_id, position, body_markdown, authored_by_person_id, \
     inserted_at, updated_at";

/// Splice a notation's `clauses` into a template `body` at
/// [`CUSTOM_CLAUSES_MARKER`], joined as separate markdown paragraphs in
/// order. A body without the marker is returned unchanged (clauses simply
/// don't appear), so adding the feature never disturbs a template that
/// doesn't opt in.
#[must_use]
pub fn splice(body: &str, clauses: &[NotationClause]) -> String {
    if !body.contains(CUSTOM_CLAUSES_MARKER) {
        return body.to_string();
    }
    let rendered = clauses
        .iter()
        .map(|c| c.body_markdown.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    body.replace(CUSTOM_CLAUSES_MARKER, &rendered)
}

fn one(
    mut response: surrealdb::IndexedResults,
) -> Result<Option<NotationClause>, surrealdb::Error> {
    let row: Option<NotationClauseRow> = response.take(0)?;
    Ok(row.and_then(NotationClauseRow::into_clause))
}

fn many(mut response: surrealdb::IndexedResults) -> Result<Vec<NotationClause>, surrealdb::Error> {
    let rows: Vec<NotationClauseRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(NotationClauseRow::into_clause)
        .collect())
}

/// One clause by id.
///
/// # Errors
///
/// Propagates any database error.
pub async fn find_by_id(
    db: &SurrealDb,
    id: Uuid,
) -> Result<Option<NotationClause>, surrealdb::Error> {
    if let Some(pg) = matter::authority_for(TABLE)
        .await
        .map_err(crate::surreal::postgres_error)?
    {
        return matter::find(&pg, TABLE, json!({"id": id}))
            .await
            .map_err(crate::surreal::postgres_error);
    }

    let response = db
        .query(format!("SELECT {SELECT} FROM ONLY $id LIMIT 1"))
        .bind(("id", record_id(TABLE, id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    one(response)
}

/// All clauses on a notation, in render (`position`, then `id`) order.
///
/// # Errors
///
/// Propagates any database error.
pub async fn for_notation(
    db: &SurrealDb,
    notation_id: Uuid,
) -> Result<Vec<NotationClause>, surrealdb::Error> {
    if let Some(pg) = matter::authority_for(TABLE)
        .await
        .map_err(crate::surreal::postgres_error)?
    {
        return matter::select(
            &pg,
            TABLE,
            json!({"notation_id": notation_id}),
            &["position", "id"],
        )
        .await
        .map_err(crate::surreal::postgres_error);
    }

    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE notation_id = $notation \
             ORDER BY position ASC, id ASC"
        ))
        .bind(("notation", record_id(crate::notations::TABLE, notation_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}

/// Append one clause to a notation at the next position, returning its id.
/// The position is `max(position) + 1` so a fresh clause always renders
/// last.
///
/// A transaction serializes position allocation for each notation.
///
/// # Errors
///
/// Propagates any database error.
pub async fn append(
    _db: &SurrealDb,
    notation_id: Uuid,
    body_markdown: &str,
    authored_by: Option<Uuid>,
) -> Result<Uuid, surrealdb::Error> {
    let pg = matter::authority_for(TABLE)
        .await
        .map_err(crate::surreal::postgres_error)?
        .ok_or_else(|| crate::surreal::postgres_error(matter::MatterAuthorityError::Unavailable))?;
    crate::postgres::notations::append_clause(&pg, notation_id, body_markdown, authored_by)
        .await
        .map_err(crate::surreal::postgres_error)
}

/// Replace one clause's body. Returns the updated row, or `Ok(None)` if no
/// row matched.
///
/// # Errors
///
/// Propagates any database error.
pub async fn update_body(
    _db: &SurrealDb,
    id: Uuid,
    body_markdown: &str,
) -> Result<Option<NotationClause>, surrealdb::Error> {
    let pg = matter::authority_for(TABLE)
        .await
        .map_err(crate::surreal::postgres_error)?
        .ok_or_else(|| crate::surreal::postgres_error(matter::MatterAuthorityError::Unavailable))?;
    Ok(matter::update(
        &pg,
        TABLE,
        json!({"id": id}),
        json!({"body_markdown": body_markdown, "updated_at": Utc::now()}),
    )
    .await
    .map_err(crate::surreal::postgres_error)?
    .into_iter()
    .next())
}

/// Delete one clause. Returns `true` if a row was removed.
///
/// # Errors
///
/// Propagates any database error.
pub async fn delete(_db: &SurrealDb, id: Uuid) -> Result<bool, surrealdb::Error> {
    let pg = matter::authority_for(TABLE)
        .await
        .map_err(crate::surreal::postgres_error)?
        .ok_or_else(|| crate::surreal::postgres_error(matter::MatterAuthorityError::Unavailable))?;
    Ok(matter::delete(&pg, TABLE, json!({"id": id}))
        .await
        .map_err(crate::surreal::postgres_error)?
        != 0)
}

/// Move one clause one step earlier (`up`) or later (`down`) in render
/// order by swapping its `position` with its neighbour's. A no-op at the
/// ends. Returns `Ok(false)` when the clause doesn't exist or can't move.
///
/// PostgreSQL swaps both positions in one transaction.
///
/// # Errors
///
/// Propagates any database error.
pub async fn move_clause(_db: &SurrealDb, id: Uuid, up: bool) -> Result<bool, surrealdb::Error> {
    let pg = matter::authority_for(TABLE)
        .await
        .map_err(crate::surreal::postgres_error)?
        .ok_or_else(|| crate::surreal::postgres_error(matter::MatterAuthorityError::Unavailable))?;
    crate::postgres::notations::move_clause(&pg, id, up)
        .await
        .map_err(crate::surreal::postgres_error)
}

/// Whether a notation carries any custom clause — half of the review gate
/// (the other half is any client-sourced answer).
///
/// # Errors
///
/// Propagates any database error.
pub async fn exists_for(db: &SurrealDb, notation_id: Uuid) -> Result<bool, surrealdb::Error> {
    if let Some(pg) = matter::authority_for(TABLE)
        .await
        .map_err(crate::surreal::postgres_error)?
    {
        return Ok(
            matter::find::<NotationClause>(&pg, TABLE, json!({"notation_id": notation_id}))
                .await
                .map_err(crate::surreal::postgres_error)?
                .is_some(),
        );
    }

    let mut response = db
        .query(format!(
            "SELECT VALUE id FROM {TABLE} WHERE notation_id = $notation LIMIT 1"
        ))
        .bind(("notation", record_id(crate::notations::TABLE, notation_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let ids: Vec<surrealdb::types::RecordId> = response.take(0)?;
    Ok(!ids.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{append, delete, exists_for, for_notation, move_clause, splice, update_body};
    use crate::test_support;
    use crate::test_support::seed_notation;

    #[tokio::test]
    async fn splice_renders_clauses_at_the_marker_in_order() {
        test_support::with_required_postgres(async {
            let surreal = crate::surreal::SurrealDb::uninitialized();
            let notation_id = seed_notation(&surreal).await;
            append(&surreal, notation_id, "Governing law is Nevada.", None)
                .await
                .unwrap();
            append(&surreal, notation_id, "Fees are due net 30.", None)
                .await
                .unwrap();
            let clauses = for_notation(&surreal, notation_id).await.unwrap();

            let body = "Engagement terms.\n\n{{custom_clauses}}\n\nSignatures.";
            let rendered = splice(body, &clauses);
            assert!(rendered.contains("Governing law is Nevada."));
            assert!(rendered.contains("Fees are due net 30."));
            assert!(!rendered.contains("{{custom_clauses}}"));
            // Order preserved, governing-law clause before the fees clause.
            let law = rendered.find("Governing law").unwrap();
            let fees = rendered.find("Fees are due").unwrap();
            assert!(law < fees);
        })
        .await;
    }

    #[test]
    fn splice_leaves_a_body_without_the_marker_unchanged() {
        let body = "No marker here.";
        assert_eq!(splice(body, &[]), body);
    }

    #[tokio::test]
    async fn append_orders_clauses_and_exists_for_reports_presence() {
        test_support::with_required_postgres(async {
            let surreal = crate::surreal::SurrealDb::uninitialized();
            let notation_id = seed_notation(&surreal).await;
            assert!(!exists_for(&surreal, notation_id).await.unwrap());

            append(&surreal, notation_id, "First clause.", None)
                .await
                .unwrap();
            append(&surreal, notation_id, "Second clause.", None)
                .await
                .unwrap();

            let clauses = for_notation(&surreal, notation_id).await.unwrap();
            assert_eq!(clauses.len(), 2);
            assert_eq!(clauses[0].body_markdown, "First clause.");
            assert_eq!(clauses[1].body_markdown, "Second clause.");
            assert!(clauses[0].position < clauses[1].position);
            assert!(exists_for(&surreal, notation_id).await.unwrap());
        })
        .await;
    }

    #[tokio::test]
    async fn move_clause_swaps_render_order() {
        test_support::with_required_postgres(async {
            let surreal = crate::surreal::SurrealDb::uninitialized();
            let notation_id = seed_notation(&surreal).await;
            let first = append(&surreal, notation_id, "First.", None).await.unwrap();
            append(&surreal, notation_id, "Second.", None)
                .await
                .unwrap();

            // Move the first clause down; "Second." now renders first.
            assert!(move_clause(&surreal, first, false).await.unwrap());
            let clauses = for_notation(&surreal, notation_id).await.unwrap();
            assert_eq!(clauses[0].body_markdown, "Second.");
            assert_eq!(clauses[1].body_markdown, "First.");

            // Moving the now-last clause down again is a no-op.
            assert!(!move_clause(&surreal, first, false).await.unwrap());
        })
        .await;
    }

    #[tokio::test]
    async fn update_and_delete_round_trip() {
        test_support::with_required_postgres(async {
            let surreal = crate::surreal::SurrealDb::uninitialized();
            let notation_id = seed_notation(&surreal).await;
            let id = append(&surreal, notation_id, "Draft.", None).await.unwrap();

            let updated = update_body(&surreal, id, "Revised.")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(updated.body_markdown, "Revised.");

            assert!(delete(&surreal, id).await.unwrap());
            assert!(for_notation(&surreal, notation_id)
                .await
                .unwrap()
                .is_empty());
        })
        .await;
    }
}
