//! Encrypted OAuth account connections for Brand social publishing.

use std::fmt::{self, Write as _};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::{DateTime, Utc};
use cloud::{KmsContext, RuntimeKms, WrappedDataKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::firm_capability::{FirmCapability, FirmCapabilityDecision};
use crate::persons::Role;
use crate::surreal::{record_id, record_uuid, SurrealDb};

const TABLE: &str = "social_account";

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

#[derive(SurrealValue)]
struct AccountRow {
    id: surrealdb::types::RecordId,
    firm_id: surrealdb::types::RecordId,
    brand_key: String,
    provider: String,
    provider_account_id: String,
    display_name: Option<String>,
    scopes: Vec<String>,
    expires_at: Option<surrealdb::types::Datetime>,
    status: String,
    connected_by_person_id: surrealdb::types::RecordId,
    revoked_by_person_id: Option<surrealdb::types::RecordId>,
    revoked_at: Option<surrealdb::types::Datetime>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl AccountRow {
    fn into_account(self) -> Option<SocialAccount> {
        Some(SocialAccount {
            id: account_key(
                &self.brand_key,
                parse_provider(&self.provider)?,
                &self.provider_account_id,
            ),
            firm_id: record_uuid(&self.firm_id)?,
            brand_key: self.brand_key,
            provider: parse_provider(&self.provider)?,
            provider_account_id: self.provider_account_id,
            display_name: self.display_name,
            scopes: self.scopes,
            expires_at: self.expires_at.map(Into::into),
            status: self.status,
            connected_by_person_id: record_uuid(&self.connected_by_person_id)?,
            revoked_by_person_id: self.revoked_by_person_id.as_ref().and_then(record_uuid),
            revoked_at: self.revoked_at.map(Into::into),
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
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

fn parse_provider(value: &str) -> Option<SocialProvider> {
    match value {
        "facebook" => Some(SocialProvider::Facebook),
        "instagram" => Some(SocialProvider::Instagram),
        "threads" => Some(SocialProvider::Threads),
        "youtube" => Some(SocialProvider::Youtube),
        "linkedin" => Some(SocialProvider::Linkedin),
        "x" => Some(SocialProvider::X),
        "tiktok" => Some(SocialProvider::Tiktok),
        _ => None,
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
    db: &SurrealDb,
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
    let now = Utc::now();
    let mut response = db
        .query(format!(
            "UPSERT $id SET firm_id = $firm, brand_key = $brand, provider = $provider, \
             provider_account_id = $remote_id, display_name = $display_name, scopes = $scopes, \
             expires_at = $expires_at, ciphertext = $ciphertext, wrapped_dek = $wrapped, \
             kms_key_version = $key_version, kms_context = $context, status = 'connected', \
             connected_by_person_id = $actor, revoked_by_person_id = NONE, revoked_at = NONE, \
             updated_at = $now RETURN {SELECT}"
        ))
        .bind(("id", surrealdb::types::RecordId::new(TABLE, key)))
        .bind(("firm", record_id("firm", firm_id)))
        .bind(("brand", input.brand_key.to_string()))
        .bind(("provider", input.provider.as_str().to_string()))
        .bind(("remote_id", input.provider_account_id.to_string()))
        .bind(("display_name", input.display_name.map(str::to_string)))
        .bind(("scopes", input.scopes.to_vec()))
        .bind((
            "expires_at",
            input.expires_at.map(surrealdb::types::Datetime::from),
        ))
        .bind(("ciphertext", BASE64.encode(ciphertext)))
        .bind(("wrapped", BASE64.encode(wrapped.ciphertext)))
        .bind(("key_version", wrapped.key_version))
        .bind(("context", context.aad()))
        .bind(("actor", record_id("person", input.actor_person_id)))
        .bind(("now", surrealdb::types::Datetime::from(now)))
        .await
        .map_err(SocialAccountError::from)?
        .check()
        .map_err(SocialAccountError::from)?;
    let row: Option<AccountRow> = response.take(0).map_err(SocialAccountError::from)?;
    let account = row
        .and_then(AccountRow::into_account)
        .ok_or(SocialAccountError::NotFound)?;
    crate::postgres_mirror::after_write(
        db,
        crate::postgres_mirror::MirrorKey::SocialAccount(account_key(
            input.brand_key,
            input.provider,
            input.provider_account_id,
        )),
    )
    .await;
    Ok(account)
}

const SELECT: &str = "id, firm_id, brand_key, provider, provider_account_id, display_name, scopes, expires_at, status, connected_by_person_id, revoked_by_person_id, revoked_at, inserted_at, updated_at";

/// List metadata for Brands the actor can manage; tokens are never selected.
pub async fn for_actor(
    db: &SurrealDb,
    actor_role: Role,
    actor_person_id: Uuid,
) -> Result<Vec<SocialAccount>, SocialAccountError> {
    let brand_keys = crate::posts::manageable_brand_keys(db, actor_role, actor_person_id)
        .await
        .map_err(|error| SocialAccountError::Db(error.to_string()))?;
    let mut accounts = Vec::new();
    for brand_key in brand_keys {
        let mut response = db
            .query(format!(
                "SELECT {SELECT} FROM {TABLE} WHERE brand_key = $brand ORDER BY provider"
            ))
            .bind(("brand", brand_key))
            .await
            .map_err(SocialAccountError::from)?
            .check()
            .map_err(SocialAccountError::from)?;
        let rows: Vec<AccountRow> = response.take(0).map_err(SocialAccountError::from)?;
        accounts.extend(rows.into_iter().filter_map(AccountRow::into_account));
    }
    Ok(accounts)
}

/// Find connected Brand records for one provider webhook identity. More than
/// one result is returned deliberately: webhook adapters must reject an
/// ambiguous cross-Brand mapping instead of guessing where to file a message.
/// Resolve tokens for one already-authorized provider request. The secret
/// wrapper still clears its bytes on drop; provider clients receive only the
/// borrowed values needed for the outbound operation.
pub async fn resolve_tokens_for_brand(
    db: &SurrealDb,
    brand_key: &str,
    provider: SocialProvider,
    provider_account_id: &str,
    kms: &dyn RuntimeKms,
) -> Result<ResolvedSocialTokens, SocialAccountError> {
    resolve_tokens(db, brand_key, provider, provider_account_id, kms).await
}

pub async fn for_provider_identity(
    db: &SurrealDb,
    provider: SocialProvider,
    provider_account_id: &str,
) -> Result<Vec<SocialAccount>, SocialAccountError> {
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE provider = $provider \
             AND provider_account_id = $account AND status = 'connected'"
        ))
        .bind(("provider", provider.as_str().to_string()))
        .bind(("account", provider_account_id.to_string()))
        .await?
        .check()?;
    let rows: Vec<AccountRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(AccountRow::into_account)
        .collect())
}

/// Load connected, non-secret account metadata selected by a Blog Post.
/// The Brand and account keys are persisted on the post, but every invocation
/// resolves them again so a revoked or rehomed account cannot receive content.
pub async fn selected_for_brand(
    db: &SurrealDb,
    brand_key: &str,
    account_keys: &[String],
) -> Result<Vec<SocialAccount>, SocialAccountError> {
    if account_keys.is_empty() {
        return Ok(Vec::new());
    }
    let ids = account_keys
        .iter()
        .map(|key| surrealdb::types::RecordId::new(TABLE, key.clone()))
        .collect::<Vec<_>>();
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE brand_key = $brand \
             AND status = 'connected' AND id IN $ids ORDER BY provider"
        ))
        .bind(("brand", brand_key.to_string()))
        .bind(("ids", ids))
        .await?
        .check()?;
    let rows: Vec<AccountRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(AccountRow::into_account)
        .collect())
}

/// One connected account of a Brand, read immediately before a provider
/// request so a revoked, deleted, or rehomed connection is never used.
pub async fn connected_account(
    db: &SurrealDb,
    brand_key: &str,
    provider: SocialProvider,
    provider_account_id: &str,
) -> Result<Option<SocialAccount>, SocialAccountError> {
    let key = account_key(brand_key, provider, provider_account_id);
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM ONLY $id WHERE status = 'connected' AND brand_key = $brand"
        ))
        .bind(("id", surrealdb::types::RecordId::new(TABLE, key)))
        .bind(("brand", brand_key.to_string()))
        .await?
        .check()?;
    let row: Option<AccountRow> = response.take(0)?;
    Ok(row.and_then(AccountRow::into_account))
}

