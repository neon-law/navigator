//! The lawyer matter dashboard (`/app/projects/{code}`) as a Dioxus component
//! (#641 Phase 3, projects cluster) — the firm-side single-matter view.
//!
//! The successor to the `admin::projects_detail_lawyer` render. Reached only
//! through `store::access::matter_viewer` — a participation row is required of
//! every tier, Owner and Admin included (ENG-81); a lawyer not on the matter,
//! and a non-lawyer caller, both get `404` (the matter does not exist for
//! them). An Owner/Admin who holds no row instead renders
//! [`crate::admin_unassigned_project_detail::AdminUnassignedProjectDetail`], a
//! narrower sibling page — see [`crate::matter_surface`].
//!
//! The page is a shell, not a pile: the header, the summary fact grid, and the
//! tabbed sections live here, while each section's panel lives in its own
//! module — documents in [`crate::lawyer_project_documents`], the notation
//! board in [`crate::lawyer_project_notations`], the participation ledger in
//! [`crate::lawyer_project_participation`], the testimonial card in
//! [`crate::lawyer_project_testimonial`], and billing in [`crate::project_billing`]. The write forms render markup that
//! posts to the existing native `/app/projects/{code}/...` handlers; only the
//! rendering moves.
//!
//! The calendar is [`crate::project_calendar`] scoped to this matter. It is
//! the matter timeline: every docket entry on the matter's cases joined with
//! every open deadline, in date order ([`crate::project_timeline`]).

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{FactCard, FactGrid, Field, FormCard, Toast, ToastTone};
#[cfg(feature = "server")]
use crate::csrf::CsrfToken;
use crate::lawyer_project_communications::{communications_tab, notes_tab, CommunicationRow};
use crate::lawyer_project_documents::{documents_table, LawyerDocRow};
use crate::lawyer_project_notations::{ProjectNotationBoard, ProjectNotationRow};
use crate::lawyer_project_participation::{
    may_govern_lawyer_dri, ParticipationRow, ParticipationTable,
};
use crate::lawyer_project_testimonial::{admin_testimonial_card, AdminTestimonialView};
use crate::people::ViewerRole;
use crate::project_billing::{BillingTab, GenerateInvoiceView, MatterTimer, TimeEntryRow};
use crate::project_stages::{MatterStageView, StageSection};
use crate::project_tasks::{AssigneeChoice, TaskRow, TasksTab};

#[cfg(feature = "server")]
use crate::lawyer_project_documents::{
    asset_kind_choices, service_lines, upload_form_limits, visibility_word, LawyerDocRevision,
};
#[cfg(feature = "server")]
use crate::lawyer_project_notations::project_notation_board;
#[cfg(feature = "server")]
use crate::lawyer_project_participation::to_participation_rows;

/// The workbench's read query: the `?error=` flash a refused write redirects
/// back with, the write forms' success flash, and the generate-invoice
/// action's confirm step (`?confirm=1`).
#[derive(Deserialize, Default)]
pub struct MatterQuery {
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub flash: Option<String>,
    #[serde(default)]
    pub confirm: Option<String>,
    #[serde(default)]
    pub review: Option<String>,
}

pub use crate::sections::page::MatterTab;

/// The summary strip under the tab bar: the handful of facts a matter opens
/// with, derived from data the view already carries. A fact this matter does
/// not have is left out rather than shown as a zero, and the strip itself is
/// skipped when nothing renders in it. The tasks tile renders from the task
/// rows the view carries. The Work in Progress and Outstanding Balance tiles
/// render from the projections the server fn derives through
/// `store::matter_financials`.
fn matter_fact_grid(view: &LawyerDetailView) -> Element {
    let open_tasks = view.tasks.iter().filter(|row| row.state != "done").count();
    let overdue_tasks = view
        .tasks
        .iter()
        .filter(|row| row.is_overdue(&view.today))
        .count();
    let wip = view.wip.clone();
    let outstanding = view.outstanding.clone();
    let unread_inbox = view.unread_inbox;
    let has_facts = open_tasks > 0 || unread_inbox > 0 || wip.is_some() || outstanding.is_some();

    if !has_facts {
        return rsx! {};
    }

    rsx! {
        section { class: "lawyer-detail__section lawyer-detail__summary",
            FactGrid {
                if open_tasks > 0 {
                    FactCard { title: "Open tasks".to_string(),
                        a {
                            href: MatterTab::Tasks.href(&view.code),
                            class: "lawyer-detail__fact-value lawyer-detail__fact-link",
                            aria_label: "View {open_tasks} open tasks",
                            "{open_tasks}"
                        }
                        if overdue_tasks > 0 {
                            span { "{overdue_tasks} overdue" }
                        }
                    }
                }
                if unread_inbox > 0 {
                    FactCard { title: "Unread correspondence".to_string(),
                        a {
                            href: format!("/app/projects/{}/inbox", view.code),
                            class: "lawyer-detail__fact-value lawyer-detail__fact-link",
                            "{unread_inbox}",
                        }
                    }
                }
                if let Some(wip) = wip {
                    FactCard { title: "Work in progress".to_string(),
                        span { class: "lawyer-detail__fact-value", "{wip}" }
                    }
                }
                if let Some(outstanding) = outstanding {
                    FactCard { title: "Outstanding balance".to_string(),
                        span { class: "lawyer-detail__fact-value", "{outstanding}" }
                    }
                }
            }
        }
    }
}

