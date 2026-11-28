//! The Firm overview and its Finances dashboard.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{Choice, ChoiceGroup, ChoiceGroupOption, Field, FormCard, Heading};
use crate::people::ViewerRole;

/// The path segment base; `{id}` is appended by the route and by every link
/// into this page. Lives under `/app/admin`, not `/app/owner`: embedded Rego
/// carves out every `/app/owner/*` path as Owner-only (`owner_only_path` in
/// `portal/policy/navigator.rego`), but this page must admit an Admin
/// scoped to their own Firm too — the fine-grained check is
/// `store::firm_capability::FirmCapability::ViewDirectory`, not the route.
pub const FIRM_SHOW_PATH: &str = "/app/admin/firms";

/// The `?secret_error=`/`secret_saved=`/`secret_revoked=` flash the
/// create/replace/revoke doors in `portal::admin` redirect back with.
/// Post/redirect/get, exactly like [`crate::firm_edit::FirmEditQuery`] —
/// nothing about the submitted value rides in the query, only an outcome.
#[derive(Deserialize, Serialize, Clone, Default, PartialEq, Eq)]
pub struct FirmShowQuery {
    #[serde(default)]
    pub secret_error: Option<String>,
    #[serde(default)]
    pub secret_saved: Option<String>,
    #[serde(default)]
    pub secret_revoked: Option<String>,
    #[serde(default)]
    pub summary_error: Option<String>,
    #[serde(default)]
    pub summary_saved: Option<String>,
    #[serde(default)]
    pub summary_revoked: Option<String>,
    /// Why a Remove admin request was refused.
    #[serde(default)]
    pub member_error: Option<String>,
    #[serde(default)]
    pub integration_error: Option<String>,
}

/// One row of the Firm's integration-secret metadata (ENG-491) — the
/// Doppler-like listing. Never a value, ciphertext, or wrapped key.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct SecretMetadataRow {
    pub provider: String,
    pub kind: String,
    pub status: String,
    pub version: i64,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct SummaryApprovalRow {
    pub id: String,
    pub project_id: Option<String>,
    pub provider_account: String,
    pub sender_email: String,
    pub status: String,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct SummaryApprovalEventRow {
    pub actor_person_id: String,
    pub project_id: Option<String>,
    pub provider_account: String,
    pub sender_email: String,
    pub action: String,
    pub inserted_at: String,
}

/// One person's membership on this Firm.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct MemberRow {
    pub person_id: String,
    pub name: String,
    pub email: String,
    /// `admin` / `lawyer` / `clerk`.
    pub membership: String,
    /// The Admin-DRI marker — read-only here; only
    /// `store::firms::appoint_admin_dri` writes it (ENG-499).
    pub is_dri: bool,
}

/// Everything the Firm detail view renders.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct FirmShowView {
    #[serde(default)]
    pub tokens_href: String,
    #[serde(default)]
    pub firm_name: String,
    pub role: ViewerRole,
    #[serde(default)]
    pub logo: Option<crate::components::AppLogo>,
    /// The Firm id from the path. Present even on a not-found render, so the
    /// not-found copy can still name what was asked for.
    pub id: String,
    /// `None` when the id resolves to no Firm this caller may see — a
    /// missing Firm and a Firm outside the caller's own membership render
    /// identically, so neither discloses which one it was
    /// (`docs/access-model.md`).
    pub fields: Option<FirmFields>,
    #[serde(default)]
    pub finances: Option<FirmFinances>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct FirmFinances {
    pub name: String,
    pub invoices: crate::firm_invoice_graphs::FirmInvoiceGraphsView,
    pub projected_cash: crate::projected_cash::FirmProjectedCashView,
    pub calibration: crate::outcome_calibration::OutcomeCalibrationView,
}

/// The Firm's own facts, present only when [`FirmShowView::fields`] is `Some`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct FirmFields {
    pub name: String,
    pub status: String,
    pub entity_name: String,
    /// The linked Entity's employer identification number.
    #[serde(default)]
    pub ein: Option<String>,
    #[serde(default)]
    pub esign_provider: String,
    #[serde(default)]
    pub workspace_provider: String,
    #[serde(default)]
    pub website_url: Option<String>,
    pub brand_keys: Vec<String>,
    /// The current Admin DRI's name and email, when exactly one eligible
    /// designation exists.
    pub admin_dri: Option<(String, String)>,
    /// A human-readable Admin-DRI problem (missing / multiple / ineligible)
    /// from `store::firms::admin_dri_invariant_report`, scoped to this one
    /// Firm — `None` when the invariant holds.
    pub admin_dri_problem: Option<String>,
    pub members: Vec<MemberRow>,
    /// Whether this caller may reach `/app/admin/firms/{id}/edit` — the same
    /// `FirmCapability::ManageMembership` check `store::firms::update` itself
    /// authorizes against (ENG-585). `false` hides the Edit link rather than
    /// rendering it toward a refusal.
    #[serde(default)]
    pub can_edit: bool,
    #[serde(default)]
    pub can_remove_admin: bool,
    /// The Firm's integration-secret metadata (ENG-491) — populated only when
    /// `FirmCapability::ViewIntegrationSecretMetadata` resolves `Allowed` for
    /// this caller (Owner, or the Firm's own Admin DRI). Empty for anyone
    /// else, and the section renders nothing rather than an empty table in
    /// that case (see [`Self::can_view_secrets`]).
    #[serde(default)]
    pub secrets: Vec<SecretMetadataRow>,
    #[serde(default)]
    pub docusign_review: Vec<(String, String)>,
    /// Whether this caller may even see the metadata section — distinct from
    /// `secrets.is_empty()`, which is also true for a DRI who has configured
    /// nothing yet.
    #[serde(default)]
    pub can_view_secrets: bool,
    /// Whether this caller may create/replace/revoke — `ManageIntegrationSecrets`,
    /// which only the Firm's own Admin DRI holds. Owner can view metadata
    /// above but gets this only when it is itself the Firm's Admin DRI:
    /// Owner appoints the DRI and is not an ordinary secret writer.
    #[serde(default)]
    pub can_manage_secrets: bool,
    #[serde(default)]
    pub summary_approvals: Vec<SummaryApprovalRow>,
    #[serde(default)]
    pub summary_dri_choices: Vec<(String, String)>,
    #[serde(default)]
    pub summary_approval_history: Vec<SummaryApprovalEventRow>,
    #[serde(default)]
    pub can_manage_summary_approvals: bool,
    #[serde(default)]
    pub csrf_token: String,
    #[serde(default)]
    pub member_error: Option<String>,
    #[serde(default)]
    pub integration_error: Option<String>,
    #[serde(default)]
    pub secret_error: Option<String>,
    #[serde(default)]
    pub secret_saved: Option<String>,
    #[serde(default)]
    pub secret_revoked: Option<String>,
    #[serde(default)]
    pub summary_error: Option<String>,
    #[serde(default)]
    pub summary_saved: Option<String>,
    #[serde(default)]
    pub summary_revoked: Option<String>,
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

