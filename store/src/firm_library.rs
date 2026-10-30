//! A Firm's [Firm Library](../../docs/glossary/firm-library.md): the
//! Drafts and defaults every matter of that Firm starts from.
//!
//! The library is one repository in the Firm's organization, named
//! [`REPOSITORY`] — `<firm-slug>/library`, the same path shape as a Project
//! repository, which is why `library` is a reserved Project code. It holds:
//!
//! - `drafts/*.md` — Drafts in exactly the Project repository format,
//!   validated by the same rules and persisted by the same engine
//!   ([`crate::template_source::persist_contents_in`]) into the Firm's own
//!   version line ([`crate::templates::Scope::Firm`]), each pinned to its
//!   commit SHA and body digest.
//! - [`MANIFEST_FILE`] — a closed [`Manifest`] naming the Firm's default
//!   `retainer`.
//!
//! The library is read the way a Project repository is: from the bare
//! repository store at `main`'s HEAD, when a door needs it. A Firm with no
//! library repository, or one with no commits, has no library — every
//! caller then behaves exactly as it does without one.

use std::sync::Arc;

use cloud::StorageService;
use serde::Deserialize;
use uuid::Uuid;

use crate::surreal::SurrealDb;
use crate::template_source::{Persisted, TemplateSourceError};
use crate::templates::{Scope, Template, TemplateError};

/// The library's repository name inside the Firm's organization. Reserved
/// as a Project code (`cloud::workspace::RESERVED_PROJECT_CODES`).
pub const REPOSITORY: &str = "library";

/// The library's root manifest.
pub const MANIFEST_FILE: &str = "library.yaml";

/// `library.yaml`. Closed: an unknown key is refused, so a misspelt
/// `retainer` cannot silently leave the Firm on the shared default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The Template code every new matter of this Firm opens on: a Draft in
    /// the library, or a shared-catalog code. Its `kind` must open a matter.
    #[serde(default)]
    pub retainer: Option<String>,
}

