//! Manually recorded observations for the three relationship graph views.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::postgres::PostgresDb;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Project,
    Attorney,
    Firm,
}

impl Scope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Attorney => "attorney",
            Self::Firm => "firm",
        }
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Subject {
    Person,
    Entity,
}

impl Subject {
    fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Entity => "entity",
        }
    }
}

pub struct Append<'a> {
    pub request_id: Uuid,
    pub scope: Scope,
    pub firm_id: Uuid,
    pub project_id: Option<Uuid>,
    pub actor_id: Uuid,
    pub subject: Subject,
    pub subject_id: Uuid,
    pub action: &'a str,
    pub detail: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AppendResult {
    Recorded,
    ScopeOrSubjectUnavailable,
    RequestIdInUse,
}

/// Append a scoped observation. A retry with the same request id and payload
/// returns the original receipt. The Project's current owner is checked at write.
pub async fn append(pg: &PostgresDb, input: &Append<'_>) -> Result<AppendResult, sqlx::Error> {
    let mut tx = pg.0.begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO relationship_log \
            (id, graph_scope, firm_id, project_id, actor_person_id, subject_type, subject_id, action, detail) \
         SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9 \
         WHERE ($4::uuid IS NULL OR EXISTS (SELECT 1 FROM project WHERE id = $4 AND firm_id = $3)) \
           AND EXISTS (SELECT 1 FROM person_firm_role
                       WHERE firm_id = $3 AND person_id = $5 AND membership IN ('lawyer', 'admin')) \
           AND (($6 = 'person' AND EXISTS (SELECT 1 FROM person WHERE id = $7)) \
                OR ($6 = 'entity' AND EXISTS (SELECT 1 FROM entity WHERE id = $7))) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(input.request_id)
    .bind(input.scope.as_str())
    .bind(input.firm_id)
    .bind(input.project_id)
    .bind(input.actor_id)
    .bind(input.subject.as_str())
    .bind(input.subject_id)
    .bind(input.action)
    .bind(input.detail)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let result = if inserted == 1 {
        AppendResult::Recorded
    } else {
        let matches = sqlx::query_scalar::<_, bool>(
            "SELECT graph_scope = $2 AND firm_id = $3 AND project_id IS NOT DISTINCT FROM $4 \
                 AND actor_person_id = $5 AND subject_type = $6 AND subject_id = $7 \
                 AND action = $8 AND detail = $9 \
             FROM relationship_log WHERE id = $1 AND actor_person_id = $5 AND graph_scope IS NOT NULL",
        )
        .bind(input.request_id)
        .bind(input.scope.as_str())
        .bind(input.firm_id)
        .bind(input.project_id)
        .bind(input.actor_id)
        .bind(input.subject.as_str())
        .bind(input.subject_id)
        .bind(input.action)
        .bind(input.detail)
        .fetch_optional(&mut *tx)
        .await?;
        match matches {
            Some(true) => AppendResult::Recorded,
            Some(false) => AppendResult::RequestIdInUse,
            None => AppendResult::ScopeOrSubjectUnavailable,
        }
    };
    tx.commit().await?;
    Ok(result)
}

/// An observation attached to a graph seed. The free-form detail remains in
/// the audit entry. Only the scope and contact label are needed by the graph.
#[derive(Debug, Serialize)]
pub struct Observation {
    pub id: Uuid,
    pub source: Uuid,
    pub source_name: String,
    pub source_kind: String,
    pub subject_id: Uuid,
    pub subject_name: String,
    pub subject_kind: String,
    pub action: String,
}

/// Return recent observations for an authorized graph seed. A Project
/// observation also appears for its author and its owning Firm. A record
/// stays attributed to its original Firm after a Project moves.
pub async fn for_graph(
    pg: &PostgresDb,
    scope: Scope,
    seed: Uuid,
    viewer: Uuid,
) -> Result<Vec<Observation>, sqlx::Error> {
    let rows = sqlx::query_as(
        "SELECT log.id, $2 AS source, seed.name AS source_name, seed.kind AS source_kind, \
                log.subject_id, COALESCE(person.name, entity.name) AS subject_name, \
                CASE log.subject_type WHEN 'person' THEN 'Person' ELSE 'Entity' END AS subject_kind, \
                log.action \
         FROM relationship_log AS log \
         JOIN (SELECT id, name, 'Project' AS kind FROM project WHERE $1 = 'project' AND id = $2 \
               UNION ALL SELECT id, name, 'Person' FROM person WHERE $1 = 'attorney' AND id = $2 \
               UNION ALL SELECT id, name, 'Firm' FROM firm WHERE $1 = 'firm' AND id = $2) AS seed \
           ON seed.id = $2 \
         LEFT JOIN person ON log.subject_type = 'person' AND person.id = log.subject_id \
         LEFT JOIN entity ON log.subject_type = 'entity' AND entity.id = log.subject_id \
         LEFT JOIN project ON project.id = log.project_id \
         WHERE COALESCE(person.name, entity.name) IS NOT NULL \
           AND (log.project_id IS NULL OR project.firm_id = log.firm_id) \
           AND EXISTS (SELECT 1 FROM person_firm_role
                       WHERE person_id = $3 AND firm_id = log.firm_id AND membership IN ('lawyer', 'admin')) \
           AND (($1 = 'project' AND log.graph_scope = 'project' AND log.project_id = $2) \
             OR ($1 = 'attorney' AND $2 = $3 AND log.graph_scope IN ('project', 'attorney')
                 AND log.actor_person_id = $2) \
             OR ($1 = 'firm' AND log.graph_scope IN ('project', 'firm') AND log.firm_id = $2)) \
         ORDER BY log.inserted_at DESC, log.id DESC LIMIT 100",
    )
    .bind(scope.as_str())
    .bind(seed)
    .bind(viewer)
    .fetch_all(&pg.0)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                source,
                source_name,
                source_kind,
                subject_id,
                subject_name,
                subject_kind,
                action,
            )| Observation {
                id,
                source,
                source_name,
                source_kind,
                subject_id,
                subject_name,
                subject_kind,
                action,
            },
        )
        .collect())
}
