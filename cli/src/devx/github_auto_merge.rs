//! `navigator ops github auto-merge --pull-request <n> [repository]`: put a
//! pull request on the path to `main` under the `production` ruleset.
//!
//! GitHub's auto-merge only queues a merge that is still waiting on something.
//! Once every required check has passed, `enablePullRequestAutoMerge` answers
//! that the pull request "is in clean status" (or "unstable status", when a
//! check the ruleset does not require failed) and queues nothing — so arming a
//! green pull request fails exactly when it is ready. This verb closes that
//! gap: it arms squash auto-merge when GitHub will take it, and when GitHub
//! says the pull request is mergeable now, it squash-merges directly, but only
//! after reading the base branch's live rules and confirming every required
//! status check passed on the head commit.
//!
//! The required checks come from the live rules rather than a constant, so the
//! direct merge holds the same gate the `production` ruleset holds — see
//! [`super::github_setup`]. A branch with no required status check has no gate
//! to hold, and the direct merge is refused rather than merging ungated.
//!
//! The merge is pinned to the head commit it checked (`sha` on the merge call),
//! so a push between the check and the merge fails the merge instead of
//! landing an unchecked commit.

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::github_setup::{GitHubClient, RepositoryTarget};

/// What GitHub says when there is nothing left for auto-merge to wait on.
const ALREADY_MERGEABLE: &[&str] = &["is in clean status", "is in unstable status"];

/// What GitHub says when an App without the `workflows` permission is asked to
/// land a pull request that edits `.github/workflows/*`. No CI change can fix
/// it, so it is reported and passed over rather than failing the job.
const WORKFLOW_PERMISSION_REFUSAL: &str =
    "refusing to allow a GitHub App to create or update workflow";

/// How a run left the pull request.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// Merged or closed already: nothing to do.
    Closed,
    /// A draft is how a ready pull request is held, so it is left alone.
    Draft,
    /// Auto-merge was already armed.
    AlreadyArmed,
    /// Auto-merge is armed and GitHub will squash-merge when the gate is green.
    Armed,
    /// The gate was already green, so the pull request was squash-merged now.
    Merged,
    /// The App cannot land a pull request that edits workflow files.
    WorkflowPermissionRefused,
}

#[derive(Debug, Deserialize)]
struct PullRequest {
    node_id: String,
    state: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    merged: bool,
    auto_merge: Option<serde_json::Value>,
    head: Ref,
    base: Ref,
}

#[derive(Debug, Deserialize)]
struct Ref {
    sha: String,
    #[serde(rename = "ref")]
    name: String,
}

