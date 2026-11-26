//! Provision and configure the secondary Forgejo remote for a GitHub Project.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{anyhow, bail, ensure, Context, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteAction {
    Add,
    AlreadyConfigured,
}

fn remote_action(existing: Option<&str>, verified: &str) -> Result<RemoteAction> {
    match existing {
        None => Ok(RemoteAction::Add),
        Some(existing) if existing == verified => Ok(RemoteAction::AlreadyConfigured),
        Some(_) => {
            bail!("the checkout already has a different forgejo remote; inspect it before retrying")
        }
    }
}

fn verified_mirror_url<'a>(value: &'a str, owner: &str, project: &str) -> Result<&'a str> {
    let parsed = url::Url::parse(value).context("Forgejo returned an invalid repository URL")?;
    let host = parsed
        .host_str()
        .context("Forgejo returned a URL without a host")?;
    let internal_http =
        host == "localhost" || host == "127.0.0.1" || host.ends_with(".svc.cluster.local");
    let path = parsed.path().trim_end_matches('/');
    let repository_path = format!("/{owner}/{project}");
    ensure!(
        (parsed.scheme() == "https" || (parsed.scheme() == "http" && internal_http))
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.query().is_none()
            && parsed.fragment().is_none()
            && (path.ends_with(&repository_path)
                || path.ends_with(&format!("{repository_path}.git"))),
        "Forgejo returned a URL that does not identify the verified Project repository"
    );
    Ok(value)
}

/// `navigator project repository repair-mirror --dir <checkout> [--host <site>]`.
pub async fn run(dir: &Path, host: Option<&str>, dry_run: bool) -> ExitCode {
    match run_inner(dir, host, dry_run).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator project repository repair-mirror: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run_inner(dir: &Path, host: Option<&str>, dry_run: bool) -> Result<()> {
    let manifest = super::manifest::read(dir)
        .ok_or_else(|| anyhow!("run from a Project checkout with navigator.yaml"))?;
    let code = manifest
        .project
        .as_deref()
        .context("navigator.yaml has no Project code")?;
    let host = host.or(manifest.host.as_deref());
    let origin = super::lifecycle::git_text(dir, &["remote", "get-url", "origin"])?;
    let (owner, name) = super::lifecycle::github_origin(dir, &origin)
        .await
        .context("the checkout origin must be its GitHub source repository")?;
    ensure!(
        name == code,
        "the checkout origin does not match its Project code"
    );

    let (base, token) = crate::remote::resolve(host)?;
    let project_id = super::surfaces::resolve_project_id(&base, &token, code).await?;
    let existing = super::lifecycle::git(dir, &["remote", "get-url", "forgejo"])?;
    let existing = existing
        .status
        .success()
        .then(|| String::from_utf8_lossy(&existing.stdout).trim().to_string());
    if dry_run {
        let body = call(&base, &token, project_id, reqwest::Method::GET).await?;
        let source = source_url(&body, &owner, &name)?;
        println!("GitHub source: {source}");
        match body["mirror_status"].as_str() {
            Some("missing") if existing.is_none() => {
                println!(
                    "Forgejo mirror is missing; repair would create it and configure the remote."
                );
            }
            Some("missing") => {
                bail!("Forgejo mirror is missing but a local Forgejo remote is configured; verify its repository before repair");
            }
            Some("present") => {
                let verified = mirror_url(&body, &name)?;
                match remote_action(existing.as_deref(), verified)? {
                    RemoteAction::Add => {
                        println!("Repair would configure the verified Forgejo remote.");
                    }
                    RemoteAction::AlreadyConfigured => {
                        println!("The verified Forgejo remote is already configured.");
                    }
                }
                println!("Forgejo mirror: {verified}");
            }
            _ => bail!("mirror repair preview has no known outcome"),
        }
        return Ok(());
    }
    let mirror = request(&base, &token, project_id, &owner, &name).await?;
    let source = &mirror.source;
    let verified = &mirror.url;
    let outcome = mirror.outcome;
    match remote_action(existing.as_deref(), verified)? {
        RemoteAction::Add => {
            super::lifecycle::git_text(dir, &["remote", "add", "forgejo", verified])
                .context("configure the Forgejo remote")?;
            println!("Forgejo mirror {outcome} and remote configured.");
        }
        RemoteAction::AlreadyConfigured => {
            println!("Forgejo mirror is present and its verified remote is configured.");
        }
    }
    println!("GitHub source: {source}");
    println!("Forgejo mirror: {verified}");
    println!("GitHub remains the source and CI/CD provider.");
    Ok(())
}

pub(super) struct Mirror {
    source: String,
    pub url: String,
    outcome: &'static str,
}

pub(super) async fn request(
    base: &str,
    token: &str,
    project_id: uuid::Uuid,
    owner: &str,
    name: &str,
) -> Result<Mirror> {
    let body = call(base, token, project_id, reqwest::Method::POST).await?;
    let source = source_url(&body, owner, name)?;
    let verified = mirror_url(&body, name)?;
    let outcome = match body["mirror_status"].as_str() {
        Some("created") => "created",
        Some("present") => "adopted",
        _ => bail!("mirror repair response has no known outcome"),
    };
    Ok(Mirror {
        source: source.to_owned(),
        url: verified.to_owned(),
        outcome,
    })
}

async fn call(
    base: &str,
    token: &str,
    project_id: uuid::Uuid,
    method: reqwest::Method,
) -> Result<serde_json::Value> {
    let client = crate::remote::authenticated_client_builder(base)?
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let request = client
        .request(
            method.clone(),
            format!("{base}/app/api/project-forgejo-mirrors/{project_id}"),
        )
        .bearer_auth(token);
    let request = if method == reqwest::Method::POST {
        request
            .header(reqwest::header::CONTENT_LENGTH, "0")
            .body(Vec::<u8>::new())
    } else {
        request
    };
    let response = request
        .send()
        .await
        .context("request Forgejo mirror repair")?;
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap_or_default();
    let detail = issue_detail(&body);
    ensure!(
        status.is_success(),
        "Forgejo mirror repair refused: {status}: {}",
        detail
            .as_deref()
            .or_else(|| body["error"].as_str())
            .unwrap_or_default()
    );
    Ok(body)
}

fn source_url<'a>(body: &'a serde_json::Value, owner: &str, name: &str) -> Result<&'a str> {
    ensure!(
        body["ci_provider"].as_str() == Some("github"),
        "the Project is not configured for GitHub CI/CD"
    );
    let source = body["source_repository_url"]
        .as_str()
        .context("mirror repair response has no GitHub source URL")?;
    let (source_owner, source_name) = super::lifecycle::github_owner_name(source)?;
    ensure!(
        source_owner.eq_ignore_ascii_case(owner) && source_name == name,
        "the checkout origin does not match the Project's GitHub source"
    );
    Ok(source)
}

