//! One Project, one source repository, created through the deployment's forge.
//!
//! A [`ForgeService`] creates or adopts a private repository named for a
//! Project code and returns its URL. It has no collaborator, invite, or
//! membership methods: Project participation never grants source-forge access.
//! Who may clone is governed on the forge itself, not from Navigator's
//! participation ledger.
//!
//! The organization and host come from [`crate::workspace::WorkspaceConfig`].
//! They are the creation target, not a Project's recorded URL — a matter whose
//! source already lives elsewhere keeps that URL and is not moved here.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;

use crate::workspace::{WorkspaceConfig, WorkspaceConfigError, NAVIGATOR_GITHUB_ORG};

/// Env var holding the GitHub token used to create Project repositories.
/// Checked before [`GITHUB_TOKEN_ENV`] so a workspace-specific token can
/// override an ambient one.
pub const NAVIGATOR_GITHUB_TOKEN_ENV: &str = "NAVIGATOR_GITHUB_TOKEN";
/// The conventional GitHub token env var, used when
/// [`NAVIGATOR_GITHUB_TOKEN_ENV`] is unset.
pub const GITHUB_TOKEN_ENV: &str = "GITHUB_TOKEN";
/// GitHub App ID for the deployment's forge integration.
pub const NAVIGATOR_GITHUB_APP_ID_ENV: &str = "NAVIGATOR_GITHUB_APP_ID";
/// Installation ID for the deployment's forge integration.
pub const NAVIGATOR_GITHUB_INSTALLATION_ID_ENV: &str = "NAVIGATOR_GITHUB_INSTALLATION_ID";
/// RSA private key used to authenticate the deployment's GitHub App.
pub const NAVIGATOR_GITHUB_APP_PRIVATE_KEY_ENV: &str = "NAVIGATOR_GITHUB_APP_PRIVATE_KEY";
/// Override the REST API base, for GitHub Enterprise or a test double.
///
/// Naming GitHub Enterprise is a **feature, not stale narration.** Navigator
/// runs on github.com; this override is how somebody running their own
/// instance points it at their own tenant.
pub const GITHUB_API_BASE_ENV: &str = "NAVIGATOR_GITHUB_API_BASE";
/// Public GitHub's REST API base.
pub const DEFAULT_API_BASE: &str = "https://api.github.com";
const API_VERSION: &str = "2022-11-28";
const USER_AGENT: &str = concat!("neon-law-navigator/", env!("CARGO_PKG_VERSION"));
/// Bound on each request. `head_commit_committed_at` now runs on every
/// `/app/projects` render (once per matching row, concurrently), not only
/// from an infrequent, deliberate admin action — so a socket GitHub never
/// answers must not hang a page render indefinitely: a hang costs one
/// degraded column, not the request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const APP_TOKEN_REFRESH_MARGIN: ChronoDuration = ChronoDuration::seconds(60);

/// A Project's source repository as the forge reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeRepository {
    /// The clone/browse URL recorded on the Project.
    pub url: String,
    /// The repository name, which is the Project code.
    pub name: String,
}

#[derive(Debug, Error)]
pub enum ForgeError {
    #[error("missing required forge configuration: {0}")]
    MissingConfig(&'static str),
    #[error("invalid forge configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("resolve the deployment forge pair: {0}")]
    Workspace(#[from] WorkspaceConfigError),
    #[error("forge authentication failed")]
    Authentication,
    #[error("could not authenticate with the configured GitHub App")]
    GitHubAppAuthentication,
    #[error("GitHub did not return a usable installation access token")]
    MissingInstallationToken,
    #[error("forge request failed while {action}")]
    Request {
        action: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("forge API returned {status} while {action}")]
    Api { action: &'static str, status: u16 },
    #[error("forge API returned an invalid response while {action}")]
    Response {
        action: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("forge API response while {action} did not include a repository URL")]
    MissingUrl { action: &'static str },
    #[error(
        "repository {name} is not private after provisioning; a policy regression or a \
         deliberate visibility change happened underneath, and both need a human, not a retry"
    )]
    NotPrivate { name: String },
    #[error("Project repository {name} does not exist in the configured GitHub organization")]
    ProjectRepositoryMissing { name: String },
    #[error(
        "repository {name} is not this Firm's repository for the Project; it is never adopted"
    )]
    ForeignRepository { name: String },
    #[error(
        "repository {name} already holds source that is not this import; it is never replaced \
         implicitly"
    )]
    RepositoryNotEmpty { name: String },
    #[error("this forge cannot {0}")]
    Unsupported(&'static str),
}

/// The commit-message trailer that binds a first commit to the canonical tree
/// it holds, so a retry finds an applied import instead of writing twice.
pub const SOURCE_TREE_TRAILER: &str = "Source-Tree:";

/// Whether a commit message carries `tree_digest` in its trailer.
#[must_use]
pub fn message_binds_tree(message: &str, tree_digest: &str) -> bool {
    message.lines().any(|line| {
        line.trim()
            .strip_prefix(SOURCE_TREE_TRAILER)
            .is_some_and(|digest| digest.trim() == tree_digest)
    })
}

/// Create or adopt one private repository named for a Project code.
///
/// The trait is deliberately narrow. Adding a collaborator method here would
/// make Project participation a back door onto the forge, which
/// [`docs/project-repositories.md`] forbids. [`Self::delete_repository`] and
/// [`Self::head_commit_sha`] are the exception: they serve a matter's
/// *close*, not its participation, and exist so a closed Project's
/// repository can be archived and safely removed (ENG-481) rather than
/// living forever once the matter has ended.
#[async_trait]
pub trait ForgeService: Send + Sync {
    async fn find_repository(
        &self,
        project_code: &str,
    ) -> Result<Option<ForgeRepository>, ForgeError>;
    async fn ensure_repository(&self, project_code: &str) -> Result<ForgeRepository, ForgeError>;
    /// The current commit SHA at the tip of the repository's default
    /// branch, or `None` if the repository does not exist. Read fresh on
    /// every call — never cached — so a caller comparing it against an
    /// archived document's recorded commit sees whatever has actually been
    /// pushed since.
    async fn head_commit_sha(&self, project_code: &str) -> Result<Option<String>, ForgeError>;
    /// The committer date (RFC 3339) of the commit at the tip of the
    /// repository's default branch, or `None` if the repository does not
    /// exist or the forge reported no date. Same freshness contract as
    /// [`Self::head_commit_sha`]: read fresh, never cached — a lawyer
    /// workbench listing calls this per row on every render rather than
    /// trusting a stale value.
    async fn head_commit_committed_at(
        &self,
        project_code: &str,
    ) -> Result<Option<String>, ForgeError>;
    /// Permanently delete the repository. Irreversible; callers must have
    /// already confirmed an archive exists and matches before calling this.
    async fn delete_repository(&self, project_code: &str) -> Result<(), ForgeError>;
    /// The text of one committed file at the tip of the default branch, or
    /// `None` when the repository or the file does not exist. Read fresh on
    /// every call, like [`Self::head_commit_sha`]: the nightly sweep compares
    /// a repository's `navigator.yaml` pin against the running release and
    /// must see the commit that is actually there.
    async fn read_file(&self, project_code: &str, path: &str)
        -> Result<Option<String>, ForgeError>;
    /// Commit `files` as the first commit of the Project's empty repository,
    /// with `tree_digest` in a [`SOURCE_TREE_TRAILER`], and return its SHA.
    /// A repository whose head already carries that digest returns that
    /// commit, so a retry after an unknown outcome never writes twice; any
    /// other existing source is [`ForgeError::RepositoryNotEmpty`].
    async fn initialize_source(
        &self,
        project_code: &str,
        files: &BTreeMap<String, String>,
        tree_digest: &str,
        message: &str,
    ) -> Result<String, ForgeError> {
        let _ = (project_code, files, tree_digest, message);
        Err(ForgeError::Unsupported("initialize Project source"))
    }
}

