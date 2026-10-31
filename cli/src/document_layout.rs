//! Where a Project document's bytes live in a checkout (LAW-189, LAW-215).
//!
//! ```text
//! blobs/<slug>.yaml               the committed pointer (a Blob)
//! documents/                      Git-ignored, organised for a lawyer
//!   Client Production/<slug>
//!   Correspondence (Served)/<slug>
//!   Discovery (Served)/<slug>
//!   Docket Filings/<slug>
//!   Court Rules/<slug>
//!   Engagement/<slug>
//!   Work Product/<slug>
//!     Client Calls (Work Product)/<slug>
//!     Correspondence (Work Product)/<slug>
//!     Discovery (Work Product)/<slug>
//!     Motions (Work Product)/<slug>
//!     Pleadings (Work Product)/<slug>
//!     Research (Work Product)/<slug>
//!     Stipulations (Work Product)/<slug>
//!   onboarding/ offboarding/ invoices/ cases/ rules/
//! ```
//!
//! The slug is unchanged by the folders: a pointer's bytes sit in the
//! [`MatterFolder`] its `folder:` names, else the one its `kind` maps to — a
//! `Work Product` subfolder's name is its whole path below `documents/`, so
//! its bytes nest under `Work Product/`. The sibling folders keep their
//! bytes at `documents/<slug>` for this release, because their own rules
//! (signed engagement letters, invoices, Authority captures) are keyed to
//! that path. The bare `Discovery` folder a pre-LAW-215 checkout holds is
//! refiled by `project repository migrate-folders`, never created here.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use store::document_pointers::{DocumentPointer, MatterFolder};

use crate::document_sync::{DOCUMENT_DIRECTORY, POINTER_DIRECTORY};

pub(crate) use store::document_pointers::SIBLING_FOLDERS;

fn first_component(relative: &Path) -> Option<&str> {
    relative
        .components()
        .next()
        .and_then(|part| part.as_os_str().to_str())
}

/// Whether the slug (or path below `documents/`) sits in a sibling folder.
pub(crate) fn is_sibling(relative: &Path) -> bool {
    first_component(relative).is_some_and(|first| SIBLING_FOLDERS.contains(&first))
}

/// Whether the path below `documents/` begins with a fixed `Work Product`
/// subfolder's two-component name (LAW-215).
fn begins_with_work_product_subfolder(relative: &Path) -> bool {
    MatterFolder::ALL
        .iter()
        .filter(|folder| folder.is_work_product_subfolder())
        .any(|folder| relative.starts_with(folder.as_str()))
}

/// The matter folder a path below `documents/` sits in, if any. A fixed
/// `Work Product` subfolder wins over its bare parent, so bytes under
/// `Work Product/Research (Work Product)/` name the subfolder (LAW-215).
pub(crate) fn matter_folder_of(relative: &Path) -> Option<MatterFolder> {
    if begins_with_work_product_subfolder(relative) {
        return MatterFolder::ALL.iter().copied().find(|folder| {
            folder.is_work_product_subfolder() && relative.starts_with(folder.as_str())
        });
    }
    first_component(relative).and_then(MatterFolder::parse)
}

/// The bytes path, below `documents/`, for `slug` kept in `folder`.
pub(crate) fn bytes_relative(slug: &Path, folder: MatterFolder) -> PathBuf {
    if is_sibling(slug) {
        slug.to_path_buf()
    } else {
        Path::new(folder.as_str()).join(slug)
    }
}

/// The bytes path for the document `pointer` describes, below the checkout.
pub(crate) fn document_path(root: &Path, slug: &Path, pointer: &DocumentPointer) -> PathBuf {
    root.join(DOCUMENT_DIRECTORY)
        .join(bytes_relative(slug, pointer.matter_folder()))
}

/// The slug a bytes path (below `documents/`) names, and the matter folder it
/// was found in. A file outside every matter folder is its own slug.
pub(crate) fn slug_of_bytes(relative: &Path) -> (PathBuf, Option<MatterFolder>) {
    match matter_folder_of(relative) {
        Some(folder) => (
            relative
                .strip_prefix(folder.as_str())
                .unwrap_or(relative)
                .to_path_buf(),
            Some(folder),
        ),
        None => (relative.to_path_buf(), None),
    }
}

