//! Content-free Project section counts for scoped CI gates.
use crate::surreal::{record_id, SurrealDb};
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
pub async fn for_project(db: &SurrealDb, project_id: Uuid) -> Result<ProjectSectionCounts, String> {
    if let Some(pg) = crate::postgres::PostgresDb::configured()
        .await
        .map_err(|_| "PostgreSQL section counts unavailable")?
    {
        let (docket, deadlines, documents): (i64,i64,i64) = sqlx::query_as(
            r#"SELECT (SELECT count(*) FROM case_docket_entry d JOIN "case" c ON c.id=d.case_id WHERE c.project_id=$1),
             (SELECT count(*) FROM project_deadlines WHERE project_id=$1 AND status='open'),
             (SELECT count(*) FROM asset WHERE project_id=$1 AND deleted_at IS NULL)"#)
            .bind(project_id).fetch_one(pg.pool()).await.map_err(|_| "PostgreSQL section counts unavailable")?;
        let count = |value| u64::try_from(value).map_err(|_| "invalid section count".to_string());
        return Ok(ProjectSectionCounts {
            docket: count(docket)?,
            deadlines: count(deadlines)?,
            documents: count(documents)?,
        });
    }
    let mut result = db.query(
        "SELECT VALUE count() FROM case_docket_entry WHERE case_id.project_id = $project GROUP ALL; \
         SELECT VALUE count() FROM project_deadlines WHERE project_id = $project AND status = 'open' GROUP ALL; \
         SELECT VALUE count() FROM asset WHERE project_id = $project AND deleted_at IS NONE GROUP ALL;"
    ).bind(("project", record_id(crate::projects::PROJECT_TABLE, project_id)))
     .await.map_err(|e| e.to_string())?.check().map_err(|e| e.to_string())?;
    Ok(ProjectSectionCounts {
        docket: count(&mut result, 0)?,
        deadlines: count(&mut result, 1)?,
        documents: count(&mut result, 2)?,
    })
}

fn count(result: &mut surrealdb::IndexedResults, index: usize) -> Result<u64, String> {
    let value: Option<i64> = result.take(index).map_err(|e| e.to_string())?;
    u64::try_from(value.unwrap_or(0)).map_err(|_| "invalid section count".into())
}
