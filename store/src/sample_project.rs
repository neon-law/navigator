//! A staged **sample project**: the source a headless sample repository
//! stages for a local boot ([`read_staged_source`]).
//!
//! The three sample repositories are headless. `navigator dev sample-project`
//! stages each one's `navigator.yaml`, its `drafts/`, and the commit it was
//! cloned at under [`STAGE_ENV`]; the development seed reads that stage to
//! store the matter's pages and open its Notations.
//!
//! Boot re-reads the manifest rather than trusting the staging path, and
//! refuses a stage naming a different Project — reading one would put a
//! matter's pages and Drafts on another matter.

use std::path::{Path, PathBuf};

/// Names the directory the staged projects sit under — one subdirectory per
/// Project code, each a `navigator.yaml` beside its `drafts/`. Generated
/// local development environments set it before `web` starts.
pub const STAGE_ENV: &str = "NAVIGATOR_SAMPLE_PROJECTS_DIR";

/// The manifest a Project repository carries at its root. It names the
/// Project the stage belongs to.
///
/// The same filename and `project:` key a Project repository declares itself
/// in (`cli::projects::repository::PROJECT_MANIFEST`) — a staged
/// manifest is a copy of that same file, not a distinct schema. Unknown keys,
/// such as `host:`, are ignored rather than refused: a repository's own
/// manifest may carry them, and this reader only needs `project:`.
pub const MANIFEST_FILE: &str = "navigator.yaml";

/// Why a manifest could not name a Project.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ManifestError {
    #[error("navigator.yaml is not valid YAML: {0}")]
    Unparsable(String),
    #[error("navigator.yaml names `{0}`, which is not a valid Project code")]
    InvalidCode(String),
    #[error("navigator.yaml names Project `{found}`, but this stage is for `{expected}`")]
    WrongProject { expected: String, found: String },
}

/// Read the Project code a bundle declares, and prove it is one.
///
/// One field is read here. It is a manifest rather than a convention over the
/// repository name because the repository name is the application's to choose:
/// a repository may be named for something other than the Project it mounts on,
/// and no rule can recover the code from a name that never encoded it. Unknown
/// keys are ignored because the full Project-repository manifest carries
/// coordinates and handles this publisher does not need.
///
/// The code is validated with the same [`crate::projects::is_valid_code`] the
/// store uses, so a manifest cannot introduce a code the rest of Navigator
/// would reject — and cannot smuggle path segments into a bucket key.
pub fn project_code_from_manifest(yaml: &str) -> Result<String, ManifestError> {
    let manifest: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(|e| ManifestError::Unparsable(e.to_string()))?;
    let mapping = manifest
        .as_mapping()
        .ok_or_else(|| ManifestError::Unparsable("navigator.yaml must be a mapping".to_string()))?;
    let project = mapping.get("project").ok_or_else(|| {
        ManifestError::Unparsable("navigator.yaml is missing project".to_string())
    })?;
    let project = project
        .as_mapping()
        .and_then(|map| map.get("name"))
        .unwrap_or(project);
    let name = project
        .as_str()
        .ok_or_else(|| {
            ManifestError::Unparsable("navigator.yaml project must be text".to_string())
        })?
        .trim()
        .to_string();
    if !crate::projects::is_valid_code(&name) {
        return Err(ManifestError::InvalidCode(name));
    }
    Ok(name)
}

/// [`project_code_from_manifest`], and additionally that the bundle declares
/// the Project it is about to be published under.
///
/// Publishing a bundle that names a different Project would put one matter's
/// application on another matter's portal, so a mismatch is refused rather
/// than reconciled.
pub fn project_code_for(yaml: &str, expected: &str) -> Result<String, ManifestError> {
    let found = project_code_from_manifest(yaml)?;
    if found == expected {
        Ok(found)
    } else {
        Err(ManifestError::WrongProject {
            expected: expected.to_string(),
            found,
        })
    }
}

/// The Drafts directory a staged headless project carries beside its
/// manifest: the repository's own `drafts/`, copied verbatim.
pub const DRAFTS_DIR: &str = "drafts";