/// In-memory forge for store and workflow tests. Idempotent on the Project
/// code: a second `ensure_repository` returns the same URL and does not
/// invent a second repository.
#[derive(Clone)]
pub struct FakeForge {
    host: String,
    organization: String,
    state: Arc<Mutex<FakeForgeState>>,
}

#[derive(Default)]
struct FakeForgeState {
    repositories: BTreeMap<String, ForgeRepository>,
    ensure_calls: usize,
    /// Set only by [`FakeForge::set_head_commit_sha`] — tests control what
    /// the "live" HEAD is rather than this fake deriving one from nothing.
    head_shas: BTreeMap<String, String>,
    /// Set only by [`FakeForge::set_head_commit_committed_at`] — tests
    /// control the "live" HEAD's committer date rather than this fake
    /// deriving one from nothing.
    head_committed_ats: BTreeMap<String, String>,
    /// Set only by [`FakeForge::set_file`]: `(project code, path)` to the
    /// committed text [`ForgeService::read_file`] reports.
    files: BTreeMap<(String, String), String>,
    /// The head commit's message per Project code, for source initialization.
    head_messages: BTreeMap<String, String>,
    /// How many initial commits were written.
    initialize_writes: usize,
    /// Fail the next write after it lands, as an unknown provider outcome.
    lose_next_write_reply: bool,
    /// Governance drift per Project code, repaired by the next reconcile.
    governance_drift: BTreeMap<String, Vec<String>>,
    /// How many governance reconciles ran.
    governance_reconciles: usize,
    /// Update pull requests by `(Project code, branch)`.
    update_pulls: BTreeMap<(String, String), FakeUpdatePull>,
}

/// One update pull request [`FakeForge`] holds: the proposed file and
/// whether it is still open. Nothing reaches `main`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeUpdatePull {
    pub path: String,
    pub content: String,
    pub title: String,
    pub open: bool,
}

impl FakeForge {
    /// A forge whose URLs live under a synthetic host and organization.
    ///
    /// Which organization a deployment creates Project repositories in is
    /// configuration, so no real organization name is a fixture value.
    #[must_use]
    pub fn new() -> Self {
        Self {
            host: "forge.example".to_string(),
            organization: "an-organization".to_string(),
            state: Arc::new(Mutex::new(FakeForgeState::default())),
        }
    }

    fn url_for(&self, project_code: &str) -> String {
        format!("https://{}/{}/{project_code}", self.host, self.organization)
    }

    /// How many times [`ForgeService::ensure_repository`] ran. A retry that
    /// adopts must not increment a *create* count; this counts the call so
    /// tests can see the method is idempotent on the stored URL, not that
    /// it was never invoked.
    #[must_use]
    pub fn ensure_calls(&self) -> usize {
        self.state
            .lock()
            .expect("fake forge mutex poisoned")
            .ensure_calls
    }

    #[must_use]
    pub fn repository_count(&self) -> usize {
        self.state
            .lock()
            .expect("fake forge mutex poisoned")
            .repositories
            .len()
    }

    /// Test control: declare what [`ForgeService::head_commit_sha`] reports
    /// for `project_code`, standing in for whatever a real forge's default
    /// branch tip would be.
    pub fn set_head_commit_sha(&self, project_code: &str, sha: impl Into<String>) {
        self.state
            .lock()
            .expect("fake forge mutex poisoned")
            .head_shas
            .insert(project_code.to_string(), sha.into());
    }

    /// Test control: declare what
    /// [`ForgeService::head_commit_committed_at`] reports for
    /// `project_code`.
    pub fn set_head_commit_committed_at(
        &self,
        project_code: &str,
        committed_at: impl Into<String>,
    ) {
        self.state
            .lock()
            .expect("fake forge mutex poisoned")
            .head_committed_ats
            .insert(project_code.to_string(), committed_at.into());
    }
}

impl FakeForge {
    /// Test control: declare the text [`ForgeService::read_file`] reports
    /// for `path` in `project_code`'s repository.
    pub fn set_file(&self, project_code: &str, path: &str, contents: impl Into<String>) {
        self.state
            .lock()
            .expect("fake forge mutex poisoned")
            .files
            .insert(
                (project_code.to_string(), path.to_string()),
                contents.into(),
            );
    }
}

impl FakeForge {
    /// How many initial source commits were actually written.
    #[must_use]
    pub fn initialize_writes(&self) -> usize {
        self.state
            .lock()
            .expect("fake forge mutex poisoned")
            .initialize_writes
    }

    /// Every committed path in `project_code`'s repository, sorted.
    #[must_use]
    pub fn committed_paths(&self, project_code: &str) -> Vec<String> {
        self.state
            .lock()
            .expect("fake forge mutex poisoned")
            .files
            .keys()
            .filter(|(code, _)| code == project_code)
            .map(|(_, path)| path.clone())
            .collect()
    }

    /// Test control: the next source write lands but its reply is lost.
    pub fn lose_next_write_reply(&self) {
        self.state
            .lock()
            .expect("fake forge mutex poisoned")
            .lose_next_write_reply = true;
    }
}

impl FakeForge {
    /// Make the next governance reconcile of `project_code` find and repair
    /// `controls`.
    pub fn set_governance_drift(&self, project_code: &str, controls: &[&str]) {
        self.state
            .lock()
            .expect("fake forge mutex poisoned")
            .governance_drift
            .insert(
                project_code.to_string(),
                controls.iter().map(ToString::to_string).collect(),
            );
    }

    /// How many governance reconciles ran.
    #[must_use]
    pub fn governance_reconciles(&self) -> usize {
        self.state
            .lock()
            .expect("fake forge mutex poisoned")
            .governance_reconciles
    }

