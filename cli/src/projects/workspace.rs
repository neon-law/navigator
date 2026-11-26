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
pub(super) struct Remotes {
    pub(super) origin: Option<String>,
    pub(super) forgejo: Option<String>,
    pub(super) forgejo_status: String,
    #[serde(default)]
    pub(super) forgejo_issue: serde_json::Value,
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

/// Check the selected checkout against the signed-in site's Project binding.
pub(super) async fn verify_checkout_binding(dir: &Path, origin: &str) -> Result<()> {
    ensure!(
        checkout_remotes(dir).await?.1.origin.as_deref() == Some(origin),
        "checkout origin must match the Project's recorded source URL"
    );
    Ok(())
}

pub(super) async fn checkout_remotes(dir: &Path) -> Result<(String, Remotes)> {
    let manifest = manifest::read(dir).context("this command needs a Project manifest")?;
    let host = manifest.host.as_deref().context("manifest needs a host")?;
    let code = manifest
        .project
        .as_deref()
        .context("manifest needs a Project code")?;
    let (base, token) = crate::remote::resolve(Some(host))?;
    let id = surfaces::resolve_project_id(&base, &token, code).await?;
    let project = get(&base, &token, id).await?;
    manifest
        .check_live_ownership(project.firm_id, Some(&project.brand))
        .map_err(anyhow::Error::msg)?;
    ensure!(
        project.code == code,
        "checkout must match the recorded Project code"
    );
    Ok((project.code, project.repository_remotes))
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
            current.repository_remotes.origin.as_deref() == Some(origin)
                && current.firm_id == project.firm_id
                && current.brand == project.brand
                && matches!(current.status.as_str(), "pitch" | "open"),
            "Project repository authority, ownership, or lifecycle changed; retry setup"
        );
        verified_manifest
            .check_live_ownership(current.firm_id, Some(&current.brand))
            .map_err(anyhow::Error::msg)?;
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
    watch::sync_project(Some(base), code, None)
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
    let checkout = super::workspace_root::selected(None)
        .ok()
        .zip(code)
        .map(|(root, code)| root.join(code));
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
        describe(&refs.0, &refs.1)
    } else {
        (
            "repository is not linked; rerun setup",
            "waiting for the source repository",
        )
    };
    let configured = checkout.filter(|path| {
        std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
            && path.join(".jj").is_dir()
            && manifest::read(path).is_some_and(|manifest| manifest.project.as_deref() == code)
    });
    let checkout_summary = if let Some(checkout) = configured {
        match watch::checkout_commit(&checkout, "main@origin ~ ::parents(@)").await {
            Ok(revisions) if revisions.is_empty() => {
                "Jujutsu checkout is current with fetched main".into()
            }
            Ok(revisions) => format!(
                "Jujutsu checkout is behind by {} from fetched main",
                revisions.lines().count()
            ),
            Err(_) => "Jujutsu checkout is present; fetched main is unavailable".into(),
        }
    } else {
        "missing or incomplete; rerun setup".into()
    };
    serde_json::json!({
        "source": source,
        "mirror": mirror,
        "branch_protection": "run single-Project setup to verify",
        "checkout": checkout_summary
    })
}

type Observed<'a> = std::result::Result<Option<String>, &'a str>;

/// Word the source and mirror state from what each remote's main answered.
fn describe<'a>(source: &Observed<'a>, mirror: &Observed<'a>) -> (&'a str, &'a str) {
    let matching = match (source, mirror) {
        (Ok(Some(source)), Ok(Some(mirror))) if source == mirror => "matches source main",
        (Ok(Some(_)), Ok(Some(_))) => "differs from source main; rerun setup",
        (_, Ok(None)) => "main is empty; rerun setup",
        (_, Err(detail)) => detail,
        (Err(_) | Ok(None), Ok(Some(_))) => "unverified: source main is unavailable",
    };
    let source = match source {
        Ok(Some(_)) => "main is present",
        Ok(None) => "main is empty; rerun setup",
        Err(detail) => detail,
    };
    (source, matching)
}

async fn observed_main(cwd: &Path, remote: &str) -> Observed<'static> {
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

