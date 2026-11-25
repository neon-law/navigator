//! The lawyer workbench at `/app/lawyer` as a Dioxus component (#956 Phase 4) — the
//! project-first landing page every signed-in lawyer reaches first.
//!
//! The successor to the `views::pages::admin::dashboard`. It carries four
//! sections: the project KPI overview (a conic-gradient status pie plus a
//! paginated, status-filtered list of the caller's matters), upcoming deadlines,
//! the project calendar of appearances, the conflicts section, and the
//! "Details" directory of administrative sub-pages and JSON endpoints.
//!
//! **Conflicts are firm-wide, oriented on the caller's own matters — not
//! scoped to them.** `store::conflicts::findings_for_matters` anchors on the
//! caller's visible matters (the same collection the KPIs and calendar read)
//! but walks the *whole* firm graph from there, so a finding arising out of a
//! matter the caller does not participate in still surfaces. ABA Model Rule
//! 1.10 imputes a conflict firm-wide; narrowing this to the caller's own
//! participation would silently defeat the check it exists to run. See
//! `docs/access-model.md` for the recorded decision.
//!
//! **The project list is the caller's workload, not the firm's.** The loader
//! reads the injected `person_id` and role and goes through
//! [`store::access::visible_projects_as_lawyer`], so every firm tier sees only
//! matters where it holds a firm-side participation row. Owner/Admin route
//! admission and administrative-listing reach do not bypass the matter-surface
//! participation gate. Both the counts and the list derive from that one
//! collection, so the page can never disclose the name of a matter the caller
//! may not see.
//!
//! The Contingency Pipeline card reads that same collection
//! ([`crate::projected_cash::PipelineCard`]), so it never counts a matter the
//! KPI list would hide.
//!
//! The calendar lists current upcoming appearances from each visible matter's
//! docket — a hearing or trial's `scheduled_on`, following a continuance chain.
//! Documents and project rows are not events.

pub mod billing;
mod deadlines;

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::people::ViewerRole;

/// How many matters the KPI list shows per page — the page's pagination.
pub const PROJECTS_PER_PAGE: usize = 10;

/// CRUD admin pages — full list / new / edit / delete surfaces.
const CRUD_PAGES: &[(&str, &str)] = &[
    // People is absent on purpose: the one browser surface that creates or edits
    // a Person is the admin console's `/app/admin/people`, which this workbench's
    // audience does not all reach. A lawyer's Person commands go through
    // `POST /app/api/people`.
    ("/app/admin/entities", "Entities"),
    ("/app/projects", "Projects"),
];

/// Read-only listings — every remaining domain table.
///
/// `entity-types`, `templates`, and `questions` live here because they are
/// seeded by the workspace (the canonical seed, `store/seeds/`) rather than
/// authored from the web UI.
const LISTING_PAGES: &[(&str, &str)] = &[
    ("/app/admin/entity-types", "Entity types"),
    ("/app/admin/templates", "Templates"),
    ("/app/admin/questions", "Questions"),
    ("/app/lawyer/notations", "Notations"),
    ("/app/lawyer/answers", "Answers"),
    ("/app/admin/addresses", "Addresses"),
    ("/app/admin/mailrooms", "Mailrooms"),
    ("/app/admin/letters", "Letters"),
    ("/app/lawyer/assets", "Assets"),
    ("/app/lawyer/person-entity-roles", "Person ↔ entity roles"),
    ("/app/lawyer/person-project-roles", "Person ↔ project roles"),
    ("/app/admin/jurisdictions", "Jurisdictions"),
    ("/app/admin/git-repositories", "Git repositories"),
    ("/app/lawyer/disclosures", "Disclosures"),
    ("/app/lawyer/relationship-logs", "Relationship logs"),
];

const API_ENDPOINTS: &[(&str, &str)] = &[
    ("/app/api/openapi.json", "OpenAPI 3.1 spec"),
    ("/app/api/people", "JSON: /app/api/people"),
    ("/app/api/entities", "JSON: /app/api/entities"),
    ("/app/api/jurisdictions", "JSON: /app/api/jurisdictions"),
    ("/app/api/entity-types", "JSON: /app/api/entity-types"),
];

/// One matter in the KPI list — the stable public code, its name, and where
/// it stands. `state` and `next` come from `workflows::client_phrase_for`,
/// resolved against whichever of the matter's Notations
/// `store::notations::furthest_along_state` picks as furthest along; both are
/// empty when the matter carries no notation to report on (never a generic
/// placeholder phrase).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct ProjectLink {
    pub id: String,
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub next: String,
    /// The earliest open Deadline's date; empty when the matter has none.
    #[serde(default)]
    pub next_due: String,
    /// `pitch` or `active` for an open matter; empty otherwise.
    #[serde(default)]
    pub lifecycle: String,
    /// The pitch's e-signature request, such as "`DocuSign` · out for signature". Empty when none.
    #[serde(default)]
    pub esign: String,
    /// The provider's own page for that request. Empty when none.
    #[serde(default)]
    pub esign_href: String,
}

/// One conflict finding, in a wasm-safe shape (ENG-307). `severity` is
/// `"Block"` or `"Review"`, mirroring `store::conflicts::Severity`; the
/// underlying `store::conflicts::Reason` is not carried separately because
/// `explanation` already states it in prose ("Adverse to a current client:
/// …", "Disclosure on …").
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct ConflictFindingView {
    pub severity: String,
    pub counterparty: String,
    pub explanation: String,
    pub confidence_pct: i32,
}

/// A participating Project's onboarding agreement and its existing review or document page.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct AgreementLink {
    pub title: String,
    pub state: String,
    pub href: String,
}

/// One participating Project, including matters without an agreement record.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct RetainerRow {
    pub code: String,
    pub name: String,
    pub methods: Vec<String>,
    pub agreements: Vec<AgreementLink>,
}

