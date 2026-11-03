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
    let definitions = match project.firm_id {
        Some(firm_id) => store::case_stages::list_definitions(&pg, firm_id)
            .await
            .map_err(crate::lawyer_project_detail::server_error)?,
        None => Vec::new(),
    };
    let current = store::case_stages::current_stage(&pg, project.id)
        .await
        .map_err(crate::lawyer_project_detail::server_error)?;
    let history = store::case_stages::history(&pg, project.id)
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
        store::persons::find_by_ids(&surreal, &actor_ids)
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

#[cfg(test)]
mod tests {
    use super::StageDefinitionRow;
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
}
