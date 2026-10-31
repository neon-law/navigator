//! The lawyer workbench's notation-workflow board: one row per template, run
//! beside never-started templates, with the frontmatter-derived workflow state
//! columns and the server-side assembly that reads templates, runs, and
//! signature records. [`crate::lawyer_project_detail`] composes the page.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// One run or never-started template in a Project's workflow board.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct ProjectNotationRow {
    pub template_code: String,
    pub notation_id: Option<String>,
    pub current_state: Option<String>,
    pub workflow_states: Vec<String>,
    pub state_entered_at: std::collections::BTreeMap<String, String>,
    pub respondent_email: Option<String>,
    pub signature_request_id: Option<String>,
    pub last_transition_at: Option<String>,
    /// Whether the run's pinned template digest has been overtaken by the
    /// template's current definition on the repository's main (LAW-201).
    #[serde(default)]
    pub template_stale: bool,
}

/// Assemble the private Project notation board from its current templates,
/// runs, transition journal, respondents, and signature records.
#[cfg(feature = "server")]
pub async fn project_notation_board(
    surreal: &store::surreal::SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    project_id: uuid::Uuid,
) -> Result<Vec<ProjectNotationRow>, String> {
    let templates = store::templates::list_for_project(surreal, project_id)
        .await
        .map_err(|error| error.to_string())?;
    let notations = store::notations::list_by_project(surreal, project_id)
        .await
        .map_err(|error| error.to_string())?;
    let repository_templates = project_repository_templates(surreal, project_id).await?;
    let mut rows =
        Vec::with_capacity(templates.len() + notations.len() + repository_templates.len());
    let mut started_sources = std::collections::BTreeSet::new();
    let mut started = std::collections::BTreeSet::new();

    for notation in notations {
        let template = store::templates::find_by_id(surreal, notation.template_id)
            .await
            .map_err(|error| error.to_string())?;
        let Some(template) = template else { continue };
        if let Some(hash) = &template.body_sha256 {
            started_sources.insert((template.code.clone(), hash.clone()));
        }
        let workflow_states = match store::templates::body(surreal, storage, &template).await {
            Ok(markdown) => workflow_state_order(&markdown),
            // Template registrations can exist before their source body is
            // attached. Keep the matter board readable while the workflow
            // definition is incomplete; other storage and integrity errors
            // still fail the read.
            Err(store::templates::TemplateBodyError::MissingBody(_)) => Vec::new(),
            Err(error) => return Err(error.to_string()),
        };
        let events = store::notation_events::for_notation(surreal, notation.id)
            .await
            .map_err(|error| error.to_string())?;
        let mut state_entered_at = std::collections::BTreeMap::new();
        for event in events
            .iter()
            .filter(|event| event.machine_kind == store::notation_events::MACHINE_WORKFLOW)
        {
            state_entered_at.insert(event.to_state.clone(), event.recorded_at.clone());
        }
        if notation.state == "BEGIN" {
            state_entered_at
                .entry("BEGIN".to_string())
                .or_insert_with(|| notation.inserted_at.to_rfc3339());
        }
        let respondent = store::persons::find_by_id(surreal, notation.person_id)
            .await
            .map_err(|error| error.to_string())?;
        let signature_request_id = store::signatures::request_id_for_notation(surreal, notation.id)
            .await
            .map_err(|error| error.to_string())?;
        let last_transition_at = state_entered_at
            .get(&notation.state)
            .cloned()
            .or_else(|| Some(notation.updated_at.to_rfc3339()));
        let template_stale = notation_run_is_stale(&notation, &template, &repository_templates);
        started.insert(notation.template_id);
        rows.push(ProjectNotationRow {
            template_code: template.code,
            notation_id: Some(notation.id.to_string()),
            current_state: Some(notation.state),
            workflow_states,
            state_entered_at,
            respondent_email: respondent.map(|person| person.email),
            signature_request_id,
            last_transition_at,
            template_stale,
        });
    }

    for template in templates {
        if started.contains(&template.id) || repository_templates.contains_key(&template.code) {
            continue;
        }
        let workflow_states = match store::templates::body(surreal, storage, &template).await {
            Ok(markdown) => workflow_state_order(&markdown),
            Err(store::templates::TemplateBodyError::MissingBody(_)) => Vec::new(),
            Err(error) => return Err(error.to_string()),
        };
        rows.push(ProjectNotationRow {
            template_code: template.code,
            workflow_states,
            ..ProjectNotationRow::default()
        });
    }
    for (code, source) in repository_templates {
        if started_sources.contains(&(code.clone(), source.body_sha256)) {
            continue;
        }
        rows.push(ProjectNotationRow {
            template_code: code,
            workflow_states: source.workflow_states,
            ..ProjectNotationRow::default()
        });
    }
    rows.sort_by(|a, b| {
        a.template_code
            .cmp(&b.template_code)
            .then(a.notation_id.cmp(&b.notation_id))
    });
    Ok(rows)
}

