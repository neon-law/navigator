//! Complete a single Project's repository and local Jujutsu workspace.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use serde::Deserialize;
use uuid::Uuid;

use super::forge::Forge;
use super::{manifest, mirror_repair, repository, set, surfaces, watch};

const MAIN: &str = "refs/heads/main";

#[derive(Deserialize)]
struct Project {
    code: String,
    status: String,
    firm_id: Option<Uuid>,
    brand: String,
    repository_remotes: Remotes,
}

#[derive(Deserialize)]
struct Remotes {
    origin: Option<String>,
    forgejo: Option<String>,
    forgejo_status: String,
}

async fn get(base: &str, token: &str, id: Uuid) -> Result<Project> {
    let response = crate::remote::authenticated_client_builder(base)?
        .timeout(Duration::from_secs(30))
        .build()?
        .get(format!("{base}/app/api/projects/{id}"))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()
        .context("read the Project's repository inventory")?;
    response
        .json()
        .await
        .context("parse Project repository inventory")
}

/// A finished setup has an initialized main, the repository's normal forge
/// policy, an exact secondary main where needed, and a usable local checkout.
#[allow(clippy::too_many_lines)] // Verify each step before advancing the same Project.
pub(super) async fn finish(base: &str, token: &str, id: Uuid, code: &str) -> Result<bool> {
    let project = get(base, token, id).await?;
    ensure!(
        project.code == code,
        "Project identity changed; retry setup"
    );
    if !matches!(project.status.as_str(), "pitch" | "open") {
        return Ok(false);
    }
    let origin = project
        .repository_remotes
        .origin
        .as_deref()
        .context("source repository is not linked")?;
    validate_transport(origin)?;
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    let github = super::lifecycle::github_origin(root, origin).await;
    let mut verified_manifest;
    let mut forgejo_run = None;
    if let Some((owner, name)) = github {
        let target =
            crate::github_setup::RepositoryTarget::resolve(Some(format!("{owner}/{name}")))?;
        crate::github_setup::verify_private_project(&target).await?;
        let main = match main_ref(root, origin).await? {
            Some(main) => main,
            None => bootstrap_github(root, base, token, id, code, origin).await?,
        };
        verified_manifest = verify_project_main(root, origin, &main, base, code).await?;
        let current = get(base, token, id).await?;
        ensure!(
            current.repository_remotes.origin.as_deref() == Some(origin),
            "Project repository authority changed; retry setup"
        );
        crate::github_setup::protect_project(&target)
            .await
            .context("GitHub branch protection and required check are incomplete; an authorized repository administrator can rerun setup")?;
        let current = get(base, token, id).await?.repository_remotes;
        ensure!(
            current.origin.as_deref() == Some(origin),
            "Project repository authority changed; retry setup"
        );
        let mirror = match (current.forgejo_status.as_str(), current.forgejo) {
            ("verified", Some(mirror)) => mirror,
            ("missing", _) => {
                mirror_repair::request(base, token, id, &owner, &name)
                    .await?
                    .url
            }
            (status, _) => {
                bail!("Forgejo mirror is {status}; verify provider access before retrying setup")
            }
        };
        let checked = get(base, token, id).await?.repository_remotes;
        ensure!(
            checked.origin.as_deref() == Some(origin)
                && checked.forgejo.as_deref() == Some(&mirror),
            "Project repository coordinates changed; retry setup"
        );
        validate_transport(&mirror)?;
        let mirrored = sync_mirror(root, origin, &mirror).await?;
        if mirrored != main {
            verified_manifest = verify_project_main(root, origin, &mirrored, base, code).await?;
        }
    } else {
        // The server's existing Forgejo reconcile owns initialization and
        // governance. It also refuses repositories outside the Firm namespace.
        let main = main_ref(root, origin).await?.context(
            "authoritative repository has no main; finish source provisioning and retry setup",
        )?;
        verified_manifest = verify_project_main(root, origin, &main, base, code).await?;
        git(root, &["remote", "add", "origin", origin]).await?;
        let forge = super::forge_forgejo::ForgejoForge::from_checkout(root).context(
            "Forgejo primary and its review policy are unverified; confirm the configured Forgejo host and authorized credential"
        )?;
        forge
            .verify_workspace()
            .await
            .context("Forgejo primary review protection or Actions is incomplete")?;
        forgejo_run = Some((forge, main));
    }
    watch::sync_project(Some(base), code)
        .await
        .context("local Jujutsu checkout is incomplete")?;
    verified_manifest
        .check_live_ownership(project.firm_id, Some(&project.brand))
        .map_err(anyhow::Error::msg)?;
    let handles = surfaces::get_handles(base, token, id).await?;
    super::gate::check_manifest_handles(&verified_manifest, &handles).context(
        "checkout is available; reconcile the manifest with project set --from-live and land the change through normal review before retrying setup"
    )?;
    if let Some((forge, main)) = forgejo_run {
        let run = forge
            .deployment(&main)
            .await
            .context("check the Forgejo Actions run for current main")?;
        ensure!(
            run.is_some_and(|run| run.conclusion.as_deref() == Some("success")),
            "checkout and review protection are ready; the Forgejo Actions run for current main is pending or unsuccessful"
        );
    }
    Ok(true)
}

