//! Keyless domain-wide delegation for the Firm's Workspace service account.
//!
//! Drive, Calendar, and Contacts call Google as one Workspace user — the
//! delegated user — through one service account the Workspace has authorized
//! for domain-wide delegation. No private key for that account exists: the
//! organization policy `iam.disableServiceAccountKeyCreation` forbids one. The
//! runtime identity (Workload Identity in a pod, Application Default
//! Credentials on a laptop) asks IAM Credentials to sign the delegation JWT
//! *as* the Workspace account, then trades the signed JWT for an access token
//! at Google's OAuth endpoint. The caller needs
//! `roles/iam.serviceAccountTokenCreator` on the Workspace account, which
//! `ops gcp setup` grants the deployment's runtime identity.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::Mutex;

use crate::workspace::{
    NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER, NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT,
};

const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
const IAM_CREDENTIALS_BASE_URL: &str = "https://iamcredentials.googleapis.com";
const OAUTH_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const JWT_BEARER_GRANT: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
/// Google accepts a delegation assertion valid for at most one hour.
const ASSERTION_LIFETIME: Duration = Duration::from_secs(3600);
/// Refresh this long before the access token's own expiry.
const REFRESH_MARGIN: Duration = Duration::from_secs(60);

/// The environment keys that declare the Workspace. Both or neither: a blank
/// value counts as absent, because `ops ship` renders an unconfigured row's
/// inline env as an empty string.
pub const WORKSPACE_DELEGATION_KEYS: [&str; 2] = [
    NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER,
    NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT,
];

#[derive(Debug, Error)]
pub enum DelegationError {
    #[error("missing Workspace configuration: {0}")]
    MissingConfig(String),
    #[error("Workspace delegation authentication failed")]
    Authentication,
}

/// The Workspace service account and the Workspace user it acts as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceDelegation {
    pub service_account: String,
    pub delegated_user: String,
}

impl WorkspaceDelegation {
    /// Whether any [`WORKSPACE_DELEGATION_KEYS`] has a value.
    pub fn declared<F>(get: F) -> bool
    where
        F: Fn(&str) -> Option<String>,
    {
        WORKSPACE_DELEGATION_KEYS
            .iter()
            .any(|key| get(key).is_some_and(|value| !value.trim().is_empty()))
    }

    /// Read both keys, naming every missing one at once.
    ///
    /// # Errors
    /// [`DelegationError::MissingConfig`] listing each absent key.
    pub fn from_lookup<F>(get: F) -> Result<Self, DelegationError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let value = |key: &str| get(key).filter(|value| !value.trim().is_empty());
        let [user_key, account_key] = WORKSPACE_DELEGATION_KEYS;
        match (value(user_key), value(account_key)) {
            (Some(delegated_user), Some(service_account)) => Ok(Self {
                service_account,
                delegated_user,
            }),
            (user, account) => Err(DelegationError::MissingConfig(
                [(user_key, user.is_none()), (account_key, account.is_none())]
                    .into_iter()
                    .filter_map(|(key, absent)| absent.then_some(key))
                    .collect::<Vec<_>>()
                    .join(", "),
            )),
        }
    }

    /// A token source for `scopes`, signed through the runtime identity.
    ///
    /// # Errors
    /// [`DelegationError::Authentication`] when no runtime credential exists.
    pub async fn token_source(
        &self,
        scopes: &[&str],
    ) -> Result<Arc<dyn google_cloud_token::TokenSource>, DelegationError> {
        let config =
            google_cloud_auth::project::Config::default().with_scopes(&[CLOUD_PLATFORM_SCOPE]);
        let provider = google_cloud_auth::token::DefaultTokenSourceProvider::new(config)
            .await
            .map_err(|_| DelegationError::Authentication)?;
        Ok(Arc::new(DelegatedTokenSource::new(
            self,
            scopes,
            google_cloud_token::TokenSourceProvider::token_source(&provider),
            IAM_CREDENTIALS_BASE_URL,
            OAUTH_TOKEN_URL,
        )))
    }
}

struct CachedToken {
    value: String,
    refresh_at: SystemTime,
}

/// Mints delegated access tokens without a key and caches each until shortly
/// before it expires.
struct DelegatedTokenSource {
    caller: Arc<dyn google_cloud_token::TokenSource>,
    http: reqwest::Client,
    service_account: String,
    delegated_user: String,
    scope: String,
    sign_jwt_url: String,
    token_url: String,
    cached: Mutex<Option<CachedToken>>,
}

impl std::fmt::Debug for DelegatedTokenSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DelegatedTokenSource")
            .field("service_account", &self.service_account)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SignJwtResponse {
    signed_jwt: String,
}

#[derive(Deserialize)]
struct AccessTokenResponse {
    access_token: String,
    expires_in: u64,
}

impl DelegatedTokenSource {
    fn new(
        delegation: &WorkspaceDelegation,
        scopes: &[&str],
        caller: Arc<dyn google_cloud_token::TokenSource>,
        iam_base_url: &str,
        token_url: &str,
    ) -> Self {
        let mut sign_jwt_url =
            url::Url::parse(iam_base_url).expect("IAM Credentials base URL parses");
        sign_jwt_url
            .path_segments_mut()
            .expect("IAM Credentials base URL has a path")
            .pop_if_empty()
            .extend([
                "v1",
                "projects",
                "-",
                "serviceAccounts",
                &format!("{}:signJwt", delegation.service_account),
            ]);
        Self {
            caller,
            http: reqwest::Client::new(),
            service_account: delegation.service_account.clone(),
            delegated_user: delegation.delegated_user.clone(),
            scope: scopes.join(" "),
            sign_jwt_url: sign_jwt_url.to_string(),
            token_url: token_url.to_string(),
            cached: Mutex::new(None),
        }
    }

