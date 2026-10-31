//! The triage human-review queue.
//!
//! The portal surface over `store::inbox_triage`: triage rows still waiting
//! for a person, with the classifier's suggestion, confidence, and rationale
//! shown, and one-click confirm / override / resolve / re-run actions. The
//! pages follow the inbox list's Dioxus pattern; every action posts to an
//! axum door in `portal` that re-checks authority server-side.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{AppLogo, Choice, Field, FormCard, Heading};
use crate::people::ViewerRole;

/// The firm-side review queue.
pub const TRIAGE_QUEUE_PATH: &str = "/app/inbox/triage";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriageRowView {
    pub triage_id: String,
    /// The conversation the triaged message landed in.
    pub conversation_href: String,
    pub sender: String,
    pub channel: String,
    pub brand: String,
    pub privilege: String,
    #[serde(default)]
    pub privilege_confidence: Option<String>,
    pub disposition: String,
    /// The classifier's suggested Project, when one still resolves.
    #[serde(default)]
    pub suggested_project: Option<String>,
    #[serde(default)]
    pub project_confidence: Option<String>,
    /// The model's audit rationale, shown so a person can judge it.
    #[serde(default)]
    pub rationale: Option<String>,
    /// This row's Firm's projects — the override form's choices.
    #[serde(default)]
    pub projects: Vec<ProjectChoiceView>,
    /// Confirm posts to a path carrying the suggested project's id; absent
    /// when the row names no suggestion.
    #[serde(default)]
    pub confirm_action: Option<String>,
    pub override_action: String,
    pub resolve_action: String,
    pub rerun_action: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectChoiceView {
    pub id: String,
    pub name: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriageQueuePage {
    pub role: ViewerRole,
    pub title: String,
    pub rows: Vec<TriageRowView>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub csrf_token: String,
    #[serde(default)]
    pub logo: Option<AppLogo>,
    #[serde(default)]
    pub tokens_href: String,
    #[serde(default)]
    pub firm_name: String,
}

/// The queue's rows for this viewer, firm-scoped exactly as the store
/// decides. A Client never reaches the route (the Rego layer denies it) and
/// the store denies them again here.
#[cfg(feature = "server")]
#[server]
pub async fn triage_queue_page() -> Result<TriageQueuePage, ServerFnError> {
    let role = crate::admin_listing::require_firm_person().await?;
    let surreal = consume_context::<store::surreal::SurrealDb>();
    let actor = crate::inbox::person_id()
        .await
        .ok_or_else(|| ServerFnError::new("firm access required"))?;
    let rows = store::inbox_triage::list_for_review(&surreal, actor)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?;
    let mut views = Vec::with_capacity(rows.len());
    for row in rows {
        // Per-row project choices come from the row's Firm — an Owner sees
        // rows from every Firm, and each override form offers only that
        // Firm's projects.
        let projects = store::inbox::projects_in_firm(&surreal, row.triage.firm_id)
            .await
            .map_err(|error| ServerFnError::new(error.to_string()))?
            .into_iter()
            .map(|project| ProjectChoiceView {
                id: project.id.to_string(),
                name: project.name,
            })
            .collect();
        let href = format!("/app/inbox/{}", row.conversation_id);
        views.push(TriageRowView {
            triage_id: row.triage.id.to_string(),
            conversation_href: href.clone(),
            sender: row.sender,
            channel: row.channel,
            brand: row.brand_key.unwrap_or_else(|| "unbranded".into()),
            privilege: row.triage.privilege.as_str().replace('_', " "),
            privilege_confidence: row
                .triage
                .privilege_confidence
                .map(|confidence| format!("{confidence:.2}")),
            disposition: row.triage.disposition.as_str().replace('_', " "),
            suggested_project: row.suggested_project_name,
            project_confidence: row
                .triage
                .project_confidence
                .map(|confidence| format!("{confidence:.2}")),
            rationale: row.triage.rationale,
            projects,
            confirm_action: row.triage.suggested_project_id.map(|project_id| {
                // The id rides the path — the same shape the assign form's
                // action uses — so the form itself needs no hidden field.
                format!(
                    "{href}/triage/{}/confirm/{}",
                    row.triage.id, project_id
                )
            }),
            override_action: format!("{href}/triage/{}/override", row.triage.id),
            resolve_action: format!("{href}/triage/{}/resolve", row.triage.id),
            rerun_action: format!("{href}/triage/{}/rerun", row.triage.id),
        });
    }
    let (logo, tokens_href, firm_name) = crate::inbox::chrome().await;
    Ok(TriageQueuePage {
        role,
        title: "Triage review".into(),
        rows: views,
        error: crate::inbox::query_error().await,
        csrf_token: crate::inbox::csrf_token().await,
        logo,
        tokens_href,
        firm_name,
    })
}

#[component]
pub fn TriageQueue() -> Element {
    let resource = use_server_future(triage_queue_page)?;
    let page = match &*resource.read() {
        Some(Ok(page)) => page.clone(),
        Some(Err(_)) => {
            return rsx! { main { id: "triage", p { "Failed to load the triage review queue." } } };
        }
        None => return rsx! { main { id: "triage", p { "Loading…" } } },
    };
    triage_queue_body(&page)
}

/// One awaiting triage row and its four doors. Confirm is offered only when
/// a suggestion still resolves; the override form carries the row's Firm's
/// projects and requires a rationale; the resolve form settles an uncertain
/// privilege label; re-run hands the row back to the rule layer.
fn triage_row(row: &TriageRowView, csrf: &str) -> Element {
    let project_confidence = row
        .project_confidence
        .clone()
        .map(|confidence| format!(" · confidence {confidence}"))
        .unwrap_or_default();
    let confirm = row.confirm_action.clone().map(|action| {
        let project = row.suggested_project.clone().unwrap_or_default();
        rsx! {
            FormCard {
                title: "Confirm the suggestion".to_string(),
                action,
                submit_label: format!("Assign to {project}"),
                heading: Heading::Hidden,
                csrf_token: Some(csrf.to_string()),
                fields: vec![],
            }
        }
    });
    let override_choices = row
        .projects
        .iter()
        .map(|project| Choice::new(project.id.clone(), project.name.clone()))
        .collect();
    let rationale_label = "Why this project instead";
    rsx! {
        li { class: "triage-review__row", id: "triage-row-{row.triage_id}",
            p { class: "triage-review__sender",
                a { href: "{row.conversation_href}", "{row.sender}" }
                span { class: "triage-review__context",
                    " · {row.channel} · {row.brand} · {row.privilege}"
                }
                if let Some(confidence) = row.privilege_confidence.as_ref() {
                    span { class: "triage-review__confidence", " · confidence {confidence}" }
                }
            }
            p { class: "triage-review__disposition", "{row.disposition}" }
            if let Some(project) = row.suggested_project.as_ref() {
                p { class: "triage-review__suggestion",
                    "Suggested project: {project}{project_confidence}"
                }
            }
            if let Some(rationale) = row.rationale.as_ref() {
                p { class: "triage-review__rationale nav-muted", "{rationale}" }
            }
            {confirm}
            FormCard {
                title: "Assign a different project".to_string(),
                action: row.override_action.clone(),
                submit_label: "Override".to_string(),
                heading: Heading::Hidden,
                csrf_token: Some(csrf.to_string()),
                fields: vec![
                    Field::select("Project", "project_id", override_choices, None).required(),
                    Field::textarea(rationale_label, "rationale", "", 2).required(),
                ],
            }
            FormCard {
                title: "Settle the privilege label".to_string(),
                action: row.resolve_action.clone(),
                submit_label: "Resolve".to_string(),
                heading: Heading::Hidden,
                csrf_token: Some(csrf.to_string()),
                fields: vec![
                    Field::select(
                        "Privilege",
                        "privilege",
                        vec![
                            Choice::new("privileged", "Privileged"),
                            Choice::new("not_privileged", "Not privileged"),
                        ],
                        None,
                    )
                    .required(),
                    Field::textarea("Reason", "rationale", "", 2).required(),
                ],
            }
            FormCard {
                title: "Re-run the rule layer".to_string(),
                action: row.rerun_action.clone(),
                submit_label: "Re-run triage".to_string(),
                heading: Heading::Hidden,
                csrf_token: Some(csrf.to_string()),
                fields: vec![],
            }
        }
    }
}

/// The queue body: a heading, the shared error flash, and one card list per
/// awaiting row — empty-state prose when the queue is clear.
pub fn triage_queue_body(page: &TriageQueuePage) -> Element {
    if page.rows.is_empty() {
        return rsx! {
            main { id: "triage",
                h1 { "{page.title}" }
                if let Some(error) = page.error.as_ref() {
                    p { class: "nav-error", "{error}" }
                }
                p { class: "nav-muted",
                    "No messages are waiting for review. Triage sends uncertain or unassigned messages here."
                }
            }
        };
    }
    let rows = page
        .rows
        .iter()
        .map(|row| triage_row(row, &page.csrf_token));
    rsx! {
        main { id: "triage",
            h1 { "{page.title}" }
            if let Some(error) = page.error.as_ref() {
                p { class: "nav-error", "{error}" }
            }
            ul { class: "triage-review", {rows} }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> TriageRowView {
        TriageRowView {
            triage_id: "019a0000-0000-7000-8000-000000000001".into(),
            conversation_href: "/app/inbox/019a0000-0000-7000-8000-0000000000aa".into(),
            sender: "Marta Rivera".into(),
            channel: "email".into(),
            brand: "neon".into(),
            privilege: "uncertain".into(),
            privilege_confidence: None,
            disposition: "needs human".into(),
            suggested_project: Some("Rivera Estate".into()),
            project_confidence: Some("0.82".into()),
            rationale: Some("The sender references an ongoing trust.".into()),
            projects: vec![ProjectChoiceView {
                id: "019a0000-0000-7000-8000-0000000000bb".into(),
                name: "Rivera Estate".into(),
            }],
            confirm_action: Some(
                "/app/inbox/019a0000-0000-7000-8000-0000000000aa/triage/019a0000-0000-7000-8000-000000000001/confirm/019a0000-0000-7000-8000-0000000000bb"
                    .into(),
            ),
            override_action: "/app/inbox/019a0000-0000-7000-8000-0000000000aa/triage/019a0000-0000-7000-8000-000000000001/override".into(),
            resolve_action: "/app/inbox/019a0000-0000-7000-8000-0000000000aa/triage/019a0000-0000-7000-8000-000000000001/resolve".into(),
            rerun_action: "/app/inbox/019a0000-0000-7000-8000-0000000000aa/triage/019a0000-0000-7000-8000-000000000001/rerun".into(),
        }
    }

    fn page(rows: Vec<TriageRowView>) -> TriageQueuePage {
        TriageQueuePage {
            role: ViewerRole::Owner,
            title: "Triage review".into(),
            rows,
            error: None,
            csrf_token: "csrf-token-1".into(),
            logo: None,
            tokens_href: "/tokens.css".into(),
            firm_name: "Neon Law".into(),
        }
    }

    #[test]
    fn an_awaiting_row_shows_its_context_suggestion_and_doors() {
        let html = dioxus_ssr::render_element(triage_queue_body(&page(vec![row()])));
        for expected in [
            "Marta Rivera",
            "neon",
            "uncertain",
            "Suggested project: Rivera Estate · confidence 0.82",
            "The sender references an ongoing trust.",
            "/triage/019a0000-0000-7000-8000-000000000001/confirm/019a0000-0000-7000-8000-0000000000bb",
            "/triage/019a0000-0000-7000-8000-000000000001/override",
            "/triage/019a0000-0000-7000-8000-000000000001/resolve",
            "/triage/019a0000-0000-7000-8000-000000000001/rerun",
        ] {
            assert!(html.contains(expected), "missing {expected}: {html}");
        }
    }

    #[test]
    fn a_row_without_a_suggestion_offers_no_confirm() {
        let mut view = row();
        view.suggested_project = None;
        view.confirm_action = None;
        let html = dioxus_ssr::render_element(triage_queue_body(&page(vec![view])));
        assert!(!html.contains("Confirm the suggestion"), "{html}");
        assert!(html.contains("Assign a different project"), "{html}");
    }

    #[test]
    fn the_empty_queue_says_so() {
        let html = dioxus_ssr::render_element(triage_queue_body(&page(Vec::new())));
        assert!(html.contains("No messages are waiting for review"), "{html}");
        assert!(!html.contains("Assign a different project"), "{html}");
    }
}