/// The `folder:` a pointer of `kind` records for bytes kept in `folder`:
/// `None` when `kind` already maps there, so a pointer names a folder only
/// when it has to.
pub(crate) fn recorded_folder(kind: &str, folder: MatterFolder) -> Option<MatterFolder> {
    (MatterFolder::for_kind(kind) != folder).then_some(folder)
}

/// Create every matter folder of a fresh checkout's tree below `documents/`,
/// and `blobs/`, so incoming documents always have a home. The bare
/// `Discovery` folder a pre-LAW-215 checkout holds is created by no fresh
/// checkout (LAW-215). Git tracks none of them while empty.
pub(crate) fn ensure_tree(root: &Path) -> Result<()> {
    let documents = root.join(DOCUMENT_DIRECTORY);
    let folders = MatterFolder::TREE
        .iter()
        .map(|folder| documents.join(folder.as_str()))
        .chain([root.join(POINTER_DIRECTORY)]);
    for path in folders {
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(anyhow!(
                    "{} must be a directory to hold matter documents",
                    path.display()
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(&path)
                    .with_context(|| format!("create {}", path.display()))?;
            }
            Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
        }
    }
    Ok(())
}

/// One file a relocation moved, or would move under a dry run, as paths
/// relative to the checkout root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Move {
    pub(crate) from: String,
    pub(crate) to: String,
}

/// What [`relocate_bytes`] did: the moves, and the files it left alone
/// because the destination already exists.
#[derive(Debug, Default)]
pub(crate) struct Relocation {
    pub(crate) moves: Vec<Move>,
    pub(crate) skipped: Vec<String>,
}

impl Relocation {
    /// Print every move and every skip, one per line.
    pub(crate) fn report(&self, dry_run: bool) {
        let verb = if dry_run { "would move" } else { "moved" };
        for movement in &self.moves {
            println!("{verb} {} -> {}", movement.from, movement.to);
        }
        for skipped in &self.skipped {
            println!("left {skipped}");
        }
    }
}

/// Move the local bytes of every committed pointer from `documents/<slug>`,
/// where they sat before the matter folders, into the folder the pointer
/// names. Pointers are not read for anything else and never written, so a
/// checksum or asset id cannot change. Idempotent; a destination that
/// already exists is left alone and reported. `dry_run` touches nothing.
pub(crate) fn relocate_bytes(root: &Path, dry_run: bool) -> Result<Relocation> {
    let documents = root.join(DOCUMENT_DIRECTORY);
    let mut relocation = Relocation::default();
    if !documents.is_dir() {
        return Ok(relocation);
    }
    crate::document_sync::ensure_documents_tree_is_safe(&documents)?;
    for relative in crate::document_read::discover_pointers(root)? {
        let Some(slug) = crate::document_sync::slug_for_pointer(&relative) else {
            continue;
        };
        if is_sibling(&slug) || matter_folder_of(&slug).is_some() {
            continue;
        }
        let legacy = documents.join(&slug);
        if !legacy.is_file() {
            continue;
        }
        let Some(pointer) = crate::document_sync::read_pointer(&root.join(&relative))? else {
            continue;
        };
        let target = document_path(root, &slug, &pointer);
        if target.exists() {
            relocation.skipped.push(format!(
                "{}: {} already exists",
                display(root, &legacy),
                display(root, &target)
            ));
            continue;
        }
        move_file(root, &legacy, &target, dry_run)?;
        relocation.moves.push(Move {
            from: display(root, &legacy),
            to: display(root, &target),
        });
    }
    if !dry_run {
        remove_emptied_directories(&documents)?;
    }
    Ok(relocation)
}

