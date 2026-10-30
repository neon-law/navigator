//! The lawyer workbench's documents panel: the grouped document table with its
//! Visibility column, per-revision history, and service lines, plus the rows
//! and server-side helpers it renders from
//! ([`crate::lawyer_project_detail`] composes the page around it).

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::lawyer_project_detail::LawyerDetailView;

/// One revision in a grouped lawyer document history.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct LawyerDocRevision {
    pub id: String,
    pub filename: String,
    /// This revision's own stored visibility, as the plain word rendered on
    /// the page (`"internal"` or `"shared"`) — never an icon or a colour
    /// alone. A chain's revisions may disagree (an internal draft beside a
    /// shared executed copy under one slug); that is normal, so each
    /// revision carries its own value rather than inheriting the row's.
    pub visibility: String,
    /// When this revision was filed, as the page prints it
    /// (`5 Nov 26 14:00 UTC`).
    #[serde(default)]
    pub created_at: String,
}

/// One document row (the operative filename + id and its revision history).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct LawyerDocRow {
    pub id: String,
    pub filename: String,
    /// The operative revision's visibility, as the plain word rendered in
    /// the table's Visibility column. See [`LawyerDocRevision::visibility`].
    pub visibility: String,
    /// The document's path below a Project checkout's `documents/`
    /// (`Docket Filings/pleadings/motion.pdf`), by the layout `navigator
    /// project sync` writes.
    #[serde(default)]
    pub path: String,
    /// The top folder of [`Self::path`]: a matter folder such as `Work
    /// Product`, or a sibling folder such as `onboarding`.
    #[serde(default)]
    pub folder: String,
    /// Newest-first revisions for a slugged document. Empty for one-off assets.
    pub revisions: Vec<LawyerDocRevision>,
    /// The Service recorded on any revision of this document, one
    /// display-ready line each, earliest served first.
    #[serde(default)]
    pub service: Vec<String>,
}

#[cfg(feature = "server")]
pub(crate) async fn service_lines(
    surreal: &store::surreal::SurrealDb,
    project_id: uuid::Uuid,
) -> Result<std::collections::HashMap<uuid::Uuid, Vec<String>>, String> {
    let rows = store::project_service::list(surreal, project_id)
        .await
        .map_err(|error| error.to_string())?;
    let mut entries: std::collections::HashMap<uuid::Uuid, String> =
        std::collections::HashMap::new();
    if !rows.is_empty() {
        for entry in store::cases::docket_for_project(surreal, project_id)
            .await
            .map_err(|error| error.to_string())?
        {
            if let Some(document) = entry.document_asset_id {
                entries
                    .entry(document)
                    .or_insert_with(|| format!("D.I. {}", entry.entry_number));
            }
        }
    }
    let mut lines: std::collections::HashMap<uuid::Uuid, Vec<String>> =
        std::collections::HashMap::new();
    for row in rows {
        let method = store::project_service::ServiceMethod::parse(&row.method)
            .map_or(row.method.clone(), |method| {
                method.certificate_phrase().to_string()
            });
        let counsel = row
            .counsel_for
            .as_deref()
            .map(|party| format!(", counsel for {party}"))
            .unwrap_or_default();
        let reference = entries
            .get(&row.document_id)
            .map(|entry| format!(" ({entry})"))
            .unwrap_or_default();
        lines.entry(row.document_id).or_default().push(format!(
            "Served {method} on {} on {}{counsel}{reference}",
            row.served_on, row.recipient
        ));
    }
    Ok(lines)
}

/// `"shared"` for `store::documents::visibility::CLIENT`; `"internal"` for
/// anything else, which is both the stored default and the safe reading of an
/// unrecognized value.
#[cfg(feature = "server")]
pub(crate) fn visibility_word(stored: &str) -> String {
    if stored == store::documents::visibility::CLIENT {
        "shared".to_string()
    } else {
        "internal".to_string()
    }
}

