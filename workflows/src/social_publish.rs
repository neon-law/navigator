//! Durable scheduling request for a Brand Blog Post's selected social accounts,
//! and the send-time credential every social provider request uses.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use store::social_accounts::{SocialProvider, TokenRenewal};
use store::surreal::SurrealDb;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialPublishRequest {
    pub post_id: Uuid,
    pub publish_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialPublishReport {
    pub post_id: Uuid,
    pub outcome: String,
    pub destinations: usize,
}

/// Where expiring social credentials are renewed, and the YouTube OAuth client
/// and X OAuth clients that renew them. Meta Page tokens never need this: they do not expire.
#[derive(Clone)]
pub struct TokenRefreshConfig {
    pub threads_refresh_url: String,
    pub google_token_url: String,
    pub youtube_client: Option<(String, String)>,
    pub x_token_url: String,
    pub tiktok_token_url: String,
    pub tiktok_client: Option<(String, String)>,
    pub x_client: Option<(String, String)>,
}

impl TokenRefreshConfig {
    /// The providers' documented endpoints and the deployment's YouTube client.
    #[must_use]
    pub fn from_env() -> Self {
        let get = |key: &str| {
            std::env::var(key)
                .ok()
                .filter(|value| !value.trim().is_empty())
        };
        Self {
            x_token_url: "https://api.x.com/2/oauth2/token".into(),
            tiktok_token_url:
                "https://business-api.tiktok.com/open_api/v1.3/tt_user/oauth2/refresh_token/".into(),
            tiktok_client: get("NAVIGATOR_TIKTOK_BUSINESS_APP_ID")
                .zip(get("NAVIGATOR_TIKTOK_BUSINESS_APP_SECRET")),
            x_client: get("NAVIGATOR_X_CLIENT_ID").zip(get("NAVIGATOR_X_CLIENT_SECRET")),
            threads_refresh_url: "https://graph.threads.net/refresh_access_token".into(),
            google_token_url: "https://oauth2.googleapis.com/token".into(),
            youtube_client: get("NAVIGATOR_YOUTUBE_CLIENT_ID")
                .zip(get("NAVIGATOR_YOUTUBE_CLIENT_SECRET")),
        }
    }
}

impl std::fmt::Debug for TokenRefreshConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TokenRefreshConfig")
            .field("threads_refresh_url", &self.threads_refresh_url)
            .field("google_token_url", &self.google_token_url)
            .field("youtube_client", &self.youtube_client.is_some())
            .field("x_token_url", &self.x_token_url)
            .field("x_client", &self.x_client.is_some())
            .field("tiktok_token_url", &self.tiktok_token_url)
            .field("tiktok_client", &self.tiktok_client.is_some())
            .finish()
    }
}

/// An access token resolved for one provider request. Its bytes are cleared on
/// drop and its Debug output never includes it.
pub struct SendCredential {
    bytes: Vec<u8>,
}

impl SendCredential {
    fn new(token: &str) -> Self {
        Self {
            bytes: token.as_bytes().to_vec(),
        }
    }

    #[must_use]
    pub fn access_token(&self) -> &str {
        std::str::from_utf8(&self.bytes).unwrap_or_default()
    }
}

impl std::fmt::Debug for SendCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SendCredential(REDACTED)")
    }
}

impl Drop for SendCredential {
    fn drop(&mut self) {
        self.bytes.fill(0);
    }
}

/// Why a connected account cannot be used for a provider request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialFailure {
    /// The account, its Brand, or the Brand's Firm no longer matches the
    /// selection; nothing is sent.
    #[error("The connected account is no longer available to this Brand")]
    Unavailable,
    /// The authorization lapsed or lacks a permission; a Brand manager must
    /// connect the account again.
    #[error("Reconnect the {provider} account: {reason}")]
    Reconnect {
        provider: &'static str,
        reason: String,
    },
}

impl CredentialFailure {
    fn reconnect(provider: SocialProvider, reason: impl Into<String>) -> Self {
        Self::Reconnect {
            provider: provider.as_str(),
            reason: reason.into(),
        }
    }
}

