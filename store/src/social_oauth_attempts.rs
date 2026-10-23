//! Short-lived, single-use OAuth handshakes for social account connections.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::firm_capability::{FirmCapability, FirmCapabilityDecision};
use crate::persons::Role;
use crate::surreal::{record_id, record_uuid, SurrealDb};

const TABLE: &str = "social_oauth_attempt";
const LIFETIME: Duration = Duration::minutes(10);

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(SurrealValue)]
struct AttemptRow {
    id: surrealdb::types::RecordId,
    firm_id: surrealdb::types::RecordId,
    brand_key: String,
    provider: String,
    provider_account_id: Option<String>,
    instagram_account_id: Option<String>,
    actor_person_id: surrealdb::types::RecordId,
    pkce_verifier: String,
    expires_at: surrealdb::types::Datetime,
}

impl AttemptRow {
    fn into_attempt(self, state: String) -> Option<SocialOAuthAttempt> {
        Some(SocialOAuthAttempt {
            state,
            pkce_verifier: self.pkce_verifier,
            firm_id: record_uuid(&self.firm_id)?,
            brand_key: self.brand_key,
            provider: self.provider,
            provider_account_id: self.provider_account_id,
            instagram_account_id: self.instagram_account_id,
            actor_person_id: record_uuid(&self.actor_person_id)?,
            expires_at: self.expires_at.into(),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SocialOAuthAttemptError {
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
    db: &SurrealDb,
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

    db.query("DELETE social_oauth_attempt WHERE expires_at <= $now OR consumed_at IS NOT NONE")
        .bind(("now", surrealdb::types::Datetime::from(now)))
        .await
        .map_err(|error| SocialOAuthAttemptError::Db(error.to_string()))?
        .check()
        .map_err(|error| SocialOAuthAttemptError::Db(error.to_string()))?;

    let state = random_token();
    let pkce_verifier = random_token();
    let expires_at = now + LIFETIME;
    let id = Uuid::now_v7();
    db.query("CREATE $id SET state_hash = $state_hash, firm_id = $firm, brand_key = $brand, provider = $provider, actor_person_id = $actor, pkce_verifier = $verifier, expires_at = $expires_at, consumed_at = NONE, inserted_at = $now")
        .bind(("id", record_id(TABLE, id)))
        .bind(("state_hash", state_hash(&state)))
        .bind(("firm", record_id("firm", firm_id)))
        .bind(("brand", brand_key.to_string()))
        .bind(("provider", provider.to_string()))
        .bind(("actor", record_id("person", actor_person_id)))
        .bind(("verifier", pkce_verifier.clone()))
        .bind(("expires_at", surrealdb::types::Datetime::from(expires_at)))
        .bind(("now", surrealdb::types::Datetime::from(now)))
        .await
        .map_err(|error| SocialOAuthAttemptError::Db(error.to_string()))?
        .check()
        .map_err(|error| SocialOAuthAttemptError::Db(error.to_string()))?;

    Ok(SocialOAuthAttempt {
        state,
        pkce_verifier,
        firm_id,
        brand_key: brand_key.to_string(),
        provider: provider.to_string(),
        provider_account_id: None,
        instagram_account_id: None,
        actor_person_id,
        expires_at,
    })
}

/// Bind an issued handshake to the operator's chosen provider account.
/// The random state is the capability; no callback can change this choice.
pub async fn bind_account(
    db: &SurrealDb,
    attempt: &SocialOAuthAttempt,
    provider_account_id: &str,
) -> Result<(), SocialOAuthAttemptError> {
    db.query("UPDATE social_oauth_attempt SET provider_account_id = $account WHERE state_hash = $hash AND consumed_at IS NONE")
        .bind(("hash", state_hash(&attempt.state)))
        .bind(("account", provider_account_id.to_string()))
        .await
        .map_err(|error| SocialOAuthAttemptError::Db(error.to_string()))?
        .check()
        .map_err(|error| SocialOAuthAttemptError::Db(error.to_string()))?;
    Ok(())
}

/// Snapshot the explicitly selected Instagram destination on a Facebook handshake.
pub async fn bind_instagram(
    db: &SurrealDb,
    attempt: &SocialOAuthAttempt,
    identity: &str,
) -> Result<(), SocialOAuthAttemptError> {
    if attempt.provider != "facebook" {
        return Err(SocialOAuthAttemptError::UnsupportedProvider);
    }
    db.query("UPDATE social_oauth_attempt SET instagram_account_id = $identity WHERE state_hash = $hash AND consumed_at IS NONE")
        .bind(("identity", identity.to_string())).bind(("hash", state_hash(&attempt.state))).await?.check()?;
    Ok(())
}

/// Atomically consume an unexpired state. The verifier is erased after it is
/// returned so callback replays cannot recover the exchange secret.
pub async fn consume(
    db: &SurrealDb,
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
    let state_hash = state_hash(state);
    let mut response = db
        .query("UPDATE social_oauth_attempt SET consumed_at = $now WHERE state_hash = $state_hash AND consumed_at IS NONE AND expires_at > $now RETURN id, firm_id, brand_key, provider, actor_person_id, pkce_verifier, provider_account_id, instagram_account_id, expires_at")
        .bind(("now", surrealdb::types::Datetime::from(now)))
        .bind(("state_hash", state_hash.clone()))
        .await
        .map_err(|error| SocialOAuthAttemptError::Db(error.to_string()))?
        .check()
        .map_err(|error| SocialOAuthAttemptError::Db(error.to_string()))?;
    let row: Option<AttemptRow> = response
        .take(0)
        .map_err(|error| SocialOAuthAttemptError::Db(error.to_string()))?;
    let attempt = row
        .and_then(|row| row.into_attempt(state.to_string()))
        .ok_or(SocialOAuthAttemptError::InvalidState)?;
    db.query("UPDATE social_oauth_attempt SET pkce_verifier = '', provider_account_id = NONE WHERE state_hash = $state_hash")
        .bind(("state_hash", state_hash))
        .await
        .map_err(|error| SocialOAuthAttemptError::Db(error.to_string()))?
        .check()
        .map_err(|error| SocialOAuthAttemptError::Db(error.to_string()))?;
    Ok(attempt)
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

impl From<surrealdb::Error> for SocialOAuthAttemptError {
    fn from(error: surrealdb::Error) -> Self {
        Self::Db(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{begin, consume, SocialOAuthAttemptError};
    use crate::{brands::NewBrand, firms::NewFirm, persons::NewPerson, persons::Role};
    use chrono::{Duration, Utc};
    use uuid::Uuid;

    async fn practice(db: &crate::surreal::SurrealDb, name: &str) -> (Uuid, Uuid, String) {
        let admin = crate::persons::create(
            db,
            &NewPerson::with_role(
                format!("{name} Admin"),
                format!("{name}-{}@example.com", Uuid::now_v7()),
                Role::Admin,
            ),
        )
        .await
        .unwrap();
        let firm = crate::firms::create(
            db,
            &NewFirm {
                name: format!("{name} Firm"),
                status: "active".into(),
                entity_id: crate::test_support::seed_entity(db).await,
                admin_dri_person_id: admin.id,
            },
        )
        .await
        .unwrap();
        let brand_key = format!("{name}-brand");
        crate::brands::create(
            db,
            Role::Admin,
            Some(admin.id),
            &NewBrand {
                name: format!("{name} Brand"),
                key: brand_key.clone(),
                firm_id: Some(firm.id),
                primary_color: None,
                accent_color: None,
                typeface: None,
            },
        )
        .await
        .unwrap();
        (firm.id, admin.id, brand_key)
    }

    #[tokio::test]
    async fn oauth_state_is_scoped_single_use_and_erases_the_pkce_verifier() {
        let db = crate::test_support::mem_surreal().await;
        let (firm_id, actor_id, brand_key) = practice(&db, "oauth-attempt").await;
        let now = Utc::now();
        let created = begin(&db, Role::Admin, actor_id, &brand_key, "instagram", now)
            .await
            .unwrap();
        assert_eq!(created.firm_id, firm_id);
        assert_eq!(created.brand_key, brand_key);
        assert_eq!(created.provider, "instagram");
        assert_eq!(created.expires_at, now + Duration::minutes(10));
        assert_eq!(created.state.len(), 43);
        super::bind_account(&db, &created, "synthetic-ig-id")
            .await
            .unwrap();

        let consumed = consume(&db, &created.state, now + Duration::seconds(1))
            .await
            .unwrap();
        assert_eq!(consumed.pkce_verifier, created.pkce_verifier);
        assert_eq!(
            consumed.provider_account_id.as_deref(),
            Some("synthetic-ig-id")
        );
        assert_eq!(consumed.actor_person_id, actor_id);
        assert!(matches!(
            consume(&db, &created.state, now + Duration::seconds(2)).await,
            Err(SocialOAuthAttemptError::InvalidState)
        ));

        let verifier: Option<String> = db
            .query("SELECT VALUE pkce_verifier FROM social_oauth_attempt WHERE state_hash = $hash")
            .bind(("hash", super::state_hash(&created.state)))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(verifier.as_deref(), Some(""));
    }

    #[tokio::test]
    async fn only_the_owning_firm_admin_can_begin_a_brand_oauth_attempt() {
        let db = crate::test_support::mem_surreal().await;
        let (_, _, brand_key) = practice(&db, "oauth-owned").await;
        let (_, other_admin, _) = practice(&db, "oauth-other").await;
        assert!(matches!(
            begin(
                &db,
                Role::Admin,
                other_admin,
                &brand_key,
                "facebook",
                Utc::now(),
            )
            .await,
            Err(SocialOAuthAttemptError::Denied)
        ));
    }
}