    /// Every update pull request opened for `project_code`, by branch.
    #[must_use]
    pub fn update_pulls(&self, project_code: &str) -> BTreeMap<String, FakeUpdatePull> {
        self.state
            .lock()
            .expect("fake forge mutex poisoned")
            .update_pulls
            .iter()
            .filter(|((code, _), _)| code == project_code)
            .map(|((_, branch), pull)| (branch.clone(), pull.clone()))
            .collect()
    }

    /// Close `branch`'s update pull request without merging it.
    pub fn close_update_pull(&self, project_code: &str, branch: &str) {
        if let Some(pull) = self
            .state
            .lock()
            .expect("fake forge mutex poisoned")
            .update_pulls
            .get_mut(&(project_code.to_string(), branch.to_string()))
        {
            pull.open = false;
        }
    }
}

#[async_trait]
impl crate::forgejo_governance::RepositoryGovernance for FakeForge {
    async fn reconcile_governance(&self, project_code: &str) -> anyhow::Result<Vec<String>> {
        let mut state = self.state.lock().expect("fake forge mutex poisoned");
        state.governance_reconciles += 1;
        Ok(state
            .governance_drift
            .remove(project_code)
            .unwrap_or_default())
    }

    async fn propose_file_update(
        &self,
        project_code: &str,
        update: &crate::forgejo_governance::FileUpdate<'_>,
    ) -> anyhow::Result<crate::forgejo_governance::UpdateProposal> {
        use crate::forgejo_governance::UpdateProposal;
        let mut state = self.state.lock().expect("fake forge mutex poisoned");
        anyhow::ensure!(
            state.repositories.contains_key(project_code),
            "the Project repository does not exist"
        );
        let key = (project_code.to_string(), update.branch.to_string());
        if let Some(existing) = state.update_pulls.get(&key) {
            return Ok(if existing.open {
                UpdateProposal::Pending
            } else {
                UpdateProposal::Declined
            });
        }
        state.update_pulls.insert(
            key,
            FakeUpdatePull {
                path: update.path.to_string(),
                content: update.content.to_string(),
                title: update.title.to_string(),
                open: true,
            },
        );
        Ok(UpdateProposal::Opened)
    }
}

impl Default for FakeForge {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ForgeService for FakeForge {
    async fn find_repository(
        &self,
        project_code: &str,
    ) -> Result<Option<ForgeRepository>, ForgeError> {
        let state = self.state.lock().expect("fake forge mutex poisoned");
        Ok(state.repositories.get(project_code).cloned())
    }

    async fn ensure_repository(&self, project_code: &str) -> Result<ForgeRepository, ForgeError> {
        let mut state = self.state.lock().expect("fake forge mutex poisoned");
        state.ensure_calls += 1;
        if let Some(existing) = state.repositories.get(project_code) {
            return Ok(existing.clone());
        }
        let created = ForgeRepository {
            url: self.url_for(project_code),
            name: project_code.to_string(),
        };
        state
            .repositories
            .insert(project_code.to_string(), created.clone());
        Ok(created)
    }

    async fn head_commit_sha(&self, project_code: &str) -> Result<Option<String>, ForgeError> {
        let state = self.state.lock().expect("fake forge mutex poisoned");
        Ok(state.head_shas.get(project_code).cloned())
    }

    async fn head_commit_committed_at(
        &self,
        project_code: &str,
    ) -> Result<Option<String>, ForgeError> {
        let state = self.state.lock().expect("fake forge mutex poisoned");
        Ok(state.head_committed_ats.get(project_code).cloned())
    }

    async fn delete_repository(&self, project_code: &str) -> Result<(), ForgeError> {
        let mut state = self.state.lock().expect("fake forge mutex poisoned");
        state.repositories.remove(project_code);
        state.head_shas.remove(project_code);
        state.head_committed_ats.remove(project_code);
        state.head_messages.remove(project_code);
        state.files.retain(|(code, _), _| code != project_code);
        Ok(())
    }

    async fn read_file(
        &self,
        project_code: &str,
        path: &str,
    ) -> Result<Option<String>, ForgeError> {
        let state = self.state.lock().expect("fake forge mutex poisoned");
        Ok(state
            .files
            .get(&(project_code.to_string(), path.to_string()))
            .cloned())
    }

    async fn initialize_source(
        &self,
        project_code: &str,
        files: &BTreeMap<String, String>,
        tree_digest: &str,
        message: &str,
    ) -> Result<String, ForgeError> {
        let mut state = self.state.lock().expect("fake forge mutex poisoned");
        if !state.repositories.contains_key(project_code) {
            return Err(ForgeError::ProjectRepositoryMissing {
                name: project_code.to_string(),
            });
        }
        if let Some(head) = state.head_shas.get(project_code).cloned() {
            let bound = state
                .head_messages
                .get(project_code)
                .is_some_and(|message| message_binds_tree(message, tree_digest));
            return if bound {
                Ok(head)
            } else {
                Err(ForgeError::RepositoryNotEmpty {
                    name: project_code.to_string(),
                })
            };
        }
        state.initialize_writes += 1;
        let sha = format!("{:0>40}", tree_digest.get(..16).unwrap_or(tree_digest));
        state
            .head_shas
            .insert(project_code.to_string(), sha.clone());
        state.head_messages.insert(
            project_code.to_string(),
            format!("{message}\n\n{SOURCE_TREE_TRAILER} {tree_digest}"),
        );
        for (path, text) in files {
            state
                .files
                .insert((project_code.to_string(), path.clone()), text.clone());
        }
        if std::mem::take(&mut state.lose_next_write_reply) {
            return Err(ForgeError::Api {
                action: "writing initial source",
                status: 504,
            });
        }
        Ok(sha)
    }
}

/// GitHub REST client that creates private repositories in one organization.
pub struct GitHubForge {
    api_base: String,
    organization: String,
    auth: ForgeAuth,
    app_token: AsyncMutex<Option<CachedAppToken>>,
    http: reqwest::Client,
}

enum ForgeAuth {
    Token(String),
    App {
        app_id: String,
        installation_id: String,
        private_key: Arc<EncodingKey>,
    },
}

struct CachedAppToken {
    token: String,
    expires_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize)]
struct GitHubAppJwtClaims {
    iat: u64,
    exp: u64,
    iss: String,
}

#[derive(Deserialize)]
struct GitHubInstallationToken {
    token: String,
    expires_at: DateTime<Utc>,
}

fn configured_value(value: Option<String>, key: &'static str) -> Result<String, ForgeError> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or(ForgeError::MissingConfig(key))
}

