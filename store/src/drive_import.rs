//! Lawyer-triggered ingestion from a Project's optional Drive dropbox.
//!
//! Drive is an ingest source, not a document store: object storage stays the
//! working-file authority. This module lists the files under the Project's
//! recorded folder, checks each download against Drive's own size and MD5
//! receipt, screens it for malware, and files it through the canonical
//! document asset lane. Every later read uses the asset row and object
//! storage, never Drive.
//!
//! The folder is verified by id inside the deployment's Projects shared drive,
//! never by name, so a Project code and its folder's display name need not
//! agree. A rerun is safe: unchanged bytes are a no-op, a new Drive version
//! becomes a new revision of the same document, and an interrupted pass
//! converges when it runs again.
//!
//! A new Drive file receives a stable document identity derived from its
//! case-sensitive provider ID. Existing identities, including historical flat
//! slugs and lawyer reslugs, are preserved. Renames and moves update the
//! checkout path without splitting the document's revision chain. A change
//! of file format requires an explicit decision instead of a silent new identity.
//!
//! The Drive file ID, version, source path, and checksum remain asset metadata.
//! Unsafe names and conflicting checkout paths are refused. Client Submissions
//! enter Client Production, while nested firm files retain their Matter Folder.
//! A dry run checks the same access, receipts, bounds, and screening without writes.

use crate::project_folders::{self, ProjectFolderSlot};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use cloud::{DriveService, StorageService};
use serde::Serialize;
use serde_json::json;
use thiserror::Error;
use uuid::Uuid;

use crate::assets::{self, Filed, RevisionError};
use crate::document_pointers::MatterFolder;
use crate::documents::{self, DocumentIdentity, IngestArgs};
use crate::persons;
use crate::projects;
use crate::surreal::SurrealDb;

