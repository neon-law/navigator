//! Portable Git recovery proof. Document bytes, LFS and forge metadata have separate stores.
//!
//! `hudson ops forgejo bundle <local-mirror> <new-output>` restores every
//! bundled ref into a scratch mirror before publishing. The caller supplies a
//! stable mirror; this command never fetches or changes a live forge. Only refs
//! already present in that mirror are covered, including explicitly fetched PR refs.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use crate::atomic_directory::publish_no_clobber as publish_mirror_no_clobber;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const BUNDLE_CONTENT_TYPE: &str = "application/x-git-bundle";

/// Private, durable facts needed to fetch and verify one exact archived object.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BundleReceipt {
    format: u32,
    object: String,
    generation: u64,
    size_bytes: u64,
    sha256: String,
    head: String,
    refs: String,
}

fn git(repository: &Path, args: &[&std::ffi::OsStr]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("run Git recovery check")?;
    // Git diagnostics can contain remote credentials or source paths.
    ensure!(output.status.success(), "Git recovery check failed");
    Ok(output.stdout)
}

fn text(repository: &Path, args: &[&str]) -> Result<Vec<u8>> {
    git(
        repository,
        &args.iter().map(AsRef::as_ref).collect::<Vec<_>>(),
    )
}

pub(in crate::devx) fn run(mirror: &Path, output: &Path) -> Result<()> {
    create(mirror, output)?;
    println!("Git bundle verified: Git history restored and all ref hashes match. LFS, document objects and forge metadata require separate backups.");
    Ok(())
}

pub(in crate::devx) fn archive(mirror: &Path, object: &str, receipt_path: &Path) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let storage = runtime.block_on(archive_storage())?;
    runtime.block_on(archive_with_storage(
        mirror,
        object,
        receipt_path,
        &storage,
        &storage,
    ))
}

async fn archive_with_storage(
    mirror: &Path,
    object: &str,
    receipt_path: &Path,
    writer: &dyn cloud::StorageService,
    reader: &dyn cloud::StorageService,
) -> Result<()> {
    let mirror = mirror.canonicalize().context("open local mirror")?;
    let temp = tempfile::tempdir().context("create private archive workspace")?;
    let bundle_path = temp.path().join("repository.bundle");
    let expected_refs = text(
        &mirror,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    )?;
    let expected_head = text(&mirror, &["rev-parse", "--symbolic-full-name", "HEAD"])?;
    create(&mirror, &bundle_path)?;
    ensure!(
        text(
            &mirror,
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        )? == expected_refs,
        "source refs changed during archive creation; retry from a stable mirror"
    );
    ensure!(
        text(&mirror, &["rev-parse", "--symbolic-full-name", "HEAD"])? == expected_head,
        "source default branch changed during archive creation; retry from a stable mirror"
    );
    let bytes = std::fs::read(&bundle_path).context("read verified bundle")?;
    let digest = sha256_hex(&bytes);
    let created = writer
        .put_immutable(object, &bytes, BUNDLE_CONTENT_TYPE)
        .await
        .context("create immutable archive object")?;
    let read = reader
        .get_generation(object, created.generation)
        .await
        .context("read back the exact archive generation")?;
    ensure!(
        read.generation == created.generation,
        "archive read-back generation changed"
    );
    ensure!(
        read.object.bytes.len() as u64 == created.size_bytes,
        "archive read-back size mismatch"
    );
    ensure!(
        sha256_hex(&read.object.bytes) == digest,
        "archive read-back digest mismatch"
    );
    verify_bundle_bytes(
        &read.object.bytes,
        temp.path(),
        &expected_refs,
        &expected_head,
    )?;
    let receipt = BundleReceipt {
        format: 1,
        object: object.to_owned(),
        generation: created.generation,
        size_bytes: created.size_bytes,
        sha256: digest,
        head: String::from_utf8(expected_head)?.trim().to_owned(),
        refs: String::from_utf8(expected_refs)?.trim_end().to_owned(),
    };
    write_receipt(receipt_path, &receipt)?;
    println!("Git bundle archive created and read-back restore verified. Receipt written.");
    Ok(())
}