impl std::fmt::Debug for GitHubForge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GitHubForge")
            .field("api_base", &self.api_base)
            .field("organization", &self.organization)
            .field(
                "authentication",
                &match &self.auth {
                    ForgeAuth::Token(_) => "token",
                    ForgeAuth::App { .. } => "GitHub App",
                },
            )
            .finish_non_exhaustive()
    }
}

impl GitHubForge {
    /// Build from an already-resolved organization, token, and API base.
    #[must_use]
    pub fn new(organization: String, token: String, api_base: &str) -> Self {
        Self {
            api_base: api_base.trim_end_matches('/').to_string(),
            organization,
            auth: ForgeAuth::Token(token),
            app_token: AsyncMutex::new(None),
            http: reqwest::Client::new(),
        }
    }

    fn with_app(
        organization: String,
        app_id: String,
        installation_id: String,
        private_key: EncodingKey,
        api_base: &str,
    ) -> Self {
        Self {
            api_base: api_base.trim_end_matches('/').to_string(),
            organization,
            auth: ForgeAuth::App {
                app_id,
                installation_id,
                private_key: Arc::new(private_key),
            },
            app_token: AsyncMutex::new(None),
            http: reqwest::Client::new(),
        }
    }

    /// Resolve the deployment's forge pair and GitHub App or token credentials.
    ///
    /// # Errors
    ///
    /// When the deployment pair or a complete credential set is missing.
    pub fn from_env() -> Result<Self, ForgeError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// The lookup form so tests can supply a pair and a token without
    /// mutating process-global environment variables.
    pub fn from_lookup<F>(get: F) -> Result<Self, ForgeError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let config = WorkspaceConfig::from_lookup(&get)?;
        let api_base = get(GITHUB_API_BASE_ENV)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_API_BASE.to_string());
        if config.organization.is_empty() {
            return Err(ForgeError::MissingConfig(NAVIGATOR_GITHUB_ORG));
        }
        let app_values = [
            get(NAVIGATOR_GITHUB_APP_ID_ENV),
            get(NAVIGATOR_GITHUB_INSTALLATION_ID_ENV),
            get(NAVIGATOR_GITHUB_APP_PRIVATE_KEY_ENV),
        ];
        if app_values.iter().any(Option::is_some) {
            let [app_id, installation_id, private_key_pem] = app_values;
            let app_id = configured_value(app_id, NAVIGATOR_GITHUB_APP_ID_ENV)?;
            let installation_id =
                configured_value(installation_id, NAVIGATOR_GITHUB_INSTALLATION_ID_ENV)?;
            let private_key_pem =
                configured_value(private_key_pem, NAVIGATOR_GITHUB_APP_PRIVATE_KEY_ENV)?;
            let private_key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
                .map_err(|_| ForgeError::GitHubAppAuthentication)?;
            app_id
                .parse::<u64>()
                .map_err(|_| ForgeError::InvalidConfig(NAVIGATOR_GITHUB_APP_ID_ENV))?;
            installation_id
                .parse::<u64>()
                .map_err(|_| ForgeError::InvalidConfig(NAVIGATOR_GITHUB_INSTALLATION_ID_ENV))?;
            return Ok(Self::with_app(
                config.organization,
                app_id,
                installation_id,
                private_key,
                &api_base,
            ));
        }

        let token = get(NAVIGATOR_GITHUB_TOKEN_ENV)
            .or_else(|| get(GITHUB_TOKEN_ENV))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or(ForgeError::MissingConfig(NAVIGATOR_GITHUB_TOKEN_ENV))?;
        Ok(Self::new(config.organization, token, &api_base))
    }

    async fn authorization_token(&self) -> Result<String, ForgeError> {
        let ForgeAuth::App {
            app_id,
            installation_id,
            private_key,
        } = &self.auth
        else {
            if let ForgeAuth::Token(token) = &self.auth {
                return Ok(token.clone());
            }
            return Err(ForgeError::GitHubAppAuthentication);
        };

        let mut cached_token = self.app_token.lock().await;
        if let Some(token) = cached_token.as_ref() {
            if token.expires_at > Utc::now() + APP_TOKEN_REFRESH_MARGIN {
                return Ok(token.token.clone());
            }
        }

        let now = jsonwebtoken::get_current_timestamp();
        let claims = GitHubAppJwtClaims {
            iat: now.saturating_sub(60),
            exp: now.saturating_add(9 * 60),
            iss: app_id.clone(),
        };
        let app_jwt = encode(&Header::new(Algorithm::RS256), &claims, private_key)
            .map_err(|_| ForgeError::GitHubAppAuthentication)?;
        let url = format!(
            "{}/app/installations/{installation_id}/access_tokens",
            self.api_base
        );
        let response = self
            .http
            .post(url)
            .bearer_auth(app_jwt)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await;
        let response = Self::checked(response, "creating installation access token")?;
        let token = response
            .json::<GitHubInstallationToken>()
            .await
            .map_err(|source| ForgeError::Response {
                action: "creating installation access token",
                source,
            })?;
        if token.token.trim().is_empty() {
            return Err(ForgeError::MissingInstallationToken);
        }
        let value = token.token.clone();
        *cached_token = Some(CachedAppToken {
            token: token.token,
            expires_at: token.expires_at,
        });
        Ok(value)
    }

    fn repos_url(&self, project_code: &str) -> String {
        format!(
            "{}/repos/{}/{project_code}",
            self.api_base, self.organization
        )
    }

    fn org_repos_url(&self) -> String {
        format!("{}/orgs/{}/repos", self.api_base, self.organization)
    }

    fn checked(
        response: Result<reqwest::Response, reqwest::Error>,
        action: &'static str,
    ) -> Result<reqwest::Response, ForgeError> {
        let response = response.map_err(|source| ForgeError::Request { action, source })?;
        let status = response.status().as_u16();
        if response.status().is_success() {
            return Ok(response);
        }
        if status == 401 {
            return Err(ForgeError::Authentication);
        }
        Err(ForgeError::Api { action, status })
    }

    async fn get_repository(
        &self,
        project_code: &str,
    ) -> Result<Option<ForgeRepository>, ForgeError> {
        self.get_repository_checked(project_code, false).await
    }

    /// [`Self::get_repository`], optionally failing closed when the live
    /// repository is not private. Provisioning uses the checked form both when
    /// it adopts an existing repository and when it adopts after a create
    /// conflict; ordinary lookups — used by callers with no provisioning
    /// stake, such as the reconciler's own drift report — must not start
    /// failing on a repository they are only reading.
    async fn get_repository_checked(
        &self,
        project_code: &str,
        verify_private: bool,
    ) -> Result<Option<ForgeRepository>, ForgeError> {
        let token = self.authorization_token().await?;
        let response = self
            .http
            .get(self.repos_url(project_code))
            .bearer_auth(token)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await;
        let response = match response {
            Ok(response) if response.status().as_u16() == 404 => return Ok(None),
            other => Self::checked(other, "finding repository")?,
        };
        Ok(Some(
            parse_repository(response, "finding repository", verify_private).await?,
        ))
    }

    async fn create_repository(&self, project_code: &str) -> Result<ForgeRepository, ForgeError> {
        let token = self.authorization_token().await?;
        let created = self
            .http
            .post(self.org_repos_url())
            .bearer_auth(token)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .timeout(REQUEST_TIMEOUT)
            .json(&CreateRepository {
                name: project_code,
                private: true,
                auto_init: false,
            })
            .send()
            .await;
        if created
            .as_ref()
            .is_ok_and(|response| response.status().as_u16() == 422)
        {
            // Name already taken in the organization: adopt rather than fail.
            // Still verify visibility — adopting a repository that is not
            // private is the same failure a fresh create must catch.
            return self
                .get_repository_checked(project_code, true)
                .await?
                .ok_or(ForgeError::Api {
                    action: "creating repository",
                    status: 422,
                });
        }
        let response = Self::checked(created, "creating repository")?;
        // Fail closed rather than trusting the request was honoured: a policy
        // change underneath (`members_can_create_private_repositories` or a
        // similar org default flipping) can land a repository public even
        // though `private: true` was requested, and that is exactly the
        // failure worth catching before anything else configures it.
        parse_repository(response, "creating repository", true).await
    }

    /// The commit at the tip of the repository's default branch — the shared
    /// two-request fetch (resolve `default_branch`, then read its tip) behind
    /// both [`ForgeService::head_commit_sha`] and
    /// [`ForgeService::head_commit_committed_at`], so a caller wanting either
    /// fact makes the same two calls rather than each growing its own copy.
    async fn head_commit(&self, project_code: &str) -> Result<Option<CommitBody>, ForgeError> {
        let action = "reading repository for HEAD commit";
        let token = self.authorization_token().await?;
        let response = self
            .http
            .get(self.repos_url(project_code))
            .bearer_auth(&token)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await;
        let response = match response {
            Ok(response) if response.status().as_u16() == 404 => return Ok(None),
            other => Self::checked(other, action)?,
        };
        let detail = response
            .json::<RepositoryDetail>()
            .await
            .map_err(|source| ForgeError::Response { action, source })?;
        let branch = detail
            .default_branch
            .ok_or(ForgeError::MissingUrl { action })?;

        let action = "reading HEAD commit";
        let commit_url = format!(
            "{}/repos/{}/{project_code}/commits/{branch}",
            self.api_base, self.organization
        );
        let token = self.authorization_token().await?;
        let response = self
            .http
            .get(commit_url)
            .bearer_auth(token)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await;
        let response = Self::checked(response, action)?;
        let commit = response
            .json::<CommitBody>()
            .await
            .map_err(|source| ForgeError::Response { action, source })?;
        Ok(Some(commit))
    }
}

