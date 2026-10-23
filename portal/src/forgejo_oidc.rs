//! Verify Forgejo Actions OIDC ID tokens from a trusted installation.
//!
//! Forgejo 15.0.9 signs the run claims used here with RS256 and publishes its
//! keys through issuer metadata. The issuer, audience, and workflow path come
//! from deployment configuration; repository identity is later joined to the
//! live Project's registered repository URL.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;
use url::Url;

use crate::auth::JwksDocument;
use crate::session::now_unix_secs;

const JWKS_TTL: Duration = Duration::from_secs(3600);
const JWKS_REFETCH_FLOOR: Duration = Duration::from_mins(5);
const TOKEN_CLOCK_LEEWAY_SECS: u64 = 30;
const MAX_TOKEN_AGE_SECS: i64 = 3600;

/// Claims emitted by Forgejo Actions v15.0.9.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ForgejoActionsClaims {
    pub sub: String,
    pub iss: String,
    pub aud: String,
    pub exp: i64,
    pub iat: i64,
    pub nbf: i64,
    pub actor: String,
    #[serde(default)]
    pub base_ref: String,
    pub event_name: String,
    #[serde(default)]
    pub head_ref: String,
    #[serde(rename = "ref")]
    pub git_ref: String,
    /// Forgejo v15 stringifies this boolean while constructing OIDC claims.
    pub ref_protected: String,
    pub ref_type: String,
    pub repository: String,
    pub repository_owner: String,
    pub run_attempt: String,
    pub run_id: String,
    pub run_number: String,
    pub sha: String,
    pub workflow: String,
    pub workflow_ref: String,
}

/// The deployment-trusted Forgejo issuer and its OIDC discovery cache.
#[derive(Clone)]
pub struct ForgejoOidc {
    inner: Arc<ForgejoOidcInner>,
    spent_token_digests: Arc<Mutex<HashMap<String, i64>>>,
}

enum ForgejoOidcInner {
    Reject,
    Jwks {
        issuer: String,
        audience: String,
        workflow_path: String,
        client: reqwest::Client,
        cache: Arc<AsyncMutex<Option<(Instant, JwksDocument)>>>,
        last_kid_refetch: Arc<AsyncMutex<Option<Instant>>>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ForgejoOidcError {
    #[error("this deployment does not accept Forgejo Actions OIDC tokens")]
    Rejected,
    #[error("Forgejo Actions OIDC token is missing")]
    Missing,
    #[error("Forgejo Actions OIDC token is invalid: {0}")]
    Invalid(String),
    #[error("Forgejo Actions OIDC token has already been exchanged")]
    Replayed,
}

#[derive(Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    jwks_uri: String,
}

impl ForgejoOidc {
    /// Read the explicit opt-in trust from deployment environment variables.
    /// Partial configuration disables the Forgejo token exchange.
    #[must_use]
    pub fn from_env() -> Self {
        let names = [
            "NAVIGATOR_FORGEJO_ACTIONS_OIDC_ISSUER",
            "NAVIGATOR_FORGEJO_ACTIONS_OIDC_AUDIENCE",
            "NAVIGATOR_FORGEJO_ACTIONS_OIDC_WORKFLOW",
        ];
        let config = names.map(std::env::var);
        if config.iter().all(Result::is_err) {
            return Self::rejecting();
        }
        let [Ok(issuer), Ok(audience), Ok(workflow_path)] = config else {
            tracing::error!("partial Forgejo Actions OIDC trust configuration; rejecting tokens");
            return Self::rejecting();
        };
        Self::configured(issuer, audience, workflow_path).unwrap_or_else(|error| {
            tracing::error!(%error, "invalid Forgejo Actions OIDC trust configuration; rejecting tokens");
            Self::rejecting()
        })
    }

    /// Configure the pinned Forgejo issuer, deployment audience, and trusted
    /// workflow path. Production issuers must use HTTPS.
    pub fn configured(
        issuer: impl Into<String>,
        audience: impl Into<String>,
        workflow_path: impl Into<String>,
    ) -> Result<Self, ForgejoOidcError> {
        Self::build(issuer.into(), audience.into(), workflow_path.into(), false)
    }

