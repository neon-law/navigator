//! PostgreSQL persistence for validated Google Fi metadata.
use crate::inbox::{self, IngestInbound};
use chrono::DateTime;
pub use model::fi_import::*;
use serde::Serialize;
use uuid::Uuid;

pub trait IngestRow {
    fn ingest<'a>(&'a self, firm: Uuid, brand: &'a str, account: &'a str) -> IngestInbound<'a>;
}

impl IngestRow for Row {
    fn ingest<'a>(&'a self, firm: Uuid, brand: &'a str, account: &'a str) -> IngestInbound<'a> {
        IngestInbound {
            project_id: None,
            firm_id: firm,
            brand_key: Some(brand),
            interaction_type: None,
            provider_post_id: None,
            parent_provider_message_id: None,
            visibility: Some("private"),
            channel: match self.metadata.kind {
                Kind::Call => inbox::channel::VOICE,
                Kind::Sms => inbox::channel::SMS,
            },
            provider_account: account,
            provider_message_id: &self.provider_message_id,
            provider_thread_ref: None,
            external_address: &self.metadata.counterparty,
            external_name: None,
            subject: Some("Google Fi metadata"),
            body: &self.body,
            source_page: Some(account),
            source_channel: Some("google_fi"),
            matter_consent: false,
            promotional_opt_in: false,
            email_conversation_id: None,
            attachments: Vec::new(),
            occurred_at: &self.metadata.occurred_at,
            raw_storage_key: None,
        }
    }
}

/// Scope resolved by the authenticated import door, never by CSV columns.
#[derive(Clone, Debug)]
pub struct Scope {
    pub actor: Uuid,
    pub firm: Uuid,
    pub brand: String,
    pub account: String,
    pub project: Option<Uuid>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Existing {
    New,
    Identical,
    Conflict,
}

/// The same native authority gate used by #151's import writer.
pub async fn native_pool() -> Result<crate::postgres::PostgresDb, FiImportError> {
    crate::inbox::inbox_authority()
        .await
        .map_err(|_| FiImportError::Storage)
}

/// Read-only preflight. Apply verifies the persisted winner again because
/// another importer can commit between preview and submission.
pub async fn preview_on(
    pg: &crate::postgres::PostgresDb,
    scope: &Scope,
    row: &Row,
) -> Result<Existing, FiImportError> {
    // A native ID changing kind must not acquire a second channel-scoped ingress.
    let ids:Vec<Uuid>=sqlx::query_scalar("SELECT DISTINCT message_id FROM inbox_ingress WHERE firm_id=$1 AND provider_account=$2 AND provider_message_id=$3")
        .bind(scope.firm).bind(&scope.account).bind(&row.provider_message_id)
        .fetch_all(pg.pool()).await.map_err(|_|FiImportError::Storage)?;
    if ids.len() > 1 {
        return Ok(Existing::Conflict);
    }
    let existing = ids.first().copied();
    let Some(id) = existing else {
        return Ok(Existing::New);
    };
    Ok(if matches_existing(pg, scope, row, id).await? {
        Existing::Identical
    } else {
        Existing::Conflict
    })
}

async fn matches_existing(
    pg: &crate::postgres::PostgresDb,
    scope: &Scope,
    row: &Row,
    id: Uuid,
) -> Result<bool, FiImportError> {
    let message = crate::postgres::inbox::message_by_id_on(pg, id)
        .await
        .map_err(|_| FiImportError::Storage)?
        .ok_or(FiImportError::Storage)?;
    let conversation = crate::postgres::inbox::conversation_by_id_on(pg, message.conversation_id)
        .await
        .map_err(|_| FiImportError::Storage)?
        .ok_or(FiImportError::Storage)?;
    matches_facts(scope, row, &message, &conversation)
}

pub(crate) fn matches_facts(
    scope: &Scope,
    row: &Row,
    message: &inbox::Message,
    conversation: &inbox::Conversation,
) -> Result<bool, FiImportError> {
    let expected = row.ingest(scope.firm, &scope.brand, &scope.account);
    let original_time =
        DateTime::parse_from_rfc3339(&message.occurred_at).map_err(|_| FiImportError::Storage)?;
    let requested_time = DateTime::parse_from_rfc3339(&row.metadata.occurred_at)
        .map_err(|_| FiImportError::Storage)?;
    Ok(conversation.firm_id == scope.firm
        && conversation.brand_key.as_deref() == Some(scope.brand.as_str())
        && conversation.provider_account == scope.account
        && conversation.channel == expected.channel
        && scope
            .project
            .is_none_or(|project| conversation.project_id.is_none_or(|id| id == project))
        && message.provider_message_id.as_deref() == Some(row.provider_message_id.as_str())
        && message.body == row.body
        && original_time == requested_time
        && message.direction == row.metadata.direction.as_str()
        && message.author_person_id.is_none()
        && message.delivery_state == inbox::delivery::RECEIVED
        && message.raw_storage_key.is_none()
        && message.attachment_asset_id.is_none())
}

/// Fence Fi imports and assignments through destination verification. Try-locking
/// releases competing coordinator connections instead of exhausting the pool.
pub(crate) async fn lock_on(
    pg: &crate::postgres::PostgresDb,
) -> Result<sqlx::Transaction<'_, sqlx::Postgres>, FiImportError> {
    let mut guard = pg
        .pool()
        .begin()
        .await
        .map_err(|_| FiImportError::Storage)?;
    let acquired: bool = sqlx::query_scalar(
        "SELECT pg_try_advisory_xact_lock(hashtextextended('google_fi:import:v1',168))",
    )
    .fetch_one(&mut *guard)
    .await
    .map_err(|_| FiImportError::Storage)?;
    if !acquired {
        return Err(FiImportError::Busy);
    }
    Ok(guard)
}

/// Commit immutable history and its requested initial attribution atomically.
pub async fn ingest_on(
    pg: &crate::postgres::PostgresDb,
    scope: &Scope,
    row: &Row,
) -> Result<inbox::IngestedInbound, FiImportError> {
    let number = scope
        .account
        .strip_prefix(ACCOUNT_PREFIX)
        .ok_or(FiImportError::Mapping("Fi account namespace required"))?;
    if account(number)? != scope.account || !canonical(row) {
        return Err(FiImportError::Mapping("canonical Fi metadata required"));
    }
    let mut guard = lock_on(pg).await?;
    let saved = crate::postgres::inbox::ingest_fi_message_in(&mut guard, scope, row).await?;
    guard.commit().await.map_err(|_| FiImportError::Storage)?;
    Ok(saved)
}

#[cfg(test)]
#[path = "fi_import_tests.rs"]
mod tests;
