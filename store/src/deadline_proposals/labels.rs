//! Resolve the citations displayed under review, scoped to the same immutable file version.
use super::{Error, Proposal, Role};
use crate::postgres::PostgresDb;
use std::collections::HashMap;
use uuid::Uuid;

pub type SourceLabels = HashMap<(bool, Uuid, String), String>;

/// The original filename for a Project record or the canonical citation for a filed Authority.
/// A clerk's query never reads Authority records.
pub async fn source_labels(
    pg: &PostgresDb,
    project: Uuid,
    proposals: &[Proposal],
    can_read_authorities: bool,
) -> Result<SourceLabels, Error> {
    let mut documents = Vec::new();
    let mut authorities = Vec::new();
    for evidence in proposals.iter().flat_map(|p| &p.draft.evidence) {
        if evidence.role == Role::Authority {
            if can_read_authorities {
                authorities.push(evidence.id);
            }
        } else {
            documents.push(evidence.id);
        }
    }
    let mut labels = HashMap::new();
    if !documents.is_empty() {
        let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
            "SELECT id, sha256_hex, filename FROM asset
             WHERE project_id=$1 AND id=ANY($2) AND deleted_at IS NULL AND filename IS NOT NULL",
        )
        .bind(project)
        .bind(&documents)
        .fetch_all(pg.pool())
        .await?;
        for (id, digest, label) in rows {
            labels.insert((false, id, digest), label);
        }
    }
    if !authorities.is_empty() {
        let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
            "SELECT a.id, s.sha256_hex, a.citation FROM authority a
             JOIN asset s ON s.id=a.archived_asset_id
             WHERE a.id=ANY($1) AND s.project_id IS NULL AND s.deleted_at IS NULL",
        )
        .bind(&authorities)
        .fetch_all(pg.pool())
        .await?;
        for (id, digest, label) in rows {
            labels.insert((true, id, digest), label);
        }
    }
    Ok(labels)
}

/// Return the openable original and verified-pin availability for each listed revision.
pub async fn available_evidence(
    pg: &PostgresDb,
    project: Uuid,
    can_read_authorities: bool,
) -> Result<HashMap<(Uuid, i64, usize), bool>, Error> {
    let rows: Vec<(Uuid,i64,i32,bool)>=sqlx::query_as(
        "WITH latest AS (
            SELECT DISTINCT ON (id) id,revision,draft,source_receipts FROM deadline_proposal
            WHERE project_id=$1 ORDER BY id,revision DESC
         ), sources AS (
            SELECT p.id,p.revision,(e.n-1)::integer AS idx,e.source,
                   p.source_receipts->(e.n-1)::integer AS receipt
            FROM latest p CROSS JOIN LATERAL jsonb_array_elements(p.draft->'evidence')
                 WITH ORDINALITY AS e(source,n)
         )
         SELECT r.id,r.revision,r.idx,COALESCE(r.receipt->'forgejo'->>'repository_id' IS NOT NULL AND (r.receipt->'forgejo'->>'firm_id')::uuid=p.firm_id,false)
         FROM sources r JOIN project p ON p.id=$1
         LEFT JOIN authority a ON $2 AND r.source->>'role'='authority' AND a.id=(r.source->>'id')::uuid
         JOIN asset s ON s.id=COALESCE((r.receipt->>'asset_id')::uuid,
               CASE WHEN r.source->>'role'='authority' THEN a.archived_asset_id ELSE (r.source->>'id')::uuid END)
         WHERE (r.source->>'role'<>'authority' OR a.id IS NOT NULL)
           AND s.project_id IS NOT DISTINCT FROM (CASE WHEN r.source->>'role'<>'authority' THEN $1::uuid END)
           AND s.sha256_hex=r.source->>'sha256_hex' AND s.deleted_at IS NULL AND s.purged_at IS NULL AND s.byte_size>0",
    ).bind(project).bind(can_read_authorities).fetch_all(pg.pool()).await?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, revision, index, verified)| {
            Some(((id, revision, usize::try_from(index).ok()?), verified))
        })
        .collect())
}