/// Errors reading or syncing a Firm Library.
#[derive(Debug, thiserror::Error)]
pub enum FirmLibraryError {
    #[error("firm `{0}` does not exist")]
    FirmNotFound(Uuid),
    #[error("`library.yaml` is invalid: {0}")]
    Manifest(String),
    #[error("library retainer `{0}` names no Draft in the library or the shared catalog")]
    RetainerNotFound(String),
    #[error("library retainer `{code}` is kind `{kind}`, which does not open a matter")]
    RetainerDoesNotOpenAMatter { code: String, kind: String },
    #[error(transparent)]
    Source(#[from] TemplateSourceError),
    #[error("template: {0}")]
    Template(#[from] TemplateError),
    #[error("repo: {0}")]
    Repo(#[from] repos::RepoError),
    #[error("{0}")]
    Store(String),
}

/// The library's name in the bare repository store: `<firm-slug>/library`,
/// its path on the forge. No Project code contains `/`, so it cannot
/// collide with a Project repository.
#[must_use]
pub fn repository_name(firm_slug: &str) -> String {
    format!("{firm_slug}/{REPOSITORY}")
}

/// Parse `library.yaml`.
///
/// # Errors
/// [`FirmLibraryError::Manifest`] for malformed YAML or an unknown key.
pub fn parse_manifest(raw: &str) -> Result<Manifest, FirmLibraryError> {
    if raw.trim().is_empty() {
        return Ok(Manifest::default());
    }
    let mut manifest: Manifest =
        serde_yaml::from_str(raw).map_err(|error| FirmLibraryError::Manifest(error.to_string()))?;
    manifest.retainer = manifest
        .retainer
        .map(|code| code.trim().to_string())
        .filter(|code| !code.is_empty());
    Ok(manifest)
}

/// One read of the library at `main`'s HEAD.
struct Snapshot {
    commit_sha: String,
    tree: Vec<(String, Vec<u8>)>,
}

/// Read the library at HEAD, or `None` when the Firm has no library
/// repository or it has no commits. Reading never creates the repository.
async fn snapshot(
    repo: &repos::RepoStore,
    firm_slug: &str,
) -> Result<Option<Snapshot>, FirmLibraryError> {
    let name = repository_name(firm_slug);
    let repo = repo.clone();
    tokio::task::spawn_blocking(move || {
        if !repo.exists_code(&name) {
            return Ok(None);
        }
        let Some(commit_sha) = repo.head_oid_code(&name)? else {
            return Ok(None);
        };
        let tree = repo.read_head_tree_code(&name)?;
        Ok(Some(Snapshot { commit_sha, tree }))
    })
    .await
    .map_err(|error| FirmLibraryError::Store(error.to_string()))?
}

fn manifest_of(tree: &[(String, Vec<u8>)]) -> Result<Manifest, FirmLibraryError> {
    let Some((_, bytes)) = tree.iter().find(|(path, _)| path == MANIFEST_FILE) else {
        return Ok(Manifest::default());
    };
    let raw = std::str::from_utf8(bytes)
        .map_err(|_| FirmLibraryError::Manifest("not UTF-8".to_string()))?;
    parse_manifest(raw)
}

/// Every Draft code directly under the blueprint directory.
fn draft_codes(tree: &[(String, Vec<u8>)]) -> Vec<String> {
    let mut codes: Vec<String> = tree
        .iter()
        .filter_map(|(path, _)| {
            let relative = path
                .strip_prefix("drafts/")
                .or_else(|| path.strip_prefix("templates/"))?;
            if relative.contains('/') {
                return None;
            }
            relative.strip_suffix(".md").map(ToOwned::to_owned)
        })
        .collect();
    codes.sort();
    codes.dedup();
    codes
}

async fn firm_slug(surreal: &SurrealDb, firm_id: Uuid) -> Result<String, FirmLibraryError> {
    Ok(crate::firms::find_by_id(surreal, firm_id)
        .await
        .map_err(|error| FirmLibraryError::Store(error.to_string()))?
        .ok_or(FirmLibraryError::FirmNotFound(firm_id))?
        .slug)
}

/// What one [`sync`] persisted.
#[derive(Debug)]
pub struct Synced {
    /// The library commit read.
    pub commit_sha: String,
    /// The now-current Firm version of every library Draft, by code.
    pub templates: Vec<Template>,
    /// The Template the manifest's `retainer` names, when it names one.
    pub retainer: Option<Template>,
}

/// Read the Firm's library at HEAD and persist every Draft as the Firm's
/// current version, then resolve the manifest's retainer. `Ok(None)` when
/// the Firm has no library.
///
/// Idempotent: byte-identical Drafts re-read at a later commit write
/// nothing. Fails closed: one invalid Draft, an invalid manifest, or a
/// retainer that names nothing (or a kind that does not open a matter)
/// refuses the whole sync, and callers fall back to the shared defaults.
///
/// # Errors
/// [`FirmLibraryError`] for any of the refusals above, or a store fault.
pub async fn sync(
    surreal: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    repo: &repos::RepoStore,
    firm_id: Uuid,
) -> Result<Option<Synced>, FirmLibraryError> {
    let slug = firm_slug(surreal, firm_id).await?;
    let Some(snapshot) = snapshot(repo, &slug).await? else {
        return Ok(None);
    };
    let manifest = manifest_of(&snapshot.tree)?;
    let mut templates = Vec::new();
    for code in draft_codes(&snapshot.tree) {
        let contents = crate::template_source::selected_blueprint(snapshot.tree.clone(), &code)?;
        let persisted = crate::template_source::persist_contents_in(
            surreal,
            storage,
            Scope::Firm(firm_id),
            &code,
            contents,
            snapshot.commit_sha.clone(),
        )
        .await?;
        templates.push(persisted.template);
    }
    let retainer = match manifest.retainer {
        Some(code) => Some(retainer_template(surreal, firm_id, &code).await?),
        None => None,
    };
    Ok(Some(Synced {
        commit_sha: snapshot.commit_sha,
        templates,
        retainer,
    }))
}

async fn retainer_template(
    surreal: &SurrealDb,
    firm_id: Uuid,
    code: &str,
) -> Result<Template, FirmLibraryError> {
    let template = crate::templates::resolve_for_firm(surreal, firm_id, code)
        .await?
        .ok_or_else(|| FirmLibraryError::RetainerNotFound(code.to_string()))?;
    let opens = template
        .kind
        .as_deref()
        .and_then(rules::kind::Kind::parse)
        .is_some_and(rules::kind::Kind::opens_a_matter);
    if !opens {
        return Err(FirmLibraryError::RetainerDoesNotOpenAMatter {
            code: code.to_string(),
            kind: template.kind.unwrap_or_else(|| "none".to_string()),
        });
    }
    Ok(template)
}

/// Persist one library Draft as the Firm's current version of `code` — the
/// notation-create step after a Project repository miss. `Ok(None)` when
/// the Firm has no library or the library has no such Draft.
///
/// # Errors
/// [`FirmLibraryError`] for an invalid Draft (it is in the library, so it
/// refuses rather than falling through) or a store fault.
pub async fn persist_draft(
    surreal: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    repo: &repos::RepoStore,
    firm_id: Uuid,
    code: &str,
) -> Result<Option<Persisted>, FirmLibraryError> {
    let slug = firm_slug(surreal, firm_id).await?;
    let Some(snapshot) = snapshot(repo, &slug).await? else {
        return Ok(None);
    };
    let contents = match crate::template_source::selected_blueprint(snapshot.tree, code) {
        Ok(contents) => contents,
        Err(TemplateSourceError::TemplateNotFound { .. }) => return Ok(None),
        Err(other) => return Err(other.into()),
    };
    Ok(Some(
        crate::template_source::persist_contents_in(
            surreal,
            storage,
            Scope::Firm(firm_id),
            code,
            contents,
            snapshot.commit_sha,
        )
        .await?,
    ))
}

/// [`sync`] against the process's configured repository store, for the
/// matter-opening doors. Never an error: a process with no repository
/// store yields `None`, as [`sync_or_default`] does for the rest.
pub async fn sync_configured(
    surreal: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    firm_id: Uuid,
) -> Option<Synced> {
    let repo = repos::RepoStore::from_env().ok()?;
    sync_or_default(surreal, storage, &repo, firm_id).await
}

/// [`sync`], never an error: a Firm with no library or a library that fails
/// to sync (logged) yields `None`, and the caller keeps its shared default.
pub async fn sync_or_default(
    surreal: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    repo: &repos::RepoStore,
    firm_id: Uuid,
) -> Option<Synced> {
    match sync(surreal, storage, repo, firm_id).await {
        Ok(synced) => synced,
        Err(error) => {
            tracing::warn!(%firm_id, error = %error, "firm library: sync refused; using shared defaults");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid, firm-owned Draft from the shared corpus; its `kind:
    /// onboarding` opens a matter.
    const VALID_DRAFT: &str =
        include_str!("../../templates/notations/neon_law/onboarding__letter.md");

    async fn fs_storage() -> Arc<dyn StorageService> {
        Arc::new(
            cloud::FsStorage::new(
                std::env::temp_dir().join(format!("navigator-firm-library-{}", Uuid::now_v7())),
            )
            .await
            .unwrap(),
        )
    }

    async fn firm_wearing(surreal: &SurrealDb, brand: &str) -> crate::firms::Firm {
        let id = crate::test_support::seed_firm_with_brand(surreal, brand).await;
        crate::firms::find_by_id(surreal, id)
            .await
            .unwrap()
            .unwrap()
    }

    async fn project_of(surreal: &SurrealDb, firm_id: Uuid) -> Uuid {
        crate::projects::create(
            surreal,
            &crate::projects::NewProject {
                code: format!("firm-library-{}", Uuid::now_v7()),
                name: "matter".into(),
                status: "open".into(),
                entity_id: crate::test_support::seed_entity(surreal).await,
                firm_id: Some(firm_id),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id
    }

    fn library(
        dir: &tempfile::TempDir,
        firm_slug: &str,
        files: &[(&str, &[u8])],
    ) -> repos::RepoStore {
        let store = repos::RepoStore::new(dir.path());
        store
            .commit_as_code(
                &repository_name(firm_slug),
                repos::Author {
                    name: "Tester",
                    email: "tester@example.com",
                },
                "library",
                files,
            )
            .unwrap();
        store
    }

    #[test]
    fn the_manifest_is_closed_and_trims_its_retainer() {
        assert_eq!(
            parse_manifest("retainer: \" firm_retainer \"\n").unwrap(),
            Manifest {
                retainer: Some("firm_retainer".into())
            }
        );
        assert_eq!(parse_manifest("").unwrap(), Manifest::default());
        assert!(matches!(
            parse_manifest("retainr: firm_retainer\n"),
            Err(FirmLibraryError::Manifest(_))
        ));
    }

    #[tokio::test]
    async fn a_firm_without_a_library_syncs_nothing_and_creates_no_repository() {
        let surreal = crate::surreal::test_support::mem().await;
        let storage = fs_storage().await;
        let firm = firm_wearing(&surreal, "neon").await;
        let dir = tempfile::tempdir().unwrap();
        let repo = repos::RepoStore::new(dir.path());
        assert!(sync(&surreal, &storage, &repo, firm.id)
            .await
            .unwrap()
            .is_none());
        assert!(!repo.exists_code(&repository_name(&firm.slug)));
    }

    #[tokio::test]
    async fn sync_persists_firm_versions_and_resolves_the_retainer_for_that_firm_only() {
        let surreal = crate::surreal::test_support::mem().await;
        let storage = fs_storage().await;
        let owner = firm_wearing(&surreal, "neon").await;
        let other = firm_wearing(&surreal, "vesta").await;
        let dir = tempfile::tempdir().unwrap();
        let repo = library(
            &dir,
            &owner.slug,
            &[
                ("drafts/firm_retainer.md", VALID_DRAFT.as_bytes()),
                (MANIFEST_FILE, b"retainer: firm_retainer\n"),
            ],
        );
        let head = repo
            .head_oid_code(&repository_name(&owner.slug))
            .unwrap()
            .unwrap();

        let synced = sync(&surreal, &storage, &repo, owner.id)
            .await
            .unwrap()
            .expect("the firm has a library");
        assert_eq!(synced.commit_sha, head);
        let retainer = synced.retainer.expect("the manifest names a retainer");
        assert_eq!(retainer.code, "firm_retainer");
        assert_eq!(retainer.firm_id, Some(owner.id));
        assert_eq!(retainer.project_id, None);
        assert_eq!(retainer.source_commit_sha.as_deref(), Some(head.as_str()));
        assert!(retainer.body_sha256.is_some());

        // The owner's Projects resolve it; another Firm's Projects, the
        // other Firm itself, and the shared catalog never see it.
        let owners_matter = project_of(&surreal, owner.id).await;
        let foreign = project_of(&surreal, other.id).await;
        let resolved = crate::templates::resolve(&surreal, Some(owners_matter), "firm_retainer")
            .await
            .unwrap();
        assert_eq!(resolved.map(|t| t.id), Some(retainer.id));
        for invisible in [
            crate::templates::resolve(&surreal, Some(foreign), "firm_retainer")
                .await
                .unwrap(),
            crate::templates::resolve_for_firm(&surreal, other.id, "firm_retainer")
                .await
                .unwrap(),
            crate::templates::resolve(&surreal, None, "firm_retainer")
                .await
                .unwrap(),
        ] {
            assert!(invisible.is_none(), "a Firm's Draft leaked: {invisible:?}");
        }
        assert!(!crate::templates::list_current(&surreal)
            .await
            .unwrap()
            .iter()
            .any(|t| t.firm_id.is_some()));
        assert_eq!(
            crate::templates::list_for_firm(&surreal, owner.id)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(crate::templates::list_for_firm(&surreal, other.id)
            .await
            .unwrap()
            .is_empty());

        // A re-sync of the same bytes writes nothing new.
        let again = sync(&surreal, &storage, &repo, owner.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(again.retainer.map(|t| t.id), Some(retainer.id));
    }

    #[tokio::test]
    async fn a_retainer_naming_nothing_refuses_the_sync() {
        let surreal = crate::surreal::test_support::mem().await;
        let storage = fs_storage().await;
        let owner = firm_wearing(&surreal, "neon").await;
        let dir = tempfile::tempdir().unwrap();
        let repo = library(
            &dir,
            &owner.slug,
            &[(MANIFEST_FILE, b"retainer: missing\n")],
        );
        assert!(matches!(
            sync(&surreal, &storage, &repo, owner.id).await,
            Err(FirmLibraryError::RetainerNotFound(code)) if code == "missing"
        ));
    }

    #[tokio::test]
    async fn persist_draft_reads_one_code_and_misses_quietly() {
        let surreal = crate::surreal::test_support::mem().await;
        let storage = fs_storage().await;
        let owner = firm_wearing(&surreal, "neon").await;
        let dir = tempfile::tempdir().unwrap();
        let repo = library(
            &dir,
            &owner.slug,
            &[("drafts/firm_retainer.md", VALID_DRAFT.as_bytes())],
        );
        let persisted = persist_draft(&surreal, &storage, &repo, owner.id, "firm_retainer")
            .await
            .unwrap()
            .expect("the library has the Draft");
        assert_eq!(persisted.template.firm_id, Some(owner.id));
        assert!(persist_draft(&surreal, &storage, &repo, owner.id, "absent")
            .await
            .unwrap()
            .is_none());
    }
}