/// The whole rendered dashboard.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct DashboardView {
    pub total_projects: u64,
    /// Open matters awaiting completed `DocuSign` onboarding (`MatterLifecycle` pitch).
    pub pitch_projects: u64,
    /// Open matters with verified completed `DocuSign` onboarding (`MatterLifecycle` active).
    pub active_projects: u64,
    pub closed_projects: u64,
    /// The current page of the status-filtered list.
    pub rows: Vec<ProjectLink>,
    #[serde(default)]
    pub retainers: Vec<RetainerRow>,
    /// `open` or `closed` — which tab is active.
    pub status: String,
    pub page: u64,
    pub total_pages: u64,
    /// The calendar's active sort, carried into every link the page emits so
    /// paginating or re-tabbing the list never resets the calendar's sort.
    pub sort: String,
    pub dir: String,
    pub role: ViewerRole,
    /// The deploy's brand mark for the navbar. `None` when the mounted brand
    /// configures none.
    #[serde(default)]
    pub logo: Option<crate::components::AppLogo>,
    /// The resolved brand's tokens stylesheet href, so the page wears
    /// its own palette rather than the firm's on a non-default host.
    #[serde(default)]
    pub tokens_href: String,
    /// The deploy's firm name, for the document title. Resolved from the
    /// request-scoped branding rather than written into the copy, so a
    /// white-label deploy's tab reads its own name.
    #[serde(default)]
    pub firm_name: String,
    /// Current upcoming appearances across the caller's visible matters.
    #[serde(default)]
    pub calendar_events: Vec<crate::project_calendar::CalendarEvent>,
    /// Open Deadlines across the caller's visible open matters, earliest first.
    #[serde(default)]
    pub deadlines: Vec<deadlines::Entry>,
    #[serde(default = "default_page")]
    pub deadline_page: u64,
    #[serde(default)]
    pub deadline_has_next: bool,
    /// Ranked firm-wide conflict findings anchored on the caller's visible
    /// matters (ENG-307). Empty when the graph raises nothing — rendered as
    /// the section's own empty state, never synthesized.
    #[serde(default)]
    pub conflict_findings: Vec<ConflictFindingView>,
    /// The Contingency Pipeline over the same visible matters.
    #[serde(default)]
    pub pipeline: crate::projected_cash::PipelineCardView,
}

fn default_page() -> u64 {
    1
}

/// The dashboard's query string. All four are lenient: an unrecognised value
/// falls back to the default rather than refusing the request, which is why
/// this page carries no `400`-on-bad-sort pre-handler.
#[derive(Deserialize, Default)]
pub struct DashboardQuery {
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub page: Option<u64>,
    #[serde(default)]
    pub deadline_page: Option<u64>,
}

/// Load the lawyer workbench for the caller: refuse non-lawyer, read the injected
/// `person_id`, and derive both the counts and the visible page of matters from
/// the one access-scoped collection.
#[server]
#[allow(clippy::too_many_lines)]
pub async fn get_lawyer_dashboard() -> Result<DashboardView, ServerFnError> {
    let role = crate::admin_listing::require_lawyer().await?;
    let axum::extract::Query(query) = dioxus_fullstack_core::FullstackContext::extract::<
        axum::extract::Query<DashboardQuery>,
        _,
    >()
    .await?;
    let person_id = crate::admin_listing::injected_person_id().await;
    let surreal = consume_context::<store::surreal::SurrealDb>();
    let projects = firm_overview_projects(&surreal, person_id, store_role(role)).await?;

    let status = match query.status.as_deref() {
        Some("closed") => "closed",
        Some("pitch") => "pitch",
        _ => "open",
    };
    // The pie splits the same three states the matter-row lifecycle pill uses.
    // Archived matters stay out of the chart; an open matter is pitch until
    // the onboarding DocuSign envelope UUID is verified as completed.
    let project_ids: Vec<_> = projects.iter().map(|project| project.id).collect();
    let pg = store::postgres::PostgresDb::application()
        .await
        .map_err(crate::lawyer_project_detail::server_error)?;
    let (verified, pending_esign_result, filed, agreement_rows) = tokio::join!(
        store::onboarding::verified_providers(&pg, &project_ids),
        store::projects::onboarding_esign_pending(&surreal, &projects),
        store::onboarding::filed_signing_methods(&pg, &project_ids),
        store::onboarding::agreements(&pg, &project_ids),
    );
    let mut verified = verified.map_err(crate::lawyer_project_detail::server_error)?;
    let pending_esign = pending_esign_result.unwrap_or_default();
    let mut filed = filed.map_err(crate::lawyer_project_detail::server_error)?;
    let agreement_rows = agreement_rows.map_err(crate::lawyer_project_detail::server_error)?;
    let has_engagement = verified.keys().copied().collect();
    let (pitch_projects, active_projects, closed_projects) =
        kpi_lifecycle_counts(&projects, &has_engagement);

    // Both the counts and the list come from `projects`, which is already
    // filtered through the lawyer lens, so the page cannot name an unrelated
    // matter.
    let filtered: Vec<&store::projects::Project> = projects
        .iter()
        .filter(|p| match status {
            "closed" => p.status == "closed",
            "pitch" => {
                store::projects::execution_lifecycle_label(
                    &p.code,
                    &p.status,
                    has_engagement.contains(&p.id),
                ) == "pitch"
            }
            _ => p.status == "open" && has_engagement.contains(&p.id),
        })
        .collect();
    let total_pages = filtered
        .len()
        .saturating_add(PROJECTS_PER_PAGE - 1)
        .checked_div(PROJECTS_PER_PAGE)
        .unwrap_or(0)
        .max(1);
    let page = usize::try_from(query.page.unwrap_or(1))
        .unwrap_or(usize::MAX)
        .max(1)
        .min(total_pages);
    let first = page.saturating_sub(1).saturating_mul(PROJECTS_PER_PAGE);
    let last = first.saturating_add(PROJECTS_PER_PAGE).min(filtered.len());
    let page_of_projects = &filtered[first..last];
    let today = chrono::Utc::now().date_naive();
    let requested_deadline_page = usize::try_from(query.deadline_page.unwrap_or(1))
        .unwrap_or(usize::MAX)
        .max(1);
    let project_page_ids: Vec<_> = page_of_projects
        .iter()
        .filter(|project| project.status == "open")
        .map(|project| project.id)
        .collect();
    let (states, deadline_result, next_due_by_project) = tokio::join!(
        current_states_for_projects(&surreal, page_of_projects),
        deadlines::load(&projects, today, requested_deadline_page),
        store::project_deadlines::next_upcoming_date_by_project(&project_page_ids, today),
    );
    let current_state_by_project = states?;
    let (deadlines, deadline_has_next) = deadline_result?;
    let next_due_by_project =
        next_due_by_project.map_err(crate::lawyer_project_detail::server_error)?;
    let deadline_page = requested_deadline_page;
    let rows = page_of_projects
        .iter()
        .map(|p| {
            let mut row = pitch_row(
                project_link(p, &current_state_by_project),
                p,
                status,
                has_engagement.contains(&p.id),
                &pending_esign,
            );
            if let Some(next) = next_due_by_project.get(&p.id) {
                row.next_due.clone_from(next);
            }
            row
        })
        .collect();

    let mut agreements = std::collections::HashMap::<_, Vec<_>>::new();
    for row in agreement_rows {
        let href = match (row.notation_id, row.document_id) {
            (Some(id), _) => format!("/app/lawyer/notations/{id}/review"),
            (_, Some(id)) => format!("/app/projects/{}/documents/{id}", row.project),
            _ => continue,
        };
        agreements
            .entry(row.project)
            .or_default()
            .push(AgreementLink {
                title: row.title,
                state: row.state,
                href,
            });
    }
    let mut retainers: Vec<_> = projects
        .iter()
        .map(|project| {
            let confirmed = verified.remove(&project.id).unwrap_or_default();
            let recorded = filed.remove(&project.id).unwrap_or_default();
            let methods = confirmed
                .iter()
                .map(|method| format!("{} · provider confirmed", method.signed_how()))
                .chain(
                    recorded
                        .difference(&confirmed)
                        .map(|method| format!("{} · filed copy", method.signed_how())),
                )
                .collect();
            RetainerRow {
                code: project.code.clone(),
                name: project.name.clone(),
                methods,
                agreements: agreements.remove(&project.code).unwrap_or_default(),
            }
        })
        .collect();
    retainers.sort_by(|left, right| left.code.cmp(&right.code));
    let (calendar_events, conflict_findings, pipeline) = tokio::join!(
        calendar_events_for_projects(&surreal, &projects),
        conflict_findings_for_projects(&surreal, &projects),
        crate::projected_cash::derive::load_pipeline(&surreal, person_id, &projects),
    );
    let calendar_events = calendar_events?;
    let conflict_findings = conflict_findings?;

    Ok(DashboardView {
        firm_name: crate::app_chrome::firm_name_from_context().await,
        total_projects: pitch_projects
            .saturating_add(active_projects)
            .saturating_add(closed_projects),
        pitch_projects,
        active_projects,
        closed_projects,
        rows,
        retainers,
        status: status.to_string(),
        page: u64::try_from(page).unwrap_or(u64::MAX),
        total_pages: u64::try_from(total_pages).unwrap_or(u64::MAX),
        sort: crate::project_calendar::sort_field(
            query.sort.as_deref(),
            crate::project_calendar::WORKBENCH_COLUMNS,
        ),
        dir: crate::project_calendar::sort_dir(query.dir.as_deref()),
        role,
        logo: crate::app_chrome::app_logo_from_context().await,
        tokens_href: crate::app_chrome::app_tokens_href_from_context().await,
        calendar_events,
        deadlines,
        deadline_page: u64::try_from(deadline_page).unwrap_or(u64::MAX),
        deadline_has_next,
        conflict_findings,
        pipeline,
    })
}

