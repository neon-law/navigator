//! Project-scoped [Matter Pages](../../docs/glossary/matter-page.md)
//! (`matter_page`): the `pages:` entries of a Project repository's
//! `navigator.yaml`, as `navigator project sync` last stored them.
//!
//! The repository is the source of truth; these rows are its synced copy, so
//! rendering a page never reads git at request time. A sync replaces the
//! Project's whole set at once — a page removed from the manifest is a row
//! removed here — and every write is validated by the same rules the gate
//! runs (`Y021` and the D-family), so a row that reaches the renderer was a
//! page the gate accepted.
//!
//! PostgreSQL owns the synced manifest after verified scoped enrollment.
//! Every replacement derives its Firm from the locked Project.

use rules::pages::PageEntry;
use uuid::Uuid;

use crate::application_store::ApplicationStore;

const TABLE: &str = "matter_page";

/// One stored page, in declared order.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MatterPage {
    pub id: Uuid,
    pub project_id: Uuid,
    /// The manifest entry exactly as synced.
    pub entry: PageEntry,
    /// The entry's position in `pages:`, from zero.
    pub position: i64,
    /// When `project sync` stored this set, RFC 3339.
    pub synced_at: String,
}

#[derive(Debug, thiserror::Error)]
pub enum MatterPageError {
    #[error("PostgreSQL Matter Page operation failed")]
    Storage(#[from] sqlx::Error),
    #[error(transparent)]
    Authority(#[from] crate::postgres::matter::MatterAuthorityError),
    /// The set fails the gate's own checks; each line is one finding.
    #[error("{}", .0.join("; "))]
    Invalid(Vec<String>),
}

async fn authority() -> Result<crate::postgres::PostgresDb, MatterPageError> {
    Ok(crate::postgres::matter::authority_for(TABLE).await?)
}

/// Every gate finding against `pages`, as one line each: the `Y021` shape
/// checks and the D-family composition checks, run on the same `pages:`
/// block a manifest would carry.
#[must_use]
pub fn findings(pages: &[PageEntry]) -> Vec<String> {
    let mut document = serde_yaml::Mapping::new();
    document.insert(
        serde_yaml::Value::from(rules::pages::KEY),
        serde_yaml::to_value(pages).unwrap_or_default(),
    );
    let contents = serde_yaml::to_string(&document).unwrap_or_default();
    let file = rules::SourceFile {
        path: std::path::PathBuf::from(rules::project_manifest::FILE),
        contents,
    };
    let mut out: Vec<String> = rules::pages::shape_problems(&file.contents)
        .into_iter()
        .map(|(_, message)| format!("Y021: {message}"))
        .collect();
    out.extend(
        rules::lint_pages(&file)
            .into_iter()
            .map(|violation| format!("{}: {}", violation.code, violation.message)),
    );
    out
}

/// Replace a Project's stored pages with `pages`, in order.
///
/// # Errors
///
/// [`MatterPageError::Invalid`] when the set fails the gate's checks;
/// [`MatterPageError::Storage`] when the write fails.
pub async fn replace_for_project(
    application_store: &ApplicationStore,
    project_id: Uuid,
    pages: &[PageEntry],
) -> Result<Vec<MatterPage>, MatterPageError> {
    let pg = authority().await?;
    let _ = application_store;
    let problems = findings(pages);
    if !problems.is_empty() {
        return Err(MatterPageError::Invalid(problems));
    }
    crate::postgres::matter_pages::replace(&pg, project_id, pages).await
}

/// A Project's stored pages, in declared order.
///
/// # Errors
///
/// [`MatterPageError::Storage`] when the read fails.
pub async fn list_for_project(
    application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<MatterPage>, MatterPageError> {
    let pg = authority().await?;
    let _ = application_store;
    crate::postgres::matter_pages::list(&pg, project_id).await
}

/// The Project Inbox reads the same authoritative manifest entries.
pub async fn inbox_entries(
    application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<PageEntry>, MatterPageError> {
    Ok(list_for_project(application_store, project_id)
        .await?
        .into_iter()
        .map(|page| page.entry)
        .collect())
}

/// Remove every stored page of a Project, for the matter-delete cascade.
///
/// # Errors
///
/// [`MatterPageError::Storage`] when the delete fails.
pub async fn delete_for_project(
    application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<(), MatterPageError> {
    let pg = authority().await?;
    let _ = application_store;
    crate::postgres::matter_pages::delete(&pg, project_id).await
}

#[cfg(test)]
mod tests {
    use super::findings;

    use rules::pages::PageEntry;
    use std::collections::BTreeMap;

    fn page(slug: &str, client: Option<&[&str]>) -> PageEntry {
        let mut lenses = BTreeMap::new();
        lenses.insert(
            "lawyer".to_string(),
            ["deadline_board", "boundary_note", "provenance_statement"]
                .map(String::from)
                .to_vec(),
        );
        if let Some(client) = client {
            lenses.insert(
                "client".to_string(),
                client.iter().map(|s| (*s).to_string()).collect(),
            );
        }
        PageEntry {
            slug: slug.to_string(),
            kind: "docket_deadline_board".to_string(),
            title: format!("Page {slug}"),
            draft: None,
            document_revision: None,
            media: Vec::new(),
            lenses,
        }
    }

    #[test]
    fn findings_run_the_gate_on_a_typed_set() {
        assert!(findings(&[page("timeline", None)]).is_empty());
    }
}