/// The access token for one Brand account, renewed first when its provider's
/// lifecycle calls for it.
///
/// The account, its Brand, and the Brand's Firm are re-read on every call, its
/// granted permissions are checked against
/// [`SocialProvider::required_scopes`], and a renewed token is re-encrypted
/// before it is used. A failed renewal keeps using a token that has not yet
/// expired and asks for a reconnect once it has.
///
/// # Errors
///
/// Returns [`CredentialFailure`] when the account must not or cannot be used.
pub async fn send_credential(
    db: &SurrealDb,
    kms: &dyn cloud::RuntimeKms,
    http: &reqwest::Client,
    refresh: &TokenRefreshConfig,
    brand_key: &str,
    provider: SocialProvider,
    provider_account_id: &str,
) -> Result<SendCredential, CredentialFailure> {
    let account =
        store::social_accounts::connected_account(db, brand_key, provider, provider_account_id)
            .await
            .map_err(|_| CredentialFailure::Unavailable)?
            .ok_or(CredentialFailure::Unavailable)?;
    let missing = provider.missing_scopes(&account.scopes);
    if !missing.is_empty() {
        return Err(CredentialFailure::reconnect(
            provider,
            format!("it is missing {}", missing.join(", ")),
        ));
    }
    let tokens =
        store::social_accounts::resolve_tokens(db, brand_key, provider, provider_account_id, kms)
            .await
            .map_err(|_| CredentialFailure::Unavailable)?;
    let access_token = tokens
        .access_token()
        .map_err(|_| CredentialFailure::Unavailable)?;
    let now = Utc::now();
    match provider.token_renewal(account.expires_at, now) {
        TokenRenewal::Current => Ok(SendCredential::new(access_token)),
        TokenRenewal::Reconnect => Err(CredentialFailure::reconnect(
            provider,
            "its authorization expired",
        )),
        TokenRenewal::Refresh => {
            let refresh_token = tokens
                .refresh_token()
                .map_err(|_| CredentialFailure::Unavailable)?;
            match refresh_access_token(http, refresh, provider, access_token, refresh_token).await {
                Ok(renewed) => {
                    if provider == SocialProvider::Tiktok
                        && renewed.open_id.as_deref() != Some(provider_account_id)
                    {
                        return Err(CredentialFailure::reconnect(
                            provider,
                            "TikTok renewed a different account identity",
                        ));
                    }
                    store::social_accounts::update_refreshed_tokens(
                        db,
                        store::social_accounts::RefreshSocialAccount {
                            brand_key,
                            provider,
                            provider_account_id,
                            expires_at: now + chrono::Duration::seconds(renewed.expires_in),
                            access_token: &renewed.access_token,
                            refresh_token: renewed.refresh_token.as_deref().or(refresh_token),
                        },
                        kms,
                    )
                    .await
                    .map_err(|_| CredentialFailure::Unavailable)?;
                    Ok(SendCredential::new(&renewed.access_token))
                }
                Err(reason) if account.expires_at.is_some_and(|expires| expires <= now) => {
                    Err(CredentialFailure::reconnect(provider, reason))
                }
                Err(_) => Ok(SendCredential::new(access_token)),
            }
        }
    }
}

#[derive(Deserialize)]
struct RenewedToken {
    access_token: String,
    expires_in: i64,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    open_id: Option<String>,
}

