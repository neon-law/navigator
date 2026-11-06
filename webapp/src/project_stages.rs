//! The stage chip's read: a matter's stage vocabulary, its current stage, and
//! the audited history.
//!
//! The dashboard's read seam for [`store::case_stages`], beside the write
//! door in the portal's stage API. Reached only through
//! `store::access::matter_viewer` — a participation row is required of every
//! tier (ENG-81), and the stage surface is a firm-side surface, so a
//! client-side participant, a clerk, an unassigned lawyer, Owner and Admin
//! included, all get the non-disclosing `404`. A direct hit on the generated
//! `#[server]` endpoint defaults to the least-privileged tier, so the
//! lawyer-tier check here is defense in depth, not a duplicate of the
//! router's gate.
//!
//! Rows are plain scalars — no `store` type crosses to the wasm build — and
//! identifiers are resolved to display words: the stage to its Firm-authored
//! label, the actor to a person name, never carried as a bare id. The
//! vocabulary is Firm data, never a hardcoded enum, and a Case Stage is
//! deliberately distinct from `project.status` and `case.status` (see
//! `docs/glossary/case-stage.md`).

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{Badge, BadgeTone, Choice, Empty, Field, FormCard, Heading};
#[cfg(feature = "server")]
use crate::people::ViewerRole;

/// One entry in the Firm's ordered stage vocabulary.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StageDefinitionRow {
    pub id: String,
    pub label: String,
    /// The Firm's ordering; a gap is deliberate — it lets the Firm slot a
    /// stage between two others later.
    pub position: i32,
    /// Narrows a stage to one litigation posture when the vocabulary differs
    /// by case kind.
    pub case_type: Option<String>,
}

/// One audited transition, resolved to display words.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StageEventRow {
    /// Where the matter moved from; `None` on the first event.
    pub from_label: Option<String>,
    pub to_label: String,
    pub actor: Option<String>,
    pub note: Option<String>,
    /// RFC 3339 timestamp of the transition.
    pub occurred_at: String,
}

/// The matter's stage read for the dashboard's stage chip and its history.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MatterStageView {
    /// The Firm's vocabulary, in the order the Firm staged it.
    pub definitions: Vec<StageDefinitionRow>,
    /// The stage the matter currently holds — `None` until the Firm places
    /// it. Not a lifecycle status: that is `project.status`, kept separate.
    pub current: Option<StageDefinitionRow>,
    /// Every transition, oldest first.
    pub history: Vec<StageEventRow>,
}

/// The stage chip in the matter header — where the matter sits in the
/// Firm's workflow, or `not set` until the Firm places it. Deliberately
/// distinct from the lifecycle `Status` the header renders beside it:
/// a stage moves as the work moves; the lifecycle status does not.
#[component]
pub fn StageChip(current: Option<String>) -> Element {
    match current {
        Some(label) => rsx! {
            " · Stage: "
            Badge { tone: BadgeTone::Active, "{label}" }
        },
        None => rsx! {
            " · Stage: not set"
        },
    }
}