#[cfg(feature = "server")]
struct RepositoryTemplate {
    body_sha256: String,
    workflow_states: Vec<String>,
}

/// The run's staleness verdict against the template's current source
/// definition. The run's pinned digest falls back through the
/// store-template binding for runs opened before the field existed
/// (LAW-201).
#[cfg(feature = "server")]
fn notation_run_is_stale(
    run: &store::notations::Notation,
    template: &store::templates::Template,
    repository_templates: &std::collections::BTreeMap<String, RepositoryTemplate>,
) -> bool {
    let pinned_sha = run
        .template_body_sha256
        .clone()
        .or_else(|| template.body_sha256.clone());
    store::templates::run_is_stale(
        pinned_sha.as_deref(),
        repository_templates
            .get(&template.code)
            .map(|source| source.body_sha256.as_str()),
    )
}

/// Read current blueprint definitions without importing versions or opening runs.
#[cfg(feature = "server")]
async fn project_repository_templates(
    surreal: &store::surreal::SurrealDb,
    project_id: uuid::Uuid,
) -> Result<std::collections::BTreeMap<String, RepositoryTemplate>, String> {
    let project = store::projects::find_by_id(surreal, project_id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Project not found".to_string())?;
    let repo = match repos::RepoStore::from_env() {
        Ok(repo) => repo,
        // Deployments without a configured bare repository still serve their
        // persisted Template inventory. Configured source failures remain errors.
        Err(repos::RepoError::RootUnset) => {
            return Ok(std::collections::BTreeMap::default());
        }
        Err(error) => return Err(error.to_string()),
    };
    // Reading must not provision: `read_head_tree_code` would `git init` an
    // absent repository, and the distroless `neon` image ships no `git`, so
    // every never-pushed matter's workbench would fail. Anything already at
    // the path is still read, so an unreadable repository remains an error.
    if !repo.path_for_code(&project.code).exists() {
        return Ok(std::collections::BTreeMap::default());
    }
    let tree = tokio::task::spawn_blocking(move || repo.read_head_tree_code(&project.code))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    repository_templates_from_tree(tree)
}

#[cfg(feature = "server")]
fn repository_templates_from_tree(
    tree: Vec<(String, Vec<u8>)>,
) -> Result<std::collections::BTreeMap<String, RepositoryTemplate>, String> {
    use rules::project_blueprints::{select, BlueprintSelection};

    let directory = match select(
        tree.iter().any(|(path, _)| path.starts_with("templates/")),
        tree.iter().any(|(path, _)| path.starts_with("drafts/")),
    ) {
        BlueprintSelection::Selected(directory) => directory,
        BlueprintSelection::Absent => return Ok(std::collections::BTreeMap::new()),
        BlueprintSelection::Ambiguous => {
            return Err("Project repository has both drafts/ and templates/".into())
        }
    };
    let prefix = format!("{}/", directory.as_str());
    let mut templates = std::collections::BTreeMap::new();
    for (path, bytes) in tree {
        let Some(relative) = path.strip_prefix(&prefix) else {
            continue;
        };
        let code = relative
            .strip_suffix(".md")
            .filter(|code| !code.is_empty() && !code.contains('/'))
            .ok_or_else(|| "Project blueprints must be direct Markdown files".to_string())?;
        let source =
            String::from_utf8(bytes).map_err(|_| "Project blueprint is not UTF-8".to_string())?;
        let compiled = rules::clauses::compile(&source)
            .map_err(|_| "Project blueprint clauses could not be composed".to_string())?;
        let frontmatter = rules::frontmatter::extract(&compiled)
            .ok_or_else(|| "Project blueprint has no frontmatter".to_string())?;
        let document = serde_yaml::from_str::<serde_yaml::Value>(frontmatter)
            .map_err(|_| "Project blueprint frontmatter is malformed".to_string())?;
        if document.get("workflow").is_some() {
            workflows::workflow_spec_from_template(&compiled)
                .map_err(|_| "Project blueprint workflow is malformed".to_string())?;
        }
        templates.insert(
            code.to_string(),
            RepositoryTemplate {
                body_sha256: store::assets::sha256_hex(compiled.as_bytes()),
                workflow_states: workflow_state_order(&compiled),
            },
        );
    }
    Ok(templates)
}

#[cfg(feature = "server")]
fn workflow_state_order(markdown: &str) -> Vec<String> {
    let Some(frontmatter) = markdown
        .strip_prefix("---")
        .and_then(|text| text.split_once("---").map(|(yaml, _)| yaml))
    else {
        return Vec::new();
    };
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(frontmatter) else {
        return Vec::new();
    };
    value
        .get("workflow")
        .and_then(serde_yaml::Value::as_mapping)
        .map(|workflow| {
            workflow
                .keys()
                .filter_map(serde_yaml::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn notation_columns(rows: &[ProjectNotationRow]) -> Vec<String> {
    let mut columns = Vec::new();
    for row in rows {
        for state in &row.workflow_states {
            if !columns.contains(state) {
                columns.push(state.clone());
            }
        }
    }
    columns
}

fn notation_state_cell(row: &ProjectNotationRow, state: &str) -> String {
    if row.notation_id.is_none() {
        return if row.workflow_states.iter().any(|item| item == state) {
            "·".to_string()
        } else {
            "—".to_string()
        };
    }
    if !row.workflow_states.iter().any(|item| item == state) {
        return "—".to_string();
    }
    let entered = row.state_entered_at.get(state);
    let mark = if row.current_state.as_deref() == Some(state) {
        "●"
    } else if entered.is_some() {
        if state.contains("declined") || state.contains("failed") {
            "✗"
        } else {
            "✓"
        }
    } else {
        "·"
    };
    match entered {
        Some(time) => format!("{mark} {time}"),
        None => mark.to_string(),
    }
}

/// The Project's workflow board: current runs beside templates not yet opened.
#[component]
pub fn ProjectNotationBoard(rows: Vec<ProjectNotationRow>) -> Element {
    let columns = notation_columns(&rows);
    rsx! {
        section { class: "lawyer-detail__section project-notations",
            h2 { "Notation workflows" }
            if rows.is_empty() {
                p { class: "projects-empty", "No notation templates are registered on this project." }
            } else {
                p { class: "nav-muted", "Every project template appears here, including ones never started." }
                div { class: "nav-table-wrap",
                    table { class: "nav-table",
                        thead { tr {
                            for state in columns.iter() { th { scope: "col", "{state}" } }
                            th { scope: "col", "Template" }
                            th { scope: "col", "Notation UUID" }
                            th { scope: "col", "Client email" }
                            th { scope: "col", "Signature request" }
                            th { scope: "col", "Last transition" }
                        } }
                        tbody {
                            for row in rows.iter() {
                                tr {
                                    for state in columns.iter() {
                                        td { "{notation_state_cell(row, state)}" }
                                    }
                                    th { scope: "row", "{row.template_code}" }
                                    td { "{row.notation_id.as_deref().unwrap_or(\"never started\")}" }
                                    td { "{row.respondent_email.as_deref().unwrap_or(\"—\")}" }
                                    td { "{row.signature_request_id.as_deref().unwrap_or(\"—\")}" }
                                    td { "{row.last_transition_at.as_deref().unwrap_or(\"—\")}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "server")]
    fn workflow_state_columns_follow_the_template_frontmatter() {
        let states = workflow_state_order(
            "---\nworkflow:\n  BEGIN:\n    label: Started\n  lawyer_review:\n    label: Review\n  reask__client:\n    label: Client\n---\n# Template\n",
        );
        assert_eq!(states, ["BEGIN", "lawyer_review", "reask__client"]);
    }

    #[test]
    #[cfg(feature = "server")]
    fn malformed_or_missing_workflow_frontmatter_has_no_state_columns() {
        assert!(workflow_state_order("# No frontmatter\n").is_empty());
        assert!(workflow_state_order("---\nworkflow: [\n---\n").is_empty());
        assert!(workflow_state_order("---\nworkflow: not-a-map\n---\n").is_empty());
        assert!(workflow_state_order("---\ntitle: Plain template\n---\n").is_empty());
    }

    #[test]
    fn notation_board_shows_never_started_templates_and_live_run_metadata() {
        let live = ProjectNotationRow {
            template_code: "engagement".to_string(),
            notation_id: Some("notation-uuid".to_string()),
            current_state: Some("lawyer_review".to_string()),
            workflow_states: vec!["BEGIN".to_string(), "lawyer_review".to_string()],
            state_entered_at: [(
                "lawyer_review".to_string(),
                "2026-09-25T14:03:00Z".to_string(),
            )]
            .into(),
            respondent_email: Some("client@example.test".to_string()),
            signature_request_id: Some("signature-uuid".to_string()),
            last_transition_at: Some("2026-09-25T14:03:00Z".to_string()),
            template_stale: false,
        };
        let waiting = ProjectNotationRow {
            template_code: "will".to_string(),
            workflow_states: vec!["BEGIN".to_string(), "lawyer_review".to_string()],
            ..ProjectNotationRow::default()
        };
        let html = dioxus_ssr::render_element(rsx! {
            ProjectNotationBoard { rows: vec![live, waiting] }
        });
        assert!(html.contains("engagement"), "{html}");
        assert!(html.contains("notation-uuid"), "{html}");
        assert!(html.contains("client@example.test"), "{html}");
        assert!(html.contains("signature-uuid"), "{html}");
        assert!(html.contains("never started"), "{html}");
        assert!(html.contains("2026-09-25T14:03:00Z"), "{html}");
    }

    #[test]
    fn an_empty_notation_board_explains_that_no_templates_are_registered() {
        let html = dioxus_ssr::render_element(rsx! { ProjectNotationBoard { rows: vec![] } });
        assert!(
            html.contains("No notation templates are registered on this project."),
            "{html}"
        );
        assert!(!html.contains("Notation UUID"), "{html}");
    }

    #[test]
    fn notation_state_cells_distinguish_current_completed_and_unvisited_states() {
        let row = ProjectNotationRow {
            notation_id: Some("notation-uuid".to_string()),
            current_state: Some("lawyer_review".to_string()),
            workflow_states: vec![
                "BEGIN".to_string(),
                "lawyer_review".to_string(),
                "filed".to_string(),
            ],
            state_entered_at: [("BEGIN".to_string(), "2026-09-24T09:00:00Z".to_string())].into(),
            ..ProjectNotationRow::default()
        };
        assert!(notation_state_cell(&row, "BEGIN").starts_with("✓"));
        assert!(notation_state_cell(&row, "lawyer_review").starts_with("●"));
        assert_eq!(notation_state_cell(&row, "filed"), "·");
        assert_eq!(notation_state_cell(&row, "not_in_workflow"), "—");
    }

    #[test]
    fn notation_state_cells_mark_never_started_and_unsuccessful_runs() {
        let waiting = ProjectNotationRow {
            workflow_states: vec!["BEGIN".to_string(), "filed".to_string()],
            ..ProjectNotationRow::default()
        };
        assert_eq!(notation_state_cell(&waiting, "BEGIN"), "·");
        assert_eq!(notation_state_cell(&waiting, "other"), "—");

        let completed = ProjectNotationRow {
            notation_id: Some("notation-uuid".to_string()),
            workflow_states: vec!["declined".to_string(), "failed".to_string()],
            state_entered_at: [
                ("declined".to_string(), "2026-09-25T15:00:00Z".to_string()),
                ("failed".to_string(), "2026-09-25T16:00:00Z".to_string()),
            ]
            .into(),
            ..ProjectNotationRow::default()
        };
        assert_eq!(
            notation_state_cell(&completed, "declined"),
            "✗ 2026-09-25T15:00:00Z"
        );
        assert_eq!(
            notation_state_cell(&completed, "failed"),
            "✗ 2026-09-25T16:00:00Z"
        );
    }
}
