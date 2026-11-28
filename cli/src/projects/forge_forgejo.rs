//! Forgejo's governed review and merge loop for a Project checkout.

use anyhow::{bail, ensure, Context, Result};
use async_trait::async_trait;
use model::forgejo_governance::{Forgejo, MergeRequestOutcome, REQUIRED_CHECK_CONTEXT};
use reqwest::Method;
use serde::Deserialize;
use serde_json::json;
use std::path::Path;
use std::time::Duration;

use super::forge::{
    ArmOutcome, CheckVerdict, DeploymentJob, DeploymentRun, Forge, ReviewRef, ReviewRequest,
};
use super::lifecycle::git_text;

pub struct ForgejoForge {
    http: reqwest::Client,
    root: String,
    repository: String,
    token: String,
    /// Where `token` came from, named in the message when Forgejo refuses it.
    credential: String,
    governance: Forgejo,
}

impl ForgejoForge {
    pub fn new(root: &str, repository: &str, token: String) -> Result<Self> {
        model::forgejo_governance::repository_target(repository)?;
        let governance = Forgejo::new(root, token.clone())?;
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            root: root.trim_end_matches('/').into(),
            repository: repository.into(),
            token,
            credential: "the supplied token".into(),
            governance,
        })
    }

    fn with_credential(mut self, credential: String) -> Self {
        self.credential = credential;
        self
    }

    pub fn from_checkout(dir: &Path) -> Result<Self> {
        let remote = git_text(dir, &["remote", "get-url", "origin"])?;
        Self::from_remote(&remote)
    }

    pub(super) fn from_remote(remote: &str) -> Result<Self> {
        Self::from_remote_with(remote, |root| git_credential_token(root, &[]))
    }

    /// Bind a checkout's remote to the forge it names. The root is
    /// `NAVIGATOR_FORGEJO_ROOT_URL` when set, otherwise the remote's own host,
    /// which callers have already tied to the Project's recorded source. The
    /// token is `FORGEJO_TOKEN` when set, otherwise the credential git holds
    /// for that host, so an agent shell signed in over git needs no variables.
    fn from_remote_with(
        remote: &str,
        git_token: impl FnOnce(&str) -> Result<String>,
    ) -> Result<Self> {
        let root = match set_env("NAVIGATOR_FORGEJO_ROOT_URL") {
            Some(root) => root,
            None => Self::remote_root(remote)?,
        };
        let repository = Self::remote_repository(&root, remote)?;
        let (token, credential) = match set_env("FORGEJO_TOKEN") {
            Some(token) => (token, "FORGEJO_TOKEN".to_owned()),
            None => (
                git_token(&root)?,
                format!(
                    "the git credential for {}",
                    root.trim_start_matches("https://")
                ),
            ),
        };
        Ok(Self::new(&root, &repository, token)?.with_credential(credential))
    }

    /// The forge's origin as the remote names it: its scheme and authority,
    /// with ssh and scp-style remotes read as the forge's https origin.
    fn remote_root(remote: &str) -> Result<String> {
        if !remote.contains("://") {
            let (user_host, path) = remote
                .split_once(':')
                .context("checkout remote is not a Forgejo URL")?;
            ensure!(
                !path.starts_with('/') && !user_host.contains('/'),
                "checkout remote is not a Forgejo URL"
            );
            let host = user_host.rsplit('@').next().unwrap_or(user_host);
            return Ok(format!("https://{host}"));
        }
        let url = url::Url::parse(remote).context("parse Forgejo origin remote")?;
        let host = url.host_str().context("Forgejo origin needs a host")?;
        match url.scheme() {
            "ssh" => Ok(format!("https://{host}")),
            scheme @ ("http" | "https") => Ok(match url.port() {
                Some(port) => format!("{scheme}://{host}:{port}"),
                None => format!("{scheme}://{host}"),
            }),
            scheme => bail!("checkout remote scheme `{scheme}` is not a Forgejo origin"),
        }
    }

    fn remote_repository(root: &str, remote: &str) -> Result<String> {
        let origin = url::Url::parse(root)?;
        let host = origin.host_str().context("Forgejo origin needs a host")?;
        if !remote.contains("://") {
            if let Some((_, path)) = remote
                .split_once('@')
                .filter(|(_, rest)| rest.starts_with(&format!("{host}:")))
                .map(|(user, rest)| (user, &rest[host.len() + 1..]))
            {
                return Ok(path.trim_end_matches(".git").to_owned());
            }
        }
        let url = url::Url::parse(remote).context("parse Forgejo origin remote")?;
        ensure!(
            url.host_str() == Some(host) && url.query().is_none() && url.fragment().is_none(),
            "checkout remote differs from configured Forgejo host"
        );
        let path = if url.scheme() == "ssh" {
            url.path().trim_start_matches('/')
        } else {
            // `https://user@host/...` is the same origin; a password is not.
            ensure!(
                url.origin() == origin.origin() && url.password().is_none(),
                "checkout remote differs from configured Forgejo origin"
            );
            let prefix = format!("{}/", origin.path().trim_end_matches('/'));
            url.path()
                .strip_prefix(&prefix)
                .context("checkout remote is outside the configured Forgejo root")?
        };
        Ok(path.trim_end_matches(".git").to_owned())
    }

    pub(super) async fn review_page(&self, page: usize) -> Result<Vec<super::reviews::Review>> {
        self.get(&format!("pulls?state=open&limit=50&page={page}"))
            .await
    }

    pub(super) async fn require_review_team(&self, team: &str, dry_run: bool) -> Result<()> {
        self.governance
            .require_review_team(&self.repository, team, dry_run)
            .await?;
        if dry_run {
            println!(
                "Review policy preview: main requires the selected team's approval after apply."
            );
        } else {
            println!("Verified the required review team on main.");
        }
        Ok(())
    }

    pub(super) async fn reconcile_policy(&self, dry_run: bool) -> Result<()> {
        let drift = self
            .governance
            .reconcile(&self.repository, REQUIRED_CHECK_CONTEXT, dry_run)
            .await?;
        if drift.is_empty() {
            println!("Repository review settings match.");
        } else {
            let action = if dry_run {
                "Would reconcile"
            } else {
                "Reconciled"
            };
            println!("{action} {}", drift.join(", "));
        }
        Ok(())
    }

    /// Read the bound primary's review controls and Actions setting.
    pub(super) async fn verify_workspace(&self) -> Result<()> {
        let repository: serde_json::Value = self
            .request(Method::GET, &format!("repos/{}", self.repository), None)
            .await?;
        ensure!(
            repository["has_actions"].as_bool() == Some(true),
            "Forgejo Actions are disabled for the Project's primary repository"
        );
        self.governance
            .verify(
                &self.repository,
                &model::forgejo_governance::Policy::new(REQUIRED_CHECK_CONTEXT)?,
            )
            .await
    }

    async fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<T> {
        let mut request = self
            .http
            .request(method, format!("{}/api/v1/{path}", self.root))
            .header("Authorization", format!("token {}", self.token));
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await?;
        ensure!(
            !matches!(response.status().as_u16(), 401 | 403),
            "Forgejo refused {}; sign in again with git for this host, or set FORGEJO_TOKEN to a token with repository access",
            self.credential
        );
        response
            .error_for_status()
            .context("Forgejo request refused")?
            .json()
            .await
            .context("decode Forgejo response")
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, suffix: &str) -> Result<T> {
        self.request(
            Method::GET,
            &format!("repos/{}/{suffix}", self.repository),
            None,
        )
        .await
    }

    async fn pull(&self, review: &ReviewRef) -> Result<Pull> {
        let pull: Pull = self.get(&format!("pulls/{}", review.number)).await?;
        ensure!(
            pull.head.sha == review.head_sha,
            "review head changed; re-run the verb before merging"
        );
        Ok(pull)
    }
    async fn workflow_run(
        &self,
        merge_commit: &str,
        event: &str,
        workflow: &str,
    ) -> Result<Option<DeploymentRun>> {
        #[derive(Deserialize)]
        struct Runs {
            workflow_runs: Vec<Run>,
        }
        #[derive(Deserialize)]
        struct Run {
            id: u64,
            index_in_repo: u64,
            commit_sha: String,
            workflow_id: String,
            event: String,
            html_url: String,
            status: String,
        }
        #[derive(Deserialize)]
        struct Tasks {
            workflow_runs: Vec<Task>,
        }
        #[derive(Deserialize)]
        struct Task {
            name: String,
            status: String,
            head_sha: String,
            workflow_id: String,
            run_number: u64,
        }
        let runs: Runs = self
            .get(&format!(
                "actions/runs?head_sha={merge_commit}&event={event}&workflow_id={workflow}&limit=50"
            ))
            .await?;
        let Some(run) = runs
            .workflow_runs
            .into_iter()
            .filter(|run| {
                run.commit_sha == merge_commit && run.event == event && run.workflow_id == workflow
            })
            .max_by_key(|run| run.id)
        else {
            return Ok(None);
        };
        let mut page = 1;
        let mut jobs = Vec::new();
        loop {
            let tasks: Tasks = self
                .get(&format!("actions/tasks?page={page}&limit=50"))
                .await?;
            let count = tasks.workflow_runs.len();
            jobs.extend(
                tasks
                    .workflow_runs
                    .into_iter()
                    .filter(|task| {
                        task.head_sha == merge_commit
                            && task.workflow_id == run.workflow_id
                            && task.run_number == run.index_in_repo
                    })
                    .map(|task| DeploymentJob {
                        name: task.name,
                        conclusion: terminal(&task.status),
                    }),
            );
            if count < 50 {
                break;
            }
            page += 1;
        }
        Ok(Some(DeploymentRun {
            url: run.html_url,
            commit: run.commit_sha,
            conclusion: terminal(&run.status),
            jobs,
        }))
    }
}

