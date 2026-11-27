//! Project-scoped [Estate Asset](../../docs/glossary/estate-asset.md) rows
//! (`estate_asset`): one line of an estate plan's asset schedule.
//!
//! A line names the property (`category`, `description`), how it passes at
//! death (`passes_by`), and whether it is funded into the plan
//! (`funding_state`). One row holds each (category, description) line on a
//! Project. The table is `estate_asset`, not `asset`: `asset` stores byte
//! pointers for documents and content, and an Estate Asset is property.

use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::matter_record_links::{one_of, optional_text, require_project, required_text};
use crate::postgres::matter::{self, MatterAuthorityError};
pub use model::estate_assets::{FUNDING_STATES, PASSES_BY};

const TABLE: &str = "estate_asset";
const LINE_INDEX: &str = "estate_asset_line";

/// One stored Estate Asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

// Deletion mirroring needs identity, not the current schedule's complete shape.

#[derive(Debug, thiserror::Error)]
pub enum EstateAssetError {
    #[error(transparent)]
    Postgres(MatterAuthorityError),
    #[error("{0}")]
    Invalid(String),
    #[error("no such estate asset")]
    NotFound,
    #[error("writing an estate asset returned no usable row")]
    WriteReturnedNothing,
}

const DUPLICATE_LINE: &str =
    "this Project's schedule already has a line with that category and description";

impl From<MatterAuthorityError> for EstateAssetError {
    fn from(error: MatterAuthorityError) -> Self {
        match error {
            MatterAuthorityError::Conflict(index) if index == LINE_INDEX => {
                Self::Invalid(DUPLICATE_LINE.into())
            }
            error => Self::Postgres(error),
        }
    }
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
    _application_store: &ApplicationStore,
    id: Uuid,
    project_id: Uuid,
    fields: EstateAssetFields,
    inserted_at: &str,
) -> Result<EstateAsset, EstateAssetError> {
    let now = chrono::Utc::now().to_rfc3339();
    let pg = matter::authority().await?;
    let row = EstateAsset {
        id,
        project_id,
        category: fields.category,
        description: fields.description,
        passes_by: fields.passes_by,
        funding_state: fields.funding_state,
        note: fields.note,
        inserted_at: inserted_at.to_owned(),
        updated_at: now,
    };
    Ok(matter::upsert(&pg, TABLE, &row).await?)
}

/// A Project's asset schedule, by category then description.
///
/// # Errors
///
/// [`EstateAssetError::Db`] if the read fails.
pub async fn list(
    _application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<EstateAsset>, EstateAssetError> {
    let pg = matter::authority().await?;
    let filter = json!({"project_id": project_id});
    Ok(matter::select(&pg, TABLE, filter, &["category", "description"]).await?)
}

/// One Estate Asset on a Project, or `None`.
///
/// # Errors
///
/// [`EstateAssetError::Db`] if the read fails.
pub async fn find(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    id: Uuid,
) -> Result<Option<EstateAsset>, EstateAssetError> {
    let pg = matter::authority().await?;
    let filter = json!({"id": id, "project_id": project_id});
    Ok(matter::find(&pg, TABLE, filter).await?)
}

/// The row holding one (category, description) line, or `None`. Both are
/// matched as stored, trimmed.
///
/// # Errors
///
/// [`EstateAssetError::Db`] if the read fails.
pub async fn find_by_line(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    category: &str,
    description: &str,
) -> Result<Option<EstateAsset>, EstateAssetError> {
    let pg = matter::authority().await?;
    let filter = json!({
        "project_id": project_id,
        "category": category.trim(),
        "description": description.trim(),
    });
    Ok(matter::find(&pg, TABLE, filter).await?)
}

/// Store a new Estate Asset and return it.
///
/// # Errors
///
/// [`EstateAssetError::Invalid`] for a blank category or description, a
/// `passes_by` or `funding_state` outside its list, a missing Project, or a
/// line already on the schedule; [`EstateAssetError::Db`] if the write fails.
pub async fn insert(
    application_store: &ApplicationStore,
    project_id: Uuid,
    fields: &EstateAssetFields,
) -> Result<EstateAsset, EstateAssetError> {
    let fields = normalize(fields)?;
    require_project(application_store, project_id)
        .await
        .map_err(EstateAssetError::Invalid)?;
    let inserted_at = chrono::Utc::now().to_rfc3339();
    persist(
        application_store,
        Uuid::now_v7(),
        project_id,
        fields,
        &inserted_at,
    )
    .await
}

/// Replace an Estate Asset's fields and return the stored row.
///
/// # Errors
///
/// [`EstateAssetError::NotFound`] when the row is not on this Project;
/// otherwise as [`insert`].
pub async fn update(
    application_store: &ApplicationStore,
    project_id: Uuid,
    id: Uuid,
    fields: &EstateAssetFields,
) -> Result<EstateAsset, EstateAssetError> {
    let existing = find(application_store, project_id, id)
        .await?
        .ok_or(EstateAssetError::NotFound)?;
    let fields = normalize(fields)?;
    persist(
        application_store,
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
    application_store: &ApplicationStore,
    project_id: Uuid,
    id: Uuid,
) -> Result<(), EstateAssetError> {
    find(application_store, project_id, id)
        .await?
        .ok_or(EstateAssetError::NotFound)?;
    let pg = matter::authority().await?;
    matter::delete(&pg, TABLE, json!({"id": id})).await?;
    Ok(())
}

/// Remove every Estate Asset on a Project. The rows are imported from the
/// Project repository's seeds, so they go with the Project row.
///
/// # Errors
///
/// [`EstateAssetError::Db`] if the delete fails.
pub async fn delete_for_project(
    _application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<(), EstateAssetError> {
    let pg = matter::authority().await?;
    matter::delete(&pg, TABLE, json!({"project_id": project_id})).await?;
    Ok(())
}

#[cfg(test)]
#[path = "estate_assets/compatibility_tests.rs"]
mod compatibility_tests;

#[cfg(test)]
#[path = "estate_assets/native_tests.rs"]
mod native_tests;