/// Maximum number of Drive files one import pass may admit.
pub const MAX_FILES: usize = 50;
/// Maximum size of one Drive file admitted to the buffered ingest seam.
pub const MAX_FILE_BYTES: usize = cloud::MAX_DRIVE_DOWNLOAD_BYTES;
/// Maximum total bytes one import pass may buffer and file.
pub const MAX_TOTAL_BYTES: usize = 500 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum DriveImportError {
    #[error("Drive: {0}")]
    Drive(#[from] cloud::DriveError),
    #[error("project: {0}")]
    Project(#[from] projects::ProjectStoreError),
    #[error("person: {0}")]
    Person(#[from] persons::PersonError),
    #[error("asset: {0}")]
    Asset(#[from] RevisionError),
    #[error("reading the document's head: {0}")]
    Head(#[from] assets::AssetError),
    #[error("Project has no Drive ingest folder")]
    MissingFolder,
    #[error("verified Project Folder storage is unavailable")]
    FolderStoreUnavailable,
    #[error("current Project Folder access could not be verified")]
    FolderAccess(#[source] crate::project_folder_reconcile::FolderReconcileError),
    #[error("Project Folder lookup failed")]
    FolderStore(#[from] crate::project_folders::ProjectFolderError),
    #[error("Drive import is not authorized for this Project")]
    Unauthorized,
    #[error("stored Drive folder is not a live folder in this deployment's Projects drive")]
    FolderMismatch,
    #[error("the malware screen is unavailable; nothing further was filed")]
    ScreenUnavailable,
    #[error("Drive import contains {actual} files; the maximum is {MAX_FILES}")]
    TooManyFiles { actual: usize },
    #[error("Drive file `{file_id}` is {actual} bytes; the maximum is {MAX_FILE_BYTES}")]
    FileTooLarge { file_id: String, actual: u64 },
    #[error("Drive import exceeds the maximum of {MAX_TOTAL_BYTES} bytes")]
    BatchTooLarge,
}

/// Inputs chosen by the operator for a Drive import pass.
#[derive(Debug, Clone, Copy)]
pub struct DriveImportArgs<'a> {
    pub kind: &'a str,
    pub visibility: &'a str,
    pub description: Option<&'a str>,
    /// Run the pass to its decision points and write nothing: every download,
    /// receipt check, screen, and refusal is real, and each reported import
    /// predicts whether a real pass would file a revision or change nothing.
    pub dry_run: bool,
}

/// The malware screen a Drive file passes before it is filed.
#[async_trait]
pub trait IngestScreen: Send + Sync {
    /// `Ok(true)` clean, `Ok(false)` malware found, `Err` the screen could not
    /// decide, which stops the pass.
    async fn is_clean(&self, bytes: &[u8]) -> Result<bool, ()>;
}

/// The asset identity produced for one Drive file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportedDriveFile {
    pub drive_file_id: String,
    pub asset_id: Uuid,
    pub filename: String,
    /// Original root-relative provider path, retained independently of display name.
    pub relative_path: String,
    /// The matter folder a Project checkout downloads this document into,
    /// resolved from the pass's document kind (LAW-189).
    pub matter_folder: MatterFolder,
    /// `false` when the bytes matched the document's newest revision and
    /// nothing was written.
    pub changed: bool,
}

/// A Drive file that was not filed, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefusedDriveFile {
    pub drive_file_id: String,
    /// `receipt_mismatch` when the download disagrees with Drive's size or
    /// MD5, `malware_found` when the screen flagged it, `name_collision`
    /// when another file in the same pass names the same document
    /// case-insensitively, and `unusable_name` when the name could not be a
    /// document in a Project checkout.
    pub reason: &'static str,
}

/// What one import pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DriveImportReport {
    pub imported: Vec<ImportedDriveFile>,
    pub refused: Vec<RefusedDriveFile>,
}

/// Import the current direct children of a Project's Drive folder.
///
/// The actor is resolved from the current Person row and must be authorized
/// for the Project through the existing lawyer-tier Project access rule. The
/// recorded folder must be a live folder in the deployment's Projects drive
/// before Drive lists or downloads any file.
///
/// Each file receives a stable Drive-specific document identity, so a
/// changed Drive file becomes a new asset revision while an unchanged retry
/// is a no-op. A download whose size or MD5 disagrees with Drive's listing is
/// refused without filing anything, as is one the screen flags. The stored
/// row carries `source = "drive"` and the Drive file id, version, modified
/// time, and checksum; later reads use only the asset row and object storage.
///
/// With `args.dry_run` the pass is read-only: every download, receipt check,
/// and screen runs, but nothing is filed, and each reported import predicts
/// whether a real pass would write a revision or change nothing.
///
/// # Errors
/// [`DriveImportError`] when the actor is not authorized, the folder is
/// missing or foreign, a bound is exceeded, Drive or storage fails, or the
/// screen cannot decide. Files filed before the error stay filed; a rerun
/// converges.
pub async fn import_project_files(
    db: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    drive: &dyn DriveService,
    screen: &dyn IngestScreen,
    project_id: Uuid,
    actor_id: Uuid,
    args: &DriveImportArgs<'_>,
) -> Result<DriveImportReport, DriveImportError> {
    // Authorization precedes configuration or provider observations.
    let actor = persons::find_by_id(db, actor_id)
        .await?
        .ok_or(DriveImportError::Unauthorized)?;
    if !matches!(
        crate::access::matter_lens(db, Some(actor.id), actor.role, project_id).await,
        Ok(Some(crate::access::ProjectLens::Lawyer))
    ) {
        return Err(DriveImportError::Unauthorized);
    }
    let pg = crate::postgres::PostgresDb::from_env()
        .await
        .map_err(|_| DriveImportError::FolderStoreUnavailable)?;
    import_project_files_with_folders(db, &pg, storage, drive, screen, project_id, actor_id, args)
        .await
}

/// Import only verified Project Folder roots using an explicit database handle.
///
/// # Errors
/// Authorization, folder verification, provider, receipt or storage failures.
// Keep admission, bounded enumeration, and per-file refusal ordering explicit.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub async fn import_project_files_with_folders(
    db: &SurrealDb,
    pg: &crate::postgres::PostgresDb,
    storage: &Arc<dyn StorageService>,
    drive: &dyn DriveService,
    screen: &dyn IngestScreen,
    project_id: Uuid,
    actor_id: Uuid,
    args: &DriveImportArgs<'_>,
) -> Result<DriveImportReport, DriveImportError> {
    let project = projects::find_by_id(db, project_id)
        .await?
        .ok_or(projects::ProjectStoreError::NoSuchProject(project_id))?;
    let Some(actor) = persons::find_by_id(db, actor_id).await? else {
        return Err(DriveImportError::Unauthorized);
    };
    if !matches!(
        crate::access::matter_lens(db, Some(actor.id), actor.role, project_id).await,
        Ok(Some(crate::access::ProjectLens::Lawyer))
    ) {
        return Err(DriveImportError::Unauthorized);
    }
    crate::project_folder_reconcile::verify_for_import(db, pg, drive, project_id)
        .await
        .map_err(DriveImportError::FolderAccess)?;
    let root = project_folders::get_verified(pg, project_id, ProjectFolderSlot::Root)
        .await?
        .ok_or(DriveImportError::MissingFolder)?;
    let submissions =
        project_folders::get_verified(pg, project_id, ProjectFolderSlot::ClientSubmissions)
            .await?
            .ok_or(DriveImportError::MissingFolder)?;
    if project.drive_folder_id.as_deref() != Some(root.drive_folder_id.as_str()) {
        return Err(DriveImportError::FolderMismatch);
    }
    let existing_assets = assets::for_project_including_deleted(db, project_id).await?;
    let mut listed_files = Vec::new();
    let mut paths = HashMap::new();
    let mut source_roots = HashMap::new();
    for (folder, client_intake) in [(&root, false), (&submissions, true)] {
        if projects::drive_folder_recorded_elsewhere(db, project_id, &folder.drive_folder_id)
            .await?
            || project_folders::recorded_elsewhere(pg, project_id, &folder.drive_folder_id).await?
            || !drive.is_ingest_folder(&folder.drive_folder_id).await?
        {
            return Err(DriveImportError::FolderMismatch);
        }
        let access = drive.folder_access(&folder.drive_folder_id).await?;
        if !access.can_list_children
            || folder
                .parent_drive_folder_id
                .as_ref()
                .is_some_and(|parent| access.parent_ids.as_slice() != [parent.clone()])
        {
            return Err(DriveImportError::FolderMismatch);
        }
        for entry in drive.list_tree(&folder.drive_folder_id).await? {
            if !usable_relative_path(&entry.relative_path)
                && (usable_name(&entry.file.name) || entry.relative_path != entry.file.name)
            {
                return Err(DriveImportError::FolderMismatch);
            }
            // Provider ancestry includes the selected root and every containing
            // folder, allowing stored cross-Project bindings to fail closed.
            for ancestor in &entry.ancestor_ids {
                if project_folders::recorded_elsewhere(pg, project_id, ancestor).await? {
                    return Err(DriveImportError::FolderMismatch);
                }
            }
            let path = if client_intake {
                format!("Client Production/{}", entry.relative_path)
            } else if classify_path(&entry.relative_path).is_some() {
                entry.relative_path.clone()
            } else {
                format!(
                    "{}/{}",
                    MatterFolder::for_kind(args.kind).as_str(),
                    entry.relative_path
                )
            };
            source_roots.insert(entry.file.id.clone(), folder.drive_folder_id.clone());
            if paths
                .insert(entry.file.id.clone(), (path, entry.relative_path))
                .is_some()
            {
                return Err(DriveImportError::FolderMismatch);
            }
            listed_files.push(entry.file);
        }
    }
    if listed_files.len() > MAX_FILES {
        return Err(DriveImportError::TooManyFiles {
            actual: listed_files.len(),
        });
    }
    // Collisions use complete checkout paths, never only leaf filenames.
    let collisions = colliding_paths(&listed_files, &paths);

    let mut total_bytes = 0usize;
    let mut report = DriveImportReport::default();
    for file in listed_files {
        if collisions.contains(&file.id) {
            report.refused.push(RefusedDriveFile {
                drive_file_id: file.id,
                reason: "name_collision",
            });
            continue;
        }
        if !usable_name(&file.name) {
            report.refused.push(RefusedDriveFile {
                drive_file_id: file.id,
                reason: "unusable_name",
            });
            continue;
        }
        let (checkout_path, source_path) = paths
            .get(&file.id)
            .ok_or(DriveImportError::FolderMismatch)?
            .clone();
        if existing_assets.iter().any(|asset| {
            asset.deleted_at.is_some()
                && asset
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("drive_file_id"))
                    .and_then(serde_json::Value::as_str)
                    == Some(file.id.as_str())
        }) {
            report.refused.push(RefusedDriveFile {
                drive_file_id: file.id,
                reason: "deleted_source",
            });
            continue;
        }
        let matching: Vec<_> = existing_assets
            .iter()
            .filter(|asset| {
                asset
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("drive_file_id"))
                    .and_then(serde_json::Value::as_str)
                    == Some(file.id.as_str())
            })
            .collect();
        let identities: HashSet<_> = matching
            .iter()
            .filter_map(|asset| asset.slug.as_deref())
            .collect();
        if identities.len() > 1 || (!matching.is_empty() && identities.is_empty()) {
            report.refused.push(RefusedDriveFile {
                drive_file_id: file.id,
                reason: "ambiguous_source_identity",
            });
            continue;
        }
        let slug = if let Some(slug) = identities.iter().next() {
            (*slug).to_owned()
        } else if let Some(slug) = reserved_drive_slug(&file) {
            slug
        } else {
            report.refused.push(RefusedDriveFile {
                drive_file_id: file.id,
                reason: "unsupported_source_identity",
            });
            continue;
        };
        // Existing identities, including lawyer reslugs and historical flat slugs,
        // always win. Format-changing names cannot silently rename that identity.
        if documents::validate_document_slug(&file.name, &slug).is_err() {
            report.refused.push(RefusedDriveFile {
                drive_file_id: file.id,
                reason: "source_format_changed",
            });
            continue;
        }
        if let Some(head) = assets::current(db, project_id, &slug, assets::Lens::Lawyer).await? {
            if head
                .metadata
                .as_ref()
                .and_then(|m| m.get("drive_file_id"))
                .and_then(serde_json::Value::as_str)
                .is_some_and(|id| id != file.id.as_str())
                || (matching.is_empty()
                    && head
                        .metadata
                        .as_ref()
                        .and_then(|m| m.get("drive_file_id"))
                        .and_then(serde_json::Value::as_str)
                        .is_none())
            {
                report.refused.push(RefusedDriveFile {
                    drive_file_id: file.id,
                    reason: "name_collision",
                });
                continue;
            }
        }
        if existing_assets.iter().any(|asset| {
            asset.deleted_at.is_none()
                && asset.documents_path().eq_ignore_ascii_case(&checkout_path)
                && asset
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("drive_file_id"))
                    .and_then(serde_json::Value::as_str)
                    != Some(file.id.as_str())
        }) {
            report.refused.push(RefusedDriveFile {
                drive_file_id: file.id,
                reason: "name_collision",
            });
            continue;
        }
        validate_declared_size(&file.id, file.size_bytes, total_bytes)?;

        let downloaded = drive.download_file(&file.id).await?;
        if downloaded.bytes.len() > MAX_FILE_BYTES {
            return Err(DriveImportError::FileTooLarge {
                file_id: file.id,
                actual: downloaded.bytes.len() as u64,
            });
        }
        total_bytes = total_bytes
            .checked_add(downloaded.bytes.len())
            .ok_or(DriveImportError::BatchTooLarge)?;
        if total_bytes > MAX_TOTAL_BYTES {
            return Err(DriveImportError::BatchTooLarge);
        }
        let source_root = source_roots
            .get(&file.id)
            .ok_or(DriveImportError::FolderMismatch)?
            .clone();
        match ingest_one(
            db,
            storage,
            screen,
            project_id,
            args,
            file,
            &downloaded,
            &slug,
            &checkout_path,
            &source_path,
            &source_root,
        )
        .await?
        {
            Outcome::Imported(imported) => report.imported.push(imported),
            Outcome::Refused(refused) => report.refused.push(refused),
        }
    }
    Ok(report)
}