#[derive(Serialize)]
struct CreateRepository<'a> {
    name: &'a str,
    private: bool,
    auto_init: bool,
}

#[derive(Deserialize)]
struct RepositoryBody {
    html_url: Option<String>,
    name: Option<String>,
    #[serde(default)]
    private: Option<bool>,
}

#[derive(Deserialize)]
struct RepositoryDetail {
    default_branch: Option<String>,
}

#[derive(Deserialize)]
struct CommitBody {
    sha: String,
    #[serde(default)]
    commit: Option<CommitDetail>,
}

#[derive(Deserialize)]
struct CommitDetail {
    #[serde(default)]
    committer: Option<CommitPerson>,
}

#[derive(Deserialize)]
struct CommitPerson {
    #[serde(default)]
    date: Option<String>,
}

async fn parse_repository(
    response: reqwest::Response,
    action: &'static str,
    verify_private: bool,
) -> Result<ForgeRepository, ForgeError> {
    let body = response
        .json::<RepositoryBody>()
        .await
        .map_err(|source| ForgeError::Response { action, source })?;
    let url = body.html_url.ok_or(ForgeError::MissingUrl { action })?;
    let name = body.name.ok_or(ForgeError::MissingUrl { action })?;
    if verify_private && body.private != Some(true) {
        return Err(ForgeError::NotPrivate { name });
    }
    Ok(ForgeRepository { url, name })
}

#[async_trait]
impl ForgeService for GitHubForge {
    async fn find_repository(
        &self,
        project_code: &str,
    ) -> Result<Option<ForgeRepository>, ForgeError> {
        self.get_repository(project_code).await
    }

    async fn ensure_repository(&self, project_code: &str) -> Result<ForgeRepository, ForgeError> {
        if let Some(existing) = self.get_repository_checked(project_code, true).await? {
            tracing::info!(
                project_code,
                repository_url = existing.url.as_str(),
                outcome = "already_exists",
                "Project repository provisioning completed"
            );
            return Ok(existing);
        }
        let created = self.create_repository(project_code).await?;
        tracing::info!(
            project_code,
            repository_url = created.url.as_str(),
            outcome = "created",
            "Project repository provisioning completed"
        );
        Ok(created)
    }

    async fn head_commit_sha(&self, project_code: &str) -> Result<Option<String>, ForgeError> {
        Ok(self
            .head_commit(project_code)
            .await?
            .map(|commit| commit.sha))
    }

    async fn head_commit_committed_at(
        &self,
        project_code: &str,
    ) -> Result<Option<String>, ForgeError> {
        Ok(self
            .head_commit(project_code)
            .await?
            .and_then(|commit| commit.commit)
            .and_then(|detail| detail.committer)
            .and_then(|person| person.date))
    }

    async fn delete_repository(&self, project_code: &str) -> Result<(), ForgeError> {
        let token = self.authorization_token().await?;
        let response = self
            .http
            .delete(self.repos_url(project_code))
            .bearer_auth(token)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await;
        Self::checked(response, "deleting repository")?;
        Ok(())
    }