/// The document-upload Kind select's options, as `(value, label)` — every
/// `Kind` valid in the asset lane, in [`rules::kind::Kind::ALL`] order. Computed
/// here rather than duplicated as literal strings, so a new asset-lane kind
/// reaches the upload form for free.
#[cfg(feature = "server")]
pub(crate) fn asset_kind_choices() -> Vec<(String, String)> {
    rules::kind::Kind::ALL
        .iter()
        .filter(|k| k.valid_for(rules::kind::Lane::Asset))
        .map(|k| {
            (
                k.as_str().to_string(),
                format!("{} ({})", k.describe(), k.as_str()),
            )
        })
        .collect()
}

/// The documents table: any `?error=` flash, then either the empty notice or
/// the grouped table with its Visibility column — the operative revision's
/// value in the row. Documents are grouped under the folders of a checkout's
/// `documents/`, every matter folder listed even while empty. A chain with
/// earlier revisions expands into a Previous versions table giving each
/// one's created-at time and its own visibility.
pub(crate) fn documents_table(view: &LawyerDetailView) -> Element {
    let groups = document_groups(&view.document_folders, &view.documents);
    rsx! {
        if let Some(error) = view.error.as_ref() {
            p { class: "nav-form-error", role: "alert", "{error}" }
        }
        if view.documents.is_empty() {
            p { class: "projects-empty", "No documents yet." }
        } else {
            div { class: "nav-table-wrap",
                table { class: "nav-table",
                    thead {
                        tr {
                            th { scope: "col", "Filename" }
                            th { scope: "col", "Visibility" }
                            th { scope: "col", "Revisions" }
                            th { scope: "col", "Download" }
                        }
                    }
                    for (folder, docs) in groups.iter() {
                        tbody {
                            tr {
                                th { scope: "colgroup", colspan: "4", "documents/{folder}/" }
                            }
                            if docs.is_empty() {
                                tr { td { colspan: "4", class: "nav-muted", "Empty" } }
                            }
                            for (name, doc) in docs.iter() {
                            tr {
                                td {
                                    if doc.revisions.len() < 2 {
                                        a { class: "nav-link", href: "/app/projects/{view.code}/documents/{doc.id}", "{name}" }
                                    } else {
                                        details {
                                            summary {
                                                a { class: "nav-link", href: "/app/projects/{view.code}/documents/{doc.id}", "{name}" }
                                            }
                                            table { class: "nav-table",
                                                caption { "Previous versions" }
                                                thead {
                                                    tr {
                                                        th { scope: "col", "Filename" }
                                                        th { scope: "col", "Created at" }
                                                        th { scope: "col", "Visibility" }
                                                        th { scope: "col", "Download" }
                                                    }
                                                }
                                                tbody {
                                                    // The first revision is the operative one the row already shows.
                                                    for revision in doc.revisions.iter().skip(1) {
                                                        tr {
                                                            td { a { class: "nav-link", href: "/app/projects/{view.code}/documents/{revision.id}", "{revision.filename}" } }
                                                            td { "{revision.created_at}" }
                                                            td { "{revision.visibility}" }
                                                            td { a { class: "nav-link", href: "/app/projects/{view.code}/documents/{revision.id}/download", "Download" } }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                td {
                                    "{doc.visibility}"
                                    if !doc.service.is_empty() {
                                        ul { class: "project-documents__service",
                                            for line in doc.service.iter() {
                                                li { "{line}" }
                                            }
                                        }
                                    }
                                }
                                td {
                                    if doc.revisions.is_empty() {
                                        "—"
                                    } else if doc.revisions.len() == 1 {
                                        "1 revision"
                                    } else {
                                        "{doc.revisions.len()} revisions"
                                    }
                                }
                                td { a { class: "nav-link", href: "/app/projects/{view.code}/documents/{doc.id}/download", "Download" } }
                            }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The documents table's folders, in order: every matter folder the view
/// names, then any other top folder a document sits in (a sibling folder
/// such as `onboarding`), alphabetically. Each folder's documents are sorted
/// by path and paired with the path below that folder they are shown as.
fn document_groups<'a>(
    folders: &[String],
    documents: &'a [LawyerDocRow],
) -> Vec<(String, Vec<(String, &'a LawyerDocRow)>)> {
    let mut names: Vec<String> = folders.to_vec();
    let mut others: Vec<String> = documents
        .iter()
        .map(|doc| doc.folder.clone())
        .filter(|folder| !names.contains(folder))
        .collect();
    others.sort();
    others.dedup();
    names.extend(others);
    names
        .into_iter()
        .map(|folder| {
            let mut docs: Vec<&LawyerDocRow> = documents
                .iter()
                .filter(|doc| doc.folder == folder)
                .collect();
            docs.sort_by(|a, b| a.path.cmp(&b.path));
            let docs = docs
                .into_iter()
                .map(|doc| {
                    let name = doc
                        .path
                        .strip_prefix(&format!("{folder}/"))
                        .filter(|name| !name.is_empty())
                        .unwrap_or(&doc.filename)
                        .to_string();
                    (name, doc)
                })
                .collect();
            (folder, docs)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lawyer_project_detail::LawyerDetailView;

    fn view_with_documents(documents: Vec<LawyerDocRow>) -> LawyerDetailView {
        LawyerDetailView {
            code: "sample-litigation".to_string(),
            documents,
            ..LawyerDetailView::default()
        }
    }

    fn render_documents(documents: Vec<LawyerDocRow>) -> String {
        dioxus_ssr::render_element(documents_table(&view_with_documents(documents)))
    }

    #[test]
    fn an_empty_document_table_shows_the_empty_state_and_error_flash() {
        let view = LawyerDetailView {
            error: Some("The document could not be updated.".to_string()),
            ..LawyerDetailView::default()
        };
        let html = dioxus_ssr::render_element(documents_table(&view));
        assert!(html.contains("No documents yet."), "{html}");
        assert!(
            html.contains("The document could not be updated."),
            "{html}"
        );
        assert!(html.contains(r#"role="alert""#), "{html}");
    }

    #[test]
    fn a_document_with_an_internal_operative_revision_reads_as_internal() {
        let html = render_documents(vec![LawyerDocRow {
            id: "00000000-0000-0000-0000-0000000000aa".to_string(),
            filename: "memo.pdf".to_string(),
            visibility: "internal".to_string(),
            path: String::new(),
            folder: String::new(),
            revisions: Vec::new(),
            service: Vec::new(),
        }]);
        assert!(html.contains("<td>internal</td>"), "{html}");
    }

    #[test]
    fn a_document_with_a_client_visible_operative_revision_reads_as_shared() {
        let html = render_documents(vec![LawyerDocRow {
            id: "00000000-0000-0000-0000-0000000000aa".to_string(),
            filename: "retainer.pdf".to_string(),
            visibility: "shared".to_string(),
            path: String::new(),
            folder: String::new(),
            revisions: Vec::new(),
            service: Vec::new(),
        }]);
        assert!(html.contains("<td>shared</td>"), "{html}");
    }

    /// LAW-124: a document's Service lines print beside its visibility,
    /// one per record, and a document with none prints no list.
    #[test]
    fn a_served_document_lists_each_service_beside_its_visibility() {
        let html = render_documents(vec![LawyerDocRow {
            id: "a".into(),
            filename: "motion.pdf".into(),
            visibility: "internal".into(),
            path: String::new(),
            folder: String::new(),
            revisions: Vec::new(),
            service: vec![
                "Served by mail on 2026-03-05 on Example Counsel, counsel for Defendant (D.I. 12)"
                    .into(),
                "Served by electronic service on 2026-03-06 on Other Counsel (D.I. 12)".into(),
            ],
        }]);
        assert_eq!(html.matches("<li>Served ").count(), 2, "{html}");
        assert!(html.contains("project-documents__service"), "{html}");
        assert!(html.contains("counsel for Defendant (D.I. 12)"), "{html}");
        let none = render_documents(vec![LawyerDocRow {
            id: "b".into(),
            filename: "letter.pdf".into(),
            visibility: "internal".into(),
            path: String::new(),
            folder: String::new(),
            revisions: Vec::new(),
            service: Vec::new(),
        }]);
        assert!(!none.contains("project-documents__service"), "{none}");
    }

    #[test]
    fn a_slugless_one_off_asset_still_shows_the_visibility_column() {
        // Empty `revisions` is exactly the one-off-asset shape (#511's
        // `NULL`-slug rows never enter a chain), and the column still renders.
        let html = render_documents(vec![LawyerDocRow {
            id: "00000000-0000-0000-0000-0000000000aa".to_string(),
            filename: "inbound-attachment.pdf".to_string(),
            visibility: "internal".to_string(),
            path: String::new(),
            folder: String::new(),
            revisions: Vec::new(),
            service: Vec::new(),
        }]);
        assert!(html.contains("Visibility"), "{html}");
        assert!(html.contains("<td>internal</td>"), "{html}");
    }

    #[test]
    fn a_chain_whose_revisions_disagree_shows_the_operative_value_and_each_revisions_own() {
        let html = render_documents(vec![LawyerDocRow {
            id: "00000000-0000-0000-0000-0000000000aa".to_string(),
            filename: "agreement.pdf".to_string(),
            // The operative (newest) revision is shared…
            visibility: "shared".to_string(),
            path: String::new(),
            folder: String::new(),
            revisions: vec![
                LawyerDocRevision {
                    id: "00000000-0000-0000-0000-0000000000aa".to_string(),
                    filename: "agreement.pdf".to_string(),
                    visibility: "shared".to_string(),
                    created_at: "2 Nov 26 09:00 UTC".to_string(),
                },
                // …while an earlier draft under the same slug stayed internal.
                LawyerDocRevision {
                    id: "00000000-0000-0000-0000-0000000000ac".to_string(),
                    filename: "agreement.pdf".to_string(),
                    visibility: "internal".to_string(),
                    created_at: "1 Nov 26 14:30 UTC".to_string(),
                },
            ],
            service: Vec::new(),
        }]);
        assert!(html.contains("<td>shared</td>"), "{html}");
        assert!(html.contains("Previous versions"), "{html}");
        assert!(html.contains("<td>1 Nov 26 14:30 UTC</td>"), "{html}");
        assert!(html.contains("<td>internal</td>"), "{html}");
        assert!(
            !html.contains("<td>2 Nov 26 09:00 UTC</td>"),
            "the operative revision is the row, not a previous version: {html}"
        );
    }

    #[test]
    fn a_chain_of_one_revision_has_no_previous_versions_table() {
        let html = render_documents(vec![LawyerDocRow {
            id: "00000000-0000-0000-0000-0000000000aa".to_string(),
            filename: "agreement.pdf".to_string(),
            visibility: "internal".to_string(),
            path: String::new(),
            folder: String::new(),
            revisions: vec![LawyerDocRevision {
                id: "00000000-0000-0000-0000-0000000000aa".to_string(),
                filename: "agreement.pdf".to_string(),
                visibility: "internal".to_string(),
                created_at: "2 Nov 26 09:00 UTC".to_string(),
            }],
            service: Vec::new(),
        }]);
        assert!(html.contains("1 revision"), "{html}");
        assert!(!html.contains("Previous versions"), "{html}");
    }

    #[test]
    fn documents_sit_under_their_documents_folders_with_every_matter_folder_listed() {
        let row = |id: &str, path: &str| LawyerDocRow {
            id: id.to_string(),
            filename: path.rsplit('/').next().unwrap().to_string(),
            visibility: "internal".to_string(),
            path: path.to_string(),
            folder: path.split('/').next().unwrap().to_string(),
            revisions: Vec::new(),
            service: Vec::new(),
        };
        let view = LawyerDetailView {
            document_folders: vec!["Docket Filings".to_string(), "Work Product".to_string()],
            ..view_with_documents(vec![
                row(
                    "00000000-0000-0000-0000-0000000000a1",
                    "onboarding/engagement.pdf",
                ),
                row(
                    "00000000-0000-0000-0000-0000000000a2",
                    "Docket Filings/pleadings/motion.pdf",
                ),
            ])
        };
        let html = dioxus_ssr::render_element(documents_table(&view));
        let docket = html
            .find("documents/Docket Filings/")
            .expect("docket folder");
        let work = html
            .find("documents/Work Product/")
            .expect("work product folder");
        let onboarding = html.find("documents/onboarding/").expect("sibling folder");
        assert!(docket < work && work < onboarding, "{html}");
        assert!(html.contains(">pleadings/motion.pdf<"), "{html}");
        assert!(html.contains(">engagement.pdf<"), "{html}");
        assert!(
            html.contains("Empty"),
            "the empty folder is still listed: {html}"
        );
    }
}
