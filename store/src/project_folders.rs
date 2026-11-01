//! Durable Drive-folder identities for a Project, stored in PostgreSQL.
//!
//! Provisioning reserves a provider ID before remote create. A binding is
//! immutable: repeated calls converge only when the slot, parent and provider
//! identity agree. Provider permissions never establish Navigator authority.

use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use crate::document_pointers::MatterFolder;
use crate::postgres::PostgresDb;

/// A provider folder's position in the Project's fixed tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectFolderSlot {
    Root,
    Matter(MatterFolder),
    ClientSubmissions,
}

impl ProjectFolderSlot {
    /// The persisted slot uses the canonical Matter Folder path verbatim.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Matter(folder) => folder.as_str(),
            Self::ClientSubmissions => "client_submissions",
        }
    }

    /// Refuse unknown slots and the retired, bare Discovery folder.
    #[must_use]
    pub fn parse(key: &str) -> Option<Self> {
        match key {
            "root" => Some(Self::Root),
            "client_submissions" => Some(Self::ClientSubmissions),
            other => MatterFolder::parse(other)
                .filter(|folder| MatterFolder::TREE.contains(folder))
                .map(Self::Matter),
        }
    }

    fn parent(self) -> Result<Option<Self>, ProjectFolderError> {
        match self {
            Self::Root | Self::ClientSubmissions => Ok(None),
            Self::Matter(MatterFolder::Discovery) => Err(ProjectFolderError::InvalidSlot),
            Self::Matter(folder) if folder.is_work_product_subfolder() => {
                Ok(Some(Self::Matter(MatterFolder::WorkProduct)))
            }
            Self::Matter(_) => Ok(Some(Self::Root)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectFolder {
    pub id: Uuid,
    pub project_id: Uuid,
    pub slot: ProjectFolderSlot,
    pub drive_folder_id: String,
    /// A persisted parent on this same Project, when the slot is in its tree.
    pub parent_id: Option<Uuid>,
    /// The provider parent; roots may name an external shared-drive parent.
    pub parent_drive_folder_id: Option<String>,
    pub inserted_at: DateTime<Utc>,
    /// Set only after provider identity and access checks; None is a reservation.
    pub verified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectFolderError {
    #[error("unknown Project Folder slot")]
    InvalidSlot,
    #[error("invalid provider folder identity")]
    InvalidIdentity,
    #[error("the folder's parent does not match its Project and slot")]
    InvalidParent,
    #[error("the Project slot or provider folder already has another binding")]
    BindingConflict,
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
}

const COLUMNS: &str =
    "id, project_id, slot, drive_folder_id, parent_id, parent_drive_folder_id, inserted_at, verified_at";

fn from_row(row: &sqlx::postgres::PgRow) -> Result<ProjectFolder, ProjectFolderError> {
    Ok(ProjectFolder {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        slot: ProjectFolderSlot::parse(&row.try_get::<String, _>("slot")?)
            .ok_or(ProjectFolderError::InvalidSlot)?,
        drive_folder_id: row.try_get("drive_folder_id")?,
        parent_id: row.try_get("parent_id")?,
        parent_drive_folder_id: row.try_get("parent_drive_folder_id")?,
        inserted_at: row.try_get("inserted_at")?,
        verified_at: row.try_get("verified_at")?,
    })
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !value.chars().any(char::is_whitespace)
        && !value.chars().any(char::is_control)
}

/// Read one Project's slot. Callers enforce Project participation first.
///
/// # Errors
/// Returns a database error or an unrepresentable stored slot.
pub async fn get(
    pg: &PostgresDb,
    project_id: Uuid,
    slot: ProjectFolderSlot,
) -> Result<Option<ProjectFolder>, ProjectFolderError> {
    slot.parent()?;
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM project_folder WHERE project_id=$1 AND slot=$2"
    )))
    .bind(project_id)
    .bind(slot.key())
    .fetch_optional(pg.pool())
    .await?;
    row.as_ref().map(from_row).transpose()
}

/// Read only this Project's bindings, including interrupted provisioning.
///
/// # Errors
/// Returns a database error or an unrepresentable stored slot.
pub async fn list(
    pg: &PostgresDb,
    project_id: Uuid,
) -> Result<Vec<ProjectFolder>, ProjectFolderError> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM project_folder WHERE project_id=$1 ORDER BY slot"
    )))
    .bind(project_id)
    .fetch_all(pg.pool())
    .await?
    .iter()
    .map(from_row)
    .collect()
}

