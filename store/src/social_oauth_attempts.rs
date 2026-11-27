//! Short-lived, single-use OAuth handshakes for social account connections.

use crate::postgres::matter;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::firm_capability::{FirmCapability, FirmCapabilityDecision};
use crate::persons::Role;

const TABLE: &str = "social_oauth_attempt";
const LIFETIME: Duration = Duration::minutes(10);

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct SocialOAuthAttempt {
    pub state: String,
    pub pkce_verifier: String,
    pub firm_id: Uuid,
    pub brand_key: String,
    pub provider: String,
    pub provider_account_id: Option<String>,
    pub instagram_account_id: Option<String>,
    pub actor_person_id: Uuid,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum SocialOAuthAttemptError {
    #[error(transparent)]
    Postgres(#[from] matter::MatterAuthorityError),
    #[error("database: {0}")]
    Db(String),
    #[error("not allowed")]
    Denied,
    #[error("the Brand was not found")]
    BrandNotFound,
    #[error("unsupported social provider")]
    UnsupportedProvider,
    #[error("authorization attempt is missing, expired, or already used")]
    InvalidState,
}

/// Create a scoped PKCE attempt after rechecking Brand authorization.
pub async fn begin(
    db: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Uuid,
    brand_key: &str,
    provider: &str,
    now: DateTime<Utc>,
) -> Result<SocialOAuthAttempt, SocialOAuthAttemptError> {
    if !matches!(
        provider,
        "facebook" | "instagram" | "threads" | "youtube" | "linkedin" | "x" | "tiktok"
    ) {
        return Err(SocialOAuthAttemptError::UnsupportedProvider);
    }
    let brand = crate::brands::find_by_key(db, brand_key)
        .await
        .map_err(|error| SocialOAuthAttemptError::Db(error.to_string()))?
        .ok_or(SocialOAuthAttemptError::BrandNotFound)?;
    let firm_id = brand.firm_id.ok_or(SocialOAuthAttemptError::Denied)?;
    let decision = FirmCapability::ManageBrandContent
        .resolve(db, actor_role, Some(actor_person_id), firm_id)
        .await
        .map_err(|error| SocialOAuthAttemptError::Db(error.to_string()))?;
    if decision != FirmCapabilityDecision::Allowed {
        return Err(SocialOAuthAttemptError::Denied);
    }

    let pg = matter::authority_for(TABLE).await?;
    sqlx::query(
        "DELETE FROM social_oauth_attempt WHERE expires_at <= $1 OR consumed_at IS NOT NULL",
    )
    .bind(now)
    .execute(pg.pool())
    .await
    .map_err(matter::MatterAuthorityError::from)?;
    let state = random_token();
    let verifier = random_token();
    let expires_at = now + LIFETIME;
    let row = json!({"id": Uuid::now_v7(), "state_hash": state_hash(&state), "firm_id": firm_id,
        "brand_key": brand_key, "provider": provider, "actor_person_id": actor_person_id,
        "pkce_verifier": verifier, "expires_at": expires_at, "consumed_at": null, "inserted_at": now,
        "provider_account_id": null, "instagram_account_id": null});
    matter::upsert::<serde_json::Value>(&pg, TABLE, &row).await?;
    Ok(SocialOAuthAttempt {
        state,
        pkce_verifier: verifier,
        firm_id,
        brand_key: brand_key.to_owned(),
        provider: provider.to_owned(),
        provider_account_id: None,
        instagram_account_id: None,
        actor_person_id,
        expires_at,
    })
}

/// Bind an issued handshake to the operator's chosen provider account.
/// The random state is the capability; no callback can change this choice.
pub async fn bind_account(
    _db: &ApplicationStore,
    attempt: &SocialOAuthAttempt,
    provider_account_id: &str,
) -> Result<(), SocialOAuthAttemptError> {
    let pg = matter::authority_for(TABLE).await?;
    matter::update::<serde_json::Value>(
        &pg,
        TABLE,
        json!({"state_hash": state_hash(&attempt.state), "consumed_at": null}),
        json!({"provider_account_id": provider_account_id}),
    )
    .await?;
    Ok(())
}

/// Snapshot the explicitly selected Instagram destination on a Facebook handshake.
pub async fn bind_instagram(
    _db: &ApplicationStore,
    attempt: &SocialOAuthAttempt,
    identity: &str,
) -> Result<(), SocialOAuthAttemptError> {
    if attempt.provider != "facebook" {
        return Err(SocialOAuthAttemptError::UnsupportedProvider);
    }
    let pg = matter::authority_for(TABLE).await?;
    matter::update::<serde_json::Value>(
        &pg,
        TABLE,
        json!({"state_hash": state_hash(&attempt.state), "consumed_at": null}),
        json!({"instagram_account_id": identity}),
    )
    .await?;
    Ok(())
}

/// Atomically consume an unexpired state. The verifier is erased after it is
/// returned so callback replays cannot recover the exchange secret.
pub async fn consume(
    _db: &ApplicationStore,
    state: &str,
    now: DateTime<Utc>,
) -> Result<SocialOAuthAttempt, SocialOAuthAttemptError> {
    if state.len() != 43
        || !state
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(SocialOAuthAttemptError::InvalidState);
    }
    let pg = matter::authority_for(TABLE).await?;
    let value = matter::consume_oauth(&pg, &state_hash(state), now)
        .await?
        .ok_or(SocialOAuthAttemptError::InvalidState)?;
    let mut value = value;
    value["state"] = json!(state);
    serde_json::from_value(value)
        .map_err(matter::MatterAuthorityError::from)
        .map_err(SocialOAuthAttemptError::from)
}

fn random_token() -> String {
    let bytes: [u8; 32] = rand::random();
    URL_SAFE_NO_PAD.encode(bytes)
}

fn state_hash(state: &str) -> String {
    let digest = Sha256::digest(state.as_bytes());
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

#[cfg(test)]
mod tests {}