/// The Overview tab's stage surface: where the matter sits in the Firm's
/// workflow, the control that moves it, and the audited trail of where it
/// has been. A Case Stage is Firm-authored workflow placement — never the
/// matter's lifecycle status (`project.status`) and never a court case's
/// status (`case.status`), and the copy keeps the three apart.
///
/// The set control is a native-form POST to the matter's stage handler,
/// which re-asks the same authorization this page rendered under.
#[component]
pub fn StageSection(
    code: String,
    csrf: String,
    stage: MatterStageView,
    /// The `?error=` flash a refused write redirected back with.
    #[props(default)]
    error: Option<String>,
) -> Element {
    rsx! {
        section { class: "lawyer-detail__section lawyer-detail__stage",
            h2 { "Case stage" }
            if let Some(error) = error {
                p { class: "nav-form-error", role: "alert", "{error}" }
            }
            match &stage.current {
                Some(current) => rsx! {
                    p { class: "lawyer-detail__stage-current",
                        "This matter is in the "
                        Badge { tone: BadgeTone::Active, "{current.label}" }
                        " stage of the Firm's workflow."
                    }
                },
                None => rsx! {
                    Empty {
                        title: "No stage set for this matter yet".to_string(),
                        description: Some(
                            "A Case Stage is where this matter sits in the Firm's own \
                             workflow — not the matter's lifecycle status, and not a \
                             court case's status. Choose the stage below.".to_string(),
                        ),
                    }
                },
            }
            if !stage.history.is_empty() {
                div { class: "nav-table-wrap",
                    table { class: "nav-table",
                        thead {
                            tr {
                                th { scope: "col", "From" }
                                th { scope: "col", "To" }
                                th { scope: "col", "By" }
                                th { scope: "col", "When" }
                                th { scope: "col", "Note" }
                            }
                        }
                        tbody {
                            for row in stage.history.iter() {
                                tr {
                                    td { "{dash_or(row.from_label.as_ref())}" }
                                    td {
                                        Badge { tone: BadgeTone::Active, "{row.to_label}" }
                                    }
                                    td { "{dash_or(row.actor.as_ref())}" }
                                    td { "{day_of(&row.occurred_at)}" }
                                    td { "{row.note.clone().unwrap_or_default()}" }
                                }
                            }
                        }
                    }
                }
            }
            if stage.definitions.is_empty() {
                Empty {
                    title: "The Firm has no stage vocabulary yet".to_string(),
                    description: Some(
                        "Stages are the Firm's own workflow vocabulary. Once the \
                         Firm defines its stages, this matter can be placed in one.".to_string(),
                    ),
                }
            } else {
                {
                    let choices: Vec<Choice> = stage
                        .definitions
                        .iter()
                        .map(|row| Choice::new(row.id.clone(), row.label.clone()))
                        .collect();
                    let selected = stage.current.as_ref().map(|row| row.id.clone());
                    rsx! {
                        FormCard {
                            title: "Set stage".to_string(),
                            action: "/app/projects/{code}/stages/set",
                            submit_label: "Set stage".to_string(),
                            heading: Heading::H2,
                            csrf_token: Some(csrf.clone()),
                            fields: vec![
                                Field::select("Stage", "stage_id", choices, selected)
                                    .help("The stage the matter has moved to, from the Firm's own vocabulary."),
                                Field::textarea("Note", "note", "", 2)
                                    .help("Optional. Why the matter moved — recorded on the audited transition."),
                            ],
                        }
                    }
                }
            }
        }
    }
}

/// The day a transition happened, out of its RFC 3339 timestamp — the
/// precision a lawyer reads a stage history at.
fn day_of(occurred_at: &str) -> String {
    occurred_at.get(..10).unwrap_or(occurred_at).to_string()
}

/// A display word or an em dash — never a blank cell.
fn dash_or(value: Option<&String>) -> String {
    value.cloned().unwrap_or_else(|| "\u{2014}".to_string())
}

/// Commit the `404` the matter surface answers for a caller the matter does
/// not exist for, and fail the server fn with it — hidden, never refused.
#[cfg(feature = "server")]
fn hidden_matter() -> ServerFnError {
    dioxus_fullstack_core::FullstackContext::commit_http_status(
        axum::http::StatusCode::NOT_FOUND,
        None,
    );
    ServerFnError::new("no such matter")
}

/// Map the store's definition rows onto the wasm-safe vocabulary rows.
#[cfg(feature = "server")]
fn definition_rows(definitions: &[store::case_stages::StageDefinition]) -> Vec<StageDefinitionRow> {
    definitions
        .iter()
        .map(|row| StageDefinitionRow {
            id: row.id.to_string(),
            label: row.label.clone(),
            position: row.position,
            case_type: row.case_type.clone(),
        })
        .collect()
}

