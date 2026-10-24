//! Which document an upload files a revision of (LAW-102).
//!
//! The Blob mirroring the file (`documents/<slug>` has `blobs/<slug>.yaml`)
//! *is* the document's name: it carries no slug field, so its path below
//! `blobs/` is the slug. An upload that ignores it and derives a slug from
//! `--file` alone can start a second chain beside the first — one no
//! `site document` verb can retire. So when that pointer exists its slug wins, and a `--slug` that disagrees, or a
//! pointer that cannot be read, is refused before anything is filed.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::document_sync::{
    read_pointer, slash_path, strip_pointer_extension, DOCUMENT_DIRECTORY, POINTER_DIRECTORY,
    POINTER_READ_EXTENSIONS,
};

/// What an upload resolved before it touches the network.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UploadTarget {
    /// The checkout the upload belongs to: the directory holding the
    /// `navigator.yaml` above `--file`, or the working directory when none
    /// names this Project.
    pub(crate) root: PathBuf,
    /// `--file` made absolute, so it reads the same wherever it was typed.
    pub(crate) file: PathBuf,
    /// The slug to file under. `None` leaves the server to name the document
    /// by its filename.
    pub(crate) slug: Option<String>,
    /// Said on stderr when the slug came from a sibling pointer.
    pub(crate) notice: Option<String>,
}

/// Resolve the checkout, file and slug for an upload of `file`.
///
/// # Errors
/// A sibling pointer that is unreadable, ambiguous (`.yaml` and `.yml`), not a
/// regular file, or that names a different slug than an explicit `--slug`.
pub(crate) fn resolve(
    cwd: &Path,
    project_code: &str,
    file: &Path,
    explicit_slug: Option<&str>,
) -> Result<UploadTarget> {
    let cwd = std::path::absolute(cwd).with_context(|| format!("resolve {}", cwd.display()))?;
    let file = locate(&cwd, file);
    let root = checkout_root(&cwd, &file, project_code);
    let explicit = explicit_slug
        .map(str::trim)
        .filter(|value| !value.is_empty());

    // A file outside `documents/` has no document pointer, so whatever sits
    // beside it is none of this command's business.
    let sibling = if is_below_documents(&root, &file) {
        match sibling_pointer(&root, &file)? {
            Some(pointer) => pointer_slug(&root, &pointer)?.map(|slug| (pointer, slug)),
            None => None,
        }
    } else {
        None
    };
    let Some((sibling, slug)) = sibling else {
        let folder = crate::remote::slug_from_documents_folder(&root, &file);
        return Ok(UploadTarget {
            slug: explicit.map(str::to_string).or(folder),
            root,
            file,
            notice: None,
        });
    };
    if sibling
        .extension()
        .is_some_and(|extension| extension == "yml")
    {
        return Err(anyhow!(
            "{} uses a retired pointer suffix; rename it to {} before uploading",
            sibling.display(),
            sibling.with_extension("yaml").display()
        ));
    }
    read_pointer(&sibling)?;
    if let Some(explicit) = explicit.filter(|explicit| *explicit != slug) {
        return Err(anyhow!(
            "`--slug {explicit}` would file a new document beside {}, which already names `{slug}`; \
             drop `--slug` to file a revision of `{slug}`, or move that document with \
             `navigator site document reslug`",
            sibling.display()
        ));
    }
    let notice = format!(
        "using slug `{slug}`, named by the pointer {} for {}",
        sibling.display(),
        file.display()
    );
    Ok(UploadTarget {
        slug: Some(slug),
        root,
        file,
        notice: Some(notice),
    })
}

/// Deterministic, nonrecursive regular-file enumeration. Symlinks are excluded.
pub(crate) fn directory_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("read directory {}", dir.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}

/// The checkout holding `file`: the nearest ancestor with a `navigator.yaml`
/// for `project_code`, else `cwd`. Walking up is what lets an upload typed
/// from a subfolder (or with `..` in the path) read the same slug as one typed
/// from the checkout root.
fn checkout_root(cwd: &Path, file: &Path, project_code: &str) -> PathBuf {
    file.ancestors()
        .skip(1)
        .find(|dir| {
            crate::projects::manifest::read(dir)
                .and_then(|manifest| manifest.project)
                .as_deref()
                == Some(project_code)
        })
        .map_or_else(|| cwd.to_path_buf(), Path::to_path_buf)
}

