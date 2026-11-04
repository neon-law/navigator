//! Native-form write doors for the matter dashboard's Case Stage surface —
//! the browser half of the stage command, beside the JSON API in
//! [`crate::project_stages_api`].
//!
//! The dashboard is a native-form surface: the set-stage control POSTs here
//! and the handler redirects (303) back to the matter's Overview, with an
//! `?error=` flash when the command is refused. The authorization, mirroring,
//! and store command are the JSON door's own — both doors call the same
//! `prepared_store`, so the two entry points cannot drift. The flash copy
//! refuses in the user's vocabulary — never a store variant name, and never
//! a word that could read as the matter's lifecycle status.
use axum::{
    extract::{Form, Path, State},
    response::{IntoResponse, Redirect, Response},
    Extension,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::admin::{encode_query_value, not_found_response};
use crate::session::SessionData;

/// Back to the Overview tab the control lives on, success or refusal alike —
/// the flash parameter carries the outcome.
fn back_to_overview(code: &str, flash: Option<&str>) -> Response {
    let mut url = webapp::sections::page::MatterTab::Overview.href(code);
    if let Some(flash) = flash {
        url.push_str("?error=");
        url.push_str(&encode_query_value(flash));
    }
    Redirect::to(&url).into_response()
}

/// The set-stage control on the matter header: the stage the matter has
/// moved to, and an optional note. The CSRF token travels with the form but
/// is consumed by the session middleware before this handler sees it, so it
/// is not a field here.
#[derive(Deserialize)]
pub(crate) struct StageSetForm {
    #[serde(default)]
    pub(crate) stage_id: String,
    #[serde(default)]
    pub(crate) note: String,
}

/// `POST /app/projects/{code}/stages/set` — place the matter in a stage from
/// the Firm's own vocabulary. The store records nothing when the matter
/// already holds the stage.
pub(crate) async fn set(
    State(surreal): State<store::surreal::SurrealDb>,
    session: Option<Extension<SessionData>>,
    Path(code): Path<String>,
    Form(input): Form<StageSetForm>,
) -> Response {
    let Some(session) = session else {
        return not_found_response();
    };
    if session.viewing_as_dri.is_some() {
        return not_found_response();
    }
    let Some(person_id) = session.person_id else {
        return not_found_response();
    };
    let Some(project) = store::projects::find_by_code(&surreal, &code)
        .await
        .ok()
        .flatten()
    else {
        return not_found_response();
    };
    if !crate::admin::is_lawyer_tier(Some(&session)) {
        return not_found_response();
    }
    // The JSON door's own admission, verbatim: a firm-side viewer of this
    // matter or the non-disclosing 404.
    let viewer =
        store::access::matter_viewer(&surreal, session.person_id, session.role, project.id)
            .await
            .ok()
            .flatten();
    match viewer {
        Some(viewer) if viewer.is_firm_side() => {}
        _ => return not_found_response(),
    }

    let Ok(stage_id) = Uuid::parse_str(input.stage_id.trim()) else {
        return back_to_overview(&code, Some("Choose the stage the matter has moved to."));
    };
    let note = input.note.trim().to_string();
    let Ok(pg) = crate::project_stages_api::prepared_store(&surreal, "set", project.id).await
    else {
        return back_to_overview(
            &code,
            Some("The stage record is unavailable right now — try again shortly."),
        );
    };
    match store::case_stages::set_stage(
        &pg,
        project.id,
        person_id,
        stage_id,
        (!note.is_empty()).then_some(note),
    )
    .await
    {
        Ok(_) => back_to_overview(&code, None),
        Err(e) => back_to_overview(&code, Some(&flash_for(&e))),
    }
}

/// The store's typed refusals, rendered as flash copy in the user's
/// vocabulary — never a variant name, and never a word that could read as
/// the matter's lifecycle status.
fn flash_for(error: &store::case_stages::CaseStageError) -> String {
    use store::case_stages::CaseStageError;
    match error {
        CaseStageError::NotFound => "That stage is not in the Firm's vocabulary.".to_string(),
        CaseStageError::NotInFirmVocabulary => {
            "That stage is not in the Firm's vocabulary.".to_string()
        }
        CaseStageError::ProjectHasNoFirm => {
            "This matter has no Firm yet, so no stage vocabulary applies.".to_string()
        }
        CaseStageError::Invalid("note") => {
            "Keep the stage note within 500 bytes, or leave it empty.".to_string()
        }
        CaseStageError::Invalid(_) => {
            "Check the stage fields and submit the form again.".to_string()
        }
        CaseStageError::Database(_) | CaseStageError::Postgres(_) => {
            "The stage record is unavailable right now — try again shortly.".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_invalid_stage_note_explains_how_to_correct_the_input() {
        let message = super::flash_for(&store::case_stages::CaseStageError::Invalid("note"));
        assert!(message.contains("500 bytes"), "{message}");
        assert!(!message.contains("unavailable"), "{message}");
    }
}