#[derive(Deserialize)]
struct ClosedProjectRow {
    code: String,
    firm_id: Option<uuid::Uuid>,
    status: String,
}

#[derive(Deserialize)]
struct ArchiveProject {
    id: uuid::Uuid,
    code: String,
    repository_url: Option<String>,
    firm_id: Option<uuid::Uuid>,
    status: String,
}

#[derive(Serialize)]
struct BackfillOutcome {
    project: String,
    outcome: &'static str,
}

#[derive(Deserialize)]
struct GitHubArchive {
    archived: bool,
}

pub(in crate::devx) fn backfill_closed(host: &str, receipts: &Path) -> Result<()> {
    let receipts = receipts
        .canonicalize()
        .context("open private receipt directory")?;
    let metadata = receipts.metadata()?;
    ensure!(
        metadata.is_dir() && metadata.permissions().mode().trailing_zeros() >= 6,
        "receipt directory must be private (mode 0700)"
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let (base, token) = cli::remote::resolve(Some(host))?;
        let client = cli::remote::authenticated_client_builder(&base)?
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        let lifecycle: Vec<ClosedProjectRow> = client
            .get(format!("{base}/app/api/project-lifecycle"))
            .bearer_auth(&token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let projects: Vec<ArchiveProject> =
            serde_json::from_str(&cli::remote::fetch_project_list(&client, &base, &token).await?)?;
        let storage = archive_storage().await?;
        let mut report = Vec::new();
        for row in lifecycle.into_iter().filter(|row| row.status == "closed") {
            let matches: Vec<_> = projects.iter().filter(|project| {
                    project.code == row.code && project.firm_id == row.firm_id && project.status == "closed"
                }).collect();
            let outcome = match matches.as_slice() {
                [project] => backfill_one(&base, project, &receipts, &storage).await,
                [] => "project-inaccessible",
                _ => "ambiguous-project",
            };
            report.push(BackfillOutcome { project: row.code, outcome });
        }
        let mut report_file = tempfile::Builder::new()
            .prefix("closed-project-export-")
            .suffix(".json")
            .tempfile_in(&receipts)?;
        serde_json::to_writer_pretty(report_file.as_file_mut(), &report)?;
        report_file.as_file().sync_all()?;
        report_file.keep()?;
        let verified = report.iter().filter(|row| row.outcome == "verified").count();
        println!("Closed Projects: {}. Verified exports: {verified}. Outcomes are in the private report.", report.len());
        ensure!(verified == report.len(), "some closed Projects require review");
        Ok(())
    })
}

