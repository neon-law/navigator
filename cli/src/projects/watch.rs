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
}

#[derive(Serialize)]
struct CheckoutPreview {
    code: String,
    checkout: CheckoutState,
    source_repository_configured: bool,
}

/// Report the checkout state of the signed-in person's active Projects.
async fn run_preview(host: &str, dir: &Path, verbose: bool) -> ExitCode {
    match preview(host, dir, verbose).await {
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

async fn preview(host: &str, dir: &Path, verbose: bool) -> Result<String> {
    if dir.is_dir() {
        let workspace = Box::pin(super::workspace_skills::sync(dir, host, true)).await?;
        print_workspace(&workspace, true, verbose);
    }
    let (base, token) = remote::resolve(Some(host))?;
    let projects = visible_projects(&base, &token).await?;
    serde_json::to_string(&checkout_preview(dir, projects)?).context("serialize checkout preview")
}

fn checkout_preview(dir: &Path, projects: Vec<Project>) -> Result<Vec<CheckoutPreview>> {
    match std::fs::symlink_metadata(dir) {
        Ok(metadata) => ensure!(metadata.is_dir(), "checkout root is not a directory"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect checkout root"),
    }
    let mut report = Vec::new();
    for project in projects
        .into_iter()
        .filter(|p| matches!(p.status.as_str(), "open" | "pitch"))
    {
        let checkout = if model::workspace::is_valid_slug(&project.code) {
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
        } else {
            CheckoutState::NeedsAttention
        };
        report.push(CheckoutPreview {
            code: project.code,
            checkout,
            source_repository_configured: project
                .repository_url
                .as_deref()
                .is_some_and(|url| !url.is_empty()),
        });
    }
    report.sort_by(|a, b| a.code.cmp(&b.code));
    Ok(report)
}

/// Synchronize participating active Projects once, keeping document hydration separate.
pub async fn run_sync(host: &str, dir: Option<&Path>, dry_run: bool, verbose: bool) -> ExitCode {
    let dir = match super::workspace_root::selected(dir) {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("navigator: {error:#}");
            return ExitCode::from(2);
        }
    };
    if dry_run {
        return run_preview(host, &dir, verbose).await;
    }
    match sync_once(host, None, &dir, verbose).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator sync: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Finish opening a matter with its local checkout; retries use `navigator sync`.
pub async fn sync_project(host: Option<&str>, code: &str, root: Option<&Path>) -> Result<()> {
    let (base, _) = remote::resolve(host)?;
    let dir = super::workspace_root::selected(root)?;
    sync_once(&base, Some(code), &dir, false).await.context(
        "Project is open, but checkout setup is incomplete; retry navigator sync --host <host>",
    )
}

async fn sync_once(host: &str, code: Option<&str>, root: &Path, verbose: bool) -> Result<()> {
    std::fs::create_dir_all(root)?;
    ensure!(
        !std::fs::symlink_metadata(root)?.is_symlink(),
        "checkout root is a symlink"
    );
    let dir = root.canonicalize()?;
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
    let workspace = Box::pin(super::workspace_skills::sync(&dir, host, false)).await?;
    print_workspace(&workspace, false, verbose);
    ensure!(
        !failed,
        "some checkouts need attention; other Projects were still processed"
    );
    Ok(())
}

fn print_workspace(workspace: &super::workspace_skills::Report, dry_run: bool, verbose: bool) {
    if verbose {
        workspace.print(dry_run);
        return;
    }
    if !workspace.changes.is_empty() || !workspace.missing.is_empty() {
        let action = if dry_run { "planned" } else { "applied" };
        eprintln!(
            "Workspace changes {action}: {}; needs attention: {}",
            workspace.changes.len(),
            workspace.missing.len()
        );
    }
    for missing in &workspace.missing {
        eprintln!("Workspace needs attention: {missing}");
    }
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
pub async fn run(dir: Option<&Path>, host: Option<&str>, checkouts_only: bool) -> ExitCode {
    let dir = match super::workspace_root::selected(dir) {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("navigator: {error:#}");
            return ExitCode::from(2);
        }
    };
    match watch(&dir, host, checkouts_only).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator: {error:#}");
            ExitCode::from(2)
        }
    }
}

async fn watch(dir: &Path, host: Option<&str>, checkouts_only: bool) -> Result<()> {
    remote::resolve(host)?;
    command(Path::new("."), "jj", &["--version"]).await?;
    std::fs::create_dir_all(dir).context("create Project watch directory")?;
    ensure!(
        !std::fs::symlink_metadata(dir)?.is_symlink(),
        "checkout root is a symlink"
    );
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
                if let Err(error) = poll(&dir, host, &mut copies, checkouts_only).await {
                    eprintln!("navigator: {error:#}");
                }
            }
        } => Ok(()),
    }
}

