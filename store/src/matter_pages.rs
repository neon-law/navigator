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
//! SurrealDB is authoritative. Each successful write mirrors the Project's
//! whole set to PostgreSQL when the opt-in mirror is configured
//! ([`crate::postgres_mirror::matter_pages_captions`]).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use rules::dashboard::Lens;
use rules::pages::PageEntry;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::surreal::{record_id, record_uuid, SurrealDb};

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
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    /// The set fails the gate's own checks; each line is one finding.
    #[error("{}", .0.join("; "))]
    Invalid(Vec<String>),
}

#[derive(SurrealValue)]
struct Row {
    id: surrealdb::types::RecordId,
    project_id: surrealdb::types::RecordId,
    slug: String,
    kind: String,
    title: String,
    draft: Option<String>,
    position: i64,
    client_sections: Option<Vec<String>>,
    lawyer_sections: Option<Vec<String>>,
    clerk_sections: Option<Vec<String>>,
    synced_at: String,
}

impl Row {
    fn into_page(self) -> Option<MatterPage> {
        let mut lenses = BTreeMap::new();
        for (lens, sections) in [
            (Lens::Client, self.client_sections),
            (Lens::Lawyer, self.lawyer_sections),
            (Lens::Clerk, self.clerk_sections),
        ] {
            if let Some(sections) = sections {
                lenses.insert(lens.as_str().to_string(), sections);
            }
        }
        Some(MatterPage {
            id: record_uuid(&self.id)?,
            project_id: record_uuid(&self.project_id)?,
            entry: PageEntry {
                slug: self.slug,
                kind: self.kind,
                title: self.title,
                draft: self.draft,
                lenses,
            },
            position: self.position,
            synced_at: self.synced_at,
        })
    }
}

const SELECT: &str = "id, project_id, slug, kind, title, draft, position, client_sections, \
     lawyer_sections, clerk_sections, synced_at";

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
/// [`MatterPageError::Db`] when the write fails.
pub async fn replace_for_project(
    surreal: &SurrealDb,
    project_id: Uuid,
    pages: &[PageEntry],
) -> Result<Vec<MatterPage>, MatterPageError> {
    let problems = findings(pages);
    if !problems.is_empty() {
        return Err(MatterPageError::Invalid(problems));
    }
    let project = record_id(crate::projects::PROJECT_TABLE, project_id);
    let now = chrono::Utc::now().to_rfc3339();
    let mut query =
        String::from("BEGIN TRANSACTION; DELETE matter_page WHERE project_id = $project;");
    for index in 0..pages.len() {
        let _ = write!(
            query,
            "CREATE $id_{index} SET project_id = $project, slug = $slug_{index}, \
             kind = $kind_{index}, title = $title_{index}, draft = $draft_{index}, \
             position = {index}, client_sections = $client_{index}, \
             lawyer_sections = $lawyer_{index}, clerk_sections = $clerk_{index}, \
             synced_at = $now;"
        );
    }
    query.push_str("COMMIT TRANSACTION;");
    let mut request = surreal
        .query(query)
        .bind(("project", project))
        .bind(("now", now));
    for (index, page) in pages.iter().enumerate() {
        let lens = |lens: Lens| page.lenses.get(lens.as_str()).cloned();
        request = request
            .bind((format!("id_{index}"), record_id(TABLE, Uuid::now_v7())))
            .bind((format!("slug_{index}"), page.slug.clone()))
            .bind((format!("kind_{index}"), page.kind.clone()))
            .bind((format!("title_{index}"), page.title.clone()))
            .bind((format!("draft_{index}"), page.draft.clone()))
            .bind((format!("client_{index}"), lens(Lens::Client)))
            .bind((format!("lawyer_{index}"), lens(Lens::Lawyer)))
            .bind((format!("clerk_{index}"), lens(Lens::Clerk)));
    }
    request.await.and_then(surrealdb::IndexedResults::check)?;
    mirror(surreal, project_id).await;
    list_for_project(surreal, project_id).await
}

/// A Project's stored pages, in declared order.
///
/// # Errors
///
/// [`MatterPageError::Db`] when the read fails.
pub async fn list_for_project(
    surreal: &SurrealDb,
    project_id: Uuid,
) -> Result<Vec<MatterPage>, MatterPageError> {
    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE project_id = $project ORDER BY position ASC"
        ))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<Row> = response.take(0)?;
    Ok(rows.into_iter().filter_map(Row::into_page).collect())
}

/// Remove every stored page of a Project, for the matter-delete cascade.
///
/// # Errors
///
/// [`MatterPageError::Db`] when the delete fails.
pub async fn delete_for_project(
    surreal: &SurrealDb,
    project_id: Uuid,
) -> Result<(), MatterPageError> {
    surreal
        .query(format!("DELETE {TABLE} WHERE project_id = $project"))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    mirror(surreal, project_id).await;
    Ok(())
}

/// Mirror the Project's whole page set, or its removal, to PostgreSQL when
/// opted in.
async fn mirror(surreal: &SurrealDb, project_id: Uuid) {
    crate::postgres_mirror::after_write(
        surreal,
        crate::postgres_mirror::MirrorKey::MatterPages(project_id),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::{delete_for_project, findings, list_for_project, replace_for_project};
    use rules::dashboard::{Lens, Section};
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
            lenses,
        }
    }

    #[tokio::test]
    async fn a_sync_replaces_the_whole_set_in_declared_order() {
        let surreal = crate::surreal::test_support::mem().await;
        let project_id = crate::test_support::seed_project_surreal(&surreal, "pages").await;
        let client: &[&str] = &["key_dates_strip", "boundary_note", "provenance_statement"];
        let stored = replace_for_project(
            &surreal,
            project_id,
            &[page("timeline", Some(client)), page("deadlines", None)],
        )
        .await
        .unwrap();
        assert_eq!(
            stored
                .iter()
                .map(|p| p.entry.slug.as_str())
                .collect::<Vec<_>>(),
            ["timeline", "deadlines"]
        );
        assert_eq!(
            stored[0].entry.sections_for(Lens::Client).unwrap(),
            [
                Section::KeyDatesStrip,
                Section::BoundaryNote,
                Section::ProvenanceStatement
            ]
        );
        assert!(
            stored[1].entry.sections_for(Lens::Client).is_none(),
            "an undeclared lens stays undeclared"
        );

        replace_for_project(&surreal, project_id, &[page("deadlines", None)])
            .await
            .unwrap();
        let after = list_for_project(&surreal, project_id).await.unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].entry.slug, "deadlines");
        assert_eq!(after[0].position, 0);

        delete_for_project(&surreal, project_id).await.unwrap();
        assert!(list_for_project(&surreal, project_id)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn a_set_the_gate_refuses_is_never_stored() {
        let surreal = crate::surreal::test_support::mem().await;
        let project_id = crate::test_support::seed_project_surreal(&surreal, "pages").await;
        let lawyer_only: &[&str] = &["deadline_board", "provenance_statement"];
        let error = replace_for_project(
            &surreal,
            project_id,
            &[page("timeline", Some(lawyer_only)), page("timeline", None)],
        )
        .await
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("Y021"), "{message}");
        assert!(message.contains("D003"), "{message}");
        assert!(list_for_project(&surreal, project_id)
            .await
            .unwrap()
            .is_empty());
    }

    #[test]
    fn findings_run_the_gate_on_a_typed_set() {
        assert!(findings(&[page("timeline", None)]).is_empty());
    }
}
