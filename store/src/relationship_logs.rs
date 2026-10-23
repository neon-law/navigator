//! The `relationship_log` table — the append-only audit trail of
//! relationship changes (`person joined entity`, `project closed`).
//!
//! # Why it moved with the graph, though it is not part of it
//!
//! A Relationship Log entry is **one-sided**: an actor took an action
//! against a subject. That is a different question from the two-sided
//! edges `store::relationships` holds, and the two are deliberately not
//! merged — the log *feeds* the graph (an LLM may later parse an edge
//! out of `detail`), but the conflict traversal never reads it.
//!
//! Its writers — `store::projects` and `store::project_modules` — are
//! Surreal-resident, so this insert lands on the same engine and inside
//! the same transaction as the matter it audits. Neither the matter nor
//! its audit entry can land without the other.
//!
//! # Append-only
//!
//! Never updated in place, never deleted. So there is no unique index —
//! two identical entries a second apart are two real events — and no
//! ASSERT on `action`: the trail records what happened, including
//! actions this code has no closed set for.

use chrono::{DateTime, Utc};
use serde::Serialize;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::surreal::{record_id, record_uuid, retry, SurrealDb};

const TABLE: &str = "relationship_log";
const PERSON_TABLE: &str = "person";
const PROJECT_TABLE: &str = "project";

/// The `subject_type` of an entry about a Person.
pub const PERSON_SUBJECT: &str = "person";

/// One audit entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelationshipLog {
    pub id: Uuid,
    /// Who acted. `None` for a system action — a scheduled workflow
    /// closing a matter has no person behind it.
    pub actor_person_id: Option<Uuid>,
    /// The table the subject lives in, and its key. Polymorphic across
    /// tables on both sides of the port, so the pair stays an
    /// unenforced id rather than becoming a link.
    pub subject_type: String,
    pub subject_id: Uuid,
    pub action: String,
    pub detail: String,
    /// The Project whose work produced the entry — the filing a service
    /// list came from, the matter a module was enabled on. `None` for an
    /// entry no one Project owns. It scopes the entry; it grants nothing.
    pub project_id: Option<Uuid>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(SurrealValue)]