async fn poll(
    dir: &Path,
    host: Option<&str>,
    copies: &mut HashMap<PathBuf, String>,
    checkouts_only: bool,
) -> Result<()> {
    let (base, token) = remote::resolve(host)?;
    let projects = visible_projects(&base, &token).await?;
    for project in projects
        .into_iter()
        .filter(|project| matches!(project.status.as_str(), "open" | "pitch"))
    {
        let result = if checkouts_only {
            sync_checkout(dir, &project, &base, &token).await
        } else {
            update(dir, &project, &base, &token, copies).await
        };
        if let Err(error) = result {
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
    let remotes = if super::lifecycle::github_origin(dir, url).await.is_some() {
        checkout_mirror(&status["repository_remotes"], url)?
    } else {
        FetchRemotes::Origin
    };
    update_checkout(dir, project, url, remotes, base).await
}

#[derive(Debug, PartialEq, Eq)]
enum FetchRemotes<'a> {
    Origin,
    MirrorUnavailable,
    Verified(&'a str),
}

fn checkout_mirror<'a>(remotes: &'a serde_json::Value, origin: &str) -> Result<FetchRemotes<'a>> {
    match remotes["forgejo_status"].as_str() {
        Some("verified") => Ok(FetchRemotes::Verified(
            remotes["forgejo"]
                .as_str()
                .filter(|url| !url.is_empty() && *url != origin)
                .context("verified Forgejo mirror has no distinct URL; retry sync")?,
        )),
        Some(status @ ("not_authorized" | "missing" | "unavailable" | "not_configured")) => {
            eprintln!("Forgejo mirror is {status}. Fetching from origin only.");
            if let Some(detail) = super::mirror_repair::issue_detail(&remotes["forgejo_issue"]) {
                eprintln!("{detail}");
            }
            Ok(FetchRemotes::MirrorUnavailable)
        }
        _ => bail!("unrecognized Forgejo mirror status; check the Navigator host and CLI versions"),
    }
}

async fn update_checkout(
    dir: &Path,
    project: &Project,
    url: &str,
    remotes: FetchRemotes<'_>,
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
    prepare_jj(&root).await?;
    let previous_main = checkout_commit(&root, "present(main@origin)").await?;
    // Fetch only the remotes the live Project inventory verified. An existing
    // checkout may retain an earlier mirror or a contributor's personal fork.
    let mut fetch = vec![
        "--ignore-working-copy",
        "--config",
        "git.abandon-unreachable-commits=false",
        "git",
        "fetch",
        "--remote",
        "origin",
    ];
    match remotes {
        FetchRemotes::Verified(mirror) => {
            configure_mirror(&root, url, mirror).await?;
            fetch.extend(["--remote", "forgejo"]);
        }
        FetchRemotes::Origin => {
            check_push_destinations(&root, &[url])?;
        }
        FetchRemotes::MirrorUnavailable => {
            let mirror =
                git_output(&root, &["config", "--local", "--get", "remote.forgejo.url"]).await?;
            ensure!(
                mirror.status.success() || mirror.status.code() == Some(1),
                "could not inspect the existing Forgejo remote"
            );
            let local = String::from_utf8(mirror.stdout)?;
            let urls = check_push_destinations(&root, &[url, local.trim()])?;
            if urls.lines().any(|push| push != url) {
                eprintln!("The checkout still has a secondary push destination. Verify it before pushing.");
            }
        }
    }
    command(&root, "jj", &fetch).await?;
    advance_clean_checkout(&root, &project.code, &previous_main).await
}

pub(super) async fn checkout_commit(root: &Path, revision: &str) -> Result<String> {
    let output = command(
        root,
        "jj",
        &[
            "--ignore-working-copy",
            "log",
            "--no-graph",
            "-r",
            revision,
            "-T",
            r#"commit_id ++ "
""#,
        ],
    )
    .await?;
    Ok(output.trim().to_owned())
}

