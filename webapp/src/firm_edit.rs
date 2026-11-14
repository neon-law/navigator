//! The Firm edit form — `/app/admin/firms/{id}/edit`.
//!
//! A descriptive edit on `store::firms::update`: name, status
//! (`active`/`suspended`/`archived`), the Entity the Firm is, and its Portal
//! Theme Brand — one of the Brands it owns, which every `/app` page reached
//! through the Firm's hosts wears. For Owner
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

/// One `<option>` in the Portal Theme Brand picker: a Brand the Firm owns.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct FirmBrandOption {
    pub key: String,
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
    /// The Firm's Portal Theme Brand key; `None` keeps each host's own look.
    #[serde(default)]
    pub portal_theme_brand_key: Option<String>,
    /// The Brands this Firm owns, the only ones the theme picker offers.
    #[serde(default)]
    pub owned_brands: Vec<FirmBrandOption>,
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
        portal_theme_brand_key: firm.portal_theme_brand_key,
        owned_brands: owned_brands(&surreal, id)
            .await
            .map_err(|error| ServerFnError::new(error.to_string()))?,
        can_appoint_dri,
        admin_dri_person_id,
        dri_candidates,
        ..base
    })
}

/// The Brands this Firm owns: the only choices for its Portal Theme Brand.
#[cfg(feature = "server")]
async fn owned_brands(
    surreal: &store::surreal::SurrealDb,
    firm_id: uuid::Uuid,
) -> Result<Vec<FirmBrandOption>, store::brands::BrandError> {
    Ok(store::brands::for_firm(surreal, firm_id)
        .await?
        .into_iter()
        .map(|brand| FirmBrandOption {
            key: brand.key,
            name: brand.name,
        })
        .collect())
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
    let candidates = store::persons::list_directory(surreal)
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

fn theme_options(brands: &[FirmBrandOption]) -> Vec<Choice> {
    let mut options = vec![Choice::new("", "Each host's own brand")];
    options.extend(
        brands
            .iter()
            .map(|brand| Choice::new(brand.key.clone(), brand.name.clone())),
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
        Field::select(
            "Portal theme",
            "portal_theme_brand_key",
            theme_options(&view.owned_brands),
            view.portal_theme_brand_key.clone(),
        )
        .help(
            "The brand whose colours, logo, and name every signed-in page wears, \
             whichever of this firm's addresses a person signs in through.",
        ),
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
    use super::{edit_form, FirmBrandOption, FirmEditView, FirmEntityOption};
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
            name: "Guieb & Shook PC".to_string(),
            status: "active".to_string(),
            entity_id: Some(ENTITY_ID.to_string()),
            entities: vec![FirmEntityOption {
                id: ENTITY_ID.to_string(),
                name: "Guieb & Shook PC".to_string(),
            }],
            portal_theme_brand_key: Some("guieb-shook".to_string()),
            owned_brands: vec![
                FirmBrandOption {
                    key: "guieb-shook".to_string(),
                    name: "Guieb & Shook".to_string(),
                },
                FirmBrandOption {
                    key: "neon".to_string(),
                    name: "Neon Law".to_string(),
                },
            ],
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
        assert!(html.contains(r#"value="Guieb &#38; Shook PC""#), "{html}");
        assert!(
            html.contains(&format!(r#"<option value="{ENTITY_ID}" selected"#)),
            "{html}"
        );
        assert!(
            html.contains(r#"<option value="active" selected"#),
            "{html}"
        );
    }

    /// The theme picker offers only the Firm's own Brands, plus the option
    /// of each host keeping its own look, with the current theme selected.
    #[test]
    fn the_portal_theme_picker_offers_the_firm_s_own_brands() {
        let html = render(&view());
        assert!(html.contains(r#"name="portal_theme_brand_key""#), "{html}");
        assert!(
            html.contains(r#"<option value="guieb-shook" selected"#),
            "{html}"
        );
        assert!(html.contains(r#"<option value="neon""#), "{html}");
        assert!(html.contains(r#"<option value="""#), "{html}");
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

    #[cfg(feature = "server")]
    mod loader {
        use crate::firm_edit::{get_firm_edit_form, FirmEditView};
        use crate::inbox::server_fn_test_support::{call, request_parts, runtime, Viewer};
        use crate::people::ViewerRole;
        use dioxus::prelude::*;
        use store::persons::{NewPerson, Role};

        const ROUTE: &str = "/app/admin/firms/{id}/edit";

        struct Practice {
            surreal: store::surreal::SurrealDb,
            owner: uuid::Uuid,
            admin: uuid::Uuid,
            outsider: uuid::Uuid,
            firm: uuid::Uuid,
        }

        async fn person(
            surreal: &store::surreal::SurrealDb,
            label: &str,
            role: Role,
        ) -> uuid::Uuid {
            store::persons::create(
                surreal,
                &NewPerson::with_role(
                    format!("{label} Person"),
                    format!("{label}-{}@example.com", uuid::Uuid::now_v7()),
                    role,
                ),
            )
            .await
            .unwrap()
            .id
        }

        /// One Firm whose Admin DRI is `admin`, plus an Owner and an Admin
        /// with no membership on it.
        async fn practice() -> Practice {
            let surreal = store::test_support::mem_surreal().await;
            let owner = person(&surreal, "owner", Role::Owner).await;
            let admin = person(&surreal, "admin", Role::Admin).await;
            let outsider = person(&surreal, "outsider", Role::Admin).await;
            let firm = store::firms::create(
                &surreal,
                &store::firms::NewFirm {
                    name: "Edit Loader Firm".into(),
                    status: "active".into(),
                    entity_id: store::test_support::seed_entity(&surreal).await,
                    admin_dri_person_id: admin,
                },
            )
            .await
            .unwrap()
            .id;
            Practice {
                surreal,
                owner,
                admin,
                outsider,
                firm,
            }
        }

        fn load(
            rt: &tokio::runtime::Runtime,
            practice: &Practice,
            uri: &str,
            viewer: &Viewer,
        ) -> (Result<FirmEditView, ServerFnError>, axum::http::StatusCode) {
            let parts = request_parts(rt, ROUTE, uri, viewer);
            call(
                rt,
                parts,
                |dom| dom.provide_root_context(practice.surreal.clone()),
                get_firm_edit_form,
            )
        }

        fn uri(firm: uuid::Uuid) -> String {
            format!("/app/admin/firms/{firm}/edit")
        }

        #[test]
        fn a_lawyer_is_forbidden() {
            let rt = runtime();
            let practice = rt.block_on(practice());
            let (result, status) = load(
                &rt,
                &practice,
                &uri(practice.firm),
                &Viewer::new(ViewerRole::Lawyer, Some(practice.admin)),
            );
            assert!(result.is_err());
            assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
        }

        #[test]
        fn an_admin_outside_the_firm_and_an_unknown_firm_read_not_found() {
            let rt = runtime();
            let practice = rt.block_on(practice());
            let (view, status) = load(
                &rt,
                &practice,
                &uri(practice.firm),
                &Viewer::new(ViewerRole::Admin, Some(practice.outsider)),
            );
            let view = view.unwrap();
            assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
            assert!(!view.found);
            assert_eq!(view.id, practice.firm.to_string(), "the id is the path's");
            assert!(view.name.is_empty(), "nothing about the Firm leaks");

            let missing = uuid::Uuid::now_v7();
            let (view, status) = load(
                &rt,
                &practice,
                &uri(missing),
                &Viewer::new(ViewerRole::Owner, Some(practice.owner)),
            );
            assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
            assert!(!view.unwrap().found);
        }

        #[test]
        fn the_owner_edits_any_firm_and_picks_its_admin_dri() {
            let rt = runtime();
            let practice = rt.block_on(practice());
            let (view, status) = load(
                &rt,
                &practice,
                &format!("{}?error=Pick%20a%20DRI", uri(practice.firm)),
                &Viewer::new(ViewerRole::Owner, Some(practice.owner)),
            );
            let view = view.unwrap();
            assert_eq!(status, axum::http::StatusCode::OK);
            assert!(view.found);
            assert_eq!(view.name, "Edit Loader Firm");
            assert_eq!(view.status, "active");
            assert!(view.entity_id.is_some());
            assert!(!view.entities.is_empty());
            assert_eq!(view.error.as_deref(), Some("Pick a DRI"));
            assert_eq!(view.csrf_token, "CSRF");
            assert!(view.can_appoint_dri);
            assert_eq!(
                view.admin_dri_person_id,
                Some(practice.admin.to_string()),
                "the current DRI is preselected"
            );
            let html = dioxus_ssr::render_element(super::edit_form(&view));
            assert!(
                html.contains(&practice.owner.to_string()),
                "an Owner is offered"
            );
            assert!(
                html.contains(&practice.admin.to_string()),
                "the admin member is offered"
            );
            assert!(
                !html.contains(&practice.outsider.to_string()),
                "an admin with no membership here is not offered: {html}"
            );
        }

        #[test]
        fn the_firms_own_admin_edits_it_without_the_dri_picker() {
            let rt = runtime();
            let practice = rt.block_on(practice());
            let (view, status) = load(
                &rt,
                &practice,
                &uri(practice.firm),
                &Viewer::new(ViewerRole::Admin, Some(practice.admin)),
            );
            let view = view.unwrap();
            assert_eq!(status, axum::http::StatusCode::OK);
            assert!(view.found);
            assert!(!view.can_appoint_dri);
            assert!(view.dri_candidates.is_empty());
            assert_eq!(view.admin_dri_person_id, None);
        }
    }
}
