//! Encrypted OAuth account connections for Brand social publishing.

use std::fmt::{self, Write as _};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::{DateTime, Utc};
use cloud::{KmsContext, RuntimeKms, WrappedDataKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::firm_capability::{FirmCapability, FirmCapabilityDecision};
use crate::persons::Role;
use crate::postgres::social::{self as authority, SocialAuthorityError};

/// A PostgreSQL account's non-secret metadata as `row`: the sealed token
/// bundle and its key material never leave the database in a listing.
macro_rules! pg_account {
    ($alias:literal) => {
        concat!(
            "to_jsonb(",
            $alias,
            ") - 'ciphertext' - 'wrapped_dek' - 'kms_key_version' - 'kms_context' AS row"
        )
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SocialProvider {
    Facebook,
    Instagram,
    Threads,
    Youtube,
    Linkedin,
    X,
    Tiktok,
}

impl SocialProvider {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Facebook => "facebook",
            Self::Instagram => "instagram",
            Self::Threads => "threads",
            Self::Youtube => "youtube",
            Self::Linkedin => "linkedin",
            Self::X => "x",
            Self::Tiktok => "tiktok",
        }
    }

    /// Permissions a connection must hold to publish and reply.
    ///
    /// Instagram uses the Instagram API with Facebook Login: a Page access token
    /// on `graph.facebook.com`, so its permissions are the `instagram_*` and
    /// Page permissions Facebook Login grants, never Instagram Login's
    /// `instagram_business_*` scopes.
    #[must_use]
    pub const fn required_scopes(self) -> &'static [&'static str] {
        match self {
            Self::Facebook => &[
                "pages_show_list",
                "pages_read_engagement",
                "pages_manage_posts",
                "pages_manage_engagement",
            ],
            Self::Instagram => &[
                "pages_show_list",
                "pages_read_engagement",
                "instagram_basic",
                "instagram_content_publish",
                "instagram_manage_comments",
                "instagram_manage_messages",
            ],
            Self::Threads => &[
                "threads_basic",
                "threads_content_publish",
                "threads_manage_replies",
            ],
            Self::Linkedin => &["r_organization_admin", "w_organization_social"],
            Self::Tiktok => &[
                "video.publish",
                "video.list",
                "user.info.basic",
                "user.info.username",
            ],
            Self::X => &["tweet.read", "tweet.write", "users.read", "offline.access"],
            Self::Youtube => &[
                "https://www.googleapis.com/auth/youtube.force-ssl",
                "https://www.googleapis.com/auth/youtube.upload",
            ],
        }
    }

    /// The required permissions absent from `granted`.
    #[must_use]
    pub fn missing_scopes(self, granted: &[String]) -> Vec<&'static str> {
        self.required_scopes()
            .iter()
            .copied()
            .filter(|required| !granted.iter().any(|scope| scope == required))
            .collect()
    }

    /// What a connection's stored expiry calls for before a provider request.
    ///
    /// Facebook and Instagram hold Page tokens issued from a long-lived user
    /// token, which carry no expiry; a stored expiry on one is a short-lived
    /// token that can only be replaced by reconnecting. A Threads long-lived
    /// token is refreshed in its last week and cannot be refreshed once
    /// expired. YouTube and X access tokens are refreshed from their refresh tokens in
    /// its last five minutes or after it lapses.
    #[must_use]
    pub fn token_renewal(
        self,
        expires_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> TokenRenewal {
        let Some(expires_at) = expires_at else {
            return TokenRenewal::Current;
        };
        // The window before expiry in which a refresh is attempted; `None`
        // where the provider offers no refresh for this connection.
        let refresh_window = match self {
            Self::Facebook | Self::Instagram | Self::Linkedin => None,
            Self::Threads => Some(chrono::Duration::days(7)),
            Self::Youtube | Self::X | Self::Tiktok => Some(chrono::Duration::minutes(5)),
        };
        let expired = expires_at <= now;
        match refresh_window {
            None if expired => TokenRenewal::Reconnect,
            // Google refreshes from the refresh token after expiry too.
            Some(_) if expired && !matches!(self, Self::Youtube | Self::X | Self::Tiktok) => {
                TokenRenewal::Reconnect
            }
            Some(window) if expires_at <= now + window => TokenRenewal::Refresh,
            None | Some(_) => TokenRenewal::Current,
        }
    }
}