/// The file holding the commit the staged checkout was cloned at, one line.
/// A Draft persisted from the stage pins this commit as its provenance.
pub const SOURCE_COMMIT_FILE: &str = "source-commit";

/// A staged Project repository read for its source, not for a bundle: the
/// manifest (whose `pages:` the seed stores) and its Drafts (which the seed
/// persists and opens a Notation on). A repository with no Portal stages
/// exactly this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedSource {
    /// The manifest text, verbatim.
    pub manifest: String,
    /// The `pages:` entries the manifest declares, in order.
    pub pages: Vec<rules::pages::PageEntry>,
    /// Each Draft as `(code, contents)`, sorted by code. The code is the
    /// file stem, the identity a Notation pins.
    pub drafts: Vec<(String, String)>,
    /// The commit the checkout was cloned at, when the stage recorded one.
    pub commit: Option<String>,
}

/// Why a staged source could not be read.
#[derive(Debug, thiserror::Error)]
pub enum StagedSourceError {
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("navigator.yaml pages: {0}")]
    Pages(String),
}

fn read_text(path: &Path) -> Result<String, StagedSourceError> {
    std::fs::read_to_string(path).map_err(|source| StagedSourceError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Read one matter's staged source from the environment: `None` when
/// nothing is staged for it, so an unstaged matter keeps whatever its last
/// sync stored.
#[must_use]
pub fn staged_source_dir(
    project_code: &str,
    get: impl Fn(&str) -> Option<String>,
) -> Option<PathBuf> {
    let configured = get(STAGE_ENV)?;
    let trimmed = configured.trim();
    if trimmed.is_empty() || !crate::projects::is_valid_code(project_code) {
        return None;
    }
    let root = PathBuf::from(trimmed).join(project_code);
    root.join(MANIFEST_FILE).is_file().then_some(root)
}

/// Read a staged source at `root`, refusing a manifest that names a Project
/// other than `expected`: one matter's pages and Drafts must never land on
/// another matter.
///
/// # Errors
/// [`StagedSourceError`] when the manifest is missing, names another
/// Project, or carries a `pages:` block that does not parse, or when a Draft
/// cannot be read.
pub fn read_staged_source(root: &Path, expected: &str) -> Result<StagedSource, StagedSourceError> {
    let manifest = read_text(&root.join(MANIFEST_FILE))?;
    project_code_for(&manifest, expected)?;
    let document: serde_yaml::Value =
        serde_yaml::from_str(&manifest).map_err(|e| ManifestError::Unparsable(e.to_string()))?;
    let pages = document
        .as_mapping()
        .map(rules::pages::entries)
        .transpose()
        .map_err(StagedSourceError::Pages)?
        .unwrap_or_default();
    let mut drafts = Vec::new();
    let drafts_dir = root.join(DRAFTS_DIR);
    if drafts_dir.is_dir() {
        let entries = std::fs::read_dir(&drafts_dir).map_err(|source| StagedSourceError::Io {
            path: drafts_dir.clone(),
            source,
        })?;
        for entry in entries {
            let path = entry
                .map_err(|source| StagedSourceError::Io {
                    path: drafts_dir.clone(),
                    source,
                })?
                .path();
            let is_markdown = path.extension().and_then(std::ffi::OsStr::to_str) == Some("md");
            let Some(code) = path.file_stem().and_then(std::ffi::OsStr::to_str) else {
                continue;
            };
            if is_markdown && path.is_file() {
                drafts.push((code.to_string(), read_text(&path)?));
            }
        }
    }
    drafts.sort();
    let commit = std::fs::read_to_string(root.join(SOURCE_COMMIT_FILE))
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());
    Ok(StagedSource {
        manifest,
        pages,
        drafts,
        commit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A headless stage is a manifest and its Drafts, with no `dist/`: it is
    /// read for its pages and Drafts, and a manifest naming another matter is
    /// refused rather than read onto this one.
    #[test]
    fn a_headless_stage_reads_pages_and_drafts_and_refuses_another_matter() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("sample-estate");
        write(
            &root,
            MANIFEST_FILE,
            b"version: 26.10.12\nproject:\n  host: h.example\n  name: sample-estate\npages:\n  \
              - slug: research\n    kind: authority_library\n    title: Research\n    lenses:\n      \
              lawyer: [authority_table, boundary_note, provenance_statement]\n",
        );
        write(
            &root,
            "drafts/last_will.md",
            b"---\ntitle: Will\n---\nBody\n",
        );
        write(&root, "drafts/notes.txt", b"not a draft");
        write(&root, SOURCE_COMMIT_FILE, b"abc123\n");

        let get = |key: &str| (key == STAGE_ENV).then(|| dir.path().display().to_string());
        assert_eq!(staged_source_dir("sample-estate", get), Some(root.clone()));
        assert_eq!(staged_source_dir("sample-litigation", get), None);

        let source = read_staged_source(&root, "sample-estate").expect("staged source");
        assert_eq!(source.pages.len(), 1);
        assert_eq!(source.pages[0].slug, "research");
        assert_eq!(source.drafts.len(), 1);
        assert_eq!(source.drafts[0].0, "last_will");
        assert_eq!(source.commit.as_deref(), Some("abc123"));

        let error = read_staged_source(&root, "sample-litigation")
            .expect_err("a manifest naming another matter is refused");
        assert!(error.to_string().contains("sample-litigation"), "{error}");
    }

    /// Write a file, creating parents, so a test can describe a stage by its
    /// leaves.
    fn write(root: &Path, relative: &str, bytes: &[u8]) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("create dirs");
        std::fs::write(path, bytes).expect("write");
    }

    #[test]
    fn the_manifest_names_the_project_the_bundle_mounts_on() {
        assert_eq!(
            project_code_from_manifest("project: sample-litigation\n").expect("a code"),
            "sample-litigation"
        );
    }

    /// The retired `name:` key is not a quiet alias for `project:` — there is
    /// no serde alias, so a manifest still spelled the old way is exactly as
    /// unreadable as one carrying neither key. Deserializing into a required
    /// `project: String` field with nothing to fill it is a missing-field
    /// parse failure, which `project_code_from_manifest` reports as
    /// `Unparsable`.
    #[test]
    fn the_retired_name_key_is_rejected_rather_than_read() {
        assert!(matches!(
            project_code_from_manifest("name: sample-litigation\n"),
            Err(ManifestError::Unparsable(_))
        ));
    }

    /// A manifest may carry keys this reader does not need — `host:`, the one
    /// a Project repository's own manifest adds — without failing to parse.
    /// This is what keeps the downstream `DEPLOYMENTS` table's own additions
    /// from breaking this gate.
    #[test]
    fn an_unknown_key_alongside_project_is_ignored() {
        assert_eq!(
            project_code_from_manifest("host: www.example.com\nproject: sample-litigation\n")
                .expect("a code"),
            "sample-litigation"
        );
    }

    #[test]
    fn a_manifest_cannot_introduce_a_code_the_store_would_reject() {
        // Path segments are the dangerous case: a bucket key is built from
        // this, so `../` or a slash must never survive validation. `new` is
        // reserved; uppercase is refused because a code is also a
        // case-insensitive drive folder name.
        for bad in [
            "../etc",
            "Donut-Litigation",
            "sample-litigation/portal",
            "",
            "new",
            "-x",
            "a--b",
        ] {
            assert!(
                matches!(
                    project_code_from_manifest(&format!("project: \"{bad}\"\n")),
                    Err(ManifestError::InvalidCode(_))
                ),
                "`{bad}` must not validate as a Project code"
            );
        }
    }

    #[test]
    fn unparsable_yaml_is_an_error_rather_than_a_default() {
        assert!(matches!(
            project_code_from_manifest("project: [this is not a string]\n"),
            Err(ManifestError::Unparsable(_))
        ));
    }

    #[test]
    fn a_bundle_declaring_another_project_is_refused() {
        assert_eq!(
            project_code_for("project: henderson\n", "sample-litigation"),
            Err(ManifestError::WrongProject {
                expected: "sample-litigation".to_string(),
                found: "henderson".to_string(),
            }),
            "one matter's application must not land on another matter's portal"
        );
        assert_eq!(
            project_code_for("project: sample-litigation\n", "sample-litigation").expect("a code"),
            "sample-litigation"
        );
    }
}
