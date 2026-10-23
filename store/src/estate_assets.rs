//! Project-scoped [Estate Asset](../../docs/glossary/estate-asset.md) rows
//! (`estate_asset`): one line of an estate plan's asset schedule.
//!
//! A line names the property (`category`, `description`), how it passes at
//! death (`passes_by`), and whether it is funded into the plan
//! (`funding_state`). One row holds each (category, description) line on a
//! Project. The table is `estate_asset`, not `asset`: `asset` stores byte
//! pointers for documents and content, and an Estate Asset is property.

use serde::Serialize;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::matter_record_links::{one_of, optional_text, require_project, required_text};
use crate::surreal::{record_id, record_uuid, retry, SurrealDb};

const TABLE: &str = "estate_asset";
const LINE_INDEX: &str = "estate_asset_line";

/// How an asset passes at death. The `ASSERT` on `estate_asset.passes_by` is
/// the engine's copy of this list.
pub const PASSES_BY: [&str; 5] = ["trust", "beneficiary_designation", "will", "joint", "other"];

/// Whether an asset is funded into the plan. The `ASSERT` on
/// `estate_asset.funding_state` is the engine's copy of this list.
pub const FUNDING_STATES: [&str; 4] = ["funded", "in_progress", "not_funded", "consent_needed"];

/// One stored Estate Asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EstateAsset {
    pub id: Uuid,
    pub project_id: Uuid,
    pub category: String,
    pub description: String,
    pub passes_by: String,
    pub funding_state: String,
    pub note: Option<String>,
    pub inserted_at: String,
    pub updated_at: String,
}

/// The fields a caller writes. Insert and update both take the whole set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EstateAssetFields {
    pub category: String,
    pub description: String,
    pub passes_by: String,
    pub funding_state: String,
    pub note: Option<String>,
}

#[derive(SurrealValue)]
struct EstateAssetRow {
    id: surrealdb::types::RecordId,
    project_id: surrealdb::types::RecordId,
    category: String,
    description: String,
    passes_by: String,
    funding_state: String,
    note: Option<String>,
    inserted_at: String,
    updated_at: String,
}

impl EstateAssetRow {
    fn into_asset(self) -> Option<EstateAsset> {
        Some(EstateAsset {
            id: record_uuid(&self.id)?,
            project_id: record_uuid(&self.project_id)?,
            category: self.category,
            description: self.description,
            passes_by: self.passes_by,
            funding_state: self.funding_state,
            note: self.note,
            inserted_at: self.inserted_at,
            updated_at: self.updated_at,
        })
    }
}

const SELECT: &str = "id, project_id, category, description, passes_by, funding_state, note, \
     inserted_at, updated_at";

#[derive(Debug, thiserror::Error)]
pub enum EstateAssetError {
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("no such estate asset")]
    NotFound,
    #[error("writing an estate asset returned no usable row")]
    WriteReturnedNothing,
}

/// The fields as stored: trimmed, checked against the closed lists.
fn normalize(fields: &EstateAssetFields) -> Result<EstateAssetFields, EstateAssetError> {
    Ok(EstateAssetFields {
        category: required_text("category", &fields.category).map_err(EstateAssetError::Invalid)?,
        description: required_text("description", &fields.description)
            .map_err(EstateAssetError::Invalid)?,
        passes_by: one_of("passes_by", &fields.passes_by, &PASSES_BY)
            .map_err(EstateAssetError::Invalid)?,
        funding_state: one_of("funding_state", &fields.funding_state, &FUNDING_STATES)
            .map_err(EstateAssetError::Invalid)?,
        note: optional_text(fields.note.as_deref()),
    })
}

