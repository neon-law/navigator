//! The live half of `navigator project gate --check`.
//!
//! Committed pointers and the live asset rows are checked in both directions.
//! A drifted pointer, a live document with no pointer, and a missing
//! `documents/.gitignore` are rewritten in the checkout. The live site is never
//! written. A live document whose pointer the repository removed is a pending
//! deletion: it is reported, never rewritten, and `navigator project sync` asks
//! whether to soft-delete it. A missing storage object is
//! `navigator site document repair`. A live row with no slug is
//! `navigator site document slug`. Under `--ci` the same fixes are reported and
//! nothing is written.
//!
//! External-id checks (an invoice, a completed envelope) are the server's
//! `integrations` list on `GET /app/api/projects/{id}/documents/integrity`.
//! A pointer has no external-id field, so that list is empty.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use store::document_pointers::{DocumentPointer, PointerVersion};
use uuid::Uuid;

use crate::document_deletion::{short_commit, RemovedPointers};
use crate::document_read::{discover_pointers, slug_from_pointer};
use crate::document_sync::{read_manifest, read_pointer, slash_path, write_pointer_atomically};
use crate::remote::{DocumentClient, IntegrityAsset, RevisionSummary, RevisionsResponse};

const REWRITE: &str = "rewrite the drifted pointer";
const WRITE_POINTER: &str = "write the missing pointer";
const MOVE_LEGACY_POINTERS: &str = "move the legacy pointers from documents/ to blobs/";

/// One live-record failure. `location` is the pointer path, or the asset id
/// when the row has no slug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckFailure {
    pub(crate) location: String,
    pub(crate) message: String,
}

/// One checkout edit the check made, or would make when it is not allowed to
/// write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckFix {
    pub(crate) path: String,
    pub(crate) action: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub(crate) errors: Vec<CheckFailure>,
    pub(crate) fixes: Vec<CheckFix>,
}

impl Outcome {
    /// Hard errors always fail. A fix fails only when the run is not allowed
    /// to write, which is `--ci`.
    #[cfg(test)]
    #[must_use]
    fn rejects(&self, ci: bool) -> bool {
        !self.errors.is_empty() || (ci && !self.fixes.is_empty())
    }
}

enum Local {
    Pointer {
        relative: String,
        absolute: PathBuf,
        pointer: Box<DocumentPointer>,
    },
    Unusable {
        relative: String,
    },
}

impl Local {
    fn relative(&self) -> &str {
        match self {
            Self::Pointer { relative, .. } | Self::Unusable { relative } => relative,
        }
    }
}

/// Read `navigator.yaml` and run the live document check.
///
/// `write` is false under `--ci`: the outcome lists the fixes and the
/// checkout is left alone. `deep` asks the server to re-hash each object.
pub(crate) async fn run(dir: &Path, ci: bool, deep: bool) -> Result<Outcome> {
    let (project, host) = read_manifest(dir)?;
    let host = host
        .filter(|host| !host.trim().is_empty())
        .ok_or_else(|| anyhow!("navigator.yaml names no host"))?;
    let client = if ci {
        let (base, token) = crate::remote::resolve_ci_document(&host).await?;
        DocumentClient::with_credential(base, token, &project).await?
    } else {
        DocumentClient::connect(Some(&host), &project).await?
    };
    check(dir, &client, !ci, deep).await
}

/// Compare `dir` with the live records `client` can see.
pub(crate) async fn check(
    dir: &Path,
    client: &DocumentClient,
    write: bool,
    deep: bool,
) -> Result<Outcome> {
    let mut fixes = Vec::new();
    if let Some(outcome) = reconcile_legacy_layout(dir, write, &mut fixes)? {
        return Ok(outcome);
    }
    let integrity = client.integrity(deep).await?;
    let mut errors = Vec::new();
    let mut locals = index_pointers(dir, &mut errors)?;
    let live_slugs = live_slugs(&integrity.assets);

    for asset in &integrity.assets {
        for message in row_problems(asset, deep) {
            errors.push(CheckFailure {
                location: location_for(asset, &locals),
                message,
            });
        }
        if let Some(message) = transcript_source_problem(asset, &integrity.assets) {
            errors.push(CheckFailure {
                location: location_for(asset, &locals),
                message,
            });
        }
    }
    for finding in &integrity.integrations {
        errors.push(CheckFailure {
            location: finding.asset_id.to_string(),
            message: format!(
                "{}: {}: {}",
                finding.integration, finding.outcome, finding.detail
            ),
        });
    }

    let removed = RemovedPointers::scan(dir);
    let mut claimed: BTreeSet<String> = BTreeSet::new();
    let (resolved, colliding) = resolve_live_slugs(client, &live_slugs, &locals, &mut errors).await;
    claimed.extend(colliding.iter().cloned());
    for (slug, live, key) in resolved {
        if colliding.contains(&key) {
            continue;
        }
        reconcile_slug(
            dir,
            &slug,
            key,
            &live,
            &integrity.assets,
            &mut locals,
            write,
            deep,
            &mut errors,
            &mut fixes,
            &mut claimed,
            &removed,
        )?;
    }
    for (slug, local) in &locals {
        if claimed.contains(slug) {
            continue;
        }
        let Local::Pointer { pointer, .. } = local else {
            continue;
        };
        // LAW-82: an Authority-backed pointer never has a live Project row —
        // its bytes are archived under a global Authority, never filed on
        // this Project as a document — so there is no live chain to report
        // missing.
        if pointer.authority_id.is_some() {
            continue;
        }
        let message = match client.list_revisions(slug).await {
            Ok(live) if live.revisions.iter().any(|revision| revision.operative) => {
                "the live chain is missing from the integrity report".to_string()
            }
            _ => "no live document for this pointer; run `navigator project sync` to upload it or remove the pointer".to_string(),
        };
        errors.push(CheckFailure {
            location: local.relative().to_string(),
            message,
        });
    }
    Ok(Outcome { errors, fixes })
}

fn transcript_source_problem(asset: &IntegrityAsset, assets: &[IntegrityAsset]) -> Option<String> {
    if asset.kind.as_deref() != Some("transcript") {
        return None;
    }
    let source = asset.derived_from.as_ref()?;
    let document_id = source
        .get("document_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok());
    let version = source
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|version| usize::try_from(version).ok());
    let sha256 = source.get("sha256").and_then(serde_json::Value::as_str);
    let (Some(document_id), Some(version), Some(sha256)) = (document_id, version, sha256) else {
        return Some(
            "transcript source linkage is incomplete; re-transcribe from the source document's Pointer".into(),
        );
    };
    let Some(source_asset) = assets
        .iter()
        .find(|candidate| candidate.asset_id == document_id)
    else {
        return Some("transcript source revision is missing from the live matter".into());
    };
    if !source_asset.operative
        || source_asset.version != Some(version)
        || source_asset.sha256 != sha256
    {
        return Some(
            "transcript source revision is stale; re-transcribe from the current source document's Pointer"
                .into(),
        );
    }
    None
}

fn live_slugs(assets: &[IntegrityAsset]) -> BTreeSet<String> {
    assets
        .iter()
        .filter_map(|asset| slug_of(asset).map(str::to_string))
        .collect()
}

fn slug_of(asset: &IntegrityAsset) -> Option<&str> {
    asset.slug.as_deref().filter(|slug| !slug.is_empty())
}

fn location_for(asset: &IntegrityAsset, locals: &BTreeMap<String, Local>) -> String {
    match slug_of(asset) {
        Some(slug) => located(locals, slug),
        None => asset.asset_id.to_string(),
    }
}

fn located(locals: &BTreeMap<String, Local>, slug: &str) -> String {
    locals.get(slug).map_or_else(
        || pointer_relative(slug),
        |local| local.relative().to_string(),
    )
}

fn pointer_relative(slug: &str) -> String {
    format!("{}/{slug}.yaml", crate::document_sync::POINTER_DIRECTORY)
}

fn filename_extension(filename: &str) -> Option<&str> {
    Path::new(filename)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
}

/// The local pointer key for a live `slug`. A document filed before #786 can
/// carry a `slug` with no extension of its own (LAW-62): the committed
/// pointer must still retain the document extension ahead of its `.yaml`
/// suffix (`Y003`), so this takes the extension from the operative
/// revision's stored `filename` and appends it, unless `slug` already ends
/// with it.
fn local_key(slug: &str, filename: &str) -> String {
    match filename_extension(filename) {
        Some(extension) if !slug.ends_with(&format!(".{extension}")) => {
            format!("{slug}.{extension}")
        }
        _ => slug.to_string(),
    }
}

