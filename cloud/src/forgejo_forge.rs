//! A Project's source repository on Forgejo, in the owning Firm's namespace.
//!
//! [`ForgejoForge`] implements [`ForgeService`] against the Forgejo v1 REST
//! API. One client serves one Firm: its namespace is the Firm's immutable slug,
//! so a Project's repository is `<firm-slug>/<project-code>` and a client
//! built for one Firm can never create, adopt, read, or delete a repository in
//! another's. Its credential is the Firm's own access token, resolved from the
//! Firm's encrypted integration secrets rather than an ambient process token.
//!
//! Adoption is verified, never assumed: an existing repository is adopted only
//! when the forge reports it private, owned by the Firm's namespace, and named
//! for the Project code. Every call is bounded by a request timeout.

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

use base64::Engine as _;

use crate::forgejo_governance::{
    FileUpdate, Forgejo, RepositoryGovernance, UpdateProposal, REQUIRED_CHECK_CONTEXT,
};

use crate::forge::{
    message_binds_tree, ForgeError, ForgeRepository, ForgeService, SOURCE_TREE_TRAILER,
};

/// The Forgejo instance's root URL, for example `https://forge.example.test`.
pub const NAVIGATOR_FORGEJO_ROOT_URL_ENV: &str = "NAVIGATOR_FORGEJO_ROOT_URL";
/// Selects the deployment's Project forge: `github` (the default) or
/// `forgejo`.
pub const NAVIGATOR_FORGE_BACKEND_ENV: &str = "NAVIGATOR_FORGE_BACKEND";
/// Every Project repository's one branch.
pub const DEFAULT_BRANCH: &str = "main";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const USER_AGENT: &str = concat!("neon-law-navigator/", env!("CARGO_PKG_VERSION"));

/// Which forge creates a deployment's Project repositories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeBackend {
    GitHub,
    Forgejo,
}

impl ForgeBackend {
    /// Read [`NAVIGATOR_FORGE_BACKEND_ENV`]. Unset is GitHub, the existing
    /// deployments' forge; any other value than the two names fails closed.
    ///
    /// # Errors
    /// [`ForgeError::InvalidConfig`] for an unknown value.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ForgeError> {
        match get(NAVIGATOR_FORGE_BACKEND_ENV).as_deref().map(str::trim) {
            None | Some("" | "github") => Ok(Self::GitHub),
            Some("forgejo") => Ok(Self::Forgejo),
            Some(_) => Err(ForgeError::InvalidConfig(NAVIGATOR_FORGE_BACKEND_ENV)),
        }
    }
}

/// One Firm's Forgejo namespace and credential.
#[derive(Clone)]
pub struct ForgejoForge {
    root: String,
    api_base: String,
    owner: String,
    token: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for ForgejoForge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ForgejoForge")
            .field("api_base", &self.api_base)
            .field("owner", &self.owner)
            .field("token", &"[redacted]")
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct CreateRepository<'a> {
    name: &'a str,
    private: bool,
    auto_init: bool,
    default_branch: &'a str,
}

#[derive(Deserialize)]
struct RepositoryBody {
    html_url: Option<String>,
    name: Option<String>,
    private: Option<bool>,
    owner: Option<OwnerBody>,
}

#[derive(Deserialize)]
struct OwnerBody {
    login: Option<String>,
}

#[derive(Serialize)]
struct ChangeFiles<'a> {
    branch: &'a str,
    message: String,
    files: Vec<ChangeFile<'a>>,
}

#[derive(Serialize)]
struct ChangeFile<'a> {
    operation: &'static str,
    path: &'a str,
    content: String,
}

#[derive(Deserialize)]
struct ChangeFilesReply {
    commit: Option<CommitReply>,
}

#[derive(Deserialize)]
struct CommitReply {
    sha: Option<String>,
}

#[derive(Deserialize)]
struct GitCommitBody {
    commit: Option<GitCommitDetail>,
}