#[derive(Deserialize)]
struct Branch {
    sha: String,
    #[serde(rename = "ref")]
    name: String,
    label: Option<String>,
    repo: Option<Repository>,
}
#[derive(Deserialize)]
struct Repository {
    full_name: String,
}
#[derive(Deserialize)]
struct Pull {
    number: u64,
    head: Branch,
    base: Branch,
    html_url: String,
    draft: bool,
    merged: bool,
    merge_commit_sha: Option<String>,
    mergeable: bool,
}
impl Pull {
    fn review(self, allow_draft: bool) -> Result<(ReviewRef, bool)> {
        ensure!(
            allow_draft || !self.draft,
            "the existing review request is a draft; mark it ready before continuing"
        );
        Ok((
            ReviewRef {
                number: self.number,
                head_sha: self.head.sha,
                url: self.html_url,
            },
            self.draft,
        ))
    }
}

impl ForgejoForge {
    async fn open(&self, request: ReviewRequest, draft: bool) -> Result<(ReviewRef, bool)> {
        let mut page = 1;
        loop {
            let pulls: Vec<Pull> = self
                .get(&format!("pulls?state=open&page={page}&limit=50"))
                .await?;
            let count = pulls.len();
            for pull in pulls {
                if pull.head.name == request.head_branch
                    && pull
                        .head
                        .repo
                        .as_ref()
                        .is_some_and(|repo| repo.full_name == self.repository)
                {
                    ensure!(
                        pull.base.name == request.base_branch,
                        "existing review request targets another base"
                    );
                    return pull.review(draft);
                }
            }
            if count < 50 {
                break;
            }
            page += 1;
        }
        let pull: Pull = self
            .request(
                Method::POST,
                &format!("repos/{}/pulls", self.repository),
                Some(&json!({"title": if draft { format!("WIP: {}", request.title) } else { request.title }, "head": request.head_branch,
                "base": request.base_branch, "body": request.body})),
            )
            .await?;
        ensure!(
            pull.head
                .repo
                .as_ref()
                .is_some_and(|repo| repo.full_name == self.repository),
            "created review request belongs to another source repository"
        );
        ensure!(
            pull.base.name == request.base_branch && pull.head.name == request.head_branch,
            "forge returned different review branches"
        );
        ensure!(!draft || pull.draft, "forge did not create a draft");
        pull.review(draft)
    }
}

