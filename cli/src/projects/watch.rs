//! Synchronize active Project checkouts and watch their document revisions.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

use crate::document_sync::{
    ensure_document_path_is_safe, ensure_pull_target_is_safe, pull_target,
    raw_document_is_git_ignored, read_manifest, read_pointer,
};
use crate::remote::{self, DocumentClient};

const INTERVAL: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct Project {
    #[serde(default)]
    id: uuid::Uuid,
    code: String,
    status: String,
    repository_url: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckoutState {
    Present,
    Missing,
    NeedsAttention,
    RepositoryUnavailable,
}

#[derive(Serialize)]
struct CheckoutPreview {
    code: String,
    checkout: CheckoutState,
}

/// Report the checkout state of the signed-in person's active Projects.
pub async fn run_preview(dir: Option<PathBuf>, host: &str) -> ExitCode {
    match preview(dir, host).await {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator: {error:#}");
            ExitCode::from(2)
        }
    }
}

async fn preview(dir: Option<PathBuf>, host: &str) -> Result<String> {
    let dir = dir
        .or_else(|| std::env::home_dir().map(|home| home.join("Work")))
        .context("no home directory; pass --dir")?;
    let (base, token) = remote::resolve(Some(host))?;
    let projects = visible_projects(&base, &token).await?;
    serde_json::to_string(&checkout_preview(&dir, projects)?).context("serialize Work sync preview")
}

fn checkout_preview(dir: &Path, projects: Vec<Project>) -> Result<Vec<CheckoutPreview>> {
    match std::fs::symlink_metadata(dir) {
        Ok(metadata) => ensure!(metadata.is_dir(), "Work path is not a directory"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect Work directory"),
    }
    let mut report = Vec::new();
    for project in projects
        .into_iter()
        .filter(|p| matches!(p.status.as_str(), "open" | "pitch"))
    {
        let checkout = if !model::workspace::is_valid_slug(&project.code) {
            CheckoutState::NeedsAttention
        } else if project.repository_url.as_deref().is_none_or(str::is_empty) {
            CheckoutState::RepositoryUnavailable
        } else {
            let root = dir.join(&project.code);
            match std::fs::symlink_metadata(&root) {
                Ok(metadata) if metadata.is_dir() && root.join(".jj").is_dir() => {
                    CheckoutState::Present
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    CheckoutState::Missing
                }
                _ => CheckoutState::NeedsAttention,
            }
        };
        report.push(CheckoutPreview {
            code: project.code,
            checkout,
        });
    }
    report.sort_by(|a, b| a.code.cmp(&b.code));
    Ok(report)
}

/// Synchronize participating active Projects once, keeping document hydration separate.
pub async fn run_sync(dir: Option<PathBuf>, host: &str, dry_run: bool) -> ExitCode {
    if dry_run {
        return run_preview(dir, host).await;
    }
    match sync_once(dir, host, None).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator sync: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Finish opening a matter with its local checkout; retries use `navigator sync`.
pub async fn sync_project(host: Option<&str>, code: &str) -> Result<()> {
    let (base, _) = remote::resolve(host)?;
    sync_once(None, &base, Some(code)).await.context(
        "Project is open, but checkout setup is incomplete; retry navigator sync --host <host>",
    )
}

async fn sync_once(dir: Option<PathBuf>, host: &str, code: Option<&str>) -> Result<()> {
    let dir = dir
        .or_else(|| std::env::home_dir().map(|home| home.join("Work")))
        .context("no home directory; pass --dir")?;
    std::fs::create_dir_all(&dir)?;
    ensure!(
        !std::fs::symlink_metadata(&dir)?.is_symlink(),
        "Work path is a symlink"
    );
    let dir = dir.canonicalize()?;
    let (base, token) = remote::resolve(Some(host))?;
    let projects = visible_projects(&base, &token).await?;
    let mut failed = false;
    let mut matched = false;
    for project in projects {
        if !matches!(project.status.as_str(), "open" | "pitch") {
            if dir.join(&project.code).exists() {
                eprintln!("Closed Project checkout retained: {}", project.code);
            }
            continue;
        }
        if code.is_some_and(|code| code != project.code) {
            continue;
        }
        matched = true;
        if let Err(error) = sync_checkout(&dir, &project, &base, &token).await {
            eprintln!("navigator sync: Project {}: {error:#}", project.code);
            failed = true;
        }
    }
    ensure!(
        code.is_none() || matched,
        "opened Project is absent from the active participation inventory"
    );
    ensure!(
        !failed,
        "some checkouts need attention; other Projects were still processed"
    );
    Ok(())
}

pub(super) async fn active_codes(base: &str, token: &str) -> Result<Vec<String>> {
    Ok(visible_projects(base, token)
        .await?
        .into_iter()
        .filter(|p| matches!(p.status.as_str(), "open" | "pitch"))
        .map(|p| p.code)
        .collect())
}

/// Keep polling until Ctrl-C. An unavailable Project does not stop the others.
pub async fn run(dir: &Path, host: Option<&str>) -> ExitCode {
    match watch(dir, host).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator: {error:#}");
            ExitCode::from(2)
        }
    }
}

async fn watch(dir: &Path, host: Option<&str>) -> Result<()> {
    remote::resolve(host)?;
    command(Path::new("."), "jj", &["--version"]).await?;
    std::fs::create_dir_all(dir).context("create Project watch directory")?;
    let dir = dir
        .canonicalize()
        .context("resolve Project watch directory")?;
    let mut copies = HashMap::new();
    println!(
        "Watching {} every 30 seconds. Press Ctrl-C to stop.",
        dir.display()
    );
    let mut interval = tokio::time::interval(INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    tokio::select! {
        signal = tokio::signal::ctrl_c() => signal.context("listen for Ctrl-C"),
        () = async {
            loop {
                interval.tick().await;
                if let Err(error) = poll(&dir, host, &mut copies).await {
                    eprintln!("navigator: {error:#}");
                }
            }
        } => Ok(()),
    }
}

async fn poll(dir: &Path, host: Option<&str>, copies: &mut HashMap<PathBuf, String>) -> Result<()> {
    let (base, token) = remote::resolve(host)?;
    let projects = visible_projects(&base, &token).await?;
    for project in projects
        .into_iter()
        .filter(|project| matches!(project.status.as_str(), "open" | "pitch"))
    {
        if let Err(error) = update(dir, &project, &base, &token, copies).await {
            eprintln!("navigator: Project {}: {error:#}", project.code);
        }
    }
    Ok(())
}

async fn visible_projects(base: &str, token: &str) -> Result<Vec<Project>> {
    let client = remote::authenticated_client_builder(base)?
        .timeout(INTERVAL)
        .build()?;
    let body = remote::fetch_project_list(&client, base, token).await?;
    serde_json::from_str(&body).context("read visible Projects")
}

async fn update(
    dir: &Path,
    project: &Project,
    base: &str,
    token: &str,
    copies: &mut HashMap<PathBuf, String>,
) -> Result<()> {
    sync_checkout(dir, project, base, token).await?;
    let root = dir.join(&project.code);
    let code = &project.code;
    let client = DocumentClient::with_credential(base.to_owned(), token.to_owned(), code).await?;
    let blobs = root.join("blobs");
    ensure!(
        !std::fs::symlink_metadata(&blobs).is_ok_and(|metadata| metadata.is_symlink()),
        "refusing a symlinked blobs directory"
    );
    for pointer in crate::document_read::discover_pointers(&root)? {
        if let Err(error) = hydrate(&root, &pointer, &client, copies).await {
            eprintln!(
                "navigator: Project {code}, {}: {error:#}",
                pointer.display()
            );
        }
    }
    Ok(())
}

async fn sync_checkout(dir: &Path, project: &Project, base: &str, token: &str) -> Result<()> {
    ensure!(
        model::workspace::is_valid_slug(&project.code),
        "invalid Project code"
    );
    let url = project
        .repository_url
        .as_deref()
        .context("no repository URL is visible")?;
    ensure!(
        url.starts_with("https://") || url.starts_with("ssh://") || url.starts_with("git@"),
        "unsupported repository transport"
    );
    let response = remote::authenticated_client_builder(base)?
        .timeout(INTERVAL)
        .build()?
        .get(format!("{base}/app/api/projects/{}", project.id))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?;
    let status: serde_json::Value = response.json().await?;
    ensure!(
        status["repository_remotes"]["origin"].as_str() == Some(url),
        "repository inventory changed; retry sync"
    );
    let forgejo = status["repository_remotes"]["forgejo"]
        .as_str()
        .filter(|mirror| *mirror != url);
    let github = super::lifecycle::github_origin(dir, url).await.is_some();
    ensure!(
        !github || forgejo.is_some(),
        "no secondary Forgejo URL is configured for this Project"
    );
    update_checkout(dir, project, url, if github { forgejo } else { None }, base).await
}

async fn update_checkout(
    dir: &Path,
    project: &Project,
    url: &str,
    forgejo: Option<&str>,
    base: &str,
) -> Result<()> {
    let root = dir.join(&project.code);
    ensure_checkout_exists(dir, project, url, base).await?;
    ensure!(
        root.canonicalize()? == root,
        "refusing a Project directory outside its assigned path"
    );
    ensure_document_path_is_safe(&root, &root.join("navigator.yaml"), false, true)?;
    let (code, manifest_host) = read_manifest(&root)?;
    ensure!(
        manifest_host.is_none_or(|host| crate::credentials::base_url(&host) == base),
        "local manifest names a different Navigator host"
    );
    ensure!(
        code == project.code,
        "local manifest names a different Project"
    );
    ensure!(
        !std::fs::symlink_metadata(root.join(".git"))?.is_symlink(),
        "Git metadata is a symlink"
    );
    let git_root = command(&root, "git", &["rev-parse", "--show-toplevel"]).await?;
    ensure!(
        Path::new(git_root.trim()).canonicalize()? == root,
        "Project folder is not a Git checkout"
    );
    let origin = ensure_origin(&root, url).await?;
    ensure!(
        origin.trim() == url,
        "local origin differs from the Project repository"
    );
    prepare_clean_jj(&root).await?;
    if let Some(mirror) = forgejo {
        configure_mirror(&root, url, mirror).await?;
    } else {
        check_push_destinations(&root, &[url])?;
    }
    command(
        &root,
        "jj",
        &["--ignore-working-copy", "git", "fetch", "--all-remotes"],
    )
    .await?;
    Ok(())
}

async fn prepare_clean_jj(root: &Path) -> Result<()> {
    let needs_jj = match std::fs::symlink_metadata(root.join(".jj")) {
        Ok(metadata) => {
            ensure!(metadata.is_dir(), "jj metadata is not a directory");
            false
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error).context("inspect jj metadata"),
    };
    ensure!(
        command(
            root,
            "git",
            &["--no-optional-locks", "status", "--porcelain"]
        )
        .await?
        .trim()
        .is_empty(),
        "checkout has local work; skipped"
    );
    if needs_jj {
        command(root, "jj", &["git", "init", "--colocate"]).await?;
    }
    let dirty = command(root, "jj", &["--ignore-working-copy", "diff", "--summary"]).await?;
    let conflicts = command(
        root,
        "jj",
        &[
            "--ignore-working-copy",
            "log",
            "--no-graph",
            "-r",
            "@",
            "-T",
            "conflict",
        ],
    )
    .await?;
    ensure!(
        dirty.trim().is_empty() && conflicts.trim() == "false",
        "checkout has local work or conflicts; skipped"
    );
    Ok(())
}

async fn ensure_checkout_exists(
    dir: &Path,
    project: &Project,
    url: &str,
    base: &str,
) -> Result<()> {
    let root = dir.join(&project.code);
    if matches!(std::fs::symlink_metadata(&root), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        let staging = tempfile::tempdir_in(dir)?;
        let checkout = staging.path().join("checkout");
        command(
            staging.path(),
            "jj",
            &[
                "git",
                "clone",
                "--colocate",
                "--branch",
                "main",
                "--",
                url,
                "checkout",
            ],
        )
        .await?;
        let (code, manifest_host) = read_manifest(&checkout)?;
        ensure!(
            manifest_host.is_none_or(|host| crate::credentials::base_url(&host) == base),
            "cloned manifest names a different Navigator host"
        );
        ensure!(
            code == project.code,
            "cloned manifest names a different Project"
        );
        std::fs::rename(checkout, &root).context("publish Project checkout")?;
        eprintln!("Cloned {}", project.code);
    }
    Ok(())
}

async fn configure_mirror(root: &Path, url: &str, mirror: &str) -> Result<()> {
    let existing = command(root, "git", &["remote"]).await?;
    if existing.lines().any(|name| name == "forgejo") {
        ensure!(
            command(
                root,
                "git",
                &["config", "--local", "--get", "remote.forgejo.url"]
            )
            .await?
            .trim()
                == mirror,
            "configured Forgejo remote differs from the Project; skipped"
        );
    } else {
        command(root, "git", &["remote", "add", "forgejo", mirror]).await?;
    }
    let urls = check_push_destinations(root, &[url, mirror])?;
    for destination in [url, mirror] {
        if !urls.lines().any(|configured| configured == destination) {
            command(
                root,
                "git",
                &[
                    "config",
                    "--local",
                    "--add",
                    "remote.origin.pushurl",
                    destination,
                ],
            )
            .await?;
        }
    }
    Ok(())
}

fn check_push_destinations(root: &Path, allowed: &[&str]) -> Result<String> {
    let configured = super::lifecycle::git(
        root,
        &["config", "--local", "--get-all", "remote.origin.pushurl"],
    )?;
    ensure!(
        configured.status.success() || configured.status.code() == Some(1),
        "could not inspect origin push URLs"
    );
    let urls = String::from_utf8(configured.stdout)?;
    ensure!(
        urls.lines().all(|url| allowed.contains(&url)),
        "origin has an unexpected push destination; review the Project's current forge authority before retrying"
    );
    Ok(urls)
}

async fn ensure_origin(root: &Path, url: &str) -> Result<String> {
    let output = git_output(root, &["config", "--local", "--get", "remote.origin.url"]).await?;
    if output.status.success() {
        return Ok(String::from_utf8(output.stdout)?);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let missing = output.status.code() == Some(1)
        && !stderr.to_ascii_lowercase().contains("fatal")
        && !stderr.to_ascii_lowercase().contains("error");
    if !missing {
        bail!(
            "git config --local --get remote.origin.url failed ({}): {}",
            output.status,
            stderr.trim()
        );
    }
    command(root, "git", &["remote", "add", "origin", url]).await?;
    command(
        root,
        "git",
        &["config", "--local", "remote.origin.pushurl", url],
    )
    .await?;
    Ok(url.to_string())
}

async fn git_output(dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("run git {}", args.join(" ")))
}

pub(super) async fn command(dir: &Path, program: &str, args: &[&str]) -> Result<String> {
    let output = tokio::process::Command::new(program)
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("run {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} {} failed ({}): {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn digest(path: &Path) -> Result<Option<String>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(model::documents::sha256_hex(&bytes))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn hydrate(
    root: &Path,
    relative: &Path,
    client: &DocumentClient,
    copies: &mut HashMap<PathBuf, String>,
) -> Result<()> {
    let pointer_path = root.join(relative);
    ensure_document_path_is_safe(root, &pointer_path, true, true)?;
    let Some(pointer) = read_pointer(&pointer_path)? else {
        return Ok(());
    };
    let target = pull_target(root, relative)?;
    ensure_pull_target_is_safe(root, &target)?;
    raw_document_is_git_ignored(root, &target)?;
    let current = digest(&target)?;
    let expected = &pointer.current_version.sha256;
    if current.as_ref() == Some(expected) {
        copies.insert(target, expected.clone());
        return Ok(());
    }
    ensure!(
        current.is_none() || current.as_ref() == copies.get(&target),
        "local document differs from the last verified copy; left in place"
    );
    let bytes = if let Some(authority) = pointer.authority_id {
        client.download_authority_archive(authority).await?
    } else {
        client
            .download_revision(pointer.current_version.asset_id)
            .await?
    };
    ensure!(
        model::documents::sha256_hex(&bytes) == *expected
            && i64::try_from(bytes.len())? == pointer.current_version.size_bytes,
        "download does not match the document pointer"
    );
    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("document has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(&bytes)?;
    ensure_pull_target_is_safe(root, &target)?;
    raw_document_is_git_ignored(root, &target)?;
    ensure_document_path_is_safe(root, &pointer_path, false, true)?;
    ensure!(
        read_pointer(&pointer_path)?.as_ref() == Some(&pointer),
        "pointer changed during download"
    );
    ensure!(
        digest(&target)? == current,
        "local document changed during download"
    );
    if current.is_none() {
        staged.persist_noclobber(&target)?;
    } else {
        staged.persist(&target)?;
    }
    println!("Downloaded {}", relative.display());
    copies.insert(target, expected.clone());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn sync_clones_colocated_jj_configures_both_forges_and_preserves_dirty_work() -> Result<()>
    {
        if std::process::Command::new("jj")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("jj is required for the checkout integration test");
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let dir = temp.path().canonicalize()?;
        let (project, origin, mirror) = seed_checkout_forges(&dir).await?;
        let sync = || {
            super::update_checkout(
                &dir,
                &project,
                origin.to_str().unwrap(),
                Some(mirror.to_str().unwrap()),
                "https://site.example.test",
            )
        };
        sync().await?;
        let existing_root = dir.join("existing");
        std::fs::create_dir(&existing_root)?;
        command(
            &existing_root,
            "git",
            &["clone", origin.to_str().unwrap(), "matter"],
        )
        .await?;
        super::update_checkout(
            &existing_root,
            &project,
            origin.to_str().unwrap(),
            Some(mirror.to_str().unwrap()),
            "https://site.example.test",
        )
        .await?;
        assert!(existing_root.join("matter/.jj").is_dir());
        let checkout = dir.join("matter");
        assert!(checkout.join(".jj").is_dir());
        assert!(checkout.join(".git").is_dir());
        let urls = command(
            &checkout,
            "git",
            &["remote", "get-url", "--push", "--all", "origin"],
        )
        .await?;
        assert_eq!(
            urls.lines().collect::<Vec<_>>(),
            vec![origin.to_str().unwrap(), mirror.to_str().unwrap()]
        );
        command(
            &checkout,
            "git",
            &[
                "config",
                "--replace-all",
                "remote.origin.pushurl",
                mirror.to_str().unwrap(),
            ],
        )
        .await?;
        sync().await?;
        let repaired = command(
            &checkout,
            "git",
            &["remote", "get-url", "--push", "--all", "origin"],
        )
        .await?;
        assert!(repaired.lines().any(|url| url == origin.to_str().unwrap()));
        assert!(repaired.lines().any(|url| url == mirror.to_str().unwrap()));
        let before = working_revision(&checkout).await?;
        std::fs::write(checkout.join("local.txt"), "unfinished work")?;
        let error = sync().await.unwrap_err();
        assert!(error.to_string().contains("local work"));
        assert_eq!(working_revision(&checkout).await?, before);
        assert_eq!(
            std::fs::read_to_string(checkout.join("local.txt"))?,
            "unfinished work"
        );
        std::fs::rename(checkout.join(".jj"), checkout.join(".jj.saved"))?;
        let outside = dir.join("must-not-be-created");
        std::os::unix::fs::symlink(&outside, checkout.join(".jj"))?;
        assert!(sync()
            .await
            .unwrap_err()
            .to_string()
            .contains("jj metadata"));
        assert!(!outside.exists());
        Ok(())
    }

    async fn working_revision(checkout: &Path) -> Result<String> {
        command(
            checkout,
            "jj",
            &[
                "--ignore-working-copy",
                "log",
                "--no-graph",
                "-r",
                "@",
                "-T",
                "commit_id",
            ],
        )
        .await
    }

    async fn seed_checkout_forges(dir: &Path) -> Result<(Project, PathBuf, PathBuf)> {
        let origin = dir.join("origin.git");
        let mirror = dir.join("mirror.git");
        let seed = dir.join("seed");
        std::fs::create_dir(&seed)?;
        for bare in [&origin, &mirror] {
            command(
                dir,
                "git",
                &[
                    "init",
                    "--bare",
                    "--initial-branch=main",
                    bare.to_str().unwrap(),
                ],
            )
            .await?;
        }
        command(&seed, "git", &["init", "--initial-branch=main"]).await?;
        command(&seed, "git", &["config", "user.name", "Fixture"]).await?;
        command(
            &seed,
            "git",
            &["config", "user.email", "fixture@example.test"],
        )
        .await?;
        command(&seed, "git", &["config", "commit.gpgsign", "false"]).await?;
        std::fs::write(
            seed.join("navigator.yaml"),
            "project:\n  name: matter\n  host: site.example.test\n",
        )?;
        command(&seed, "git", &["add", "navigator.yaml"]).await?;
        command(&seed, "git", &["commit", "-m", "fixture"]).await?;
        for bare in [&origin, &mirror] {
            command(&seed, "git", &["push", bare.to_str().unwrap(), "main"]).await?;
        }
        let project = Project {
            id: uuid::Uuid::nil(),
            code: "matter".into(),
            status: "pitch".into(),
            repository_url: Some(origin.to_str().unwrap().into()),
        };
        Ok((project, origin, mirror))
    }

    #[tokio::test]
    async fn a_checkout_without_origin_receives_the_live_repository_url() -> Result<()> {
        let root = tempfile::tempdir()?;
        command(
            root.path(),
            "git",
            &["init", "--quiet", "--initial-branch=main"],
        )
        .await?;
        let url = "https://github.com/example/matter";
        let origin = super::ensure_origin(root.path(), url).await?;
        assert_eq!(origin, url);
        let pushurl = command(
            root.path(),
            "git",
            &["config", "--local", "--get", "remote.origin.pushurl"],
        )
        .await?;
        assert_eq!(pushurl.trim(), url);
        Ok(())
    }

    #[tokio::test]
    async fn git_config_failure_includes_the_git_error() -> Result<()> {
        let root = tempfile::tempdir()?;
        command(root.path(), "git", &["init", "--quiet"]).await?;
        std::fs::write(root.path().join(".git/config"), "[\n")?;
        let error = command(
            root.path(),
            "git",
            &["config", "--local", "--get", "remote.origin.url"],
        )
        .await
        .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("git config --local --get remote.origin.url failed"),
            "{message}"
        );
        assert!(
            message.contains("bad config") || message.contains("fatal"),
            "{message}"
        );
        Ok(())
    }

    #[test]
    fn preview_reports_only_active_projects_and_preserves_local_files() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        std::fs::create_dir_all(root.join("existing/.jj"))?;
        std::fs::write(root.join("occupied"), b"local work")?;
        let projects = serde_json::from_value(serde_json::json!([
            {"code": "missing", "status": "pitch", "repository_url": "https://forge.example.test/missing"},
            {"code": "existing", "status": "open", "repository_url": "https://forge.example.test/existing"},
            {"code": "occupied", "status": "open", "repository_url": "https://forge.example.test/occupied"},
            {"code": "unprovisioned", "status": "open"},
            {"code": "closed", "status": "closed", "repository_url": "https://forge.example.test/closed"}
        ]))?;
        let report = checkout_preview(root, projects)?;
        assert_eq!(
            report
                .iter()
                .map(|item| (item.code.as_str(), &item.checkout))
                .collect::<Vec<_>>(),
            vec![
                ("existing", &CheckoutState::Present),
                ("missing", &CheckoutState::Missing),
                ("occupied", &CheckoutState::NeedsAttention),
                ("unprovisioned", &CheckoutState::RepositoryUnavailable),
            ]
        );
        assert!(!root.join("missing").exists());
        assert_eq!(std::fs::read(root.join("occupied"))?, b"local work");
        Ok(())
    }

    #[tokio::test]
    async fn hydrates_nested_blobs_and_preserves_a_local_edit() -> Result<()> {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": Uuid::nil(), "code": "sample-matter"}
            ])))
            .mount(&server)
            .await;
        let root = tempfile::tempdir()?;
        let root = root.path();
        command(root, "git", &["init"]).await?;
        std::fs::write(root.join(".gitignore"), "/documents/\n")?;
        let relative = Path::new("blobs/exhibits/sample.pdf.yaml");
        std::fs::create_dir_all(root.join("blobs/exhibits"))?;
        let target = root.join("documents/exhibits/sample.pdf");
        let client =
            DocumentClient::with_credential(server.uri(), "synthetic".into(), "sample-matter")
                .await?;
        let mut copies = HashMap::new();
        let mut previous = String::new();
        for (version, bytes) in [(1, "first"), (2, "second"), (3, "third")] {
            let asset = Uuid::new_v4();
            let sha = store::documents::sha256_hex(bytes.as_bytes());
            std::fs::write(
                root.join(relative),
                format!(
                    "{previous}kind: unclassified\nvisibility: client\ncurrent_version:\n  version: {version}\n  \
                     asset_id: {asset}\n  created_at: '2026-01-01T00:00:00Z'\n  sha256: {sha}\n  size_bytes: {}\n",
                    bytes.len()
                ),
            )?;
            previous = format!("previous_version: {asset}\n");
            Mock::given(method("GET"))
                .and(path(format!(
                    "/app/projects/sample-matter/documents/{asset}/download"
                )))
                .respond_with(ResponseTemplate::new(200).set_body_string(bytes))
                .expect(u64::from(version != 3))
                .mount(&server)
                .await;

            if version == 3 {
                std::fs::write(&target, "local edit")?;
                let error = hydrate(root, relative, &client, &mut copies)
                    .await
                    .unwrap_err();
                assert!(error.to_string().contains("left in place"));
                assert_eq!(std::fs::read_to_string(&target)?, "local edit");
            } else {
                hydrate(root, relative, &client, &mut copies).await?;
                assert_eq!(std::fs::read_to_string(&target)?, bytes);
            }
        }
        Ok(())
    }
}