#[derive(Debug, Deserialize)]
struct BranchRule {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    parameters: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct CheckRuns {
    check_runs: Vec<CheckRun>,
}

#[derive(Debug, Deserialize)]
struct CheckRun {
    status: String,
    conclusion: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphqlResponse {
    #[serde(default)]
    errors: Vec<GraphqlError>,
}

#[derive(Debug, Deserialize)]
struct GraphqlError {
    message: String,
}

/// Arm, or merge, one pull request.
///
/// # Errors
///
/// When GitHub refuses for any reason other than the two this verb resolves,
/// when the base branch requires no status check, or when a required check has
/// not passed on the head commit.
pub(super) fn run(target: &RepositoryTarget, number: u64) -> Result<()> {
    let client = GitHubClient::from_env(target)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    let outcome = runtime.block_on(auto_merge(&client, number))?;
    let repository = &client.repository;
    match outcome {
        Outcome::Closed => println!("{repository}#{number} is already closed; nothing to do"),
        Outcome::Draft => println!("{repository}#{number} is a draft; left alone"),
        Outcome::AlreadyArmed => println!("{repository}#{number} already has auto-merge armed"),
        Outcome::Armed => println!("{repository}#{number}: squash auto-merge armed"),
        Outcome::Merged => {
            println!("{repository}#{number}: required checks already passed; squash-merged");
        }
        Outcome::WorkflowPermissionRefused => println!(
            "::warning::{repository}#{number} edits workflow files and the App lacks the \
             `workflows` permission. Merge this pull request manually."
        ),
    }
    Ok(())
}

async fn auto_merge(client: &GitHubClient, number: u64) -> Result<Outcome> {
    let pull: PullRequest = client
        .get_json(&client.repo_path(&format!("/pulls/{number}")))
        .await?;
    if pull.merged || pull.state != "open" {
        return Ok(Outcome::Closed);
    }
    if pull.draft {
        return Ok(Outcome::Draft);
    }
    if pull.auto_merge.is_some() {
        return Ok(Outcome::AlreadyArmed);
    }

    let errors = enable_auto_merge(client, &pull.node_id).await?;
    if errors.is_empty() {
        return Ok(Outcome::Armed);
    }
    if refused_for_workflows(&errors) {
        return Ok(Outcome::WorkflowPermissionRefused);
    }
    if !errors
        .iter()
        .any(|error| ALREADY_MERGEABLE.iter().any(|text| error.contains(text)))
    {
        bail!(
            "GitHub refused to arm auto-merge on {}#{number}: {}",
            client.repository,
            errors.join("; ")
        );
    }

    let required = required_checks(client, &pull.base.name).await?;
    assert_checks_passed(client, &pull.head.sha, &required).await?;
    merge(client, number, &pull.head.sha).await
}

/// The `enablePullRequestAutoMerge` mutation, squash only: the merge settings
/// `ops github setup` reconciles allow no other method. Returns GitHub's error
/// messages, empty when auto-merge was armed.
async fn enable_auto_merge(client: &GitHubClient, node_id: &str) -> Result<Vec<String>> {
    let url = client.url("/graphql");
    let response = client
        .http
        .post(&url)
        .json(&serde_json::json!({
            "query": "mutation($pr: ID!) { enablePullRequestAutoMerge(input: \
                      { pullRequestId: $pr, mergeMethod: SQUASH }) { pullRequest { number } } }",
            "variables": { "pr": node_id },
        }))
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("GitHub POST {url} returned {}: {body}", status.as_u16());
    }
    let parsed: GraphqlResponse = serde_json::from_str(&body)
        .with_context(|| format!("decode GitHub response from {url}"))?;
    Ok(parsed
        .errors
        .into_iter()
        .map(|error| error.message)
        .collect())
}

fn refused_for_workflows(messages: &[String]) -> bool {
    messages
        .iter()
        .any(|message| message.contains(WORKFLOW_PERMISSION_REFUSAL))
}

/// Every status-check context the base branch's live rules require — the
/// `production` ruleset's gate, read rather than assumed.
async fn required_checks(client: &GitHubClient, base: &str) -> Result<Vec<String>> {
    let rules: Vec<BranchRule> = client
        .get_json(&client.repo_path(&format!("/rules/branches/{base}")))
        .await?;
    let contexts: Vec<String> = rules
        .iter()
        .filter(|rule| rule.kind == "required_status_checks")
        .filter_map(|rule| rule.parameters.as_ref())
        .filter_map(|parameters| parameters["required_status_checks"].as_array())
        .flatten()
        .filter_map(|check| check["context"].as_str().map(str::to_string))
        .collect();
    if contexts.is_empty() {
        bail!(
            "{}'s `{base}` requires no status check, so a direct merge would be ungated; \
             run `navigator ops github setup {}` to put the `production` ruleset in place",
            client.repository,
            client.repository
        );
    }
    Ok(contexts)
}

/// Refuse unless each required context has a completed, successful check run
/// on the head commit.
async fn assert_checks_passed(client: &GitHubClient, sha: &str, required: &[String]) -> Result<()> {
    for context in required {
        let runs: CheckRuns = client
            .get_json(&client.repo_path(&format!(
                "/commits/{sha}/check-runs?check_name={context}&filter=latest"
            )))
            .await?;
        let passed = runs
            .check_runs
            .iter()
            .any(|run| run.status == "completed" && run.conclusion.as_deref() == Some("success"));
        if !passed {
            bail!(
                "required check {context:?} has not passed on {sha}; \
                 GitHub reported the pull request mergeable, but the gate is not green"
            );
        }
    }
    Ok(())
}

/// Squash-merge, pinned to the head commit the checks were read on.
async fn merge(client: &GitHubClient, number: u64, sha: &str) -> Result<Outcome> {
    let url = client.url(&client.repo_path(&format!("/pulls/{number}/merge")));
    let response = client
        .http
        .put(&url)
        .json(&serde_json::json!({ "merge_method": "squash", "sha": sha }))
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if status.is_success() {
        return Ok(Outcome::Merged);
    }
    if body.contains(WORKFLOW_PERMISSION_REFUSAL) {
        return Ok(Outcome::WorkflowPermissionRefused);
    }
    bail!("GitHub PUT {url} returned {}: {body}", status.as_u16())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const REPO: &str = "acme/navigator";
    const HEAD: &str = "abc123";

    fn pull(overrides: &serde_json::Value) -> serde_json::Value {
        let mut pull = serde_json::json!({
            "node_id": "PR_1",
            "state": "open",
            "draft": false,
            "merged": false,
            "auto_merge": null,
            "head": { "sha": HEAD, "ref": "feature" },
            "base": { "sha": "def456", "ref": "main" },
        });
        for (key, value) in overrides.as_object().unwrap() {
            pull[key] = value.clone();
        }
        pull
    }

    async fn mount_pull(server: &MockServer, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path(format!("/repos/{REPO}/pulls/7")))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    async fn mount_graphql(server: &MockServer, errors: &[&str]) {
        let errors: Vec<_> = errors
            .iter()
            .map(|message| serde_json::json!({ "message": message }))
            .collect();
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "enablePullRequestAutoMerge": null },
                "errors": errors,
            })))
            .mount(server)
            .await;
    }

    async fn mount_rules(server: &MockServer, contexts: &[&str]) {
        let checks: Vec<_> = contexts
            .iter()
            .map(|context| serde_json::json!({ "context": context, "integration_id": 15368 }))
            .collect();
        let mut rules = vec![serde_json::json!({ "type": "pull_request" })];
        if !checks.is_empty() {
            rules.push(serde_json::json!({
                "type": "required_status_checks",
                "parameters": { "required_status_checks": checks },
            }));
        }
        Mock::given(method("GET"))
            .and(path(format!("/repos/{REPO}/rules/branches/main")))
            .respond_with(ResponseTemplate::new(200).set_body_json(rules))
            .mount(server)
            .await;
    }

    async fn mount_check(server: &MockServer, context: &str, conclusion: &str) {
        Mock::given(method("GET"))
            .and(path(format!("/repos/{REPO}/commits/{HEAD}/check-runs")))
            .and(query_param("check_name", context))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "check_runs": [{ "status": "completed", "conclusion": conclusion }],
            })))
            .mount(server)
            .await;
    }

    async fn mount_merge(server: &MockServer) {
        Mock::given(method("PUT"))
            .and(path(format!("/repos/{REPO}/pulls/7/merge")))
            .and(body_json(
                serde_json::json!({ "merge_method": "squash", "sha": HEAD }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "merged": true,
            })))
            .mount(server)
            .await;
    }

    async fn merge_requests(server: &MockServer) -> usize {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.method == wiremock::http::Method::PUT)
            .count()
    }

    #[tokio::test]
    async fn a_pull_request_still_waiting_on_its_gate_is_armed() {
        let server = MockServer::start().await;
        mount_pull(&server, pull(&serde_json::json!({}))).await;
        mount_graphql(&server, &[]).await;
        let client = GitHubClient::for_test(&server.uri(), REPO);
        assert_eq!(auto_merge(&client, 7).await.unwrap(), Outcome::Armed);
        assert_eq!(merge_requests(&server).await, 0);
    }

    /// The case that failed every re-run of the CI job: `ci` had already
    /// passed, so GitHub had nothing to queue. The verb merges instead, pinned
    /// to the head it checked.
    #[tokio::test]
    async fn a_green_pull_request_github_will_not_queue_is_squash_merged() {
        for refusal in [
            "Pull request Pull request is in clean status",
            "Pull request Pull request is in unstable status",
        ] {
            let server = MockServer::start().await;
            mount_pull(&server, pull(&serde_json::json!({}))).await;
            mount_graphql(&server, &[refusal]).await;
            mount_rules(&server, &["ci"]).await;
            mount_check(&server, "ci", "success").await;
            mount_merge(&server).await;
            let client = GitHubClient::for_test(&server.uri(), REPO);
            assert_eq!(
                auto_merge(&client, 7).await.unwrap(),
                Outcome::Merged,
                "{refusal}"
            );
            assert_eq!(merge_requests(&server).await, 1, "{refusal}");
        }
    }

    /// GitHub saying "unstable" is not the gate saying green: a required check
    /// that failed stops the direct merge.
    #[tokio::test]
    async fn a_failed_required_check_is_never_merged_around() {
        let server = MockServer::start().await;
        mount_pull(&server, pull(&serde_json::json!({}))).await;
        mount_graphql(
            &server,
            &["Pull request Pull request is in unstable status"],
        )
        .await;
        mount_rules(&server, &["ci"]).await;
        mount_check(&server, "ci", "failure").await;
        mount_merge(&server).await;
        let client = GitHubClient::for_test(&server.uri(), REPO);
        let error = auto_merge(&client, 7).await.unwrap_err().to_string();
        assert!(error.contains("\"ci\" has not passed"), "{error}");
        assert_eq!(merge_requests(&server).await, 0);
    }

    /// Without a required status check there is no gate for the direct merge
    /// to hold, which is the state a fresh repository is in before
    /// `ops github setup` runs.
    #[tokio::test]
    async fn a_branch_without_a_required_check_is_refused() {
        let server = MockServer::start().await;
        mount_pull(&server, pull(&serde_json::json!({}))).await;
        mount_graphql(
            &server,
            &["Pull request Pull request is in unstable status"],
        )
        .await;
        mount_rules(&server, &[]).await;
        mount_merge(&server).await;
        let client = GitHubClient::for_test(&server.uri(), REPO);
        let error = auto_merge(&client, 7).await.unwrap_err().to_string();
        assert!(error.contains("ops github setup"), "{error}");
        assert_eq!(merge_requests(&server).await, 0);
    }

    #[tokio::test]
    async fn drafts_closed_and_already_armed_pull_requests_are_left_alone() {
        for (overrides, expected) in [
            (serde_json::json!({ "draft": true }), Outcome::Draft),
            (serde_json::json!({ "state": "closed" }), Outcome::Closed),
            (serde_json::json!({ "merged": true }), Outcome::Closed),
            (
                serde_json::json!({ "auto_merge": { "merge_method": "squash" } }),
                Outcome::AlreadyArmed,
            ),
        ] {
            let server = MockServer::start().await;
            mount_pull(&server, pull(&overrides)).await;
            let client = GitHubClient::for_test(&server.uri(), REPO);
            assert_eq!(auto_merge(&client, 7).await.unwrap(), expected);
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn a_workflow_permission_refusal_is_reported_not_failed() {
        let server = MockServer::start().await;
        mount_pull(&server, pull(&serde_json::json!({}))).await;
        mount_graphql(
            &server,
            &["refusing to allow a GitHub App to create or update workflow `.github/workflows/ci.yml` without `workflows` permission"],
        )
        .await;
        let client = GitHubClient::for_test(&server.uri(), REPO);
        assert_eq!(
            auto_merge(&client, 7).await.unwrap(),
            Outcome::WorkflowPermissionRefused
        );
    }

    #[tokio::test]
    async fn any_other_refusal_fails() {
        let server = MockServer::start().await;
        mount_pull(&server, pull(&serde_json::json!({}))).await;
        mount_graphql(&server, &["Auto merge is not allowed for this repository"]).await;
        let client = GitHubClient::for_test(&server.uri(), REPO);
        let error = auto_merge(&client, 7).await.unwrap_err().to_string();
        assert!(error.contains("Auto merge is not allowed"), "{error}");
    }
}