/// Report observed source and checkout state without writing local or remote state.
pub(super) async fn inspect(status: &serde_json::Value) -> serde_json::Value {
    let remotes = &status["repository_remotes"];
    let code = status["code"]
        .as_str()
        .filter(|code| model::projects::is_valid_code(code));
    let checkout = std::env::home_dir()
        .zip(code)
        .map(|(home, code)| home.join("Work").join(code));
    let cwd = Path::new(".");
    let (source, mirror) = if let Some(origin) = remotes["origin"].as_str() {
        let refs = tokio::time::timeout(Duration::from_secs(10), async {
            let source = observed_main(cwd, origin).await;
            let mirror = match remotes["forgejo"].as_str() {
                Some(mirror) => observed_main(cwd, mirror).await,
                None => Err(remotes["forgejo_status"].as_str().unwrap_or("unverified")),
            };
            (source, mirror)
        })
        .await
        .unwrap_or((
            Err("unverified: source check timed out"),
            Err("unverified: mirror check timed out"),
        ));
        let matching = match (&refs.0, &refs.1) {
            (Ok(Some(source)), Ok(Some(mirror))) if source == mirror => "matches source main",
            (Ok(Some(_)), Ok(Some(_))) => "differs from source main; rerun setup",
            (_, Ok(None)) => "main is empty; rerun setup",
            (_, Err(detail)) => detail,
            (Err(_) | Ok(None), Ok(Some(_))) => "unverified: source main is unavailable",
        };
        let source = match refs.0 {
            Ok(Some(_)) => "main is present",
            Ok(None) => "main is empty; rerun setup",
            Err(detail) => detail,
        };
        (source, matching)
    } else {
        (
            "repository is not linked; rerun setup",
            "waiting for the source repository",
        )
    };
    let configured = checkout.is_some_and(|path| {
        std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir())
            && path.join(".jj").is_dir()
            && manifest::read(&path).is_some_and(|manifest| manifest.project.as_deref() == code)
    });
    serde_json::json!({
        "source": source,
        "mirror": mirror,
        "branch_protection": "run single-Project setup to verify",
        "checkout": if configured { "Jujutsu checkout is present; setup verifies its remotes" } else { "missing or incomplete; rerun setup" }
    })
}

async fn observed_main(
    cwd: &Path,
    remote: &str,
) -> std::result::Result<Option<String>, &'static str> {
    validate_transport(remote).map_err(|_| "unverified: unsupported repository transport")?;
    main_ref(cwd, remote)
        .await
        .map_err(|_| "unverified: repository could not be read")
}

fn validate_transport(remote: &str) -> Result<()> {
    ensure!(
        remote.starts_with("https://")
            || remote.starts_with("ssh://")
            || remote.starts_with("git@")
            || url::Url::parse(remote).is_ok_and(|url| {
                url.scheme() == "http" && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
            }),
        "workspace requires a supported repository transport"
    );
    Ok(())
}

async fn git(dir: &Path, args: &[&str]) -> Result<String> {
    tokio::time::timeout(Duration::from_secs(60), watch::command(dir, "git", args))
        .await
        .context("repository check timed out")?
}

async fn main_ref(dir: &Path, remote: &str) -> Result<Option<String>> {
    let output = git(dir, &["ls-remote", "--refs", "--", remote, MAIN]).await?;
    if output.trim().is_empty() {
        return Ok(None);
    }
    let (sha, reference) = output
        .trim()
        .split_once('\t')
        .context("malformed remote main response")?;
    ensure!(
        reference == MAIN
            && matches!(sha.len(), 40 | 64)
            && sha.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid remote main response"
    );
    Ok(Some(sha.to_owned()))
}

async fn check_empty(dir: &Path, origin: &str) -> Result<()> {
    ensure!(
        git(dir, &["ls-remote", "--refs", "--", origin])
            .await?
            .trim()
            .is_empty(),
        "source repository already has refs but no main; review its history before retrying"
    );
    Ok(())
}