    async fn read_file(
        &self,
        project_code: &str,
        path: &str,
    ) -> Result<Option<String>, ForgeError> {
        let action = "reading repository file";
        let token = self.authorization_token().await?;
        let url = format!(
            "{}/contents/{}",
            self.repos_url(project_code),
            path.trim_start_matches('/')
        );
        // `raw` returns the file body itself, so a manifest arrives as the
        // text committed, not a base64 envelope to unwrap.
        let response = self
            .http
            .get(url)
            .bearer_auth(token)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header(reqwest::header::ACCEPT, "application/vnd.github.raw+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await;
        let response = match response {
            Ok(response) if response.status().as_u16() == 404 => return Ok(None),
            other => Self::checked(other, action)?,
        };
        response
            .text()
            .await
            .map(Some)
            .map_err(|source| ForgeError::Response { action, source })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FakeForge, ForgeService, GitHubForge, GITHUB_API_BASE_ENV, GITHUB_TOKEN_ENV,
        NAVIGATOR_GITHUB_APP_ID_ENV, NAVIGATOR_GITHUB_APP_PRIVATE_KEY_ENV,
        NAVIGATOR_GITHUB_INSTALLATION_ID_ENV, NAVIGATOR_GITHUB_TOKEN_ENV,
    };
    use crate::workspace::{NAVIGATOR_GCP_PROJECT_ID, NAVIGATOR_GITHUB_ORG, NAVIGATOR_GIT_HOST};
    use chrono::{Duration as ChronoDuration, Utc};
    use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
    use serde_json::json;
    use std::collections::HashMap;
    use std::process::Command;
    use tempfile::TempDir;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    fn generate_github_app_keypair() -> (TempDir, Vec<u8>, Vec<u8>) {
        let temp = tempfile::tempdir().expect("temporary directory for ephemeral test key");
        let private_key_path = temp.path().join("github-app-private.pem");
        let public_key_path = temp.path().join("github-app-public.pem");
        let generated = Command::new("openssl")
            .args(["genrsa", "-out"])
            .arg(&private_key_path)
            .arg("2048")
            .output()
            .expect("OpenSSL must be available to generate an ephemeral RSA test key");
        assert!(
            generated.status.success(),
            "OpenSSL could not generate an RSA test key"
        );
        let exported = Command::new("openssl")
            .args(["pkey", "-in"])
            .arg(&private_key_path)
            .args(["-pubout", "-out"])
            .arg(&public_key_path)
            .output()
            .expect("OpenSSL must be available to export the ephemeral RSA test key");
        assert!(
            exported.status.success(),
            "OpenSSL could not export the RSA test key"
        );
        let private_key = std::fs::read(private_key_path).expect("read ephemeral private key");
        let public_key = std::fs::read(public_key_path).expect("read ephemeral public key");
        (temp, private_key, public_key)
    }

    #[tokio::test]
    async fn fake_forge_is_idempotent_on_the_project_code() {
        let forge = FakeForge::new();
        let first = forge.ensure_repository("acme").await.unwrap();
        let second = forge.ensure_repository("acme").await.unwrap();
        assert_eq!(first, second);
        assert_eq!(first.name, "acme");
        assert_eq!(first.url, "https://forge.example/an-organization/acme");
        assert_eq!(forge.repository_count(), 1);
        assert_eq!(forge.ensure_calls(), 2);
    }

    #[tokio::test]
    async fn fake_forge_reports_no_head_sha_until_one_is_set() {
        let forge = FakeForge::new();
        forge.ensure_repository("acme").await.unwrap();
        assert_eq!(forge.head_commit_sha("acme").await.unwrap(), None);
        forge.set_head_commit_sha("acme", "deadbeef");
        assert_eq!(
            forge.head_commit_sha("acme").await.unwrap(),
            Some("deadbeef".to_string())
        );
    }

    #[tokio::test]
    async fn fake_forge_reports_no_committed_at_until_one_is_set() {
        let forge = FakeForge::new();
        forge.ensure_repository("acme").await.unwrap();
        assert_eq!(forge.head_commit_committed_at("acme").await.unwrap(), None);
        forge.set_head_commit_committed_at("acme", "2026-09-01T12:00:00Z");
        assert_eq!(
            forge.head_commit_committed_at("acme").await.unwrap(),
            Some("2026-09-01T12:00:00Z".to_string())
        );
    }

    #[tokio::test]
    async fn fake_forge_delete_removes_the_repository_and_its_head_sha() {
        let forge = FakeForge::new();
        forge.ensure_repository("acme").await.unwrap();
        forge.set_head_commit_sha("acme", "deadbeef");
        assert_eq!(forge.repository_count(), 1);

        forge.delete_repository("acme").await.unwrap();

        assert_eq!(forge.repository_count(), 0);
        assert_eq!(forge.head_commit_sha("acme").await.unwrap(), None);
        assert_eq!(forge.find_repository("acme").await.unwrap(), None);
    }

    #[test]
    fn github_forge_requires_a_token_and_a_deployment_pair() {
        let error = GitHubForge::from_lookup(lookup(&[(NAVIGATOR_GCP_PROJECT_ID, "neon-law-stg")]))
            .unwrap_err();
        assert!(error.to_string().contains(NAVIGATOR_GITHUB_ORG), "{error}");

        let error = GitHubForge::from_lookup(lookup(&[
            (NAVIGATOR_GCP_PROJECT_ID, "neon-law-stg"),
            (NAVIGATOR_GITHUB_ORG, "an-organization"),
        ]))
        .unwrap_err();
        assert!(
            error.to_string().contains(NAVIGATOR_GITHUB_TOKEN_ENV),
            "{error}"
        );
    }

    #[test]
    fn github_forge_accepts_complete_app_credentials_without_a_token() {
        let (_key_files, private_key, _) = generate_github_app_keypair();
        let private_key = String::from_utf8(private_key).expect("OpenSSL emits UTF-8 PEM");
        let forge = GitHubForge::from_lookup(lookup(&[
            (NAVIGATOR_GCP_PROJECT_ID, "neon-law-stg"),
            (NAVIGATOR_GITHUB_ORG, "an-organization"),
            (NAVIGATOR_GITHUB_APP_ID_ENV, "12345"),
            (NAVIGATOR_GITHUB_INSTALLATION_ID_ENV, "67890"),
            (NAVIGATOR_GITHUB_APP_PRIVATE_KEY_ENV, private_key.as_str()),
        ]));

        assert!(forge.is_ok(), "{forge:?}");
    }

    #[test]
    fn malformed_app_private_key_fails_provider_initialization() {
        let error = GitHubForge::from_lookup(lookup(&[
            (NAVIGATOR_GCP_PROJECT_ID, "neon-law-stg"),
            (NAVIGATOR_GITHUB_ORG, "an-organization"),
            (NAVIGATOR_GITHUB_APP_ID_ENV, "12345"),
            (NAVIGATOR_GITHUB_INSTALLATION_ID_ENV, "67890"),
            (NAVIGATOR_GITHUB_APP_PRIVATE_KEY_ENV, "not a PEM"),
            (NAVIGATOR_GITHUB_TOKEN_ENV, "test-token"),
        ]))
        .unwrap_err();

        assert!(matches!(error, super::ForgeError::GitHubAppAuthentication));
    }

    #[test]
    fn partial_app_credentials_do_not_fall_back_to_a_personal_token() {
        let error = GitHubForge::from_lookup(lookup(&[
            (NAVIGATOR_GCP_PROJECT_ID, "neon-law-stg"),
            (NAVIGATOR_GITHUB_ORG, "an-organization"),
            (NAVIGATOR_GITHUB_APP_ID_ENV, "12345"),
            (NAVIGATOR_GITHUB_TOKEN_ENV, "test-token"),
        ]))
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains(NAVIGATOR_GITHUB_INSTALLATION_ID_ENV),
            "{error}"
        );
    }

