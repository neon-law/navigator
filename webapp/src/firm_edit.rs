//! The Firm edit form — `/app/admin/firms/{id}/edit`.
//!
//! A descriptive edit on `store::firms::update`: name, status
//! (`active`/`suspended`/`archived`), and the Entity the Firm is. For Owner
//! only, it also offers the Admin DRI picker, which the update handler hands
//! to `store::firms::appoint_admin_dri` — the designation's only writer. It
//! never touches any other membership row; those move only through
//! `update_membership` and `remove_membership` (see [`crate::firm_show`]).
//!
//! # Authorization
//!
//! Owner, or the Firm's own Admin membership
//! (`store::firm_capability::FirmCapability::ManageMembership`) — the exact
//! capability `store::firms::update` itself authorizes against, mirroring
//! [`crate::firm_show`]'s `ViewDirectory` gate. A Firm outside the caller's
//! reach renders the same not-found body a nonexistent id would, so the edit
//! page discloses nothing the show page does not. The Admin DRI picker
//! renders only under `FirmCapability::ManageAdminDri`, which only Owner
//! holds.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{Choice, Field, FormCard, Heading, PersonChoice};
use crate::people::ViewerRole;

/// The form's `?error=` flash, set by the update handler's
/// redirect-on-failure.
#[derive(Deserialize, Serialize, Clone, Default, PartialEq, Eq)]
pub struct FirmEditQuery {
    #[serde(default)]
    pub error: Option<String>,
}

/// One `<option>` in the Entity picker.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct FirmEntityOption {
    pub id: String,
    pub name: String,
}

/// The rendered "edit firm" form.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct FirmEditView {
    /// `false` when the id resolves to no Firm this caller may manage — a
    /// missing Firm and a Firm outside the caller's own membership render
    /// identically (`docs/access-model.md`).
    pub found: bool,
    /// The Firm id from the path. Present even on a not-found render.
    pub id: String,
    pub name: String,
    pub status: String,
    pub entity_id: Option<String>,
    pub entities: Vec<FirmEntityOption>,
    /// `true` when this caller holds `FirmCapability::ManageAdminDri` —
    /// Owner only — and so sees the Admin DRI picker.
    #[serde(default)]
    pub can_appoint_dri: bool,
    /// The current Admin DRI's person id, preselected in the picker.
    #[serde(default)]
    pub admin_dri_person_id: Option<String>,
    /// Who the picker offers: every Owner, plus every `admin`-role person
    /// holding an `admin` membership on this Firm — exactly the people
    /// `store::firms::appoint_admin_dri` accepts. Empty unless
    /// `can_appoint_dri`.
    #[serde(default)]
    pub dri_candidates: Vec<PersonChoice>,
    pub csrf_token: String,
    pub error: Option<String>,
    pub role: ViewerRole,
    #[serde(default)]
    pub logo: Option<crate::components::AppLogo>,
    #[serde(default)]
    pub tokens_href: String,
    #[serde(default)]
    pub firm_name: String,
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

/// Load the Firm edit form for the `{id}` in the request path.
///
/// Admission mirrors `store::firm_capability::FirmCapability::ManageMembership`
/// exactly as `store::firms::update` itself authorizes: Owner edits every
/// Firm; an Admin edits only a Firm they hold an admin `person_firm_role` on.
#[server]
pub async fn get_firm_edit_form() -> Result<FirmEditView, ServerFnError> {
    let role = crate::admin_listing::require_admin().await?;
    let axum::extract::Path(id) =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Path<uuid::Uuid>, _>()
            .await?;
    let surreal = consume_context::<store::surreal::SurrealDb>();
    let actor_person_id = crate::admin_listing::injected_person_id().await;

    let csrf_token = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<crate::csrf::CsrfToken>,
        _,
    >()
    .await
    .map(|axum::Extension(token)| token.0)
    .unwrap_or_default();
    let error =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Query<FirmEditQuery>, _>(
        )
        .await
        .ok()
        .and_then(|axum::extract::Query(q)| q.error);

    let base = FirmEditView {
        tokens_href: crate::app_chrome::app_tokens_href_from_context().await,
        firm_name: crate::app_chrome::firm_name_from_context().await,
        logo: crate::app_chrome::app_logo_from_context().await,
        role,
        id: id.to_string(),
        csrf_token,
        error,
        found: false,
        ..FirmEditView::default()
    };

    let decision = store::firm_capability::resolve(
        &surreal,
        store_role(role),
        actor_person_id,
        id,
        store::firm_capability::FirmCapability::ManageMembership,
    )
    .await
    .map_err(|error| ServerFnError::new(error.to_string()))?;
    if !decision.is_allowed() {
        dioxus_fullstack_core::FullstackContext::commit_http_status(
            axum::http::StatusCode::NOT_FOUND,
            None,
        );
        return Ok(base);
    }

    let Some(firm) = store::firms::find_by_id(&surreal, id)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?
    else {
        dioxus_fullstack_core::FullstackContext::commit_http_status(
            axum::http::StatusCode::NOT_FOUND,
            None,
        );
        return Ok(base);
    };

    let entities = store::entities::all(&surreal)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?
        .into_iter()
        .map(|e| FirmEntityOption {
            id: e.id.to_string(),
            name: e.name,
        })
        .collect();

    let can_appoint_dri = store::firm_capability::resolve(
        &surreal,
        store_role(role),
        actor_person_id,
        id,
        store::firm_capability::FirmCapability::ManageAdminDri,
    )
    .await
    .map_err(|error| ServerFnError::new(error.to_string()))?
    .is_allowed();
    let (admin_dri_person_id, dri_candidates) = if can_appoint_dri {
        admin_dri_choices(&surreal, id)
            .await
            .map_err(|error| ServerFnError::new(error.to_string()))?
    } else {
        (None, Vec::new())
    };

    Ok(FirmEditView {
        found: true,
        name: firm.name,
        status: firm.status,
        entity_id: firm.entity_id.map(|id| id.to_string()),
        entities,
        can_appoint_dri,
        admin_dri_person_id,
        dri_candidates,
        ..base
    })
}