/// `file` made absolute against `cwd`. A path with `..` in it is resolved
/// against the filesystem when the file exists, so a `..` after a symlinked
/// folder reads the file the OS would open; the lexical form is the fallback.
fn locate(cwd: &Path, file: &Path) -> PathBuf {
    let lexical = crate::document_read::lexical(cwd, file);
    if !file
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return lexical;
    }
    let joined = if file.is_absolute() {
        file.to_path_buf()
    } else {
        cwd.join(file)
    };
    std::fs::canonicalize(joined).unwrap_or(lexical)
}

/// Whether `file` sits below the checkout's `documents/`.
fn is_below_documents(root: &Path, file: &Path) -> bool {
    let (Ok(documents), Ok(file)) = (
        std::fs::canonicalize(root.join(DOCUMENT_DIRECTORY)),
        std::fs::canonicalize(file),
    ) else {
        return false;
    };
    file.starts_with(documents)
}

/// The Blob committed for `file` below `documents/`, in either written
/// spelling.
fn sibling_pointer(root: &Path, file: &Path) -> Result<Option<PathBuf>> {
    let (Ok(documents), Ok(canonical)) = (
        std::fs::canonicalize(root.join(DOCUMENT_DIRECTORY)),
        std::fs::canonicalize(file),
    ) else {
        return Ok(None);
    };
    let Ok(relative) = canonical.strip_prefix(&documents) else {
        return Ok(None);
    };
    let stem = root.join(POINTER_DIRECTORY).join(relative);
    let mut found: Vec<PathBuf> = Vec::new();
    for extension in POINTER_READ_EXTENSIONS {
        let candidate = PathBuf::from(format!("{}.{extension}", stem.display()));
        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.is_file() => found.push(candidate),
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(anyhow!(
                    "{} is a symlink, so the document it should name cannot be trusted",
                    candidate.display()
                ))
            }
            Ok(_) => {
                return Err(anyhow!(
                    "{} is not a regular file, so the document it should name cannot be read",
                    candidate.display()
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", candidate.display()))
            }
        }
    }
    match found.len() {
        0 | 1 => Ok(found.pop()),
        _ => Err(anyhow!(
            "{} and {} both name this document; keep one before uploading",
            found[0].display(),
            found[1].display()
        )),
    }
}

