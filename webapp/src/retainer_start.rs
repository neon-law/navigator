//! The "start a retainer walk" form as a Dioxus component (#956 Phase 4) — the
//! lawyer on-ramp that opens a matter.
//!
//! The successor to the `views::pages::admin::retainers::start_walk`. It
//! lists the seeded `onboarding__*` templates as the picker's options, reads the
//! session CSRF token, and renders the shared [`crate::components::FormCard`] as
//! a native `POST` to `/app/lawyer/retainers/new` — the existing create handler,
//! unchanged.
//!
//! A refused start (a client email with no `@`, no template chosen, an unknown
//! template) used to re-render this form inline from the `POST`. It now
//! redirects back here with the reason as `?error=` and the submitted values
//! echoed, so nothing is retyped — the post/redirect/get convention the other
//! migrated create forms already use.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{Choice, Field, FormCard};
use crate::people::ViewerRole;

/// The template picker's `?error=` flash and the echoed submitted values a
/// refused start redirects back with.
#[derive(Deserialize, Default)]
pub struct RetainerStartQuery {
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub client_email: Option<String>,
    #[serde(default)]
    pub retainer_template_code: Option<String>,
}

/// One onboarding-template option: the submitted `code` and its display label.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct TemplateChoice {
    pub code: String,
    pub label: String,
}

/// The rendered start form: the onboarding-template options, the echoed values
/// and refusal flash from a rejected submit, the session CSRF token, and the
/// viewer's tier.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct RetainerStartView {
    /// The resolved brand's tokens stylesheet href, so the page wears
    /// its own palette rather than the firm's on a non-default host.
    #[serde(default)]
    pub tokens_href: String,
    pub templates: Vec<TemplateChoice>,
    /// The option selected when nothing was submitted: the Firm Library's
    /// `retainer` when the Firm's library names one, otherwise
    /// [`SHARED_DEFAULT_RETAINER`].
    #[serde(default)]
    pub default_template_code: String,
    pub client_email: String,
    pub retainer_template_code: String,
    pub csrf_token: String,
    pub role: ViewerRole,
    #[serde(default)]
    pub error: Option<String>,
    /// The deploy's firm name, for the document title. Resolved from the
    /// request-scoped branding rather than written into the copy, so a
    /// white-label deploy's tab reads its own name.
    #[serde(default)]
    pub firm_name: String,
}

/// The shared retainer a matter opens on when the Firm's Library names none.
pub const SHARED_DEFAULT_RETAINER: &str = "onboarding__letter";

/// The picker's options: the shared `onboarding__*` templates, each replaced
/// by the Firm's own Library version of the same code, plus the Firm's other
/// `onboarding__*` Drafts and its Library `retainer`, sorted by label.
///
/// Restricting the picker to the `onboarding__*` family (and the Firm's chosen
/// retainer, whose kind the Library sync already checked opens a matter) is
/// what makes "opening a matter" always start it with a retainer-type notation.
#[cfg(any(feature = "server", test))]
#[must_use]
pub fn picker_choices(
    shared: &[store::templates::Template],
    firm: &[store::templates::Template],
    retainer: Option<&str>,
) -> Vec<TemplateChoice> {
    let offered = |t: &&store::templates::Template| {
        t.code.starts_with("onboarding__") || Some(t.code.as_str()) == retainer
    };
    let firm: Vec<&store::templates::Template> = firm.iter().filter(offered).collect();
    let mut choices: Vec<TemplateChoice> = shared
        .iter()
        .filter(|t| t.project_id.is_none() && t.firm_id.is_none())
        .filter(offered)
        .filter(|t| !firm.iter().any(|f| f.code == t.code))
        .chain(firm.iter().copied())
        .map(|t| TemplateChoice {
            label: format!("{} — {}", t.title, t.code),
            code: t.code.clone(),
        })
        .collect();
    choices.sort_by(|a, b| a.label.cmp(&b.label));
    choices
}

/// Load the start form: refuse non-lawyer, read the injected CSRF token and the
/// echoed `?error=` / values, sync the Firm Library of the Firm wearing this
/// brand, and list the onboarding templates as `(code, label)` options sorted
/// by label ([`picker_choices`]).
#[server]
pub async fn get_retainer_start_form() -> Result<RetainerStartView, ServerFnError> {
    let role = crate::admin_listing::require_lawyer().await?;
    let csrf_token = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<crate::csrf::CsrfToken>,
        _,
    >()
    .await
    .map(|axum::Extension(token)| token.0)
    .unwrap_or_default();
    let axum::extract::Query(query) = dioxus_fullstack_core::FullstackContext::extract::<
        axum::extract::Query<RetainerStartQuery>,
        _,
    >()
    .await?;

    let surreal = consume_context::<store::surreal::SurrealDb>();
    let shared = store::templates::list_current(&surreal)
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    // The Firm wearing this brand: the one `POST` opens the matter under.
    let firm_id = store::firms::firm_id_for_brand_key(&surreal, views::brand::brand_key().as_str())
        .await
        .ok()
        .flatten();
    let storage = try_consume_context::<std::sync::Arc<dyn cloud::StorageService>>();
    let library = match (firm_id, storage) {
        (Some(firm_id), Some(storage)) => {
            store::firm_library::sync_configured(&surreal, &storage, firm_id).await
        }
        _ => None,
    };
    let retainer = library
        .as_ref()
        .and_then(|synced| synced.retainer.as_ref())
        .map(|template| template.code.clone());
    let firm = library.map(|synced| synced.templates).unwrap_or_default();
    let templates = picker_choices(&shared, &firm, retainer.as_deref());

    Ok(RetainerStartView {
        tokens_href: crate::app_chrome::app_tokens_href_from_context().await,
        firm_name: crate::app_chrome::firm_name_from_context().await,
        templates,
        default_template_code: retainer.unwrap_or_else(|| SHARED_DEFAULT_RETAINER.to_string()),
        client_email: query.client_email.unwrap_or_default(),
        retainer_template_code: query.retainer_template_code.unwrap_or_default(),
        csrf_token,
        role,
        error: query.error.filter(|message| !message.is_empty()),
    })
}

