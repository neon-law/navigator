//! A Firm user's SSH public keys on the installation's Forgejo, reconciled
//! from Navigator (ENG-1219).
//!
//! Navigator holds the authoritative list of a Person's SSH public keys
//! (`store::ssh_public_keys`); this module makes one Forgejo account's keys
//! match it. The client speaks only to the admin user-key endpoints
//! (`POST /api/v1/admin/users/{username}/keys`,
//! `DELETE /api/v1/admin/users/{username}/keys/{id}`), the public key list
//! (`GET /api/v1/users/{username}/keys`), the admin user list (to find the
//! account by its exact email), and `GET /api/v1/user` (to prove which
//! identity holds the token).
//!
//! The credential belongs to a dedicated key-sync service identity, never the
//! reserved `navigator-admin` seat or an operator's own account: every
//! reconciliation first reads `GET /api/v1/user` and refuses unless the token
//! answers as the configured service login. Forgejo 15.0.9 serves the admin
//! key endpoints only to a site administrator, so the account carries that
//! flag; its token is scoped to `write:admin` and `read:user` alone, with no
//! repository or organization scope, so it cannot reach a repository merge
//! (see `docs/forgejo-deployment.md#ssh-public-keys`).
//!
//! Reconciliation is idempotent and conservative. A key Navigator added is
//! titled with [`MANAGED_TITLE_PREFIX`]; only such keys are ever removed. A
//! desired key already on the account, under any title, is left alone, so a
//! rerun writes nothing and no key is ever duplicated. A key on the account
//! that Navigator neither holds nor manages is drift: it is reported by
//! fingerprint and never silently removed. Keys compare by their OpenSSH
//! SHA-256 fingerprint, computed here from the key body rather than trusted
//! from the forge's reply.

use std::collections::BTreeSet;
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::forgejo_forge::NAVIGATOR_FORGEJO_ROOT_URL_ENV;

/// The Forgejo login of the dedicated key-sync service identity.
pub const NAVIGATOR_FORGEJO_KEY_SYNC_LOGIN_ENV: &str = "NAVIGATOR_FORGEJO_KEY_SYNC_LOGIN";
/// The key-sync service identity's access token (`write:admin`, `read:user`).
pub const NAVIGATOR_FORGEJO_KEY_SYNC_TOKEN_ENV: &str = "NAVIGATOR_FORGEJO_KEY_SYNC_TOKEN";
/// Every key Navigator writes carries this title prefix; only such keys are
/// ever removed by reconciliation.
pub const MANAGED_TITLE_PREFIX: &str = "navigator:";
/// The seat `k8s/forgejo/identity.sh` reserves as Forgejo's first site
/// administrator. Nobody signs in as it, and the key sync never runs as it.
pub const RESERVED_ADMIN_LOGIN: &str = "navigator-admin";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const USER_AGENT: &str = concat!("neon-law-navigator/", env!("CARGO_PKG_VERSION"));
const PAGE_LIMIT: usize = 50;
/// A bound on every paged read, so a misbehaving forge cannot hold a sync open.
const MAX_PAGES: usize = 200;
/// Forgejo's key title column is generous, but a short title reads well in
/// its settings page; the label is cut to fit.
const TITLE_MAX_CHARS: usize = 50;

/// Why a key reconciliation could not run or finish.
#[derive(Debug, thiserror::Error)]
pub enum KeySyncError {
    #[error("Forgejo key sync is misconfigured: {0}")]
    InvalidConfig(&'static str),
    #[error("the key-sync token answers as a different Forgejo identity")]
    WrongIdentity,
    #[error("Forgejo rejected the key-sync credential")]
    Authentication,
    #[error("Forgejo request failed while {action}")]
    Request {
        action: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("Forgejo answered {status} while {action}")]
    Api { action: &'static str, status: u16 },
    #[error("Forgejo's reply could not be read while {action}")]
    Response {
        action: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("more than one Forgejo account carries this email address")]
    AmbiguousAccount,
    #[error("Forgejo paged past the read bound while {action}")]
    TooManyPages { action: &'static str },
}

/// One key Navigator wants on the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredKey {
    /// The canonical `<algorithm> <base64>` line, no comment.
    pub public_key: String,
    /// The OpenSSH `SHA256:` fingerprint of `public_key`.
    pub fingerprint: String,
    /// The Person's label for the key.
    pub label: String,
}

/// One key the forge reports on the account.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ForgeKey {
    pub id: i64,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub fingerprint: String,
}

impl ForgeKey {
    /// The fingerprint computed from the key body; the forge's own field only
    /// when the body cannot be read.
    fn computed_fingerprint(&self) -> String {
        fingerprint_of_line(&self.key).unwrap_or_else(|| self.fingerprint.clone())
    }