async fn backfill_one(
    base: &str,
    project: &ArchiveProject,
    receipts: &Path,
    storage: &dyn cloud::StorageService,
) -> &'static str {
    let Some(url) = project.repository_url.as_deref() else {
        return "repository-unrecorded";
    };
    let Ok((owner, repo)) = cli::projects::lifecycle::github_owner_name(url) else {
        return "source-is-not-github";
    };
    if repo != project.code
        || !cloud::workspace::is_valid_slug(&project.code)
        || !cloud::workspace::is_valid_slug(&owner)
    {
        return "source-project-mismatch";
    }
    if let Err(outcome) = check_github_archive(&owner, &repo).await {
        return outcome;
    }

    let Ok(client) = cli::remote::DocumentClient::connect(Some(base), &project.code).await else {
        return "project-inaccessible";
    };
    if client.project_id() != project.id {
        return "ambiguous-project";
    }
    let Ok(documents) = client.list_documents().await else {
        return "documents-inaccessible";
    };
    let commits: std::collections::BTreeSet<_> = documents
        .iter()
        .filter(|document| document.kind.as_deref() == Some("closed_repository"))
        .filter_map(|document| document.metadata.as_ref()?.get("commit_sha")?.as_str())
        .collect();
    let Some(commit) = commits.first().filter(|_| commits.len() == 1) else {
        return "missing-or-ambiguous-close-commit";
    };
    let Ok(scratch) = tempfile::tempdir() else {
        return "workspace-unavailable";
    };
    let mirror = scratch.path().join("mirror");
    let clone = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        tokio::process::Command::new("git")
            .args(["clone", "--mirror", "--", url])
            .arg(&mirror)
            .env("GIT_TERMINAL_PROMPT", "0")
            .kill_on_drop(true)
            .output(),
    )
    .await;
    if !matches!(clone, Ok(Ok(output)) if output.status.success()) {
        return "clone-failed";
    }
    if !matches!(text(&mirror, &["rev-parse", "HEAD"]), Ok(head) if verify_close_head(commit, &head))
    {
        return "close-commit-mismatch";
    }
    let Ok(refs) = text(
        &mirror,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    ) else {
        return "refs-unavailable";
    };
    let Ok(head) = text(&mirror, &["rev-parse", "--symbolic-full-name", "HEAD"]) else {
        return "refs-unavailable";
    };
    let fingerprint = sha256_hex(&[head.as_slice(), refs.as_slice()].concat());
    let object_prefix = format!("closed/{}/{fingerprint}/", project.id);
    let key = format!("{object_prefix}{}.bundle", uuid::Uuid::new_v4());
    let receipt = receipts.join(format!("{}-{fingerprint}.json", project.id));
    let result = if receipt.exists() {
        let verified_receipt = std::fs::read(&receipt)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<BundleReceipt>(&bytes).ok())
            .is_some_and(|receipt| {
                receipt.object.starts_with(&object_prefix)
                    && format!("{}\n", receipt.head).as_bytes() == head
                    && format!("{}\n", receipt.refs).as_bytes() == refs
            });
        if !verified_receipt {
            return "receipt-does-not-match-source";
        }
        restore_with_storage(&receipt, &scratch.path().join("restored"), storage).await
    } else {
        archive_with_storage(&mirror, &key, &receipt, storage, storage).await
    };
    if result.is_ok() {
        "verified"
    } else {
        "archive-or-restore-failed"
    }
}

async fn check_github_archive(owner: &str, repo: &str) -> Result<(), &'static str> {
    let metadata = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::process::Command::new("gh")
            .args([
                "api",
                "--hostname",
                "github.com",
                &format!("repos/{owner}/{repo}"),
            ])
            .env("GH_PROMPT_DISABLED", "1")
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let Ok(Ok(metadata)) = metadata else {
        return Err("github-inaccessible");
    };
    if !metadata.status.success() {
        return Err("github-inaccessible");
    }
    let Ok(metadata) = serde_json::from_slice::<GitHubArchive>(&metadata.stdout) else {
        return Err("github-metadata-unavailable");
    };
    if !metadata.archived {
        return Err("repository-not-archived");
    }

    Ok(())
}

fn verify_close_head(filed: &str, actual: &[u8]) -> bool {
    matches!(filed.len(), 40 | 64)
        && filed.bytes().all(|byte| byte.is_ascii_hexdigit())
        && std::str::from_utf8(actual).is_ok_and(|head| head.trim_end().eq_ignore_ascii_case(filed))
}

pub(in crate::devx) fn restore(receipt_path: &Path, output: &Path) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let storage = archive_storage().await?;
        restore_with_storage(receipt_path, output, &storage).await
    })
}