#[async_trait]
impl super::release_review::ReleaseForge for ForgejoForge {
    async fn source(&self) -> Result<super::release_review::Source> {
        let source: super::release_review::Source = self
            .request(Method::GET, &format!("repos/{}", self.repository), None)
            .await?;
        source.verify(&self.repository)?;
        Ok(source)
    }

    async fn checks(&self, review: &ReviewRef, base: &str) -> Result<bool> {
        #[derive(Deserialize)]
        struct Protection {
            enable_status_check: bool,
            status_check_contexts: Vec<String>,
        }
        ensure!(
            self.pull(review).await?.base.name == base,
            "review base changed"
        );
        let combined: serde_json::Value = self
            .get(&format!("commits/{}/status", review.head_sha))
            .await?;
        ensure!(
            combined["sha"] == review.head_sha,
            "check result belongs to another commit"
        );
        let statuses = combined["statuses"]
            .as_array()
            .context("missing status list")?;
        let rules: Protection = self.get(&format!("branch_protections/{base}")).await?;
        ensure!(
            rules.enable_status_check && !rules.status_check_contexts.is_empty(),
            "base branch has no required check"
        );
        let mut passed = true;
        for context in rules.status_check_contexts {
            let latest = statuses
                .iter()
                .filter(|s| s["context"] == context)
                .max_by_key(|s| s["id"].as_u64());
            if let Some(check) = latest {
                match check["status"].as_str().or_else(|| check["state"].as_str()) {
                    Some("success") => (),
                    Some("pending" | "running") => passed = false,
                    _ => bail!("required Forgejo check {context} did not pass"),
                }
            } else {
                passed = false;
            }
        }
        Ok(passed)
    }

    async fn draft(&self, request: ReviewRequest) -> Result<(ReviewRef, bool)> {
        self.open(request, true).await
    }
}

#[async_trait]
impl Forge for ForgejoForge {
    async fn identity(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct RepositoryAccess {
            full_name: String,
            permissions: PushPermission,
        }
        #[derive(Deserialize)]
        struct PushPermission {
            push: bool,
        }
        let access: RepositoryAccess = self.get("").await?;
        ensure!(
            access.full_name == self.repository && access.permissions.push,
            "Forgejo credential needs push access to the selected repository"
        );
        Ok(format!("repository credential for {}", access.full_name))
    }

