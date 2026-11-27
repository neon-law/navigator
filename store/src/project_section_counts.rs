//! Content-free Project section counts for scoped CI gates.
use crate::application_store::ApplicationStore;
use serde::Serialize;
use uuid::Uuid;

/// Only cardinalities leave the store; no matter content is selected.
#[derive(Debug, Serialize)]
pub struct ProjectSectionCounts {
    pub docket: u64,
    pub deadlines: u64,
    pub documents: u64,
}

/// Count Docket Entries, open Deadlines and live Documents in one request.
/// Document revisions count exactly as the default Documents read door does.
pub async fn for_project(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<ProjectSectionCounts, String> {
    let pg = crate::postgres::PostgresDb::application()
        .await
        .map_err(|_| "PostgreSQL section counts unavailable")?;
    let (docket, deadlines, documents): (i64,i64,i64) = sqlx::query_as(
            r#"SELECT (SELECT count(*) FROM case_docket_entry d JOIN "case" c ON c.id=d.case_id WHERE c.project_id=$1),
             (SELECT count(*) FROM project_deadlines WHERE project_id=$1 AND status='open'),
             (SELECT count(*) FROM asset WHERE project_id=$1 AND deleted_at IS NULL)"#)
            .bind(project_id).fetch_one(pg.pool()).await.map_err(|_| "PostgreSQL section counts unavailable")?;
    let count = |value| u64::try_from(value).map_err(|_| "invalid section count".to_string());
    Ok(ProjectSectionCounts {
        docket: count(docket)?,
        deadlines: count(deadlines)?,
        documents: count(documents)?,
    })
}