#[derive(Deserialize)]
struct GitCommitDetail {
    message: Option<String>,
}

#[derive(Deserialize)]
struct BranchBody {
    commit: BranchCommit,
}

#[derive(Deserialize)]
struct BranchCommit {
    id: String,
    timestamp: Option<String>,
}

impl ForgejoForge {
    /// A client for one Firm's namespace.
    ///
    /// # Errors
    /// [`ForgeError::InvalidConfig`] when the root URL is not `https` (or
    /// `http` on a loopback test host), the namespace is not a valid slug, or
    /// the token is blank.
    pub fn new(root_url: &str, firm_slug: &str, token: &str) -> Result<Self, ForgeError> {
        let root = root_url.trim().trim_end_matches('/');
        let loopback = root.starts_with("http://127.0.0.1") || root.starts_with("http://localhost");
        if !(root.starts_with("https://") || loopback) {
            return Err(ForgeError::InvalidConfig(NAVIGATOR_FORGEJO_ROOT_URL_ENV));
        }
        if !crate::workspace::is_valid_slug(firm_slug) {
            return Err(ForgeError::InvalidConfig("Firm slug"));
        }
        if token.trim().is_empty() {
            return Err(ForgeError::MissingConfig("Firm Forgejo access token"));
        }
        Ok(Self {
            root: format!("{root}/"),
            api_base: format!("{root}/api/v1"),
            owner: firm_slug.to_string(),
            token: token.trim().to_string(),
            http: reqwest::Client::new(),
        })
    }

    /// The Firm namespace this client is confined to.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    fn repo_url(&self, project_code: &str) -> Result<String, ForgeError> {
        if !crate::workspace::is_valid_slug(project_code) {
            return Err(ForgeError::InvalidConfig("Project code"));
        }
        Ok(format!(
            "{}/repos/{}/{project_code}",
            self.api_base, self.owner
        ))
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
    ) -> Result<reqwest::Response, ForgeError> {
        let response = response.map_err(|source| ForgeError::Request { action, source })?;
        match response.status().as_u16() {
            200..=299 => Ok(response),
            401 | 403 => Err(ForgeError::Authentication),
            status => Err(ForgeError::Api { action, status }),
        }
    }

    /// Verify the forge's description of a repository before adopting it.
    async fn verified(
        &self,
        project_code: &str,
        response: reqwest::Response,
        action: &'static str,
    ) -> Result<ForgeRepository, ForgeError> {
        let body = response
            .json::<RepositoryBody>()
            .await
            .map_err(|source| ForgeError::Response { action, source })?;
        let url = body.html_url.ok_or(ForgeError::MissingUrl { action })?;
        let name = body.name.ok_or(ForgeError::MissingUrl { action })?;
        let owner = body.owner.and_then(|owner| owner.login);
        if name != project_code || owner.as_deref() != Some(self.owner.as_str()) {
            return Err(ForgeError::ForeignRepository { name });
        }
        if body.private != Some(true) {
            return Err(ForgeError::NotPrivate { name });
        }
        Ok(ForgeRepository { url, name })
    }

    async fn branch(&self, project_code: &str) -> Result<Option<BranchCommit>, ForgeError> {
        let action = "reading the default branch";
        let url = format!("{}/branches/{DEFAULT_BRANCH}", self.repo_url(project_code)?);
        let response = self.request(self.http.get(url)).send().await;
        let response = match response {
            // A missing repository and an empty one both have no branch.
            Ok(response) if response.status().as_u16() == 404 => return Ok(None),
            other => Self::checked(other, action)?,
        };
        Ok(Some(
            response
                .json::<BranchBody>()
                .await
                .map_err(|source| ForgeError::Response { action, source })?
                .commit,
        ))
    }
}

