//! The Owner-only "move a brand to another firm" control —
//! `/app/owner/firms/move-brand`.
//!
//! Brand mobility is an ownership decision: a move changes whose legal entity
//! the public site's copyright names ([`crate::firm_show`] and
//! [`crate::firm_new`] are where firms themselves are managed). The form
//! picks a brand wearing a firm, a target firm, and demands an explicit
//! confirmation — the POST redirects back here on refusal with the error and
//! both pickers echoed, and reports the new attribution on success. The
//! handler lives in `portal/src/admin.rs` and calls `store::firms::move_brand`,
//! which re-enforces the Owner gate beneath this route.
//!
//! # Authorization
//!
//! Owner only. [`crate::admin_listing::require_owner`] commits a real `403`
//! for anyone else — Rego's `owner_only_path` rule already denies every
//! non-Owner session at the route, so this is defense in depth, not the
//! primary gate.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{Choice, Field, FormCard, Heading};
use crate::firm_new::IdChoice;
use crate::people::ViewerRole;

/// Everything the move-brand page reads off the query string: the POST's
/// `?error=` flash and echoed fields, and the success flash naming the move.
#[derive(Deserialize, Serialize, Clone, Default, PartialEq, Eq)]
pub struct FirmMoveQuery {
    #[serde(default)]
    pub error: Option<String>,
    /// The brand key the POST just moved.
    #[serde(default)]
    pub moved: Option<String>,
    /// The target firm's name — the new attribution.
    #[serde(default)]
    pub firm: Option<String>,
    #[serde(default)]
    pub brand_key: Option<String>,
    #[serde(default)]
    pub target_firm_id: Option<String>,
}

/// The rendered "move brand" page.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct MoveBrandView {
    /// `false` for a non-Owner caller — the page renders not-found under a
    /// committed `403` (`require_owner` already committed the status).
    pub found: bool,
    /// Every brand currently wearing a firm, as `brand_key` → display label.
    pub brands: Vec<IdChoice>,
    /// Every firm a brand can move to.
    pub firms: Vec<IdChoice>,
    pub csrf_token: String,
    pub query: FirmMoveQuery,
    pub role: ViewerRole,
    #[serde(default)]
    pub logo: Option<crate::components::AppLogo>,
    #[serde(default)]
    pub tokens_href: String,
    #[serde(default)]
    pub firm_name: String,
}

/// Load the move-brand form: the brand picker, the target-firm picker, and
/// the query flash.
#[server]
pub async fn get_move_brand_form() -> Result<MoveBrandView, ServerFnError> {
    let role = crate::admin_listing::require_owner().await?;

    let csrf_token = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<crate::csrf::CsrfToken>,
        _,
    >()
    .await
    .map(|axum::Extension(token)| token.0)
    .unwrap_or_default();
    let query =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Query<FirmMoveQuery>, _>(
        )
        .await
        .map(|axum::extract::Query(q)| q)
        .unwrap_or_default();

    let surreal = consume_context::<store::surreal::SurrealDb>();
    // Owner sees every firm-scoped brand; a brand with no firm has no seat to
    // move and nothing here to reassign.
    let brands = store::brands::all_firm_scoped(&surreal)
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?
        .into_iter()
        .map(|b| IdChoice {
            label: format!("{} ({})", b.name, b.key),
            id: b.key,
        })
        .collect();
    let firms = store::firms::all(&surreal)
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?
        .into_iter()
        .map(|f| IdChoice {
            id: f.id.to_string(),
            label: f.name,
        })
        .collect();

    Ok(MoveBrandView {
        firm_name: crate::app_chrome::firm_name_from_context().await,
        found: true,
        brands,
        firms,
        csrf_token,
        query,
        role,
        logo: crate::app_chrome::app_logo_from_context().await,
        tokens_href: crate::app_chrome::app_tokens_href_from_context().await,
    })
}

/// Build a picker's options from `choices`, leading with `blank`.
fn picker_options(blank: &str, choices: &[IdChoice]) -> Vec<Choice> {
    let mut options = vec![Choice::new("", blank)];
    options.extend(
        choices
            .iter()
            .map(|c| Choice::new(c.id.clone(), c.label.clone())),
    );
    options
}

/// The move-brand form: two pickers and the confirmation checkbox. The
/// confirmation is a server-enforced gate (`portal/src/admin.rs`), not
/// decoration — moving a brand rewrites the public copyright.
fn move_brand_form(view: &MoveBrandView) -> Element {
    let q = &view.query;
    let fields = vec![
        Field::select(
            "Brand",
            "brand_key",
            picker_options("— pick the brand to move —", &view.brands),
            q.brand_key.clone().filter(|v| !v.is_empty()),
        )
        .required(),
        Field::select(
            "Target firm",
            "target_firm_id",
            picker_options("— pick the firm —", &view.firms),
            q.target_firm_id.clone().filter(|v| !v.is_empty()),
        )
        .required(),
        Field::checkbox(
            "Confirm: the brand's public pages will name the target firm's legal entity.",
            "confirm",
            "on",
            false,
        )
        .required(),
    ];

    rsx! {
        if let Some(error) = q.error.as_ref() {
            p { class: "nav-form-error", role: "alert", "{error}" }
        }
        if let Some(moved) = q.moved.as_ref() {
            p { role: "status", "Moved {moved}. Its public pages now name {q.firm.clone().unwrap_or_default()}'s legal entity." }
        }
        FormCard {
            title: "Move brand".to_string(),
            action: "/app/owner/firms/move-brand".to_string(),
            submit_label: "Move brand".to_string(),
            heading: Heading::H2,
            csrf_token: Some(view.csrf_token.clone()),
            fields,
        }
    }
}

