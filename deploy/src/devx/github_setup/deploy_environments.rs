//! The two GitHub Environments `release-deploy.yml` ships through, kept in Rust
//! rather than in GitHub's settings UI.
//!
//! `staging-deploy` and `production-deploy` hold the only deployment
//! credentials the release pipeline uses. What decides *where* a tag goes is
//! the workflow, not the environment: a prerelease (`-rc.N`, `-hotfix.N`)
//! rolls to staging and is held there, and every other tag rolls on to
//! production once staging's health checks pass. A required reviewer on either
//! environment would put a person back between a green staging roll and
//! production, which is exactly the hold a stable release is meant not to
//! have, and it is a setting nobody can review in a pull request. So the
//! desired state is written down here: no reviewers, no wait timer, and
//! deployment only from `main`.
//!
//! Only the protection half is reconciled. The secrets and variables each
//! environment carries are provisioned by hand (docs/release-automation.md)
//! and are never read or written by this command.

use anyhow::{bail, Result};
use serde::Deserialize;

use super::{parse_json, Action, GitHubClient, RepositoryPolicy};

/// Every environment a `release-deploy.yml` ship job runs in.
pub(super) const DEPLOY_ENVIRONMENTS: [&str; 2] = ["staging-deploy", "production-deploy"];

/// The one branch a deploy environment admits. `release-deploy.yml` is only
/// ever dispatched on `main`, and the release-ship workload identity provider
/// refuses any other ref; this keeps the environment's own gate saying the same.
const DEPLOY_BRANCH: &str = "main";

#[derive(Debug, Deserialize)]
struct LiveEnvironment {
    #[serde(default)]
    protection_rules: Vec<ProtectionRule>,
    deployment_branch_policy: Option<BranchPolicy>,
}