async fn persist(
    surreal: &SurrealDb,
    id: Uuid,
    project_id: Uuid,
    fields: EstateAssetFields,
    inserted_at: &str,
) -> Result<EstateAsset, EstateAssetError> {
    let now = chrono::Utc::now().to_rfc3339();
    let response = surreal
        .query(format!(
            "UPSERT $id SET project_id = $project_id, category = $category, \
             description = $description, passes_by = $passes_by, \
             funding_state = $funding_state, note = $note, inserted_at = $inserted_at, \
             updated_at = $now RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind(("project_id", record_id("project", project_id)))
        .bind(("category", fields.category))
        .bind(("description", fields.description))
        .bind(("passes_by", fields.passes_by))
        .bind(("funding_state", fields.funding_state))
        .bind(("note", fields.note))
        .bind(("inserted_at", inserted_at.to_string()))
        .bind(("now", now))
        .await
        .and_then(surrealdb::IndexedResults::check);
    let mut response = match response {
        Ok(response) => response,
        Err(error) if retry::unique_violation(&error) == Some(LINE_INDEX) => {
            return Err(EstateAssetError::Invalid(
                "this Project's schedule already has a line with that category and description"
                    .into(),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    let row: Option<EstateAssetRow> = response.take(0)?;
    row.and_then(EstateAssetRow::into_asset)
        .ok_or(EstateAssetError::WriteReturnedNothing)
}

/// A Project's asset schedule, by category then description.
///
/// # Errors
///
/// [`EstateAssetError::Db`] if the read fails.
pub async fn list(
    surreal: &SurrealDb,
    project_id: Uuid,
) -> Result<Vec<EstateAsset>, EstateAssetError> {
    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE project_id = $project_id \
             ORDER BY category, description"
        ))
        .bind(("project_id", record_id("project", project_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<EstateAssetRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(EstateAssetRow::into_asset)
        .collect())
}

/// One Estate Asset on a Project, or `None`.
///
/// # Errors
///
/// [`EstateAssetError::Db`] if the read fails.
pub async fn find(
    surreal: &SurrealDb,
    project_id: Uuid,
    id: Uuid,
) -> Result<Option<EstateAsset>, EstateAssetError> {
    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM ONLY {TABLE} \
             WHERE id = $id AND project_id = $project_id LIMIT 1"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind(("project_id", record_id("project", project_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<EstateAssetRow> = response.take(0)?;
    Ok(row.and_then(EstateAssetRow::into_asset))
}

/// The row holding one (category, description) line, or `None`. Both are
/// matched as stored, trimmed.
///
/// # Errors
///
/// [`EstateAssetError::Db`] if the read fails.
pub async fn find_by_line(
    surreal: &SurrealDb,
    project_id: Uuid,
    category: &str,
    description: &str,
) -> Result<Option<EstateAsset>, EstateAssetError> {
    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM ONLY {TABLE} WHERE project_id = $project_id \
             AND category = $category AND description = $description LIMIT 1"
        ))
        .bind(("project_id", record_id("project", project_id)))
        .bind(("category", category.trim().to_string()))
        .bind(("description", description.trim().to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<EstateAssetRow> = response.take(0)?;
    Ok(row.and_then(EstateAssetRow::into_asset))
}

/// Store a new Estate Asset and return it.
///
/// # Errors
///
/// [`EstateAssetError::Invalid`] for a blank category or description, a
/// `passes_by` or `funding_state` outside its list, a missing Project, or a
/// line already on the schedule; [`EstateAssetError::Db`] if the write fails.
pub async fn insert(
    surreal: &SurrealDb,
    project_id: Uuid,
    fields: &EstateAssetFields,
) -> Result<EstateAsset, EstateAssetError> {
    let fields = normalize(fields)?;
    require_project(surreal, project_id)
        .await
        .map_err(EstateAssetError::Invalid)?;
    let inserted_at = chrono::Utc::now().to_rfc3339();
    persist(surreal, Uuid::now_v7(), project_id, fields, &inserted_at).await
}

/// Replace an Estate Asset's fields and return the stored row.
///
/// # Errors
///
/// [`EstateAssetError::NotFound`] when the row is not on this Project;
/// otherwise as [`insert`].
pub async fn update(
    surreal: &SurrealDb,
    project_id: Uuid,
    id: Uuid,
    fields: &EstateAssetFields,
) -> Result<EstateAsset, EstateAssetError> {
    let existing = find(surreal, project_id, id)
        .await?
        .ok_or(EstateAssetError::NotFound)?;
    let fields = normalize(fields)?;
    persist(
        surreal,
        existing.id,
        project_id,
        fields,
        &existing.inserted_at,
    )
    .await
}

/// Remove one Estate Asset.
///
/// # Errors
///
/// [`EstateAssetError::NotFound`] when the row is not on this Project;
/// [`EstateAssetError::Db`] if the delete fails.
pub async fn delete(
    surreal: &SurrealDb,
    project_id: Uuid,
    id: Uuid,
) -> Result<(), EstateAssetError> {
    find(surreal, project_id, id)
        .await?
        .ok_or(EstateAssetError::NotFound)?;
    surreal
        .query("DELETE $id")
        .bind(("id", record_id(TABLE, id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    Ok(())
}

/// Remove every Estate Asset on a Project. The rows are imported from the
/// Project repository's seeds, so they go with the Project row.
///
/// # Errors
///
/// [`EstateAssetError::Db`] if the delete fails.
pub async fn delete_for_project(
    surreal: &SurrealDb,
    project_id: Uuid,
) -> Result<(), EstateAssetError> {
    surreal
        .query(format!("DELETE {TABLE} WHERE project_id = $project_id"))
        .bind(("project_id", record_id("project", project_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{mem_surreal, seed_entity};

    async fn project(surreal: &SurrealDb, code: &str) -> Uuid {
        crate::projects::create(
            surreal,
            &crate::projects::NewProject {
                code: code.into(),
                name: "Sample estate plan".into(),
                status: "open".into(),
                entity_id: seed_entity(surreal).await,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id
    }

    fn line(category: &str, description: &str, funding_state: &str) -> EstateAssetFields {
        EstateAssetFields {
            category: category.into(),
            description: description.into(),
            passes_by: "trust".into(),
            funding_state: funding_state.into(),
            note: None,
        }
    }

    #[tokio::test]
    async fn insert_list_update_and_delete_round_trip() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal, "sample-schedule").await;
        let home = insert(
            &surreal,
            project_id,
            &line("  real_property ", " Primary residence ", "in_progress"),
        )
        .await
        .unwrap();
        assert_eq!(home.category, "real_property");
        assert_eq!(home.description, "Primary residence");
        let account = insert(
            &surreal,
            project_id,
            &EstateAssetFields {
                passes_by: "beneficiary_designation".into(),
                note: Some("  ".into()),
                ..line("account", "Retirement account", "funded")
            },
        )
        .await
        .unwrap();
        assert_eq!(account.note, None);

        let schedule: Vec<Uuid> = list(&surreal, project_id)
            .await
            .unwrap()
            .into_iter()
            .map(|asset| asset.id)
            .collect();
        assert_eq!(schedule, vec![account.id, home.id]);

        let funded = update(
            &surreal,
            project_id,
            home.id,
            &EstateAssetFields {
                note: Some("Deed recorded".into()),
                ..line("real_property", "Primary residence", "funded")
            },
        )
        .await
        .unwrap();
        assert_eq!(funded.funding_state, "funded");
        assert_eq!(funded.note.as_deref(), Some("Deed recorded"));
        assert_eq!(funded.inserted_at, home.inserted_at);

        delete(&surreal, project_id, home.id).await.unwrap();
        assert_eq!(list(&surreal, project_id).await.unwrap().len(), 1);
        assert!(matches!(
            delete(&surreal, project_id, home.id).await,
            Err(EstateAssetError::NotFound)
        ));
    }

    #[tokio::test]
    async fn a_value_outside_a_closed_list_or_a_blank_is_refused() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal, "sample-schedule-refusals").await;
        for bad in [
            EstateAssetFields {
                passes_by: "intestacy".into(),
                ..line("account", "Checking", "funded")
            },
            line("account", "Checking", "half_funded"),
            line(" ", "Checking", "funded"),
            line("account", "", "funded"),
        ] {
            assert!(matches!(
                insert(&surreal, project_id, &bad).await.unwrap_err(),
                EstateAssetError::Invalid(_)
            ));
        }
        assert!(list(&surreal, project_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn one_row_holds_each_line() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal, "sample-schedule-line").await;
        insert(
            &surreal,
            project_id,
            &line("vehicle", "Sedan", "not_funded"),
        )
        .await
        .unwrap();
        let error = insert(&surreal, project_id, &line("vehicle", "Sedan", "funded"))
            .await
            .unwrap_err();
        assert!(matches!(error, EstateAssetError::Invalid(_)), "{error}");
        assert!(find_by_line(&surreal, project_id, " vehicle", "Sedan ")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn the_engine_refuses_a_funding_state_outside_the_closed_list() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal, "sample-schedule-engine").await;
        let result = surreal
            .query(
                "CREATE estate_asset SET project_id = $project_id, category = 'account', \
                 description = 'Checking', passes_by = 'trust', funding_state = 'maybe', \
                 inserted_at = '', updated_at = ''",
            )
            .bind(("project_id", record_id("project", project_id)))
            .await
            .and_then(surrealdb::IndexedResults::check);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rows_are_scoped_to_their_project() {
        let surreal = mem_surreal().await;
        let mine = project(&surreal, "sample-schedule-mine").await;
        let other = project(&surreal, "sample-schedule-other").await;
        let stored = insert(
            &surreal,
            mine,
            &line("account", "Brokerage", "consent_needed"),
        )
        .await
        .unwrap();
        assert!(list(&surreal, other).await.unwrap().is_empty());
        assert!(find(&surreal, other, stored.id).await.unwrap().is_none());
        delete_for_project(&surreal, mine).await.unwrap();
        assert!(list(&surreal, mine).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_missing_project_is_refused() {
        let surreal = mem_surreal().await;
        assert!(matches!(
            insert(
                &surreal,
                Uuid::now_v7(),
                &line("account", "Savings", "funded")
            )
            .await
            .unwrap_err(),
            EstateAssetError::Invalid(_)
        ));
    }
}
