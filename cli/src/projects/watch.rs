//! Watch open projects and download document revisions.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, ensure, Context, Result};
use serde::Deserialize;

use crate::document_sync::{
    ensure_document_path_is_safe, ensure_pull_target_is_safe, pull_target,
    raw_document_is_git_ignored, read_manifest, read_pointer,
};
use crate::remote::{self, DocumentClient};

const INTERVAL: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct Project {
    code: String,
    status: String,
    repository_url: Option<String>,
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
    let client = remote::authenticated_client_builder(&base)?
        .timeout(INTERVAL)
        .build()?;
    let body = remote::fetch_project_list(&client, &base, &token).await?;
    let projects: Vec<Project> = serde_json::from_str(&body).context("read visible Projects")?;
    for project in projects
        .into_iter()
        .filter(|project| project.status == "open")
    {
        if let Err(error) = update(dir, &project, &base, &token, copies).await {
            eprintln!("navigator: Project {}: {error:#}", project.code);
        }
    }
    Ok(())
}

async fn update(
    dir: &Path,
    project: &Project,
    base: &str,
    token: &str,
    copies: &mut HashMap<PathBuf, String>,
) -> Result<()> {
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
    let root = dir.join(&project.code);
    if matches!(std::fs::symlink_metadata(&root), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        let staging = tempfile::tempdir_in(dir)?;
        let checkout = staging.path().join("checkout");
        command(staging.path(), "git", &["clone", "--", url, "checkout"]).await?;
        let (code, _) = read_manifest(&checkout)?;
        ensure!(
            code == project.code,
            "cloned manifest names a different Project"
        );
        command(&checkout, "jj", &["git", "init", "--colocate"]).await?;
        std::fs::rename(checkout, &root).context("publish Project checkout")?;
        println!("Cloned {}", project.code);
    }
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
    let git_root = command(&root, "git", &["rev-parse", "--show-toplevel"]).await?;
    ensure!(
        Path::new(git_root.trim()).canonicalize()? == root,
        "Project folder is not a Git checkout"
    );
    let origin = command(
        &root,
        "git",
        &["config", "--local", "--get", "remote.origin.url"],
    )
    .await?;
    ensure!(
        origin.trim() == url,
        "local origin differs from the Project repository"
    );
    if !root.join(".jj").is_dir() {
        command(&root, "jj", &["git", "init", "--colocate"]).await?;
    }
    let fetch = command(
        &root,
        "jj",
        &[
            "--ignore-working-copy",
            "git",
            "fetch",
            "--remote",
            "origin",
        ],
    )
    .await;
    let client = DocumentClient::with_credential(base.to_owned(), token.to_owned(), &code).await?;
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
    fetch.map(|_| ())
}

async fn command(dir: &Path, program: &str, args: &[&str]) -> Result<String> {
    let output = tokio::process::Command::new(program)
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("run {program}"))?;
    ensure!(
        output.status.success(),
        "{program} {} failed ({})",
        args[0],
        output.status
    );
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
