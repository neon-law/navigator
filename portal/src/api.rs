//! JSON API commands and reads over the canonical store.

#[path = "billing_report_api.rs"]
mod billing_report_api;
#[path = "firm_assumptions_api.rs"]
mod firm_assumptions_api;
#[path = "firm_envelope_api.rs"]
mod firm_envelope_api;
#[path = "firm_journal_api.rs"]
mod firm_journal_api;
#[path = "legal_plans_api.rs"]
mod legal_plans_api;
#[path = "plan_claims_api.rs"]
mod plan_claims_api;
#[path = "relationship_log_api.rs"]
mod relationship_log_api;

use std::sync::Arc;

use axum::extract::{
    DefaultBodyLimit, FromRef, FromRequest, FromRequestParts, Multipart, Path, Query, Request,
    State,
};
use axum::http::request::Parts;
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use store::relationship_logs::scoped as graph_logs;
use uuid::Uuid;

use crate::people_commands::{PeopleCommandError, UpdateContext};
use crate::session::SessionScope;
use crate::SessionData;
use relationship_log_api::{
    graph_with_observations, has_relationship_log_membership, may_view_firm_graph,
    record_relationship_observation,
};

/// Client-safe Project payload. Firm-private integration coordinates never
/// cross the client API boundary, even when a client may see the Project.
#[derive(Debug, Serialize)]
struct ClientProjectView {
    id: Uuid,
    code: String,
    name: String,
    status: String,
    brand: String,
    entity_id: Uuid,
    description: Option<String>,
    external_slack_channel_url: Option<String>,
    shared_notion_page_url: Option<String>,
    /// Where the client books time. The Project calendar's id stays
    /// firm-side.
    booking_url: Option<String>,
    inserted_at: String,
    updated_at: String,
}

impl From<store::projects::Project> for ClientProjectView {
    fn from(project: store::projects::Project) -> Self {
        Self {
            id: project.id,
            code: project.code,
            name: project.name,
            status: project.status,
            brand: project.brand,
            entity_id: project.entity_id,
            description: project.description,
            external_slack_channel_url: project.external_slack_channel_url,
            shared_notion_page_url: project.shared_notion_page_url,
            booking_url: project.booking_url,
            inserted_at: project.inserted_at,
            updated_at: project.updated_at,
        }
    }
}

fn project_payload(
    role: store::persons::Role,
    project: store::projects::Project,
) -> Result<serde_json::Value, serde_json::Error> {
    if role == store::persons::Role::Client {
        serde_json::to_value(ClientProjectView::from(project))
    } else {
        serde_json::to_value(project)
    }
}

/// State the `/app/api/*` router runs against. Read handlers extract just
/// `State<ApplicationStore>` via the [`FromRef`] below; the People command
/// handlers (delete/update guard the bootstrap Owner, welcome dispatches
/// email) reach the extra seams through `State<ApiState>`.
#[derive(Clone)]
pub struct ApiState {
    /// The store. `persons` lives here, so the `/app/api/people*` surface and
    /// every participant reference read it.
    pub application_store: store::application_store::ApplicationStore,
    pub email: Arc<dyn crate::email::EmailService>,
    pub bootstrap_owner_email: Option<String>,
    /// The operator's firm Entity name (`NAVIGATOR_BOOTSTRAP_COMPANY`),
    /// resolved once at router build. The Entity create command refuses to
    /// mint a second row under it, so a white-label operator's anchor is as
    /// unforkable through the API as through the lawyer form.
    pub bootstrap_company: String,
    /// The questionnaire timeline runtime, threaded through so the matter-open
    /// notation command (`POST /app/api/projects/{id}/notations`) can start a
    /// notation's questionnaire — the same runtime the lawyer notation door and
    /// the retainer walk use.
    pub questionnaire_runtime: Arc<dyn workflows::StateMachineRuntime>,
    /// Object storage, threaded through for the notation command (it persists
    /// the notation's frozen questionnaire snapshot).
    pub storage: Arc<dyn cloud::StorageService>,
    /// The post-questionnaire workflow runtime, for the notation-approve
    /// command (`POST /app/api/notations/{id}/approval`) which fires the `approved`
    /// transition. Distinct from `questionnaire_runtime`: they run different
    /// state machines (see `AppState`).
    pub workflow_runtime: Arc<dyn workflows::StateMachineRuntime>,
    /// The public assets bucket the government-form blank is pulled from during
    /// approve (the AcroForm-fill path).
    pub assets_storage: Arc<dyn cloud::StorageService>,
    /// The vendored government-form registry (AcroForm field maps + pins) the
    /// approve command reads when a template declares a `form:` binding.
    pub forms_registry: Arc<Vec<forms::FormMeta>>,
    /// The e-signature provider, for the send command
    /// (`POST /app/api/notations/{id}/signature`) which dispatches the reviewed
    /// PDF for signature.
    pub signature_provider: Arc<dyn crate::signature::SignatureProvider>,
    /// The contract-review deviation analyzer (the LLM reviewer, or the stub),
    /// for `POST /app/api/projects/{id}/contract-review`.
    pub contract_reviewer: Arc<dyn crate::contract_review::ContractReviewer>,
    /// Resolves one Project's Firm-private provider client from its Firm's
    /// stored credential, for the `/app/api/integrations/*` doors. Refuses on
    /// a deployment with no runtime KMS key — see [`crate::integrations`].
    pub integration_providers: Arc<dyn crate::integrations::IntegrationProviders>,
    /// The billing provider seam — the only door a matter invoice goes
    /// through to Xero ("Xero is the books"). The stub in dev/tests; Xero in
    /// production. See [`crate::project_billing_api`].
    pub billing_provider: Arc<dyn crate::billing::BillingProvider>,
    /// The deployment's canonical host, for links rendered beside stored
    /// rows — the Deadline mirror's matter-page line. `None` locally.
    pub canonical_base_url: Option<String>,
    /// The Jev assigner behind the LAW-346 zero-data-retention gate —
    /// `None` whenever the gate is closed, so no code path can call the
    /// model without both operator flags set. Tests inject a client whose
    /// base URL points at a mock server.
    pub jev: Option<typesafe::JevClient>,
}

impl FromRef<ApiState> for store::application_store::ApplicationStore {
    fn from_ref(s: &ApiState) -> Self {
        s.application_store.clone()
    }
}

/// The single registration table for the `/app/api/*` surface: one row per
/// operation as `(HTTP method, path, handler)`, in OpenAPI path-template
/// form (`{id}`, not axum's `:id`).
///
/// Both [`routes`] (which folds rows sharing a path into one
/// `MethodRouter`) and [`documented_api_operations`] (which reads the
/// `(method, path)` pairs) derive from *this* table — so an operation
/// cannot exist on the router without appearing in the inventory the
/// drift guard checks. That closes the route-drift shape entirely: a new
/// route added here can never be silently absent from
/// `documented_api_operations()`, so `web/tests/openapi_drift.rs` will
/// flag it against the OpenAPI document even if the author forgot the
/// doc. (The live-router probe in `web/tests/routes.rs` can only probe
/// paths it already knows; deriving the inventory from the table is what
/// catches an entirely new undocumented path.)
#[allow(clippy::too_many_lines)]
fn api_operation_table() -> Vec<(&'static str, &'static str, MethodRouter<ApiState>)> {
    use axum::routing::{delete, get, patch, post, put};
    vec![
        ("GET", "/app/api/people", get(list_people)),
        ("GET", "/app/api/ephemeris/sources", get(crate::ephemeris_reference::sources)),
        (
            "POST",
            "/app/api/projects/{id}/astrology-chart",
            post(crate::private_chart::calculate)
                .layer(DefaultBodyLimit::max(crate::private_chart::MAX_BODY_BYTES)),
        ),
        ("POST", "/app/api/people", post(create_person)),
        ("GET", "/app/api/mail-signature", get(mail_signature)),
        ("GET", "/app/api/forms/{code}/blank", get(form_blank)),
        ("POST", "/app/api/firms/{id}/people", post(create_firm_person)),
        ("POST", "/app/api/people/merge", post(merge_people)),
        ("GET", "/app/api/people/{id}", get(get_person)),
        ("PATCH", "/app/api/people/{id}", patch(update_person)),
        ("DELETE", "/app/api/people/{id}", delete(delete_person)),
        ("POST", "/app/api/people/{id}/welcome", post(send_welcome)),
        // A Firm user's own SSH public keys (ENG-1219); revocation also
        // admits that user's Firm administrator. See [`crate::ssh_keys`].
        ("GET", "/app/api/ssh-keys", get(crate::ssh_keys::list)),
        ("POST", "/app/api/ssh-keys", post(crate::ssh_keys::add)),
        (
            "POST",
            "/app/api/ssh-keys/sync",
            post(crate::ssh_keys::sync_own),
        ),
        (
            "DELETE",
            "/app/api/ssh-keys/{id}",
            delete(crate::ssh_keys::revoke),
        ),
        (
            "GET",
            "/app/api/firms/{id}/journal/signals",
            get(firm_journal_api::signals),
        ),
        (
            "POST",
            "/app/api/firms/{id}/journal/solana",
            post(firm_journal_api::solana),
        ),
        (
            "POST",
            "/app/api/firms/{id}/journal/xero",
            post(firm_journal_api::xero),
        ),
        ("GET", "/app/api/entities", get(list_entities)),
        ("GET", "/app/api/firms/{id}/secrets", get(crate::firm_secret_api::list)),
        ("PUT", "/app/api/firms/{id}/secrets/stripe/{account}/{kind}/{mode}", put(crate::firm_secret_api::set).layer(DefaultBodyLimit::max(4096))),
        ("DELETE", "/app/api/firms/{id}/secrets/stripe/{account}/{kind}/{mode}", delete(crate::firm_secret_api::delete)),
        ("GET", "/app/api/firms", get(list_firms)),
        (
            "PATCH",
            "/app/api/firms/{id}",
            patch(crate::firm_admin_api::update),
        ),
        (
            "DELETE",
            "/app/api/firms/{id}",
            delete(crate::firm_admin_api::delete),
        ),
        (
            "DELETE",
            "/app/api/firms/{id}/brands/{key}",
            delete(crate::firm_admin_api::detach_brand),
        ),
        (
            "PUT",
            "/app/api/brands/{key}/firm",
            put(crate::firm_admin_api::move_brand),
        ),
        (
            "DELETE",
            "/app/api/brands/{key}",
            delete(crate::firm_admin_api::delete_brand),
        ),
        (
            "GET",
            "/app/api/firms/{id}/forgejo-parity",
            get(firm_forgejo_parity_door),
        ),
        (
            "POST",
            "/app/api/firms/{id}/forgejo-organization",
            post(crate::firm_forgejo_api::provision),
        ),
        (
            "GET",
            "/app/api/firm-libraries",
            get(crate::firm_forgejo_api::libraries),
        ),
        (
            "GET",
            "/app/api/firms/{id}/journal/audits",
            get(firm_journal_api::audits),
        ),
        (
            "GET",
            "/app/api/firms/{id}/journal/migration-triage",
            get(firm_journal_api::migration_triage),
        ),
        (
            "GET",
            "/app/api/firms/{id}/journal/statement",
            get(firm_journal_api::statement),
        ),
        (
            "GET",
            "/app/api/firms/{id}/journal",
            get(firm_journal_api::list),
        ),
        (
            "GET",
            "/app/api/firms/{id}/assumptions",
            get(firm_assumptions_api::list),
        ),
        (
            "GET",
            "/app/api/firms/{id}/assumptions/{key}/history",
            get(firm_assumptions_api::history),
        ),
        (
            "GET",
            "/app/api/firms/{id}/assumptions/{key}",
            get(firm_assumptions_api::current),
        ),
        (
            "POST",
            "/app/api/firms/{id}/assumptions/{key}",
            post(firm_assumptions_api::set),
        ),
        (
            "GET",
            "/app/api/assumptions",
            get(firm_assumptions_api::catalog),
        ),
        (
            "GET",
            "/app/api/firms/{id}/legal-plans",
            get(legal_plans_api::list),
        ),
        (
            "POST",
            "/app/api/firms/{id}/legal-plans",
            post(legal_plans_api::create),
        ),
        (
            "PUT",
            "/app/api/firms/{id}/legal-plans/configuration",
            put(legal_plans_api::configure),
        ),
        (
            "GET",
            "/app/api/projects/{id}/legal-plan",
            get(legal_plans_api::project),
        ),
        (
            "PATCH",
            "/app/api/projects/{id}/legal-plan",
            patch(legal_plans_api::set_project),
        ),
        ("GET", "/app/api/billing", get(billing_report_api::personal)),
        (
            "GET",
            "/app/api/projects/{id}/billing",
            get(billing_report_api::project),
        ),
        (
            "GET",
            "/app/api/projects/{id}/legal-plan-claims",
            get(plan_claims_api::list),
        ),
        (
            "POST",
            "/app/api/projects/{id}/legal-plan-claims",
            post(plan_claims_api::record),
        ),
        (
            "GET",
            "/app/api/firms/{id}/legal-plan-collections",
            get(plan_claims_api::worklist),
        ),
        (
            "POST",
            "/app/api/firms/{id}/journal",
            post(firm_journal_api::post),
        ),
        (
            "POST",
            "/app/api/firms/{id}/journal/preview",
            post(firm_journal_api::preview),
        ),
        (
            "POST",
            "/app/api/firms/{id}/journal/reconcile",
            post(firm_journal_api::reconcile),
        ),
        (
            "POST",
            "/app/api/firms/{id}/journal/mercury",
            post(firm_journal_api::mercury),
        ),
        (
            "POST",
            "/app/api/firms/{id}/journal/mercury/import",
            post(firm_journal_api::mercury_import),
        ),
        ("POST", "/app/api/entities", post(create_entity)),
        ("POST", "/app/api/seed", post(reconcile_seed)),
        ("POST", "/app/api/blog", post(crate::blog_api::publish)),
        (
            "POST",
            "/app/api/blog/record",
            post(crate::blog_api::record),
        ),
        (
            "POST",
            "/app/api/blog/schedule",
            post(crate::blog_api::schedule),
        ),
        (
            "GET",
            "/app/api/blog/{brand}/{slug}",
            get(crate::blog_api::status),
        ),
        (
            "PUT",
            "/app/api/blog/{brand}/handles/{provider}",
            put(crate::blog_api::handle),
        ),
        (
            "PUT",
            "/app/api/blog/{brand}/{slug}/links/{provider}",
            put(crate::blog_api::link),
        ),
        // The global citation apparatus's write door (ENG-712). Its own
        // noun and module ([`crate::authorities_api`]) — an Authority
        // carries no `project_id`, so it sits beside `people`/`entities`,
        // not under `projects`.
        (
            "POST",
            "/app/api/authorities",
            post(crate::authorities_api::create_authority_door).layer(DefaultBodyLimit::max(
                store::documents::MAX_DOCUMENT_UPLOAD_REQUEST_BYTES,
            )),
        ),
        // LAW-61: correct a field on an existing Authority. Shares the
        // create door's body-size limit since it can carry the same
        // archived-artifact bytes.
        (
            "PATCH",
            "/app/api/authorities",
            patch(crate::authorities_api::update_authority_door).layer(DefaultBodyLimit::max(
                store::documents::MAX_DOCUMENT_UPLOAD_REQUEST_BYTES,
            )),
        ),
        (
            "GET",
            "/app/api/authorities/{id}",
            get(crate::authorities_api::read_filed_authority_door),
        ),
        // LAW-93: read an Authority's archived artifact back down, gated
        // like the write.
        (
            "GET",
            "/app/api/authorities/{id}/archive",
            get(crate::authorities_api::read_authority_archive_door),
        ),
        // The public-assets publish door (ENG-909): `navigator site asset
        // upload`'s authenticated sibling to the ADC-backed
        // `navigator ops assets upload` batch command. No `project_id`, like
        // `authorities` above — a public brand asset is deployment-wide, not
        // matter-scoped.
        (
            "POST",
            "/app/api/assets",
            post(crate::assets_api::upload_asset_door).layer(DefaultBodyLimit::max(
                crate::assets_api::MAX_ASSET_UPLOAD_REQUEST_BYTES,
            )),
        ),
        ("GET", "/app/api/entities/{id}", get(get_entity)),
        ("PATCH", "/app/api/entities/{id}", patch(update_entity)),
        ("DELETE", "/app/api/entities/{id}", delete(delete_entity)),
        ("GET", "/app/api/jurisdictions", get(list_jurisdictions)),
        ("GET", "/app/api/entity-types", get(list_entity_types)),
        // The Firm-private provider doors, on their own noun for the same
        // reason as `project-surfaces` below: nesting a provisioning path
        // under `projects` would make it policy-reachable by a client. See
        // [`crate::integrations_api`].
        (
            "POST",
            "/app/api/integrations/notion/ensure",
            post(crate::integrations_api::notion_ensure_door),
        ),
        (
            "POST",
            "/app/api/integrations/calendar/events",
            post(crate::integrations_api::calendar_events_door),
        ),
        (
            "POST",
            "/app/api/calendars/freebusy",
            post(crate::attorney_calendars_api::freebusy_door),
        ),
        (
            "POST",
            "/app/api/calendars/events",
            post(crate::attorney_calendars_api::events_door),
        ),
        (
            "POST",
            "/app/api/calendars/check",
            post(crate::attorney_calendars_api::check_door),
        ),
        (
            "POST",
            "/app/api/calendars/access",
            post(crate::attorney_calendars_api::access_door),
        ),
        (
            "POST",
            "/app/api/calendars/register",
            post(crate::attorney_calendars_api::register_door),
        ),
        (
            "POST",
            "/app/api/integrations/calendar/ensure",
            post(crate::integrations_api::calendar_ensure_door),
        ),
        (
            "POST",
            "/app/api/integrations/contacts/sync",
            post(crate::integrations_api::contacts_sync_door),
        ),
        (
            "POST",
            "/app/api/integrations/calendar/deadline-mirror",
            post(crate::integrations_api::calendar_deadline_mirror_door),
        ),
        (
            "POST",
            "/app/api/integrations/notion/reconcile",
            post(crate::integrations_api::notion_reconcile_door),
        ),
        (
            "POST",
            "/app/api/integrations/slack/ensure",
            post(crate::integrations_api::slack_ensure_door),
        ),
        (
            "POST",
            "/app/api/integrations/slack/notify",
            post(crate::integrations_api::slack_notify_door),
        ),
        (
            "POST",
            "/app/api/integrations/slack/lifecycle",
            post(crate::integrations_api::slack_lifecycle_door),
        ),
        // Deliberately *not* under `/app/api/projects/`: the policy rule for
        // that prefix admits any authenticated caller up to five segments, so a
        // reconciliation path nested there would be policy-reachable by a
        // client even though the handler refuses one. Its own noun keeps the
        // admin-only rule exact.
        (
            "GET",
            "/app/api/project-repositories",
            get(reconcile_project_repositories_door),
        ),
        // Own noun, same reason as `project-repositories`: the `projects`
        // GET rule admits any authenticated caller up to five segments, and
        // a private-terms reconciliation nested there would be
        // policy-reachable by a client even though the handler refuses one.
        (
            "GET",
            "/app/api/project-private-terms-reconciliation",
            get(reconcile_private_terms_door),
        ),
        ("GET", "/app/api/onboarding-agreements", get(onboarding_agreements)),
        (
            "GET",
            "/app/api/lawyer/deadline-projects",
            get(lawyer_deadline_projects),
        ),
        // Own noun, same reason as `project-repositories`: this reads every
        // matter's lifecycle without granting a matter-content read.
        (
            "GET",
            "/app/api/project-lifecycle",
            get(project_lifecycle_door),
        ),
        (
            "POST",
            "/app/api/project-forgejo-mirrors/{id}",
            post(repair_project_forgejo_mirror_door),
        ),
        (
            "GET",
            "/app/api/project-forgejo-mirrors/{id}",
            get(repair_project_forgejo_mirror_door),
        ),
        // Own noun, same reason as `project-repositories`: the `projects`
        // GET rule admits any authenticated caller up to five segments, and
        // an admin-only reconcile nested there would be policy-reachable by
        // a client even though the handler refuses one.
        (
            "POST",
            "/app/api/project-surfaces/{id}",
            post(reconcile_project_surfaces_door),
        ),
        (
            "GET",
            "/app/api/project-surfaces/{id}",
            get(read_project_handles_door),
        ),
        // Read clusters (#866): the matter-centric reads the portal pages load.
        ("GET", "/app/api/projects", get(list_projects_door)),
        ("GET", "/app/api/projects/{id}", get(get_project_door)),
        (
            "GET",
            "/app/api/projects/{id}/status",
            get(project_status_door),
        ),
        (
            "GET",
            "/app/api/projects/{id}/portal-invitation",
            get(project_portal_invitation_door),
        ),
        (
            "POST",
            "/app/api/projects/{id}/testimonial",
            post(save_testimonial_door),
        ),
        (
            "GET",
            "/app/api/projects/{id}/participants",
            get(list_participants_door),
        ),
        (
            "GET",
            "/app/api/projects/{id}/private-terms",
            get(private_terms_door),
        ),
        (
            "GET",
            "/app/api/projects/{id}/section-counts",
            get(project_section_counts_door),
        ),
        (
            "GET",
            "/app/api/projects/{id}/court-date-proposals",
            get(crate::court_dates_api::list),
        ),
        (
            "POST",
            "/app/api/projects/{id}/court-date-proposals",
            post(crate::court_dates_api::stage),
        ),
        (
            "PATCH",
            "/app/api/projects/{id}/court-date-proposals/{proposal_id}",
            patch(crate::court_dates_api::correct),
        ),
        (
            "POST",
            "/app/api/projects/{id}/court-date-proposals/{proposal_id}/confirm",
            post(crate::court_dates_api::confirm),
        ),
        (
            "POST",
            "/app/api/projects/{id}/court-date-proposals/{proposal_id}/return",
            post(crate::court_dates_api::return_for_correction),
        ),
        (
            "GET",
            "/app/api/projects/{id}/court-date-proposals/{proposal_id}/evidence/{index}",
            get(crate::deadline_evidence::page),
        ),
        (
            "GET",
            "/app/api/projects/{id}/court-date-proposals/{proposal_id}/evidence/{index}/forgejo",
            get(crate::deadline_evidence::forgejo_source),
        ),
        (
            "GET",
            "/app/api/projects/{id}/deadlines",
            get(crate::deadlines_api::list_deadlines),
        ),
        (
            "POST",
            "/app/api/projects/{id}/handoffs",
            post(crate::handoffs_api::prepare),
        ),
        (
            "GET",
            "/app/api/projects/{id}/handoffs/{handoff_id}",
            get(crate::handoffs_api::manifest),
        ),
        (
            "POST",
            "/app/api/projects/{id}/handoffs/{handoff_id}/acknowledge",
            post(crate::handoffs_api::acknowledge),
        ),
        (
            "GET",
            "/app/api/projects/{id}/handoffs/{handoff_id}/bundle",
            get(crate::handoffs_api::bundle),
        ),
        (
            "POST",
            "/app/api/projects/{id}/source-imports",
            post(crate::source_imports_api::request).layer(DefaultBodyLimit::max(
                rules::source_archive::MAX_ARCHIVE_BYTES,
            )),
        ),
        (
            "GET",
            "/app/api/projects/{id}/source-imports/{import_id}",
            get(crate::source_imports_api::status),
        ),
        (
            "GET",
            "/app/api/projects/{id}/recovery-archives",
            get(crate::recovery_archives_api::list),
        ),
        (
            "POST",
            "/app/api/projects/{id}/recovery-archives",
            post(crate::recovery_archives_api::request),
        ),
        (
            "POST",
            "/app/api/projects/{id}/recovery-archives/{generation_id}/verify",
            post(crate::recovery_archives_api::verify),
        ),
        (
            "GET",
            "/app/api/projects/{id}/costs",
            get(crate::project_costs_api::list),
        ),
        (
            "POST",
            "/app/api/projects/{id}/costs",
            post(crate::project_costs_api::create),
        ),
        (
            "POST",
            "/app/api/projects/{id}/costs/from-receipt",
            post(crate::project_costs_api::create_from_receipt),
        ),
        (
            "POST",
            "/app/api/projects/{id}/costs/{cost_id}/confirm",
            post(crate::project_costs_api::confirm),
        ),
        (
            "POST",
            "/app/api/projects/{id}/costs/{cost_id}/invoice",
            post(crate::project_costs_api::link_invoice),
        ),
        (
            "POST",
            "/app/api/projects/{id}/costs/{cost_id}/resolve",
            post(crate::project_costs_api::resolve),
        ),
        (
            "GET",
            "/app/api/projects/{id}/costs/{cost_id}/events",
            get(crate::project_costs_api::events),
        ),
        (
            "POST",
            "/app/api/projects/{id}/invoices",
            post(crate::project_billing_api::generate),
        ),
        (
            "GET",
            "/app/api/projects/{id}/tasks",
            get(crate::project_tasks_api::list),
        ),
        (
            "POST",
            "/app/api/projects/{id}/tasks",
            post(crate::project_tasks_api::create),
        ),
        (
            "POST",
            "/app/api/projects/{id}/tasks/{task_id}/assign",
            post(crate::project_tasks_api::assign),
        ),
        (
            "POST",
            "/app/api/projects/{id}/tasks/{task_id}/transition",
            post(crate::project_tasks_api::transition),
        ),
        ("POST", "/app/api/projects/{id}/billing-annotations", post(crate::time_tracking_api::stage_annotation)),
        (
            "GET",
            "/app/api/projects/{id}/time-entry-proposals",
            get(crate::time_proposals_api::list),
        ),
        (
            "POST",
            "/app/api/projects/{id}/time-entry-proposals",
            post(crate::time_proposals_api::add),
        ),
        (
            "POST",
            "/app/api/projects/{id}/time-entry-proposals/backfill",
            post(crate::time_proposals_api::backfill),
        ),
        (
            "PATCH",
            "/app/api/projects/{id}/time-entry-proposals/{proposal_id}",
            patch(crate::time_proposals_api::edit),
        ),
        (
            "POST",
            "/app/api/projects/{id}/time-entry-proposals/{proposal_id}/approve",
            post(crate::time_proposals_api::approve),
        ),
        (
            "POST",
            "/app/api/projects/{id}/time-entry-proposals/{proposal_id}/discard",
            post(crate::time_proposals_api::discard),
        ),
        (
            "GET",
            "/app/api/projects/{id}/time-entries",
            get(crate::time_tracking_api::list_entries),
        ),
        (
            "POST",
            "/app/api/projects/{id}/time-entries",
            post(crate::time_tracking_api::create_entry),
        ),
        (
            "POST",
            "/app/api/projects/{id}/time-entries/{entry_id}/invoice",
            post(crate::time_tracking_api::mark_invoiced),
        ),
        (
            "POST",
            "/app/api/projects/{id}/time-entries/{entry_id}/write-off",
            post(crate::time_tracking_api::write_off),
        ),
        (
            "GET",
            "/app/api/projects/{id}/timer",
            get(crate::time_tracking_api::running_timer),
        ),
        (
            "POST",
            "/app/api/projects/{id}/timer",
            post(crate::time_tracking_api::start_timer),
        ),
        (
            "POST",
            "/app/api/projects/{id}/timer/stop",
            post(crate::time_tracking_api::stop_timer),
        ),
        (
            "POST",
            "/app/api/projects/{id}/timer/discard",
            post(crate::time_tracking_api::discard_timer),
        ),
        (
            "GET",
            "/app/api/attorney-rates",
            get(crate::time_tracking_api::list_rates),
        ),
        (
            "POST",
            "/app/api/attorney-rates",
            post(crate::time_tracking_api::create_rate),
        ),
        (
            "GET",
            "/app/api/projects/{id}/flat-fee",
            get(crate::time_tracking_api::flat_fee),
        ),
        (
            "POST",
            "/app/api/projects/{id}/flat-fee",
            post(crate::time_tracking_api::set_flat_fee),
        ),
        (
            "GET",
            "/app/api/projects/{id}/contingency-terms",
            get(crate::contingency_api::list_terms),
        ),
        (
            "POST",
            "/app/api/projects/{id}/contingency-terms",
            post(crate::contingency_api::record_term),
        ),
        (
            "GET",
            "/app/api/projects/{id}/outcome-estimates",
            get(crate::contingency_api::list_estimates),
        ),
        (
            "POST",
            "/app/api/projects/{id}/outcome-estimates",
            post(crate::contingency_api::record_estimate),
        ),
        (
            "GET",
            "/app/api/projects/{id}/matter-outcome",
            get(crate::contingency_api::list_outcomes),
        ),
        (
            "POST",
            "/app/api/projects/{id}/matter-outcome",
            post(crate::contingency_api::record_outcome),
        ),
        (
            "GET",
            "/app/api/projects/{id}/labels",
            get(crate::project_labels_api::list),
        ),
        (
            "POST",
            "/app/api/projects/{id}/labels",
            post(crate::project_labels_api::command),
        ),
        (
            "GET",
            "/app/api/projects/{id}/stages",
            get(crate::project_stages_api::list),
        ),
        (
            "POST",
            "/app/api/projects/{id}/stages/set",
            post(crate::project_stages_api::set),
        ),
        (
            "POST",
            "/app/api/projects/{id}/deadlines",
            post(crate::deadlines_api::create_deadline),
        ),
        (
            "PATCH",
            "/app/api/projects/{id}/deadlines/{deadline_id}",
            patch(crate::deadlines_api::update_deadline),
        ),
        (
            "POST",
            "/app/api/projects/{id}/deadlines/{deadline_id}/complete",
            post(crate::deadlines_api::complete_deadline),
        ),
        (
            "POST",
            "/app/api/projects/{id}/deadlines/{deadline_id}/cancel",
            post(crate::deadlines_api::cancel_deadline),
        ),
        (
            "POST",
            "/app/api/projects/{id}/deadlines/{deadline_id}/reopen",
            post(crate::deadlines_api::reopen_deadline),
        ),
        (
            "GET",
            "/app/api/projects/{id}/docket",
            get(crate::docket_api::read_docket),
        ),
        (
            "POST",
            "/app/api/projects/{id}/docket",
            post(crate::docket_api::record_entry),
        ),
        (
            "POST",
            "/app/api/projects/{id}/docket/{entry_id}/link-document",
            post(crate::docket_api::link_document),
        ),
        (
            "POST",
            "/app/api/projects/{id}/docket/{entry_id}/supersede",
            post(crate::docket_api::supersede_entry),
        ),
        (
            "GET",
            "/app/api/projects/{id}/service",
            get(crate::service_api::list_service),
        ),
        (
            "POST",
            "/app/api/projects/{id}/service",
            post(crate::service_api::record_service),
        ),
        (
            "GET",
            "/app/api/projects/{id}/service/{service_id}",
            get(crate::service_api::show_service),
        ),
        ("PUT", "/app/api/projects/{id}/jurisdiction", put(put_project_jurisdiction)),
        (
            "PUT",
            "/app/api/projects/{id}/pages",
            put(crate::pages_api::put_pages),
        ),
        (
            "PUT",
            "/app/api/projects/{id}/matter",
            put(crate::matter_caption_api::put_matter),
        ),
        (
            "GET",
            "/app/api/projects/{id}/fiduciary-roles",
            get(crate::module_records_api::list_fiduciary_roles),
        ),
        (
            "GET",
            "/app/api/projects/{id}/estate-assets",
            get(crate::module_records_api::list_estate_assets),
        ),
        (
            "GET",
            "/app/api/projects/{id}/cap-table-holdings",
            get(crate::module_records_api::list_cap_table_holdings),
        ),
        (
            "GET",
            "/app/api/projects/{id}/communications",
            get(crate::communications_api::list_communications),
        ),
        (
            "POST",
            "/app/api/projects/{id}/communications",
            post(crate::communications_api::create_communication),
        ),
        (
            "GET",
            "/app/api/projects/{id}/efilings",
            get(crate::efilings_api::list_efilings),
        ),
        (
            "POST",
            "/app/api/projects/{id}/efilings",
            post(crate::efilings_api::record_efiling),
        ),
        (
            "POST",
            "/app/api/projects/{id}/efilings/settle-fee",
            post(crate::efilings_api::settle_fee),
        ),
        (
            "GET",
            "/app/api/projects/{id}/notations",
            get(list_notations_door),
        ),
        (
            "GET",
            "/app/api/projects/{id}/notation-inventory",
            get(notation_inventory_door),
        ),
        (
            "GET",
            "/app/api/projects/{id}/finance",
            get(get_finance_door),
        ),
        ("GET", "/app/api/notations/{id}", get(get_notation_door)),
        (
            "GET",
            "/app/api/notations/{id}/review-documents",
            get(list_review_documents_door),
        ),
        (
            "GET",
            "/app/api/projects/{id}/documents",
            get(list_documents_door),
        ),
        (
            "GET",
            "/app/api/projects/{id}/documents/revisions",
            get(list_document_revisions_door),
        ),
        (
            "POST",
            "/app/api/projects/{id}/documents/upload-session/status",
            post(upload_session_status_door),
        ),
        (
            "GET",
            "/app/api/projects/{id}/documents/integrity",
            get(document_integrity_door),
        ),
        (
            "GET",
            "/app/api/projects/{id}/conversation",
            get(get_conversation_door),
        ),
        (
            "GET",
            "/app/api/expunge-requests",
            get(list_expunge_requests_door),
        ),
        ("GET", "/app/api/playbooks", get(list_playbooks_door)),
        ("GET", "/app/api/playbooks/{id}", get(get_playbook_door)),
        (
            "GET",
            "/app/api/contract-reviews/{id}",
            get(get_contract_review_door),
        ),
        ("POST", "/app/api/projects", post(open_project)),
        ("PATCH", "/app/api/projects/{id}", patch(update_project)),
        ("DELETE", "/app/api/projects/{id}", delete(delete_project)),
        ("POST", "/app/api/projects/{id}/close", post(close_matter)),
        (
            "POST",
            "/app/api/projects/{id}/finance/run",
            post(run_finance_door),
        ),
        (
            "POST",
            "/app/api/testimonials/{id}/publish",
            post(publish_testimonial_door),
        ),
        (
            "POST",
            "/app/api/testimonials/{id}/unpublish",
            post(unpublish_testimonial_door),
        ),
        (
            "POST",
            "/app/api/projects/{id}/lifecycle",
            post(transition_matter),
        ),
        (
            "POST",
            "/app/api/projects/{id}/conversation/messages",
            post(post_conversation_message_door),
        ),
        (
            "POST",
            "/app/api/projects/{id}/participants",
            post(add_participant),
        ),
        (
            "POST",
            "/app/api/projects/{id}/relationships/adverse",
            post(record_adversity),
        ),
        ("POST", "/app/api/graph/relationship-log", post(record_relationship_observation)),
        ("GET", "/app/api/graph/project/{id}", get(project_graph)),
        ("GET", "/app/api/graph/lawyer/{id}", get(lawyer_graph)),
        ("GET", "/app/api/graph/firm/{id}", get(firm_graph)),
        (
            "PATCH",
            "/app/api/projects/{id}/participants/{role_id}",
            patch(update_participant),
        ),
        (
            "DELETE",
            "/app/api/projects/{id}/participants/{role_id}",
            delete(remove_participant),
        ),
        (
            "PUT",
            "/app/api/projects/{id}/participants/{role_id}/dri",
            put(designate_participant_dri),
        ),
        (
            "DELETE",
            "/app/api/projects/{id}/participants/{role_id}/dri",
            delete(remove_participant_dri),
        ),
        (
            "POST",
            "/app/api/projects/{id}/notations",
            post(create_notation),
        ),
        (
            "GET",
            "/app/api/notations/{id}/answers",
            get(list_notation_answers_door),
        ),
        (
            "POST",
            "/app/api/notations/{id}/answers",
            post(answer_notation_step),
        ),
        (
            "POST",
            "/app/api/notations/{id}/request-changes",
            post(request_notation_changes_door),
        ),
        (
            "POST",
            "/app/api/notations/{id}/reask",
            post(resubmit_reask_door),
        ),
        (
            "POST",
            "/app/api/notations/{id}/transcript",
            post(transcript_coverage_door),
        ),
        (
            "POST",
            "/app/api/notations/{id}/intake",
            post(send_notation_intake),
        ),
        (
            "GET",
            "/app/api/envelopes/{envelope_id}",
            get(firm_envelope_api::read),
        ),
        (
            "POST",
            "/app/api/envelopes/{envelope_id}/void",
            post(firm_envelope_api::void),
        ),
        (
            "GET",
            "/app/api/projects/{id}/envelopes/{envelope_id}",
            get(lookup_project_envelope),
        ),
        (
            "POST",
            "/app/api/projects/{id}/envelopes/refresh",
            post(refresh_project_envelopes),
        ),
        (
            "POST",
            "/app/api/projects/{id}/envelope-adoptions",
            post(adopt_envelope),
        ),
        (
            "POST",
            "/app/api/notations/{id}/approval",
            post(approve_notation),
        ),
        (
            "POST",
            "/app/api/notations/{id}/signature",
            post(send_notation_signature),
        ),
        (
            "GET",
            "/app/api/notations/{id}/signature",
            get(status_notation_signature),
        ),
        (
            "POST",
            "/app/api/review-documents/{id}/comments",
            post(add_review_comment),
        ),
        (
            "POST",
            "/app/api/documents/{id}/deletion-requests",
            post(create_deletion_request),
        ),
        ("POST", "/app/api/notations/{id}/clauses", post(add_clause)),
        (
            "POST",
            "/app/api/projects/{id}/contract-review",
            post(upload_contract_review),
        ),
        (
            "POST",
            "/app/api/projects/{id}/documents",
            // This route carries base64 JSON, so its request cap is derived
            // from the decoded document limit rather than applied globally.
            post(upload_document_door).layer(DefaultBodyLimit::max(
                store::documents::MAX_DOCUMENT_UPLOAD_REQUEST_BYTES,
            )),
        ),
        (
            "GET",
            "/app/api/inbox/unprocessed",
            get(crate::inbox_api::unprocessed),
        ),
        (
            "POST",
            "/app/api/inbox/process",
            post(crate::inbox_api::process),
        ),
        (
            "POST",
            "/app/api/inbox/attachment",
            post(crate::inbox_api::attachment),
        ),
        (
            "POST",
            "/app/api/inbox/assign-matches",
            post(crate::inbox_api::assign_matches),
        ),
        (
            "POST",
            "/app/api/projects/{id}/documents/upload-session",
            post(create_upload_session_door),
        ),
        (
            "POST",
            "/app/api/projects/{id}/documents/upload-session/chunk",
            // One chunk plus its JSON envelope, never the whole file.
            post(append_upload_chunk_door).layer(DefaultBodyLimit::max(
                store::documents::MAX_UPLOAD_CHUNK_REQUEST_BYTES,
            )),
        ),
        (
            "POST",
            "/app/api/projects/{id}/documents/upload-session/complete",
            post(complete_upload_session_door),
        ),
        (
            "POST",
            "/app/api/projects/{id}/inbox-ingest",
            // This route carries base64 JSON like the document upload door,
            // so its request cap admits the base64 inflation of a max-size
            // raw message; the door re-checks the decoded size itself.
            post(ingest_provider_message_door).layer(DefaultBodyLimit::max(
                crate::inbound_email::MAX_INBOUND_MESSAGE_BYTES / 3 * 4 + 65_536,
            )),
        ),
        (
            "GET",
            "/app/api/projects/{id}/inbox/{conversation_id}/messages/{message_id}/attachments/{asset_id}",
            get(crate::communications_api::download_message_attachment),
        ),
        (
            "PATCH",
            "/app/api/projects/{id}/documents/{asset_id}",
            patch(update_document_door),
        ),
        (
            "POST",
            "/app/api/projects/{id}/documents/{asset_id}/storage",
            post(repair_document_storage_door),
        ),
        (
            "POST",
            "/app/api/projects/{id}/documents/{asset_id}/reclassify",
            post(reclassify_document_door),
        ),
        (
            "POST",
            "/app/api/projects/{id}/documents/{asset_id}/verify-dropbox-sign",
            post(verify_dropbox_sign_door),
        ),
        (
            "POST",
            "/app/api/projects/{id}/documents/{asset_id}/delete",
            post(delete_document_door),
        ),
        (
            "POST",
            "/app/api/projects/{id}/documents/{asset_id}/restore",
            post(restore_document_door),
        ),
        (
            "POST",
            "/app/api/projects/{id}/documents/{asset_id}/hold",
            post(hold_document_door),
        ),
        (
            "POST",
            "/app/api/documents/purge",
            post(purge_documents_door),
        ),
        (
            "POST",
            "/app/api/projects/{id}/mail/file",
            post(mail_file_door),
        ),
        (
            "POST",
            "/app/api/projects/{id}/mail/eml",
            // A legacy CLI still ships the whole raw .eml base64-encoded in
            // one request, so the cap admits the base64 inflation of the
            // largest raw message the door accepts (the inbound-message
            // ceiling); the new CLI's reference requests carry no MIME
            // bytes and stay far under it. An over-limit payload fails as
            // the door's classified JSON — never axum's bare 413.
            post(eml_upload_door).layer(DefaultBodyLimit::max(
                crate::inbound_email::MAX_INBOUND_MESSAGE_BYTES / 3 * 4 + 65_536,
            )),
        ),
        (
            "POST",
            "/app/api/inbox/import/fi",
            post(crate::inbox_import_api::fi_import_door)
                .layer(DefaultBodyLimit::max(store::fi_import::MAX_CSV_BYTES * 2)),
        ),
        (
            "POST",
            "/app/api/inbox/import",
            // Carries base64 payloads up to the document upload ceiling, so
            // its request cap is derived rather than applied globally.
            post(crate::inbox_import_api::inbox_import_door).layer(DefaultBodyLimit::max(
                store::documents::MAX_DOCUMENT_UPLOAD_REQUEST_BYTES,
            )),
        ),
        (
            "PATCH",
            "/app/api/notations/{id}/clauses/{clause_id}",
            patch(edit_clause),
        ),
        (
            "DELETE",
            "/app/api/notations/{id}/clauses/{clause_id}",
            delete(delete_clause),
        ),
        (
            "POST",
            "/app/api/notations/{id}/clauses/{clause_id}/move",
            post(move_clause),
        ),
        // The GitHub-era main-push notice (ENG-957): a Project repository's
        // CI, holding the document-scoped session `project-gate.yml` mints,
        // reports a push to `main` and the worker posts to the Project's own
        // private channel. Kept in its own block: another lane registers
        // routes in this table too.
        (
            "POST",
            "/app/api/projects/{id}/events/main-push",
            post(main_push_event_door),
        ),
        // Each pull-request run reports to the same channel; one message per
        // pull request, edited in place.
        (
            "POST",
            "/app/api/projects/{id}/events/pull-request",
            post(crate::integrations_api::pull_request_event_door),
        ),
        ("POST", "/app/api/playbooks", post(create_playbook_door)),
        ("PUT", "/app/api/playbooks/{id}", put(update_playbook_door)),
        (
            "POST",
            "/app/api/contract-reviews/{id}/findings/{idx}",
            post(save_review_finding_door),
        ),
        (
            "POST",
            "/app/api/contract-reviews/{id}/summary",
            post(save_review_summary_door),
        ),
        (
            "POST",
            "/app/api/contract-reviews/{id}/approve",
            post(approve_review_door),
        ),
        (
            "POST",
            "/app/api/contract-reviews/{id}/reject",
            post(reject_review_door),
        ),
        (
            "POST",
            "/app/api/expunge-requests/{id}/authorize",
            post(authorize_expunge_door),
        ),
        (
            "POST",
            "/app/api/expunge-requests/{id}/deny",
            post(deny_expunge_door),
        ),
        (
            "POST",
            "/app/api/templates/validate",
            post(validate_template),
        ),
        // Draft is Template: the same validator under its other name. The
        // exact path wins over the raw-source `/app/api/drafts/{*path}` GET.
        ("POST", "/app/api/drafts/validate", post(validate_template)),
        (
            "PATCH",
            "/app/api/brands/{key}",
            patch(update_brand_presentation),
        ),
        // The CLI's `notation` render reads a Brand's letterhead row through
        // this door (LAW-341) instead of the deployment's stores directly:
        // one authenticated read of presentation data, the same resolve
        // `/app/admin/brands` saves through.
        (
            "GET",
            "/app/api/brands/{key}/letterhead",
            get(brand_letterhead_door),
        ),
    ]
}

/// Mount every gated `/app/api/*` data route onto a router. Every route here
/// goes behind `require_policy` (embedded Rego policy) in [`crate::bootstrap`]; the
/// public documentation surfaces live in [`doc_routes`], which mounts
/// *outside* that gate.
///
/// Built by folding [`api_operation_table`]: rows sharing a path are
/// merged into one `MethodRouter` (axum panics on a duplicate `.route`
/// for the same path), preserving table order.
pub fn routes() -> Router<ApiState> {
    let mut by_path: Vec<(&'static str, MethodRouter<ApiState>)> = Vec::new();
    for (_method, path, method_router) in api_operation_table() {
        if let Some(slot) = by_path.iter_mut().find(|(p, _)| *p == path) {
            let merged = std::mem::replace(&mut slot.1, MethodRouter::new()).merge(method_router);
            slot.1 = merged;
        } else {
            by_path.push((path, method_router));
        }
    }
    by_path
        .into_iter()
        .fold(Router::new(), |router, (path, method_router)| {
            router.route(path, method_router)
        })
}

/// The API-documentation surfaces — the Swagger UI shell at the
/// `/app/api` root (and its short, publicly-linkable alias `/api`, which is
/// what the site footer's "API" entry names) and the OpenAPI document at
/// `/app/api/openapi.json`. They describe the API but are not the API: they
/// read no client data and call no protected data endpoint themselves.
///
/// [`crate::bootstrap`] mounts all three with no session boundary and no
/// `require_policy` layer at all — they are public. A reader needs no
/// session just to see what the API looks like, the same way a public
/// developer portal publishes its reference without gating the reference
/// itself; what stays gated is [`routes`], the API this documents. "Try it
/// out" against a real operation still runs the full session + policy stack
/// that operation always ran, so an anonymous reader can browse the whole
/// reference and still cannot call anything through it —
/// [`crate::policy::swagger_ui_unauthenticated`] is the friendly 401 that
/// failure renders instead of a redirect an XHR can't follow.
///
/// This mounting was gated once, briefly, and the reversal is deliberate
/// rather than a drift back to an old mistake — see the note at
/// `portal/policy/navigator.rego` naming the #204 incident for why a public
/// *routing* decision here carries none of the risk a public *policy*
/// exemption once did.
///
/// Neither handler reads `Db`, so this router needs no state.
pub fn doc_routes() -> Router {
    Router::new()
        .route("/app/api", axum::routing::get(api_docs))
        .route("/api", axum::routing::get(api_docs))
        .route("/app/api/openapi.json", axum::routing::get(openapi_json))
}

/// Every `/app/api/*` operation this router registers, as `(HTTP method,
/// path)` in OpenAPI path-template form (`{id}`, not axum's `:id`).
///
/// Derived from [`api_operation_table`] — the *same* table [`routes`]
/// builds the router from — so this inventory can never silently omit a
/// registered route. `web/tests/openapi_drift.rs` asserts this exact
/// `(method, path)` set equals the operations in
/// [`crate::openapi::document`], so both a new *method* on an
/// already-listed path (e.g. `PUT /app/api/people`) and an entirely new
/// *path* are caught against the document — the old path-only inventory
/// was blind to method drift, and a hand-maintained list could omit a
/// registered route outright.
///
/// Excludes the `/app/api` shell (and its `/api` alias) and
/// `/app/api/openapi.json`: those are documentation surfaces mounted
/// *outside* the policy gate by [`doc_routes`], not part of the API the
/// document describes.
#[must_use]
pub fn documented_api_operations() -> Vec<(&'static str, &'static str)> {
    api_operation_table()
        .into_iter()
        .map(|(method, path, _handler)| (method, path))
        .collect()
}

/// Static Swagger UI shell, served publicly at the `/app/api` root — the
/// private prefix's own front door, so a reader who lands on the API
/// namespace gets the explorer for the operations beneath it — and again at
/// the short `/api` alias the public footer links. Loads the vendored
/// `swagger-ui-dist` assets from `/public/swagger-ui/` and points the
/// renderer at the sibling, equally public `/app/api/openapi.json`. See
/// [`doc_routes`] for why this and the document both carry no session or
/// policy gate while the operations they describe still carry both. The
/// per-response `Content-Security-Policy` header keeps script execution on
/// the same origin — the whole point of vendoring rather than CDN-loading
/// the dist is so this header can stay strict even on a page anyone can
/// load.
async fn api_docs() -> impl IntoResponse {
    const HTML: &str = include_str!("../../server/public/swagger-ui/index.html");
    (
        [
            (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (
                axum::http::header::CONTENT_SECURITY_POLICY,
                "default-src 'self'; \
                 script-src 'self'; \
                 style-src 'self' 'unsafe-inline'; \
                 img-src 'self' data:; \
                 connect-src 'self'; \
                 frame-ancestors 'none'",
            ),
        ],
        axum::response::Html(HTML),
    )
}

async fn openapi_json(headers: axum::http::HeaderMap) -> Json<serde_json::Value> {
    let authority = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok());
    let base = crate::openapi::base_url_for(authority);
    Json(crate::openapi::document_with_base(&base))
}

/// `GET /app/api/people`: one keyset page of the directory, filtered and
/// sorted per [`store::firms::PEOPLE_DIRECTORY`] ([`crate::list_api`]).
async fn list_people(
    State(application_store): State<store::application_store::ApplicationStore>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> Result<Json<serde_json::Value>, ApiError> {
    let params = crate::list_api::parse(raw.as_deref(), &store::firms::PEOPLE_DIRECTORY)?;
    let page = store::firms::people_directory(
        &application_store,
        &params,
        store::firms::DirectoryScope::Everyone,
    )
    .await
    .map_err(|e| ApiError::Db(e.to_string()))?;
    Ok(crate::list_api::document(
        "/app/api/people",
        &params,
        &page.map(|entry| entry.person),
    ))
}

#[derive(Deserialize)]
struct CreatePersonRequest {
    #[serde(flatten)]
    person: crate::people_commands::CreatePersonCommand,
    #[serde(default)]
    project_id: Option<Uuid>,
}

async fn create_person(
    State(application_store): State<store::application_store::ApplicationStore>,
    LawyerSession(session): LawyerSession,
    JsonOrForm(input): JsonOrForm<CreatePersonRequest>,
) -> Response {
    if session.viewing_as_dri.is_some() {
        return ApiError::Forbidden.into_response();
    }
    match store::people_commands::create_person_for_actor(
        &application_store,
        input.person,
        session.role,
        session.person_id,
        input.project_id,
    )
    .await
    {
        Ok(created) => (StatusCode::CREATED, Json(created)).into_response(),
        Err(PeopleCommandError::Blocked(_)) => ApiError::Forbidden.into_response(),
        Err(error) => mutation_error(error),
    }
}

#[derive(Deserialize)]
struct MailSignatureQuery {
    account: String,
}

async fn mail_signature(
    State(application_store): State<store::application_store::ApplicationStore>,
    LawyerSession(session): LawyerSession,
    Query(query): Query<MailSignatureQuery>,
) -> Result<Json<store::mail_signatures::MailSignature>, ApiError> {
    let person_id = session.person_id.ok_or(ApiError::Forbidden)?;
    let account = store::persons::find_by_any_email(&application_store, &query.account)
        .await
        .map_err(|_| ApiError::Db("Person lookup failed".into()))?;
    if account.is_none_or(|person| person.id != person_id) {
        return Err(ApiError::NotFound);
    }
    let pg = store::postgres::PostgresDb::from_env()
        .await
        .map_err(|_| ApiError::Db("Signature storage unavailable".into()))?;
    let signature = store::mail_signatures::for_person(&application_store, &pg, person_id)
        .await
        .map_err(|_| ApiError::Db("Signature read failed".into()))?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(signature))
}

async fn form_blank(
    State(state): State<ApiState>,
    _lawyer: LawyerSession,
    Path(code): Path<String>,
) -> Result<Response, ApiError> {
    let form = state
        .forms_registry
        .iter()
        .find(|form| form.code == code)
        .ok_or(ApiError::NotFound)?;
    let blank = state
        .assets_storage
        .get(form.object_path)
        .await
        .map_err(|_| ApiError::Db("Form blank storage unavailable".into()))?;
    forms::verify_sha256(form.pinned_sha256(), &blank.bytes)
        .map_err(|_| ApiError::Db("Form blank failed its content pin".into()))?;
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/pdf")],
        blank.bytes,
    )
        .into_response())
}

pub(crate) fn may_create_person_role(
    actor: store::persons::Role,
    requested: store::persons::Role,
) -> bool {
    use store::persons::Role;
    match requested {
        Role::Admin | Role::Owner => actor.is_owner(),
        _ => requested.authority_rank() <= actor.authority_rank(),
    }
}

async fn create_firm_person(
    State(application_store): State<store::application_store::ApplicationStore>,
    AdminSession(session): AdminSession,
    Path(firm_id): Path<Uuid>,
    Json(mut command): Json<crate::people_commands::CreatePersonCommand>,
) -> Result<(StatusCode, Json<store::persons::Person>), ApiError> {
    require_firm_administration(&application_store, &session, firm_id).await?;
    if crate::people_commands::parse_role(&command.role)
        .is_some_and(|role| !may_create_person_role(session.role, role))
    {
        return Err(ApiError::Forbidden);
    }
    command.firm_id = Some(firm_id);
    let person = crate::people_commands::create_person(&application_store, &command)
        .await
        .map_err(|error| match error {
            PeopleCommandError::Db(_) => ApiError::Db("Person storage failed".into()),
            PeopleCommandError::FirmMembership(_) => {
                ApiError::Db("Firm membership grant failed".into())
            }
            other => ApiError::from(other),
        })?;
    Ok((StatusCode::CREATED, Json(person)))
}

async fn update_person(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<crate::people_commands::UpdatePersonCommand>,
) -> Response {
    let ctx = UpdateContext {
        bootstrap_owner_email: state.bootstrap_owner_email.as_deref(),
        actor_role: session.role,
        may_change_roles: may_change_roles(&session),
    };
    match crate::people_commands::update_person(&state.application_store, id, &input, &ctx).await {
        Ok(updated) => Json(updated).into_response(),
        Err(e) => mutation_error(e),
    }
}

async fn delete_person(
    State(state): State<ApiState>,
    _lawyer: LawyerSession,
    Path(id): Path<Uuid>,
) -> Response {
    match crate::people_commands::delete_person(
        &state.application_store,
        id,
        state.bootstrap_owner_email.as_deref(),
    )
    .await
    {
        Ok(deleted) => Json(deleted).into_response(),
        Err(e) => mutation_error(e),
    }
}

/// `POST /app/api/people/merge`: fold a duplicate Person into the one to keep
/// (LAW-169). Both are named by an email they answer to. Scoped sessions
/// (CI seed or document tokens) never reach it.
async fn merge_people(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Json(input): Json<MergePeopleRequest>,
) -> Response {
    use store::person_merge::PersonMergeError;
    if session.scope.is_some() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "scope_violation",
                "message": "a scoped session cannot merge people",
            })),
        )
            .into_response();
    }
    match store::person_merge::merge_by_email_as_actor(
        &state.application_store,
        &input.keep,
        &input.drop,
        session.role,
    )
    .await
    {
        Ok(report) => Json(report).into_response(),
        Err(error) => {
            let (status, code) = match &error {
                PersonMergeError::Governance => (StatusCode::FORBIDDEN, "identity_governance"),
                PersonMergeError::SameRecord => (StatusCode::BAD_REQUEST, "same_person"),
                PersonMergeError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
                PersonMergeError::Blocked => (StatusCode::CONFLICT, "blocked"),
                PersonMergeError::RetainedBillingHistory => {
                    (StatusCode::CONFLICT, "retained_billing_history")
                }
                PersonMergeError::RetainedClaimHistory => {
                    (StatusCode::CONFLICT, "retained_claim_history")
                }
                PersonMergeError::Person(_) | PersonMergeError::Project(_) => {
                    (StatusCode::INTERNAL_SERVER_ERROR, "merge_failed")
                }
            };
            (
                status,
                Json(serde_json::json!({ "error": code, "message": error.to_string() })),
            )
                .into_response()
        }
    }
}

/// The two Persons a merge names, each by an email it answers to.
#[derive(Debug, Deserialize)]
struct MergePeopleRequest {
    keep: String,
    drop: String,
}

async fn send_welcome(
    State(state): State<ApiState>,
    _lawyer: LawyerSession,
    Path(id): Path<Uuid>,
) -> Response {
    let base_url = workflows::email::base_url_from_env();
    match crate::people_commands::send_welcome(
        &state.application_store,
        state.email.as_ref(),
        &base_url,
        id,
    )
    .await
    {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "sent" })),
        )
            .into_response(),
        // A send failure is a typed `502`, not a 5xx page: the caller is a
        // machine, and the Dioxus show view surfaces its own `?notice=` flag
        // from its own native POST to `/app/admin/people/{id}/welcome`.
        Err(PeopleCommandError::SendFailed) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": "send_failed" })),
        )
            .into_response(),
        Err(e) => mutation_error(e),
    }
}

/// A lawyer-tier caller, resolved from the session the auth layers
/// injected. As a [`FromRequestParts`] extractor it runs *before* the
/// body extractor ([`JsonOrForm`], a [`FromRequest`]), so an anonymous
/// (401) or `client` (403) caller is rejected before the body is parsed
/// — a malformed body can never mask an auth failure as a 400. API
/// writes are never anonymous and never `client`; this is the explicit
/// defense-in-depth check that holds even if the embedded Rego policy layer in front is
/// ever misconfigured to allow more through.
///
/// `pub(crate)` (rather than private to this module) so a door that lives in
/// its own module for a namespacing reason — [`crate::authorities_api`], the
/// same way [`crate::integrations_api`] reaches [`AdminSession`] — can gate
/// on the lawyer tier without duplicating this extractor.
pub(crate) struct LawyerSession(pub(crate) SessionData);

impl<S> FromRequestParts<S> for LawyerSession
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        match parts.extensions.get::<SessionData>() {
            None => Err(ApiError::Unauthenticated),
            Some(session) if !session.role.is_lawyer_tier() => Err(ApiError::Forbidden),
            Some(session) => Ok(Self(session.clone())),
        }
    }
}

/// Any authenticated session, of *any* tier — 401 only when there is no
/// session at all. Unlike [`LawyerSession`] it does not gate on `lawyer`,
/// because a few `/app/api` doors are client-writable (a client comments on their
/// own review document). The per-matter scope (client-lens or firm-lens) is
/// then enforced in the handler, so this extractor only proves "someone is
/// logged in".
pub(crate) struct AuthedSession(pub(crate) SessionData);

impl<S> FromRequestParts<S> for AuthedSession
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        match parts.extensions.get::<SessionData>() {
            None => Err(ApiError::Unauthenticated),
            Some(session) => Ok(Self(session.clone())),
        }
    }
}

/// Owner or Admin only. Lawyer is lawyer-tier, not this extractor: brand
/// presentation writes are never a lawyer command.
///
/// Being an extractor rather than a check in the handler body is the point:
/// extractors run before the request body is deserialized, so a caller
/// outside the tier is refused with a 403 whatever their body looked like. A
/// tier check after `Json` reports a malformed body to someone who was never
/// allowed to send one.
pub(crate) struct AdminSession(pub(crate) SessionData);

impl<S> FromRequestParts<S> for AdminSession
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        match parts.extensions.get::<SessionData>() {
            None => Err(ApiError::Unauthenticated),
            Some(session) if session.role.is_admin_tier() => Ok(Self(session.clone())),
            Some(_) => Err(ApiError::Forbidden),
        }
    }
}

/// The presentation fields a brand's colour + font may be edited through.
/// `primary_color` is a free `#rrggbb` hex behind `store::brands`' own WCAG
/// AA gate (ENG-586), not a closed palette id. `font_family` names the CSS
/// family for an already-uploaded font (`typeface = "uploaded"`); uploading
/// the font object itself is native multipart only, never this JSON/form
/// door — a blank `font_family` clears it, the same convention every other
/// text field on this door follows.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateBrandPresentation {
    typeface: String,
    primary_color: String,
    #[serde(default)]
    font_family: String,
}

/// Shared by `PATCH /app/api/brands/{key}` and the native edit form. There is
/// no separate forbidden answer: an out-of-scope brand and a missing brand are
/// both `NotFound`, so neither door discloses that another Firm holds the key
/// (ENG-626).
#[derive(Debug)]
pub enum BrandPresentationError {
    NotFound,
    UnknownChoice(String),
    Internal(String),
}

/// `typeface`/`font_family` are `None` to leave that field unchanged
/// (ENG-659: the native Admin edit form sends this when its Firm has no
/// uploaded font to choose, or none was selected) — `Some(value)` behaves
/// exactly as before, catalog validation included, so `PATCH
/// /app/api/brands/{key}` is unaffected: it always sends `Some`.
pub async fn apply_brand_presentation(
    application_store: &store::application_store::ApplicationStore,
    actor_role: store::persons::Role,
    actor_person_id: Option<uuid::Uuid>,
    key: &str,
    typeface: Option<&str>,
    primary_color: &str,
    font_family: Option<&str>,
) -> Result<store::brands::Brand, BrandPresentationError> {
    let brand = match store::brands::find_by_key_for_actor(
        application_store,
        actor_role,
        actor_person_id,
        key,
    )
    .await
    {
        Ok(brand) => brand,
        Err(
            store::brands::BrandError::NotAuthorized
            | store::brands::BrandError::NoSuchBrand(_)
            | store::brands::BrandError::NoSuchFirm(_),
        ) => return Err(BrandPresentationError::NotFound),
        Err(error) => return Err(BrandPresentationError::Internal(error.to_string())),
    };
    if let Some(typeface) = typeface {
        if typeface != "uploaded" && views::brand::typeface_by_id(typeface).is_none() {
            return Err(BrandPresentationError::UnknownChoice(format!(
                "typeface must be one of: {}, uploaded",
                views::brand::TYPEFACES
                    .iter()
                    .map(|face| face.id)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }
    match store::brands::update(
        application_store,
        actor_role,
        actor_person_id,
        brand.id,
        &store::brands::BrandEdit {
            typeface: typeface.map(|value| Some(value.to_string())),
            primary_color: Some(Some(primary_color.to_string())),
            font_family: font_family.map(|value| (!value.is_empty()).then(|| value.to_string())),
            ..store::brands::BrandEdit::default()
        },
    )
    .await
    {
        Ok(updated) => Ok(updated),
        Err(
            store::brands::BrandError::NotAuthorized
            | store::brands::BrandError::NoSuchBrand(_)
            | store::brands::BrandError::NoSuchFirm(_),
        ) => Err(BrandPresentationError::NotFound),
        Err(
            error @ (store::brands::BrandError::InvalidHex(_)
            | store::brands::BrandError::InsufficientContrast { .. }),
        ) => Err(BrandPresentationError::UnknownChoice(error.user_message())),
        Err(error) => Err(BrandPresentationError::Internal(error.to_string())),
    }
}

async fn update_brand_presentation(
    State(state): State<ApiState>,
    AdminSession(session): AdminSession,
    Path(key): Path<String>,
    JsonOrForm(input): JsonOrForm<UpdateBrandPresentation>,
) -> Response {
    match apply_brand_presentation(
        &state.application_store,
        session.role,
        session.person_id,
        &key,
        Some(&input.typeface),
        &input.primary_color,
        Some(&input.font_family),
    )
    .await
    {
        Ok(updated) => Json(updated).into_response(),
        Err(BrandPresentationError::NotFound) => ApiError::NotFound.into_response(),
        Err(BrandPresentationError::UnknownChoice(message)) => {
            ApiError::MalformedBody(message).into_response()
        }
        Err(BrandPresentationError::Internal(error)) => {
            tracing::error!(error = %error, "api: brand presentation update failed");
            ApiError::Db(error).into_response()
        }
    }
}

/// `GET /app/api/brands/{key}/letterhead` — the resolved letterhead row the
/// Brand's letters wear (LAW-341). The CLI's `notation` render reads this
/// door with the lawyer's `site login` bearer instead of the deployment's
/// stores directly: the render frame travels over the same authenticated
/// HTTPS surface every other CLI read uses, and the resolve is the exact
/// `store::brand_letterheads::for_brand_key` the admin save answers with —
/// so a row served here frames a byte-identical render to the row the
/// stores hold. A Brand with no admin-set row still answers (the resolve
/// applies the Brand's own fallbacks); an unknown key is the same bare 404
/// every brand door gives (ENG-626).
async fn brand_letterhead_door(
    State(state): State<ApiState>,
    _lawyer: LawyerSession,
    Path(key): Path<String>,
) -> Result<Response, ApiError> {
    let pg = store::postgres::PostgresDb::from_env()
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    match store::brand_letterheads::for_brand_key(&state.application_store, &pg, &key).await {
        Ok(Some(row)) => Ok((StatusCode::OK, Json(row)).into_response()),
        // An unknown key is the same bare 404 whether the stores answer
        // None or the typed NotFound.
        Ok(None) | Err(store::brand_letterheads::LetterheadError::NotFound) => {
            Err(ApiError::NotFound)
        }
        Err(error) => Err(ApiError::Db(error.to_string())),
    }
}

/// Whether the caller may set another person's role: Owner/Admin, and not
/// currently in a read-only client-DRI view (that session already acts with
/// the DRI's reach, and every mutation from it is refused regardless).
/// Mirrors the lawyer form's role lock.
fn may_change_roles(session: &SessionData) -> bool {
    session.role.is_admin_tier() && session.viewing_as_dri.is_none()
}

/// Render a command failure as the typed JSON [`ApiError`] with its
/// proper status. Every caller of these endpoints is a machine.
fn mutation_error(error: PeopleCommandError) -> Response {
    ApiError::from(error).into_response()
}

/// Extractor that accepts a body as either JSON (`application/json`,
/// machine clients) or url-encoded form. Lets one command endpoint serve
/// both callers.
pub struct JsonOrForm<T>(pub T);

impl<S, T> FromRequest<S> for JsonOrForm<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let is_json = req
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("application/json"));
        if is_json {
            let Json(value) = Json::<T>::from_request(req, state)
                .await
                .map_err(|rejection| ApiError::MalformedBody(rejection.to_string()))?;
            Ok(Self(value))
        } else {
            let axum::extract::Form(value) = axum::extract::Form::<T>::from_request(req, state)
                .await
                .map_err(|rejection| ApiError::MalformedBody(rejection.to_string()))?;
            Ok(Self(value))
        }
    }
}

async fn get_person(
    State(application_store): State<store::application_store::ApplicationStore>,
    Path(id): Path<Uuid>,
) -> Result<Json<store::persons::Person>, ApiError> {
    store::persons::find_by_id(&application_store, id)
        .await?
        .map(Json)
        .ok_or(ApiError::NotFound)
}

// --- Read clusters (#866) --------------------------------------------------
//
// The matter-centric reads the portal pages load. Each self-scopes to what the
// caller may see: `visible_projects` returns the caller's own matters (the
// directory lens for Owner/Admin, participation for lawyer/clerk, the client's
// own matter for a client); the by-id reads gate on `can_see_project` and
// collapse an out-of-scope resource to a non-disclosing 404. Playbooks and
// contract reviews are firm tools, so they take the lawyer tier.

/// `GET /app/api/project-repositories` — reconcile every Project row against
/// the repository it records.
///
/// Admin-tier only, and the tier is the point rather than a precaution. Every
/// other matter read on this surface is participation-scoped — Owner and Admin
/// included, because `store::access::visible_projects_as_lawyer` grants no
/// silent bypass and privileged reach is a place you navigate to. A
/// reconciliation report is that place: the question is about the whole
/// deployment, so it reads [`store::projects::all`] and gates on
/// `is_admin_tier` instead of pretending a per-caller lens is an inventory.
///
/// What it discloses is one code and one repository URL per matter, and no
/// matter content — which is what makes reading every row proportionate.
///
/// The report also carries `project_codes`: every live code, sorted, including
/// the ones with no finding. That is what the *repository* side reads — a
/// checkout asking whether its declared code names a live row cannot learn it
/// from the findings, because a row that is entirely fine produces none.
///
/// The deployment's forge pair is read where it is available and skipped where
/// it is not. Absent configuration is the local loop and the test suite, not an
/// error: every *failing* finding is computable without it, and the report
/// carries `compared_against_deployment_forge` so a reader is told which
/// comparison did not run rather than reading its absence as agreement.
async fn reconcile_project_repositories_door(
    State(state): State<ApiState>,
    authed: AuthedSession,
) -> Result<Response, ApiError> {
    if !authed.0.role.is_admin_tier() {
        return Err(ApiError::Forbidden);
    }
    let projects = store::projects::all(&state.application_store)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    let rows: Vec<store::project_reconcile::RowUnderReview<'_>> =
        projects.iter().map(Into::into).collect();
    let deployment = cloud::workspace::WorkspaceConfig::from_env().ok();
    let report = store::project_reconcile::reconcile(&rows, deployment.as_ref());
    Ok((StatusCode::OK, Json(report)).into_response())
}

#[derive(Debug, Serialize)]
struct ProjectLifecycleEntry {
    lifecycle: String,
    code: String,
    status: String,
    brand: String,
    firm_id: Option<Uuid>,
    closed_at: Option<String>,
    closure_reason: Option<String>,
    /// Derived, never a stored column — see
    /// [`store::project_surfaces::SourceState`]. Carries no repository
    /// content: not the URL, not the Drive folder id, only which of six
    /// states the three provisioning columns describe together.
    source_state: store::project_surfaces::SourceState,
}

/// `GET /app/api/project-lifecycle` — read Project lifecycle and ownership fields.
///
/// Admin-tier only. This is an oversight read rather than a matter read: it
/// deliberately reads every row and returns only the stable code, lifecycle,
/// Firm/brand, and derived `source_state`, so an
/// operator can compare deployment state without receiving matter content
/// or needing participation rows. `source_state` is computed here rather
/// than widening the row itself with `repository_url` or `drive_folder_id`
/// — this route stays a minimal oversight read, and a repository URL names
/// a matter's source per matter (ENG-466).
async fn project_lifecycle_door(
    State(state): State<ApiState>,
    authed: AuthedSession,
) -> Result<Response, ApiError> {
    if !authed.0.role.is_admin_tier() {
        return Err(ApiError::Forbidden);
    }
    let projects = store::projects::all(&state.application_store)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    let official = store::projects::onboarding_official_sets(&state.application_store, &projects)
        .await
        .map_err(ApiError::Db)?;
    let lifecycle = projects
        .into_iter()
        .map(|project| ProjectLifecycleEntry {
            lifecycle: store::projects::execution_lifecycle_label(
                &project.code,
                &project.status,
                official.contains(&project.id),
            )
            .into(),
            source_state: store::project_surfaces::source_state(&project),
            code: project.code,
            status: project.status,
            brand: project.brand,
            firm_id: project.firm_id,
            closed_at: project.closed_at,
            closure_reason: project.closure_reason,
        })
        .collect::<Vec<_>>();
    Ok((StatusCode::OK, Json(lifecycle)).into_response())
}

/// `POST /app/api/project-surfaces/{id}` — create or adopt the three handles
/// a Project opens with: the documents-bucket prefix, the Drive ingest
/// folder, and the source repository.
///
/// Admin-tier only. Matter-open already runs the same reconcile best-effort
/// and does not roll the open back when Drive or the forge is down; this
/// door is the retry for that pass and for a legacy row that never got one.
/// Missing Drive or forge configuration skips that surface rather than
/// failing. A recorded `repository_url` is left alone.
async fn reconcile_project_surfaces_door(
    State(state): State<ApiState>,
    authed: AuthedSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    if !authed.0.role.is_admin_tier() {
        return Err(ApiError::Forbidden);
    }
    let surfaces = store::project_surfaces::reconcile_from_env(&state.application_store, id)
        .await
        .map_err(surface_error)?;
    Ok((StatusCode::OK, Json(surfaces)).into_response())
}

/// `POST /app/api/project-forgejo-mirrors/{id}` — create or adopt the
/// Forgejo mirror for a GitHub-backed Project and return Forgejo's verified
/// repository URL. The Project's GitHub `repository_url` is never changed.
async fn repair_project_forgejo_mirror_door(
    State(state): State<ApiState>,
    AdminSession(session): AdminSession,
    Path(id): Path<Uuid>,
    method: Method,
) -> Result<Response, ApiError> {
    if session.scope.is_some() {
        return Err(ApiError::NotFound);
    }
    let project = store::projects::find_by_id(&state.application_store, id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?
        .ok_or(ApiError::NotFound)?;
    let origin = project
        .repository_url
        .as_deref()
        .filter(|origin| github_project_origin(origin))
        .ok_or(ApiError::NotFound)?;
    let firm_id = project.firm_id.ok_or(ApiError::NotFound)?;
    if !may_use_mirror(&state, &session, firm_id).await? {
        return Err(ApiError::NotFound);
    }
    let forge = connect_project_mirror(&state, firm_id).await?;
    let repair = if method == Method::GET {
        let repository = find_forgejo_mirror(&forge, &project.code).await?;
        if repository.is_none()
            && !forge
                .can_create_repository()
                .await
                .map_err(MirrorIssue::from_forge)?
        {
            return Err(MirrorIssue::organization_permission().into());
        }
        let status = if repository.is_some() {
            "present"
        } else {
            "missing"
        };
        serde_json::json!({
            "repository_url": repository.map(|repository| repository.url),
            "forgejo_owner": forge.owner(),
            "mirror_status": status,
            "source_repository_url": origin,
            "ci_provider": "github"
        })
    } else {
        repair_forgejo_mirror(&forge, &project.code, origin).await?
    };
    Ok(Json(repair).into_response())
}

/// `GET /app/api/project-surfaces/{id}` — read only the nonsecret coordinates
/// used by `navigator project set --from-live` and the Project gate.
async fn read_project_handles_door(
    State(state): State<ApiState>,
    authed: AuthedSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    if !authed.0.role.is_admin_tier() {
        match &authed.0.scope {
            Some(SessionScope::Document(scope)) if scope.project_id == id => {}
            _ => return Err(ApiError::NotFound),
        }
    }
    let handles = store::project_surfaces::read_handles(&state.application_store, id)
        .await
        .map_err(surface_error)?;
    Ok((StatusCode::OK, Json(handles)).into_response())
}

fn surface_error(error: store::project_surfaces::SurfaceError) -> ApiError {
    match error {
        store::project_surfaces::SurfaceError::NotFound => ApiError::NotFound,
        store::project_surfaces::SurfaceError::Command(error) => ApiError::Project(error),
        store::project_surfaces::SurfaceError::ForgejoUnavailable(missing) => {
            ApiError::ForgejoUnavailable(missing)
        }
        other => ApiError::Db(other.to_string()),
    }
}

/// `GET /app/api/projects` — the matters the caller may see, already scoped.
async fn list_projects_door(
    State(state): State<ApiState>,
    authed: AuthedSession,
) -> Result<Response, ApiError> {
    if let Some(SessionScope::Document(scope)) = &authed.0.scope {
        let project = store::projects::find_by_id(&state.application_store, scope.project_id)
            .await
            .map_err(|error| ApiError::Db(error.to_string()))?
            .filter(|project| project.code == scope.project_code)
            .ok_or(ApiError::NotFound)?;
        let lookup = vec![DocumentProjectLookup {
            id: project.id,
            code: project.code,
            firm_id: project.firm_id,
            brand: project.brand,
            status: project.status,
            repository_url: project.repository_url,
        }];
        return Ok((StatusCode::OK, Json(lookup)).into_response());
    }
    let projects = crate::client_portal::access()
        .visible_projects(&state.application_store, authed.0.person_id, authed.0.role)
        .await
        .map_err(ApiError::Db)?;
    let projects = projects
        .into_iter()
        .map(|project| project_payload(authed.0.role, project))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ApiError::Db(error.to_string()))?;
    Ok((StatusCode::OK, Json(projects)).into_response())
}

/// The only Project fields the document-verification CLI needs before it asks
/// for revision metadata and the coordinates `navigator project gate --ci`
/// reconciles against `navigator.yaml` (status, repository URL, Firm and brand). Keeping
/// this response separate from the human Project payload prevents a CI token
/// from inheriting unrelated fields such as `name`, `entity_id`,
/// `description`, or the Slack/Notion URLs.
#[derive(Debug, Serialize)]
struct DocumentProjectLookup {
    id: Uuid,
    code: String,
    firm_id: Option<Uuid>,
    brand: String,
    status: String,
    repository_url: Option<String>,
}

/// `GET /app/api/projects/{id}` — one matter, or 404 if the caller may not see it.
async fn get_project_door(
    State(state): State<ApiState>,
    authed: AuthedSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    if !can_see(&state, &authed, id).await {
        return Err(ApiError::NotFound);
    }
    let project = store::projects::find_by_id(&state.application_store, id)
        .await
        .ok()
        .flatten()
        .ok_or(ApiError::NotFound)?;
    let remotes = if authed.0.role == store::persons::Role::Client {
        None
    } else {
        Some(repository_remotes(&state, &project, &authed.0).await?)
    };
    let mut payload =
        project_payload(authed.0.role, project).map_err(|error| ApiError::Db(error.to_string()))?;
    if let Some(remotes) = remotes {
        payload["repository_remotes"] = remotes;
    }
    Ok((StatusCode::OK, Json(payload)).into_response())
}

fn github_project_origin(remote: &str) -> bool {
    remote.starts_with("git@github.com:")
        || remote.starts_with("git@ssh.github.com:")
        || url::Url::parse(remote).is_ok_and(|url| {
            matches!(
                (url.scheme(), url.host_str()),
                ("https", Some("github.com")) | ("ssh", Some("github.com" | "ssh.github.com"))
            )
        })
}

async fn repository_remotes(
    state: &ApiState,
    project: &store::projects::Project,
    viewer: &crate::SessionData,
) -> Result<serde_json::Value, ApiError> {
    let origin = project.repository_url.as_deref();
    if !origin.is_some_and(github_project_origin) {
        return Ok(serde_json::json!({
            "origin": origin,
            "forgejo": null,
            "forgejo_status": "not_configured"
        }));
    }
    let mirror = match project.firm_id {
        Some(firm_id) if may_use_mirror(state, viewer, firm_id).await? => {
            match connect_project_mirror(state, firm_id).await {
                Ok(forge) => find_forgejo_mirror(&forge, &project.code).await,
                Err(issue) => Err(issue),
            }
        }
        Some(_) => Err(MirrorIssue::new(
            StatusCode::FORBIDDEN,
            "not_authorized",
            "firm_access_required",
            "Firm integration access is required to verify this mirror.",
            "Ask an administrator of the owning Firm to check your Firm membership.",
        )),
        None => Err(MirrorIssue::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "not_configured",
            "firm_missing",
            "The Project has no owning Firm.",
            "Assign the Project to its owning Firm before retrying.",
        )),
    };
    Ok(forgejo_mirror_payload(origin, mirror))
}

#[derive(Deserialize)]
struct ForgeParityPage {
    after: Option<Uuid>,
}

async fn firm_forgejo_parity_door(
    State(state): State<ApiState>,
    AdminSession(viewer): AdminSession,
    Path(firm): Path<Uuid>,
    Query(page): Query<ForgeParityPage>,
) -> Result<Response, ApiError> {
    use futures::{stream, StreamExt};

    if viewer.scope.is_some() || !may_use_mirror(&state, &viewer, firm).await? {
        return Err(ApiError::NotFound);
    }
    let page_size = 8_u16;
    let max_projects = usize::from(page_size);
    let pg = store::postgres::PostgresDb::application()
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    let mut projects = store::postgres::projects::firm_project_sources_on(
        &pg,
        firm,
        page.after,
        i64::from(page_size) + 1,
    )
    .await
    .map_err(|error| ApiError::Db(error.to_string()))?;
    let next = (projects.len() > max_projects).then(|| projects[max_projects - 1].id);
    projects.truncate(max_projects);
    let forgejo = connect_project_mirror(&state, firm).await;
    let github = cloud::forge::GitHubForge::from_env().ok();
    let enrolled = std::env::var("NAVIGATOR_MAINTENANCE_GITHUB_MIRRORS")
        .ok()
        .and_then(|value| {
            serde_json::from_str::<std::collections::HashMap<Uuid, String>>(&value).ok()
        })
        .unwrap_or_default();
    let rows = stream::iter(projects)
        .map(|project| {
            let forgejo = &forgejo;
            let github = github.as_ref();
            let enrolled = enrolled.get(&project.id);
            async move {
                let result = match tokio::time::timeout(
                    std::time::Duration::from_secs(8),
                    read_forge_parity(&project, forgejo, github, enrolled.map(String::as_str)),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => serde_json::json!({"status": "probe_timeout"}),
                };
                serde_json::json!({"code": project.code, "result": result})
            }
        })
        .buffered(max_projects)
        .collect::<Vec<_>>()
        .await;
    Ok(Json(serde_json::json!({"projects": rows, "next": next})).into_response())
}

async fn read_forge_parity(
    project: &store::postgres::projects::FirmProjectSource,
    forgejo: &Result<cloud::ForgejoForge, MirrorIssue>,
    github: Option<&cloud::forge::GitHubForge>,
    enrolled: Option<&str>,
) -> serde_json::Value {
    use cloud::ForgeService;
    use serde_json::json;

    let forgejo = match forgejo {
        Ok(forgejo) => forgejo,
        Err(issue) => return json!({"status": "forgejo_unavailable", "issue": issue}),
    };
    match find_forgejo_mirror(forgejo, &project.code).await {
        Ok(Some(_)) => {}
        Ok(None) => return json!({"status": "forgejo_missing"}),
        Err(issue) => return json!({"status": "forgejo_unavailable", "issue": issue}),
    }
    let forgejo_main = match forgejo.head_commit_sha(&project.code).await {
        Ok(Some(sha)) => sha,
        Ok(None) => return json!({"status": "forgejo_main_missing"}),
        Err(_) => return json!({"status": "forgejo_unavailable"}),
    };
    let Some(primary) = project.repository_url.as_deref() else {
        return json!({"status": "primary_unrecorded", "forgejo_main": forgejo_main});
    };
    if !valid_git_main(&forgejo_main) {
        return json!({"status": "forgejo_invalid_main"});
    }
    let forgejo_primary = forgejo.verify_repository_url(&project.code, primary);
    let github_source = match (forgejo_primary, enrolled) {
        (true, None) => return json!({"status": "forgejo_only", "forgejo_main": forgejo_main}),
        (true, Some(url)) => url,
        (false, _) => primary,
    };
    let Some(github) = github else {
        return json!({"status": "github_unavailable", "forgejo_main": forgejo_main});
    };
    let github_main = match github.private_main_sha(&project.code, github_source).await {
        Ok(Some(sha)) => sha,
        Ok(None) => return json!({"status": "github_main_missing", "forgejo_main": forgejo_main}),
        Err(cloud::forge::ForgeError::ForeignRepository { .. }) => {
            return json!({"status": "unverified_source", "forgejo_main": forgejo_main});
        }
        Err(_) => return json!({"status": "github_unavailable", "forgejo_main": forgejo_main}),
    };
    if !valid_git_main(&github_main) {
        return json!({"status": "github_invalid_main", "forgejo_main": forgejo_main});
    }
    let status = if github_main.eq_ignore_ascii_case(&forgejo_main) {
        "equal"
    } else {
        "different"
    };
    json!({"status": status, "forgejo_main": forgejo_main, "github_main": github_main})
}

fn valid_git_main(sha: &str) -> bool {
    sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
}

async fn may_use_mirror(
    state: &ApiState,
    viewer: &crate::SessionData,
    firm_id: Uuid,
) -> Result<bool, ApiError> {
    store::firm_capability::resolve(
        &state.application_store,
        viewer.role,
        viewer.person_id,
        firm_id,
        store::firm_capability::FirmCapability::UseIntegrations,
    )
    .await
    .map(store::firm_capability::FirmCapabilityDecision::is_allowed)
    .map_err(|error| ApiError::Db(error.to_string()))
}

async fn connect_project_mirror(
    state: &ApiState,
    firm_id: Uuid,
) -> Result<cloud::ForgejoForge, MirrorIssue> {
    use cloud::forgejo_forge::NAVIGATOR_FORGEJO_ROOT_URL_ENV;
    use store::project_surfaces::SurfaceError;

    let root = std::env::var(NAVIGATOR_FORGEJO_ROOT_URL_ENV)
        .ok()
        .filter(|root| !root.trim().is_empty())
        .ok_or_else(MirrorIssue::root_configuration)?;
    let kms = crate::integrations::runtime_kms_from_env().await;
    store::project_surfaces::forgejo_for_firm(&state.application_store, Some(firm_id), kms.as_ref(), |key| {
        (key == NAVIGATOR_FORGEJO_ROOT_URL_ENV).then(|| root.clone())
    })
    .await
    .map_err(|error| match error {
        SurfaceError::ForgejoUnavailable(_) => MirrorIssue::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unavailable",
            "credential_missing",
            "No owning Firm or active Forgejo access token could be resolved for this Project.",
            "Check the owning Firm and save its Forgejo access token in Integration secrets.",
        ),
        SurfaceError::ForgejoCredential(store::firm_secrets::SecretStoreError::Kms(_)) => {
            MirrorIssue::upstream(
                "credential_decryption_failed",
                "The deployment could not decrypt the Firm's Forgejo access token.",
                "Check the deployment's runtime KMS configuration and workload permissions, then retry.",
            )
        }
        SurfaceError::Forge(cloud::forge::ForgeError::InvalidConfig(
            NAVIGATOR_FORGEJO_ROOT_URL_ENV,
        )) => MirrorIssue::root_configuration(),
        SurfaceError::Forge(error) => MirrorIssue::from_forge(error),
        _ => MirrorIssue::upstream(
            "configuration_unavailable",
            "The deployment could not load the Project's Forgejo configuration.",
            "Check the Firm and integration credential store, then retry.",
        ),
    })
}

async fn find_forgejo_mirror(
    forge: &cloud::ForgejoForge,
    code: &str,
) -> Result<Option<cloud::forge::ForgeRepository>, MirrorIssue> {
    let repository = cloud::ForgeService::find_repository(forge, code)
        .await
        .map_err(MirrorIssue::from_forge)?;
    if let Some(repository) = &repository {
        verify_forgejo_mirror(forge, code, repository)?;
    }
    Ok(repository)
}

fn verify_forgejo_mirror(
    forge: &cloud::ForgejoForge,
    code: &str,
    repository: &cloud::forge::ForgeRepository,
) -> Result<(), MirrorIssue> {
    if forge.verify_repository_url(code, &repository.url) {
        Ok(())
    } else {
        Err(MirrorIssue::identity(
            "repository_url_mismatch",
            "Forgejo returned a repository URL outside the configured root or Project path.",
        ))
    }
}

async fn repair_forgejo_mirror(
    forge: &cloud::ForgejoForge,
    code: &str,
    origin: &str,
) -> Result<serde_json::Value, ApiError> {
    let (repository, outcome) = if let Some(repository) = find_forgejo_mirror(forge, code).await? {
        (repository, "present")
    } else {
        let repository = cloud::ForgeService::ensure_repository(forge, code)
            .await
            .map_err(MirrorIssue::from_forge)?;
        verify_forgejo_mirror(forge, code, &repository)?;
        (repository, "created")
    };
    Ok(serde_json::json!({
        "repository_url": repository.url,
        "forgejo_owner": forge.owner(),
        "mirror_status": outcome,
        "source_repository_url": origin,
        "ci_provider": "github"
    }))
}

fn forgejo_mirror_payload(
    origin: Option<&str>,
    mirror: Result<Option<cloud::forge::ForgeRepository>, MirrorIssue>,
) -> serde_json::Value {
    let (url, status, issue) = match mirror {
        Ok(Some(repository)) => (Some(repository.url), "verified", None),
        Ok(None) => (
            None,
            "missing",
            Some(MirrorIssue::new(
                StatusCode::NOT_FOUND,
                "missing",
                "repository_not_found",
                "Forgejo did not return the Project repository for the Firm's access token.",
                "Confirm the token can read the Firm's repositories. If the mirror is absent, ask a Firm administrator to run navigator project repository repair-mirror.",
            )),
        ),
        Err(issue) => (None, issue.status, Some(issue)),
    };
    serde_json::json!({
        "origin": origin,
        "forgejo": url,
        "forgejo_status": status,
        "forgejo_issue": issue
    })
}

/// Public diagnostics describe a verified failure without returning provider
/// bodies, credentials or deployment coordinates.
#[derive(Debug, Serialize)]
pub struct MirrorIssue {
    #[serde(skip)]
    http_status: StatusCode,
    #[serde(skip)]
    status: &'static str,
    code: &'static str,
    message: &'static str,
    remedy: &'static str,
}

impl MirrorIssue {
    fn new(
        http_status: StatusCode,
        status: &'static str,
        code: &'static str,
        message: &'static str,
        remedy: &'static str,
    ) -> Self {
        Self {
            http_status,
            status,
            code,
            message,
            remedy,
        }
    }

    fn upstream(code: &'static str, message: &'static str, remedy: &'static str) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            code,
            message,
            remedy,
        )
    }

    fn root_configuration() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "not_configured",
            "root_url_unconfigured",
            "The deployment has no usable Forgejo root URL.",
            "Set NAVIGATOR_FORGEJO_ROOT_URL to the deployment's Forgejo root and retry.",
        )
    }

    fn identity(code: &'static str, message: &'static str) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "unavailable",
            code,
            message,
            "Confirm the repository's private visibility, Firm organization and Project code in Forgejo, then retry.",
        )
    }

    fn organization_permission() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "unavailable",
            "organization_permission_denied",
            "Forgejo did not grant the Firm token permission to create repositories in the configured organization.",
            "Check the token scopes and grant its account repository creation permission in the Firm organization, then retry.",
        )
    }

    fn from_forge(error: cloud::forge::ForgeError) -> Self {
        use cloud::forge::ForgeError;
        match error {
            ForgeError::Authentication => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "unavailable",
                "credential_rejected",
                "Forgejo rejected the Firm's stored access token. It may be invalid, expired, or revoked.",
                "Rotate the Forgejo access token in the Firm's Integration secrets and retry.",
            ),
            ForgeError::Api { action: "creating repository", status: 403 } => Self::organization_permission(),
            ForgeError::Api { status: 403, .. } => Self::new(
                StatusCode::FORBIDDEN,
                "unavailable",
                "access_denied",
                "Forgejo denied the requested operation with the Firm's stored access token.",
                "Check the token scopes and its account's permissions in the configured Forgejo organization, then retry.",
            ),
            ForgeError::NotPrivate { .. } => Self::identity(
                "repository_not_private",
                "Forgejo did not confirm private visibility for this mirror.",
            ),
            ForgeError::ForeignRepository { .. } => Self::identity(
                "repository_identity_mismatch",
                "Forgejo returned a repository with a different owner or name.",
            ),
            ForgeError::MissingUrl { .. } | ForgeError::Response { .. } => Self::upstream(
                "invalid_forgejo_response",
                "Forgejo did not return a usable repository description.",
                "Check the deployment's Forgejo API and retry.",
            ),
            ForgeError::Request { source, .. } if source.is_timeout() => Self::upstream(
                "forgejo_timeout",
                "Forgejo timed out while verifying the mirror.",
                "Check Forgejo availability from the deployment and retry.",
            ),
            _ => Self::upstream(
                "forgejo_request_failed",
                "The Forgejo mirror request failed.",
                "Check Forgejo health and the owning Firm's repository access, then retry.",
            ),
        }
    }
}

/// The Drive folder a Project links, if any.
fn linked_drive_folder(project: &store::projects::Project) -> Option<&str> {
    project
        .drive_folder_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
}

async fn onboarding_agreements(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
) -> Result<Response, ApiError> {
    let projects = store::access::visible_projects_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
    )
    .await
    .map_err(ApiError::Db)?;
    let ids = projects
        .into_iter()
        .map(|project| project.id)
        .collect::<Vec<_>>();
    let db = store::postgres::PostgresDb::from_env()
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?;
    let rows = store::onboarding::agreements(&db, &ids)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?;
    Ok(Json(rows).into_response())
}

#[derive(Deserialize)]
struct DeadlineProjectQuery {
    #[serde(default)]
    include_pitch: bool,
    #[serde(default)]
    include_deadlines: bool,
    deadline_schedule_timezone: Option<chrono_tz::Tz>,
}

/// The dashboard inventory for deadline and readiness reporting.
/// Keep its Firm boundary identical to `webapp::lawyer_dashboard` so an
/// Owner/Admin with participation at another Firm does not broaden this view.
async fn lawyer_deadline_projects(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Query(query): Query<DeadlineProjectQuery>,
) -> Result<Response, ApiError> {
    let mut projects = store::access::visible_projects_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
    )
    .await
    .map_err(ApiError::Db)?;
    let firm_id = store::firms::firm_id_for_brand_key(
        &state.application_store,
        views::brand::brand_key().as_str(),
    )
    .await
    .map_err(|error| ApiError::Db(error.to_string()))?;
    let firm_id = firm_id.ok_or_else(|| ApiError::Db("host brand has no Firm".into()))?;
    let firm_brands = store::firms::brand_keys_for_firm(&state.application_store, firm_id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    projects.retain(|project| {
        deadline_project_in_dashboard_scope(
            if query.include_pitch && project.status == "pitch" {
                "open"
            } else {
                &project.status
            },
            project.firm_id,
            &project.brand,
            Some(firm_id),
            &firm_brands,
        )
    });
    let project_ids = projects
        .iter()
        .map(|project| project.id)
        .collect::<Vec<_>>();
    let rows = projects
        .into_iter()
        .map(|project| {
            let mut row = serde_json::json!({
                "id": project.id,
                "code": project.code,
                "name": project.name,
                "status": project.status
            });
            if query.include_pitch {
                row["practice_area"] = serde_json::json!(project
                    .kit_version
                    .as_deref()
                    .and_then(|version| version.split('/').nth(1)));
            }
            row
        })
        .collect::<Vec<_>>();
    if query.include_deadlines {
        let deadlines = store::project_deadlines::list_for_projects(&project_ids)
            .await
            .map_err(|error| ApiError::Db(error.to_string()))?;
        let scheduling_policy = store::project_deadlines::scheduling_policy(Some(firm_id))
            .await
            .map_err(|error| ApiError::Db(error.to_string()))?;
        let mut response = serde_json::json!({
            "projects": rows, "deadlines": deadlines, "scheduling_policy": scheduling_policy,
        });
        if let (Some(zone), Some(person)) = (query.deadline_schedule_timezone, lawyer.0.person_id) {
            response["work_schedule"] = match crate::deadline_scheduling::plan(
                &state.application_store,
                state.integration_providers.as_ref(),
                person,
                zone,
                chrono::Utc::now(),
            )
            .await
            {
                Ok(plan) => serde_json::json!(plan),
                Err(_) => serde_json::json!({ "availability": "unverified" }),
            };
        }
        return Ok(Json(response).into_response());
    }
    if query.include_pitch {
        return Ok(Json(serde_json::json!({"projects": rows})).into_response());
    }
    Ok(Json(rows).into_response())
}

fn deadline_project_in_dashboard_scope(
    status: &str,
    project_firm_id: Option<Uuid>,
    project_brand: &str,
    firm_id: Option<Uuid>,
    firm_brands: &[String],
) -> bool {
    let Some(firm_id) = firm_id else {
        return false;
    };
    status == "open"
        && (project_firm_id == Some(firm_id)
            || (project_firm_id.is_none()
                && firm_brands.iter().any(|brand| brand == project_brand)))
}

/// Live lifecycle evidence for a firm-side participant. Filing establishes
/// onboarding presence; `onboarding.status` carries the execution reading —
/// `official` once the onboarding envelope is verified as completed.
async fn project_status_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    if !participates_as_firm(&state, &lawyer, id).await {
        return Err(ApiError::NotFound);
    }
    let project = store::projects::find_by_id(&state.application_store, id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?
        .ok_or(ApiError::NotFound)?;
    let (project, history) =
        store::projects::status_with_history(&state.application_store, project).await?;
    let (opening, closing) = store::projects::matter_lifecycle_sets(
        &state.application_store,
        std::slice::from_ref(&project),
    )
    .await
    .map_err(ApiError::Db)?;
    let onboarding_status =
        store::projects::onboarding_status(&state.application_store, id, &project.code)
            .await
            .map_err(ApiError::Db)?;
    let lifecycle = store::projects::project_lifecycle(
        &project.code,
        if project.status == "archived" {
            "closed"
        } else {
            &project.status
        },
        onboarding_status != store::projects::OnboardingStatus::Official,
        !closing.contains(&id),
    );
    let documents = store::assets::for_project(&state.application_store, id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?
        .into_iter()
        .filter(|asset| store::projects::template_opens_a_matter(asset.kind.as_deref()))
        .map(|asset| serde_json::json!({"id": asset.id, "visibility": asset.visibility}))
        .collect::<Vec<_>>();
    let mut missing = if lifecycle == store::projects::MatterLifecycle::NeedsOnboarding {
        vec!["Verify execution of the onboarding letter.".to_string()]
    } else {
        Vec::new()
    };
    let integrations = firm_integrations(project.firm_id).await?;
    let drive_folder_id = linked_drive_folder(&project);
    if project.status == "open" && integrations.drive && drive_folder_id.is_none() {
        missing.push(format!(
            "Link the matter's Drive folder: `navigator project set {} --drive-folder <link>`.",
            project.code
        ));
    }
    let remotes = repository_remotes(&state, &project, &lawyer.0).await?;
    let (envelopes, adopted_envelopes) = stored_project_envelopes(&state, id).await?;
    Ok(Json(serde_json::json!({
        "code": project.code,
        "status": if project.status == "archived" { "archived" } else { lifecycle.label() },
        "history": history,
        "project_status": project.status,
        // Project creation is the opening timestamp; there is no separate opened_at column.
        "opened_at": project.inserted_at,
        "closed_at": project.closed_at,
        "source_state": store::project_surfaces::source_state(&project),
        "drive_folder_id": drive_folder_id,
        "firm_integrations": integrations,
        "onboarding": {
            "filed": opening.contains(&id),
            "status": onboarding_status.label(),
            "documents": documents,
            "execution_verified": onboarding_status == store::projects::OnboardingStatus::Official
        },
        "repository_remotes": remotes,
        "envelopes": envelopes,
        "adopted_envelopes": adopted_envelopes,
        "missing": missing
    }))
    .into_response())
}

/// The owning Firm's Firm Integrations switches; a Project without a Firm has both off.
#[derive(serde::Serialize)]
struct FirmIntegrations {
    calendar: bool,
    drive: bool,
}

async fn firm_integrations(firm: Option<Uuid>) -> Result<FirmIntegrations, ApiError> {
    let Some(firm) = firm else {
        return Ok(FirmIntegrations {
            calendar: false,
            drive: false,
        });
    };
    let unavailable = |error: store::firms::FirmError| ApiError::Db(error.to_string());
    Ok(FirmIntegrations {
        calendar: store::firms::calendar_owner(firm)
            .await
            .map_err(unavailable)?
            .is_some(),
        drive: store::firms::google_drive(firm)
            .await
            .map_err(unavailable)?,
    })
}

async fn stored_project_envelopes(
    state: &ApiState,
    id: Uuid,
) -> Result<(Vec<serde_json::Value>, Vec<serde_json::Value>), ApiError> {
    let mut adopted_envelopes = Vec::new();
    let mut envelopes = Vec::new();
    for notation in store::notations::list_by_project(&state.application_store, id).await? {
        if let Some(envelope) =
            crate::signature_adoption::evidence(state.storage.as_ref(), notation.id).await?
        {
            let entry = serde_json::json!({"notation_id": notation.id, "envelope": envelope});
            envelopes.push(entry.clone());
            adopted_envelopes.push(entry);
        } else {
            let mut signature = crate::signature_status::status_for_notation(
                &state.application_store,
                &state.storage,
                &notation,
            )
            .await;
            if signature.signature_request_id.is_some() {
                crate::signature_status::read_signer_identities(
                    &mut signature,
                    &state.application_store,
                    notation.project_id,
                    state.signature_provider.as_ref(),
                )
                .await;
                envelopes.push(serde_json::json!({
                    "notation_id": notation.id,
                    "envelope": {
                        "envelopeId": signature.signature_request_id,
                        "status": signature.state,
                        "updatedAt": signature.updated_at,
                        "signers": signature.signers.iter().map(|signer| serde_json::json!({
                            "role": signer.role,
                            "name": signer.name.as_deref().unwrap_or(&signer.role),
                            "email": signer.email,
                            "status": if signer.signed { "completed" } else { "pending" }
                        })).collect::<Vec<_>>()
                    }
                }));
            }
        }
    }
    {
        let pg = store::postgres::PostgresDb::application()
            .await
            .map_err(crate::signature_adoption::internal)?;
        for observed in store::project_envelopes::for_project(&pg, id)
            .await
            .map_err(crate::signature_adoption::internal)?
        {
            let key = observed.envelope_id.to_string();
            if let Some(entry) = envelopes
                .iter_mut()
                .find(|entry| entry["envelope"]["envelopeId"].as_str() == Some(&key))
            {
                entry["envelope"] =
                    serde_json::to_value(observed).map_err(crate::signature_adoption::internal)?;
            } else {
                envelopes.push(serde_json::json!({"notation_id": null, "envelope": observed}));
            }
        }
    }
    Ok((envelopes, adopted_envelopes))
}

pub(crate) fn project_client_dri(
    pinned: Option<Uuid>,
    participants: &[store::projects::PersonProjectRole],
) -> Option<Uuid> {
    let mut eligible = participants
        .iter()
        .filter(|row| row.is_client_dri && row.participation == "client")
        .map(|row| row.person_id);
    if let Some(pinned) = pinned {
        return eligible.find(|person| *person == pinned);
    }
    let client = eligible.next()?;
    eligible.next().is_none().then_some(client)
}

/// Resolve the recipient and destination of a lawyer's unsent invitation.
async fn project_portal_invitation_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    if !participates_as_firm(&state, &lawyer, id).await {
        return Err(ApiError::NotFound);
    }
    let project = store::projects::find_by_id(&state.application_store, id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?
        .ok_or(ApiError::NotFound)?;
    let conflict = |message: &'static str| Ok((StatusCode::CONFLICT, message).into_response());
    if project.legal_plan_id.is_none()
        || project
            .plan_accepted_on
            .as_deref()
            .is_none_or(|date| date.trim().is_empty())
        || !matches!(project.status.as_str(), "pitch" | "open")
    {
        return conflict("This Project has no active, confirmed Legal Plan acceptance.");
    }
    let participants = store::projects::participations_for_project(&state.application_store, id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    let Some(client_id) = project_client_dri(project.client_dri_person_id, &participants) else {
        return conflict("Designate the member as the Project's client DRI first.");
    };
    if !store::access::can_see_project_as_client(&state.application_store, Some(client_id), id)
        .await
        .map_err(ApiError::Db)?
    {
        return conflict("The member needs client participation on this Project.");
    }
    let client = store::persons::find_by_id(&state.application_store, client_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if client.role != store::persons::Role::Client {
        return conflict("The member needs a client account for this Project's portal.");
    }
    let base = crate::openapi::base_url_for(None);
    let Some(origin) = crate::client_portal::project_origin(&project.brand, &base) else {
        return conflict("The Project Brand's portal origin could not be verified.");
    };
    Ok((
        [(header::CACHE_CONTROL, "private, no-store")],
        Json(serde_json::json!({
            "recipient": client.email,
            "portal_url": format!("{origin}/app/projects/{}", project.code),
        })),
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TestimonialSubmissionInput {
    quote: String,
    #[serde(default)]
    attribution: Option<String>,
    request_public: bool,
}

#[derive(Debug, Serialize)]
struct TestimonialResponse {
    id: Uuid,
    quote: String,
    attribution: Option<String>,
    consented_at: Option<String>,
    published_at: Option<String>,
}

fn testimonial_response(testimonial: store::testimonials::Testimonial) -> Response {
    Json(TestimonialResponse {
        id: testimonial.id,
        quote: testimonial.quote,
        attribution: testimonial.attribution_label,
        consented_at: testimonial.consented_at,
        published_at: testimonial.published_at,
    })
    .into_response()
}

/// `POST /app/api/projects/{id}/testimonial` — save the authenticated
/// project's client-DRI testimonial. The body contains content only; both
/// identity references are resolved from the session and path and are checked
/// again by the store authorization seam.
async fn save_testimonial_door(
    State(state): State<ApiState>,
    AuthedSession(session): AuthedSession,
    Path(project_id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<TestimonialSubmissionInput>,
) -> Result<Response, ApiError> {
    let person_id = session.person_id.ok_or(ApiError::NotFound)?;
    if session.viewing_as_dri.is_some() {
        return Err(ApiError::NotFound);
    }
    let result = store::testimonials::save_for_client_dri(
        &state.application_store,
        person_id,
        project_id,
        &store::testimonials::TestimonialSubmission {
            quote: &input.quote,
            attribution_label: input.attribution,
            request_public: input.request_public,
        },
    )
    .await?;
    Ok(testimonial_response(result))
}

async fn publish_testimonial_door(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(testimonial_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    Ok(testimonial_response(
        store::testimonials::publish(
            &state.application_store,
            session.person_id,
            session.role,
            testimonial_id,
        )
        .await?,
    ))
}

async fn unpublish_testimonial_door(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(testimonial_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    Ok(testimonial_response(
        store::testimonials::unpublish(
            &state.application_store,
            session.person_id,
            session.role,
            testimonial_id,
        )
        .await?,
    ))
}

/// `GET /app/api/projects/{id}/participants` — the matter's participation ledger.
async fn list_participants_door(
    State(state): State<ApiState>,
    authed: AuthedSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    if !can_see(&state, &authed, id).await {
        return Err(ApiError::NotFound);
    }
    let rows = store::projects::participations_for_project(&state.application_store, id)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?;
    Ok((StatusCode::OK, Json(rows)).into_response())
}

/// One participant's identifiers on [`PrivateTerms`].
#[derive(Serialize)]
struct PrivateTermsParticipant {
    name: String,
    email: String,
    phone: Option<String>,
}

/// The identifiers a Pre-Filing PII Check refuses for one Project.
#[derive(Serialize)]
struct PrivateTerms {
    code: String,
    name: String,
    participants: Vec<PrivateTermsParticipant>,
    identifiers: Vec<String>,
}

/// `GET /app/api/projects/{id}/private-terms` — the Project's code, its matter
/// name, participant identities, and scoped client and reference identifiers.
/// `navigator project feedback-check` scans the draft locally against them. Only a
/// firm-side participant reads it: a client-side participant, a Clerk, and a
/// caller with no participation all get the bare 404 an unknown id gets.
async fn private_terms_door(
    State(state): State<ApiState>,
    authed: AuthedSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let viewer = store::access::matter_viewer(
        &state.application_store,
        authed.0.person_id,
        authed.0.role,
        id,
    )
    .await
    .map_err(ApiError::Db)?;
    if !viewer.is_some_and(store::access::MatterViewer::is_firm_side) {
        return Err(ApiError::NotFound);
    }
    let project = store::projects::find_by_id(&state.application_store, id)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?
        .ok_or(ApiError::NotFound)?;
    let terms = private_terms_for_project(&state.application_store, project).await?;
    Ok((StatusCode::OK, Json(terms)).into_response())
}

/// Resolve only parties attached to this authorized Project. A dangling
/// attachment refuses the response instead of claiming a complete check.
async fn private_terms_for_project(
    db: &store::application_store::ApplicationStore,
    project: store::projects::Project,
) -> Result<PrivateTerms, ApiError> {
    let resolution = store::private_terms::resolve(db, &project)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?;
    // The refusal and the admin-tier reconciliation door read the same
    // records through the same checks (`store::private_terms`); here a
    // finding refuses, the first one in the report's order.
    if let Some(finding) = resolution.findings.first() {
        return Err(match finding {
            store::private_terms::Finding::MissingPerson { .. } => {
                ApiError::PrivateTermsConflict("private terms: missing Person record")
            }
            store::private_terms::Finding::MissingEntity { .. } => {
                ApiError::PrivateTermsConflict("private terms: missing Entity record")
            }
            store::private_terms::Finding::InvalidEntityMetadata { .. } => {
                ApiError::PrivateTermsInvalid("private terms: invalid Entity metadata")
            }
            store::private_terms::Finding::InvalidEntityEin { .. } => {
                ApiError::PrivateTermsInvalid("private terms: invalid Entity EIN")
            }
        });
    }
    let mut identifiers = vec![project.id.to_string()];
    let mut participants = Vec::new();
    for person in resolution.people {
        identifiers.push(person.id.to_string());
        identifiers.extend(person.given_name);
        identifiers.extend(person.middle_name);
        identifiers.extend(person.family_name);
        identifiers.extend(person.xero_contact_id);
        if resolution.participation_ids.contains(&person.id) {
            participants.push(PrivateTermsParticipant {
                name: person.name,
                email: person.email,
                phone: person.phone,
            });
        } else {
            identifiers.extend([person.name, person.email]);
            identifiers.extend(person.phone);
        }
    }
    for entity in resolution.entities {
        if let Some(metadata) = entity.metadata.as_ref() {
            // Every stored EIN reaching this loop is standardized — a
            // malformed one came back as a finding above.
            if let Some(ein) = metadata.get("ein").and_then(serde_json::Value::as_str) {
                identifiers.push(ein.to_owned());
            }
        }
        identifiers.extend([entity.id.to_string(), entity.name]);
        identifiers.extend(entity.phone);
        identifiers.extend(entity.url);
        identifiers.extend(entity.xero_id);
    }
    Ok(PrivateTerms {
        code: project.code,
        name: project.name,
        participants,
        identifiers,
    })
}

#[derive(Debug, Serialize)]
struct PrivateTermsReconciliationEntry {
    project_id: Uuid,
    code: String,
    items: Vec<store::private_terms::Finding>,
}

#[derive(Debug, Serialize)]
struct PrivateTermsReconciliationReport {
    findings: Vec<PrivateTermsReconciliationEntry>,
}

/// `GET /app/api/project-private-terms-reconciliation` — every matter whose
/// private-terms response would be refused, with the record that breaks it.
///
/// Admin-tier only via [`AdminSession`], and the tier is the point: the
/// question is about the whole deployment, so it reads
/// [`store::projects::all`] instead of pretending a per-caller lens is an
/// inventory (the [`reconcile_project_repositories_door`] doctrine). It runs
/// the same reconciliation the private-terms refusal runs —
/// [`store::private_terms::resolve`] — and reports every finding instead of
/// refusing on the first, so a repair can name its work without correlating
/// server logs.
///
/// The payload is findings only: per broken matter, its code and each
/// finding's kind, dangling id, and attachment or field path. A dangling
/// UUID is the same value class the terms list treats as private, and is the
/// most the reconciliation needs — no name, mailbox, phone, or EIN crosses
/// this door, and healthy matters are omitted. Fail-closed is untouched: the
/// private-terms door still refuses 409/422, and this door never repairs.
async fn reconcile_private_terms_door(
    State(state): State<ApiState>,
    _authed: AdminSession,
) -> Result<Response, ApiError> {
    let projects = store::projects::all(&state.application_store)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    let mut findings = Vec::new();
    for project in projects {
        let resolution = store::private_terms::resolve(&state.application_store, &project)
            .await
            .map_err(|error| ApiError::Db(error.to_string()))?;
        if resolution.findings.is_empty() {
            continue;
        }
        findings.push(PrivateTermsReconciliationEntry {
            project_id: project.id,
            code: project.code,
            items: resolution.findings,
        });
    }
    Ok((
        StatusCode::OK,
        Json(PrivateTermsReconciliationReport { findings }),
    )
        .into_response())
}

/// `GET /app/api/projects/{id}/finance`: the Project's latest finance aggregate,
/// in the shape the Project portal's finance view checks (`source: "workflow"`,
/// whole-cent integers). Same scope rule as the other Project reads: any
/// authenticated session that can see the Project, a bare 404 otherwise. A
/// Project no `FinanceAggregate` run has stored for is also 404, which is what
/// sends the portal to its bundled fixture.
async fn get_finance_door(
    State(state): State<ApiState>,
    authed: AuthedSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    if !can_see(&state, &authed, id).await {
        return Err(ApiError::NotFound);
    }
    match store::finance_aggregate::latest_for_project(&state.application_store, id).await {
        Ok(Some(report)) => Ok((StatusCode::OK, Json(report)).into_response()),
        Ok(None) => Err(ApiError::NotFound),
        Err(e) => Err(ApiError::Db(e.to_string())),
    }
}

/// `POST /app/api/projects/{id}/finance/run`: start one `FinanceAggregate`
/// run for the Project. Lawyer tier, matter-scoped like `close_matter`: a
/// Project the caller cannot see is a bare 404. The run reads
/// `finance/<project code>/input.json` from object storage and is keyed
/// `manual-<uuid>`, so each call is a new run. `202 Accepted` carries the run
/// id; `503` when no Restate ingress is configured (dev, tests).
async fn run_finance_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    let project = store::projects::find_by_id(&state.application_store, id)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?
        .ok_or(ApiError::NotFound)?;
    let run_id = format!("manual-{}", Uuid::new_v4());
    let Ok((url, token)) = workflows::trigger::dbos_ingress_from_env() else {
        return Ok(workflow_ingress_unavailable());
    };
    let dispatch = queue_finance_run_dbos(&url, &token, id, &project.code, &run_id).await;
    match dispatch {
        Ok(()) => {
            tracing::info!(project_id = %id, run_id, "finance aggregate run queued");
            Ok((
                StatusCode::ACCEPTED,
                Json(serde_json::json!({ "run_id": run_id })),
            )
                .into_response())
        }
        Err(e) => {
            tracing::error!(project_id = %id, run_id, error = %e, "finance aggregate run not queued");
            Err(ApiError::Db("the finance run could not be queued".into()))
        }
    }
}

/// The outcomes `project-gate.yml` or a Forgejo `cd.yml` reports for a push to `main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum CiOutcome {
    Pass,
    Fail,
    Skipped,
}

/// `POST /app/api/projects/{id}/events/main-push`'s body: the run's
/// mechanism fields plus the commit's subject line (ENG-1285). The subject
/// is the one free-text field; the worker renders it as escaped plain text,
/// so it can never mention or link anything.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MainPushEvent {
    short_sha: String,
    run_url: String,
    author_login: String,
    gate: CiOutcome,
    /// Absent when the gate reports before the publish half has run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    publish: Option<CiOutcome>,
    run_attempt: u32,
    /// The commit's subject line. Absent from callers on an older release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    subject: Option<String>,
}

/// The longest subject the door accepts. Git itself has no limit, but the
/// worker shows about 120 characters, and anything longer is not a subject.
const MAIN_PUSH_SUBJECT_LIMIT: usize = 512;

impl MainPushEvent {
    /// Refuse anything that is not a short SHA, an `https` run URL, a GitHub
    /// login, and a positive attempt number, naming the first bad field.
    fn validate(&self) -> Result<(), &'static str> {
        let sha_ok = (7..=40).contains(&self.short_sha.len())
            && self
                .short_sha
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
        if !sha_ok {
            return Err("short_sha must be 7 to 40 lowercase hex characters");
        }
        if !self.run_url.starts_with("https://") || self.run_url.len() > 2048 {
            return Err("run_url must be an https URL");
        }
        // A GitHub login, or a Forgejo one, which also allows `_` and `.`.
        let login_ok = (1..=40).contains(&self.author_login.len())
            && self
                .author_login
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
        if !login_ok {
            return Err("author_login must be a GitHub or Forgejo login");
        }
        if self.run_attempt == 0 {
            return Err("run_attempt must be at least 1");
        }
        if let Some(subject) = &self.subject {
            if subject.chars().count() > MAIN_PUSH_SUBJECT_LIMIT
                || subject.chars().any(char::is_control)
            {
                return Err("subject must be one line of at most 512 characters");
            }
        }
        Ok(())
    }
}

/// `POST /app/api/projects/{id}/events/main-push` — a Project repository's
/// CI reports a push to `main`. The caller is the document-scoped CI session
/// `project-gate.yml` mints for exactly this Project; any other session, and
/// a CI session scoped to another Project, is refused with 403. The event is
/// handed to the `project-slack` lane the service's switch selects — the
/// worker's DBOS ingress, else the Restate object keyed by the Project id —
/// which resolves the channel from the Project row and posts once per
/// `(sha, run attempt)`. `202 Accepted` when queued.
async fn main_push_event_door(
    State(state): State<ApiState>,
    authed: AuthedSession,
    Path(id): Path<Uuid>,
    Json(event): Json<MainPushEvent>,
) -> Result<Response, ApiError> {
    match &authed.0.scope {
        Some(SessionScope::Document(scope)) if scope.project_id == id => {}
        _ => return Err(ApiError::Forbidden),
    }
    if let Err(message) = event.validate() {
        return Ok(bad_request("invalid_event", message));
    }
    store::projects::find_by_id(&state.application_store, id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?
        .ok_or(ApiError::NotFound)?;
    // ENG-1287: a notice the worker cannot post must not look delivered. The
    // lane posts with the owning Firm's own Slack bot (ENG-392), so the Firm's
    // credential is resolved here first: a Firm that has not connected Slack,
    // or a deployment that cannot unseal Firm credentials, is refused with a
    // body the CI step prints instead of a 202 the lane would later drop.
    if let Err(error) = state
        .integration_providers
        .slack(&state.application_store, id)
        .await
    {
        tracing::warn!(project_id = %id, short_sha = %event.short_sha, outcome = error.slug(), "main push notice refused: Slack is not connected for the Project's Firm");
        return Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "slack_not_connected",
                "outcome": error.slug(),
                "message": format!("the Project channel notice was not queued: {error}"),
            })),
        )
            .into_response());
    }
    let Ok((url, token)) = workflows::trigger::dbos_ingress_from_env() else {
        return Ok(workflow_ingress_unavailable());
    };
    let dispatch = queue_main_push_dbos(&url, &token, id, &event).await;
    match dispatch {
        Ok(()) => {
            tracing::info!(
                project_id = %id,
                short_sha = %event.short_sha,
                run_attempt = event.run_attempt,
                "main push notice queued"
            );
            Ok((
                StatusCode::ACCEPTED,
                Json(serde_json::json!({ "queued": true })),
            )
                .into_response())
        }
        Err(error) => {
            tracing::error!(project_id = %id, short_sha = %event.short_sha, %error, "main push notice not queued");
            Err(ApiError::Db(
                "the main push notice could not be queued".into(),
            ))
        }
    }
}

fn workflow_ingress_unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": "workflows_unavailable",
            "message": "The durable workflow ingress is unavailable."
        })),
    )
        .into_response()
}

/// Start one manual FinanceAggregate run through the authenticated internal
/// DBOS worker ingress. The caller has already checked the lawyer's project
/// scope and resolved the canonical Project code from storage.
async fn queue_finance_run_dbos(
    ingress: &str,
    token: &str,
    project_id: Uuid,
    project_code: &str,
    run_id: &str,
) -> Result<(), workflows::trigger::TriggerError> {
    workflows::trigger::start_dbos_finance_aggregate(
        ingress,
        token,
        &serde_json::json!({
            "workflow_id": format!("FinanceAggregate:{run_id}"),
            "request": { "project_id": project_id, "project_code": project_code }
        }),
    )
    .await
}

/// Queue a Project Slack notice keyed by commit SHA and run attempt after authorization.
async fn queue_main_push_dbos(
    ingress: &str,
    token: &str,
    project_id: Uuid,
    event: &MainPushEvent,
) -> Result<(), workflows::TriggerError> {
    workflows::trigger::start_dbos_project_slack(
        ingress,
        token,
        &serde_json::json!({
            "workflow_id": workflows::trigger::project_slack_main_push_workflow_id(
                project_id,
                &event.short_sha,
                event.run_attempt,
            ),
            "event": event,
        }),
    )
    .await
}

/// `GET /app/api/projects/{id}/notations` — the notations opened on the matter.
async fn list_notations_door(
    State(state): State<ApiState>,
    authed: AuthedSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    if !can_see(&state, &authed, id).await {
        return Err(ApiError::NotFound);
    }
    let mut notations = store::notations::list_by_project(&state.application_store, id).await?;
    let lawyer_lens = store::access::matter_lens(
        &state.application_store,
        authed.0.person_id,
        authed.0.role,
        id,
    )
    .await
    .map_err(|error| ApiError::Db(error.clone()))?
        == Some(store::access::ProjectLens::Lawyer);
    if !lawyer_lens {
        let template_ids: Vec<_> = notations
            .iter()
            .map(|notation| notation.template_id)
            .collect();
        let templates = store::templates::find_by_ids(&state.application_store, &template_ids)
            .await
            .map_err(|error| ApiError::Db(error.to_string()))?;
        notations.retain(|notation| {
            templates
                .iter()
                .find(|template| template.id == notation.template_id)
                .is_some_and(|template| {
                    template
                        .kind
                        .as_deref()
                        .is_none_or(client_may_read_notation_kind)
                })
        });
    }
    Ok((StatusCode::OK, Json(notations)).into_response())
}

/// `GET /app/api/projects/{id}/notation-inventory` — a lawyer's private,
/// matter-scoped board of runs and templates not yet opened on one Project.
/// Participation is required of every tier, Owner and Admin included.
async fn notation_inventory_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    if !participates_as_firm(&state, &lawyer, id).await {
        return Err(ApiError::NotFound);
    }

    let rows = webapp::lawyer_project_notations::project_notation_board(
        &state.application_store,
        &state.storage,
        id,
    )
    .await
    .map_err(ApiError::Db)?;
    Ok((StatusCode::OK, Json(rows)).into_response())
}

/// `GET /app/api/notations/{id}` — one notation, scoped by its matter.
async fn get_notation_door(
    State(state): State<ApiState>,
    authed: AuthedSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let notation = store::notations::find_by_id(&state.application_store, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !can_see(&state, &authed, notation.project_id).await {
        return Err(ApiError::NotFound);
    }
    let lawyer_lens = store::access::matter_lens(
        &state.application_store,
        authed.0.person_id,
        authed.0.role,
        notation.project_id,
    )
    .await
    .map_err(|error| ApiError::Db(error.clone()))?
        == Some(store::access::ProjectLens::Lawyer);
    if !lawyer_lens && !notation_client_visible(&state, &notation).await? {
        return Err(ApiError::NotFound);
    }
    Ok((StatusCode::OK, Json(notation)).into_response())
}

/// TrialPrep is internal work product. A participating client may read other
/// Notation metadata, but cannot resolve a TrialPrep Notation by its id.
async fn notation_client_visible(
    state: &ApiState,
    notation: &store::notations::Notation,
) -> Result<bool, ApiError> {
    let template = store::templates::find_by_id(&state.application_store, notation.template_id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    let Some(template) = template else {
        return Ok(false);
    };
    Ok(template
        .kind
        .as_deref()
        .is_some_and(client_may_read_notation_kind))
}

fn client_may_read_notation_kind(kind: &str) -> bool {
    rules::kind::Kind::parse(kind).is_none_or(rules::kind::Kind::is_client_visible)
}

/// `GET /app/api/notations/{id}/answers` — the filed answers for one notation.
/// The answer values, source, and author provenance are firm work product, so
/// this route is lawyer-tier and matter-scoped rather than client-readable.
/// Participation is required of every tier, Owner and Admin included.
async fn list_notation_answers_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let notation = notation_in_firm_participation(&state, &lawyer, id).await?;
    let answers = store::answers::for_notation(&state.application_store, notation.id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    Ok((StatusCode::OK, Json(answers)).into_response())
}

/// `GET /app/api/playbooks` — the firm's contract-review playbooks (lawyer tier).
async fn list_playbooks_door(
    State(state): State<ApiState>,
    _lawyer: LawyerSession,
) -> Result<Response, ApiError> {
    let playbooks = store::playbooks::all(&state.application_store)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?;
    Ok((StatusCode::OK, Json(playbooks)).into_response())
}

/// `GET /app/api/playbooks/{id}` — one playbook (lawyer tier).
async fn get_playbook_door(
    State(state): State<ApiState>,
    _lawyer: LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let playbook = store::playbooks::by_id(&state.application_store, id)
        .await
        .ok()
        .flatten()
        .ok_or(ApiError::NotFound)?;
    Ok((StatusCode::OK, Json(playbook)).into_response())
}

/// `GET /app/api/contract-reviews/{id}` — one inbound-contract review, scoped to
/// its matter (a lawyer not on the matter, or a non-firm caller, gets 404).
async fn get_contract_review_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let review = store::contract_reviews::by_id(&state.application_store, id)
        .await
        .ok()
        .flatten()
        .ok_or(ApiError::NotFound)?;
    let notation = store::notations::find_by_id(&state.application_store, review.notation_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        notation.project_id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    Ok((StatusCode::OK, Json(review)).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PutProjectJurisdiction {
    code: String,
}

async fn put_project_jurisdiction(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    Json(input): Json<PutProjectJurisdiction>,
) -> Result<StatusCode, ApiError> {
    if !participates_as_firm(&state, &lawyer, id).await {
        return Err(ApiError::NotFound);
    }
    let jurisdiction = store::jurisdictions::find_by_code(&state.application_store, &input.code)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?
        .ok_or(store::projects::ProjectCommandError::Invalid(
            "unknown jurisdiction code",
        ))?;
    store::projects::set_jurisdiction(&state.application_store, id, Some(jurisdiction.id))
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Whether a lawyer-tier caller holds a *firm-side* participation row on
/// `project_id` — the gate for matter content that is firm work product.
///
/// Deliberately not [`store::access::can_see_project_as_lawyer`]: that helper
/// still carries the Owner/Admin project-scoping bypass for the surfaces ENG-83
/// has not collapsed. Owner and Admin bypass project-scoping at *route
/// admission*; that is oversight, not a key to a matter's work product, and a
/// notation inventory or a filed answer is content. `matter_viewer` scopes
/// every tier by the participation ledger with no short-circuit (ENG-81), and
/// `is_firm_side` keeps a lawyer who holds a client-side row on their own
/// matter in the client lens rather than handing them the firm's notes.
async fn participates_as_firm(state: &ApiState, lawyer: &LawyerSession, project_id: Uuid) -> bool {
    store::access::matter_viewer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        project_id,
    )
    .await
    .ok()
    .flatten()
    .is_some_and(store::access::MatterViewer::is_firm_side)
}

/// Whether `authed` may see matter `project_id`, in either lens.
async fn can_see(state: &ApiState, authed: &AuthedSession, project_id: Uuid) -> bool {
    crate::client_portal::access()
        .matter_viewer(
            &state.application_store,
            authed.0.person_id,
            authed.0.role,
            project_id,
        )
        .await
        .ok()
        .flatten()
        .is_some()
}

/// Query for `GET /app/api/projects/{id}/documents`.
#[derive(Deserialize, Default)]
struct ListDocumentsQuery {
    /// A lawyer asks for soft-deleted documents too. A client lens ignores it.
    #[serde(default)]
    include_deleted: bool,
}

/// `GET /app/api/projects/{id}/documents` — the matter's filed documents. The
/// matter participation lens decides whether internal work product is filtered
/// out; a firm-tier person with client-side participation is still client-scoped.
/// A soft-deleted document is left out unless a lawyer-lens caller sends
/// `?include_deleted=true`; a client never sees one, nor the deletion or Legal
/// Hold stamps on a document it can see.
async fn list_documents_door(
    State(state): State<ApiState>,
    authed: AuthedSession,
    Path(id): Path<Uuid>,
    Query(query): Query<ListDocumentsQuery>,
) -> Result<Response, ApiError> {
    let lens = store::access::matter_lens(
        &state.application_store,
        authed.0.person_id,
        authed.0.role,
        id,
    )
    .await
    .map_err(ApiError::Db)?
    .ok_or(ApiError::NotFound)?;
    let client_lens = matches!(lens, store::access::ProjectLens::Client);
    let mut docs = if query.include_deleted && !client_lens {
        store::assets::for_project_including_deleted(&state.application_store, id).await
    } else {
        store::assets::for_project(&state.application_store, id).await
    }
    .map_err(|e| ApiError::Db(e.to_string()))?;
    if client_lens {
        docs.retain(|a| a.visibility == store::documents::visibility::CLIENT);
        for doc in &mut docs {
            doc.legal_hold_at = None;
            doc.deleted_by_person_id = None;
            doc.deleted_commit_sha = None;
        }
    }
    Ok((StatusCode::OK, Json(docs)).into_response())
}

/// `GET /app/api/projects/{id}/conversation` — the matter conversation. A client
/// sees the client-visible thread (internal notes filtered out); the firm sees
/// the full thread.
async fn get_conversation_door(
    State(state): State<ApiState>,
    authed: AuthedSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    if !can_see(&state, &authed, id).await {
        return Err(ApiError::NotFound);
    }
    let messages = if matches!(authed.0.role, store::persons::Role::Client) {
        store::communications::for_project_client_visible(&state.application_store, id).await
    } else {
        store::communications::for_project(&state.application_store, id).await
    }
    .map_err(|e| ApiError::Db(e.to_string()))?;
    Ok((StatusCode::OK, Json(messages)).into_response())
}

/// `GET /app/api/notations/{id}/review-documents` — the review drafts on a
/// notation (firm work product). Lawyer-tier and matter-scoped.
async fn list_review_documents_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let notation = store::notations::find_by_id(&state.application_store, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        notation.project_id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    let docs = store::review_documents::for_notation(&state.application_store, id)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?;
    Ok((StatusCode::OK, Json(docs)).into_response())
}

/// `GET /app/api/expunge-requests` — the pending client document-deletion queue
/// (lawyer tier; the firm's review queue).
async fn list_expunge_requests_door(
    State(state): State<ApiState>,
    _lawyer: LawyerSession,
) -> Result<Response, ApiError> {
    let requests = store::expunge_requests::list_pending(&state.application_store)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?;
    Ok((StatusCode::OK, Json(requests)).into_response())
}

/// `POST /app/api/entities` — the Entity create command. Lawyer-tier only, like
/// every other `/app/api/*` write: the [`LawyerSession`] extractor rejects an
/// anonymous caller with 401 and a `client`/`clerk` one with 403 before the
/// body is parsed. The write itself — the blank-name check, the firm-anchor
/// guard, and the insert — belongs to `store::entity_commands::create_entity`,
/// which the `/app/admin/entities` form and the inline project modal call too, so
/// this handler only authorizes and renders.
async fn create_entity(
    State(state): State<ApiState>,
    _lawyer: LawyerSession,
    JsonOrForm(input): JsonOrForm<store::entity_commands::CreateEntityCommand>,
) -> Result<Response, ApiError> {
    let created = store::entity_commands::create_entity(
        &state.application_store,
        &state.bootstrap_company,
        &input,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(created)).into_response())
}

/// The authenticated command behind `navigator site import`. The CLI sends the
/// seed document verbatim; parsing, natural-key lookup, and writes happen in
/// the typed store registry rather than in a client with database credentials.
#[derive(Debug, Deserialize)]
struct SeedRequest {
    model: String,
    yaml: String,
    #[serde(default)]
    overwrite: bool,
    #[serde(default)]
    dry_run: bool,
    /// Apply the scoped-session rules a CI-minted session would carry, so an
    /// operator's dry run answers whether CI could perform the same write.
    /// Requires `dry_run` without `overwrite`, and `project` naming the code
    /// CI would be scoped to. A session that already carries a seed scope
    /// keeps its own scope.
    #[serde(default)]
    as_ci: bool,
    #[serde(default)]
    project: Option<String>,
}

/// This route's own address, matched against a scoped session's
/// [`crate::session::SeedScope::endpoint`]. A session scoped to a different
/// endpoint string is refused before the body is even parsed further.
pub(crate) const SEED_ENDPOINT: &str = "/app/api/seed";

/// Scope Project API routes to the Admin's Firm access or the client's Brand and participation.
pub(crate) async fn scope_project_request(
    State(application_store): State<store::application_store::ApplicationStore>,
    params: Result<axum::extract::RawPathParams, axum::extract::rejection::RawPathParamsRejection>,
    request: Request,
    next: Next,
) -> Response {
    let session = request.extensions().get::<SessionData>();
    let route = request.extensions().get::<axum::extract::MatchedPath>();
    if let (Some(session), Some(route), Ok(params)) = (session, route, params) {
        if route.as_str().starts_with("/app/api/projects/{id}") {
            let project_id = params.iter().find_map(|(key, value)| {
                (key == "id").then(|| value.parse::<Uuid>().ok()).flatten()
            });
            if let Some(project_id) = project_id {
                let result = match session.role {
                    store::persons::Role::Admin => store::projects::can_access_as_lawyer(
                        &application_store,
                        session.person_id,
                        session.role,
                        project_id,
                    )
                    .await
                    .map_err(|error| error.to_string()),
                    store::persons::Role::Client if session.scope.is_none() => {
                        crate::client_portal::access()
                            .can_see_project_as_client(
                                &application_store,
                                session.person_id,
                                session.role,
                                project_id,
                            )
                            .await
                    }
                    _ => return next.run(request).await,
                };
                match result {
                    Ok(true) => {}
                    Ok(false) => return ApiError::NotFound.into_response(),
                    Err(error) => return ApiError::Db(error).into_response(),
                }
            }
        }
    }
    next.run(request).await
}

/// A CI-minted session may only reach its scoped endpoint on `/app/api`.
pub(crate) async fn refuse_scoped_elsewhere(request: Request, next: Next) -> Response {
    if let Some(session) = request.extensions().get::<SessionData>() {
        if let Some(scope) = &session.scope {
            if !scope.allows_request(request.method().as_str(), request.uri().path()) {
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({
                        "error": "scope_violation",
                        "message": store::seed::ScopeViolation::EndpointNotScoped.to_string(),
                    })),
                )
                    .into_response();
            }
        }
    }
    next.run(request).await
}

async fn reconcile_seed(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Json(input): Json<SeedRequest>,
) -> Response {
    let outcome = async {
        let model = store::seed::SeedModel::parse(&input.model)?;
        let (session_scope, as_ci_scope) = seed_project_scope(&session, &input, model)?;
        if model == store::seed::SeedModel::AttorneyRate {
            return reconcile_attorney_rate_seed(
                &state,
                &session,
                &input,
                session_scope.as_deref(),
                as_ci_scope.as_deref(),
            )
            .await;
        }
        let report = store::seed::reconcile_yaml(
            &state.application_store,
            model,
            &input.yaml,
            &state.bootstrap_company,
            input.overwrite,
            &store::seed::ReconcileActor {
                bootstrap_owner_email: state.bootstrap_owner_email.as_deref(),
                actor_role: session.role,
                may_change_roles: may_change_roles(&session) && !input.as_ci,
                project_scope: session_scope.as_deref().or(as_ci_scope.as_deref()),
                dry_run: input.dry_run,
                actor_person_id: session.person_id,
            },
        )
        .await?;
        Ok(serde_json::to_value(report)?)
    }
    .await;
    match outcome {
        Ok(report) => (StatusCode::OK, Json(report)).into_response(),
        Err(error) => {
            // A scope refusal is distinguishable from an ordinary malformed
            // or invalid seed document — a 403 the audit trail can tell
            // apart from a 422, per ENG-344.
            if error
                .downcast_ref::<store::seed::ScopeViolation>()
                .is_some()
            {
                (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({
                        "error": "scope_violation",
                        "message": error.to_string(),
                    })),
                )
                    .into_response()
            } else {
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(serde_json::json!({
                        "error": "invalid_seed",
                        "message": error.to_string(),
                    })),
                )
                    .into_response()
            }
        }
    }
}

fn seed_project_scope(
    session: &SessionData,
    input: &SeedRequest,
    model: store::seed::SeedModel,
) -> anyhow::Result<(Option<String>, Option<String>)> {
    match &session.scope {
        None => {}
        Some(SessionScope::Seed(scope)) => {
            if scope.endpoint != SEED_ENDPOINT {
                return Err(anyhow::Error::new(
                    store::seed::ScopeViolation::EndpointNotScoped,
                ));
            }
            if !scope.models.contains(&model) {
                return Err(anyhow::Error::new(
                    store::seed::ScopeViolation::ModelNotScoped(model.term()),
                ));
            }
            if scope.dry_run_only && (!input.dry_run || input.overwrite) {
                return Err(anyhow::Error::new(store::seed::ScopeViolation::DryRunOnly));
            }
        }
        Some(SessionScope::Document(_)) => {
            return Err(anyhow::Error::new(
                store::seed::ScopeViolation::EndpointNotScoped,
            ));
        }
    }
    // `as_ci` is a validation capability, exactly like the pull-request CI
    // session it stands in for: a live write under borrowed scope rules is refused.
    let as_ci_scope = if input.as_ci {
        if !input.dry_run || input.overwrite {
            return Err(anyhow::Error::new(store::seed::ScopeViolation::DryRunOnly));
        }
        Some(
            input
                .project
                .as_deref()
                .map(str::trim)
                .filter(|code| !code.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("as_ci requires `project`: the code CI would be scoped to")
                })?
                .to_owned(),
        )
    } else {
        None
    };
    let session_scope = session
        .scope
        .as_ref()
        .and_then(SessionScope::as_seed)
        .map(|scope| scope.project_code.clone());
    Ok((session_scope, as_ci_scope))
}

async fn reconcile_attorney_rate_seed(
    state: &ApiState,
    session: &SessionData,
    input: &SeedRequest,
    session_scope: Option<&str>,
    as_ci_scope: Option<&str>,
) -> anyhow::Result<serde_json::Value> {
    if input.overwrite {
        anyhow::bail!("Attorney Rate history is insert-only");
    }
    let project_code = session_scope
        .or(as_ci_scope)
        .or(input.project.as_deref())
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .ok_or_else(|| anyhow::anyhow!("attorney_rate import requires a Project scope"))?;
    if let Some(scoped) = session_scope {
        if !scoped.eq_ignore_ascii_case(project_code) {
            return Err(anyhow::Error::new(
                store::seed::ScopeViolation::CrossProject,
            ));
        }
    }
    let project = store::projects::find_by_code(&state.application_store, project_code)
        .await?
        .ok_or_else(|| {
            anyhow::Error::new(store::seed::ScopeViolation::UnknownProject(
                project_code.to_string(),
            ))
        })?;
    if !input.as_ci {
        let viewer = store::access::matter_viewer(
            &state.application_store,
            session.person_id,
            session.role,
            project.id,
        )
        .await
        .map_err(anyhow::Error::msg)?;
        if !viewer.is_some_and(store::access::MatterViewer::is_firm_side) {
            return Err(anyhow::Error::new(
                store::seed::ScopeViolation::CrossProject,
            ));
        }
    }
    let pg = crate::time_tracking_api::prepared_store(
        &state.application_store,
        "import_attorney_rates",
        project.id,
    )
    .await
    .map_err(|_| anyhow::anyhow!("PostgreSQL is unavailable"))?;
    let report =
        store::attorney_rates::import_project_seed(&pg, project.id, &input.yaml, input.dry_run)
            .await?;
    Ok(serde_json::json!({
        "model": store::seed::SeedModel::AttorneyRate.term(),
        "created": report.created,
        "updated": 0,
        "unchanged": report.unchanged,
    }))
}

/// `PATCH /app/api/entities/{id}` — the Entity update command. Same lawyer-tier
/// gate as create. Every field is optional and a partial update: an absent
/// field leaves its column unchanged, so a caller correcting only
/// `jurisdiction_id` sends only that field. The firm anchor's *name* is
/// immutable while its type and jurisdiction stay editable, and a rename
/// into the anchor's name is refused. Those rules live in
/// `store::entity_commands::update_entity`, which the `/app/admin/entities/{id}`
/// edit form calls too.
async fn update_entity(
    State(state): State<ApiState>,
    _lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<store::entity_commands::EntityPatch>,
) -> Result<Json<store::entities::Entity>, ApiError> {
    let updated = store::entity_commands::patch_entity(
        &state.application_store,
        id,
        &state.bootstrap_company,
        &input,
    )
    .await?;
    Ok(Json(updated))
}

/// `DELETE /app/api/entities/{id}` — the Entity delete command. Same lawyer-tier
/// gate as create and update. The firm anchor is undeletable (409), and an
/// Entity other rows still reference is refused, naming the tables and
/// counts that still point at it. Those rules live in
/// `store::entity_commands::delete_entity`, which the lawyer delete button
/// calls too. Returns the removed row.
async fn delete_entity(
    State(state): State<ApiState>,
    _lawyer: LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Json<store::entities::Entity>, ApiError> {
    let deleted = store::entity_commands::delete_entity(
        &state.application_store,
        id,
        &state.bootstrap_company,
    )
    .await?;
    Ok(Json(deleted))
}

/// Request body for `POST /app/api/projects` — open a matter. Every field is the
/// caller's *except* the attester, which the server resolves from the
/// authenticated session (never trusting the client to name who attested).
#[derive(Debug, Deserialize)]
struct OpenProjectRequest {
    #[serde(default)]
    firm_id: Option<Uuid>,
    #[serde(default)]
    brand: Option<String>,
    name: String,
    /// The matter's code, stored exactly as given. Required — no
    /// `serde(default)`, so an omitted `code` is a 422 at deserialize.
    /// `store::projects::open_matter` does not generate or append anything to
    /// it: a code is chosen once and never changes (`docs/glossary/project.md`),
    /// so a collision with an existing matter's code is a 409, not silently
    /// resolved.
    code: String,
    client_id: Uuid,
    entity_id: Uuid,
    #[serde(default)]
    jurisdiction_id: Option<Uuid>,
    #[serde(default)]
    description: Option<String>,
    /// The opening attorney's conflict attestation. Must be `true`: a Project
    /// open with no attestation is refused. The attorney attests they have
    /// checked for conflicts, and that either none prevent the open or this
    /// Project is not legal advice.
    #[serde(default)]
    attestation: bool,
    /// Open the matter already closed — an engagement that ended before
    /// anyone opened its row. The only accepted value is `"closed"`;
    /// omitted, this is the ordinary open. Requires [`Self::closed_at`],
    /// and refused if it is present without this.
    #[serde(default)]
    status: Option<String>,
    /// The close time. Required exactly when [`Self::status`] is
    /// `"closed"`, and validated by the same
    /// [`store::projects::transition_project`] this door calls afterward —
    /// an effective time may not precede the matter's own open or fall in
    /// the future.
    #[serde(default)]
    closed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// `POST /app/api/projects` — open a matter. Lawyer-tier only, which at this firm
/// means an attorney: the `LawyerSession` gate is therefore the "an attorney is
/// acting" check, and the session's person is the attester recorded on the
/// audit row and designated the accountable lawyer DRI. The command
/// (`store::projects::open_matter`) runs the conflict check, requires the
/// attestation on every open, and writes the audit trail; a blocking conflict
/// (adverse to a current client) is a hard 409 no attestation overrides. The
/// Drive ingest folder and source repository are then created or adopted
/// best-effort: a Drive or forge fault leaves the matter open, and
/// [`reconcile_project_surfaces_door`] retries.
///
/// Opening a matter already closed is the same call with `status: "closed"`
/// and a required `closed_at`: [`store::projects::OpenMatterCommand`] writes
/// `status`/`closed_at` into the same insert the open already makes, so an
/// engagement that ended before anyone opened its row is recorded in one
/// call rather than an open followed by a separate close. This is
/// deliberately not create-then-[`store::projects::transition_project`]:
/// that command refuses an effective time before the row's own
/// `inserted_at`, which a row created this instant would make impossible to
/// satisfy for any genuinely historical close date.
async fn open_project(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    JsonOrForm(input): JsonOrForm<OpenProjectRequest>,
) -> Result<Response, ApiError> {
    // The attester must resolve to a person: a lawyer session always carries
    // one, and a matter cannot be opened (nor attested) by a session that
    // doesn't name who is acting.
    let acting = session.person_id.ok_or(ApiError::Forbidden)?;
    match input.status.as_deref() {
        Some("closed") if input.closed_at.is_none() => {
            return Err(ApiError::Project(
                store::projects::ProjectCommandError::Invalid(
                    "closed_at is required when status is \"closed\"",
                ),
            ));
        }
        Some("closed") | None => {}
        Some(_) => {
            return Err(ApiError::Project(
                store::projects::ProjectCommandError::Invalid(
                    "status must be \"closed\" when present",
                ),
            ));
        }
    }
    if input.status.is_none() && input.closed_at.is_some() {
        return Err(ApiError::Project(
            store::projects::ProjectCommandError::Invalid(
                "closed_at is only accepted alongside status: \"closed\"",
            ),
        ));
    }
    let brand = selected_project_brand(&state, &session, input.firm_id, input.brand).await?;
    let command = store::projects::OpenMatterCommand {
        firm_id: input.firm_id,
        name: input.name,
        code: input.code,
        client_id: input.client_id,
        entity_id: input.entity_id,
        jurisdiction_id: input.jurisdiction_id,
        description: input.description,
        brand,
        attestation: input.attestation,
        acting_person_id: acting,
        closed_at: input.closed_at,
    };
    let matter = store::projects::open_matter(&state.application_store, &command).await?;
    store::project_surfaces::reconcile_after_open(&state.application_store, matter.id).await;
    Ok((StatusCode::CREATED, Json(matter)).into_response())
}

/// Validate explicit ownership before the matter-open command writes anything.
async fn selected_project_brand(
    state: &ApiState,
    session: &SessionData,
    firm_id: Option<Uuid>,
    brand: Option<String>,
) -> Result<String, ApiError> {
    if firm_id.is_some() != brand.is_some() {
        return Err(ApiError::Project(
            store::projects::ProjectCommandError::Invalid(
                "firm_id and brand must be supplied together",
            ),
        ));
    }
    if let (Some(firm_id), Some(brand)) = (firm_id, brand.as_deref()) {
        require_firm_administration(&state.application_store, session, firm_id).await?;
        let wearing = store::firms::firm_id_for_brand_key(&state.application_store, brand)
            .await
            .map_err(|error| ApiError::Db(error.to_string()))?;
        if wearing != Some(firm_id) {
            return Err(ApiError::Project(
                store::projects::ProjectCommandError::Invalid(
                    "the Firm must wear the selected brand",
                ),
            ));
        }
    }
    Ok(brand.unwrap_or_else(|| views::brand::brand_key().as_str().to_string()))
}

/// Authorize administration of one explicitly selected Firm.
pub(crate) async fn require_firm_administration(
    application_store: &store::application_store::ApplicationStore,
    session: &SessionData,
    firm_id: Uuid,
) -> Result<(), ApiError> {
    if !session.role.is_admin_tier() || session.viewing_as_dri.is_some() {
        return Err(ApiError::Forbidden);
    }
    let decision = store::firm_capability::resolve(
        application_store,
        session.role,
        session.person_id,
        firm_id,
        store::firm_capability::FirmCapability::ManageMembership,
    )
    .await
    .map_err(|error| ApiError::Db(error.to_string()))?;
    if !decision.is_allowed() {
        return Err(ApiError::NotFound);
    }
    Ok(())
}

/// `PATCH /app/api/projects/{id}` — update a matter's descriptive fields (name,
/// entity, scope narrative) or, as Owner or an administering Firm Admin, its
/// Firm and brand together. Descriptive updates require Lawyer tier; ownership
/// changes require Owner or an Admin who can administer the target Firm. This
/// is the edit path, not matter-open: it runs no conflict check
/// and provisions no repo. It also never touches `status`/`closed_at` —
/// unknown fields, including `status` and an independently supplied
/// `closed_at`, are rejected during request deserialization; move a matter's
/// lifecycle through `POST /app/api/projects/{id}/lifecycle` instead. The
/// rules live in `store::projects::update_project`, which the
/// `/app/projects/{project_code}` edit form calls too. A patch naming a Drive
/// folder is first checked with the deployment's Drive provider, and the
/// response's `navigator-drive-folder-check` header says whether the folder
/// was `verified` or the check `skipped` for want of a provider.
async fn update_project(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<store::projects::UpdateProjectCommand>,
) -> Result<Response, ApiError> {
    if let Some(firm_id) = input.firm_id {
        require_firm_administration(&state.application_store, &session, firm_id).await?;
    }
    let drive_folder_check = match input.drive_folder_id.as_deref().map(str::trim) {
        Some(folder) if store::projects::is_valid_drive_folder_id(folder) => Some(
            store::project_surfaces::check_drive_folder_from_env(folder)
                .await
                .map_err(surface_error)?,
        ),
        _ => None,
    };
    let updated = store::projects::update_project(&state.application_store, id, &input).await?;
    let mut response = Json(updated).into_response();
    if let Some(check) = drive_folder_check {
        response.headers_mut().insert(
            store::project_surfaces::DRIVE_FOLDER_CHECK_HEADER,
            axum::http::HeaderValue::from_static(check.as_str()),
        );
    }
    Ok(response)
}

/// Request body for `POST /app/api/projects/{id}/lifecycle`. `status` and
/// `closed_at` are never independently settable: the transition owns both,
/// while `effective_at` may state when a close or archive actually took
/// effect. The shared command validates that timestamp before deriving the
/// coupled `closed_at` value.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransitionProjectRequest {
    transition: store::projects::Transition,
    reason: Option<LifecycleReason>,
    effective_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(untagged)]
enum LifecycleReason {
    Close(store::projects::ClosureReason),
    Reopen(store::projects::ReopenReason),
}

const PITCH_OFFBOARDING_FILENAME: &str = "pitch-offboarding-letter.pdf";

/// File the short closing letter required for an unengaged pitch. This runs
/// only for the initial close; later administrative corrections to `closed_at`
/// retain the original letter as the historical closure artifact.
/// The stable key makes retries resume after a successful file write.
async fn generate_pitch_offboarding_if_needed(
    state: &ApiState,
    project: &store::projects::Project,
    effective_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<(), ApiError> {
    if let Some(effective_at) = effective_at {
        let now = chrono::Utc::now();
        if effective_at > now {
            return Err(ApiError::Project(
                store::projects::ProjectCommandError::Invalid(
                    "An effective date cannot be in the future.",
                ),
            ));
        }
        let opened_at = chrono::DateTime::parse_from_rfc3339(&project.inserted_at)
            .map_err(|error| ApiError::Db(error.to_string()))?
            .with_timezone(&chrono::Utc);
        if effective_at < opened_at {
            return Err(ApiError::Project(
                store::projects::ProjectCommandError::Invalid(
                    "An effective date cannot precede the matter's opened date.",
                ),
            ));
        }
    }

    let client_name = if let Some(entity) =
        store::entities::find_by_id(&state.application_store, project.entity_id).await?
    {
        entity.name
    } else {
        let dri_id =
            store::projects::participations_for_project(&state.application_store, project.id)
                .await
                .map_err(|error| ApiError::Db(error.to_string()))?
                .into_iter()
                .find(|participant| participant.is_client_dri)
                .map(|participant| participant.person_id)
                .ok_or_else(|| ApiError::Db("Pitch has no client Entity or DRI Person.".into()))?;
        store::persons::find_by_id(&state.application_store, dri_id)
            .await?
            .map(|person| person.name)
            .ok_or_else(|| ApiError::Db("Pitch client DRI Person was not found.".into()))?
    };
    let letter_date = effective_at
        .unwrap_or_else(chrono::Utc::now)
        .format("%B %-d, %Y")
        .to_string();
    let typst_source = pitch_offboarding_typst(&client_name, &letter_date);
    let bytes = pdf::render(&typst_source).map_err(|error| ApiError::Db(error.to_string()))?;
    let args = store::documents::IngestArgs {
        project_id: project.id,
        source: store::documents::source::GENERATED,
        filename: PITCH_OFFBOARDING_FILENAME,
        kind: "offboarding",
        content_type: "application/pdf",
        description: Some("Generated pitch offboarding letter"),
        secondary_storage_key: None,
        visibility: store::documents::visibility::INTERNAL,
    };
    let idempotency_key = format!(
        "pitch-offboarding/v1/{}/{}",
        project.id, PITCH_OFFBOARDING_FILENAME
    );
    store::documents::ingest_bytes_with_idempotency_key(
        &state.application_store,
        &state.storage,
        &args,
        &idempotency_key,
        &bytes,
    )
    .await
    .map_err(|error| match error {
        store::documents::IngestError::IdempotencyConflict(_) => ApiError::PitchArtifactConflict,
        error => ApiError::Ingest(error),
    })?;
    Ok(())
}

/// Escape a value as a Typst string literal so client names cannot be parsed
/// as markup or executable Typst source.
fn typst_string(value: &str) -> String {
    format!("#\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn pitch_offboarding_typst(client_name: &str, letter_date: &str) -> String {
    format!(
        "#set page(paper: \"us-letter\", margin: 1in)\n#set text(size: 11pt)\n\n{}\n\n{} did not accept the pitch.\n",
        typst_string(letter_date),
        typst_string(client_name),
    )
}

/// `POST /app/api/projects/{id}/lifecycle` — move a matter directly through
/// its lifecycle (`close`, `reopen`, or `archive`), the REST door onto
/// `store::projects::transition_project`. Distinct from `POST
/// /app/api/projects/{id}/close` (the closing-letter ceremony): no notation
/// opens here. Lawyer-tier only, same as the bare `PATCH`/`DELETE` matter
/// path — a lifecycle move is a firm-administration action, not scoped to
/// the acting lawyer's own matters. Idempotent per `transition_project`'s own
/// rules — a repeated transition the matter already made is a no-op success
/// — and `archived` is terminal, so any transition off it but a repeated
/// archive is refused as `400`.
async fn transition_matter(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<TransitionProjectRequest>,
) -> Result<Response, ApiError> {
    use store::projects::Transition;
    let (reason, reopen_reason) = match (input.transition, input.reason) {
        (Transition::Close, Some(LifecycleReason::Close(reason))) => (Some(reason), None),
        (Transition::Reopen, Some(LifecycleReason::Reopen(reason))) => (None, Some(reason)),
        (_, None) => (None, None),
        _ => {
            return Err(store::projects::ProjectCommandError::Invalid(
                "The reason must match the lifecycle transition.",
            )
            .into())
        }
    };
    if reopen_reason.is_some() && !participates_as_firm(&state, &lawyer, id).await {
        return Err(ApiError::NotFound);
    }
    // Final archival waits on a verified recovery generation cut after the
    // close and every required handoff's acknowledgment; a repeat of an
    // archive already done is not gated again.
    if input.transition == store::projects::Transition::Archive {
        let already = store::projects::find_by_id(&state.application_store, id)
            .await
            .map_err(|error| ApiError::Db(error.to_string()))?
            .is_some_and(|project| project.status == "archived");
        if !already {
            let pg = store::postgres::PostgresDb::from_env().await.ok();
            let readiness = store::project_closeout::readiness(
                &state.application_store,
                pg.as_ref(),
                id,
                store::project_closeout::Closeout::Archive,
            )
            .await
            .map_err(ApiError::Db)?;
            if !readiness.is_ready() {
                return Ok((
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error": "archive_not_ready",
                        "blockers": readiness.blockers,
                    })),
                )
                    .into_response());
            }
        }
    }
    if input.transition == store::projects::Transition::Close
        && reason.is_some_and(|reason| {
            matches!(
                reason,
                store::projects::ClosureReason::PitchDeclined
                    | store::projects::ClosureReason::PitchLapsed
                    | store::projects::ClosureReason::PitchWithdrawn
            )
        })
    {
        prepare_pitch_close(&state, id, input.effective_at).await?;
    }
    let updated = store::projects::transition_project_attested(
        &state.application_store,
        id,
        input.transition,
        reason,
        reopen_reason,
        input.effective_at,
        lawyer.0.person_id,
    )
    .await?;
    let updated = if updated.status == "open" {
        updated
    } else {
        close_project_calendar(&state, updated).await
    };
    // The private Slack channel follows the matter's status right away; the
    // Nightly Project Update is the backstop. A provider fault is logged,
    // never a failed lifecycle move.
    let channel = crate::integrations_api::converge_project_channel(
        &state,
        lawyer.0.person_id,
        &updated,
        false,
    )
    .await;
    tracing::info!(
        project_id = %updated.id,
        outcome = channel.outcome,
        "project channel followed the lifecycle move"
    );
    Ok(Json(updated).into_response())
}

async fn prepare_pitch_close(
    state: &ApiState,
    id: Uuid,
    effective_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<(), ApiError> {
    let Some(project) = store::projects::find_by_id(&state.application_store, id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?
        .filter(|project| project.status == "open")
    else {
        return Ok(());
    };
    let projects = std::slice::from_ref(&project);
    let official = store::projects::onboarding_official_sets(&state.application_store, projects)
        .await
        .map_err(ApiError::Db)?;
    if official.contains(&id) {
        return Ok(());
    }
    let (_, offboarding) =
        store::projects::matter_lifecycle_sets(&state.application_store, projects)
            .await
            .map_err(ApiError::Db)?;
    if offboarding.contains(&id) {
        return Ok(());
    }
    generate_pitch_offboarding_if_needed(state, &project, effective_at).await
}

/// Make a closed or archived matter's calendar read-only and clear its
/// booking link. Best-effort: the matter is already closed, so a provider
/// fault is logged rather than reported as a failed close, and the next
/// `navigator project setup` converges it. Without a configured calendar the
/// booking link is still cleared.
async fn close_project_calendar(
    state: &ApiState,
    project: store::projects::Project,
) -> store::projects::Project {
    let result = match state
        .integration_providers
        .calendar(&state.application_store, project.id)
        .await
    {
        Ok(calendar) => {
            store::project_calendar::close(&state.application_store, project.id, calendar.as_ref())
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        }
        Err(_) => store::projects::clear_booking_url(&state.application_store, project.id)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string()),
    };
    if let Err(error) = result {
        tracing::error!(
            %error,
            project_id = %project.id,
            "Project calendar close failed; retry with navigator project setup"
        );
    }
    store::projects::find_by_id(&state.application_store, project.id)
        .await
        .ok()
        .flatten()
        .unwrap_or(project)
}

/// `DELETE /app/api/projects/{id}` — delete a matter. Lawyer-tier only. A matter
/// opened the normal way carries dependents (DRI participations, notations),
/// whose foreign keys block the delete — surfaced as
/// 409 with the database's own detail naming the referencing table, a conflict
/// the caller resolves by detaching those first, not a server fault. The rules
/// live in `store::projects::delete_project`, which the `/app/projects` door
/// calls too. Returns the removed matter.
async fn delete_project(
    State(state): State<ApiState>,
    _lawyer: LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Json<store::projects::Project>, ApiError> {
    let deleted = store::projects::delete_project(&state.application_store, id).await?;
    Ok(Json(deleted))
}

/// Request body for `POST /app/api/projects/{id}/participants` — add a person to a
/// matter's participation ledger. The matter is the path id; the body names the
/// person, and nothing else — the participation follows their `persons.role`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddParticipantRequest {
    person_id: Uuid,
}

/// `POST /app/api/projects/{id}/participants` — add a person to a matter's
/// participation ledger. Lawyer-tier and matter-scoped: the acting lawyer must
/// already participate in this matter (admin/owner bypass, same as
/// [`close_matter`]) — a lawyer with no row on the matter gets the same
/// non-disclosing `404` an unrelated caller would. The command
/// (`store::participation::add_participant`) validates the matter and person
/// exist, derives the participation from the person's tier, and enforces one row
/// per person + matter; the same command the lawyer participation form funnels
/// through. This is also the door a participating lawyer uses to grant or
/// revoke a Clerk's portal visibility on their own matter: adding a Clerk here
/// is what makes `store::access::can_see_project` admit them, and removing the
/// row (see [`remove_participant`]) is the toggle back off.
async fn add_participant(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<AddParticipantRequest>,
) -> Result<Response, ApiError> {
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    let row = store::participation::add_participant(
        &state.application_store,
        &store::participation::AddParticipantCommand {
            project_id: id,
            person_id: input.person_id,
            dri: store::participation::DriRequest::Unchanged,
            // This door cannot move a marker, so there is no DRI actor to name.
            // Wiring `dri` here means wiring the session's person id with it.
            actor: store::participation::DriActor::System,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(row)).into_response())
}

/// Request body for `POST /app/api/projects/{id}/relationships/adverse`: the
/// referenced party the client is adverse to, named by id or by the natural
/// key the seed documents use, and which side of the client the edge starts
/// from.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordAdversityRequest {
    #[serde(default)]
    person_id: Option<Uuid>,
    #[serde(default)]
    entity_id: Option<Uuid>,
    #[serde(default)]
    person_email: Option<String>,
    #[serde(default)]
    entity_name: Option<String>,
    #[serde(default)]
    subject: store::project_references::AdversitySubject,
}

impl RecordAdversityRequest {
    /// Exactly one party, resolved to the row it names. A natural key that
    /// names no row is a party the matter cannot reference either, so it is
    /// refused as unreferenced rather than as a bare `404`.
    async fn party(
        &self,
        application_store: &store::application_store::ApplicationStore,
    ) -> Result<store::project_references::Party, ApiError> {
        let named = [
            self.person_id.is_some(),
            self.entity_id.is_some(),
            self.person_email.is_some(),
            self.entity_name.is_some(),
        ]
        .into_iter()
        .filter(|named| *named)
        .count();
        if named != 1 {
            return Err(ApiError::MalformedBody(
                "name exactly one of person_id, entity_id, person_email, or entity_name".into(),
            ));
        }
        if let Some(id) = self.person_id {
            return Ok(store::project_references::Party::person(id));
        }
        if let Some(id) = self.entity_id {
            return Ok(store::project_references::Party::entity(id));
        }
        let unreferenced =
            || ApiError::from(store::project_references::AdversityError::PartyNotReferenced);
        if let Some(email) = self.person_email.as_deref() {
            let person = store::persons::find_by_email_ci(application_store, email)
                .await
                .map_err(|error| ApiError::Db(error.to_string()))?
                .ok_or_else(unreferenced)?;
            return Ok(store::project_references::Party::person(person.id));
        }
        let name = self.entity_name.as_deref().unwrap_or_default();
        let entity = store::entities::find_by_name(application_store, name)
            .await
            .map_err(|error| ApiError::Db(error.to_string()))?
            .ok_or_else(unreferenced)?;
        Ok(store::project_references::Party::entity(entity.id))
    }
}

/// `POST /app/api/projects/{id}/relationships/adverse` — record that the
/// matter's client is adverse to a party the matter references. Lawyer-tier
/// and matter-scoped like [`add_participant`]: a lawyer with no row on the
/// matter gets the same non-disclosing `404` an unrelated caller would. The
/// command (`store::project_references::record_adversity`) requires the party
/// to be a Project reference first, then lands a `manual` `adverse_to` edge at
/// full confidence sourced to the Relationship Log row it appends on the
/// Project — the write path that lets `store::conflicts::check_new_matter`
/// raise this adversity on the next open.
async fn record_adversity(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<RecordAdversityRequest>,
) -> Result<Response, ApiError> {
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    let party = input.party(&state.application_store).await?;
    let edge = store::project_references::record_adversity(
        &state.application_store,
        &store::project_references::RecordAdversityCommand {
            project_id: id,
            party,
            subject: input.subject,
            actor_person_id: lawyer.0.person_id,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(edge)).into_response())
}

/// Query for the `GET /app/api/graph/*` doors: the neighborhood depth.
/// Defaults to 2; anything above the store's hop ceiling is refused before
/// the graph is read — the cap is the server's, never the caller's.
#[derive(Deserialize, Default)]
struct GraphQuery {
    #[serde(default)]
    depth: Option<String>,
}

/// Parses the `depth` query parameter. `Err` carries the ready 400 response.
fn graph_depth(query: &GraphQuery) -> Result<u8, Response> {
    const DEFAULT_DEPTH: &str = "2";
    let raw = query.depth.as_deref().unwrap_or(DEFAULT_DEPTH);
    let depth: u8 = raw
        .parse()
        .map_err(|_| bad_request("invalid_depth", "depth must be a non-negative integer."))?;
    if depth == 0 || depth > store::postgres_graph_views::MAX_GRAPH_DEPTH {
        return Err(bad_request(
            "depth_out_of_range",
            "depth must be between 1 and 3.",
        ));
    }
    Ok(depth)
}

/// The AGE graph pool. The graph views read only from the projection, so a
/// deployment with no configured PostgreSQL host has nothing to serve — the
/// door reports 503 rather than pretending the graph is empty.
async fn graph_store() -> Result<store::postgres::PostgresDb, Response> {
    store::postgres::PostgresDb::from_env()
        .await
        .map_err(|error| {
            tracing::error!(%error, "api: graph store unavailable");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "graph_requires_postgresql" })),
            )
                .into_response()
        })
}

/// Whether the lawyer-tier caller may read the lawyer graph of `person_id`:
/// the person themself, or a caller who holds
/// [`store::firm_capability::FirmCapability::ViewDirectory`] on a Firm the
/// target belongs to — the same directory signal the firm surface uses,
/// never an inference from graph shape. Membership or resolution failures
/// deny, and deny silently (the door answers `404` either way).
async fn can_view_lawyer_graph(state: &ApiState, lawyer: &LawyerSession, person_id: Uuid) -> bool {
    if lawyer.0.person_id.is_some_and(|viewer| viewer == person_id) {
        return true;
    }
    let memberships =
        match store::firms::memberships_for_person(&state.application_store, person_id).await {
            Ok(memberships) => memberships,
            Err(error) => {
                tracing::error!(%error, %person_id, "api: lawyer-graph membership read failed");
                return false;
            }
        };
    for membership in memberships {
        let decision = store::firm_capability::FirmCapability::ViewDirectory
            .resolve(
                &state.application_store,
                lawyer.0.role,
                lawyer.0.person_id,
                membership.firm_id,
            )
            .await;
        if decision.is_ok_and(store::firm_capability::FirmCapabilityDecision::is_allowed) {
            return true;
        }
    }
    false
}

/// `GET /app/api/graph/project/{id}?depth=2` — the matter's neighborhood:
/// its parties, their entity roles, the adverse/related relationships, and
/// the firms handling each side — the conflict check, made visible.
/// Lawyer-tier and matter-scoped like [`record_adversity`]: a caller with no
/// view of the matter gets the same non-disclosing `404`. The neighborhood
/// itself is a read-only AGE view (`store::postgres_graph_views`); depth is
/// capped server-side.
async fn project_graph(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    Query(query): Query<GraphQuery>,
) -> Result<Response, ApiError> {
    let depth = match graph_depth(&query) {
        Ok(depth) => depth,
        Err(response) => return Ok(response),
    };
    let in_scope = store::access::can_see_project(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    let pg = match graph_store().await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    let view = store::postgres_graph_views::project_view(&pg, id, depth)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    let viewer = lawyer.0.person_id.filter(|_| lawyer.0.scope.is_none());
    let observe = if viewer.is_some() {
        let project = store::projects::find_by_id(&state.application_store, id)
            .await
            .map_err(|e| ApiError::Db(e.to_string()))?;
        if let Some(firm_id) = project.and_then(|project| project.firm_id) {
            participates_as_firm(&state, &lawyer, id).await
                && has_relationship_log_membership(&state, &lawyer, firm_id).await
        } else {
            false
        }
    } else {
        false
    };
    graph_with_observations(
        &pg,
        graph_logs::Scope::Project,
        id,
        viewer.filter(|_| observe),
        view,
    )
    .await
}

/// `GET /app/api/graph/lawyer/{id}?depth=2` — one person's working
/// neighborhood: matters worked on, entity roles, firm membership, and the
/// client/adverse relationships around them. Lawyer-tier; visible to the
/// person themself or to a caller authorized through the firm directory
/// ([`can_view_lawyer_graph`]) — anyone else gets the non-disclosing `404`.
async fn lawyer_graph(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    Query(query): Query<GraphQuery>,
) -> Result<Response, ApiError> {
    let depth = match graph_depth(&query) {
        Ok(depth) => depth,
        Err(response) => return Ok(response),
    };
    if !can_view_lawyer_graph(&state, &lawyer, id).await {
        return Err(ApiError::NotFound);
    }
    let pg = match graph_store().await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    let view = store::postgres_graph_views::lawyer_view(&pg, id, depth)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    graph_with_observations(
        &pg,
        graph_logs::Scope::Attorney,
        id,
        lawyer
            .0
            .person_id
            .filter(|viewer| lawyer.0.scope.is_none() && *viewer == id),
        view,
    )
    .await
}

/// `GET /app/api/graph/firm/{id}` — firm-to-firm ties aggregated up from
/// person-level edges: adversary, co-counsel, of-counsel, client. Lawyer-tier
/// and directory-gated (`FirmCapability::ViewDirectory` on the Firm itself);
/// the aggregation is a fixed three-hop read, so it takes no meaningful
/// `depth` — the parameter is validated for consistency and then ignored.
async fn firm_graph(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    Query(query): Query<GraphQuery>,
) -> Result<Response, ApiError> {
    if let Err(response) = graph_depth(&query) {
        return Ok(response);
    }
    if !may_view_firm_graph(&state, &lawyer, id).await {
        return Err(ApiError::NotFound);
    }
    let pg = match graph_store().await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    let view = store::postgres_graph_views::firm_view(&pg, id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    let viewer = lawyer.0.person_id.filter(|_| lawyer.0.scope.is_none());
    let observe = if viewer.is_some() {
        has_relationship_log_membership(&state, &lawyer, id).await
    } else {
        false
    };
    graph_with_observations(
        &pg,
        graph_logs::Scope::Firm,
        id,
        viewer.filter(|_| observe),
        view,
    )
    .await
}

/// Request body for `PATCH /app/api/projects/{id}/participants/{role_id}` — re-point
/// a participation row at a different person.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateParticipantRequest {
    person_id: Uuid,
}

/// `PATCH /app/api/projects/{id}/participants/{role_id}` — edit a matter
/// participation row. Lawyer-tier and matter-scoped, like [`add_participant`].
/// The command (`store::participation::update_participant`) validates the row
/// belongs to the matter and the person exists, re-derives the participation
/// from that person's tier, and checks no other row duplicates the person and
/// that the edit does not strand the matter's lawyer DRI.
async fn update_participant(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path((id, role_id)): Path<(Uuid, Uuid)>,
    JsonOrForm(input): JsonOrForm<UpdateParticipantRequest>,
) -> Result<Response, ApiError> {
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    let row = store::participation::update_participant(
        &state.application_store,
        &store::participation::UpdateParticipantCommand {
            project_id: id,
            role_id,
            person_id: input.person_id,
            dri: store::participation::DriRequest::Unchanged,
            // As above: no marker moves through this door, so no actor is read.
            actor: store::participation::DriActor::System,
        },
    )
    .await?;
    Ok((StatusCode::OK, Json(row)).into_response())
}

/// `DELETE /app/api/projects/{id}/participants/{role_id}` — remove a matter
/// participation row. Lawyer-tier and matter-scoped, like [`add_participant`].
/// The command (`store::participation::remove_participant`) refuses to remove
/// the matter's *last* lawyer DRI (that would strand its accountable lawyer).
/// `204 No Content` on success.
///
/// Removing a row that carries a marker is a DRI change, so this door names its
/// actor: the command gates the removal on it and writes it to the audit trail.
/// A session with no `persons` row cannot be that actor and is refused.
async fn remove_participant(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path((id, role_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    let acting = lawyer.0.person_id.ok_or(ApiError::Forbidden)?;
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    store::participation::remove_participant(
        &state.application_store,
        id,
        role_id,
        store::participation::DriActor::Person(acting),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `PUT /app/api/projects/{id}/participants/{role_id}/dri` — designate the
/// participant named by `role_id` as one of the matter's DRIs. The side
/// (lawyer or client) follows from that person's tier, exactly as the lawyer
/// workbench derives it, so the door takes no body. Lawyer-tier only, and the
/// shared command additionally gates the change on the caller already holding
/// the marker for that side — a matter's lawyer DRIs govern their own set — so
/// a lawyer who is not a current holder is refused `403`. `204` on success.
async fn designate_participant_dri(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path((id, role_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    let acting = lawyer.0.person_id.ok_or(ApiError::Forbidden)?;
    let row = participation_on_matter(&state.application_store, id, role_id).await?;
    let person = store::persons::find_by_id(&state.application_store, row.person_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let side = if person.role.is_lawyer_tier() {
        store::projects::DriSide::Lawyer
    } else {
        store::projects::DriSide::Client
    };
    store::participation::update_participant(
        &state.application_store,
        &store::participation::UpdateParticipantCommand {
            project_id: id,
            role_id,
            person_id: row.person_id,
            dri: store::participation::DriRequest::Designate(side),
            actor: store::participation::DriActor::Person(acting),
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `DELETE /app/api/projects/{id}/participants/{role_id}/dri` — clear the DRI
/// marker from the participant named by `role_id`. Lawyer-tier only and
/// actor-gated like designation; the shared command refuses to clear the
/// matter's last lawyer DRI (a matter always keeps one), answering `422`.
/// `204 No Content` on success. Clearing a marker that is not set is a no-op
/// the command accepts.
async fn remove_participant_dri(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path((id, role_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    let acting = lawyer.0.person_id.ok_or(ApiError::Forbidden)?;
    let row = participation_on_matter(&state.application_store, id, role_id).await?;
    store::participation::update_participant(
        &state.application_store,
        &store::participation::UpdateParticipantCommand {
            project_id: id,
            role_id,
            person_id: row.person_id,
            dri: store::participation::DriRequest::Clear,
            actor: store::participation::DriActor::Person(acting),
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// One participation row confirmed to belong to `project_id`, or
/// [`ApiError::NotFound`]. The API mirror of the workbench's `participation_row`:
/// a `role_id` from another matter must not disclose that it exists, so a
/// mismatch — like a missing row or a lookup failure — collapses to 404.
async fn participation_on_matter(
    application_store: &store::application_store::ApplicationStore,
    project_id: Uuid,
    role_id: Uuid,
) -> Result<store::projects::PersonProjectRole, ApiError> {
    store::projects::participation_by_id(application_store, role_id)
        .await
        .ok()
        .flatten()
        .filter(|row| row.project_id == project_id)
        .ok_or(ApiError::NotFound)
}

/// `POST /app/api/projects/{id}/close` — open the matter's firm-signed
/// closing-letter notation, the REST mirror of the lawyer close control. Both
/// converge on `retainer_walk::open_closing_notation`. Lawyer-tier only and
/// matter-scoped: a lawyer who does not participate in the matter (admin
/// bypasses) gets a bare 404. `201 Created` carrying the new notation id; a
/// matter with no client to address the letter to is `409`. The status flip to
/// `closed` is the close workflow's job once the walk completes, not this door's.
async fn close_matter(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    match crate::retainer_walk::open_closing_notation(&state.application_store, &state.storage, id)
        .await
    {
        Ok(notation_id) => Ok((
            StatusCode::CREATED,
            Json(serde_json::json!({ "notation_id": notation_id })),
        )
            .into_response()),
        Err(crate::retainer_walk::CloseMatterError::MatterNotFound) => Err(ApiError::NotFound),
        Err(crate::retainer_walk::CloseMatterError::NoClient) => Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "no_client",
                "message": "This matter has no client to address the closing letter to."
            })),
        )
            .into_response()),
        Err(e) => {
            tracing::error!(error = %e, project_id = %id, "api close_matter: failed");
            Err(ApiError::Db(
                "the closing notation could not be opened".into(),
            ))
        }
    }
}

/// Request body for `POST /app/api/projects/{id}/conversation/messages` — the
/// message body and, for the lawyer lens only, whether it is an internal note.
#[derive(Deserialize)]
struct PostConversationMessageRequest {
    body: String,
    #[serde(default)]
    internal: bool,
}

/// `POST /app/api/projects/{id}/conversation/messages` — post one message to a
/// matter's conversation, the REST mirror of the portal message control. Both
/// converge on `conversation::post_conversation_message`. Client-writable: any
/// authenticated caller reaches it (embedded Rego), and the command enforces
/// either-lens matter access (a non-participant is a non-disclosing 404). The
/// tier decides the side (inbound for a client, outbound/internal for a lawyer);
/// a client's `internal` flag is ignored. `204` on success; an empty body is
/// `400`.
async fn post_conversation_message_door(
    State(state): State<ApiState>,
    authed: AuthedSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<PostConversationMessageRequest>,
) -> Result<Response, ApiError> {
    match crate::conversation::post_conversation_message(
        &state.application_store,
        authed.0.person_id,
        authed.0.role,
        id,
        &input.body,
        input.internal,
    )
    .await
    {
        Ok(()) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(crate::conversation::PostMessageError::NotAuthorized) => Err(ApiError::NotFound),
        Err(crate::conversation::PostMessageError::EmptyBody) => Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "empty_body",
                "message": "The message body is empty."
            })),
        )
            .into_response()),
        Err(e) => {
            tracing::error!(error = %e, project_id = %id, "api conversation message: failed");
            Err(ApiError::Db("the message could not be posted".into()))
        }
    }
}

/// Request body for `POST /app/api/projects/{id}/notations` — open a notation on
/// an existing matter from a template (authored in the matter's repo, or the
/// firm catalog fallback), bound to a client email.
#[derive(Deserialize)]
struct CreateNotationRequest {
    /// Canonical template code. `draft_code` is that same code under the
    /// lawyer's name for a Template. Both keys are invalid, including when
    /// the values are equal. The HTML form posts `template_code` unchanged.
    #[serde(alias = "draft_code")]
    template_code: String,
    client_email: String,
    #[serde(default)]
    delivery: store::notations::SigningDelivery,
}

/// Success body for `POST /app/api/projects/{id}/notations` — the opened
/// notation's id, so the caller can drive its questionnaire next.
#[derive(Serialize)]
struct CreateNotationResponse {
    notation_id: Uuid,
}

/// `POST /app/api/projects/{id}/notations` — open a notation on a matter. Lawyer-tier
/// only (embedded Rego policy) *and* matter-scoped: the shared command
/// (`crate::project_notation::create_project_notation`) re-checks that the
/// acting lawyer participates in the project (admin bypasses), collapsing an
/// out-of-scope matter to 404 so the door never discloses it. This is the same
/// command the lawyer browser form drives, so both surfaces open notations
/// identically. `201 Created` with the new notation id on success.
async fn create_notation(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<CreateNotationRequest>,
) -> Result<Response, ApiError> {
    let outcome = crate::project_notation::create_project_notation(
        &state.application_store,
        state.questionnaire_runtime.as_ref(),
        &state.storage,
        lawyer.0.person_id,
        lawyer.0.role,
        id,
        &input.template_code,
        &input.client_email,
        input.delivery,
        None,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(CreateNotationResponse {
            notation_id: outcome.notation_id,
        }),
    )
        .into_response())
}

#[derive(Deserialize)]
struct AdoptEnvelopeRequest {
    envelope_id: Uuid,
    template_code: String,
    source: String,
    commit_sha: String,
}

/// Read provider state without registering, filing, or advancing the Project.
async fn lookup_project_envelope(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path((id, envelope_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    if !participates_as_firm(&state, &lawyer, id).await {
        return Err(ApiError::NotFound);
    }
    let (envelope, notation) = read_project_envelope(&state, id, envelope_id).await?;
    let envelope = crate::signature_status::project_envelope_json(
        &state.application_store,
        &state.storage,
        notation.as_ref(),
        &envelope,
    )
    .await
    .map_err(crate::signature_adoption::internal)?;
    Ok(Json(envelope).into_response())
}

async fn read_project_envelope(
    state: &ApiState,
    id: Uuid,
    envelope_id: Uuid,
) -> Result<
    (
        crate::signature::ExternalEnvelope,
        Option<store::notations::Notation>,
    ),
    ApiError,
> {
    use crate::signature::SignatureProvider as _;
    use crate::signature_adoption::internal;
    let db = &state.application_store;
    let firm = store::projects::find_by_id(db, id)
        .await
        .map_err(internal)?
        .and_then(|project| project.firm_id)
        .ok_or(ApiError::NotFound)?;
    let provider = state
        .signature_provider
        .for_project(db, id)
        .await
        .map_err(ApiError::SignatureUnavailable)?
        .ok_or(ApiError::NotFound)?;
    if !crate::docusign_completion::unique_account(
        db,
        state.signature_provider.as_ref(),
        firm,
        provider.account_id(),
    )
    .await
    .map_err(internal)?
    {
        return Err(ApiError::NotFound);
    }
    let signature = store::signatures::by_provider(
        db,
        store::signatures::SignatureProvider::DocuSign,
        &envelope_id.to_string(),
    )
    .await
    .map_err(internal)?;
    let notation = match signature {
        Some(signature) => Some(
            store::notations::find_by_id(db, signature.notation_id)
                .await?
                .filter(|notation| notation.project_id == id)
                .ok_or(ApiError::NotFound)?,
        ),
        None => None,
    };
    let envelope = provider
        .read_external_envelope(envelope_id)
        .await
        .map_err(|_| ApiError::NotFound)?;
    if notation.is_none() {
        let (fields, names) = provider
            .envelope_project_hints(envelope_id)
            .await
            .map_err(internal)?;
        let matched =
            crate::docusign_completion::identify_project(db, firm, &envelope, &fields, &names)
                .await
                .map_err(internal)?;
        if !matched.is_ok_and(|project| project.id == id) {
            return Err(ApiError::NotFound);
        }
    }
    Ok((envelope, notation))
}

/// Refresh only envelopes already correlated to this participating Project.
async fn refresh_project_envelopes(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    use crate::signature_adoption::{internal, lock, reconcile};
    if !participates_as_firm(&state, &lawyer, id).await {
        return Err(ApiError::NotFound);
    }
    let scoped = state
        .signature_provider
        .for_project(&state.application_store, id)
        .await
        .map_err(ApiError::SignatureUnavailable)?;
    let provider =
        crate::firm_docusign::resolved(scoped.as_ref(), state.signature_provider.as_ref());
    let lane = lock().await?;
    let account = if let Some(provider) = scoped.as_ref() {
        let pg = store::postgres::PostgresDb::from_env()
            .await
            .map_err(internal)?;
        let firm = store::projects::find_by_id(&state.application_store, id)
            .await
            .map_err(internal)?
            .and_then(|project| project.firm_id)
            .ok_or(ApiError::NotFound)?;
        let account = provider.account_id().parse().map_err(internal)?;
        Some((pg, firm, account))
    } else {
        None
    };
    let mut refreshed = Vec::new();
    for notation in store::notations::list_by_project(&state.application_store, id).await? {
        let Some(signature) =
            store::signatures::latest_for_notation(&state.application_store, notation.id)
                .await
                .map_err(internal)?
        else {
            continue;
        };
        if signature.provider != store::signatures::SignatureProvider::DocuSign.as_str() {
            continue;
        }
        let envelope_id = signature.provider_id.parse().map_err(internal)?;
        let started = chrono::Utc::now();
        let envelope = provider
            .read_external_envelope(envelope_id)
            .await
            .map_err(internal)?;
        if envelope.envelope_id != envelope_id {
            return Err(ApiError::EnvelopeAdoption(
                "DocuSign returned a different envelope",
            ));
        }
        // An unsent provider draft remains a draft, never a recorded send.
        if matches!(envelope.status.as_str(), "created" | "draft") {
            continue;
        }
        reconcile(
            &state.application_store,
            &state.storage,
            provider,
            notation.id,
            &envelope,
        )
        .await?;
        if let Some((pg, firm, account)) = &account {
            store::project_envelopes::observe(pg, *firm, *account, id, started, &envelope)
                .await
                .map_err(internal)?;
        }
        let envelope = crate::signature_status::project_envelope_json(
            &state.application_store,
            &state.storage,
            Some(&notation),
            &envelope,
        )
        .await
        .map_err(internal)?;
        refreshed.push(serde_json::json!({"notation_id": notation.id, "envelope": envelope}));
    }
    if let Some((pg, firm, account)) = &account {
        let saved = store::project_envelopes::for_project(pg, id)
            .await
            .map_err(internal)?;
        for previous in saved {
            let key = previous.envelope_id.to_string();
            if refreshed
                .iter()
                .any(|entry| entry["envelope"]["envelopeId"].as_str() == Some(&key))
            {
                continue;
            }
            let started = chrono::Utc::now();
            let (envelope, _) = read_project_envelope(&state, id, previous.envelope_id).await?;
            store::project_envelopes::observe(pg, *firm, *account, id, started, &envelope)
                .await
                .map_err(internal)?;
            refreshed.push(serde_json::json!({"notation_id": null, "envelope": envelope}));
        }
    }
    lane.commit().await.map_err(internal)?;
    Ok(Json(serde_json::json!({"envelopes": refreshed})).into_response())
}

async fn adopt_envelope(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<AdoptEnvelopeRequest>,
) -> Result<Response, ApiError> {
    use crate::signature_adoption::{internal, lock, reconcile, validate};

    if !participates_as_firm(&state, &lawyer, id).await {
        return Err(ApiError::NotFound);
    }
    let db = &state.application_store;
    let project = store::projects::find_by_id(db, id)
        .await
        .map_err(internal)?
        .ok_or(ApiError::NotFound)?;
    let participants = store::projects::participations_for_project(db, id)
        .await
        .map_err(internal)?;
    let client = project_client_dri(project.client_dri_person_id, &participants).ok_or(
        ApiError::EnvelopeAdoption(
            "Assign a client DRI to the Project before adopting an envelope",
        ),
    )?;
    let lane = lock().await?;
    let scoped = state
        .signature_provider
        .for_project(db, id)
        .await
        .map_err(ApiError::SignatureUnavailable)?;
    let provider =
        crate::firm_docusign::resolved(scoped.as_ref(), state.signature_provider.as_ref());
    let started = chrono::Utc::now();
    let mut envelope = provider
        .read_external_envelope(input.envelope_id)
        .await
        .map_err(internal)?;
    validate(&envelope)?;
    let mut client_is_signer = false;
    for signer in &envelope.signers {
        if store::persons::find_by_any_email(db, &signer.email)
            .await?
            .is_some_and(|person| person.id == client)
        {
            client_is_signer = true;
            break;
        }
    }
    if !client_is_signer {
        return Err(ApiError::EnvelopeAdoption(
            "The Project's client DRI is not an envelope signer",
        ));
    }

    let envelope_id = input.envelope_id;
    let notation = resolve_adoption_notation(&state, &project, client, input).await?;
    reconcile(db, &state.storage, provider, notation.id, &envelope).await?;
    if matches!(envelope.status.as_str(), "sent" | "delivered" | "signed") {
        // A terminal Connect delivery can arrive before the Signature exists.
        // Check again after registration so completion cannot fall into that gap.
        let latest = provider
            .read_external_envelope(envelope_id)
            .await
            .map_err(internal)?;
        reconcile(db, &state.storage, provider, notation.id, &latest).await?;
        envelope = latest;
    }
    if let (Some(firm), Some(provider)) = (project.firm_id, scoped.as_ref()) {
        let pg = store::postgres::PostgresDb::from_env()
            .await
            .map_err(internal)?;
        store::project_envelopes::observe(
            &pg,
            firm,
            provider.account_id().parse().map_err(internal)?,
            id,
            started,
            &envelope,
        )
        .await
        .map_err(internal)?;
    }
    lane.commit().await.map_err(internal)?;
    if envelope.status == "completed" {
        if let (Some(firm), Some(account)) = (
            project.firm_id,
            scoped
                .as_ref()
                .and_then(|provider| provider.account_id().parse().ok()),
        ) {
            if let Ok(pg) = store::postgres::PostgresDb::application().await {
                if store::docusign_completion::resume_adopted(&pg, firm, account, envelope_id, id)
                    .await
                    .is_err()
                {
                    tracing::warn!(firm_id = %firm, %envelope_id, "DocuSign completion review could not be cleared");
                }
            }
        }
    }

    Ok(Json(serde_json::json!({
        "notation_id": notation.id,
        "envelope": envelope,
    }))
    .into_response())
}

async fn resolve_adoption_notation(
    state: &ApiState,
    project: &store::projects::Project,
    client: Uuid,
    input: AdoptEnvelopeRequest,
) -> Result<store::notations::Notation, ApiError> {
    use crate::signature_adoption::internal;
    use store::signatures::SignatureProvider::DocuSign;

    let db = &state.application_store;
    let id = project.id;
    let envelope_key = input.envelope_id.to_string();
    let linked = store::signatures::by_provider(db, DocuSign, &envelope_key)
        .await
        .map_err(internal)?;
    let linked_notation = if let Some(signature) = linked {
        let notation = store::notations::find_by_id(db, signature.notation_id)
            .await?
            .filter(|notation| notation.project_id == id && notation.person_id == client)
            .ok_or(ApiError::EnvelopeAdoption(
                "This envelope is already linked to another Notation",
            ))?;
        if crate::signature_adoption::evidence(state.storage.as_ref(), notation.id)
            .await?
            .is_none_or(|evidence| evidence.envelope_id != input.envelope_id)
        {
            return Err(ApiError::EnvelopeAdoption(
                "This envelope is already linked to another Notation",
            ));
        }
        Some(notation)
    } else {
        None
    };

    let draft = store::template_source::persist_contents(
        db,
        &state.storage,
        id,
        &input.template_code,
        input.source,
        input.commit_sha,
    )
    .await
    .map_err(|_| ApiError::EnvelopeAdoption("The Project Draft could not be saved"))?;
    if let Some(notation) = linked_notation {
        if notation.template_id != draft.template.id {
            return Err(ApiError::EnvelopeAdoption(
                "This envelope is already linked to another Draft version",
            ));
        }
        return Ok(notation);
    }
    if let Some(existing) =
        store::notations::find_by_project_template_person(db, id, draft.template.id, client).await?
    {
        if store::signatures::latest_for_notation(db, existing.id)
            .await
            .map_err(internal)?
            .is_some()
        {
            return Err(ApiError::EnvelopeAdoption(
                "This Notation already has a Signature",
            ));
        }
        return Ok(existing);
    }
    let new = store::notations::NewNotation::new(draft.template.id, client, id, "END")
        .with_entity(project.entity_id);
    Ok(store::notations::create(db, &new).await?)
}

/// Request body for `POST /app/api/notations/{id}/answers` — answer the
/// questionnaire's current step. `question_code` must match the step the
/// questionnaire is currently asking (it advances one step at a time);
/// `reference_id` names the picked row for a record/reference question and is
/// omitted for a free-typed answer.
#[derive(Deserialize)]
struct AnswerStepRequest {
    question_code: String,
    value: String,
    #[serde(default)]
    reference_id: Option<Uuid>,
}

/// One question the caller must collect next.
#[derive(Serialize)]
struct NotationQuestion {
    id: Uuid,
    code: String,
    prompt: String,
    answer_type: String,
    choices: Vec<workflows::QuestionChoice>,
    /// The attorney-authored guidance for this question; omitted when the
    /// question declares none.
    #[serde(skip_serializing_if = "Option::is_none")]
    help_text: Option<String>,
}

/// Success body for `POST /app/api/notations/{id}/answers` — where the
/// questionnaire is after recording the answer: either the next question to
/// collect, or that it is complete.
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum NotationStepResponse {
    /// Collect this question and POST it back to the same endpoint.
    NeedsAnswer { question: NotationQuestion },
    /// The questionnaire reached END; there is nothing more to answer.
    Complete,
}

impl From<workflows::NextStep> for NotationStepResponse {
    fn from(next: workflows::NextStep) -> Self {
        match next {
            workflows::NextStep::NeedsAnswer { question } => Self::NeedsAnswer {
                question: NotationQuestion {
                    id: question.id,
                    code: question.code,
                    prompt: question.prompt,
                    answer_type: question.answer_type,
                    choices: question.choices,
                    help_text: question.help_text,
                },
            },
            workflows::NextStep::QuestionnaireComplete => Self::Complete,
        }
    }
}

/// `POST /app/api/notations/{id}/answers` — record an answer to the notation's
/// current questionnaire step, attributed to the acting lawyer (source =
/// lawyer; the notation's bound Person stays the respondent). Lawyer-tier only
/// (embedded Rego policy) *and* matter-scoped: the acting lawyer must participate in the
/// notation's matter (admin bypasses), collapsing an out-of-scope notation to
/// 404 so the door never discloses it. The client-facing self-serve intake
/// (the magic-link walk) is a separate surface, not this REST command
/// boundary. Returns the next step (a question to collect, or complete).
async fn answer_notation_step(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<AnswerStepRequest>,
) -> Result<Response, ApiError> {
    // Resolve the notation and enforce matter scope on its project before
    // touching the questionnaire. A miss (no notation, or out of scope)
    // collapses to 404 so the door never discloses a notation the caller
    // cannot see.
    let notation = store::notations::find_by_id(&state.application_store, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        notation.project_id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }

    let next = workflows::answer_step_with_reference(
        &state.application_store,
        state.questionnaire_runtime.as_ref(),
        Some(&state.storage),
        id,
        input.question_code.trim(),
        &input.value,
        input.reference_id,
        workflows::AnswerAuthor::lawyer(lawyer.0.person_id),
    )
    .await?;
    // The questionnaire reaching END is what begins the notation's workflow,
    // whichever door recorded the last answer. This one used to return
    // `complete` and stop, leaving the notation parked at a machine nobody
    // had started while the same answer through the lawyer's form walk
    // reached `lawyer_review`.
    if matches!(next, workflows::NextStep::QuestionnaireComplete) {
        let deps = crate::retainer_walk::RenderDeps {
            application_store: &state.application_store,
            runtime: state.workflow_runtime.as_ref(),
            storage: &state.storage,
            assets_storage: &state.assets_storage,
            forms_registry: &state.forms_registry,
        };
        crate::retainer_walk::begin_post_questionnaire_workflow(&deps, id, lawyer.0.person_id)
            .await?;
    }
    Ok((StatusCode::OK, Json(NotationStepResponse::from(next))).into_response())
}

/// Load a notation and confirm the caller participates on its matter from the
/// firm side, or 404.
///
/// The content-read sibling of [`notation_in_lawyer_scope`]. That one keeps the
/// Owner/Admin bypass the write and workflow doors were built on, which ENG-83
/// owns collapsing; this one goes through [`participates_as_firm`], so a
/// privileged tier with no row on the matter is refused exactly like an
/// unassigned lawyer. Use this one whenever the door returns the matter's work
/// product rather than driving its workflow.
async fn notation_in_firm_participation(
    state: &ApiState,
    lawyer: &LawyerSession,
    notation_id: Uuid,
) -> Result<store::notations::Notation, ApiError> {
    let notation = store::notations::find_by_id(&state.application_store, notation_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !participates_as_firm(state, lawyer, notation.project_id).await {
        return Err(ApiError::NotFound);
    }
    Ok(notation)
}

/// Load a notation and confirm the lawyer caller may see its matter, or 404.
/// The notation-door mirror of [`participation_on_matter`]: an out-of-scope or
/// unknown notation collapses to 404 so a door never discloses one the caller
/// cannot reach (admin bypasses the scope check).
async fn notation_in_lawyer_scope(
    state: &ApiState,
    lawyer: &LawyerSession,
    notation_id: Uuid,
) -> Result<store::notations::Notation, ApiError> {
    let notation = store::notations::find_by_id(&state.application_store, notation_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        notation.project_id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    Ok(notation)
}

/// Request body for `POST /app/api/notations/{id}/request-changes` — the flagged
/// question codes to send back and an optional note.
#[derive(Deserialize)]
struct RequestNotationChangesRequest {
    #[serde(default)]
    flagged: Vec<String>,
    #[serde(default)]
    note: Option<String>,
}

/// `POST /app/api/notations/{id}/request-changes` — send a notation at
/// `lawyer_review` back to its client for changes, the REST mirror of the lawyer
/// request-changes control. Both converge on
/// `retainer_walk::request_notation_changes`. Lawyer-tier only and matter-scoped
/// (out-of-scope → 404, admin bypasses). `204` on success; not at review is
/// `409`; an empty or off-route flagged set is `400`.
async fn request_notation_changes_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<RequestNotationChangesRequest>,
) -> Result<Response, ApiError> {
    let acting = lawyer.0.person_id.ok_or(ApiError::Forbidden)?;
    notation_in_lawyer_scope(&state, &lawyer, id).await?;
    let note = input
        .note
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match crate::retainer_walk::request_notation_changes(
        &state.application_store,
        state.workflow_runtime.as_ref(),
        Some(&state.storage),
        id,
        Some(acting),
        &input.flagged,
        note,
    )
    .await
    {
        Ok(()) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(crate::retainer_walk::RequestChangesError::NotationNotFound) => Err(ApiError::NotFound),
        Err(crate::retainer_walk::RequestChangesError::NotInReview) => Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "not_in_review",
                "message": "This notation is not awaiting review; there is nothing to send back."
            })),
        )
            .into_response()),
        Err(crate::retainer_walk::RequestChangesError::QuestionnaireAuthorityMissing) => {
            Ok(bad_request(
                "questionnaire_authority_missing",
                "The completed questionnaire definition is unavailable; begin a fresh intake.",
            ))
        }
        Err(crate::retainer_walk::RequestChangesError::OffRouteQuestion) => Ok(bad_request(
            "question_not_on_route",
            "Flag only answers on the completed questionnaire route.",
        )),
        Err(crate::retainer_walk::RequestChangesError::NothingFlagged) => Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "nothing_flagged",
                "message": "Flag at least one answer to send back for changes."
            })),
        )
            .into_response()),
        Err(e) => {
            tracing::error!(error = %e, notation_id = %id, "api request_changes: failed");
            Err(ApiError::Db(
                "the change request could not be recorded".into(),
            ))
        }
    }
}

/// Request body for `POST /app/api/notations/{id}/reask` — the re-collected
/// answers, keyed by bare question code.
#[derive(Deserialize)]
struct ReaskResubmitRequest {
    #[serde(default)]
    answers: std::collections::BTreeMap<String, String>,
}

/// `POST /app/api/notations/{id}/reask` — resubmit a re-collected notation for
/// review, the REST mirror of the lawyer reask control. Both converge on
/// `retainer_walk::resubmit_reask`. Lawyer-tier only and matter-scoped
/// (out-of-scope → 404, admin bypasses). `204` on success; not awaiting
/// re-collection is `409`, a flagged answer left blank is `400`.
async fn resubmit_reask_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<ReaskResubmitRequest>,
) -> Result<Response, ApiError> {
    let acting = lawyer.0.person_id.ok_or(ApiError::Forbidden)?;
    notation_in_lawyer_scope(&state, &lawyer, id).await?;
    match crate::retainer_walk::resubmit_reask(
        &state.application_store,
        state.workflow_runtime.as_ref(),
        Some(&state.storage),
        id,
        Some(acting),
        &input.answers,
    )
    .await
    {
        Ok(()) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(crate::retainer_walk::ResubmitReaskError::NotationNotFound) => Err(ApiError::NotFound),
        Err(crate::retainer_walk::ResubmitReaskError::NotAwaitingReask) => Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "not_awaiting_reask",
                "message": "This notation is not awaiting re-collection."
            })),
        )
            .into_response()),
        Err(crate::retainer_walk::ResubmitReaskError::QuestionnaireAuthorityMissing) => Ok(bad_request(
            "questionnaire_authority_missing",
            "The completed questionnaire definition is unavailable; begin a fresh intake.",
        )),
        Err(crate::retainer_walk::ResubmitReaskError::OffRouteQuestion) => Ok(bad_request(
            "question_not_on_route",
            "Re-collect only questions on the completed questionnaire route.",
        )),
        Err(crate::retainer_walk::ResubmitReaskError::QuestionnaireRouteChanged) => Ok(bad_request(
            "questionnaire_route_changed",
            "This correction changes the questionnaire route; begin a fresh intake.",
        )),
        Err(crate::retainer_walk::ResubmitReaskError::MissingAnswer(code)) => Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "missing_answer",
                "message":
                    format!("Re-collect the flagged answer `{code}` before resubmitting for review.")
            })),
        )
            .into_response()),
        Err(crate::retainer_walk::ResubmitReaskError::AnswerWriteFailed(_)) => Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "answer_write_failed",
                "message": "A re-collected answer could not be saved; the resubmit was refused."
            })),
        )
            .into_response()),
        Err(e) => {
            tracing::error!(error = %e, notation_id = %id, "api reask: failed");
            Err(ApiError::Db("the re-collection could not be resubmitted".into()))
        }
    }
}

/// Request body for `POST /app/api/notations/{id}/transcript` — a batch
/// transcript to run against the notation's questionnaire.
#[derive(Deserialize)]
struct TranscriptCoverageRequest {
    #[serde(default)]
    transcript: String,
}

/// `POST /app/api/notations/{id}/transcript` — run a transcript against the
/// notation's bound questionnaire, recording every likely-answered inquiry as a
/// proposed default; the REST mirror of the lawyer/CLI transcript control,
/// converging on `retainer_walk::record_transcript_coverage`. Lawyer-tier only
/// and matter-scoped (out-of-scope → 404). `200` with `{template_code, covered,
/// uncovered}`; an empty transcript is `400`; a template with no questionnaire
/// is `422`.
async fn transcript_coverage_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<TranscriptCoverageRequest>,
) -> Result<Response, ApiError> {
    notation_in_lawyer_scope(&state, &lawyer, id).await?;
    let transcript = input.transcript.trim();
    if transcript.is_empty() {
        return Ok(bad_request(
            "transcript_required",
            "A transcript is required.",
        ));
    }
    match crate::retainer_walk::record_transcript_coverage(
        &state.application_store,
        &state.storage,
        id,
        transcript,
    )
    .await
    {
        Ok(c) => Ok((
            StatusCode::OK,
            Json(serde_json::json!({
                "template_code": c.template_code,
                "covered": c.covered,
                "uncovered": c.uncovered,
            })),
        )
            .into_response()),
        Err(
            crate::retainer_walk::TranscriptCoverageError::NotationNotFound
            | crate::retainer_walk::TranscriptCoverageError::TemplateNotFound,
        ) => Err(ApiError::NotFound),
        Err(crate::retainer_walk::TranscriptCoverageError::NoQuestionnaire) => Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": "no_questionnaire",
                "message": "This notation's template has no questionnaire to cover."
            })),
        )
            .into_response()),
        Err(e) => {
            tracing::error!(error = %e, notation_id = %id, "api transcript coverage: failed");
            Err(ApiError::Db("the transcript coverage pass failed".into()))
        }
    }
}

/// Success body for `POST /app/api/notations/{id}/intake` — the notation and the
/// client address the intake link was dispatched to.
#[derive(Serialize)]
struct SendIntakeResponse {
    notation_id: Uuid,
    recipient: String,
}

/// `POST /app/api/notations/{id}/intake` — email the notation's client their
/// self-serve intake magic link. Lawyer-tier only (embedded Rego policy) *and* matter-scoped:
/// the acting lawyer must participate in the notation's matter (admin bypasses),
/// collapsing an out-of-scope or unknown notation to 404. This is the same
/// command the lawyer form drives. Email delivery is best-effort (a send failure
/// is logged, not surfaced — the link is idempotent), so `200 OK` reports that
/// the link was dispatched to the returned recipient.
async fn send_notation_intake(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    // Resolve the notation and enforce matter scope on its project before
    // dispatching anything. A miss collapses to 404 so the door never
    // discloses a notation the caller cannot see.
    let notation = store::notations::find_by_id(&state.application_store, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        notation.project_id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }

    let recipient =
        crate::retainer_walk::send_intake(&state.application_store, state.email.as_ref(), id)
            .await?;
    Ok((
        StatusCode::OK,
        Json(SendIntakeResponse {
            notation_id: id,
            recipient,
        }),
    )
        .into_response())
}

/// Success body for a notation lifecycle transition — the notation and its
/// workflow state after the action.
#[derive(Serialize)]
struct NotationLifecycleResponse {
    notation_id: Uuid,
    state: String,
}

/// `POST /app/api/notations/{id}/approval` — the attorney approves a notation
/// parked at `lawyer_review`: re-assembles the reviewed document and fires the
/// `approved` transition so the worker renders + persists its PDF, parking at
/// `generate_pdf__*` (the deliberate signature dispatch is a separate door).
/// Lawyer-tier only (embedded Rego policy) *and* matter-scoped (admin bypasses); an out-of-scope
/// or unknown notation is 404. A repeated approval reports the authoritative
/// workflow state and reconciles a stale Lawyer Review projection.
/// This drives the same command core (`render_and_park`) as the lawyer form.
async fn approve_notation(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    // Resolve the notation and enforce matter scope on its project first. A
    // miss collapses to 404 so the door never discloses a notation the caller
    // cannot see.
    let notation = store::notations::find_by_id(&state.application_store, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        notation.project_id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }

    let deps = crate::retainer_walk::RenderDeps {
        application_store: &state.application_store,
        runtime: state.workflow_runtime.as_ref(),
        storage: &state.storage,
        assets_storage: &state.assets_storage,
        forms_registry: &state.forms_registry,
    };
    let final_state = crate::retainer_walk::render_and_park(&deps, id, lawyer.0.person_id).await?;
    Ok((
        if final_state.prefix() == "lawyer_review" {
            StatusCode::ACCEPTED
        } else {
            StatusCode::OK
        },
        Json(NotationLifecycleResponse {
            notation_id: id,
            state: final_state.as_str().to_string(),
        }),
    )
        .into_response())
}

/// Success body for `POST /app/api/notations/{id}/signature` — the notation, its
/// workflow state after dispatch, and the provider's signature request id (so
/// the caller can correlate the inbound completion webhook).
#[derive(Serialize)]
struct NotationSignatureResponse {
    notation_id: Uuid,
    state: String,
    signature_request_id: String,
}

/// Success body for `GET /app/api/notations/{id}/signature` — the read-only
/// envelope status ([`crate::signature_status`]): the provider, the
/// envelope-wide lifecycle state, and each declared signer's own step. A
/// notation that has never been prepared or sent reports nulls and an
/// empty signer list — "no envelope yet" is an answer, not a 404.
#[derive(Serialize)]
struct NotationSignatureStatusResponse {
    notation_id: Uuid,
    #[serde(flatten)]
    report: crate::signature_status::EnvelopeStatusReport,
}

/// Request body for `POST /app/api/notations/{id}/signature`. An empty body
/// (or an omitted `prepare`) sends the envelope; `{"prepare": true}` holds
/// it as a draft on the provider — the CLI's `notation envelope prepare`.
#[derive(Deserialize, Default)]
struct NotationSignatureRequest {
    #[serde(default)]
    prepare: bool,
}

/// Resolve the notation and enforce matter scope on its project — the
/// shared front door of both signature REST handlers. A miss collapses to
/// 404 so the door never discloses a notation the caller cannot see.
async fn lawyer_scoped_notation(
    state: &ApiState,
    lawyer: &LawyerSession,
    id: Uuid,
) -> Result<store::notations::Notation, ApiError> {
    let notation = store::notations::find_by_id(&state.application_store, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        notation.project_id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    Ok(notation)
}

/// `POST /app/api/notations/{id}/signature` — dispatch the notation's rendered
/// document for signature: with `{"prepare": true}` the envelope is created as
/// a draft (the workflow does not advance); otherwise it fires `pdf_persisted`
/// and sends exactly one envelope through the signature provider. Lawyer-tier
/// only (embedded Rego policy) *and* matter-scoped (admin bypasses); an
/// out-of-scope or unknown notation is 404. Idempotent: a notation that
/// already has a dispatched envelope reuses its request id and sends nothing.
/// When the worker has not rendered the PDF yet, returns `409
/// document_not_ready` (retry) rather than dispatching a missing document.
/// This drives the same command core (`send_notation_for_signature`) as the lawyer form.
async fn send_notation_signature(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    // Existence + matter scope; the dispatch itself reads nothing else off
    // the notation.
    lawyer_scoped_notation(&state, &lawyer, id).await?;

    let ctx = crate::signature_flow::SendFlowContext {
        application_store: &state.application_store,
        runtime: state.workflow_runtime.as_ref(),
        storage: &state.storage,
        acting: lawyer.0.person_id,
    };
    let mode = {
        // An empty body (or an omitted `prepare`) dispatches — today's
        // behavior; `{"prepare": true}` holds a draft. A non-empty body that
        // does not parse is the caller's mistake, never a silent send.
        let prepare = if body.is_empty() {
            false
        } else {
            serde_json::from_slice::<NotationSignatureRequest>(&body)
                .map_err(|error| ApiError::MalformedBody(error.to_string()))?
                .prepare
        };
        if prepare {
            crate::signature_flow::SendMode::Prepare
        } else {
            crate::signature_flow::SendMode::Send
        }
    };
    let (final_state, signature_request_id) = crate::signature_flow::send_notation_for_signature(
        &ctx,
        id,
        state.signature_provider.as_ref(),
        mode,
    )
    .await?;
    Ok((
        StatusCode::OK,
        Json(NotationSignatureResponse {
            notation_id: id,
            state: final_state.as_str().to_string(),
            signature_request_id: signature_request_id.0,
        }),
    )
        .into_response())
}

/// `GET /app/api/notations/{id}/signature` — read the notation's envelope
/// status ([`crate::signature_status`]): the provider, the envelope-wide
/// lifecycle state, and each declared signer's own step. Same gates as the
/// dispatch `POST` (lawyer tier + matter scope, out-of-scope is 404), but
/// read-only: a notation with no envelope answers 200 with nulls rather
/// than a 404 — "nothing sent yet" is exactly what a lawyer preparing one
/// needs to see. Composes the same durable record the webhook stamps
/// (`store::signatures::latest_for_notation`), so per-signer progress here
/// and on the matter page come from one source.
async fn status_notation_signature(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let notation = lawyer_scoped_notation(&state, &lawyer, id).await?;
    let mut report = crate::signature_status::status_for_notation(
        &state.application_store,
        &state.storage,
        &notation,
    )
    .await;
    crate::signature_status::refresh_envelope_status(
        &mut report,
        &state.application_store,
        notation.project_id,
        state.signature_provider.as_ref(),
    )
    .await;
    Ok((
        StatusCode::OK,
        Json(NotationSignatureStatusResponse {
            notation_id: id,
            report,
        }),
    )
        .into_response())
}

/// Request body for `POST /app/api/review-documents/{id}/comments` — one anchored
/// comment on a review document. The anchor is a ProseMirror position range
/// plus the text it covered, captured client-side from the read-only document.
#[derive(Deserialize)]
struct AddReviewCommentRequest {
    anchor_start: i32,
    anchor_end: i32,
    quoted_text: String,
    body: String,
}

/// Success body for `POST /app/api/review-documents/{id}/comments` — the created
/// comment and its conversation-log spine row.
#[derive(Serialize)]
struct AddReviewCommentResponse {
    comment_id: Uuid,
    communication_id: Uuid,
}

/// `POST /app/api/review-documents/{id}/comments` — add one anchored comment to a
/// review document, folding it into the matter's conversation log. This is the
/// first **client-writable** `/app/api` door: any authenticated caller reaches it
/// (embedded Rego policy), but the shared command (`crate::review::create_review_comment`)
/// enforces **client-lens** matter scope — the caller must participate in the
/// document's matter through the client side, the same gate the read-only
/// review surface uses — so a firm-side-only lawyer or a non-participant gets a
/// bare 404. `direction` is derived from the caller's role. `201` on success.
async fn add_review_comment(
    State(state): State<ApiState>,
    authed: AuthedSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<AddReviewCommentRequest>,
) -> Result<Response, ApiError> {
    let created = crate::review::create_review_comment(
        &state.application_store,
        authed.0.role,
        authed.0.person_id,
        id,
        None,
        input.anchor_start,
        input.anchor_end,
        &input.quoted_text,
        &input.body,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(AddReviewCommentResponse {
            comment_id: created.comment_id,
            communication_id: created.communication_id,
        }),
    )
        .into_response())
}

/// Success body for `POST /app/api/documents/{id}/deletion-requests` — the pending
/// expunge request, and whether it already existed (idempotent re-ask).
#[derive(Serialize)]
struct DeletionRequestResponse {
    request_id: Uuid,
    already_pending: bool,
}

/// `POST /app/api/documents/{id}/deletion-requests` — a matter participant asks for
/// a document to be deleted (a lawyer/admin must later authorize the actual
/// expunge; this only records the request). Client-writable like the review
/// surface: any authenticated caller reaches it (embedded Rego policy), but the shared command
/// (`crate::expunge_request_route::request_document_deletion`) enforces
/// **client-lens** matter scope — a firm-side-only lawyer or a non-participant
/// gets a bare 404. Idempotent: a second ask while one is pending returns the
/// existing request (`200`); a fresh request is `201`.
async fn create_deletion_request(
    State(state): State<ApiState>,
    authed: AuthedSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let outcome = crate::expunge_request_route::request_document_deletion(
        &state.application_store,
        &authed.0,
        id,
        None,
    )
    .await?;
    let status = if outcome.already_pending {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((
        status,
        Json(DeletionRequestResponse {
            request_id: outcome.request_id,
            already_pending: outcome.already_pending,
        }),
    )
        .into_response())
}

/// Request body for `POST /app/api/notations/{id}/clauses` — one custom clause to
/// append to a notation's assembled document.
#[derive(Deserialize)]
struct AddClauseRequest {
    body: String,
}

/// Success body for `POST /app/api/notations/{id}/clauses` — the appended clause.
#[derive(Serialize)]
struct AddClauseResponse {
    clause_id: Uuid,
}

/// `POST /app/api/notations/{id}/clauses` — append a custom clause to a notation's
/// document (spliced into the assembled body at render time). Lawyer-tier only
/// (embedded Rego policy) *and* matter-scoped: the acting lawyer must participate in the
/// notation's matter (admin bypasses), collapsing an out-of-scope or unknown
/// notation to 404. Appends through `store::notation_clauses::append` — the
/// same store command the lawyer clause form drives. `201` with the clause id.
async fn add_clause(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<AddClauseRequest>,
) -> Result<Response, ApiError> {
    let body = input.body.trim();
    if body.is_empty() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_request",
                "message": "A clause body is required."
            })),
        )
            .into_response());
    }

    // Resolve the notation and enforce matter scope before appending.
    let notation = store::notations::find_by_id(&state.application_store, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        notation.project_id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }

    let clause_id =
        store::notation_clauses::append(&state.application_store, id, body, lawyer.0.person_id)
            .await
            .map_err(|e| ApiError::Db(e.to_string()))?;
    Ok((StatusCode::CREATED, Json(AddClauseResponse { clause_id })).into_response())
}

/// Resolve a clause under a notation and enforce lawyer matter scope. Returns
/// 404 (never disclosing) when the notation is missing or out of the acting
/// lawyer's scope, or the clause does not belong to that notation.
async fn clause_in_scope(
    state: &ApiState,
    lawyer: &SessionData,
    notation_id: Uuid,
    clause_id: Uuid,
) -> Result<(), ApiError> {
    let notation = store::notations::find_by_id(&state.application_store, notation_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.person_id,
        lawyer.role,
        notation.project_id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    let clause = store::notation_clauses::find_by_id(&state.application_store, clause_id)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?
        .ok_or(ApiError::NotFound)?;
    if clause.notation_id != notation_id {
        return Err(ApiError::NotFound);
    }
    Ok(())
}

/// `PATCH /app/api/notations/{id}/clauses/{clause_id}` — replace a clause's body.
/// Lawyer-tier only (embedded Rego policy) *and* matter-scoped; a blank body is 400, and a clause
/// on another notation (or an out-of-scope notation) is 404. Drives the same
/// store command as the lawyer clause form.
async fn edit_clause(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path((id, clause_id)): Path<(Uuid, Uuid)>,
    JsonOrForm(input): JsonOrForm<AddClauseRequest>,
) -> Result<Response, ApiError> {
    let body = input.body.trim();
    if body.is_empty() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_request",
                "message": "A clause body is required."
            })),
        )
            .into_response());
    }
    clause_in_scope(&state, &lawyer.0, id, clause_id).await?;
    store::notation_clauses::update_body(&state.application_store, clause_id, body)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?;
    Ok((StatusCode::OK, Json(AddClauseResponse { clause_id })).into_response())
}

/// `DELETE /app/api/notations/{id}/clauses/{clause_id}` — remove a clause.
/// Lawyer-tier only (embedded Rego policy) *and* matter-scoped. `204 No Content` on success.
async fn delete_clause(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path((id, clause_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    clause_in_scope(&state, &lawyer.0, id, clause_id).await?;
    store::notation_clauses::delete(&state.application_store, clause_id)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Request body for `POST /app/api/notations/{id}/clauses/{clause_id}/move`.
#[derive(Deserialize)]
struct MoveClauseRequest {
    /// `up` moves the clause one step earlier in render order; anything else
    /// (e.g. `down`) moves it later. A move at the ends is a no-op.
    direction: String,
}

/// `POST /app/api/notations/{id}/clauses/{clause_id}/move` — reorder a clause by
/// swapping it with its neighbour. Lawyer-tier only (embedded Rego policy) *and* matter-scoped.
/// `204 No Content` on success (a move at the ends is an idempotent no-op).
async fn move_clause(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path((id, clause_id)): Path<(Uuid, Uuid)>,
    JsonOrForm(input): JsonOrForm<MoveClauseRequest>,
) -> Result<Response, ApiError> {
    clause_in_scope(&state, &lawyer.0, id, clause_id).await?;
    let up = input.direction == "up";
    store::notation_clauses::move_clause(&state.application_store, clause_id, up)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Success body for `POST /app/api/projects/{id}/contract-review` — the created
/// `contract_reviews` row (its deviation analysis is attached and the matter
/// is parked at `lawyer_review`).
#[derive(Serialize)]
struct ContractReviewResponse {
    review_id: Uuid,
}

/// `POST /app/api/projects/{id}/contract-review` — upload an inbound third-party
/// contract for playbook review (`multipart/form-data`: a `file` part, or a
/// `text` part with the pasted contract). Opens a `memo__contract_review`
/// notation, files the contract, runs the deviation analysis against the
/// client Entity's playbook, and lands the matter at `lawyer_review`.
///
/// Client-writable like the review surface (a matter's client can submit their
/// own contract, or the firm can): any authenticated caller reaches it (embedded Rego policy),
/// but the door then enforces matter scope through *either* lens (lawyer or
/// client), so a non-participant gets a bare 404. If the client Entity has no
/// playbook yet, returns `422`. Drives the same command
/// (`crate::contract_review_walk::drive_contract_review`) as the lawyer/portal
/// upload form. `201` with the new review id.
#[allow(clippy::too_many_lines)]
async fn upload_contract_review(
    State(state): State<ApiState>,
    authed: AuthedSession,
    Path(id): Path<Uuid>,
    multipart: Multipart,
) -> Result<Response, ApiError> {
    // A contract must be attributable to a person.
    let Some(person_id) = authed.0.person_id else {
        return Err(ApiError::Forbidden);
    };
    // Matter scope through either lens — the firm reviews its client's
    // contract, and the client may submit one. A non-participant sees 404.
    let lawyer_ok = store::access::can_see_project_as_lawyer(
        &state.application_store,
        Some(person_id),
        authed.0.role,
        id,
    )
    .await
    .unwrap_or(false);
    let client_ok =
        store::access::can_see_project_as_client(&state.application_store, Some(person_id), id)
            .await
            .unwrap_or(false);
    if !lawyer_ok && !client_ok {
        return Err(ApiError::NotFound);
    }

    let bad_request = |message: &str| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid_request", "message": message })),
        )
            .into_response()
    };
    let Some(form) = crate::contract_review_walk::parse_form(multipart).await else {
        return Ok(bad_request("malformed multipart body"));
    };
    let Some(artifact) = form.into_artifact() else {
        return Ok(bad_request(
            "a `file` part or a non-empty `text` part is required",
        ));
    };
    let contract_text = artifact.contract_text();
    let filename = artifact.default_filename();

    let deps = crate::contract_review_walk::ReviewDeps {
        application_store: &state.application_store,
        workflow_runtime: state.workflow_runtime.as_ref(),
        storage: &state.storage,
        contract_reviewer: state.contract_reviewer.as_ref(),
    };
    let review_id = crate::contract_review_walk::drive_contract_review(
        &deps,
        id,
        person_id,
        &filename,
        &contract_text,
        artifact.into_workflow_artifact(),
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(ContractReviewResponse { review_id }),
    )
        .into_response())
}

async fn list_entities(
    State(application_store): State<store::application_store::ApplicationStore>,
) -> Result<Json<Vec<store::entities::Entity>>, ApiError> {
    let rows = store::entities::all(&application_store).await?;
    Ok(Json(rows))
}

/// `GET /app/api/firms` — list Firms the caller may administer.
///
/// Owner can resolve any Firm name; Admin can resolve only Firms where their
/// membership admits `ManageMembership`. The CLI needs this directory to turn
/// a Firm name into the internal id accepted by Project creation and update,
/// and to discover the brand keys that Firm wears.
async fn list_firms(
    State(application_store): State<store::application_store::ApplicationStore>,
    AuthedSession(session): AuthedSession,
) -> Result<Json<Vec<FirmDirectoryEntry>>, ApiError> {
    if !session.role.is_admin_tier() {
        return Err(ApiError::Forbidden);
    }
    let visible = store::firm_capability::allowed_firm_ids(
        &application_store,
        session.role,
        session.person_id,
        store::firm_capability::FirmCapability::ManageMembership,
    )
    .await
    .map_err(|error| ApiError::Db(error.to_string()))?;
    let firms = store::firms::all(&application_store)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    let mut entries = Vec::new();
    for firm in firms.into_iter().filter(|firm| visible.contains(&firm.id)) {
        let brands = store::firms::brand_keys_for_firm(&application_store, firm.id)
            .await
            .map_err(|error| ApiError::Db(error.to_string()))?;
        entries.push(FirmDirectoryEntry {
            id: firm.id,
            name: firm.name,
            slug: firm.slug,
            brands,
        });
    }
    Ok(Json(entries))
}

#[derive(Debug, Serialize)]
struct FirmDirectoryEntry {
    brands: Vec<String>,
    id: Uuid,
    name: String,
    /// The Firm's immutable slug, which names its Forgejo organization.
    slug: String,
}

async fn get_entity(
    State(application_store): State<store::application_store::ApplicationStore>,
    Path(id): Path<Uuid>,
) -> Result<Json<store::entities::Entity>, ApiError> {
    store::entities::find_by_id(&application_store, id)
        .await?
        .map(Json)
        .ok_or(ApiError::NotFound)
}

async fn list_jurisdictions(
    State(application_store): State<store::application_store::ApplicationStore>,
) -> Result<Json<Vec<store::jurisdictions::Jurisdiction>>, ApiError> {
    let rows = store::jurisdictions::list_all(&application_store).await?;
    Ok(Json(rows))
}

async fn list_entity_types(
    State(application_store): State<store::application_store::ApplicationStore>,
) -> Result<Json<Vec<store::entity_types::EntityType>>, ApiError> {
    let rows = store::entity_types::list(&application_store, &[]).await?;
    Ok(Json(rows))
}

/// Request body for `POST /app/api/templates/validate`. The caller hands
/// over markdown they're drafting; we lint it and return violations
/// without touching the database.
#[derive(Debug, Deserialize)]
pub struct ValidateRequest {
    /// Raw markdown body, including any YAML frontmatter.
    pub contents: String,
    /// Optional pretend filename so rules that key off the path
    /// (`N103` snake_case) and the response have something meaningful
    /// to report. Defaults to `template.md` — a snake_case placeholder
    /// so the default doesn't pollute the response with a filename
    /// complaint the caller never intended.
    #[serde(default)]
    pub path: Option<String>,
    /// When true, lint with the Markdown-only rule set (drops the
    /// N-family, adds `S102` line packing) — use this for plain prose.
    /// Defaults to false: the full Neon Law Navigator notation rule set
    /// runs.
    #[serde(default)]
    pub markdown_only: bool,
}

#[derive(Debug, Serialize)]
pub struct ValidateResponse {
    pub path: String,
    pub clean: bool,
    pub violations: Vec<ValidationViolation>,
}

#[derive(Debug, Serialize)]
pub struct ValidationViolation {
    pub code: &'static str,
    pub line: usize,
    pub message: String,
}

/// Request body for `POST /app/api/projects/{id}/documents` — a single document
/// to file into the matter. The browser uploads via multipart; the REST door
/// takes the bytes base64-encoded so a programmatic caller needs no form.
#[derive(Deserialize)]
struct UploadDocumentRequest {
    filename: String,
    /// Stable identity below a Project repository's `documents/` directory.
    /// Defaults to `filename` for the one-file upload command.
    slug: Option<String>,
    /// Base64-encoded file bytes.
    content_base64: String,
    content_type: Option<String>,
    /// Required asset-lane classification. Omitted or blank is `400
    /// kind_required`; a value outside the lane is `400 invalid_kind`.
    kind: Option<String>,
    /// `"client"` makes the document client-visible; anything else (the default)
    /// files it as internal work product.
    visibility: Option<String>,
    description: Option<String>,
    /// Free-form provenance the uploading caller already knows and the
    /// bytes alone cannot say — a source repository's commit SHA, for
    /// instance (ENG-481). Never inferred here; passed through verbatim
    /// to the asset row's own `metadata` column.
    metadata: Option<serde_json::Value>,
    /// Exact source revision for a generated transcript.
    derived_from: Option<store::documents::DerivedFrom>,
    /// Quality for a transcript revision; absent on unrelated upload lanes.
    transcript_quality: Option<String>,
    /// How an executed document was signed: `{ method, signed_on }`.
    /// Optional on every kind; a malformed block is rejected before any
    /// byte is stored.
    #[serde(default)]
    signing: Option<store::documents::Signing>,
    envelope_id: Option<Uuid>,
    /// The notarial acknowledgments on an executed document. Optional; each
    /// entry is validated before any byte is stored.
    #[serde(default)]
    notarization: Vec<store::documents::Notarization>,
    /// The exhibit metadata the matter's evidence index reads (LAW-185).
    /// Optional on every kind; validated before any byte is stored.
    #[serde(default)]
    evidence: Option<store::documents::Evidence>,
    /// The matter folder the document is filed in (LAW-241), spelled
    /// exactly as the Firm's matter folders spell it (`Client Production`,
    /// `Work Product`, ...). Optional; an unknown value is `400
    /// invalid_folder` and writes nothing — never a silent default.
    #[serde(default)]
    folder: Option<String>,
    /// The chain head the caller read before uploading (LAW-284). Present
    /// turns the append conditional: a chain that moved past that head is a
    /// `409 head_conflict` naming the live head, never a silently forked
    /// chain. Absent — an old CLI — files exactly as before, no CAS.
    #[serde(default)]
    expected_head: Option<store::assets::ExpectedHead>,
    /// Mail-lane provenance the CLI's `mail parse` stamps when it files a
    /// `.eml` source document or one of its attachments through this door:
    /// the message identity the raw `.eml` door's parse derives on its own
    /// variant. Optional; an incomplete block is `400
    /// invalid_mail_provenance` and writes nothing.
    #[serde(default)]
    mail_provenance: Option<UploadMailProvenance>,
}

/// The mail provenance a document-lane mail filing stamps onto the asset row,
/// spelled on the wire as the strings the ingest stores (`source_message_id`,
/// `source_sender`, `source_received_at`, `source_subject`).
#[derive(Deserialize)]
struct UploadMailProvenance {
    message_id: String,
    sender: String,
    received_at: String,
    subject: Option<String>,
    /// A named attachment has its own identity inside the message's directory.
    #[serde(default)]
    attachment: bool,
}

/// Reserve provider evidence for server writes and verify completion in the Firm account.
async fn verified_upload_metadata(
    state: &ApiState,
    project_id: Uuid,
    kind: &str,
    signing: Option<store::documents::Signing>,
    envelope_id: Option<Uuid>,
    mut metadata: Option<serde_json::Value>,
) -> Result<Option<serde_json::Value>, ApiError> {
    use crate::signature_adoption::internal;
    use store::assets::VERIFIED_ONBOARDING_ENVELOPE_KEY;
    use store::document_pointers::DOCUSIGN_ENVELOPE_METADATA_KEY;

    if let Some(fields) = metadata.as_mut().and_then(serde_json::Value::as_object_mut) {
        fields.remove(VERIFIED_ONBOARDING_ENVELOPE_KEY);
        fields.remove(DOCUSIGN_ENVELOPE_METADATA_KEY);
    }
    store::documents::validate_onboarding_envelope(kind, signing, envelope_id)
        .map_err(ApiError::EnvelopeAdoption)?;
    let Some(id) = envelope_id else {
        return Ok(metadata);
    };
    let db = &state.application_store;
    let scoped = state
        .signature_provider
        .for_project(db, project_id)
        .await
        .map_err(ApiError::SignatureUnavailable)?;
    let provider =
        crate::firm_docusign::resolved(scoped.as_ref(), state.signature_provider.as_ref());
    let envelope = provider
        .read_external_envelope(id)
        .await
        .map_err(internal)?;
    if envelope.envelope_id != id || envelope.status != "completed" {
        return Err(ApiError::EnvelopeAdoption(
            "The DocuSign envelope must be completed.",
        ));
    }
    crate::signature_adoption::validate(&envelope)?;
    if let Some(signature) = store::signatures::by_provider(
        db,
        store::signatures::SignatureProvider::DocuSign,
        &id.to_string(),
    )
    .await
    .map_err(internal)?
    {
        let linked = store::notations::find_by_id(db, signature.notation_id)
            .await?
            .filter(|notation| notation.project_id == project_id);
        let template = match linked {
            Some(notation) => store::templates::find_by_id(db, notation.template_id)
                .await
                .map_err(internal)?,
            None => None,
        };
        if template.is_none_or(|template| template.kind.as_deref() != Some("onboarding")) {
            return Err(ApiError::EnvelopeAdoption(
                "This DocuSign envelope belongs to a different Project or document.",
            ));
        }
    } else if let Some(provider) = scoped.as_ref() {
        let firm = store::projects::find_by_id(db, project_id)
            .await
            .map_err(internal)?
            .and_then(|project| project.firm_id)
            .ok_or(ApiError::NotFound)?;
        let (fields, names) = provider
            .envelope_project_hints(id)
            .await
            .map_err(internal)?;
        let matched =
            crate::docusign_completion::identify_project(db, firm, &envelope, &fields, &names)
                .await
                .map_err(internal)?;
        if !matched.is_ok_and(|project| project.id == project_id) {
            return Err(ApiError::EnvelopeAdoption(
                "The completed DocuSign envelope could not be matched to this Project.",
            ));
        }
    }

    let fields = metadata
        .get_or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or(ApiError::EnvelopeAdoption(
            "Document metadata must be an object.",
        ))?;
    fields.insert(
        DOCUSIGN_ENVELOPE_METADATA_KEY.to_string(),
        serde_json::json!(id),
    );
    fields.insert(
        VERIFIED_ONBOARDING_ENVELOPE_KEY.to_string(),
        serde_json::json!(id),
    );
    Ok(metadata)
}

fn invalid_document_slug_response(filename: &str, slug: &str) -> Option<Response> {
    store::documents::validate_document_slug(filename, slug)
        .err()
        .map(|error| bad_request("invalid_slug", &error.to_string()))
}

/// The body of `POST /app/api/projects/{id}/inbox-ingest`.
#[derive(Debug, Deserialize)]
struct IngestProviderMessageRequest {
    /// The raw RFC 5322 message bytes, base64-encoded, exactly as the
    /// provider holds them.
    raw: String,
    /// The provider account the message was read from (for example the
    /// Gmail address the CLI fetched it with). Ingress dedup keys on it,
    /// so a caller should keep one value per mailbox. Defaults to
    /// `cli-ingest`.
    provider_account: Option<String>,
}

/// `POST /app/api/projects/{id}/inbox-ingest` — file a provider message the
/// CLI already read (the lawyer's Gmail, a saved `.eml`) into the Firm inbox,
/// attributed to this Project. Ingest-only: nothing is sent, and the
/// conversation stays in the Inbox workflow where an Owner can reassign it.
/// Re-running the verb is safe: the message dedupes on Firm, channel,
/// provider account, and message id. A message that answers one already
/// filed in this Project (`In-Reply-To`, `References`, or Gmail's
/// `X-GM-THRID`) joins that message's Conversation, so one email thread is
/// one Conversation; a thread filed in another Project is never joined.
#[allow(clippy::too_many_lines)] // Keep the authorization checks beside the effect.
async fn ingest_provider_message_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    Json(input): Json<IngestProviderMessageRequest>,
) -> Result<Response, ApiError> {
    use base64::Engine as _;
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    let Some(project) = store::projects::find_by_id(&state.application_store, id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?
    else {
        return Err(ApiError::NotFound);
    };
    let Some(firm_id) = project.firm_id else {
        return Ok(bad_request(
            "project_has_no_firm",
            "The matter has no owning Firm, so it has no inbox to file into.",
        ));
    };
    let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(input.raw.as_bytes()) else {
        return Ok(bad_request("invalid_base64", "raw is not valid base64."));
    };
    if raw.len() > crate::inbound_email::MAX_INBOUND_MESSAGE_BYTES {
        return Ok(bad_request(
            "message_too_large",
            "The message exceeds the inbound mail size cap.",
        ));
    }
    let Some(email) = crate::inbound_email::parse_raw_eml(&raw) else {
        return Ok(bad_request(
            "unparseable_message",
            "The message could not be parsed as RFC 5322 mail.",
        ));
    };
    let provider_account = input
        .provider_account
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("cli-ingest")
        .to_string();
    let attachments = crate::inbox::ingest_attachments(
        &state.application_store,
        &state.storage,
        &email,
        firm_id,
        Some(id),
    )
    .await
    .map_err(ApiError::Db)?;
    let provider_message_id = email
        .message_id
        .clone()
        .unwrap_or_else(|| format!("raw-{}", store::assets::sha256_hex(&raw)));
    let occurred_at = crate::inbound_email::date_from_raw(&raw)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let thread = crate::inbound_email::thread_keys_from_raw(&raw);
    let ingested = store::inbox::ingest_inbound_threaded(
        &state.application_store,
        &store::inbox::IngestInbound {
            raw_storage_key: None,
            firm_id,
            project_id: Some(id),
            brand_key: None,
            interaction_type: Some("message"),
            provider_post_id: None,
            parent_provider_message_id: thread.references.first().map(String::as_str),
            visibility: None,
            channel: store::inbox::channel::EMAIL,
            provider_account: &provider_account,
            provider_message_id: &provider_message_id,
            provider_thread_ref: thread.provider_thread_ref.as_deref(),
            external_address: &email.from,
            external_name: None,
            subject: Some(email.subject.as_str()),
            body: email.text.as_str(),
            source_page: None,
            source_channel: Some(store::inbox::channel::EMAIL),
            matter_consent: true,
            promotional_opt_in: false,
            email_conversation_id: None,
            attachments,
            occurred_at: &occurred_at,
        },
        &thread.references,
    )
    .await
    .map_err(|error| ApiError::Db(error.to_string()))?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "conversation_id": ingested.conversation_id,
            "message_id": ingested.message_id,
            "deduped": ingested.deduped,
        })),
    )
        .into_response())
}

/// `POST /app/api/projects/{id}/documents` — file a document into a matter, the
/// REST mirror of the lawyer upload control. Bytes go to authoritative object
/// storage and the response is the source-safe pointer that may be committed.
/// Lawyer-tier only and matter-scoped (out-of-scope → 404). `201` for a new
/// revision and `200` for an identical retry; a blank filename,
/// a missing or blank `kind`, undecodable base64, or a `kind` the asset lane
/// does not accept is `400`.
///
/// That last one is why this door carries [`ApiError::Ingest`] rather than collapsing
/// every ingest failure into a 500: the lawyer form constrains `kind` to a `<select>`
/// and so cannot produce a bad value, but this door is reachable without the form.
#[allow(clippy::too_many_lines)]
async fn upload_document_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<UploadDocumentRequest>,
) -> Result<Response, ApiError> {
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    let filename = input.filename.trim();
    if filename.is_empty() {
        return Ok(bad_request("filename_required", "A filename is required."));
    }
    let kind = input
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(kind) = kind else {
        return Ok(bad_request(
            "kind_required",
            &format!(
                "A document kind is required. Accepted values are: {}.",
                accepted_asset_kinds().join(", ")
            ),
        ));
    };
    if let Err(error) =
        store::documents::ReceiptMetadata::from_asset_metadata(kind, input.metadata.as_ref())
    {
        return Ok(bad_request("invalid_receipt", &error.to_string()));
    }
    if let Some(quality) = input.transcript_quality.as_deref() {
        if kind != "transcript" || !matches!(quality, "machine" | "proofread") {
            return Ok(bad_request(
                "invalid_transcript_quality",
                "Transcript quality must be `machine` or `proofread` and requires kind `transcript`.",
            ));
        }
    }
    if let Some(error) = input
        .notarization
        .iter()
        .find_map(|notarization| notarization.validate().err())
    {
        return Ok(bad_request("invalid_notarization", &error.to_string()));
    }
    if let Some(error) = input.evidence.as_ref().and_then(|e| e.validate().err()) {
        return Ok(bad_request("invalid_evidence", &error.to_string()));
    }
    let folder = match store::document_pointers::MatterFolder::parse_checked_folder(
        input
            .folder
            .as_deref()
            .map(str::trim)
            .filter(|folder| !folder.is_empty()),
    ) {
        Ok(folder) => folder,
        Err(error) => return Ok(bad_request("invalid_folder", &error.to_string())),
    };
    if input.derived_from.is_some() && kind != "transcript" {
        return Ok(bad_request(
            "invalid_derived_from",
            "A source revision may be attached only to a transcript.",
        ));
    }
    if let Some(response) = input
        .expected_head
        .as_ref()
        .and_then(invalid_expected_head_response)
    {
        return Ok(response);
    }
    if let Some(source) = input.derived_from.as_ref() {
        if source.version == 0
            || source.sha256.len() != 64
            || !source
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Ok(bad_request(
                "invalid_derived_from",
                "The source revision needs a positive version and a lowercase SHA-256 digest.",
            ));
        }
        let Some(source_asset) =
            store::assets::find_by_id(&state.application_store, source.document_id)
                .await
                .map_err(ApiError::Asset)?
        else {
            return Ok(bad_request(
                "invalid_derived_from",
                "The source revision is not a current document on this matter.",
            ));
        };
        let Some(source_slug) = source_asset
            .slug
            .as_deref()
            .filter(|_| source_asset.project_id == Some(id))
        else {
            return Ok(bad_request(
                "invalid_derived_from",
                "The source revision is not a current document on this matter.",
            ));
        };
        let revisions = store::assets::revisions(&state.application_store, id, source_slug)
            .await
            .map_err(ApiError::Asset)?;
        if revisions.first().map(|current| current.id) != Some(source.document_id)
            || revisions.len() != source.version
            || source_asset.sha256_hex != source.sha256
        {
            return Ok(bad_request(
                "invalid_derived_from",
                "The source revision is no longer operative on this matter.",
            ));
        }
    }
    let bytes = {
        use base64::Engine as _;
        match base64::engine::general_purpose::STANDARD.decode(input.content_base64.as_bytes()) {
            Ok(b) => b,
            Err(_) => {
                return Ok(bad_request(
                    "invalid_base64",
                    "content_base64 is not valid base64.",
                ));
            }
        }
    };
    if bytes.len() > store::documents::max_document_upload_bytes(kind) {
        return Ok(bad_request(
            "document_too_large",
            &store::documents::document_upload_size_message_for_kind(kind, bytes.len()),
        ));
    }
    let visibility = if input.visibility.as_deref() == Some(store::documents::visibility::CLIENT) {
        store::documents::visibility::CLIENT
    } else {
        store::documents::visibility::INTERNAL
    };
    let slug = input
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|slug| !slug.is_empty())
        .unwrap_or(filename);
    if let Some(response) = invalid_document_slug_response(filename, slug) {
        return Ok(response);
    }
    if store::assets::revisions(&state.application_store, id, slug)
        .await
        .map_err(ApiError::Asset)?
        .iter()
        .any(|revision| revision.deleted_at.is_some())
    {
        return Ok(conflict(
            "document_deleted",
            "This document was soft-deleted; restore it before filing a new revision.",
        ));
    }
    let inherited_source = if kind == "transcript"
        && input.transcript_quality.as_deref() == Some("proofread")
        && input.derived_from.is_none()
    {
        store::assets::revisions(&state.application_store, id, slug)
            .await
            .map_err(ApiError::Asset)?
            .first()
            .and_then(|current| current.derived_from.clone())
    } else {
        None
    };
    let derived_from = input
        .derived_from
        .as_ref()
        .map(|source| {
            serde_json::json!({
                "document_id": source.document_id,
                "version": source.version,
                "sha256": source.sha256,
            })
        })
        .or(inherited_source);
    let metadata = verified_upload_metadata(
        &state,
        id,
        kind,
        input.signing,
        input.envelope_id,
        input.metadata,
    )
    .await?;
    let mail_provenance = match input.mail_provenance.as_ref() {
        Some(provenance) => {
            let message_id = provenance.message_id.trim();
            let sender = provenance.sender.trim();
            let received_at = provenance.received_at.trim();
            if message_id.is_empty() || sender.is_empty() || received_at.is_empty() {
                return Ok(bad_request(
                    "invalid_mail_provenance",
                    "Mail provenance needs the message id, sender, and received date.",
                ));
            }
            Some(store::documents::MailProvenance {
                message_id,
                sender,
                received_at,
                subject: provenance.subject.as_deref().unwrap_or(""),
            })
        }
        None => None,
    };
    // The original mail document may already live under a different slug.
    // A named attachment belongs to one message, and its explicit slug
    // governs retries even when another message carries the same attachment.
    if input
        .mail_provenance
        .as_ref()
        .is_some_and(|provenance| !provenance.attachment)
    {
        let sha_hex = store::assets::sha256_hex(&bytes);
        match store::assets::find_filed_copy(&state.application_store, id, filename, &sha_hex).await
        {
            Ok(Some(existing)) => {
                let pointer = document_pointer(
                    &state.application_store,
                    id,
                    existing.slug.as_deref().unwrap_or(slug),
                    existing.kind.as_deref().unwrap_or(kind),
                )
                .await?;
                return Ok((StatusCode::OK, Json(pointer)).into_response());
            }
            Ok(None) => {}
            Err(error) => return Err(ApiError::Asset(error)),
        }
    }
    let args = store::documents::IngestArgs {
        project_id: id,
        source: store::documents::source::UPLOAD,
        filename,
        kind,
        content_type: input
            .content_type
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("application/octet-stream"),
        description: input
            .description
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        secondary_storage_key: None,
        visibility,
    };
    let revision = {
        let identity = store::documents::DocumentIdentity {
            slug: Some(slug),
            published_at: None,
            metadata,
            derived_from,
            transcript_quality: if kind == "transcript" {
                input.transcript_quality.as_deref()
            } else {
                None
            },
            signing: input.signing,
            notarization: input.notarization.clone(),
            evidence: input.evidence.clone(),
            folder: folder.map(store::document_pointers::MatterFolder::as_str),
        };
        match mail_provenance.as_ref() {
            Some(provenance) => {
                store::assets::file_revision_with_provenance_and_expected_head(
                    &state.application_store,
                    &state.storage,
                    &args,
                    &identity,
                    provenance,
                    input.expected_head.as_ref(),
                    &bytes,
                )
                .await
            }
            None => {
                store::assets::file_revision_with_expected_head(
                    &state.application_store,
                    &state.storage,
                    &args,
                    &identity,
                    input.expected_head.as_ref(),
                    &bytes,
                )
                .await
            }
        }
    };
    let filed = match revision {
        Ok(filed) => filed,
        Err(store::assets::RevisionError::HeadConflict { current, .. }) => {
            return Ok(head_conflict_response(current.as_ref()));
        }
        Err(error) => return Err(ApiError::Revision(error)),
    };
    let status = match filed {
        store::assets::Filed::Revision(_) => StatusCode::CREATED,
        store::assets::Filed::Unchanged { .. } => StatusCode::OK,
    };
    let pointer = document_pointer(&state.application_store, id, slug, kind).await?;
    Ok((status, Json(pointer)).into_response())
}

/// Request body for `POST /app/api/projects/{id}/documents/upload-session` —
/// the document to stream, with the same document metadata the buffered door
/// takes, minus the bytes. The size and SHA-256 of the whole original file
/// arrive here instead; the staged chunks are verified against both before
/// anything is filed.
#[derive(Deserialize)]
struct CreateUploadSessionRequest {
    filename: String,
    slug: Option<String>,
    content_type: Option<String>,
    kind: Option<String>,
    visibility: Option<String>,
    description: Option<String>,
    metadata: Option<serde_json::Value>,
    derived_from: Option<store::documents::DerivedFrom>,
    transcript_quality: Option<String>,
    signing: Option<store::documents::Signing>,
    envelope_id: Option<Uuid>,
    #[serde(default)]
    notarization: Vec<store::documents::Notarization>,
    /// The matter folder the document is filed in (LAW-241), spelled
    /// exactly as the Firm's matter folders spell it. Optional; an unknown
    /// value is `400 invalid_folder` and opens no session.
    #[serde(default)]
    folder: Option<String>,
    /// The byte size of the whole file, as the client counts it.
    declared_size: usize,
    /// Lowercase hex SHA-256 of the whole file, as the client hashed it.
    declared_sha256: String,
}

/// Request body for the chunk-append door: one base64-encoded chunk. The
/// session the chunk belongs to rides in the body — never URL material.
#[derive(Deserialize)]
struct UploadChunkRequest {
    session_id: Uuid,
    offset: i64,
    content_base64: String,
}

/// Request body for the status and completion doors: the session to read or
/// file. The session id arrives in the body, not the path, so the identifier
/// never lands in a URL, a proxy log, or a CodeQL cleartext finding.
#[derive(Deserialize)]
struct UploadSessionRefRequest {
    session_id: Uuid,
    /// The chain head the caller read before uploading (LAW-284), honored
    /// by the complete door — the moment the bytes are actually filed.
    /// `409 head_conflict` names the live head when the chain moved; absent
    /// files exactly as before.
    #[serde(default)]
    expected_head: Option<store::assets::ExpectedHead>,
}

/// The wire shape of an upload session: where the upload stands and what the
/// next append must carry. `complete` is true once every declared byte has
/// arrived and only the completion call remains.
#[derive(Serialize)]
struct UploadSessionResponse {
    id: Uuid,
    chunk_size: i64,
    received_bytes: i64,
    declared_size: i64,
    complete: bool,
    expires_at: String,
}

impl From<&store::documents::UploadSession> for UploadSessionResponse {
    fn from(session: &store::documents::UploadSession) -> Self {
        Self {
            id: session.id,
            chunk_size: session.chunk_size,
            received_bytes: session.received_bytes,
            declared_size: session.declared_size,
            complete: session.received_bytes >= session.declared_size,
            expires_at: session.expires_at.to_rfc3339(),
        }
    }
}

/// `410 Gone` — the session expired between calls; the client opens a new
/// one. There is no `gone` helper because only the upload session expires.
fn upload_session_expired(at: &str) -> Response {
    (
        StatusCode::GONE,
        Json(serde_json::json!({
            "error": "upload_session_expired",
            "message": format!("This upload session expired at {at}; open a new one and start over."),
        })),
    )
        .into_response()
}

/// `422 Unprocessable Entity` — the staged bytes hashed to something other
/// than the digest the client declared. The session and its parts are gone
/// by the time this response leaves, so a retry starts clean rather than
/// completing into a corrupted document.
fn upload_sha_mismatch(stored: &str, declared: &str) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(serde_json::json!({
            "error": "sha_mismatch",
            "message": format!(
                "The staged bytes hash to {stored}, not the declared {declared}; \
                 the session was removed, so a retry starts clean."
            ),
        })),
    )
        .into_response()
}

async fn streaming_source_revision(
    state: &ApiState,
    id: Uuid,
    source: &store::documents::DerivedFrom,
) -> Result<serde_json::Value, Response> {
    if source.version == 0
        || source.sha256.len() != 64
        || !source
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(bad_request(
            "invalid_derived_from",
            "The source revision needs a positive version and a lowercase SHA-256 digest.",
        ));
    }
    let Some(source_asset) =
        store::assets::find_by_id(&state.application_store, source.document_id)
            .await
            .map_err(|error| ApiError::Asset(error).into_response())?
    else {
        return Err(bad_request(
            "invalid_derived_from",
            "The source revision is not a current document on this matter.",
        ));
    };
    let Some(source_slug) = source_asset
        .slug
        .as_deref()
        .filter(|_| source_asset.project_id == Some(id))
    else {
        return Err(bad_request(
            "invalid_derived_from",
            "The source revision is not a current document on this matter.",
        ));
    };
    let revisions = store::assets::revisions(&state.application_store, id, source_slug)
        .await
        .map_err(|error| ApiError::Asset(error).into_response())?;
    if revisions.first().map(|current| current.id) != Some(source.document_id)
        || revisions.len() != source.version
        || source_asset.sha256_hex != source.sha256
    {
        return Err(bad_request(
            "invalid_derived_from",
            "The source revision is no longer operative on this matter.",
        ));
    }
    Ok(serde_json::json!({
        "document_id": source.document_id,
        "version": source.version,
        "sha256": source.sha256,
    }))
}

fn streaming_session_args<'a>(
    id: Uuid,
    input: &'a CreateUploadSessionRequest,
    filename: &'a str,
    kind: &'a str,
    slug: &'a str,
    derived_from: Option<serde_json::Value>,
    folder: Option<&'a str>,
) -> store::documents::UploadSessionArgs<'a> {
    let visibility = if input.visibility.as_deref() == Some(store::documents::visibility::CLIENT) {
        store::documents::visibility::CLIENT
    } else {
        store::documents::visibility::INTERNAL
    };
    let signing = input
        .signing
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .ok()
        .flatten();
    let notarization = serde_json::to_value(&input.notarization).ok();
    store::documents::UploadSessionArgs {
        project_id: id,
        filename,
        kind,
        content_type: input
            .content_type
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("application/octet-stream"),
        visibility,
        description: input
            .description
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        slug: Some(slug),
        transcript_quality: if kind == "transcript" {
            input.transcript_quality.as_deref()
        } else {
            None
        },
        metadata: input.metadata.clone(),
        derived_from,
        signing,
        notarization,
        folder,
        declared_size: input.declared_size,
        declared_sha256: &input.declared_sha256,
    }
}

/// The metadata checks the streaming door shares with the buffered one: a
/// non-empty filename, a known `kind`, transcript-quality rules,
/// notarization validity, a recognized `folder`, and a derived source only
/// on transcripts. Returns the trimmed filename, resolved kind, and parsed
/// folder, or the `400` response the door replies with.
fn validate_upload_session_metadata(
    input: &CreateUploadSessionRequest,
) -> Result<(&str, &str, Option<store::document_pointers::MatterFolder>), Response> {
    let filename = input.filename.trim();
    if filename.is_empty() {
        return Err(bad_request("filename_required", "A filename is required."));
    }
    let Some(kind) = input
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Err(bad_request(
            "kind_required",
            &format!(
                "A document kind is required. Accepted values are: {}.",
                accepted_asset_kinds().join(", ")
            ),
        ));
    };
    if let Err(error) =
        store::documents::ReceiptMetadata::from_asset_metadata(kind, input.metadata.as_ref())
    {
        return Err(bad_request("invalid_receipt", &error.to_string()));
    }
    if let Some(quality) = input.transcript_quality.as_deref() {
        if kind != "transcript" || !matches!(quality, "machine" | "proofread") {
            return Err(bad_request(
                "invalid_transcript_quality",
                "Transcript quality must be `machine` or `proofread` and requires kind `transcript`.",
            ));
        }
    }
    if let Some(error) = input
        .notarization
        .iter()
        .find_map(|notarization| notarization.validate().err())
    {
        return Err(bad_request("invalid_notarization", &error.to_string()));
    }
    let folder = match store::document_pointers::MatterFolder::parse_checked_folder(
        input
            .folder
            .as_deref()
            .map(str::trim)
            .filter(|folder| !folder.is_empty()),
    ) {
        Ok(folder) => folder,
        Err(error) => return Err(bad_request("invalid_folder", &error.to_string())),
    };
    if input.derived_from.is_some() && kind != "transcript" {
        return Err(bad_request(
            "invalid_derived_from",
            "A source revision may be attached only to a transcript.",
        ));
    }
    Ok((filename, kind, folder))
}

/// `POST /app/api/projects/{id}/documents/upload-session` — open (or resume)
/// the streaming upload for one document (LAW-172). The streaming mirror of
/// [`upload_document_door`]: the same document metadata, the same lawyer-tier
/// and matter scoping, and the same validation of `kind`, transcript quality,
/// notarization, and derived source. Instead of bytes it takes the whole
/// file's `declared_size` and `declared_sha256`; the response carries the
/// server-chosen `chunk_size` and `received_bytes` the client resumes from.
/// `201` for a new session, `200` for the resumed one; `413` past the
/// streaming cap; the same `400` cases the buffered door refuses.
async fn create_upload_session_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<CreateUploadSessionRequest>,
) -> Result<Response, ApiError> {
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    let (filename, kind, folder) = match validate_upload_session_metadata(&input) {
        Ok(parsed) => parsed,
        Err(response) => return Ok(response),
    };
    let derived_from = match input.derived_from.as_ref() {
        Some(source) => match streaming_source_revision(&state, id, source).await {
            Ok(source) => Some(source),
            Err(response) => return Ok(response),
        },
        None => None,
    };
    if input.declared_size > store::documents::MAX_STREAMING_UPLOAD_BYTES {
        return Ok((
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({
                "error": "upload_too_large",
                "message": store::documents::streaming_upload_size_message(input.declared_size),
            })),
        )
            .into_response());
    }
    if input.declared_size == 0 {
        return Ok(bad_request(
            "empty_upload",
            "A streaming upload needs at least one byte.",
        ));
    }
    let slug = input
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|slug| !slug.is_empty())
        .unwrap_or(filename)
        .to_string();
    if let Some(response) = invalid_document_slug_response(filename, &slug) {
        return Ok(response);
    }
    let metadata = verified_upload_metadata(
        &state,
        id,
        kind,
        input.signing,
        input.envelope_id,
        input.metadata.clone(),
    )
    .await?;
    let mut args = streaming_session_args(
        id,
        &input,
        filename,
        kind,
        &slug,
        derived_from,
        folder.map(store::document_pointers::MatterFolder::as_str),
    );
    args.metadata = metadata;
    match store::documents::create_upload_session(&state.application_store, &args).await {
        Ok(session) => {
            let status = if session.received_bytes > 0 {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            Ok((status, Json(UploadSessionResponse::from(&session))).into_response())
        }
        Err(error) => Err(ApiError::UploadSession(error)),
    }
}

/// `POST /app/api/projects/{id}/documents/upload-session/status` — where
/// the streaming upload stands (LAW-172). The resume probe: a client that
/// lost its connection reads the acknowledged byte count from here instead of
/// restarting a file it cannot re-fetch. The session id rides in the body,
/// never the path. `404` for a foreign, finished, or never-opened session.
async fn upload_session_status_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<UploadSessionRefRequest>,
) -> Result<Response, ApiError> {
    let session_id = input.session_id;
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    match store::documents::find_upload_session(&state.application_store, id, session_id).await {
        Ok(Some(session)) => {
            Ok((StatusCode::OK, Json(UploadSessionResponse::from(&session))).into_response())
        }
        Ok(None) => Err(ApiError::NotFound),
        Err(error) => Err(ApiError::UploadSession(error)),
    }
}

/// `POST /app/api/projects/{id}/documents/upload-session/chunk` —
/// stage one chunk at exactly the offset the session counts (LAW-172). The
/// base64 mirror of the buffered door's byte field; the route's request cap
/// is [`store::documents::MAX_UPLOAD_CHUNK_REQUEST_BYTES`], one chunk plus
/// JSON headroom. The response is the session status, so the client resumes
/// from the acknowledged count. `200` on an applied chunk or an idempotent
/// replay of the last one (`complete: true`), `409` when the chunk is not the
/// one the offset names, `410` when the session expired, `404` for a foreign
/// or finished session.
async fn append_upload_chunk_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<UploadChunkRequest>,
) -> Result<Response, ApiError> {
    let session_id = input.session_id;
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    let chunk = {
        use base64::Engine as _;
        match base64::engine::general_purpose::STANDARD.decode(input.content_base64.as_bytes()) {
            Ok(bytes) => bytes,
            Err(_) => {
                return Ok(bad_request(
                    "invalid_base64",
                    "content_base64 is not valid base64.",
                ))
            }
        }
    };
    match store::documents::append_upload_chunk(
        &state.application_store,
        &state.storage,
        id,
        session_id,
        input.offset,
        &chunk,
    )
    .await
    {
        Ok(session) => {
            Ok((StatusCode::OK, Json(UploadSessionResponse::from(&session))).into_response())
        }
        // A replayed last chunk is not an error: the client may not have
        // seen the first acknowledgement.
        Err(store::documents::UploadSessionError::AlreadyComplete) => {
            match store::documents::find_upload_session(&state.application_store, id, session_id).await {
                Ok(Some(session)) => Ok((
                    StatusCode::OK,
                    Json(UploadSessionResponse::from(&session)),
                )
                    .into_response()),
                Ok(None) | Err(_) => Err(ApiError::NotFound),
            }
        }
        Err(store::documents::UploadSessionError::NoSuchSession(_)) => Err(ApiError::NotFound),
        Err(store::documents::UploadSessionError::Expired(at)) => Ok(upload_session_expired(&at)),
        Err(store::documents::UploadSessionError::ChunkOffset { expected, actual }) => Ok(conflict(
            "chunk_offset_mismatch",
            &format!("The next append must start at offset {expected}, not {actual}. Re-read the session status."),
        )),
        Err(store::documents::UploadSessionError::ChunkLength {
            offset,
            expected,
            actual,
        }) => Ok(conflict(
            "chunk_offset_mismatch",
            &format!(
                "The next append must carry exactly {expected} bytes at offset \
                 {offset}, not {actual}. Re-read the session status and resume \
                 from the acknowledged offset."
            ),
        )),
        Err(error) => Err(ApiError::UploadSession(error)),
    }
}

/// `POST /app/api/projects/{id}/documents/upload-session/complete`
/// — verify the staged bytes against the declared digest and file the
/// document through the ordinary revision ingest (LAW-172). The session id
/// rides in the body, never the path. The response is
/// the same source-safe pointer the buffered door returns. `201` for a new
/// revision and `200` for an identical one; `409` until every byte has
/// arrived; `409 head_conflict` when the caller sent an `expected_head` the
/// live chain no longer matches (LAW-284), naming the live head; `422`
/// when the staged bytes hash to something other than the
/// declared digest (the session is removed, so a retry starts clean); `404`
/// for a foreign, finished, or never-opened session.
async fn complete_upload_session_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<UploadSessionRefRequest>,
) -> Result<Response, ApiError> {
    let session_id = input.session_id;
    if let Some(response) = input
        .expected_head
        .as_ref()
        .and_then(invalid_expected_head_response)
    {
        return Ok(response);
    }
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    let session =
        match store::documents::find_upload_session(&state.application_store, id, session_id).await
        {
            Ok(Some(session)) => session,
            Ok(None) => return Err(ApiError::NotFound),
            Err(error) => return Err(ApiError::UploadSession(error)),
        };
    let slug = session
        .slug
        .clone()
        .unwrap_or_else(|| session.filename.clone());
    let kind = session.kind.clone();
    match store::documents::complete_upload(
        &state.application_store,
        &state.storage,
        id,
        session_id,
        input.expected_head.as_ref(),
    )
    .await
    {
        Ok(filed) => {
            let status = match filed {
                store::assets::Filed::Revision(_) => StatusCode::CREATED,
                store::assets::Filed::Unchanged { .. } => StatusCode::OK,
            };
            let pointer = document_pointer(&state.application_store, id, &slug, &kind).await?;
            Ok((status, Json(pointer)).into_response())
        }
        Err(store::documents::UploadSessionError::NoSuchSession(_)) => Err(ApiError::NotFound),
        Err(store::documents::UploadSessionError::Revision(
            store::assets::RevisionError::HeadConflict { current, .. },
        )) => Ok(head_conflict_response(current.as_ref())),
        Err(store::documents::UploadSessionError::Expired(at)) => Ok(upload_session_expired(&at)),
        Err(store::documents::UploadSessionError::Incomplete { received, declared }) => {
            Ok(conflict(
                "upload_incomplete",
                &format!(
                    "The upload is incomplete: {received} of {declared} bytes \
                     received. Resume from the acknowledged offset."
                ),
            ))
        }
        Err(store::documents::UploadSessionError::ShaMismatch { stored, declared }) => {
            Ok(upload_sha_mismatch(&stored, &declared))
        }
        Err(error) => Err(ApiError::UploadSession(error)),
    }
}

async fn document_pointer(
    application_store: &store::application_store::ApplicationStore,
    project_id: Uuid,
    slug: &str,
    kind: &str,
) -> Result<store::document_pointers::DocumentPointer, ApiError> {
    let revisions = store::assets::revisions(application_store, project_id, slug)
        .await
        .map_err(ApiError::Asset)?;
    let current = revisions.first().ok_or_else(|| {
        ApiError::Db("a filed document revision could not be read back".to_string())
    })?;
    Ok(store::document_pointers::DocumentPointer {
        receipt: store::documents::ReceiptMetadata::from_asset_metadata(
            kind,
            current.metadata.as_ref().filter(|_| kind == "receipt"),
        )
        .map_err(|error| ApiError::Db(error.to_string()))?,
        checkout_path: current.drive_checkout_path().map(str::to_owned),
        kind: kind.to_string(),
        visibility: current.visibility.clone(),
        // LAW-241: the live row's persisted folder renders back on the
        // pointer — the same value `project sync` reconciles. Validated at
        // every write, so a value that fails to parse is a corrupted row,
        // not a caller error; it renders as the kind's default placement.
        folder: current
            .folder
            .as_deref()
            .and_then(store::document_pointers::MatterFolder::parse),
        current_version: store::document_pointers::PointerVersion {
            version: revisions.len(),
            asset_id: current.id,
            created_at: current
                .inserted_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            sha256: current.sha256_hex.clone(),
            size_bytes: current.byte_size,
            canonical_url: None,
            checked_on: None,
        },
        previous_version: revisions.get(1).map(|asset| asset.id),
        // A plain Project document, filed through `POST
        // /app/api/projects/{id}/documents`, never carries an Authority —
        // that field is written only by `project sync`'s evidence route, which
        // constructs its own pointer client-side rather than reaching this
        // door at all (see `cli::document_sync`).
        authority_id: None,
        // ENG-859 + LAW-344: this door recovers the envelope id from the
        // asset metadata the completion webhook's file-back stamps — a
        // signed copy filed on the matter names the envelope that executed
        // it. A non-UUID value (a fake envelope) yields no id.
        docusign_envelope_id: store::document_pointers::envelope_id_from_asset_metadata(
            current.metadata.as_ref(),
        ),
        xero_invoice_id: None,
        signing: current.signing,
        notarization: current.notarization.clone(),
        // The evidence block rides with the current revision, so a pointer
        // filed here carries the same provenance `project sync` records.
        evidence: current.evidence.clone(),
        service: Vec::new(),
    })
}

/// Request body for `POST /app/api/projects/{id}/mail/file` (ENG-517).
#[derive(Deserialize)]
struct MailFileRequest {
    /// The `email_conversation_message` row naming the inbound hop to file
    /// attachments from.
    message_id: Uuid,
    /// Required asset-lane kind, applied to every attachment in this
    /// message — a decision, never a default, matching `POST .../documents`.
    kind: Option<String>,
    /// `"client"` makes every filed attachment client-visible; anything else
    /// (the default) files them as internal work product.
    visibility: Option<String>,
    /// List what would be filed, writing nothing.
    #[serde(default)]
    dry_run: bool,
}

/// One attachment as `navigator site mail file` reports it, filed or not.
#[derive(Serialize)]
struct MailFileAttachment {
    filename: String,
    content_type: String,
    size_bytes: usize,
}

#[derive(Serialize)]
struct MailFileResponse {
    message_id: Uuid,
    dry_run: bool,
    attachments: Vec<MailFileAttachment>,
    /// One pointer per filed attachment, in message-part order. Empty when
    /// `dry_run`, or when the message carried no attachments.
    filed: Vec<store::document_pointers::DocumentPointer>,
}

/// `POST /app/api/projects/{id}/mail/file` — file one inbound message's
/// attachments into the matter (ENG-517), entirely server-side: the raw
/// MIME is read from object storage and parsed here, so the bytes never
/// reach the caller's disk, let alone a Project repository checkout.
///
/// Lawyer-tier and matter-scoped, same as the upload door (out-of-scope →
/// 404); a missing or blank `kind` is `400 kind_required`; a message with no
/// `raw_storage_key` (nothing generated in-house has one) is
/// `400 no_raw_message`. A message with no attachments returns `200` with an
/// empty `attachments`/`filed` list rather than an error.
///
/// Each attachment's slug is derived from the message id and its position
/// (`mail/<message_id>/<index>`), never from an operator's path choice, so
/// re-running against the same message is a no-op: `file_revision`'s
/// identical-bytes rule (#511) makes a retry — or resuming after a partial
/// failure — safe, and a failed attachment leaves no pointer and no partial
/// asset because each is filed as its own independent write.
async fn mail_file_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    Json(input): Json<MailFileRequest>,
) -> Result<Response, ApiError> {
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    let kind = input
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(kind) = kind else {
        return Ok(bad_request(
            "kind_required",
            &format!(
                "A document kind is required. Accepted values are: {}.",
                accepted_asset_kinds().join(", ")
            ),
        ));
    };
    let visibility = if input.visibility.as_deref() == Some(store::documents::visibility::CLIENT) {
        store::documents::visibility::CLIENT
    } else {
        store::documents::visibility::INTERNAL
    };

    let message =
        store::email_conversations::message_by_id(&state.application_store, input.message_id)
            .await
            .map_err(|e| ApiError::Db(e.to_string()))?
            .ok_or(ApiError::NotFound)?;
    let Some(raw_key) = message.raw_storage_key.as_deref() else {
        return Ok(bad_request(
            "no_raw_message",
            "This message has no archived raw MIME to file attachments from.",
        ));
    };
    let raw = state
        .storage
        .get(raw_key)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?;
    let attachments = crate::inbound_email::attachments_from_raw(&raw.bytes);
    let summaries: Vec<MailFileAttachment> = attachments
        .iter()
        .map(|attachment| MailFileAttachment {
            filename: attachment.filename.clone(),
            content_type: attachment.content_type.clone(),
            size_bytes: attachment.bytes.len(),
        })
        .collect();

    if input.dry_run || attachments.is_empty() {
        return Ok((
            StatusCode::OK,
            Json(MailFileResponse {
                message_id: input.message_id,
                dry_run: input.dry_run,
                attachments: summaries,
                filed: Vec::new(),
            }),
        )
            .into_response());
    }

    let filed = file_mail_attachments(
        &state,
        id,
        kind,
        visibility,
        &MailAttachmentSource {
            exactly_once: false,
            attachment_kinds: None,
            skip: &[],
            message_id: &input.message_id.to_string(),
            sender: &message.from_addr,
            received_at: &message.inserted_at.to_rfc3339(),
            subject: &message.subject,
        },
        &attachments,
    )
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(MailFileResponse {
            message_id: input.message_id,
            dry_run: false,
            attachments: summaries,
            filed,
        }),
    )
        .into_response())
}

/// The message-identity half of attachment filing: what the deterministic
/// slug and the stamped [`store::documents::MailProvenance`] derive from,
/// shared by both filing doors — a stored inbound hop (`mail_file_door`) and
/// a raw `.eml` upload (`eml_upload_door`).
/// The provenance a mail-lane filing stamps. Shared with the NEF pipeline
/// (`nef_pipeline`), which files a notice's attachments the same way.
pub(crate) struct MailAttachmentSource<'a> {
    /// Source-message artifacts retain one immutable row across overlapping applies.
    pub(crate) exactly_once: bool,
    /// Optional per-part classifications; absent parts use the filing kind.
    pub(crate) attachment_kinds: Option<&'a [&'a str]>,
    /// Part indexes left unfiled; every other part keeps its own index slug.
    pub(crate) skip: &'a [usize],
    /// Slug component. A UUID for a stored message; a sanitized `.eml`
    /// message reference for an upload.
    pub(crate) message_id: &'a str,
    pub(crate) sender: &'a str,
    pub(crate) received_at: &'a str,
    pub(crate) subject: &'a str,
}

/// File every parsed attachment under its message-part slug
/// (`mail/<message_id>/<index>`), stamping [`store::documents::MailProvenance`]
/// onto each, and return one pointer per attachment in message-part order.
/// Source-message callers can require one immutable row per slug.
/// Each attachment is filed as its own independent write, so a failure here
/// leaves every earlier attachment filed and no pointer or partial asset for
/// the one that failed.
pub(crate) async fn file_mail_attachments(
    state: &ApiState,
    project_id: Uuid,
    kind: &str,
    visibility: &str,
    source: &MailAttachmentSource<'_>,
    attachments: &[crate::inbound_email::InboundAttachment],
) -> Result<Vec<store::document_pointers::DocumentPointer>, ApiError> {
    let message_id_str = source.message_id.to_string();
    let received_at = source.received_at.to_string();
    let mut filed = Vec::with_capacity(attachments.len());
    for (index, attachment) in attachments.iter().enumerate() {
        if source.skip.contains(&index) {
            continue;
        }
        let kind = source
            .attachment_kinds
            .and_then(|kinds| kinds.get(index))
            .copied()
            .unwrap_or(kind);
        let slug = format!("mail/{message_id_str}/{index}");
        let args = store::documents::IngestArgs {
            project_id,
            source: store::documents::source::EMAIL,
            filename: &attachment.filename,
            kind,
            content_type: &attachment.content_type,
            description: None,
            secondary_storage_key: None,
            visibility,
        };
        let identity = store::documents::DocumentIdentity {
            slug: Some(&slug),
            published_at: None,
            metadata: None,
            derived_from: None,
            transcript_quality: None,
            signing: None,
            notarization: Vec::new(),
            evidence: None,
            folder: None,
        };
        let provenance = store::documents::MailProvenance {
            message_id: &message_id_str,
            sender: source.sender,
            received_at: &received_at,
            subject: source.subject,
        };
        if source.exactly_once {
            store::documents::ingest_bytes_with_mail_provenance_once(
                &state.application_store,
                &state.storage,
                &args,
                &identity,
                &provenance,
                &attachment.bytes,
            )
            .await
            .map_err(|error| ApiError::Revision(store::assets::RevisionError::Ingest(error)))?;
        } else {
            store::assets::file_revision_with_provenance(
                &state.application_store,
                &state.storage,
                &args,
                &identity,
                &provenance,
                &attachment.bytes,
            )
            .await
            .map_err(ApiError::Revision)?;
        }
        filed.push(document_pointer(&state.application_store, project_id, &slug, kind).await?);
    }
    Ok(filed)
}

/// Request body for `POST /app/api/projects/{id}/mail/eml`.
#[derive(Deserialize)]
struct EmlUploadRequest {
    /// Provider message ID, scoped by the filing mailbox for Gmail deduplication.
    gmail_message_id: Option<String>,
    /// Gmail's SENT label determines direction independently of sending aliases.
    gmail_sent: Option<bool>,
    /// The raw RFC 822 message, base64-encoded: bytes the caller already
    /// holds (a saved copy, a downloaded export), so no per-machine Gmail
    /// OAuth grant is involved in filing. The legacy CLI's variant — the
    /// newer CLI files the bytes through the document lane and sends
    /// `source_document` instead. At most one of the two is present;
    /// neither is `400 missing_payload`.
    raw_base64: Option<String>,
    /// The firm mailbox the filing lawyer files from. When it matches the
    /// message's `From`, the message files as outbound. Defaults to the
    /// signed-in lawyer's own address.
    mailbox: Option<String>,
    /// Required asset-lane kind when the message carries attachments — a
    /// decision, never a default, matching `POST .../documents`. Not
    /// needed on the reference variant: its attachments are already filed.
    kind: Option<String>,
    /// `"client"` makes every filed attachment client-visible; anything else
    /// (the default) files them as internal work product.
    visibility: Option<String>,
    /// List what would file, writing nothing.
    #[serde(default)]
    dry_run: bool,
    /// The new CLI's variant: the asset id of the full raw `.eml` the
    /// caller already filed through the document lane (`mail/<message
    /// ref>/source`, `message/rfc822`). The request then carries only
    /// headers, text, and `source_ref` — no MIME bytes — and the filed
    /// Communication links this document as its own (`asset_id`, LAW-469).
    /// The document must already be filed on this matter (`400
    /// source_document_not_on_matter`); the attachments are not re-ingested.
    source_document: Option<Uuid>,
    /// Asset ids of the attachments the caller already filed through the
    /// document lane under `mail/<message ref>/<index>`. Recorded for the
    /// response's pointer list, never re-ingested; each must be on this
    /// matter (`400 attachment_document_not_on_matter`).
    #[serde(default)]
    attachment_documents: Vec<Uuid>,
    /// The external id the message dedups on (`eml:<Message-ID or
    /// digest>`), as the filing CLI parsed it. Reference filings only.
    source_ref: Option<String>,
    /// The message's `From` address; reference filings only.
    from: Option<String>,
    /// The message's `To` addresses, comma-joined; reference filings only.
    to: Option<String>,
    /// The message subject; reference filings only.
    subject: Option<String>,
    /// The plain-text body as the filing CLI parsed it; reference filings
    /// only.
    text_body: Option<String>,
    /// RFC 2822 date the message was sent or received; reference filings
    /// only.
    occurred_at: Option<String>,
}

/// One attachment as the .eml door reports it, filed or not.
#[derive(Serialize)]
struct EmlUploadAttachment {
    filename: String,
    content_type: String,
    size_bytes: usize,
}

#[derive(Serialize)]
struct EmlUploadResponse {
    /// The external id the message deduped on (`eml:<Message-ID or digest>`).
    source_ref: String,
    /// `inbound` or `outbound`, decided by the mailbox match.
    direction: String,
    /// `true` when a message with this `(channel, source_ref)` was already
    /// filed on this matter — nothing was written a second time.
    deduped: bool,
    /// The filed Communication, absent on a dry run.
    communication_id: Option<Uuid>,
    attachments: Vec<EmlUploadAttachment>,
    /// One pointer per filed attachment, in message-part order. Empty when
    /// `dry_run`, or when the message carried no attachments.
    filed: Vec<store::document_pointers::DocumentPointer>,
    /// Attachments left unfiled, and why.
    skipped: Vec<EmlSkippedAttachment>,
}

/// An attachment the .eml door did not file: an `inline_image` the HTML body
/// shows by `cid:` (a signature logo), or one `already_on_file`, which names
/// the live document on the matter that already holds the same bytes.
#[derive(Serialize)]
struct EmlSkippedAttachment {
    filename: String,
    reason: &'static str,
    document_slug: Option<String>,
}

/// The `400` error code a filing refusal names.
fn eml_parse_error_code(error: &crate::eml_filing::EmlParseError) -> &'static str {
    match error {
        crate::eml_filing::EmlParseError::NotMime => "not_mime",
        crate::eml_filing::EmlParseError::NoDate => "no_date",
        crate::eml_filing::EmlParseError::BadDate => "bad_date",
        crate::eml_filing::EmlParseError::NoBody => "no_body",
    }
}

/// `POST /app/api/projects/{id}/mail/eml` — file a raw `.eml` the caller
/// already holds as a matter Communication, each attachment filing as a
/// linked document. No Gmail OAuth grant is involved anywhere: the MIME is
/// parsed here, and the message files through the same idempotent
/// `(channel, source_ref)` ingest every email Communication uses, so
/// re-uploading the same message writes nothing a second time.
///
/// Lawyer-tier and matter-scoped, same as the sibling doors (out-of-scope →
/// 404). Attachments require a `kind` (`400 kind_required`) and file under
/// deterministic slugs (`mail/<message reference>/<index>`). `dry_run` lists
/// the attachments and writes nothing.
async fn eml_upload_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    Json(input): Json<EmlUploadRequest>,
) -> Result<Response, ApiError> {
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    // The new CLI's variant: the .eml and its attachments already sit in
    // object storage as matter documents, so this request carries only
    // their ids, headers, and text — never MIME bytes — and stays far
    // under any body limit at any message size. `source_document` links
    // the filed Communication to the stored raw message (LAW-469).
    if let Some(source_document) = input.source_document {
        if input.raw_base64.is_some() {
            return Ok(bad_request(
                "ambiguous_payload",
                "Send either raw_base64 or source_document, never both.",
            ));
        }
        if input.gmail_message_id.is_some() {
            return Ok(bad_request(
                "invalid_message_id",
                "Gmail filing uses raw_base64; a reference filing has no Gmail source.",
            ));
        }
        return Box::pin(file_eml_reference(
            &state,
            id,
            &lawyer,
            source_document,
            input.attachment_documents,
            input.source_ref.as_deref(),
            input.from.as_deref(),
            input.to.as_deref(),
            input.subject.as_deref(),
            input.text_body.as_deref(),
            input.occurred_at.as_deref(),
            input.dry_run,
            input.mailbox,
        ))
        .await;
    }
    file_raw_eml(&state, id, &lawyer, input).await
}

async fn file_raw_eml(
    state: &ApiState,
    id: Uuid,
    lawyer: &LawyerSession,
    input: EmlUploadRequest,
) -> Result<Response, ApiError> {
    use base64::Engine as _;
    let Some(raw_base64) = input.raw_base64.as_deref() else {
        return Ok(bad_request(
            "missing_payload",
            "Send the message as raw_base64, or as source_document referencing an already-filed .eml document.",
        ));
    };
    let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(raw_base64.trim()) else {
        return Ok(bad_request(
            "not_base64",
            "raw_base64 must be the raw .eml message, base64-encoded.",
        ));
    };
    if raw.len() > crate::inbound_email::MAX_INBOUND_MESSAGE_BYTES {
        return Ok(bad_request(
            "message_too_large",
            &format!(
                "The raw .eml is {} bytes; the filing door accepts at most {} bytes (the inbound-message cap). File it through the document lane instead — `navigator mail parse` does.",
                raw.len(),
                crate::inbound_email::MAX_INBOUND_MESSAGE_BYTES
            ),
        ));
    }
    let mailbox = match input.mailbox {
        Some(mailbox) => Some(mailbox),
        None => filing_lawyer_mailbox(&state.application_store, lawyer).await?,
    };
    let mut parsed = match crate::eml_filing::parse_eml(&raw, mailbox.as_deref()) {
        Ok(parsed) => parsed,
        Err(error) => {
            return Ok(bad_request(
                eml_parse_error_code(&error),
                &error.to_string(),
            ));
        }
    };
    if input.gmail_sent.is_some() && input.gmail_message_id.is_none() {
        return Ok(bad_request(
            "invalid_message_id",
            "Gmail direction requires a Gmail message ID",
        ));
    }
    if let Some(message_id) = input.gmail_message_id.as_deref() {
        parsed.source_ref = match gmail_source_ref(message_id, mailbox.as_deref()) {
            Ok(source_ref) => source_ref,
            Err((code, reason)) => return Ok(bad_request(code, reason)),
        };
        if let Some(sent) = input.gmail_sent {
            parsed.direction = if sent { "outbound" } else { "inbound" };
        }
    }
    let has_attachments = !parsed.attachments.is_empty();
    let kind = input
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if !input.dry_run && has_attachments && kind.is_none() {
        return Ok(bad_request(
            "kind_required",
            &format!(
                "A document kind is required when the message carries attachments. Accepted values are: {}.",
                accepted_asset_kinds().join(", ")
            ),
        ));
    }
    let visibility = if input.visibility.as_deref() == Some(store::documents::visibility::CLIENT) {
        store::documents::visibility::CLIENT
    } else {
        store::documents::visibility::INTERNAL
    };
    let attachments = eml_attachments(&parsed);
    if input.dry_run {
        return Ok((
            StatusCode::OK,
            Json(EmlUploadResponse {
                source_ref: parsed.source_ref,
                direction: parsed.direction.to_string(),
                deduped: false,
                communication_id: None,
                attachments,
                filed: Vec::new(),
                skipped: Vec::new(),
            }),
        )
            .into_response());
    }
    Box::pin(file_parsed_eml(
        state, id, lawyer, parsed, kind, visibility, &raw,
    ))
    .await
}

fn gmail_source_ref(
    message_id: &str,
    mailbox: Option<&str>,
) -> Result<String, (&'static str, &'static str)> {
    let mailbox = mailbox
        .filter(|mailbox| !mailbox.trim().is_empty())
        .ok_or((
            "mailbox_required",
            "Gmail filing requires the source mailbox",
        ))?;
    if !(8..=128).contains(&message_id.len())
        || !message_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(("invalid_message_id", "Gmail message ID is invalid"));
    }
    Ok(format!(
        "gmail:{}:{message_id}",
        mailbox.trim().to_ascii_lowercase()
    ))
}

/// The signed-in lawyer's own address, the mailbox a request without one
/// files from: the Person's email, else the session's sign-in email.
async fn filing_lawyer_mailbox(
    db: &store::application_store::ApplicationStore,
    lawyer: &LawyerSession,
) -> Result<Option<String>, ApiError> {
    if let Some(person_id) = lawyer.0.person_id {
        if let Some(person) = store::persons::find_by_id(db, person_id).await? {
            return Ok(Some(person.email));
        }
    }
    Ok(lawyer.0.email.clone())
}

fn eml_attachments(parsed: &crate::eml_filing::ParsedEml) -> Vec<EmlUploadAttachment> {
    parsed
        .attachments
        .iter()
        .map(|attachment| EmlUploadAttachment {
            filename: attachment.filename.clone(),
            content_type: attachment.content_type.clone(),
            size_bytes: attachment.bytes.len(),
        })
        .collect()
}

/// The filing half of the .eml door: ingest the parsed message as a
/// Communication, file its attachments, and report the outcome. Validation
/// and the dry-run report stay in the handler above.
async fn file_parsed_eml(
    state: &ApiState,
    project_id: Uuid,
    lawyer: &LawyerSession,
    parsed: crate::eml_filing::ParsedEml,
    kind: Option<&str>,
    visibility: &'static str,
    raw: &[u8],
) -> Result<Response, ApiError> {
    let attachments = eml_attachments(&parsed);
    let (channel, direction) = if parsed.direction == "outbound" {
        (
            store::communications::channel::EMAIL_OUTBOUND,
            store::communications::direction::OUTBOUND,
        )
    } else {
        (
            store::communications::channel::EMAIL_INBOUND,
            store::communications::direction::INBOUND,
        )
    };
    let counterparty = if parsed.direction == "outbound" {
        &parsed.to
    } else {
        &parsed.from
    };
    // Keep the request future small while the raw source is ingested.
    let raw_asset = Box::pin(store::assets::ingest_content(
        &state.application_store,
        &state.storage,
        raw,
        "message/rfc822",
    ))
    .await
    .map_err(|error| ApiError::Db(error.to_string()))?;
    let author_person_id = lawyer.0.person_id.ok_or(ApiError::NotFound)?;
    let ingested = store::communications::ingest_email(
        &state.application_store,
        &store::communications::IngestArgs {
            project_id,
            channel,
            direction,
            author_person_id: Some(author_person_id),
            counterparty: Some(counterparty),
            subject: parsed.subject.as_deref(),
            body: &parsed.body,
            source_ref: Some(&parsed.source_ref),
            asset_id: Some(raw_asset),
            occurred_at: &parsed.at,
        },
        store::communications::EmailDetails::default(),
    )
    .await
    .map_err(crate::communications_api::map_communication)?;
    let mut skipped: Vec<EmlSkippedAttachment> = parsed
        .inline_images
        .iter()
        .map(|filename| EmlSkippedAttachment {
            filename: filename.clone(),
            reason: "inline_image",
            document_slug: None,
        })
        .collect();
    let filed = Box::pin(file_eml_documents(
        state,
        project_id,
        &parsed,
        kind,
        visibility,
        &mut skipped,
    ))
    .await?;
    let status = if ingested.deduped {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((
        status,
        Json(EmlUploadResponse {
            source_ref: parsed.source_ref,
            direction: parsed.direction.to_string(),
            deduped: ingested.deduped,
            communication_id: Some(ingested.communication_id),
            attachments,
            filed,
            skipped,
        }),
    )
        .into_response())
}

/// The filing half of the reference variant: the caller (the new CLI) has
/// already filed the raw `.eml` and its attachments through the document
/// lane, so this files only the Communication — headers, text, and
/// `source_ref` travel in the request, the source document's asset id
/// links the message to its stored bytes (LAW-469), and no document
/// ingest runs here. The message still dedups idempotently on
/// `(channel, source_ref)`, so a retry after a partial failure writes
/// nothing a second time.
#[allow(clippy::too_many_arguments)]
async fn file_eml_reference(
    state: &ApiState,
    project_id: Uuid,
    lawyer: &LawyerSession,
    source_document: Uuid,
    attachment_documents: Vec<Uuid>,
    source_ref: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    subject: Option<&str>,
    text_body: Option<&str>,
    occurred_at: Option<&str>,
    dry_run: bool,
    mailbox: Option<String>,
) -> Result<Response, ApiError> {
    let Some(source_ref) = source_ref.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(bad_request(
            "invalid_source_ref",
            "A reference filing needs the source_ref the CLI parsed (`eml:<Message-ID or digest>`).",
        ));
    };
    if source_ref.len() > 512 {
        return Ok(bad_request(
            "invalid_source_ref",
            "The source_ref is unreasonably long.",
        ));
    }
    let Some(from) = from.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(bad_request(
            "invalid_reference",
            "A reference filing needs the message's From address.",
        ));
    };
    let Some(occurred_at) = occurred_at.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(bad_request(
            "no_date",
            "A reference filing needs the message's date.",
        ));
    };
    let Some(text_body) = text_body.filter(|s| !s.is_empty()) else {
        return Ok(bad_request(
            "no_body",
            "A reference filing needs the message's plain-text body.",
        ));
    };
    if dry_run {
        return Ok((
            StatusCode::OK,
            Json(EmlUploadResponse {
                source_ref: source_ref.to_string(),
                direction: "inbound".to_string(),
                deduped: false,
                communication_id: None,
                attachments: Vec::new(),
                filed: Vec::new(),
                skipped: Vec::new(),
            }),
        )
            .into_response());
    }
    file_validated_eml_reference(
        state,
        project_id,
        lawyer,
        source_document,
        attachment_documents,
        source_ref,
        from,
        to,
        subject,
        text_body,
        occurred_at,
        mailbox,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn file_validated_eml_reference(
    state: &ApiState,
    project_id: Uuid,
    lawyer: &LawyerSession,
    source_document: Uuid,
    attachment_documents: Vec<Uuid>,
    source_ref: &str,
    from: &str,
    to: Option<&str>,
    subject: Option<&str>,
    text_body: &str,
    occurred_at: &str,
    mailbox: Option<String>,
) -> Result<Response, ApiError> {
    let source = store::assets::find_by_id(&state.application_store, source_document)
        .await
        .map_err(ApiError::Asset)?;
    let Some(source) = source else {
        return Ok(bad_request(
            "source_document_not_on_matter",
            "The referenced source document does not exist.",
        ));
    };
    if source.project_id != Some(project_id) {
        return Ok(bad_request(
            "source_document_not_on_matter",
            "The referenced source document is not filed on this matter.",
        ));
    }
    if !attachment_documents.is_empty() {
        let attachments =
            store::assets::find_by_ids(&state.application_store, &attachment_documents)
                .await
                .map_err(ApiError::Asset)?;
        if attachments.len() != attachment_documents.len()
            || attachments
                .iter()
                .any(|asset| asset.project_id != Some(project_id))
        {
            return Ok(bad_request(
                "attachment_document_not_on_matter",
                "Every referenced attachment document must be filed on this matter.",
            ));
        }
    }
    let mailbox = match mailbox {
        Some(mailbox) => Some(mailbox),
        None => filing_lawyer_mailbox(&state.application_store, lawyer).await?,
    };
    let outbound = mailbox.as_deref().map(str::trim) == Some(from);
    let (channel, direction) = if outbound {
        (
            store::communications::channel::EMAIL_OUTBOUND,
            store::communications::direction::OUTBOUND,
        )
    } else {
        (
            store::communications::channel::EMAIL_INBOUND,
            store::communications::direction::INBOUND,
        )
    };
    let counterparty = if outbound { to.unwrap_or("") } else { from };
    let author_person_id = lawyer.0.person_id.ok_or(ApiError::NotFound)?;
    let ingested = store::communications::ingest_email(
        &state.application_store,
        &store::communications::IngestArgs {
            project_id,
            channel,
            direction,
            author_person_id: Some(author_person_id),
            counterparty: Some(counterparty),
            subject,
            body: text_body,
            source_ref: Some(source_ref),
            asset_id: Some(source_document),
            occurred_at,
        },
        store::communications::EmailDetails::default(),
    )
    .await
    .map_err(crate::communications_api::map_communication)?;
    let status = if ingested.deduped {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((
        status,
        Json(EmlUploadResponse {
            source_ref: source_ref.to_string(),
            direction: direction.to_string(),
            deduped: ingested.deduped,
            communication_id: Some(ingested.communication_id),
            attachments: Vec::new(),
            filed: Vec::new(),
            skipped: Vec::new(),
        }),
    )
        .into_response())
}

async fn file_eml_documents(
    state: &ApiState,
    project_id: Uuid,
    parsed: &crate::eml_filing::ParsedEml,
    kind: Option<&str>,
    visibility: &'static str,
    skipped: &mut Vec<EmlSkippedAttachment>,
) -> Result<Vec<store::document_pointers::DocumentPointer>, ApiError> {
    let filed = if parsed.attachments.is_empty() {
        Vec::new()
    } else {
        let message_id_slug: String = parsed
            .message_ref
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let kind = kind.expect("invariant: kind_required returned early without one");
        let skip =
            attachments_on_file(state, project_id, &message_id_slug, parsed, skipped).await?;
        Box::pin(file_mail_attachments(
            state,
            project_id,
            kind,
            visibility,
            &MailAttachmentSource {
                exactly_once: false,
                attachment_kinds: None,
                skip: &skip,
                message_id: &message_id_slug,
                sender: &parsed.from,
                received_at: &parsed.at,
                subject: parsed.subject.as_deref().unwrap_or(""),
            },
            &parsed.attachments,
        ))
        .await?
    };
    Ok(filed)
}

/// The part indexes of `parsed` whose bytes a live document on the matter
/// already holds, each named in `skipped`. A part's own earlier filing under
/// its mail slug is a replay, not a copy, so it files (and reads back) again.
async fn attachments_on_file(
    state: &ApiState,
    project_id: Uuid,
    message_id_slug: &str,
    parsed: &crate::eml_filing::ParsedEml,
    skipped: &mut Vec<EmlSkippedAttachment>,
) -> Result<Vec<usize>, ApiError> {
    let live = store::assets::for_project(&state.application_store, project_id)
        .await
        .map_err(ApiError::Asset)?;
    let mut skip = Vec::new();
    for (index, attachment) in parsed.attachments.iter().enumerate() {
        let sha = store::assets::sha256_hex(&attachment.bytes);
        let slug = format!("mail/{message_id_slug}/{index}");
        let holders: Vec<_> = live.iter().filter(|a| a.sha256_hex == sha).collect();
        if holders
            .iter()
            .any(|a| a.slug.as_deref() == Some(slug.as_str()))
        {
            continue;
        }
        if let Some(holder) = holders.first() {
            skip.push(index);
            skipped.push(EmlSkippedAttachment {
                filename: attachment.filename.clone(),
                reason: "already_on_file",
                document_slug: holder.slug.clone(),
            });
        }
    }
    Ok(skip)
}

/// `GET /app/api/projects/{id}/documents/revisions?slug=`, the query, not a
/// path segment — a slug may itself contain `/` (`navigator project sync`
/// derives it from a path below `documents/`), so it cannot be a single
/// route segment the way `asset_id` is.
#[derive(Deserialize)]
struct RevisionsQuery {
    slug: String,
}

/// One revision as `log`/`get` (the CLI's read verbs, #485) report it.
#[derive(Debug, Serialize)]
struct RevisionSummary {
    /// 1-based, oldest first — `1` is the first revision ever filed under
    /// this slug (as the caller's lens sees it; see `operative` below for
    /// why a client's numbering never reveals a hidden revision).
    version: usize,
    asset_id: Uuid,
    created_at: String,
    sha256: String,
    size_bytes: i64,
    filename: String,
    /// `internal` or `client` on this revision.
    visibility: String,
    /// Whether this is the operative revision for the caller's lens — the
    /// lawyer's newest row, or the client's newest published+client-visible
    /// row. At most one `true` per response.
    operative: bool,
    /// How this revision was signed, when that was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    signing: Option<store::documents::Signing>,
    /// The notarial acknowledgments recorded on this revision.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    notarization: Vec<store::documents::Notarization>,
    /// When the document was soft-deleted. Only a lawyer-lens caller reads a
    /// soft-deleted chain at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    deleted_at: Option<String>,
    /// When the purge may expunge a soft-deleted chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    purge_after: Option<String>,
}

#[derive(Serialize)]
struct DocumentRevisionsResponse {
    kind: String,
    /// Newest first, matching `store::assets::revisions`.
    revisions: Vec<RevisionSummary>,
}

/// `GET /app/api/projects/{id}/documents/revisions?slug=` — a document's
/// revision chain under the caller's own lens, for `navigator site document log`
/// and the version lookup `navigator site document get --version` needs.
///
/// A client lens sees only published, client-visible revisions, filtered
/// *before* numbering — so version numbers are renumbered over the visible
/// subset rather than the true chain, and a gap (a lawyer-only revision
/// between two client-visible ones) never appears. That is what keeps a
/// client-lens caller from learning a lawyer-only revision exists at all,
/// matching the guarantee `store::assets::current`'s `Lens::Client` already
/// gives the single-row read.
async fn list_document_revisions_door(
    State(state): State<ApiState>,
    authed: AuthedSession,
    Path(id): Path<Uuid>,
    Query(query): Query<RevisionsQuery>,
) -> Result<Response, ApiError> {
    let lens = store::access::matter_lens(
        &state.application_store,
        authed.0.person_id,
        authed.0.role,
        id,
    )
    .await
    .map_err(ApiError::Db)?
    .ok_or(ApiError::NotFound)?;
    let slug = query.slug.trim();
    if slug.is_empty() {
        return Err(ApiError::NotFound);
    }
    let mut revisions = store::assets::revisions(&state.application_store, id, slug)
        .await
        .map_err(ApiError::Asset)?;
    if matches!(lens, store::access::ProjectLens::Client) {
        revisions.retain(|asset| {
            asset.visibility == store::documents::visibility::CLIENT
                && asset.published_at.is_some()
                && asset.deleted_at.is_none()
        });
    }
    if revisions.is_empty() {
        return Err(ApiError::NotFound);
    }
    let kind = revisions[0].kind.clone().unwrap_or_default();
    let total = revisions.len();
    let body = DocumentRevisionsResponse {
        kind,
        revisions: revisions
            .into_iter()
            .enumerate()
            .map(|(index, asset)| revision_summary(&asset, total - index, index == 0))
            .collect(),
    };
    Ok((StatusCode::OK, Json(body)).into_response())
}

fn revision_summary(
    asset: &store::assets::Asset,
    version: usize,
    operative: bool,
) -> RevisionSummary {
    RevisionSummary {
        version,
        asset_id: asset.id,
        created_at: asset
            .inserted_at
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        sha256: asset.sha256_hex.clone(),
        size_bytes: asset.byte_size,
        filename: asset.filename.clone().unwrap_or_default(),
        visibility: asset.visibility.clone(),
        operative,
        signing: asset.signing,
        notarization: asset.notarization.clone(),
        deleted_at: asset
            .deleted_at
            .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        purge_after: asset
            .purge_after
            .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
    }
}

/// Most rows one integrity page may carry. A page's storage reads run
/// [`INTEGRITY_CONCURRENCY`] at a time, so this bounds the page's wall time
/// under the gateway's idle timeout.
const INTEGRITY_MAX_PAGE: usize = 100;
/// Storage objects read at once while answering one integrity page.
const INTEGRITY_CONCURRENCY: usize = 8;
/// A page slower than this is logged as slow.
const INTEGRITY_SLOW_PAGE: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug, Deserialize, Default)]
struct IntegrityQuery {
    #[serde(default)]
    deep: bool,
    /// Rows to skip in the slug-then-id ordering. Defaults to the first row.
    #[serde(default)]
    offset: usize,
    /// Rows in this page, capped at [`INTEGRITY_MAX_PAGE`]. Absent returns
    /// every row in one response.
    #[serde(default)]
    limit: Option<usize>,
}

/// One asset row and the object stored at its `storage_key`.
#[derive(Debug, Serialize)]
struct IntegrityAsset {
    asset_id: Uuid,
    slug: Option<String>,
    kind: Option<String>,
    sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    filename: Option<String>,
    /// Revision metadata for the lawyer gate's reconciliation.
    #[serde(skip_serializing_if = "Option::is_none")]
    revision: Option<RevisionSummary>,
    /// Structured transcript source revision. Returned to the lawyer gate only.
    #[serde(skip_serializing_if = "Option::is_none")]
    derived_from: Option<serde_json::Value>,
    /// `machine` or `proofread`, when the row is a transcript.
    #[serde(skip_serializing_if = "Option::is_none")]
    transcript_quality: Option<String>,
    /// Whether this asset is the current revision of its slug chain.
    operative: bool,
    /// One-based position in the slug chain, newest is the total revision count.
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<usize>,
    /// Whether the object at `storage_key` exists.
    exists: bool,
    /// Byte size of that object. Absent when the object is missing.
    #[serde(skip_serializing_if = "Option::is_none")]
    size_bytes: Option<i64>,
    /// `assets.byte_size`, the size the row records.
    recorded_size: i64,
    /// Present only when `deep=true`: whether the stored bytes hash to the
    /// row's sha256.
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256_matches: Option<bool>,
    /// Effective live placement. Present for the lawyer gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    matter_folder: Option<store::document_pointers::MatterFolder>,
    /// Only a validation result, never docket content. Present for the lawyer gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    docket_name_matches: Option<bool>,
    /// Derived link state for a Docket Filings document. Present for the lawyer gate.
    /// Never an entry number.
    #[serde(skip_serializing_if = "Option::is_none")]
    docket_link: Option<store::document_pointers::DocketLink>,
}

#[derive(Debug, Serialize)]
struct DocumentIntegrityResponse {
    assets: Vec<IntegrityAsset>,
    /// Rows the caller's lens can see across every page.
    total: usize,
    /// Where the next page starts. Absent on the last page.
    #[serde(skip_serializing_if = "Option::is_none")]
    next_offset: Option<usize>,
    /// Server-side external-id checks. Empty while a pointer carries no
    /// external id.
    integrations: Vec<crate::document_integrations::Finding>,
}

/// `GET /app/api/projects/{id}/documents/integrity` — every asset row the
/// caller's lens can see, plus whether the object at `storage_key` exists
/// and its stored size. `?deep=true` reads each object and compares its
/// sha256 to the row. `?offset=&limit=` pages the rows in slug-then-id order
/// so a large matter is checked in bounded requests; `total` and
/// `next_offset` say where the rest is. Lawyer-tier sessions and the CI
/// document scope.
async fn document_integrity_door(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
    Query(query): Query<IntegrityQuery>,
) -> Result<Response, ApiError> {
    let started = std::time::Instant::now();
    let lens = store::access::matter_lens(
        &state.application_store,
        session.person_id,
        session.role,
        id,
    )
    .await
    .map_err(ApiError::Db)?
    .ok_or(ApiError::NotFound)?;
    let mut assets = store::assets::for_project(&state.application_store, id)
        .await
        .map_err(ApiError::Asset)?;
    if matches!(lens, store::access::ProjectLens::Client) {
        assets.retain(|asset| asset.visibility == store::documents::visibility::CLIENT);
    }
    assets.sort_by(|left, right| {
        left.slug
            .cmp(&right.slug)
            .then_with(|| left.id.cmp(&right.id))
    });
    let total = assets.len();
    let start = query.offset.min(total);
    let end = query.limit.map_or(total, |limit| {
        start
            .saturating_add(limit.clamp(1, INTEGRITY_MAX_PAGE))
            .min(total)
    });
    let next_offset = (end < total).then_some(end);
    let page = &assets[start..end];

    let (current_by_slug, version_by_id) = integrity_chains(
        &state.application_store,
        id,
        page,
        matches!(lens, store::access::ProjectLens::Client),
    )
    .await?;
    let integrations = crate::document_integrations::findings(page);
    let storage = state.storage.as_ref();
    let mut rows = Vec::with_capacity(page.len());
    for chunk in page.chunks(INTEGRITY_CONCURRENCY) {
        let chunk_rows = futures::future::try_join_all(chunk.iter().map(|asset| {
            integrity_row(
                storage,
                asset,
                query.deep,
                matches!(lens, store::access::ProjectLens::Lawyer),
                &current_by_slug,
                &version_by_id,
            )
        }))
        .await
        .map_err(ApiError::Asset)?;
        rows.extend(chunk_rows);
    }
    if matches!(lens, store::access::ProjectLens::Lawyer) {
        populate_docket_name_checks(&state, id, &mut rows).await?;
    }
    let elapsed = started.elapsed();
    if elapsed >= INTEGRITY_SLOW_PAGE {
        tracing::warn!(
            project_id = %id,
            offset = start,
            rows = rows.len(),
            total,
            deep = query.deep,
            elapsed_ms = elapsed.as_millis(),
            "slow document integrity page"
        );
    } else {
        tracing::debug!(
            project_id = %id,
            offset = start,
            rows = rows.len(),
            total,
            deep = query.deep,
            elapsed_ms = elapsed.as_millis(),
            "document integrity page"
        );
    }
    Ok((
        StatusCode::OK,
        Json(DocumentIntegrityResponse {
            assets: rows,
            total,
            next_offset,
            integrations,
        }),
    )
        .into_response())
}

/// The current revision of each slug on `page`, and every revision's
/// one-based position in its chain. A client lens counts only the revisions
/// it can see.
async fn integrity_chains(
    application_store: &store::application_store::ApplicationStore,
    id: Uuid,
    page: &[store::assets::Asset],
    client_lens: bool,
) -> Result<
    (
        std::collections::BTreeMap<String, Option<Uuid>>,
        std::collections::BTreeMap<Uuid, usize>,
    ),
    ApiError,
> {
    let mut current_by_slug = std::collections::BTreeMap::new();
    let mut version_by_id = std::collections::BTreeMap::new();
    let slugs: Vec<String> = page
        .iter()
        .filter_map(|asset| asset.slug.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let revisions = store::assets::revisions_for_slugs(application_store, id, &slugs)
        .await
        .map_err(ApiError::Asset)?;
    let mut revisions_by_slug =
        std::collections::BTreeMap::<String, Vec<store::assets::Asset>>::new();
    for revision in revisions {
        if let Some(slug) = revision.slug.clone() {
            revisions_by_slug.entry(slug).or_default().push(revision);
        }
    }
    for slug in slugs {
        let mut revisions = revisions_by_slug.remove(&slug).unwrap_or_default();
        if client_lens {
            revisions.retain(|revision| {
                revision.visibility == store::documents::visibility::CLIENT
                    && revision.published_at.is_some()
            });
        }
        let count = revisions.len();
        for (index, revision) in revisions.iter().enumerate() {
            version_by_id.insert(revision.id, count - index);
        }
        current_by_slug.insert(slug, revisions.first().map(|revision| revision.id));
    }
    Ok((current_by_slug, version_by_id))
}

/// Populate only validation outcomes on the existing document metadata projection.
async fn populate_docket_name_checks(
    state: &ApiState,
    id: Uuid,
    rows: &mut [IntegrityAsset],
) -> Result<(), ApiError> {
    use store::document_pointers::MatterFolder;
    if !rows
        .iter()
        .any(|row| row.matter_folder == Some(MatterFolder::DocketFilings))
    {
        return Ok(());
    }
    let entries = store::cases::docket_for_project(&state.application_store, id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    for row in rows
        .iter_mut()
        .filter(|row| row.matter_folder == Some(MatterFolder::DocketFilings))
    {
        let linked: Vec<_> = entries
            .iter()
            .filter(|entry| entry.document_asset_id == Some(row.asset_id))
            .collect();
        let name_matches = |entry: &store::cases::DocketEntry| {
            row.slug.as_deref().is_some_and(|slug| {
                let path = std::path::Path::new(slug);
                let ext = path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .unwrap_or("pdf");
                store::cases::filing_name(&entry.entry_number, &entry.title, ext).is_ok_and(
                    |expected| {
                        path.file_name()
                            .is_some_and(|name| name == expected.as_str())
                    },
                )
            })
        };
        let (matches, link) = match linked.as_slice() {
            [entry] if name_matches(entry) => {
                (true, store::document_pointers::DocketLink::Verified)
            }
            [] => (false, store::document_pointers::DocketLink::Unresolved),
            _ => (false, store::document_pointers::DocketLink::Disagrees),
        };
        row.docket_name_matches = Some(matches);
        row.docket_link = Some(link);
    }
    Ok(())
}

/// The integrity row for one asset: its storage presence and size, and with
/// `deep` whether the stored bytes hash to the row's sha256.
async fn integrity_row(
    storage: &dyn cloud::StorageService,
    asset: &store::assets::Asset,
    deep: bool,
    lawyer: bool,
    current_by_slug: &std::collections::BTreeMap<String, Option<Uuid>>,
    version_by_id: &std::collections::BTreeMap<Uuid, usize>,
) -> Result<IntegrityAsset, store::assets::AssetError> {
    let head = store::assets::object_head(storage, &asset.storage_key).await?;
    let stored_size = head.and_then(|size| i64::try_from(size).ok());
    let sha256_matches = if deep {
        let digest = store::assets::object_sha256(storage, &asset.storage_key).await?;
        Some(digest.as_deref() == Some(asset.sha256_hex.as_str()))
    } else {
        None
    };
    let operative = asset
        .slug
        .as_deref()
        .and_then(|slug| current_by_slug.get(slug))
        .is_some_and(|current_id| *current_id == Some(asset.id));
    let version = version_by_id.get(&asset.id).copied();
    Ok(IntegrityAsset {
        asset_id: asset.id,
        slug: asset.slug.clone(),
        kind: asset.kind.clone(),
        sha256: asset.sha256_hex.clone(),
        filename: if lawyer { asset.filename.clone() } else { None },
        revision: version
            .filter(|_| lawyer)
            .map(|version| revision_summary(asset, version, operative)),
        derived_from: if lawyer {
            asset.derived_from.clone()
        } else {
            None
        },
        transcript_quality: if lawyer {
            asset.transcript_quality.clone()
        } else {
            None
        },
        operative,
        version,
        exists: head.is_some(),
        size_bytes: stored_size,
        recorded_size: asset.byte_size,
        sha256_matches,
        docket_link: None,
        matter_folder: lawyer.then(|| {
            use store::document_pointers::MatterFolder;
            asset
                .folder
                .as_deref()
                .and_then(MatterFolder::parse)
                .unwrap_or_else(|| {
                    MatterFolder::for_kind(asset.kind.as_deref().unwrap_or_default())
                })
        }),
        docket_name_matches: None,
    })
}

#[derive(Deserialize)]
struct UpdateDocumentRequest {
    #[serde(default)]
    visibility: Option<String>,
    /// Set on a row whose slug is null. Refused when the row already has a
    /// slug or the slug already names a revision chain on this matter.
    #[serde(default)]
    slug: Option<String>,
    /// Replaces `kind` together with `slug` while the row has no slug. Sent
    /// alone, it replaces a kind the asset lane rejects — a legacy free-text
    /// classification — on the row and its whole slug chain, and is refused
    /// when the row's kind is already accepted.
    #[serde(default)]
    kind: Option<String>,
    /// With `slug`, move the row's document identity (its whole chain) to the
    /// given `slug` instead of assigning it to a row that has none. Refused
    /// when `slug` equals the current slug, is not a valid identity for this
    /// row's filename, or already names a chain.
    #[serde(default)]
    reslug: bool,
    /// Set or clear the matter folder (LAW-241). Tri-state: the outer
    /// `Some` means the field was sent; the inner `None` is an explicit
    /// clear back to the kind's default placement, and the inner `Some`
    /// names the folder, spelled exactly. Sent alone; combined with
    /// `visibility`, `slug`, or `kind` is `400 invalid_request`.
    #[serde(default)]
    #[allow(clippy::option_option)]
    folder: Option<Option<String>>,
    /// Validate and report the write. The row is left unchanged.
    #[serde(default)]
    dry_run: bool,
}

#[derive(Deserialize, Default)]
struct RepairDocumentStorageRequest {
    #[serde(default)]
    dry_run: bool,
}

fn conflict(error: &str, message: &str) -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({ "error": error, "message": message })),
    )
        .into_response()
}

/// The `409` a conditional append answers when the chain moved past the
/// caller's expected head (LAW-284). The body names the live head's triple
/// so the client restarts against it without a second read; `current` is
/// `null` when the chain has no live head at all.
fn head_conflict_response(current: Option<&store::assets::ExpectedHead>) -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "error": "head_conflict",
            "current": current,
        })),
    )
        .into_response()
}

/// The one malformed shape an expected head can arrive in: version 0 names
/// no revision (a chain's head is one-based) and a digest that is not 64
/// lowercase hex characters can never match a stored one — both are the
/// caller's mistake, rejected before any read, mirroring the
/// `derived_from` validation.
fn invalid_expected_head_response(expected: &store::assets::ExpectedHead) -> Option<Response> {
    let well_formed = expected.version > 0
        && expected.sha256.len() == 64
        && expected
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    (!well_formed).then(|| {
        bad_request(
            "invalid_expected_head",
            "The expected head needs a positive version and a lowercase SHA-256 digest.",
        )
    })
}

fn assign_slug_response(error: store::assets::AssignSlugError) -> Result<Response, ApiError> {
    match error {
        store::assets::AssignSlugError::NotOnProject => Err(ApiError::NotFound),
        store::assets::AssignSlugError::AlreadySlugged => Ok(conflict(
            "slug_present",
            "This document already has a slug.",
        )),
        store::assets::AssignSlugError::ChainExists(slug) => Ok(conflict(
            "slug_taken",
            &format!("`{slug}` already names a document on this matter."),
        )),
        store::assets::AssignSlugError::MissingFilename => Ok(bad_request(
            "filename_required",
            "The document has no filename to check the slug against.",
        )),
        store::assets::AssignSlugError::InvalidSlug(error) => {
            Ok(bad_request("invalid_slug", &error.to_string()))
        }
        store::assets::AssignSlugError::InvalidKind(kind) => Ok(bad_request(
            "invalid_kind",
            &format!(
                "`{kind}` is not a document kind. Accepted values are: {}.",
                accepted_asset_kinds().join(", ")
            ),
        )),
        store::assets::AssignSlugError::Asset(error) => Err(ApiError::Asset(error)),
    }
}

// Mirrors `assign_slug_response`: the door handlers carry `ApiError`.
#[allow(clippy::result_large_err)]
fn reclassify_kind_response(
    error: store::assets::ReclassifyKindError,
) -> Result<Response, ApiError> {
    match error {
        store::assets::ReclassifyKindError::NotOnProject => Err(ApiError::NotFound),
        store::assets::ReclassifyKindError::KindAccepted(kind) => Ok(conflict(
            "kind_accepted",
            &format!("`{kind}` is already an accepted document kind; a valid kind is not changed."),
        )),
        store::assets::ReclassifyKindError::ChainHasAcceptedKind(slug) => Ok(conflict(
            "kind_accepted",
            &format!("Another revision of `{slug}` already carries an accepted kind."),
        )),
        store::assets::ReclassifyKindError::InvalidKind(kind) => Ok(bad_request(
            "invalid_kind",
            &format!(
                "`{kind}` is not a document kind. Accepted values are: {}.",
                accepted_asset_kinds().join(", ")
            ),
        )),
        store::assets::ReclassifyKindError::Asset(error) => Err(ApiError::Asset(error)),
    }
}

// Mirrors `assign_slug_response`: the door handlers carry `ApiError`.
#[allow(clippy::result_large_err)]
fn reslug_response(error: store::assets::ReslugError) -> Result<Response, ApiError> {
    match error {
        store::assets::ReslugError::NotOnProject => Err(ApiError::NotFound),
        store::assets::ReslugError::NoSlug => Ok(bad_request(
            "no_slug",
            "This row has no slug to move; use `slug` to set one.",
        )),
        store::assets::ReslugError::MissingFilename => Ok(bad_request(
            "filename_required",
            "The document has no filename to check the slug against.",
        )),
        store::assets::ReslugError::SlugUnchanged(slug) => Ok(conflict(
            "slug_unchanged",
            &format!("`{slug}` is already this document's identity."),
        )),
        store::assets::ReslugError::InvalidTarget(error) => {
            Ok(bad_request("invalid_slug", &error.to_string()))
        }
        store::assets::ReslugError::TargetOccupied(slug) => Ok(conflict(
            "slug_taken",
            &format!("`{slug}` already names a document on this matter."),
        )),
        store::assets::ReslugError::Asset(error) => Err(ApiError::Asset(error)),
    }
}

fn repair_storage_response(error: store::assets::RepairStorageError) -> Result<Response, ApiError> {
    match error {
        store::assets::RepairStorageError::NotOnProject => Err(ApiError::NotFound),
        store::assets::RepairStorageError::ObjectCorrupt => Ok(conflict(
            "object_corrupt",
            "The stored object is present and does not match the recorded sha256.",
        )),
        store::assets::RepairStorageError::NoSibling => Ok(conflict(
            "no_sibling",
            "No other document in this matter holds these bytes.",
        )),
        store::assets::RepairStorageError::DigestMismatch => Ok(conflict(
            "digest_mismatch",
            "The repaired bytes do not match the recorded sha256.",
        )),
        store::assets::RepairStorageError::Asset(error) => Err(ApiError::Asset(error)),
        store::assets::RepairStorageError::Project(error) => {
            tracing::error!(error = %error, "api document storage repair: project lookup failed");
            Err(ApiError::Db("the document could not be repaired".into()))
        }
    }
}

/// The `reslug` branch of [`update_document_door`], split out to keep that
/// function under the line-count lint: move the document identity to the
/// given `slug`.
async fn reslug_document_door(
    state: &ApiState,
    project_id: Uuid,
    asset_id: Uuid,
    input: &UpdateDocumentRequest,
) -> Result<Response, ApiError> {
    if input.visibility.is_some() || input.kind.is_some() {
        return Ok(bad_request(
            "invalid_request",
            "`reslug` cannot be combined with `visibility` or `kind`.",
        ));
    }
    let Some(new_slug) = input.slug.as_deref() else {
        return Ok(bad_request("invalid_request", "`reslug` requires `slug`."));
    };
    let previous_slug = match store::assets::find_by_id(&state.application_store, asset_id).await {
        Ok(Some(asset)) if asset.project_id == Some(project_id) => asset.slug,
        Ok(_) => None,
        Err(error) => return Err(ApiError::Asset(error)),
    };
    match store::assets::reslug(
        &state.application_store,
        project_id,
        asset_id,
        new_slug,
        input.dry_run,
    )
    .await
    {
        Ok(asset) => Ok(Json(serde_json::json!({
            "asset_id": asset.id,
            "slug": asset.slug,
            "previous_slug": previous_slug,
            "kind": asset.kind,
            "dry_run": input.dry_run,
        }))
        .into_response()),
        Err(error) => reslug_response(error),
    }
}

/// The `folder` branch of [`update_document_door`], split out to keep that
/// function under the line-count lint: set or clear the matter folder, the
/// same shape as the `reslug` branch beside it.
#[allow(clippy::result_large_err)]
async fn set_folder_door(
    state: &ApiState,
    project_id: Uuid,
    asset_id: Uuid,
    folder: Option<&str>,
) -> Result<Response, ApiError> {
    match store::assets::set_folder(&state.application_store, project_id, asset_id, folder).await {
        Ok(Some(changed)) => Ok(Json(serde_json::json!({ "changed": changed })).into_response()),
        Ok(None) => Err(ApiError::NotFound),
        Err(store::assets::SetFolderError::UnknownFolder(error)) => {
            Ok(bad_request("invalid_folder", &error.to_string()))
        }
        Err(store::assets::SetFolderError::Asset(error)) => Err(ApiError::Asset(error)),
    }
}

/// Reconcile a committed pointer's visibility, set `slug` (and optionally
/// `kind`) on a row that has no slug, replace a legacy `kind` the asset lane
/// rejects, or (with `reslug`) move a document identity to another valid slug. The standard API audit middleware
/// records the actor, scoped path, method, status, and request id for every
/// attempt. Lawyer or admin, and both the matter and asset are scoped
/// (out-of-scope → 404).
#[allow(clippy::too_many_lines)] // Keep the authorization checks beside the effect.
async fn update_document_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path((project_id, asset_id)): Path<(Uuid, Uuid)>,
    JsonOrForm(input): JsonOrForm<UpdateDocumentRequest>,
) -> Result<Response, ApiError> {
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        project_id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    if input.reslug {
        return reslug_document_door(&state, project_id, asset_id, &input).await;
    }
    if let Some(folder) = &input.folder {
        if input.visibility.is_some() || input.slug.is_some() || input.kind.is_some() {
            return Ok(bad_request(
                "invalid_request",
                "Send `folder` alone, not with `visibility`, `slug`, or `kind`.",
            ));
        }
        return set_folder_door(&state, project_id, asset_id, folder.as_deref()).await;
    }
    match (input.visibility.as_deref(), input.slug.as_deref()) {
        (Some(visibility), None) => {
            let visibility = visibility.trim();
            if !matches!(
                visibility,
                store::documents::visibility::INTERNAL | store::documents::visibility::CLIENT
            ) {
                return Ok(bad_request(
                    "invalid_visibility",
                    "visibility must be `internal` or `client`.",
                ));
            }
            let Some(changed) = store::assets::set_visibility(
                &state.application_store,
                project_id,
                asset_id,
                visibility,
            )
            .await
            .map_err(ApiError::Asset)?
            else {
                return Err(ApiError::NotFound);
            };
            Ok(Json(serde_json::json!({ "changed": changed })).into_response())
        }
        (None, Some(slug)) => {
            match store::assets::assign_slug(
                &state.application_store,
                project_id,
                asset_id,
                store::assets::AssignSlug {
                    slug,
                    kind: input.kind.as_deref(),
                    dry_run: input.dry_run,
                },
            )
            .await
            {
                Ok(asset) => Ok(Json(serde_json::json!({
                    "asset_id": asset.id,
                    "slug": asset.slug,
                    "kind": asset.kind,
                    "dry_run": input.dry_run,
                }))
                .into_response()),
                Err(error) => assign_slug_response(error),
            }
        }
        (Some(_), Some(_)) => Ok(bad_request(
            "invalid_request",
            "Send `visibility` or `slug`, not both.",
        )),
        (None, None) => match input.kind.as_deref() {
            Some(kind) => match store::assets::reclassify_kind(
                &state.application_store,
                project_id,
                asset_id,
                kind,
                input.dry_run,
            )
            .await
            {
                Ok(asset) => Ok(Json(serde_json::json!({
                    "asset_id": asset.id,
                    "slug": asset.slug,
                    "kind": asset.kind,
                    "dry_run": input.dry_run,
                }))
                .into_response()),
                Err(error) => reclassify_kind_response(error),
            },
            None => Ok(bad_request(
                "invalid_request",
                "Send `visibility`, `slug`, or `kind`.",
            )),
        },
    }
}

#[derive(Deserialize)]
struct ReclassifyDocumentRequest {
    /// The accepted asset-lane kind the chain moves to.
    kind: String,
    /// Validate and report the write. The row is left unchanged.
    #[serde(default)]
    dry_run: bool,
}

// Mirrors `assign_slug_response`: the door handlers carry `ApiError`.
#[allow(clippy::result_large_err)]
fn reclassify_accepted_kind_response(
    error: store::assets::ReclassifyAcceptedKindError,
) -> Result<Response, ApiError> {
    use store::assets::ReclassifyAcceptedKindError as E;
    match error {
        E::NotOnProject => Err(ApiError::NotFound),
        E::KindNotAccepted(kind) => Ok(conflict(
            "kind_not_accepted",
            &format!(
                "`{kind}` is not an accepted document kind; replace it with `navigator site document kind`."
            ),
        )),
        E::KindUnchanged(kind) => Ok(conflict(
            "kind_unchanged",
            &format!("The document is already filed as `{kind}`."),
        )),
        E::InvalidKind(kind) => Ok(bad_request(
            "invalid_kind",
            &format!(
                "`{kind}` is not a document kind. Accepted values are: {}.",
                accepted_asset_kinds().join(", ")
            ),
        )),
        E::Asset(error) => Err(ApiError::Asset(error)),
    }
}

/// Change a document chain from one accepted kind to another: the correction
/// for a document filed under the wrong valid kind. Every revision of the
/// slug chain moves together; `sha256`, the storage key, byte size, and the
/// slug stay unchanged. A row whose kind is not accepted belongs to the
/// legacy-kind door (`PATCH .../documents/{asset_id}` with `kind`). The
/// standard API audit middleware records the actor, scoped path, method,
/// status, and request id for every attempt. Lawyer or admin, and both the
/// matter and asset are scoped (out-of-scope → 404).
async fn reclassify_document_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path((project_id, asset_id)): Path<(Uuid, Uuid)>,
    JsonOrForm(input): JsonOrForm<ReclassifyDocumentRequest>,
) -> Result<Response, ApiError> {
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        project_id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    match store::assets::reclassify_accepted_kind(
        &state.application_store,
        project_id,
        asset_id,
        &input.kind,
        input.dry_run,
    )
    .await
    {
        Ok(asset) => Ok(Json(serde_json::json!({
            "asset_id": asset.id,
            "slug": asset.slug,
            "kind": asset.kind,
            "dry_run": input.dry_run,
        }))
        .into_response()),
        Err(error) => reclassify_accepted_kind_response(error),
    }
}

#[derive(Deserialize)]
struct VerifyDropboxSignRequest {
    request_id: String,
    sha256: String,
    account_email: String,
    client_email: String,
    signed_on: chrono::NaiveDate,
    #[serde(default)]
    apply: bool,
}

async fn verify_dropbox_sign_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path((project_id, asset_id)): Path<(Uuid, Uuid)>,
    JsonOrForm(input): JsonOrForm<VerifyDropboxSignRequest>,
) -> Result<Response, ApiError> {
    let db = &state.application_store;
    if !participates_as_firm(&state, &lawyer, project_id).await {
        return Err(ApiError::NotFound);
    }
    let actor = lawyer.0.person_id.ok_or(ApiError::NotFound)?;
    let asset = store::assets::find_by_id(db, asset_id)
        .await
        .map_err(ApiError::Asset)?
        .filter(|asset| asset.project_id == Some(project_id) && asset.deleted_at.is_none())
        .ok_or(ApiError::NotFound)?;
    if asset.kind.as_deref() != Some("onboarding") || asset.content_type != "application/pdf" {
        return Err(ApiError::EnvelopeAdoption(
            "File the signed PDF as Onboarding before verification.",
        ));
    }
    if !store::onboarding::valid_dropbox_sign_request_id(&input.request_id) {
        return Ok(bad_request(
            "invalid_request_id",
            "Use the 40-character lowercase hexadecimal Dropbox Sign request ID.",
        ));
    }
    if input.sha256 != asset.sha256_hex {
        return Ok(conflict(
            "document_hash_mismatch",
            "The selected revision does not match the SHA-256 of the reviewed export.",
        ));
    }
    if input.signed_on > chrono::Utc::now().date_naive() {
        return Ok(bad_request(
            "invalid_signed_on",
            "Use the completion date shown on the audit trail.",
        ));
    }
    let account_email = input.account_email.trim();
    if !account_email
        .split_once('@')
        .is_some_and(|(local, domain)| {
            !local.is_empty() && !domain.is_empty() && !domain.contains('@')
        })
        || account_email.chars().any(char::is_whitespace)
    {
        return Ok(bad_request(
            "invalid_account_email",
            "Use the requesting Firm account email from the audit trail.",
        ));
    }
    let project = store::projects::find_by_id(db, project_id)
        .await
        .map_err(crate::signature_adoption::internal)?
        .ok_or(ApiError::NotFound)?;
    let firm_id = project.firm_id.ok_or(ApiError::EnvelopeAdoption(
        "The Project has no owning Firm.",
    ))?;
    let participants = store::projects::participations_for_project(db, project_id)
        .await
        .map_err(crate::signature_adoption::internal)?;
    let client_id = project_client_dri(project.client_dri_person_id, &participants).ok_or(
        ApiError::EnvelopeAdoption(
            "Designate the Project's client before verifying the signed retainer.",
        ),
    )?;
    let client = store::persons::find_by_id(db, client_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !client.email.eq_ignore_ascii_case(input.client_email.trim()) {
        return Err(ApiError::EnvelopeAdoption(
            "The audited signer email must match the Project's client.",
        ));
    }
    let pg = store::postgres::PostgresDb::application()
        .await
        .map_err(crate::signature_adoption::internal)?;
    let evidence = store::onboarding::VerifiedDocument {
        asset_id,
        project_id,
        firm_id,
        client_id,
        client_email: &client.email,
        sha256: &input.sha256,
        request_id: &input.request_id,
        account_email: &account_email.to_ascii_lowercase(),
        signed_on: input.signed_on,
        actor,
    };
    if !store::onboarding::record_verified_document(&pg, &evidence, !input.apply)
        .await
        .map_err(crate::signature_adoption::internal)?
    {
        return Ok(conflict("verification_conflict", "The Project is closed, the revision or its signing details changed, or this request is already recorded."));
    }
    Ok(Json(serde_json::json!({ "asset_id": asset_id, "signed_on": input.signed_on, "applied": input.apply })).into_response())
}

/// Request body for `POST .../documents/{asset_id}/delete`.
#[derive(Deserialize)]
struct DeleteDocumentRequest {
    /// The source-repository commit whose pointer removal asked for the
    /// deletion, recorded on the row.
    #[serde(default)]
    commit_sha: Option<String>,
    /// Retire a mistaken chain that has no repository pointer-removal commit.
    #[serde(default)]
    manual: bool,
}

/// A commit id as Git prints it: 7 to 64 lowercase hexadecimal digits.
fn is_commit_sha(value: &str) -> bool {
    (7..=64).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Soft-delete a document: every revision of its chain is stamped with the
/// deleting person, an optional source commit, and a purge date 180 days out. The
/// document drops out of the matter page and the default lists; a lawyer
/// can still read it and restore it. A `closed_repository` archive is `409`.
/// Lawyer or admin, matter-scoped (out-of-scope → 404). The standard API
/// audit records the actor and request, and the row records the person and
/// commit.
async fn delete_document_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path((project_id, asset_id)): Path<(Uuid, Uuid)>,
    JsonOrForm(input): JsonOrForm<DeleteDocumentRequest>,
) -> Result<Response, ApiError> {
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        project_id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    let deleted_by = lawyer.0.person_id.ok_or(ApiError::Forbidden)?;
    let commit_sha = input.commit_sha.as_deref().map(str::trim);
    if input.manual == commit_sha.is_some()
        || commit_sha.is_some_and(|commit_sha| !is_commit_sha(commit_sha))
    {
        return Ok(bad_request(
            "invalid_delete_source",
            "provide either the 7 to 64 hexadecimal digit pointer-removal commit_sha or manual: true.",
        ));
    }
    let started = std::time::Instant::now();
    tracing::info!(%project_id, %asset_id, "document delete started");
    match store::assets::soft_delete(
        &state.application_store,
        project_id,
        asset_id,
        deleted_by,
        commit_sha,
        chrono::Utc::now(),
    )
    .await
    {
        Ok(deletion) => {
            tracing::info!(
                %project_id,
                %asset_id,
                elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                changed = deletion.changed,
                revisions = deletion.revisions,
                "document delete finished"
            );
            Ok(Json(deletion).into_response())
        }
        Err(store::assets::SoftDeleteError::NotOnProject) => Err(ApiError::NotFound),
        Err(store::assets::SoftDeleteError::ClosedRepository) => Ok(conflict(
            "closed_repository_protected",
            "A closed_repository archive is never soft-deleted.",
        )),
        Err(store::assets::SoftDeleteError::Asset(error)) => Err(ApiError::Asset(error)),
    }
}

/// Restore a soft-deleted document with its whole revision history. A chain the
/// purge already expunged is `409 purged`. Lawyer or admin, matter-scoped.
async fn restore_document_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path((project_id, asset_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        project_id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    let pg = store::postgres::PostgresDb::from_env().await.ok();
    let archive = cloud::archived_projects_from_env().await.ok();
    match store::document_retention::restore_chain(
        &state.application_store,
        pg.as_ref(),
        &state.storage,
        archive.as_ref(),
        project_id,
        asset_id,
        chrono::Utc::now(),
    )
    .await
    {
        Ok(restoration) => {
            tracing::warn!(
                %project_id,
                %asset_id,
                restored_by = ?lawyer.0.person_id,
                changed = restoration.changed,
                "soft-deleted document restored"
            );
            Ok(Json(restoration).into_response())
        }
        Err(store::document_retention::RetentionError::Restore(
            store::assets::RestoreError::NotOnProject,
        )) => Err(ApiError::NotFound),
        Err(store::document_retention::RetentionError::Restore(
            store::assets::RestoreError::Purged,
        )) => Ok(conflict(
            "purged",
            "The document was purged after its retention window and cannot be restored.",
        )),
        Err(store::document_retention::RetentionError::Restore(
            store::assets::RestoreError::Asset(error),
        )) => Err(ApiError::Asset(error)),
        Err(error) => {
            tracing::error!(%error, %project_id, %asset_id, "document restore stopped");
            Ok(conflict(
                "archive_unavailable",
                "The document's archived bytes could not be restored; nothing was changed.",
            ))
        }
    }
}

#[derive(Deserialize)]
struct HoldDocumentRequest {
    /// `true` places a Legal Hold on the document's chain; `false` releases it.
    held: bool,
}

/// Place or release a Legal Hold on a document's whole chain. A held chain is
/// never purged. Lawyer or admin, matter-scoped.
async fn hold_document_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path((project_id, asset_id)): Path<(Uuid, Uuid)>,
    JsonOrForm(input): JsonOrForm<HoldDocumentRequest>,
) -> Result<Response, ApiError> {
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        lawyer.0.person_id,
        lawyer.0.role,
        project_id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    let Some(revisions) = store::assets::set_legal_hold(
        &state.application_store,
        project_id,
        asset_id,
        input.held,
        chrono::Utc::now(),
    )
    .await
    .map_err(ApiError::Asset)?
    else {
        return Err(ApiError::NotFound);
    };
    tracing::warn!(
        %project_id,
        %asset_id,
        held = input.held,
        set_by = ?lawyer.0.person_id,
        "document legal hold changed"
    );
    Ok(Json(serde_json::json!({ "held": input.held, "revisions": revisions })).into_response())
}

/// Expunge every soft-deleted document whose retention window has closed,
/// through the governed-expunge primitive, skipping a chain under a Legal Hold
/// and a matter that is closed but not yet archived. Admin only; the calling
/// admin is the expunge's authorizer. Safe to call repeatedly: a purged chain
/// is never touched again.
async fn purge_documents_door(
    State(state): State<ApiState>,
    admin: AdminSession,
) -> Result<Response, ApiError> {
    let authorized_by = admin.0.person_id.ok_or(ApiError::Forbidden)?;
    // With the retention lanes configured, the purge records its runs and
    // removes archived copies too; without them it purges active copies only.
    let pg = store::postgres::PostgresDb::from_env().await.ok();
    let archive = cloud::archived_projects_from_env().await.ok();
    let report = crate::expunge::purge::purge_expired_documents_with(
        &state.application_store,
        &state.storage,
        crate::expunge::purge::RetentionLanes {
            pg: pg.as_ref(),
            archive: archive.as_ref(),
        },
        &crate::expunge::purge::PurgeAuthority::Admin(authorized_by),
        chrono::Utc::now(),
    )
    .await
    .map_err(|error| {
        tracing::error!(%error, "document purge failed");
        ApiError::Db("the purge could not run".into())
    })?;
    Ok(Json(report).into_response())
}

/// Copy a missing storage object from a same-hash sibling in the matter, or
/// re-point the row at the content-addressed key that already holds it.
/// Admin only. The API audit records the actor and operation. Out of scope
/// is 404. No sibling leaves the row unchanged.
async fn repair_document_storage_door(
    State(state): State<ApiState>,
    admin: AdminSession,
    Path((project_id, asset_id)): Path<(Uuid, Uuid)>,
    JsonOrForm(input): JsonOrForm<RepairDocumentStorageRequest>,
) -> Result<Response, ApiError> {
    let in_scope = store::access::can_see_project_as_lawyer(
        &state.application_store,
        admin.0.person_id,
        admin.0.role,
        project_id,
    )
    .await
    .unwrap_or(false);
    if !in_scope {
        return Err(ApiError::NotFound);
    }
    match store::assets::repair_missing_object(
        &state.application_store,
        state.storage.as_ref(),
        project_id,
        asset_id,
        input.dry_run,
    )
    .await
    {
        Ok(repaired) => Ok(Json(serde_json::json!({
            "storage_key": repaired.storage_key,
            "copied": repaired.copied,
            "dry_run": repaired.dry_run,
        }))
        .into_response()),
        Err(error) => repair_storage_response(error),
    }
}

/// Map a contract-review action outcome onto an API response: `204` on success,
/// `404` for a missing/out-of-scope review or finding, `409` off-gate, `422`
/// when approval is blocked by unacted findings, `500` on a store/runtime fault.
// `ApiError` is the pervasive Err type across this whole surface; the async
// door handlers carry it without the lint, and this sync helper mirrors them.
#[allow(clippy::result_large_err)]
fn review_action_response(
    result: Result<(), crate::admin_contract_reviews::ReviewActionError>,
    review_id: Uuid,
) -> Result<Response, ApiError> {
    use crate::admin_contract_reviews::ReviewActionError as E;
    match result {
        Ok(()) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(E::NotFoundOrScoped | E::FindingNotFound) => Err(ApiError::NotFound),
        Err(E::NotOpen) => Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "not_open",
                "message": "This contract review is not at the gate this action requires."
            })),
        )
            .into_response()),
        Err(E::FindingsUnacted) => Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": "findings_unacted",
                "message": "Every finding must be accepted or rejected before the memo can be approved."
            })),
        )
            .into_response()),
        Err(E::DocumentCommentsUnacted) => Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": "document_comments_unacted",
                "message": "Every document comment must be accepted, rejected, or edited before the memo can be approved."
            })),
        )
            .into_response()),
        Err(e @ E::Db(_)) => {
            tracing::error!(error = %e, %review_id, "api contract-review action failed");
            Err(ApiError::Db("the contract-review action failed".into()))
        }
    }
}

/// Request body for `POST /app/api/contract-reviews/{id}/findings/{idx}` — the
/// attorney's decision and edits on one finding.
#[derive(Deserialize)]
struct SaveReviewFindingRequest {
    /// Accept (`true`) or reject (`false`) the finding for delivery.
    #[serde(default)]
    accept: bool,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    suggested_redline: String,
    #[serde(default)]
    attorney_note: String,
}

/// `POST /app/api/contract-reviews/{id}/findings/{idx}` — save the attorney's
/// edits and accept/reject decision on one finding, the REST mirror of the
/// review surface. Both converge on `admin_contract_reviews::save_review_finding`.
/// Lawyer-tier only and matter-scoped (out-of-scope → 404). `204` on success; a
/// closed review is `409`; an out-of-range finding index is `404`.
async fn save_review_finding_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path((review_id, idx)): Path<(Uuid, usize)>,
    JsonOrForm(input): JsonOrForm<SaveReviewFindingRequest>,
) -> Result<Response, ApiError> {
    let result = crate::admin_contract_reviews::save_review_finding(
        &state.application_store,
        review_id,
        idx,
        lawyer.0.person_id,
        lawyer.0.role,
        input.accept,
        &input.severity,
        &input.suggested_redline,
        &input.attorney_note,
    )
    .await;
    review_action_response(result, review_id)
}

/// Request body for `POST /app/api/contract-reviews/{id}/summary`.
#[derive(Deserialize)]
struct SaveReviewSummaryRequest {
    #[serde(default)]
    risk_summary: String,
}

/// `POST /app/api/contract-reviews/{id}/summary` — edit a review's risk summary,
/// the REST mirror of the review surface. Converges on
/// `admin_contract_reviews::save_review_summary`. Lawyer-tier only and
/// matter-scoped. `204` on success; out of scope is `404`.
async fn save_review_summary_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(review_id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<SaveReviewSummaryRequest>,
) -> Result<Response, ApiError> {
    let result = crate::admin_contract_reviews::save_review_summary(
        &state.application_store,
        review_id,
        lawyer.0.person_id,
        lawyer.0.role,
        &input.risk_summary,
    )
    .await;
    review_action_response(result, review_id)
}

/// `POST /app/api/contract-reviews/{id}/approve` — assemble and deliver the
/// review memo and approve, the REST mirror of the review surface. Converges on
/// `admin_contract_reviews::approve_review`. Lawyer-tier only and matter-scoped.
/// `204` on success; not at the review gate is `409`; approving before every
/// finding is acted on is `422`.
async fn approve_review_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(review_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let result = crate::admin_contract_reviews::approve_review(
        &state.application_store,
        &state.storage,
        state.workflow_runtime.as_ref(),
        review_id,
        lawyer.0.person_id,
        lawyer.0.role,
    )
    .await;
    review_action_response(result, review_id)
}

/// `POST /app/api/contract-reviews/{id}/reject` — reject a review without a memo,
/// the REST mirror of the review surface. Converges on
/// `admin_contract_reviews::reject_review`. Lawyer-tier only and matter-scoped.
/// `204` on success; not at the review gate is `409`.
async fn reject_review_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(review_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let result = crate::admin_contract_reviews::reject_review(
        &state.application_store,
        state.workflow_runtime.as_ref(),
        review_id,
        lawyer.0.person_id,
        lawyer.0.role,
    )
    .await;
    review_action_response(result, review_id)
}

/// Request body for `POST /app/api/playbooks` — a client Entity's negotiating
/// positions. Unlike the lawyer form (a pipe-delimited textarea), the REST door
/// takes the positions as structured objects.
#[derive(Deserialize)]
struct CreatePlaybookRequest {
    entity_id: Uuid,
    name: String,
    #[serde(default)]
    positions: Vec<store::playbooks::Position>,
}

/// `POST /app/api/playbooks` — create a contract-review playbook for a Company,
/// the REST mirror of the lawyer playbook form. Both converge on
/// `store::playbooks::create`. Lawyer-tier only. `201` with the new id; a blank
/// name or empty position set is `400`; a duplicate name on that Company is
/// `409`.
async fn create_playbook_door(
    State(state): State<ApiState>,
    _lawyer: LawyerSession,
    JsonOrForm(input): JsonOrForm<CreatePlaybookRequest>,
) -> Result<Response, ApiError> {
    if input.name.trim().is_empty() {
        return Ok(bad_request(
            "playbook_name_required",
            "A playbook name is required.",
        ));
    }
    if input.positions.is_empty() {
        return Ok(bad_request(
            "positions_required",
            "Enter at least one negotiating position.",
        ));
    }
    match store::playbooks::create(
        &state.application_store,
        &store::playbooks::NewPlaybook {
            entity_id: input.entity_id,
            name: input.name.trim(),
            positions: &input.positions,
        },
    )
    .await
    {
        Ok(id) => Ok((
            StatusCode::CREATED,
            Json(serde_json::json!({ "playbook_id": id })),
        )
            .into_response()),
        Err(store::playbooks::PlaybookError::DuplicateName(_)) => Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "duplicate_name",
                "message": "That Company already has a playbook with that name."
            })),
        )
            .into_response()),
        Err(e) => {
            tracing::error!(error = %e, "api create playbook: failed");
            Err(ApiError::Db("the playbook could not be created".into()))
        }
    }
}

/// Request body for `PUT /app/api/playbooks/{id}` — the replacement position set.
#[derive(Deserialize)]
struct UpdatePlaybookRequest {
    #[serde(default)]
    positions: Vec<store::playbooks::Position>,
}

/// `PUT /app/api/playbooks/{id}` — replace a playbook's position set, the REST
/// mirror of the lawyer edit form. Both converge on
/// `store::playbooks::update_positions`. Lawyer-tier only. `204` on success; an
/// unknown playbook is `404`; an empty position set is `400`.
async fn update_playbook_door(
    State(state): State<ApiState>,
    _lawyer: LawyerSession,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<UpdatePlaybookRequest>,
) -> Result<Response, ApiError> {
    if store::playbooks::by_id(&state.application_store, id)
        .await
        .ok()
        .flatten()
        .is_none()
    {
        return Err(ApiError::NotFound);
    }
    if input.positions.is_empty() {
        return Ok(bad_request(
            "positions_required",
            "Enter at least one negotiating position.",
        ));
    }
    match store::playbooks::update_positions(&state.application_store, id, &input.positions).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(e) => {
            tracing::error!(error = %e, %id, "api update playbook: failed");
            Err(ApiError::Db(
                "the playbook positions could not be saved".into(),
            ))
        }
    }
}

/// A `400 Bad Request` JSON body in the shared `{error, message}` shape.
pub(crate) fn bad_request(error: &str, message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": error, "message": message })),
    )
        .into_response()
}

/// `POST /app/api/expunge-requests/{id}/authorize` — an admin authorizes a
/// pending client expunge request, running the governed expunge. Admin-tier only
/// (owner/admin): embedded Rego admits admin, and the extractor re-checks the
/// tier. `204` on success; a request already resolved is `409`; an unknown
/// request or its vanished document is `404`. Converges on
/// `expunge_request_route::authorize_expunge_request`.
async fn authorize_expunge_door(
    State(state): State<ApiState>,
    authed: AuthedSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    if !authed.0.role.is_admin_tier() {
        return Err(ApiError::Forbidden);
    }
    let authorizer = authed.0.person_id.ok_or(ApiError::Forbidden)?;
    match crate::expunge_request_route::authorize_expunge_request(
        &state.application_store,
        &state.storage,
        id,
        authorizer,
    )
    .await
    {
        Ok(()) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(crate::expunge_request_route::ExpungeRequestActionError::NotFound) => {
            Err(ApiError::NotFound)
        }
        Err(crate::expunge_request_route::ExpungeRequestActionError::AlreadyResolved) => Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "already_resolved",
                "message": "This expunge request has already been resolved."
            })),
        )
            .into_response()),
        Err(e) => {
            tracing::error!(error = %e, request_id = %id, "api authorize expunge: failed");
            Err(ApiError::Db(
                "the expunge request could not be authorized".into(),
            ))
        }
    }
}

/// `POST /app/api/expunge-requests/{id}/deny` — a lawyer or admin denies a
/// pending client expunge request without deleting anything. Lawyer-tier only.
/// `204` on success; an unknown or already-resolved request is `404`. Converges
/// on `expunge_request_route::deny_expunge_request`.
async fn deny_expunge_door(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let resolver = lawyer.0.person_id.ok_or(ApiError::Forbidden)?;
    match crate::expunge_request_route::deny_expunge_request(&state.application_store, id, resolver)
        .await
    {
        Ok(()) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(crate::expunge_request_route::ExpungeRequestActionError::NotFound) => {
            Err(ApiError::NotFound)
        }
        Err(e) => {
            tracing::error!(error = %e, request_id = %id, "api deny expunge: failed");
            Err(ApiError::Db(
                "the expunge request could not be denied".into(),
            ))
        }
    }
}

/// Lint markdown without persisting it. Mirrors the `cli validate`
/// rule-set selection so a notation that passes the CLI passes here
/// and vice versa. Requires an authenticated session — same posture
/// as the rest of `/app/api/*`.
async fn validate_template(Json(req): Json<ValidateRequest>) -> Json<ValidateResponse> {
    let path = req.path.unwrap_or_else(|| "template.md".to_string());
    let rule_set = if req.markdown_only {
        rules::navigator_markdown_only_rules()
    } else {
        rules::navigator_default_rules()
    };
    let file = rules::SourceFile {
        path: std::path::PathBuf::from(&path),
        contents: req.contents,
    };
    let mut violations = Vec::new();
    for rule in &rule_set {
        for v in rule.lint(&file) {
            violations.push(ValidationViolation {
                code: v.code,
                line: v.line,
                message: v.message,
            });
        }
    }
    // `clean` means no *blocking* (Error-severity) violations. Yellow
    // advisories like N112 ("step allowed but not built yet") are still
    // returned in `violations` so the caller sees them, but they don't
    // flip `clean` to false — mirroring `navigator project gate`.
    let clean = !violations
        .iter()
        .any(|v| rules::severity_for_code(v.code) == rules::Severity::Error);
    Json(ValidateResponse {
        path,
        clean,
        violations,
    })
}

#[derive(Debug)]
pub enum ApiError {
    Unauthenticated,
    Forbidden,
    NotFound,
    Command(crate::people_commands::PeopleCommandError),
    /// A read or write against the person directory failed.
    Person(store::persons::PersonError),
    /// A read against the jurisdiction reference table failed.
    Jurisdictions(store::jurisdictions::JurisdictionError),
    EntityTypes(store::entity_types::EntityTypeError),
    Entity(store::entity_commands::EntityCommandError),
    Project(store::projects::ProjectCommandError),
    PitchArtifactConflict,
    EnvelopeAdoption(&'static str),
    OpenMatter(store::projects::OpenMatterError),
    AddParticipant(store::participation::AddParticipantError),
    UpdateParticipant(store::participation::UpdateParticipantError),
    RemoveParticipant(store::participation::RemoveParticipantError),
    /// Recording adversity from a matter failed.
    Adversity(store::project_references::AdversityError),
    CreateNotation(crate::project_notation::CreateProjectNotationError),
    AnswerNotation(workflows::NotationSessionError),
    SendIntake(crate::retainer_walk::SendIntakeError),
    /// A retainer workflow drive (approve or send-for-signature) failed.
    NotationWorkflow(crate::retainer_walk::WorkflowDriveError),
    ReviewComment(crate::review::ReviewCommentError),
    RequestDeletion(crate::expunge_request_route::RequestDeletionError),
    ContractReview(crate::contract_review_walk::ContractReviewError),
    Db(String),
    PrivateTermsConflict(&'static str),
    PrivateTermsInvalid(&'static str),
    /// A read or write against the Surreal-backed `notations` store failed.
    Notation(store::notations::NotationError),
    /// Filing a document into a matter failed. Carried as the store's own
    /// error rather than a string so the door can tell a caller's bad `kind`
    /// (a 400) from a database or storage fault (a 500) by matching the
    /// variant — never by matching the message text.
    Ingest(store::documents::IngestError),
    Revision(store::assets::RevisionError),
    Asset(store::assets::AssetError),
    /// A streaming-upload step failed on the store side. The client-facing
    /// states — a missing or expired session, a chunk that is not the one
    /// the offset names, an early completion, a digest mismatch — never land
    /// here; the doors answer those directly.
    UploadSession(store::documents::UploadSessionError),
    Testimonial(store::testimonials::TestimonialError),
    /// A [`JsonOrForm`] body was well-formed for its content type but failed
    /// deserialization — a missing required field, or (with a command's own
    /// `#[serde(deny_unknown_fields)]`) an unrecognized one. Carries the
    /// extractor's own rejection text, which names the field, so
    /// `PATCH /app/api/entities/{id}` with `{"jurisdiction_id":…}` reports
    /// what is missing rather than the opaque "Malformed JSON body." every
    /// deserialize failure used to share (ENG-518).
    MalformedBody(String),
    /// A Deadline write the caller can correct: a missing citation, a due
    /// instant with no offset, or a lead time that is out of order.
    DeadlineInvalid(String),
    /// The proposal or its evidence changed after the lawyer opened it.
    CourtDateConflict,
    CourtDateSourceUnavailable,
    /// An E-Filing Transaction report the caller can correct: an unknown
    /// status, a malformed date, or a filed date on a transaction that is
    /// not accepted.
    EfilingInvalid(String),
    /// A Docket Entry write the caller can correct: an unknown kind, a
    /// hearing without a date, a date on a filing, or a document that is
    /// not this Project's.
    DocketInvalid(String),
    /// A Service write the caller can correct: an unknown method, a
    /// non-date, a blank name, two recipient links, or a document that is
    /// not this Project's.
    ServiceInvalid(String),
    /// A Communication the caller can correct: an unknown channel,
    /// direction, role, or consent value, a non-RFC 3339 time, or an empty
    /// body.
    CommunicationInvalid(String),
    /// A `pages:` set the gate refuses: a `Y021` shape problem or a D-family
    /// composition finding, one per `;`.
    PagesInvalid(String),
    /// A Project surface reconcile could not run because the Project's Firm
    /// is missing a Forgejo coordinate — the message names each piece. An
    /// expected outcome of a fleet-wide backfill sweep, reported per Project
    /// rather than a bare internal fault.
    ForgejoUnavailable(String),
    ProjectMirror(MirrorIssue),
    /// The Project's Firm has no DocuSign connection this deployment can
    /// use. The message names the failed check, never a credential byte;
    /// a Firm administrator fixes it by saving the connection.
    SignatureUnavailable(crate::signature::SignatureError),
    /// A collection request the caller can correct: an unadvertised filter
    /// or sort field, a `page[size]` out of range, or a stale cursor.
    ListInvalid(store::listing::ListError),
}

impl From<MirrorIssue> for ApiError {
    fn from(issue: MirrorIssue) -> Self {
        Self::ProjectMirror(issue)
    }
}

impl From<store::persons::PersonError> for ApiError {
    fn from(e: store::persons::PersonError) -> Self {
        Self::Person(e)
    }
}

impl From<store::testimonials::TestimonialError> for ApiError {
    fn from(e: store::testimonials::TestimonialError) -> Self {
        Self::Testimonial(e)
    }
}

impl From<store::entities::EntityError> for ApiError {
    fn from(e: store::entities::EntityError) -> Self {
        // Routed through the command error so one match arm renders every
        // entity failure — the read doors and the write doors agree.
        Self::Entity(store::entity_commands::EntityCommandError::from(e))
    }
}

impl From<store::jurisdictions::JurisdictionError> for ApiError {
    fn from(e: store::jurisdictions::JurisdictionError) -> Self {
        Self::Jurisdictions(e)
    }
}

impl From<store::entity_types::EntityTypeError> for ApiError {
    fn from(e: store::entity_types::EntityTypeError) -> Self {
        Self::EntityTypes(e)
    }
}

impl From<store::participation::AddParticipantError> for ApiError {
    fn from(e: store::participation::AddParticipantError) -> Self {
        Self::AddParticipant(e)
    }
}

impl From<store::participation::UpdateParticipantError> for ApiError {
    fn from(e: store::participation::UpdateParticipantError) -> Self {
        Self::UpdateParticipant(e)
    }
}

impl From<store::participation::RemoveParticipantError> for ApiError {
    fn from(e: store::participation::RemoveParticipantError) -> Self {
        Self::RemoveParticipant(e)
    }
}

impl From<store::project_references::AdversityError> for ApiError {
    fn from(e: store::project_references::AdversityError) -> Self {
        Self::Adversity(e)
    }
}

impl From<crate::project_notation::CreateProjectNotationError> for ApiError {
    fn from(e: crate::project_notation::CreateProjectNotationError) -> Self {
        Self::CreateNotation(e)
    }
}

impl From<workflows::NotationSessionError> for ApiError {
    fn from(e: workflows::NotationSessionError) -> Self {
        Self::AnswerNotation(e)
    }
}

impl From<crate::retainer_walk::SendIntakeError> for ApiError {
    fn from(e: crate::retainer_walk::SendIntakeError) -> Self {
        Self::SendIntake(e)
    }
}

impl From<crate::retainer_walk::WorkflowDriveError> for ApiError {
    fn from(e: crate::retainer_walk::WorkflowDriveError) -> Self {
        Self::NotationWorkflow(e)
    }
}

impl From<crate::review::ReviewCommentError> for ApiError {
    fn from(e: crate::review::ReviewCommentError) -> Self {
        Self::ReviewComment(e)
    }
}

impl From<crate::expunge_request_route::RequestDeletionError> for ApiError {
    fn from(e: crate::expunge_request_route::RequestDeletionError) -> Self {
        Self::RequestDeletion(e)
    }
}

impl From<crate::contract_review_walk::ContractReviewError> for ApiError {
    fn from(e: crate::contract_review_walk::ContractReviewError) -> Self {
        Self::ContractReview(e)
    }
}

pub(crate) fn signature_workflow_response(error: &crate::signature::SignatureError) -> Response {
    use crate::signature::SignatureError as S;
    let (status, code, message) = match error {
        S::AnchorValidation(_) | S::TabValidation(_) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "signature_fields",
            "Check the signing fields in the rendered PDF and their assigned signers.",
        ),
        S::Connection(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "signature_connection",
            "Check the owning Firm’s DocuSign connection and account configuration.",
        ),
        S::Authentication(reason) => (StatusCode::BAD_GATEWAY, "signature_authentication", reason.as_str()),
        S::Unreachable => (StatusCode::BAD_GATEWAY, "signature_unreachable", "Navigator could not reach DocuSign."),
        S::FirmSigner => (StatusCode::UNPROCESSABLE_ENTITY, "firm_signer", "The Firm signer must match the Project’s lawyer DRI or the configured signing lawyer when no DRI is assigned."),
        S::OnboardingMessage => (StatusCode::UNPROCESSABLE_ENTITY, "onboarding_message", "The onboarding draft does not have the required client message."),
        S::Provider(_) => (
            StatusCode::BAD_GATEWAY,
            "signature_provider",
            "The signature provider could not complete the envelope operation. Check its status before retrying.",
        ),
    };
    (
        status,
        Json(serde_json::json!({"error": code, "message": message})),
    )
        .into_response()
}

/// Map the approval and signature workflow failures to their HTTP status and public diagnosis.
fn workflow_drive_error(e: crate::retainer_walk::WorkflowDriveError) -> Response {
    use crate::retainer_walk::WorkflowDriveError as E;
    match e {
        E::Signature(error) => signature_workflow_response(&error),
        e @ E::ApprovalNotReady { .. } => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "approval_not_ready",
                "reason": e.to_string(),
            })),
        )
            .into_response(),
        e @ E::SignatureStepRequired { .. } => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "signature_step_required",
                "reason": e.to_string(),
            })),
        )
            .into_response(),
        e @ E::ApprovalRunFailed => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "approval_run_failed",
                "reason": e.to_string(),
            })),
        )
            .into_response(),
        e @ E::DispatchNeedsReconciliation => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "dispatch_needs_reconciliation",
                "reason": e.to_string(),
            })),
        )
            .into_response(),
        // The worker has not rendered + persisted the PDF yet — not a fault,
        // a "retry in a moment" the caller resolves by re-polling and retrying.
        E::DocumentNotReady(_) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "document_not_ready",
                "message": "The document PDF has not been rendered yet — the worker is still \
                            rendering, or its storage is misconfigured. Retry in a moment."
            })),
        )
            .into_response(),
        // The engagement agreement leaves its fee terms to the custom-clause
        // slot and no clause was written. Actionable by the lawyer: add the
        // fee clause, then send again. The walk is still at `lawyer_review`.
        E::ClausesRequired(_) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": "clauses_required",
                "message": "The fee terms have not been written. This engagement agreement \
                            leaves the fee to its custom clauses, so add at least one clause \
                            before sending it for signature."
            })),
        )
            .into_response(),
        // The filled document still carries text a person is meant to
        // complete. Actionable: answer the questionnaire or fix the clause.
        E::Unfinished(findings) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": "unfinished_document",
                "message": format!(
                    "The document is not final, so it is not sent: {}. Answer the \
                     questionnaire or correct the clause, then send again.",
                    findings.join("; ")
                )
            })),
        )
            .into_response(),
        E::UnresolvedSigner { role } => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": "unresolved_signer",
                "message": format!(
                    "Declared signer role `{role}` has no resolved name and email. \
                     The envelope is not sent until every declared role can sign."
                )
            })),
        )
            .into_response(),
        // A government-form (AcroForm) fill or config failure. The reason is
        // actionable (a missing blank, a pin mismatch, a mis-mapped field).
        E::Form { form_code, reason } => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": "form_error",
                "message": format!("Government form `{form_code}` could not be prepared: {reason}")
            })),
        )
            .into_response(),
        other => {
            tracing::error!(error = %other, "api: notation workflow drive failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "internal" })),
            )
                .into_response()
        }
    }
}

/// Map the notation questionnaire engine's error to a typed JSON response. The
/// caller-facing variants (wrong step, already complete, a question this intake
/// does not accept here) get their own status + code; the rest are internal
/// faults, logged and returned as a bare 500.
fn answer_notation_error(e: workflows::NotationSessionError) -> Response {
    use workflows::NotationSessionError as E;
    match e {
        // No such notation. (Out-of-scope is handled in the handler before the
        // engine is reached, and also collapses to a bare 404.)
        E::NotationNotFound(_) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "not_found" })),
        )
            .into_response(),
        // The answer named a different step than the one the questionnaire is
        // currently asking — the caller is out of sync. 409 with both codes so
        // it can re-fetch and retry the expected step.
        E::QuestionMismatch { expected, got } => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "question_mismatch",
                "message": format!("The questionnaire is asking `{expected}`, not `{got}`."),
                "expected": expected,
                "got": got
            })),
        )
            .into_response(),
        // Nothing left to answer.
        E::AlreadyComplete => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "already_complete",
                "message": "This notation's questionnaire is already complete."
            })),
        )
            .into_response(),
        // The question exists but this door does not accept it: not client-facing,
        // or not flagged by the lawyer review for re-collection.
        E::QuestionNotClientFacing(code) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": "question_not_answerable",
                "message": format!("`{code}` is not a client-facing question on this notation's intake.")
            })),
        )
            .into_response(),
        E::QuestionNotFlagged(code) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": "question_not_answerable",
                "message": format!("`{code}` was not flagged for re-collection by the lawyer review.")
            })),
        )
            .into_response(),
        // The answer named an option the question never declared. A caller
        // error, not a fault: 422 with the declared options (firm-authored
        // template metadata) so it can retry with one of them. The rejected
        // value is a client answer and is never echoed back or logged.
        E::UndeclaredChoice { state, declared } => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": "undeclared_choice",
                "message": format!("`{state}` accepts only its declared options."),
                "state": state,
                "declared": declared
            })),
        )
            .into_response(),
        // Everything else is an internal fault (seed gap, runtime, db, spec,
        // snapshot, or a creation-time error that cannot arise on answer).
        other => {
            tracing::error!(error = %other, "api: answer notation step failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "internal" })),
            )
                .into_response()
        }
    }
}

impl From<store::projects::ProjectCommandError> for ApiError {
    fn from(e: store::projects::ProjectCommandError) -> Self {
        Self::Project(e)
    }
}

impl From<store::projects::OpenMatterError> for ApiError {
    fn from(e: store::projects::OpenMatterError) -> Self {
        Self::OpenMatter(e)
    }
}

impl From<store::entity_commands::EntityCommandError> for ApiError {
    fn from(e: store::entity_commands::EntityCommandError) -> Self {
        Self::Entity(e)
    }
}

impl From<String> for ApiError {
    fn from(e: String) -> Self {
        Self::Db(e)
    }
}

impl From<store::notations::NotationError> for ApiError {
    fn from(e: store::notations::NotationError) -> Self {
        Self::Notation(e)
    }
}

impl From<store::documents::IngestError> for ApiError {
    fn from(e: store::documents::IngestError) -> Self {
        Self::Ingest(e)
    }
}

impl From<crate::people_commands::PeopleCommandError> for ApiError {
    fn from(e: crate::people_commands::PeopleCommandError) -> Self {
        Self::Command(e)
    }
}

impl IntoResponse for ApiError {
    // One arm per command-error variant; the match grows with each resource on
    // the command boundary. Splitting a flat dispatch into sub-functions would
    // scatter the wire-shape mapping, so allow the length (as `openapi` does).
    #[allow(clippy::too_many_lines)]
    fn into_response(self) -> axum::response::Response {
        match self {
            Self::Unauthenticated => (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": "unauthenticated" })),
            )
                .into_response(),
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({ "error": "forbidden" })),
            )
                .into_response(),
            Self::NotFound
            | Self::Command(crate::people_commands::PeopleCommandError::NotFound)
            | Self::Entity(store::entity_commands::EntityCommandError::NotFound)
            | Self::Project(store::projects::ProjectCommandError::NotFound)
            | Self::AddParticipant(
                store::participation::AddParticipantError::ProjectNotFound
                | store::participation::AddParticipantError::PersonNotFound,
            )
            | Self::UpdateParticipant(
                store::participation::UpdateParticipantError::NotFound
                | store::participation::UpdateParticipantError::PersonNotFound,
            )
            | Self::RemoveParticipant(store::participation::RemoveParticipantError::NotFound)
            | Self::Adversity(store::project_references::AdversityError::ProjectNotFound)
            // No such matter, or the acting lawyer is not in its scope: both
            // collapse to a bare not_found so the door never discloses a
            // project outside the caller's matters.
            | Self::CreateNotation(
                crate::project_notation::CreateProjectNotationError::ProjectNotFound,
            )
            | Self::SendIntake(crate::retainer_walk::SendIntakeError::NotationNotFound(_))
            // No such review document, still a draft, out of the caller's
            // client-lens scope, or the caller can't author (anon/Clerk): all
            // collapse to a bare not_found so the door never discloses it.
            | Self::ReviewComment(crate::review::ReviewCommentError::NotFound)
            | Self::RequestDeletion(
                crate::expunge_request_route::RequestDeletionError::NotFound,
            )
            | Self::Testimonial(
                store::testimonials::TestimonialError::NotFound
                | store::testimonials::TestimonialError::NotAuthorized,
            ) => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "not_found" })),
            )
                .into_response(),
            Self::Testimonial(store::testimonials::TestimonialError::NotConsented) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "consent_required",
                    "message": "The client must request public use before publication."
                })),
            )
                .into_response(),
            Self::Testimonial(error) => {
                tracing::error!(error = %error, "api: testimonial operation failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            // The matter still has dependents (participations, notations): a
            // conflict the caller resolves by detaching those first, carrying
            // the database's own detail so they see which records.
            Self::Project(store::projects::ProjectCommandError::Referenced(detail)) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "conflict",
                    "message": format!("This matter is still referenced by other records ({detail}).")
                })),
            )
                .into_response(),
            Self::PitchArtifactConflict => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "conflict",
                    "message": "A pitch offboarding document already exists with different content."
                })),
            )
                .into_response(),
            Self::Project(store::projects::ProjectCommandError::Db(e)) => {
                tracing::error!(error = %e, "api: project command failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            // --- matter open ---
            // A missing attestation is its own error code so a client can
            // distinguish "you must attest" from ordinary validation.
            Self::OpenMatter(store::projects::OpenMatterError::AttestationRequired) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "attestation_required",
                    "message": "Opening a Project requires the attorney's conflict attestation: \
                                they have checked for conflicts, and either none prevent the \
                                open or this Project is not legal advice."
                })),
            )
                .into_response(),
            // A blocking conflict is a hard stop — adverse to a current client,
            // no attestation overrides it. Its own code + the findings, so the
            // caller sees why.
            Self::OpenMatter(store::projects::OpenMatterError::BlockingConflict(findings)) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "conflict_blocked",
                    "message": "This matter is adverse to a current client. Resolve the conflict or record a waiver before opening.",
                    "findings": findings
                })),
            )
                .into_response(),
            Self::OpenMatter(store::projects::OpenMatterError::CodeConflict) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "conflict",
                    "message": "That project code is already in use. Choose a different code."
                })),
            )
                .into_response(),
            Self::OpenMatter(store::projects::OpenMatterError::ClientNotAllowed) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid_request",
                    "message": "The client of record must be a client, not a firm attorney."
                })),
            )
                .into_response(),
            Self::OpenMatter(store::projects::OpenMatterError::AttesterNotAllowed) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid_request",
                    "message": "The attesting attorney must be a firm lawyer."
                })),
            )
                .into_response(),
            Self::OpenMatter(store::projects::OpenMatterError::NotFound(what)) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid_request",
                    "message": format!("No such {what}.")
                })),
            )
                .into_response(),
            // The matter is open by the time an attestation write fails —
            // only its audit entry is missing — so it renders as the
            // server-side fault it is rather than as a refused open.
            Self::OpenMatter(store::projects::OpenMatterError::Attestation(e)) => {
                tracing::error!(error = %e, "api: matter opened but its attestation was not recorded");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            Self::OpenMatter(store::projects::OpenMatterError::Db(e)) => {
                tracing::error!(error = %e, "api: open matter failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            // A refused accountability change. Two of these are about *who is
            // asking* rather than about the request, so they answer `403` — a
            // caller who retries the same body with a different field will
            // never get past them.
            Self::AddParticipant(store::participation::AddParticipantError::Dri(
                e @ (store::participation::DriError::NotPermitted
                | store::participation::DriError::ActorUnknown),
            ))
            | Self::UpdateParticipant(store::participation::UpdateParticipantError::Dri(
                e @ (store::participation::DriError::NotPermitted
                | store::participation::DriError::ActorUnknown),
            ))
            | Self::RemoveParticipant(store::participation::RemoveParticipantError::Dri(
                e @ (store::participation::DriError::NotPermitted
                | store::participation::DriError::ActorUnknown),
            )) => (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": "forbidden",
                    "message": e.to_string()
                })),
            )
                .into_response(),
            // The rest are about the request: the wrong tier for that side, or
            // emptying the lawyer set. Unreachable on the add and update doors
            // while their request shapes carry no DRI field — mapped rather
            // than folded into the `500` arm so adding that field cannot
            // silently report a caller-correctable refusal as a server fault.
            // The command's own sentence is the message.
            Self::AddParticipant(store::participation::AddParticipantError::Dri(e))
            | Self::UpdateParticipant(store::participation::UpdateParticipantError::Dri(e))
            | Self::RemoveParticipant(store::participation::RemoveParticipantError::Dri(e)) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "unprocessable",
                    "message": e.to_string()
                })),
            )
                .into_response(),
            Self::AddParticipant(store::participation::AddParticipantError::Duplicate)
            | Self::UpdateParticipant(store::participation::UpdateParticipantError::Duplicate) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "conflict",
                    "message": "That person is already assigned to this matter."
                })),
            )
                .into_response(),
            Self::AddParticipant(store::participation::AddParticipantError::Db(e)) => {
                tracing::error!(error = %e, "api: add participant failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            // The lawyer DRI can't be stranded off the firm side — a state
            // conflict the caller resolves by reassigning the DRI first, so 409.
            Self::UpdateParticipant(store::participation::UpdateParticipantError::DriLockout) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "dri_lockout",
                    "message": "That row is the matter's lawyer DRI; reassign the DRI before moving it off the firm side."
                })),
            )
                .into_response(),
            Self::RemoveParticipant(store::participation::RemoveParticipantError::DriLockout) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "dri_lockout",
                    "message": "That row is the matter's last lawyer DRI; designate another before removing it."
                })),
            )
                .into_response(),
            Self::UpdateParticipant(store::participation::UpdateParticipantError::Db(e)) => {
                tracing::error!(error = %e, "api: update participant failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            Self::RemoveParticipant(store::participation::RemoveParticipantError::Db(e)) => {
                tracing::error!(error = %e, "api: remove participant failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            // --- open a notation on a matter ---
            Self::CreateNotation(
                crate::project_notation::CreateProjectNotationError::EmptyInput,
            ) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid_request",
                    "message": "template_code and client_email are required."
                })),
            )
                .into_response(),
            Self::CreateNotation(
                crate::project_notation::CreateProjectNotationError::ClientEmailTaken,
            ) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "conflict",
                    "message": "That client email already belongs to another person."
                })),
            )
                .into_response(),
            // The code was authored in neither this matter's repo nor the
            // bundled firm catalog.
            Self::CreateNotation(crate::project_notation::CreateProjectNotationError::Session(
                workflows::NotationSessionError::TemplateNotFound(code),
            )) => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": "not_found",
                    "message": format!("No template `{code}` in this matter's repo or the firm catalog.")
                })),
            )
                .into_response(),
            // The matter's first notation must be the engagement that opens it.
            Self::CreateNotation(crate::project_notation::CreateProjectNotationError::Session(
                workflows::NotationSessionError::EngagementMustBeFirst { code, kind },
            )) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "engagement_first",
                    "message": format!(
                        "The first notation on this matter must be the engagement that opens it \
                         (a retainer or an onboarding); `{code}` is kind `{kind}`."
                    )
                })),
            )
                .into_response(),
            // The template is in the repo but fails a blocking authoring rule.
            Self::CreateNotation(crate::project_notation::CreateProjectNotationError::Session(
                workflows::NotationSessionError::TemplateSource(
                    store::template_source::TemplateSourceError::Invalid { code, violations },
                ),
            )) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "template_invalid",
                    "message": crate::project_notation::invalid_template_message(&code, &violations),
                })),
            )
                .into_response(),
            // The template's declared workflow/questionnaire failed the
            // engine's parse — a template fault, not a server one (LAW-200:
            // these answered `500 internal`).
            Self::CreateNotation(crate::project_notation::CreateProjectNotationError::Session(
                workflows::NotationSessionError::Spec(e),
            )) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "template_spec",
                    "message": format!("Template spec: {e}")
                })),
            )
                .into_response(),
            Self::CreateNotation(crate::project_notation::CreateProjectNotationError::Session(
                workflows::NotationSessionError::TemplateSource(
                    store::template_source::TemplateSourceError::AmbiguousBlueprintDirectories,
                ),
            )) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "ambiguous_blueprint_directories",
                    "message": "The matter repo contains both `templates/` and `drafts/` blueprint directories; remove one so the template is unambiguous."
                })),
            )
                .into_response(),
            Self::CreateNotation(crate::project_notation::CreateProjectNotationError::Session(
                workflows::NotationSessionError::TemplateSource(
                    store::template_source::TemplateSourceError::InvalidLayout(details),
                ),
            )) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "invalid_layout",
                    "message": format!("Invalid matter blueprint layout: {details}")
                })),
            )
                .into_response(),
            Self::CreateNotation(crate::project_notation::CreateProjectNotationError::Session(
                workflows::NotationSessionError::TemplateSource(
                    store::template_source::TemplateSourceError::NotUtf8(path),
                ),
            )) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "template_not_utf8",
                    "message": format!("Template `{path}` body is not valid UTF-8.")
                })),
            )
                .into_response(),
            Self::CreateNotation(crate::project_notation::CreateProjectNotationError::Session(
                workflows::NotationSessionError::TemplateSource(
                    store::template_source::TemplateSourceError::Composition(details),
                ),
            )) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "composition",
                    "message": format!("Clause composition: {details}")
                })),
            )
                .into_response(),
            Self::CreateNotation(crate::project_notation::CreateProjectNotationError::Session(
                e,
            )) => {
                if let Some(message) = e.lawyer_fault() {
                    tracing::warn!(error = %e, "api: lawyer-fixable create notation failure");
                    return (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        Json(serde_json::json!({
                            "error": "template_invalid",
                            "message": message,
                        })),
                    )
                        .into_response();
                }
                tracing::error!(error = %e, "api: create notation failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            Self::CreateNotation(
                crate::project_notation::CreateProjectNotationError::RepoUnconfigured,
            ) => {
                tracing::error!("api: create notation — git repo storage not configured");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            Self::CreateNotation(crate::project_notation::CreateProjectNotationError::Db(e)) => {
                tracing::error!(error = %e, "api: create notation database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            // --- answer a notation's questionnaire step ---
            // The engine error enum has many variants; `answer_notation_error`
            // maps the caller-facing ones to typed JSON and the rest to 500.
            Self::AnswerNotation(e) => answer_notation_error(e),
            // --- send a notation's client their intake link ---
            // A notation with no bound client cannot be sent an intake link — a
            // state conflict the caller resolves by binding a client first.
            Self::SendIntake(crate::retainer_walk::SendIntakeError::NoClient(_)) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "conflict",
                    "message": "This notation has no client to send an intake link to."
                })),
            )
                .into_response(),
            Self::SendIntake(crate::retainer_walk::SendIntakeError::Db(e)) => {
                tracing::error!(error = %e, "api: send notation intake failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            Self::SendIntake(crate::retainer_walk::SendIntakeError::Notation(e)) => {
                tracing::error!(error = %e, "api: send notation intake failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            // --- approve / send a notation (fire the workflow transition) ---
            Self::NotationWorkflow(e) => workflow_drive_error(e),
            // --- upload a contract for playbook review ---
            // The client Entity has no playbook to measure the contract against
            // — a caller-actionable precondition, so 422 with the store's own
            // guidance rather than an opaque 500.
            Self::ContractReview(
                crate::contract_review_walk::ContractReviewError::NoPlaybook,
            ) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "no_playbook",
                    "message": "This company has no contract-review playbook on file yet. An attorney must create one before a contract can be reviewed."
                })),
            )
                .into_response(),
            Self::ContractReview(e) => {
                tracing::error!(error = %e, "api: contract-review upload failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            // --- request a document's deletion ---
            // Authenticated, but the session carries no Person to attribute the
            // request to — forbidden rather than not-found, since the caller
            // *is* known to the door, they just can't author.
            Self::RequestDeletion(
                crate::expunge_request_route::RequestDeletionError::NoRequester,
            ) => (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": "forbidden",
                    "message": "The session has no linked person to attribute the request to."
                })),
            )
                .into_response(),
            Self::RequestDeletion(crate::expunge_request_route::RequestDeletionError::Db(e)) => {
                tracing::error!(error = %e, "api: request document deletion failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            Self::RequestDeletion(crate::expunge_request_route::RequestDeletionError::Asset(e)) => {
                tracing::error!(error = %e, "api: request document deletion failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            Self::RequestDeletion(crate::expunge_request_route::RequestDeletionError::Request(
                e,
            )) => {
                tracing::error!(error = %e, "api: request document deletion failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            // --- add a comment to a review document ---
            Self::ReviewComment(crate::review::ReviewCommentError::Db(e)) => {
                tracing::error!(error = %e, "api: add review comment failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            // Caller-correctable validation, whichever resource raised it:
            // one wire shape means a client parses `invalid_request` the same
            // way across the command surface.
            Self::Command(crate::people_commands::PeopleCommandError::Invalid(message))
            | Self::Entity(store::entity_commands::EntityCommandError::Invalid(message))
            | Self::Project(store::projects::ProjectCommandError::Invalid(message))
            | Self::OpenMatter(store::projects::OpenMatterError::Invalid(message))
            | Self::ReviewComment(crate::review::ReviewCommentError::Invalid(message)) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid_request",
                    "message": message
                })),
            )
                .into_response(),
            Self::Command(crate::people_commands::PeopleCommandError::EmailConflict) => {
                tracing::warn!("api: create person email conflict");
                (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error": "conflict",
                        "message": "That email is already in use."
                    })),
                )
                    .into_response()
            }
            Self::Command(crate::people_commands::PeopleCommandError::ExternalIdentity) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "conflict",
                    "message": "That Notion user is already linked to another person."
                })),
            )
                .into_response(),
            Self::Command(crate::people_commands::PeopleCommandError::Blocked(message)) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "blocked",
                    "message": message
                })),
            )
                .into_response(),
            Self::Command(crate::people_commands::PeopleCommandError::SendFailed) => (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": "send_failed" })),
            )
                .into_response(),
            // Both entity conflicts are the caller's to correct: a unique
            // violation and a second row under the firm anchor's name.
            Self::Entity(
                ref e @ (store::entity_commands::EntityCommandError::Conflict
                | store::entity_commands::EntityCommandError::FirmAnchorExists
                | store::entity_commands::EntityCommandError::FirmAnchorImmutable
                | store::entity_commands::EntityCommandError::FirmAnchorProtected
                | store::entity_commands::EntityCommandError::Referenced(_)),
            ) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "conflict",
                    "message": e.user_message()
                })),
            )
                .into_response(),
            Self::Entity(store::entity_commands::EntityCommandError::Entities(e)) => {
                tracing::error!(error = %e, "api: entity command failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            Self::Entity(store::entity_commands::EntityCommandError::Jurisdictions(e)) => {
                tracing::error!(error = %e, "api: entity jurisdiction lookup failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            Self::Entity(store::entity_commands::EntityCommandError::EntityTypes(e)) => {
                tracing::error!(error = %e, "api: entity type lookup failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            Self::CreateNotation(crate::project_notation::CreateProjectNotationError::Person(e))
            | Self::SendIntake(crate::retainer_walk::SendIntakeError::Person(e)) => {
                tracing::error!(error = %e, "api: person directory error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            Self::Command(crate::people_commands::PeopleCommandError::Db(e)) => {
                tracing::error!(error = %e, "api: people command failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            Self::Command(crate::people_commands::PeopleCommandError::FirmMembership(detail)) => {
                tracing::error!(error = %detail, "api: default firm membership grant failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            Self::Command(crate::people_commands::PeopleCommandError::Dependency(detail)) => {
                tracing::error!(error = %detail, "api: people command dependency check failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            Self::Command(
                ref e @ crate::people_commands::PeopleCommandError::ParticipatesInProjects(_),
            ) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "conflict",
                    "message": e.user_message()
                })),
            )
                .into_response(),
            Self::Person(store::persons::PersonError::EmailTaken) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "conflict",
                    "message": "That email is already in use."
                })),
            )
                .into_response(),
            Self::Person(store::persons::PersonError::OidcSubjectTaken) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "conflict",
                    "message": "That IdP identity is already linked to another person."
                })),
            )
                .into_response(),
            Self::Person(e) => {
                tracing::error!(error = %e, "api: person directory error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            Self::Jurisdictions(store::jurisdictions::JurisdictionError::CodeTaken) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "conflict",
                    "message": "That jurisdiction code is already in use."
                })),
            )
                .into_response(),
            Self::Jurisdictions(e) => {
                tracing::error!(error = %e, "api: jurisdictions error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            Self::EntityTypes(store::entity_types::EntityTypeError::NameTaken) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "conflict",
                    "message": "That entity type name is already in use."
                })),
            )
                .into_response(),
            Self::EntityTypes(e) => {
                tracing::error!(error = %e, "api: entity types error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            Self::EnvelopeAdoption(message) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({"error": "envelope_adoption", "message": message})),
            ).into_response(),
            Self::PrivateTermsConflict(message) | Self::PrivateTermsInvalid(message) => {
                let status = if matches!(self, Self::PrivateTermsConflict(_)) {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::UNPROCESSABLE_ENTITY
                };
                tracing::warn!(reason = message, "private terms incomplete");
                (status, Json(serde_json::json!({"error": "private_terms_incomplete"}))).into_response()
            }
            Self::Db(e) => {
                tracing::error!(error = %e, "api: db error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            // The party is not referenced, or the matter has no client DRI
            // to start the edge from: both are the caller's to correct, so
            // they are a 422 naming the command's own sentence.
            Self::Adversity(
                e @ (store::project_references::AdversityError::PartyNotReferenced
                | store::project_references::AdversityError::NoClientDri),
            ) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "invalid_request",
                    "message": e.to_string()
                })),
            )
                .into_response(),
            Self::Adversity(e) => {
                tracing::error!(error = %e, "record adversity failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal", "message": e.to_string() })),
                )
                    .into_response()
            }
            Self::ListInvalid(error) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid_request",
                    "message": error.to_string()
                })),
            )
                .into_response(),
            Self::CourtDateSourceUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error":"court_date_source_unavailable","message":"The owning Firm’s Forgejo could not verify the pinned source."})),
            ).into_response(),
            Self::CourtDateConflict => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error":"stale_proposal","message":"Reload the court date and its sources before reviewing again."})),
            ).into_response(),
            Self::DeadlineInvalid(message)
            | Self::EfilingInvalid(message)
            | Self::DocketInvalid(message)
            | Self::ServiceInvalid(message)
            | Self::CommunicationInvalid(message)
            | Self::PagesInvalid(message)
            | Self::MalformedBody(message) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid_request",
                    "message": message
                })),
            )
                .into_response(),
            Self::Notation(e) => {
                tracing::error!(error = %e, "api: notation error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            // The one caller-reachable ingest failure: the `kind` sent is not
            // an asset-lane classification. Every other `IngestError` variant
            // is a database, storage, or write-invariant fault, so only this
            // one is matched and the rest fall through to 500 below — a
            // catch-all 400 here would report a storage outage as the
            // caller's mistake.
            Self::Ingest(store::documents::IngestError::InvalidKind(kind))
            | Self::Revision(store::assets::RevisionError::KindNotFilable(kind)) => {
                bad_request(
                    "invalid_kind",
                    &format!(
                        "`{kind}` is not a document kind. Accepted values are: {}.",
                        accepted_asset_kinds().join(", ")
                    ),
                )
            }
            Self::Ingest(e) => {
                tracing::error!(error = %e, "api: document ingest error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "internal" })),
                )
                    .into_response()
            }
            Self::Revision(store::assets::RevisionError::KindChanged { .. }) => bad_request(
                "kind_changed",
                "A document revision cannot change the document kind.",
            ),
            Self::Revision(e) => {
                tracing::error!(error = %e, "api: document revision error");
                (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": "internal" }))).into_response()
            }
            Self::Asset(e) => {
                tracing::error!(error = %e, "api: asset error");
                (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": "internal" }))).into_response()
            }
            Self::UploadSession(store::documents::UploadSessionError::InvalidKind(_)) =>
                bad_request("invalid_kind", "Choose an accepted document kind."),
            Self::UploadSession(store::documents::UploadSessionError::BlankFilename) =>
                bad_request("filename_required", "A filename is required."),
            Self::UploadSession(store::documents::UploadSessionError::InvalidVisibility) =>
                bad_request("invalid_visibility", "Visibility must be internal or client."),
            Self::UploadSession(store::documents::UploadSessionError::ShaFormat(_)) =>
                bad_request("invalid_sha256", "The digest must be 64 lowercase hexadecimal characters."),
            Self::UploadSession(store::documents::UploadSessionError::EmptyUpload { .. }) =>
                bad_request("empty_upload", "A streaming upload needs at least one byte."),
            Self::UploadSession(store::documents::UploadSessionError::TooLarge { .. }) => (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(serde_json::json!({ "error": "upload_too_large" })),
            ).into_response(),
            Self::UploadSession(e) => {
                tracing::error!(error = %e, "api: upload session error");
                (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": "internal" }))).into_response()
            }
            // An unconfigured Firm's Forgejo is operator configuration, not
            // an internal fault: the door answers a classifiable 422 whose
            // message names the missing coordinates, so a fleet-wide
            // backfill sweep can record the skip per Project and continue.
            Self::SignatureUnavailable(error) => {
                tracing::warn!(error = %error, "api: DocuSign connection unavailable");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({
                        "error": "docusign_unavailable",
                        "message": error.to_string(),
                    })),
                )
                    .into_response()
            }
            Self::ProjectMirror(issue) => (
                issue.http_status,
                Json(serde_json::json!({
                    "error": issue.code,
                    "message": issue.message,
                    "remedy": issue.remedy
                })),
            )
                .into_response(),
            Self::ForgejoUnavailable(missing) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "forgejo_unavailable",
                    "message": format!(
                        "Project surface reconciliation is unavailable until Forgejo is configured: {missing}."
                    )
                })),
            )
                .into_response(),
        }
    }
}

/// Every `kind` [`store::documents::ingest_bytes`] accepts, in the order
/// [`rules::kind::Kind::ALL`] declares them.
///
/// Derived from the same predicate the ingest boundary enforces
/// (`valid_for(Lane::Asset)`) rather than written out here, so the list a
/// rejected caller is shown cannot drift from the list actually accepted.
pub(crate) fn accepted_asset_kinds() -> Vec<&'static str> {
    rules::kind::Kind::ALL
        .iter()
        .filter(|k| k.valid_for(rules::kind::Lane::Asset))
        .map(|k| k.as_str())
        .collect()
}

/// Content-free cardinalities. CI scope is checked before any store access.
async fn project_section_counts_door(
    State(state): State<ApiState>,
    authed: LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    if let Some(SessionScope::Document(scope)) = &authed.0.scope {
        if scope.project_id != id {
            return Err(ApiError::NotFound);
        }
    } else if !store::access::can_see_project_as_lawyer(
        &state.application_store,
        authed.0.person_id,
        authed.0.role,
        id,
    )
    .await
    .map_err(ApiError::Db)?
    {
        return Err(ApiError::NotFound);
    }
    let counts = store::project_section_counts::for_project(&state.application_store, id)
        .await
        .map_err(ApiError::Db)?;
    Ok(Json(counts).into_response())
}

#[cfg(test)]
mod tests {
    use super::NotationStepResponse;
    use super::{api_operation_table, documented_api_operations, routes, ApiError};

    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    use std::collections::BTreeSet;
    use uuid::Uuid;

    #[test]
    fn deadline_inventory_fails_closed_across_firms_and_legacy_rows() {
        let current = Uuid::new_v4();
        let other = Uuid::new_v4();
        let brands = vec!["house-brand".to_string()];

        assert!(super::deadline_project_in_dashboard_scope(
            "open",
            Some(current),
            "outside-brand",
            Some(current),
            &brands
        ));
        assert!(super::deadline_project_in_dashboard_scope(
            "open",
            None,
            "house-brand",
            Some(current),
            &brands
        ));
        assert!(!super::deadline_project_in_dashboard_scope(
            "open",
            Some(other),
            "outside-brand",
            Some(current),
            &brands
        ));
        assert!(!super::deadline_project_in_dashboard_scope(
            "open",
            None,
            "outside-brand",
            Some(current),
            &brands
        ));
        assert!(!super::deadline_project_in_dashboard_scope(
            "open",
            None,
            "house-brand",
            None,
            &brands
        ));
        assert!(!super::deadline_project_in_dashboard_scope(
            "closed",
            Some(current),
            "house-brand",
            Some(current),
            &brands
        ));
    }

    #[tokio::test]
    async fn project_mirror_status_and_repair_recover_after_forgejo_denies_access() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let code = "sample-matter";
        let origin = "https://github.com/sample-firm/sample-matter";
        let repository = "/sample-firm/sample-matter";
        let url = format!("{}{repository}", server.uri());
        let forge = cloud::ForgejoForge::new(&server.uri(), "sample-firm", "fixture").unwrap();
        let route = format!("/api/v1/repos{repository}");
        for (provider_status, expected_status, expected_code) in [
            (401, StatusCode::UNPROCESSABLE_ENTITY, "credential_rejected"),
            (403, StatusCode::FORBIDDEN, "access_denied"),
        ] {
            Mock::given(method("GET"))
                .and(path(&route))
                .and(header("Authorization", "token fixture"))
                .respond_with(ResponseTemplate::new(provider_status))
                .expect(2)
                .mount(&server)
                .await;
            let read = super::forgejo_mirror_payload(
                Some(origin),
                super::find_forgejo_mirror(&forge, code).await,
            );
            assert_eq!(read["forgejo_status"], "unavailable");
            assert_eq!(read["forgejo_issue"]["code"], expected_code);
            assert!(read["forgejo"].is_null());
            let error = super::repair_forgejo_mirror(&forge, code, origin)
                .await
                .unwrap_err();
            let response = error.into_response();
            assert_eq!(response.status(), expected_status);
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let failure: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(failure["error"], expected_code);
            assert_eq!(failure["remedy"], read["forgejo_issue"]["remedy"]);

            server.verify().await;
            server.reset().await;
        }

        let forge = cloud::ForgejoForge::new(&server.uri(), "sample-firm", "rotated").unwrap();
        Mock::given(method("GET"))
            .and(path(&route))
            .and(header("Authorization", "token rotated"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/orgs/sample-firm/repos"))
            .and(header("Authorization", "token rotated"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "html_url": url,
                "name": code,
                "owner": { "login": "sample-firm" },
                "private": true
            })))
            .expect(1)
            .mount(&server)
            .await;
        let repaired = super::repair_forgejo_mirror(&forge, code, origin)
            .await
            .unwrap();
        assert_eq!(repaired["mirror_status"], "created");
        assert_eq!(repaired["forgejo_owner"], "sample-firm");
        assert_eq!(repaired["repository_url"], url);
        assert_eq!(repaired["source_repository_url"], origin);
    }

    #[tokio::test]
    async fn forgejo_primary_reports_the_verified_main_without_a_github_mirror() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let source = format!("{}/sample-firm/sample-matter", server.uri());
        let forge = Ok(cloud::ForgejoForge::new(&server.uri(), "sample-firm", "fixture").unwrap());
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/sample-firm/sample-matter"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "html_url": source,
                "name": "sample-matter",
                "owner": {"login": "sample-firm"},
                "private": true
            })))
            .expect(1)
            .mount(&server)
            .await;
        let sha = "a".repeat(40);
        Mock::given(method("GET"))
            .and(path(
                "/api/v1/repos/sample-firm/sample-matter/branches/main",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "commit": {"id": sha}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let project = store::postgres::projects::FirmProjectSource {
            id: Uuid::from_u128(1),
            code: "sample-matter".into(),
            repository_url: Some(source),
        };
        let report = super::read_forge_parity(&project, &forge, None, None).await;
        assert_eq!(report["status"], "forgejo_only");
        assert_eq!(report["forgejo_main"], sha);
    }

    #[test]
    fn client_notation_list_and_id_visibility_keep_only_client_visible_kinds() {
        assert!(!super::client_may_read_notation_kind("trial_prep"));
        assert!(super::client_may_read_notation_kind("agreement"));
        assert!(super::client_may_read_notation_kind("unknown_legacy_kind"));
    }

    #[test]
    fn pitch_offboarding_letter_uses_the_effective_date_and_exact_client_sentence() {
        let source = super::pitch_offboarding_typst("Acme LLC", "October 15, 2026");
        assert!(source.contains("#\"October 15, 2026\""));
        assert!(source.contains("#\"Acme LLC\" did not accept the pitch."));
    }

    #[test]
    fn pitch_offboarding_artifact_mismatch_is_a_conflict() {
        let response = ApiError::PitchArtifactConflict.into_response();
        assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
    }

    #[test]
    fn a_step_body_carries_the_questions_help_text() {
        // End of the pipe: whatever the walk resolved has to survive the
        // `NextStep` -> `NotationStepResponse` mapping and serde, or the
        // guidance still reaches no caller. Assert on the serialized JSON
        // rather than the struct, because that body is the API surface.
        let step = workflows::NextStep::NeedsAnswer {
            question: workflows::QuestionDescriptor {
                id: uuid::Uuid::now_v7(),
                code: "entity".into(),
                prompt: "Which entity is the client?".into(),
                answer_type: "entity".into(),
                choices: Vec::new(),
                help_text: Some("Select the entity record this answer refers to.".into()),
            },
        };
        let body = serde_json::to_value(NotationStepResponse::from(step)).unwrap();
        assert_eq!(
            body["question"]["help_text"],
            "Select the entity record this answer refers to.",
        );
    }

    #[test]
    fn a_step_body_omits_help_text_when_the_question_declares_none() {
        // `None` must drop the key rather than serialize `null`, so a client
        // reading the body never renders an empty guidance line.
        let step = workflows::NextStep::NeedsAnswer {
            question: workflows::QuestionDescriptor {
                id: uuid::Uuid::now_v7(),
                code: "entity".into(),
                prompt: "Which entity is the client?".into(),
                answer_type: "entity".into(),
                choices: Vec::new(),
                help_text: None,
            },
        };
        let body = serde_json::to_value(NotationStepResponse::from(step)).unwrap();
        assert!(body["question"].get("help_text").is_none());
    }

    // A code collision is caller-correctable (the caller picks another code),
    // so it must surface as 409, never as a 500 that reads as a Navigator
    // fault (ENG-414: `open_matter` refuses a colliding code rather than
    // generating around it).
    #[test]
    fn a_code_conflict_on_matter_open_is_409_not_500() {
        let response =
            ApiError::OpenMatter(store::projects::OpenMatterError::CodeConflict).into_response();
        assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
    }

    /// An unconfigured Firm's Forgejo is operator configuration, not an
    /// internal fault: the door answers a classifiable `422
    /// forgejo_unavailable` whose message names every missing piece, so a
    /// fleet-wide backfill sweep can report it per Project and continue.
    #[tokio::test]
    async fn a_firm_missing_its_forgejo_coordinates_is_422_forgejo_unavailable() {
        let response = ApiError::ForgejoUnavailable(
            "NAVIGATOR_FORGEJO_ROOT_URL_ENV, the Firm's forgejo_access_token integration secret"
                .to_string(),
        )
        .into_response();

        assert_eq!(
            response.status(),
            axum::http::StatusCode::UNPROCESSABLE_ENTITY
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("the 422 response body should be readable");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("the 422 response should be JSON");
        assert_eq!(json["error"], "forgejo_unavailable");
        assert!(json["message"]
            .as_str()
            .expect("the message names the missing pieces")
            .contains("forgejo_access_token"));
    }

    /// A Firm without a usable DocuSign connection is configuration the
    /// Firm administrator fixes, so the door answers `503
    /// docusign_unavailable` naming the failed check, not a bare 500.
    #[tokio::test]
    async fn a_firm_without_a_docusign_connection_is_503_docusign_unavailable() {
        let response = ApiError::SignatureUnavailable(crate::signature::SignatureError::Provider(
            "The Firm's DocuSign connection is unavailable: the Project has no owning Firm".into(),
        ))
        .into_response();

        assert_eq!(
            response.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("the 503 response body should be readable");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("the 503 response should be JSON");
        assert_eq!(json["error"], "docusign_unavailable");
        assert!(json["message"]
            .as_str()
            .expect("the message names the failed check")
            .contains("no owning Firm"));
    }

    #[tokio::test]
    async fn a_person_email_unique_violation_is_409_with_a_field_message() {
        let response = ApiError::Person(store::persons::PersonError::EmailTaken).into_response();

        assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("the conflict response body should be readable");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("the conflict response should be JSON");
        assert_eq!(json["message"], "That email is already in use.");
    }

    #[test]
    fn documented_operations_are_derived_from_the_route_table() {
        // The inventory the drift guard checks is exactly the table's
        // (method, path) projection — it cannot silently omit a
        // registered route, which is what closes the "new undocumented
        // path stays unprobed" gap: any row added to the table appears
        // here, and `openapi_drift.rs` then compares it to the document.
        let table: Vec<(&str, &str)> = api_operation_table()
            .into_iter()
            .map(|(method, path, _handler)| (method, path))
            .collect();
        assert_eq!(documented_api_operations(), table);
    }

    #[test]
    fn documented_operations_have_no_duplicates() {
        let ops = documented_api_operations();
        let unique: BTreeSet<_> = ops.iter().copied().collect();
        assert_eq!(
            unique.len(),
            ops.len(),
            "duplicate (method, path) in the API operation table: {ops:?}"
        );
    }

    #[test]
    fn routes_builds_without_panicking_folding_multi_method_paths() {
        // Exercises the path-folding/merge branch (two methods share
        // `/app/api/people` and three share `/app/api/people/{id}`); axum panics
        // if the same path is registered twice, so a green build here
        // proves the fold merges rather than re-registers.
        let _router = routes();
    }

    // Viewing a matter must never provision a repository or Drive folder —
    // only `open_project` (create) and the explicit admin retry door call
    // reconcile. A source scan rather than a live-service test: the
    // invariant is "this handler never calls that function," which a
    // running test can prove absent but never prove present.
    #[test]
    fn viewing_a_project_never_calls_project_surfaces_reconcile() {
        let src = include_str!("api.rs");
        let handler = src
            .split("async fn get_project_door(")
            .nth(1)
            .expect("get_project_door is defined in this file")
            .split("\n}\n")
            .next()
            .expect("the handler body ends at its closing brace");
        assert!(
            !handler.contains("reconcile"),
            "GET /app/api/projects/{{id}} must stay read-only with respect to \
             repository/Drive provisioning"
        );
    }

    #[test]
    fn testimonial_save_body_rejects_consent_and_publication_fields() {
        assert!(serde_json::from_str::<super::TestimonialSubmissionInput>(
            r#"{"quote":"quote","request_public":true,"consented_at":"2026-01-01T00:00:00Z"}"#
        )
        .is_err());
        assert!(serde_json::from_str::<super::TestimonialSubmissionInput>(
            r#"{"quote":"quote","request_public":true,"published_at":"2026-01-01T00:00:00Z"}"#
        )
        .is_err());
        let accepted: super::TestimonialSubmissionInput = serde_json::from_str(
            r#"{"quote":"quote","attribution":"Founder","request_public":true}"#,
        )
        .unwrap();
        assert!(accepted.request_public);
    }

    #[tokio::test]
    async fn queue_finance_run_posts_the_same_run_to_authenticated_dbos_ingress() {
        use wiremock::matchers::{body_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let project_id = uuid::Uuid::from_u128(7);
        let run_id = "manual-0190e0d0-0000-7000-8000-000000000001";
        Mock::given(method("POST"))
            .and(path("/internal/dbos/finance-aggregate"))
            .and(header("authorization", "Bearer service-token"))
            .and(body_json(serde_json::json!({
                "workflow_id": format!("FinanceAggregate:{run_id}"),
                "request": {
                    "project_id": project_id,
                    "project_code": "sample-finance"
                }
            })))
            .respond_with(ResponseTemplate::new(202))
            .expect(1)
            .mount(&server)
            .await;
        super::queue_finance_run_dbos(
            &server.uri(),
            "service-token",
            project_id,
            "sample-finance",
            run_id,
        )
        .await
        .expect("authenticated DBOS run accepted");
    }

    #[tokio::test]
    async fn queue_main_push_sends_the_event_to_authenticated_dbos_ingress() {
        use wiremock::matchers::{body_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let project_id = uuid::Uuid::from_u128(9);
        Mock::given(method("POST"))
            .and(path("/internal/dbos/project-slack"))
            .and(header("authorization", "Bearer tok"))
            .and(body_json(serde_json::json!({
                "workflow_id": workflows::trigger::project_slack_main_push_workflow_id(
                    project_id, "abc1234", 1,
                ),
                "event": {
                "short_sha": "abc1234",
                "run_url": "https://github.com/an-organization/sample-push/actions/runs/1",
                "author_login": "octocat",
                "gate": "pass",
                "run_attempt": 1
                }
            })))
            .respond_with(ResponseTemplate::new(202))
            .expect(1)
            .mount(&server)
            .await;
        let event = super::MainPushEvent {
            short_sha: "abc1234".into(),
            run_url: "https://github.com/an-organization/sample-push/actions/runs/1".into(),
            author_login: "octocat".into(),
            gate: super::CiOutcome::Pass,
            publish: None,
            run_attempt: 1,
            subject: None,
        };
        event.validate().expect("a well-formed event");
        super::queue_main_push_dbos(&server.uri(), "tok", project_id, &event)
            .await
            .expect("queued");
    }

    #[test]
    fn a_main_push_event_refuses_a_malformed_sha_url_login_or_attempt() {
        let good = super::MainPushEvent {
            short_sha: "abc1234".into(),
            run_url: "https://github.com/an-organization/sample-push/actions/runs/1".into(),
            author_login: "octocat".into(),
            gate: super::CiOutcome::Fail,
            publish: Some(super::CiOutcome::Skipped),
            run_attempt: 2,
            subject: Some("feat: a commit subject".into()),
        };
        assert!(good.validate().is_ok());
        let two_lines = super::MainPushEvent {
            subject: Some("feat: a subject\nand a body".into()),
            ..good.clone()
        };
        assert!(two_lines.validate().is_err(), "a subject is one line");
        let too_long = super::MainPushEvent {
            subject: Some("s".repeat(513)),
            ..good.clone()
        };
        assert!(too_long.validate().is_err());
        let bad_sha = super::MainPushEvent {
            short_sha: "ABC1234".into(),
            ..good.clone()
        };
        assert!(bad_sha.validate().is_err());
        let bad_url = super::MainPushEvent {
            run_url: "http://example.com/run".into(),
            ..good.clone()
        };
        assert!(bad_url.validate().is_err());
        let bad_login = super::MainPushEvent {
            author_login: "octo cat".into(),
            ..good.clone()
        };
        assert!(bad_login.validate().is_err());
        let bad_attempt = super::MainPushEvent {
            run_attempt: 0,
            ..good
        };
        assert!(bad_attempt.validate().is_err());
        let without_subject = serde_json::from_value::<super::MainPushEvent>(serde_json::json!({
            "short_sha": "abc1234",
            "run_url": "https://example.com",
            "author_login": "octocat",
            "gate": "pass",
            "run_attempt": 1
        }))
        .expect("a caller on an older release sends no subject");
        assert_eq!(without_subject.subject, None);
        assert!(
            serde_json::from_value::<super::MainPushEvent>(serde_json::json!({
                "short_sha": "abc1234",
                "run_url": "https://example.com",
                "author_login": "octocat",
                "gate": "pass",
                "run_attempt": 1,
                "client": "Acme"
            }))
            .is_err(),
            "the subject is the only free-text field this door accepts"
        );
    }

    #[tokio::test]
    async fn queue_finance_run_reports_a_rejected_ingress() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let result = super::queue_finance_run_dbos(
            &server.uri(),
            "synthetic-token",
            uuid::Uuid::nil(),
            "sample-finance",
            "manual-run-2",
        )
        .await;
        assert!(result.is_err());
    }

    /// Render one `ApiError` and return its status plus decoded JSON body.
    async fn rendered(error: ApiError) -> (StatusCode, serde_json::Value) {
        response_json(error.into_response()).await
    }

    async fn response_json(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read the response body");
        let json = if body.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&body).expect("the response body is JSON")
        };
        (status, json)
    }

    /// Every caller-facing refusal on the command boundary keeps its status
    /// and its machine-readable `error` code; every store fault is a bare
    /// `500 internal` that never echoes the fault.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn every_api_error_variant_maps_to_its_status_and_error_code() {
        use crate::contract_review_walk::ContractReviewError as Review;
        use crate::expunge_request_route::RequestDeletionError as Deletion;
        use crate::people_commands::PeopleCommandError as People;
        use crate::project_notation::CreateProjectNotationError as Create;
        use crate::retainer_walk::SendIntakeError as Intake;
        use store::entity_commands::EntityCommandError as Entity;
        use store::participation::{
            AddParticipantError as Add, DriError, RemoveParticipantError as Remove,
            UpdateParticipantError as Update,
        };
        use store::projects::{OpenMatterError as Open, ProjectCommandError as Project};
        use store::template_source::TemplateSourceError as Source;
        use workflows::NotationSessionError as Session;

        let id = Uuid::nil();
        let cases: Vec<(ApiError, StatusCode, &str)> = vec![
            (
                ApiError::Unauthenticated,
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
            ),
            (ApiError::Forbidden, StatusCode::FORBIDDEN, "forbidden"),
            (ApiError::NotFound, StatusCode::NOT_FOUND, "not_found"),
            (
                ApiError::Command(People::NotFound),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                ApiError::Entity(Entity::NotFound),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                ApiError::Project(Project::NotFound),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                ApiError::AddParticipant(Add::ProjectNotFound),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                ApiError::AddParticipant(Add::PersonNotFound),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                ApiError::UpdateParticipant(Update::NotFound),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                ApiError::UpdateParticipant(Update::PersonNotFound),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                ApiError::RemoveParticipant(Remove::NotFound),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                ApiError::Adversity(store::project_references::AdversityError::ProjectNotFound),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                ApiError::CreateNotation(Create::ProjectNotFound),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                ApiError::SendIntake(Intake::NotationNotFound(id)),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                ApiError::ReviewComment(crate::review::ReviewCommentError::NotFound),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                ApiError::RequestDeletion(Deletion::NotFound),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                ApiError::Testimonial(store::testimonials::TestimonialError::NotFound),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                ApiError::Testimonial(store::testimonials::TestimonialError::NotAuthorized),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                ApiError::Testimonial(store::testimonials::TestimonialError::NotConsented),
                StatusCode::CONFLICT,
                "consent_required",
            ),
            (
                ApiError::Testimonial(store::testimonials::TestimonialError::WriteReturnedNothing),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::Project(Project::Referenced("participation".into())),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                ApiError::Project(Project::Db("down".into())),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::Project(Project::Invalid("bad")),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                ApiError::OpenMatter(Open::AttestationRequired),
                StatusCode::BAD_REQUEST,
                "attestation_required",
            ),
            (
                ApiError::OpenMatter(Open::BlockingConflict(vec!["adverse".into()])),
                StatusCode::CONFLICT,
                "conflict_blocked",
            ),
            (
                ApiError::OpenMatter(Open::ClientNotAllowed),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                ApiError::OpenMatter(Open::AttesterNotAllowed),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                ApiError::OpenMatter(Open::NotFound("client")),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                ApiError::OpenMatter(Open::Attestation(
                    store::relationship_logs::RelationshipLogError::WriteReturnedNothing,
                )),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::OpenMatter(Open::Db("down".into())),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::OpenMatter(Open::Invalid("bad")),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                ApiError::AddParticipant(Add::Dri(DriError::NotPermitted)),
                StatusCode::FORBIDDEN,
                "forbidden",
            ),
            (
                ApiError::UpdateParticipant(Update::Dri(DriError::ActorUnknown)),
                StatusCode::FORBIDDEN,
                "forbidden",
            ),
            (
                ApiError::RemoveParticipant(Remove::Dri(DriError::NotPermitted)),
                StatusCode::FORBIDDEN,
                "forbidden",
            ),
            (
                ApiError::AddParticipant(Add::Dri(DriError::TierMismatch)),
                StatusCode::UNPROCESSABLE_ENTITY,
                "unprocessable",
            ),
            (
                ApiError::UpdateParticipant(Update::Dri(DriError::LawyerDriRequired)),
                StatusCode::UNPROCESSABLE_ENTITY,
                "unprocessable",
            ),
            (
                ApiError::RemoveParticipant(Remove::Dri(DriError::LawyerDriRequired)),
                StatusCode::UNPROCESSABLE_ENTITY,
                "unprocessable",
            ),
            (
                ApiError::AddParticipant(Add::Duplicate),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                ApiError::UpdateParticipant(Update::Duplicate),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                ApiError::AddParticipant(Add::Db("down".into())),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::UpdateParticipant(Update::DriLockout),
                StatusCode::CONFLICT,
                "dri_lockout",
            ),
            (
                ApiError::RemoveParticipant(Remove::DriLockout),
                StatusCode::CONFLICT,
                "dri_lockout",
            ),
            (
                ApiError::UpdateParticipant(Update::Db("down".into())),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::RemoveParticipant(Remove::Db("down".into())),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::CreateNotation(Create::EmptyInput),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                ApiError::CreateNotation(Create::ClientEmailTaken),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                ApiError::CreateNotation(Create::Session(Session::TemplateNotFound("nda".into()))),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                ApiError::CreateNotation(Create::Session(Session::EngagementMustBeFirst {
                    code: "nda".into(),
                    kind: "agreement".into(),
                })),
                StatusCode::UNPROCESSABLE_ENTITY,
                "engagement_first",
            ),
            (
                ApiError::CreateNotation(Create::Session(Session::TemplateSource(
                    Source::Invalid {
                        code: "nda".into(),
                        violations: Vec::new(),
                    },
                ))),
                StatusCode::UNPROCESSABLE_ENTITY,
                "template_invalid",
            ),
            (
                ApiError::CreateNotation(Create::Session(Session::Spec(
                    workflows::WorkflowSpecError::MissingBegin,
                ))),
                StatusCode::UNPROCESSABLE_ENTITY,
                "template_spec",
            ),
            (
                ApiError::CreateNotation(Create::Session(Session::TemplateSource(
                    Source::AmbiguousBlueprintDirectories,
                ))),
                StatusCode::UNPROCESSABLE_ENTITY,
                "ambiguous_blueprint_directories",
            ),
            (
                ApiError::CreateNotation(Create::Session(Session::TemplateSource(
                    Source::InvalidLayout("stray".into()),
                ))),
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_layout",
            ),
            (
                ApiError::CreateNotation(Create::Session(Session::TemplateSource(
                    Source::NotUtf8("templates/nda.md".into()),
                ))),
                StatusCode::UNPROCESSABLE_ENTITY,
                "template_not_utf8",
            ),
            (
                ApiError::CreateNotation(Create::Session(Session::TemplateSource(
                    Source::Composition("missing clause".into()),
                ))),
                StatusCode::UNPROCESSABLE_ENTITY,
                "composition",
            ),
            (
                ApiError::CreateNotation(Create::Session(Session::Db("down".into()))),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::CreateNotation(Create::RepoUnconfigured),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::CreateNotation(Create::Db("down".into())),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::CreateNotation(Create::Person(
                    store::persons::PersonError::WriteReturnedNothing,
                )),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::SendIntake(Intake::NoClient(id)),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                ApiError::SendIntake(Intake::Db("down".into())),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::SendIntake(Intake::Notation(store::notations::NotationError::NotFound(
                    id,
                ))),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::SendIntake(Intake::Person(
                    store::persons::PersonError::WriteReturnedNothing,
                )),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::ContractReview(Review::NoPlaybook),
                StatusCode::UNPROCESSABLE_ENTITY,
                "no_playbook",
            ),
            (
                ApiError::ContractReview(Review::TemplateMissing),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::RequestDeletion(Deletion::NoRequester),
                StatusCode::FORBIDDEN,
                "forbidden",
            ),
            (
                ApiError::RequestDeletion(Deletion::Db("down".into())),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::RequestDeletion(Deletion::Asset(
                    store::assets::AssetError::WriteReturnedNothing,
                )),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::ReviewComment(crate::review::ReviewCommentError::Db("down".into())),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::ReviewComment(crate::review::ReviewCommentError::Invalid("empty")),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                ApiError::Command(People::Invalid("bad")),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                ApiError::Entity(Entity::Invalid("bad")),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                ApiError::Command(People::EmailConflict),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                ApiError::Command(People::ExternalIdentity),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                ApiError::Command(People::Blocked("no")),
                StatusCode::CONFLICT,
                "blocked",
            ),
            (
                ApiError::Command(People::SendFailed),
                StatusCode::BAD_GATEWAY,
                "send_failed",
            ),
            (
                ApiError::Command(People::Db(
                    store::persons::PersonError::WriteReturnedNothing,
                )),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::Command(People::FirmMembership("grant".into())),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::Command(People::Dependency("check".into())),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::Command(People::ParticipatesInProjects(2)),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                ApiError::Entity(Entity::Conflict),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                ApiError::Entity(Entity::FirmAnchorExists),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                ApiError::Entity(Entity::FirmAnchorImmutable),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                ApiError::Entity(Entity::FirmAnchorProtected),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                ApiError::Entity(Entity::Referenced("project".into())),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                ApiError::Entity(Entity::Entities(
                    store::entities::EntityError::WriteReturnedNothing,
                )),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::Entity(Entity::Jurisdictions(
                    store::jurisdictions::JurisdictionError::WriteReturnedNothing,
                )),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::Entity(Entity::EntityTypes(
                    store::entity_types::EntityTypeError::WriteReturnedNothing,
                )),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::Person(store::persons::PersonError::OidcSubjectTaken),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                ApiError::Person(store::persons::PersonError::WriteReturnedNothing),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::Jurisdictions(store::jurisdictions::JurisdictionError::CodeTaken),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                ApiError::Jurisdictions(
                    store::jurisdictions::JurisdictionError::WriteReturnedNothing,
                ),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::EntityTypes(store::entity_types::EntityTypeError::NameTaken),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                ApiError::EntityTypes(store::entity_types::EntityTypeError::WriteReturnedNothing),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::Db("down".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::Adversity(store::project_references::AdversityError::PartyNotReferenced),
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
            ),
            (
                ApiError::Adversity(store::project_references::AdversityError::NoClientDri),
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
            ),
            (
                ApiError::Adversity(store::project_references::AdversityError::Project(
                    store::projects::ProjectStoreError::CodeTaken,
                )),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::DeadlineInvalid("d".into()),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                ApiError::EfilingInvalid("e".into()),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                ApiError::DocketInvalid("k".into()),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                ApiError::ServiceInvalid("s".into()),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                ApiError::CommunicationInvalid("c".into()),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                ApiError::PagesInvalid("p".into()),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                ApiError::MalformedBody("m".into()),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                ApiError::Notation(store::notations::NotationError::NotFound(id)),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::Ingest(store::documents::IngestError::InvalidKind(
                    "memo-ish".into(),
                )),
                StatusCode::BAD_REQUEST,
                "invalid_kind",
            ),
            (
                ApiError::Revision(store::assets::RevisionError::KindNotFilable(
                    "memo-ish".into(),
                )),
                StatusCode::BAD_REQUEST,
                "invalid_kind",
            ),
            (
                ApiError::Ingest(store::documents::IngestError::WriteReturnedNothing),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::Revision(store::assets::RevisionError::KindChanged {
                    slug: "letter.pdf".into(),
                    existing: "letter".into(),
                    attempted: "memo".into(),
                }),
                StatusCode::BAD_REQUEST,
                "kind_changed",
            ),
            (
                ApiError::Revision(store::assets::RevisionError::InvalidTreeMetadata),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::Asset(store::assets::AssetError::WriteReturnedNothing),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::UploadSession(store::documents::UploadSessionError::InvalidKind(
                    "x".into(),
                )),
                StatusCode::BAD_REQUEST,
                "invalid_kind",
            ),
            (
                ApiError::UploadSession(store::documents::UploadSessionError::BlankFilename),
                StatusCode::BAD_REQUEST,
                "filename_required",
            ),
            (
                ApiError::UploadSession(store::documents::UploadSessionError::InvalidVisibility),
                StatusCode::BAD_REQUEST,
                "invalid_visibility",
            ),
            (
                ApiError::UploadSession(store::documents::UploadSessionError::ShaFormat(
                    "zz".into(),
                )),
                StatusCode::BAD_REQUEST,
                "invalid_sha256",
            ),
            (
                ApiError::UploadSession(store::documents::UploadSessionError::EmptyUpload {
                    declared: 0,
                }),
                StatusCode::BAD_REQUEST,
                "empty_upload",
            ),
            (
                ApiError::UploadSession(store::documents::UploadSessionError::TooLarge {
                    declared: 10,
                    max: 1,
                }),
                StatusCode::PAYLOAD_TOO_LARGE,
                "upload_too_large",
            ),
            (
                ApiError::UploadSession(store::documents::UploadSessionError::NoSuchSession(id)),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                ApiError::PitchArtifactConflict,
                StatusCode::CONFLICT,
                "conflict",
            ),
        ];
        for (error, status, code) in cases {
            let label = format!("{error:?}");
            let (got_status, body) = rendered(error).await;
            assert_eq!(got_status, status, "{label}");
            assert_eq!(body["error"], code, "{label}");
            if got_status == StatusCode::INTERNAL_SERVER_ERROR {
                assert!(
                    !body.to_string().contains("down"),
                    "{label}: a fault must not echo its detail"
                );
            }
        }
    }

    #[tokio::test]
    async fn refusal_messages_carry_the_callers_detail() {
        let (_, body) = rendered(ApiError::OpenMatter(
            store::projects::OpenMatterError::BlockingConflict(vec!["Acme v. Client".into()]),
        ))
        .await;
        assert_eq!(body["findings"][0], "Acme v. Client");

        let (_, body) = rendered(ApiError::Project(
            store::projects::ProjectCommandError::Referenced("2 notations".into()),
        ))
        .await;
        assert!(body["message"].as_str().unwrap().contains("2 notations"));

        let (_, body) = rendered(ApiError::Ingest(
            store::documents::IngestError::InvalidKind("memo-ish".into()),
        ))
        .await;
        let message = body["message"].as_str().unwrap();
        assert!(message.contains("`memo-ish` is not a document kind"));
        for kind in super::accepted_asset_kinds() {
            assert!(message.contains(kind), "{kind} is listed");
        }

        let (_, body) = rendered(ApiError::CreateNotation(
            crate::project_notation::CreateProjectNotationError::Session(
                workflows::NotationSessionError::EngagementMustBeFirst {
                    code: "nda".into(),
                    kind: "agreement".into(),
                },
            ),
        ))
        .await;
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("`nda` is kind `agreement`"));

        let (_, body) = rendered(ApiError::Command(
            crate::people_commands::PeopleCommandError::Blocked("You cannot do that."),
        ))
        .await;
        assert_eq!(body["message"], "You cannot do that.");

        // An adversity store fault is the one 500 that names its cause.
        let (_, body) = rendered(ApiError::Adversity(
            store::project_references::AdversityError::Project(
                store::projects::ProjectStoreError::CodeTaken,
            ),
        ))
        .await;
        assert_eq!(body["message"], "that project code is already in use");
    }

    #[tokio::test]
    async fn signature_failures_report_the_next_step_without_provider_payloads() {
        use crate::retainer_walk::WorkflowDriveError;
        use crate::signature::SignatureError;
        let private = "private diagnostic";
        for (failure, status, code) in [
            (
                SignatureError::Connection(private.into()),
                StatusCode::SERVICE_UNAVAILABLE,
                "signature_connection",
            ),
            (
                SignatureError::Authentication("Check the registered key.".into()),
                StatusCode::BAD_GATEWAY,
                "signature_authentication",
            ),
            (
                SignatureError::Unreachable,
                StatusCode::BAD_GATEWAY,
                "signature_unreachable",
            ),
            (
                SignatureError::Provider(private.into()),
                StatusCode::BAD_GATEWAY,
                "signature_provider",
            ),
            (
                SignatureError::AnchorValidation(private.into()),
                StatusCode::UNPROCESSABLE_ENTITY,
                "signature_fields",
            ),
            (
                SignatureError::TabValidation(private.into()),
                StatusCode::UNPROCESSABLE_ENTITY,
                "signature_fields",
            ),
        ] {
            let (response, body) =
                rendered(ApiError::from(WorkflowDriveError::Signature(failure))).await;
            assert_eq!(response, status);
            assert_eq!(body["error"], code);
            assert!(!body["message"].as_str().unwrap().is_empty());
            assert!(!body.to_string().contains(private));
        }
    }

    #[tokio::test]
    async fn the_notation_workflow_drive_errors_map_to_their_codes() {
        use crate::retainer_walk::WorkflowDriveError as E;
        let id = Uuid::nil();
        let cases: Vec<(E, StatusCode, &str)> = vec![
            (
                E::DocumentNotReady(id),
                StatusCode::CONFLICT,
                "document_not_ready",
            ),
            (
                E::ClausesRequired(id),
                StatusCode::UNPROCESSABLE_ENTITY,
                "clauses_required",
            ),
            (
                E::Unfinished(vec!["[name]".into(), "[date]".into()]),
                StatusCode::UNPROCESSABLE_ENTITY,
                "unfinished_document",
            ),
            (
                E::UnresolvedSigner {
                    role: "client".into(),
                },
                StatusCode::UNPROCESSABLE_ENTITY,
                "unresolved_signer",
            ),
            (
                E::Form {
                    form_code: "ss-4".into(),
                    reason: "pin mismatch".into(),
                },
                StatusCode::UNPROCESSABLE_ENTITY,
                "form_error",
            ),
            (
                E::Db("down".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
            (
                E::Spec("bad".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
        ];
        for (error, status, code) in cases {
            let label = format!("{error}");
            let (got, body) = rendered(ApiError::from(error)).await;
            assert_eq!(got, status, "{label}");
            assert_eq!(body["error"], code, "{label}");
        }
        let (_, body) = rendered(ApiError::NotationWorkflow(E::Unfinished(vec![
            "[name]".into(),
            "[date]".into(),
        ])))
        .await;
        assert!(body["message"].as_str().unwrap().contains("[name]; [date]"));
        let (_, body) = rendered(ApiError::NotationWorkflow(E::UnresolvedSigner {
            role: "guarantor".into(),
        }))
        .await;
        assert!(body["message"].as_str().unwrap().contains("`guarantor`"));
        let (_, body) = rendered(ApiError::NotationWorkflow(E::Form {
            form_code: "ss-4".into(),
            reason: "pin mismatch".into(),
        }))
        .await;
        assert!(body["message"].as_str().unwrap().contains("`ss-4`"));
    }

    #[tokio::test]
    async fn the_questionnaire_answer_errors_map_to_their_codes() {
        use workflows::NotationSessionError as E;
        let cases: Vec<(E, StatusCode, &str)> = vec![
            (
                E::NotationNotFound(Uuid::nil()),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                E::QuestionMismatch {
                    expected: "entity".into(),
                    got: "name".into(),
                },
                StatusCode::CONFLICT,
                "question_mismatch",
            ),
            (E::AlreadyComplete, StatusCode::CONFLICT, "already_complete"),
            (
                E::QuestionNotClientFacing("fee".into()),
                StatusCode::UNPROCESSABLE_ENTITY,
                "question_not_answerable",
            ),
            (
                E::QuestionNotFlagged("fee".into()),
                StatusCode::UNPROCESSABLE_ENTITY,
                "question_not_answerable",
            ),
            (
                E::UndeclaredChoice {
                    state: "tier".into(),
                    declared: vec!["a".into(), "b".into()],
                },
                StatusCode::UNPROCESSABLE_ENTITY,
                "undeclared_choice",
            ),
            (
                E::Db("down".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
        ];
        for (error, status, code) in cases {
            let label = format!("{error}");
            let (got, body) = rendered(ApiError::from(error)).await;
            assert_eq!(got, status, "{label}");
            assert_eq!(body["error"], code, "{label}");
        }
        let (_, body) = rendered(ApiError::AnswerNotation(E::QuestionMismatch {
            expected: "entity".into(),
            got: "name".into(),
        }))
        .await;
        assert_eq!(body["expected"], "entity");
        assert_eq!(body["got"], "name");
        let (_, body) = rendered(ApiError::AnswerNotation(E::UndeclaredChoice {
            state: "tier".into(),
            declared: vec!["a".into(), "b".into()],
        }))
        .await;
        assert_eq!(body["declared"], serde_json::json!(["a", "b"]));
    }

    #[tokio::test]
    async fn from_impls_route_each_store_error_to_its_variant() {
        assert!(matches!(
            ApiError::from(store::testimonials::TestimonialError::NotFound),
            ApiError::Testimonial(_)
        ));
        assert!(matches!(
            ApiError::from(store::entities::EntityError::WriteReturnedNothing),
            ApiError::Entity(_)
        ));
        assert!(matches!(
            ApiError::from(store::jurisdictions::JurisdictionError::CodeTaken),
            ApiError::Jurisdictions(_)
        ));
        assert!(matches!(
            ApiError::from(store::entity_types::EntityTypeError::NameTaken),
            ApiError::EntityTypes(_)
        ));
        assert!(matches!(
            ApiError::from(store::participation::AddParticipantError::Duplicate),
            ApiError::AddParticipant(_)
        ));
        assert!(matches!(
            ApiError::from(store::participation::UpdateParticipantError::Duplicate),
            ApiError::UpdateParticipant(_)
        ));
        assert!(matches!(
            ApiError::from(store::participation::RemoveParticipantError::DriLockout),
            ApiError::RemoveParticipant(_)
        ));
        assert!(matches!(
            ApiError::from(store::project_references::AdversityError::NoClientDri),
            ApiError::Adversity(_)
        ));
        assert!(matches!(
            ApiError::from(crate::project_notation::CreateProjectNotationError::EmptyInput),
            ApiError::CreateNotation(_)
        ));
        assert!(matches!(
            ApiError::from(crate::retainer_walk::SendIntakeError::NoClient(Uuid::nil())),
            ApiError::SendIntake(_)
        ));
        assert!(matches!(
            ApiError::from(crate::review::ReviewCommentError::NotFound),
            ApiError::ReviewComment(_)
        ));
        assert!(matches!(
            ApiError::from(crate::expunge_request_route::RequestDeletionError::NoRequester),
            ApiError::RequestDeletion(_)
        ));
        assert!(matches!(
            ApiError::from(crate::contract_review_walk::ContractReviewError::NoPlaybook),
            ApiError::ContractReview(_)
        ));
        assert!(matches!(
            ApiError::from(store::projects::ProjectCommandError::NotFound),
            ApiError::Project(_)
        ));
        assert!(matches!(
            ApiError::from(store::projects::OpenMatterError::CodeConflict),
            ApiError::OpenMatter(_)
        ));
        assert!(matches!(
            ApiError::from(store::entity_commands::EntityCommandError::Conflict),
            ApiError::Entity(_)
        ));
        assert!(matches!(ApiError::from("x".to_string()), ApiError::Db(_)));
        assert!(matches!(
            ApiError::from(store::notations::NotationError::WriteReturnedNothing),
            ApiError::Notation(_)
        ));
        assert!(matches!(
            ApiError::from(store::documents::IngestError::WriteReturnedNothing),
            ApiError::Ingest(_)
        ));
        assert!(matches!(
            ApiError::from(crate::people_commands::PeopleCommandError::SendFailed),
            ApiError::Command(_)
        ));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn document_error_helpers_answer_typed_refusals() {
        use store::assets::{
            AssetError, AssignSlugError, ReclassifyAcceptedKindError, ReclassifyKindError,
            RepairStorageError, ReslugError,
        };
        let slug_error = || store::documents::DocumentSlugError::MissingOrMismatchedExtension {
            filename: "letter.pdf".into(),
            slug: "letter".into(),
        };
        let ok = |result: Result<axum::response::Response, ApiError>| {
            result.expect("a typed refusal, not an ApiError")
        };

        let assign: Vec<(AssignSlugError, StatusCode, &str)> = vec![
            (
                AssignSlugError::AlreadySlugged,
                StatusCode::CONFLICT,
                "slug_present",
            ),
            (
                AssignSlugError::ChainExists("a.pdf".into()),
                StatusCode::CONFLICT,
                "slug_taken",
            ),
            (
                AssignSlugError::MissingFilename,
                StatusCode::BAD_REQUEST,
                "filename_required",
            ),
            (
                AssignSlugError::InvalidSlug(slug_error()),
                StatusCode::BAD_REQUEST,
                "invalid_slug",
            ),
            (
                AssignSlugError::InvalidKind("x".into()),
                StatusCode::BAD_REQUEST,
                "invalid_kind",
            ),
        ];
        for (error, status, code) in assign {
            let (got, body) = response_json(ok(super::assign_slug_response(error))).await;
            assert_eq!((got, body["error"].as_str().unwrap()), (status, code));
        }
        assert!(matches!(
            super::assign_slug_response(AssignSlugError::NotOnProject),
            Err(ApiError::NotFound)
        ));
        assert!(matches!(
            super::assign_slug_response(AssignSlugError::Asset(AssetError::WriteReturnedNothing)),
            Err(ApiError::Asset(_))
        ));

        let reclassify: Vec<(ReclassifyKindError, StatusCode, &str)> = vec![
            (
                ReclassifyKindError::KindAccepted("letter".into()),
                StatusCode::CONFLICT,
                "kind_accepted",
            ),
            (
                ReclassifyKindError::ChainHasAcceptedKind("a.pdf".into()),
                StatusCode::CONFLICT,
                "kind_accepted",
            ),
            (
                ReclassifyKindError::InvalidKind("x".into()),
                StatusCode::BAD_REQUEST,
                "invalid_kind",
            ),
        ];
        for (error, status, code) in reclassify {
            let (got, body) = response_json(ok(super::reclassify_kind_response(error))).await;
            assert_eq!((got, body["error"].as_str().unwrap()), (status, code));
        }
        assert!(matches!(
            super::reclassify_kind_response(ReclassifyKindError::NotOnProject),
            Err(ApiError::NotFound)
        ));
        assert!(matches!(
            super::reclassify_kind_response(ReclassifyKindError::Asset(
                AssetError::WriteReturnedNothing
            )),
            Err(ApiError::Asset(_))
        ));

        let accepted: Vec<(ReclassifyAcceptedKindError, StatusCode, &str)> = vec![
            (
                ReclassifyAcceptedKindError::KindNotAccepted("x".into()),
                StatusCode::CONFLICT,
                "kind_not_accepted",
            ),
            (
                ReclassifyAcceptedKindError::KindUnchanged("letter".into()),
                StatusCode::CONFLICT,
                "kind_unchanged",
            ),
            (
                ReclassifyAcceptedKindError::InvalidKind("x".into()),
                StatusCode::BAD_REQUEST,
                "invalid_kind",
            ),
        ];
        for (error, status, code) in accepted {
            let (got, body) =
                response_json(ok(super::reclassify_accepted_kind_response(error))).await;
            assert_eq!((got, body["error"].as_str().unwrap()), (status, code));
        }
        assert!(matches!(
            super::reclassify_accepted_kind_response(ReclassifyAcceptedKindError::NotOnProject),
            Err(ApiError::NotFound)
        ));
        assert!(matches!(
            super::reclassify_accepted_kind_response(ReclassifyAcceptedKindError::Asset(
                AssetError::WriteReturnedNothing
            )),
            Err(ApiError::Asset(_))
        ));

        let reslug: Vec<(ReslugError, StatusCode, &str)> = vec![
            (ReslugError::NoSlug, StatusCode::BAD_REQUEST, "no_slug"),
            (
                ReslugError::MissingFilename,
                StatusCode::BAD_REQUEST,
                "filename_required",
            ),
            (
                ReslugError::SlugUnchanged("a.pdf".into()),
                StatusCode::CONFLICT,
                "slug_unchanged",
            ),
            (
                ReslugError::InvalidTarget(slug_error()),
                StatusCode::BAD_REQUEST,
                "invalid_slug",
            ),
            (
                ReslugError::TargetOccupied("a.pdf".into()),
                StatusCode::CONFLICT,
                "slug_taken",
            ),
        ];
        for (error, status, code) in reslug {
            let (got, body) = response_json(ok(super::reslug_response(error))).await;
            assert_eq!((got, body["error"].as_str().unwrap()), (status, code));
        }
        assert!(matches!(
            super::reslug_response(ReslugError::NotOnProject),
            Err(ApiError::NotFound)
        ));
        assert!(matches!(
            super::reslug_response(ReslugError::Asset(AssetError::WriteReturnedNothing)),
            Err(ApiError::Asset(_))
        ));

        let repair: Vec<(RepairStorageError, &str)> = vec![
            (RepairStorageError::ObjectCorrupt, "object_corrupt"),
            (RepairStorageError::NoSibling, "no_sibling"),
            (RepairStorageError::DigestMismatch, "digest_mismatch"),
        ];
        for (error, code) in repair {
            let (got, body) = response_json(ok(super::repair_storage_response(error))).await;
            assert_eq!(
                (got, body["error"].as_str().unwrap()),
                (StatusCode::CONFLICT, code)
            );
        }
        assert!(matches!(
            super::repair_storage_response(RepairStorageError::NotOnProject),
            Err(ApiError::NotFound)
        ));
        assert!(matches!(
            super::repair_storage_response(RepairStorageError::Asset(
                AssetError::WriteReturnedNothing
            )),
            Err(ApiError::Asset(_))
        ));
        assert!(matches!(
            super::repair_storage_response(RepairStorageError::Project(
                store::projects::ProjectStoreError::CodeTaken
            )),
            Err(ApiError::Db(_))
        ));
    }

    #[tokio::test]
    async fn review_actions_and_upload_helpers_answer_typed_bodies() {
        use crate::admin_contract_reviews::ReviewActionError as E;
        let id = Uuid::nil();
        let response = super::review_action_response(Ok(()), id).unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        for error in [E::NotFoundOrScoped, E::FindingNotFound] {
            assert!(matches!(
                super::review_action_response(Err(error), id),
                Err(ApiError::NotFound)
            ));
        }
        for (error, status, code) in [
            (E::NotOpen, StatusCode::CONFLICT, "not_open"),
            (
                E::FindingsUnacted,
                StatusCode::UNPROCESSABLE_ENTITY,
                "findings_unacted",
            ),
            (
                E::DocumentCommentsUnacted,
                StatusCode::UNPROCESSABLE_ENTITY,
                "document_comments_unacted",
            ),
        ] {
            let (got, body) =
                response_json(super::review_action_response(Err(error), id).unwrap()).await;
            assert_eq!((got, body["error"].as_str().unwrap()), (status, code));
        }
        assert!(matches!(
            super::review_action_response(Err(E::Db("down".into())), id),
            Err(ApiError::Db(_))
        ));

        let (status, body) =
            response_json(super::upload_session_expired("2026-01-01T00:00:00Z")).await;
        assert_eq!(status, StatusCode::GONE);
        assert_eq!(body["error"], "upload_session_expired");
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("2026-01-01T00:00:00Z"));

        let (status, body) = response_json(super::upload_sha_mismatch("aa", "bb")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"], "sha_mismatch");
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("aa, not the declared bb"));

        let (status, body) = response_json(super::conflict("c", "m")).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body, serde_json::json!({ "error": "c", "message": "m" }));

        assert!(super::invalid_document_slug_response("letter.pdf", "letter.pdf").is_none());
        let (status, body) =
            response_json(super::invalid_document_slug_response("letter.pdf", "letter").unwrap())
                .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_slug");
    }

    #[tokio::test]
    async fn small_pure_helpers_hold_their_contracts() {
        use crate::eml_filing::EmlParseError;
        assert_eq!(
            super::eml_parse_error_code(&EmlParseError::NotMime),
            "not_mime"
        );
        assert_eq!(
            super::eml_parse_error_code(&EmlParseError::NoDate),
            "no_date"
        );
        assert_eq!(
            super::eml_parse_error_code(&EmlParseError::BadDate),
            "bad_date"
        );
        assert_eq!(
            super::eml_parse_error_code(&EmlParseError::NoBody),
            "no_body"
        );

        assert!(super::is_commit_sha("abcdef0"));
        assert!(super::is_commit_sha(&"a".repeat(64)));
        assert!(!super::is_commit_sha("abcdef"));
        assert!(!super::is_commit_sha(&"a".repeat(65)));
        assert!(!super::is_commit_sha("ABCDEF0"));
        assert!(!super::is_commit_sha("abcdefg"));

        assert_eq!(super::typst_string("a\"b\\c"), "#\"a\\\"b\\\\c\"");

        let depth = |raw: Option<&str>| {
            super::graph_depth(&super::GraphQuery {
                depth: raw.map(str::to_string),
            })
        };
        assert_eq!(depth(None).unwrap(), 2);
        assert_eq!(depth(Some("3")).unwrap(), 3);
        for (raw, code) in [
            ("0", "depth_out_of_range"),
            ("4", "depth_out_of_range"),
            ("-1", "invalid_depth"),
            ("two", "invalid_depth"),
        ] {
            let (status, body) = response_json(depth(Some(raw)).unwrap_err()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{raw}");
            assert_eq!(body["error"], code, "{raw}");
        }

        assert!(matches!(
            super::surface_error(store::project_surfaces::SurfaceError::NotFound),
            ApiError::NotFound
        ));
        assert!(matches!(
            super::surface_error(store::project_surfaces::SurfaceError::Command(
                store::projects::ProjectCommandError::NotFound
            )),
            ApiError::Project(_)
        ));
        assert!(matches!(
            super::surface_error(store::project_surfaces::SurfaceError::ForgejoUnavailable(
                "root url".into()
            )),
            ApiError::ForgejoUnavailable(_)
        ));
        assert!(matches!(
            super::surface_error(store::project_surfaces::SurfaceError::Db("down".into())),
            ApiError::Db(_)
        ));

        let lawyer = crate::SessionData::fresh("l", store::persons::Role::Lawyer);
        let admin = crate::SessionData::fresh("a", store::persons::Role::Admin);
        assert!(!super::may_change_roles(&lawyer));
        assert!(super::may_change_roles(&admin));
    }

    /// The door and the refusal read the same records through the same
    /// checks (`store::private_terms`), so the report's kind must match the
    /// class the private-terms response refuses with — for every failure
    /// class, on the same broken records.
    // One matter per finding kind: the door's report and the private-terms
    // refusal must classify the same broken records identically, so neither
    // can drift from the other.

    // Each matter breaks a different way, the way an orphan arrives: out of
    // band, never through the in-band command boundary.

    #[tokio::test]
    async fn the_doc_routes_serve_the_explorer_and_the_document() {
        use tower::ServiceExt;
        for path in ["/app/api", "/api"] {
            let response = super::doc_routes()
                .oneshot(
                    axum::http::Request::builder()
                        .uri(path)
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let csp = response
                .headers()
                .get(axum::http::header::CONTENT_SECURITY_POLICY)
                .expect("the explorer carries a CSP")
                .to_str()
                .unwrap();
            assert!(csp.contains("script-src 'self'"));
            assert!(csp.contains("frame-ancestors 'none'"));
        }
        let response = super::doc_routes()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/app/api/openapi.json")
                    .header("host", "navigator.example.com")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let (status, json) = response_json(response).await;
        assert_eq!(status, StatusCode::OK);
        assert!(json["paths"].is_object());
    }
}