    #[tokio::test]
    async fn github_forge_mints_and_reuses_an_app_installation_token() {
        let (_key_files, private_key, public_key) = generate_github_app_keypair();
        let server = MockServer::start().await;
        let expires_at = (Utc::now() + ChronoDuration::hours(1)).to_rfc3339();
        Mock::given(method("POST"))
            .and(path("/app/installations/67890/access_tokens"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "token": "installation-token",
                "expires_at": expires_at
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/an-organization/acme"))
            .and(header("authorization", "Bearer installation-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "html_url": "https://forge.example/an-organization/acme",
                "name": "acme",
                "private": true
            })))
            .expect(2)
            .mount(&server)
            .await;

        let private_key = String::from_utf8(private_key).expect("OpenSSL emits UTF-8 PEM");
        let forge = GitHubForge::from_lookup(lookup(&[
            (NAVIGATOR_GCP_PROJECT_ID, "neon-law-stg"),
            (NAVIGATOR_GITHUB_ORG, "an-organization"),
            (NAVIGATOR_GITHUB_APP_ID_ENV, "12345"),
            (NAVIGATOR_GITHUB_INSTALLATION_ID_ENV, "67890"),
            (NAVIGATOR_GITHUB_APP_PRIVATE_KEY_ENV, private_key.as_str()),
            (GITHUB_API_BASE_ENV, server.uri().as_str()),
        ]))
        .expect("complete GitHub App configuration");

        forge.ensure_repository("acme").await.unwrap();
        forge.ensure_repository("acme").await.unwrap();

        let requests = server.received_requests().await.expect("recorded requests");
        let app_request = requests
            .iter()
            .find(|request| request.method.as_str() == "POST")
            .expect("mint an installation token once");
        let app_jwt = app_request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .expect("app JWT bearer header");
        let decoded = decode::<super::GitHubAppJwtClaims>(
            app_jwt,
            &DecodingKey::from_rsa_pem(&public_key).expect("ephemeral public RSA key parses"),
            &Validation::new(Algorithm::RS256),
        )
        .expect("GitHub App JWT is RS256-signed");
        let now = jsonwebtoken::get_current_timestamp();
        assert_eq!(decoded.claims.iss, "12345");
        assert!(decoded.claims.iat <= now);
        assert!(decoded.claims.iat >= now.saturating_sub(120));
        assert!(decoded.claims.exp > now);
        assert!(decoded.claims.exp - decoded.claims.iat <= 600);
        assert!(!format!("{forge:?}").contains(&private_key));
    }