async fn advance_clean_checkout(root: &Path, code: &str, previous_main: &str) -> Result<()> {
    let main = checkout_commit(root, "present(main@origin)").await?;
    let parent = checkout_commit(root, "parents(@)").await?;
    if main == parent {
        return Ok(());
    }
    if main.is_empty() {
        eprintln!("Skipped {code}: origin has no main bookmark");
        return Ok(());
    }
    let clean = git_working_copy_clean(root).await?;
    if clean
        && !previous_main.is_empty()
        && parent == previous_main
        && git_output(root, &["merge-base", "--is-ancestor", previous_main, &main])
            .await?
            .status
            .success()
    {
        let source = format!(
            r#"@ & empty() & ~conflicts() & children({previous_main}) & heads(@::) & ~bookmarks() & ~tags() & description(exact:"")"#
        );
        if !checkout_commit(root, &source).await?.is_empty() {
            // Re-evaluate the selection after jj snapshots the working copy,
            // so an edit between the Git status check and rebase is retained.
            command(root, "jj", &["rebase", "-r", &source, "--onto", &main]).await?;
            if checkout_commit(root, "parents(@)").await? == main {
                eprintln!("Updated {code}");
                return Ok(());
            }
        }
    }
    let behind = checkout_commit(root, "main@origin ~ ::parents(@)").await?;
    eprintln!(
        "Skipped {code}: kept local checkout, behind by {}",
        behind.lines().count()
    );
    Ok(())
}

async fn git_working_copy_clean(root: &Path) -> Result<bool> {
    Ok(command(
        root,
        "git",
        &["--no-optional-locks", "status", "--porcelain"],
    )
    .await?
    .trim()
    .is_empty())
}