/// `navigator project repository migrate-documents`: move a checkout to the
/// `blobs/` layout and its bytes into their matter folders, reporting every
/// move. Offline and idempotent.
pub(crate) fn migrate_checkout(root: &Path, dry_run: bool) -> Result<()> {
    let root = root
        .canonicalize()
        .with_context(|| format!("resolve {}", root.display()))?;
    if !root.join(crate::projects::manifest::FILE).is_file() {
        return Err(anyhow!(
            "{} is not a Project checkout: it has no navigator.yaml",
            root.display()
        ));
    }
    let (legacy, _) = crate::document_sync::legacy_layout(&root)?;
    let verb = if dry_run { "would move" } else { "moved" };
    for pointer in &legacy {
        println!(
            "{verb} {} -> {}",
            display(&root, &pointer.from),
            display(&root, &pointer.to)
        );
    }
    let pointers = if dry_run {
        legacy.len()
    } else {
        crate::document_sync::migrate_legacy_layout(&root)?
    };
    // A dry run has not moved the pointers yet, so it cannot read where
    // their bytes belong; it reports the pointer moves only.
    let relocation = if dry_run {
        Relocation::default()
    } else {
        let relocation = relocate_bytes(&root, false)?;
        relocation.report(false);
        ensure_tree(&root)?;
        relocation
    };
    println!(
        "{} file(s) {verb}, {} left in place",
        pointers + relocation.moves.len(),
        relocation.skipped.len()
    );
    Ok(())
}

/// `navigator project repository migrate-folders`: reslug a checkout into
/// the extended matter folder tree (LAW-215). Pointers naming the bare
/// `Discovery` folder are rewritten to `Discovery (Served)`; every committed
/// pointer's bytes move from wherever the five-folder tree kept them into
/// the folder the pointer resolves to now — checksums, asset ids, and
/// version chains untouched. Bytes under `documents/` no pointer names are
/// left where they are and listed, never guessed at. Idempotent; `dry_run`
/// reports without touching anything.
///
/// # Errors
/// A checkout that is not one, an unsafe documents tree, or a move the
/// filesystem refuses.
pub(crate) fn migrate_folder_tree(root: &Path, dry_run: bool) -> Result<()> {
    let root = root
        .canonicalize()
        .with_context(|| format!("resolve {}", root.display()))?;
    if !root.join(crate::projects::manifest::FILE).is_file() {
        return Err(anyhow!(
            "{} is not a Project checkout: it has no navigator.yaml",
            root.display()
        ));
    }
    let documents = root.join(DOCUMENT_DIRECTORY);
    let mut moved = 0_usize;
    let mut refiled = 0_usize;
    let mut slugs = std::collections::BTreeSet::new();
    if documents.is_dir() {
        crate::document_sync::ensure_documents_tree_is_safe(&documents)?;
        for relative in crate::document_read::discover_pointers(&root)? {
            let Some(slug) = crate::document_sync::slug_for_pointer(&relative) else {
                continue;
            };
            if is_sibling(&slug) {
                continue;
            }
            let pointer_path = root.join(&relative);
            let Some(pointer) = crate::document_sync::read_pointer(&pointer_path)? else {
                continue;
            };
            let _ = slugs.insert(slug.clone());
            // A pointer on the bare `Discovery` folder is refiled onto the
            // served twin the fresh tree creates; its YAML is rewritten —
            // the only pointer write this verb makes (LAW-215).
            let legacy_discovery = pointer.folder == Some(MatterFolder::Discovery);
            let mut effective = pointer.clone();
            if legacy_discovery {
                effective.folder = Some(MatterFolder::DiscoveryServed);
            }
            if legacy_discovery && !dry_run {
                let raw = std::fs::read_to_string(&pointer_path)
                    .with_context(|| format!("read {}", display(&root, &pointer_path)))?;
                let refiled_yaml =
                    raw.replace("folder: Discovery\n", "folder: Discovery (Served)\n");
                std::fs::write(&pointer_path, refiled_yaml)
                    .with_context(|| format!("rewrite {}", display(&root, &pointer_path)))?;
            }
            if legacy_discovery {
                refiled += 1;
                println!(
                    "{verb} {} -> folder: Discovery (Served)",
                    display(&root, &pointer_path),
                    verb = if dry_run { "would refile" } else { "refiled" }
                );
            }
            let target = document_path(&root, &slug, &effective);
            if target.is_file() {
                continue;
            }
            let source = MatterFolder::ALL
                .iter()
                .map(|folder| documents.join(folder.as_str()).join(&slug))
                .find(|path| path.is_file())
                .or_else(|| {
                    let flat = documents.join(&slug);
                    flat.is_file().then_some(flat)
                });
            let Some(legacy) = source else {
                continue;
            };
            if legacy == target {
                continue;
            }
            move_file(&root, &legacy, &target, dry_run)?;
            moved += 1;
            println!(
                "{verb} {} -> {}",
                display(&root, &legacy),
                display(&root, &target),
                verb = if dry_run { "would move" } else { "moved" }
            );
        }
        if !dry_run {
            ensure_tree(&root)?;
        }
        list_unplaced_discovery(&root, &documents, &slugs);
    }
    println!("{refiled} pointer(s) refiled, {moved} file(s) moved");
    Ok(())
}