enum Outcome {
    Imported(ImportedDriveFile),
    Refused(RefusedDriveFile),
}

/// Check, screen, and file one downloaded Drive file.
#[allow(clippy::too_many_arguments)]
async fn ingest_one(
    db: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    screen: &dyn IngestScreen,
    project_id: Uuid,
    args: &DriveImportArgs<'_>,
    file: cloud::DriveFile,
    downloaded: &cloud::DriveFileDownload,
    slug: &str,
    checkout_path: &str,
    source_path: &str,
    source_root: &str,
) -> Result<Outcome, DriveImportError> {
    if !receipt_matches(&file, &downloaded.bytes) {
        return Ok(Outcome::Refused(RefusedDriveFile {
            drive_file_id: file.id,
            reason: "receipt_mismatch",
        }));
    }
    match screen.is_clean(&downloaded.bytes).await {
        Ok(true) => {}
        Ok(false) => {
            return Ok(Outcome::Refused(RefusedDriveFile {
                drive_file_id: file.id,
                reason: "malware_found",
            }));
        }
        Err(()) => return Err(DriveImportError::ScreenUnavailable),
    }
    let matter_folder =
        classify_path(checkout_path).unwrap_or_else(|| MatterFolder::for_kind(args.kind));
    if args.dry_run {
        return Ok(Outcome::Imported(
            plan_one(
                db,
                project_id,
                args,
                file,
                &downloaded.bytes,
                matter_folder,
                slug,
                source_path,
            )
            .await?,
        ));
    }
    let content_type = if downloaded.content_type == "application/octet-stream"
        && !file.mime_type.trim().is_empty()
    {
        file.mime_type.as_str()
    } else {
        downloaded.content_type.as_str()
    };
    let identity = DocumentIdentity {
        slug: Some(slug),
        published_at: None,
        metadata: Some(json!({
            "drive_file_id": file.id,
            "drive_relative_path": source_path,
            "drive_source_root_id": source_root,
            "drive_checkout_path": checkout_path,
            "drive_matter_folder": matter_folder.as_str(),
            "drive_modified_time": file.modified_time,
            "drive_version": file.version,
            "drive_md5_checksum": file.md5_checksum,
        })),
        derived_from: None,
        transcript_quality: None,
        signing: None,
        notarization: Vec::new(),
        evidence: None,
    };
    let ingest = IngestArgs {
        project_id,
        source: documents::source::DRIVE,
        filename: &file.name,
        kind: args.kind,
        content_type,
        description: args.description,
        secondary_storage_key: None,
        visibility: args.visibility,
    };
    let (asset_id, changed) =
        match assets::file_revision(db, storage, &ingest, &identity, &downloaded.bytes).await? {
            Filed::Revision(document) => (document.asset_id, true),
            Filed::Unchanged { asset_id } => (asset_id, false),
        };
    assets::refresh_drive_import(
        db,
        project_id,
        asset_id,
        &file.id,
        identity
            .metadata
            .as_ref()
            .ok_or(DriveImportError::FolderMismatch)?,
        &file.name,
    )
    .await?;
    Ok(Outcome::Imported(ImportedDriveFile {
        drive_file_id: file.id,
        asset_id,
        matter_folder,
        filename: file.name,
        relative_path: source_path.to_owned(),
        changed,
    }))
}