/// Reserve an immutable provider ID before creating or adopting its folder.
///
/// A reserved ID is not evidence that a remote folder exists or is safe. The
/// reconciler must verify the provider identity, parent and permissions, then
/// call [`mark_verified`] before resources or importers consume the binding.
///
/// Roots and Client Submissions have no owned parent; their optional remote
/// parent may be outside the Project tree. Matter slots must name their exact
/// persisted root or Work Product parent on this Project. The database pins
/// parent scope and global provider identity even during concurrent calls.
///
/// # Errors
/// Refuses malformed identities, wrong parents, a conflicting binding, or a
/// database failure. The referenced Project must already exist in PostgreSQL.
pub async fn bind(
    pg: &PostgresDb,
    project_id: Uuid,
    slot: ProjectFolderSlot,
    drive_folder_id: &str,
    parent_id: Option<Uuid>,
    parent_drive_folder_id: Option<&str>,
) -> Result<ProjectFolder, ProjectFolderError> {
    let expected_parent = slot.parent()?;
    if !valid_identity(drive_folder_id)
        || parent_drive_folder_id.is_some_and(|value| !valid_identity(value))
    {
        return Err(ProjectFolderError::InvalidIdentity);
    }
    let mut tx = pg.pool().begin().await?;
    match (expected_parent, parent_id) {
        (None, None) => {}
        (Some(expected), Some(parent)) => {
            let row = sqlx::query(
                "SELECT slot, drive_folder_id FROM project_folder WHERE project_id=$1 AND id=$2 FOR KEY SHARE",
            )
            .bind(project_id)
            .bind(parent)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ProjectFolderError::InvalidParent)?;
            if row.try_get::<String, _>("slot")? != expected.key()
                || parent_drive_folder_id != Some(row.try_get::<&str, _>("drive_folder_id")?)
            {
                return Err(ProjectFolderError::InvalidParent);
            }
        }
        _ => return Err(ProjectFolderError::InvalidParent),
    }
    let inserted = sqlx::query(
        "INSERT INTO project_folder (id,project_id,slot,drive_folder_id,parent_id,parent_drive_folder_id,inserted_at) \
         VALUES ($1,$2,$3,$4,$5,$6,now()) ON CONFLICT (project_id,slot) DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(slot.key())
    .bind(drive_folder_id)
    .bind(parent_id)
    .bind(parent_drive_folder_id)
    .execute(&mut *tx)
    .await;
    if let Err(error) = inserted {
        if error
            .as_database_error()
            .is_some_and(|db| db.code().as_deref() == Some("23505"))
        {
            return Err(ProjectFolderError::BindingConflict);
        }
        return Err(error.into());
    }
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM project_folder WHERE project_id=$1 AND slot=$2"
    )))
    .bind(project_id)
    .bind(slot.key())
    .fetch_one(&mut *tx)
    .await?;
    let saved = from_row(&row)?;
    if saved.drive_folder_id != drive_folder_id
        || saved.parent_id != parent_id
        || saved.parent_drive_folder_id.as_deref() != parent_drive_folder_id
    {
        return Err(ProjectFolderError::BindingConflict);
    }
    tx.commit().await?;
    Ok(saved)
}