/// List the bytes under the bare `Discovery` folder no committed pointer
/// names: whether they were served or are the firm's draft is the lawyer's
/// call, not the migration's (LAW-215).
fn list_unplaced_discovery(
    root: &Path,
    documents: &Path,
    slugs: &std::collections::BTreeSet<PathBuf>,
) {
    let bare = documents.join(MatterFolder::Discovery.as_str());
    if !bare.is_dir() {
        return;
    }
    for entry in walkdir::WalkDir::new(&bare)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let below = entry
            .path()
            .strip_prefix(&bare)
            .unwrap_or(entry.path())
            .to_path_buf();
        if !slugs.contains(&below) {
            println!(
                "unplaced {} — no pointer names it; file it as served or draft",
                display(root, entry.path())
            );
        }
    }
}

fn move_file(root: &Path, from: &Path, to: &Path, dry_run: bool) -> Result<()> {
    if dry_run {
        return Ok(());
    }
    crate::document_sync::ensure_document_path_is_safe(root, from, false, true)?;
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    crate::document_sync::ensure_document_path_is_safe(root, to, true, false)?;
    std::fs::rename(from, to)
        .with_context(|| format!("move {} to {}", from.display(), to.display()))
}

/// Move bytes left at `documents/<slug>` for a live document with no pointer
/// yet into the matter folder its live `kind` names, so `project sync`
/// compares them with the live copy instead of downloading a second one.
/// `removed` slugs are a pending deletion's leftovers and stay where they are.
pub(crate) fn relocate_unpointed(
    root: &Path,
    live: &[(PathBuf, String)],
    removed: impl Fn(&str) -> bool,
) -> Result<Vec<Move>> {
    let documents = root.join(DOCUMENT_DIRECTORY);
    let mut moves = Vec::new();
    for (slug, kind) in live {
        if is_sibling(slug) || matter_folder_of(slug).is_some() {
            continue;
        }
        let legacy = documents.join(slug);
        let pointer = crate::document_sync::pointer_path_for_slug(root, slug);
        let slashed = crate::document_sync::slash_path(slug)?;
        if !legacy.is_file() || pointer.exists() || removed(&slashed) {
            continue;
        }
        let target = documents.join(bytes_relative(slug, MatterFolder::for_kind(kind)));
        if target.exists() {
            continue;
        }
        move_file(root, &legacy, &target, false)?;
        moves.push(Move {
            from: display(root, &legacy),
            to: display(root, &target),
        });
    }
    Ok(moves)
}

/// Remove directories the relocation emptied, outside the matter folders and
/// the sibling folders. A directory still holding anything is kept.
fn remove_emptied_directories(documents: &Path) -> Result<()> {
    let mut directories: Vec<PathBuf> = walkdir::WalkDir::new(documents)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_dir())
        .map(walkdir::DirEntry::into_path)
        .filter(|path| {
            path.strip_prefix(documents)
                .is_ok_and(|below| !is_sibling(below) && matter_folder_of(below).is_none())
        })
        .collect();
    // Deepest first, so a parent is empty once its children are gone.
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        let empty = std::fs::read_dir(&directory)
            .with_context(|| format!("read {}", directory.display()))?
            .next()
            .is_none();
        if empty {
            std::fs::remove_dir(&directory)
                .with_context(|| format!("remove {}", directory.display()))?;
        }
    }
    Ok(())
}