    async fn remote_reachable(&self) -> Result<()> {
        self.get::<serde_json::Value>("").await?;
        Ok(())
    }

    async fn open_review(&self, request: ReviewRequest) -> Result<ReviewRef> {
        self.open(request, false).await.map(|(review, _)| review)
    }

    async fn find_merged_review(
        &self,
        branch: &str,
        base: &str,
        sha: &str,
    ) -> Result<Option<ReviewRef>> {
        let mut page = 1;
        loop {
            let pulls: Vec<Pull> = self
                .get(&format!("pulls?state=closed&page={page}&limit=50"))
                .await?;
            let count = pulls.len();
            for pull in pulls {
                // Forgejo keeps the original topic in the label after it
                // replaces a deleted branch with the review's retained ref.
                let matches_topic = pull.head.name == branch
                    || (pull.head.label.as_deref() == Some(branch)
                        && pull.head.name == format!("refs/pull/{}/head", pull.number));
                if pull.merged
                    && pull.base.name == base
                    && matches_topic
                    && pull.head.sha == sha
                    && pull
                        .head
                        .repo
                        .as_ref()
                        .is_some_and(|repo| repo.full_name == self.repository)
                {
                    return pull.review(false).map(|(review, _)| Some(review));
                }
            }
            if count < 50 {
                return Ok(None);
            }
            page += 1;
        }
    }

    async fn arm_merge(&self, review: &ReviewRef) -> Result<ArmOutcome> {
        if let Some(merge_commit) = self.merge_commit(review).await? {
            return Ok(ArmOutcome::MergedNow { merge_commit });
        }
        match self
            .governance
            .schedule_expected(
                &self.repository,
                REQUIRED_CHECK_CONTEXT,
                review.number,
                Some(&review.head_sha),
            )
            .await?
        {
            MergeRequestOutcome::ConflictNeedsInspection => {
                bail!("Forgejo reports a merge conflict")
            }
            MergeRequestOutcome::Accepted => {
                if let Some(merge_commit) = self.merge_commit(review).await? {
                    Ok(ArmOutcome::MergedNow { merge_commit })
                } else {
                    // Forgejo's API acknowledges scheduling, but exposes no queue read API.
                    // Only the observed merge proves completion; the watcher keeps polling.
                    Ok(ArmOutcome::Scheduled)
                }
            }
        }
    }

    async fn check_verdict(&self, review: &ReviewRef) -> Result<CheckVerdict> {
        #[derive(Deserialize)]
        struct Status {
            context: String,
            #[serde(alias = "status")]
            state: String,
        }
        #[derive(Deserialize)]
        struct Combined {
            statuses: Vec<Status>,
        }
        let pull = self.pull(review).await?;
        let combined: Combined = self
            .get(&format!("commits/{}/status", review.head_sha))
            .await?;
        let Some(status) = combined
            .statuses
            .iter()
            .find(|status| status.context == REQUIRED_CHECK_CONTEXT)
        else {
            return Ok(CheckVerdict::Pending);
        };
        Ok(match status.state.as_str() {
            "failure" | "error" => CheckVerdict::Failed(status.context.clone()),
            "success" if pull.merged || pull.mergeable => CheckVerdict::Green,
            "success" => CheckVerdict::Conflict,
            _ => CheckVerdict::Pending,
        })
    }

    async fn merge_commit(&self, review: &ReviewRef) -> Result<Option<String>> {
        let pull = self.pull(review).await?;
        Ok(if pull.merged {
            pull.merge_commit_sha.filter(|sha| !sha.is_empty())
        } else {
            None
        })
    }

    async fn deployment(&self, merge_commit: &str) -> Result<Option<DeploymentRun>> {
        self.workflow_run(merge_commit, "push", "cd.yml").await
    }

    async fn verification(&self, review: &ReviewRef) -> Result<Option<DeploymentRun>> {
        self.workflow_run(&review.head_sha, "pull_request", "gate.yml")
            .await
    }
}

fn terminal(status: &str) -> Option<String> {
    match status {
        "success" | "failure" | "cancelled" | "skipped" => Some(status.into()),
        _ => None,
    }
}