/// Resolve tokens by account identity for a provider service. Revoked accounts
/// cannot be resolved; there is deliberately no access-token cache here.
pub async fn resolve_tokens(
    db: &SurrealDb,
    brand_key: &str,
    provider: SocialProvider,
    provider_account_id: &str,
    kms: &dyn RuntimeKms,
) -> Result<ResolvedSocialTokens, SocialAccountError> {
    #[derive(SurrealValue)]
    struct SecretRow {
        firm_id: surrealdb::types::RecordId,
        ciphertext: String,
        wrapped_dek: String,
        kms_key_version: String,
        kms_context: String,
        status: String,
    }

    let key = account_key(brand_key, provider, provider_account_id);
    let mut response = db
        .query(
            "SELECT firm_id, ciphertext, wrapped_dek, kms_key_version, kms_context, status \
             FROM ONLY $id"
                .to_string(),
        )
        .bind(("id", surrealdb::types::RecordId::new(TABLE, key.clone())))
        .await?
        .check()?;
    let row: Option<SecretRow> = response.take(0)?;
    let row = row
        .filter(|row| row.status == "connected")
        .ok_or(SocialAccountError::NotFound)?;
    let firm_id = record_uuid(&row.firm_id).ok_or(SocialAccountError::NotFound)?;
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
    db: &SurrealDb,
    input: RefreshSocialAccount<'_>,
    kms: &dyn RuntimeKms,
) -> Result<(), SocialAccountError> {
    #[derive(SurrealValue)]
    struct ContextRow {
        firm_id: surrealdb::types::RecordId,
        kms_context: String,
    }

    if input.access_token.trim().is_empty() {
        return Err(SocialAccountError::MissingCredential);
    }
    let key = account_key(input.brand_key, input.provider, input.provider_account_id);
    let mut response = db
        .query("SELECT firm_id, kms_context FROM ONLY $id WHERE status = 'connected'")
        .bind(("id", surrealdb::types::RecordId::new(TABLE, key.clone())))
        .await?
        .check()?;
    let row: Option<ContextRow> = response.take(0)?;
    let row = row.ok_or(SocialAccountError::NotFound)?;
    let firm_id = record_uuid(&row.firm_id).ok_or(SocialAccountError::NotFound)?;
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
    db.query("UPDATE $id SET ciphertext = $ciphertext, wrapped_dek = $wrapped, kms_key_version = $key_version, expires_at = $expires_at, updated_at = time::now() WHERE status = 'connected'")
        .bind(("id", surrealdb::types::RecordId::new(TABLE, key)))
        .bind(("ciphertext", BASE64.encode(ciphertext)))
        .bind(("wrapped", BASE64.encode(wrapped.ciphertext)))
        .bind(("key_version", wrapped.key_version))
        .bind(("expires_at", surrealdb::types::Datetime::from(input.expires_at)))
        .await?
        .check()?;
    crate::postgres_mirror::after_write(
        db,
        crate::postgres_mirror::MirrorKey::SocialAccount(account_key(
            input.brand_key,
            input.provider,
            input.provider_account_id,
        )),
    )
    .await;
    Ok(())
}