/// The read-only prediction [`ingest_one`] reports for a `dry_run` pass: what
/// [`crate::assets::file_revision`] would do with these bytes — its rules 2
/// and 3, read and not written — or that a real pass would file the first
/// revision. A planned file carries the nil asset id, because nothing was
/// filed.
#[allow(clippy::too_many_arguments)]
async fn plan_one(
    db: &SurrealDb,
    project_id: Uuid,
    args: &DriveImportArgs<'_>,
    file: cloud::DriveFile,
    bytes: &[u8],
    matter_folder: MatterFolder,
    slug: &str,
    source_path: &str,
) -> Result<ImportedDriveFile, DriveImportError> {
    let head = assets::current(db, project_id, slug, assets::Lens::Lawyer).await?;
    if let Some(head) = head {
        if let Some(existing) = head.kind.as_deref() {
            if existing != args.kind {
                return Err(RevisionError::KindChanged {
                    slug: file.name.clone(),
                    existing: existing.to_string(),
                    attempted: args.kind.to_string(),
                }
                .into());
            }
        }
        if head.sha256_hex == assets::sha256_hex(bytes) {
            return Ok(ImportedDriveFile {
                drive_file_id: file.id,
                asset_id: head.id,
                matter_folder,
                filename: file.name,
                relative_path: source_path.to_owned(),
                changed: false,
            });
        }
    }
    Ok(ImportedDriveFile {
        drive_file_id: file.id,
        asset_id: Uuid::nil(),
        matter_folder,
        filename: file.name,
        relative_path: source_path.to_owned(),
        changed: true,
    })
}

