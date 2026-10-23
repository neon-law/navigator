//! Receipt-backed Project costs stored relationally in PostgreSQL.
//!
//! A cost records a third-party charge and its receipt. It never changes a
//! Xero invoice; a later invoice is linked by its existing Xero identifier.
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Row, Transaction};
use uuid::Uuid;

use crate::{postgres::PostgresDb, surreal::SurrealDb};

#[derive(Debug, thiserror::Error)]
pub enum ProjectCostError {
    #[error("invalid Project cost: {0}")]
    Invalid(&'static str),
    #[error("receipt is already recorded for this Project")]
    ReceiptTaken,
    #[error("idempotency key has different content")]
    Conflict,
    #[error("Project cost was not found")]
    NotFound,
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
    #[error("Project lookup failed")]
    Project(#[from] crate::projects::ProjectStoreError),
    #[error("Document lookup failed")]
    Asset(#[from] crate::assets::AssetError),
    #[error("invoice lookup failed")]
    Invoice(#[from] Box<crate::xero_invoices::XeroInvoiceError>),
    #[error("PostgreSQL is unavailable")]
    Postgres(#[from] crate::postgres::PostgresError),
    #[error("Project cost parent could not be mirrored to PostgreSQL")]
    Mirror(#[from] crate::postgres_mirror::MirrorError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectCost {
    pub id: Uuid,
    pub project_id: Uuid,
    pub occurred_on: NaiveDate,
    pub payee: String,
    pub description: String,
    pub amount_cents: i64,
    pub currency: String,
    pub amount_status: String,
    pub billing_status: String,
    pub receipt_document_id: Uuid,
    pub xero_invoice_id: Option<String>,
    pub inserted_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectCostEvent {
    pub id: Uuid,
    pub actor_person_id: Uuid,
    pub event_kind: String,
    pub before_state: Option<serde_json::Value>,
    pub after_state: serde_json::Value,
    pub occurred_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewProjectCost {
    pub occurred_on: NaiveDate,
    pub payee: String,
    pub description: String,
    pub amount_cents: i64,
    pub currency: String,
    pub amount_status: String,
    pub receipt_document_id: Uuid,
    pub idempotency_key: String,
}

fn from_row(row: &sqlx::postgres::PgRow) -> Result<ProjectCost, sqlx::Error> {
    Ok(ProjectCost {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        occurred_on: row.try_get("occurred_on")?,
        payee: row.try_get("payee")?,
        description: row.try_get("description")?,
        amount_cents: row.try_get("amount_cents")?,
        currency: row.try_get("currency")?,
        amount_status: row.try_get("amount_status")?,
        billing_status: row.try_get("billing_status")?,
        receipt_document_id: row.try_get("receipt_document_id")?,
        xero_invoice_id: row.try_get("xero_invoice_id")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

async fn set_audit_context(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    actor_id: Uuid,
    kind: &str,
    detail: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT set_config('navigator.project_cost_actor',$1,true), set_config('navigator.project_cost_event_kind',$2,true), set_config('navigator.project_cost_event_detail',$3,true)")
        .bind(actor_id.to_string())
        .bind(kind)
        .bind(detail.unwrap_or(""))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn validate(input: &NewProjectCost) -> Result<(), ProjectCostError> {
    if input.payee.trim().is_empty() || input.payee.trim().len() > 300 {
        return Err(ProjectCostError::Invalid("payee"));
    }
    if input.description.trim().is_empty() || input.description.trim().len() > 2000 {
        return Err(ProjectCostError::Invalid("description"));
    }
    if input.amount_cents <= 0 {
        return Err(ProjectCostError::Invalid("amount"));
    }
    if input.currency.len() != 3 || !input.currency.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(ProjectCostError::Invalid("currency"));
    }
    if !matches!(input.amount_status.as_str(), "estimated" | "confirmed") {
        return Err(ProjectCostError::Invalid("amount status"));
    }
    if input.idempotency_key.trim().is_empty() || input.idempotency_key.len() > 200 {
        return Err(ProjectCostError::Invalid("idempotency key"));
    }
    Ok(())
}

/// Add one Project cost. A repeated identical request returns its original id.
pub async fn create(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    project_id: Uuid,
    actor_id: Uuid,
    input: &NewProjectCost,
) -> Result<ProjectCost, ProjectCostError> {
    validate(input)?;
    let project = crate::projects::find_by_id(surreal, project_id)
        .await?
        .ok_or(ProjectCostError::Invalid("Project not found"))?;
    let asset = crate::assets::find_by_id(surreal, input.receipt_document_id)
        .await?
        .ok_or(ProjectCostError::Invalid("receipt Document not found"))?;
    if asset.project_id != Some(project_id)
        || asset.kind.is_none()
        || asset.deleted_at.is_some()
        || asset.purged_at.is_some()
    {
        return Err(ProjectCostError::Invalid(
            "receipt must be a live Document on this Project",
        ));
    }
    if let Some(firm_id) = project.firm_id {
        crate::postgres_mirror::mirror(
            surreal,
            pg,
            &crate::postgres_mirror::MirrorKey::Firm(firm_id),
        )
        .await?;
    }
    if let Some(jurisdiction_id) = project.jurisdiction_id {
        crate::postgres_mirror::mirror(
            surreal,
            pg,
            &crate::postgres_mirror::MirrorKey::Jurisdiction(jurisdiction_id),
        )
        .await?;
    }
    for key in [
        crate::postgres_mirror::MirrorKey::Project(project_id),
        crate::postgres_mirror::MirrorKey::Person(actor_id),
        crate::postgres_mirror::MirrorKey::Asset(input.receipt_document_id),
    ] {
        crate::postgres_mirror::mirror(surreal, pg, &key).await?;
    }
    create_postgres(pg, project_id, actor_id, input).await
}

async fn create_postgres(
    pg: &PostgresDb,
    project_id: Uuid,
    actor_id: Uuid,
    input: &NewProjectCost,
) -> Result<ProjectCost, ProjectCostError> {
    validate(input)?;
    let id = Uuid::new_v4();
    let now = Utc::now();
    let desired = serde_json::json!({
        "project_id": project_id, "occurred_on": input.occurred_on,
        "payee": input.payee.trim(), "description": input.description.trim(),
        "amount_cents": input.amount_cents, "currency": input.currency,
        "amount_status": input.amount_status, "billing_status": "unbilled",
        "receipt_document_id": input.receipt_document_id, "xero_invoice_id": null
    });
    let mut tx = pg.0.begin().await?;
    sqlx::query("SELECT id FROM project WHERE id=$1 FOR UPDATE")
        .bind(project_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ProjectCostError::Invalid(
            "Project is not mirrored to PostgreSQL",
        ))?;
    let existing = sqlx::query("SELECT id,project_id,occurred_on,payee,description,amount_cents,currency,amount_status,billing_status,receipt_document_id,xero_invoice_id,inserted_at,updated_at FROM project_costs WHERE project_id=$1 AND idempotency_key=$2 FOR UPDATE")
        .bind(project_id).bind(input.idempotency_key.trim()).fetch_optional(&mut *tx).await?.as_ref().map(from_row).transpose()?;
    if let Some(cost) = existing {
        let payload: Option<(sqlx::types::Json<serde_json::Value>,)> = sqlx::query_as("SELECT after_state->'request' FROM project_cost_events WHERE project_cost_id=$1 AND event_kind='created' ORDER BY occurred_at,id LIMIT 1")
            .bind(cost.id).fetch_optional(&mut *tx).await?;
        return if payload.is_some_and(|(stored,)| stored.0 == desired) {
            Ok(cost)
        } else {
            Err(ProjectCostError::Conflict)
        };
    }
    set_audit_context(&mut tx, actor_id, "created", None).await?;
    let result = sqlx::query("INSERT INTO project_costs(id,project_id,occurred_on,payee,description,amount_cents,currency,amount_status,billing_status,receipt_document_id,idempotency_key,idempotency_payload,inserted_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'unbilled',$9,$10,$11,$12,$12)")
        .bind(id).bind(project_id).bind(input.occurred_on).bind(input.payee.trim()).bind(input.description.trim()).bind(input.amount_cents).bind(&input.currency).bind(&input.amount_status).bind(input.receipt_document_id).bind(input.idempotency_key.trim()).bind(sqlx::types::Json(desired.clone())).bind(now).execute(&mut *tx).await;
    if let Err(error) = result {
        if error.as_database_error().is_some_and(|e| {
            e.constraint() == Some("project_costs_project_id_receipt_document_id_key")
        }) {
            return Err(ProjectCostError::ReceiptTaken);
        }
        return Err(ProjectCostError::Database(error));
    }
    tx.commit().await?;
    fetch(pg, id).await
}

/// Confirm a pending amount. The prior amount and state remain in the event log.
pub async fn confirm_amount(
    pg: &PostgresDb,
    id: Uuid,
    actor_id: Uuid,
    amount_cents: i64,
) -> Result<ProjectCost, ProjectCostError> {
    if amount_cents <= 0 {
        return Err(ProjectCostError::Invalid("amount"));
    }
    let now = Utc::now();
    let mut tx = pg.0.begin().await?;
    let before = sqlx::query("SELECT id,project_id,occurred_on,payee,description,amount_cents,currency,amount_status,billing_status,receipt_document_id,xero_invoice_id,inserted_at,updated_at FROM project_costs WHERE id=$1 FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await?.as_ref().map(from_row).transpose()?.ok_or(ProjectCostError::NotFound)?;
    if before.amount_status != "estimated" {
        return Err(ProjectCostError::Invalid(
            "only an estimated cost can be confirmed",
        ));
    }
    set_audit_context(&mut tx, actor_id, "amount_confirmed", None).await?;
    sqlx::query("UPDATE project_costs SET amount_cents=$2,amount_status='confirmed',updated_at=$3 WHERE id=$1")
        .bind(id).bind(amount_cents).bind(now).execute(&mut *tx).await?;
    tx.commit().await?;
    fetch(pg, id).await
}

/// Link the cost to a later invoice already present in the Xero mirror.
pub async fn link_invoice(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
    actor_id: Uuid,
    invoice_id: &str,
) -> Result<ProjectCost, ProjectCostError> {
    let mut tx = pg.0.begin().await?;
    let before = sqlx::query("SELECT id,project_id,occurred_on,payee,description,amount_cents,currency,amount_status,billing_status,receipt_document_id,xero_invoice_id,inserted_at,updated_at FROM project_costs WHERE id=$1 FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await?.as_ref().map(from_row).transpose()?.ok_or(ProjectCostError::NotFound)?;
    let invoice = crate::xero_invoices::find_for_allocation(surreal, invoice_id)
        .await
        .map_err(Box::new)?
        .ok_or(ProjectCostError::Invalid("Xero invoice not found"))?;
    if invoice.project_id != before.project_id {
        return Err(ProjectCostError::Invalid(
            "invoice belongs to another Project",
        ));
    }
    if invoice.currency != before.currency {
        return Err(ProjectCostError::Invalid(
            "invoice currency does not match the cost currency",
        ));
    }
    if invoice.status == "VOIDED" {
        return Err(ProjectCostError::Invalid("invoice is voided"));
    }
    if invoice.issued_at.date_naive() < before.occurred_on {
        return Err(ProjectCostError::Invalid(
            "cost cannot be linked to an earlier invoice",
        ));
    }
    if before.billing_status != "unbilled" {
        return Err(ProjectCostError::Invalid("cost is already resolved"));
    }
    let now = Utc::now();
    set_audit_context(&mut tx, actor_id, "invoice_linked", None).await?;
    sqlx::query("UPDATE project_costs SET billing_status='invoiced',xero_invoice_id=$2,updated_at=$3 WHERE id=$1")
        .bind(id).bind(invoice_id).bind(now).execute(&mut *tx).await?;
    tx.commit().await?;
    fetch(pg, id).await
}

/// Resolve an unbilled cost without attaching it to a Xero invoice, retaining the reason.
pub async fn resolve(
    pg: &PostgresDb,
    id: Uuid,
    actor_id: Uuid,
    reason: &str,
) -> Result<ProjectCost, ProjectCostError> {
    if reason.trim().is_empty() || reason.trim().len() > 1000 {
        return Err(ProjectCostError::Invalid("resolution reason"));
    }
    let now = Utc::now();
    let mut tx = pg.0.begin().await?;
    let before = sqlx::query("SELECT id,project_id,occurred_on,payee,description,amount_cents,currency,amount_status,billing_status,receipt_document_id,xero_invoice_id,inserted_at,updated_at FROM project_costs WHERE id=$1 FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await?.as_ref().map(from_row).transpose()?.ok_or(ProjectCostError::NotFound)?;
    if before.billing_status != "unbilled" {
        return Err(ProjectCostError::Invalid("cost is already resolved"));
    }
    set_audit_context(&mut tx, actor_id, "resolved", Some(reason.trim())).await?;
    sqlx::query("UPDATE project_costs SET billing_status='resolved',updated_at=$2 WHERE id=$1")
        .bind(id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    fetch(pg, id).await
}

pub async fn list_for_project(
    pg: &PostgresDb,
    project_id: Uuid,
) -> Result<Vec<ProjectCost>, ProjectCostError> {
    sqlx::query("SELECT id,project_id,occurred_on,payee,description,amount_cents,currency,amount_status,billing_status,receipt_document_id,xero_invoice_id,inserted_at,updated_at FROM project_costs WHERE project_id=$1 ORDER BY occurred_on DESC,id")
        .bind(project_id).fetch_all(&pg.0).await?.iter().map(from_row).collect::<Result<_,_>>().map_err(Into::into)
}

pub async fn find_by_id(pg: &PostgresDb, id: Uuid) -> Result<ProjectCost, ProjectCostError> {
    fetch(pg, id).await
}

/// Read the immutable audit history of one Project cost.
pub async fn list_events(
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Vec<ProjectCostEvent>, ProjectCostError> {
    let rows = sqlx::query("SELECT id,actor_person_id,event_kind,before_state,after_state,occurred_at FROM project_cost_events WHERE project_cost_id=$1 ORDER BY occurred_at,id")
        .bind(id)
        .fetch_all(&pg.0)
        .await?;
    rows.into_iter()
        .map(|row| {
            Ok(ProjectCostEvent {
                id: row.try_get("id")?,
                actor_person_id: row.try_get("actor_person_id")?,
                event_kind: row.try_get("event_kind")?,
                before_state: row
                    .try_get::<Option<sqlx::types::Json<serde_json::Value>>, _>("before_state")?
                    .map(|json| json.0),
                after_state: row
                    .try_get::<sqlx::types::Json<serde_json::Value>, _>("after_state")?
                    .0,
                occurred_at: row.try_get("occurred_at")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(Into::into)
}

/// Client lens: keep a cost visible only while its receipt Document is live and client-visible.
pub async fn list_client_visible(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    project_id: Uuid,
) -> Result<Vec<ProjectCost>, ProjectCostError> {
    let rows = list_for_project(pg, project_id).await?;
    let mut visible = Vec::new();
    for row in rows {
        if let Some(asset) = crate::assets::find_by_id(surreal, row.receipt_document_id).await? {
            if asset.project_id == Some(project_id)
                && asset.kind.is_some()
                && asset.visibility == crate::documents::visibility::CLIENT
                && asset.deleted_at.is_none()
                && asset.purged_at.is_none()
            {
                visible.push(row);
            }
        }
    }
    Ok(visible)
}

async fn fetch(pg: &PostgresDb, id: Uuid) -> Result<ProjectCost, ProjectCostError> {
    sqlx::query("SELECT id,project_id,occurred_on,payee,description,amount_cents,currency,amount_status,billing_status,receipt_document_id,xero_invoice_id,inserted_at,updated_at FROM project_costs WHERE id=$1")
        .bind(id).fetch_optional(&pg.0).await?.as_ref().map(from_row).transpose()?.ok_or(ProjectCostError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::{validate, NewProjectCost, ProjectCostError};
    use chrono::NaiveDate;
    use uuid::Uuid;

    fn input() -> NewProjectCost {
        NewProjectCost {
            occurred_on: NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
            payee: "Synthetic Vendor".into(),
            description: "Filing copy".into(),
            amount_cents: 1250,
            currency: "USD".into(),
            amount_status: "estimated".into(),
            receipt_document_id: Uuid::now_v7(),
            idempotency_key: "receipt:one".into(),
        }
    }
    #[test]
    fn accepts_a_receipt_backed_estimate() {
        assert!(validate(&input()).is_ok());
    }
    #[test]
    fn refuses_invalid_amount_currency_and_status() {
        let mut value = input();
        value.amount_cents = 0;
        assert!(matches!(
            validate(&value),
            Err(ProjectCostError::Invalid("amount"))
        ));
        let mut value = input();
        value.currency = "US".into();
        assert!(matches!(
            validate(&value),
            Err(ProjectCostError::Invalid("currency"))
        ));
        let mut value = input();
        value.amount_status = "paid".into();
        assert!(matches!(
            validate(&value),
            Err(ProjectCostError::Invalid("amount status"))
        ));
    }
}