/// A variable's value, when it is set and not blank.
fn set_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// The password git's credential helpers hold for a forge origin. The helper
/// is asked with prompting off, so a missing credential fails fast instead of
/// hanging an agent shell; the value is returned to the caller and appears in
/// no message. `env` lets a test isolate git's configuration.
fn git_credential_token(root: &str, env: &[(&str, &str)]) -> Result<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let url = url::Url::parse(root)?;
    let host = url.host_str().context("Forgejo origin needs a host")?;
    let authority = url
        .port()
        .map_or_else(|| host.to_owned(), |port| format!("{host}:{port}"));
    let missing = || {
        format!(
            "no Forgejo credential for {authority}: set FORGEJO_TOKEN, or sign in so git holds one for this host"
        )
    };
    let mut child = Command::new("git")
        .args(["credential", "fill"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context(missing())?;
    child
        .stdin
        .take()
        .context("git credential stdin")?
        .write_all(format!("protocol={}\nhost={authority}\n\n", url.scheme()).as_bytes())?;
    let output = child.wait_with_output()?;
    ensure!(output.status.success(), "{}", missing());
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("password="))
        .filter(|password| !password.is_empty())
        .map(str::to_owned)
        .with_context(missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn release_remote_uses_the_configured_forgejo_root() -> Result<()> {
        let root = "https://forge.example.test/git/";
        assert_eq!(
            ForgejoForge::remote_repository(
                root,
                "https://forge.example.test/git/firm/matter.git"
            )?,
            "firm/matter"
        );
        assert_eq!(
            ForgejoForge::remote_repository(root, "git@forge.example.test:firm/matter.git")?,
            "firm/matter"
        );
        assert!(ForgejoForge::remote_repository(
            root,
            "https://forge.example.test/other/firm/matter"
        )
        .is_err());
        Ok(())
    }
    #[tokio::test]
    async fn deployment_selects_exact_push_commit_and_its_jobs() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/example/sample-litigation/actions/runs"))
            .and(query_param("head_sha", "merged"))
            .and(query_param("event", "push"))
            .and(query_param("workflow_id", "cd.yml"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"workflow_runs": [
                {"id": 4, "index_in_repo": 3, "commit_sha": "merged", "workflow_id": "cd.yml",
                 "event": "push", "html_url": "https://forge.example/actions/runs/3", "status": "failure"},
                {"id": 5, "index_in_repo": 4, "commit_sha": "different", "workflow_id": "cd.yml",
                 "event": "push", "html_url": "wrong", "status": "success"},
                {"id": 6, "index_in_repo": 5, "commit_sha": "merged", "workflow_id": "gate.yml",
                 "event": "push", "html_url": "wrong workflow", "status": "success"}
            ]}))).expect(1).mount(&server).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/example/sample-litigation/actions/tasks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"workflow_runs": [
                {"name": "seeds", "status": "failure", "head_sha": "merged", "workflow_id": "cd.yml", "run_number": 3},
                {"name": "gate", "status": "success", "head_sha": "merged", "workflow_id": "cd.yml", "run_number": 2}
            ]}))).expect(1).mount(&server).await;
        let forge =
            ForgejoForge::new(&server.uri(), "example/sample-litigation", "t".into()).unwrap();
        let run = forge.deployment("merged").await.unwrap().unwrap();
        assert_eq!(run.conclusion.as_deref(), Some("failure"));
        assert_eq!(run.jobs.len(), 1);
        assert_eq!(run.jobs[0].name, "seeds");
    }

    #[tokio::test]
    async fn branch_reuse_ignores_a_fork_with_the_same_branch_name() {
        let server = MockServer::start().await;
        let pull = |number, repository| {
            json!({
                "number": number,
                "head": {"sha": "head", "ref": "change", "repo": {"full_name": repository}},
                "base": {"sha": "base", "ref": "main"},
                "html_url": format!("https://forge.example/pulls/{number}"),
                "draft": false, "merged": false, "merge_commit_sha": null, "mergeable": true
            })
        };
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/example/sample-litigation/pulls"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                pull(7, "another/sample-litigation"),
                pull(8, "example/sample-litigation")
            ])))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/example/sample-litigation/pulls"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let forge =
            ForgejoForge::new(&server.uri(), "example/sample-litigation", "t".into()).unwrap();
        let review = forge
            .open_review(ReviewRequest {
                head_branch: "change".into(),
                base_branch: "main".into(),
                title: "A change".into(),
                body: String::new(),
            })
            .await
            .unwrap();
        assert_eq!(review.number, 8);
    }

    #[tokio::test]
    async fn merged_review_resumption_matches_the_exact_topic_and_head() {
        let server = MockServer::start().await;
        let pull = |number, repository, head, topic, base| {
            json!({
                "number": number,
                "head": {
                    "sha": head,
                    "ref": format!("refs/pull/{number}/head"),
                    "label": topic,
                    "repo": {"full_name": repository}
                },
                "base": {"sha": "base", "ref": base},
                "html_url": format!("https://forge.example/pulls/{number}"),
                "draft": false, "merged": true, "merge_commit_sha": "merged", "mergeable": false
            })
        };
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/example/sample-litigation/pulls"))
            .and(query_param("state", "closed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                pull(
                    7,
                    "another/sample-litigation",
                    "head",
                    "push/change",
                    "release"
                ),
                pull(
                    8,
                    "example/sample-litigation",
                    "older-head",
                    "push/change",
                    "release"
                ),
                pull(
                    9,
                    "example/sample-litigation",
                    "head",
                    "push/other",
                    "release"
                ),
                pull(
                    10,
                    "example/sample-litigation",
                    "head",
                    "push/change",
                    "main"
                ),
                pull(
                    11,
                    "example/sample-litigation",
                    "head",
                    "push/change",
                    "release"
                ),
            ])))
            .expect(1)
            .mount(&server)
            .await;
        let forge =
            ForgejoForge::new(&server.uri(), "example/sample-litigation", "t".into()).unwrap();
        let review = forge
            .find_merged_review("push/change", "release", "head")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(review.number, 11);
    }

    #[tokio::test]
    async fn a_missing_required_status_stays_pending() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/example/sample-litigation/pulls/7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "number": 7, "head": {"sha": "head", "ref": "change"},
                "base": {"sha": "base", "ref": "main"}, "html_url": "https://forge.example/pulls/7",
                "draft": false, "merged": false, "merge_commit_sha": null, "mergeable": true
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v1/repos/example/sample-litigation/commits/head/status",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"statuses": []})))
            .mount(&server)
            .await;
        let forge =
            ForgejoForge::new(&server.uri(), "example/sample-litigation", "t".into()).unwrap();
        let review = ReviewRef {
            number: 7,
            head_sha: "head".into(),
            url: "https://forge.example/pulls/7".into(),
        };
        assert_eq!(
            forge.check_verdict(&review).await.unwrap(),
            CheckVerdict::Pending
        );
    }

    const REPOSITORY: &str = "example/sample-litigation";

    fn review() -> ReviewRef {
        ReviewRef {
            number: 7,
            head_sha: "head".into(),
            url: "https://forge.example/pulls/7".into(),
        }
    }

    /// Pull 7 from this repository's `change` branch, with `fields` laid over it.
    fn pull_with(fields: &serde_json::Value) -> serde_json::Value {
        let mut pull = json!({
            "number": 7,
            "head": {"sha": "head", "ref": "change", "repo": {"full_name": REPOSITORY}},
            "base": {"sha": "base", "ref": "main"},
            "html_url": "https://forge.example/pulls/7",
            "draft": false, "merged": false, "merge_commit_sha": null, "mergeable": true
        });
        for (key, value) in fields.as_object().unwrap() {
            pull[key] = value.clone();
        }
        pull
    }

    async fn answer(server: &MockServer, verb: &str, suffix: &str, body: serde_json::Value) {
        Mock::given(method(verb))
            .and(path(format!("/api/v1/{suffix}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn the_required_status_and_mergeability_decide_the_verdict() {
        for (pull, state, expected) in [
            (
                json!({}),
                "failure",
                CheckVerdict::Failed(REQUIRED_CHECK_CONTEXT.into()),
            ),
            (json!({}), "success", CheckVerdict::Green),
            (
                json!({"mergeable": false}),
                "success",
                CheckVerdict::Conflict,
            ),
            (
                json!({"mergeable": false, "merged": true}),
                "success",
                CheckVerdict::Green,
            ),
            (json!({}), "pending", CheckVerdict::Pending),
        ] {
            let server = MockServer::start().await;
            let pulls = format!("repos/{REPOSITORY}/pulls/7");
            answer(&server, "GET", &pulls, pull_with(&pull)).await;
            let statuses = json!({"statuses": [
                {"context": "another", "state": "failure"},
                {"context": REQUIRED_CHECK_CONTEXT, "state": state},
            ]});
            let status = format!("repos/{REPOSITORY}/commits/head/status");
            answer(&server, "GET", &status, statuses).await;
            let forge = ForgejoForge::new(&server.uri(), REPOSITORY, "t".into()).unwrap();
            assert_eq!(forge.check_verdict(&review()).await.unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn a_merge_is_reported_only_from_the_reviewed_head() {
        for (pull, expected) in [
            (json!({}), Ok(None)),
            (
                json!({"merged": true, "merge_commit_sha": "merged"}),
                Ok(Some("merged".to_string())),
            ),
            (json!({"merged": true, "merge_commit_sha": ""}), Ok(None)),
            (
                json!({"head": {"sha": "newer", "ref": "change"}}),
                Err("review head changed; re-run the verb before merging".to_string()),
            ),
        ] {
            let server = MockServer::start().await;
            let pulls = format!("repos/{REPOSITORY}/pulls/7");
            answer(&server, "GET", &pulls, pull_with(&pull)).await;
            let forge = ForgejoForge::new(&server.uri(), REPOSITORY, "t".into()).unwrap();
            let merged = forge.merge_commit(&review()).await;
            assert_eq!(merged.map_err(|error| error.to_string()), expected);
            if let Ok(Some(merge_commit)) = expected {
                let armed = forge.arm_merge(&review()).await.unwrap();
                assert_eq!(armed, ArmOutcome::MergedNow { merge_commit });
            }
        }
    }

    #[tokio::test]
    async fn a_new_review_is_opened_for_this_repository_and_never_as_a_draft() {
        let request = || ReviewRequest {
            head_branch: "change".into(),
            base_branch: "main".into(),
            title: "A change".into(),
            body: "Why".into(),
        };
        for (existing, created, expected) in [
            (json!([]), json!({}), Ok(7)),
            (
                json!([]),
                json!({"head": {"sha": "head", "ref": "change", "repo": {"full_name": "fork/x"}}}),
                Err("created review request belongs to another source repository"),
            ),
            (
                json!([pull_with(&json!({"draft": true}))]),
                json!({}),
                Err("the existing review request is a draft; mark it ready before continuing"),
            ),
            (
                json!([pull_with(
                    &json!({"base": {"sha": "base", "ref": "release"}})
                )]),
                json!({}),
                Err("existing review request targets another base"),
            ),
        ] {
            let server = MockServer::start().await;
            let pulls = format!("repos/{REPOSITORY}/pulls");
            answer(&server, "GET", &pulls, existing).await;
            Mock::given(method("POST"))
                .and(path(format!("/api/v1/{pulls}")))
                .and(wiremock::matchers::body_json(json!({
                    "title": "A change", "head": "change", "base": "main", "body": "Why"
                })))
                .respond_with(ResponseTemplate::new(201).set_body_json(pull_with(&created)))
                .mount(&server)
                .await;
            let forge = ForgejoForge::new(&server.uri(), REPOSITORY, "t".into()).unwrap();
            let opened = forge.open_review(request()).await;
            assert_eq!(
                opened
                    .map(|review| review.number)
                    .map_err(|error| error.to_string()),
                expected.map_err(str::to_owned)
            );
        }
    }

    #[tokio::test]
    async fn scoped_preflight_refuses_wrong_repository_or_read_only_access() {
        for (full_name, push) in [(REPOSITORY, false), ("other/repository", true)] {
            let server = MockServer::start().await;
            let forge = ForgejoForge::new(&server.uri(), REPOSITORY, "t".into()).unwrap();
            answer(
                &server,
                "GET",
                &format!("repos/{REPOSITORY}/"),
                json!({
                    "full_name": full_name, "permissions": {"push": push}
                }),
            )
            .await;
            assert!(forge.identity().await.is_err());
        }
    }

    #[tokio::test]
    async fn repository_scoped_access_needs_no_user_lookup_and_still_checks_actions() {
        let server = MockServer::start().await;
        let forge = ForgejoForge::new(&server.uri(), REPOSITORY, "t".into()).unwrap();
        let error = forge.identity().await.unwrap_err();
        assert_eq!(error.to_string(), "Forgejo request refused");
        assert!(forge.remote_reachable().await.is_err());
        assert!(forge
            .find_merged_review("change", "main", "head")
            .await
            .is_err());

        let repository = format!("repos/{REPOSITORY}/");
        answer(
            &server,
            "GET",
            &repository,
            json!({
                "full_name": REPOSITORY, "permissions": {"push": true}
            }),
        )
        .await;
        assert_eq!(
            forge.identity().await.unwrap(),
            format!("repository credential for {REPOSITORY}")
        );
        forge.remote_reachable().await.unwrap();
        let primary = format!("repos/{REPOSITORY}");
        answer(&server, "GET", &primary, json!({"has_actions": false})).await;
        let error = forge.verify_workspace().await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "Forgejo Actions are disabled for the Project's primary repository"
        );
        let runs = format!("repos/{REPOSITORY}/actions/runs");
        answer(&server, "GET", &runs, json!({"workflow_runs": []})).await;
        assert!(forge.verification(&review()).await.unwrap().is_none());
        assert!(ForgejoForge::new(&server.uri(), "not a repository", "t".into()).is_err());
    }

    #[test]
    fn a_checkout_binds_only_to_the_configured_forgejo_origin() {
        use crate::projects::test_login::checkout;
        std::env::set_var("NAVIGATOR_FORGEJO_ROOT_URL", "https://forge.example.test");
        std::env::set_var("FORGEJO_TOKEN", "forge-token");
        for (origin, expected) in [
            (
                "git@forge.example.test:example/sample-litigation.git",
                Ok(()),
            ),
            (
                "https://forge.example.test/example/sample-litigation",
                Ok(()),
            ),
            (
                "ssh://git@forge.example.test/example/sample-litigation.git",
                Ok(()),
            ),
            (
                "https://other.example.test/example/sample-litigation",
                Err("checkout remote differs from configured Forgejo host"),
            ),
            (
                "http://forge.example.test/example/sample-litigation",
                Err("checkout remote differs from configured Forgejo origin"),
            ),
        ] {
            let dir = checkout("project: sample-litigation\n", Some(origin));
            let bound = ForgejoForge::from_checkout(dir.path());
            assert_eq!(
                bound
                    .map(|forge| assert_eq!(forge.repository, REPOSITORY))
                    .map_err(|error| error.to_string()),
                expected.map_err(str::to_owned),
                "{origin}"
            );
        }
    }

    #[test]
    fn the_forge_root_comes_from_the_remote_when_none_is_configured() {
        for (remote, root) in [
            (
                "git@forge.example.test:a/b.git",
                "https://forge.example.test",
            ),
            (
                "ssh://git@forge.example.test:2222/a/b",
                "https://forge.example.test",
            ),
            (
                "https://forge.example.test/a/b",
                "https://forge.example.test",
            ),
            (
                "https://git@forge.example.test:3000/a/b",
                "https://forge.example.test:3000",
            ),
            ("http://127.0.0.1:8080/a/b", "http://127.0.0.1:8080"),
        ] {
            assert_eq!(ForgejoForge::remote_root(remote).unwrap(), root, "{remote}");
        }
        for remote in [
            "file:///srv/a/b",
            "/srv/a/b",
            "ftp://forge.example.test/a/b",
        ] {
            assert!(ForgejoForge::remote_root(remote).is_err(), "{remote}");
        }
    }

    /// `git clone https://user@host/...` is the same origin as the plain URL;
    /// only an embedded password is refused.
    #[test]
    fn a_remote_that_names_a_user_is_the_same_origin() {
        let root = "https://forge.example.test";
        for remote in [
            "https://git@forge.example.test/a/b.git",
            "https://nick@forge.example.test/a/b",
        ] {
            assert_eq!(
                ForgejoForge::remote_repository(root, remote).unwrap(),
                "a/b"
            );
        }
        let error =
            ForgejoForge::remote_repository(root, "https://nick:secret@forge.example.test/a/b")
                .unwrap_err();
        assert!(error.to_string().contains("differs from configured"));
        assert!(!error.to_string().contains("secret"));
    }

    /// With nothing in the environment, the checkout binds from its remote and
    /// the credential git already holds; `FORGEJO_TOKEN` still wins when set.
    #[test]
    fn the_token_comes_from_the_environment_first_then_from_git() {
        let remote = "https://git@forge.example.test/example/sample-litigation.git";
        let asked = std::cell::RefCell::new(Vec::new());
        let git = |root: &str| {
            asked.borrow_mut().push(root.to_owned());
            Ok("git-token".to_owned())
        };
        std::env::remove_var("NAVIGATOR_FORGEJO_ROOT_URL");
        std::env::remove_var("FORGEJO_TOKEN");
        let forge = ForgejoForge::from_remote_with(remote, git).unwrap();
        assert_eq!(forge.token, "git-token");
        assert_eq!(forge.root, "https://forge.example.test");
        assert_eq!(forge.repository, REPOSITORY);
        assert_eq!(asked.borrow().as_slice(), ["https://forge.example.test"]);

        std::env::set_var("FORGEJO_TOKEN", "env-token");
        let forge = ForgejoForge::from_remote_with(remote, git).unwrap();
        assert_eq!(forge.token, "env-token");
        assert_eq!(
            asked.borrow().len(),
            1,
            "git is not asked when the env is set"
        );

        std::env::remove_var("FORGEJO_TOKEN");
        let error = ForgejoForge::from_remote_with(remote, |_| bail!("no credential"))
            .err()
            .unwrap();
        assert!(error.to_string().contains("no credential"));
    }

    /// The real lookup, against an isolated git configuration whose helper
    /// holds one credential: no keychain, no prompt, nothing printed.
    #[test]
    fn git_credential_fill_reads_the_helper_for_the_forge_host_only() {
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join("gitconfig");
        std::fs::write(
            &config,
            "[credential \"https://forge.example.test\"]\n\
             \thelper = \"!f() { echo username=oauth; echo password=oauth-token; }; f\"\n",
        )
        .unwrap();
        let env = [
            ("GIT_CONFIG_GLOBAL", config.to_str().unwrap()),
            ("GIT_CONFIG_NOSYSTEM", "1"),
        ];
        assert_eq!(
            git_credential_token("https://forge.example.test", &env).unwrap(),
            "oauth-token"
        );
        let error = git_credential_token("https://other.example.test", &env).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("other.example.test"), "{message}");
        assert!(message.contains("FORGEJO_TOKEN"), "{message}");
        assert!(!message.contains("oauth-token"), "{message}");
    }

    #[tokio::test]
    async fn a_refused_credential_names_its_source_and_the_recovery() {
        let server = MockServer::start().await;
        let forge = ForgejoForge::new(&server.uri(), REPOSITORY, "t".into())
            .unwrap()
            .with_credential("the git credential for forge.example.test".into());
        for status in [401, 403] {
            wiremock::Mock::given(wiremock::matchers::any())
                .respond_with(wiremock::ResponseTemplate::new(status))
                .mount(&server)
                .await;
            let message = forge.remote_reachable().await.unwrap_err().to_string();
            assert!(
                message.contains("the git credential for forge.example.test"),
                "{message}"
            );
            assert!(message.contains("sign in"), "{message}");
            server.reset().await;
        }
    }
}