/// The token action a connection needs before a provider request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenRenewal {
    /// The stored access token is valid.
    Current,
    /// Exchange the stored credential for a fresh access token first.
    Refresh,
    /// The authorization lapsed; a Brand manager must connect the account again.
    Reconnect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialAccount {
    pub id: String,
    pub firm_id: Uuid,
    pub brand_key: String,
    pub provider: SocialProvider,
    pub provider_account_id: String,
    pub display_name: Option<String>,
    pub scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub status: String,
    pub connected_by_person_id: Uuid,
    pub revoked_by_person_id: Option<Uuid>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum SocialAccountError {
    #[error("database: {0}")]
    Db(String),
    #[error("not allowed")]
    Denied,
    #[error("the Brand was not found")]
    BrandNotFound,
    #[error("the social account was not found")]
    NotFound,
    #[error("unsupported social provider")]
    UnsupportedProvider,
    #[error("provider account identity is missing")]
    MissingAccountIdentity,
    #[error("provider credential is missing")]
    MissingCredential,
    #[error("credential encryption failed")]
    Encryption(#[source] cloud::KmsError),
    #[error("credential could not be decoded")]
    InvalidCredential,
}

/// OAuth tokens are accepted only by the authorization callback and never
/// returned by account-list APIs.
pub struct ConnectSocialAccount<'a> {
    pub actor_role: Role,
    pub actor_person_id: Uuid,
    pub brand_key: &'a str,
    pub provider: SocialProvider,
    pub provider_account_id: &'a str,
    pub display_name: Option<&'a str>,
    pub scopes: &'a [String],
    pub expires_at: Option<DateTime<Utc>>,
    pub access_token: &'a str,
    pub refresh_token: Option<&'a str>,
}

/// Replace encrypted credentials after an OAuth refresh. The caller is a
/// trusted provider adapter; no UI or arbitrary route receives this write
/// capability.
pub struct RefreshSocialAccount<'a> {
    pub brand_key: &'a str,
    pub provider: SocialProvider,
    pub provider_account_id: &'a str,
    pub expires_at: DateTime<Utc>,
    pub access_token: &'a str,
    pub refresh_token: Option<&'a str>,
}

#[derive(Serialize)]
struct TokenBundleRef<'a> {
    access_token: &'a str,
    refresh_token: Option<&'a str>,
}

#[derive(Deserialize)]
struct TokenBundleView<'a> {
    #[serde(borrow)]
    access_token: &'a str,
    #[serde(borrow)]
    refresh_token: Option<&'a str>,
}

/// Resolved only for a provider request. Its secret bytes are cleared on drop
/// and its Debug output never includes the token.
pub struct ResolvedSocialTokens {
    bytes: Vec<u8>,
}

impl ResolvedSocialTokens {
    pub fn access_token(&self) -> Result<&str, SocialAccountError> {
        Ok(self.bundle()?.access_token)
    }

    pub fn refresh_token(&self) -> Result<Option<&str>, SocialAccountError> {
        Ok(self.bundle()?.refresh_token)
    }

    fn bundle(&self) -> Result<TokenBundleView<'_>, SocialAccountError> {
        serde_json::from_slice(&self.bytes).map_err(|_| SocialAccountError::InvalidCredential)
    }
}

impl fmt::Debug for ResolvedSocialTokens {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ResolvedSocialTokens(REDACTED)")
    }
}

impl Drop for ResolvedSocialTokens {
    fn drop(&mut self) {
        self.bytes.fill(0);
    }
}

fn account_key(brand_key: &str, provider: SocialProvider, remote_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(brand_key.as_bytes());
    hasher.update([0]);
    hasher.update(provider.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(remote_id.as_bytes());
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(encoded, "{byte:02x}");
    }
    format!("{}_{}_{}", brand_key, provider.as_str(), encoded)
}