/// The current Admin DRI and every person `store::firms::appoint_admin_dri`
/// would accept on this Firm: any Owner (granted an `admin` membership on
/// appointment if they lack one), and any `admin`-role person already holding
/// an `admin` membership here.
#[cfg(feature = "server")]
async fn admin_dri_choices(
    surreal: &store::surreal::SurrealDb,
    firm_id: uuid::Uuid,
) -> Result<(Option<String>, Vec<PersonChoice>), store::firms::FirmError> {
    let memberships = store::firms::memberships_for_firm(surreal, firm_id).await?;
    let current = memberships
        .iter()
        .find(|row| row.is_dri)
        .map(|row| row.person_id.to_string());
    let admin_members: std::collections::HashSet<uuid::Uuid> = memberships
        .iter()
        .filter(|row| row.membership == store::firms::FirmMembership::Admin)
        .map(|row| row.person_id)
        .collect();
    let candidates = store::persons::list_directory(surreal, "", "", &[])
        .await?
        .into_iter()
        .filter(|p| {
            p.role.is_owner()
                || (p.role == store::persons::Role::Admin && admin_members.contains(&p.id))
        })
        .map(|p| PersonChoice::new(p.id.to_string(), p.name, p.email).with_detail(p.role.as_str()))
        .collect();
    Ok((current, candidates))
}

const STATUS_CHOICES: [&str; 3] = ["active", "suspended", "archived"];

fn entity_options(entities: &[FirmEntityOption]) -> Vec<Choice> {
    let mut options = vec![Choice::new("", "—")];
    options.extend(
        entities
            .iter()
            .map(|e| Choice::new(e.id.clone(), e.name.clone())),
    );
    options
}

fn edit_form(view: &FirmEditView) -> Element {
    let mut fields = vec![
        Field::text("Name", "name", view.name.clone()).required(),
        Field::select(
            "Status",
            "status",
            STATUS_CHOICES.iter().map(|s| Choice::new(*s, *s)).collect(),
            Some(view.status.clone()),
        )
        .required(),
        Field::select(
            "Entity",
            "entity_id",
            entity_options(&view.entities),
            view.entity_id.clone(),
        )
        .required()
        .help("The legal organization this Firm is. Create the entity first if it isn't listed."),
    ];
    if view.can_appoint_dri {
        fields.push(
            Field::person_picker(
                "Admin DRI",
                "admin_dri_person_id",
                "— pick the Admin DRI —",
                view.dri_candidates.clone(),
                view.admin_dri_person_id.clone(),
            )
            .required()
            .help(
                "Who administers this Firm's settings, membership, brands, and integrations. \
                 Any owner, or an admin who is already an admin member of this Firm.",
            ),
        );
    }

    rsx! {
        if let Some(error) = view.error.as_ref() {
            p { class: "nav-form-error", role: "alert", "{error}" }
        }
        FormCard {
            title: "Edit firm".to_string(),
            action: format!("/app/admin/firms/{}/edit", view.id),
            submit_label: "Save".to_string(),
            heading: Heading::H2,
            csrf_token: Some(view.csrf_token.clone()),
            fields,
        }
    }
}

