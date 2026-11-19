//! Atomic summary-sender approval decisions and audit events.

use crate::summary_sender_approvals::{ApprovalError, SummarySenderApproval};
use chrono::Utc;
use uuid::Uuid;

pub(crate) struct ApprovalAudit<'a> {
    pub(crate) id: Uuid,
    pub(crate) firm_id: Uuid,
    pub(crate) actor: Uuid,
    pub(crate) project_id: Option<Uuid>,
    pub(crate) account: &'a str,
    pub(crate) email: &'a str,
    pub(crate) person: Uuid,
    pub(crate) action: &'a str,
}

async fn audit_postgres(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    event: ApprovalAudit<'_>,
) -> Result<(), ApprovalError> {
    sqlx::query("INSERT INTO summary_sender_approval_event (id,firm_id,actor_person_id,project_id,provider_account,sender_email,person_id,action,inserted_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,now())")
        .bind(event.id).bind(event.firm_id).bind(event.actor).bind(event.project_id)
        .bind(event.account).bind(event.email).bind(event.person).bind(event.action)
        .execute(&mut **tx).await.map_err(crate::postgres::matter::MatterAuthorityError::from)?;
    Ok(())
}

pub(crate) async fn approve(
    pg: &crate::postgres::PostgresDb,
    actor: Uuid,
    firm: Uuid,
    project: Uuid,
    account: &str,
    email: &str,
    person: Uuid,
) -> Result<SummarySenderApproval, ApprovalError> {
    use crate::postgres::matter::{self, MatterAuthorityError as PgError};
    let mut tx = pg.pool().begin().await.map_err(PgError::from)?;
    sqlx::query("SELECT pg_advisory_xact_lock(1312902740, hashtext($1::text))")
        .bind(serde_json::json!([firm, project, account, email]).to_string())
        .execute(&mut *tx)
        .await
        .map_err(PgError::from)?;
    let locked: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM project WHERE id=$1 AND firm_id=$2 FOR SHARE")
            .bind(project)
            .bind(firm)
            .fetch_optional(&mut *tx)
            .await
            .map_err(PgError::from)?;
    if locked.is_none() {
        return Err(ApprovalError::SenderNotClientDri);
    }
    let valid: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM person p JOIN person_project_role r ON r.person_id=p.id WHERE p.id=$1 AND p.email_confirmed AND lower(p.email::text)=$2 AND r.project_id=$3 AND r.is_client_dri)")
        .bind(person).bind(email).bind(project).fetch_one(&mut *tx).await.map_err(PgError::from)?;
    if !valid {
        return Err(ApprovalError::SenderNotVerified);
    }
    let existing: Option<serde_json::Value> = sqlx::query_scalar("SELECT to_jsonb(r) FROM summary_sender_approval r WHERE firm_id=$1 AND project_id=$2 AND provider_account=$3 AND sender_email=$4 FOR UPDATE")
        .bind(firm).bind(project).bind(account).bind(email).fetch_optional(&mut *tx).await.map_err(PgError::from)?;
    let existing: Option<SummarySenderApproval> = existing
        .map(serde_json::from_value)
        .transpose()
        .map_err(PgError::from)?;
    if existing.as_ref().is_some_and(|row| row.person_id != person) {
        return Err(ApprovalError::SenderNotVerified);
    }
    let id = existing.as_ref().map_or_else(Uuid::now_v7, |row| row.id);
    let row = matter::upsert_on(
        &mut *tx,
        "summary_sender_approval",
        serde_json::json!({"id": id,"firm_id": firm,
        "project_id": project,"provider_account": account,"person_id": person,"sender_email": email,
        "status": "approved","updated_at": Utc::now()}),
    )
    .await?;
    audit_postgres(
        &mut tx,
        ApprovalAudit {
            id: Uuid::now_v7(),
            firm_id: firm,
            actor,
            project_id: Some(project),
            account,
            email,
            person,
            action: "approved",
        },
    )
    .await?;
    tx.commit().await.map_err(PgError::from)?;
    Ok(row)
}

pub(crate) async fn revoke(
    pg: &crate::postgres::PostgresDb,
    actor: Uuid,
    firm: Uuid,
    id: Uuid,
) -> Result<(), ApprovalError> {
    use crate::postgres::matter::MatterAuthorityError as PgError;
    let mut tx = pg.pool().begin().await.map_err(PgError::from)?;
    let previous: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(r) FROM summary_sender_approval r WHERE id=$1 AND firm_id=$2 FOR UPDATE",
    )
    .bind(id)
    .bind(firm)
    .fetch_optional(&mut *tx)
    .await
    .map_err(PgError::from)?;
    let Some(previous) = previous else {
        return Ok(());
    };
    let previous: SummarySenderApproval =
        serde_json::from_value(previous).map_err(PgError::from)?;
    if previous.status == "revoked" {
        return Ok(());
    }
    sqlx::query("UPDATE summary_sender_approval SET status='revoked',updated_at=now() WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(PgError::from)?;
    audit_postgres(
        &mut tx,
        ApprovalAudit {
            id: Uuid::now_v7(),
            firm_id: firm,
            actor,
            project_id: previous.project_id,
            account: &previous.provider_account,
            email: &previous.sender_email,
            person: previous.person_id,
            action: "revoked",
        },
    )
    .await?;
    tx.commit().await.map_err(PgError::from)?;
    Ok(())
}