#[derive(Debug, Deserialize)]
struct ProtectionRule {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    wait_timer: Option<u64>,
    #[serde(default)]
    reviewers: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct BranchPolicy {
    protected_branches: bool,
    custom_branch_policies: bool,
}

#[derive(Debug, Deserialize)]
struct BranchPolicies {
    #[serde(default)]
    branch_policies: Vec<NamedBranchPolicy>,
}

#[derive(Debug, Deserialize)]
struct NamedBranchPolicy {
    id: u64,
    name: String,
    #[serde(rename = "type", default = "branch_kind")]
    kind: String,
}

fn branch_kind() -> String {
    "branch".to_string()
}

const DESIRED_BRANCH_POLICY: BranchPolicy = BranchPolicy {
    protected_branches: false,
    custom_branch_policies: true,
};

/// Whether a live environment's protection rules already hold nothing between
/// a ship job and its row: no reviewer and no wait timer.
fn rules_unattended(environment: &LiveEnvironment) -> bool {
    environment
        .protection_rules
        .iter()
        .all(|rule| match rule.kind.as_str() {
            "required_reviewers" => rule.reviewers.is_empty(),
            "wait_timer" => rule.wait_timer.unwrap_or(0) == 0,
            _ => true,
        })
}

/// Whether the environment's custom branch policies are exactly `main`.
fn only_main(policies: &[NamedBranchPolicy]) -> bool {
    matches!(policies, [only] if only.name == DEPLOY_BRANCH && only.kind == "branch")
}

fn environment_path(client: &GitHubClient, name: &str) -> String {
    client.repo_path(&format!("/environments/{name}"))
}

fn branch_policies_path(client: &GitHubClient, name: &str) -> String {
    format!(
        "{}/deployment-branch-policies",
        environment_path(client, name)
    )
}

/// Read one environment, `None` when it does not exist yet.
async fn read_environment(client: &GitHubClient, name: &str) -> Result<Option<LiveEnvironment>> {
    let url = client.url(&environment_path(client, name));
    let response = client.http.get(&url).send().await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    parse_json(response, &url).await.map(Some)
}

async fn read_branch_policies(client: &GitHubClient, name: &str) -> Result<Vec<NamedBranchPolicy>> {
    let policies: BranchPolicies = client
        .get_json(&format!(
            "{}?per_page=100",
            branch_policies_path(client, name)
        ))
        .await?;
    Ok(policies.branch_policies)
}

/// One update per deploy environment whose live protection differs from the
/// desired state, or none for a repository without `release-deploy.yml`.
/// Read-only, like every other half of the reconcile's plan.
pub(super) async fn plan(policy: RepositoryPolicy, client: &GitHubClient) -> Result<Vec<Action>> {
    if !policy.deploy_environments {
        return Ok(Vec::new());
    }
    Ok(drifted(client)
        .await?
        .into_iter()
        .map(|name| Action::UpdateDeployEnvironment { name })
        .collect())
}

/// The deploy environments whose live protection differs from the desired state.
async fn drifted(client: &GitHubClient) -> Result<Vec<String>> {
    let mut drifted = Vec::new();
    for name in DEPLOY_ENVIRONMENTS {
        let converged = match read_environment(client, name).await? {
            None => false,
            Some(environment) => {
                rules_unattended(&environment)
                    && environment.deployment_branch_policy.as_ref() == Some(&DESIRED_BRANCH_POLICY)
                    && only_main(&read_branch_policies(client, name).await?)
            }
        };
        if !converged {
            drifted.push(name.to_string());
        }
    }
    Ok(drifted)
}

/// Converge one deploy environment: create or update it with no reviewer and
/// no wait timer, then leave `main` as its only deployment branch.
pub(super) async fn apply(client: &GitHubClient, name: &str) -> Result<()> {
    if !DEPLOY_ENVIRONMENTS.contains(&name) {
        bail!("{name} is not a deploy environment this command governs");
    }
    client
        .put_json(
            &environment_path(client, name),
            &serde_json::json!({
                "wait_timer": 0,
                "prevent_self_review": false,
                "reviewers": [],
                "deployment_branch_policy": {
                    "protected_branches": DESIRED_BRANCH_POLICY.protected_branches,
                    "custom_branch_policies": DESIRED_BRANCH_POLICY.custom_branch_policies,
                },
            }),
        )
        .await?;
    let policies = read_branch_policies(client, name).await?;
    for policy in &policies {
        if policy.name != DEPLOY_BRANCH || policy.kind != "branch" {
            let url = client.url(&format!(
                "{}/{}",
                branch_policies_path(client, name),
                policy.id
            ));
            let response = client.http.delete(&url).send().await?;
            let status = response.status();
            if !status.is_success() && status != reqwest::StatusCode::NOT_FOUND {
                let body = response.text().await.unwrap_or_default();
                bail!("GitHub DELETE {url} returned {}: {body}", status.as_u16());
            }
        }
    }
    if !policies
        .iter()
        .any(|policy| policy.name == DEPLOY_BRANCH && policy.kind == "branch")
    {
        client
            .post_json(
                &branch_policies_path(client, name),
                &serde_json::json!({"name": DEPLOY_BRANCH, "type": "branch"}),
            )
            .await?;
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Mount converged reads for both deploy environments, so a reconcile test
    /// that is about something else sees no environment drift.
    pub(in crate::devx::github_setup) async fn mount_converged(server: &MockServer, repo: &str) {
        for name in DEPLOY_ENVIRONMENTS {
            mount_environment(
                server,
                repo,
                name,
                serde_json::json!([{"type": "branch_policy"}]),
            )
            .await;
            mount_branch_policies(
                server,
                repo,
                name,
                serde_json::json!([{"id": 1, "name": "main", "type": "branch"}]),
            )
            .await;
        }
    }

    async fn mount_environment(
        server: &MockServer,
        repo: &str,
        name: &str,
        rules: serde_json::Value,
    ) {
        Mock::given(method("GET"))
            .and(path(format!("/repos/{repo}/environments/{name}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": name,
                "protection_rules": rules,
                "deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}
            })))
            .mount(server)
            .await;
    }

    async fn mount_branch_policies(
        server: &MockServer,
        repo: &str,
        name: &str,
        policies: serde_json::Value,
    ) {
        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/{repo}/environments/{name}/deployment-branch-policies"
            )))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"total_count": 1, "branch_policies": policies}),
                ),
            )
            .mount(server)
            .await;
    }

    fn client(server: &MockServer) -> GitHubClient {
        GitHubClient::for_test(&server.uri(), "acme/navigator")
    }

    #[tokio::test]
    async fn converged_environments_plan_nothing() {
        let server = MockServer::start().await;
        mount_converged(&server, "acme/navigator").await;
        assert!(drifted(&client(&server)).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_required_reviewer_on_production_is_drift() {
        let server = MockServer::start().await;
        mount_environment(
            &server,
            "acme/navigator",
            "staging-deploy",
            serde_json::json!([]),
        )
        .await;
        mount_branch_policies(
            &server,
            "acme/navigator",
            "staging-deploy",
            serde_json::json!([{"id": 1, "name": "main", "type": "branch"}]),
        )
        .await;
        mount_environment(
            &server,
            "acme/navigator",
            "production-deploy",
            serde_json::json!([
                {"type": "required_reviewers", "reviewers": [{"type": "User", "reviewer": {"login": "owner"}}]},
                {"type": "branch_policy"}
            ]),
        )
        .await;
        mount_branch_policies(
            &server,
            "acme/navigator",
            "production-deploy",
            serde_json::json!([{"id": 1, "name": "main", "type": "branch"}]),
        )
        .await;

        assert_eq!(
            drifted(&client(&server)).await.unwrap(),
            vec!["production-deploy"]
        );
    }

    #[tokio::test]
    async fn a_missing_environment_or_extra_branch_is_drift() {
        let server = MockServer::start().await;
        // staging-deploy is unmocked, so wiremock answers 404: it does not exist.
        mount_environment(
            &server,
            "acme/navigator",
            "production-deploy",
            serde_json::json!([]),
        )
        .await;
        mount_branch_policies(
            &server,
            "acme/navigator",
            "production-deploy",
            serde_json::json!([
                {"id": 1, "name": "main", "type": "branch"},
                {"id": 2, "name": "release/*", "type": "branch"}
            ]),
        )
        .await;

        assert_eq!(
            drifted(&client(&server)).await.unwrap(),
            vec!["staging-deploy", "production-deploy"]
        );
    }

    #[tokio::test]
    async fn apply_clears_reviewers_and_leaves_only_main() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/repos/acme/navigator/environments/production-deploy"))
            .and(body_json(serde_json::json!({
                "wait_timer": 0,
                "prevent_self_review": false,
                "reviewers": [],
                "deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        mount_branch_policies(
            &server,
            "acme/navigator",
            "production-deploy",
            serde_json::json!([{"id": 2, "name": "release/*", "type": "branch"}]),
        )
        .await;
        Mock::given(method("DELETE"))
            .and(path(
                "/repos/acme/navigator/environments/production-deploy/deployment-branch-policies/2",
            ))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(
                "/repos/acme/navigator/environments/production-deploy/deployment-branch-policies",
            ))
            .and(body_json(
                serde_json::json!({"name": "main", "type": "branch"}),
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        apply(&client(&server), "production-deploy").await.unwrap();
    }

    #[tokio::test]
    async fn apply_refuses_an_environment_it_does_not_govern() {
        let server = MockServer::start().await;
        assert!(apply(&client(&server), "github-pages").await.is_err());
    }

    #[tokio::test]
    async fn only_navigator_plans_deploy_environments() {
        // No mocks: a policy without `release-deploy.yml` must not even read.
        let server = MockServer::start().await;
        for policy in [
            super::super::COMMON_POLICY,
            super::super::CLIENT_POLICY,
            super::super::TAP_POLICY,
        ] {
            assert!(plan(policy, &client(&server)).await.unwrap().is_empty());
        }
        assert_eq!(
            plan(super::super::NAVIGATOR_POLICY, &client(&server))
                .await
                .unwrap(),
            vec![
                Action::UpdateDeployEnvironment {
                    name: "staging-deploy".into()
                },
                Action::UpdateDeployEnvironment {
                    name: "production-deploy".into()
                },
            ]
        );
    }
}