    /// Test constructor for a local discovery/JWKS server. It accepts HTTP
    /// only on loopback and cannot configure a non-loopback issuer.
    #[doc(hidden)]
    pub fn local_test_issuer(
        issuer: impl Into<String>,
        audience: impl Into<String>,
        workflow_path: impl Into<String>,
    ) -> Result<Self, ForgejoOidcError> {
        Self::build(issuer.into(), audience.into(), workflow_path.into(), true)
    }

    fn build(
        issuer: String,
        audience: String,
        workflow_path: String,
        allow_local_http: bool,
    ) -> Result<Self, ForgejoOidcError> {
        let issuer_url =
            Url::parse(&issuer).map_err(|error| ForgejoOidcError::Invalid(error.to_string()))?;
        if audience.trim().is_empty()
            || !workflow_path.starts_with(".forgejo/workflows/")
            || workflow_path.contains("..")
            || workflow_path.contains('@')
            || workflow_path.ends_with('/')
            || !issuer_url.username().is_empty()
            || issuer_url.password().is_some()
            || issuer_url.query().is_some()
            || issuer_url.fragment().is_some()
            || issuer_url.path().ends_with('/')
            || !issuer_url.path().ends_with("/api/actions")
            || !allowed_issuer_scheme(&issuer_url, allow_local_http)
        {
            return Err(ForgejoOidcError::Invalid(
                "issuer, audience, or trusted workflow configuration is invalid".into(),
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| ForgejoOidcError::Invalid(error.to_string()))?;
        Ok(Self {
            inner: Arc::new(ForgejoOidcInner::Jwks {
                issuer,
                audience,
                workflow_path,
                client,
                cache: Arc::new(AsyncMutex::new(None)),
                last_kid_refetch: Arc::new(AsyncMutex::new(None)),
            }),
            spent_token_digests: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Refuse all Forgejo tokens.
    #[must_use]
    pub fn rejecting() -> Self {
        Self {
            inner: Arc::new(ForgejoOidcInner::Reject),
            spent_token_digests: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Verify the token against the configured Forgejo discovery endpoint.
    pub async fn verify(&self, token: &str) -> Result<ForgejoActionsClaims, ForgejoOidcError> {
        if token.trim().is_empty() {
            return Err(ForgejoOidcError::Missing);
        }
        match &*self.inner {
            ForgejoOidcInner::Reject => Err(ForgejoOidcError::Rejected),
            ForgejoOidcInner::Jwks {
                issuer,
                audience,
                client,
                cache,
                last_kid_refetch,
                ..
            } => {
                let header = decode_header(token)
                    .map_err(|error| ForgejoOidcError::Invalid(error.to_string()))?;
                if header.alg != Algorithm::RS256 {
                    return Err(ForgejoOidcError::Invalid(
                        "Forgejo token must use RS256".into(),
                    ));
                }
                let kid = header
                    .kid
                    .ok_or_else(|| ForgejoOidcError::Invalid("token header has no kid".into()))?;
                let doc = load_jwks(issuer, client, cache, false).await?;
                let claims = decode_claims(token, issuer, audience, &kid, &doc);
                let claims = if let Ok(claims) = claims {
                    claims
                } else {
                    let mut last_refetch = last_kid_refetch.lock().await;
                    let should_refetch = last_refetch
                        .as_ref()
                        .is_none_or(|last| last.elapsed() >= JWKS_REFETCH_FLOOR);
                    if !should_refetch {
                        return Err(ForgejoOidcError::Invalid(
                            "token verification failed while key refresh is throttled".into(),
                        ));
                    }
                    *last_refetch = Some(Instant::now());
                    let doc = load_jwks(issuer, client, cache, true).await?;
                    decode_claims(token, issuer, audience, &kid, &doc)?
                };
                validate_claim_times(&claims)?;
                Ok(claims)
            }
        }
    }

    /// Enforce the deployment-owned workflow and event/ref contract.
    pub fn authorize_run(&self, claims: &ForgejoActionsClaims) -> Result<(), ForgejoOidcError> {
        let ForgejoOidcInner::Jwks { workflow_path, .. } = &*self.inner else {
            return Err(ForgejoOidcError::Rejected);
        };
        let Some(repository) = forgejo_repository(&claims.repository) else {
            return Err(invalid_claims());
        };
        let expected_workflow = format!("{repository}/{workflow_path}@{}", claims.git_ref);
        let workflow_file = workflow_path.rsplit('/').next().unwrap_or_default();
        let sha_valid =
            claims.sha.len() == 40 && claims.sha.bytes().all(|byte| byte.is_ascii_hexdigit());
        let run_fields_valid = !claims.run_id.is_empty()
            && claims.run_id.bytes().all(|byte| byte.is_ascii_digit())
            && claims
                .run_attempt
                .parse::<u32>()
                .is_ok_and(|attempt| attempt > 0);
        let subject_valid = match claims.event_name.as_str() {
            "push" | "workflow_dispatch" => {
                claims.git_ref == "refs/heads/main"
                    && claims.ref_type == "branch"
                    && claims.sub == format!("repo:{repository}:ref:{}", claims.git_ref)
            }
            // Forgejo 15.0.9 runs a pull request on its head ref,
            // `refs/pull/<n>/head` (`PullRequest.GetGitRefName`); it has no
            // GitHub-style `/merge` ref.
            "pull_request" => {
                let is_head_ref = claims
                    .git_ref
                    .strip_prefix("refs/pull/")
                    .and_then(|refname| refname.strip_suffix("/head"))
                    .is_some_and(|number| {
                        !number.is_empty()
                            && number.bytes().all(|byte| byte.is_ascii_digit())
                            && number.parse::<u64>().is_ok_and(|value| value > 0)
                    });
                claims.base_ref == "main"
                    && is_head_ref
                    && claims.sub == format!("repo:{repository}:pull_request")
            }
            _ => false,
        };
        let actual_workflow = claims.workflow_ref.as_str();
        let workflow_path_matches = if claims.event_name == "pull_request" {
            let suffix = actual_workflow
                .rsplit_once('@')
                .map(|(_, reference)| reference);
            suffix == Some(claims.git_ref.as_str())
                && actual_workflow
                    .strip_suffix(&format!("@{}", claims.git_ref))
                    .is_some_and(|path| path == format!("{repository}/{workflow_path}"))
        } else {
            actual_workflow == expected_workflow
        };
        if claims.repository_owner != repository.split('/').next().unwrap_or_default()
            || claims.workflow != workflow_file
            || !workflow_path_matches
            || !sha_valid
            || !run_fields_valid
            || !subject_valid
        {
            return Err(invalid_claims());
        }
        Ok(())
    }

    /// Spend the exact verified JWT until its signed expiry. Forgejo v15 has
    /// no `jti`; the SHA-256 token digest distinguishes legitimate renewed
    /// tokens issued in the same run and attempt.
    pub fn spend_token(&self, token: &str, exp: i64) -> Result<(), ForgejoOidcError> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let digest = Sha256::digest(token.as_bytes())
            .iter()
            .flat_map(|byte| {
                [
                    char::from(HEX[usize::from(byte >> 4)]),
                    char::from(HEX[usize::from(byte & 0x0f)]),
                ]
            })
            .collect::<String>();
        let now = now_unix_secs();
        let mut spent = self
            .spent_token_digests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        spent.retain(|_, until| *until > now);
        if spent.get(&digest).is_some_and(|until| *until > now) {
            return Err(ForgejoOidcError::Replayed);
        }
        spent.insert(digest, exp);
        Ok(())
    }
}

fn invalid_claims() -> ForgejoOidcError {
    ForgejoOidcError::Invalid("Forgejo run does not match the trusted workflow".into())
}

fn forgejo_repository(repository: &str) -> Option<String> {
    let (owner, name) = repository.split_once('/')?;
    if owner.is_empty()
        || name.is_empty()
        || name.contains('/')
        || repository.contains(['?', '#', '@', '\\'])
        || owner.bytes().any(|byte| byte.is_ascii_whitespace())
        || name.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

fn validate_claim_times(claims: &ForgejoActionsClaims) -> Result<(), ForgejoOidcError> {
    let now = now_unix_secs();
    if claims.exp <= now
        || claims.iat <= 0
        || claims.iat > now + TOKEN_CLOCK_LEEWAY_SECS.cast_signed()
        || claims.nbf > now + TOKEN_CLOCK_LEEWAY_SECS.cast_signed()
        || claims.exp <= claims.iat
        || claims.exp - claims.iat > MAX_TOKEN_AGE_SECS
    {
        return Err(ForgejoOidcError::Invalid(
            "token timestamps are outside the accepted window".into(),
        ));
    }
    Ok(())
}

async fn load_jwks(
    issuer: &str,
    client: &reqwest::Client,
    cache: &AsyncMutex<Option<(Instant, JwksDocument)>>,
    force: bool,
) -> Result<JwksDocument, ForgejoOidcError> {
    if !force {
        let guard = cache.lock().await;
        if let Some((fetched_at, doc)) = guard.as_ref() {
            if fetched_at.elapsed() < JWKS_TTL {
                return Ok(doc.clone());
            }
        }
    }
    let discovery_url = format!("{issuer}/.well-known/openid-configuration");
    let issuer_url =
        Url::parse(issuer).map_err(|error| ForgejoOidcError::Invalid(error.to_string()))?;
    let discovery: DiscoveryDocument = client
        .get(&discovery_url)
        .send()
        .await
        .map_err(|error| ForgejoOidcError::Invalid(error.to_string()))?
        .error_for_status()
        .map_err(|error| ForgejoOidcError::Invalid(error.to_string()))?
        .json()
        .await
        .map_err(|error| ForgejoOidcError::Invalid(error.to_string()))?;
    let jwks_url = Url::parse(&discovery.jwks_uri)
        .map_err(|error| ForgejoOidcError::Invalid(error.to_string()))?;
    if discovery.issuer != issuer
        || !same_origin(&issuer_url, &jwks_url)
        || discovery.jwks_uri != format!("{issuer}/.well-known/keys")
        || !jwks_url.username().is_empty()
        || jwks_url.password().is_some()
        || jwks_url.query().is_some()
        || jwks_url.fragment().is_some()
    {
        return Err(ForgejoOidcError::Invalid(
            "Forgejo discovery metadata is not bound to the trusted issuer".into(),
        ));
    }
    let is_local_http = issuer_url.scheme() == "http";
    if (!is_local_http && jwks_url.scheme() != "https")
        || (is_local_http && jwks_url.scheme() != "http")
    {
        return Err(ForgejoOidcError::Invalid(
            "Forgejo JWKS URL uses an unsupported scheme".into(),
        ));
    }
    let doc: JwksDocument = client
        .get(jwks_url)
        .send()
        .await
        .map_err(|error| ForgejoOidcError::Invalid(error.to_string()))?
        .error_for_status()
        .map_err(|error| ForgejoOidcError::Invalid(error.to_string()))?
        .json()
        .await
        .map_err(|error| ForgejoOidcError::Invalid(error.to_string()))?;
    *cache.lock().await = Some((Instant::now(), doc.clone()));
    Ok(doc)
}

fn decode_claims(
    token: &str,
    issuer: &str,
    audience: &str,
    kid: &str,
    doc: &JwksDocument,
) -> Result<ForgejoActionsClaims, ForgejoOidcError> {
    let jwk = doc
        .keys
        .iter()
        .find(|key| key.kid.as_deref() == Some(kid) && key.kty == "RSA")
        .ok_or_else(|| ForgejoOidcError::Invalid("no RSA JWKS entry for token kid".into()))?;
    if jwk.alg.as_deref().is_some_and(|alg| alg != "RS256") {
        return Err(ForgejoOidcError::Invalid(
            "Forgejo JWKS key is not declared for RS256".into(),
        ));
    }
    let n = jwk
        .n
        .as_deref()
        .ok_or_else(|| ForgejoOidcError::Invalid("JWKS key is missing n".into()))?;
    let e = jwk
        .e
        .as_deref()
        .ok_or_else(|| ForgejoOidcError::Invalid("JWKS key is missing e".into()))?;
    let key = DecodingKey::from_rsa_components(n, e)
        .map_err(|error| ForgejoOidcError::Invalid(error.to_string()))?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&[audience]);
    validation.set_issuer(&[issuer]);
    validation.leeway = TOKEN_CLOCK_LEEWAY_SECS;
    decode::<ForgejoActionsClaims>(token, &key, &validation)
        .map(|token| token.claims)
        .map_err(|error| ForgejoOidcError::Invalid(error.to_string()))
}

fn allowed_issuer_scheme(url: &Url, allow_local_http: bool) -> bool {
    if url.scheme() == "https" {
        return true;
    }
    allow_local_http
        && url.scheme() == "http"
        && matches!(url.host_str(), Some("127.0.0.1" | "::1" | "localhost"))
        && url.port().is_some()
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn forgejo_root(issuer: &Url) -> Result<Url, ForgejoOidcError> {
    let path = issuer.path().trim_end_matches('/');
    let root_path = path
        .strip_suffix("/api/actions")
        .ok_or_else(|| ForgejoOidcError::Invalid("Forgejo issuer path is invalid".into()))?;
    let mut root = issuer.clone();
    root.set_path(if root_path.is_empty() { "/" } else { root_path });
    Ok(root)
}

/// Match a registered repository URL against the issuer root and signed
/// repository path. Both the installation root and `owner/name` must match.
#[must_use]
pub fn repository_matches_issuer(repository_url: &str, issuer: &str, repository: &str) -> bool {
    let (Ok(issuer_url), Ok(repository_url)) = (Url::parse(issuer), Url::parse(repository_url))
    else {
        return false;
    };
    let (Ok(root), Some(repository)) = (forgejo_root(&issuer_url), forgejo_repository(repository))
    else {
        return false;
    };
    if repository_url.scheme() != "https"
        || root.host_str() != repository_url.host_str()
        || root.port_or_known_default() != repository_url.port_or_known_default()
        || !repository_url.username().is_empty()
        || repository_url.password().is_some()
        || repository_url.query().is_some()
        || repository_url.fragment().is_some()
    {
        return false;
    }
    let expected_path = format!("{}/{}", root.path().trim_end_matches('/'), repository);
    let actual_path = repository_url.path().trim_end_matches('/');
    actual_path == expected_path || actual_path.strip_suffix(".git") == Some(expected_path.as_str())
}

#[cfg(test)]
mod tests {
    use super::{forgejo_repository, repository_matches_issuer, ForgejoOidc};

    #[test]
    fn repository_matching_binds_forge_host_root_and_owner_name() {
        let issuer = "https://forgejo.example/firm/api/actions";
        assert!(repository_matches_issuer(
            "https://forgejo.example/firm/firm/acme.git",
            issuer,
            "firm/acme"
        ));
        assert!(!repository_matches_issuer(
            "https://other.example/firm/firm/acme.git",
            issuer,
            "firm/acme"
        ));
        assert!(!repository_matches_issuer(
            "https://forgejo.example/other-root/firm/acme.git",
            issuer,
            "firm/acme"
        ));
        assert!(!repository_matches_issuer(
            "https://forgejo.example/firm/firm/other.git",
            issuer,
            "firm/acme"
        ));
    }

    #[test]
    fn repository_claim_must_be_exactly_owner_and_name() {
        assert_eq!(forgejo_repository("firm/acme"), Some("firm/acme".into()));
        assert_eq!(forgejo_repository("firm/acme/extra"), None);
        assert_eq!(forgejo_repository("/acme"), None);
    }

    #[tokio::test]
    async fn missing_forgejo_trust_rejects_by_default() {
        assert!(ForgejoOidc::rejecting().verify("synthetic").await.is_err());
    }
}