/// The loaded move-brand page.
fn move_body(view: &MoveBrandView) -> Element {
    rsx! {
        document::Title { "{view.firm_name} | Owner | Move brand" }
        header { class: "page-header",
            h1 { "Move brand" }
            p { a { href: "/app/owner", "← Back to firms" } }
        }
        {move_brand_form(view)}
        p { class: "project-form-cancel",
            a { class: "nav-btn nav-btn--secondary", href: "/app/owner", "Cancel" }
        }
    }
}

/// `/app/owner/firms/move-brand` — the Owner moves a brand between firms.
#[component]
pub fn MoveBrand() -> Element {
    let resource = use_server_future(get_move_brand_form)?;

    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        Some(Err(_)) => {
            return rsx! {
                main { id: "firm-move", p { "Failed to load the form." } }
            }
        }
        None => {
            return rsx! {
                main { id: "firm-move", p { "Loading…" } }
            }
        }
    };

    rsx! {
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{view.tokens_href}" }
        crate::components::AppNavbar {
            destinations: crate::app_chrome::app_destinations(view.role),
            logo: view.logo.clone(),
        }
        main { id: "firm-move", class: "nav-theme",
            if view.found {
                {move_body(&view)}
            } else {
                document::Title { "{view.firm_name} | Not found" }
                h1 { "Not found" }
                p { "No move-brand form is available at this address." }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{move_body, FirmMoveQuery, IdChoice, MoveBrandView};
    use crate::people::ViewerRole;

    const BRAND_KEY: &str = "iep";
    const FIRM_ID: &str = "00000000-0000-0000-0000-000000000001";

    fn view(query: FirmMoveQuery) -> MoveBrandView {
        MoveBrandView {
            tokens_href: String::new(),
            firm_name: "Neon Law".to_string(),
            found: true,
            brands: vec![IdChoice {
                id: BRAND_KEY.to_string(),
                label: "IEP (iep)".to_string(),
            }],
            firms: vec![IdChoice {
                id: FIRM_ID.to_string(),
                label: "Lex Tecnica LLP".to_string(),
            }],
            csrf_token: "CSRF-TOKEN".to_string(),
            query,
            role: ViewerRole::Owner,
            logo: None,
        }
    }

    fn render(view: &MoveBrandView) -> String {
        dioxus_ssr::render_element(move_body(view))
    }

    #[test]
    fn the_move_form_targets_the_move_route_with_both_pickers_and_confirm() {
        let html = render(&view(FirmMoveQuery::default()));
        assert!(
            html.contains(r#"action="/app/owner/firms/move-brand""#),
            "{html}"
        );
        assert!(
            html.contains(r#"name="_csrf" value="CSRF-TOKEN""#),
            "{html}"
        );
        assert!(html.contains(r#"name="brand_key""#), "{html}");
        assert!(html.contains(r#"name="target_firm_id""#), "{html}");
        assert!(html.contains(r#"name="confirm""#), "{html}");
    }

    #[test]
    fn a_refused_move_names_the_rule_and_echoes_both_pickers() {
        let html = render(&view(FirmMoveQuery {
            error: Some("Confirm the move — it changes which firm's legal entity the brand's public pages name.".to_string()),
            brand_key: Some(BRAND_KEY.to_string()),
            target_firm_id: Some(FIRM_ID.to_string()),
            ..FirmMoveQuery::default()
        }));
        assert!(
            // Server-side rendering escapes the apostrophes (`&#39;`), so the
            // needle does too.
            html.contains("Confirm the move — it changes which firm&#39;s legal entity the brand&#39;s public pages name."),
            "{html}"
        );
        assert!(
            html.contains(&format!(r#"<option value="{BRAND_KEY}" selected"#)),
            "{html}"
        );
        assert!(
            html.contains(&format!(r#"<option value="{FIRM_ID}" selected"#)),
            "{html}"
        );
    }

    #[test]
    fn a_successful_move_reports_the_new_attribution() {
        let html = render(&view(FirmMoveQuery {
            moved: Some(BRAND_KEY.to_string()),
            firm: Some("Lex Tecnica LLP".to_string()),
            ..FirmMoveQuery::default()
        }));
        assert!(
            // Server-side rendering escapes the apostrophe (`&#39;`).
            html.contains(
                "Moved iep. Its public pages now name Lex Tecnica LLP&#39;s legal entity."
            ),
            "{html}"
        );
    }

    #[test]
    fn every_form_on_the_page_meets_the_layer_one_a11y_invariants() {
        let html = render(&view(FirmMoveQuery::default()));
        crate::components::assert_forms_accessible(&html, "firm_move");
    }
}
