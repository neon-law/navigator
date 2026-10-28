//! Native PostgreSQL Project records, preparatory to coherent application authority.
//!
//! These operations use the supplied PostgreSQL pool exclusively. The shared
//! tenancy authority guard remains closed until lifecycle and access consumers move.

use super::PostgresDb;
use crate::projects::{NewProject, Project};
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::Row;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("that Project code is already in use")]
    CodeTaken,
    #[error("the Project repository URL is invalid")]
    InvalidRepositoryUrl,
    #[error("a Project code cannot change after creation")]
    CodeImmutable,
    #[error("that Project code is reserved")]
    ReservedCode,
    #[error("the Project Brand does not exist")]
    UnknownBrand,
    #[error("the Project Entity does not exist")]
    NoSuchEntity,
    #[error("the Project Firm does not exist")]
    NoSuchFirm,
    #[error("the Project Jurisdiction does not exist")]
    NoSuchJurisdiction,
    #[error("the concurrent Project could not be found")]
    WriteReturnedNothing,
    #[error("PostgreSQL Project operation failed")]
    Query(#[from] sqlx::Error),
}

fn write_error(error: sqlx::Error) -> ProjectError {
    if let sqlx::Error::Database(ref database) = error {
        match database.constraint() {
            Some("project_code_immutable") => return ProjectError::CodeImmutable,
            Some("project_code_reserved") => return ProjectError::ReservedCode,
            Some("project_entity_id_fkey") => return ProjectError::NoSuchEntity,
            Some("project_firm_id_fkey") => return ProjectError::NoSuchFirm,
            Some("project_jurisdiction_id_fkey") => return ProjectError::NoSuchJurisdiction,
            _ if database.is_unique_violation() => return ProjectError::CodeTaken,
            _ => {}
        }
    }
    ProjectError::Query(error)
}

fn row(row: &sqlx::postgres::PgRow) -> Result<Project, ProjectError> {
    let time = |name| -> Result<String, sqlx::Error> {
        let time: DateTime<Utc> = row.try_get(name)?;
        Ok(time.to_rfc3339_opts(SecondsFormat::Micros, true))
    };
    Ok(Project {
        id: row.try_get("id")?,
        code: row.try_get("code")?,
        name: row.try_get("name")?,
        status: row.try_get("status")?,
        brand: row.try_get("brand")?,
        entity_id: row.try_get("entity_id")?,
        firm_id: row.try_get("firm_id")?,
        jurisdiction_id: row.try_get("jurisdiction_id")?,
        description: row.try_get("description")?,
        drive_folder_id: row.try_get("drive_folder_id")?,
        repository_url: row.try_get("repository_url")?,
        git_initialized_at: row.try_get("git_initialized_at")?,
        forge_provisioned_at: row.try_get("forge_provisioned_at")?,
        closed_at: row.try_get("closed_at")?,
        closure_reason: row.try_get("closure_reason")?,
        internal_slack_channel_url: row.try_get("internal_slack_channel_url")?,
        external_slack_channel_url: row.try_get("external_slack_channel_url")?,
        internal_slack_channel_id: row.try_get("internal_slack_channel_id")?,
        private_notion_page_url: row.try_get("private_notion_page_url")?,
        shared_notion_page_url: row.try_get("shared_notion_page_url")?,
        calendar_id: row.try_get("calendar_id")?,
        booking_url: row.try_get("booking_url")?,
        inserted_at: time("inserted_at")?,
        updated_at: time("updated_at")?,
    })
}

/// Find the Project with this exact UUID; missing rows remain absent.
pub async fn find_by_id_on(pg: &PostgresDb, id: Uuid) -> Result<Option<Project>, ProjectError> {
    sqlx::query("SELECT * FROM project WHERE id=$1")
        .bind(id)
        .fetch_optional(pg.pool())
        .await?
        .as_ref()
        .map(row)
        .transpose()
}

/// Resolve the exact external Project coordinate without rewriting its spelling.
pub async fn find_by_code_on(pg: &PostgresDb, code: &str) -> Result<Option<Project>, ProjectError> {
    sqlx::query("SELECT * FROM project WHERE code=$1")
        .bind(code)
        .fetch_optional(pg.pool())
        .await?
        .as_ref()
        .map(row)
        .transpose()
}

/// Seed lookup by display name; ties use UUID order.
pub async fn find_by_name_on(pg: &PostgresDb, name: &str) -> Result<Option<Project>, ProjectError> {
    sqlx::query("SELECT * FROM project WHERE name=$1 ORDER BY id LIMIT 1")
        .bind(name)
        .fetch_optional(pg.pool())
        .await?
        .as_ref()
        .map(row)
        .transpose()
}

/// All Projects in the existing stable directory order.
pub async fn all_on(pg: &PostgresDb) -> Result<Vec<Project>, ProjectError> {
    sqlx::query("SELECT * FROM project ORDER BY name,id")
        .fetch_all(pg.pool())
        .await?
        .iter()
        .map(row)
        .collect()
}

/// Create a Project with a fresh UUIDv4 and database-maintained timestamps.
pub async fn create_on(pg: &PostgresDb, input: &NewProject) -> Result<Project, ProjectError> {
    write_on(pg, Uuid::new_v4(), input, false).await
}