async fn prepare_jj(root: &Path) -> Result<()> {
    let needs_jj = match std::fs::symlink_metadata(root.join(".jj")) {
        Ok(metadata) => {
            ensure!(metadata.is_dir(), "jj metadata is not a directory");
            false
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error).context("inspect jj metadata"),
    };
    if needs_jj {
        ensure!(
            git_working_copy_clean(root).await?,
            "checkout has local work; skipped"
        );
        command(root, "jj", &["git", "init", "--colocate"]).await?;
    }

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

    #[test]
    fn only_a_verified_mirror_is_used_for_sync() -> Result<()> {
        let origin = "https://github.com/example/matter";
        let mirror = "https://forge.example.test/example/matter";
        for status in ["not_authorized", "missing", "not_configured", "unavailable"] {
            let remotes = serde_json::json!({"forgejo_status": status, "forgejo": mirror});
            assert_eq!(
                checkout_mirror(&remotes, origin)?,
                FetchRemotes::MirrorUnavailable
            );
        }
        let mut remotes = serde_json::json!({"forgejo_status": "verified", "forgejo": mirror});
        assert_eq!(
            checkout_mirror(&remotes, origin)?,
            FetchRemotes::Verified(mirror)
        );
        remotes["forgejo"] = serde_json::Value::Null;
        assert!(checkout_mirror(&remotes, origin).is_err());
        Ok(())
    }

    fn jj_available() -> bool {
        std::process::Command::new("jj")
            .arg("--version")
            .output()
            .is_ok()
    }

    #[tokio::test]
    async fn sync_fetches_from_origin_when_mirror_access_changes() -> Result<()> {
        if !jj_available() {
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let dir = temp.path().canonicalize()?;
        let (project, origin, mirror) = seed_checkout_forges(&dir, "site.example.test").await?;
        let origin = origin.to_str().unwrap();
        let checkout = dir.join("matter");
        let sync =
            |remotes| update_checkout(&dir, &project, origin, remotes, "https://site.example.test");
        sync(FetchRemotes::MirrorUnavailable).await?;
        assert_eq!(
            command(&checkout, "git", &["remote"]).await?.trim(),
            "origin"
        );

        sync(FetchRemotes::Verified(mirror.to_str().unwrap())).await?;
        let push_urls = ["remote", "get-url", "--push", "--all", "origin"];
        let before = command(&checkout, "git", &push_urls).await?;
        std::fs::rename(&mirror, dir.join("offline.git"))?;
        sync(FetchRemotes::MirrorUnavailable).await?;
        assert_eq!(command(&checkout, "git", &push_urls).await?, before);
        Ok(())
    }

    #[tokio::test]
    async fn sync_clones_colocated_jj_configures_both_forges_and_preserves_dirty_work() -> Result<()>
    {
        if !jj_available() {
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let dir = temp.path().canonicalize()?;
        let (project, origin, mirror) = seed_checkout_forges(&dir, "site.example.test").await?;
        let sync = || {
            super::update_checkout(
                &dir,
                &project,
                origin.to_str().unwrap(),
                FetchRemotes::Verified(mirror.to_str().unwrap()),
                "https://site.example.test",
            )
        };
        sync().await?;
        let checkout = dir.join("matter");
        assert!(checkout.join(".jj").is_dir());
        assert!(checkout.join(".git").is_dir());
        assert_eq!(std::fs::read(checkout.join(SKILL_PATH))?, SKILL_BYTES);
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
        std::fs::write(dir.join("seed/server.txt"), "first")?;
        publish_checkout_change(&dir, &origin).await?;
        sync().await?;
        assert_eq!(
            std::fs::read_to_string(checkout.join("server.txt"))?,
            "first"
        );
        let before = working_revision(&checkout).await?;
        let previous_main = checkout_commit(&checkout, "main@origin").await?;
        std::fs::write(checkout.join("local.txt"), "unfinished work")?;
        std::fs::write(dir.join("seed/server.txt"), "second")?;
        publish_checkout_change(&dir, &origin).await?;
        sync().await?;
        assert_ne!(
            checkout_commit(&checkout, "main@origin").await?,
            previous_main
        );
        assert_eq!(
            checkout_commit(&checkout, "main@origin ~ ::parents(@)")
                .await?
                .lines()
                .count(),
            1
        );
        assert_eq!(
            std::fs::read_to_string(checkout.join("server.txt"))?,
            "first"
        );
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

    #[tokio::test]
    async fn sync_colocates_an_existing_git_checkout() -> Result<()> {
        if !jj_available() {
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let dir = temp.path().canonicalize()?;
        let (project, origin, mirror) = seed_checkout_forges(&dir, "site.example.test").await?;
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
            FetchRemotes::Verified(mirror.to_str().unwrap()),
            "https://site.example.test",
        )
        .await?;
        assert!(existing_root.join("matter/.jj").is_dir());
        Ok(())
    }

    async fn publish_checkout_change(dir: &Path, origin: &Path) -> Result<()> {
        let seed = dir.join("seed");
        command(&seed, "git", &["add", "server.txt"]).await?;
        command(&seed, "git", &["commit", "-m", "server update"]).await?;
        command(&seed, "git", &["push", origin.to_str().unwrap(), "main"]).await?;
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

    const SKILL_PATH: &str = ".agents/skills/fixture/SKILL.md";
    const SKILL_BYTES: &[u8] = b"# Fixture skill\n";

    async fn seed_checkout_forges(dir: &Path, host: &str) -> Result<(Project, PathBuf, PathBuf)> {
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
            format!("project:\n  name: matter\n  host: {host}\n"),
        )?;
        std::fs::create_dir_all(seed.join(".agents/skills/fixture"))?;
        std::fs::write(seed.join(SKILL_PATH), SKILL_BYTES)?;
        command(&seed, "git", &["add", "navigator.yaml", SKILL_PATH]).await?;
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
                .map(|item| (
                    item.code.as_str(),
                    &item.checkout,
                    item.source_repository_configured
                ))
                .collect::<Vec<_>>(),
            vec![
                ("existing", &CheckoutState::Present, true),
                ("missing", &CheckoutState::Missing, true),
                ("occupied", &CheckoutState::NeedsAttention, true),
                ("unprovisioned", &CheckoutState::Missing, false),
            ]
        );
        assert!(!root.join("missing").exists());
        assert_eq!(std::fs::read(root.join("occupied"))?, b"local work");
        Ok(())
    }

    use crate::projects::test_login::{answer, checkout, site};
    use serde_json::json;

    const GITHUB: &str = "https://github.com/sample-firm/sample-matter";
    const MIRROR: &str = "https://forge.example.test/sample-firm/sample-matter";
    const SITE: &str = "https://site.example.test";
    const MANIFEST: &str = "project:\n  name: sample-matter\n  host: site.example.test\n";

    fn listed(code: &str, url: Option<&str>) -> Project {
        Project {
            id: Uuid::new_v4(),
            code: code.into(),
            status: "open".into(),
            repository_url: url.map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn a_checkout_is_refused_before_any_clone_when_its_inventory_is_unsafe() {
        let server = MockServer::start().await;
        let base = server.uri();
        let dir = tempfile::tempdir().unwrap();
        let sync = |project: Project| {
            let base = base.clone();
            let dir = dir.path().to_path_buf();
            async move {
                sync_checkout(&dir, &project, &base, "t")
                    .await
                    .unwrap_err()
                    .to_string()
            }
        };
        assert_eq!(
            sync(listed("Not A Code", Some(GITHUB))).await,
            "invalid Project code"
        );
        assert_eq!(
            sync(listed("sample-matter", None)).await,
            "no repository URL is visible"
        );
        assert_eq!(
            sync(listed("sample-matter", Some("/srv/git/matter"))).await,
            "unsupported repository transport"
        );
        for (remotes, expected) in [
            (
                json!({"origin": MIRROR}),
                "repository inventory changed; retry sync",
            ),
            (
                json!({"origin": GITHUB, "forgejo": GITHUB, "forgejo_status": "verified"}),
                "verified Forgejo mirror has no distinct URL; retry sync",
            ),
        ] {
            let project = listed("sample-matter", Some(GITHUB));
            let door = format!("/app/api/projects/{}", project.id);
            let status = json!({"repository_remotes": remotes});
            answer(&server, "GET", &door, 200, status).await;
            assert_eq!(sync(project).await, expected);
        }
        assert!(!dir.path().join("sample-matter").exists());
    }

    #[tokio::test]
    async fn an_existing_checkout_must_be_this_projects_clean_git_checkout() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let dir = temp.path().canonicalize()?;
        let project = listed("sample-matter", Some(GITHUB));
        let place = |manifest: &str, origin: &str| -> Result<PathBuf> {
            let root = dir.join("sample-matter");
            if root.exists() {
                std::fs::remove_dir_all(&root)?;
            }
            let fixture = checkout(manifest, Some(origin));
            std::fs::rename(fixture.path(), &root)?;
            Ok(root)
        };
        let update = || async {
            update_checkout(&dir, &project, GITHUB, FetchRemotes::Origin, SITE)
                .await
                .unwrap_err()
                .to_string()
        };
        place(&MANIFEST.replace("site.", "other."), GITHUB)?;
        assert_eq!(
            update().await,
            "local manifest names a different Navigator host"
        );
        place(&MANIFEST.replace("sample-matter", "another-matter"), GITHUB)?;
        assert_eq!(update().await, "local manifest names a different Project");
        place(MANIFEST, MIRROR)?;
        assert_eq!(
            update().await,
            "local origin differs from the Project repository"
        );
        let root = place(MANIFEST, GITHUB)?;
        assert_eq!(update().await, "checkout has local work; skipped");
        std::fs::write(root.join(".jj"), "not a store")?;
        assert_eq!(update().await, "jj metadata is not a directory");
        Ok(())
    }

    #[tokio::test]
    async fn a_mirror_is_added_to_origin_pushes_only_when_no_other_destination_is_set() -> Result<()>
    {
        let fixture = checkout(MANIFEST, Some(GITHUB));
        let root = fixture.path();
        let push_urls = || check_push_destinations(root, &[GITHUB, MIRROR]);
        assert_eq!(push_urls()?, "");
        for _ in 0..2 {
            configure_mirror(root, GITHUB, MIRROR).await?;
            assert_eq!(
                push_urls()?.lines().collect::<Vec<_>>(),
                vec![GITHUB, MIRROR]
            );
        }
        let error = check_push_destinations(root, &[GITHUB]).unwrap_err();
        assert!(error
            .to_string()
            .contains("origin has an unexpected push destination"));
        let other = "https://forge.example.test/other/sample-matter";
        let error = configure_mirror(root, GITHUB, other).await.unwrap_err();
        assert!(error
            .to_string()
            .contains("configured Forgejo remote differs from the Project"));
        Ok(())
    }

    #[tokio::test]
    async fn sync_processes_every_active_project_and_reports_the_ones_needing_attention() {
        let inventory = json!([
            {"id": Uuid::new_v4(), "code": "closed-matter", "status": "closed"},
            {"id": Uuid::new_v4(), "code": "sample-matter", "status": "open"},
        ]);
        let (server, _login) = site(inventory).await;
        let host = server.uri();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("checkouts");

        assert_eq!(
            run_sync(&host, Some(&root), true, false).await,
            ExitCode::SUCCESS
        );
        assert!(!root.exists(), "a dry run writes nothing");
        assert_eq!(
            active_codes(&host, "test-token").await.unwrap(),
            vec!["sample-matter"]
        );

        std::fs::create_dir_all(root.join("closed-matter")).unwrap();
        let error = sync_once(&host, None, &root, false).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "some checkouts need attention; other Projects were still processed"
        );
        assert!(root.join("AGENTS.md").is_file());
        assert!(root.join("closed-matter").is_dir());
        assert_eq!(
            run_sync(&host, Some(&root), false, false).await,
            ExitCode::FAILURE
        );
        assert_eq!(
            run_sync(&host, Some(&root), true, false).await,
            ExitCode::SUCCESS
        );

        let error = sync_project(Some(&host), "closed-matter", Some(&root))
            .await
            .unwrap_err();
        assert!(format!("{error:#}")
            .contains("opened Project is absent from the active participation inventory"));

        poll(&root, Some(&host), &mut HashMap::new(), false)
            .await
            .unwrap();

        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        let error = sync_once(&host, None, &link, false).await.unwrap_err();
        assert_eq!(error.to_string(), "checkout root is a symlink");

        let signed_out = Some("signed-out.example.test");
        assert_eq!(run(Some(&root), signed_out, false).await, ExitCode::from(2));
        assert_eq!(
            run_sync("signed-out.example.test", Some(&root), true, false).await,
            ExitCode::from(2)
        );
    }

    #[tokio::test]
    async fn sync_clones_an_active_project_then_leaves_its_local_work_alone() -> Result<()> {
        let _jj = crate::projects::test_login::jj_stand_in();
        let url = "https://forge.example.test/sample-firm/matter";
        let id = Uuid::new_v4();
        let inventory =
            json!([{"id": id, "code": "matter", "status": "open", "repository_url": url}]);
        let (server, _login) = site(inventory).await;
        let host = server.uri();
        let door = format!("/app/api/projects/{id}");
        let status = json!({"repository_remotes": {"origin": url}});
        answer(&server, "GET", &door, 200, status).await;
        let temp = tempfile::tempdir()?;
        let forges = temp.path().join("forges");
        std::fs::create_dir(&forges)?;
        let (_, origin, _) = seed_checkout_forges(&forges, &host).await?;
        // Git reads the hosted source URL from the local bare repository.
        std::env::set_var("GIT_CONFIG_COUNT", "1");
        let rewrite = format!("url.{}.insteadOf", origin.display());
        std::env::set_var("GIT_CONFIG_KEY_0", rewrite);
        std::env::set_var("GIT_CONFIG_VALUE_0", url);

        let root = temp.path().join("checkouts");
        assert_eq!(
            run_sync(&host, Some(&root), false, false).await,
            ExitCode::SUCCESS
        );
        let checkout = root.join("matter");
        assert!(checkout.join(".jj").is_dir());
        assert_eq!(std::fs::read(checkout.join(SKILL_PATH))?, SKILL_BYTES);
        let configured = command(&checkout, "git", &["config", "--get", "remote.origin.url"]);
        assert_eq!(configured.await?.trim(), url);

        sync_project(Some(&host), "matter", Some(&root)).await?;
        poll(
            &root.canonicalize()?,
            Some(&host),
            &mut HashMap::new(),
            false,
        )
        .await?;

        std::fs::write(checkout.join("local.txt"), "unfinished work")?;
        assert_eq!(
            run_sync(&host, Some(&root), false, false).await,
            ExitCode::SUCCESS
        );
        assert_eq!(
            std::fs::read_to_string(checkout.join("local.txt"))?,
            "unfinished work"
        );

        // The watch reports that checkout on each pass and keeps running.
        let watching =
            tokio::time::timeout(Duration::from_millis(500), watch(&root, Some(&host), false));
        assert!(watching.await.is_err(), "the watch runs until interrupted");
        Ok(())
    }

    #[test]
    fn a_checkout_root_that_is_a_file_cannot_be_previewed() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("occupied");
        std::fs::write(&file, b"local work").unwrap();
        let error = checkout_preview(&file, Vec::new()).err().unwrap();
        assert_eq!(error.to_string(), "checkout root is not a directory");
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
