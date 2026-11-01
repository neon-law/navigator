//! Forgejo's governed review and merge loop for a Project checkout.

use anyhow::{bail, ensure, Context, Result};
use async_trait::async_trait;
use cloud::forgejo_governance::{Forgejo, MergeRequestOutcome, REQUIRED_CHECK_CONTEXT};
use reqwest::Method;
use serde::Deserialize;
use serde_json::json;
use std::path::Path;
use std::time::Duration;

use super::forge::{
    ArmOutcome, CheckVerdict, DeploymentJob, DeploymentRun, Forge, ReviewRef, ReviewRequest,
};
use super::lifecycle::git_text;

pub(crate) struct ForgejoForge {
    http: reqwest::Client,
    root: String,
    repository: String,
    token: String,
    governance: Forgejo,
}

impl ForgejoForge {
    pub(crate) fn new(root: &str, repository: &str, token: String) -> Result<Self> {
        cloud::forgejo_governance::repository_target(repository)?;
        let governance = Forgejo::new(root, token.clone())?;
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            root: root.trim_end_matches('/').into(),
            repository: repository.into(),
            token,
            governance,
        })
    }

    pub(crate) fn from_checkout(dir: &Path) -> Result<Self> {
        let remote = git_text(dir, &["remote", "get-url", "origin"])?;
        let root = std::env::var("NAVIGATOR_FORGEJO_ROOT_URL")
            .context("configure NAVIGATOR_FORGEJO_ROOT_URL for this Forgejo checkout")?;
        let origin = url::Url::parse(&root)?;
        let host = origin.host_str().context("Forgejo origin needs a host")?;
        let repository = if let Some(path) = remote.strip_prefix(&format!("git@{host}:")) {
            path.trim_end_matches(".git").to_owned()
        } else {
            let url = url::Url::parse(&remote).context("parse Forgejo origin remote")?;
            ensure!(
                url.host_str() == Some(host),
                "checkout remote differs from configured Forgejo host"
            );
            if url.scheme() != "ssh" {
                ensure!(
                    url.origin() == origin.origin(),
                    "checkout remote differs from configured Forgejo origin"
                );
            }
            url.path()
                .trim_start_matches('/')
                .trim_end_matches(".git")
                .to_owned()
        };
        Self::new(
            &root,
            &repository,
            std::env::var("FORGEJO_TOKEN")
                .context("FORGEJO_TOKEN must be configured on this trusted host")?,
        )
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
        request
            .send()
            .await?
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
    async fn workflow_run(&self, merge_commit: &str, event: &str) -> Result<Option<DeploymentRun>> {
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
        // Forgejo's one gate workflow runs on both PRs and pushes to main.
        let runs: Runs = self
            .get(&format!(
                "actions/runs?head_sha={merge_commit}&event={event}&workflow_id=gate.yml&limit=50"
            ))
            .await?;
        let Some(run) = runs
            .workflow_runs
            .into_iter()
            .filter(|run| {
                run.commit_sha == merge_commit
                    && run.event == event
                    && run.workflow_id == "gate.yml"
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
    fn review(self) -> Result<ReviewRef> {
        ensure!(
            !self.draft,
            "the existing review request is a draft; mark it ready before continuing"
        );
        Ok(ReviewRef {
            number: self.number,
            head_sha: self.head.sha,
            url: self.html_url,
        })
    }
}

#[async_trait]
impl Forge for ForgejoForge {
    async fn identity(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct User {
            login: String,
        }
        let user: User = self.request(Method::GET, "user", None).await?;
        Ok(user.login)
    }

    async fn remote_reachable(&self) -> Result<()> {
        self.get::<serde_json::Value>("").await?;
        Ok(())
    }

    async fn open_review(&self, request: ReviewRequest) -> Result<ReviewRef> {
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
                    return pull.review();
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
                Some(&json!({"title": request.title, "head": request.head_branch,
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
        pull.review()
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
        self.workflow_run(merge_commit, "push").await
    }

    async fn verification(&self, review: &ReviewRef) -> Result<Option<DeploymentRun>> {
        self.workflow_run(&review.head_sha, "pull_request").await
    }
}

fn terminal(status: &str) -> Option<String> {
    match status {
        "success" | "failure" | "cancelled" | "skipped" => Some(status.into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn deployment_selects_exact_push_commit_and_its_jobs() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/example/sample-litigation/actions/runs"))
            .and(query_param("head_sha", "merged"))
            .and(query_param("event", "push"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"workflow_runs": [
                {"id": 4, "index_in_repo": 3, "commit_sha": "merged", "workflow_id": "gate.yml",
                 "event": "push", "html_url": "https://forge.example/actions/runs/3", "status": "failure"},
                {"id": 5, "index_in_repo": 4, "commit_sha": "different", "workflow_id": "gate.yml",
                 "event": "push", "html_url": "wrong", "status": "success"}
            ]}))).expect(1).mount(&server).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/example/sample-litigation/actions/tasks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"workflow_runs": [
                {"name": "seeds", "status": "failure", "head_sha": "merged", "workflow_id": "gate.yml", "run_number": 3},
                {"name": "gate", "status": "success", "head_sha": "merged", "workflow_id": "gate.yml", "run_number": 2}
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
}
