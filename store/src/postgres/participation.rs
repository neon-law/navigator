//! Canonical PostgreSQL Project participation ledger.
//!
//! The application authority guard remains closed until Projects and their
//! access consumers move together. These operations never read SurrealDB.

use super::PostgresDb;
use crate::projects::{DriSide, PersonProjectRole};
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::Row;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum ParticipationError {
    #[error("Project participation person does not exist")]
    NoSuchPerson,
    #[error("Project participation Project does not exist")]
    NoSuchProject,
    #[error("Project participation already exists")]
    Duplicate,
    #[error("Project participation person has an invalid role")]
    InvalidRole,
    #[error("PostgreSQL participation operation failed")]
    Query(#[from] sqlx::Error),
}

fn query_error(error: sqlx::Error) -> ParticipationError {
    if let sqlx::Error::Database(ref database) = error {
        if database.is_unique_violation() {
            return ParticipationError::Duplicate;
        }
    }
    ParticipationError::Query(error)
}

fn row(row: &sqlx::postgres::PgRow) -> Result<PersonProjectRole, ParticipationError> {
    let time = |name| -> Result<String, sqlx::Error> {
        let time: DateTime<Utc> = row.try_get(name)?;
        Ok(time.to_rfc3339_opts(SecondsFormat::Micros, true))
    };
    Ok(PersonProjectRole {
        id: row.try_get("id")?,
        person_id: row.try_get("person_id")?,
        project_id: row.try_get("project_id")?,
        participation: row.try_get("participation")?,
        is_lawyer_dri: row.try_get("is_lawyer_dri")?,
        is_client_dri: row.try_get("is_client_dri")?,
        inserted_at: time("inserted_at")?,
        updated_at: time("updated_at")?,
    })
}

/// Read one person's participation, preserving imported client-side labels and DRI flags.
pub async fn for_person_on(
    pg: &PostgresDb,
    person: Uuid,
    project: Uuid,
) -> Result<Option<PersonProjectRole>, ParticipationError> {
    sqlx::query("SELECT * FROM person_project_role WHERE person_id=$1 AND project_id=$2")
        .bind(person)
        .bind(project)
        .fetch_optional(pg.pool())
        .await?
        .as_ref()
        .map(row)
        .transpose()
}

/// Read the Project's ledger in stable insertion order.
pub async fn for_project_on(
    pg: &PostgresDb,
    project: Uuid,
) -> Result<Vec<PersonProjectRole>, ParticipationError> {
    sqlx::query("SELECT * FROM person_project_role WHERE project_id=$1 ORDER BY inserted_at,id")
        .bind(project)
        .fetch_all(pg.pool())
        .await?
        .iter()
        .map(row)
        .collect()
}

/// Read a person's Project scopes in stable insertion order.
pub async fn for_person_projects_on(
    pg: &PostgresDb,
    person: Uuid,
) -> Result<Vec<PersonProjectRole>, ParticipationError> {
    sqlx::query("SELECT * FROM person_project_role WHERE person_id=$1 ORDER BY inserted_at,id")
        .bind(person)
        .fetch_all(pg.pool())
        .await?
        .iter()
        .map(row)
        .collect()
}

/// Insert one membership; participation comes only from the current Person role.
/// Duplicate memberships are rejected by PostgreSQL, including concurrent requests.
pub async fn add_on(
    pg: &PostgresDb,
    project: Uuid,
    person: Uuid,
) -> Result<PersonProjectRole, ParticipationError> {
    write_on(pg, project, person, None).await
}

/// Designate a DRI atomically, creating their membership when absent. Existing
/// participation, IDs, insertion timestamps, and the other side's flag are retained.
/// Callers enforce the legal and access rules for designating each side.
pub async fn designate_dri_on(
    pg: &PostgresDb,
    project: Uuid,
    person: Uuid,
    side: DriSide,
) -> Result<PersonProjectRole, ParticipationError> {
    write_on(pg, project, person, Some(side)).await
}

async fn write_on(
    pg: &PostgresDb,
    project: Uuid,
    person: Uuid,
    side: Option<DriSide>,
) -> Result<PersonProjectRole, ParticipationError> {
    let mut tx = pg.pool().begin().await?;
    let exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM project WHERE id=$1 FOR UPDATE")
        .bind(project)
        .fetch_optional(&mut *tx)
        .await?;
    if exists.is_none() {
        return Err(ParticipationError::NoSuchProject);
    }
    let role: Option<String> = sqlx::query_scalar("SELECT role FROM person WHERE id=$1 FOR SHARE")
        .bind(person)
        .fetch_optional(&mut *tx)
        .await?;
    let role = role.ok_or(ParticipationError::NoSuchPerson)?;
    let role = crate::persons::Role::parse(&role).ok_or(ParticipationError::InvalidRole)?;
    let (lawyer, client) = match side {
        Some(DriSide::Lawyer) => (true, false),
        Some(DriSide::Client) => (false, true),
        None => (false, false),
    };
    let sql = if side.is_some() {
        "INSERT INTO person_project_role (id,person_id,project_id,participation,is_lawyer_dri,is_client_dri) \
         VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (person_id,project_id) DO UPDATE SET \
         is_lawyer_dri=person_project_role.is_lawyer_dri OR EXCLUDED.is_lawyer_dri, \
         is_client_dri=person_project_role.is_client_dri OR EXCLUDED.is_client_dri,updated_at=clock_timestamp() RETURNING *"
    } else {
        "INSERT INTO person_project_role (id,person_id,project_id,participation,is_lawyer_dri,is_client_dri) \
         VALUES ($1,$2,$3,$4,$5,$6) RETURNING *"
    };
    let result = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(Uuid::new_v4())
        .bind(person)
        .bind(project)
        .bind(crate::projects::participation_for_role(role))
        .bind(lawyer)
        .bind(client)
        .fetch_one(&mut *tx)
        .await
        .map_err(query_error)?;
    if side.is_some() {
        sqlx::query("UPDATE project SET updated_at=clock_timestamp() WHERE id=$1")
            .bind(project)
            .execute(&mut *tx)
            .await?;
    }
    let result = row(&result)?;
    tx.commit().await?;
    Ok(result)
}

/// Clear only the requested DRI flag; callers enforce the last-lawyer invariant.
pub async fn clear_dri_on(
    pg: &PostgresDb,
    id: Uuid,
    side: DriSide,
) -> Result<(), ParticipationError> {
    let sql = match side {
        DriSide::Lawyer => "UPDATE person_project_role SET is_lawyer_dri=false,updated_at=clock_timestamp() WHERE id=$1",
        DriSide::Client => "UPDATE person_project_role SET is_client_dri=false,updated_at=clock_timestamp() WHERE id=$1",
    };
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .execute(pg.pool())
        .await?;
    Ok(())
}

/// Replace a membership's Person while retaining its DRI flags and insertion
/// timestamp. The caller checks whether that replacement is valid for its side.
/// The participation label is derived under the same transaction as the write.
pub async fn update_on(
    pg: &PostgresDb,
    id: Uuid,
    person: Uuid,
) -> Result<Option<PersonProjectRole>, ParticipationError> {
    let mut tx = pg.pool().begin().await?;
    let role: Option<String> = sqlx::query_scalar("SELECT role FROM person WHERE id=$1 FOR SHARE")
        .bind(person)
        .fetch_optional(&mut *tx)
        .await?;
    let role = role.ok_or(ParticipationError::NoSuchPerson)?;
    let role = crate::persons::Role::parse(&role).ok_or(ParticipationError::InvalidRole)?;
    let result = sqlx::query(
        "UPDATE person_project_role SET person_id=$2,participation=$3,updated_at=clock_timestamp() WHERE id=$1 RETURNING *",
    ).bind(id).bind(person).bind(crate::projects::participation_for_role(role)).fetch_optional(&mut *tx).await.map_err(query_error)?;
    let result = result.as_ref().map(row).transpose()?;
    tx.commit().await?;
    Ok(result)
}

/// Remove one row idempotently. Callers enforce the last-lawyer invariant.
pub async fn remove_on(pg: &PostgresDb, id: Uuid) -> Result<(), ParticipationError> {
    sqlx::query("DELETE FROM person_project_role WHERE id=$1")
        .bind(id)
        .execute(pg.pool())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests;