async fn refresh_access_token(
    http: &reqwest::Client,
    refresh: &TokenRefreshConfig,
    provider: SocialProvider,
    access_token: &str,
    refresh_token: Option<&str>,
) -> Result<RenewedToken, String> {
    let response = match provider {
        SocialProvider::Tiktok => {
            let Some((id, secret)) = refresh.tiktok_client.as_ref() else { return Err("TikTok OAuth is not configured".into()); };
            let Some(token) = refresh_token else { return Err("no refresh token was granted".into()); };
            http.post(&refresh.tiktok_token_url).json(&serde_json::json!({
                "client_id": id, "client_secret": secret, "grant_type": "refresh_token", "refresh_token": token
            })).send().await
        }
        SocialProvider::X => {
            let Some((id, secret)) = refresh.x_client.as_ref() else {
                return Err("X OAuth is not configured".into());
            };
            let Some(token) = refresh_token else {
                return Err("no refresh token was granted".into());
            };
            http.post(&refresh.x_token_url)
                .basic_auth(id, Some(secret))
                .form(&[("grant_type", "refresh_token"), ("refresh_token", token)])
                .send()
                .await
        }
        SocialProvider::Threads => {
            http.get(&refresh.threads_refresh_url)
                .query(&[
                    ("grant_type", "th_refresh_token"),
                    ("access_token", access_token),
                ])
                .send()
                .await
        }
        SocialProvider::Youtube => {
            let Some((client_id, client_secret)) = refresh.youtube_client.as_ref() else {
                return Err("YouTube OAuth is not configured".into());
            };
            let Some(refresh_token) = refresh_token else {
                return Err("no refresh token was granted".into());
            };
            http.post(&refresh.google_token_url)
                .form(&[
                    ("client_id", client_id.as_str()),
                    ("client_secret", client_secret.as_str()),
                    ("grant_type", "refresh_token"),
                    ("refresh_token", refresh_token),
                ])
                .send()
                .await
        }
        SocialProvider::Facebook | SocialProvider::Instagram | SocialProvider::Linkedin => {
            return Err("This provider is renewed only by reconnecting".into());
        }
    }
    .map_err(|_| "the token endpoint could not be reached".to_string())?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|_| "the token response could not be read".to_string())?;
    if !status.is_success() || body.len() > 64 * 1024 {
        return Err(format!("the provider refused the token refresh ({status})"));
    }
    let renewed: RenewedToken = if provider == SocialProvider::Tiktok {
        let envelope: serde_json::Value =
            serde_json::from_slice(&body).map_err(|_| "TikTok token response was invalid")?;
        if envelope.get("code").and_then(serde_json::Value::as_i64) != Some(0) {
            return Err("TikTok refused token renewal".into());
        }
        serde_json::from_value(
            envelope
                .get("data")
                .cloned()
                .ok_or("TikTok token data is missing")?,
        )
        .map_err(|_| "TikTok token data is invalid")?
    } else {
        serde_json::from_slice(&body).map_err(|_| "the token response was invalid".to_string())?
    };
    if renewed.access_token.trim().is_empty() || renewed.expires_in <= 0 {
        return Err("the token response was incomplete".into());
    }
    validate_rotating_token(provider, &renewed)?;
    Ok(renewed)
}