/// Whether downloaded bytes match what Drive listed. A listed size or MD5 that
/// disagrees is a malformed or stale receipt; a Google-native file with no
/// MD5 is checked by size alone.
fn receipt_matches(file: &cloud::DriveFile, bytes: &[u8]) -> bool {
    file.size_bytes
        .is_none_or(|size| size == bytes.len() as u64)
        && file
            .md5_checksum
            .as_deref()
            .is_none_or(|md5| md5.eq_ignore_ascii_case(&cloud::md5_hex(bytes)))
}

fn validate_declared_size(
    file_id: &str,
    size_bytes: Option<u64>,
    total_bytes: usize,
) -> Result<(), DriveImportError> {
    let Some(size) = size_bytes else {
        return Ok(());
    };
    if size > MAX_FILE_BYTES as u64 {
        return Err(DriveImportError::FileTooLarge {
            file_id: file_id.to_owned(),
            actual: size,
        });
    }
    let size = usize::try_from(size).map_err(|_| DriveImportError::BatchTooLarge)?;
    if total_bytes
        .checked_add(size)
        .is_none_or(|total| total > MAX_TOTAL_BYTES)
    {
        return Err(DriveImportError::BatchTooLarge);
    }
    Ok(())
}

/// Whether a Drive filename can be a document identity in a Project checkout,
/// which names a downloaded document by its slug: no separators, no control
/// characters, no `.`/`..`, and no more than 255 bytes. A name the checkout
/// tree could never write is refused before its bytes are downloaded.
#[must_use]
fn usable_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control())
        && name.len() <= 255
}