/// Map the store's history onto the wasm-safe event rows, resolving stage ids
/// to the Firm's labels and actor ids to person names.
#[cfg(feature = "server")]
fn event_rows(
    events: &[store::case_stages::StageEvent],
    labels: &std::collections::HashMap<uuid::Uuid, String>,
    names: &std::collections::HashMap<uuid::Uuid, String>,
) -> Vec<StageEventRow> {
    events
        .iter()
        .map(|row| StageEventRow {
            from_label: row.from_stage_id.and_then(|id| labels.get(&id).cloned()),
            to_label: labels
                .get(&row.to_stage_id)
                .cloned()
                .unwrap_or_else(|| "unknown stage".to_string()),
            actor: names.get(&row.actor_person_id).cloned(),
            note: row.note.clone(),
            occurred_at: row.occurred_at.to_rfc3339(),
        })
        .collect()
}

/// Fetch the matter's stage read for a dashboard server fn that has already
/// resolved its viewer — the same wasm-safe view [`get_matter_stage`]
/// returns, without re-asking router admission. The stage store is
/// PostgreSQL-native: the caller decides what a deployment without
/// PostgreSQL renders (the dashboard workbench renders without a stage).
#[cfg(feature = "server")]
pub(crate) async fn read_stage(
    surreal: &store::surreal::SurrealDb,
    pg: &store::postgres::PostgresDb,
    project: &store::projects::Project,
) -> Result<MatterStageView, ServerFnError> {
    let definitions = match project.firm_id {
        Some(firm_id) => store::case_stages::list_definitions(pg, firm_id)
            .await
            .map_err(crate::lawyer_project_detail::server_error)?,
        None => Vec::new(),
    };
    let current = store::case_stages::current_stage(pg, project.id)
        .await
        .map_err(crate::lawyer_project_detail::server_error)?;
    let history = store::case_stages::history(pg, project.id)
        .await
        .map_err(crate::lawyer_project_detail::server_error)?;

    // Resolve every id the rows carry to a display word: stage ids to the
    // Firm's labels, actor ids to person names from the directory.
    let labels: std::collections::HashMap<uuid::Uuid, String> = definitions
        .iter()
        .map(|row| (row.id, row.label.clone()))
        .collect();
    let actor_ids: Vec<uuid::Uuid> = history.iter().map(|row| row.actor_person_id).collect();
    let names: std::collections::HashMap<uuid::Uuid, String> =
        store::persons::find_by_ids(surreal, &actor_ids)
            .await
            .map_err(crate::lawyer_project_detail::server_error)?
            .into_iter()
            .map(|person| (person.id, person.name))
            .collect();

    Ok(MatterStageView {
        definitions: definition_rows(&definitions),
        current: current.as_ref().map(|row| StageDefinitionRow {
            id: row.id.to_string(),
            label: row.label.clone(),
            position: row.position,
            case_type: row.case_type.clone(),
        }),
        history: event_rows(&history, &labels, &names),
    })
}