fn kms_context(firm_id: Uuid, key: &str) -> KmsContext {
    KmsContext::new(
        firm_id,
        format!("social-account:{key}"),
        "oauth-token-bundle",
    )
}

/// Encrypt and upsert the OAuth connection after resolving Brand authority.
pub async fn connect(
    db: &ApplicationStore,
    input: ConnectSocialAccount<'_>,
    kms: &dyn RuntimeKms,
) -> Result<SocialAccount, SocialAccountError> {
    if input.provider_account_id.trim().is_empty() {
        return Err(SocialAccountError::MissingAccountIdentity);
    }
    if input.access_token.trim().is_empty() {
        return Err(SocialAccountError::MissingCredential);
    }
    let brand = crate::brands::find_by_key(db, input.brand_key)
        .await
        .map_err(|error| SocialAccountError::Db(error.to_string()))?
        .ok_or(SocialAccountError::BrandNotFound)?;
    let firm_id = brand.firm_id.ok_or(SocialAccountError::Denied)?;
    let decision = FirmCapability::ManageBrandContent
        .resolve(db, input.actor_role, Some(input.actor_person_id), firm_id)
        .await
        .map_err(|error| SocialAccountError::Db(error.to_string()))?;
    if decision != FirmCapabilityDecision::Allowed {
        return Err(SocialAccountError::Denied);
    }
    let key = account_key(input.brand_key, input.provider, input.provider_account_id);
    let context = kms_context(firm_id, &key);
    let bundle = TokenBundleRef {
        access_token: input.access_token,
        refresh_token: input.refresh_token,
    };
    let mut plaintext =
        serde_json::to_vec(&bundle).map_err(|_| SocialAccountError::InvalidCredential)?;
    let mut data_key: [u8; 32] = rand::random();
    let sealed = cloud::seal(&plaintext, &data_key, &context);
    plaintext.fill(0);
    let ciphertext = sealed.map_err(SocialAccountError::Encryption)?;
    let wrapped_result = kms.wrap_data_key(&data_key, &context).await;
    data_key.fill(0);
    let wrapped = wrapped_result.map_err(SocialAccountError::Encryption)?;
    let pg = authority::authority().await?;
    let sealed = Sealed {
        firm_id,
        ciphertext: BASE64.encode(ciphertext),
        wrapped_dek: BASE64.encode(wrapped.ciphertext),
        kms_key_version: wrapped.key_version,
        kms_context: context.aad(),
    };
    connect_pg(&pg, key, &input, sealed).await
}

/// Upsert a connection under its stable key; a reconnect clears revocation.
async fn connect_pg(
    pg: &crate::postgres::PostgresDb,
    key: String,
    input: &ConnectSocialAccount<'_>,
    sealed: Sealed,
) -> Result<SocialAccount, SocialAccountError> {
    let query = sqlx::query(concat!(
        "WITH w AS (INSERT INTO social_account (id, firm_id, brand_key, provider, \
         provider_account_id, display_name, scopes, expires_at, ciphertext, wrapped_dek, \
         kms_key_version, kms_context, status, connected_by_person_id, inserted_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'connected', $13, now(), now()) \
         ON CONFLICT (id) DO UPDATE SET firm_id = EXCLUDED.firm_id, \
         display_name = EXCLUDED.display_name, scopes = EXCLUDED.scopes, \
         expires_at = EXCLUDED.expires_at, ciphertext = EXCLUDED.ciphertext, \
         wrapped_dek = EXCLUDED.wrapped_dek, kms_key_version = EXCLUDED.kms_key_version, \
         kms_context = EXCLUDED.kms_context, status = 'connected', \
         connected_by_person_id = EXCLUDED.connected_by_person_id, \
         revoked_by_person_id = NULL, revoked_at = NULL, updated_at = now() RETURNING *) \
         SELECT ",
        pg_account!("w"),
        " FROM w"
    ))
    .bind(key)
    .bind(sealed.firm_id)
    .bind(input.brand_key.to_owned())
    .bind(input.provider.as_str())
    .bind(input.provider_account_id.to_owned())
    .bind(input.display_name.map(str::to_owned))
    .bind(input.scopes.to_vec())
    .bind(input.expires_at)
    .bind(sealed.ciphertext)
    .bind(sealed.wrapped_dek)
    .bind(sealed.kms_key_version)
    .bind(sealed.kms_context)
    .bind(input.actor_person_id);
    authority::maybe(pg, query)
        .await?
        .ok_or(SocialAccountError::NotFound)
}