/// The slug a pointer names: its path below the checkout's `blobs/`
/// without the pointer extension. `None` when it is not below `blobs/`.
fn pointer_slug(root: &Path, pointer: &Path) -> Result<Option<String>> {
    // Canonical on both sides, so `..` and the on-disk case agree.
    let blobs = root.join(POINTER_DIRECTORY);
    let (Ok(blobs), Ok(pointer)) = (
        std::fs::canonicalize(&blobs),
        std::fs::canonicalize(pointer),
    ) else {
        return Ok(None);
    };
    let Ok(relative) = pointer.strip_prefix(&blobs) else {
        return Ok(None);
    };
    let slashed = slash_path(relative)?;
    Ok(Some(
        strip_pointer_extension(&slashed)
            .unwrap_or(&slashed)
            .to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::resolve;

    const POINTER: &str = "kind: unclassified\nvisibility: internal\ncurrent_version:\n  version: 1\n  \
        asset_id: 0190a0a0-0000-7000-8000-000000000001\n  created_at: '2026-09-05T12:00:00Z'\n  \
        sha256: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n  size_bytes: 5\n";

    fn checkout() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("documents/drafts")).unwrap();
        std::fs::create_dir_all(dir.path().join("blobs/drafts")).unwrap();
        std::fs::write(
            dir.path().join("navigator.yaml"),
            "project:\n  host: navigator.example.com\n  name: acme\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("documents/drafts/n.md"), "x").unwrap();
        std::fs::write(dir.path().join("blobs/drafts/n.md.yaml"), POINTER).unwrap();
        dir
    }

    /// LAW-140: a file in a folder below `documents/` with no pointer beside
    /// it defaults its slug to its own path there, so the pointer lands
    /// beside the source rather than at the root; a file outside
    /// `documents/` defaults to no slug (the server names it by filename).
    #[test]
    fn a_file_below_documents_defaults_its_slug_to_its_path_there() {
        let dir = checkout();
        std::fs::create_dir_all(dir.path().join("documents/communications")).unwrap();
        std::fs::create_dir_all(dir.path().join("blobs/communications")).unwrap();
        let note = dir
            .path()
            .join("documents/communications/2026-10-09-call.md");
        std::fs::write(&note, "x").unwrap();
        let target = resolve(dir.path(), "acme", &note, None).unwrap();
        assert_eq!(
            target.slug.as_deref(),
            Some("communications/2026-10-09-call.md")
        );
        assert!(target.notice.is_none());

        let outside = dir.path().join("scratch.md");
        std::fs::write(&outside, "x").unwrap();
        let target = resolve(dir.path(), "acme", &outside, None).unwrap();
        assert_eq!(target.slug, None);
        let target = resolve(dir.path(), "acme", &outside, Some("memos/scratch.md")).unwrap();
        assert_eq!(target.slug.as_deref(), Some("memos/scratch.md"));
    }

    #[test]
    fn a_sibling_pointer_names_the_slug_and_says_so() {
        let dir = checkout();
        let target = resolve(
            dir.path(),
            "acme",
            std::path::Path::new("documents/drafts/n.md"),
            None,
        )
        .unwrap();
        assert_eq!(target.slug.as_deref(), Some("drafts/n.md"));
        assert!(target.notice.unwrap().contains("`drafts/n.md`"));
    }

    #[test]
    fn a_disagreeing_slug_is_refused_naming_both_and_the_fix() {
        let dir = checkout();
        let error = resolve(
            dir.path(),
            "acme",
            std::path::Path::new("documents/drafts/n.md"),
            Some("n.md"),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("--slug n.md") && error.contains("`drafts/n.md`"));
        assert!(error.contains("reslug"), "{error}");
    }

    #[test]
    fn another_projects_manifest_does_not_move_the_checkout() {
        let dir = checkout();
        let target = resolve(
            dir.path(),
            "other",
            std::path::Path::new("documents/drafts/n.md"),
            None,
        )
        .unwrap();
        assert_eq!(target.root, std::path::absolute(dir.path()).unwrap());
    }

    fn refusal(dir: &tempfile::TempDir, slug: Option<&str>) -> String {
        resolve(
            dir.path(),
            "acme",
            std::path::Path::new("documents/drafts/n.md"),
            slug,
        )
        .unwrap_err()
        .to_string()
    }

    #[test]
    fn the_notice_says_where_the_slug_came_from_without_claiming_a_revision() {
        let dir = checkout();
        let notice = resolve(
            dir.path(),
            "acme",
            std::path::Path::new("documents/drafts/n.md"),
            None,
        )
        .unwrap()
        .notice
        .unwrap();
        assert!(notice.contains("using slug `drafts/n.md`"), "{notice}");
        assert!(notice.contains("named by the pointer"), "{notice}");
        assert!(notice.contains("blobs/drafts/n.md.yaml"), "{notice}");
        assert!(!notice.contains("revision"), "{notice}");
    }

    #[test]
    fn a_malformed_pointer_is_refused_as_unreadable() {
        let dir = checkout();
        std::fs::write(dir.path().join("blobs/drafts/n.md.yaml"), "kind: [x\n").unwrap();
        assert!(refusal(&dir, None).contains("validate"));
    }

    #[test]
    fn a_retired_yml_pointer_is_refused_as_retired() {
        let dir = checkout();
        std::fs::rename(
            dir.path().join("blobs/drafts/n.md.yaml"),
            dir.path().join("blobs/drafts/n.md.yml"),
        )
        .unwrap();
        assert!(refusal(&dir, None).contains("retired pointer suffix"));
    }

    #[test]
    fn both_spellings_are_refused_as_ambiguous() {
        let dir = checkout();
        std::fs::write(dir.path().join("blobs/drafts/n.md.yml"), POINTER).unwrap();
        assert!(refusal(&dir, None).contains("both name this document"));
    }

    #[test]
    fn a_pointer_that_is_a_directory_is_refused_as_not_a_file() {
        let dir = checkout();
        std::fs::remove_file(dir.path().join("blobs/drafts/n.md.yaml")).unwrap();
        std::fs::create_dir(dir.path().join("blobs/drafts/n.md.yaml")).unwrap();
        assert!(refusal(&dir, None).contains("not a regular file"));
    }

    #[test]
    fn a_symlinked_pointer_is_refused_as_a_symlink() {
        let dir = checkout();
        let real = dir.path().join("blobs/drafts/real.yaml");
        std::fs::rename(dir.path().join("blobs/drafts/n.md.yaml"), &real).unwrap();
        let link = dir.path().join("blobs/drafts/n.md.yaml");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        assert!(refusal(&dir, None).contains("is a symlink"));
    }

    #[test]
    fn a_directory_beside_a_file_outside_documents_is_not_a_refusal() {
        let dir = checkout();
        std::fs::create_dir_all(dir.path().join("scratch/x.md.yaml")).unwrap();
        std::fs::write(dir.path().join("scratch/x.md"), "x").unwrap();
        let target = resolve(
            dir.path(),
            "acme",
            std::path::Path::new("scratch/x.md"),
            None,
        )
        .unwrap();
        assert_eq!(target.slug, None);
    }

    #[test]
    fn the_nearest_matching_manifest_above_the_file_is_the_checkout() {
        let dir = checkout();
        let inner = dir.path().join("inner");
        std::fs::create_dir_all(inner.join("documents/x")).unwrap();
        std::fs::create_dir_all(inner.join("blobs/x")).unwrap();
        std::fs::write(
            inner.join("navigator.yaml"),
            "project:\n  host: navigator.example.com\n  name: acme\n",
        )
        .unwrap();
        std::fs::write(inner.join("documents/x/y.md"), "x").unwrap();
        let target = resolve(
            dir.path(),
            "acme",
            std::path::Path::new("inner/documents/x/y.md"),
            None,
        )
        .unwrap();
        assert_eq!(target.root, inner);
        assert_eq!(target.slug.as_deref(), Some("x/y.md"));

        // A nested manifest for another Project is skipped.
        let other = resolve(
            dir.path(),
            "elsewhere",
            std::path::Path::new("inner/documents/x/y.md"),
            None,
        )
        .unwrap();
        assert_eq!(other.root, std::path::absolute(dir.path()).unwrap());
    }
}

#[cfg(test)]
mod batch_tests {
    #[test]
    fn same_stem_and_punctuation_keep_distinct_extension_preserving_identities() {
        let dir = tempfile::tempdir().unwrap();
        let mut slugs = std::collections::HashSet::new();
        for filename in ["a.pdf", "a.PNG", "a (1).pdf", "a-1.pdf"] {
            let file = dir.path().join(filename);
            std::fs::write(&file, filename).unwrap();
            let target = super::resolve(dir.path(), "acme", &file, None).unwrap();
            let slug = target.slug.as_deref().unwrap_or(filename);
            assert!(store::documents::validate_document_slug(filename, slug).is_ok());
            assert!(
                slugs.insert(slug.to_string()),
                "filename identities must not collapse"
            );
        }
    }

    #[test]
    fn enumeration_is_sorted_nonrecursive_and_excludes_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("z.png"), "z").unwrap();
        std::fs::write(dir.path().join("a.PDF"), "a").unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested/x.pdf"), "x").unwrap();
        std::os::unix::fs::symlink(dir.path().join("z.png"), dir.path().join("link.png")).unwrap();
        let files = super::directory_files(dir.path()).unwrap();
        assert_eq!(
            files,
            vec![dir.path().join("a.PDF"), dir.path().join("z.png")]
        );
        for file in files {
            let target = super::resolve(dir.path(), "acme", &file, None).unwrap();
            assert!(target.slug.is_none());
            let filename = file.file_name().unwrap().to_str().unwrap();
            assert!(store::documents::validate_document_slug(filename, filename).is_ok());
        }
    }
}
