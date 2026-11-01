//! The GitHub backend for the landing lane's forge seam.
//!
//! Everything GitHub-specific about the lane lives behind this file: the
//! check reported as `<caller job> / <called job>` is carried through as the
//! forge names it, and auto-merge is both armed and read back, because
//! GitHub only queues a merge that is still waiting on something — arming a
//! request that merges now fails with "clean status", and the direct,
//! sha-pinned merge that covers that case is the same escape
//! `devx::github_auto_merge` uses for the operator verb.

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::Deserialize;
use serde_json::json;

use super::forge::{
    ArmOutcome, CheckVerdict, DeploymentJob, DeploymentRun, Forge, ReviewRef, ReviewRequest,
};
use super::lifecycle::{git_text, github_owner_name, github_token};

const API_VERSION: &str = "2022-11-28";

/// Conclusions that end a check run badly. Everything else with a conclusion
/// (`success`, `skipped`, `neutral`, `stale`) lets the request go on merging.
const FAILURE_CONCLUSIONS: &[&str] = &[
    "failure",
    "cancelled",
    "timed_out",
    "action_required",
    "startup_failure",
];

pub(crate) struct GitHubForge {
    http: reqwest::Client,
    api_base: String,
    owner: String,
    name: String,
}

impl GitHubForge {
    /// A client against an explicit API base.
    pub(crate) fn new(api_base: &str, owner: &str, name: &str, token: &str) -> Result<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            ACCEPT,
            "application/vnd.github+json"
                .parse()
                .expect("invariant: a literal header value parses"),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("x-github-api-version"),
            API_VERSION
                .parse()
                .expect("invariant: a literal version parses"),
        );
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {token}")
                .parse()
                .context("encode GitHub authorization header")?,
        );
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .user_agent(USER_AGENT)
            .build()
            .context("build GitHub HTTP client")?;
        Ok(Self {
            http,
            api_base: api_base.trim_end_matches('/').to_string(),
            owner: owner.to_string(),
            name: name.to_string(),
        })
    }

    /// A client for a Project checkout: the owner and repository come from
    /// the checkout's `origin` remote, the token from the environment or
    /// `gh auth token` — the same resolution every other GitHub-speaking
    /// verb uses.
    pub(crate) fn from_checkout(dir: &std::path::Path) -> Result<Self> {
        let remote = git_text(dir, &["remote", "get-url", "origin"])
            .context("read the checkout's origin remote; run from a Project repository")?;
        let (owner, name) = github_owner_name(&remote)?;
        let token = github_token()?;
        Self::new("https://api.github.com", &owner, &name, &token)
    }

    fn repo_path(&self, suffix: &str) -> String {
        format!("/repos/{}/{}/{}", self.owner, self.name, suffix)
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.api_base, path);
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .with_context(|| format!("read the response body of GET {path}"))?;
        if !status.is_success() {
            return Err(anyhow!("GET {path} returned {status}: {}", snippet(&body)));
        }
        serde_json::from_str(&body).with_context(|| format!("parse the response of GET {path}"))
    }

    /// Write a JSON body and answer with the status and body text, for the
    /// callers that must branch on GitHub's own error wording.
    async fn write_json(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<(u16, String)> {
        let url = format!("{}{}", self.api_base, path);
        let response = self
            .http
            .request(method.clone(), &url)
            .json(body)
            .send()
            .await
            .with_context(|| format!("{method} {path}"))?;
        let status = response.status().as_u16();
        let text = response
            .text()
            .await
            .with_context(|| format!("read the response body of {method} {path}"))?;
        Ok((status, text))
    }
    async fn workflow_run(
        &self,
        merge_commit: &str,
        workflow: &str,
        event: &str,
    ) -> Result<Option<DeploymentRun>> {
        #[derive(Deserialize)]
        struct Runs {
            workflow_runs: Vec<Run>,
        }
        #[derive(Deserialize)]
        struct Run {
            id: u64,
            html_url: String,
            head_sha: String,
            event: String,
            status: String,
            conclusion: Option<String>,
        }
        #[derive(Deserialize)]
        struct Jobs {
            jobs: Vec<Job>,
        }
        #[derive(Deserialize)]
        struct Job {
            name: String,
            conclusion: Option<String>,
        }
        let runs: Runs = self
            .get_json(&self.repo_path(&format!(
                "actions/workflows/{workflow}/runs?head_sha={merge_commit}&event={event}&per_page=100"
            )))
            .await?;
        let Some(run) = runs
            .workflow_runs
            .into_iter()
            .filter(|run| run.head_sha == merge_commit && run.event == event)
            .max_by_key(|run| run.id)
        else {
            return Ok(None);
        };
        let mut jobs = Vec::new();
        let mut page = 1;
        loop {
            let batch: Jobs = self
                .get_json(&self.repo_path(&format!(
                    "actions/runs/{}/jobs?filter=latest&per_page=100&page={page}",
                    run.id
                )))
                .await?;
            let count = batch.jobs.len();
            jobs.extend(batch.jobs.into_iter().map(|job| DeploymentJob {
                name: job.name,
                conclusion: job.conclusion,
            }));
            if count < 100 {
                break;
            }
            page += 1;
        }
        Ok(Some(DeploymentRun {
            url: run.html_url,
            commit: run.head_sha,
            conclusion: if run.status == "completed" {
                run.conclusion
            } else {
                None
            },
            jobs,
        }))
    }
}

