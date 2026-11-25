//! Atomic Lead identity and consent persistence.

use crate::leads::{Lead, LeadError, LeadIntakeDetails, NewConsentEvent, NewLead};
use uuid::Uuid;

/// Serialize the natural identity and commit consent with its current contact projection.
pub(crate) async fn record(
    pg: &crate::postgres::PostgresDb,
    new: &NewLead,
    event: Option<&NewConsentEvent>,
    intake: Option<&crate::leads::LeadIntake<'_>>,
) -> Result<Lead, LeadError> {
    use crate::postgres::matter::MatterAuthorityError as PgError;
    let email = new.email.trim();
    let email_lower = email.to_lowercase();
    let mut tx = pg.pool().begin().await.map_err(PgError::from)?;
    sqlx::query("SELECT pg_advisory_xact_lock(1312902738, hashtext($1::text))")
        .bind(serde_json::json!([email_lower, new.brand_key]).to_string())
        .execute(&mut *tx)
        .await
        .map_err(PgError::from)?;
    if let Some(answers) = intake {
        let prior: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT to_jsonb(r) FROM lead r JOIN lead_intake i ON i.lead_id=r.id
             WHERE i.id=$1 AND r.email_lower=$2 AND r.brand_key=$3",
        )
        .bind(&answers.submission_key)
        .bind(&email_lower)
        .bind(&new.brand_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(PgError::from)?;
        if let Some(row) = prior {
            return Ok(serde_json::from_value(row).map_err(PgError::from)?);
        }
    }
    let existing: Option<Uuid> = sqlx::query_scalar("SELECT id FROM lead WHERE email_lower=$1 AND brand_key=$2 ORDER BY updated_at DESC,id DESC LIMIT 1 FOR UPDATE")
        .bind(&email_lower).bind(&new.brand_key).fetch_optional(&mut *tx).await.map_err(PgError::from)?;
    let id = existing.unwrap_or_else(Uuid::now_v7);
    let row: serde_json::Value = if existing.is_some() {
        sqlx::query_scalar("UPDATE lead AS r SET email=$2,phone=$3,source_path=$4,submissions=submissions+1,updated_at=now() WHERE id=$1 RETURNING to_jsonb(r)")
            .bind(id).bind(email).bind(&new.phone).bind(&new.source_path)
            .fetch_one(&mut *tx).await.map_err(PgError::from)?
    } else {
        sqlx::query_scalar("INSERT INTO lead AS r (id,email,email_lower,phone,brand_key,source_path,consent_version,consented_at,status,submissions,inserted_at,updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'new',1,now(),now()) RETURNING to_jsonb(r)")
            .bind(id).bind(email).bind(&email_lower).bind(&new.phone).bind(&new.brand_key)
            .bind(&new.source_path).bind(&new.consent_version).bind(new.consented_at)
            .fetch_one(&mut *tx).await.map_err(PgError::from)?
    };
    if let Some(event) = event {
        sqlx::query("INSERT INTO lead_consent (id,lead_id,consented_at,phone,source_path,sms_consent_version,sms_policy_version,sms_opt_in,call_consent_version,call_opt_in) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
            .bind(Uuid::now_v7()).bind(id).bind(event.consented_at).bind(&event.phone)
            .bind(&event.source_path).bind(&event.sms_consent_version).bind(&event.sms_policy_version)
            .bind(event.sms_opt_in).bind(&event.call_consent_version).bind(event.call_opt_in)
            .execute(&mut *tx).await.map_err(PgError::from)?;
    }
    if let Some(answers) = intake {
        let (district, relationship, next_date, name) = match answers.details {
            LeadIntakeDetails::Iep {
                district,
                relationship,
                next_date,
            } => (Some(district), Some(relationship), next_date, None),
            LeadIntakeDetails::Contact { name } => (None, None, None, Some(name)),
        };
        sqlx::query("INSERT INTO lead_intake (id,lead_id,firm_id,school_district,relationship,concern,next_date,contact_name)
            VALUES ($1,$2,(SELECT firm_id FROM firm_brand WHERE brand_key=$3),$4,$5,$6,$7,$8)")
            .bind(&answers.submission_key).bind(id).bind(&new.brand_key)
            .bind(district).bind(relationship).bind(answers.concern).bind(next_date).bind(name)
            .execute(&mut *tx).await.map_err(PgError::from)?;
    }
    let lead = serde_json::from_value(row).map_err(PgError::from)?;
    tx.commit().await.map_err(PgError::from)?;
    Ok(lead)
}