fn project_page_link(tab: &crate::sections::page::PageTab, current: &str) -> Element {
    use crate::components::resource_mark::{
        BillingGlyph, DeadlinesGlyph, OnboardingGlyph, OverviewGlyph,
    };
    let icon_class = "project-resources__mark".to_string();
    let icon = match MatterTab::from_path(&tab.href) {
        Some(MatterTab::Overview) => rsx! { OverviewGlyph { class: icon_class } },
        Some(MatterTab::Onboarding) => rsx! { OnboardingGlyph { class: icon_class } },
        Some(MatterTab::Deadlines) => rsx! { DeadlinesGlyph { class: icon_class } },
        Some(MatterTab::Billing) => rsx! { BillingGlyph { class: icon_class } },
        _ => return rsx! {},
    };
    rsx! {
        a {
            href: "{tab.href}", title: "{tab.label}", aria_label: "{tab.label}",
            aria_current: (tab.href == current).then_some("page"),
            class: "nav-btn nav-btn--secondary project-toolbar__page",
            {icon}
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct EnvelopeRow {
    pub id: String,
    pub status: String,
    pub documents: Vec<String>,
    pub signers: Vec<String>,
}

/// The selected view on the lawyer's Deadlines tab.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)]
pub enum DeadlineTabView {
    #[default]
    Details,
    Timeline {
        sort: String,
        dir: String,
    },
    CourtDateReview,
}

#[cfg(any(feature = "server", test))]
impl DeadlineTabView {
    fn from_query(review: Option<&str>, calendar: &crate::project_calendar::CalendarQuery) -> Self {
        use crate::project_calendar::{sort_dir, sort_field, MATTER_COLUMNS};
        if review == Some("court-dates") {
            Self::CourtDateReview
        } else if let Some(sort) = calendar.sort.as_deref() {
            Self::Timeline {
                sort: sort_field(Some(sort), MATTER_COLUMNS),
                dir: sort_dir(calendar.dir.as_deref()),
            }
        } else {
            Self::Details
        }
    }
}

/// The rendered lawyer workbench — every field wasm-safe (plain scalars; no
/// `store`/`SeaORM`/`cloud`/`repos` type crosses to the client build).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
#[allow(clippy::struct_excessive_bools)] // Independent facts about one matter page.
pub struct LawyerDetailView {
    /// The matter's one tab strip: Overview, its synced `pages:` through the
    /// lawyer lens, then the workbench built-ins.
    pub page_tabs: Vec<crate::sections::page::PageTab>,
    #[serde(default)]
    pub onboarding: crate::sections::engagement::OnboardingSummary,
    pub id: String,
    pub code: String,
    pub name: String,
    pub status: String,
    /// Which house brand's storefront this matter was opened through
    /// (`store::projects::Project::brand`) — `"neon"` or
    /// `"delete-your-data"` today.
    pub brand: String,
    pub entity_name: Option<String>,
    /// `store::projects::Project::inserted_at` (RFC 3339): when the matter
    /// was opened.
    #[serde(default)]
    pub opened_at: String,
    /// The committer date (RFC 3339) of the tip commit on the matter's
    /// repository, read live from the deployment's GitHub organization.
    /// `None` when the matter has no repository there or the read failed.
    #[serde(default)]
    pub last_committed_at: Option<String>,
    /// A `closed` matter with no offboarding letter on file
    /// (`store::projects::matter_flags`), shown as a warning badge.
    #[serde(default)]
    pub missing_offboarding_letter: bool,
    /// Everyone accountable on each side, by name. Empty on the lawyer side is
    /// the unassigned matter the oversight lens exists to flag.
    pub lawyer_dris: Vec<String>,
    pub client_dris: Vec<String>,
    /// `true` when the reader is one of this matter's lawyer DRIs, which is what
    /// entitles them to govern the lawyer side from this page. The server
    /// resolves it; the render never infers accountability from the tier.
    pub viewer_is_lawyer_dri: bool,
    /// The matter's six collaboration resources, already filtered to what this
    /// reader may see. The two Slack columns and the Notion pages are not
    /// carried loose on this view: a firm-only URL must not be serialised into
    /// a page whose reader may not see it, and one filtered list is the only
    /// place that decision is made.
    pub resources: crate::project_resources::ProjectResourcesView,
    /// This matter's client-trust position — the same client-safe numbers
    /// the client sees on their own page, for the lawyers on this matter.
    /// A Firm-wide view of the pooled account is a different surface and
    /// never renders here.
    #[serde(default)]
    pub trust: crate::portal_project_detail::TrustView,
    /// The matter's Work in Progress — unbilled billable time at snapshot
    /// rates plus unbilled confirmed costs, formatted server-side by
    /// `store::matter_financials`. Absent when the projection is zero (a fact
    /// the matter does not have is never shown as a zero) or when the
    /// deployment has no PostgreSQL configured.
    #[serde(default)]
    pub wip: Option<String>,
    /// The matter's Outstanding Balance — invoiced minus paid across the
    /// matter's mirrored Xero invoices, formatted server-side. Same absence
    /// rules as `wip`.
    #[serde(default)]
    pub outstanding: Option<String>,
    pub repository_url: Option<String>,
    pub participations: Vec<ParticipationRow>,
    #[serde(default)]
    pub testimonial: Option<AdminTestimonialView>,
    pub documents: Vec<LawyerDocRow>,
    /// Every matter folder a checkout's `documents/` holds, in Finder order,
    /// so the table lists each one even while it is empty.
    #[serde(default)]
    pub document_folders: Vec<String>,
    #[serde(default)]
    pub notations: Vec<ProjectNotationRow>,
    #[serde(default)]
    pub envelopes: Vec<EnvelopeRow>,
    /// The `?error=` flash a refused write redirects back with — a re-upload
    /// whose kind conflicts with its chain, a DRI change, or an admin's
    /// testimonial save.
    #[serde(default)]
    pub error: Option<String>,
    /// The section this render shows, resolved server-side from the request
    /// path (`/app/projects/{code}/{segment}`). URL state, never client state.
    #[serde(default)]
    pub active_tab: MatterTab,
    /// The upload form's Kind select, as `(value, label)` pairs — every
    /// `rules::kind::Kind::valid_for(Lane::Asset)` value, computed
    /// server-side so the wasm client never needs the `rules` crate.
    pub asset_kind_choices: Vec<(String, String)>,
    /// The upload form's file and byte limits and the copy stating them.
    #[serde(default)]
    pub upload_limits: crate::lawyer_project_documents::UploadFormLimits,
    #[serde(default)]
    pub client_preview_new_tab: bool,
    pub csrf_token: String,
    #[serde(default)]
    pub deadline_view: DeadlineTabView,
    /// The matter timeline: docket entries and open deadlines, in date order.
    #[serde(default)]
    pub calendar_events: Vec<crate::project_calendar::CalendarEvent>,
    /// Every open deadline on this matter, soonest first, overdue marked.
    #[serde(default)]
    pub deadlines: Vec<crate::project_deadlines::DeadlineRow>,
    /// Completed and cancelled deadlines, collapsed in the Deadlines panel.
    #[serde(default)]
    pub past_deadlines: Vec<crate::project_deadlines::DeadlineRow>,
    /// The matter's firm-internal notes (the `note` channel), oldest first —
    /// rendered with the Communications log.
    #[serde(default)]
    pub notes: Vec<CommunicationRow>,
    /// Every logged email, text, and call on this matter, oldest first — the
    /// body of the Communications tab. The logging is verified working for
    /// emails without a call (LAW-230); this is that log's surface.
    #[serde(default)]
    pub correspondence: Vec<CommunicationRow>,
    /// Unread Inbox conversations assigned to this matter — the summary
    /// grid's correspondence tile, deep-linking the existing per-project
    /// inbox. Firm-wide as the Inbox keeps its read-state (there is no
    /// per-participant read flag to derive a per-reader count from).
    #[serde(default)]
    pub unread_inbox: i64,
    /// The matter's tasks — assignable work with a state and an assignee,
    /// deliberately not the docket's deadlines. Empty when the deployment
    /// has no PostgreSQL configured (the task store is PostgreSQL-native).
    #[serde(default)]
    pub tasks: Vec<TaskRow>,
    pub pending_handoffs: Vec<crate::project_tasks::HandoffRow>,
    /// The people a task on this matter can be assigned to, resolved
    /// server-side to `(person id, display name)` — the matter's
    /// participants, since the store refuses an assignee off the matter.
    #[serde(default)]
    pub task_assignees: Vec<AssigneeChoice>,
    /// The matter's stage — where it sits in the Firm's workflow. Not a
    /// lifecycle status (`project.status`) and not a court case's status
    /// (`case.status`); the copy keeps the three apart. Empty when the
    /// deployment has no PostgreSQL configured (the stage store is
    /// PostgreSQL-native).
    #[serde(default)]
    pub stage: MatterStageView,
    /// The Billing tab's captured time — the time-entry table (PostgreSQL-
    /// native; empty without a configured deployment, like the tasks above).
    #[serde(default)]
    pub billing_entries: Vec<TimeEntryRow>,
    #[serde(default)]
    pub billing_claims: Vec<crate::project_billing::PlanClaimRow>,
    #[serde(default)]
    pub billing_annotations: Vec<crate::project_billing::AnnotationRow>,
    /// The Billing tab's resolved fee structure — the reader's hourly rate
    /// on this matter and the matter's agreed flat fee, server-formatted.
    #[serde(default)]
    pub billing_fees: crate::project_billing::FeeStructureView,
    /// The Billing tab's mirrored Xero invoice rows — the mirror, never Xero
    /// live.
    #[serde(default)]
    pub billing_invoices: Vec<crate::project_billing::InvoiceRow>,
    /// The Billing tab's expense rows — the same `project_cost` rows the
    /// project-costs JSON API serves.
    #[serde(default)]
    pub billing_expenses: Vec<crate::project_billing::ExpenseRow>,
    /// The reader's open Timer, if any — shown in the header on every tab.
    /// One on another matter renders as a link to that matter's Billing tab,
    /// because the timer's write doors are matter-scoped.
    #[serde(default)]
    pub running_timer: Option<crate::project_billing::TimerView>,
    /// The generate-invoice action's composed state — the preview of the
    /// exact composition the command would raise.
    #[serde(default)]
    pub invoice_generate: GenerateInvoiceView,
    /// Today as `yyyy-mm-dd` — pre-fills the log-time form's date.
    #[serde(default)]
    pub today: String,
    /// The write forms' success flash (`?flash=`) — rendered as a toast
    /// naming what happened (e.g. the invoice just raised).
    #[serde(default)]
    pub flash: Option<String>,
    /// `true` when the URL carries `?confirm=1` — the Billing tab shows the
    /// generate-invoice compose and its confirm step.
    #[serde(default)]
    pub confirm_generate: bool,
    pub role: ViewerRole,
    /// The deploy's brand mark for the navbar. `None` when the mounted brand
    /// configures none.
    #[serde(default)]
    pub logo: Option<crate::components::AppLogo>,
    /// The resolved brand's tokens stylesheet href, so the page wears
    /// its own palette rather than the firm's on a non-default host.
    #[serde(default)]
    pub tokens_href: String,
}

/// Resolve a side's DRI names from the designated ids, alphabetical.
///
/// A designated person who is no longer on file drops out rather than blanking
/// the whole side: the other names are still the answer to "who is accountable".
#[cfg(feature = "server")]
async fn dri_names(
    surreal: &store::surreal::SurrealDb,
    person_ids: &[uuid::Uuid],
) -> Result<Vec<String>, ServerFnError> {
    if person_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut names: Vec<String> = store::persons::find_by_ids(surreal, person_ids)
        .await
        .map_err(server_error)?
        .into_iter()
        .map(|person| person.name)
        .collect();
    names.sort();
    Ok(names)
}

/// Resolve the browser link and activity for the same Project source.
#[cfg(feature = "server")]
async fn project_source(
    surreal: &store::surreal::SurrealDb,
    project: &store::projects::Project,
) -> (Option<String>, Option<String>) {
    use cloud::forge::ForgeService;
    use cloud::forgejo_forge::NAVIGATOR_FORGEJO_ROOT_URL_ENV;

    let Some(recorded) = project.repository_url.as_deref() else {
        return (None, None);
    };
    let Ok(backend) = cloud::ForgeBackend::from_lookup(|key| std::env::var(key).ok()) else {
        return (None, None);
    };
    let forgejo_url = match (
        std::env::var(NAVIGATOR_FORGEJO_ROOT_URL_ENV),
        project.firm_id,
    ) {
        (Ok(root), Some(firm_id)) => store::firms::find_by_id(surreal, firm_id)
            .await
            .ok()
            .flatten()
            .and_then(|firm| forgejo_repository_url(recorded, &root, &firm.slug, &project.code)),
        _ => None,
    };
    if let Some(url) = forgejo_url {
        let kms = cloud::runtime_kms_from_env().await;
        let activity = match store::project_surfaces::forgejo_for_project(
            surreal,
            project.id,
            kms.as_ref(),
            |key| std::env::var(key).ok(),
        )
        .await
        {
            Ok(forge) => forge
                .head_commit_committed_at(&project.code)
                .await
                .ok()
                .flatten(),
            Err(_) => None,
        };
        return (Some(url), activity);
    }

    if backend == cloud::ForgeBackend::Forgejo {
        return (None, None);
    }
    let activity = match cloud::workspace::WorkspaceConfig::from_env() {
        Ok(workspace) if recorded == workspace.expected_repository_url(&project.code) => {
            match cloud::forge::GitHubForge::from_env() {
                Ok(forge) => forge
                    .head_commit_committed_at(&project.code)
                    .await
                    .ok()
                    .flatten(),
                Err(_) => None,
            }
        }
        _ => None,
    };
    (Some(recorded.to_owned()), activity)
}

#[cfg(feature = "server")]
fn forgejo_repository_url(recorded: &str, root: &str, firm: &str, code: &str) -> Option<String> {
    let expected = format!("{}/{firm}/{code}", root.trim().trim_end_matches('/'));
    let normalized = recorded.trim_end_matches('/');
    (normalized == expected || normalized.strip_suffix(".git") == Some(expected.as_str()))
        .then_some(expected)
}

/// Fetch the lawyer-lens workbench for one matter. Refuses a non-lawyer caller and
/// a lawyer not on the matter with `404`; a query failure is a `500`.
#[server]
#[cfg_attr(feature = "server", allow(clippy::too_many_lines))]
pub async fn get_lawyer_project_detail() -> Result<LawyerDetailView, ServerFnError> {
    let axum::extract::Path(code) =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Path<String>, _>()
            .await?;
    // The calendar's sort. Lenient like the workbench's — an unrecognised
    // column falls back to the leftmost rather than refusing the matter, which
    // is why this route carries no bad-sort pre-handler.
    let calendar_query = dioxus_fullstack_core::FullstackContext::extract::<
        axum::extract::Query<crate::project_calendar::CalendarQuery>,
        _,
    >()
    .await
    .map_or_else(
        |_| crate::project_calendar::CalendarQuery::default(),
        |axum::extract::Query(q)| q,
    );
    // The `?error=` flash a refused write's redirect carries, the write
    // forms' success flash, and the generate-invoice action's confirm step.
    let query =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Query<MatterQuery>, _>()
            .await
            .map_or_else(|_| MatterQuery::default(), |axum::extract::Query(q)| q);
    let error = query.error;
    let flash = query.flash;
    let confirm_generate = query.confirm.is_some();
    // The section is the request path's; the dispatcher already refused one
    // the workbench does not have (`matter_surface::serves`).
    let active_tab = crate::sections::page::request_tab()
        .await
        .unwrap_or_default();
    let role = dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<ViewerRole>, _>()
        .await
        .map(|axum::Extension(role)| role)
        .unwrap_or_default();
    // The navbar renders on the 404 body too, so the mark is resolved before the
    // first early return rather than only on the happy path.
    let logo = crate::app_chrome::app_logo_from_context().await;
    let tokens_href = crate::app_chrome::app_tokens_href_from_context().await;
    // A non-lawyer caller gets the handler's 404 — the workbench is hidden,
    // not merely refused.
    if !role.is_lawyer_tier() {
        return Ok(not_found(uuid::Uuid::nil(), role, logo, String::new()));
    }
    let csrf_token =
        dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<CsrfToken>, _>()
            .await
            .map(|axum::Extension(token)| token.0)
            .unwrap_or_default();
    let person_id = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<crate::portal_project_list::PersonId>,
        _,
    >()
    .await
    .ok()
    .and_then(|axum::Extension(pid)| pid.0)
    .and_then(|raw| raw.parse::<uuid::Uuid>().ok());

    let store_role = match role {
        ViewerRole::Owner => store::persons::Role::Owner,
        ViewerRole::Admin => store::persons::Role::Admin,
        ViewerRole::Lawyer => store::persons::Role::Lawyer,
        ViewerRole::Clerk => store::persons::Role::Clerk,
        ViewerRole::Client => store::persons::Role::Client,
    };

    let surreal = consume_context::<store::surreal::SurrealDb>();
    let storage = consume_context::<std::sync::Arc<dyn cloud::StorageService>>();
    let Some(project) = store::projects::find_by_code(&surreal, &code)
        .await
        .map_err(server_error)?
    else {
        return Ok(not_found(uuid::Uuid::nil(), role, logo, String::new()));
    };
    let id = project.id;

    // Anyone not on the matter gets a 404, not a peek — Owner and Admin
    // included. This goes through `store::access`, which is the layer that
    // requires the participation row of every tier; the raw
    // `store::projects` predicate still carries the privileged short-circuit
    // its own remaining callers depend on, and reaching for it here is what
    // silently handed the workbench to an unassigned Owner.
    let viewer = store::access::matter_viewer(&surreal, person_id, store_role, id)
        .await
        .map_err(server_error)?;
    let Some(viewer) = viewer else {
        return Ok(not_found(id, role, logo, csrf_token));
    };
    let entity = store::entities::find_by_id(&surreal, project.entity_id)
        .await
        .map_err(server_error)?;
    let entity_name = entity.map(|entity| entity.name);
    let (repository_url, last_committed_at) = project_source(&surreal, &project).await;
    let onboarding_status = store::projects::onboarding_status(&surreal, id, &project.code)
        .await
        .map_err(server_error)?;
    let onboarding_runs = crate::sections::engagement::onboarding_runs(&surreal, id)
        .await
        .map_err(server_error)?;
    let onboarding_files = store::assets::for_project(&surreal, id)
        .await
        .map_err(server_error)?;
    let onboarding_notation_ids: Vec<_> =
        onboarding_runs.iter().map(|run| run.notation_id).collect();
    let completed_signatures =
        store::signatures::completed_for_notations(&surreal, &onboarding_notation_ids)
            .await
            .map_err(|error| server_error(error.to_string()))?
            .into_iter()
            .filter_map(|signature| {
                let run = onboarding_runs
                    .iter()
                    .find(|run| run.notation_id == signature.notation_id)?;
                Some(crate::sections::engagement::ExecutedSignature {
                    notation_id: signature.notation_id,
                    title: run.title.clone(),
                    provider: signature.provider,
                    signed_at: signature.signed_at.unwrap_or_default(),
                })
            })
            .collect::<Vec<_>>();
    let onboarding = crate::sections::engagement::onboarding_summary(
        &onboarding_runs,
        &onboarding_files,
        &completed_signatures,
        onboarding_status == store::projects::OnboardingStatus::Official,
    );
    let (has_engagement, has_closing) =
        store::projects::matter_lifecycle_sets(&surreal, std::slice::from_ref(&project))
            .await
            .map_err(server_error)?;
    let (_, missing_offboarding_letter) = store::projects::matter_flags(
        has_engagement.contains(&id),
        &project.status,
        has_closing.contains(&id),
    );

    let trust = if active_tab == MatterTab::Billing {
        let position = store::trust::position_for_project(&surreal, id)
            .await
            .map_err(server_error)?;
        crate::portal_project_detail::trust_view(
            &position,
            &store::iolta_withdrawals::for_project(&surreal, id)
                .await
                .map_err(server_error)?,
        )
    } else {
        crate::portal_project_detail::TrustView::default()
    };

    // Per-matter financial projections — Work in Progress and Outstanding
    // Balance, read-only over the time-entry, project-cost, and invoice-mirror
    // stores. A deployment without PostgreSQL configured renders the
    // workbench without the two billing tiles rather than failing the whole
    // page; a read failure inside a configured deployment is a 500 like every
    // other read here. A zero projection is left out like any other fact the
    // matter does not have. The same handle feeds the task store below.
    let pg = dashboard_postgres().await.map_err(server_error)?;
    let financials = match pg.as_ref().filter(|_| active_tab == MatterTab::Overview) {
        Some(pg) => Some(
            store::matter_financials::for_project(&surreal, pg, id)
                .await
                .map_err(server_error)?,
        ),
        None => None,
    };
    let (wip, outstanding) = financials.map_or((None, None), |financials| {
        let wip = (financials.wip_cents != 0)
            .then(|| crate::portal_project_detail::format_usd(financials.wip_cents));
        let outstanding = (financials.outstanding_cents != 0)
            .then(|| crate::portal_project_detail::format_usd(financials.outstanding_cents));
        (wip, outstanding)
    });

    // The Tasks tab's rows — assignable work, not the docket's deadlines. A
    // deployment without PostgreSQL renders the workbench without them, the
    // same absence rule as the financial projections; a read failure inside a
    // configured deployment is a 500.
    let tasks = match pg.as_ref() {
        Some(pg) => crate::project_tasks::read_rows(&surreal, pg, id).await?,
        None => Vec::new(),
    };

    let pending_handoffs = if active_tab == MatterTab::Tasks {
        store::task_handoffs::for_project(pg.as_ref(), viewer, id, &project.code, true)
            .await
            .map_err(server_error)?
            .into_iter()
            .map(crate::project_tasks::HandoffRow::from)
            .collect()
    } else {
        Vec::new()
    };

    // The stage surface's read — where the matter sits in the Firm's
    // workflow. Same absence rule as the task rows: a deployment without
    // PostgreSQL renders the workbench without a stage; a read failure
    // inside a configured deployment is a 500.
    let stage = match pg.as_ref().filter(|_| active_tab == MatterTab::Overview) {
        Some(pg) => crate::project_stages::read_stage(&surreal, pg, &project).await?,
        None => MatterStageView::default(),
    };

    // The Billing tab's reads — captured time, resolved fee structures,
    // mirrored invoices, expenses — plus the reader's open timer for the
    // header and the generate-invoice action's composed preview. Same
    // absence rule as the tasks and stage: a deployment without PostgreSQL
    // renders the workbench without them; a read failure inside a configured
    // deployment is a 500.
    let (billing_annotations, billing_entries, billing_fees, billing_invoices, billing_expenses) =
        match (active_tab, pg.as_ref()) {
            (MatterTab::Billing, Some(pg)) => {
                let reads =
                    crate::project_billing::read_billing(&surreal, pg, id, person_id).await?;
                (
                    reads.annotations,
                    reads.entry_rows,
                    reads.fees,
                    reads.invoice_rows,
                    reads.expense_rows,
                )
            }
            (MatterTab::Billing, None) => (
                Vec::new(),
                Vec::new(),
                crate::project_billing::FeeStructureView::default(),
                // Invoices mirror in Surreal, independent of PostgreSQL — the
                // workbench keeps showing them (and their payment state) even in
                // a deployment that has not configured the time and expense
                // store. Only the PostgreSQL-only reads degrade to empty.
                crate::project_billing::mirrored_invoice_rows(&surreal, id).await?,
                Vec::new(),
            ),
            _ => Default::default(),
        };
    let billing_claims = match pg.as_ref().filter(|_| active_tab == MatterTab::Billing) {
        Some(pg) => {
            crate::project_billing::read_claims(
                &surreal, pg, &project, viewer, store_role, person_id,
            )
            .await?
        }
        None => Vec::new(),
    };
    let running_timer = match (pg.as_ref(), person_id) {
        (Some(pg), Some(person_id)) => {
            crate::project_billing::read_timer(&surreal, pg, person_id, id).await?
        }
        _ => None,
    };
    let invoice_generate = match pg.as_ref().filter(|_| active_tab == MatterTab::Billing) {
        Some(pg) => crate::project_billing::read_generate(&surreal, pg, &project).await?,
        None => GenerateInvoiceView::default(),
    };
    let today = chrono::Utc::now().date_naive().to_string();

    // The two DRIs — a read failure is a 500, never a silently-blank DRI on an
    // accountability surface.
    let participation_rows = store::projects::participations_for_project(&surreal, id)
        .await
        .map_err(server_error)?;
    // Who a task on this matter can name as assignee: the matter's own
    // participants, resolved to display names, alphabetical. The store
    // refuses an assignee off the matter, so the picker offers exactly the
    // set the command would honour.
    let task_assignees = {
        let person_ids: Vec<uuid::Uuid> =
            participation_rows.iter().map(|row| row.person_id).collect();
        let mut people = store::persons::find_by_ids(&surreal, &person_ids)
            .await
            .map_err(server_error)?;
        people.sort_by(|a, b| a.name.cmp(&b.name));
        people
            .into_iter()
            .map(|person| (person.id.to_string(), person.name))
            .collect()
    };
    let lawyer_dri_ids: Vec<uuid::Uuid> = participation_rows
        .iter()
        .filter(|row| row.is_lawyer_dri)
        .map(|row| row.person_id)
        .collect();
    let client_dri_ids: Vec<uuid::Uuid> = participation_rows
        .iter()
        .filter(|row| row.is_client_dri)
        .map(|row| row.person_id)
        .collect();
    // Whether *this* reader is accountable, resolved from the ledger rather
    // than from their tier: a lawyer who is not a DRI on this matter does not
    // govern its lawyer side.
    let viewer_is_lawyer_dri = person_id.is_some_and(|me| lawyer_dri_ids.contains(&me));
    let lawyer_dris = dri_names(&surreal, &lawyer_dri_ids).await?;
    let client_dris = dri_names(&surreal, &client_dri_ids).await?;
    let testimonial = if role.is_admin_tier() {
        match client_dri_ids.first().copied() {
            Some(client_id) => store::testimonials::for_person_project(&surreal, client_id, id)
                .await
                .map_err(server_error)?
                .map(|testimonial| AdminTestimonialView {
                    quote: testimonial.quote,
                    attribution: testimonial.attribution_label.unwrap_or_default(),
                    public_requested: testimonial.consented_at.is_some(),
                    published: testimonial.consented_at.is_some()
                        && testimonial.published_at.is_some(),
                }),
            None => None,
        }
    } else {
        None
    };

    let service_lines = service_lines(&surreal, id).await.map_err(server_error)?;
    let documents = store::assets::grouped_for_project(&surreal, id)
        .await
        .map_err(server_error)?
        .into_iter()
        .map(|group| {
            let service = std::iter::once(group.current.id)
                .chain(group.revisions.iter().map(|revision| revision.id))
                .filter_map(|asset| service_lines.get(&asset))
                .flatten()
                .cloned()
                .collect();
            let path = group.current.documents_path();
            let folder = path.split('/').next().unwrap_or_default().to_string();
            LawyerDocRow {
                id: group.current.id.to_string(),
                filename: group.current.filename.unwrap_or_default(),
                path,
                folder,
                visibility: visibility_word(&group.current.visibility),
                revisions: group
                    .revisions
                    .into_iter()
                    .map(|revision| LawyerDocRevision {
                        id: revision.id.to_string(),
                        filename: revision.filename.unwrap_or_default(),
                        visibility: visibility_word(&revision.visibility),
                        created_at: revision
                            .inserted_at
                            .format("%-d %b %y %H:%M UTC")
                            .to_string(),
                    })
                    .collect(),
                service,
            }
        })
        .collect();
    let envelopes = match pg
        .as_ref()
        .filter(|_| active_tab == MatterTab::Documents && role.is_lawyer_tier())
    {
        Some(pg) => store::project_envelopes::for_project(pg, id)
            .await
            .map_err(server_error)?
            .into_iter()
            .map(|envelope| EnvelopeRow {
                id: envelope.envelope_id.to_string(),
                status: envelope.status,
                documents: envelope
                    .envelope_documents
                    .into_iter()
                    .map(|doc| doc.name)
                    .collect(),
                signers: envelope
                    .signers
                    .into_iter()
                    .map(|s| format!("{} ({})", s.name, s.status))
                    .collect(),
            })
            .collect(),
        None => Vec::new(),
    };
    let notations = if active_tab == MatterTab::Documents {
        project_notation_board(&surreal, &storage, id)
            .await
            .map_err(server_error)?
    } else {
        Vec::new()
    };

    // The participation ledger: the rows, plus the linked people in one batched
    // query so the system tier is visible without conflating it with
    // participation.
    let people = store::persons::find_by_ids(
        &surreal,
        &participation_rows
            .iter()
            .map(|r| r.person_id)
            .collect::<Vec<_>>(),
    )
    .await
    .map_err(server_error)?;
    let participations = to_participation_rows(&participation_rows, &people);

    // The matter timeline: every docket entry joined with every open
    // deadline, in date order, through the calendar the page already carries.
    let stored_deadlines = store::project_deadlines::list(&surreal, id)
        .await
        .map_err(server_error)?;
    let docket = store::cases::docket_for_project(&surreal, id)
        .await
        .map_err(server_error)?;
    let calendar_events = crate::project_timeline::timeline_events(
        &docket,
        &stored_deadlines,
        &project.name,
        entity_name.as_deref().unwrap_or_default(),
    );

    let deadline_groups =
        crate::project_deadlines::deadline_groups(&stored_deadlines, chrono::Utc::now(), false);

    // The Communications tab reads the
    // firm-visible thread once; the projection splits notes from
    // correspondence so the render never matches channel discriminators.
    let (notes, correspondence) = if active_tab == MatterTab::Communications {
        let thread = store::communications::for_project(&surreal, id)
            .await
            .map_err(server_error)?;
        let author_ids: Vec<uuid::Uuid> = thread
            .iter()
            .filter_map(|row| row.author_person_id)
            .collect();
        let authors: std::collections::HashMap<uuid::Uuid, String> =
            store::persons::find_by_ids(&surreal, &author_ids)
                .await
                .map_err(server_error)?
                .into_iter()
                .map(|person| (person.id, person.name))
                .collect();
        crate::lawyer_project_communications::communication_views(&thread, &authors)
    } else {
        (Vec::new(), Vec::new())
    };

    // The summary grid's correspondence tile: unread Inbox conversations on
    // this matter, a read-only count over the Inbox's own read-state. The
    // reader's participation is already resolved by the `matter_viewer` seam
    // above.
    let unread_inbox = store::inbox::unread_count_for_project(&surreal, id)
        .await
        .map_err(server_error)?;

    let code_for_resources = project.code.clone();
    let page_tabs =
        crate::sections::page::stored_page_tabs(&surreal, id, &project.code, viewer).await;
    let (verified_drive_root, verified_submissions) =
        crate::project_resources::verified_drive_links(&surreal, person_id, role, id).await;

    Ok(LawyerDetailView {
        client_preview_new_tab: crate::brand_website::client_preview_elsewhere(&project).await,
        page_tabs,
        onboarding,
        id: project.id.to_string(),
        status: store::projects::execution_lifecycle_label(
            &project.code,
            &project.status,
            onboarding_status == store::projects::OnboardingStatus::Official,
        )
        .into(),
        code: project.code,
        name: project.name,
        brand: project.brand,
        entity_name,
        opened_at: project.inserted_at,
        last_committed_at,
        missing_offboarding_letter,
        lawyer_dris,
        client_dris,
        viewer_is_lawyer_dri,
        resources: crate::project_resources::ProjectResourcesView {
            resources: crate::project_resources::visible_resources(
                &crate::project_resources::ProjectResourceLinks {
                    client_submissions_folder_id: verified_submissions,
                    private_slack_channel_url: project.internal_slack_channel_url,
                    private_slack_channel_id: project.internal_slack_channel_id,
                    private_notion_page_url: project.private_notion_page_url,
                    drive_folder_id: verified_drive_root,
                    shared_slack_channel_url: project.external_slack_channel_url,
                    shared_notion_page_url: project.shared_notion_page_url,
                    calendar_id: project.calendar_id,
                    booking_url: project.booking_url,
                },
                role,
            ),
            can_configure: viewer.is_firm_side(),
            project_code: code_for_resources,
        },
        trust,
        wip,
        outstanding,
        repository_url,
        participations,
        testimonial,
        documents,
        document_folders: store::document_pointers::MatterFolder::ALL
            .iter()
            .map(|folder| folder.as_str().to_string())
            .collect(),
        notations,
        envelopes,
        error,
        active_tab,
        asset_kind_choices: asset_kind_choices(),
        upload_limits: upload_form_limits(),
        csrf_token,
        deadline_view: DeadlineTabView::from_query(query.review.as_deref(), &calendar_query),
        calendar_events,
        deadlines: deadline_groups.open,
        past_deadlines: deadline_groups.past,
        notes,
        correspondence,
        unread_inbox,
        tasks,
        pending_handoffs,
        task_assignees,
        stage,
        billing_entries,
        billing_claims,
        billing_annotations,
        billing_fees,
        billing_invoices,
        billing_expenses,
        running_timer,
        invoice_generate,
        today,
        flash,
        confirm_generate,
        role,
        logo,
        tokens_href,
    })
}

/// The optional workbench store: an absent deployment is distinct from a
/// configured store that failed to open.
#[cfg(feature = "server")]
async fn dashboard_postgres(
) -> Result<Option<store::postgres::PostgresDb>, store::postgres::PostgresError> {
    if std::env::var_os("NAVIGATOR_POSTGRES_HOST").is_none() {
        return Ok(None);
    }
    store::postgres::PostgresDb::from_env().await.map(Some)
}

/// Commit a `500` and wrap a query error — an unavailable workbench is a server
/// error, not a `200` with an error body.
#[cfg(feature = "server")]
pub(crate) fn server_error(e: impl std::fmt::Display) -> ServerFnError {
    dioxus_fullstack_core::FullstackContext::commit_http_status(
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        None,
    );
    ServerFnError::new(e.to_string())
}

/// Commit the `404` the handler returned for a matter the caller cannot see
/// and return an empty (nameless) view.
#[cfg(feature = "server")]
fn not_found(
    id: uuid::Uuid,
    role: ViewerRole,
    logo: Option<crate::components::AppLogo>,
    csrf_token: String,
) -> LawyerDetailView {
    dioxus_fullstack_core::FullstackContext::commit_http_status(
        axum::http::StatusCode::NOT_FOUND,
        None,
    );
    LawyerDetailView {
        id: id.to_string(),
        role,
        logo,
        csrf_token,
        ..LawyerDetailView::default()
    }
}

/// The lawyer matter-detail workbench, server-side rendered.
#[component]
pub fn LawyerProjectDetail() -> Element {
    let resource = use_server_future(get_lawyer_project_detail)?;

    let view = match &*resource.read() {
        Some(Ok(view)) if !view.name.is_empty() => view.clone(),
        Some(Ok(_)) => {
            return rsx! {
                main { id: "lawyer-project", p { "That project was not found." } }
            };
        }
        Some(Err(_)) => {
            return rsx! {
                main { id: "lawyer-project", p { "Failed to load this project." } }
            };
        }
        None => {
            return rsx! {
                main { id: "lawyer-project", p { "Loading…" } }
            };
        }
    };

    let is_admin = view.role.is_admin_tier();
    let csrf = view.csrf_token.clone();
    let lawyer_dri_disp = view.lawyer_dris.join(", ");
    let client_dri_disp = view.client_dris.join(", ");
    let current_page = view.active_tab.href(&view.code);
    // Who may govern each side, mirroring `store::participation::authorize` so
    // the page renders exactly the controls the command would honour. An empty
    // lawyer set is the bootstrap case: a lawyer already on this workbench may
    // name the first DRI, because there is no current holder to admit them.
    let may_govern_lawyer_side = may_govern_lawyer_dri(
        is_admin,
        view.viewer_is_lawyer_dri,
        view.lawyer_dris.is_empty() && view.role.is_lawyer_tier(),
    );
    let may_govern_client_side = view.role.is_lawyer_tier();
    let has_stage = view.stage.current.is_some()
        || !view.stage.definitions.is_empty()
        || !view.stage.history.is_empty();
    let brand_disp = crate::brand_website::BrandWebsite::from_key(&view.brand).attached_line();

    rsx! {
        document::Title { "Project | {view.name}" }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{view.tokens_href}" }
        crate::components::AppNavbar {
            destinations: crate::app_chrome::app_destinations(view.role),
            logo: view.logo.clone(),
        }
        main { id: "lawyer-project", class: "nav-theme lawyer-detail",
            header { class: "page-header",
                h1 { class: "project-heading",
                    span { "{view.name}" }
                    span { class: "project-heading__code", "{view.code}" }
                }
                nav { class: "lawyer-detail__actions project-toolbar", "aria-label": "Project actions",
                    for tab in &view.page_tabs {
                        {project_page_link(tab, &current_page)}
                    }
                    a { class: "nav-btn nav-btn--secondary lawyer-detail__resource-action", href: "/app/projects/{view.code}/notations/new", title: "New notation", aria_label: "New notation",
                        crate::components::resource_mark::ClipboardPlusGlyph { class: "project-resources__mark".to_string() }
                    }
                    a { class: "nav-btn nav-btn--secondary lawyer-detail__resource-action", href: "/app/projects/{view.code}/edit", title: "Edit project", aria_label: "Edit project",
                        crate::components::resource_mark::PencilGlyph { class: "project-resources__mark".to_string() }
                    }
                    {crate::project_resources::resource_actions(&view.resources)}
                    if let Some(url) = view.repository_url.as_ref() {
                        a {
                            class: "nav-btn nav-btn--secondary lawyer-detail__resource-action",
                            href: "{url}",
                            target: "_blank",
                            rel: "noopener noreferrer",
                            title: "Source repository",
                            aria_label: "Source repository",
                            crate::components::resource_mark::GitRepositoryGlyph {
                                class: "project-resources__mark".to_string(),
                            }
                        }
                    }
                    if view.role.is_lawyer_tier() {
                        // The conflict check, made visible: the matter's
                        // relationship-graph page, fed by the read-only
                        // graph door.
                        a {
                            class: "nav-btn nav-btn--secondary lawyer-detail__resource-action",
                            href: "/app/projects/{view.code}/graph",
                            title: "Relationship graph",
                            aria_label: "Relationship graph",
                            crate::components::resource_mark::WebGraphGlyph {
                                class: "project-resources__mark".to_string(),
                            }
                        }
                    }
                    crate::components::dri_view::ViewAsClient {
                        code: view.code.clone(), csrf: csrf.clone(), new_tab: view.client_preview_new_tab, icon: true,
                    }
                }
                // The reader's open Timer — on every tab, because time runs
                // while you work. The table row in PostgreSQL is the truth;
                // the header just shows it (and ticks, when the ticker script
                // loads).
                if let Some(timer) = view.running_timer.as_ref() {
                    MatterTimer { code: view.code.clone(), csrf: csrf.clone(), timer: timer.clone() }
                }
                if let Some(flash) = view.flash.as_ref() {
                    Toast { message: flash.clone(), tone: ToastTone::Success }
                }
                // The editable sections render their errors beside the form.
                if !(matches!(view.active_tab, MatterTab::Tasks | MatterTab::Billing) || view.active_tab == MatterTab::Overview && has_stage) {
                    if let Some(error) = view.error.as_ref() {
                        p { class: "nav-form-error", role: "alert", "{error}" }
                    }
                }
            }

            {
                match view.active_tab {
                    MatterTab::Overview => rsx! {
                        p { class: "nav-muted",
                            "Status: {view.status}"
                            if !view.lawyer_dris.is_empty() { " · Lawyer DRI: {lawyer_dri_disp}" }
                            if !view.client_dris.is_empty() { " · Client DRI: {client_dri_disp}" }
                            if view.missing_offboarding_letter {
                                span { class: "matter-flag", "Missing offboarding letter" }
                            }
                        }
                        {matter_fact_grid(&view)}
                        crate::sections::page::CustomPagePanels { tabs: view.page_tabs.clone() }
                        details { open: view.error.is_some(),
                            summary { "Project details" }
                            nav { class: "lawyer-detail__actions", aria_label: "Project records",
                                for tab in [MatterTab::Tasks, MatterTab::Documents, MatterTab::Communications, MatterTab::Participants] {
                                    a { href: tab.href(&view.code), "{tab.label()}" }
                                }
                            }
                            p { class: "nav-muted",
                                "{brand_disp}"
                                if let Some(entity) = &view.entity_name { " · {entity}" }
                                " · Opened " crate::components::UtcTime { at: view.opened_at.clone() }
                                if let Some(at) = &view.last_committed_at {
                                    " · Last commit " crate::components::UtcTime { at: at.clone() }
                                }
                            }
                            if has_stage {
                                StageSection {
                                    code: view.code.clone(), csrf: csrf.clone(),
                                    stage: view.stage.clone(), error: view.error.clone(),
                                }
                            }
                        }

                        if is_admin && !view.client_dris.is_empty() {
                            {admin_testimonial_card(&view.code, &csrf, view.testimonial.as_ref())}
                        }

                        // Closing a matter is bespoke: it is asked for by email and opened
                        // by the lawyer DRI, never fired from a button here. Every firm
                        // participant reads where to ask — the accountability marker
                        // decides who acts on the request, not who may raise it.
                        if matches!(view.status.as_str(), "active" | "pitch") {
                            section { class: "lawyer-detail__section project-close",
                                p { class: "nav-muted",
                                    "To close this project, email the lawyer DRI"
                                    " or "
                                    "support@neonlaw.com"
                                    "."
                                }
                                // A matter always has at least one lawyer DRI — they are who
                                // close it. An empty set is therefore a gap to fill, not a
                                // neutral blank, so the workbench says so. Admin can add a
                                // person and mark them; a lawyer already on the matter uses
                                // Make DRI on a firm-side row in the ledger below.
                                if view.lawyer_dris.is_empty() && view.role.is_lawyer_tier() {
                                    p { class: "nav-form-error", role: "alert",
                                        "This project has no lawyer DRI. "
                                        if is_admin {
                                            a { class: "nav-link", href: "/app/projects/{view.code}/people/new", "Designate the accountable lawyer" }
                                        } else {
                                            "Use Make DRI on a lawyer already assigned to this project"
                                        }
                                        "."
                                    }
                                }
                            }
                        }
                    },
                    MatterTab::Onboarding => rsx! {
                        section { class: "lawyer-detail__section project-onboarding",
                            h2 { "Onboarding" }
                            if !view.onboarding.proposed.is_empty() {
                                div { class: "project-onboarding__state project-onboarding__state--proposed",
                                    strong { "Proposed" }
                                    for item in view.onboarding.proposed.iter() { p { "{item}" } }
                                }
                            }
                            if !view.onboarding.executed.is_empty() {
                                div { class: "project-onboarding__state project-onboarding__state--executed",
                                    strong { "Onboarded" }
                                    for item in view.onboarding.executed.iter() { p { "{item}" } }
                                }
                            }
                            if view.onboarding.proposed.is_empty() && view.onboarding.executed.is_empty() {
                                p { "No onboarding Notation or executed onboarding document is recorded." }
                            }
                        }
                    },
                    MatterTab::Deadlines => rsx! {
                        if view.deadline_view == DeadlineTabView::CourtDateReview {
                            a { class: "nav-link", href: "{current_page}", "Back to Deadlines" }
                            crate::court_date_review::CourtDateReview {
                                project_id: view.id.clone(), project_code: view.code.clone(), csrf: csrf.clone(),
                            }
                        } else {
                            a { class: "nav-link", href: "{current_page}?review=court-dates", "Review proposed court dates" }
                            if let DeadlineTabView::Timeline { sort, dir } = &view.deadline_view {
                                a { class: "nav-link", href: "{current_page}", "View deadline details" }
                                crate::project_calendar::ProjectCalendar {
                                    section_class: "lawyer-detail__section project-calendar".to_string(),
                                    heading: "Calendar timeline".to_string(),
                                    empty_message: "No calendar events scheduled for this project.".to_string(),
                                    columns: crate::project_calendar::MATTER_COLUMNS.to_vec(),
                                    path: MatterTab::Deadlines.href(&view.code),
                                    query_prefix: String::new(),
                                    sort: sort.clone(),
                                    dir: dir.clone(),
                                    events: view.calendar_events.clone(),
                                }
                            } else {
                                a { class: "nav-link", href: "{current_page}?sort=date&dir=asc", "View calendar timeline" }
                                crate::project_deadlines::ProjectDeadlinesPanel {
                                    section_class: "lawyer-detail__section".to_string(),
                                    rows: view.deadlines.clone(),
                                    past: view.past_deadlines.clone(),
                                }
                            }
                        }
                    },
                    MatterTab::Tasks => rsx! {
                        crate::project_tasks::PendingHandoffs { rows: view.pending_handoffs.clone() }
                        TasksTab {
                            code: view.code.clone(),
                            csrf: csrf.clone(),
                            tasks: view.tasks.clone(),
                            today: view.today.clone(),
                            assignees: view.task_assignees.clone(),
                            error: view.error.clone(),
                        }
                    },
                    MatterTab::Communications => rsx! { {communications_tab(&view)} {notes_tab(&view)} },
                    MatterTab::Documents => rsx! {
                        ProjectNotationBoard { rows: view.notations.clone() }
                        if !view.envelopes.is_empty() {
                            section { class: "lawyer-detail__section",
                                h2 { "DocuSign envelopes" }
                                ul {
                                    for envelope in &view.envelopes {
                                        li { key: "{envelope.id}",
                                            code { "{envelope.id}" }
                                            " — {envelope.status}"
                                            for document in &envelope.documents {
                                                div { "{document}" }
                                            }
                                            for signer in &envelope.signers {
                                                div { "{signer}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        section { class: "lawyer-detail__section project-documents",
                            h2 { "Documents" }
                            {documents_table(&view)}
                            // Real-time upload progress: `upload-progress.js` finds the
                            // form via this field's id and replays the native submit as
                            // an XHR so it can render `upload.loaded` / `upload.total`
                            // as they arrive — a plain `Field::file(...)` id defaults to
                            // its `name` ("file"), so this needs its own id to stay
                            // unique against any other file field on the page.
                            document::Script { src: "/public/js/upload-progress.js", defer: true }
                            // `upload-progress.js` reads the limits here and shows this
                            // refusal instead of sending a selection over them; the
                            // handler answers the same copy with a 413.
                            p {
                                id: "document-upload-limit",
                                class: "nav-alert nav-alert--danger",
                                role: "alert",
                                hidden: true,
                                "data-max-files": "{view.upload_limits.max_files}",
                                "data-max-bytes": "{view.upload_limits.max_bytes}",
                                "{view.upload_limits.refusal}"
                            }
                            FormCard {
                                title: "Upload documents".to_string(),
                                action: "/app/projects/{view.code}/documents/upload",
                                submit_label: "Upload".to_string(),
                                disclosure: Some(view.documents.is_empty()),
                                multipart: true,
                                csrf_token: Some(csrf.clone()),
                                fields: vec![
                                    Field::file("Files", "file").id("document-upload-file").required().multiple()
                                        .help(view.upload_limits.help.clone()),
                                    Field::select(
                                        "Kind",
                                        "kind",
                                        view.asset_kind_choices
                                            .iter()
                                            .map(|(value, label)| crate::components::Choice::new(value.clone(), label.clone()))
                                            .collect(),
                                        Some("unclassified".to_string()),
                                    )
                                    .required()
                                    .help(
                                        "Required. The stored enum is in parentheses. Applies to every file in this batch.",
                                    ),
                                    Field::text("Description", "description", "")
                                        .placeholder("Letter from Acme Bank dated 2026-05-23")
                                        .help("Optional. Applies to every file in this batch."),
                                    Field::select("Visibility", "visibility",
                                        vec![
                                            crate::components::Choice::new("internal", "Internal (lawyer only)"),
                                            crate::components::Choice::new("client", "Client-visible"),
                                        ],
                                        Some("internal".to_string()))
                                        .help("Applies to every file in this batch. Internal work product — memos, drafts — stays internal."),
                                ],
                            }
                        }
                    },
                    MatterTab::Billing => rsx! {
                        BillingTab {
                            code: view.code.clone(),
                            csrf: csrf.clone(),
                            error: view.error.clone(),
                            confirming: view.confirm_generate,
                            timer: view.running_timer.clone(),
                            entries: view.billing_entries.clone(),
                            claims: view.billing_claims.clone(),
                            annotations: view.billing_annotations.clone(),
                            fees: view.billing_fees.clone(),
                            invoices: view.billing_invoices.clone(),
                            expenses: view.billing_expenses.clone(),
                            trust: view.trust.clone(),
                            generate: view.invoice_generate.clone(),
                            today: view.today.clone(),
                        }

                    },
                    MatterTab::Participants => rsx! {
                        ParticipationTable {
                            code: view.code.clone(),
                            csrf: csrf.clone(),
                            participations: view.participations.clone(),
                            is_admin,
                            may_govern_lawyer_side,
                            may_govern_client_side,
                        }
                    },
                    // The dispatcher 404s a client's testimonial path first.
                    MatterTab::Testimonial => rsx! {},
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "server")]
    #[tokio::test]
    async fn dashboard_postgres_distinguishes_absence_from_configured_failures() {
        use store::postgres::PostgresError;
        const CASE: &str = "NAV_DASHBOARD_POSTGRES_CASE";
        if let Ok(case) = std::env::var(CASE) {
            let result = super::dashboard_postgres().await;
            match case.as_str() {
                "absent" => assert!(matches!(result, Ok(None)), "{result:?}"),
                "incomplete" => assert!(
                    matches!(result, Err(PostgresError::Configuration)),
                    "{result:?}"
                ),
                "unreachable" => assert!(
                    matches!(result, Err(PostgresError::Connection)),
                    "{result:?}"
                ),
                _ => panic!("unknown isolated dashboard PostgreSQL case"),
            }
            return;
        }
        for case in ["absent", "incomplete", "unreachable"] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args([
                "--exact",
                "lawyer_project_detail::tests::dashboard_postgres_distinguishes_absence_from_configured_failures",
                "--nocapture",
            ]).env(CASE, case);
            for key in [
                "NAVIGATOR_POSTGRES_HOST",
                "NAVIGATOR_POSTGRES_PORT",
                "NAVIGATOR_POSTGRES_USER",
                "NAVIGATOR_POSTGRES_PASSWORD",
                "NAVIGATOR_POSTGRES_DATABASE",
                "NAVIGATOR_POSTGRES_SSLMODE",
                "NAVIGATOR_POSTGRES_ROOT_CERT",
            ] {
                command.env_remove(key);
            }
            if case != "absent" {
                command.env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1");
            }
            if case == "unreachable" {
                command
                    .env("NAVIGATOR_POSTGRES_PORT", "1")
                    .env("NAVIGATOR_POSTGRES_USER", "navigator")
                    .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
                    .env("NAVIGATOR_POSTGRES_DATABASE", "synthetic")
                    .env("NAVIGATOR_POSTGRES_SSLMODE", "disable");
            }
            let status = command.status().unwrap();
            assert!(status.success(), "dashboard PostgreSQL case {case} failed");
        }
    }

    #[cfg(feature = "server")]
    #[test]
    fn source_link_uses_the_browser_url_for_its_own_forgejo_binding() {
        let root = "https://forge.example.test";
        for repo in [
            "https://forge.example.test/example/sample-project",
            "https://forge.example.test/example/sample-project.git",
        ] {
            assert_eq!(
                forgejo_repository_url(repo, root, "example", "sample-project").as_deref(),
                Some("https://forge.example.test/example/sample-project")
            );
        }
        for repo in [
            "https://forge.example.test/other-firm/sample-project",
            "https://forge.example.test/example/another-sample-project",
            "https://forge.example.test.evil.test/example/sample-project",
            "https://github.com/example/sample-project",
        ] {
            assert!(forgejo_repository_url(repo, root, "example", "sample-project").is_none());
        }
    }

    #[test]
    fn only_the_open_page_is_marked_current_in_the_toolbar() {
        let current = MatterTab::Billing.href("synthetic");
        for tab in [
            MatterTab::Overview,
            MatterTab::Onboarding,
            MatterTab::Deadlines,
            MatterTab::Billing,
        ] {
            let html =
                dioxus_ssr::render_element(project_page_link(&tab.page_tab("synthetic"), &current));
            assert!(html.contains(&format!(r#"aria-label="{}""#, tab.label())));
            assert!(html.contains("<svg"));
            assert_eq!(
                html.contains(r#"aria-current="page""#),
                tab == MatterTab::Billing
            );
        }
    }

    #[test]
    fn fact_grid_skips_empty_matters() {
        let html = dioxus_ssr::render_element(matter_fact_grid(&LawyerDetailView::default()));
        assert!(!html.contains("lawyer-detail__summary"), "{html}");
        assert!(!html.contains("Open deadlines"), "{html}");
    }

    #[test]
    fn court_review_and_the_sorted_timeline_are_distinct_deadline_views() {
        let calendar = crate::project_calendar::CalendarQuery {
            sort: Some("date".into()),
            dir: Some("desc".into()),
        };
        assert_eq!(
            DeadlineTabView::from_query(Some("court-dates"), &calendar),
            DeadlineTabView::CourtDateReview
        );
        assert_eq!(
            DeadlineTabView::from_query(None, &calendar),
            DeadlineTabView::Timeline {
                sort: "date".into(),
                dir: "desc".into()
            }
        );
        assert_eq!(
            DeadlineTabView::from_query(
                Some("unrecognized"),
                &crate::project_calendar::CalendarQuery::default()
            ),
            DeadlineTabView::Details
        );
    }

    #[test]
    fn fact_grid_renders_the_facts_the_matter_has() {
        let view = LawyerDetailView {
            code: "synthetic".into(),
            today: "2026-11-16".into(),
            tasks: vec![TaskRow {
                state: "in_progress".into(),
                due_on: Some("2026-11-15".into()),
                ..TaskRow::default()
            }],
            deadlines: vec![crate::project_deadlines::DeadlineRow {
                due: "Monday, March 14, 2026".to_string(),
                ..crate::project_deadlines::DeadlineRow::default()
            }],
            trust: crate::portal_project_detail::TrustView {
                any: true,
                held: "$5,000".to_string(),
                ..crate::portal_project_detail::TrustView::default()
            },
            ..Default::default()
        };

        let html = dioxus_ssr::render_element(matter_fact_grid(&view));
        assert!(html.contains("lawyer-detail__summary"), "{html}");
        assert!(
            html.contains(r#"href="/app/projects/synthetic/tasks""#),
            "{html}"
        );
        assert!(html.contains("1 overdue"), "{html}");
        assert!(!html.contains("Next deadline"), "{html}");
        assert!(!html.contains("$5,000"), "{html}");
    }

    #[test]
    fn fact_grid_renders_the_financial_projections() {
        let view = LawyerDetailView {
            wip: Some("$4,210".to_string()),
            outstanding: Some("$1,850".to_string()),
            ..Default::default()
        };

        let html = dioxus_ssr::render_element(matter_fact_grid(&view));
        assert!(html.contains("lawyer-detail__summary"), "{html}");
        assert!(html.contains("Work in progress"), "{html}");
        assert!(html.contains("$4,210"), "{html}");
        assert!(html.contains("Outstanding balance"), "{html}");
        assert!(html.contains("$1,850"), "{html}");
    }

    #[test]
    fn fact_grid_renders_the_unread_correspondence_count_deep_linked() {
        let view = LawyerDetailView {
            code: "acme".to_string(),
            unread_inbox: 2,
            ..Default::default()
        };

        let html = dioxus_ssr::render_element(matter_fact_grid(&view));
        assert!(html.contains("lawyer-detail__summary"), "{html}");
        assert!(html.contains("Unread correspondence"), "{html}");
        // The tile deep-links the matter's own inbox — the existing
        // per-project surface, not a re-built one.
        assert!(
            html.contains(r#"href="/app/projects/acme/inbox""#),
            "{html}"
        );
        assert!(html.contains(">2<"), "{html}");
    }

    #[test]
    fn a_zero_unread_count_leaves_no_correspondence_tile() {
        // Zero unread is a fact the matter does not have — no tile, like
        // every other absent fact in the grid.
        let view = LawyerDetailView {
            code: "acme".to_string(),
            unread_inbox: 0,
            ..Default::default()
        };

        let html = dioxus_ssr::render_element(matter_fact_grid(&view));
        assert!(!html.contains("Unread correspondence"), "{html}");
        assert!(!html.contains("/app/projects/acme/inbox"), "{html}");
    }

    #[test]
    fn a_zero_or_absent_projection_leaves_no_tile() {
        // Absent projections render nothing — the default view, like a
        // deployment without PostgreSQL configured.
        let html = dioxus_ssr::render_element(matter_fact_grid(&LawyerDetailView::default()));
        assert!(!html.contains("Work in progress"), "{html}");
        assert!(!html.contains("Outstanding balance"), "{html}");
    }
}