/// Problems with one row. A missing slug and a bad object are both reported.
/// Size and digest are only meaningful once the object is there.
fn row_problems(asset: &IntegrityAsset, deep: bool) -> Vec<String> {
    let mut problems = Vec::new();
    if slug_of(asset).is_none() {
        problems.push("live row has no slug".to_string());
    }
    if let Some(problem) = storage_problem(asset, deep) {
        problems.push(problem);
    }
    problems
}

fn storage_problem(asset: &IntegrityAsset, deep: bool) -> Option<String> {
    if !asset.exists {
        return Some("storage object missing".to_string());
    }
    if asset.size_bytes != Some(asset.recorded_size) {
        let got = asset
            .size_bytes
            .map_or_else(|| "none".to_string(), |size| size.to_string());
        return Some(format!(
            "stored size {got} does not match the recorded size {}",
            asset.recorded_size
        ));
    }
    if deep && asset.sha256_matches != Some(true) {
        return Some("stored bytes do not hash to the recorded sha256".to_string());
    }
    None
}

fn index_pointers(dir: &Path, errors: &mut Vec<CheckFailure>) -> Result<BTreeMap<String, Local>> {
    let mut locals: BTreeMap<String, Local> = BTreeMap::new();
    for path in discover_pointers(dir)? {
        let slug = slug_from_pointer(dir, &path)?;
        let relative = slash_path(&path)?;
        let absolute = dir.join(&path);
        if let Some(existing) = locals.get_mut(&slug) {
            errors.push(CheckFailure {
                location: relative,
                message: format!("two committed pointers name `{slug}`"),
            });
            *existing = Local::Unusable {
                relative: existing.relative().to_string(),
            };
            continue;
        }
        let local = match read_pointer(&absolute) {
            Ok(Some(pointer)) => Local::Pointer {
                relative,
                absolute,
                pointer: Box::new(pointer),
            },
            Ok(None) => {
                errors.push(CheckFailure {
                    location: relative.clone(),
                    message: "pointer file disappeared".to_string(),
                });
                Local::Unusable { relative }
            }
            Err(error) => {
                errors.push(CheckFailure {
                    location: relative.clone(),
                    message: format!("{error:#}"),
                });
                Local::Unusable { relative }
            }
        };
        locals.insert(slug, local);
    }
    Ok(locals)
}

/// Fetch every live slug's revisions once, then group them by pointer key.
///
/// A slug that turns out to share another slug's pointer key (LAW-81 — a
/// bare legacy slug and its extension-carrying twin both resolving to the
/// same `documents/<key>.yaml`) is reported directly in `errors` and named in
/// the returned set instead of being handed to [`reconcile_slug`]:
/// reconciling both in turn would rewrite the one pointer back and forth,
/// which reads like an ordinary drifted pointer and invites another commit
/// round.
async fn resolve_live_slugs(
    client: &DocumentClient,
    live_slugs: &BTreeSet<String>,
    locals: &BTreeMap<String, Local>,
    errors: &mut Vec<CheckFailure>,
) -> (Vec<(String, RevisionsResponse, String)>, BTreeSet<String>) {
    let mut resolved: Vec<(String, RevisionsResponse, String)> = Vec::new();
    for slug in live_slugs {
        match client.list_revisions(slug).await {
            Ok(live) => {
                let key = pointer_key(slug, &live);
                resolved.push((slug.clone(), live, key));
            }
            Err(error) => {
                errors.push(CheckFailure {
                    location: located(locals, slug),
                    message: format!("{error:#}"),
                });
            }
        }
    }
    let mut by_key: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (slug, _, key) in &resolved {
        by_key.entry(key.as_str()).or_default().push(slug.as_str());
    }
    let mut colliding: BTreeSet<String> = BTreeSet::new();
    for (key, slugs) in &by_key {
        if slugs.len() < 2 {
            continue;
        }
        colliding.insert((*key).to_string());
        let names = slugs
            .iter()
            .map(|slug| format!("`{slug}`"))
            .collect::<Vec<_>>()
            .join(" and ");
        errors.push(CheckFailure {
            location: located(locals, key),
            message: format!(
                "live chains {names} both resolve to `{}`",
                pointer_relative(key)
            ),
        });
    }
    (resolved, colliding)
}

// A legacy extensionless slug (LAW-62) is keyed in `locals` under the
// pointer's own filename-derived key, not the bare slug, because the pointer
// must retain the document extension (`Y003`).
fn pointer_key(slug: &str, live: &RevisionsResponse) -> String {
    live.revisions
        .iter()
        .find(|revision| revision.operative)
        .map_or_else(
            || slug.to_string(),
            |revision| local_key(slug, &revision.filename),
        )
}

#[allow(clippy::too_many_arguments)]
fn reconcile_slug(
    dir: &Path,
    slug: &str,
    key: String,
    live: &RevisionsResponse,
    assets: &[IntegrityAsset],
    locals: &mut BTreeMap<String, Local>,
    write: bool,
    deep: bool,
    errors: &mut Vec<CheckFailure>,
    fixes: &mut Vec<CheckFix>,
    claimed: &mut BTreeSet<String>,
    removed: &RemovedPointers,
) -> Result<()> {
    // A pointer the repository removed asks `navigator project sync` to
    // soft-delete the live document. Writing it back would undo that, so it
    // is reported as drift instead, and this check never writes the cloud.
    if locals.get(&key).is_none() && live.kind != "closed_repository" {
        if let Some(commit) = removed.pending_removal(slug) {
            errors.push(CheckFailure {
                location: pointer_relative(&key),
                message: format!(
                    "its pointer was removed in {}; `navigator project sync` asks whether to soft-delete the live document",
                    short_commit(commit)
                ),
            });
            claimed.insert(key);
            return Ok(());
        }
    }
    let desired =
        match desired_pointer(local_pointer(locals.get(&key)), &live.kind, &live.revisions) {
            Ok(pointer) => pointer,
            Err(message) => {
                errors.push(CheckFailure {
                    location: located(locals, slug),
                    message,
                });
                return Ok(());
            }
        };
    let storage_ok = operative_storage_ok(assets, desired.current_version.asset_id, deep, errors);
    match locals.get(&key) {
        Some(Local::Pointer {
            relative,
            absolute,
            pointer,
        }) => {
            if pointer.as_ref() == &desired {
                claimed.insert(key);
                return Ok(());
            }
            let relative = relative.clone();
            let absolute = absolute.clone();
            apply_fix(write, &absolute, &relative, REWRITE, &desired, fixes)?;
            claimed.insert(key);
        }
        None if storage_ok => {
            let relative = pointer_relative(&key);
            let absolute = dir.join(&relative);
            apply_fix(write, &absolute, &relative, WRITE_POINTER, &desired, fixes)?;
            claimed.insert(key.clone());
            if write {
                if let Ok(Some(pointer)) = read_pointer(&absolute) {
                    locals.insert(
                        key,
                        Local::Pointer {
                            relative,
                            absolute,
                            pointer: Box::new(pointer),
                        },
                    );
                }
            }
        }
        Some(Local::Unusable { .. }) | None => {}
    }
    Ok(())
}

fn local_pointer(local: Option<&Local>) -> Option<&DocumentPointer> {
    if let Some(Local::Pointer { pointer, .. }) = local {
        Some(pointer.as_ref())
    } else {
        None
    }
}

/// Whether the operative object's storage is healthy. A missing integrity row
/// is its own error, and it blocks creating a pointer for a document the
/// checkout does not already name.
fn operative_storage_ok(
    assets: &[IntegrityAsset],
    asset_id: Uuid,
    deep: bool,
    errors: &mut Vec<CheckFailure>,
) -> bool {
    if let Some(asset) = assets.iter().find(|asset| asset.asset_id == asset_id) {
        return storage_problem(asset, deep).is_none();
    }
    errors.push(CheckFailure {
        location: asset_id.to_string(),
        message: "operative revision is absent from the integrity report".to_string(),
    });
    false
}