/// List metadata for Brands the actor can manage; tokens are never selected.
pub async fn for_actor(
    db: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Uuid,
) -> Result<Vec<SocialAccount>, SocialAccountError> {
    let brand_keys = crate::posts::manageable_brand_keys(db, actor_role, actor_person_id)
        .await
        .map_err(|error| SocialAccountError::Db(error.to_string()))?;
    let pg = authority::authority().await?;
    let query = sqlx::query(concat!(
        "SELECT ",
        pg_account!("a"),
        " FROM social_account a WHERE a.brand_key = ANY($1) \
         ORDER BY array_position($1, a.brand_key), a.provider"
    ))
    .bind(brand_keys);
    Ok(authority::rows(&pg, query).await?)
}

/// Find connected Brand records for one provider webhook identity. More than
/// one result is returned deliberately: webhook adapters must reject an
/// ambiguous cross-Brand mapping instead of guessing where to file a message.
/// Resolve tokens for one already-authorized provider request. The secret
/// wrapper still clears its bytes on drop; provider clients receive only the
/// borrowed values needed for the outbound operation.
pub async fn resolve_tokens_for_brand(
    db: &ApplicationStore,
    brand_key: &str,
    provider: SocialProvider,
    provider_account_id: &str,
    kms: &dyn RuntimeKms,
) -> Result<ResolvedSocialTokens, SocialAccountError> {
    resolve_tokens(db, brand_key, provider, provider_account_id, kms).await
}

pub async fn for_provider_identity(
    _db: &ApplicationStore,
    provider: SocialProvider,
    provider_account_id: &str,
) -> Result<Vec<SocialAccount>, SocialAccountError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(concat!(
        "SELECT ",
        pg_account!("a"),
        " FROM social_account a WHERE a.provider = $1 \
         AND a.provider_account_id = $2 AND a.status = 'connected'"
    ))
    .bind(provider.as_str())
    .bind(provider_account_id.to_owned());
    Ok(authority::rows(&pg, query).await?)
}

/// Load connected, non-secret account metadata selected by a Blog Post.
/// The Brand and account keys are persisted on the post, but every invocation
/// resolves them again so a revoked or rehomed account cannot receive content.
pub async fn selected_for_brand(
    _db: &ApplicationStore,
    brand_key: &str,
    account_keys: &[String],
) -> Result<Vec<SocialAccount>, SocialAccountError> {
    if account_keys.is_empty() {
        return Ok(Vec::new());
    }
    let pg = authority::authority().await?;
    let query = sqlx::query(concat!(
        "SELECT ",
        pg_account!("a"),
        " FROM social_account a WHERE a.brand_key = $1 \
         AND a.status = 'connected' AND a.id = ANY($2) ORDER BY a.provider"
    ))
    .bind(brand_key.to_owned())
    .bind(account_keys.to_vec());
    Ok(authority::rows(&pg, query).await?)
}

/// One connected account of a Brand, read immediately before a provider
/// request so a revoked, deleted, or rehomed connection is never used.
pub async fn connected_account(
    _db: &ApplicationStore,
    brand_key: &str,
    provider: SocialProvider,
    provider_account_id: &str,
) -> Result<Option<SocialAccount>, SocialAccountError> {
    let key = account_key(brand_key, provider, provider_account_id);
    let pg = authority::authority().await?;
    let query = sqlx::query(concat!(
        "SELECT ",
        pg_account!("a"),
        " FROM social_account a WHERE a.id = $1 AND a.status = 'connected' \
         AND a.brand_key = $2"
    ))
    .bind(key)
    .bind(brand_key.to_owned());
    Ok(authority::maybe(&pg, query).await?)
}