async fn restore_with_storage(
    receipt_path: &Path,
    output: &Path,
    reader: &dyn cloud::StorageService,
) -> Result<()> {
    let receipt: BundleReceipt = serde_json::from_slice(
        &std::fs::read(receipt_path).context("read private bundle receipt")?,
    )
    .context("parse bundle receipt")?;
    ensure!(receipt.format == 1, "unsupported bundle receipt format");
    ensure!(!output.exists(), "restore destination already exists");
    let expected_generation = receipt.generation;
    let read = reader
        .get_generation(&receipt.object, expected_generation)
        .await
        .context("read the receipt's exact archive generation")?;
    ensure!(
        read.generation == expected_generation,
        "archive generation does not match receipt"
    );
    ensure!(
        read.object.bytes.len() as u64 == receipt.size_bytes,
        "archive size does not match receipt"
    );
    ensure!(
        sha256_hex(&read.object.bytes) == receipt.sha256,
        "archive digest does not match receipt"
    );
    let refs = format!("{}\n", receipt.refs).into_bytes();
    let head = format!("{}\n", receipt.head).into_bytes();
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .context("open restore directory")?;
    let temp = tempfile::tempdir_in(&parent).context("create private restore workspace")?;
    let restored = verify_bundle_bytes(&read.object.bytes, temp.path(), &refs, &head)?;
    publish_mirror_no_clobber(&restored, output)?;
    println!("Git bundle restored and verified from the receipt's exact generation.");
    Ok(())
}

async fn archive_storage() -> Result<cloud::GcsStorage> {
    let config = cloud::GcsStorageConfig::forgejo_archives_from_env()
        .context("configure the explicit private Git archive bucket")?;
    cloud::GcsStorage::new_from_config(config)
        .await
        .context("configure Git archive storage")
}

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn write_receipt(path: &Path, receipt: &BundleReceipt) -> Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .context("open receipt directory")?;
    ensure!(!path.exists(), "receipt destination already exists");
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        temp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    temp.write_all(&serde_json::to_vec_pretty(receipt)?)?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(path)
        .context("publish private archive receipt without overwrite")?;
    Ok(())
}

fn verify_bundle_bytes(
    bytes: &[u8],
    workspace: &Path,
    expected_refs: &[u8],
    expected_head: &[u8],
) -> Result<std::path::PathBuf> {
    let bundle = workspace.join("readback.bundle");
    std::fs::write(&bundle, bytes).context("write private bundle read-back")?;
    let verifier = workspace.join("verifier.git");
    git(
        workspace,
        &["init".as_ref(), "--bare".as_ref(), verifier.as_os_str()],
    )?;
    git(
        &verifier,
        &["bundle".as_ref(), "verify".as_ref(), bundle.as_os_str()],
    )?;
    let restored = workspace.join("restored.git");
    git(
        workspace,
        &[
            "clone".as_ref(),
            "--mirror".as_ref(),
            bundle.as_os_str(),
            restored.as_os_str(),
        ],
    )?;
    text(&restored, &["fsck", "--full", "--strict"])?;
    ensure!(
        text(&restored, &["rev-parse", "--symbolic-full-name", "HEAD"])? == expected_head,
        "restored default branch does not match receipt"
    );
    ensure!(
        text(
            &restored,
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        )? == expected_refs,
        "restored refs do not match receipt"
    );
    Ok(restored)
}