/// The first line of a GitHub error body, for error messages an agent reads.
fn snippet(body: &str) -> String {
    let line = body.lines().next().unwrap_or_default();
    line.chars().take(300).collect()
}

/// One pull request as this backend reads it.
#[derive(Debug, Deserialize)]
struct Pull {
    number: u64,
    head: PullHead,
    base: PullBase,
    html_url: String,
    #[serde(default)]
    merged: bool,
    #[serde(default)]
    merge_commit_sha: Option<String>,
    #[serde(default)]
    mergeable_state: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PullHead {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct PullBase {
    #[serde(rename = "ref")]
    git_ref: String,
}

#[derive(Debug, Deserialize)]
struct User {
    login: String,
}

#[derive(Debug, Deserialize)]
struct AutoMerge {
    enabled: bool,
}

#[derive(Debug, Deserialize)]
struct CheckRun {
    name: String,
    #[serde(default)]
    conclusion: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CheckRuns {
    #[serde(default)]
    check_runs: Vec<CheckRun>,
}

#[derive(Debug, Deserialize)]
struct MergeResult {
    #[serde(default)]
    sha: Option<String>,
}

/// The required check's verdict from GitHub's own words. A check run without
/// a conclusion is still running, whatever its status field claims.
fn verdict(mergeable_state: &str, runs: &[CheckRun]) -> CheckVerdict {
    for run in runs {
        if run
            .conclusion
            .as_deref()
            .is_some_and(|c| FAILURE_CONCLUSIONS.contains(&c))
        {
            return CheckVerdict::Failed(run.name.clone());
        }
        if run.conclusion.is_none() {
            return CheckVerdict::Pending;
        }
    }
    match mergeable_state {
        "behind" => CheckVerdict::BranchBehind,
        "blocked" => CheckVerdict::HeldReview,
        "dirty" => CheckVerdict::Conflict,
        _ => CheckVerdict::Green,
    }
}

#[async_trait]
impl Forge for GitHubForge {
    async fn identity(&self) -> Result<String> {
        let user: User = self.get_json("/user").await?;
        Ok(user.login)
    }

    async fn remote_reachable(&self) -> Result<()> {
        self.get_json::<serde_json::Value>(&self.repo_path(""))
            .await
            .with_context(|| format!("reach {}/{} on GitHub", self.owner, self.name))?;
        Ok(())
    }

    async fn open_review(&self, request: ReviewRequest) -> Result<ReviewRef> {
        let existing: Vec<Pull> = self
            .get_json(&self.repo_path(&format!(
                "pulls?head={}:{}&state=open",
                self.owner, request.head_branch
            )))
            .await?;
        if let Some(pull) = existing
            .iter()
            .find(|pull| pull.base.git_ref == request.base_branch)
        {
            return Ok(ReviewRef {
                number: pull.number,
                head_sha: pull.head.sha.clone(),
                url: pull.html_url.clone(),
            });
        }
        if let Some(pull) = existing.first() {
            bail!(
                "an open review request for {} targets {} instead of {}; close it or pass --base {}",
                request.head_branch,
                pull.base.git_ref,
                request.base_branch,
                pull.base.git_ref
            );
        }
        let created: Pull = self
            .write_json(
                reqwest::Method::POST,
                &self.repo_path("pulls"),
                &json!({
                    "title": request.title,
                    "head": request.head_branch,
                    "base": request.base_branch,
                    "body": request.body,
                    "draft": false,
                }),
            )
            .await
            .and_then(|(status, body)| {
                if (200..300).contains(&status) {
                    Ok(body)
                } else {
                    Err(anyhow!(
                        "opening the review request failed: {status}: {body}"
                    ))
                }
            })
            .and_then(|body| serde_json::from_str(&body).context("decode the review request"))
            .context("open the review request")?;
        Ok(ReviewRef {
            number: created.number,
            head_sha: created.head.sha,
            url: created.html_url,
        })
    }

    async fn arm_merge(&self, review: &ReviewRef) -> Result<ArmOutcome> {
        let arm_path = self.repo_path(&format!("pulls/{}/auto-merge", review.number));
        let (status, body) = self
            .write_json(
                reqwest::Method::PUT,
                &arm_path,
                &json!({ "merge_method": "squash" }),
            )
            .await?;
        if status == 200 || status == 202 {
            // Read the arm back: the write succeeding is not the arm
            // queueing, and only the read back tells the two apart.
            let armed: AutoMerge = self.get_json(&arm_path).await?;
            if armed.enabled {
                return Ok(ArmOutcome::Armed);
            }
            bail!(
                "merge-when-green did not stay armed for review request #{}",
                review.number
            );
        }
        if body.contains("clean status") || body.contains("unstable status") {
            // The request merges now, so GitHub refused to queue it. Merge
            // directly, pinned to the head commit the check ran against.
            let (merge_status, merge_body) = self
                .write_json(
                    reqwest::Method::PUT,
                    &self.repo_path(&format!("pulls/{}/merge", review.number)),
                    &json!({ "sha": review.head_sha, "merge_method": "squash" }),
                )
                .await?;
            if !(200..300).contains(&merge_status) {
                bail!(
                    "the direct merge of review request #{} failed: {}: {}",
                    review.number,
                    merge_status,
                    snippet(&merge_body)
                );
            }
            let merged: MergeResult =
                serde_json::from_str(&merge_body).context("parse the merge response")?;
            return merged
                .sha
                .map(|sha| ArmOutcome::MergedNow { merge_commit: sha })
                .ok_or_else(|| {
                    anyhow!(
                        "GitHub merged review request #{} without reporting a merge commit",
                        review.number
                    )
                });
        }
        // An arm a previous run left enabled answers the retry here.
        let armed: AutoMerge = self.get_json(&arm_path).await?;
        if armed.enabled {
            return Ok(ArmOutcome::AlreadyArmed);
        }
        bail!(
            "arming merge-when-green for review request #{} failed: {}: {}",
            review.number,
            status,
            snippet(&body)
        )
    }

    async fn check_verdict(&self, review: &ReviewRef) -> Result<CheckVerdict> {
        let pull: Pull = self
            .get_json(&self.repo_path(&format!("pulls/{}", review.number)))
            .await?;
        let state = pull.mergeable_state.as_deref().unwrap_or("unknown");
        let runs: CheckRuns = self
            .get_json(&self.repo_path(&format!("commits/{}/check-runs", review.head_sha)))
            .await?;
        Ok(verdict(state, &runs.check_runs))
    }

    async fn merge_commit(&self, review: &ReviewRef) -> Result<Option<String>> {
        let pull: Pull = self
            .get_json(&self.repo_path(&format!("pulls/{}", review.number)))
            .await?;
        if pull.merged {
            Ok(pull.merge_commit_sha.filter(|sha| !sha.is_empty()))
        } else {
            Ok(None)
        }
    }
    async fn deployment(&self, merge_commit: &str) -> Result<Option<DeploymentRun>> {
        self.workflow_run(merge_commit, "cd.yml", "push").await
    }

    async fn verification(&self, review: &ReviewRef) -> Result<Option<DeploymentRun>> {
        self.workflow_run(&review.head_sha, "ci.yml", "pull_request")
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::{GitHubForge, verdict};
    use crate::projects::forge::{ArmOutcome, CheckVerdict, Forge, ReviewRef, ReviewRequest};
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn review(number: u64, head_sha: &str) -> ReviewRef {
        ReviewRef {
            number,
            head_sha: head_sha.to_string(),
            url: format!("https://github.com/acme/nav/pull/{number}"),
        }
    }

    fn check_run(name: &str, status: &str, conclusion: Option<&str>) -> serde_json::Value {
        json!({
            "name": name,
            "status": status,
            "conclusion": conclusion,
        })
    }

    fn pull_body(
        number: u64,
        head_sha: &str,
        mergeable_state: &str,
        merged: bool,
    ) -> serde_json::Value {
        json!({
            "number": number,
            "head": { "sha": head_sha },
            "base": { "ref": "main" },
            "html_url": format!("https://github.com/acme/nav/pull/{number}"),
            "state": if merged { "closed" } else { "open" },
            "merged": merged,
            "merge_commit_sha": if merged { json!("merge555") } else { serde_json::Value::Null },
            "mergeable_state": mergeable_state,
        })
    }

    #[test]
    fn a_failing_check_run_names_itself_as_the_forge_reports_it() {
        let runs = vec![
            serde_json::from_value::<super::CheckRun>(check_run(
                "ci / test",
                "completed",
                Some("failure"),
            ))
            .unwrap(),
            serde_json::from_value::<super::CheckRun>(check_run(
                "ci / gate",
                "completed",
                Some("success"),
            ))
            .unwrap(),
        ];
        assert_eq!(
            verdict("clean", &runs),
            CheckVerdict::Failed("ci / test".to_string())
        );
    }

    #[test]
    fn a_running_check_run_is_pending() {
        let runs = vec![
            serde_json::from_value::<super::CheckRun>(check_run("ci / test", "in_progress", None))
                .unwrap(),
        ];
        assert_eq!(verdict("blocked", &runs), CheckVerdict::Pending);
    }

    #[test]
    fn green_checks_answer_by_mergeable_state() {
        let runs = vec![
            serde_json::from_value::<super::CheckRun>(check_run(
                "ci / test",
                "completed",
                Some("success"),
            ))
            .unwrap(),
        ];
        assert_eq!(verdict("clean", &runs), CheckVerdict::Green);
        assert_eq!(verdict("behind", &runs), CheckVerdict::BranchBehind);
        assert_eq!(verdict("blocked", &runs), CheckVerdict::HeldReview);
        assert_eq!(verdict("dirty", &runs), CheckVerdict::Conflict);
    }

    #[test]
    fn skipped_and_stale_conclusions_do_not_fail_the_check() {
        let runs = vec![
            serde_json::from_value::<super::CheckRun>(check_run(
                "ci / macos",
                "completed",
                Some("skipped"),
            ))
            .unwrap(),
            serde_json::from_value::<super::CheckRun>(check_run(
                "ci / linux",
                "completed",
                Some("stale"),
            ))
            .unwrap(),
        ];
        assert_eq!(verdict("clean", &runs), CheckVerdict::Green);
    }

    #[tokio::test]
    async fn deployment_reads_only_the_merge_commit_cd_run_and_all_jobs() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/nav/actions/workflows/cd.yml/runs"))
            .and(query_param("head_sha", "merge555"))
            .and(query_param("event", "push"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "workflow_runs": [
                    {"id": 9, "html_url": "wrong", "head_sha": "another", "event": "push",
                     "status": "completed", "conclusion": "success"},
                    {"id": 8, "html_url": "https://github.com/acme/nav/actions/runs/8",
                     "head_sha": "merge555", "event": "push", "status": "completed",
                     "conclusion": "failure"}
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/nav/actions/runs/8/jobs"))
            .and(query_param("filter", "latest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jobs": [{"name": "gate / seeds", "conclusion": "failure"},
                         {"name": "gate / notify", "conclusion": "skipped"}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let forge = GitHubForge::new(&server.uri(), "acme", "nav", "t").unwrap();
        let run = forge.deployment("merge555").await.unwrap().unwrap();
        assert_eq!(run.commit, "merge555");
        assert_eq!(run.conclusion.as_deref(), Some("failure"));
        assert_eq!(run.jobs.len(), 2);
        assert_eq!(run.jobs[1].conclusion.as_deref(), Some("skipped"));
    }

    #[tokio::test]
    async fn open_review_reuses_the_open_request_for_the_head_branch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/nav/pulls"))
            .and(query_param("head", "acme:push-a-change"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "number": 7,
                "head": { "sha": "abc123" },
                "base": { "ref": "main" },
                "html_url": "https://github.com/acme/nav/pull/7",
            }])))
            .expect(1)
            .mount(&server)
            .await;
        let forge = GitHubForge::new(&server.uri(), "acme", "nav", "t").unwrap();
        let opened = forge
            .open_review(ReviewRequest {
                head_branch: "push-a-change".to_string(),
                base_branch: "main".to_string(),
                title: "A change".to_string(),
                body: String::new(),
            })
            .await
            .unwrap();
        assert_eq!(opened.number, 7);
        assert_eq!(opened.head_sha, "abc123");
    }

    #[tokio::test]
    async fn open_review_refuses_a_request_aimed_at_another_base() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/nav/pulls"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "number": 7,
                "head": { "sha": "abc123" },
                "base": { "ref": "release/26.10" },
                "html_url": "https://github.com/acme/nav/pull/7",
            }])))
            .mount(&server)
            .await;
        let forge = GitHubForge::new(&server.uri(), "acme", "nav", "t").unwrap();
        let error = forge
            .open_review(ReviewRequest {
                head_branch: "push-a-change".to_string(),
                base_branch: "main".to_string(),
                title: "A change".to_string(),
                body: String::new(),
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("targets release/26.10"));
    }

