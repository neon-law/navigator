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
pub async fn run(dir: &Path, host: Option<&str>) -> ExitCode {
    match run_inner(dir, host).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator project repository repair-mirror: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run_inner(dir: &Path, host: Option<&str>) -> Result<()> {
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
    let mirror = request(&base, &token, project_id, &owner, &name).await?;
    let source = &mirror.source;
    let verified = &mirror.url;
    let outcome = mirror.outcome;

    let existing = super::lifecycle::git(dir, &["remote", "get-url", "forgejo"])?;
    let existing = existing
        .status
        .success()
        .then(|| String::from_utf8_lossy(&existing.stdout).trim().to_string());
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
    let response = crate::remote::authenticated_client_builder(base)?
        .timeout(std::time::Duration::from_secs(30))
        .build()?
        .post(format!(
            "{base}/app/api/project-forgejo-mirrors/{project_id}"
        ))
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_LENGTH, "0")
        .body(Vec::<u8>::new())
        .send()
        .await
        .context("request Forgejo mirror repair")?;
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap_or_default();
    ensure!(
        status.is_success(),
        "Forgejo mirror repair refused: {status}: {}",
        body["message"]
            .as_str()
            .or_else(|| body["error"].as_str())
            .unwrap_or_default()
    );
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
    let verified = body["repository_url"]
        .as_str()
        .context("mirror repair response has no verified Forgejo URL")?;
    let forgejo_owner = body["forgejo_owner"]
        .as_str()
        .context("mirror repair response has no verified Forgejo organization")?;
    let verified = verified_mirror_url(verified, forgejo_owner, name)?;
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

#[cfg(test)]
mod tests {
    use super::{remote_action, verified_mirror_url, RemoteAction};

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
