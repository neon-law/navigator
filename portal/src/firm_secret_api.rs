//! The CLI manages Stripe credentials through the same Firm capability and KMS as settings.
use std::sync::Arc;

use crate::api::{AdminSession, ApiError, ApiState};
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use model::stripe_keys::{Account, Directory, KeyKind, KeyMetadata, Mode};
use store::firm_capability::{FirmCapability, FirmCapabilityDecision};
use store::firm_secrets::{
    self, IntegrationProvider::Stripe, IntegrationSecretKind, SecretPutRequest, SecretStoreError,
};
use uuid::Uuid;

impl From<SecretStoreError> for ApiError {
    fn from(error: SecretStoreError) -> Self {
        match error {
            SecretStoreError::NotAuthorized | SecretStoreError::NotConfigured { .. } => {
                ApiError::NotFound
            }
            SecretStoreError::InvalidStripeKey => ApiError::MalformedBody(
                "Stripe key does not match its kind or deployment mode".into(),
            ),
            _ => ApiError::Db("Firm credential storage unavailable".into()),
        }
    }
}

pub(crate) async fn list(
    State(state): State<ApiState>,
    AdminSession(session): AdminSession,
    Path(id): Path<Uuid>,
) -> Result<Json<Directory>, ApiError> {
    if session.scope.is_some() {
        return Err(ApiError::Forbidden);
    }
    if FirmCapability::ManageIntegrationSecrets
        .resolve(
            &state.application_store,
            session.role,
            session.person_id,
            id,
        )
        .await
        .map_err(|_| ApiError::Db("Firm capability unavailable".into()))?
        != FirmCapabilityDecision::Allowed
    {
        return Err(ApiError::NotFound);
    }
    let rows = firm_secrets::list_metadata_for_firm(
        &state.application_store,
        session.role,
        session.person_id,
        id,
    )
    .await?;
    let mut keys = Vec::new();
    for row in rows {
        if let Some((account, kind)) = row.kind.stripe_slot() {
            let mode = row
                .stripe_mode
                .as_deref()
                .map(str::parse)
                .transpose()
                .map_err(|_| ApiError::Db("Invalid stored Stripe mode".into()))?;
            keys.push(KeyMetadata {
                account,
                kind,
                mode,
                updated_at: row.updated_at,
            });
        }
    }
    Ok(Json(Directory {
        mode: firm_secrets::stripe_mode()?,
        keys,
    }))
}

fn check_mode(mode: Mode) -> Result<(), ApiError> {
    if firm_secrets::stripe_mode()? != mode {
        return Err(ApiError::MalformedBody(
            "Stripe mode does not match the deployment".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn set(
    State(state): State<ApiState>,
    AdminSession(session): AdminSession,
    Path((id, account, kind, mode)): Path<(Uuid, Account, KeyKind, Mode)>,
    Extension(kms): Extension<Arc<dyn cloud::RuntimeKms>>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    if session.scope.is_some() {
        return Err(ApiError::Forbidden);
    }
    check_mode(mode)?;
    let value = std::str::from_utf8(&body)
        .ok()
        .filter(|value| mode.accepts(kind, value))
        .ok_or_else(|| ApiError::from(SecretStoreError::InvalidStripeKey))?;
    firm_secrets::put(
        &state.application_store,
        SecretPutRequest {
            firm_id: id,
            provider: Stripe,
            kind: IntegrationSecretKind::stripe(account, kind),
            actor_role: session.role,
            actor_person_id: session.person_id,
            value,
        },
        kms.as_ref(),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn delete(
    State(state): State<ApiState>,
    AdminSession(session): AdminSession,
    Path((id, account, kind, mode)): Path<(Uuid, Account, KeyKind, Mode)>,
) -> Result<StatusCode, ApiError> {
    if session.scope.is_some() {
        return Err(ApiError::Forbidden);
    }
    check_mode(mode)?;
    firm_secrets::revoke_stripe(
        &state.application_store,
        session.role,
        session.person_id,
        id,
        account,
        kind,
        mode,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