    fn is_managed(&self) -> bool {
        self.title.starts_with(MANAGED_TITLE_PREFIX)
    }
}

/// What reconciliation will do (or, in a dry run, would do) to one account.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyPlan {
    /// Desired keys missing from the account.
    pub add: Vec<DesiredKey>,
    /// Navigator-managed keys on the account that Navigator no longer holds,
    /// by forge key id and fingerprint.
    pub remove: Vec<(i64, String)>,
    /// Keys on the account Navigator neither holds nor manages, by
    /// fingerprint. Reported, never removed.
    pub drift: Vec<String>,
}

/// The outcome of one account's reconciliation, by fingerprint only — no key
/// bodies and no titles, which a person may have written freely.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct KeyReconciliation {
    pub forgejo_login: String,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub drift: Vec<String>,
    pub dry_run: bool,
}

/// Compare what Navigator wants with what the forge holds.
#[must_use]
pub fn plan(desired: &[DesiredKey], current: &[ForgeKey]) -> KeyPlan {
    let wanted: BTreeSet<&str> = desired.iter().map(|key| key.fingerprint.as_str()).collect();
    let mut present = BTreeSet::new();
    let mut plan = KeyPlan::default();
    for key in current {
        let fingerprint = key.computed_fingerprint();
        let first = present.insert(fingerprint.clone());
        if wanted.contains(fingerprint.as_str()) {
            // A second managed copy of a wanted key is a duplicate to drop;
            // Forgejo refuses duplicate content, so this is defensive.
            if !first && key.is_managed() {
                plan.remove.push((key.id, fingerprint));
            }
        } else if key.is_managed() {
            plan.remove.push((key.id, fingerprint));
        } else {
            plan.drift.push(fingerprint);
        }
    }
    let mut queued = BTreeSet::new();
    for key in desired {
        if !present.contains(&key.fingerprint) && queued.insert(key.fingerprint.clone()) {
            plan.add.push(key.clone());
        }
    }
    plan
}

/// The OpenSSH SHA-256 fingerprint (`SHA256:<unpadded base64>`) of a decoded
/// public-key blob — the same spelling `ssh-keygen -l` prints.
#[must_use]
pub fn fingerprint_of_blob(blob: &[u8]) -> String {
    let digest = Sha256::digest(blob);
    format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
    )
}

/// The fingerprint of an `<algorithm> <base64> [comment]` line, or `None`
/// when its body is not base64.
#[must_use]
pub fn fingerprint_of_line(line: &str) -> Option<String> {
    let body = line.split_whitespace().nth(1)?;
    let blob = base64::engine::general_purpose::STANDARD
        .decode(body)
        .ok()?;
    Some(fingerprint_of_blob(&blob))
}

/// The title a managed key carries on the forge: the prefix plus the
/// Person's label, cut to fit.
#[must_use]
pub fn managed_title(label: &str) -> String {
    let room = TITLE_MAX_CHARS - MANAGED_TITLE_PREFIX.len();
    let label: String = label.trim().chars().take(room).collect();
    format!("{MANAGED_TITLE_PREFIX}{label}")
}

/// A Forgejo login is safe to place in a URL path only when it is the plain
/// alphabet Forgejo itself allows.
fn is_valid_login(login: &str) -> bool {
    !login.is_empty()
        && login.len() <= 40
        && login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

#[derive(Deserialize)]
struct UserBody {
    #[serde(default)]
    login: String,
    #[serde(default)]
    email: String,
}

#[derive(Serialize)]
struct CreateKey<'a> {
    key: &'a str,
    title: &'a str,
    read_only: bool,
}

