//! A Project repository's governance on Forgejo, and the one guarded way a
//! change reaches its `main`.
//!
//! The governance is squash merge only, `main` protected so every change
//! arrives by pull request, the required status check, signed commits, and
//! no exemption for administrators. [`Forgejo::reconcile`] converges it,
//! [`Forgejo::verify`] proves it by reading it back, and [`Forgejo::schedule`]
//! re-verifies it before asking Forgejo to squash-merge a pull request once
//! its checks pass. The `navigator ops forgejo` commands and the Nightly
//! Project Update both use this one copy.

use anyhow::{bail, ensure, Context, Result};
use reqwest::{Client, Method, StatusCode};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::Duration;

pub mod scheduler;
mod update;

pub use update::{FileUpdate, UpdateProposal};

/// Every Project repository's one protected branch.
const DEFAULT_BRANCH: &str = "main";

/// A Project repository's governance and its guarded updates, addressed by
/// Project code on a forge confined to one Firm's namespace. The Nightly
/// Project Update holds this, never the raw [`Forgejo`] client.
#[async_trait::async_trait]
pub trait RepositoryGovernance: Send + Sync {
    /// Converge the repository onto the governance with
    /// [`REQUIRED_CHECK_CONTEXT`] and return the drifted controls repaired.
    async fn reconcile_governance(&self, project_code: &str) -> Result<Vec<String>>;
    /// [`Forgejo::propose_file_update`] for the Project's repository.
    async fn propose_file_update(
        &self,
        project_code: &str,
        update: &FileUpdate<'_>,
    ) -> Result<UpdateProposal>;
}

/// A forge that both holds Project repositories and governs them.
pub trait GovernedForge: crate::forge::ForgeService + RepositoryGovernance {}
impl<T: crate::forge::ForgeService + RepositoryGovernance + ?Sized> GovernedForge for T {}

/// The commit-status context Forgejo Actions posts for a Project
/// repository's `gate` workflow on a pull request: `navigator project gate`.
pub const REQUIRED_CHECK_CONTEXT: &str = "gate / gate (pull_request)";

/// What Forgejo said to a guarded merge request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeRequestOutcome {
    Accepted,
    ConflictNeedsInspection,
}

/// The context must be observed from this installation's Actions runner.
#[derive(Debug)]
pub struct Policy<'a> {
    context: &'a str,
}

impl<'a> Policy<'a> {
    /// # Errors
    /// When the context is blank or spans lines.
    pub fn new(context: &'a str) -> Result<Self> {
        ensure!(
            !context.trim().is_empty() && !context.contains(['\r', '\n']),
            "an exact, nonempty required Actions check context is necessary"
        );
        Ok(Self { context })
    }

    /// The readable repository merge settings.
    #[must_use]
    pub fn repository() -> Value {
        json!({
            "default_branch": "main",
            "has_pull_requests": true,
            "allow_squash_merge": true,
            "allow_merge_commits": false,
            "allow_rebase": false,
            "allow_rebase_explicit": false,
            "allow_fast_forward_only_merge": false,
            "default_merge_style": "squash",
            "default_delete_branch_after_merge": true,
        })
    }

    /// [`Self::repository`] plus the write-only manual-merge controls.
    #[must_use]
    pub fn repository_write() -> Value {
        let mut repository = Self::repository();
        // Forgejo accepts these controls but omits them from its read API.
        repository["allow_manual_merge"] = false.into();
        repository["autodetect_manual_merge"] = false.into();
        repository
    }

    /// The protection rule for `main`.
    #[must_use]
    pub fn branch(&self) -> Value {
        json!({
            "rule_name": "main",
            "enable_push": false,
            "enable_push_whitelist": false,
            "push_whitelist_deploy_keys": false,
            "enable_merge_whitelist": false,
            "enable_status_check": true,
            "status_check_contexts": [self.context],
            "required_approvals": 0,
            "enable_approvals_whitelist": false,
            "block_on_rejected_reviews": true,
            "block_on_official_review_requests": false,
            "block_on_outdated_branch": false,
            "require_signed_commits": true,
            "apply_to_admins": true,
            "protected_file_patterns": "",
            "unprotected_file_patterns": "",
        })
    }
}

fn differences(wanted: &Value, actual: &Value) -> Vec<String> {
    wanted
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, value)| actual.get(*key) != Some(*value))
        .map(|(key, _)| key.clone())
        .collect()
}

/// `owner/name`, refusing URL syntax and path traversal.
///
/// # Errors
/// When `repository` is not exactly two safe segments.
pub fn repository_target(repository: &str) -> Result<&str> {
    let parts: Vec<_> = repository.split('/').collect();
    ensure!(
        parts.len() == 2
            && parts.iter().all(|part| {
                !part.is_empty()
                    && !matches!(*part, "." | "..")
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            }),
        "repository must be an explicit owner/name without URL syntax"
    );
    Ok(repository)
}