struct RelationshipLogRow {
    id: surrealdb::types::RecordId,
    actor_person_id: Option<surrealdb::types::RecordId>,
    subject_type: String,
    subject_id: Uuid,
    action: String,
    detail: String,
    project_id: Option<surrealdb::types::RecordId>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl RelationshipLogRow {
    fn into_log(self) -> Option<RelationshipLog> {
        Some(RelationshipLog {
            id: record_uuid(&self.id)?,
            // An actor link this module could not read back reads as
            // absent rather than dropping the whole entry: the trail is
            // the point, and an unattributable entry still records that
            // something happened.
            actor_person_id: self.actor_person_id.as_ref().and_then(record_uuid),
            subject_type: self.subject_type,
            subject_id: self.subject_id,
            action: self.action,
            detail: self.detail,
            project_id: self.project_id.as_ref().and_then(record_uuid),
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

const SELECT: &str = "id, actor_person_id, subject_type, subject_id, action, detail, \
                      project_id, inserted_at, updated_at";

/// What a write stores.
#[derive(Debug, Clone)]
pub struct NewRelationshipLog {
    pub actor_person_id: Option<Uuid>,
    pub subject_type: String,
    pub subject_id: Uuid,
    pub action: String,
    pub detail: String,
    /// The Project the entry belongs to, when one does.
    pub project_id: Option<Uuid>,
}

#[derive(Debug, thiserror::Error)]
pub enum RelationshipLogError {
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    #[error("writing a relationship log entry returned no usable row")]
    WriteReturnedNothing,
}

/// Append one entry to the trail.
///
/// # Errors
///
/// [`RelationshipLogError::Db`] if the write fails.
pub async fn record(
    db: &SurrealDb,
    input: &NewRelationshipLog,
) -> Result<RelationshipLog, RelationshipLogError> {
    let id = Uuid::now_v7();
    let mut response = retry::writing(|| {
        db.query(format!(
            "CREATE $id SET actor_person_id = $actor, subject_type = $subject_type, \
             subject_id = $subject_id, action = $action, detail = $detail, \
             project_id = $project, inserted_at = time::now(), updated_at = time::now() \
             RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind((
            "actor",
            input
                .actor_person_id
                .map(|person| record_id(PERSON_TABLE, person)),
        ))
        .bind(("subject_type", input.subject_type.clone()))
        .bind(("subject_id", input.subject_id))
        .bind(("action", input.action.clone()))
        .bind(("detail", input.detail.clone()))
        .bind((
            "project",
            input
                .project_id
                .map(|project| record_id(PROJECT_TABLE, project)),
        ))
    })
    .await?;

    let row: Option<RelationshipLogRow> = response.take(0)?;
    row.and_then(RelationshipLogRow::into_log)
        .ok_or(RelationshipLogError::WriteReturnedNothing)
}

/// Whether an entry identical to `input` — same actor-independent subject,
/// action, and detail — is already on the trail. The seed import uses it to
/// keep a re-run from appending the same event twice.
///
/// # Errors
///
/// [`RelationshipLogError::Db`] if the lookup fails.
pub async fn exists(
    db: &SurrealDb,
    input: &NewRelationshipLog,
) -> Result<bool, RelationshipLogError> {
    let mut response = db
        .query(format!(
            "SELECT id FROM {TABLE} WHERE subject_type = $subject_type \
             AND subject_id = $subject_id AND action = $action AND detail = $detail LIMIT 1"
        ))
        .bind(("subject_type", input.subject_type.clone()))
        .bind(("subject_id", input.subject_id))
        .bind(("action", input.action.clone()))
        .bind(("detail", input.detail.clone()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<surrealdb::types::Value> = response.take(0)?;
    Ok(!rows.is_empty())
}

/// The whole trail, newest first — the order the lawyer listing reads,
/// which `relationship_log_inserted_at` indexes.
///
/// # Errors
///
/// [`RelationshipLogError::Db`] if the lookup fails.
pub async fn all(db: &SurrealDb) -> Result<Vec<RelationshipLog>, RelationshipLogError> {
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} ORDER BY inserted_at DESC"
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<RelationshipLogRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(RelationshipLogRow::into_log)
        .collect())
}

/// Every entry about one subject, newest first.
///
/// # Errors
///
/// [`RelationshipLogError::Db`] if the lookup fails.
pub async fn for_subject(
    db: &SurrealDb,
    subject_type: &str,
    subject_id: Uuid,
) -> Result<Vec<RelationshipLog>, RelationshipLogError> {
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} \
             WHERE subject_type = $subject_type AND subject_id = $subject_id \
             ORDER BY inserted_at DESC"
        ))
        .bind(("subject_type", subject_type.to_string()))
        .bind(("subject_id", subject_id))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<RelationshipLogRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(RelationshipLogRow::into_log)
        .collect())
}

/// Every entry recorded for one Project, newest first.
///
/// # Errors
///
/// [`RelationshipLogError::Db`] if the lookup fails.
pub async fn for_project(
    db: &SurrealDb,
    project_id: Uuid,
) -> Result<Vec<RelationshipLog>, RelationshipLogError> {
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE project_id = $project \
             ORDER BY inserted_at DESC"
        ))
        .bind(("project", record_id(PROJECT_TABLE, project_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<RelationshipLogRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(RelationshipLogRow::into_log)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{all, for_project, for_subject, record, NewRelationshipLog};
    use crate::surreal::test_support::mem;
    use uuid::Uuid;

    fn entry(action: &str, subject_id: Uuid) -> NewRelationshipLog {
        NewRelationshipLog {
            actor_person_id: None,
            subject_type: "projects".into(),
            subject_id,
            action: action.into(),
            detail: String::new(),
            project_id: None,
        }
    }

    #[tokio::test]
    async fn an_entry_reads_back_with_its_actor_resolved() {
        let db = mem().await;
        let actor = crate::persons::create(
            &db,
            &crate::persons::NewPerson::new(
                "Acting Lawyer",
                format!("{}@example.com", Uuid::now_v7()),
            ),
        )
        .await
        .unwrap();
        let subject = Uuid::now_v7();

        let written = record(
            &db,
            &NewRelationshipLog {
                actor_person_id: Some(actor.id),
                detail: "closed on the signed letter".into(),
                ..entry("project closed", subject)
            },
        )
        .await
        .unwrap();

        assert_eq!(written.actor_person_id, Some(actor.id));
        assert_eq!(written.subject_id, subject);
        assert_eq!(written.action, "project closed");
        assert_eq!(all(&db).await.unwrap(), vec![written]);
    }

    /// A system action has no person behind it, and that has to stay
    /// distinguishable from an unattributed one.
    #[tokio::test]
    async fn an_entry_with_no_actor_is_recorded_as_such() {
        let db = mem().await;
        let written = record(&db, &entry("project archived", Uuid::now_v7()))
            .await
            .unwrap();
        assert_eq!(written.actor_person_id, None);
    }

    /// Append-only: two identical actions a moment apart are two real
    /// events, so nothing dedupes them.
    #[tokio::test]
    async fn the_trail_appends_rather_than_upserting() {
        let db = mem().await;
        let subject = Uuid::now_v7();
        record(&db, &entry("module enabled", subject))
            .await
            .unwrap();
        record(&db, &entry("module enabled", subject))
            .await
            .unwrap();

        assert_eq!(all(&db).await.unwrap().len(), 2);
        assert_eq!(
            for_subject(&db, "projects", subject).await.unwrap().len(),
            2
        );
        assert!(for_subject(&db, "projects", Uuid::now_v7())
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn the_trail_reads_newest_first() {
        let db = mem().await;
        let subject = Uuid::now_v7();
        for action in ["opened", "priced", "closed"] {
            record(&db, &entry(action, subject)).await.unwrap();
        }
        let actions: Vec<String> = all(&db)
            .await
            .unwrap()
            .into_iter()
            .map(|log| log.action)
            .collect();
        assert_eq!(actions, ["closed", "priced", "opened"]);
    }

    /// A row recorded for a Project reads back through `for_project`; a row
    /// for another Project, and a row with no Project, do not.
    #[tokio::test]
    async fn a_row_recorded_for_a_project_reads_back_through_for_project_and_another_project_does_not(
    ) {
        let db = mem().await;
        let (ours, theirs) = (Uuid::now_v7(), Uuid::now_v7());
        let person = Uuid::now_v7();

        let written = record(
            &db,
            &NewRelationshipLog {
                subject_type: "person".into(),
                detail: "on the service list in 26-10042".into(),
                project_id: Some(ours),
                ..entry("on_service_list", person)
            },
        )
        .await
        .unwrap();
        record(
            &db,
            &NewRelationshipLog {
                project_id: Some(theirs),
                ..entry("on_service_list", person)
            },
        )
        .await
        .unwrap();
        record(&db, &entry("on_service_list", person))
            .await
            .unwrap();

        assert_eq!(written.project_id, Some(ours));
        assert_eq!(for_project(&db, ours).await.unwrap(), vec![written]);
        assert!(for_project(&db, Uuid::now_v7()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn for_project_reads_newest_first() {
        let db = mem().await;
        let project = Uuid::now_v7();
        for action in ["opened", "priced", "closed"] {
            record(
                &db,
                &NewRelationshipLog {
                    project_id: Some(project),
                    ..entry(action, project)
                },
            )
            .await
            .unwrap();
        }
        let actions: Vec<String> = for_project(&db, project)
            .await
            .unwrap()
            .into_iter()
            .map(|log| log.action)
            .collect();
        assert_eq!(actions, ["closed", "priced", "opened"]);
    }
}