fn create(mirror: &Path, output: &Path) -> Result<()> {
    let mirror = mirror.canonicalize().context("open local mirror")?;
    ensure!(!output.exists(), "bundle destination already exists");
    ensure!(
        text(&mirror, &["rev-parse", "--is-bare-repository"])? == b"true\n",
        "backup source must be a bare mirror"
    );
    ensure!(
        text(&mirror, &["config", "--bool", "remote.origin.mirror"])? == b"true\n",
        "backup source must be made with git clone --mirror"
    );
    ensure!(
        text(&mirror, &["rev-parse", "--is-shallow-repository"])? == b"false\n",
        "shallow repositories cannot provide a complete backup"
    );
    text(&mirror, &["fsck", "--full", "--strict"])?;
    let refs = text(
        &mirror,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    )?;
    let head = text(&mirror, &["rev-parse", "--symbolic-full-name", "HEAD"])?;
    ensure!(!refs.is_empty(), "cannot back up a repository without refs");
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .context("open backup destination directory")?;
    let work = tempfile::tempdir_in(parent).context("create private backup workspace")?;
    let bundle = work.path().join("repository.bundle");
    git(
        &mirror,
        &[
            "bundle".as_ref(),
            "create".as_ref(),
            bundle.as_os_str(),
            "--all".as_ref(),
        ],
    )?;
    git(
        &mirror,
        &["bundle".as_ref(), "verify".as_ref(), bundle.as_os_str()],
    )?;
    let restored = work.path().join("restored.git");
    git(
        work.path(),
        &[
            "clone".as_ref(),
            "--mirror".as_ref(),
            bundle.as_os_str(),
            restored.as_os_str(),
        ],
    )?;
    text(&restored, &["fsck", "--full", "--strict"])?;
    ensure!(
        text(&restored, &["rev-parse", "--symbolic-full-name", "HEAD"])? == head,
        "restored default branch does not match the backup source"
    );
    ensure!(
        text(
            &restored,
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        )? == refs,
        "restored refs do not match the backup source"
    );
    ensure!(
        text(
            &mirror,
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        )? == refs,
        "source refs changed during backup; retry from a stable mirror"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o600))?;
    }
    // Flush through a writable handle before publishing the file.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&bundle)?
        .sync_all()?;
    // A hard link publishes the verified file atomically without replacing any destination.
    std::fs::hard_link(&bundle, output).context("publish verified bundle without overwrite")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn restores_refs_to_relative_output_and_never_overwrites() -> Result<()> {
        let work = tempfile::tempdir_in(".")?;
        text(work.path(), &["init", "--initial-branch=main", "source"])?;
        let root = work.path().canonicalize()?;
        let source = root.join("source");
        text(
            &source,
            &[
                "-c",
                "user.name=Synthetic",
                "-c",
                "user.email=synthetic@example.test",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "Synthetic backup fixture",
            ],
        )?;
        text(&source, &["branch", "feature"])?;
        text(&source, &["tag", "v1"])?;
        text(&source, &["update-ref", "refs/pull/1/head", "HEAD"])?;
        let mirror = root.join("mirror.git");
        git(
            work.path(),
            &[
                "clone".as_ref(),
                "--mirror".as_ref(),
                source.as_os_str(),
                mirror.as_os_str(),
            ],
        )?;
        let output = work.path().join("backup.bundle");
        let expected_refs = text(
            &mirror,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        )?;
        let expected_head = text(&mirror, &["rev-parse", "--symbolic-full-name", "HEAD"])?;
        create(&mirror, &output)?;
        let proof = tempfile::tempdir_in(&root)?;
        let restored_from_bytes = verify_bundle_bytes(
            &std::fs::read(&output)?,
            proof.path(),
            &expected_refs,
            &expected_head,
        )?;
        assert_eq!(
            text(
                &restored_from_bytes,
                &["for-each-ref", "--format=%(refname) %(objectname)"]
            )?,
            expected_refs
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&output)?.permissions().mode() & 0o777,
                0o600
            );
        }
        let before = std::fs::read(&output)?;
        assert!(create(&mirror, &output).is_err());
        assert_eq!(std::fs::read(&output)?, before);
        let published = output.canonicalize()?;
        let restored = root.join("independent.git");
        git(
            work.path(),
            &[
                "clone".as_ref(),
                "--mirror".as_ref(),
                published.as_os_str(),
                restored.as_os_str(),
            ],
        )?;
        let refs = text(&restored, &["for-each-ref", "--format=%(refname)"])?;
        assert_eq!(
            String::from_utf8(refs)?,
            "refs/heads/feature\nrefs/heads/main\nrefs/pull/1/head\nrefs/tags/v1\n"
        );
        std::fs::write(
            mirror.join("shallow"),
            text(&mirror, &["rev-parse", "main"])?,
        )?;
        let shallow_output = work.path().join("shallow.bundle");
        assert!(create(&mirror, &shallow_output).is_err());
        assert!(!shallow_output.exists());
        Ok(())
    }

    #[test]
    fn rejects_non_mirror_and_empty_repository_without_publishing() -> Result<()> {
        let work = tempfile::tempdir()?;
        text(work.path(), &["init", "--bare", "source.git"])?;
        let source = work.path().join("source.git");
        let output = work.path().join("backup.bundle");
        assert!(create(&source, &output).is_err());
        text(&source, &["config", "remote.origin.mirror", "true"])?;
        assert!(create(&source, &output).is_err());
        assert!(!output.exists());
        Ok(())
    }

    #[derive(Default)]
    struct MemoryStorage {
        bytes: Mutex<Option<Vec<u8>>>,
        corrupt_read: bool,
    }

    #[async_trait::async_trait]
    impl cloud::StorageService for MemoryStorage {
        async fn put(
            &self,
            _key: &str,
            _bytes: &[u8],
            _content_type: &str,
        ) -> std::result::Result<(), cloud::StorageError> {
            Err(cloud::StorageError::Unsupported("overwrite put"))
        }
        async fn get(
            &self,
            key: &str,
        ) -> std::result::Result<cloud::StoredObject, cloud::StorageError> {
            let bytes = self
                .bytes
                .lock()
                .map_err(|_| cloud::StorageError::Gcs {
                    key: key.into(),
                    message: "lock poisoned".into(),
                })?
                .clone()
                .ok_or_else(|| cloud::StorageError::NotFound(key.into()))?;
            Ok(cloud::StoredObject {
                key: key.into(),
                bytes,
                content_type: BUNDLE_CONTENT_TYPE.into(),
            })
        }
        async fn delete(&self, _key: &str) -> std::result::Result<(), cloud::StorageError> {
            Err(cloud::StorageError::Unsupported("delete"))
        }
        async fn signed_url(
            &self,
            _key: &str,
            _expires_in: std::time::Duration,
        ) -> std::result::Result<String, cloud::StorageError> {
            Err(cloud::StorageError::Unsupported("signed_url"))
        }
        async fn put_immutable(
            &self,
            key: &str,
            bytes: &[u8],
            _content_type: &str,
        ) -> std::result::Result<cloud::ImmutableObject, cloud::StorageError> {
            let mut slot = self.bytes.lock().map_err(|_| cloud::StorageError::Gcs {
                key: key.into(),
                message: "lock poisoned".into(),
            })?;
            if slot.is_some() {
                return Err(cloud::StorageError::Gcs {
                    key: key.into(),
                    message: "precondition failed".into(),
                });
            }
            *slot = Some(bytes.to_vec());
            Ok(cloud::ImmutableObject {
                generation: 23,
                size_bytes: bytes.len() as u64,
            })
        }
        async fn get_generation(
            &self,
            key: &str,
            generation: u64,
        ) -> std::result::Result<cloud::VersionedObject, cloud::StorageError> {
            if generation != 23 {
                return Err(cloud::StorageError::NotFound(key.into()));
            }
            let mut object = self.get(key).await?;
            if self.corrupt_read && !object.bytes.is_empty() {
                object.bytes[0] ^= 1;
            }
            Ok(cloud::VersionedObject { object, generation })
        }
    }

    fn synthetic_mirror(root: &Path) -> Result<std::path::PathBuf> {
        text(root, &["init", "--initial-branch=main", "source"])?;
        let source = root.join("source");
        text(
            &source,
            &[
                "-c",
                "user.name=Synthetic",
                "-c",
                "user.email=synthetic@example.test",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "Synthetic archive fixture",
            ],
        )?;
        text(&source, &["branch", "feature"])?;
        text(&source, &["tag", "v1"])?;
        text(&source, &["update-ref", "refs/pull/9/head", "HEAD"])?;
        let mirror = root.join("mirror.git");
        git(
            root,
            &[
                "clone".as_ref(),
                "--mirror".as_ref(),
                source.as_os_str(),
                mirror.as_os_str(),
            ],
        )?;
        Ok(mirror)
    }

    #[tokio::test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    async fn archive_receipt_and_restore_follow_the_complete_verified_round_trip() -> Result<()> {
        let work = tempfile::tempdir()?;
        let mirror = synthetic_mirror(work.path())?;
        let receipt = work.path().join("archive.receipt");
        let output = work.path().join("restored.git");
        let storage = MemoryStorage::default();
        archive_with_storage(&mirror, "opaque-object-key", &receipt, &storage, &storage).await?;
        restore_with_storage(&receipt, &output, &storage).await?;
        assert_eq!(
            text(
                &mirror,
                &["for-each-ref", "--format=%(refname) %(objectname)"]
            )?,
            text(
                &output,
                &["for-each-ref", "--format=%(refname) %(objectname)"]
            )?
        );
        assert_eq!(
            text(&mirror, &["rev-parse", "--symbolic-full-name", "HEAD"])?,
            text(&output, &["rev-parse", "--symbolic-full-name", "HEAD"])?
        );
        Ok(())
    }

    #[tokio::test]
    async fn failed_read_back_never_publishes_a_receipt() -> Result<()> {
        let work = tempfile::tempdir()?;
        let mirror = synthetic_mirror(work.path())?;
        let receipt = work.path().join("archive.receipt");
        let storage = MemoryStorage {
            bytes: Mutex::new(None),
            corrupt_read: true,
        };
        assert!(
            archive_with_storage(&mirror, "opaque-object-key", &receipt, &storage, &storage)
                .await
                .is_err()
        );
        assert!(!receipt.exists());
        assert!(
            storage.bytes.lock().expect("lock").is_some(),
            "an immutable failed read-back may leave an orphan object"
        );
        Ok(())
    }

    #[test]
    fn read_back_verifier_rejects_ref_and_default_branch_mismatches() -> Result<()> {
        let work = tempfile::tempdir()?;
        let mirror = synthetic_mirror(work.path())?;
        let bundle = work.path().join("verified.bundle");
        create(&mirror, &bundle)?;
        let bytes = std::fs::read(bundle)?;
        let refs = text(
            &mirror,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        )?;
        let head = text(&mirror, &["rev-parse", "--symbolic-full-name", "HEAD"])?;
        let wrong_refs = tempfile::tempdir_in(work.path())?;
        assert!(verify_bundle_bytes(
            &bytes,
            wrong_refs.path(),
            b"refs/heads/main deadbeef\n",
            &head
        )
        .is_err());
        let wrong_head = tempfile::tempdir_in(work.path())?;
        assert!(
            verify_bundle_bytes(&bytes, wrong_head.path(), &refs, b"refs/heads/other\n").is_err()
        );
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn no_clobber_publication_refuses_an_existing_empty_directory() -> Result<()> {
        let work = tempfile::tempdir()?;
        let source = work.path().join("verified.git");
        std::fs::create_dir(&source)?;
        std::fs::write(source.join("proof"), b"verified")?;
        let destination = work.path().join("restore.git");
        std::fs::create_dir(&destination)?;
        let error = publish_mirror_no_clobber(&source, &destination)
            .expect_err("existing empty path must not be replaced");
        assert!(error.to_string().contains("publish verified restore"));
        assert!(std::fs::read_dir(&destination)?.next().is_none());
        assert_eq!(std::fs::read(source.join("proof"))?, b"verified");
        Ok(())
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn no_clobber_publication_is_explicitly_unsupported() {
        let error = publish_mirror_no_clobber(Path::new("source"), Path::new("destination"))
            .expect_err("unsupported platform must not use a weaker rename");
        assert!(error.to_string().contains("unsupported"));
    }
}

#[cfg(test)]
mod close_tests {
    use super::verify_close_head;

    #[test]
    fn closed_source_requires_the_exact_filed_head() {
        let filed = "a".repeat(40);
        assert!(verify_close_head(&filed, format!("{filed}\n").as_bytes()));
        assert!(!verify_close_head(&filed, "b".repeat(40).as_bytes()));
        assert!(!verify_close_head("aaaa", b"aaaa\n"));
    }
}