async fn bootstrap_github(
    dir: &Path,
    base: &str,
    token: &str,
    id: Uuid,
    code: &str,
    origin: &str,
) -> Result<String> {
    check_empty(dir, origin).await?;
    let handles = surfaces::get_handles(base, token, id).await?;
    ensure!(
        handles.code == code && handles.repository_url.as_deref() == Some(origin),
        "Project repository authority changed; retry setup"
    );
    let lawyer = handles
        .lawyer_dri
        .first()
        .context("Project needs a lawyer DRI")?;
    let client = handles
        .client_dri
        .first()
        .context("Project needs a client DRI")?;
    let host = url::Url::parse(base)?;
    let authority = host.authority();
    ensure!(!authority.is_empty(), "deployment URL has no host");
    let manifest_host = if host.scheme() == "http" {
        base
    } else {
        authority
    };
    repository::emit_scaffold_for(
        dir,
        code,
        manifest_host,
        lawyer,
        client,
        model::forge::ForgeBackend::GitHub,
    )?;
    set::set_at(
        dir,
        set::SetRequest {
            host_override: Some(base),
            project_override: Some(code),
            from_live: true,
            supplied: [None; 4],
            live_fields: set::LiveFields::default(),
            firm: None,
            brand: None,
            dry_run: false,
        },
    )
    .await
    .context("fill the initial scaffold from the live Project")?;
    let gate = repository::validate_gate(dir, Some(code), false);
    ensure!(
        gate.errors == 0,
        "canonical Project scaffold failed its gate"
    );
    git(dir, &["add", "--all"]).await?;
    git(
        dir,
        &["commit", "-S", "-m", "Initialize canonical Project source"],
    )
    .await
    .context("sign the initial Project commit")?;
    let sha = git(dir, &["rev-parse", "HEAD"]).await?;
    let sha = sha.trim();
    check_empty(dir, origin).await?;
    let current = get(base, token, id).await?;
    ensure!(
        current.repository_remotes.origin.as_deref() == Some(origin)
            && matches!(current.status.as_str(), "pitch" | "open"),
        "Project repository authority or lifecycle changed; retry setup"
    );
    // An empty expected ref prevents any overwrite, even if another setup
    // initializes main between the read and the push.
    git(
        dir,
        &[
            "push",
            "--force-with-lease=refs/heads/main:",
            "--no-follow-tags",
            "--",
            origin,
            &format!("{sha}:{MAIN}"),
        ],
    )
    .await
    .context("push the signed initial Project main")?;
    main_ref(dir, origin)
        .await?
        .context("initial main was not visible after push")
}

async fn verify_project_main(
    dir: &Path,
    origin: &str,
    main: &str,
    base: &str,
    code: &str,
) -> Result<manifest::Manifest> {
    git(dir, &["init", "--quiet"]).await?;
    git(
        dir,
        &[
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--",
            origin,
            main,
        ],
    )
    .await?;
    let raw = git(dir, &["show", &format!("{main}:navigator.yaml")])
        .await
        .context("authoritative main has no canonical Project manifest")?;
    let manifest = manifest::parse(&raw).map_err(anyhow::Error::msg)?;
    ensure!(
        manifest.project.as_deref() == Some(code)
            && manifest
                .host
                .as_deref()
                .is_some_and(|host| crate::credentials::base_url(host) == base),
        "authoritative main names a different Project or deployment"
    );
    ensure!(
        manifest.version.is_some(),
        "authoritative main has no pinned Navigator version"
    );
    Ok(manifest)
}

async fn sync_mirror(dir: &Path, origin: &str, mirror: &str) -> Result<String> {
    ensure!(
        origin != mirror,
        "Forgejo mirror must be a separate repository"
    );
    let main = main_ref(dir, origin)
        .await?
        .context("authoritative main is missing")?;
    if main_ref(dir, mirror).await?.as_deref() != Some(&main) {
        git(
            dir,
            &[
                "fetch",
                "--no-tags",
                "--no-write-fetch-head",
                "--",
                origin,
                &main,
            ],
        )
        .await?;
        git(
            dir,
            &[
                "push",
                "--no-force",
                "--no-follow-tags",
                "--",
                mirror,
                &format!("{main}:{MAIN}"),
            ],
        )
        .await
        .context("fast-forward Forgejo from the authoritative GitHub main")?;
    }
    ensure!(
        main_ref(dir, origin).await?.as_deref() == Some(&main)
            && main_ref(dir, mirror).await?.as_deref() == Some(&main),
        "Forgejo main does not match the authoritative main; retry setup"
    );
    Ok(main)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_empty_mirror_catches_up_on_retry_without_rewriting_main() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path();
        for name in ["source", "mirror"] {
            git(root, &["init", "--bare", name]).await?;
        }
        let source = root.join("source");
        let mirror = root.join("mirror");
        let source = source.to_str().unwrap();
        let mirror = mirror.to_str().unwrap();
        git(root, &["init", "--initial-branch=main"]).await?;
        assert!(main_ref(root, source).await?.is_none());
        git(
            root,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--no-gpg-sign",
                "--allow-empty",
                "-m",
                "Initial source",
            ],
        )
        .await?;
        git(root, &["push", "--", source, "main"]).await?;
        assert!(check_empty(root, source).await.is_err());
        assert!(main_ref(root, mirror).await?.is_none());
        sync_mirror(root, source, mirror).await?;
        let main = main_ref(root, source).await?;
        assert_eq!(main_ref(root, mirror).await?, main);
        sync_mirror(root, source, mirror).await?;
        assert_eq!(main_ref(root, mirror).await?, main);
        git(
            root,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--no-gpg-sign",
                "--allow-empty",
                "-m",
                "Mirror-only change",
            ],
        )
        .await?;
        git(root, &["push", "--", mirror, "main"]).await?;
        let divergent = main_ref(root, mirror).await?;
        assert!(sync_mirror(root, source, mirror).await.is_err());
        assert_eq!(main_ref(root, mirror).await?, divergent);
        assert_eq!(main_ref(root, source).await?, main);
        Ok(())
    }
}