    async fn mint(&self) -> Result<CachedToken, DelegationError> {
        let now = SystemTime::now();
        let issued = now
            .duration_since(UNIX_EPOCH)
            .map_err(|_| DelegationError::Authentication)?
            .as_secs();
        let claims = serde_json::json!({
            "iss": self.service_account,
            "sub": self.delegated_user,
            "scope": self.scope,
            "aud": self.token_url,
            "iat": issued,
            "exp": issued + ASSERTION_LIFETIME.as_secs(),
        });
        let caller = self
            .caller
            .token()
            .await
            .map_err(|_| DelegationError::Authentication)?;
        let signed: SignJwtResponse = self
            .http
            .post(&self.sign_jwt_url)
            .header(reqwest::header::AUTHORIZATION, caller)
            .json(&serde_json::json!({ "payload": claims.to_string() }))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|_| DelegationError::Authentication)?
            .json()
            .await
            .map_err(|_| DelegationError::Authentication)?;
        let token: AccessTokenResponse = self
            .http
            .post(&self.token_url)
            .form(&[
                ("grant_type", JWT_BEARER_GRANT),
                ("assertion", signed.signed_jwt.as_str()),
            ])
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|_| DelegationError::Authentication)?
            .json()
            .await
            .map_err(|_| DelegationError::Authentication)?;
        let lifetime = Duration::from_secs(token.expires_in).saturating_sub(REFRESH_MARGIN);
        Ok(CachedToken {
            value: format!("Bearer {}", token.access_token),
            refresh_at: now + lifetime,
        })
    }
}

#[async_trait]
impl google_cloud_token::TokenSource for DelegatedTokenSource {
    async fn token(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let mut cached = self.cached.lock().await;
        if let Some(token) = cached
            .as_ref()
            .filter(|token| SystemTime::now() < token.refresh_at)
        {
            return Ok(token.value.clone());
        }
        let token = self.mint().await?;
        let value = token.value.clone();
        *cached = Some(token);
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use google_cloud_token::TokenSource;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    use super::{DelegatedTokenSource, DelegationError, WorkspaceDelegation};

    const ACCOUNT: &str = "example-drive@example-project.iam.gserviceaccount.com";

    #[derive(Debug)]
    struct CallerToken;

    #[async_trait]
    impl TokenSource for CallerToken {
        async fn token(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok("Bearer runtime-identity".into())
        }
    }

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        move |key| {
            pairs
                .iter()
                .find(|(candidate, _)| candidate == key)
                .map(|(_, value)| value.clone())
        }
    }

    fn source(server: &MockServer) -> DelegatedTokenSource {
        DelegatedTokenSource::new(
            &WorkspaceDelegation {
                service_account: ACCOUNT.into(),
                delegated_user: "calendar@example.com".into(),
            },
            &["https://www.googleapis.com/auth/calendar"],
            Arc::new(CallerToken),
            &server.uri(),
            &format!("{}/token", server.uri()),
        )
    }

    #[test]
    fn blank_keys_are_undeclared_and_a_half_configuration_names_the_gap() {
        let blank = lookup(&[
            ("NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER", " "),
            ("NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT", ""),
        ]);
        assert!(!WorkspaceDelegation::declared(&blank));

        let half = lookup(&[(
            "NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER",
            "calendar@example.com",
        )]);
        assert!(WorkspaceDelegation::declared(&half));
        let Err(DelegationError::MissingConfig(missing)) = WorkspaceDelegation::from_lookup(&half)
        else {
            panic!("a half configuration is refused");
        };
        assert_eq!(missing, "NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT");
    }

    #[tokio::test]
    async fn signs_the_delegation_as_the_workspace_account_and_caches_the_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/v1/projects/-/serviceAccounts/{ACCOUNT}:signJwt"
            )))
            .and(header("authorization", "Bearer runtime-identity"))
            .respond_with(|request: &Request| {
                let body: serde_json::Value = request.body_json().unwrap();
                let claims: serde_json::Value =
                    serde_json::from_str(body["payload"].as_str().unwrap()).unwrap();
                assert_eq!(claims["iss"], ACCOUNT);
                assert_eq!(claims["sub"], "calendar@example.com");
                assert_eq!(claims["scope"], "https://www.googleapis.com/auth/calendar");
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "signedJwt": "signed.jwt.value" }))
            })
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("assertion=signed.jwt.value"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "access_token": "delegated", "expires_in": 3600 }),
            ))
            .expect(1)
            .mount(&server)
            .await;

        let source = source(&server);
        assert_eq!(source.token().await.unwrap(), "Bearer delegated");
        assert_eq!(source.token().await.unwrap(), "Bearer delegated");
    }

    #[tokio::test]
    async fn a_refused_signature_is_an_authentication_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        assert!(source(&server).token().await.is_err());
    }
}