    #[tokio::test]
    async fn fake_forge_reports_no_file_until_one_is_set() {
        let forge = FakeForge::new();
        assert_eq!(
            forge.read_file("acme", "navigator.yaml").await.unwrap(),
            None
        );
        forge.set_file("acme", "navigator.yaml", "version: 26.10.4\n");
        assert_eq!(
            forge
                .read_file("acme", "navigator.yaml")
                .await
                .unwrap()
                .as_deref(),
            Some("version: 26.10.4\n")
        );
        assert_eq!(
            forge.read_file("other", "navigator.yaml").await.unwrap(),
            None
        );
        forge.delete_repository("acme").await.unwrap();
        assert_eq!(
            forge.read_file("acme", "navigator.yaml").await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn github_forge_reads_a_committed_file_raw_and_reports_a_missing_one_as_none() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/an-organization/acme/contents/navigator.yaml"))
            .and(header("authorization", "Bearer test-token"))
            .and(header("accept", "application/vnd.github.raw+json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("version: 26.10.4\n"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/an-organization/acme/contents/missing.yaml"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;

        let forge = GitHubForge::from_lookup(lookup(&[
            (NAVIGATOR_GCP_PROJECT_ID, "neon-law-stg"),
            (NAVIGATOR_GITHUB_ORG, "an-organization"),
            (NAVIGATOR_GIT_HOST, "forge.example"),
            (NAVIGATOR_GITHUB_TOKEN_ENV, "test-token"),
            (GITHUB_API_BASE_ENV, server.uri().as_str()),
        ]))
        .expect("configured forge");
        assert_eq!(
            forge
                .read_file("acme", "navigator.yaml")
                .await
                .unwrap()
                .as_deref(),
            Some("version: 26.10.4\n")
        );
        assert_eq!(forge.read_file("acme", "missing.yaml").await.unwrap(), None);
    }

    #[tokio::test]
    async fn github_forge_adopts_an_existing_private_repository() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/an-organization/acme"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "html_url": "https://forge.example/an-organization/acme",
                "name": "acme",
                "private": true
            })))
            .expect(1)
            .mount(&server)
            .await;

        let forge = GitHubForge::from_lookup(lookup(&[
            (NAVIGATOR_GCP_PROJECT_ID, "neon-law-stg"),
            (NAVIGATOR_GITHUB_ORG, "an-organization"),
            (NAVIGATOR_GIT_HOST, "forge.example"),
            (NAVIGATOR_GITHUB_TOKEN_ENV, "test-token"),
            (GITHUB_API_BASE_ENV, server.uri().as_str()),
        ]))
        .expect("configured forge");
        let repo = forge.ensure_repository("acme").await.unwrap();
        assert_eq!(repo.name, "acme");
        assert_eq!(repo.url, "https://forge.example/an-organization/acme");
    }

    #[tokio::test]
    async fn github_forge_fails_closed_when_an_existing_repository_is_not_private() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/an-organization/acme"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "html_url": "https://forge.example/an-organization/acme",
                "name": "acme",
                "private": false
            })))
            .expect(1)
            .mount(&server)
            .await;

        let forge = GitHubForge::from_lookup(lookup(&[
            (NAVIGATOR_GCP_PROJECT_ID, "neon-law-stg"),
            (NAVIGATOR_GITHUB_ORG, "an-organization"),
            (NAVIGATOR_GITHUB_TOKEN_ENV, "test-token"),
            (GITHUB_API_BASE_ENV, server.uri().as_str()),
        ]))
        .expect("configured forge");

        let error = forge.ensure_repository("acme").await.unwrap_err();
        assert!(error.to_string().contains("acme"), "{error}");
        assert!(error.to_string().contains("not private"), "{error}");
    }

    #[tokio::test]
    async fn github_forge_creates_a_private_repository_when_none_exists() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/an-organization/acme"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/orgs/an-organization/repos"))
            .and(body_json(json!({
                "name": "acme",
                "private": true,
                "auto_init": false
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "html_url": "https://forge.example/an-organization/acme",
                "name": "acme",
                "private": true
            })))
            .expect(1)
            .mount(&server)
            .await;

        let forge = GitHubForge::from_lookup(lookup(&[
            (NAVIGATOR_GCP_PROJECT_ID, "neon-law-stg"),
            (NAVIGATOR_GITHUB_ORG, "an-organization"),
            (GITHUB_TOKEN_ENV, "test-token"),
            (GITHUB_API_BASE_ENV, server.uri().as_str()),
        ]))
        .expect("configured forge");
        let repo = forge.ensure_repository("acme").await.unwrap();
        assert_eq!(repo.url, "https://forge.example/an-organization/acme");
    }

    #[tokio::test]
    async fn github_forge_fails_closed_when_a_created_repository_is_not_private() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/an-organization/acme"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/orgs/an-organization/repos"))
            .and(body_json(json!({
                "name": "acme",
                "private": true,
                "auto_init": false
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "html_url": "https://forge.example/an-organization/acme",
                "name": "acme",
                "private": false
            })))
            .expect(1)
            .mount(&server)
            .await;

        let forge = GitHubForge::from_lookup(lookup(&[
            (NAVIGATOR_GCP_PROJECT_ID, "neon-law-stg"),
            (NAVIGATOR_GITHUB_ORG, "an-organization"),
            (GITHUB_TOKEN_ENV, "test-token"),
            (GITHUB_API_BASE_ENV, server.uri().as_str()),
        ]))
        .expect("configured forge");

        let error = forge.ensure_repository("acme").await.unwrap_err();
        assert!(error.to_string().contains("acme"), "{error}");
        assert!(error.to_string().contains("not private"), "{error}");
    }

    #[tokio::test]
    async fn github_forge_fails_closed_when_an_adopted_repository_is_not_private() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/an-organization/acme"))
            .respond_with(ResponseTemplate::new(404))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/orgs/an-organization/repos"))
            .respond_with(ResponseTemplate::new(422))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/an-organization/acme"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "html_url": "https://forge.example/an-organization/acme",
                "name": "acme",
                "private": false
            })))
            .expect(1)
            .mount(&server)
            .await;

        let forge = GitHubForge::from_lookup(lookup(&[
            (NAVIGATOR_GCP_PROJECT_ID, "neon-law-stg"),
            (NAVIGATOR_GITHUB_ORG, "an-organization"),
            (NAVIGATOR_GITHUB_TOKEN_ENV, "test-token"),
            (GITHUB_API_BASE_ENV, server.uri().as_str()),
        ]))
        .expect("configured forge");

        let error = forge.ensure_repository("acme").await.unwrap_err();
        assert!(error.to_string().contains("acme"), "{error}");
        assert!(error.to_string().contains("not private"), "{error}");
    }

    #[tokio::test]
    async fn github_forge_adopts_when_create_reports_the_name_is_taken() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/an-organization/acme"))
            .respond_with(ResponseTemplate::new(404))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/orgs/an-organization/repos"))
            .respond_with(ResponseTemplate::new(422))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/an-organization/acme"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "html_url": "https://forge.example/an-organization/acme",
                "name": "acme",
                "private": true
            })))
            .expect(1)
            .mount(&server)
            .await;

        let forge = GitHubForge::from_lookup(lookup(&[
            (NAVIGATOR_GCP_PROJECT_ID, "neon-law-stg"),
            (NAVIGATOR_GITHUB_ORG, "an-organization"),
            (NAVIGATOR_GITHUB_TOKEN_ENV, "test-token"),
            (GITHUB_API_BASE_ENV, server.uri().as_str()),
        ]))
        .expect("configured forge");
        let repo = forge.ensure_repository("acme").await.unwrap();
        assert_eq!(repo.url, "https://forge.example/an-organization/acme");
    }

    #[tokio::test]
    async fn github_forge_reads_the_default_branchs_head_sha() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/an-organization/acme"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "html_url": "https://forge.example/an-organization/acme",
                "name": "acme",
                "default_branch": "main"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/an-organization/acme/commits/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "sha": "deadbeefcafe"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let forge = GitHubForge::from_lookup(lookup(&[
            (NAVIGATOR_GCP_PROJECT_ID, "neon-law-stg"),
            (NAVIGATOR_GITHUB_ORG, "an-organization"),
            (NAVIGATOR_GITHUB_TOKEN_ENV, "test-token"),
            (GITHUB_API_BASE_ENV, server.uri().as_str()),
        ]))
        .expect("configured forge");
        let sha = forge.head_commit_sha("acme").await.unwrap();
        assert_eq!(sha, Some("deadbeefcafe".to_string()));
    }

    #[tokio::test]
    async fn github_forge_reads_the_default_branchs_head_commit_committed_at() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/an-organization/acme"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "html_url": "https://forge.example/an-organization/acme",
                "name": "acme",
                "default_branch": "main"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/an-organization/acme/commits/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "sha": "deadbeefcafe",
                "commit": { "committer": { "date": "2026-09-01T12:00:00Z" } }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let forge = GitHubForge::from_lookup(lookup(&[
            (NAVIGATOR_GCP_PROJECT_ID, "neon-law-stg"),
            (NAVIGATOR_GITHUB_ORG, "an-organization"),
            (NAVIGATOR_GITHUB_TOKEN_ENV, "test-token"),
            (GITHUB_API_BASE_ENV, server.uri().as_str()),
        ]))
        .expect("configured forge");
        let committed_at = forge.head_commit_committed_at("acme").await.unwrap();
        assert_eq!(committed_at, Some("2026-09-01T12:00:00Z".to_string()));
    }

    #[tokio::test]
    async fn github_forge_head_sha_is_none_for_a_repository_that_does_not_exist() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/an-organization/ghost"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;

        let forge = GitHubForge::from_lookup(lookup(&[
            (NAVIGATOR_GCP_PROJECT_ID, "neon-law-stg"),
            (NAVIGATOR_GITHUB_ORG, "an-organization"),
            (NAVIGATOR_GITHUB_TOKEN_ENV, "test-token"),
            (GITHUB_API_BASE_ENV, server.uri().as_str()),
        ]))
        .expect("configured forge");
        let sha = forge.head_commit_sha("ghost").await.unwrap();
        assert_eq!(sha, None);
    }

    #[tokio::test]
    async fn github_forge_deletes_the_repository() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/repos/an-organization/acme"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let forge = GitHubForge::from_lookup(lookup(&[
            (NAVIGATOR_GCP_PROJECT_ID, "neon-law-stg"),
            (NAVIGATOR_GITHUB_ORG, "an-organization"),
            (NAVIGATOR_GITHUB_TOKEN_ENV, "test-token"),
            (GITHUB_API_BASE_ENV, server.uri().as_str()),
        ]))
        .expect("configured forge");
        forge.delete_repository("acme").await.unwrap();
    }
}