/// Credentials travel only in the authorization header, never in the URL.
pub struct Forgejo {
    http: Client,
    root: String,
    token: String,
}

impl Forgejo {
    /// # Errors
    /// When the origin is not HTTPS (or loopback HTTP), carries a path,
    /// credentials, query, or fragment, or the token is blank.
    pub fn new(root: &str, token: String) -> Result<Self> {
        let url = url::Url::parse(root).context("parse Forgejo origin")?;
        let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        ensure!(
            url.scheme() == "https" || (url.scheme() == "http" && loopback),
            "Forgejo requires HTTPS outside the loopback test tier"
        );
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/",
            "Forgejo origin must not contain credentials, a path, query, or fragment"
        );
        ensure!(!token.trim().is_empty(), "FORGEJO_TOKEN must be configured");
        Ok(Self {
            http: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .build()
                .context("build Forgejo client")?,
            root: root.trim_end_matches('/').to_owned(),
            token,
        })
    }

    pub(crate) async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<reqwest::Response> {
        let mut request = self
            .http
            .request(method, format!("{}/api/v1/{path}", self.root))
            .header("Authorization", format!("token {}", self.token));
        if let Some(body) = body {
            request = request.json(body);
        }
        request.send().await.context("Forgejo request failed")
    }

    async fn read(&self, path: &str) -> Result<Value> {
        self.request(Method::GET, path, None)
            .await?
            .error_for_status()
            .context("Forgejo policy read failed")?
            .json()
            .await
            .context("parse Forgejo response")
    }

    async fn state(&self, repository: &str) -> Result<(Value, Option<Value>)> {
        let repo = self.read(&format!("repos/{repository}")).await?;
        let protections = self
            .read(&format!("repos/{repository}/branch_protections"))
            .await?;
        let protections = protections
            .as_array()
            .context("branch protections must be a list")?;
        ensure!(
            protections.len() <= 1 && protections.iter().all(|rule| rule["rule_name"] == "main"),
            "additional branch protection rules need review before reconciliation or auto-merge"
        );
        let response = self
            .request(
                Method::GET,
                &format!("repos/{repository}/branch_protections/main"),
                None,
            )
            .await?;
        let branch = if response.status() == StatusCode::NOT_FOUND {
            None
        } else {
            Some(
                response
                    .error_for_status()
                    .context("read protected main")?
                    .json()
                    .await
                    .context("parse main protection")?,
            )
        };
        Ok((repo, branch))
    }

    /// Dry runs issue GETs only. Readable controls are verified after apply.
    /// Returns the readable controls that drifted, which an apply repaired.
    ///
    /// # Errors
    /// An invalid target or context, a refused or failed call, an extra
    /// protection rule, or a read-back that still does not match.
    pub async fn reconcile(
        &self,
        repository: &str,
        context: &str,
        dry_run: bool,
    ) -> Result<Vec<String>> {
        let repository = repository_target(repository)?;
        let policy = Policy::new(context)?;
        let (repo, branch) = self.state(repository).await?;
        let repo_drift = differences(&Policy::repository(), &repo);
        let branch_drift = differences(&policy.branch(), branch.as_ref().unwrap_or(&Value::Null));
        let drift: Vec<_> = repo_drift
            .iter()
            .map(|key| format!("repository.{key}"))
            .chain(branch_drift.iter().map(|key| format!("main.{key}")))
            .collect();
        if dry_run {
            return Ok(drift);
        }
        // Protect main before enabling any repository merge settings.
        if !branch_drift.is_empty() {
            let (method, path) = if branch.is_some() {
                (
                    Method::PATCH,
                    format!("repos/{repository}/branch_protections/main"),
                )
            } else {
                (
                    Method::POST,
                    format!("repos/{repository}/branch_protections"),
                )
            };
            self.request(method, &path, Some(&policy.branch()))
                .await?
                .error_for_status()
                .context("apply main protection")?;
        }
        // The API exposes neither manual-merge flag. Every explicit apply
        // writes them off, including when all readable fields already match.
        let repository_patch = if repo_drift.is_empty() {
            json!({"allow_manual_merge": false, "autodetect_manual_merge": false})
        } else {
            Policy::repository_write()
        };
        self.request(
            Method::PATCH,
            &format!("repos/{repository}"),
            Some(&repository_patch),
        )
        .await?
        .error_for_status()
        .context("apply repository merge settings")?;
        self.verify(repository, &policy).await?;
        Ok(drift)
    }

    /// Read the governance back and fail unless every readable control
    /// matches `policy`.
    ///
    /// # Errors
    /// Any drift, or a failed read.
    pub async fn verify(&self, repository: &str, policy: &Policy<'_>) -> Result<()> {
        let (repo, branch) = self.state(repository).await?;
        ensure!(
            differences(&Policy::repository(), &repo).is_empty()
                && differences(&policy.branch(), branch.as_ref().unwrap_or(&Value::Null))
                    .is_empty(),
            "Forgejo repository governance does not match; auto-merge stays off"
        );
        Ok(())
    }

    /// Refuse to schedule a merge if Forgejo reports any unresolved code-review
    /// conversation, or if its review API omits state needed to prove that none
    /// remain.
    async fn verify_review_conversations(&self, repository: &str, number: u64) -> Result<()> {
        const PAGE_SIZE: usize = 50;

        let mut page = 1_usize;
        let mut expected_total = None;
        let mut review_count = 0_usize;
        let mut seen_reviews = HashSet::new();
        let mut seen_comments = HashSet::new();
        loop {
            let path =
                format!("repos/{repository}/pulls/{number}/reviews?page={page}&limit={PAGE_SIZE}");
            let response = self
                .request(Method::GET, &path, None)
                .await?
                .error_for_status()
                .context("read pull-request reviews")?;
            let total = response
                .headers()
                .get("x-total-count")
                .context("Forgejo review list omitted its total count")?
                .to_str()
                .context("Forgejo review count is not valid text")?
                .parse::<usize>()
                .context("Forgejo review count is not a nonnegative integer")?;
            if let Some(previous) = expected_total {
                ensure!(
                    previous == total,
                    "Forgejo review list changed while reading; auto-merge stays off"
                );
            } else {
                expected_total = Some(total);
            }
            let reviews: Vec<Value> = response
                .json()
                .await
                .context("parse Forgejo pull-request reviews")?;
            ensure!(
                !reviews.is_empty() || review_count == total,
                "Forgejo review list ended before its reported total; auto-merge stays off"
            );

            for review in &reviews {
                let review_id = review["id"]
                    .as_u64()
                    .filter(|id| *id > 0)
                    .context("Forgejo review omitted its identifier; auto-merge stays off")?;
                ensure!(
                    seen_reviews.insert(review_id),
                    "Forgejo repeated a review across pages; auto-merge stays off"
                );
                let comment_count = review["code_comments_count"]
                    .as_u64()
                    .and_then(|count| usize::try_from(count).ok())
                    .context(
                        "Forgejo review omitted its code-comment count; auto-merge stays off",
                    )?;
                if comment_count > 0 {
                    self.verify_review_comments(
                        repository,
                        number,
                        review_id,
                        comment_count,
                        &mut seen_comments,
                    )
                    .await?;
                }
            }

            review_count += reviews.len();
            ensure!(
                review_count <= total,
                "Forgejo returned more reviews than its reported total; auto-merge stays off"
            );
            if review_count == total {
                break;
            }
            page = page
                .checked_add(1)
                .context("Forgejo review pagination overflow")?;
        }
        Ok(())
    }

    async fn verify_review_comments(
        &self,
        repository: &str,
        number: u64,
        review_id: u64,
        expected_count: usize,
        seen_comments: &mut HashSet<u64>,
    ) -> Result<()> {
        let comments: Vec<Value> = self
            .read(&format!(
                "repos/{repository}/pulls/{number}/reviews/{review_id}/comments"
            ))
            .await?
            .as_array()
            .context("Forgejo review comments must be a list")?
            .clone();
        ensure!(
            comments.len() == expected_count,
            "Forgejo returned an incomplete review-comment list; auto-merge stays off"
        );
        for comment in &comments {
            let comment_id = comment["id"]
                .as_u64()
                .filter(|id| *id > 0)
                .context("Forgejo review comment omitted its identifier; auto-merge stays off")?;
            ensure!(
                seen_comments.insert(comment_id),
                "Forgejo repeated a review comment; auto-merge stays off"
            );
            match comment.get("resolver") {
                Some(Value::Object(resolver))
                    if resolver
                        .get("id")
                        .and_then(Value::as_u64)
                        .is_some_and(|id| id > 0) => {}
                Some(Value::Object(_)) => {
                    bail!("Forgejo review comment has invalid resolution metadata; auto-merge stays off")
                }
                Some(Value::Null) => {
                    bail!(
                        "pull request has an unresolved review conversation; auto-merge stays off"
                    )
                }
                _ => bail!(
                    "Forgejo review comment omitted its resolution state; auto-merge stays off"
                ),
            }
        }
        Ok(())
    }

    /// Re-read protection immediately before asking Forgejo to wait for checks.
    ///
    /// # Errors
    /// Drift, an unready or unresolved pull request, a conflict, or a refusal.
    pub async fn schedule(&self, repository: &str, context: &str, number: u64) -> Result<()> {
        match self
            .schedule_expected(repository, context, number, None)
            .await?
        {
            MergeRequestOutcome::Accepted => Ok(()),
            MergeRequestOutcome::ConflictNeedsInspection => {
                bail!("Forgejo reports a merge conflict; inspect the pull request before retrying")
            }
        }
    }

    /// [`Self::schedule`], optionally pinned to a head discovered earlier: a
    /// squash merge that waits for the required check, or merges now when it
    /// already passed. A conflict is an outcome, not an error.
    ///
    /// # Errors
    /// As [`Self::schedule`], or a head that moved since discovery.
    pub async fn schedule_expected(
        &self,
        repository: &str,
        context: &str,
        number: u64,
        expected_head: Option<&str>,
    ) -> Result<MergeRequestOutcome> {
        let repository = repository_target(repository)?;
        let policy = Policy::new(context)?;
        let pr = self
            .read(&format!("repos/{repository}/pulls/{number}"))
            .await?;
        ensure!(
            pr["state"] == "open" && pr["merged"] == false && pr["draft"] == false,
            "only an open, ready, unmerged pull request may schedule auto-merge"
        );
        ensure!(
            pr["base"]["ref"] == "main",
            "auto-merge requires main as the base"
        );
        let title = pr["title"].as_str().context("pull request has no title")?;
        let head = pr["head"]["sha"]
            .as_str()
            .context("pull request has no head SHA")?;
        if let Some(expected_head) = expected_head {
            ensure!(
                head == expected_head,
                "pull request head changed after scheduler discovery"
            );
        }
        self.verify_review_conversations(repository, number).await?;
        self.verify(repository, &policy).await?;
        // Forgejo 15.0.9 re-evaluates a scheduled merge only on a later status,
        // review, or base push, so a head whose required check already passed
        // would wait indefinitely. Merge it now instead; Forgejo still checks
        // protection, and `head_commit_id` pins the verified head.
        let green = self
            .required_check_succeeded(repository, head, context)
            .await?;
        let body = json!({
            "Do": "squash", "merge_when_checks_succeed": !green,
            "delete_branch_after_merge": true, "force_merge": false,
            "head_commit_id": head, "MergeTitleField": title,
            "MergeMessageField": pr["body"].as_str().unwrap_or_default(),
        });
        let response = self
            .request(
                Method::POST,
                &format!("repos/{repository}/pulls/{number}/merge"),
                Some(&body),
            )
            .await?;
        if response.status() == StatusCode::CONFLICT {
            return Ok(MergeRequestOutcome::ConflictNeedsInspection);
        }
        if !matches!(response.status(), StatusCode::OK | StatusCode::CREATED) {
            bail!(
                "Forgejo refused to {} the governed squash merge ({})",
                if green { "perform" } else { "schedule" },
                response.status()
            );
        }
        Ok(MergeRequestOutcome::Accepted)
    }

    /// Whether Forgejo's latest status for `context` on `head` is a success.
    async fn required_check_succeeded(
        &self,
        repository: &str,
        head: &str,
        context: &str,
    ) -> Result<bool> {
        let combined = self
            .read(&format!("repos/{repository}/commits/{head}/status"))
            .await?;
        Ok(combined["statuses"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|status| status["context"] == context && status["status"] == "success"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::State,
        http::{StatusCode, Uri},
        response::IntoResponse,
        routing::any,
        Json, Router,
    };
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Fixture {
        repo: Value,
        branch: Option<Value>,
        pr: Value,
        writes: Vec<(Method, String, Value)>,
        accept_writes: bool,
        merge_response: Option<StatusCode>,
        extra_rule: bool,
        manual_controls: Value,
        reviews: Vec<Value>,
        review_comments: HashMap<u64, Vec<Value>>,
        statuses: Value,
    }

    impl Default for Fixture {
        fn default() -> Self {
            Self {
                repo: json!({}),
                branch: None,
                pr: json!({
                    "state": "open", "merged": false, "draft": false,
                    "base": {"ref": "main"}, "head": {"sha": "synthetic-head"},
                    "title": "Publish source", "body": "Client-safe source change",
                }),
                writes: Vec::new(),
                accept_writes: true,
                merge_response: None,
                extra_rule: false,
                manual_controls: json!({"allow_manual_merge": true, "autodetect_manual_merge": true}),
                reviews: Vec::new(),
                review_comments: HashMap::new(),
                statuses: json!([]),
            }
        }
    }

    struct Server {
        client: Forgejo,
        state: Arc<Mutex<Fixture>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn fixture_get(state: &Fixture, uri: &Uri) -> axum::response::Response {
        let path = uri.path();
        if path.ends_with("/commits/synthetic-head/status") {
            return (StatusCode::OK, Json(json!({"statuses": state.statuses}))).into_response();
        }
        if path.ends_with("/branch_protections") {
            let mut rules: Vec<_> = state.branch.iter().cloned().collect();
            if state.extra_rule {
                rules.push(json!({"rule_name": "*"}));
            }
            return (StatusCode::OK, Json(Value::Array(rules))).into_response();
        }
        if path.ends_with("branch_protections/main") {
            return state
                .branch
                .clone()
                .map_or((StatusCode::NOT_FOUND, Json(Value::Null)), |value| {
                    (StatusCode::OK, Json(value))
                })
                .into_response();
        }
        if path.ends_with("/pulls/7/reviews") {
            return review_page(state, uri);
        }
        if path.contains("/pulls/7/reviews/") && path.ends_with("/comments") {
            let review_id = path
                .split("/reviews/")
                .nth(1)
                .and_then(|suffix| suffix.strip_suffix("/comments"))
                .and_then(|id| id.parse::<u64>().ok())
                .unwrap_or_default();
            return (
                StatusCode::OK,
                Json(
                    state
                        .review_comments
                        .get(&review_id)
                        .cloned()
                        .unwrap_or_default(),
                ),
            )
                .into_response();
        }
        (
            StatusCode::OK,
            Json(if path.ends_with("pulls/7") {
                state.pr.clone()
            } else {
                state.repo.clone()
            }),
        )
            .into_response()
    }

    fn review_page(state: &Fixture, uri: &Uri) -> axum::response::Response {
        const PAGE_SIZE: usize = 50;
        let page = uri
            .query()
            .and_then(|query| query.split('&').find_map(|pair| pair.strip_prefix("page=")))
            .and_then(|page| page.parse::<usize>().ok())
            .unwrap_or(1);
        let start = page.saturating_sub(1) * PAGE_SIZE;
        let reviews = state
            .reviews
            .iter()
            .skip(start)
            .take(PAGE_SIZE)
            .cloned()
            .collect::<Vec<_>>();
        let total = state.reviews.len().to_string();
        let mut response = (StatusCode::OK, Json(reviews)).into_response();
        response
            .headers_mut()
            .insert("x-total-count", total.parse().unwrap());
        response
    }

    fn fixture_write(
        state: &mut Fixture,
        method: Method,
        path: &str,
        body: Value,
    ) -> axum::response::Response {
        state.writes.push((method, path.to_owned(), body.clone()));
        if path.ends_with("/merge") {
            let status = state.merge_response.unwrap_or(StatusCode::CREATED);
            return (status, Json(Value::Null)).into_response();
        }
        if state.accept_writes {
            if path.contains("branch_protections") {
                state.branch = Some(body);
            } else {
                for key in ["allow_manual_merge", "autodetect_manual_merge"] {
                    if let Some(value) = body.get(key) {
                        state.manual_controls[key] = value.clone();
                    }
                }
                state
                    .repo
                    .as_object_mut()
                    .unwrap()
                    .extend(body.as_object().unwrap().clone());
            }
        }
        (StatusCode::OK, Json(Value::Null)).into_response()
    }

    async fn handler(
        State(state): State<Arc<Mutex<Fixture>>>,
        method: Method,
        uri: Uri,
        body: Option<Json<Value>>,
    ) -> axum::response::Response {
        let mut state = state.lock().unwrap();
        if method == Method::GET {
            return fixture_get(&state, &uri);
        }
        fixture_write(
            &mut state,
            method,
            uri.path(),
            body.map_or(Value::Null, |Json(body)| body),
        )
    }

    async fn serve(fixture: Fixture) -> Server {
        let state = Arc::new(Mutex::new(fixture));
        let router = Router::new()
            .route("/{*path}", any(handler))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Server {
            client: Forgejo::new(&origin, "synthetic-token".into()).unwrap(),
            state,
            task,
        }
    }

    #[tokio::test]
    async fn dry_run_does_not_write_then_apply_and_rerun_converge() {
        let server = serve(Fixture::default()).await;
        let drift = server
            .client
            .reconcile("example/sample", "ci", true)
            .await
            .unwrap();
        assert!(!drift.is_empty());
        assert!(server.state.lock().unwrap().writes.is_empty());
        assert_eq!(
            server
                .client
                .reconcile("example/sample", "ci", false)
                .await
                .unwrap(),
            drift
        );
        assert_eq!(server.state.lock().unwrap().writes.len(), 2);
        assert!(server
            .client
            .reconcile("example/sample", "ci", false)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(server.state.lock().unwrap().writes.len(), 3);
        server.state.lock().unwrap().branch.as_mut().unwrap()["enable_status_check"] = false.into();
        assert_eq!(
            server
                .client
                .reconcile("example/sample", "ci", false)
                .await
                .unwrap(),
            ["main.enable_status_check"]
        );
        assert_eq!(
            server.state.lock().unwrap().writes.last().unwrap().0,
            Method::PATCH
        );
    }

    #[tokio::test]
    async fn only_the_gate_workflows_pull_request_context_satisfies_the_policy() {
        let required = REQUIRED_CHECK_CONTEXT.to_owned();
        // The same job on another event posts a different context.
        let other_event = "gate / gate (push)".to_owned();
        let server = serve(Fixture {
            repo: Policy::repository(),
            branch: Some(Policy::new(&other_event).unwrap().branch()),
            ..Fixture::default()
        })
        .await;

        assert_eq!(
            server
                .client
                .reconcile("example/sample", &required, true)
                .await
                .unwrap(),
            ["main.status_check_contexts"]
        );
        assert!(server
            .client
            .schedule("example/sample", &required, 7)
            .await
            .is_err());
        assert!(server.state.lock().unwrap().writes.is_empty());

        server
            .client
            .reconcile("example/sample", &required, false)
            .await
            .unwrap();
        assert_eq!(
            server.state.lock().unwrap().branch.as_ref().unwrap()["status_check_contexts"],
            json!(["gate / gate (pull_request)"])
        );
        server
            .client
            .schedule("example/sample", &required, 7)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn rejected_review_policy_drift_blocks_scheduling_until_reconciled() {
        let policy = Policy::new("ci").unwrap();
        let mut branch = policy.branch();
        branch["block_on_rejected_reviews"] = false.into();
        let server = serve(Fixture {
            repo: Policy::repository(),
            branch: Some(branch),
            ..Fixture::default()
        })
        .await;

        assert_eq!(
            server
                .client
                .reconcile("example/sample", "ci", true)
                .await
                .unwrap(),
            ["main.block_on_rejected_reviews"]
        );
        assert!(server
            .client
            .schedule("example/sample", "ci", 7)
            .await
            .is_err());
        assert!(server.state.lock().unwrap().writes.is_empty());

        assert_eq!(
            server
                .client
                .reconcile("example/sample", "ci", false)
                .await
                .unwrap(),
            ["main.block_on_rejected_reviews"]
        );
        assert_eq!(
            server.state.lock().unwrap().branch.as_ref().unwrap()["block_on_rejected_reviews"],
            true
        );
        server
            .client
            .schedule("example/sample", "ci", 7)
            .await
            .unwrap();
        assert!(server
            .state
            .lock()
            .unwrap()
            .writes
            .last()
            .unwrap()
            .1
            .ends_with("/merge"));
    }

    #[tokio::test]
    async fn an_apply_disables_write_only_manual_controls_even_without_readable_drift() {
        let policy = Policy::new("ci").unwrap();
        let server = serve(Fixture {
            repo: Policy::repository(),
            branch: Some(policy.branch()),
            ..Fixture::default()
        })
        .await;
        assert!(server
            .client
            .reconcile("example/sample", "ci", true)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            server.state.lock().unwrap().manual_controls["allow_manual_merge"],
            true
        );
        assert!(server
            .client
            .reconcile("example/sample", "ci", false)
            .await
            .unwrap()
            .is_empty());
        let state = server.state.lock().unwrap();
        assert_eq!(
            state.manual_controls,
            json!({"allow_manual_merge": false, "autodetect_manual_merge": false})
        );
        assert_eq!(state.writes.len(), 1);
        assert_eq!(
            state.writes[0].2,
            json!({"allow_manual_merge": false, "autodetect_manual_merge": false})
        );
    }

    #[tokio::test]
    async fn an_additional_protection_rule_blocks_mutation_and_scheduling() {
        let server = serve(Fixture {
            extra_rule: true,
            ..Fixture::default()
        })
        .await;
        assert!(server
            .client
            .reconcile("example/sample", "ci", false)
            .await
            .is_err());
        assert!(server
            .client
            .schedule("example/sample", "ci", 7)
            .await
            .is_err());
        assert!(server.state.lock().unwrap().writes.is_empty());
    }

    #[tokio::test]
    async fn ignored_writes_fail_read_back_verification() {
        let server = serve(Fixture {
            accept_writes: false,
            ..Fixture::default()
        })
        .await;
        assert!(server
            .client
            .reconcile("example/sample", "ci", false)
            .await
            .unwrap_err()
            .to_string()
            .contains("does not match"));
    }

    #[tokio::test]
    async fn scheduling_fails_closed_on_missing_checks_and_drafts() {
        let server = serve(Fixture::default()).await;
        assert!(server
            .client
            .schedule("example/sample", "ci", 7)
            .await
            .is_err());
        assert!(server.state.lock().unwrap().writes.is_empty());
        server
            .client
            .reconcile("example/sample", "ci", false)
            .await
            .unwrap();
        for (key, value) in [
            ("draft", json!(true)),
            ("state", json!("closed")),
            ("merged", json!(true)),
        ] {
            let previous = server.state.lock().unwrap().pr[key].clone();
            server.state.lock().unwrap().pr[key] = value;
            assert!(server
                .client
                .schedule("example/sample", "ci", 7)
                .await
                .is_err());
            server.state.lock().unwrap().pr[key] = previous;
        }
        server.state.lock().unwrap().pr["base"]["ref"] = "other".into();
        assert!(server
            .client
            .schedule("example/sample", "ci", 7)
            .await
            .is_err());
        assert_eq!(server.state.lock().unwrap().writes.len(), 2);
    }

    #[tokio::test]
    async fn unresolved_review_conversation_on_later_review_page_blocks_merge() {
        let fixture = Fixture {
            reviews: (1..=55)
                .map(|id| json!({"id": id, "code_comments_count": i32::from(id == 55)}))
                .collect(),
            review_comments: HashMap::from([(55, vec![json!({"id": 550, "resolver": null})])]),
            ..Fixture::default()
        };
        let server = serve(fixture).await;
        server
            .client
            .reconcile("example/sample", "ci", false)
            .await
            .unwrap();

        let error = server
            .client
            .schedule("example/sample", "ci", 7)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("unresolved review conversation"));
        let state = server.state.lock().unwrap();
        assert_eq!(state.writes.len(), 2);
    }

    #[tokio::test]
    async fn resolved_review_conversation_allows_merge() {
        let fixture = Fixture {
            reviews: vec![json!({"id": 12, "code_comments_count": 1})],
            review_comments: HashMap::from([(12, vec![json!({"id": 120, "resolver": {"id": 2}})])]),
            ..Fixture::default()
        };
        let server = serve(fixture).await;
        server
            .client
            .reconcile("example/sample", "ci", false)
            .await
            .unwrap();

        server
            .client
            .schedule("example/sample", "ci", 7)
            .await
            .unwrap();
        assert!(server
            .state
            .lock()
            .unwrap()
            .writes
            .last()
            .unwrap()
            .1
            .ends_with("/merge"));
    }

    #[tokio::test]
    async fn missing_review_resolution_metadata_blocks_merge() {
        let fixture = Fixture {
            reviews: vec![json!({"id": 12, "code_comments_count": 1})],
            review_comments: HashMap::from([(12, vec![json!({"id": 120})])]),
            ..Fixture::default()
        };
        let server = serve(fixture).await;
        server
            .client
            .reconcile("example/sample", "ci", false)
            .await
            .unwrap();

        let error = server
            .client
            .schedule("example/sample", "ci", 7)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("omitted its resolution state"));
        assert_eq!(server.state.lock().unwrap().writes.len(), 2);
    }

    #[tokio::test]
    async fn empty_review_resolver_object_blocks_merge() {
        let fixture = Fixture {
            reviews: vec![json!({"id": 12, "code_comments_count": 1})],
            review_comments: HashMap::from([(12, vec![json!({"id": 120, "resolver": {}})])]),
            ..Fixture::default()
        };
        let server = serve(fixture).await;
        server
            .client
            .reconcile("example/sample", "ci", false)
            .await
            .unwrap();

        let error = server
            .client
            .schedule("example/sample", "ci", 7)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid resolution metadata"));
        assert_eq!(server.state.lock().unwrap().writes.len(), 2);
    }

    #[tokio::test]
    async fn ready_pr_schedules_squash_with_its_title_body_and_branch_deletion() {
        let server = serve(Fixture::default()).await;
        server
            .client
            .reconcile("example/sample", "ci", false)
            .await
            .unwrap();
        server
            .client
            .schedule("example/sample", "ci", 7)
            .await
            .unwrap();
        let body = server
            .state
            .lock()
            .unwrap()
            .writes
            .last()
            .unwrap()
            .2
            .clone();
        assert_eq!(body["Do"], "squash");
        assert_eq!(body["merge_when_checks_succeed"], true);
        assert_eq!(body["force_merge"], false);
        assert_eq!(body["delete_branch_after_merge"], true);
        assert_eq!(body["MergeTitleField"], "Publish source");
        assert_eq!(body["MergeMessageField"], "Client-safe source change");
        assert_eq!(body["head_commit_id"], "synthetic-head");
        server.state.lock().unwrap().merge_response = Some(StatusCode::METHOD_NOT_ALLOWED);
        assert!(server
            .client
            .schedule("example/sample", "ci", 7)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn an_already_green_head_merges_now_instead_of_waiting_for_another_check() {
        let server = serve(Fixture::default()).await;
        server
            .client
            .reconcile("example/sample", "ci", false)
            .await
            .unwrap();
        // Another context succeeding, or the required one pending or failed,
        // leaves the merge to Forgejo's scheduler.
        for statuses in [
            json!([{"context": "other", "status": "success"}]),
            json!([{"context": "ci", "status": "pending"}]),
            json!([{"context": "ci", "status": "failure"}]),
        ] {
            server.state.lock().unwrap().statuses = statuses;
            server
                .client
                .schedule("example/sample", "ci", 7)
                .await
                .unwrap();
            let body = server
                .state
                .lock()
                .unwrap()
                .writes
                .last()
                .unwrap()
                .2
                .clone();
            assert_eq!(body["merge_when_checks_succeed"], true);
        }
        // Forgejo 15.0.9 re-checks a scheduled merge only on a later event, so
        // a head whose required check already passed is merged now, pinned to
        // that head and still subject to Forgejo's own protection checks.
        server.state.lock().unwrap().statuses = json!([
            {"context": "other", "status": "failure"},
            {"context": "ci", "status": "success"},
        ]);
        server
            .client
            .schedule("example/sample", "ci", 7)
            .await
            .unwrap();
        let body = server
            .state
            .lock()
            .unwrap()
            .writes
            .last()
            .unwrap()
            .2
            .clone();
        assert_eq!(body["merge_when_checks_succeed"], false);
        assert_eq!(body["Do"], "squash");
        assert_eq!(body["force_merge"], false);
        assert_eq!(body["delete_branch_after_merge"], true);
        assert_eq!(body["head_commit_id"], "synthetic-head");
        assert_eq!(body["MergeTitleField"], "Publish source");
        assert_eq!(body["MergeMessageField"], "Client-safe source change");
        // A refused immediate merge is reported, never retried as forced.
        server.state.lock().unwrap().merge_response = Some(StatusCode::METHOD_NOT_ALLOWED);
        assert!(server
            .client
            .schedule("example/sample", "ci", 7)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn single_pull_request_conflict_requires_manual_inspection() {
        let server = serve(Fixture {
            merge_response: Some(StatusCode::CONFLICT),
            ..Fixture::default()
        })
        .await;
        server
            .client
            .reconcile("example/sample", "ci", false)
            .await
            .unwrap();

        let error = server
            .client
            .schedule("example/sample", "ci", 7)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("merge conflict"));
        assert!(error.contains("inspect the pull request"));
    }

    #[test]
    fn credentials_and_remote_plaintext_origins_are_rejected() {
        for origin in [
            "http://example.test/",
            "https://token@example.test/",
            "https://example.test/subpath",
            "https://example.test/?query=token",
            "https://example.test/#token",
        ] {
            assert!(Forgejo::new(origin, "synthetic-token".into()).is_err());
        }
        assert!(Forgejo::new("https://example.test/", String::new()).is_err());
        assert!(Forgejo::new("https://example.test/", "synthetic-token".into()).is_ok());
    }

    #[test]
    fn policy_requires_prs_checks_signatures_and_squash_without_reviews() {
        let policy = Policy::new("ci / ci (pull_request)").unwrap();
        let branch = policy.branch();
        assert_eq!(branch["enable_push"], false);
        assert_eq!(branch["apply_to_admins"], true);
        assert_eq!(branch["require_signed_commits"], true);
        assert_eq!(
            branch["status_check_contexts"],
            json!(["ci / ci (pull_request)"])
        );
        assert_eq!(branch["required_approvals"], 0);
        let repo = Policy::repository();
        assert_eq!(repo["allow_squash_merge"], true);
        assert_eq!(repo["allow_merge_commits"], false);
        assert_eq!(Policy::repository_write()["allow_manual_merge"], false);
        assert_eq!(repo["allow_rebase"], false);
        assert_eq!(repo["default_delete_branch_after_merge"], true);
    }

    #[test]
    fn empty_or_multiline_check_context_is_rejected() {
        for context in ["", " ", "ci\nother", "ci\rother"] {
            assert!(Policy::new(context).is_err());
        }
    }

    #[test]
    fn missing_and_weakened_controls_are_drift_but_metadata_is_ignored() {
        let wanted = json!({"enable_push": false, "status_check_contexts": ["ci"]});
        assert_eq!(differences(&wanted, &json!({})).len(), 2);
        assert_eq!(
            differences(
                &wanted,
                &json!({"enable_push": true, "status_check_contexts": ["ci"]})
            ),
            ["enable_push"]
        );
        let actual = json!({"enable_push": false, "status_check_contexts": ["ci"], "id": 42});
        assert!(differences(&wanted, &actual).is_empty());
    }

    #[test]
    fn repository_targets_cannot_escape_the_explicit_owner_and_name() {
        for target in [
            "",
            "repo",
            "/repo",
            "owner/",
            "owner/repo/extra",
            "../repo",
            "owner/repo?x",
            "owner/repo#x",
        ] {
            assert!(repository_target(target).is_err(), "{target}");
        }
        assert_eq!(
            repository_target("example-law/sample-litigation").unwrap(),
            "example-law/sample-litigation"
        );
    }
}