fn desired_pointer(
    local: Option<&DocumentPointer>,
    kind: &str,
    revisions: &[RevisionSummary],
) -> Result<DocumentPointer, String> {
    let operative = revisions
        .iter()
        .find(|revision| revision.operative)
        .ok_or_else(|| "the live chain has no operative revision".to_string())?;
    if !matches!(operative.visibility.as_str(), "internal" | "client") {
        return Err("the operative revision has no visibility".to_string());
    }
    let previous_version = if operative.version == 1 {
        None
    } else {
        Some(
            revisions
                .iter()
                .find(|revision| revision.version + 1 == operative.version)
                .map(|revision| revision.asset_id)
                .ok_or_else(|| {
                    format!(
                        "the live chain has no revision {} to name as previous_version",
                        operative.version - 1
                    )
                })?,
        )
    };
    // ENG-859: `docusign_envelope_id` and `xero_invoice_id` are, like
    // `authority_id`, facts this offline reconciliation never resolves for
    // itself — it only carries a local value forward when the operative
    // revision it names is unchanged. Live resolution against DocuSign/Xero
    // (confirming the id it names still exists) is `project gate --check`'s
    // separate live-check pass (ENG-863), not this function.
    let (authority_id, canonical_url, checked_on, docusign_envelope_id, xero_invoice_id) =
        match local {
            Some(pointer) if pointer.current_version.asset_id == operative.asset_id => (
                pointer.authority_id,
                pointer.current_version.canonical_url.clone(),
                pointer.current_version.checked_on.clone(),
                pointer.docusign_envelope_id,
                pointer.xero_invoice_id,
            ),
            _ => (None, None, None, None, None),
        };
    let pointer = DocumentPointer {
        kind: kind.to_string(),
        visibility: operative.visibility.clone(),
        // The live chain has no folder; a committed one is kept (LAW-189).
        folder: local.and_then(|pointer| pointer.folder),
        current_version: PointerVersion {
            version: operative.version,
            asset_id: operative.asset_id,
            created_at: crate::document_sync::normalize_created_at(&operative.created_at),
            sha256: operative.sha256.clone(),
            size_bytes: operative.size_bytes,
            canonical_url,
            checked_on,
        },
        previous_version,
        authority_id,
        docusign_envelope_id,
        xero_invoice_id,
        // The live revision's record wins; a block recorded by hand on the
        // committed pointer is kept while it still names this revision and
        // the live row has none.
        signing: operative.signing.or_else(|| {
            local
                .filter(|pointer| pointer.current_version.asset_id == operative.asset_id)
                .and_then(|pointer| pointer.signing)
        }),
        notarization: if operative.notarization.is_empty() {
            local
                .filter(|pointer| pointer.current_version.asset_id == operative.asset_id)
                .map(|pointer| pointer.notarization.clone())
                .unwrap_or_default()
        } else {
            operative.notarization.clone()
        },
        // The live inventory carries no service list; the committed one is
        // kept while it still names this revision.
        service: local
            .filter(|pointer| pointer.current_version.asset_id == operative.asset_id)
            .map(|pointer| pointer.service.clone())
            .unwrap_or_default(),
    };
    pointer.validate().map_err(|error| error.to_string())?;
    Ok(pointer)
}

fn apply_fix(
    write: bool,
    absolute: &Path,
    relative: &str,
    action: &str,
    pointer: &DocumentPointer,
    fixes: &mut Vec<CheckFix>,
) -> Result<()> {
    if write {
        if let Some(parent) = absolute.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let yaml = pointer.to_yaml()?;
        write_pointer_atomically(absolute, &yaml)?;
    }
    fixes.push(CheckFix {
        path: relative.to_string(),
        action: action.to_string(),
    });
    Ok(())
}

/// Move a checkout still on the legacy layout — pointers beside their bytes
/// under `documents/` — to `blobs/` before anything is compared, so a moved
/// pointer is never reported missing. Under `--ci` the checkout is left
/// alone and the one finding is the whole outcome.
fn reconcile_legacy_layout(
    dir: &Path,
    write: bool,
    fixes: &mut Vec<CheckFix>,
) -> Result<Option<Outcome>> {
    let (legacy, guard) = crate::document_sync::legacy_layout(dir)?;
    if legacy.is_empty() && guard.is_none() {
        return Ok(None);
    }
    if !write {
        return Ok(Some(Outcome {
            errors: vec![CheckFailure {
                location: crate::document_sync::DOCUMENT_DIRECTORY.to_string(),
                message: format!(
                    "{} still sit under documents/; run `navigator project gate` locally to move to the blobs/ layout and commit the move",
                    crate::document_sync::describe_legacy_layout(legacy.len(), guard.is_some())
                ),
            }],
            fixes: Vec::new(),
        }));
    }
    crate::document_sync::migrate_legacy_layout(dir)?;
    fixes.push(CheckFix {
        path: crate::document_sync::POINTER_DIRECTORY.to_string(),
        action: MOVE_LEGACY_POINTERS.to_string(),
    });
    Ok(None)
}

/// The path prefix Google Drive for desktop mounts matter folders under,
/// below `$HOME` (LAW-217).
const CLOUD_STORAGE: &str = "Library/CloudStorage";

/// The `EPERM` explanation for a path under `~/Library/CloudStorage`
/// (LAW-217): macOS privacy controls block that folder for processes
/// without the permission, and `Operation not permitted` alone says nothing
/// about why or what would fix it. `None` for every other error.
pub(crate) fn cloud_storage_permission_note(error: &std::io::Error, path: &Path) -> Option<String> {
    if error.kind() != std::io::ErrorKind::PermissionDenied {
        return None;
    }
    let home = std::env::var("HOME").ok()?;
    let cloud = Path::new(&home).join(CLOUD_STORAGE);
    note_for_block(error, path, &cloud)
}

/// The core of [`cloud_storage_permission_note`], testable without touching
/// `$HOME`: the note when `path` sits under `cloud_storage` and `error` is
/// the permission error macOS gives.
fn note_for_block(error: &std::io::Error, path: &Path, cloud_storage: &Path) -> Option<String> {
    if error.kind() != std::io::ErrorKind::PermissionDenied || !path.starts_with(cloud_storage) {
        return None;
    }
    Some(format!(
        "macOS blocked access to {cloud}: allow the terminal app under Files and Folders or Full \
         Disk Access in System Settings, Privacy & Security, or fetch the file through the Drive \
         API instead",
        cloud = cloud_storage.display(),
    ))
}

/// An `anyhow` error for `path` carrying the [`cloud_storage_permission_note`]
/// when macOS blocked it, the plain context otherwise. Shared by every read
/// the standalone check makes, so one blocked folder explains itself once.
pub(crate) fn with_permission_note<T>(
    result: std::io::Result<T>,
    path: &Path,
) -> anyhow::Result<T> {
    result.map_err(|error| match cloud_storage_permission_note(&error, path) {
        Some(note) => anyhow!("{}: {note}", path.display()),
        None => io_error_with(error, path, "read"),
    })
}

/// `error` as an `anyhow` error whose context is `action` plus `path` — the
/// plain form when macOS did not block the read. `anyhow::Context` fits
/// `Result`/`Option`, not a bare `anyhow::Error`, so this goes through the
/// `Result` form to attach the message.
fn io_error_with(error: std::io::Error, path: &Path, action: &str) -> anyhow::Error {
    let message = format!("{action} {}", path.display());
    Err::<(), std::io::Error>(error)
        .with_context(|| message)
        .unwrap_err()
}

