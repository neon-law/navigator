//! Binary wiring for `navigator ops github setup <repository> --dry-run`.

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn dry_run_prints_plan_without_writes() {
    let server = MockServer::start().await;
    mount_reads(&server).await;
    Command::cargo_bin("navigator")
        .unwrap()
        .args(["ops", "github", "setup", "neon-law/navigator", "--dry-run"])
        .env("GITHUB_TOKEN", "test-token")
        .env_remove("GITHUB_REPOSITORY")
        .env("NAVIGATOR_GITHUB_API_BASE", server.uri())
        .assert()
        .success()
        .stderr(contains("would update ruleset production"))
        // The repository below has never had the tag gate applied, so the plan
        // creates it rather than skipping the ruleset it could not find.
        .stderr(contains("would create ruleset release-tags"))
        // The existing approval ruleset is removed so it cannot hold up
        // auto-merge after the integrity checks pass.
        .stderr(contains("would delete ruleset production-review"));
    let requests = server.received_requests().await.unwrap();
    assert!(requests
        .iter()
        .all(|request| request.method == wiremock::http::Method::GET));
}

/// The workflow home reads `none`, so the dry run lists the one setting it
/// would change and still writes nothing.
#[tokio::test]
async fn dry_run_lists_the_actions_access_a_none_home_would_get() {
    let server = MockServer::start().await;
    mount_reads(&server).await;
    mount_actions_access(&server, "none").await;
    Command::cargo_bin("navigator")
        .unwrap()
        .args([
            "ops",
            "github",
            "setup",
            "neon-law/navigator",
            "--dry-run",
            "--workflow-home",
            "neon-law/navigator",
        ])
        .env("GITHUB_TOKEN", "test-token")
        .env_remove("GITHUB_REPOSITORY")
        .env("NAVIGATOR_GITHUB_API_BASE", server.uri())
        .assert()
        .success()
        .stderr(contains(
            "would set Actions access on neon-law/navigator to organization",
        ));
    let requests = server.received_requests().await.unwrap();
    assert!(requests
        .iter()
        .any(|request| request.url.path().ends_with("/actions/permissions/access")));
    assert!(requests
        .iter()
        .all(|request| request.method == wiremock::http::Method::GET));
}

/// A home that already admits the organization has nothing to set.
#[tokio::test]
async fn dry_run_leaves_an_organization_access_home_alone() {
    let server = MockServer::start().await;
    mount_reads(&server).await;
    mount_actions_access(&server, "organization").await;
    Command::cargo_bin("navigator")
        .unwrap()
        .args([
            "ops",
            "github",
            "setup",
            "neon-law/navigator",
            "--dry-run",
            "--workflow-home",
            "neon-law/navigator",
        ])
        .env("GITHUB_TOKEN", "test-token")
        .env_remove("GITHUB_REPOSITORY")
        .env("NAVIGATOR_GITHUB_API_BASE", server.uri())
        .assert()
        .success()
        .stderr(predicates::str::contains("Actions access").not());
}

async fn mount_actions_access(server: &MockServer, level: &str) {
    Mock::given(method("GET"))
        .and(path("/repos/neon-law/navigator/actions/permissions/access"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "access_level": level })),
        )
        .mount(server)
        .await;
}

async fn mount_reads(server: &MockServer) {
    // The required-check rule is bound to the Actions App id, and the host is
    // the only authority on what that id is, so the reconcile reads it before
    // it plans. Without this the dry run stops on a 404 rather than printing a
    // plan — deliberately, because a guessed id writes a gate that does not
    // gate.
    Mock::given(method("GET"))
        .and(path("/apps/github-actions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 4242})))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/neon-law/navigator"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "allow_squash_merge": true,
            "allow_merge_commit": false,
            "allow_rebase_merge": false,
            "allow_auto_merge": true,
            "delete_branch_on_merge": true,
            "squash_merge_commit_title": "PR_TITLE",
            "squash_merge_commit_message": "PR_BODY",
            "pull_request_creation_policy": "collaborators_only",
            "has_issues": false,
            "has_projects": false,
            "has_wiki": false,
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/repos/neon-law/navigator/contents/.github/CODEOWNERS",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string("* @owner\n"))
        .mount(server)
        .await;
    // Every owner the file names is resolved against the host before the review
    // gate is written, because GitHub silently ignores one that does not exist.
    Mock::given(method("GET"))
        .and(path("/users/owner"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 42})))
        .mount(server)
        .await;
    // Existing is not owning: GitHub honors a code owner only where that owner
    // can write, so the reconcile asks this too.
    Mock::given(method("GET"))
        .and(path(
            "/repos/neon-law/navigator/collaborators/owner/permission",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"permission": "admin"})),
        )
        .mount(server)
        .await;
    // The required `ci` context is only bound to a repository whose workflow
    // actually defines that job.
    Mock::given(method("GET"))
        .and(path(
            "/repos/neon-law/navigator/contents/.github/workflows/ci.yml",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "jobs:\n  rust:\n    name: cargo test (workspace)\n  ci:\n    name: ci\n",
        ))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/neon-law/navigator/rulesets"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id":7,"name":"production"},
            {"id":8,"name":"production-review"}
        ])))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/neon-law/navigator/rulesets/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "name": "production",
            "target": "branch",
            "enforcement": "active",
            "bypass_actors": [],
            "conditions": {"ref_name": {"exclude": [], "include": ["~DEFAULT_BRANCH"]}},
            "rules": []
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/neon-law/navigator/rulesets/8"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "name": "production-review",
            "target": "branch",
            "enforcement": "active",
            "bypass_actors": [],
            "conditions": {"ref_name": {"exclude": [], "include": ["~DEFAULT_BRANCH"]}},
            "rules": [{"type": "pull_request", "parameters": {"required_approving_review_count": 1}}]
        })))
        .mount(server)
        .await;
    // NAVIGATOR_POLICY carries no labels, so `reconcile` never calls
    // `GET .../labels` at all — no mock is mounted for it.
}