/// The lawyer "start a retainer walk" form. Server-side rendered as a native
/// `POST` form to `/app/lawyer/retainers/new` carrying the CSRF token, so it works
/// without JavaScript.
#[component]
pub fn LawyerRetainerStart() -> Element {
    let resource = use_server_future(get_retainer_start_form)?;

    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        Some(Err(_)) => {
            return rsx! {
                main { id: "retainer-new", p { "Failed to load the form." } }
            }
        }
        None => {
            return rsx! {
                main { id: "retainer-new", p { "Loading…" } }
            }
        }
    };

    start_body(&view)
}

/// The loaded form: the nav chrome, the refusal flash, the intro, and the native
/// `POST` card. Split from the component so the tests render a fixed view
/// without standing up the server function.
fn start_body(view: &RetainerStartView) -> Element {
    let role = view.role;
    // A matter opens on an onboarding template; the dropdown is the canonical
    // picker so lawyers choose by title rather than typing a code. Default the
    // selection to the Firm's Library retainer, or the shared generic one.
    let selected = if !view.retainer_template_code.is_empty() {
        view.retainer_template_code.clone()
    } else if !view.default_template_code.is_empty() {
        view.default_template_code.clone()
    } else {
        SHARED_DEFAULT_RETAINER.to_string()
    };
    let options: Vec<Choice> = view
        .templates
        .iter()
        .map(|t| Choice::new(t.code.clone(), t.label.clone()))
        .collect();
    let fields = vec![
        Field::email("Client email", "client_email", &view.client_email)
            .required()
            .placeholder("libra@example.com"),
        Field::select(
            "Onboarding template",
            "retainer_template_code",
            options,
            Some(selected),
        )
        .required()
        .help("Every matter opens on an onboarding template. Pick any one and edit it for this client."),
    ];

    rsx! {
        document::Title { "{view.firm_name} | Lawyer | Retainers | New" }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{view.tokens_href}" }
        nav { class: "lawyer-nav",
            a { class: "nav-link", href: "/app/projects", "Portal" }
            if role.is_lawyer_tier() {
                a { class: "nav-link", href: "/app/lawyer", "Lawyer" }
            }
            if role.is_admin_tier() {
                a { class: "nav-link", href: "/app/admin", "Admin" }
            }
            a { class: "nav-link", href: "/auth/logout", "Sign out" }
            crate::components::AppProfileMenu {
                destinations: vec![
                    crate::components::AppNavLink::new("Projects", "/app/projects"),
                    crate::components::AppNavLink::new("Sign out", "/auth/logout"),
                ],
            }
        }
        main { id: "retainer-new", class: "nav-theme",
            if let Some(error) = view.error.as_ref() {
                p { class: "nav-form-error", role: "alert", "{error}" }
            }
            p { class: "nav-muted",
                "Creates a Notation and walks the questionnaire one question at a "
                "time. Once the questionnaire reaches "
                code { "END" }
                ", the retainer-intake workflow takes over (intake → render → "
                "signature)."
            }
            FormCard {
                title: "New retainer".to_string(),
                action: "/app/lawyer/retainers/new".to_string(),
                submit_label: "Start walk".to_string(),
                csrf_token: Some(view.csrf_token.clone()),
                fields,
            }
            p { a { href: "/app/lawyer", "← Cancel" } }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::form::assert_forms_accessible;

    fn view(error: Option<&str>) -> RetainerStartView {
        RetainerStartView {
            tokens_href: String::new(),
            firm_name: "Neon Law".to_string(),
            templates: vec![TemplateChoice {
                code: "onboarding__letter".to_string(),
                label: "Onboarding Letter — onboarding__letter".to_string(),
            }],
            client_email: "libra@example.com".to_string(),
            default_template_code: SHARED_DEFAULT_RETAINER.to_string(),
            retainer_template_code: "onboarding__letter".to_string(),
            csrf_token: "tok".to_string(),
            role: ViewerRole::Lawyer,
            error: error.map(str::to_string),
        }
    }

    /// Render the page body for a fixed view, bypassing the server function.
    fn render_start(view: &RetainerStartView) -> String {
        dioxus_ssr::render_element(start_body(view))
    }

    /// The picker offers every `onboarding__*` template and refuses no
    /// pairing. Lawyers choose the engagement agreement and edit it per
    /// client; nothing narrows the list to a matter's service.
    #[test]
    fn the_picker_offers_every_onboarding_template_and_refuses_none() {
        let codes = ["onboarding__letter", "onboarding__letter_transcript"];
        let mut view = view(None);
        view.templates = codes
            .iter()
            .map(|code| TemplateChoice {
                code: (*code).to_string(),
                label: format!("Label — {code}"),
            })
            .collect();
        // Selecting one that shares no service with the others must still
        // render — the old product/retainer pairing rule is gone.
        view.retainer_template_code = "onboarding__letter_transcript".to_string();

        let html = render_start(&view);
        for code in codes {
            assert!(
                html.contains(&format!("value=\"{code}\"")),
                "`{code}` must be offered; the picker narrows by nothing\n{html}"
            );
        }
        assert!(
            !html.contains("does not match the selected service"),
            "no pairing is refused\n{html}"
        );
    }

    #[test]
    fn the_form_keeps_the_browser_e2e_field_names() {
        // `server/tests/browser_e2e.rs` drives this form by
        // `input[name='client_email']` and `select[name='retainer_template_code']`,
        // and `accessibility_e2e.rs` scopes axe to `form.admin-form`. Losing any
        // of the three times the nightly deploy gate out.
        let html = render_start(&view(None));
        assert!(html.contains("name=\"client_email\""), "{html}");
        assert!(html.contains("name=\"retainer_template_code\""), "{html}");
        assert!(html.contains("admin-form"), "{html}");
        assert!(
            html.contains("action=\"/app/lawyer/retainers/new\""),
            "{html}"
        );
        assert_forms_accessible(&html, "retainer_start::LawyerRetainerStart");
    }

    #[test]
    fn a_refused_start_renders_the_flash_over_the_echoed_values() {
        // The `POST` refusals redirect back here rather than re-rendering, so
        // the reason and the submitted values have to survive the round trip.
        let html = render_start(&view(Some("client email needs an @")));
        assert!(html.contains("nav-form-error"), "{html}");
        assert!(html.contains(">client email needs an @<"), "{html}");
        assert!(html.contains("value=\"libra@example.com\""), "{html}");
        // The echoed template stays selected, not the default.
        let selected_at = html.find("selected").expect("a chosen option: {html}");
        let letter_at = html
            .find("onboarding__letter")
            .expect("the echoed option renders");
        assert!(letter_at < selected_at, "{html}");
    }

    fn template(code: &str, title: &str, firm: Option<uuid::Uuid>) -> store::templates::Template {
        store::templates::Template {
            id: uuid::Uuid::now_v7(),
            code: code.to_string(),
            title: title.to_string(),
            respondent_type: "person".to_string(),
            project_id: None,
            firm_id: firm,
            is_current: true,
            asset_id: None,
            form_code: None,
            kind: Some("onboarding".to_string()),
            source_commit_sha: None,
            body_sha256: None,
            inserted_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    /// A Firm's Library replaces the shared row of the same code, adds its
    /// own onboarding Drafts and its named retainer, and leaves out a Firm
    /// Draft that is neither.
    #[test]
    fn the_picker_lays_the_firm_library_over_the_shared_catalog() {
        let firm = Some(uuid::Uuid::now_v7());
        let shared = [
            template("onboarding__letter", "Shared Letter", None),
            template("nv__llc_formation", "Formation", None),
        ];
        let library = [
            template("onboarding__letter", "Firm Letter", firm),
            template("firm_retainer", "Firm Retainer", firm),
            template("firm_memo", "Firm Memo", firm),
        ];
        let labels: Vec<String> = picker_choices(&shared, &library, Some("firm_retainer"))
            .into_iter()
            .map(|choice| choice.label)
            .collect();
        assert_eq!(
            labels,
            [
                "Firm Letter — onboarding__letter",
                "Firm Retainer — firm_retainer",
            ]
        );
    }

    /// With nothing submitted, the Firm's Library retainer is the selection.
    #[test]
    fn the_firm_library_retainer_is_the_default_selection() {
        let mut view = view(None);
        view.retainer_template_code = String::new();
        view.default_template_code = "firm_retainer".to_string();
        view.templates.push(TemplateChoice {
            code: "firm_retainer".to_string(),
            label: "Firm Retainer — firm_retainer".to_string(),
        });
        let html = render_start(&view);
        let selected_at = html.find("selected").expect("a chosen option");
        let retainer_at = html
            .find("value=\"firm_retainer\"")
            .expect("the Firm option");
        let letter_at = html
            .find("value=\"onboarding__letter\"")
            .expect("the shared option");
        assert!(
            letter_at < retainer_at && retainer_at < selected_at,
            "{html}"
        );
    }
}
