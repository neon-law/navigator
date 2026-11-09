//! The plan-intake review queue: parsed plan-assignment notices held for an
//! attorney's prepare-and-confirm decision.
//!
//! The portal surface over `store::plan_intake`: each staged row presents the
//! parsed fields, the conflict check result, and the fee quote from the
//! firm's plan fee schedule — and stops. Acceptance is the attorney's
//! attestation through the accept door; decline records a reason. Every
//! action posts to an axum door in `portal` that re-checks authority
//! server-side, exactly like the triage queue.

use std::fmt::Write as _;

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{AppLogo, Choice, Field, FormCard, Heading};
use crate::people::ViewerRole;

/// The firm-side plan-intake review queue.
pub const PLAN_INTAKE_PATH: &str = "/app/plan-intake";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRowView {
    pub staged_id: String,
    pub member_name: Option<String>,
    #[serde(default)]
    pub member_email: Option<String>,
    pub provider: String,
    #[serde(default)]
    pub member_number: Option<String>,
    #[serde(default)]
    pub plan_number: Option<String>,
    pub matter_type: String,
    #[serde(default)]
    pub state: Option<String>,
    /// The matter-code prefill reduced from the member number; the
    /// attorney owns the final code. Empty when the notice printed no
    /// member number to reduce.
    pub suggested_code: String,
    /// When the assignment email landed — the acceptance clock's origin,
    /// shown so the attorney sees how old the notice is.
    pub assigned_at: String,
    /// The resolved plan's name, or the unresolved-plan prose.
    pub plan: String,
    pub plan_portal_only: bool,
    /// The rendered fee quote, or the unpriced prose.
    pub fee: String,
    /// The rendered conflict-check line.
    pub conflict: String,
    #[serde(default)]
    pub conflict_findings: Vec<String>,
    pub conflict_blocking: bool,
    pub accept_action: String,
    pub decline_action: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanIntakePage {
    pub role: ViewerRole,
    pub title: String,
    pub rows: Vec<PlanRowView>,
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

/// The queue's rows for this viewer: staged records from every firm the
/// actor is a member of, newest landing first — the same firm scoping the
/// triage queue applies.
#[server]
pub async fn plan_intake_page() -> Result<PlanIntakePage, ServerFnError> {
    let role = crate::admin_listing::require_firm_person().await?;
    let surreal = consume_context::<store::surreal::SurrealDb>();
    let actor = crate::inbox::person_id()
        .await
        .ok_or_else(|| ServerFnError::new("firm access required"))?;
    let firms = store::firms::memberships_for_person(&surreal, actor)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?;
    let mut rows = Vec::new();
    for membership in firms {
        let staged_list = store::plan_intake::list_by_firm(&surreal, membership.firm_id)
            .await
            .map_err(|error| ServerFnError::new(error.to_string()))?;
        for staged in staged_list {
            if staged.status != store::plan_intake::Status::Staged.as_str() {
                continue;
            }
            rows.push(row_view(staged, actor));
        }
    }
    let (logo, tokens_href, firm_name) = crate::inbox::chrome().await;
    Ok(PlanIntakePage {
        role,
        title: "Plan intake".into(),
        rows,
        error: crate::inbox::query_error().await,
        csrf_token: crate::inbox::csrf_token().await,
        logo,
        tokens_href,
        firm_name,
    })
}

/// The fee-quote line for one row: the plan fee plus the supplemental
/// terms, the schedule's effective date, and the covered benefit's name.
fn fee_quote(staged: &store::plan_intake::StagedAssignment) -> String {
    let Some(plan_fee) = staged.fee_plan_fee_cents else {
        return "Not priced on the schedule yet — confirm the state and matter type on acceptance."
            .to_string();
    };
    let mut fee = format!("Plan fee ${}.{:02}", plan_fee / 100, plan_fee % 100);
    if let Some(hourly) = staged.fee_supplemental_hourly_cents {
        let _ = write!(
            fee,
            " · supplemental ${}.{:02}/hr",
            hourly / 100,
            hourly % 100
        );
    }
    if let Some(flat) = staged.fee_supplemental_flat_cents {
        let _ = write!(
            fee,
            " · supplemental ${}.{:02} flat",
            flat / 100,
            flat % 100
        );
    }
    let effective_on = staged
        .fee_effective_on
        .map_or_else(|| "unknown date".to_string(), |date| date.to_string());
    let _ = write!(
        fee,
        " · effective {} · {}",
        effective_on,
        staged
            .fee_covered_benefit
            .as_deref()
            .unwrap_or("covered benefit unnamed")
    );
    fee
}

/// Map one staged record to its row view. The actor id rides along so the
/// accept door can attribute the decision.
fn row_view(staged: store::plan_intake::StagedAssignment, _actor: uuid::Uuid) -> PlanRowView {
    let plan = staged
        .legal_plan_name
        .clone()
        .unwrap_or_else(|| "No matching plan — pick the plan on acceptance".to_string());
    let fee = fee_quote(&staged);
    let conflict = if !staged.conflict_checked {
        "Conflict check has not run — the notice printed no contact to anchor.".to_string()
    } else if staged.conflict_blocking {
        "Blocking conflict finding — the matter cannot open.".to_string()
    } else {
        "Conflict check clear.".to_string()
    };
    let member_number = staged.plan_member_id.clone();
    let suggested_code = member_number
        .as_deref()
        .map(store::plan_intake::suggest_code)
        .unwrap_or_default();
    let accept_action = format!("{PLAN_INTAKE_PATH}/{}/accept", staged.id);
    let decline_action = format!("{PLAN_INTAKE_PATH}/{}/decline", staged.id);
    PlanRowView {
        staged_id: staged.id.to_string(),
        member_name: staged.member_name,
        member_email: staged.member_email,
        provider: staged.provider,
        member_number: staged.plan_member_id,
        plan_number: staged.plan_number,
        matter_type: staged.plan_matter_type,
        state: staged.plan_state,
        suggested_code,
        assigned_at: staged.assigned_at.to_rfc3339(),
        plan,
        plan_portal_only: staged.plan_portal_only,
        fee,
        conflict,
        conflict_findings: staged.conflict_summary,
        conflict_blocking: staged.conflict_blocking,
        accept_action,
        decline_action,
    }
}

/// The closed plan matter-type vocabulary, as the accept form's choices.
fn matter_type_choices() -> Vec<Choice> {
    vec![
        Choice::new("document_review", "Document review"),
        Choice::new("letter", "Letter"),
        Choice::new("uncontested_divorce", "Uncontested divorce"),
        Choice::new("court_appearance", "Court appearance"),
    ]
}

/// One awaiting row: the parsed fields, the conflict result, the fee quote,
/// and the two decisions. Acceptance requires the attorney's attestation
/// checkbox; the doors re-check everything server-side.
fn plan_row(row: &PlanRowView, csrf: &str) -> Element {
    let matter_type_choices = matter_type_choices();
    let selected_matter_type = row.matter_type.clone();
    let state_value = row.state.clone().unwrap_or_default();
    let member_name = row.member_name.as_deref().unwrap_or("Unnamed member");
    rsx! {
        li { class: "plan-intake__row", id: "plan-row-{row.staged_id}",
            p { class: "plan-intake__member",
                strong { "{member_name}" }
                if let Some(email) = row.member_email.as_ref() {
                    span { class: "plan-intake__email", " · {email}" }
                }
                span { class: "plan-intake__context", " · via {row.provider}" }
            }
            {plan_fields(row)}
            p { class: "plan-intake__fee", "{row.fee}" }
            p { class: if row.conflict_blocking { "plan-intake__conflict plan-intake__conflict--blocking" } else { "plan-intake__conflict" },
                "{row.conflict}"
            }
            ul { class: "plan-intake__findings",
                {row.conflict_findings.iter().map(|finding| rsx! { li { "{finding}" } })}
            }
            FormCard {
                title: "Accept this assignment".to_string(),
                action: row.accept_action.clone(),
                submit_label: "Accept and open the matter".to_string(),
                heading: Heading::Hidden,
                csrf_token: Some(csrf.to_string()),
                fields: vec![
                    Field::checkbox(
                        "I have read the conflict result and accept this assignment as the responsible attorney.",
                        "attestation",
                        "yes",
                        false,
                    )
                    .id(format!("plan-{}-attestation", row.staged_id))
                    .required(),
                    Field::select(
                        "Matter type",
                        "plan_matter_type",
                        matter_type_choices,
                        Some(selected_matter_type),
                    )
                    .id(format!("plan-{}-matter-type", row.staged_id))
                    .required(),
                    Field::text("State", "plan_state", state_value)
                        .id(format!("plan-{}-state", row.staged_id)),
                    Field::text("Matter code", "code", row.suggested_code.clone())
                        .id(format!("plan-{}-code", row.staged_id))
                        .required(),
                    Field::text("Matter name", "name", "")
                        .id(format!("plan-{}-name", row.staged_id)),
                    Field::textarea("Questions for the member (one per line)", "questions", "", 3)
                        .id(format!("plan-{}-questions", row.staged_id)),
                ],
            }
            FormCard {
                title: "Decline this assignment".to_string(),
                action: row.decline_action.clone(),
                submit_label: "Decline".to_string(),
                heading: Heading::Hidden,
                csrf_token: Some(csrf.to_string()),
                fields: vec![
                    Field::textarea("Reason", "reason", "", 2)
                        .id(format!("plan-{}-decline-reason", row.staged_id))
                        .required(),
                ],
            }
        }
    }
}

/// The parsed-field definition list for one row, including the plan's
/// acceptance channel.
fn plan_fields(row: &PlanRowView) -> Element {
    let member_number = row.member_number.as_deref().unwrap_or("-");
    let plan_number = row.plan_number.as_deref().unwrap_or("-");
    let state = row.state.as_deref().unwrap_or("-");
    let acceptance = if row.plan_portal_only {
        "On the provider's portal — reply manually"
    } else {
        "By reply email"
    };
    rsx! {
        dl { class: "plan-intake__fields",
            div {
                dt { "Member number" }
                dd { "{member_number}" }
            }
            div {
                dt { "Plan" }
                dd { "{row.plan}" }
            }
            div {
                dt { "Plan number" }
                dd { "{plan_number}" }
            }
            div {
                dt { "Matter type" }
                dd { "{row.matter_type}" }
            }
            div {
                dt { "State" }
                dd { "{state}" }
            }
            div {
                dt { "Landed" }
                dd { "{row.assigned_at}" }
            }
            div {
                dt { "Acceptance on the plan" }
                dd { "{acceptance}" }
            }
        }
    }
}

/// The queue body: a heading, the shared error flash, and one card per
/// staged assignment — empty-state prose when nothing waits.
pub fn plan_intake_body(page: &PlanIntakePage) -> Element {
    let rows = page.rows.iter().map(|row| plan_row(row, &page.csrf_token));
    rsx! {
        document::Title { "{page.firm_name} | {page.title}" }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{page.tokens_href}" }
        crate::components::AppNavbar {
            destinations: crate::app_chrome::app_destinations(page.role),
            logo: page.logo.clone(),
        }
        main { id: "plan-intake", class: "nav-theme",
            h1 { "{page.title}" }
            if let Some(error) = page.error.as_ref() {
                p { class: "nav-error", "{error}" }
            }
            if page.rows.is_empty() {
                p { class: "nav-muted",
                    "No plan assignments are waiting for review. Parsed notices from the firm's legal plans land here."
                }
            } else {
                ul { class: "plan-intake", {rows} }
            }
        }
    }
}

#[component]
pub fn PlanIntake() -> Element {
    let resource = use_server_future(plan_intake_page)?;
    let page = match &*resource.read() {
        Some(Ok(page)) => page.clone(),
        Some(Err(_)) => {
            return rsx! {
                main { id: "plan-intake", p { "Failed to load the plan intake queue." } }
            };
        }
        None => return rsx! { main { id: "plan-intake", p { "Loading…" } } },
    };
    plan_intake_body(&page)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> PlanRowView {
        PlanRowView {
            staged_id: "019a0000-0000-7000-8000-000000000001".into(),
            member_name: Some("Dana Example".into()),
            member_email: Some("dana@example.com".into()),
            provider: "arag".into(),
            member_number: Some("AR-004512".into()),
            plan_number: Some("PLAN-778812".into()),
            matter_type: "document_review".into(),
            state: Some("nv".into()),
            suggested_code: "plan-ar-004512".into(),
            assigned_at: "2026-11-09T14:00:00+00:00".into(),
            plan: "ARAG".into(),
            plan_portal_only: false,
            fee: "Plan fee $150.00 · supplemental $250.00/hr · effective 2026-01-01 · Will review"
                .into(),
            conflict: "Conflict check clear.".into(),
            conflict_findings: vec![],
            conflict_blocking: false,
            accept_action: "/app/plan-intake/019a0000-0000-7000-8000-000000000001/accept".into(),
            decline_action: "/app/plan-intake/019a0000-0000-7000-8000-000000000001/decline".into(),
        }
    }

    fn page(rows: Vec<PlanRowView>) -> PlanIntakePage {
        PlanIntakePage {
            role: ViewerRole::Owner,
            title: "Plan intake".into(),
            rows,
            error: None,
            csrf_token: "csrf-token-1".into(),
            logo: None,
            tokens_href: "/tokens.css".into(),
            firm_name: "Neon Law".into(),
        }
    }

    #[test]
    fn a_staged_row_shows_the_fields_conflict_and_fee_before_either_decision() {
        let html = dioxus_ssr::render_element(plan_intake_body(&page(vec![row()])));
        crate::components::form::assert_forms_accessible(&html, "plan intake");
        for expected in [
            "Dana Example",
            "dana@example.com",
            "via arag",
            "AR-004512",
            "PLAN-778812",
            "Plan fee $150.00 · supplemental $250.00/hr",
            "Conflict check clear.",
            "Matter code",
            "value=\"plan-ar-004512\"",
            "Accept and open the matter",
            "/app/plan-intake/019a0000-0000-7000-8000-000000000001/accept",
            "/app/plan-intake/019a0000-0000-7000-8000-000000000001/decline",
        ] {
            assert!(html.contains(expected), "missing {expected}: {html}");
        }
    }

    #[test]
    fn an_unpriced_row_says_so_and_a_blocking_conflict_says_the_matter_cannot_open() {
        let mut unpriced = row();
        unpriced.fee =
            "Not priced on the schedule yet — confirm the state and matter type on acceptance."
                .into();
        unpriced.conflict = "Blocking conflict finding — the matter cannot open.".into();
        unpriced.conflict_blocking = true;
        unpriced.conflict_findings = vec!["Dana Example matches an open adverse party.".into()];
        let html = dioxus_ssr::render_element(plan_intake_body(&page(vec![unpriced])));
        assert!(html.contains("Not priced on the schedule yet"), "{html}");
        assert!(html.contains("the matter cannot open"), "{html}");
        assert!(
            html.contains("Dana Example matches an open adverse party."),
            "{html}"
        );
    }

    #[test]
    fn a_portal_only_plan_says_the_reply_is_manual() {
        let mut portal = row();
        portal.plan_portal_only = true;
        let html = dioxus_ssr::render_element(plan_intake_body(&page(vec![portal])));
        assert!(html.contains("reply manually"), "{html}");
    }

    #[test]
    fn the_empty_queue_says_so() {
        let html = dioxus_ssr::render_element(plan_intake_body(&page(Vec::new())));
        assert!(
            html.contains("No plan assignments are waiting for review"),
            "{html}"
        );
        assert!(!html.contains("Accept and open the matter"), "{html}");
    }
}
