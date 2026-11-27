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
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::application_store::ApplicationStore;
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

async fn firm_slug(
    application_store: &ApplicationStore,
    firm_id: Uuid,
) -> Result<String, FirmLibraryError> {
    Ok(crate::firms::find_by_id(application_store, firm_id)
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
    application_store: &ApplicationStore,
    storage: &Arc<dyn StorageService>,
    repo: &repos::RepoStore,
    firm_id: Uuid,
) -> Result<Option<Synced>, FirmLibraryError> {
    let slug = firm_slug(application_store, firm_id).await?;
    let Some(snapshot) = snapshot(repo, &slug).await? else {
        return Ok(None);
    };
    let manifest = manifest_of(&snapshot.tree)?;
    let mut templates = Vec::new();
    for code in draft_codes(&snapshot.tree) {
        let contents = crate::template_source::selected_blueprint(snapshot.tree.clone(), &code)?;
        let persisted = crate::template_source::persist_contents_in(
            application_store,
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
        Some(code) => Some(retainer_template(application_store, firm_id, &code).await?),
        None => None,
    };
    Ok(Some(Synced {
        commit_sha: snapshot.commit_sha,
        templates,
        retainer,
    }))
}

async fn retainer_template(
    application_store: &ApplicationStore,
    firm_id: Uuid,
    code: &str,
) -> Result<Template, FirmLibraryError> {
    let template = crate::templates::resolve_for_firm(application_store, firm_id, code)
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
    application_store: &ApplicationStore,
    storage: &Arc<dyn StorageService>,
    repo: &repos::RepoStore,
    firm_id: Uuid,
    code: &str,
) -> Result<Option<Persisted>, FirmLibraryError> {
    let slug = firm_slug(application_store, firm_id).await?;
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
            application_store,
            storage,
            Scope::Firm(firm_id),
            code,
            contents,
            snapshot.commit_sha,
        )
        .await?,
    ))
}

/// Where one Firm's library lives, as a Person who belongs to that Firm
/// reads it: the Firm's slug and the `https` clone URL of
/// `<organization>/library` on this deployment's Forgejo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Location {
    pub slug: String,
    /// `None` when the Firm has no recorded Forgejo organization or the
    /// deployment has no Forgejo.
    pub repository_url: Option<String>,
}

/// Every Firm `person_id` belongs to — a Firm role (`person_firm_role`),
/// participation in a Project the Firm owns (`project.firm_id`), or the
/// deployment-wide Owner tier (`person.role = 'owner'`, which is never a
/// per-Firm row; see `store::firms`) — with its library's [`Location`],
/// sorted by slug. No other Firm is ever listed.
///
/// `forgejo_root` is the deployment's Forgejo root URL
/// (`NAVIGATOR_FORGEJO_ROOT_URL`); the organization is the Firm's own
/// `firm_forgejo_organization` row.
///
/// # Errors
/// A PostgreSQL failure.
pub async fn locations_for_person(
    pg: &crate::postgres::PostgresDb,
    person_id: Uuid,
    forgejo_root: Option<&str>,
) -> Result<Vec<Location>, sqlx::Error> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT f.slug, o.organization FROM firm f \
         LEFT JOIN firm_forgejo_organization o ON o.firm_id = f.id \
         WHERE f.slug IS NOT NULL AND ( \
           EXISTS (SELECT 1 FROM person_firm_role r WHERE r.firm_id = f.id AND r.person_id = $1) \
           OR EXISTS (SELECT 1 FROM person_project_role pr JOIN project p ON p.id = pr.project_id \
                      WHERE p.firm_id = f.id AND pr.person_id = $1) \
           OR EXISTS (SELECT 1 FROM person p WHERE p.id = $1 AND p.role = 'owner')) \
         ORDER BY f.slug",
    )
    .bind(person_id)
    .fetch_all(pg.pool())
    .await?;
    let root = forgejo_root
        .map(|root| root.trim().trim_end_matches('/'))
        .filter(|root| !root.is_empty());
    Ok(rows
        .into_iter()
        .map(|(slug, organization)| Location {
            slug,
            repository_url: root
                .zip(organization)
                .map(|(root, organization)| format!("{root}/{organization}/{REPOSITORY}.git")),
        })
        .collect())
}

/// [`sync`] against the process's configured repository store, for the
/// matter-opening doors. Never an error: a process with no repository
/// store yields `None`, as [`sync_or_default`] does for the rest.
pub async fn sync_configured(
    application_store: &ApplicationStore,
    storage: &Arc<dyn StorageService>,
    firm_id: Uuid,
) -> Option<Synced> {
    let repo = repos::RepoStore::from_env().ok()?;
    sync_or_default(application_store, storage, &repo, firm_id).await
}