/// Disconnect a Brand account while retaining non-secret audit metadata.
pub async fn revoke(
    db: &SurrealDb,
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
    let mut response = db
        .query("UPDATE $id SET status = 'revoked', ciphertext = '', wrapped_dek = '', revoked_by_person_id = $actor, revoked_at = time::now(), updated_at = time::now() WHERE status = 'connected' RETURN VALUE id")
        .bind(("id", surrealdb::types::RecordId::new(TABLE, key)))
        .bind(("actor", record_id("person", actor_person_id)))
        .await?
        .check()?;
    let ids: Vec<surrealdb::types::RecordId> = response.take(0)?;
    if ids.is_empty() {
        return Err(SocialAccountError::NotFound);
    }
    crate::postgres_mirror::after_write(
        db,
        crate::postgres_mirror::MirrorKey::SocialAccount(account_key(
            brand_key,
            provider,
            provider_account_id,
        )),
    )
    .await;
    Ok(())
}

impl From<surrealdb::Error> for SocialAccountError {
    fn from(error: surrealdb::Error) -> Self {
        Self::Db(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{brands::NewBrand, firms::NewFirm, persons::NewPerson, test_support};

    async fn practice(db: &SurrealDb, name: &str) -> (Uuid, Uuid, String) {
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
                entity_id: test_support::seed_entity(db).await,
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

    #[tokio::test]
    async fn account_tokens_are_encrypted_scoped_and_revocable() {
        let db = test_support::mem_surreal().await;
        let (_, admin, brand) = practice(&db, "social-test").await;
        let kms = cloud::FakeKms::new("social-test-key");
        let scopes = vec![
            "pages_manage_posts".to_string(),
            "pages_read_engagement".to_string(),
        ];
        let account = connect(
            &db,
            ConnectSocialAccount {
                actor_role: Role::Admin,
                actor_person_id: admin,
                brand_key: &brand,
                provider: SocialProvider::Facebook,
                provider_account_id: "page-123",
                display_name: Some("Synthetic Page"),
                scopes: &scopes,
                expires_at: Some(Utc::now() + chrono::Duration::days(30)),
                access_token: "synthetic-access-secret",
                refresh_token: Some("synthetic-refresh-secret"),
            },
            &kms,
        )
        .await
        .unwrap();
        assert_eq!(account.brand_key, brand);
        assert_eq!(account.provider, SocialProvider::Facebook);
        assert_eq!(account.scopes, scopes);
        assert_eq!(
            for_actor(&db, Role::Admin, admin).await.unwrap(),
            vec![account.clone()]
        );

        let tokens = resolve_tokens(&db, &brand, SocialProvider::Facebook, "page-123", &kms)
            .await
            .unwrap();
        assert_eq!(tokens.access_token().unwrap(), "synthetic-access-secret");
        assert_eq!(
            tokens.refresh_token().unwrap(),
            Some("synthetic-refresh-secret")
        );
        assert_eq!(format!("{tokens:?}"), "ResolvedSocialTokens(REDACTED)");

        let stored: Option<String> = db
            .query("SELECT VALUE ciphertext FROM ONLY $id")
            .bind((
                "id",
                surrealdb::types::RecordId::new(TABLE, account.id.clone()),
            ))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        let ciphertext = stored.unwrap();
        assert!(!ciphertext.contains("synthetic-access-secret"));
        assert!(!ciphertext.contains("synthetic-refresh-secret"));

        revoke(
            &db,
            Role::Admin,
            admin,
            &brand,
            SocialProvider::Facebook,
            "page-123",
        )
        .await
        .unwrap();
        let revoked = for_actor(&db, Role::Admin, admin).await.unwrap();
        assert_eq!(revoked.len(), 1);
        assert_eq!(revoked[0].status, "revoked");
        assert_eq!(revoked[0].revoked_by_person_id, Some(admin));
        assert!(revoked[0].revoked_at.is_some());
        assert!(matches!(
            resolve_tokens(&db, &brand, SocialProvider::Facebook, "page-123", &kms).await,
            Err(SocialAccountError::NotFound)
        ));
    }

    #[tokio::test]
    async fn another_firms_admin_cannot_connect_to_or_revoke_a_brand_account() {
        let db = test_support::mem_surreal().await;
        let (_, owner_admin, brand) = practice(&db, "social-owner").await;
        let (_, other_admin, _) = practice(&db, "social-other").await;
        let kms = cloud::FakeKms::new("social-isolation-key");
        let scopes = Vec::new();
        let input = || ConnectSocialAccount {
            actor_role: Role::Admin,
            actor_person_id: other_admin,
            brand_key: &brand,
            provider: SocialProvider::Youtube,
            provider_account_id: "channel-xyz",
            display_name: None,
            scopes: &scopes,
            expires_at: None,
            access_token: "synthetic-token",
            refresh_token: None,
        };
        assert!(matches!(
            connect(&db, input(), &kms).await,
            Err(SocialAccountError::Denied)
        ));
        assert!(matches!(
            revoke(
                &db,
                Role::Admin,
                other_admin,
                &brand,
                SocialProvider::Youtube,
                "channel-xyz",
            )
            .await,
            Err(SocialAccountError::Denied)
        ));
        assert!(for_actor(&db, Role::Admin, owner_admin)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn connected_account_reads_only_the_brands_connected_account() {
        let db = test_support::mem_surreal().await;
        let (_, admin, brand) = practice(&db, "social-connected").await;
        let (_, _, other_brand) = practice(&db, "social-connected-other").await;
        let kms = cloud::FakeKms::new("social-connected-key");
        let connected = connect(
            &db,
            ConnectSocialAccount {
                actor_role: Role::Admin,
                actor_person_id: admin,
                brand_key: &brand,
                provider: SocialProvider::Threads,
                provider_account_id: "threads-1",
                display_name: None,
                scopes: &[],
                expires_at: None,
                access_token: "synthetic-access",
                refresh_token: None,
            },
            &kms,
        )
        .await
        .unwrap();
        let read = |brand: String| {
            let db = db.clone();
            async move {
                connected_account(&db, &brand, SocialProvider::Threads, "threads-1")
                    .await
                    .unwrap()
                    .map(|account| account.id)
            }
        };
        assert_eq!(read(brand.clone()).await, Some(connected.id));
        assert_eq!(read(other_brand).await, None);
        revoke(
            &db,
            Role::Admin,
            admin,
            &brand,
            SocialProvider::Threads,
            "threads-1",
        )
        .await
        .unwrap();
        assert_eq!(read(brand).await, None);
    }

    #[tokio::test]
    async fn identity_and_brand_selection_ignore_revoked_and_foreign_accounts() {
        let db = test_support::mem_surreal().await;
        let (_, admin, brand) = practice(&db, "social-select").await;
        let (_, _, other_brand) = practice(&db, "social-other-select").await;
        let kms = cloud::FakeKms::new("social-select-key");
        let scopes = vec!["pages_manage_posts".to_string()];
        let connected = connect(
            &db,
            ConnectSocialAccount {
                actor_role: Role::Admin,
                actor_person_id: admin,
                brand_key: &brand,
                provider: SocialProvider::Facebook,
                provider_account_id: "page-select",
                display_name: Some("Selected Page"),
                scopes: &scopes,
                expires_at: Some(Utc::now() + chrono::Duration::days(7)),
                access_token: "synthetic-access",
                refresh_token: Some("synthetic-refresh"),
            },
            &kms,
        )
        .await
        .unwrap();
        let matches = for_provider_identity(&db, SocialProvider::Facebook, "page-select")
            .await
            .unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, connected.id);
        assert!(
            for_provider_identity(&db, SocialProvider::Facebook, "missing")
                .await
                .unwrap()
                .is_empty()
        );
        let selected = selected_for_brand(&db, &brand, std::slice::from_ref(&connected.id))
            .await
            .unwrap();
        assert_eq!(selected.len(), 1);
        assert!(
            selected_for_brand(&db, &other_brand, std::slice::from_ref(&connected.id))
                .await
                .unwrap()
                .is_empty()
        );
        assert!(selected_for_brand(&db, &brand, &[])
            .await
            .unwrap()
            .is_empty());
        update_refreshed_tokens(
            &db,
            RefreshSocialAccount {
                brand_key: &brand,
                provider: SocialProvider::Facebook,
                provider_account_id: "page-select",
                expires_at: Utc::now() + chrono::Duration::days(14),
                access_token: "rotated-access",
                refresh_token: Some("rotated-refresh"),
            },
            &kms,
        )
        .await
        .unwrap();
        let tokens =
            resolve_tokens_for_brand(&db, &brand, SocialProvider::Facebook, "page-select", &kms)
                .await
                .unwrap();
        assert_eq!(tokens.access_token().unwrap(), "rotated-access");
        revoke(
            &db,
            Role::Admin,
            admin,
            &brand,
            SocialProvider::Facebook,
            "page-select",
        )
        .await
        .unwrap();
        assert!(
            for_provider_identity(&db, SocialProvider::Facebook, "page-select")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(selected_for_brand(&db, &brand, &[connected.id])
            .await
            .unwrap()
            .is_empty());
    }
}