/// `/app/admin/firms/{id}/edit`.
#[component]
pub fn FirmEdit() -> Element {
    let resource = use_server_future(get_firm_edit_form)?;
    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        Some(Err(_)) => {
            return rsx! {
                main { id: "firm-edit", p { "Failed to load the firm." } }
            }
        }
        None => {
            return rsx! {
                main { id: "firm-edit", p { "Loading…" } }
            }
        }
    };

    let back_href = format!("{}/{}", crate::firm_show::FIRM_SHOW_PATH, view.id);

    rsx! {
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{view.tokens_href}" }
        crate::components::AppNavbar {
            destinations: crate::app_chrome::app_destinations(view.role),
            logo: view.logo.clone(),
        }
        main { id: "firm-edit", class: "nav-theme",
            if view.found {
                document::Title { "{view.firm_name} | Edit firm" }
                header { class: "page-header",
                    h1 { "Edit firm" }
                    p { a { href: "{back_href}", "← Back" } }
                }
                {edit_form(&view)}
            } else {
                document::Title { "{view.firm_name} | Not found" }
                h1 { "Firm not found" }
                p { a { href: "/app/admin", "← Back" } }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{edit_form, FirmEditView, FirmEntityOption};
    use crate::components::PersonChoice;
    use crate::people::ViewerRole;

    const ENTITY_ID: &str = "00000000-0000-0000-0000-000000000001";
    const ADMIN_ID: &str = "00000000-0000-0000-0000-0000000000a1";
    const OWNER_ID: &str = "00000000-0000-0000-0000-0000000000b1";

    fn view() -> FirmEditView {
        FirmEditView {
            tokens_href: String::new(),
            firm_name: "Neon Law".to_string(),
            found: true,
            id: "firm-1".to_string(),
            name: "Shook Law PC".to_string(),
            status: "active".to_string(),
            entity_id: Some(ENTITY_ID.to_string()),
            entities: vec![FirmEntityOption {
                id: ENTITY_ID.to_string(),
                name: "Shook Law PC".to_string(),
            }],
            can_appoint_dri: true,
            admin_dri_person_id: Some(ADMIN_ID.to_string()),
            dri_candidates: vec![
                PersonChoice::new(ADMIN_ID, "Ada Admin", "ada@example.com").with_detail("admin"),
                PersonChoice::new(OWNER_ID, "Olive Owner", "olive@example.com")
                    .with_detail("owner"),
            ],
            csrf_token: "CSRF-TOKEN".to_string(),
            error: None,
            role: ViewerRole::Owner,
            logo: None,
        }
    }

    /// Owner sees the Admin DRI picker, offering an Owner alongside the
    /// Firm's admin members, with the current DRI preselected.
    #[test]
    fn owner_may_pick_an_owner_as_the_admin_dri() {
        let html = render(&view());
        assert!(html.contains(r#"name="admin_dri_person_id""#), "{html}");
        assert!(
            html.contains(&format!(r#"<option value="{ADMIN_ID}" selected"#)),
            "{html}"
        );
        assert!(
            html.contains(&format!(r#"<option value="{OWNER_ID}""#)),
            "{html}"
        );
    }

    /// An Admin editing their own Firm never sees the picker: appointing the
    /// designation is Owner's alone.
    #[test]
    fn a_non_owner_gets_no_admin_dri_picker() {
        let mut view = view();
        view.role = ViewerRole::Admin;
        view.can_appoint_dri = false;
        view.dri_candidates.clear();
        let html = render(&view);
        assert!(!html.contains("admin_dri_person_id"), "{html}");
    }

    fn render(view: &FirmEditView) -> String {
        dioxus_ssr::render_element(edit_form(view))
    }

    #[test]
    fn the_edit_form_targets_the_firm_s_own_edit_path_and_prefills_every_field() {
        let html = render(&view());
        assert!(
            html.contains(r#"action="/app/admin/firms/firm-1/edit""#),
            "{html}"
        );
        assert!(html.contains(r#"value="Shook Law PC""#), "{html}");
        assert!(
            html.contains(&format!(r#"<option value="{ENTITY_ID}" selected"#)),
            "{html}"
        );
        assert!(
            html.contains(r#"<option value="active" selected"#),
            "{html}"
        );
    }

    #[test]
    fn every_status_is_offered() {
        let html = render(&view());
        for status in ["active", "suspended", "archived"] {
            assert!(html.contains(&format!(r#"value="{status}""#)), "{html}");
        }
    }

    #[test]
    fn a_refusal_names_the_rule() {
        let mut view = view();
        view.error = Some(
            "This firm must always have an Admin DRI. Transfer the designation first.".to_string(),
        );
        let html = render(&view);
        assert!(html.contains("Transfer the designation first."), "{html}");
    }

    #[test]
    fn the_form_meets_the_layer_one_a11y_invariants() {
        let html = render(&view());
        crate::components::assert_forms_accessible(&html, "firm_edit");
    }
}
