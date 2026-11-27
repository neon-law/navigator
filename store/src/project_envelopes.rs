//! Project-scoped provider observations, including envelopes without a Notation.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::types::Json;
use uuid::Uuid;

use crate::postgres::PostgresDb;

/// Envelope evidence returned by the Firm's configured DocuSign account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalEnvelope {
    pub envelope_id: Uuid,
    pub status: String,
    pub completed_date_time: Option<chrono::DateTime<chrono::FixedOffset>>,
    #[serde(default)]
    pub sent_date_time: Option<chrono::DateTime<chrono::FixedOffset>>,
    #[serde(default)]
    pub delivered_date_time: Option<chrono::DateTime<chrono::FixedOffset>>,
    #[serde(default)]
    pub status_changed_date_time: Option<chrono::DateTime<chrono::FixedOffset>>,
    #[serde(default)]
    pub envelope_documents: Vec<EnvelopeDocument>,
    #[serde(default)]
    pub signers: Vec<ExternalSigner>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeDocument {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalSigner {
    pub name: String,
    pub email: String,
    pub status: String,
    pub signed_date_time: Option<chrono::DateTime<chrono::FixedOffset>>,
    #[serde(default)]
    pub routing_order: Option<String>,
}

/// A later provider read supersedes an earlier read, including overlapping callbacks.
pub async fn observe(
    db: &PostgresDb,
    firm: Uuid,
    account: Uuid,
    project: Uuid,
    started_at: DateTime<Utc>,
    envelope: &ExternalEnvelope,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO project_envelope (firm_id, account_id, envelope_id, project_id, observed_at, evidence)
         SELECT $1,$2,$3,id,$5,$6 FROM project WHERE id=$4 AND firm_id=$1
         ON CONFLICT (firm_id, account_id, envelope_id) DO UPDATE SET
           observed_at=excluded.observed_at, evidence=excluded.evidence, project_id=excluded.project_id
         WHERE project_envelope.observed_at<=excluded.observed_at AND
           (project_envelope.project_id=excluded.project_id OR EXISTS (
             SELECT 1 FROM signature s JOIN notation n ON n.id=s.notation_id
             WHERE s.provider='docusign' AND s.provider_id=excluded.envelope_id::text
               AND n.project_id=excluded.project_id))",
    )
    .bind(firm).bind(account).bind(envelope.envelope_id).bind(project)
    .bind(started_at).bind(Json(envelope)).execute(db.pool()).await?;
    Ok(())
}

pub async fn for_project(
    db: &PostgresDb,
    project: Uuid,
) -> Result<Vec<ExternalEnvelope>, sqlx::Error> {
    let rows: Vec<Json<ExternalEnvelope>> = sqlx::query_scalar(
        "SELECT e.evidence FROM project_envelope e JOIN project p ON p.id=e.project_id AND p.firm_id=e.firm_id
         WHERE e.project_id=$1 AND NOT EXISTS (
           SELECT 1 FROM signature s JOIN notation n ON n.id=s.notation_id
           WHERE s.provider='docusign' AND s.provider_id=e.envelope_id::text AND n.project_id<>p.id)
         ORDER BY e.observed_at DESC, e.envelope_id",
    ).bind(project).fetch_all(db.pool()).await?;
    Ok(rows.into_iter().map(|row| row.0).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::postgres::fixtures::{firm_for_notation, workflow_fixture};

    #[tokio::test]
    async fn later_progress_survives_replay_and_stays_on_the_matched_project() {
        let Some((options, database)) = crate::postgres::fixtures::fresh_schema().await else {
            return;
        };
        let pg = crate::postgres::fixtures::connect(&options, &database).await;
        let (_, notation, _) = workflow_fixture(&pg).await;
        let (firm, project) = firm_for_notation(&pg, notation).await;
        let account = Uuid::new_v4();
        let id = Uuid::new_v4();
        let mut envelope: ExternalEnvelope = serde_json::from_value(serde_json::json!({
            "envelopeId": id, "status": "delivered", "signers": []
        }))
        .unwrap();
        let now = Utc::now();
        observe(&pg, firm, account, project, now, &envelope)
            .await
            .unwrap();
        envelope.status = "sent".into();
        observe(
            &pg,
            firm,
            account,
            project,
            now - chrono::Duration::seconds(10),
            &envelope,
        )
        .await
        .unwrap();
        let snapshots = for_project(&pg, project).await.unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].status, "delivered");
        let (_, other_notation, _) = workflow_fixture(&pg).await;
        let other = sqlx::query_scalar::<_, Uuid>(
            "UPDATE project SET firm_id=$1 WHERE id=(SELECT project_id FROM notation WHERE id=$2) RETURNING id",
        ).bind(firm).bind(other_notation).fetch_one(pg.pool()).await.unwrap();
        observe(&pg, firm, account, other, now, &envelope)
            .await
            .unwrap();
        assert!(for_project(&pg, other).await.unwrap().is_empty());
        sqlx::query(
            "INSERT INTO signature(id,notation_id,provider,provider_id,state,inserted_at,updated_at)
             VALUES($1,$2,'docusign',$3,'requested',now(),now())",
        ).bind(Uuid::new_v4()).bind(other_notation).bind(id.to_string()).execute(pg.pool()).await.unwrap();
        assert!(for_project(&pg, project).await.unwrap().is_empty());
        observe(&pg, firm, account, other, now, &envelope)
            .await
            .unwrap();
        assert_eq!(for_project(&pg, other).await.unwrap(), [envelope]);
        pg.pool().close().await;
    }
}