/// The closed secret-kind vocabulary as `(slug, label)` pairs for the "add a
/// secret" select. Plain strings rather than `store::firm_secrets` types: this
/// function also runs in the client wasm bundle (pre-hydration), which does
/// not link `store`. Kept in the same order as
/// `store::firm_secrets::ALL_KINDS`; a store test pins that the sets agree.
fn secret_kind_choices() -> Vec<Choice> {
    vec![
        Choice::new("xero_client_id", "Xero client ID"),
        Choice::new("xero_client_secret", "Xero client secret"),
        Choice::new("xero_refresh_token", "Xero refresh token"),
        Choice::new("slack_bot_token", "Slack bot token"),
        Choice::new("notion_token", "Notion token"),
        Choice::new("github_app_private_key", "GitHub App private key"),
        Choice::new("forgejo_access_token", "Forgejo access token"),
        Choice::new("twilio_auth_token", "Twilio auth token"),
        Choice::new("docusign_connection", "DocuSign connection"),
        Choice::new("stripe_nevada_app_key", "STRIPE_NEVADA_APP_KEY"),
        Choice::new("stripe_operating_app_key", "STRIPE_OPERATING_APP_KEY"),
    ]
}

#[cfg(feature = "server")]
fn admin_dri_problem_text(problem: &store::firms::AdminDriProblem) -> String {
    match problem {
        store::firms::AdminDriProblem::Missing => "No Admin DRI is designated.".to_string(),
        store::firms::AdminDriProblem::Multiple(ids) => {
            format!("{} people hold the Admin DRI designation.", ids.len())
        }
        store::firms::AdminDriProblem::Ineligible(_) => {
            "The designated Admin DRI no longer holds an eligible admin membership.".to_string()
        }
    }
}