/// Read a folder only after successful provider verification.
///
/// # Errors
/// Returns a database error or an unrepresentable stored slot.
pub async fn get_verified(
    pg: &PostgresDb,
    project_id: Uuid,
    slot: ProjectFolderSlot,
) -> Result<Option<ProjectFolder>, ProjectFolderError> {
    Ok(get(pg, project_id, slot)
        .await?
        .filter(|folder| folder.verified_at.is_some()))
}

/// Read only verified folders; unresolved reservations stay internal.
///
/// # Errors
/// Returns a database error or an unrepresentable stored slot.
pub async fn list_verified(
    pg: &PostgresDb,
    project_id: Uuid,
) -> Result<Vec<ProjectFolder>, ProjectFolderError> {
    Ok(list(pg, project_id)
        .await?
        .into_iter()
        .filter(|folder| folder.verified_at.is_some())
        .collect())
}

/// Record a successful provider identity, parent and permission check.
///
/// The trusted reconciler calls this only after every check succeeds. Repeating
/// the same success retains its timestamp. Importers still revalidate ancestry
/// and authority immediately before reading remote bytes.
///
/// # Errors
/// Refuses a mismatched reservation or a database failure.
pub async fn mark_verified(
    pg: &PostgresDb,
    project_id: Uuid,
    slot: ProjectFolderSlot,
    drive_folder_id: &str,
) -> Result<ProjectFolder, ProjectFolderError> {
    slot.parent()?;
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE project_folder SET verified_at=COALESCE(verified_at,now()) \
         WHERE project_id=$1 AND slot=$2 AND drive_folder_id=$3 RETURNING {COLUMNS}"
    )))
    .bind(project_id)
    .bind(slot.key())
    .bind(drive_folder_id)
    .fetch_optional(pg.pool())
    .await?
    .ok_or(ProjectFolderError::BindingConflict)?;
    from_row(&row)
}

/// Remove a previous success before retrying remote validation.
///
/// A failed or interrupted reconciliation then leaves an internal reservation,
/// never an old verification exposed as a current success.
///
/// # Errors
/// Refuses a mismatched reservation or a database failure.
pub async fn clear_verification(
    pg: &PostgresDb,
    project_id: Uuid,
    slot: ProjectFolderSlot,
    drive_folder_id: &str,
) -> Result<(), ProjectFolderError> {
    slot.parent()?;
    let result = sqlx::query(
        "UPDATE project_folder SET verified_at=NULL WHERE project_id=$1 AND slot=$2 AND drive_folder_id=$3",
    )
    .bind(project_id).bind(slot.key()).bind(drive_folder_id)
    .execute(pg.pool()).await?;
    if result.rows_affected() != 1 {
        return Err(ProjectFolderError::BindingConflict);
    }
    Ok(())
}

/// Invalidate all prior successes before a Project reconciliation begins.
///
/// Call once while holding the Project provisioning guard, before provider or
/// permission checks. A parent-check failure must not leave old child successes
/// visible. Reservation identities are retained for recovery.
///
/// # Errors
/// Returns a database failure.
pub async fn clear_project_verification(
    pg: &PostgresDb,
    project_id: Uuid,
) -> Result<(), ProjectFolderError> {
    sqlx::query("UPDATE project_folder SET verified_at=NULL WHERE project_id=$1")
        .bind(project_id)
        .execute(pg.pool())
        .await?;
    Ok(())
}

/// Whether another Project reserves this provider identity.
///
/// Importers check every selected root and descendant against all reservations,
/// including unverified ones, before reading remote bytes.
///
/// # Errors
/// Returns a database failure.
pub async fn recorded_elsewhere(
    pg: &PostgresDb,
    project_id: Uuid,
    drive_folder_id: &str,
) -> Result<bool, ProjectFolderError> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM project_folder WHERE drive_folder_id=$1 AND project_id<>$2)",
    )
    .bind(drive_folder_id)
    .bind(project_id)
    .fetch_one(pg.pool())
    .await?)
}