fn display(root: &Path, path: &Path) -> String {
    crate::document_sync::slash_path(path.strip_prefix(root).unwrap_or(path))
        .unwrap_or_else(|_| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slug_maps_to_a_matter_folder_and_back() {
        let slug = Path::new("pleadings/complaint.pdf");
        assert_eq!(
            bytes_relative(slug, MatterFolder::DocketFilings),
            PathBuf::from("Docket Filings/pleadings/complaint.pdf")
        );
        assert_eq!(
            slug_of_bytes(Path::new("Docket Filings/pleadings/complaint.pdf")),
            (slug.to_path_buf(), Some(MatterFolder::DocketFilings))
        );
        assert_eq!(
            slug_of_bytes(Path::new("pleadings/complaint.pdf")),
            (slug.to_path_buf(), None)
        );
    }

    #[test]
    fn a_sibling_folder_keeps_its_bytes_at_the_slug() {
        let slug = Path::new("onboarding/letter_signed.pdf");
        assert_eq!(bytes_relative(slug, MatterFolder::WorkProduct), slug);
        assert!(is_sibling(slug));
    }

    #[test]
    fn a_folder_is_recorded_only_when_the_kind_does_not_name_it() {
        assert_eq!(
            recorded_folder("exhibit", MatterFolder::ClientProduction),
            None
        );
        assert_eq!(
            recorded_folder("unclassified", MatterFolder::Discovery),
            Some(MatterFolder::Discovery)
        );
        // LAW-215: the new kind-to-folder defaults record the subfolder a
        // kind now names, and a file dropped into one needs no `folder:`.
        assert_eq!(
            recorded_folder("memo", MatterFolder::ResearchWorkProduct),
            None
        );
        assert_eq!(
            recorded_folder("transcript", MatterFolder::ClientCallsWorkProduct),
            None
        );
        assert_eq!(
            recorded_folder("onboarding", MatterFolder::Engagement),
            None
        );
        assert_eq!(
            recorded_folder("memo", MatterFolder::WorkProduct),
            Some(MatterFolder::WorkProduct)
        );
    }

    /// LAW-215: a fixed `Work Product` subfolder's two-component name is
    /// matched whole, so bytes under one name the subfolder, not the bare
    /// parent, and its slug strips the whole nested prefix.
    #[test]
    fn a_work_product_subfolder_is_matched_whole_and_strips_its_nested_prefix() {
        let slug = Path::new("motions/dismiss.pdf");
        assert!(begins_with_work_product_subfolder(Path::new(
            "Work Product/Motions (Work Product)/motions/dismiss.pdf"
        )));
        assert_eq!(
            matter_folder_of(Path::new(
                "Work Product/Motions (Work Product)/motions/dismiss.pdf"
            )),
            Some(MatterFolder::MotionsWorkProduct)
        );
        assert_eq!(
            slug_of_bytes(Path::new(
                "Work Product/Motions (Work Product)/motions/dismiss.pdf"
            )),
            (slug.to_path_buf(), Some(MatterFolder::MotionsWorkProduct))
        );
        // A file directly under `Work Product/` still names the parent, and
        // a lookalike first component is not a subfolder.
        assert_eq!(
            matter_folder_of(Path::new("Work Product/notes.md")),
            Some(MatterFolder::WorkProduct)
        );
        assert_eq!(
            matter_folder_of(Path::new("Work Product (old)/notes.md")),
            None
        );
        assert_eq!(
            bytes_relative(slug, MatterFolder::MotionsWorkProduct),
            PathBuf::from("Work Product/Motions (Work Product)/motions/dismiss.pdf")
        );
    }
}
