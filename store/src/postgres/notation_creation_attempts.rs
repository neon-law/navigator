//! Project-scoped history of failed Notation creation.

use super::{PostgresDb, PostgresError};
use uuid::Uuid;

/// Return the configured store, if the deployment has PostgreSQL.
pub async fn database() -> Result<Option<PostgresDb>, PostgresError> {
    if std::env::var_os("NAVIGATOR_POSTGRES_HOST").is_none() {
        return Ok(None);
    }
    PostgresDb::from_env().await.map(Some)
}

pub async fn record_failure(
    db: &PostgresDb,
    project_id: Uuid,
    template_code: &str,
    respondent_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO notation_creation_attempt (id, project_id, template_code, respondent_id)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(template_code)
    .bind(respondent_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn link_retry(
    db: &PostgresDb,
    project_id: Uuid,
    template_code: &str,
    respondent_id: Uuid,
    notation_id: Uuid,
    started_at: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE notation_creation_attempt SET notation_id = $4
         WHERE project_id = $1 AND template_code = $2 AND respondent_id = $3
           AND notation_id IS NULL AND failed_at <= $5",
    )
    .bind(project_id)
    .bind(template_code)
    .bind(respondent_id)
    .bind(notation_id)
    .bind(started_at)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn unresolved(
    db: &PostgresDb,
    project_id: Uuid,
) -> Result<Vec<(String, chrono::DateTime<chrono::Utc>)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT template_code, max(failed_at) FROM notation_creation_attempt
         WHERE project_id = $1 AND notation_id IS NULL GROUP BY template_code",
    )
    .bind(project_id)
    .fetch_all(db.pool())
    .await
}