/// A connected account's sealed token bundle and the KMS context it was
/// sealed under.
#[derive(Deserialize)]
struct Sealed {
    firm_id: Uuid,
    ciphertext: String,
    wrapped_dek: String,
    kms_key_version: String,
    kms_context: String,
}

/// The sealed bundle of a connected account; `None` once it is revoked.
async fn sealed(_db: &ApplicationStore, key: &str) -> Result<Option<Sealed>, SocialAccountError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(a) AS row FROM social_account a \
         WHERE a.id = $1 AND a.status = 'connected'",
    )
    .bind(key.to_owned());
    Ok(authority::maybe(&pg, query).await?)
}

/// Resolve tokens by account identity for a provider service. Revoked accounts
/// cannot be resolved; there is deliberately no access-token cache here.
pub async fn resolve_tokens(
    db: &ApplicationStore,
    brand_key: &str,
    provider: SocialProvider,
    provider_account_id: &str,
    kms: &dyn RuntimeKms,
) -> Result<ResolvedSocialTokens, SocialAccountError> {
    let key = account_key(brand_key, provider, provider_account_id);
    let row = sealed(db, &key)
        .await?
        .ok_or(SocialAccountError::NotFound)?;
    let firm_id = row.firm_id;
    let brand = crate::brands::find_by_key(db, brand_key)
        .await
        .map_err(|error| SocialAccountError::Db(error.to_string()))?
        .ok_or(SocialAccountError::BrandNotFound)?;
    if brand.firm_id != Some(firm_id) {
        return Err(SocialAccountError::NotFound);
    }
    let context = kms_context(firm_id, &key);
    if context.aad() != row.kms_context {
        return Err(SocialAccountError::InvalidCredential);
    }
    let wrapped = WrappedDataKey {
        ciphertext: BASE64
            .decode(row.wrapped_dek)
            .map_err(|_| SocialAccountError::InvalidCredential)?,
        key_version: row.kms_key_version,
    };
    let mut data_key = kms
        .unwrap_data_key(&wrapped, &context)
        .await
        .map_err(SocialAccountError::Encryption)?;
    let ciphertext = BASE64
        .decode(row.ciphertext)
        .map_err(|_| SocialAccountError::InvalidCredential)?;
    let opened = cloud::open(&ciphertext, &data_key, &context);
    data_key.fill(0);
    let bytes = opened.map_err(SocialAccountError::Encryption)?;
    Ok(ResolvedSocialTokens { bytes })
}

/// Encrypt refreshed tokens under the same Brand and account KMS context.
pub async fn update_refreshed_tokens(
    db: &ApplicationStore,
    input: RefreshSocialAccount<'_>,
    kms: &dyn RuntimeKms,
) -> Result<(), SocialAccountError> {
    if input.access_token.trim().is_empty() {
        return Err(SocialAccountError::MissingCredential);
    }
    let key = account_key(input.brand_key, input.provider, input.provider_account_id);
    let row = sealed(db, &key)
        .await?
        .ok_or(SocialAccountError::NotFound)?;
    let firm_id = row.firm_id;
    let context = kms_context(firm_id, &key);
    if context.aad() != row.kms_context {
        return Err(SocialAccountError::InvalidCredential);
    }
    let mut plaintext = serde_json::to_vec(&TokenBundleRef {
        access_token: input.access_token,
        refresh_token: input.refresh_token,
    })
    .map_err(|_| SocialAccountError::InvalidCredential)?;
    let mut data_key: [u8; 32] = rand::random();
    let sealed = cloud::seal(&plaintext, &data_key, &context);
    plaintext.fill(0);
    let ciphertext = sealed.map_err(SocialAccountError::Encryption)?;
    let wrapped_result = kms.wrap_data_key(&data_key, &context).await;
    data_key.fill(0);
    let wrapped = wrapped_result.map_err(SocialAccountError::Encryption)?;
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "UPDATE social_account SET ciphertext = $2, wrapped_dek = $3, kms_key_version = $4, \
         expires_at = $5, updated_at = now() WHERE id = $1 AND status = 'connected'",
    )
    .bind(key)
    .bind(BASE64.encode(ciphertext))
    .bind(BASE64.encode(wrapped.ciphertext))
    .bind(wrapped.key_version)
    .bind(input.expires_at);
    authority::execute(&pg, query).await?;
    Ok(())
}