/// Load the Firm detail page for the `{id}` in the request path.
///
/// Admission mirrors `store::firm_capability::FirmCapability::ViewDirectory`:
/// Owner sees every Firm; an Admin sees only a Firm they hold a
/// `person_firm_role` on. A Firm outside that reach renders the same
/// `fields: None` a nonexistent id would, so neither discloses the other.
#[server]
#[cfg_attr(feature = "server", allow(clippy::too_many_lines))]
pub async fn get_firm_show() -> Result<FirmShowView, ServerFnError> {
    let role = crate::admin_listing::require_admin().await?;
    let axum::extract::Path(id) =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Path<uuid::Uuid>, _>()
            .await?;
    let application_store = consume_context::<store::application_store::ApplicationStore>();
    let actor_person_id = crate::admin_listing::injected_person_id().await;

    let decision = store::firm_capability::resolve(
        &application_store,
        store_role(role),
        actor_person_id,
        id,
        store::firm_capability::FirmCapability::ViewDirectory,
    )
    .await
    .map_err(|error| ServerFnError::new(error.to_string()))?;

    let base = FirmShowView {
        tokens_href: crate::app_chrome::app_tokens_href_from_context().await,
        firm_name: crate::app_chrome::firm_name_from_context().await,
        logo: crate::app_chrome::app_logo_from_context().await,
        role,
        id: id.to_string(),
        fields: None,
        finances: None,
    };

    if !decision.is_allowed() {
        dioxus_fullstack_core::FullstackContext::commit_http_status(
            axum::http::StatusCode::NOT_FOUND,
            None,
        );
        return Ok(base);
    }

    let Some(firm) = store::firms::find_by_id(&application_store, id)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?
    else {
        dioxus_fullstack_core::FullstackContext::commit_http_status(
            axum::http::StatusCode::NOT_FOUND,
            None,
        );
        return Ok(base);
    };

    let mut base = base;
    let website_url = crate::app_chrome::firm_settings_brand(&application_store, &firm)
        .await
        .map(|(url, mark)| {
            base.logo = mark.logo;
            base.tokens_href = mark.tokens_href;
            base.firm_name = mark.firm_name;
            url
        });
    let uri = dioxus_fullstack_core::FullstackContext::extract::<axum::http::Uri, _>().await?;
    if uri.path().ends_with("/finances") {
        let invoices = store::xero_invoices::firm_thirty_day_rollup(
            &application_store,
            firm.id,
            chrono::Utc::now(),
        )
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?
        .into();
        let projected_cash =
            crate::projected_cash::derive::load_firm(&application_store, actor_person_id, firm.id)
                .await;
        let calibration = crate::outcome_calibration::derive::load_firm(
            &application_store,
            actor_person_id,
            firm.id,
        )
        .await;
        return Ok(FirmShowView {
            finances: Some(FirmFinances {
                name: firm.name,
                invoices,
                projected_cash,
                calibration,
            }),
            ..base
        });
    }

    let entity = match firm.entity_id {
        Some(entity_id) => store::entities::find_by_id(&application_store, entity_id)
            .await
            .map_err(|error| ServerFnError::new(error.to_string()))?,
        None => None,
    };
    let entity_name = entity.as_ref().map_or_else(
        || "Unlinked entity".to_string(),
        |entity| entity.name.clone(),
    );
    let ein = entity
        .as_ref()
        .and_then(|entity| entity.metadata.as_ref())
        .and_then(|metadata| metadata.get("ein"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let esign_provider = store::firm_esign::for_firm(firm.id)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?
        .as_str()
        .to_string();
    let workspace_provider = store::firms::workspace_provider(id)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?;
    let brand_keys = store::firms::brand_keys_for_firm(&application_store, firm.id)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?;

    let mut members = Vec::new();
    let mut admin_dri = None;
    // `visible_person_ids`/`memberships_for_person` answer "which Firms a
    // person belongs to"; this page asks the inverse — "which people belong
    // to this Firm" — so it reads `person_firm_role` directly rather than
    // bending an existing helper to a question it does not ask.
    for row in store::firms::memberships_for_firm(&application_store, firm.id)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?
    {
        let Some(person) = store::persons::find_by_id(&application_store, row.person_id)
            .await
            .map_err(|error| ServerFnError::new(error.to_string()))?
        else {
            continue;
        };
        if row.is_dri {
            admin_dri = Some((person.name.clone(), person.email.clone()));
        }
        members.push(MemberRow {
            person_id: row.person_id.to_string(),
            name: person.name,
            email: person.email,
            membership: row.membership.as_str().to_string(),
            is_dri: row.is_dri,
        });
    }

    let admin_dri_problem = store::firms::admin_dri_invariant_report(&application_store)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?
        .into_iter()
        .find(|status| status.firm_id == firm.id)
        .and_then(|status| status.problem)
        .as_ref()
        .map(admin_dri_problem_text);

    // The Edit link is offered only to a caller who could actually reach the
    // edit page — the same `ManageMembership` capability `store::firms::update`
    // itself authorizes against, resolved separately from `ViewDirectory`
    // above (Owner holds both; a Firm's non-Admin membership holds only the
    // first).
    let can_edit = matches!(
        store::firm_capability::resolve(
            &application_store,
            store_role(role),
            actor_person_id,
            id,
            store::firm_capability::FirmCapability::ManageMembership,
        )
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?,
        store::firm_capability::FirmCapabilityDecision::Allowed
    );

    let can_remove_admin = store::firm_capability::FirmCapability::RemoveAdmin
        .resolve(&application_store, store_role(role), actor_person_id, id)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?
        .is_allowed();

    // Integration-secret metadata (ENG-491): a separate, stricter capability
    // than `ViewDirectory` above. Owner holds it (governance without
    // plaintext); a non-DRI Admin does not, even though they can see
    // everything else on this page.
    let can_view_secrets = matches!(
        store::firm_capability::resolve(
            &application_store,
            store_role(role),
            actor_person_id,
            id,
            store::firm_capability::FirmCapability::ViewIntegrationSecretMetadata,
        )
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?,
        store::firm_capability::FirmCapabilityDecision::Allowed
    );
    let secrets = if can_view_secrets {
        store::firm_secrets::list_metadata_for_firm(
            &application_store,
            store_role(role),
            actor_person_id,
            id,
        )
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?
        .into_iter()
        .map(|m| SecretMetadataRow {
            provider: m.provider.as_str().to_string(),
            kind: m.kind.as_str().to_string(),
            status: m.status,
            version: m.version,
            updated_at: m.updated_at,
        })
        .collect()
    } else {
        Vec::new()
    };
    let can_manage_secrets = can_view_secrets
        && matches!(
            store::firm_capability::resolve(
                &application_store,
                store_role(role),
                actor_person_id,
                id,
                store::firm_capability::FirmCapability::ManageIntegrationSecrets,
            )
            .await
            .map_err(|error| ServerFnError::new(error.to_string()))?,
            store::firm_capability::FirmCapabilityDecision::Allowed
        );
    let can_manage_summary_approvals = matches!(
        store::firm_capability::resolve(
            &application_store,
            store_role(role),
            actor_person_id,
            id,
            store::firm_capability::FirmCapability::ManageSummaryApprovals,
        )
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?,
        store::firm_capability::FirmCapabilityDecision::Allowed
    );
    let summary_approvals = if can_manage_summary_approvals {
        store::summary_sender_approvals::list_for_firm(
            &application_store,
            store_role(role),
            actor_person_id,
            id,
        )
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?
        .into_iter()
        .map(|approval| SummaryApprovalRow {
            id: approval.id.to_string(),
            project_id: approval.project_id.map(|id| id.to_string()),
            provider_account: approval.provider_account,
            sender_email: approval.sender_email,
            status: approval.status,
            updated_at: approval.updated_at.to_rfc3339(),
        })
        .collect()
    } else {
        Vec::new()
    };
    let summary_dri_choices = if can_manage_summary_approvals {
        let mut choices = Vec::new();
        for project in store::projects::all(&application_store)
            .await
            .map_err(|error| ServerFnError::new(error.to_string()))?
            .into_iter()
            .filter(|project| project.firm_id == Some(id))
        {
            let participants =
                store::projects::participations_for_project(&application_store, project.id)
                    .await
                    .map_err(|error| ServerFnError::new(error.to_string()))?;
            let people = store::persons::find_by_ids(
                &application_store,
                &participants
                    .iter()
                    .filter(|participant| {
                        participant.participation == "client" && participant.is_client_dri
                    })
                    .map(|participant| participant.person_id)
                    .collect::<Vec<_>>(),
            )
            .await
            .map_err(|error| ServerFnError::new(error.to_string()))?;
            for person in people.into_iter().filter(|person| {
                person.role == store::persons::Role::Client && person.email_confirmed
            }) {
                choices.push((
                    format!("{}:{}", project.id, person.email),
                    format!("{} — {} ({})", project.name, person.name, person.email),
                ));
            }
        }
        choices
    } else {
        Vec::new()
    };
    let summary_approval_history = if can_manage_summary_approvals {
        store::summary_sender_approvals::audit_history_for_firm(
            &application_store,
            store_role(role),
            actor_person_id,
            id,
        )
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?
        .into_iter()
        .map(|event| SummaryApprovalEventRow {
            actor_person_id: event.actor_person_id.to_string(),
            project_id: event.project_id.map(|id| id.to_string()),
            provider_account: event.provider_account,
            sender_email: event.sender_email,
            action: event.action,
            inserted_at: event.inserted_at.to_rfc3339(),
        })
        .collect()
    } else {
        Vec::new()
    };
    let docusign_review = if can_view_secrets {
        let unavailable = || ServerFnError::new("DocuSign completion review unavailable");
        let pg = store::postgres::PostgresDb::application()
            .await
            .map_err(|_| unavailable())?;
        store::docusign_completion::review_queue(&pg, id)
            .await
            .map_err(|_| unavailable())?
            .into_iter()
            .map(|(envelope, reason)| (envelope.to_string(), reason.replace('_', " ")))
            .collect()
    } else {
        Vec::new()
    };
    let csrf_token = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<crate::csrf::CsrfToken>,
        _,
    >()
    .await
    .map(|axum::Extension(token)| token.0)
    .unwrap_or_default();
    let FirmShowQuery {
        member_error,
        integration_error,
        secret_error,
        secret_saved,
        secret_revoked,
        summary_error,
        summary_saved,
        summary_revoked,
    } = dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Query<FirmShowQuery>, _>(
    )
    .await
    .map(|axum::extract::Query(q)| q)
    .unwrap_or_default();

    Ok(FirmShowView {
        fields: Some(FirmFields {
            name: firm.name,
            status: firm.status,
            entity_name,
            ein,
            esign_provider,
            workspace_provider,
            website_url,
            brand_keys,
            admin_dri,
            admin_dri_problem,
            members,
            can_edit,
            can_remove_admin,
            secrets,
            docusign_review,
            can_view_secrets,
            can_manage_secrets,
            csrf_token,
            member_error,
            integration_error,
            secret_error,
            secret_saved,
            secret_revoked,
            summary_approvals,
            summary_dri_choices,
            summary_approval_history,
            can_manage_summary_approvals,
            summary_error,
            summary_saved,
            summary_revoked,
        }),
        ..base
    })
}

/// The route entry for `{FIRM_SHOW_PATH}/{id}`.
#[component]
pub fn FirmShow() -> Element {
    let resource = use_server_future(get_firm_show)?;
    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        Some(Err(_)) => {
            return rsx! {
                main { id: "firm-show", p { "Failed to load the firm." } }
            }
        }
        None => {
            return rsx! {
                main { id: "firm-show", p { "Loading…" } }
            }
        }
    };
    firm_show_body(&view)
}

/// The loaded page. Split from the component so tests render a fixed view.
#[allow(clippy::too_many_lines)]
pub fn firm_show_body(view: &FirmShowView) -> Element {
    let role = view.role;
    let firm_name = view.firm_name.clone();
    let title = format!("{firm_name} | Owner | Firms");
    // Owner has a Firms inventory to return to; an Admin — who can reach
    // this page for their own Firm but not the Owner-only `/app/owner`
    // listing — goes back to the admin console home instead.
    let back_href = if role.is_owner() {
        "/app/owner"
    } else {
        "/app/admin"
    };

    if let Some(finances) = &view.finances {
        return firm_finances_body(view, finances);
    }

    let Some(fields) = view.fields.as_ref() else {
        return rsx! {
            document::Title { "{title} | Not found" }
            document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
            document::Stylesheet { href: "{view.tokens_href}" }
            crate::components::AppNavbar {
                destinations: crate::app_chrome::app_destinations(role),
                logo: view.logo.clone(),
            }
            main { id: "firm-show",
                h1 { "Firm not found" }
                p { a { href: "{back_href}", "← Back" } }
            }
        };
    };

    let brands = crate::brand_website::attached_lines(&fields.brand_keys);

    rsx! {
        document::Title { "{title}" }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{view.tokens_href}" }
        crate::components::AppNavbar {
            destinations: crate::app_chrome::app_destinations(role),
            logo: view.logo.clone(),
        }
        main { id: "firm-show", class: "nav-theme",
            header { class: "page-header",
                h1 { "{fields.name}" }
                p { class: "page-subtitle",
                    "Entity: {fields.entity_name}. Status: {fields.status}."
                }
                if let Some(ein) = &fields.ein {
                    p { class: "firm-ein", style: "font-size: 1.25rem; font-weight: 600;",
                        "Employer identification number (EIN): "
                        span { style: "font-variant-numeric: tabular-nums; user-select: all;", "{ein}" }
                    }
                }
                if let Some(url) = &fields.website_url {
                    p { a { class: "nav-link", href: "{url}", "{url}" } }
                }
                if fields.can_edit {
                    p {
                        a {
                            class: "nav-btn nav-btn--secondary",
                            href: "{FIRM_SHOW_PATH}/{view.id}/edit",
                            "Edit",
                        }
                    }
                }
                // The firm-graph page, the aggregated read-only neighborhood
                // of this firm — a lawyer-tier view, independent of edit
                // permission; the destination's policy rule gates it.
                p {
                    a { class: "nav-btn nav-btn--secondary", href: "/app/lawyer/firms/{view.id}/graph", "Relationship graph" }
                    " "
                    a { class: "nav-btn nav-btn--secondary", href: "{FIRM_SHOW_PATH}/{view.id}/finances", "Finances" }
                }
            }
            section { id: "firm-brands",
                h2 { "Brands" }
                p { "{brands}" }
            }
            section { id: "firm-admin-dri",
                h2 { "Admin DRI" }
                match (&fields.admin_dri, &fields.admin_dri_problem) {
                    (Some((name, email)), None) => rsx! {
                        p { "{name} ({email})" }
                    },
                    (_, Some(problem)) => rsx! {
                        p { class: "nav-form-error", role: "alert", "{problem}" }
                    },
                    (None, None) => rsx! {
                        p { "No Admin DRI is designated." }
                    },
                }
            }
            {firm_integrations(view, fields)}
            section { id: "firm-social",
                h2 { "Social publishing" }
                p { "Provider app keys are shared by the production publishing service. Each Brand connects its own accounts and keeps its own encrypted account tokens." }
                p { a { href: "/app/admin/brands", "Manage Brand accounts" } " · "
                    a { href: "/app/social", "Social" } " · "
                    a { href: "/app/social/calendar", "Calendar" }
                }
                if role == ViewerRole::Owner {
                    p { a { href: crate::deployment_keys::PATH, "Manage shared social app keys" } }
                } else {
                    p { class: "nav-muted", "An Owner manages shared app keys through the production SOPS workflow." }
                }
            }
            if fields.can_view_secrets && !fields.docusign_review.is_empty() {
                section { id: "docusign-review",
                    h2 { "DocuSign completion review" }
                    p { "Use the envelope ID to verify the Project in DocuSign, then adopt the envelope from that Project." }
                    ul {
                        for (envelope, reason) in fields.docusign_review.iter() {
                            li { key: "{envelope}", "{envelope} · {reason}" }
                        }
                    }
                }
            }
            if fields.can_view_secrets {
                section { id: "firm-secrets",
                    h2 { "Integration secrets" }
                    p { class: "nav-muted",
                        "Write-only: a value is never shown again after it is submitted. \
                         Revoking here stops Navigator from using the credential; it does \
                         not revoke access on the provider's own side."
                    }
                    if let Some(error) = fields.secret_error.as_ref() {
                        p { class: "nav-form-error", role: "alert", "{error}" }
                    }
                    if let Some(kind) = fields.secret_saved.as_ref() {
                        p { role: "status", "Saved {kind}." }
                    }
                    if let Some(kind) = fields.secret_revoked.as_ref() {
                        p { role: "status", "Revoked {kind}." }
                    }
                    if fields.secrets.is_empty() {
                        p { "No integration secrets are configured yet." }
                    } else {
                        div { class: "nav-table-wrap",
                            table { class: "nav-table",
                                thead {
                                    tr {
                                        th { "Provider" }
                                        th { "Kind" }
                                        th { "Status" }
                                        th { "Version" }
                                        th { "Updated" }
                                        if fields.can_manage_secrets {
                                            th { "" }
                                        }
                                    }
                                }
                                tbody {
                                    for row in fields.secrets.iter().cloned() {
                                        tr { class: "firm-secret-row", key: "{row.provider}-{row.kind}-{row.version}",
                                            td { "{row.provider}" }
                                            td { "{row.kind}" }
                                            td { "{row.status}" }
                                            td { "{row.version}" }
                                            td { "{row.updated_at}" }
                                            if fields.can_manage_secrets {
                                                td {
                                                    form {
                                                        class: "nav-form",
                                                        method: "post",
                                                        action: "/app/admin/firms/{view.id}/secrets/revoke",
                                                        "aria-label": "Revoke {row.kind}",
                                                        input { r#type: "hidden", name: "_csrf", value: "{fields.csrf_token}" }
                                                        input { r#type: "hidden", name: "kind", value: "{row.kind}" }
                                                        button { class: "nav-btn nav-btn--secondary", r#type: "submit",
                                                            if row.kind == "twilio_auth_token" {
                                                                "Revoke all Twilio tokens"
                                                            } else {
                                                                "Revoke"
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
                    }
                    if fields.can_manage_secrets {
                        p { class: "page-subtitle",
                            "Saving a Twilio token keeps earlier versions active. Revoke all Twilio tokens disables every active Twilio token for this Firm."
                        }
                        FormCard {
                            title: "Add or replace a secret".to_string(),
                            action: format!("/app/admin/firms/{}/secrets", view.id),
                            submit_label: "Save".to_string(),
                            heading: Heading::H2,
                            csrf_token: Some(fields.csrf_token.clone()),
                            fields: vec![
                                Field::select("Secret", "kind", secret_kind_choices(), None),
                                Field::password("Value", "value").required(),
                            ],
                        }
                    }
                }
            }
            if fields.can_manage_summary_approvals {
                section { id: "firm-summary-approvals",
                    h2 { "Email summary approvals" }
                    p { class: "nav-muted", "Only a current, email-confirmed Client DRI on one of this Firm’s Projects can be approved for the configured provider account. Every change is audited." }
                    if let Some(error) = fields.summary_error.as_ref() { p { class: "nav-form-error", role: "alert", "{error}" } }
                    if let Some(email) = fields.summary_saved.as_ref() { p { role: "status", "Approved {email}." } }
                    if let Some(email) = fields.summary_revoked.as_ref() { p { role: "status", "Revoked {email}." } }
                    if fields.summary_approvals.is_empty() { p { "No email summary senders are approved." } }
                    else { div { class: "nav-table-wrap", table { class: "nav-table",
                        thead { tr { th { "Provider account" } th { "Sender" } th { "Status" } th { "Updated" } th { "" } } }
                        tbody { for row in fields.summary_approvals.iter().cloned() {
                            tr { key: "{row.id}", td { "{row.provider_account}" } td { "{row.sender_email}" } td { "{row.status}" } td { "{row.updated_at}" }
                                td { if row.status == "approved" { form { method: "post", action: "/app/admin/firms/{view.id}/summary-approvals/revoke",
                                    input { r#type: "hidden", name: "_csrf", value: "{fields.csrf_token}" }
                                    input { r#type: "hidden", name: "approval_id", value: "{row.id}" }
                                    button { class: "nav-btn nav-btn--secondary", r#type: "submit", "Revoke" }
                                } } }
                            }
                        } }
                    } } }
                    h3 { "Approval history" }
                    if fields.summary_approval_history.is_empty() { p { "No approval changes recorded." } }
                    else { div { class: "nav-table-wrap", table { class: "nav-table",
                        thead { tr { th { "Action" } th { "Sender" } th { "Provider account" } th { "Actor ID" } th { "Time" } } }
                        tbody { for event in fields.summary_approval_history.iter().cloned() {
                            tr { key: "{event.actor_person_id}-{event.inserted_at}", td { "{event.action}" } td { "{event.sender_email}" } td { "{event.provider_account}" } td { "{event.actor_person_id}" } td { "{event.inserted_at}" } }
                        } }
                    } } }
                    FormCard {
                        title: "Approve a sender".to_string(),
                        action: format!("/app/admin/firms/{}/summary-approvals", view.id),
                        submit_label: "Approve sender".to_string(),
                        heading: Heading::Hidden,
                        csrf_token: Some(fields.csrf_token.clone()),
                        fields: vec![
                            Field::input("Provider account", "provider_account", "", "text").required(),
                            Field::select("Project Client DRI", "project_sender", fields.summary_dri_choices.iter().map(|(value, label)| Choice::new(value.clone(), label.clone())).collect(), None),
                        ],
                    }
                }
            }
            section { id: "firm-members",
                h2 { "People" }
                if let Some(error) = fields.member_error.as_ref() {
                    p { class: "nav-form-error", role: "alert", "{error}" }
                }
                if fields.members.is_empty() {
                    p { "No one holds a membership on this Firm yet." }
                } else {
                    div { class: "nav-table-wrap",
                        table { class: "nav-table",
                            thead {
                                tr {
                                    th { "Name" }
                                    th { "Email" }
                                    th { "Membership" }
                                    th { "Admin DRI" }
                                    if fields.can_remove_admin {
                                        th { "" }
                                    }
                                }
                            }
                            tbody {
                                for member in fields.members.iter().cloned() {
                                    tr { class: "firm-member-row", key: "{member.person_id}",
                                        td { "{member.name}" }
                                        td { "{member.email}" }
                                        td { "{member.membership}" }
                                        td { if member.is_dri { "Yes" } else { "" } }
                                        if fields.can_remove_admin {
                                            td {
                                                if member.membership == "admin" && !member.is_dri {
                                                    form {
                                                        class: "nav-form",
                                                        method: "post",
                                                        action: "/app/admin/firms/{view.id}/members/remove-admin",
                                                        "aria-label": "Remove admin from {member.name}",
                                                        input { r#type: "hidden", name: "_csrf", value: "{fields.csrf_token}" }
                                                        input { r#type: "hidden", name: "person_id", value: "{member.person_id}" }
                                                        button { class: "nav-btn nav-btn--secondary", r#type: "submit", "Remove admin" }
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
            }
            p { a { href: "{back_href}", "← Back" } }
        }
    }
}

fn firm_finances_body(view: &FirmShowView, finances: &FirmFinances) -> Element {
    rsx! {
        document::Title { "{finances.name} | Finances" }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{view.tokens_href}" }
        crate::components::AppNavbar {
            destinations: crate::app_chrome::app_destinations(view.role),
            logo: view.logo.clone(),
        }
        main { id: "firm-finances", class: "nav-theme",
            header { class: "page-header",
                p { a { class: "nav-link", href: "{FIRM_SHOW_PATH}/{view.id}", "{finances.name}" } }
                h1 { "Finances" }
            }
            crate::firm_invoice_graphs::FirmInvoiceGraphs { view: finances.invoices.clone() }
            crate::projected_cash::FirmProjectedCash { view: finances.projected_cash.clone() }
            crate::outcome_calibration::FirmOutcomeCalibration { view: finances.calibration.clone() }
        }
    }
}

fn firm_integrations(view: &FirmShowView, fields: &FirmFields) -> Element {
    let can_edit = fields.can_edit;
    let status = |provider: &str| {
        if !fields.can_view_secrets {
            return "Ask the Firm’s Admin DRI about setup.";
        }
        if fields
            .secrets
            .iter()
            .any(|row| row.provider == provider && row.status == "active")
        {
            "Credential stored"
        } else {
            "No credential stored"
        }
    };
    rsx! {
        section { id: "firm-integrations",
            h2 { "Integrations" }
            if let Some(error) = &fields.integration_error {
                p { class: "nav-form-error", role: "alert", "{error}" }
            }
            ProviderChoice {
                firm_id: view.id.clone(),
                csrf_token: fields.csrf_token.clone(),
                kind: "workspace",
                label: "Office suite",
                providers: [("google_workspace", "Google Workspace"), ("microsoft_365", "Microsoft 365")],
                selected: fields.workspace_provider.clone(),
                can_edit,
            }
            if fields.workspace_provider == "microsoft_365" {
                p { class: "nav-muted", "Google Calendar and Drive are paused. Microsoft calendar and file sync are unavailable in Navigator." }
            }
            ProviderChoice {
                firm_id: view.id.clone(),
                csrf_token: fields.csrf_token.clone(),
                kind: "esign",
                label: "E-signature",
                providers: [("docusign", "DocuSign"), ("dropbox_sign", "Dropbox Sign")],
                selected: fields.esign_provider.clone(),
                can_edit,
            }
            if fields.esign_provider == "dropbox_sign" {
                p { class: "nav-muted", "Use Dropbox Sign to send envelopes and record the completed signature in Navigator." }
            }
            div { class: "nav-choice-group--cols-2",
                div { class: "nav-choice-group__options",
                    for (label, provider) in [("Notion", "notion"), ("Slack", "slack")] {
                        article { class: "nav-card", key: "{provider}",
                            div { class: "nav-card__body",
                                h3 { "{label}" }
                                p { "{status(provider)}" }
                                if fields.can_manage_secrets {
                                    a { class: "nav-link", href: "#firm-secrets", "Manage {label} credentials" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ProviderChoice(
    firm_id: String,
    csrf_token: String,
    kind: &'static str,
    label: &'static str,
    providers: [(&'static str, &'static str); 2],
    selected: String,
    can_edit: bool,
) -> Element {
    rsx! {
        form { class: "nav-form", method: "post", action: "{FIRM_SHOW_PATH}/{firm_id}/integrations",
            input { r#type: "hidden", name: "_csrf", value: "{csrf_token}" }
            input { r#type: "hidden", name: "integration", value: "{kind}" }
            ChoiceGroup {
                legend: rsx! { "{label}" },
                name: "provider",
                columns: 2,
                selected: vec![selected],
                options: providers.into_iter().map(|(value, label)| {
                    ChoiceGroupOption::new(value, label).disabled(!can_edit)
                }).collect::<Vec<_>>(),
            }
            if can_edit {
                button { class: "nav-btn nav-btn--secondary", r#type: "submit", "Save {label}" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        firm_show_body, FirmFields, FirmFinances, FirmShowView, MemberRow, SecretMetadataRow,
        SummaryApprovalEventRow, SummaryApprovalRow,
    };
    use crate::firm_invoice_graphs::{
        CurrencyInvoiceGraphsView, FirmInvoiceGraphsView, InvoiceBarView,
    };
    use crate::people::ViewerRole;

    fn render(fields: Option<FirmFields>) -> String {
        dioxus_ssr::render_element(firm_show_body(&FirmShowView {
            tokens_href: String::new(),
            firm_name: "Neon Law".to_string(),
            role: ViewerRole::Owner,
            logo: None,
            id: "firm-1".to_string(),
            fields,
            ..Default::default()
        }))
    }

    #[test]
    fn shared_social_keys_are_owner_only_and_accounts_are_discoverable() {
        for role in [ViewerRole::Owner, ViewerRole::Admin] {
            let html = dioxus_ssr::render_element(firm_show_body(&FirmShowView {
                role,
                fields: Some(FirmFields::default()),
                ..Default::default()
            }));
            assert!(html.contains(r#"href="/app/admin/brands""#));
            assert!(html.contains(r#"href="/app/social/calendar""#));
            assert_eq!(
                html.contains(r#"href="/app/owner/keys""#),
                role == ViewerRole::Owner
            );
        }
    }

    #[test]
    fn ein_is_prominent_in_the_firm_header_and_absent_when_unset() {
        let with_ein = render(Some(FirmFields {
            name: "Example Firm".into(),
            ein: Some("12-3456789".into()),
            ..Default::default()
        }));
        assert!(with_ein.contains("Employer identification number (EIN):"));
        assert!(with_ein.contains("12-3456789"));
        assert!(with_ein.find("12-3456789").unwrap() < with_ein.find("</header>").unwrap());
        let without_ein = render(Some(FirmFields::default()));
        assert!(!without_ein.contains("Employer identification number (EIN):"));
    }

    #[test]
    fn renders_members_with_a_read_only_dri_marker() {
        let html = render(Some(FirmFields {
            name: "Guieb & Shook PC".to_string(),
            status: "active".to_string(),
            entity_name: "Guieb & Shook PC".to_string(),
            brand_keys: vec!["neon".to_string()],
            admin_dri: Some(("Nick Shook".to_string(), "nick@neonlaw.com".to_string())),
            admin_dri_problem: None,
            members: vec![
                MemberRow {
                    person_id: "p-1".to_string(),
                    name: "Nick Shook".to_string(),
                    email: "nick@neonlaw.com".to_string(),
                    membership: "admin".to_string(),
                    is_dri: true,
                },
                MemberRow {
                    person_id: "p-2".to_string(),
                    name: "Pat Lawyer".to_string(),
                    email: "pat@example.com".to_string(),
                    membership: "lawyer".to_string(),
                    is_dri: false,
                },
            ],
            can_edit: true,
            ..Default::default()
        }));
        assert!(html.contains("Nick Shook (nick@neonlaw.com)"), "{html}");
        assert!(html.contains("neon (www.neonlaw.com)"), "{html}");
        assert!(html.contains("Pat Lawyer"), "{html}");
        // Only a non-DRI admin gets the demotion button.
        assert_eq!(html.matches("Remove admin").count(), 0, "{html}");
        assert!(html.contains(r#"id="firm-show""#), "{html}");
        assert!(
            html.contains(r#"href="/app/admin/firms/firm-1/edit""#),
            "{html}"
        );
    }

    #[test]
    fn firm_link_uses_its_website_and_provider_cards_keep_the_saved_choices() {
        let html = render(Some(FirmFields {
            website_url: Some("https://practice.example.test".into()),
            workspace_provider: "microsoft_365".into(),
            esign_provider: "dropbox_sign".into(),
            can_edit: true,
            ..Default::default()
        }));
        assert!(html.contains(r#"href="https://practice.example.test""#));
        for choice in ["microsoft_365", "dropbox_sign"] {
            assert!(
                html.split_once(&format!(r#"id="provider-{choice}""#))
                    .is_some_and(|(_, rest)| rest
                        .split_once('>')
                        .is_some_and(|(input, _)| input.contains("checked"))),
                "{html}"
            );
        }
        assert!(html.contains("Notion"));
        assert!(html.contains("Slack"));
    }

    #[test]
    fn offers_remove_admin_for_a_non_dri_admin_only() {
        let mut fields = FirmFields {
            members: vec![MemberRow {
                person_id: "p-3".to_string(),
                name: "Dana Admin".to_string(),
                email: "dana@example.com".to_string(),
                membership: "admin".to_string(),
                is_dri: false,
            }],
            can_edit: true,
            can_remove_admin: true,
            ..Default::default()
        };
        let html = render(Some(fields.clone()));
        assert!(
            html.contains("/app/admin/firms/firm-1/members/remove-admin"),
            "{html}"
        );
        assert!(html.contains("Remove admin"), "{html}");
        fields.can_remove_admin = false;
        assert!(!render(Some(fields)).contains("/members/remove-admin"));
    }

    /// A caller without `ManageMembership` sees the Firm's facts but no Edit
    /// link — the same "offer what the capability admits" rule the create
    /// form's Owner-only door mirrors from the other side.
    #[test]
    fn hides_the_edit_link_when_the_caller_cannot_edit() {
        let html = render(Some(FirmFields {
            name: "Read Only Practice".to_string(),
            status: "active".to_string(),
            entity_name: "Read Only Entity".to_string(),
            brand_keys: Vec::new(),
            admin_dri: None,
            admin_dri_problem: None,
            members: Vec::new(),
            can_edit: false,
            ..Default::default()
        }));
        assert!(!html.contains("/edit\""), "{html}");
    }

    #[test]
    fn surfaces_an_admin_dri_problem_instead_of_a_name() {
        let html = render(Some(FirmFields {
            name: "Orphaned Practice".to_string(),
            status: "active".to_string(),
            entity_name: "Orphaned Entity".to_string(),
            brand_keys: Vec::new(),
            admin_dri: None,
            admin_dri_problem: Some("No Admin DRI is designated.".to_string()),
            members: Vec::new(),
            can_edit: false,
            ..Default::default()
        }));
        assert!(html.contains("No Admin DRI is designated."), "{html}");
        assert!(html.contains("No house brands attached."), "{html}");
    }

    #[test]
    fn financial_rollups_render_on_the_dashboard() {
        let overview = render(Some(FirmFields::default()));
        assert!(overview.contains(r#"href="/app/admin/firms/firm-1/finances""#));
        assert!(!overview.contains(r#"id="firm-invoice-graphs""#));
        let html = dioxus_ssr::render_element(firm_show_body(&FirmShowView {
            id: "firm-1".into(),
            finances: Some(FirmFinances {
                name: "Graphed Practice".into(),
                invoices: FirmInvoiceGraphsView {
                    currencies: vec![CurrencyInvoiceGraphsView {
                        currency: "USD".into(),
                        by_brand: vec![InvoiceBarView {
                            label: "Synthetic Brand".into(),
                            invoiced_cents: 10_000,
                            paid_cents: 5_000,
                        }],
                        by_lawyer_dri: vec![],
                    }],
                },
                ..Default::default()
            }),
            ..Default::default()
        }));
        for heading in [
            "Finances",
            "firm-invoice-graphs",
            "firm-projected-cash",
            "firm-outcome-calibration",
            "Synthetic Brand invoiced: $100",
        ] {
            assert!(html.contains(heading), "{html}");
        }
    }

    #[test]
    fn a_firm_outside_the_caller_s_reach_renders_not_found() {
        let html = render(None);
        assert!(html.contains("Firm not found"), "{html}");
        assert!(!html.contains("firm-member-row"), "{html}");
    }

    /// A caller who cannot even view metadata (a non-DRI Admin) sees no
    /// secrets section at all — not an empty one, which would disclose that
    /// the Firm has (or lacks) integration secrets configured.
    #[test]
    fn hides_the_secrets_section_from_a_caller_who_cannot_view_metadata() {
        let html = render(Some(FirmFields {
            name: "Quiet Practice".to_string(),
            status: "active".to_string(),
            entity_name: "Quiet Entity".to_string(),
            can_view_secrets: false,
            ..Default::default()
        }));
        assert!(!html.contains(r#"id="firm-secrets""#), "{html}");
    }

    /// Owner (or the Firm's own Admin DRI) sees the metadata table — never a
    /// value, only provider/kind/status/version/updated — and only the DRI
    /// also sees the write forms.
    #[test]
    fn renders_secret_metadata_without_a_value_and_gates_the_write_forms_on_management() {
        let secrets = vec![SecretMetadataRow {
            provider: "notion".to_string(),
            kind: "notion_token".to_string(),
            status: "active".to_string(),
            version: 2,
            updated_at: "2026-09-19T00:00:00Z".to_string(),
        }];

        let owner_view = render(Some(FirmFields {
            name: "Viewed Practice".to_string(),
            status: "active".to_string(),
            entity_name: "Viewed Entity".to_string(),
            can_view_secrets: true,
            can_manage_secrets: false,
            secrets: secrets.clone(),
            ..Default::default()
        }));
        assert!(owner_view.contains(r#"id="firm-secrets""#), "{owner_view}");
        assert!(owner_view.contains("notion_token"), "{owner_view}");
        assert!(owner_view.contains(">active<"), "{owner_view}");
        assert!(
            !owner_view.contains("Add or replace a secret"),
            "{owner_view}"
        );
        assert!(!owner_view.contains("Revoke"), "{owner_view}");

        let dri_view = render(Some(FirmFields {
            name: "Managed Practice".to_string(),
            status: "active".to_string(),
            entity_name: "Managed Entity".to_string(),
            can_view_secrets: true,
            can_manage_secrets: true,
            secrets,
            csrf_token: "CSRF-TOKEN".to_string(),
            ..Default::default()
        }));
        assert!(dri_view.contains("Add or replace a secret"), "{dri_view}");
        assert!(dri_view.contains("Revoke"), "{dri_view}");
        assert!(dri_view.contains(r#"value="CSRF-TOKEN""#), "{dri_view}");
        assert!(
            dri_view.contains(r#"action="/app/admin/firms/firm-1/secrets""#),
            "{dri_view}"
        );
    }

    #[test]
    fn multiple_twilio_versions_name_the_kind_wide_revocation() {
        let secrets = [1, 2]
            .into_iter()
            .map(|version| SecretMetadataRow {
                provider: "twilio".into(),
                kind: "twilio_auth_token".into(),
                status: "active".into(),
                version,
                updated_at: "2026-11-01T00:00:00Z".into(),
            })
            .collect();
        let html = render(Some(FirmFields {
            name: "Managed Practice".into(),
            status: "active".into(),
            entity_name: "Managed Entity".into(),
            can_view_secrets: true,
            can_manage_secrets: true,
            secrets,
            ..Default::default()
        }));
        assert_eq!(html.matches("firm-secret-row").count(), 2, "{html}");
        assert_eq!(
            html.matches(">Revoke all Twilio tokens</button>").count(),
            2,
            "{html}"
        );
        assert!(html.contains(">1<") && html.contains(">2<"), "{html}");
    }

    #[test]
    fn renders_email_summary_approvals_and_hides_them_from_other_callers() {
        let hidden = render(Some(FirmFields {
            name: "Quiet Practice".into(),
            status: "active".into(),
            entity_name: "Quiet Entity".into(),
            can_manage_summary_approvals: false,
            ..Default::default()
        }));
        assert!(
            !hidden.contains(r#"id="firm-summary-approvals""#),
            "{hidden}"
        );

        let html = render(Some(FirmFields {
            name: "Managed Practice".into(),
            status: "active".into(),
            entity_name: "Managed Entity".into(),
            can_manage_summary_approvals: true,
            csrf_token: "CSRF-TOKEN".into(),
            summary_error: Some("That sender is not a Client DRI.".into()),
            summary_saved: Some("client@example.com".into()),
            summary_revoked: Some("old@example.com".into()),
            summary_dri_choices: vec![("proj:person".into(), "Client DRI".into())],
            summary_approvals: vec![SummaryApprovalRow {
                id: "approval-1".into(),
                project_id: Some("project-1".into()),
                provider_account: "summaries@example.com".into(),
                sender_email: "client@example.com".into(),
                status: "approved".into(),
                updated_at: "2026-10-08T00:00:00Z".into(),
            }],
            summary_approval_history: vec![SummaryApprovalEventRow {
                actor_person_id: "actor-1".into(),
                project_id: Some("project-1".into()),
                provider_account: "summaries@example.com".into(),
                sender_email: "client@example.com".into(),
                action: "approved".into(),
                inserted_at: "2026-10-08T00:00:00Z".into(),
            }],
            ..Default::default()
        }));
        for expected in [
            r#"id="firm-summary-approvals""#,
            "Email summary approvals",
            "That sender is not a Client DRI.",
            "Approved client@example.com.",
            "Revoked old@example.com.",
            "summaries@example.com",
            "client@example.com",
            "Approve a sender",
            "Approval history",
            r#"action="/app/admin/firms/firm-1/summary-approvals""#,
            r#"action="/app/admin/firms/firm-1/summary-approvals/revoke""#,
        ] {
            assert!(html.contains(expected), "missing {expected:?}: {html}");
        }
    }
}