/// Fetch the matter's stage read: the Firm's vocabulary, the stage the matter
/// currently holds, and its audited history, oldest first.
#[cfg_attr(feature = "server", allow(clippy::too_many_lines))]
#[server]
pub async fn get_matter_stage() -> Result<MatterStageView, ServerFnError> {
    let axum::extract::Path(code) =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Path<String>, _>()
            .await?;
    let role = dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<ViewerRole>, _>()
        .await
        .map(|axum::Extension(role)| role)
        .unwrap_or_default();
    if !role.is_lawyer_tier() {
        return Err(hidden_matter());
    }
    let crate::portal_project_list::PersonId(person_id) =
        dioxus_fullstack_core::FullstackContext::extract::<
            axum::Extension<crate::portal_project_list::PersonId>,
            _,
        >()
        .await
        .map(|axum::Extension(id)| id)
        .unwrap_or_default();
    let person_id = person_id.and_then(|raw| raw.parse::<uuid::Uuid>().ok());

    let surreal = consume_context::<store::surreal::SurrealDb>();
    let Some(project) = store::projects::find_by_code(&surreal, &code)
        .await
        .map_err(crate::lawyer_project_detail::server_error)?
    else {
        return Err(hidden_matter());
    };

    let store_role = match role {
        ViewerRole::Owner => store::persons::Role::Owner,
        ViewerRole::Admin => store::persons::Role::Admin,
        ViewerRole::Lawyer => store::persons::Role::Lawyer,
        ViewerRole::Clerk => store::persons::Role::Clerk,
        ViewerRole::Client => store::persons::Role::Client,
    };
    let viewer = store::access::matter_viewer(&surreal, person_id, store_role, project.id)
        .await
        .map_err(crate::lawyer_project_detail::server_error)?;
    if !viewer.is_some_and(store::access::MatterViewer::is_firm_side) {
        return Err(hidden_matter());
    }

    let pg = store::postgres::PostgresDb::from_env().await.map_err(|_| {
        crate::lawyer_project_detail::server_error("the stage store is unavailable")
    })?;

    read_stage(&surreal, &pg, &project).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    use std::collections::HashMap;
    use store::case_stages::{StageDefinition, StageEvent};
    use uuid::Uuid;

    fn a_definition(id: Uuid, label: &str, position: i32) -> StageDefinition {
        StageDefinition {
            id,
            firm_id: Uuid::nil(),
            label: label.to_string(),
            position,
            case_type: None,
            default_odds: None,
            inserted_at: DateTime::default(),
            updated_at: DateTime::default(),
        }
    }

    fn an_event(
        from: Option<Uuid>,
        to: Uuid,
        actor: Uuid,
        at: chrono::DateTime<chrono::Utc>,
    ) -> StageEvent {
        StageEvent {
            id: Uuid::nil(),
            project_id: Uuid::nil(),
            from_stage_id: from,
            to_stage_id: to,
            actor_person_id: actor,
            note: None,
            occurred_at: at,
        }
    }

    #[test]
    fn vocabulary_rows_carry_the_firm_authored_words() {
        let discovery = Uuid::nil();
        let rows = super::definition_rows(&[a_definition(discovery, "Discovery", 3)]);

        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0],
            StageDefinitionRow {
                id: discovery.to_string(),
                label: "Discovery".into(),
                position: 3,
                case_type: None,
            }
        );
    }

    #[test]
    fn history_rows_resolve_stages_and_actors_to_display_words() {
        let intake = Uuid::nil();
        let discovery = Uuid::max();
        let actor = Uuid::now_v7();
        let events = [
            an_event(
                None,
                intake,
                actor,
                DateTime::from_timestamp(0, 0).expect("invariant: fixed instant"),
            ),
            an_event(
                Some(intake),
                discovery,
                actor,
                DateTime::from_timestamp(60, 0).expect("invariant: fixed instant"),
            ),
        ];
        let labels = HashMap::from([
            (intake, "Intake".to_string()),
            (discovery, "Discovery".to_string()),
        ]);
        let names = HashMap::from([(actor, "A. Rivera".to_string())]);

        let rows = super::event_rows(&events, &labels, &names);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].from_label, None, "the first event has no origin");
        assert_eq!(rows[0].to_label, "Intake");
        assert_eq!(rows[1].from_label.as_deref(), Some("Intake"));
        assert_eq!(rows[1].to_label, "Discovery");
        assert_eq!(rows[1].actor.as_deref(), Some("A. Rivera"));
        assert_eq!(rows[1].occurred_at, "1970-01-01T00:01:00+00:00");
    }

    #[test]
    fn an_actor_absent_from_the_directory_carries_no_name() {
        let stage = Uuid::nil();
        let events = [an_event(None, stage, Uuid::max(), DateTime::default())];

        let rows: Vec<super::StageEventRow> =
            super::event_rows(&events, &HashMap::new(), &HashMap::new());

        assert_eq!(rows[0].to_label, "unknown stage");
        assert_eq!(rows[0].actor, None);
    }

    #[test]
    fn an_empty_matter_has_no_current_stage() {
        let view = super::MatterStageView::default();

        assert_eq!(view.definitions, Vec::<StageDefinitionRow>::new());
        assert_eq!(view.current, None);
        assert_eq!(view.history, Vec::new());
    }

    fn a_view() -> MatterStageView {
        MatterStageView {
            definitions: vec![
                StageDefinitionRow {
                    id: "00000000-0000-0000-0000-0000000000aa".to_string(),
                    label: "Intake".to_string(),
                    position: 1,
                    case_type: None,
                },
                StageDefinitionRow {
                    id: "00000000-0000-0000-0000-0000000000bb".to_string(),
                    label: "Discovery".to_string(),
                    position: 3,
                    case_type: None,
                },
            ],
            current: None,
            history: Vec::new(),
        }
    }

    #[test]
    fn the_header_chip_names_the_stage_or_owns_the_absence() {
        let placed = dioxus_ssr::render_element(rsx! {
            StageChip { current: Some("Discovery".to_string()) }
        });
        assert!(placed.contains("Stage:"), "{placed}");
        assert!(placed.contains(">Discovery</span>"), "{placed}");

        let unplaced = dioxus_ssr::render_element(rsx! { StageChip { current: None } });
        assert!(unplaced.contains("Stage: not set"), "{unplaced}");
    }

    #[test]
    fn an_unplaced_matter_with_a_vocabulary_offers_the_set_control() {
        let html = dioxus_ssr::render_element(rsx! {
            StageSection {
                code: "acme".to_string(),
                csrf: "token".to_string(),
                stage: a_view(),
            }
        });

        // The empty state explains itself and hands the reader the next
        // action — the set control, carried by the same section.
        assert!(html.contains("No stage set for this matter yet"), "{html}");
        assert!(
            html.contains(r#"action="/app/projects/acme/stages/set""#),
            "{html}"
        );
        assert!(html.contains("Intake"), "{html}");
        assert!(html.contains("Discovery"), "{html}");
        assert!(html.contains(r#"name="_csrf""#), "{html}");
        // The copy keeps a Case Stage apart from the two statuses it is not.
        assert!(html.contains("lifecycle status"), "{html}");
        assert!(html.contains("court case&#39;s status"), "{html}");
    }

    #[test]
    fn the_history_names_the_transitions_in_display_words() {
        let mut stage = a_view();
        stage.current = Some(StageDefinitionRow {
            id: "00000000-0000-0000-0000-0000000000bb".to_string(),
            label: "Discovery".to_string(),
            position: 3,
            case_type: None,
        });
        stage.history = vec![StageEventRow {
            from_label: Some("Intake".to_string()),
            to_label: "Discovery".to_string(),
            actor: Some("A. Rivera".to_string()),
            note: Some("Answers served".to_string()),
            occurred_at: "1970-01-01T00:01:00+00:00".to_string(),
        }];

        let html = dioxus_ssr::render_element(rsx! {
            StageSection {
                code: "acme".to_string(),
                csrf: "token".to_string(),
                stage,
            }
        });

        // The current stage reads as a sentence with the stage named, and
        // the transition is a display word trail, not an id log.
        assert!(html.contains("stage of the Firm&#39;s workflow"), "{html}");
        assert!(html.contains("Intake"), "{html}");
        assert!(html.contains("A. Rivera"), "{html}");
        assert!(html.contains("Answers served"), "{html}");
        assert!(html.contains("1970-01-01"), "{html}");
        assert!(!html.contains("unknown stage"), "{html}");
    }

    #[test]
    fn a_refused_stage_write_rides_back_as_the_error_flash() {
        let html = dioxus_ssr::render_element(rsx! {
            StageSection {
                code: "acme".to_string(),
                csrf: "token".to_string(),
                stage: a_view(),
                error: Some("That stage is not in the Firm's vocabulary.".to_string()),
            }
        });

        assert!(html.contains(r#"role="alert""#), "{html}");
        assert!(
            html.contains("That stage is not in the Firm&#39;s vocabulary."),
            "{html}"
        );
    }

    #[test]
    fn a_matter_without_a_vocabulary_says_so_instead_of_offering_a_control() {
        let html = dioxus_ssr::render_element(rsx! {
            StageSection {
                code: "acme".to_string(),
                csrf: "token".to_string(),
                stage: MatterStageView::default(),
            }
        });

        assert!(
            html.contains("The Firm has no stage vocabulary yet"),
            "{html}"
        );
        assert!(
            !html.contains("/stages/set"),
            "no control for a command the vocabulary cannot honour: {html}"
        );
    }
}