impl ForgejoForge {
    /// Whether the commit `sha` carries `tree_digest` in its trailer.
    async fn commit_binds_tree(
        &self,
        project_code: &str,
        sha: &str,
        tree_digest: &str,
    ) -> Result<bool, ForgeError> {
        let action = "reading the head commit";
        let url = format!("{}/git/commits/{sha}", self.repo_url(project_code)?);
        let response = Self::checked(self.request(self.http.get(url)).send().await, action)?;
        let body = response
            .json::<GitCommitBody>()
            .await
            .map_err(|source| ForgeError::Response { action, source })?;
        Ok(body
            .commit
            .and_then(|commit| commit.message)
            .is_some_and(|message| message_binds_tree(&message, tree_digest)))
    }

    /// The head commit if it already holds this tree; refuses other source.
    async fn applied(
        &self,
        project_code: &str,
        tree_digest: &str,
    ) -> Result<Option<String>, ForgeError> {
        let Some(head) = self.head_commit_sha(project_code).await? else {
            return Ok(None);
        };
        if self
            .commit_binds_tree(project_code, &head, tree_digest)
            .await?
        {
            Ok(Some(head))
        } else {
            Err(ForgeError::RepositoryNotEmpty {
                name: project_code.to_string(),
            })
        }
    }
}

#[async_trait]
impl ForgeService for ForgejoForge {
    async fn find_repository(
        &self,
        project_code: &str,
    ) -> Result<Option<ForgeRepository>, ForgeError> {
        let action = "finding repository";
        let response = self
            .request(self.http.get(self.repo_url(project_code)?))
            .send()
            .await;
        let response = match response {
            Ok(response) if response.status().as_u16() == 404 => return Ok(None),
            other => Self::checked(other, action)?,
        };
        self.verified(project_code, response, action)
            .await
            .map(Some)
    }

    async fn ensure_repository(&self, project_code: &str) -> Result<ForgeRepository, ForgeError> {
        if let Some(existing) = self.find_repository(project_code).await? {
            tracing::info!(
                project_code,
                outcome = "already_exists",
                "Forgejo repository provisioning completed"
            );
            return Ok(existing);
        }
        let action = "creating repository";
        let created = self
            .request(
                self.http
                    .post(format!("{}/orgs/{}/repos", self.api_base, self.owner))
                    .json(&CreateRepository {
                        name: project_code,
                        private: true,
                        auto_init: false,
                        default_branch: DEFAULT_BRANCH,
                    }),
            )
            .send()
            .await;
        if created
            .as_ref()
            .is_ok_and(|response| response.status().as_u16() == 409)
        {
            // Another writer created it first: adopt only after verifying it.
            return self
                .find_repository(project_code)
                .await?
                .ok_or(ForgeError::Api {
                    action,
                    status: 409,
                });
        }
        let response = Self::checked(created, action)?;
        let repository = self.verified(project_code, response, action).await?;
        tracing::info!(
            project_code,
            outcome = "created",
            "Forgejo repository provisioning completed"
        );
        Ok(repository)
    }

    async fn head_commit_sha(&self, project_code: &str) -> Result<Option<String>, ForgeError> {
        Ok(self.branch(project_code).await?.map(|commit| commit.id))
    }

    async fn head_commit_committed_at(
        &self,
        project_code: &str,
    ) -> Result<Option<String>, ForgeError> {
        Ok(self
            .branch(project_code)
            .await?
            .and_then(|commit| commit.timestamp))
    }