/// The offline document checks over any tree laid out like `documents/`
/// (LAW-217): the same matter-folder, filename, and collision rules a
/// checkout's `documents/` lives under, run read-only over any root — a
/// Google-Drive matter folder included. Nothing is uploaded, renamed, or
/// moved.
///
/// Each failure's `location` is the path below the tree root, so the same
/// tree reports identically wherever it lives.
pub(crate) fn check_documents_root(documents: &Path) -> Result<Vec<CheckFailure>> {
    match std::fs::symlink_metadata(documents) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err(anyhow!("{} is not a directory", documents.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(anyhow!("{} does not exist", documents.display()));
        }
        Err(error) => {
            return Err(match cloud_storage_permission_note(&error, documents) {
                Some(note) => anyhow!("{}: {note}", documents.display()),
                None => io_error_with(error, documents, "inspect"),
            })
        }
    }
    let mut errors = Vec::new();
    let mut unknown: Vec<PathBuf> = Vec::new();
    // `slug -> the first path filing as it`, for the collision rule.
    let mut filed: BTreeMap<String, String> = BTreeMap::new();
    // Sorted: which of two same-slug files is reported as the collision
    // finding must not depend on readdir order, and two filesystems holding
    // the same tree must produce the same report (LAW-217).
    for entry in walkdir::WalkDir::new(documents)
        .sort_by_file_name()
        .follow_links(false)
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => return Err(walk_error(error, documents)),
        };
        let path = entry.path().to_path_buf();
        let Ok(relative) = path.strip_prefix(documents) else {
            continue;
        };
        if entry.file_type().is_dir() {
            // A top-level directory must be a Matter Folder or a sibling
            // folder: a misspelled one (LAW-217's example) is invisible to
            // every check keyed to the checkout. Anything below an unknown
            // directory is that one finding, not one per file. A
            // dot-directory is Finder and sync-tool litter, like `.DS_Store`,
            // and is never a document.
            if entry.depth() == 1 && !entry.file_name().to_string_lossy().starts_with('.') {
                let name = relative
                    .to_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| relative.display().to_string());
                if crate::document_layout::matter_folder_of(relative).is_none()
                    && !crate::document_layout::is_sibling(relative)
                {
                    errors.push(CheckFailure {
                        location: name,
                        message: format!(
                            "not a Matter Folder or sibling folder; the known ones are {}",
                            known_folders()
                        ),
                    });
                    unknown.push(relative.to_path_buf());
                }
            }
            continue;
        }
        if unknown.iter().any(|folder| relative.starts_with(folder)) {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy();
        // `.gitignore`, and the `.DS_Store` a Finder window leaves in every
        // matter folder it opens, are never documents.
        if file_name.starts_with('.') {
            continue;
        }
        let location = slash_path(relative)?;
        // A pointer left under documents/ is the legacy-layout trap
        // `slug_from_pointer` refuses: as bytes it names a document that does
        // not exist. (Authority sidecars are not pointer paths.)
        if crate::document_sync::is_pointer_path(&path) {
            errors.push(CheckFailure {
                location,
                message: format!(
                    "looks like a committed Pointer left under the tree; Pointers live under {}/",
                    crate::document_sync::POINTER_DIRECTORY
                ),
            });
            continue;
        }
        // The invoice folder's filename rule, the same one `project sync`
        // refuses a capture on before anything is uploaded. The message
        // names no path: `location` already does, and keeping the finding
        // root-independent means the same tree reports identically wherever
        // it lives (LAW-217).
        if crate::document_sync::is_invoice_capture(relative)
            && !crate::document_sync::is_invoice_filename(&file_name)
        {
            errors.push(CheckFailure {
                location,
                message: "does not match the invoice filename pattern \
                          `INV-<digits>.<ext>` (e.g. INV-1.pdf)"
                    .to_string(),
            });
            continue;
        }
        // Two files naming one slug cannot both be filed: a Pointer claims
        // the slug, and `project sync` uploads at most one of them.
        let (slug, _) = crate::document_layout::slug_of_bytes(relative);
        let slug = slash_path(&slug)?;
        match filed.get(&slug) {
            Some(first) => errors.push(CheckFailure {
                location,
                message: format!("files as `{slug}`, which `{first}` already files"),
            }),
            None => {
                filed.insert(slug, location);
            }
        }
    }
    Ok(errors)
}

fn known_folders() -> String {
    let mut names: Vec<&str> = store::document_pointers::MatterFolder::ALL
        .iter()
        .map(|folder| folder.as_str())
        .collect();
    names.extend_from_slice(store::document_pointers::SIBLING_FOLDERS);
    names.join(", ")
}

/// The walk error for [`check_documents_root`]: the macOS permission note
/// when the block is the CloudStorage one, the plain context otherwise.
fn walk_error(error: walkdir::Error, documents: &Path) -> anyhow::Error {
    let Some(io) = error.io_error() else {
        return anyhow!("{error}");
    };
    let path = error.path().unwrap_or(documents).to_path_buf();
    match cloud_storage_permission_note(io, &path) {
        Some(note) => anyhow!("{}: {note}", path.display()),
        None => io_error_with(
            std::io::Error::new(io.kind(), io.to_string()),
            documents,
            "walk",
        ),
    }
}

/// Resolve the Project's matter folder in Google Drive for desktop to its
/// path on this machine (LAW-217): a folder named after the Project code in
/// `~/Library/CloudStorage/GoogleDrive-*/My Drive/` or
/// `~/Library/CloudStorage/GoogleDrive-*/Shared drives/<drive>/`, so nobody
/// types the CloudStorage path. A folder the Project records by id (LAW-213)
/// is not resolved here yet: offline, the name is the only handle this
/// machine has.
pub(crate) fn resolve_drive_local(project: &str) -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| anyhow!("HOME is not set; cannot look for the Drive for desktop mount"))?;
    resolve_drive_local_in(project, &Path::new(&home).join(CLOUD_STORAGE))
}

/// The testable core of [`resolve_drive_local`]: search one
/// `~/Library/CloudStorage` root.
fn resolve_drive_local_in(project: &str, cloud_storage: &Path) -> Result<PathBuf> {
    let mounts = match std::fs::read_dir(cloud_storage) {
        Ok(mounts) => mounts,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(anyhow!(
                "no Google Drive for desktop mount: {} does not exist",
                cloud_storage.display()
            ));
        }
        Err(error) => {
            return Err(match cloud_storage_permission_note(&error, cloud_storage) {
                Some(note) => anyhow!("{}: {note}", cloud_storage.display()),
                None => io_error_with(error, cloud_storage, "read"),
            })
        }
    };
    let mut found = Vec::new();
    for mount in mounts {
        let mount = match mount {
            Ok(mount) => mount,
            Err(error) => {
                return Err(match cloud_storage_permission_note(&error, cloud_storage) {
                    Some(note) => anyhow!("{}: {note}", cloud_storage.display()),
                    None => io_error_with(error, cloud_storage, "read"),
                })
            }
        };
        if !mount
            .file_name()
            .to_string_lossy()
            .starts_with("GoogleDrive-")
        {
            continue;
        }
        let mount = mount.path();
        for roots in [
            vec![mount.join("My Drive")],
            read_mount_children(&mount.join("Shared drives"))?,
        ] {
            for root in roots {
                let candidate = root.join(project);
                match std::fs::symlink_metadata(&candidate) {
                    Ok(metadata) if metadata.is_dir() => found.push(candidate),
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return with_permission_note(Err(error), &candidate),
                }
            }
        }
    }
    match found.as_slice() {
        [] => Err(anyhow!(
            "no folder named `{project}` was found under any Drive for desktop mount in {}; \
             pass --documents-dir to point the check at the folder itself",
            cloud_storage.display()
        )),
        [only] => Ok(only.to_path_buf()),
        many => Err(anyhow!(
            "{} folders named `{project}` exist across the Drive for desktop mounts; \
             pass --documents-dir to choose one",
            many.len()
        )),
    }
}