/// Disconnect a Brand account while retaining non-secret audit metadata.
pub async fn revoke(
    db: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Uuid,
    brand_key: &str,
    provider: SocialProvider,
    provider_account_id: &str,
) -> Result<(), SocialAccountError> {
    let brand = crate::brands::find_by_key(db, brand_key)
        .await
        .map_err(|error| SocialAccountError::Db(error.to_string()))?
        .ok_or(SocialAccountError::BrandNotFound)?;
    let firm_id = brand.firm_id.ok_or(SocialAccountError::Denied)?;
    let decision = FirmCapability::ManageBrandContent
        .resolve(db, actor_role, Some(actor_person_id), firm_id)
        .await
        .map_err(|error| SocialAccountError::Db(error.to_string()))?;
    if decision != FirmCapabilityDecision::Allowed {
        return Err(SocialAccountError::Denied);
    }
    let key = account_key(brand_key, provider, provider_account_id);
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "UPDATE social_account SET status = 'revoked', ciphertext = '', wrapped_dek = '', \
         revoked_by_person_id = $2, revoked_at = now(), updated_at = now() \
         WHERE id = $1 AND status = 'connected'",
    )
    .bind(key)
    .bind(actor_person_id);
    match authority::execute(&pg, query).await? {
        0 => Err(SocialAccountError::NotFound),
        _ => Ok(()),
    }
}

impl From<SocialAuthorityError> for SocialAccountError {
    fn from(error: SocialAuthorityError) -> Self {
        Self::Db(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_renewal_follows_each_provider_lifecycle() {
        let now = Utc::now();
        let day = chrono::Duration::days(1);
        let cases = [
            (SocialProvider::Facebook, None, TokenRenewal::Current),
            (
                SocialProvider::Linkedin,
                Some(now + day),
                TokenRenewal::Current,
            ),
            (
                SocialProvider::Linkedin,
                Some(now - day),
                TokenRenewal::Reconnect,
            ),
            (
                SocialProvider::Instagram,
                Some(now + day),
                TokenRenewal::Current,
            ),
            (
                SocialProvider::Instagram,
                Some(now - day),
                TokenRenewal::Reconnect,
            ),
            (
                SocialProvider::Threads,
                Some(now + day * 30),
                TokenRenewal::Current,
            ),
            (
                SocialProvider::Threads,
                Some(now + day * 3),
                TokenRenewal::Refresh,
            ),
            (
                SocialProvider::Threads,
                Some(now - day),
                TokenRenewal::Reconnect,
            ),
            (
                SocialProvider::Youtube,
                Some(now + chrono::Duration::hours(1)),
                TokenRenewal::Current,
            ),
            (
                SocialProvider::Youtube,
                Some(now + chrono::Duration::minutes(2)),
                TokenRenewal::Refresh,
            ),
            (
                SocialProvider::Youtube,
                Some(now - day),
                TokenRenewal::Refresh,
            ),
        ];
        for (provider, expires_at, expected) in cases {
            assert_eq!(
                provider.token_renewal(expires_at, now),
                expected,
                "{provider:?} {expires_at:?}"
            );
        }
    }

    #[test]
    fn instagram_requires_facebook_login_permissions() {
        let legacy = vec![
            "instagram_business_basic".to_string(),
            "instagram_business_content_publish".to_string(),
        ];
        assert!(SocialProvider::Instagram
            .missing_scopes(&legacy)
            .contains(&"instagram_content_publish"));
        let granted = SocialProvider::Instagram
            .required_scopes()
            .iter()
            .map(|scope| (*scope).to_string())
            .collect::<Vec<_>>();
        assert!(SocialProvider::Instagram
            .missing_scopes(&granted)
            .is_empty());
    }
}