/// Reconcile a fixed-ID seed without clearing optional Firm/Jurisdiction or
/// resource fields. The immutable-code trigger refuses a renamed seed.
pub async fn upsert_with_id_on(
    pg: &PostgresDb,
    id: Uuid,
    input: &NewProject,
) -> Result<Project, ProjectError> {
    write_on(pg, id, input, true).await
}

async fn write_on(
    pg: &PostgresDb,
    id: Uuid,
    input: &NewProject,
    upsert: bool,
) -> Result<Project, ProjectError> {
    let mut tx = pg.pool().begin().await?;
    let brand: Option<String> =
        sqlx::query_scalar("SELECT brand_key FROM brand WHERE brand_key=$1 FOR KEY SHARE")
            .bind(&input.brand)
            .fetch_optional(&mut *tx)
            .await?;
    if brand.is_none() {
        return Err(ProjectError::UnknownBrand);
    }
    let sql = if upsert {
        "INSERT INTO project (id,code,name,status,brand,entity_id,firm_id,jurisdiction_id,description) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT (id) DO UPDATE SET \
         code=EXCLUDED.code,name=EXCLUDED.name,status=EXCLUDED.status,brand=EXCLUDED.brand, \
         entity_id=EXCLUDED.entity_id,firm_id=COALESCE(EXCLUDED.firm_id,project.firm_id), \
         jurisdiction_id=COALESCE(EXCLUDED.jurisdiction_id,project.jurisdiction_id), \
         description=EXCLUDED.description,updated_at=clock_timestamp() RETURNING *"
    } else {
        "INSERT INTO project (id,code,name,status,brand,entity_id,firm_id,jurisdiction_id,description) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING *"
    };
    let result = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .bind(&input.code)
        .bind(&input.name)
        .bind(&input.status)
        .bind(&input.brand)
        .bind(input.entity_id)
        .bind(input.firm_id)
        .bind(input.jurisdiction_id)
        .bind(&input.description)
        .fetch_one(&mut *tx)
        .await
        .map_err(write_error)?;
    let result = row(&result)?;
    tx.commit().await?;
    Ok(result)
}

/// Resolve one code under concurrent fixture creation, adopting the winning row.
/// Existing rows retain their recorded identity and contents.
pub async fn find_or_create_by_code_on(
    pg: &PostgresDb,
    id: Uuid,
    input: &NewProject,
) -> Result<Project, ProjectError> {
    if let Some(existing) = find_by_code_on(pg, &input.code).await? {
        return Ok(existing);
    }
    match upsert_with_id_on(pg, id, input).await {
        Err(ProjectError::CodeTaken) => find_by_code_on(pg, &input.code)
            .await?
            .ok_or(ProjectError::WriteReturnedNothing),
        result => result,
    }
}

/// Record a repository and provisioning stamp together, retaining a concurrent
/// winner. The returned row always carries the stored coordinate.
pub async fn record_provisioned_repository_on(
    pg: &PostgresDb,
    project_id: Uuid,
    repository_url: &str,
    provisioned_at: &str,
) -> Result<Option<Project>, ProjectError> {
    record_repository_on(pg, project_id, repository_url, provisioned_at, false).await
}

/// Record verified source initialization without replacing another repository
/// or either first-success stamp. Uses only the supplied PostgreSQL pool.
pub async fn record_source_initialized_on(
    pg: &PostgresDb,
    project_id: Uuid,
    repository_url: &str,
    initialized_at: &str,
) -> Result<Option<Project>, ProjectError> {
    record_repository_on(pg, project_id, repository_url, initialized_at, true).await
}

async fn record_repository_on(
    pg: &PostgresDb,
    project_id: Uuid,
    repository_url: &str,
    recorded_at: &str,
    initialized: bool,
) -> Result<Option<Project>, ProjectError> {
    let url = repository_url.trim();
    if !crate::projects::is_valid_repository_url(url) {
        return Err(ProjectError::InvalidRepositoryUrl);
    }
    let mut tx = pg.pool().begin().await?;
    // Lock before inspecting the pointer: a competing provisioning request
    // observes the winning row after waiting, rather than its earlier snapshot.
    let current = sqlx::query("SELECT * FROM project WHERE id=$1 FOR UPDATE")
        .bind(project_id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(current) = current else {
        return Ok(None);
    };
    let current = row(&current)?;
    let accepts = current.repository_url.is_none()
        || (initialized && current.repository_url.as_deref() == Some(url));
    if !accepts {
        tx.commit().await?;
        return Ok(Some(current));
    }
    let result = sqlx::query(
        "UPDATE project SET repository_url=$2, \
         forge_provisioned_at=COALESCE(forge_provisioned_at,$3), \
         git_initialized_at=CASE WHEN $4 THEN COALESCE(git_initialized_at,$3) ELSE git_initialized_at END, \
         updated_at=clock_timestamp() WHERE id=$1 RETURNING *",
    )
    .bind(project_id)
    .bind(url)
    .bind(recorded_at)
    .bind(initialized)
    .fetch_one(&mut *tx)
    .await?;
    let result = row(&result)?;
    tx.commit().await?;
    Ok(Some(result))
}