/// The key-sync service identity's client on one Forgejo installation.
#[derive(Clone)]
pub struct ForgejoUserKeys {
    api_base: String,
    login: String,
    token: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for ForgejoUserKeys {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ForgejoUserKeys")
            .field("api_base", &self.api_base)
            .field("login", &self.login)
            .field("token", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl ForgejoUserKeys {
    /// A client for the service identity `login` on the Forgejo at
    /// `root_url`.
    ///
    /// # Errors
    /// [`KeySyncError::InvalidConfig`] when the root URL is not `https` (or
    /// `http` on a loopback test host), the login is malformed or is the
    /// reserved administrator seat, or the token is blank.
    pub fn new(root_url: &str, login: &str, token: &str) -> Result<Self, KeySyncError> {
        let root = root_url.trim().trim_end_matches('/');
        let loopback = root.starts_with("http://127.0.0.1") || root.starts_with("http://localhost");
        if !(root.starts_with("https://") || loopback) {
            return Err(KeySyncError::InvalidConfig(NAVIGATOR_FORGEJO_ROOT_URL_ENV));
        }
        let login = login.trim();
        if !is_valid_login(login) {
            return Err(KeySyncError::InvalidConfig(
                NAVIGATOR_FORGEJO_KEY_SYNC_LOGIN_ENV,
            ));
        }
        if login.eq_ignore_ascii_case(RESERVED_ADMIN_LOGIN) {
            return Err(KeySyncError::InvalidConfig(
                "the reserved navigator-admin seat cannot run the key sync",
            ));
        }
        if token.trim().is_empty() {
            return Err(KeySyncError::InvalidConfig(
                NAVIGATOR_FORGEJO_KEY_SYNC_TOKEN_ENV,
            ));
        }
        Ok(Self {
            api_base: format!("{root}/api/v1"),
            login: login.to_string(),
            token: token.trim().to_string(),
            http: reqwest::Client::new(),
        })
    }

    /// Build the client from the environment. `Ok(None)` when the deployment
    /// has not configured the key sync at all (any of the three settings
    /// unset or blank); a configured but invalid value is an error.
    ///
    /// # Errors
    /// As [`Self::new`].
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, KeySyncError> {
        let value = |name: &str| get(name).filter(|value| !value.trim().is_empty());
        match (
            value(NAVIGATOR_FORGEJO_ROOT_URL_ENV),
            value(NAVIGATOR_FORGEJO_KEY_SYNC_LOGIN_ENV),
            value(NAVIGATOR_FORGEJO_KEY_SYNC_TOKEN_ENV),
        ) {
            (Some(root), Some(login), Some(token)) => Self::new(&root, &login, &token).map(Some),
            _ => Ok(None),
        }
    }

    /// [`Self::from_lookup`] over the process environment.
    ///
    /// # Errors
    /// As [`Self::new`].
    pub fn from_env() -> Result<Option<Self>, KeySyncError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn request(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        builder
            .header(
                reqwest::header::AUTHORIZATION,
                format!("token {}", self.token),
            )
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .timeout(REQUEST_TIMEOUT)
    }

    fn checked(
        response: Result<reqwest::Response, reqwest::Error>,
        action: &'static str,
    ) -> Result<reqwest::Response, KeySyncError> {
        let response = response.map_err(|source| KeySyncError::Request { action, source })?;
        match response.status().as_u16() {
            200..=299 => Ok(response),
            401 | 403 => Err(KeySyncError::Authentication),
            status => Err(KeySyncError::Api { action, status }),
        }
    }

    async fn json<T: serde::de::DeserializeOwned>(
        response: reqwest::Response,
        action: &'static str,
    ) -> Result<T, KeySyncError> {
        response
            .json::<T>()
            .await
            .map_err(|source| KeySyncError::Response { action, source })
    }

    /// Refuse unless the token answers as the configured service login.
    ///
    /// # Errors
    /// [`KeySyncError::WrongIdentity`] for any other identity, including the
    /// reserved administrator seat; transport and API errors otherwise.
    pub async fn verify_identity(&self) -> Result<(), KeySyncError> {
        let action = "reading the key-sync identity";
        let url = format!("{}/user", self.api_base);
        let response = Self::checked(self.request(self.http.get(url)).send().await, action)?;
        let user: UserBody = Self::json(response, action).await?;
        if user.login != self.login || user.login.eq_ignore_ascii_case(RESERVED_ADMIN_LOGIN) {
            return Err(KeySyncError::WrongIdentity);
        }
        Ok(())
    }

    /// The login of the one Forgejo account whose email is exactly `email`
    /// (compared without case), or `None` when there is no such account yet —
    /// the Person has not signed in to Forgejo.
    ///
    /// # Errors
    /// [`KeySyncError::AmbiguousAccount`] when more than one account matches.
    pub async fn find_login_by_email(&self, email: &str) -> Result<Option<String>, KeySyncError> {
        let action = "listing Forgejo accounts";
        let wanted = email.trim();
        let mut found: Option<String> = None;
        for page in 1..=MAX_PAGES {
            let url = format!(
                "{}/admin/users?page={page}&limit={PAGE_LIMIT}",
                self.api_base
            );
            let response = Self::checked(self.request(self.http.get(url)).send().await, action)?;
            let users: Vec<UserBody> = Self::json(response, action).await?;
            for user in &users {
                if user.email.trim().eq_ignore_ascii_case(wanted) {
                    if found.is_some() || !is_valid_login(&user.login) {
                        return Err(KeySyncError::AmbiguousAccount);
                    }
                    found = Some(user.login.clone());
                }
            }
            if users.len() < PAGE_LIMIT {
                return Ok(found);
            }
        }
        Err(KeySyncError::TooManyPages { action })
    }

    /// Every SSH key on `login`'s account.
    ///
    /// # Errors
    /// Transport, API, and paging errors.
    pub async fn list_keys(&self, login: &str) -> Result<Vec<ForgeKey>, KeySyncError> {
        let action = "listing an account's SSH keys";
        if !is_valid_login(login) {
            return Err(KeySyncError::InvalidConfig("Forgejo account login"));
        }
        let mut keys = Vec::new();
        for page in 1..=MAX_PAGES {
            let url = format!(
                "{}/users/{login}/keys?page={page}&limit={PAGE_LIMIT}",
                self.api_base
            );
            let response = Self::checked(self.request(self.http.get(url)).send().await, action)?;
            let batch: Vec<ForgeKey> = Self::json(response, action).await?;
            let last = batch.len() < PAGE_LIMIT;
            keys.extend(batch);
            if last {
                return Ok(keys);
            }
        }
        Err(KeySyncError::TooManyPages { action })
    }

    /// Add one key to `login`'s account with a managed title.
    ///
    /// # Errors
    /// Transport and API errors; Forgejo answers 422 for a key already on
    /// another account.
    pub async fn add_key(&self, login: &str, key: &DesiredKey) -> Result<(), KeySyncError> {
        let action = "adding an SSH key";
        if !is_valid_login(login) {
            return Err(KeySyncError::InvalidConfig("Forgejo account login"));
        }
        let url = format!("{}/admin/users/{login}/keys", self.api_base);
        let title = managed_title(&key.label);
        let body = CreateKey {
            key: &key.public_key,
            title: &title,
            read_only: false,
        };
        Self::checked(
            self.request(self.http.post(url)).json(&body).send().await,
            action,
        )?;
        Ok(())
    }

    /// Remove one key from `login`'s account by its forge id.
    ///
    /// # Errors
    /// Transport and API errors. A key already gone (404) is success.
    pub async fn delete_key(&self, login: &str, id: i64) -> Result<(), KeySyncError> {
        let action = "removing an SSH key";
        if !is_valid_login(login) {
            return Err(KeySyncError::InvalidConfig("Forgejo account login"));
        }
        let url = format!("{}/admin/users/{login}/keys/{id}", self.api_base);
        match self.request(self.http.delete(url)).send().await {
            Ok(response) if response.status().as_u16() == 404 => Ok(()),
            other => Self::checked(other, action).map(|_| ()),
        }
    }

    /// Make the account whose email is `email` hold exactly `desired` among
    /// its Navigator-managed keys. `Ok(None)` when no Forgejo account carries
    /// that email yet. With `dry_run`, reads only and reports the plan.
    ///
    /// # Errors
    /// Identity, transport, and API errors. A failure part-way leaves the
    /// account between states; the next run converges it.
    pub async fn reconcile(
        &self,
        email: &str,
        desired: &[DesiredKey],
        dry_run: bool,
    ) -> Result<Option<KeyReconciliation>, KeySyncError> {
        self.verify_identity().await?;
        let Some(login) = self.find_login_by_email(email).await? else {
            return Ok(None);
        };
        let current = self.list_keys(&login).await?;
        let plan = plan(desired, &current);
        if !dry_run {
            for (id, _) in &plan.remove {
                self.delete_key(&login, *id).await?;
            }
            for key in &plan.add {
                self.add_key(&login, key).await?;
            }
        }
        Ok(Some(KeyReconciliation {
            forgejo_login: login,
            added: plan.add.into_iter().map(|key| key.fingerprint).collect(),
            removed: plan
                .remove
                .into_iter()
                .map(|(_, fingerprint)| fingerprint)
                .collect(),
            drift: plan.drift,
            dry_run,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Synthetic ed25519 bodies: a 32-byte point of one repeated byte.
    fn ed25519(fill: u8) -> String {
        let mut blob = Vec::new();
        for part in [b"ssh-ed25519".as_slice(), &[fill; 32]] {
            blob.extend_from_slice(&u32::try_from(part.len()).unwrap().to_be_bytes());
            blob.extend_from_slice(part);
        }
        format!(
            "ssh-ed25519 {}",
            base64::engine::general_purpose::STANDARD.encode(blob)
        )
    }

    fn desired(fill: u8, label: &str) -> DesiredKey {
        let public_key = ed25519(fill);
        DesiredKey {
            fingerprint: fingerprint_of_line(&public_key).unwrap(),
            public_key,
            label: label.into(),
        }
    }

    fn forge_key(id: i64, fill: u8, title: &str) -> ForgeKey {
        ForgeKey {
            id,
            key: ed25519(fill),
            title: title.into(),
            fingerprint: String::new(),
        }
    }

    #[test]
    fn fingerprint_matches_the_openssh_spelling() {
        // `ssh-keygen -lf` over the all-zero synthetic ed25519 key.
        let line = ed25519(0);
        let fingerprint = fingerprint_of_line(&line).unwrap();
        assert!(fingerprint.starts_with("SHA256:"));
        assert!(
            !fingerprint.ends_with('='),
            "unpadded, as ssh-keygen prints"
        );
        assert_eq!(fingerprint.len(), "SHA256:".len() + 43);
        assert_eq!(
            fingerprint_of_line(&format!("{line} a-comment")),
            Some(fingerprint),
            "a comment does not change the fingerprint"
        );
        assert_eq!(fingerprint_of_line("ssh-ed25519 not*base64"), None);
    }

    #[test]
    fn plan_adds_missing_removes_stale_managed_and_reports_unmanaged() {
        let want = [desired(1, "laptop"), desired(2, "desktop")];
        let current = [
            forge_key(10, 1, "navigator:laptop"),
            forge_key(11, 3, "navigator:old"),
            forge_key(12, 4, "added by hand"),
        ];
        let plan = plan(&want, &current);
        assert_eq!(plan.add, vec![want[1].clone()]);
        assert_eq!(
            plan.remove,
            vec![(11, fingerprint_of_line(&ed25519(3)).unwrap())]
        );
        assert_eq!(plan.drift, vec![fingerprint_of_line(&ed25519(4)).unwrap()]);
    }

    #[test]
    fn plan_is_a_no_op_once_converged_whatever_the_title() {
        let want = [desired(1, "laptop"), desired(2, "desktop")];
        let current = [
            forge_key(10, 1, "navigator:laptop"),
            // A wanted key someone added by hand is present, not drift.
            forge_key(11, 2, "my desktop"),
        ];
        assert_eq!(plan(&want, &current), KeyPlan::default());
    }

    #[test]
    fn plan_never_duplicates_and_drops_a_managed_duplicate() {
        let want = [desired(1, "laptop"), desired(1, "laptop again")];
        let plan_empty = plan(&want, &[]);
        assert_eq!(plan_empty.add.len(), 1, "one add per fingerprint");
        let current = [
            forge_key(10, 1, "navigator:laptop"),
            forge_key(11, 1, "navigator:laptop"),
        ];
        let plan = plan(&want, &current);
        assert!(plan.add.is_empty());
        assert_eq!(plan.remove.len(), 1);
        assert_eq!(plan.remove[0].0, 11);
    }

    #[test]
    fn nothing_desired_removes_every_managed_key_and_only_those() {
        let current = [
            forge_key(10, 1, "navigator:laptop"),
            forge_key(12, 4, "added by hand"),
        ];
        let plan = plan(&[], &current);
        assert_eq!(plan.remove.len(), 1);
        assert_eq!(plan.remove[0].0, 10);
        assert_eq!(plan.drift.len(), 1);
    }

    #[test]
    fn managed_title_fits_and_keeps_the_prefix() {
        assert_eq!(managed_title(" laptop "), "navigator:laptop");
        let long = managed_title(&"x".repeat(200));
        assert!(long.starts_with(MANAGED_TITLE_PREFIX));
        assert_eq!(long.chars().count(), TITLE_MAX_CHARS);
    }

    #[test]
    fn configuration_refuses_the_reserved_seat_plain_http_and_blank_tokens() {
        assert!(
            ForgejoUserKeys::new("https://forge.example.test", "navigator-key-sync", "t").is_ok()
        );
        assert!(
            ForgejoUserKeys::new("http://forge.example.test", "navigator-key-sync", "t").is_err()
        );
        assert!(
            ForgejoUserKeys::new("https://forge.example.test", "navigator-admin", "t").is_err()
        );
        assert!(ForgejoUserKeys::new("https://forge.example.test", "a/b", "t").is_err());
        assert!(
            ForgejoUserKeys::new("https://forge.example.test", "navigator-key-sync", " ").is_err()
        );
        let unset = ForgejoUserKeys::from_lookup(|_| None).unwrap();
        assert!(unset.is_none(), "an unconfigured deployment skips the sync");
        let partial = ForgejoUserKeys::from_lookup(|name| {
            (name == NAVIGATOR_FORGEJO_ROOT_URL_ENV).then(|| "https://forge.example.test".into())
        })
        .unwrap();
        assert!(partial.is_none());
    }

    #[test]
    fn debug_redacts_the_token() {
        let client = ForgejoUserKeys::new(
            "https://forge.example.test",
            "navigator-key-sync",
            "secret-token",
        )
        .unwrap();
        assert!(!format!("{client:?}").contains("secret-token"));
    }

    async fn identity(server: &MockServer, login: &str) {
        Mock::given(method("GET"))
            .and(path("/api/v1/user"))
            .and(header("authorization", "token synthetic-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"login": login})))
            .mount(server)
            .await;
    }

    async fn accounts(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/api/v1/admin/users"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"login": "someone-else", "email": "other@example.test"},
                {"login": "synthetic-lawyer", "email": "Synthetic.Lawyer@example.test"},
            ])))
            .mount(server)
            .await;
    }

    fn client(server: &MockServer) -> ForgejoUserKeys {
        ForgejoUserKeys::new(&server.uri(), "navigator-key-sync", "synthetic-token").unwrap()
    }

    #[tokio::test]
    async fn reconcile_adds_missing_and_removes_stale_managed_keys() {
        let server = MockServer::start().await;
        identity(&server, "navigator-key-sync").await;
        accounts(&server).await;
        let stale = ed25519(3);
        let unmanaged = ed25519(4);
        Mock::given(method("GET"))
            .and(path("/api/v1/users/synthetic-lawyer/keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"id": 11, "key": stale, "title": "navigator:old"},
                {"id": 12, "key": unmanaged, "title": "by hand"},
            ])))
            .mount(&server)
            .await;
        let want = desired(1, "laptop");
        Mock::given(method("POST"))
            .and(path("/api/v1/admin/users/synthetic-lawyer/keys"))
            .and(body_partial_json(json!({
                "key": want.public_key,
                "title": "navigator:laptop",
                "read_only": false,
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 13})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/admin/users/synthetic-lawyer/keys/11"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/admin/users/synthetic-lawyer/keys/12"))
            .respond_with(ResponseTemplate::new(204))
            .expect(0)
            .mount(&server)
            .await;

        let report = client(&server)
            .reconcile(
                "synthetic.lawyer@example.test",
                std::slice::from_ref(&want),
                false,
            )
            .await
            .unwrap()
            .expect("the account exists");
        assert_eq!(report.forgejo_login, "synthetic-lawyer");
        assert_eq!(report.added, vec![want.fingerprint]);
        assert_eq!(report.removed, vec![fingerprint_of_line(&stale).unwrap()]);
        assert_eq!(report.drift, vec![fingerprint_of_line(&unmanaged).unwrap()]);
    }

    #[tokio::test]
    async fn a_converged_account_and_a_dry_run_write_nothing() {
        let server = MockServer::start().await;
        identity(&server, "navigator-key-sync").await;
        accounts(&server).await;
        let want = desired(1, "laptop");
        Mock::given(method("GET"))
            .and(path("/api/v1/users/synthetic-lawyer/keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"id": 10, "key": want.public_key, "title": "navigator:laptop"},
                {"id": 11, "key": ed25519(3), "title": "navigator:old"},
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(201))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(204))
            .expect(0)
            .mount(&server)
            .await;
        let report = client(&server)
            .reconcile("synthetic.lawyer@example.test", &[want], true)
            .await
            .unwrap()
            .unwrap();
        assert!(report.dry_run);
        assert!(report.added.is_empty());
        assert_eq!(
            report.removed.len(),
            1,
            "the dry run reports the removal it would make"
        );
    }

    #[tokio::test]
    async fn a_token_answering_as_another_identity_is_refused_before_any_write() {
        let server = MockServer::start().await;
        identity(&server, "navigator-admin").await;
        Mock::given(method("GET"))
            .and(path("/api/v1/admin/users"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(0)
            .mount(&server)
            .await;
        let error = client(&server)
            .reconcile(
                "synthetic.lawyer@example.test",
                &[desired(1, "laptop")],
                false,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, KeySyncError::WrongIdentity));
    }

    #[tokio::test]
    async fn no_forgejo_account_yet_is_reported_not_an_error() {
        let server = MockServer::start().await;
        identity(&server, "navigator-key-sync").await;
        accounts(&server).await;
        let outcome = client(&server)
            .reconcile("nobody@example.test", &[desired(1, "laptop")], false)
            .await
            .unwrap();
        assert!(outcome.is_none());
    }

    #[tokio::test]
    async fn account_lookup_pages_until_a_short_page() {
        let server = MockServer::start().await;
        let full: Vec<_> = (0..PAGE_LIMIT)
            .map(|n| json!({"login": format!("user-{n}"), "email": format!("user-{n}@example.test")}))
            .collect();
        Mock::given(method("GET"))
            .and(path("/api/v1/admin/users"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!(full)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/admin/users"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"login": "late-user", "email": "late@example.test"}
            ])))
            .mount(&server)
            .await;
        let login = client(&server)
            .find_login_by_email("late@example.test")
            .await
            .unwrap();
        assert_eq!(login.as_deref(), Some("late-user"));
    }

    #[tokio::test]
    async fn a_rejected_credential_is_an_authentication_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let error = client(&server).verify_identity().await.unwrap_err();
        assert!(matches!(error, KeySyncError::Authentication));
    }
}
