//! Native PostgreSQL Project records.
//!
//! `crate::projects` routes Project reads and writes here under tenancy
//! authority. These operations use the supplied PostgreSQL pool exclusively.

use super::PostgresDb;
use crate::projects::{NewProject, Project};
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use sqlx::{Postgres, QueryBuilder, Row};
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
    #[error("the Firm no longer wears the Project Brand")]
    FirmBrandChanged,
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

pub(super) fn row(row: &sqlx::postgres::PgRow) -> Result<Project, ProjectError> {
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
        onboarding_jurisdiction: row.try_get("onboarding_jurisdiction")?,
        client_dri_person_id: row.try_get("client_dri_person_id")?,
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
        calendar_timezone: row.try_get("calendar_timezone")?,
        booking_url: row.try_get("booking_url")?,
        // Plan-panel economics (ENG-1269). PG-native rows carry the dates
        // as DATE; the SurrealDB-authored surface keeps ISO text, so this
        // reader renders the same shape both lanes read.
        legal_plan_id: row.try_get("legal_plan_id")?,
        plan_member_id: row.try_get("plan_member_id")?,
        plan_case_number: row.try_get("plan_case_number")?,
        benefit_code: row.try_get("benefit_code")?,
        plan_matter_type: row.try_get("plan_matter_type")?,
        plan_state: row.try_get("plan_state")?,
        plan_assigned_on: row
            .try_get::<Option<chrono::NaiveDate>, _>("plan_assigned_on")?
            .map(|date| date.to_string()),
        plan_accepted_on: row
            .try_get::<Option<chrono::NaiveDate>, _>("plan_accepted_on")?
            .map(|date| date.to_string()),
        plan_closed_on: row
            .try_get::<Option<chrono::NaiveDate>, _>("plan_closed_on")?
            .map(|date| date.to_string()),
        kit_version: row.try_get("kit_version")?,
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

/// Whether another Project owns this Drive folder.
pub(crate) async fn drive_folder_recorded_elsewhere_on(
    pg: &PostgresDb,
    project_id: Uuid,
    drive_folder_id: &str,
) -> Result<bool, ProjectError> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM project WHERE drive_folder_id = $1 AND id <> $2)",
    )
    .bind(drive_folder_id)
    .bind(project_id)
    .fetch_one(pg.pool())
    .await?)
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

/// Fetch the participant's Project rows under one admission check. The C collation
/// preserves the directory's Rust string ordering across database locales.
pub(crate) async fn participating_projects_on(
    pg: &PostgresDb,
    person: Uuid,
    lens: crate::access::ProjectLens,
    code: Option<&str>,
) -> Result<Vec<Project>, ProjectError> {
    sqlx::query(
        r#"SELECT (project).* FROM project_list
           WHERE person_id = $1 AND (is_client_dri OR participation = ANY($2)) = $3
             AND ($4::text IS NULL OR (project).code = $4)
           ORDER BY (project).name COLLATE "C", (project).id"#,
    )
    .bind(person)
    .bind(crate::projects::PARTICIPATION_CLIENT_SIDE)
    .bind(lens == crate::access::ProjectLens::Client)
    .bind(code)
    .fetch_all(pg.pool())
    .await?
    .iter()
    .map(row)
    .collect()
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

/// The repository coordinates of Projects explicitly owned by the selected Firm.
#[derive(Debug)]
pub struct FirmProjectSource {
    pub id: Uuid,
    pub code: String,
    pub repository_url: Option<String>,
}

/// List a Firm's Projects in stable order, including closed and unprovisioned Projects.
pub async fn firm_project_sources_on(
    pg: &PostgresDb,
    firm_id: Uuid,
    after: Option<Uuid>,
    limit: i64,
) -> Result<Vec<FirmProjectSource>, ProjectError> {
    let rows: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT id, code, repository_url FROM project
         WHERE firm_id = $1 AND ($2::uuid IS NULL OR id > $2)
         ORDER BY id LIMIT $3",
    )
    .bind(firm_id)
    .bind(after)
    .bind(limit)
    .fetch_all(pg.pool())
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, code, repository_url)| FirmProjectSource {
            id,
            code,
            repository_url,
        })
        .collect())
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
    crate::durable_object_handover::hold_creation_epoch(&mut tx).await?;
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
    if !upsert {
        crate::project_slack_deliveries::initialize_fresh(&mut tx, result.id).await?;
    }
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

