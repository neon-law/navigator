//! The plan-intake review queue: parsed plan-assignment notices held for an
//! attorney's prepare-and-confirm decision.
//!
//! The portal surface over `store::plan_intake`: each staged row presents the
//! parsed fields, the conflict check result, and the fee quote from the
//! firm's plan fee schedule — and stops. Acceptance is the attorney's
//! attestation through the accept door; decline records a reason. Every
//! action posts to an axum door in `portal` that re-checks authority
//! server-side, exactly like the triage queue.

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
    pub plan_case_number: Option<String>,
    pub notice_matter_type: Option<String>,
    review: ReferralReview,
    pub resolve_action: String,
    pub notice_href: String,
    pub matter_type: String,
    #[serde(default)]
    pub state: Option<String>,
    /// The suggested code, confirmed or edited by the lawyer.
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

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum ReferralReview {
    ConfirmMember,
    Take,
    Hold,
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

/// The lawyer's plan queue for the Firm wearing the current Brand.
#[server]
pub async fn plan_intake_page() -> Result<PlanIntakePage, ServerFnError> {
    let role = crate::admin_listing::require_lawyer().await?;
    let application_store = consume_context::<store::application_store::ApplicationStore>();
    let actor = crate::inbox::person_id()
        .await
        .ok_or_else(|| ServerFnError::new("firm access required"))?;
    let brand = views::brand::brand_key();
    let firm = store::plan_intake::review_firm(&application_store, actor, brand.as_str())
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?;
    let pg = store::postgres::PostgresDb::from_env()
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?;
    let plans = store::legal_plans::list(&pg, firm)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?;
    let mut rows = Vec::new();
    for row in store::plan_intake::list_by_firm(&application_store, firm)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?
    {
        if row.status == store::plan_intake::Status::Staged.as_str() {
            let plan = store::plan_intake::plan_for_provider(&plans, &row.provider);
            let pending_id = row.pending_project_id();
            let mut view = row_view(row, plan);
            if let Some(id) = pending_id {
                let roles = store::projects::participations_for_project(&application_store, id)
                    .await
                    .map_err(|error| ServerFnError::new(error.to_string()))?;
                if roles
                    .iter()
                    .any(|role| role.is_lawyer_dri && role.person_id == actor)
                {
                    if let Some(project) = store::projects::find_by_id(&application_store, id)
                        .await
                        .map_err(|error| ServerFnError::new(error.to_string()))?
                    {
                        view.suggested_code = project.code;
                    }
                }
            }
            rows.push(view);
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
/// Server-only: the wasm client build has no `store`, and this helper only
/// runs inside `#[server]` bodies.
#[cfg(feature = "server")]
fn fee_quote(staged: &store::plan_intake::StagedAssignment) -> String {
    use std::fmt::Write as _;

    let Some(plan_fee) = staged.fee_plan_fee_cents else {
        return "Coverage and fees are unverified.".to_string();
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

#[cfg(feature = "server")]
fn row_view(
    staged: store::plan_intake::StagedAssignment,
    plan: Option<&store::legal_plans::LegalPlan>,
) -> PlanRowView {
    let plan_name = plan.map_or_else(
        || {
            "Configure one matching Legal Plan for this Firm before taking this assignment."
                .to_owned()
        },
        |plan| plan.name.clone(),
    );
    let matter_type = staged
        .notice_matter_type
        .as_deref()
        .and_then(store::plan_intake::matter_type_from_notice)
        .map_or("", store::legal_plans::PlanMatterType::as_str);
    let fee = if !matter_type.is_empty()
        && matter_type == staged.plan_matter_type
        && plan.is_some_and(|plan| Some(plan.id) == staged.legal_plan_id)
    {
        fee_quote(&staged)
    } else {
        "Coverage and fees are unverified.".to_owned()
    };
    let member_resolved = staged.member_person_id.is_some()
        && staged.member_entity_id.is_some()
        && staged.conflict_checked;
    let conflict = if !member_resolved {
        "Confirm the member to run the conflict check.".to_owned()
    } else if staged.conflict_blocking {
        "Blocking conflict finding. The matter cannot open.".to_owned()
    } else if staged.conflict_summary.is_empty() {
        "Conflict check clear.".to_owned()
    } else {
        "Review the conflict findings before taking this assignment.".to_owned()
    };
    let suggested_code = store::plan_intake::suggest_code(
        staged.member_name.as_deref().unwrap_or_default(),
        staged.notice_matter_type.as_deref(),
        staged.plan_state.as_deref(),
    );
    let id = staged.id;
    PlanRowView {
        staged_id: id.to_string(),
        member_name: staged.member_name,
        member_email: staged.member_email,
        provider: staged.provider,
        member_number: staged.plan_member_id,
        plan_number: staged.plan_number,
        plan_case_number: staged.plan_case_number,
        notice_matter_type: staged.notice_matter_type,
        matter_type: matter_type.to_owned(),
        state: staged.plan_state,
        suggested_code,
        assigned_at: staged.assigned_at.to_rfc3339(),
        plan: plan_name,
        plan_portal_only: plan.is_some_and(|plan| plan.portal.is_some()),
        review: if !member_resolved {
            ReferralReview::ConfirmMember
        } else if plan.is_none() || staged.conflict_blocking {
            ReferralReview::Hold
        } else {
            ReferralReview::Take
        },
        fee,
        conflict,
        conflict_findings: staged.conflict_summary,
        conflict_blocking: staged.conflict_blocking,
        accept_action: format!("{PLAN_INTAKE_PATH}/{id}/accept"),
        resolve_action: format!("{PLAN_INTAKE_PATH}/{id}/resolve-member"),
        notice_href: format!("/app/inbox/{}", staged.conversation_id),
        decline_action: format!("{PLAN_INTAKE_PATH}/{id}/decline"),
    }
}

/// The closed plan matter-type vocabulary, as the accept form's choices.
fn matter_type_choices() -> Vec<Choice> {
    vec![
        Choice::new("", "Unclassified"),
        Choice::new("document_review", "Document review"),
        Choice::new("letter", "Letter"),
        Choice::new("uncontested_divorce", "Uncontested divorce"),
        Choice::new("court_appearance", "Court appearance"),
        Choice::new("contested_divorce", "Contested divorce"),
        Choice::new("custody", "Custody"),
        Choice::new("probate", "Probate"),
    ]
}

/// One awaiting row: the parsed fields, the conflict result, the fee quote,
/// and the two decisions. Acceptance requires the attorney's attestation
/// checkbox; the doors re-check everything server-side.
fn plan_row(row: &PlanRowView, csrf: &str) -> Element {
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
            a { href: row.notice_href.clone(), "Open original notice" }
            {plan_fields(row)}
            p { class: "plan-intake__fee", "{row.fee}" }
            p { class: if row.conflict_blocking { "plan-intake__conflict plan-intake__conflict--blocking" } else { "plan-intake__conflict" },
                "{row.conflict}"
            }
            ul { class: "plan-intake__findings",
                {row.conflict_findings.iter().map(|finding| rsx! { li { "{finding}" } })}
            }
            if row.review == ReferralReview::ConfirmMember {
                FormCard {
                    title: "Confirm the member".to_string(),
                    action: row.resolve_action.clone(),
                    submit_label: "Confirm member and check conflicts".to_string(),
                    heading: Heading::Hidden,
                    csrf_token: Some(csrf.to_string()),
                    fields: member_fields(row),
                }
            } else if row.review == ReferralReview::Take {
                p { "Taking this assignment opens a Pitch. Any correspondence is reviewed and sent separately by the lawyer." }
                FormCard {
                    title: "Take this assignment".to_string(),
                    action: row.accept_action.clone(),
                    submit_label: "Take assignment and open Pitch".to_string(),
                    heading: Heading::Hidden,
                    csrf_token: Some(csrf.to_string()),
                    fields: take_fields(row),
                }
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

fn member_fields(row: &PlanRowView) -> Vec<Field> {
    let id = &row.staged_id;
    let mut fields = Vec::new();
    if row.member_name.is_none() {
        fields.push(
            Field::text("Member name", "member_name", "")
                .id(format!("plan-{id}-member-name"))
                .required(),
        );
    }
    fields.push(
        Field::email(
            "Verified member email",
            "member_email",
            row.member_email.clone().unwrap_or_default(),
        )
        .id(format!("plan-{id}-member-email"))
        .required(),
    );
    fields
}

fn take_fields(row: &PlanRowView) -> Vec<Field> {
    let id = &row.staged_id;
    let mut fields = vec![
        Field::checkbox(
            "I have reviewed the conflict findings and take this assignment as the responsible lawyer.",
            "attestation", "yes", false,
        ).id(format!("plan-{id}-attestation")).required(),
        Field::select(
            "Plan matter type", "plan_matter_type", matter_type_choices(), Some(row.matter_type.clone()),
        ).id(format!("plan-{id}-matter-type")),
        Field::text("State", "plan_state", row.state.clone().unwrap_or_default()).id(format!("plan-{id}-state")),
        Field::text("Project code", "code", row.suggested_code.clone()).id(format!("plan-{id}-code")).required(),
        Field::text("Project name", "name", "").id(format!("plan-{id}-name")),
    ];
    if row.plan_case_number.is_none() {
        fields.insert(
            0,
            Field::text(
                "Provider case number from the original notice",
                "plan_case_number",
                "",
            )
            .id(format!("plan-{id}-case-number"))
            .required(),
        );
    }
    fields
}

/// The parsed-field definition list for one row, including the plan's
/// acceptance channel.
fn plan_fields(row: &PlanRowView) -> Element {
    let member_number = row.member_number.as_deref().unwrap_or("-");
    let plan_number = row.plan_number.as_deref().unwrap_or("-");
    let case_number = row
        .plan_case_number
        .as_deref()
        .unwrap_or("Needs verification");
    let work = row.notice_matter_type.as_deref().unwrap_or("-");
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
                dt { "Provider case number" }
                dd { "{case_number}" }
            }
            div {
                dt { "Plan or certificate number" }
                dd { "{plan_number}" }
            }
            div {
                dt { "Requested work" }
                dd { "{work}" }
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
            plan_case_number: Some("4900000-001".into()),
            notice_matter_type: Some("Document review".into()),
            review: ReferralReview::Take,
            notice_href: "/app/inbox/synthetic".into(),
            resolve_action: "/app/plan-intake/019a0000-0000-7000-8000-000000000001/resolve-member"
                .into(),
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
            "Project code",
            "value=\"plan-ar-004512\"",
            "Take assignment and open Pitch",
            "/app/plan-intake/019a0000-0000-7000-8000-000000000001/accept",
            "/app/plan-intake/019a0000-0000-7000-8000-000000000001/decline",
        ] {
            assert!(html.contains(expected), "missing {expected}: {html}");
        }
    }

    #[test]
    fn an_unpriced_row_says_so_and_a_blocking_conflict_says_the_matter_cannot_open() {
        let mut unpriced = row();
        unpriced.fee = "Coverage and fees are unverified.".into();
        unpriced.conflict = "Blocking conflict finding — the matter cannot open.".into();
        unpriced.conflict_blocking = true;
        unpriced.review = ReferralReview::Hold;
        unpriced.conflict_findings = vec!["Dana Example matches an open adverse party.".into()];
        let html = dioxus_ssr::render_element(plan_intake_body(&page(vec![unpriced])));
        assert!(html.contains("Coverage and fees are unverified."), "{html}");
        assert!(html.contains("the matter cannot open"), "{html}");
        assert!(
            html.contains("Dana Example matches an open adverse party."),
            "{html}"
        );
    }

    #[test]
    fn a_notice_without_email_requires_member_confirmation_before_taking() {
        let mut unresolved = row();
        unresolved.review = ReferralReview::ConfirmMember;
        unresolved.member_email = None;
        let html = dioxus_ssr::render_element(plan_intake_body(&page(vec![unresolved])));
        assert!(
            html.contains("Confirm member and check conflicts"),
            "{html}"
        );
        assert!(!html.contains("Take assignment and open Pitch"), "{html}");
    }

    #[test]
    fn an_older_notice_requires_its_provider_case_number_separately_from_coverage() {
        let mut legacy = row();
        legacy.plan_case_number = None;
        let html = dioxus_ssr::render_element(plan_intake_body(&page(vec![legacy])));
        assert!(
            html.contains("Provider case number from the original notice"),
            "{html}"
        );
        assert!(html.contains("PLAN-778812"), "{html}");
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
        assert!(!html.contains("Take assignment and open Pitch"), "{html}");
    }
}