/// Every child directory of a mount's `Shared drives/`, or none when the
/// mount has no such folder. A read the macOS permission block refuses
/// carries the explanation (LAW-217).
fn read_mount_children(shared_drives: &Path) -> Result<Vec<PathBuf>> {
    match std::fs::read_dir(shared_drives) {
        Ok(drives) => Ok(drives
            .filter_map(|drive| drive.ok())
            .filter(|drive| drive.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|drive| drive.path())
            .collect()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(match cloud_storage_permission_note(&error, shared_drives) {
            Some(note) => anyhow!("{}: {note}", shared_drives.display()),
            None => io_error_with(error, shared_drives, "read"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::DocumentClient;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const OTHER_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BODY: &str = "Synthetic fixture bytes.\n";

    #[test]
    fn transcript_gate_rejects_a_source_revision_that_is_no_longer_operative() {
        let source_id = Uuid::now_v7();
        let transcript = IntegrityAsset {
            asset_id: Uuid::now_v7(),
            slug: Some("transcripts/order.transcript.md".into()),
            kind: Some("transcript".into()),
            sha256: OTHER_SHA.into(),
            derived_from: Some(serde_json::json!({
                "document_id": source_id,
                "version": 1,
                "sha256": SHA,
            })),
            transcript_quality: Some("machine".into()),
            operative: true,
            version: Some(1),
            exists: true,
            size_bytes: Some(100),
            recorded_size: 100,
            sha256_matches: None,
            docket_name_matches: None,
        };
        let source = IntegrityAsset {
            asset_id: source_id,
            slug: Some("pleadings/order.pdf".into()),
            kind: Some("filing".into()),
            sha256: SHA.into(),
            derived_from: None,
            transcript_quality: None,
            operative: false,
            version: Some(1),
            exists: true,
            size_bytes: Some(200),
            recorded_size: 200,
            sha256_matches: None,
            docket_name_matches: None,
        };
        let message = transcript_source_problem(&transcript, &[transcript.clone(), source])
            .expect("stale transcript is a gate failure");
        assert!(message.contains("source revision is stale"));
    }

    #[test]
    fn transcript_gate_checks_linkage_and_accepts_the_current_source_revision() {
        let source_id = Uuid::now_v7();
        let transcript = IntegrityAsset {
            asset_id: Uuid::now_v7(),
            slug: Some("transcripts/order.transcript.md".into()),
            kind: Some("transcript".into()),
            sha256: OTHER_SHA.into(),
            derived_from: Some(serde_json::json!({
                "document_id": source_id,
                "version": 1,
                "sha256": SHA,
            })),
            transcript_quality: Some("machine".into()),
            operative: true,
            version: Some(1),
            exists: true,
            size_bytes: Some(100),
            recorded_size: 100,
            sha256_matches: None,
            docket_name_matches: None,
        };
        let source = IntegrityAsset {
            asset_id: source_id,
            slug: Some("pleadings/order.pdf".into()),
            kind: Some("filing".into()),
            sha256: SHA.into(),
            derived_from: None,
            transcript_quality: None,
            operative: true,
            version: Some(1),
            exists: true,
            size_bytes: Some(200),
            recorded_size: 200,
            sha256_matches: None,
            docket_name_matches: None,
        };
        assert!(transcript_source_problem(&transcript, std::slice::from_ref(&source)).is_none());

        let mut stale_source = source.clone();
        stale_source.version = Some(2);
        assert!(
            transcript_source_problem(&transcript, &[stale_source.clone()])
                .expect("a changed version is stale")
                .contains("source revision is stale")
        );
        stale_source.version = Some(1);
        stale_source.sha256 = OTHER_SHA.into();
        assert!(transcript_source_problem(&transcript, &[stale_source])
            .expect("changed bytes are stale")
            .contains("source revision is stale"));

        let incomplete = IntegrityAsset {
            derived_from: Some(serde_json::json!({"document_id": "not-a-uuid"})),
            ..transcript.clone()
        };
        assert!(transcript_source_problem(&incomplete, &[])
            .expect("invalid linkage is rejected")
            .contains("linkage is incomplete"));
        assert!(transcript_source_problem(&transcript, &[])
            .expect("missing source is rejected")
            .contains("source revision is missing"));

        let ordinary_asset = IntegrityAsset {
            kind: Some("filing".into()),
            ..transcript
        };
        assert!(transcript_source_problem(&ordinary_asset, &[]).is_none());
    }

    fn operative_revision(asset_id: Uuid) -> crate::remote::RevisionSummary {
        crate::remote::RevisionSummary {
            version: 1,
            asset_id,
            created_at: "2026-09-05T12:00:00Z".into(),
            sha256: SHA.into(),
            size_bytes: 18,
            filename: "letter_signed.pdf".into(),
            visibility: "internal".into(),
            operative: true,
            signing: None,
            notarization: Vec::new(),
        }
    }

    /// The live signing record lands on the pointer; a block recorded by
    /// hand survives while the live row has none and the pointer still
    /// names the operative revision; neither is required.
    #[test]
    fn desired_pointer_carries_the_signing_record() {
        let asset_id = Uuid::now_v7();
        let wet_ink = store::documents::Signing {
            method: store::documents::SigningMethod::WetInk,
            signed_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 4).unwrap(),
        };
        let docusign = store::documents::Signing {
            method: store::documents::SigningMethod::Docusign,
            signed_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 5).unwrap(),
        };

        let unsigned =
            desired_pointer(None, "onboarding", &[operative_revision(asset_id)]).unwrap();
        assert_eq!(unsigned.signing, None);

        let mut live = operative_revision(asset_id);
        live.signing = Some(wet_ink);
        let from_live = desired_pointer(None, "onboarding", &[live.clone()]).unwrap();
        assert_eq!(from_live.signing, Some(wet_ink));

        let mut hand_recorded = unsigned.clone();
        hand_recorded.signing = Some(docusign);
        hand_recorded.notarization = vec![store::documents::Notarization {
            method: store::documents::NotarizationMethod::InPerson,
            acknowledged_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 4).unwrap(),
            state: "NV".into(),
            county: "Clark".into(),
            commission_expires: chrono::NaiveDate::from_ymd_opt(2029, 1, 31).unwrap(),
        }];
        let kept = desired_pointer(
            Some(&hand_recorded),
            "onboarding",
            &[operative_revision(asset_id)],
        )
        .unwrap();
        assert_eq!(kept, hand_recorded);
        assert_eq!(
            desired_pointer(Some(&hand_recorded), "onboarding", &[live])
                .unwrap()
                .signing,
            Some(wet_ink)
        );
        let newer = desired_pointer(
            Some(&hand_recorded),
            "onboarding",
            &[operative_revision(Uuid::now_v7())],
        )
        .unwrap();
        assert_eq!(newer.signing, None);
        assert!(newer.notarization.is_empty());
    }

    fn pointer_yaml(asset_id: Uuid, sha: &str) -> String {
        format!(
            "kind: filing\nvisibility: internal\ncurrent_version:\n  version: 1\n  \
             asset_id: {asset_id}\n  created_at: \"2026-09-05T12:00:00Z\"\n  sha256: {sha}\n  \
             size_bytes: 18\n"
        )
    }

    fn write_tree(dir: &Path, relative: &str, bytes: &str) {
        let path = dir.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn gitignore(dir: &Path) {
        write_tree(
            dir,
            ".gitignore",
            crate::projects::repository::ROOT_GITIGNORE,
        );
    }

    async fn server() -> (MockServer, Uuid, Uuid) {
        (MockServer::start().await, Uuid::now_v7(), Uuid::now_v7())
    }

    fn client(server: &MockServer, project_id: Uuid) -> DocumentClient {
        DocumentClient::for_check_tests(server.uri(), project_id)
    }

    async fn mount_integrity(server: &MockServer, project_id: Uuid, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/integrity"
            )))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(server)
            .await;
    }

    async fn mount_revisions(
        server: &MockServer,
        project_id: Uuid,
        slug: &str,
        asset_id: Uuid,
        sha: &str,
    ) {
        mount_revisions_named(server, project_id, slug, asset_id, sha, "summons.pdf").await;
    }

    async fn mount_revisions_named(
        server: &MockServer,
        project_id: Uuid,
        slug: &str,
        asset_id: Uuid,
        sha: &str,
        filename: &str,
    ) {
        Mock::given(method("GET"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/revisions"
            )))
            .and(query_param("slug", slug))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "kind": "filing",
                "revisions": [{
                    "version": 1,
                    "asset_id": asset_id,
                    "created_at": "2026-09-05T12:00:00Z",
                    "sha256": sha,
                    "size_bytes": 18,
                    "filename": filename,
                    "visibility": "internal",
                    "operative": true
                }]
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    fn asset(
        asset_id: Uuid,
        slug: Option<&str>,
        exists: bool,
        sha256_matches: Option<bool>,
    ) -> serde_json::Value {
        let mut value = serde_json::json!({
            "asset_id": asset_id,
            "slug": slug,
            "exists": exists,
            "recorded_size": 18,
        });
        if exists {
            value["size_bytes"] = serde_json::json!(18);
        }
        if let Some(matches) = sha256_matches {
            value["sha256_matches"] = serde_json::json!(matches);
        }
        value
    }

    #[tokio::test]
    async fn a_missing_object_fails_when_the_pointer_matches_the_row() {
        let (server, project_id, asset_id) = server().await;
        let dir = tempfile::tempdir().unwrap();
        let relative = "blobs/pleadings/summons.pdf.yaml";
        let yaml = pointer_yaml(asset_id, SHA);
        write_tree(dir.path(), relative, &yaml);
        gitignore(dir.path());
        mount_integrity(
            &server,
            project_id,
            serde_json::json!({ "assets": [asset(asset_id, Some("pleadings/summons.pdf"), false, None)] }),
        )
        .await;
        mount_revisions(&server, project_id, "pleadings/summons.pdf", asset_id, SHA).await;

        let outcome = check(dir.path(), &client(&server, project_id), true, false)
            .await
            .unwrap();

        assert!(outcome.errors.iter().any(|error| {
            error.location == relative && error.message.contains("storage object missing")
        }));
        assert!(outcome.fixes.is_empty());
        assert!(outcome.rejects(false));
        assert_eq!(
            std::fs::read_to_string(dir.path().join(relative)).unwrap(),
            yaml
        );
    }

    #[tokio::test]
    async fn a_live_document_with_no_pointer_fails_under_ci() {
        let (server, project_id, asset_id) = server().await;
        let dir = tempfile::tempdir().unwrap();
        mount_integrity(
            &server,
            project_id,
            serde_json::json!({ "assets": [asset(asset_id, Some("pleadings/summons.pdf"), true, None)] }),
        )
        .await;
        mount_revisions(&server, project_id, "pleadings/summons.pdf", asset_id, SHA).await;

        let outcome = check(dir.path(), &client(&server, project_id), false, false)
            .await
            .unwrap();

        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert!(outcome.fixes.iter().any(|fix| {
            fix.path == "blobs/pleadings/summons.pdf.yaml" && fix.action == WRITE_POINTER
        }));
        assert!(outcome.rejects(true));
        assert!(!dir.path().join("blobs").exists());
    }

    /// ENG-862: a pointer the lawyer removed is a pending deletion. The check
    /// reports it as drift and does not write the pointer back; a document
    /// that never had a pointer still gets one.
    #[tokio::test]
    async fn a_removed_pointer_is_reported_as_a_pending_deletion_and_not_rewritten() {
        let (server, project_id, asset_id) = server().await;
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.test"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(dir.path())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "--quiet"]);
        let relative = "blobs/pleadings/summons.pdf.yaml";
        write_tree(dir.path(), relative, &pointer_yaml(asset_id, SHA));
        git(&["add", "blobs"]);
        git(&["commit", "--quiet", "-m", "file"]);
        git(&["rm", "--quiet", relative]);
        git(&["commit", "--quiet", "-m", "remove"]);
        mount_integrity(
            &server,
            project_id,
            serde_json::json!({ "assets": [asset(asset_id, Some("pleadings/summons.pdf"), true, None)] }),
        )
        .await;
        mount_revisions(&server, project_id, "pleadings/summons.pdf", asset_id, SHA).await;

        let outcome = check(dir.path(), &client(&server, project_id), true, false)
            .await
            .unwrap();

        assert!(outcome.rejects(false));
        assert!(
            outcome.errors.iter().any(|error| error.location == relative
                && error.message.contains("its pointer was removed in")),
            "{:?}",
            outcome.errors
        );
        assert!(
            !outcome.fixes.iter().any(|fix| fix.action == WRITE_POINTER),
            "{:?}",
            outcome.fixes
        );
        assert!(!dir.path().join(relative).exists());
    }

    #[tokio::test]
    async fn a_local_check_writes_the_missing_pointer_and_gitignore() {
        let (server, project_id, asset_id) = server().await;
        let dir = tempfile::tempdir().unwrap();
        mount_integrity(
            &server,
            project_id,
            serde_json::json!({ "assets": [asset(asset_id, Some("pleadings/summons.pdf"), true, None)] }),
        )
        .await;
        mount_revisions(&server, project_id, "pleadings/summons.pdf", asset_id, SHA).await;

        let outcome = check(dir.path(), &client(&server, project_id), true, false)
            .await
            .unwrap();

        assert!(!outcome.rejects(false), "{outcome:?}");
        let pointer = DocumentPointer::from_yaml(
            &std::fs::read_to_string(dir.path().join("blobs/pleadings/summons.pdf.yaml")).unwrap(),
        )
        .unwrap();
        assert_eq!(pointer.current_version.asset_id, asset_id);
        assert_eq!(pointer.current_version.sha256, SHA);
    }

    #[tokio::test]
    async fn auto_fix_rewrites_a_drifted_pointer() {
        let (server, project_id, asset_id) = server().await;
        let dir = tempfile::tempdir().unwrap();
        let relative = "blobs/pleadings/summons.pdf.yaml";
        write_tree(dir.path(), relative, &pointer_yaml(asset_id, OTHER_SHA));
        gitignore(dir.path());
        mount_integrity(
            &server,
            project_id,
            serde_json::json!({ "assets": [asset(asset_id, Some("pleadings/summons.pdf"), true, None)] }),
        )
        .await;
        mount_revisions(&server, project_id, "pleadings/summons.pdf", asset_id, SHA).await;

        let outcome = check(dir.path(), &client(&server, project_id), true, false)
            .await
            .unwrap();

        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert_eq!(
            outcome.fixes,
            vec![CheckFix {
                path: relative.to_string(),
                action: REWRITE.to_string(),
            }]
        );
        let pointer = DocumentPointer::from_yaml(
            &std::fs::read_to_string(dir.path().join(relative)).unwrap(),
        )
        .unwrap();
        assert_eq!(pointer.current_version.sha256, SHA);
        assert!(!outcome.rejects(false));
    }

    #[tokio::test]
    async fn ci_reports_a_drifted_pointer_and_does_not_write_it() {
        let (server, project_id, asset_id) = server().await;
        let dir = tempfile::tempdir().unwrap();
        let relative = "blobs/pleadings/summons.pdf.yaml";
        let original = pointer_yaml(asset_id, OTHER_SHA);
        write_tree(dir.path(), relative, &original);
        gitignore(dir.path());
        mount_integrity(
            &server,
            project_id,
            serde_json::json!({ "assets": [asset(asset_id, Some("pleadings/summons.pdf"), true, None)] }),
        )
        .await;
        mount_revisions(&server, project_id, "pleadings/summons.pdf", asset_id, SHA).await;

        let outcome = check(dir.path(), &client(&server, project_id), false, false)
            .await
            .unwrap();

        assert!(outcome.rejects(true));
        assert!(outcome.fixes.iter().any(|fix| fix.action == REWRITE));
        assert_eq!(
            std::fs::read_to_string(dir.path().join(relative)).unwrap(),
            original
        );
    }

    #[tokio::test]
    async fn a_deep_hash_mismatch_fails_without_rewriting_the_pointer() {
        let (server, project_id, asset_id) = server().await;
        let dir = tempfile::tempdir().unwrap();
        let relative = "blobs/pleadings/summons.pdf.yaml";
        let yaml = pointer_yaml(asset_id, SHA);
        write_tree(dir.path(), relative, &yaml);
        gitignore(dir.path());
        mount_integrity(
            &server,
            project_id,
            serde_json::json!({
                "assets": [asset(asset_id, Some("pleadings/summons.pdf"), true, Some(false))]
            }),
        )
        .await;
        mount_revisions(&server, project_id, "pleadings/summons.pdf", asset_id, SHA).await;

        let outcome = check(dir.path(), &client(&server, project_id), true, true)
            .await
            .unwrap();

        assert!(outcome
            .errors
            .iter()
            .any(|error| { error.message.contains("do not hash to the recorded sha256") }));
        assert!(outcome.fixes.is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(relative)).unwrap(),
            yaml
        );
    }

    #[tokio::test]
    async fn a_slugless_row_is_an_error_named_by_asset_id() {
        let (server, project_id, asset_id) = server().await;
        let dir = tempfile::tempdir().unwrap();
        mount_integrity(
            &server,
            project_id,
            serde_json::json!({ "assets": [asset(asset_id, None, true, None)] }),
        )
        .await;

        let outcome = check(dir.path(), &client(&server, project_id), true, false)
            .await
            .unwrap();

        assert!(outcome.errors.iter().any(|error| {
            error.location == asset_id.to_string() && error.message.contains("no slug")
        }));
        assert!(!dir.path().join("documents").exists());
    }

    /// LAW-62: a document filed before #786 can carry a live `slug` with no
    /// extension of its own. Writing the pointer at `documents/<slug>.yaml`
    /// fails `Y003` (no document extension before the `.yaml` suffix), so
    /// the check must take the extension from the stored filename and write
    /// `documents/<slug>.<ext>.yaml` instead.
    #[tokio::test]
    async fn a_legacy_extensionless_slug_writes_a_pointer_with_the_stored_extension() {
        let (server, project_id, asset_id) = server().await;
        let dir = tempfile::tempdir().unwrap();
        let slug = "summons-wendell-prine";
        mount_integrity(
            &server,
            project_id,
            serde_json::json!({ "assets": [asset(asset_id, Some(slug), true, None)] }),
        )
        .await;
        mount_revisions_named(
            &server,
            project_id,
            slug,
            asset_id,
            SHA,
            "summons-wendell-prine.pdf",
        )
        .await;

        let outcome = check(dir.path(), &client(&server, project_id), true, false)
            .await
            .unwrap();

        assert!(!outcome.rejects(false), "{outcome:?}");
        assert!(
            outcome.fixes.iter().any(|fix| {
                fix.path == "blobs/summons-wendell-prine.pdf.yaml" && fix.action == WRITE_POINTER
            }),
            "{:?}",
            outcome.fixes
        );
        assert!(dir
            .path()
            .join("blobs/summons-wendell-prine.pdf.yaml")
            .exists());
        assert!(!dir.path().join("blobs/summons-wendell-prine.yaml").exists());
    }

    /// The extension-carrying pointer this fix writes must not be mistaken
    /// for an orphan (no live document matches its filename-derived local
    /// key) or rewritten on every subsequent run.
    #[tokio::test]
    async fn a_legacy_extensionless_pointer_already_on_disk_is_not_reported_as_orphaned() {
        let (server, project_id, asset_id) = server().await;
        let dir = tempfile::tempdir().unwrap();
        let slug = "summons-wendell-prine";
        let relative = "blobs/summons-wendell-prine.pdf.yaml";
        write_tree(dir.path(), relative, &pointer_yaml(asset_id, SHA));
        gitignore(dir.path());
        mount_integrity(
            &server,
            project_id,
            serde_json::json!({ "assets": [asset(asset_id, Some(slug), true, None)] }),
        )
        .await;
        mount_revisions_named(
            &server,
            project_id,
            slug,
            asset_id,
            SHA,
            "summons-wendell-prine.pdf",
        )
        .await;

        let outcome = check(dir.path(), &client(&server, project_id), true, false)
            .await
            .unwrap();

        assert!(!outcome.rejects(false), "{outcome:?}");
        assert!(outcome.fixes.is_empty(), "{:?}", outcome.fixes);
    }

    /// LAW-82: an Authority-backed pointer's asset id names the Authority's
    /// archived asset, which carries no `project_id` and so never appears in
    /// this Project's integrity report. The gate must not mistake that for a
    /// missing live document.
    #[tokio::test]
    async fn an_authority_backed_pointer_is_not_reported_as_missing_its_live_document() {
        let (server, project_id, _) = server().await;
        let dir = tempfile::tempdir().unwrap();
        let authority_asset = Uuid::now_v7();
        let authority_id = Uuid::now_v7();
        let relative = "blobs/cases/roe.html.yaml";
        write_tree(
            dir.path(),
            relative,
            &format!(
                "kind: ''\nvisibility: internal\ncurrent_version:\n  version: 1\n  \
                 asset_id: {authority_asset}\n  created_at: \"2026-09-05T12:00:00Z\"\n  \
                 sha256: {SHA}\n  size_bytes: 18\nauthority_id: {authority_id}\n"
            ),
        );
        gitignore(dir.path());
        mount_integrity(&server, project_id, serde_json::json!({ "assets": [] })).await;

        let outcome = check(dir.path(), &client(&server, project_id), true, false)
            .await
            .unwrap();

        assert!(!outcome.rejects(false), "{outcome:?}");
        assert!(outcome.fixes.is_empty(), "{:?}", outcome.fixes);
    }

    /// LAW-81: a bare legacy slug and its extension-carrying twin both
    /// resolve to the same pointer key. Reconciling either one in turn would
    /// rewrite `documents/x.md.yaml` back and forth forever; the check must
    /// instead report the collision directly and leave the pointer alone.
    #[tokio::test]
    async fn two_live_chains_resolving_to_the_same_pointer_are_reported_and_not_rewritten() {
        let (server, project_id, _) = server().await;
        let dir = tempfile::tempdir().unwrap();
        let bare_id = Uuid::now_v7();
        let dotted_id = Uuid::now_v7();
        mount_integrity(
            &server,
            project_id,
            serde_json::json!({
                "assets": [
                    asset(bare_id, Some("x"), true, None),
                    asset(dotted_id, Some("x.md"), true, None),
                ]
            }),
        )
        .await;
        mount_revisions_named(&server, project_id, "x", bare_id, SHA, "x.md").await;
        mount_revisions_named(&server, project_id, "x.md", dotted_id, OTHER_SHA, "x.md").await;

        let outcome = check(dir.path(), &client(&server, project_id), true, false)
            .await
            .unwrap();

        assert!(outcome.rejects(false), "{outcome:?}");
        assert!(
            outcome.errors.iter().any(|error| {
                error.message.contains("`x`")
                    && error.message.contains("`x.md`")
                    && error.message.contains("blobs/x.md.yaml")
            }),
            "{:?}",
            outcome.errors
        );
        assert!(outcome.fixes.is_empty(), "{:?}", outcome.fixes);
        assert!(!dir.path().join("blobs/x.md.yaml").exists());
        assert!(!dir.path().join("blobs/x.yaml").exists());
    }

    /// Serves `rows` the way the integrity route does: `limit` rows from
    /// `offset`, with `total` and `next_offset`.
    struct Paged {
        rows: Vec<serde_json::Value>,
        limits: std::sync::Arc<std::sync::Mutex<Vec<usize>>>,
    }

    impl wiremock::Respond for Paged {
        fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
            let query: BTreeMap<_, _> = request.url.query_pairs().into_owned().collect();
            let offset: usize = query["offset"].parse().unwrap();
            let limit: usize = query["limit"].parse().unwrap();
            self.limits.lock().unwrap().push(limit);
            let end = (offset + limit).min(self.rows.len());
            let mut body = serde_json::json!({
                "assets": self.rows[offset..end],
                "total": self.rows.len(),
            });
            if end < self.rows.len() {
                body["next_offset"] = serde_json::json!(end);
            }
            ResponseTemplate::new(200).set_body_json(body)
        }
    }

    /// LAW-146: roughly 380 pointers are fetched as bounded pages and merged,
    /// so no single request carries the whole matter. A deep check pages
    /// smaller still, because each row costs a full read.
    #[tokio::test]
    async fn a_large_matter_is_fetched_in_bounded_pages_and_fully_checked() {
        for (deep, page) in [(false, 50), (true, 10)] {
            let (server, project_id, _) = server().await;
            let rows: Vec<_> = (0..380)
                .map(|index| {
                    asset(
                        Uuid::now_v7(),
                        Some(&format!("d/{index:04}.pdf")),
                        true,
                        None,
                    )
                })
                .collect();
            let limits = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            Mock::given(method("GET"))
                .and(path(format!(
                    "/app/api/projects/{project_id}/documents/integrity"
                )))
                .respond_with(Paged {
                    rows,
                    limits: limits.clone(),
                })
                .mount(&server)
                .await;

            let integrity = client(&server, project_id).integrity(deep).await.unwrap();

            assert_eq!(integrity.assets.len(), 380);
            let ids: BTreeSet<_> = integrity
                .assets
                .iter()
                .map(|asset| asset.asset_id)
                .collect();
            assert_eq!(ids.len(), 380);
            let limits = limits.lock().unwrap();
            assert_eq!(limits.len(), 380_usize.div_ceil(page));
            assert!(limits.iter().all(|limit| *limit == page));
        }
    }

    /// A gateway error on one page is retried and the check still covers
    /// every row.
    #[tokio::test]
    async fn a_gateway_error_on_one_page_is_retried() {
        let (server, project_id, _) = server().await;
        let path_ = format!("/app/api/projects/{project_id}/documents/integrity");
        Mock::given(method("GET"))
            .and(path(path_.clone()))
            .respond_with(ResponseTemplate::new(502).set_body_string("<html><head>502 Bad Gateway"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(path_))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "assets": [asset(Uuid::now_v7(), Some("a.pdf"), true, None)] }),
            ))
            .mount(&server)
            .await;

        let integrity = client(&server, project_id).integrity(false).await.unwrap();

        assert_eq!(integrity.assets.len(), 1);
    }

    /// A page that never recovers fails the check, naming the status, the
    /// page, the attempts and the elapsed time without dumping the HTML body.
    #[tokio::test]
    async fn a_page_that_keeps_failing_fails_with_bounded_diagnostics() {
        let (server, project_id, _) = server().await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/integrity"
            )))
            .respond_with(
                ResponseTemplate::new(502)
                    .set_body_string(format!("<html><head>{}", "x".repeat(5000))),
            )
            .expect(3)
            .mount(&server)
            .await;

        let error = client(&server, project_id)
            .integrity(true)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("502 Bad Gateway"), "{error}");
        assert!(error.contains("offset 0"), "{error}");
        assert!(error.contains("10 per page, deep"), "{error}");
        assert!(error.contains("attempt 3 of 3"), "{error}");
        assert!(!error.contains("<html"), "{error}");
        assert!(error.len() < 400, "{error}");
    }

    /// LAW-157: a gateway that gives up on a page is reported as a timeout
    /// naming the page, not as the gateway's HTML.
    #[tokio::test]
    async fn a_gateway_timeout_is_reported_as_a_timeout() {
        let (server, project_id, _) = server().await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/integrity"
            )))
            .respond_with(
                ResponseTemplate::new(504).set_body_string(
                    "<html><head><title>504 Gateway Time-out</title></head></html>",
                ),
            )
            .expect(3)
            .mount(&server)
            .await;

        let error = client(&server, project_id)
            .integrity(false)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("504 Gateway Timeout"), "{error}");
        assert!(error.contains("timed out"), "{error}");
        assert!(error.contains("offset 0"), "{error}");
        assert!(!error.contains("<html"), "{error}");
        assert!(!error.contains("<title"), "{error}");
    }

    /// A page that is not a gateway failure is not retried.
    #[tokio::test]
    async fn a_refusal_is_not_retried() {
        let (server, project_id, _) = server().await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/integrity"
            )))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .expect(1)
            .mount(&server)
            .await;

        let error = client(&server, project_id)
            .integrity(false)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("404 Not Found: not found"), "{error}");
        assert!(error.contains("attempt 1 of 3"), "{error}");
    }

    /// A tree laid out like `documents/`: one Matter Folder, a good invoice,
    /// and a Finder stray.
    fn matter_tree(root: &std::path::Path) {
        std::fs::create_dir_all(root.join("Work Product")).unwrap();
        std::fs::create_dir_all(root.join("invoices")).unwrap();
        std::fs::create_dir_all(root.join(".apupdates")).unwrap();
        std::fs::write(root.join("Work Product/memo.md"), BODY).unwrap();
        std::fs::write(root.join("invoices/INV-1.pdf"), BODY).unwrap();
        std::fs::write(root.join(".DS_Store"), b"").unwrap();
    }

    fn locations(errors: &[CheckFailure]) -> Vec<&str> {
        errors
            .iter()
            .map(|failure| failure.location.as_str())
            .collect()
    }

    #[test]
    fn a_matter_folder_misspelling_and_a_bad_invoice_are_both_reported() {
        let dir = tempfile::tempdir().unwrap();
        matter_tree(dir.path());
        std::fs::create_dir_all(dir.path().join("Docket Filing")).unwrap();
        std::fs::write(dir.path().join("Docket Filing/order.pdf"), BODY).unwrap();
        std::fs::write(dir.path().join("invoices/receipt.pdf"), BODY).unwrap();

        let errors = check_documents_root(dir.path()).unwrap();
        let locations = locations(&errors);
        assert_eq!(
            locations,
            vec!["Docket Filing", "invoices/receipt.pdf"],
            "both findings, with the paths below the tree root"
        );
        assert!(
            errors[0]
                .message
                .contains("not a Matter Folder or sibling folder"),
            "{}",
            errors[0].message
        );
        assert!(
            errors[1]
                .message
                .contains("does not match the invoice filename pattern"),
            "{}",
            errors[1].message
        );
    }

    #[test]
    fn the_same_tree_reports_identically_wherever_it_lives() {
        let checkout = tempfile::tempdir().unwrap();
        matter_tree(&checkout.path().join("documents"));
        let drive = tempfile::tempdir().unwrap();
        matter_tree(&drive.path().join("shook-estate"));

        let from_checkout = check_documents_root(&checkout.path().join("documents")).unwrap();
        let from_drive = check_documents_root(&drive.path().join("shook-estate")).unwrap();
        assert_eq!(from_checkout, from_drive);
    }

    #[test]
    fn a_clean_tree_has_no_findings() {
        let dir = tempfile::tempdir().unwrap();
        matter_tree(dir.path());
        assert!(check_documents_root(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn a_loose_file_at_the_root_is_the_tolerated_legacy_layout() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("memo.md"), BODY).unwrap();
        assert!(check_documents_root(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn two_folders_filing_one_slug_is_a_collision() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Work Product")).unwrap();
        std::fs::create_dir_all(dir.path().join("Discovery")).unwrap();
        std::fs::write(dir.path().join("Work Product/complaint.pdf"), BODY).unwrap();
        std::fs::write(dir.path().join("Discovery/complaint.pdf"), BODY).unwrap();

        let errors = check_documents_root(dir.path()).unwrap();
        assert_eq!(locations(&errors), vec!["Work Product/complaint.pdf"]);
        assert!(
            errors[0]
                .message
                .contains("files as `complaint.pdf`, which `Discovery/complaint.pdf` already"),
            "{}",
            errors[0].message
        );
    }

    #[test]
    fn a_pointer_left_under_the_tree_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Work Product")).unwrap();
        std::fs::write(dir.path().join("Work Product/memo.md.yaml"), BODY).unwrap();
        // An Authority sidecar is not a pointer path and is not reported.
        std::fs::create_dir_all(dir.path().join("cases")).unwrap();
        std::fs::write(dir.path().join("cases/acme.authority.yaml"), BODY).unwrap();

        let errors = check_documents_root(dir.path()).unwrap();
        assert_eq!(locations(&errors), vec!["Work Product/memo.md.yaml"]);
        assert!(errors[0].message.contains("Pointers live under blobs/"));
    }

    #[test]
    fn a_missing_or_non_directory_root_is_an_error_of_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let error = check_documents_root(&dir.path().join("absent"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("does not exist"), "{error}");
        std::fs::write(dir.path().join("a-file"), b"").unwrap();
        let error = check_documents_root(&dir.path().join("a-file"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("is not a directory"), "{error}");
    }

    #[test]
    fn the_permission_note_names_the_macos_setting_for_cloud_storage_paths_only() {
        let cloud = Path::new("/home/lawyer/Library/CloudStorage");
        let blocked = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let note = note_for_block(&blocked, &cloud.join("GoogleDrive-x/My Drive/acme"), cloud)
            .expect("the CloudStorage block is explained");
        assert!(note.contains("Full Disk Access"), "{note}");
        assert!(note.contains("Files and Folders"), "{note}");

        let elsewhere = Path::new("/home/lawyer/documents");
        assert!(note_for_block(&blocked, elsewhere, cloud).is_none());
        assert!(note_for_block(
            &std::io::Error::from(std::io::ErrorKind::NotFound),
            &cloud.join("x"),
            cloud
        )
        .is_none());
    }

    #[test]
    fn drive_local_finds_one_project_folder_across_mounts() {
        let cloud = tempfile::tempdir().unwrap();
        let mount = cloud.path().join("GoogleDrive-nick@firm");
        std::fs::create_dir_all(mount.join("My Drive/other")).unwrap();
        std::fs::create_dir_all(mount.join("Shared drives/Legal/shook-estate")).unwrap();
        let resolved = resolve_drive_local_in("shook-estate", cloud.path()).unwrap();
        assert_eq!(resolved, mount.join("Shared drives/Legal/shook-estate"));
    }

    #[test]
    fn drive_local_refuses_an_ambiguous_or_absent_folder() {
        let cloud = tempfile::tempdir().unwrap();
        let error = resolve_drive_local_in("shook-estate", cloud.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("no folder named `shook-estate`"), "{error}");
        assert!(error.contains("--documents-dir"), "{error}");

        for drive in ["Shared drives/Legal", "Shared drives/Other", "My Drive"] {
            std::fs::create_dir_all(
                cloud
                    .path()
                    .join("GoogleDrive-nick@firm")
                    .join(drive)
                    .join("shook-estate"),
            )
            .unwrap();
        }
        let error = resolve_drive_local_in("shook-estate", cloud.path())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("pass --documents-dir to choose one"),
            "{error}"
        );
    }
}