fn validate_rotating_token(provider: SocialProvider, renewed: &RenewedToken) -> Result<(), String> {
    if matches!(provider, SocialProvider::X | SocialProvider::Tiktok) {
        if renewed.expires_in > 86_400 {
            return Err("Provider refresh expiry was invalid".into());
        }
        let scopes = renewed
            .scope
            .as_deref()
            .unwrap_or_default()
            .split([',', ' '])
            .filter(|scope| !scope.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        if !provider.missing_scopes(&scopes).is_empty()
            || renewed
                .refresh_token
                .as_deref()
                .is_none_or(|token| token.trim().is_empty())
        {
            return Err(
                "Token renewal did not retain required scopes and a rotated refresh token".into(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{send_credential, CredentialFailure, TokenRefreshConfig};
    use chrono::{DateTime, Duration, Utc};
    use store::brands::NewBrand;
    use store::firms::NewFirm;
    use store::persons::{NewPerson, Role};
    use store::social_accounts::{ConnectSocialAccount, SocialProvider};
    use store::surreal::SurrealDb;
    use uuid::Uuid;
    use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

    struct Account {
        db: SurrealDb,
        kms: cloud::FakeKms,
        admin: Uuid,
    }

    async fn connected(
        provider: SocialProvider,
        scopes: &[&str],
        expires_at: Option<DateTime<Utc>>,
    ) -> Account {
        let db = store::test_support::mem_surreal().await;
        let admin = store::persons::create(
            &db,
            &NewPerson::with_role("Token Admin", "token-admin@example.com", Role::Admin),
        )
        .await
        .unwrap();
        let firm = store::firms::create(
            &db,
            &NewFirm {
                name: "Token Firm".into(),
                status: "active".into(),
                entity_id: store::test_support::seed_entity(&db).await,
                admin_dri_person_id: admin.id,
            },
        )
        .await
        .unwrap();
        store::brands::create(
            &db,
            Role::Admin,
            Some(admin.id),
            &NewBrand {
                name: "Token Brand".into(),
                key: "token-brand".into(),
                firm_id: Some(firm.id),
                primary_color: None,
                accent_color: None,
                typeface: None,
            },
        )
        .await
        .unwrap();
        let kms = cloud::FakeKms::new("token-lifecycle");
        let scopes = scopes
            .iter()
            .map(|scope| (*scope).to_string())
            .collect::<Vec<_>>();
        store::social_accounts::connect(
            &db,
            ConnectSocialAccount {
                actor_role: Role::Admin,
                actor_person_id: admin.id,
                brand_key: "token-brand",
                provider,
                provider_account_id: "account-1",
                display_name: None,
                scopes: &scopes,
                expires_at,
                access_token: "stored-access",
                refresh_token: Some("stored-refresh"),
            },
            &kms,
        )
        .await
        .unwrap();
        Account {
            db,
            kms,
            admin: admin.id,
        }
    }

    #[tokio::test]
    async fn expired_tiktok_access_refreshes_rotates_and_retains_identity() {
        let server = MockServer::start().await;
        let account = connected(
            SocialProvider::Tiktok,
            SocialProvider::Tiktok.required_scopes(),
            Some(Utc::now() - Duration::hours(1)),
        )
        .await;
        Mock::given(matchers::method("POST")).and(matchers::path("/tiktok-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{
                "access_token":"renewed-access","expires_in":86400,"refresh_token":"rotated-refresh",
                "open_id":"account-1","scope":"video.publish,video.list,user.info.basic,user.info.username"
            }}))).expect(1).mount(&server).await;
        assert_eq!(
            credential(&account, SocialProvider::Tiktok, &server)
                .await
                .unwrap(),
            "renewed-access"
        );
        let tokens = store::social_accounts::resolve_tokens(
            &account.db,
            "token-brand",
            SocialProvider::Tiktok,
            "account-1",
            &account.kms,
        )
        .await
        .unwrap();
        assert_eq!(tokens.refresh_token().unwrap(), Some("rotated-refresh"));
    }

    #[tokio::test]
    async fn tiktok_refresh_refuses_changed_identity() {
        let server = MockServer::start().await;
        let account = connected(
            SocialProvider::Tiktok,
            SocialProvider::Tiktok.required_scopes(),
            Some(Utc::now() - Duration::hours(1)),
        )
        .await;
        Mock::given(matchers::method("POST")).and(matchers::path("/tiktok-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{
                "access_token":"wrong-account-access","expires_in":86400,"refresh_token":"rotated-refresh",
                "open_id":"different-account","scope":"video.publish,video.list,user.info.basic,user.info.username"
            }}))).mount(&server).await;
        assert!(matches!(
            credential(&account, SocialProvider::Tiktok, &server).await,
            Err(CredentialFailure::Reconnect { .. })
        ));
        let tokens = store::social_accounts::resolve_tokens(
            &account.db,
            "token-brand",
            SocialProvider::Tiktok,
            "account-1",
            &account.kms,
        )
        .await
        .unwrap();
        assert_eq!(tokens.access_token().unwrap(), "stored-access");
    }

    fn refresh(server: &MockServer) -> TokenRefreshConfig {
        TokenRefreshConfig {
            x_token_url: format!("{}/token", server.uri()),
            tiktok_token_url: format!("{}/tiktok-token", server.uri()),
            tiktok_client: Some(("synthetic-app".into(), "synthetic-secret".into())),
            x_client: Some(("x-id".into(), "x-secret".into())),
            threads_refresh_url: format!("{}/refresh_access_token", server.uri()),
            google_token_url: format!("{}/token", server.uri()),
            youtube_client: Some(("synthetic-client".into(), "synthetic-secret".into())),
        }
    }

    async fn credential(
        account: &Account,
        provider: SocialProvider,
        server: &MockServer,
    ) -> Result<String, CredentialFailure> {
        send_credential(
            &account.db,
            &account.kms,
            &reqwest::Client::new(),
            &refresh(server),
            "token-brand",
            provider,
            "account-1",
        )
        .await
        .map(|credential| credential.access_token().to_string())
    }

    async fn no_requests() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(matchers::any())
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        server
    }

    const THREADS: &[&str] = &[
        "threads_basic",
        "threads_content_publish",
        "threads_manage_replies",
    ];

    #[tokio::test]
    async fn current_tokens_are_used_without_a_provider_request() {
        let server = no_requests().await;
        let account = connected(
            SocialProvider::Threads,
            THREADS,
            Some(Utc::now() + Duration::days(40)),
        )
        .await;
        assert_eq!(
            credential(&account, SocialProvider::Threads, &server)
                .await
                .unwrap(),
            "stored-access"
        );
        let debug = send_credential(
            &account.db,
            &account.kms,
            &reqwest::Client::new(),
            &refresh(&server),
            "token-brand",
            SocialProvider::Threads,
            "account-1",
        )
        .await
        .unwrap();
        assert_eq!(format!("{debug:?}"), "SendCredential(REDACTED)");
        server.verify().await;
    }

    #[tokio::test]
    async fn a_scope_mismatch_asks_for_a_reconnect() {
        let server = no_requests().await;
        let account = connected(
            SocialProvider::Instagram,
            &[
                "instagram_business_basic",
                "instagram_business_content_publish",
            ],
            None,
        )
        .await;
        assert!(matches!(
            credential(&account, SocialProvider::Instagram, &server).await,
            Err(CredentialFailure::Reconnect { provider: "instagram", reason })
                if reason.contains("instagram_content_publish")
        ));
        server.verify().await;
    }

    #[tokio::test]
    async fn expired_meta_and_threads_tokens_ask_for_a_reconnect() {
        let server = no_requests().await;
        let expired = Some(Utc::now() - Duration::hours(1));
        let threads = connected(SocialProvider::Threads, THREADS, expired).await;
        assert!(matches!(
            credential(&threads, SocialProvider::Threads, &server).await,
            Err(CredentialFailure::Reconnect {
                provider: "threads",
                ..
            })
        ));
        let facebook = connected(
            SocialProvider::Facebook,
            SocialProvider::Facebook.required_scopes(),
            expired,
        )
        .await;
        let failure = credential(&facebook, SocialProvider::Facebook, &server)
            .await
            .unwrap_err();
        assert_eq!(
            failure.to_string(),
            "Reconnect the facebook account: its authorization expired"
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn a_threads_token_in_its_last_week_is_refreshed_and_stored() {
        let server = MockServer::start().await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/refresh_access_token"))
            .and(matchers::query_param("grant_type", "th_refresh_token"))
            .and(matchers::query_param("access_token", "stored-access"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "renewed-access",
                "token_type": "bearer",
                "expires_in": 5_183_944
            })))
            .expect(1)
            .mount(&server)
            .await;
        let account = connected(
            SocialProvider::Threads,
            THREADS,
            Some(Utc::now() + Duration::days(2)),
        )
        .await;
        assert_eq!(
            credential(&account, SocialProvider::Threads, &server)
                .await
                .unwrap(),
            "renewed-access"
        );
        let stored = store::social_accounts::connected_account(
            &account.db,
            "token-brand",
            SocialProvider::Threads,
            "account-1",
        )
        .await
        .unwrap()
        .unwrap();
        assert!(stored.expires_at.unwrap() > Utc::now() + Duration::days(50));
    }

    #[tokio::test]
    async fn a_failed_refresh_keeps_an_unexpired_token_and_reconnects_an_expired_one() {
        let server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_string("invalid_grant"))
            .expect(2)
            .mount(&server)
            .await;
        let youtube = SocialProvider::Youtube.required_scopes();
        let due = connected(
            SocialProvider::Youtube,
            youtube,
            Some(Utc::now() + Duration::minutes(2)),
        )
        .await;
        assert_eq!(
            credential(&due, SocialProvider::Youtube, &server)
                .await
                .unwrap(),
            "stored-access"
        );
        let expired = connected(
            SocialProvider::Youtube,
            youtube,
            Some(Utc::now() - Duration::minutes(2)),
        )
        .await;
        assert!(matches!(
            credential(&expired, SocialProvider::Youtube, &server).await,
            Err(CredentialFailure::Reconnect {
                provider: "youtube",
                ..
            })
        ));
    }

    #[tokio::test]
    async fn a_revoked_account_is_unavailable() {
        let server = no_requests().await;
        let account = connected(
            SocialProvider::Facebook,
            SocialProvider::Facebook.required_scopes(),
            None,
        )
        .await;
        store::social_accounts::revoke(
            &account.db,
            Role::Admin,
            account.admin,
            "token-brand",
            SocialProvider::Facebook,
            "account-1",
        )
        .await
        .unwrap();
        assert_eq!(
            credential(&account, SocialProvider::Facebook, &server).await,
            Err(CredentialFailure::Unavailable)
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn x_refresh_rotates_encrypted_tokens_and_checks_scopes() {
        let server = MockServer::start().await;
        let account = connected(
            SocialProvider::X,
            SocialProvider::X.required_scopes(),
            Some(Utc::now() - Duration::seconds(1)),
        )
        .await;
        Mock::given(matchers::method("POST")).and(matchers::path("/token"))
            .and(matchers::header("authorization", "Basic eC1pZDp4LXNlY3JldA=="))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "rotated-access", "refresh_token": "rotated-refresh", "expires_in": 7200,
                "scope": "tweet.read tweet.write users.read offline.access"
            }))).expect(1).mount(&server).await;
        let token = send_credential(
            &account.db,
            &account.kms,
            &reqwest::Client::new(),
            &refresh(&server),
            "token-brand",
            SocialProvider::X,
            "account-1",
        )
        .await
        .unwrap();
        assert_eq!(token.access_token(), "rotated-access");
        let stored = store::social_accounts::resolve_tokens(
            &account.db,
            "token-brand",
            SocialProvider::X,
            "account-1",
            &account.kms,
        )
        .await
        .unwrap();
        assert_eq!(stored.refresh_token().unwrap(), Some("rotated-refresh"));
        server.verify().await;
    }

    #[tokio::test]
    async fn x_refresh_refuses_missing_scopes_and_retains_encrypted_account() {
        let server = MockServer::start().await;
        let account = connected(
            SocialProvider::X,
            SocialProvider::X.required_scopes(),
            Some(Utc::now() - Duration::seconds(1)),
        )
        .await;
        Mock::given(matchers::method("POST")).and(matchers::path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "invalid-scope-access", "refresh_token": "rotated-refresh", "expires_in": 7200,
                "scope": "tweet.read"
            }))).mount(&server).await;
        assert!(send_credential(
            &account.db,
            &account.kms,
            &reqwest::Client::new(),
            &refresh(&server),
            "token-brand",
            SocialProvider::X,
            "account-1"
        )
        .await
        .is_err());
        let stored = store::social_accounts::resolve_tokens(
            &account.db,
            "token-brand",
            SocialProvider::X,
            "account-1",
            &account.kms,
        )
        .await
        .unwrap();
        assert_eq!(stored.access_token().unwrap(), "stored-access");
    }
}