/// One column a sparse Project update writes. The name is always one of this
/// crate's own column names, never request input.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Column<'a> {
    Text(&'static str, Option<&'a str>),
    Id(&'static str, Option<Uuid>),
    Date(&'static str, Option<NaiveDate>),
    Integer(&'static str, Option<i32>),
    /// Retain benefit terms while both identifying fields still describe the same benefit.
    KeepPlanBenefit(&'static str, Option<Uuid>, Option<&'a str>),
}

/// Update only `columns` on one Project and return the committed row, or
/// `None` when no Project matches. A Firm guard prevents overwriting a
/// concurrent transfer. A supplied Brand must still belong to that Firm.
pub(crate) async fn update_on(
    pg: &PostgresDb,
    id: Uuid,
    columns: &[Column<'_>],
    within_firm: Option<(Uuid, Option<&str>)>,
) -> Result<Option<Project>, ProjectError> {
    let mut builder =
        QueryBuilder::<Postgres>::new("UPDATE project SET updated_at=clock_timestamp()");
    for column in columns {
        match *column {
            Column::Text(name, value) => builder.push(format_args!(", {name}=")).push_bind(value),
            Column::Id(name, value) => builder.push(format_args!(", {name}=")).push_bind(value),
            Column::Date(name, value) => builder.push(format_args!(", {name}=")).push_bind(value),
            Column::Integer(name, value) => {
                builder.push(format_args!(", {name}=")).push_bind(value)
            }
            Column::KeepPlanBenefit(name, plan, benefit) => builder
                .push(format_args!(
                    ", {name}=CASE WHEN legal_plan_id IS NOT DISTINCT FROM "
                ))
                .push_bind(plan)
                .push(" AND benefit_code IS NOT DISTINCT FROM ")
                .push_bind(benefit)
                .push(format_args!(" THEN {name} END")),
        };
    }
    builder.push(" WHERE id=").push_bind(id);
    if let Some((firm_id, brand)) = within_firm {
        builder.push(" AND firm_id=").push_bind(firm_id);
        if let Some(brand) = brand {
            builder
                .push(" AND EXISTS (SELECT 1 FROM firm_brand WHERE firm_brand.firm_id=")
                .push_bind(firm_id)
                .push(" AND firm_brand.brand_key=")
                .push_bind(brand)
                .push(")");
        }
    }
    builder.push(" RETURNING *");
    builder
        .build()
        .fetch_optional(pg.pool())
        .await
        .map_err(write_error)?
        .as_ref()
        .map(row)
        .transpose()
}

/// Add a Legal Plan link while the Project still has the expected owner and no plan.
pub(crate) async fn link_legal_plan_on(
    pg: &PostgresDb,
    id: Uuid,
    firm: Uuid,
    brand: &str,
    plan: Uuid,
) -> Result<Option<Project>, ProjectError> {
    sqlx::query(
        "UPDATE project SET legal_plan_id=$4, updated_at=clock_timestamp() \
         WHERE id=$1 AND firm_id=$2 AND brand=$3 AND legal_plan_id IS NULL RETURNING *",
    )
    .bind(id)
    .bind(firm)
    .bind(brand)
    .bind(plan)
    .fetch_optional(pg.pool())
    .await
    .map_err(write_error)?
    .as_ref()
    .map(row)
    .transpose()
}

/// Delete one Project with no participation left on it. `false` when no
/// Project has that id; a row still referenced elsewhere is a database error.
pub(crate) async fn delete_on(pg: &PostgresDb, id: Uuid) -> Result<bool, ProjectError> {
    Ok(sqlx::query("DELETE FROM project WHERE id=$1")
        .bind(id)
        .execute(pg.pool())
        .await?
        .rows_affected()
        > 0)
}

/// A matter being opened: the Project row and who is accountable for it.
pub(crate) struct OpenMatter<'a> {
    pub id: Option<Uuid>,
    pub project: &'a NewProject,
    pub closed_at: Option<&'a str>,
    pub lawyer: Uuid,
    pub lawyer_participation: &'a str,
    pub client: Uuid,
}

/// Open a matter in one transaction: the Project, the attesting lawyer's
/// Lawyer DRI row, and the client's Client DRI row. The Firm must still wear
/// the Brand when the transaction commits.
pub(crate) async fn open_on(
    pg: &PostgresDb,
    open: &OpenMatter<'_>,
) -> Result<Project, ProjectError> {
    let input = open.project;
    let mut tx = pg.pool().begin().await?;
    crate::durable_object_handover::hold_creation_epoch(&mut tx).await?;
    let wearer: Option<Uuid> =
        sqlx::query_scalar("SELECT firm_id FROM firm_brand WHERE brand_key=$1 FOR KEY SHARE")
            .bind(&input.brand)
            .fetch_optional(&mut *tx)
            .await?;
    if wearer.is_none() || wearer != input.firm_id {
        return Err(ProjectError::FirmBrandChanged);
    }
    let project = sqlx::query(
        "INSERT INTO project (id,code,name,status,brand,entity_id,firm_id,jurisdiction_id,description,closed_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING *",
    )
    .bind(open.id.unwrap_or_else(Uuid::new_v4))
    .bind(&input.code)
    .bind(&input.name)
    .bind(&input.status)
    .bind(&input.brand)
    .bind(input.entity_id)
    .bind(input.firm_id)
    .bind(input.jurisdiction_id)
    .bind(&input.description)
    .bind(open.closed_at)
    .fetch_one(&mut *tx)
    .await
    .map_err(write_error)?;
    let project = row(&project)?;
    sqlx::query(
        "INSERT INTO person_project_role (id,person_id,project_id,participation,is_lawyer_dri,is_client_dri) \
         VALUES ($1,$2,$4,$3,true,false), ($5,$6,$4,'client',false,true)",
    )
    .bind(Uuid::new_v4())
    .bind(open.lawyer)
    .bind(open.lawyer_participation)
    .bind(project.id)
    .bind(Uuid::new_v4())
    .bind(open.client)
    .execute(&mut *tx)
    .await?;
    crate::project_slack_deliveries::initialize_fresh(&mut tx, project.id).await?;
    tx.commit().await?;
    Ok(project)
}