fn reserved_drive_slug(file: &cloud::DriveFile) -> Option<String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    if file.id.is_empty() || file.id.len() > 120 {
        return None;
    }
    // Case-sensitive provider IDs must not alias on a case-insensitive macOS checkout.
    let mut encoded = String::with_capacity(file.id.len() * 2);
    for byte in file.id.bytes() {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 15)]));
    }
    let extension = std::path::Path::new(&file.name).extension()?.to_str()?;
    let slug = format!("drive/{encoded}.{}", extension.to_ascii_lowercase());
    if slug.rsplit('/').next()?.len() > 255 {
        return None;
    }
    documents::validate_document_slug(&file.name, &slug).ok()?;
    Some(slug)
}

fn usable_relative_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= 4096 && path.split('/').all(usable_name)
}

fn classify_path(path: &str) -> Option<MatterFolder> {
    MatterFolder::TREE
        .iter()
        .copied()
        .filter(|folder| {
            path.strip_prefix(folder.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
        })
        .max_by_key(|folder| folder.as_str().len())
}

fn colliding_paths(
    files: &[cloud::DriveFile],
    paths: &HashMap<String, (String, String)>,
) -> HashSet<String> {
    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    for file in files {
        if let Some((path, _)) = paths.get(&file.id) {
            groups
                .entry(path.to_ascii_lowercase())
                .or_default()
                .push(file.id.clone());
        }
    }
    groups
        .into_values()
        .filter(|ids| ids.len() > 1)
        .flatten()
        .collect()
}

/// The drive file ids whose names collide case-insensitively: every member of
/// a same-name group, because a pass has no honest way to pick which one the
/// lawyer meant. ASCII-only folding matches what the checkout filesystems
/// (APFS, NTFS) fold; anything finer is one rename away in Drive.
#[must_use]
#[cfg(test)]
fn colliding_names(files: &[cloud::DriveFile]) -> HashSet<String> {
    let mut groups: HashMap<String, Vec<&str>> = HashMap::new();
    for file in files {
        groups
            .entry(file.name.to_ascii_lowercase())
            .or_default()
            .push(&file.id);
    }
    groups
        .into_values()
        .filter(|ids| ids.len() > 1)
        .flatten()
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{colliding_names, usable_name};
    use cloud::DriveFile;

    fn file(id: &str, name: &str) -> DriveFile {
        DriveFile {
            id: id.to_string(),
            name: name.to_string(),
            mime_type: "application/pdf".into(),
            size_bytes: Some(4),
            modified_time: None,
            md5_checksum: None,
            version: None,
        }
    }

    #[test]
    fn a_checkout_writable_name_is_usable_and_its_escapes_are_not() {
        assert!(usable_name("signed agreement.pdf"));
        assert!(usable_name("LICENSE"));
        assert!(usable_name(&"x".repeat(255)));
        let long = "x".repeat(256);
        for unusable in [
            "",
            ".",
            "..",
            "a/b.pdf",
            "a\\b.pdf",
            "a\u{7}b.pdf",
            long.as_str(),
        ] {
            assert!(!usable_name(unusable), "{unusable:?} must be refused");
        }
    }

    #[test]
    fn every_member_of_a_same_name_group_collides_case_insensitively() {
        let files = [
            file("a", "report.pdf"),
            file("b", "Report.PDF"),
            file("c", "notes.md"),
            file("d", "REPORT.pdf"),
        ];
        let mut collided = colliding_names(&files).into_iter().collect::<Vec<_>>();
        collided.sort();
        assert_eq!(collided, ["a", "b", "d"]);
    }

    #[test]
    fn distinct_names_never_collide() {
        let files = [file("a", "a.pdf"), file("b", "b.pdf")];
        assert!(colliding_names(&files).is_empty());
    }
}

#[cfg(test)]
mod recursive_path_tests {
    use super::*;
    #[test]
    fn nested_paths_are_safe_and_longest_canonical_folder_wins() {
        assert!(usable_relative_path(
            "Work Product/Research (Work Product)/folder/opinion.pdf"
        ));
        for path in ["/a.pdf", "a/../b.pdf", "a//b.pdf", "a/./b.pdf", "a\\b.pdf"] {
            assert!(!usable_relative_path(path), "{path}");
        }
        assert_eq!(
            classify_path("Work Product/Research (Work Product)/folder/opinion.pdf"),
            Some(
                MatterFolder::parse("Work Product/Research (Work Product)")
                    .expect("canonical folder")
            )
        );
        assert_eq!(classify_path("Work Productive/a.pdf"), None);
    }
    #[test]
    fn same_basename_in_different_directories_is_not_a_collision() {
        let files = vec![file("a"), file("b"), file("c")];
        let paths = HashMap::from([
            (
                "a".to_owned(),
                ("Client Production/one/notice.pdf".to_owned(), String::new()),
            ),
            (
                "b".to_owned(),
                ("Client Production/two/notice.pdf".to_owned(), String::new()),
            ),
            (
                "c".to_owned(),
                ("client production/ONE/NOTICE.pdf".to_owned(), String::new()),
            ),
        ]);
        assert_eq!(
            colliding_paths(&files, &paths),
            HashSet::from(["a".to_owned(), "c".to_owned()])
        );
    }
    #[test]
    fn source_reservation_is_case_safe_and_ignores_mutable_name() {
        let mut upper = file("A");
        let lower = file("a");
        assert_eq!(reserved_drive_slug(&upper).as_deref(), Some("drive/41.pdf"));
        assert_eq!(reserved_drive_slug(&lower).as_deref(), Some("drive/61.pdf"));
        upper.name = "renamed.pdf".to_owned();
        assert_eq!(reserved_drive_slug(&upper).as_deref(), Some("drive/41.pdf"));
    }
    fn file(id: &str) -> cloud::DriveFile {
        cloud::DriveFile {
            id: id.to_owned(),
            name: "notice.pdf".to_owned(),
            mime_type: "application/pdf".to_owned(),
            size_bytes: None,
            modified_time: None,
            md5_checksum: None,
            version: None,
        }
    }
}