#[cfg(feature = "server")]
fn store_role(role: ViewerRole) -> store::persons::Role {
    match role {
        ViewerRole::Owner => store::persons::Role::Owner,
        ViewerRole::Admin => store::persons::Role::Admin,
        ViewerRole::Lawyer => store::persons::Role::Lawyer,
        ViewerRole::Clerk => store::persons::Role::Clerk,
        ViewerRole::Client => store::persons::Role::Client,
    }
}

#[cfg(feature = "server")]
async fn firm_overview_projects(
    surreal: &store::surreal::SurrealDb,
    person_id: Option<uuid::Uuid>,
    store_role: store::persons::Role,
) -> Result<Vec<store::projects::Project>, ServerFnError> {
    // A failed access query is not an empty workload. Commit a real 500 rather
    // than rendering an honest-looking zero-count dashboard over a database
    // that never answered — the handler drew the same line.
    let mut projects = store::access::visible_projects_as_lawyer(surreal, person_id, store_role)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "dashboard: visible_projects_as_lawyer failed");
            dioxus_fullstack_core::FullstackContext::commit_http_status(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                None,
            );
            ServerFnError::new(e.clone())
        })?;

    let host_brand = crate::app_chrome::request_brand().await;
    let firm_id = store::firms::firm_id_for_brand_key(surreal, host_brand.as_str())
        .await
        .map_err(|error| {
            dioxus_fullstack_core::FullstackContext::commit_http_status(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                None,
            );
            ServerFnError::new(error.to_string())
        })?;
    let firm_brands = if let Some(id) = firm_id {
        store::firms::brand_keys_for_firm(surreal, id)
            .await
            .map_err(|error| ServerFnError::new(error.to_string()))?
    } else {
        Vec::new()
    };
    projects.retain(|project| {
        project.firm_id == firm_id
            || (project.firm_id.is_none() && firm_brands.contains(&project.brand))
    });
    Ok(projects)
}

/// Ranked conflict findings anchored on the caller's visible matters
/// (ENG-307). A query failure is a 500 — the same line every other
/// dashboard lookup on this page draws, since an empty section is only
/// honest when the store actually answered.
#[cfg(feature = "server")]
async fn conflict_findings_for_projects(
    surreal: &store::surreal::SurrealDb,
    projects: &[store::projects::Project],
) -> Result<Vec<ConflictFindingView>, ServerFnError> {
    let project_ids: Vec<uuid::Uuid> = projects.iter().map(|project| project.id).collect();
    let findings = store::conflicts::findings_for_matters(surreal, &project_ids)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "dashboard: findings_for_matters failed");
            dioxus_fullstack_core::FullstackContext::commit_http_status(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                None,
            );
            ServerFnError::new(e)
        })?;
    Ok(findings
        .into_iter()
        .map(|finding| ConflictFindingView {
            severity: match finding.severity {
                store::conflicts::Severity::Block => "Block".to_string(),
                store::conflicts::Severity::Review => "Review".to_string(),
            },
            counterparty: finding.counterparty,
            explanation: finding.explanation,
            confidence_pct: finding.confidence_pct,
        })
        .collect())
}

/// One matter's KPI-list row: name and code straight from the Project,
/// State/Next resolved through `workflows::client_phrase_for` against
/// whichever state `current_state_by_project` names for it — empty when the
/// map names none, rather than a generic placeholder phrase.
#[cfg(feature = "server")]
fn project_link(
    project: &store::projects::Project,
    current_state_by_project: &std::collections::HashMap<uuid::Uuid, String>,
) -> ProjectLink {
    let (state, next) = current_state_by_project
        .get(&project.id)
        .map(|state| {
            let phrase = workflows::client_phrase_for(state);
            (
                phrase.where_this_is.to_string(),
                phrase.whats_next.to_string(),
            )
        })
        .unwrap_or_default();
    ProjectLink {
        id: project.id.to_string(),
        code: project.code.clone(),
        name: project.name.clone(),
        state,
        next,
        next_due: String::new(),
        lifecycle: String::new(),
        esign: String::new(),
        esign_href: String::new(),
    }
}

/// Stamp a row's lifecycle and, on the Pitches tab, its e-signature request and provider link.
#[cfg(feature = "server")]
fn pitch_row(
    mut link: ProjectLink,
    project: &store::projects::Project,
    tab: &str,
    official: bool,
    pending: &std::collections::HashMap<uuid::Uuid, store::onboarding::EsignSummary>,
) -> ProjectLink {
    link.lifecycle =
        store::projects::execution_lifecycle_label(&project.code, &project.status, official)
            .to_string();
    if tab == "pitch" {
        if let Some(request) = pending.get(&project.id) {
            let provider = store::signatures::SignatureProvider::from_stored(&request.provider);
            link.esign = esign_pill(provider, &request.state);
            link.esign_href = provider
                .map(|provider| provider.deep_link(&request.provider_id))
                .unwrap_or_default();
        }
    }
    link
}