/// [`sync`], never an error: a Firm with no library or a library that fails
/// to sync (logged) yields `None`, and the caller keeps its shared default.
pub async fn sync_or_default(
    application_store: &ApplicationStore,
    storage: &Arc<dyn StorageService>,
    repo: &repos::RepoStore,
    firm_id: Uuid,
) -> Option<Synced> {
    match sync(application_store, storage, repo, firm_id).await {
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

    // Needs a live PostgreSQL; self-skips without one.
    #[tokio::test]
    async fn a_person_lists_only_the_firms_they_belong_to_with_each_librarys_clone_url() {
        let Some((options, database)) = crate::postgres::fixtures::fresh_schema().await else {
            return;
        };
        let pg = crate::postgres::fixtures::connect(&options, &database).await;
        let [member, participant, outsider, owner, firm_a, firm_b, firm_c, firm_d, entity] =
            std::array::from_fn(|_| Uuid::now_v7());
        // Firm A and Firm B each have an organization; `member` holds a Firm
        // role in A only. `participant` holds no Firm role, only a Project of
        // Firm C, which has no organization. `owner` holds no membership row
        // at all: Owner is a deployment-wide tier on `person.role` (see
        // `store::firms`), never a per-Firm row.
        let seed = format!(
            "INSERT INTO person (id,name,email) VALUES \
               ('{member}','Synthetic Person','{member}@example.com'), \
               ('{participant}','Synthetic Person','{participant}@example.com'), \
               ('{outsider}','Synthetic Person','{outsider}@example.com'), \
               ('{owner}','Synthetic Person','{owner}@example.com'); \
             UPDATE person SET role = 'owner' WHERE id = '{owner}'; \
             INSERT INTO firm (id,name,slug,status) VALUES ('{firm_a}','example-a','example-a','active'), \
               ('{firm_b}','example-b','example-b','active'), ('{firm_c}','example-c','example-c','active'), \
               ('{firm_d}','example-d','example-d','active'); \
             INSERT INTO firm_forgejo_organization (firm_id,organization,forgejo_id,owner_login) VALUES \
               ('{firm_a}','example-a',1,'synthetic-owner'), ('{firm_b}','example-b',2,'synthetic-owner'); \
             INSERT INTO person_firm_role (person_id,firm_id,membership) VALUES ('{member}','{firm_a}','lawyer'); \
             INSERT INTO entity_type (id,name) VALUES ('{entity}','Synthetic Entity Type'); \
             INSERT INTO jurisdiction (id,name) VALUES ('{entity}','Synthetic Jurisdiction'); \
             INSERT INTO entity (id,name,entity_type_id,jurisdiction_id) \
               VALUES ('{entity}','Synthetic Entity','{entity}','{entity}'); \
             INSERT INTO project (id,code,name,status,entity_id,firm_id) \
               VALUES ('{entity}','example-matter','Synthetic Matter','active','{entity}','{firm_c}'); \
             INSERT INTO person_project_role (person_id,project_id,participation) \
               VALUES ('{participant}','{entity}','client');"
        );
        sqlx::raw_sql(sqlx::AssertSqlSafe(seed))
            .execute(pg.pool())
            .await
            .unwrap();

        let root = Some("https://forge.example.test/");
        assert_eq!(
            locations_for_person(&pg, member, root).await.unwrap(),
            vec![Location {
                slug: "example-a".into(),
                repository_url: Some("https://forge.example.test/example-a/library.git".into()),
            }],
            "a Firm A member never sees Firm B"
        );
        assert_eq!(
            locations_for_person(&pg, participant, root).await.unwrap(),
            vec![Location {
                slug: "example-c".into(),
                repository_url: None,
            }],
            "a Project participant sees the owning Firm; it has no organization"
        );
        assert!(locations_for_person(&pg, outsider, root)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            locations_for_person(&pg, owner, root).await.unwrap(),
            vec![
                Location {
                    slug: "example-a".into(),
                    repository_url: Some("https://forge.example.test/example-a/library.git".into()),
                },
                Location {
                    slug: "example-b".into(),
                    repository_url: Some("https://forge.example.test/example-b/library.git".into()),
                },
                Location {
                    slug: "example-c".into(),
                    repository_url: None,
                },
                Location {
                    slug: "example-d".into(),
                    repository_url: None,
                },
            ],
            "the deployment-wide Owner sees every Firm, with no membership row"
        );
        assert_eq!(
            locations_for_person(&pg, member, None).await.unwrap()[0].repository_url,
            None,
            "no Forgejo configured"
        );
    }
}