fn mirror_url<'a>(body: &'a serde_json::Value, name: &str) -> Result<&'a str> {
    let verified = body["repository_url"]
        .as_str()
        .context("mirror repair response has no verified Forgejo URL")?;
    let forgejo_owner = body["forgejo_owner"]
        .as_str()
        .context("mirror repair response has no verified Forgejo organization")?;
    verified_mirror_url(verified, forgejo_owner, name)
}

pub(super) fn issue_detail(issue: &serde_json::Value) -> Option<String> {
    let message = issue["message"].as_str()?;
    Some(match issue["remedy"].as_str() {
        Some(remedy) => format!("{message} {remedy}"),
        None => message.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::{remote_action, request, run, verified_mirror_url, RemoteAction};
    use crate::projects::test_login::{answer, checkout, site};
    use serde_json::json;
    use std::process::ExitCode;

    const MANIFEST: &str = "project:\n  name: sample-matter\n  host: site.example.test\n";
    const ORIGIN: &str = "https://github.com/sample-firm/sample-matter";
    const MIRROR: &str = "https://forge.example.test/sample-firm/sample-matter";

    fn repaired(status: &str) -> serde_json::Value {
        json!({
            "ci_provider": "github",
            "source_repository_url": ORIGIN,
            "repository_url": MIRROR,
            "forgejo_owner": "sample-firm",
            "mirror_status": status,
        })
    }

    #[tokio::test]
    async fn repair_adds_the_verified_remote_once_and_refuses_a_different_one() {
        let id = uuid::Uuid::new_v4();
        let (server, _login) = site(json!([{"id": id, "code": "sample-matter"}])).await;
        let door = format!("/app/api/project-forgejo-mirrors/{id}");
        answer(&server, "POST", &door, 200, repaired("created")).await;
        let dir = checkout(MANIFEST, Some(ORIGIN));
        let host = server.uri();
        let remote =
            || crate::projects::lifecycle::git_text(dir.path(), &["remote", "get-url", "forgejo"]);

        assert_eq!(run(dir.path(), Some(&host), false).await, ExitCode::SUCCESS);
        assert_eq!(remote().unwrap(), MIRROR);
        assert_eq!(run(dir.path(), Some(&host), false).await, ExitCode::SUCCESS);

        let other = "https://forge.example.test/other/sample-matter";
        crate::projects::lifecycle::git_text(dir.path(), &["remote", "set-url", "forgejo", other])
            .unwrap();
        assert_eq!(run(dir.path(), Some(&host), false).await, ExitCode::FAILURE);
        assert_eq!(remote().unwrap(), other);
    }

    #[tokio::test]
    async fn repair_preview_reads_provider_status_without_writing_a_remote() {
        let id = uuid::Uuid::new_v4();
        let (server, _login) = site(json!([{"id": id, "code": "sample-matter"}])).await;
        let door = format!("/app/api/project-forgejo-mirrors/{id}");
        answer(&server, "GET", &door, 200, repaired("present")).await;
        let dir = checkout(MANIFEST, Some(ORIGIN));
        let host = server.uri();
        assert_eq!(run(dir.path(), Some(&host), true).await, ExitCode::SUCCESS);
        assert!(crate::projects::lifecycle::git_text(
            dir.path(),
            &["remote", "get-url", "forgejo"]
        )
        .is_err());
        let requests = server.received_requests().await.unwrap();
        assert!(requests
            .iter()
            .all(|request| request.method == reqwest::Method::GET));
    }

    #[tokio::test]
    async fn repair_needs_a_project_checkout_whose_origin_is_its_github_source() {
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(run(empty.path(), None, false).await, ExitCode::FAILURE);
        for (manifest, origin, expected) in [
            ("host: site.example.test\n", ORIGIN, "no Project code"),
            (
                MANIFEST,
                "https://forge.example.test/sample-firm/sample-matter",
                "must be its GitHub source repository",
            ),
            (
                MANIFEST,
                "https://github.com/sample-firm/another-matter",
                "does not match its Project code",
            ),
        ] {
            let dir = checkout(manifest, Some(origin));
            let error = super::run_inner(dir.path(), None, false).await.unwrap_err();
            assert!(format!("{error:#}").contains(expected), "{error:#}");
        }
    }

    #[tokio::test]
    async fn a_repair_answer_must_name_this_github_source_and_a_known_outcome() {
        let server = wiremock::MockServer::start().await;
        let cases = [
            (
                403,
                json!({"message": "not an admin"}),
                "refused: 403 Forbidden: not an admin",
            ),
            (
                503,
                json!({"message": "Forgejo denied access.", "remedy": "Update the Firm token."}),
                "Forgejo denied access. Update the Firm token.",
            ),
            (
                200,
                json!({"ci_provider": "forgejo"}),
                "not configured for GitHub CI/CD",
            ),
            (
                200,
                json!({"ci_provider": "github"}),
                "no GitHub source URL",
            ),
            (
                200,
                json!({"ci_provider": "github", "source_repository_url": "https://github.com/other/sample-matter"}),
                "does not match the Project's GitHub source",
            ),
            (
                200,
                json!({"ci_provider": "github", "source_repository_url": ORIGIN}),
                "no verified Forgejo URL",
            ),
            (
                200,
                json!({"ci_provider": "github", "source_repository_url": ORIGIN, "repository_url": MIRROR}),
                "no verified Forgejo organization",
            ),
            (200, repaired("unknown"), "no known outcome"),
        ];
        for (status, body, expected) in cases {
            let id = uuid::Uuid::new_v4();
            let door = format!("/app/api/project-forgejo-mirrors/{id}");
            answer(&server, "POST", &door, status, body).await;
            let error = request(&server.uri(), "t", id, "sample-firm", "sample-matter")
                .await
                .err()
                .unwrap();
            assert!(error.to_string().contains(expected), "{error}");
        }
        let id = uuid::Uuid::new_v4();
        let door = format!("/app/api/project-forgejo-mirrors/{id}");
        answer(&server, "POST", &door, 200, repaired("present")).await;
        let mirror = request(&server.uri(), "t", id, "Sample-Firm", "sample-matter")
            .await
            .unwrap();
        assert_eq!((mirror.url.as_str(), mirror.outcome), (MIRROR, "adopted"));
    }

    #[test]
    fn mirror_repair_adds_only_a_missing_or_matching_remote() {
        assert_eq!(
            remote_action(None, "https://forge.example.test/firm/project").unwrap(),
            RemoteAction::Add
        );
        assert_eq!(
            remote_action(
                Some("https://forge.example.test/firm/project"),
                "https://forge.example.test/firm/project"
            )
            .unwrap(),
            RemoteAction::AlreadyConfigured
        );
        assert!(remote_action(
            Some("https://forge.example.test/other/project"),
            "https://forge.example.test/firm/project"
        )
        .is_err());
    }

    #[test]
    fn verified_mirror_url_must_match_the_project_and_use_a_safe_origin() {
        assert!(
            verified_mirror_url("https://forge.example.test/firm/project", "firm", "project")
                .is_ok()
        );
        assert!(
            verified_mirror_url("http://127.0.0.1:3000/firm/project", "firm", "project").is_ok()
        );
        assert!(verified_mirror_url(
            "https://forge.example.test/other/project",
            "firm",
            "project"
        )
        .is_err());
        assert!(
            verified_mirror_url("http://forge.example.test/firm/project", "firm", "project")
                .is_err()
        );
    }
}