    async fn delete_repository(&self, project_code: &str) -> Result<(), ForgeError> {
        let response = self
            .request(self.http.delete(self.repo_url(project_code)?))
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
        let url = format!(
            "{}/raw/{}?ref={DEFAULT_BRANCH}",
            self.repo_url(project_code)?,
            path.trim_start_matches('/')
        );
        let response = self.request(self.http.get(url)).send().await;
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

    async fn initialize_source(
        &self,
        project_code: &str,
        files: &BTreeMap<String, String>,
        tree_digest: &str,
        message: &str,
    ) -> Result<String, ForgeError> {
        if let Some(head) = self.applied(project_code, tree_digest).await? {
            return Ok(head);
        }
        let action = "writing initial source";
        let body = ChangeFiles {
            branch: DEFAULT_BRANCH,
            message: format!("{message}\n\n{SOURCE_TREE_TRAILER} {tree_digest}"),
            files: files
                .iter()
                .map(|(path, text)| ChangeFile {
                    operation: "create",
                    path,
                    content: base64::engine::general_purpose::STANDARD.encode(text.as_bytes()),
                })
                .collect(),
        };
        let url = format!("{}/contents", self.repo_url(project_code)?);
        let written = self.request(self.http.post(url).json(&body)).send().await;
        let outcome = match Self::checked(written, action) {
            Ok(response) => response
                .json::<ChangeFilesReply>()
                .await
                .ok()
                .and_then(|reply| reply.commit)
                .and_then(|commit| commit.sha),
            Err(ForgeError::Authentication) => return Err(ForgeError::Authentication),
            // The write may have landed before the failure; the head settles it.
            Err(error) => {
                tracing::warn!(%error, project_code, "initial source write outcome unknown");
                None
            }
        };
        match self.applied(project_code, tree_digest).await? {
            Some(head) if outcome.as_deref().is_none_or(|sha| sha == head) => Ok(head),
            Some(_) => Err(ForgeError::Api {
                action,
                status: 409,
            }),
            None => Err(ForgeError::Api {
                action,
                status: 502,
            }),
        }
    }
}

impl ForgejoForge {
    fn governance(&self, project_code: &str) -> anyhow::Result<(Forgejo, String)> {
        anyhow::ensure!(
            crate::workspace::is_valid_slug(project_code),
            "Project code must be a slug"
        );
        Ok((
            Forgejo::new(&self.root, self.token.clone())?,
            format!("{}/{project_code}", self.owner),
        ))
    }
}

#[async_trait]
impl RepositoryGovernance for ForgejoForge {
    async fn reconcile_governance(&self, project_code: &str) -> anyhow::Result<Vec<String>> {
        let (forgejo, repository) = self.governance(project_code)?;
        forgejo
            .reconcile(&repository, REQUIRED_CHECK_CONTEXT, false)
            .await
    }