    #[tokio::test]
    async fn arm_merge_arms_and_reads_the_arm_back() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/repos/acme/nav/pulls/7/auto-merge"))
            .respond_with(ResponseTemplate::new(202))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/nav/pulls/7/auto-merge"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "enabled": true })))
            .expect(1)
            .mount(&server)
            .await;
        let forge = GitHubForge::new(&server.uri(), "acme", "nav", "t").unwrap();
        assert_eq!(
            forge.arm_merge(&review(7, "abc123")).await.unwrap(),
            ArmOutcome::Armed
        );
    }

    #[tokio::test]
    async fn a_request_that_merges_now_is_merged_directly_and_pinned() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/repos/acme/nav/pulls/7/auto-merge"))
            .respond_with(
                ResponseTemplate::new(422)
                    .set_body_string("{\"message\":\"Pull request is in clean status\"}"),
            )
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/repos/acme/nav/pulls/7/merge"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "sha": "merge555",
                "merged": true,
            })))
            .mount(&server)
            .await;
        let forge = GitHubForge::new(&server.uri(), "acme", "nav", "t").unwrap();
        assert_eq!(
            forge.arm_merge(&review(7, "abc123")).await.unwrap(),
            ArmOutcome::MergedNow {
                merge_commit: "merge555".to_string()
            }
        );
    }

    #[tokio::test]
    async fn merge_commit_answers_the_sha_only_after_the_merge() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/nav/pulls/7"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(pull_body(7, "abc123", "clean", true)),
            )
            .mount(&server)
            .await;
        let forge = GitHubForge::new(&server.uri(), "acme", "nav", "t").unwrap();
        assert_eq!(
            forge.merge_commit(&review(7, "abc123")).await.unwrap(),
            Some("merge555".to_string())
        );
    }
}