pub(super) async fn git(dir: &Path, args: &[&str]) -> Result<String> {
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
            apply_legal_plan: false,
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

    #[test]
    fn a_new_github_workspace_includes_its_required_review_files() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path();
        repository::emit_scaffold_for(
            root,
            "sample-workspace",
            "example.test",
            "lawyer@example.com",
            "client@example.com",
            model::forge::ForgeBackend::GitHub,
        )?;
        for path in [
            ".github/CODEOWNERS",
            ".github/workflows/ci.yml",
            ".github/workflows/cd.yml",
            ".github/workflows/automerge.yml",
        ] {
            assert!(root.join(path).is_file(), "new workspace needs {path}");
        }
        assert_eq!(
            repository::validate_gate(root, Some("sample-workspace"), false).errors,
            0
        );
        Ok(())
    }

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
        let error = sync_mirror(root, source, source).await.unwrap_err();
        assert!(error.to_string().contains("must be a separate repository"));
        Ok(())
    }

    #[test]
    fn source_and_mirror_state_is_worded_from_each_remotes_main() {
        let sha = |value: &str| Ok(Some(value.to_owned()));
        for (source, mirror, expected) in [
            (
                sha("a"),
                sha("a"),
                ("main is present", "matches source main"),
            ),
            (
                sha("a"),
                sha("b"),
                ("main is present", "differs from source main; rerun setup"),
            ),
            (
                sha("a"),
                Ok(None),
                ("main is present", "main is empty; rerun setup"),
            ),
            (sha("a"), Err("missing"), ("main is present", "missing")),
            (
                Ok(None),
                sha("b"),
                (
                    "main is empty; rerun setup",
                    "unverified: source main is unavailable",
                ),
            ),
            (
                Err("unverified: repository could not be read"),
                sha("b"),
                (
                    "unverified: repository could not be read",
                    "unverified: source main is unavailable",
                ),
            ),
        ] {
            assert_eq!(describe(&source, &mirror), expected);
        }
    }

    #[tokio::test]
    async fn inspection_reports_an_unlinked_or_unreadable_source_without_writing() {
        let unlinked = inspect(&serde_json::json!({"repository_remotes": {}})).await;
        assert_eq!(unlinked["source"], "repository is not linked; rerun setup");
        assert_eq!(unlinked["mirror"], "waiting for the source repository");
        assert_eq!(unlinked["checkout"], "missing or incomplete; rerun setup");

        let unsupported = inspect(&serde_json::json!({"repository_remotes": {
            "origin": "ftp://forge.example.test/firm/matter",
            "forgejo_status": "missing",
        }}))
        .await;
        assert_eq!(
            unsupported["source"],
            "unverified: unsupported repository transport"
        );
        assert_eq!(unsupported["mirror"], "missing");
    }

    #[test]
    fn only_secure_or_loopback_transports_are_supported() {
        for remote in [
            "https://forge.example.test/firm/matter",
            "ssh://git@forge.example.test/firm/matter",
            "git@forge.example.test:firm/matter",
            "http://localhost:3000/firm/matter",
            "http://127.0.0.1:3000/firm/matter",
        ] {
            assert!(validate_transport(remote).is_ok(), "{remote}");
        }
        for remote in [
            "http://forge.example.test/firm/matter",
            "/srv/git/matter",
            "file:///srv/git/matter",
        ] {
            assert!(validate_transport(remote).is_err(), "{remote}");
        }
    }

    const BASE: &str = "https://site.example.test";
    const PINNED: &str =
        "version: 26.9.14\nproject:\n  host: site.example.test\n  name: sample-matter\n";

    /// A bare source repository whose main holds `manifest`, with a scratch
    /// directory to verify it from.
    async fn source(manifest: Option<&str>) -> Result<(tempfile::TempDir, String, String)> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path();
        git(root, &["init", "--bare", "source"]).await?;
        let origin = root.join("source").to_str().unwrap().to_owned();
        let seed = root.join("seed");
        std::fs::create_dir(&seed)?;
        std::fs::create_dir(root.join("verify"))?;
        git(&seed, &["init", "--initial-branch=main"]).await?;
        if let Some(manifest) = manifest {
            std::fs::write(seed.join("navigator.yaml"), manifest)?;
            git(&seed, &["add", "navigator.yaml"]).await?;
        }
        git(
            &seed,
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
        git(&seed, &["push", "--", &origin, "main"]).await?;
        let main = main_ref(&seed, &origin).await?.context("pushed main")?;
        Ok((fixture, origin, main))
    }

    #[tokio::test]
    async fn authoritative_main_must_carry_this_projects_pinned_manifest() -> Result<()> {
        let (fixture, origin, main) = source(Some(PINNED)).await?;
        let dir = fixture.path().join("verify");
        let manifest = verify_project_main(&dir, &origin, &main, BASE, "sample-matter").await?;
        assert_eq!(manifest.version.as_deref(), Some("26.9.14"));
        for (base, code) in [
            (BASE, "another-matter"),
            ("https://other.example.test", "sample-matter"),
        ] {
            let error = verify_project_main(&dir, &origin, &main, base, code)
                .await
                .err()
                .unwrap();
            assert!(error
                .to_string()
                .contains("names a different Project or deployment"));
        }

        let unpinned = PINNED.replace("version: 26.9.14\n", "");
        for (manifest, expected) in [
            (Some(unpinned.as_str()), "no pinned Navigator version"),
            (None, "no canonical Project manifest"),
        ] {
            let (fixture, origin, main) = source(manifest).await?;
            let dir = fixture.path().join("verify");
            let error = verify_project_main(&dir, &origin, &main, BASE, "sample-matter")
                .await
                .err()
                .unwrap();
            assert!(error.to_string().contains(expected), "{error:#}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn setup_stops_before_any_repository_work_when_the_project_is_not_ready() {
        use crate::projects::test_login::answer;
        let server = wiremock::MockServer::start().await;
        let base = server.uri();
        let project = |code: &str, status: &str, origin: serde_json::Value| {
            serde_json::json!({
                "code": code, "status": status, "firm_id": null, "brand": "sample",
                "repository_remotes": {"origin": origin, "forgejo": null, "forgejo_status": "missing"},
            })
        };
        let null = serde_json::Value::Null;
        for (body, expected) in [
            (
                project("another-matter", "open", null.clone()),
                Err("Project identity changed; retry setup"),
            ),
            (project("sample-matter", "closed", null.clone()), Ok(false)),
            (
                project("sample-matter", "open", null),
                Err("source repository is not linked"),
            ),
            (
                project(
                    "sample-matter",
                    "pitch",
                    "ftp://forge.example.test/m".into(),
                ),
                Err("workspace requires a supported repository transport"),
            ),
        ] {
            let id = Uuid::new_v4();
            answer(
                &server,
                "GET",
                &format!("/app/api/projects/{id}"),
                200,
                body,
            )
            .await;
            let finished = finish(&base, "t", id, "sample-matter")
                .await
                .map_err(|error| error.to_string());
            assert_eq!(finished, expected.map_err(str::to_owned));
        }
        let error = finish(&base, "t", Uuid::new_v4(), "sample-matter")
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("read the Project's repository inventory"));
    }
}