    async fn propose_file_update(
        &self,
        project_code: &str,
        update: &FileUpdate<'_>,
    ) -> anyhow::Result<UpdateProposal> {
        let (forgejo, repository) = self.governance(project_code)?;
        forgejo
            .propose_file_update(&repository, REQUIRED_CHECK_CONTEXT, update)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::{ForgeBackend, ForgejoForge, NAVIGATOR_FORGE_BACKEND_ENV};
    use crate::forge::{ForgeError, ForgeService};
    use serde_json::json;
    use wiremock::matchers::{body_json, body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn repo(owner: &str, name: &str, private: bool) -> serde_json::Value {
        json!({
            "html_url": format!("https://forge.example.test/{owner}/{name}"),
            "name": name,
            "private": private,
            "owner": { "login": owner },
        })
    }

    fn forge(server: &MockServer, firm: &str) -> ForgejoForge {
        ForgejoForge::new(&server.uri(), firm, "synthetic-token").unwrap()
    }

    #[tokio::test]
    async fn governance_repairs_the_firms_project_repository_with_the_gate_check() {
        use crate::forgejo_governance::{Policy, RepositoryGovernance, REQUIRED_CHECK_CONTEXT};
        let server = MockServer::start().await;
        let api = "/api/v1/repos/acme-law/sample-estate";
        let mut governed = Policy::repository();
        governed["allow_manual_merge"] = false.into();
        let main = Policy::new(REQUIRED_CHECK_CONTEXT).unwrap().branch();
        // Drifted on the first read; governed once written.
        for (route, before, after) in [
            (api.to_string(), json!({}), governed),
            (
                format!("{api}/branch_protections"),
                json!([]),
                json!([main.clone()]),
            ),
        ] {
            Mock::given(method("GET"))
                .and(path(route.clone()))
                .respond_with(ResponseTemplate::new(200).set_body_json(before))
                .up_to_n_times(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path(route))
                .respond_with(ResponseTemplate::new(200).set_body_json(after))
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path(format!("{api}/branch_protections/main")))
            .respond_with(ResponseTemplate::new(404))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{api}/branch_protections/main")))
            .respond_with(ResponseTemplate::new(200).set_body_json(main))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("{api}/branch_protections")))
            .and(body_partial_json(json!({
                "rule_name": "main", "enable_push": false, "require_signed_commits": true,
                "apply_to_admins": true, "status_check_contexts": ["gate / gate (pull_request)"],
            })))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(api))
            .and(body_partial_json(
                json!({"allow_squash_merge": true, "allow_merge_commits": false}),
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let drift = forge(&server, "acme-law")
            .reconcile_governance("sample-estate")
            .await
            .unwrap();
        assert!(drift.contains(&"main.enable_push".to_string()), "{drift:?}");
        assert!(drift.contains(&"repository.allow_squash_merge".to_string()));
        assert!(forge(&server, "acme-law")
            .reconcile_governance("../other")
            .await
            .is_err());
    }

    #[test]
    fn the_backend_defaults_to_github_and_rejects_unknown_names() {
        assert_eq!(
            ForgeBackend::from_lookup(|_| None).unwrap(),
            ForgeBackend::GitHub
        );
        assert_eq!(
            ForgeBackend::from_lookup(|_| Some("forgejo".into())).unwrap(),
            ForgeBackend::Forgejo
        );
        assert!(matches!(
            ForgeBackend::from_lookup(|_| Some("gitlab".into())),
            Err(ForgeError::InvalidConfig(NAVIGATOR_FORGE_BACKEND_ENV))
        ));
    }

    #[test]
    fn a_client_refuses_plaintext_hosts_blank_tokens_and_bad_namespaces() {
        assert!(ForgejoForge::new("http://forge.example.test", "firm", "t").is_err());
        assert!(ForgejoForge::new("https://forge.example.test", "Firm A", "t").is_err());
        assert!(ForgejoForge::new("https://forge.example.test", "firm", " ").is_err());
        let forge = ForgejoForge::new("https://forge.example.test/", "firm-a", "secret").unwrap();
        assert!(!format!("{forge:?}").contains("secret"));
    }

    #[tokio::test]
    async fn ensure_creates_a_private_repository_in_the_firms_namespace() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/firm-a/matter-1"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/orgs/firm-a/repos"))
            .and(header("authorization", "token synthetic-token"))
            .and(body_json(json!({
                "name": "matter-1",
                "private": true,
                "auto_init": false,
                "default_branch": "main",
            })))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(repo("firm-a", "matter-1", true)),
            )
            .expect(1)
            .mount(&server)
            .await;
        let created = forge(&server, "firm-a")
            .ensure_repository("matter-1")
            .await
            .unwrap();
        assert_eq!(created.url, "https://forge.example.test/firm-a/matter-1");
    }

    #[tokio::test]
    async fn adoption_requires_private_same_owner_same_name() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/firm-a/public-matter"))
            .respond_with(ResponseTemplate::new(200).set_body_json(repo(
                "firm-a",
                "public-matter",
                false,
            )))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/firm-a/moved-matter"))
            .respond_with(ResponseTemplate::new(200).set_body_json(repo(
                "firm-b",
                "moved-matter",
                true,
            )))
            .mount(&server)
            .await;
        let forge = forge(&server, "firm-a");
        assert!(matches!(
            forge.ensure_repository("public-matter").await,
            Err(ForgeError::NotPrivate { .. })
        ));
        assert!(matches!(
            forge.ensure_repository("moved-matter").await,
            Err(ForgeError::ForeignRepository { .. })
        ));
    }

    #[tokio::test]
    async fn a_create_race_adopts_only_after_verification() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/firm-a/matter-2"))
            .respond_with(ResponseTemplate::new(404))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/orgs/firm-a/repos"))
            .respond_with(ResponseTemplate::new(409))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/firm-a/matter-2"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(repo("firm-a", "matter-2", true)),
            )
            .mount(&server)
            .await;
        let adopted = forge(&server, "firm-a")
            .ensure_repository("matter-2")
            .await
            .unwrap();
        assert_eq!(adopted.name, "matter-2");
    }

    #[tokio::test]
    async fn an_empty_repository_has_no_head_and_a_populated_one_reports_it() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/firm-a/empty/branches/main"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/firm-a/full/branches/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "commit": { "id": "a".repeat(40), "timestamp": "2026-10-18T10:00:00Z" }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/firm-a/full/raw/navigator.yaml"))
            .respond_with(ResponseTemplate::new(200).set_body_string("code: full\n"))
            .mount(&server)
            .await;
        let forge = forge(&server, "firm-a");
        assert_eq!(forge.head_commit_sha("empty").await.unwrap(), None);
        assert_eq!(
            forge.head_commit_sha("full").await.unwrap(),
            Some("a".repeat(40))
        );
        assert_eq!(
            forge
                .head_commit_committed_at("full")
                .await
                .unwrap()
                .as_deref(),
            Some("2026-10-18T10:00:00Z")
        );
        assert_eq!(
            forge
                .read_file("full", "navigator.yaml")
                .await
                .unwrap()
                .as_deref(),
            Some("code: full\n")
        );
        assert_eq!(
            forge.read_file("empty", "navigator.yaml").await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn initial_source_is_written_once_and_a_lost_reply_is_settled_by_the_head() {
        use std::collections::BTreeMap;
        let server = MockServer::start().await;
        let digest = "d".repeat(64);
        let sha = "c".repeat(40);
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/firm-a/matter-4/branches/main"))
            .respond_with(ResponseTemplate::new(404))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // The write lands but its reply is lost.
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/firm-a/matter-4/contents"))
            .respond_with(ResponseTemplate::new(504))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/firm-a/matter-4/branches/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "commit": { "id": sha, "timestamp": "2026-10-18T10:00:00Z" }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/api/v1/repos/firm-a/matter-4/git/commits/{sha}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "commit": { "message": format!("Initialize source\n\nSource-Tree: {digest}") }
            })))
            .mount(&server)
            .await;
        let files: BTreeMap<String, String> =
            [("navigator.yaml".to_string(), "version: 1\n".to_string())].into();
        let forge = forge(&server, "firm-a");
        assert_eq!(
            forge
                .initialize_source("matter-4", &files, &digest, "Initialize source")
                .await
                .unwrap(),
            sha
        );
        // A replay recognizes the applied tree and writes nothing more.
        assert_eq!(
            forge
                .initialize_source("matter-4", &files, &digest, "Initialize source")
                .await
                .unwrap(),
            sha
        );
        // Different source is never replaced.
        assert!(matches!(
            forge
                .initialize_source("matter-4", &files, &"e".repeat(64), "Initialize source")
                .await,
            Err(ForgeError::RepositoryNotEmpty { .. })
        ));
    }

    #[tokio::test]
    async fn a_rejected_credential_is_an_authentication_failure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/firm-a/matter-3"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        assert!(matches!(
            forge(&server, "firm-a").find_repository("matter-3").await,
            Err(ForgeError::Authentication)
        ));
        assert!(matches!(
            forge(&server, "firm-a").find_repository("../escape").await,
            Err(ForgeError::InvalidConfig(_))
        ));
    }
}