/// The pill text for a pitch's e-signature request: provider and where it stands.
#[cfg(feature = "server")]
fn esign_pill(provider: Option<store::signatures::SignatureProvider>, state: &str) -> String {
    let provider = provider.map_or("E-signature", |provider| provider.label());
    let state = match state {
        "requested" => "out for signature",
        "declined" => "declined",
        "voided" => "voided",
        "expired" => "expired",
        other => other,
    };
    format!("{provider} · {state}")
}

/// The current workflow state for each of `projects` that carries at least
/// one Notation, keyed by project id — the page's rendered State/Next
/// columns read `workflows::client_phrase_for` against this state. A project
/// absent from the map has no Notation to report on, so its row renders both
/// columns empty rather than a generic placeholder phrase. A query failure is
/// a 500, the same line every other dashboard lookup on this page draws.
#[cfg(feature = "server")]
async fn current_states_for_projects(
    surreal: &store::surreal::SurrealDb,
    projects: &[&store::projects::Project],
) -> Result<std::collections::HashMap<uuid::Uuid, String>, ServerFnError> {
    let project_ids: Vec<uuid::Uuid> = projects.iter().map(|project| project.id).collect();
    let notations = store::notations::list_by_projects(surreal, &project_ids)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "dashboard: list_by_projects failed");
            dioxus_fullstack_core::FullstackContext::commit_http_status(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                None,
            );
            ServerFnError::new(e.to_string())
        })?;
    let mut states_by_project: std::collections::HashMap<uuid::Uuid, Vec<&str>> =
        std::collections::HashMap::new();
    for notation in &notations {
        states_by_project
            .entry(notation.project_id)
            .or_default()
            .push(notation.state.as_str());
    }
    Ok(states_by_project
        .into_iter()
        .filter_map(|(project_id, states)| {
            store::notations::furthest_along_state(states)
                .map(|state| (project_id, state.to_string()))
        })
        .collect())
}

/// Appearances on the caller's visible open matters, labelled with project and
/// entity names. A query failure is a 500 — an empty calendar is only honest
/// when the store answered.
#[cfg(feature = "server")]
async fn calendar_events_for_projects(
    surreal: &store::surreal::SurrealDb,
    projects: &[store::projects::Project],
) -> Result<Vec<crate::project_calendar::CalendarEvent>, ServerFnError> {
    let project_ids: Vec<uuid::Uuid> = projects
        .iter()
        .filter(|project| project.status == "open")
        .map(|project| project.id)
        .collect();
    let appearances = store::cases::current_appearances_for_projects(surreal, &project_ids)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "dashboard: current_appearances_for_projects failed");
            dioxus_fullstack_core::FullstackContext::commit_http_status(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                None,
            );
            ServerFnError::new(e.to_string())
        })?;
    let entity_ids: Vec<uuid::Uuid> = projects.iter().map(|project| project.entity_id).collect();
    let entities = store::entities::find_by_ids(surreal, &entity_ids)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "dashboard: entities for calendar failed");
            dioxus_fullstack_core::FullstackContext::commit_http_status(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                None,
            );
            ServerFnError::new(e.to_string())
        })?;
    let entity_name: std::collections::HashMap<uuid::Uuid, String> = entities
        .into_iter()
        .map(|entity| (entity.id, entity.name))
        .collect();
    let project_by_id: std::collections::HashMap<uuid::Uuid, &store::projects::Project> = projects
        .iter()
        .map(|project| (project.id, project))
        .collect();
    Ok(appearances
        .iter()
        .filter_map(|appearance| {
            let project = project_by_id.get(&appearance.project_id)?;
            let date = appearance.calendar_date();
            Some(crate::project_calendar::CalendarEvent {
                date: date.clone(),
                sort_date: Some(date),
                event: appearance.title.clone(),
                status: appearance.calendar_status().to_string(),
                project: project.name.clone(),
                entity: entity_name
                    .get(&project.entity_id)
                    .cloned()
                    .unwrap_or_default(),
            })
        })
        .collect())
}

/// Pitch / active / closed counts for the KPI pie. Archived matters are
/// excluded. An open matter is pitch until the onboarding `DocuSign` envelope UUID is verified as completed.
#[cfg(feature = "server")]
fn kpi_lifecycle_counts(
    projects: &[store::projects::Project],
    has_engagement: &std::collections::HashSet<uuid::Uuid>,
) -> (u64, u64, u64) {
    let mut pitch = 0_u64;
    let mut active = 0_u64;
    let mut closed = 0_u64;
    for project in projects {
        match store::projects::execution_lifecycle_label(
            &project.code,
            &project.status,
            has_engagement.contains(&project.id),
        ) {
            "closed" => closed = closed.saturating_add(1),
            "active" => {
                active = active.saturating_add(1);
            }
            "pitch" => pitch = pitch.saturating_add(1),
            _ => {}
        }
    }
    (pitch, active, closed)
}

/// The lawyer workbench. Server-side rendered; every control is a real anchor,
/// so the page works without JavaScript.
#[component]
pub fn LawyerDashboard() -> Element {
    let resource = use_server_future(get_lawyer_dashboard)?;

    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        Some(Err(_)) => {
            return rsx! {
                main { id: "lawyer-dashboard", p { "Failed to load the workbench." } }
            }
        }
        None => {
            return rsx! {
                main { id: "lawyer-dashboard", p { "Loading…" } }
            }
        }
    };

    lawyer_dashboard_body(&view)
}

/// The loaded workbench. Split from the component so the tests render a fixed
/// view without standing up the server function.
fn lawyer_dashboard_body(view: &DashboardView) -> Element {
    let view = view.clone();
    let role = view.role;

    rsx! {
        document::Title { "{view.firm_name} | Lawyer Workbench" }
        document::Meta {
            name: "description",
            content: "Neon Law Navigator lawyer project overview.",
        }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{view.tokens_href}" }
        crate::components::AppNavbar {
            destinations: crate::app_chrome::app_destinations(role),
            logo: view.logo.clone(),
        }
        main { id: "lawyer-dashboard", class: "nav-theme",
            header { class: "page-header", h1 { "Lawyer workbench" } a { class: "nav-link", href: billing::PATH, "Billing" } }
            section { id: "lawyer-inbox-card", class: "team-home__cards", aria_label: "Messages",
                article { class: "team-home__card",
                    h2 { class: "team-home__card-title", "Inbox" }
                    p { class: "team-home__card-desc", "See incoming conversations across your Firm’s Brands, check where each message came from, and reply on your assigned Projects." }
                    a { class: "nav-btn nav-btn--primary", href: crate::inbox::INBOX_PATH, "Open Inbox" }
                }
            }
            ProjectKpiOverview { view: view.clone() }
            section { id: "onboarding-agreements", aria_labelledby: "retainer-heading",
                h2 { id: "retainer-heading", "Onboarding retainers" }
                if view.retainers.is_empty() {
                    p { "No participating Projects." }
                } else {
                    for (signed, label) in [(false, "No signed retainer recorded"), (true, "Signed retainer recorded")] {
                        { retainer_group(&view.retainers, signed, label) }
                    }
                }
            }
            deadlines::WorkbenchDeadlines {
                entries: view.deadlines.clone(),
                page: view.deadline_page,
                has_next: view.deadline_has_next,
                status: view.status.clone(),
                sort: view.sort.clone(),
                dir: view.dir.clone(),
                project_page: view.page,
            }
            crate::projected_cash::PipelineCard { view: view.pipeline.clone() }
            // The workbench calendar spans every matter the caller can see, and
            // carries the adjacent list's status so re-sorting one control does
            // not reset the other.
            crate::project_calendar::ProjectCalendar {
                section_class: "lawyer-project-calendar".to_string(),
                heading: "Project calendar".to_string(),
                empty_message: "No project calendar events scheduled.".to_string(),
                columns: crate::project_calendar::WORKBENCH_COLUMNS.to_vec(),
                path: "/app/lawyer".to_string(),
                query_prefix: format!("status={}&", view.status),
                sort: view.sort.clone(),
                dir: view.dir.clone(),
                events: view.calendar_events.clone(),
            }
            ConflictsSection { findings: view.conflict_findings.clone() }
            DashboardDetails { role }
        }
    }
}

