//! Firm-scoped Brand call and SMS reporting through the Inbox authority seam.
use crate::{inbox::InboxError, postgres::PostgresDb, surreal::SurrealDb};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

/// Public reporting metadata; receiving-number credentials are never returned.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrandCommunications {
    pub brand_key: String,
    pub calls: usize,
    pub sms: usize,
    pub billed_tenths: i64,
    pub parked_calls: usize,
    pub registered_numbers: Vec<String>,
}

/// Admin reporting for a Firm-owned Brand. Route admission comes from embedded
/// Rego; the capability check binds the actual actor to this Firm's rows.
pub async fn for_brand(
    pg: &PostgresDb,
    db: &SurrealDb,
    actor: Uuid,
    brand_key: &str,
) -> Result<BrandCommunications, InboxError> {
    let person = crate::persons::find_by_id(db, actor)
        .await?
        .ok_or(InboxError::Denied)?;
    let brand = crate::brands::find_by_key(db, brand_key)
        .await
        .map_err(|error| InboxError::Db(error.to_string()))?
        .ok_or(InboxError::NotFound)?;
    let firm = crate::firms::firm_id_for_brand_key(db, &brand.key)
        .await
        .map_err(|error| InboxError::Db(error.to_string()))?
        .ok_or(InboxError::NotFound)?;
    if crate::firm_capability::resolve(
        db,
        person.role,
        Some(actor),
        firm,
        crate::firm_capability::FirmCapability::ViewCommunications,
    )
    .await
    .map_err(|error| InboxError::Db(error.to_string()))?
        != crate::firm_capability::FirmCapabilityDecision::Allowed
    {
        return Err(InboxError::Denied);
    }
    let mut report = BrandCommunications {
        brand_key: brand.key.clone(),
        ..Default::default()
    };
    let conversations = crate::inbox::list_intake(db, actor).await?;
    for conversation in conversations
        .into_iter()
        .filter(|c| c.firm_id == firm && c.brand_key.as_deref() == Some(brand.key.as_str()))
    {
        if !matches!(
            conversation.channel.as_str(),
            crate::inbox::channel::VOICE | crate::inbox::channel::SMS
        ) {
            continue;
        }
        let messages = crate::inbox::messages_for_reporting(db, conversation.id).await?;
        let count = messages
            .iter()
            .filter(|m| m.direction != crate::inbox::direction::INTERNAL)
            .count();
        if conversation.channel == crate::inbox::channel::SMS {
            report.sms += count;
        } else {
            report.calls += count;
            if conversation.project_id.is_none() {
                report.parked_calls += count;
            }
            let rows = sqlx::query(
                "SELECT tenths,project_id FROM billed_call WHERE conversation_id=$1 AND firm_id=$2",
            )
            .bind(conversation.id)
            .bind(firm)
            .fetch_all(pg.pool())
            .await
            .map_err(|error| InboxError::Db(error.to_string()))?;
            for row in rows {
                report.billed_tenths = report.billed_tenths.saturating_add(
                    row.try_get::<i64, _>("tenths")
                        .map_err(|error| InboxError::Db(error.to_string()))?,
                );
                if conversation.project_id.is_some()
                    && row
                        .try_get::<Option<Uuid>, _>("project_id")
                        .map_err(|error| InboxError::Db(error.to_string()))?
                        .is_none()
                {
                    report.parked_calls += 1;
                }
            }
        }
    }
    report.registered_numbers = sqlx::query_scalar(
        "SELECT number_e164 FROM brand_phone_number WHERE brand_key=$1 ORDER BY number_e164",
    )
    .bind(&brand.key)
    .fetch_all(pg.pool())
    .await
    .map_err(|error| InboxError::Db(error.to_string()))?;
    Ok(report)
}

/// Brands worn by Firms whose communications this admin may inspect. This
/// includes a system-wide presentation Brand attached through firm_brand.
pub async fn brand_keys_for_actor(db: &SurrealDb, actor: Uuid) -> Result<Vec<String>, InboxError> {
    let person = crate::persons::find_by_id(db, actor)
        .await?
        .ok_or(InboxError::Denied)?;
    let firms = crate::firm_capability::allowed_firm_ids(
        db,
        person.role,
        Some(actor),
        crate::firm_capability::FirmCapability::ViewCommunications,
    )
    .await
    .map_err(|error| InboxError::Db(error.to_string()))?;
    let mut keys = Vec::new();
    for firm in firms {
        keys.extend(
            crate::firms::brand_keys_for_firm(db, firm)
                .await
                .map_err(|error| InboxError::Db(error.to_string()))?,
        );
    }
    keys.sort();
    keys.dedup();
    Ok(keys)
}