fn retainer_group(rows: &[RetainerRow], signed: bool, label: &str) -> Element {
    let projects: Vec<_> = rows
        .iter()
        .filter(|row| row.methods.is_empty() != signed)
        .collect();
    rsx! {
        details { class: "nav-table-wrap",
            summary { "{label} ({projects.len()})" }
            if projects.is_empty() {
                p { "No Projects in this group." }
            } else {
                table { class: "nav-table", aria_label: label,
                    thead { tr { th { scope: "col", "Project" } th { scope: "col", "Retainer" } } }
                    tbody {
                        for project in projects {
                            tr { key: "{project.code}",
                                th { scope: "row",
                                    a { href: crate::sections::page::MatterTab::Onboarding.href(&project.code), "{project.name} ({project.code})" }
                                }
                                td {
                                    for method in &project.methods {
                                        div { "{method}" }
                                    }
                                    if project.agreements.is_empty() && project.methods.is_empty() {
                                        "No onboarding agreement recorded"
                                    }
                                    for agreement in &project.agreements {
                                        div { a { href: "{agreement.href}", "{agreement.title}" } " · {agreement.state}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Format a pie-share CSS percentage with integer maths to four decimals so
/// the emitted style (and its test) is architecture-independent.
fn pie_share_css(part: u64, total: u64) -> String {
    let units = part
        .saturating_mul(1_000_000)
        .saturating_add(total / 2)
        .checked_div(total)
        .unwrap_or(0);
    format!("{}.{:04}%", units / 10_000, units % 10_000)
}

/// The status pie beside the caller's paginated matter list.
#[component]
fn ProjectKpiOverview(view: DashboardView) -> Element {
    // Two cumulative stops: pitch (yellow) from 0, then active (green), then
    // closed (red) fills the remainder. Rounding each end from a running
    // count keeps the stops monotonic.
    let pitch_end = pie_share_css(view.pitch_projects, view.total_projects);
    let active_end = pie_share_css(
        view.pitch_projects.saturating_add(view.active_projects),
        view.total_projects,
    );
    let chart_label = format!(
        "{} total projects: {} pitch, {} active, {} closed",
        view.total_projects, view.pitch_projects, view.active_projects, view.closed_projects
    );
    let pie_class = if view.total_projects == 0 {
        "project-status-pie project-status-pie-empty"
    } else {
        "project-status-pie"
    };
    let pie_style = format!("--project-pitch-end:{pitch_end};--project-active-end:{active_end}");

    rsx! {
        div { class: "project-kpi-overview",
            section { class: "project-kpi-chart-col",
                h2 { "Project KPIs" }
                div { class: "nav-card project-kpi-chart",
                    div { class: "nav-card__body project-kpi-chart__body",
                        div {
                            class: "{pie_class}",
                            role: "img",
                            aria_label: "{chart_label}",
                            style: "{pie_style}",
                        }
                        div { class: "project-kpi-figures",
                            div { class: "project-kpi-total", "{view.total_projects}" }
                            div { class: "nav-muted", "Total projects" }
                            ul { class: "project-status-legend",
                                li {
                                    span {
                                        class: "project-status-key project-status-key-pitch",
                                        aria_hidden: "true",
                                    }
                                    strong { "Pitch projects: " }
                                    "{view.pitch_projects}"
                                }
                                li {
                                    span {
                                        class: "project-status-key project-status-key-active",
                                        aria_hidden: "true",
                                    }
                                    strong { "Active projects: " }
                                    "{view.active_projects}"
                                }
                                li {
                                    span {
                                        class: "project-status-key project-status-key-closed",
                                        aria_hidden: "true",
                                    }
                                    strong { "Closed projects: " }
                                    "{view.closed_projects}"
                                }
                            }
                        }
                    }
                }
            }
            section { class: "project-kpi-list-col",
                h2 { "Projects" }
                ProjectKpiList { view }
            }
        }
    }
}

/// The status-tabbed, paginated list of the caller's matters.
#[component]
fn ProjectKpiList(view: DashboardView) -> Element {
    let tab_class = |name: &str| {
        if view.status == name || (name == "open" && view.status.is_empty()) {
            "nav-tab is-active"
        } else {
            "nav-tab"
        }
    };
    let pitch_class = tab_class("pitch");
    let open_class = tab_class("open");
    let closed_class = tab_class("closed");
    let empty_message = match view.status.as_str() {
        "closed" => "No closed projects in your workload.",
        "pitch" => "No pitches in your workload.",
        _ => "No active projects in your workload.",
    };
    let pitch_href = format!(
        "/app/lawyer?status=pitch&sort={}&dir={}",
        view.sort, view.dir
    );
    let open_href = format!(
        "/app/lawyer?status=open&sort={}&dir={}",
        view.sort, view.dir
    );
    let closed_href = format!(
        "/app/lawyer?status=closed&sort={}&dir={}",
        view.sort, view.dir
    );
    let rows = view.rows.clone();

    rsx! {
        div { class: "nav-card project-kpi-list",
            div { class: "nav-card__body",
                nav { class: "nav-tabs", aria_label: "Project status",
                    a { class: "{pitch_class}", href: "{pitch_href}", "Pitches" }
                    a { class: "{open_class}", href: "{open_href}", "Active" }
                    a { class: "{closed_class}", href: "{closed_href}", "Closed" }
                }
                if rows.is_empty() {
                    p { class: "nav-muted project-kpi-empty", "{empty_message}" }
                } else {
                    div { class: "nav-table-wrap",
                        table { class: "nav-table",
                            thead {
                                tr {
                                    th { scope: "col", "Name" }
                                    th { scope: "col", "State" }
                                    th { scope: "col", "Next" }
                                    th { scope: "col", "Next due" }
                                }
                            }
                            tbody {
                                for row in rows.iter() {
                                    tr {
                                        td {
                                            a { href: "/app/projects/{row.code}", "{row.name}" }
                                            if !row.lifecycle.is_empty() {
                                                " "
                                                span { class: "nav-pill nav-pill--{row.lifecycle}", "{row.lifecycle}" }
                                            }
                                            if !row.esign.is_empty() {
                                                " "
                                                if row.esign_href.is_empty() {
                                                    span { class: "nav-pill", "{row.esign}" }
                                                } else {
                                                    a {
                                                        class: "nav-pill",
                                                        href: "{row.esign_href}",
                                                        rel: "noopener noreferrer",
                                                        target: "_blank",
                                                        "{row.esign}"
                                                    }
                                                }
                                            }
                                        }
                                        td { "{row.state}" }
                                        td { "{row.next}" }
                                        td { "{row.next_due}" }
                                    }
                                }
                            }
                        }
                    }
                }
                ProjectListPagination { view }
            }
        }
    }
}

/// Previous / Page N of M / Next. Renders nothing on a single page.
#[component]
fn ProjectListPagination(view: DashboardView) -> Element {
    if view.total_pages <= 1 {
        return rsx! {};
    }
    let page = view.page.max(1).min(view.total_pages);
    let href = |target: u64| {
        format!(
            "/app/lawyer?status={}&sort={}&dir={}&page={target}",
            view.status, view.sort, view.dir
        )
    };
    let prev_href = href(page.saturating_sub(1));
    let next_href = href(page.saturating_add(1));
    let summary = format!("Page {page} of {}", view.total_pages);

    rsx! {
        nav { class: "nav-pagination", aria_label: "Project list pagination",
            if page > 1 {
                a { class: "nav-pagination__link", href: "{prev_href}", "Previous" }
            } else {
                span { class: "nav-pagination__link is-disabled", aria_disabled: "true", "Previous" }
            }
            span { class: "nav-pagination__status", aria_current: "page", "{summary}" }
            if page < view.total_pages {
                a { class: "nav-pagination__link", href: "{next_href}", "Next" }
            } else {
                span { class: "nav-pagination__link is-disabled", aria_disabled: "true", "Next" }
            }
        }
    }
}

/// The conflicts section (ENG-307): a ranked list of firm-wide conflict
/// findings anchored on the caller's visible matters. Follows the
/// calendar's precedent immediately above — when the graph raises nothing,
/// this renders its own empty state rather than a placeholder invented for
/// class. Each finding links out to the three routes that carry the
/// underlying graph data — Disclosures, Person ↔ entity roles, and
/// Relationship logs — so a lawyer can go investigate it.
#[component]
fn ConflictsSection(findings: Vec<ConflictFindingView>) -> Element {
    rsx! {
        section { class: "lawyer-conflicts",
            h2 { "Conflicts" }
            if findings.is_empty() {
                p { class: "nav-muted", "No conflict findings across your matters." }
            } else {
                ul { class: "conflict-findings",
                    for finding in findings.iter() {
                        li { class: "conflict-finding",
                            span { class: "conflict-finding__severity conflict-finding__severity--{finding.severity.to_lowercase()}",
                                "{finding.severity}"
                            }
                            span { class: "conflict-finding__explanation", "{finding.explanation}" }
                            span { class: "conflict-finding__confidence", "{finding.confidence_pct}% confidence" }
                            nav { class: "conflict-finding__links", aria_label: "Investigate this finding",
                                a { href: "/app/lawyer/disclosures", "Disclosures" }
                                a { href: "/app/lawyer/person-entity-roles", "Person ↔ entity roles" }
                                a { href: "/app/lawyer/relationship-logs", "Relationship logs" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The directory of every administrative sub-page and JSON endpoint.
#[component]
fn DashboardDetails(role: ViewerRole) -> Element {
    rsx! {
        section { class: "admin-details",
            h2 { "Details" }
            div { class: "admin-details__columns",
                div {
                    h3 { class: "admin-details__heading", "Manage" }
                    ul {
                        for (href , label) in CRUD_PAGES.iter() {
                            li { a { href: "{href}", "{label}" } }
                        }
                    }
                }
                div {
                    h3 { class: "admin-details__heading", "Listings" }
                    ul {
                        for (href , label) in LISTING_PAGES.iter() {
                            li { a { href: "{href}", "{label}" } }
                        }
                    }
                }
                div {
                    h3 { class: "admin-details__heading", "Operations" }
                    ul {
                        // The Plausible link is admin-only; a lawyer must
                        // not be offered it. It leaves the app, so it opens
                        // in a new tab with no opener handle.
                        if role.is_admin_tier() {
                            li { a { href: "https://plausible.io/sites", target: "_blank", rel: "noopener", "Plausible" } }
                        }
                        li { a { href: "/app/admin/schedules", "Cron schedules" } }
                    }
                    h3 { class: "admin-details__heading", "JSON API" }
                    ul {
                        for (href , label) in API_ENDPOINTS.iter() {
                            li { a { href: "{href}", "{label}" } }
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
    fn lawyer_workbench_has_a_prominent_inbox_card() {
        let html = dioxus_ssr::render_element(lawyer_dashboard_body(&view()));
        assert!(html.contains("id=\"lawyer-inbox-card\""), "{html}");
        assert!(html.contains("href=\"/app/inbox\""), "{html}");
        assert!(html.contains("Open Inbox"), "{html}");
    }

    #[test]
    fn agreement_list_opens_the_existing_review_page() {
        let mut fixture = view();
        let review = "/app/lawyer/notations/01941f80-0000-7000-8000-000000000001/review";
        fixture.retainers = vec![
            RetainerRow {
                code: "example-awaiting-signature".into(),
                name: "Pending example".into(),
                methods: vec![],
                agreements: vec![AgreementLink {
                    title: "Engagement letter".into(),
                    state: "requested".into(),
                    href: review.into(),
                }],
            },
            RetainerRow {
                code: "example-no-agreement".into(),
                ..Default::default()
            },
            RetainerRow {
                code: "example-filed-agreement".into(),
                methods: vec!["Signed in wet ink".into()],
                ..Default::default()
            },
        ];
        let html = dioxus_ssr::render_element(lawyer_dashboard_body(&fixture));
        assert!(html.contains("No signed retainer recorded (2)"));
        assert!(html.contains("Signed retainer recorded (1)"));
        assert!(html.contains("Signed in wet ink"));
        assert!(html.contains("No onboarding agreement recorded"));
        assert!(html.contains("/app/projects/example-no-agreement/onboarding"));
        assert!(html.contains(&format!("href=\"{review}\"")));
        assert!(
            html.contains("Engagement letter")
                && html.contains("example-awaiting-signature")
                && html.contains("requested")
        );
    }

    fn view() -> DashboardView {
        DashboardView {
            tokens_href: String::new(),
            firm_name: "Neon Law".to_string(),
            retainers: vec![],
            total_projects: 3,
            pitch_projects: 1,
            active_projects: 1,
            closed_projects: 1,
            rows: vec![ProjectLink {
                id: "00000000-0000-0000-0000-000000000001".to_string(),
                code: "acme-contract-review".to_string(),
                name: "Acme contract review".to_string(),
                state: String::new(),
                next: String::new(),
                ..Default::default()
            }],
            status: "open".to_string(),
            page: 1,
            total_pages: 1,
            sort: "date".to_string(),
            dir: "asc".to_string(),
            role: ViewerRole::Lawyer,
            logo: None,
            calendar_events: vec![],
            deadlines: vec![],
            deadline_page: 1,
            deadline_has_next: false,
            conflict_findings: vec![],
            pipeline: crate::projected_cash::PipelineCardView::default(),
        }
    }

    #[test]
    fn the_kpi_pie_reports_pitch_active_and_closed_shares_and_names_itself_for_a_screen_reader() {
        let html = dioxus_ssr::render_element(lawyer_dashboard_body(&view()));
        // The pie is decorative geometry, so the counts must also be readable:
        // the aria-label is the only thing a screen reader gets from it.
        assert!(
            html.contains("aria-label=\"3 total projects: 1 pitch, 1 active, 1 closed\""),
            "{html}"
        );
        // Equal thirds: pitch ends at 1/3, active at 2/3.
        assert!(html.contains("--project-pitch-end:33.3333%"), "{html}");
        assert!(html.contains("--project-active-end:66.6667%"), "{html}");
        assert!(html.contains("project-status-key-pitch"), "{html}");
        assert!(html.contains("project-status-key-active"), "{html}");
        assert!(html.contains("Pitch projects:"), "{html}");
        assert!(html.contains("Active projects:"), "{html}");
        assert!(html.contains("Closed projects:"), "{html}");
    }

    #[test]
    fn an_empty_workload_renders_the_pie_in_its_empty_state() {
        let html = dioxus_ssr::render_element(lawyer_dashboard_body(&DashboardView {
            total_projects: 0,
            pitch_projects: 0,
            active_projects: 0,
            closed_projects: 0,
            rows: vec![],
            ..view()
        }));
        assert!(html.contains("project-status-pie-empty"), "{html}");
        // Dividing by a zero total must not panic or emit a bogus share.
        assert!(html.contains("--project-pitch-end:0.0000%"), "{html}");
        assert!(html.contains("--project-active-end:0.0000%"), "{html}");
        assert!(
            html.contains("No active projects in your workload."),
            "{html}"
        );
    }

    #[test]
    fn the_closed_tab_is_active_and_names_its_own_empty_state() {
        let html = dioxus_ssr::render_element(lawyer_dashboard_body(&DashboardView {
            status: "closed".to_string(),
            rows: vec![],
            ..view()
        }));
        assert!(
            html.contains("No closed projects in your workload."),
            "{html}"
        );
        assert!(html.contains("nav-tab is-active"), "{html}");
    }

    #[test]
    fn pagination_carries_the_calendar_sort_so_the_two_controls_do_not_reset_each_other() {
        let html = dioxus_ssr::render_element(lawyer_dashboard_body(&DashboardView {
            page: 2,
            total_pages: 3,
            sort: "project".to_string(),
            dir: "desc".to_string(),
            ..view()
        }));
        // Both neighbours reachable, each preserving status + sort + dir.
        assert!(
            html.contains("/app/lawyer?status=open&#38;sort=project&#38;dir=desc&#38;page=1"),
            "{html}"
        );
        assert!(
            html.contains("/app/lawyer?status=open&#38;sort=project&#38;dir=desc&#38;page=3"),
            "{html}"
        );
        assert!(html.contains("Page 2 of 3"), "{html}");
    }

    #[test]
    fn a_single_page_offers_no_pagination() {
        let html = dioxus_ssr::render_element(lawyer_dashboard_body(&view()));
        assert!(!html.contains("nav-pagination"), "{html}");
    }

    /// ENG-720: the KPI list carries State and Next columns read from
    /// whichever Notation `store::notations::furthest_along_state` picked
    /// for the row — a matter with no such Notation renders both cells
    /// empty rather than a placeholder phrase.
    #[test]
    fn the_kpi_list_renders_state_and_next_columns() {
        let html = dioxus_ssr::render_element(lawyer_dashboard_body(&DashboardView {
            rows: vec![
                ProjectLink {
                    id: "00000000-0000-0000-0000-000000000001".to_string(),
                    code: "acme-contract-review".to_string(),
                    name: "Acme contract review".to_string(),
                    state: "The firm is reviewing".to_string(),
                    next: "The firm approves it or asks you for changes. Nothing needed from you right now.".to_string(),
                    next_due: "2028-05-03".to_string(),
                    lifecycle: String::new(),
                    esign: String::new(),
                    esign_href: String::new(),
                },
                ProjectLink {
                    id: "00000000-0000-0000-0000-000000000002".to_string(),
                    code: "no-notation-yet".to_string(),
                    name: "No notation yet".to_string(),
                    state: String::new(),
                    next: String::new(),
                ..Default::default()
                },
            ],
            ..view()
        }));
        assert!(html.contains("<th scope=\"col\">State</th>"), "{html}");
        assert!(html.contains("<th scope=\"col\">Next</th>"), "{html}");
        assert!(html.contains("The firm is reviewing"), "{html}");
        assert!(
            html.contains(
                "The firm approves it or asks you for changes. Nothing needed from you right now."
            ),
            "{html}"
        );
        assert!(html.contains("<th scope=\"col\">Next due</th>"), "{html}");
        assert!(html.contains("<td>2028-05-03</td>"), "{html}");
        assert!(html.contains("No notation yet"), "{html}");
        // The empty-state row renders no generic placeholder — the two
        // trailing cells are simply empty.
        assert!(
            html.contains(
                r#"<a href="/app/projects/no-notation-yet">No notation yet</a></td><td></td><td></td><td></td></tr>"#
            ),
            "{html}"
        );
    }

    #[test]
    fn the_calendar_stays_empty_without_appearances_and_its_headers_toggle_direction() {
        // Appearances come from the docket. The seeded matter in `view()` is a
        // witness — it must not appear as an event just because it is listed.
        let html = dioxus_ssr::render_element(lawyer_dashboard_body(&DashboardView {
            sort: "project".to_string(),
            dir: "asc".to_string(),
            ..view()
        }));
        assert!(
            html.contains("No project calendar events scheduled."),
            "{html}"
        );
        // The active column shows its direction and offers the reverse.
        assert!(html.contains("Project (asc)"), "{html}");
        assert!(
            html.contains("/app/lawyer?status=open&#38;sort=project&#38;dir=desc"),
            "{html}"
        );
        // An inactive column offers ascending and carries no marker.
        assert!(
            html.contains("/app/lawyer?status=open&#38;sort=entity&#38;dir=asc"),
            "{html}"
        );
    }

    /// ENG-307: follows the calendar's precedent — a truthfully empty
    /// section renders its own empty state rather than a placeholder.
    #[test]
    fn conflicts_section_renders_its_empty_state_with_no_findings() {
        let html = dioxus_ssr::render_element(lawyer_dashboard_body(&view()));
        assert!(
            html.contains("No conflict findings across your matters."),
            "{html}"
        );
        assert!(!html.contains("conflict-finding__severity"), "{html}");
    }

    /// ENG-307: a ranked finding renders its severity, explanation,
    /// confidence, and the three investigate-further links.
    #[test]
    fn conflicts_section_renders_a_ranked_finding_with_its_investigate_links() {
        let html = dioxus_ssr::render_element(lawyer_dashboard_body(&DashboardView {
            conflict_findings: vec![ConflictFindingView {
                severity: "Block".to_string(),
                counterparty: "Opposing Party".to_string(),
                explanation:
                    "Adverse to a current client: Visible Client —adverse_to→ Opposing Party"
                        .to_string(),
                confidence_pct: 100,
            }],
            ..view()
        }));
        assert!(html.contains("conflict-finding__severity--block"), "{html}");
        assert!(html.contains(">Block<"), "{html}");
        assert!(
            html.contains(
                "Adverse to a current client: Visible Client —adverse_to→ Opposing Party"
            ),
            "{html}"
        );
        assert!(html.contains("100% confidence"), "{html}");
        assert!(html.contains(r#"href="/app/lawyer/disclosures""#), "{html}");
        assert!(
            html.contains(r#"href="/app/lawyer/person-entity-roles""#),
            "{html}"
        );
        assert!(
            html.contains(r#"href="/app/lawyer/relationship-logs""#),
            "{html}"
        );
        assert!(
            !html.contains("No conflict findings across your matters."),
            "{html}"
        );
    }

    #[test]
    fn plausible_is_offered_to_an_admin_and_withheld_from_lawyer() {
        let lawyer = dioxus_ssr::render_element(lawyer_dashboard_body(&view()));
        assert!(!lawyer.contains("plausible.io/sites"), "{lawyer}");
        // Every lawyer still gets the non-admin operations links.
        assert!(lawyer.contains("/app/admin/schedules"), "{lawyer}");

        let admin = dioxus_ssr::render_element(lawyer_dashboard_body(&DashboardView {
            role: ViewerRole::Admin,
            ..view()
        }));
        assert!(
            admin.contains(r#"href="https://plausible.io/sites""#),
            "{admin}"
        );
        assert!(admin.contains(r#"target="_blank""#), "{admin}");
        assert!(admin.contains(r#"rel="noopener""#), "{admin}");
    }

    /// The navbar the workbench renders is the shared `/app` row: the workbench
    /// itself is offered to lawyer (the destination the hand-written nav dropped),
    /// the admin desk only to an admin, and the mark comes from the view.
    #[test]
    fn the_navbar_offers_the_role_appropriate_app_destinations() {
        let lawyer = dioxus_ssr::render_element(lawyer_dashboard_body(&view()));
        assert!(lawyer.contains(r#"href="/app/projects""#), "{lawyer}");
        assert!(lawyer.contains(r#"href="/app/team""#), "{lawyer}");
        assert!(lawyer.contains(r#"href="/auth/logout""#), "{lawyer}");
        assert!(!lawyer.contains(r#"href="/app/admin""#), "{lawyer}");
        assert!(!lawyer.contains("lawyer-nav__brand"), "no mark configured");

        // The row does not grow with authority: the workbench and admin doors
        // are cards on the Team home, so an Admin's navbar is a Lawyer's.
        let admin = dioxus_ssr::render_element(lawyer_dashboard_body(&DashboardView {
            role: ViewerRole::Admin,
            logo: Some(crate::components::AppLogo {
                src: "/public/brand/firm-logo.svg".to_string(),
                href: "/".to_string(),
                brand_name: "Example Law".to_string(),
            }),
            ..view()
        }));
        assert!(!admin.contains(r#"href="/app/admin""#), "{admin}");
        assert!(admin.contains(r#"href="/app/team""#), "{admin}");
        assert!(
            admin.contains(r#"src="/public/brand/firm-logo.svg""#),
            "{admin}"
        );
    }

    #[test]
    fn every_directory_link_is_rendered() {
        let html = dioxus_ssr::render_element(lawyer_dashboard_body(&view()));
        for (href, label) in CRUD_PAGES
            .iter()
            .chain(LISTING_PAGES.iter())
            .chain(API_ENDPOINTS.iter())
        {
            assert!(html.contains(&format!("href=\"{href}\"")), "{href}: {html}");
            assert!(html.contains(label), "{label}: {html}");
        }
    }

    /// The billing and cap-table listings are gone — the Firm bills through
    /// Xero and keeps each cap-table ledger outside Navigator — and their
    /// routes are unmounted. So
    /// is the people index: ENG-304 deleted the `/app/lawyer/people` mirror, and the
    /// one people surface now answers at `/app/admin/people`, which this workbench's
    /// lawyer-tier audience is refused at.
    ///
    /// A nav entry outliving its route is a link straight to a 404, and
    /// `every_directory_link_is_rendered` above cannot catch that: it walks
    /// the same tables this nav is built from, so re-adding an entry would
    /// satisfy it. This names the dead paths directly instead.
    #[test]
    fn the_removed_listings_are_not_advertised() {
        let html = dioxus_ssr::render_element(lawyer_dashboard_body(&view()));
        for href in [
            "/app/lawyer/entity-billing-profiles",
            "/app/lawyer/invoices",
            "/app/lawyer/invoice-line-items",
            "/cap-table",
            "/app/lawyer/people",
        ] {
            assert!(!html.contains(href), "{href} is still linked: {html}");
        }
        // A surviving listing anchors the assertion: the nav renders, so the
        // absences above are removals rather than an empty page.
        assert!(html.contains("/app/lawyer/disclosures"), "{html}");
    }

    #[test]
    fn the_pitches_tab_names_each_pitchs_esign_request_and_links_to_the_provider() {
        let mut fixture = view();
        fixture.status = "pitch".to_string();
        fixture.rows = vec![ProjectLink {
            id: "p".to_string(),
            code: "example".to_string(),
            name: "Example matter".to_string(),
            state: String::new(),
            next: String::new(),
            lifecycle: "pitch".to_string(),
            esign: "DocuSign · out for signature".to_string(),
            esign_href: "https://app.docusign.com/documents/details/abc".to_string(),
            ..Default::default()
        }];
        let html = dioxus_ssr::render_element(lawyer_dashboard_body(&fixture));
        assert!(html.contains("status=pitch"), "{html}");
        assert!(html.contains("DocuSign · out for signature"), "{html}");
        assert!(
            html.contains("https://app.docusign.com/documents/details/abc"),
            "{html}"
        );
    }
}
