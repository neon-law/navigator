//! Brand presentation and separate social account connections.
//! Published logo and font assets are read-only here; the native form edits colour.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{Card, Field, FormCard, Heading, Icon, IconName, SocialMarkGlyph};
use crate::people::ViewerRole;

/// Everything the edit page renders.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct BrandsEditView {
    #[serde(default)]
    pub tokens_href: String,
    #[serde(default)]
    pub firm_name: String,
    pub role: ViewerRole,
    #[serde(default)]
    pub logo: Option<crate::components::AppLogo>,
    pub key: String,
    pub fields: Option<BrandPresentationFields>,
    #[serde(default)]
    pub firm_href: Option<String>,
    #[serde(default)]
    pub social_accounts: Vec<SocialAccountView>,
    #[serde(default)]
    pub social_selections: std::collections::BTreeMap<String, String>,
    pub csrf_token: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub notice: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct SocialAccountView {
    pub provider: String,
    pub provider_account_id: String,
    pub display_name: Option<String>,
    pub status: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
}

impl SocialAccountView {
    fn matches_selection(&self, selection: &str) -> bool {
        self.provider_account_id == selection
            || match self.provider.as_str() {
                "x" => self
                    .display_name
                    .as_deref()
                    .is_some_and(|name| name.eq_ignore_ascii_case(selection)),
                "threads" => self.display_name.as_deref() == Some(selection),
                "tiktok" => self
                    .display_name
                    .as_deref()
                    .is_some_and(|name| name.ends_with(&format!(" (@{selection})"))),
                _ => false,
            }
    }
}

/// Prefill for the presentation form and the read-only upload status.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct BrandPresentationFields {
    pub name: String,
    pub typeface: String,
    pub primary_color: String,
    pub font_family: String,
    /// The uploaded logo's serving URL, when one exists.
    pub logo_url: Option<String>,
    /// The uploaded font's serving URL and its attested licence, when one
    /// exists.
    pub font: Option<(String, String)>,
    /// The registered Brand's built-in logo, when no replacement is attached.
    #[serde(default)]
    pub default_logo_url: Option<String>,
    /// The currently configured catalog typeface, separate from Firm uploads.
    #[serde(default)]
    pub default_typeface_label: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct BrandsEditQuery {
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub social_reused: bool,
    #[serde(default)]
    pub social_connected: Option<usize>,
    #[serde(default)]
    pub social_disconnected: Option<usize>,
}

/// Load the edit form for `{key}`.
#[server]
#[allow(clippy::too_many_lines)]
pub async fn get_brands_edit() -> Result<BrandsEditView, ServerFnError> {
    let role = crate::admin_listing::require_admin().await?;
    let axum::extract::Path(key) =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Path<String>, _>()
            .await?;
    let axum::extract::Query(query) = dioxus_fullstack_core::FullstackContext::extract::<
        axum::extract::Query<BrandsEditQuery>,
        _,
    >()
    .await?;
    let csrf_token = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<crate::csrf::CsrfToken>,
        _,
    >()
    .await
    .map(|axum::Extension(token)| token.0)
    .unwrap_or_default();

    let surreal = consume_context::<store::surreal::SurrealDb>();
    let actor_person_id = crate::admin_listing::injected_person_id().await;
    let Some(actor_person_id) = actor_person_id else {
        return Err(ServerFnError::new(
            "The current person could not be resolved.",
        ));
    };

    let base = BrandsEditView {
        tokens_href: crate::app_chrome::app_tokens_href_from_context().await,
        firm_name: crate::app_chrome::firm_name_from_context().await,
        logo: crate::app_chrome::app_logo_from_context().await,
        role,
        key: key.clone(),
        fields: None,
        firm_href: None,
        social_accounts: Vec::new(),
        social_selections: std::collections::BTreeMap::default(),
        csrf_token,
        error: query.error.clone(),
        notice: if query.social_reused {
            Some(
                "This saved destination is already connected with usable authorization."
                    .to_string(),
            )
        } else if query.social_connected.is_some() {
            Some(
                "Social account authorization completed. Verified connections are shown below."
                    .to_string(),
            )
        } else if query.social_disconnected.is_some() {
            Some("The account is disconnected. Its saved destination is unchanged.".to_string())
        } else {
            None
        },
    };

    let brand = match store::brands::find_by_key_for_actor(
        &surreal,
        store_role(role),
        Some(actor_person_id),
        &key,
    )
    .await
    {
        Ok(brand) => brand,
        Err(
            store::brands::BrandError::NotAuthorized
            | store::brands::BrandError::NoSuchBrand(_)
            | store::brands::BrandError::NoSuchFirm(_),
        ) => {
            dioxus_fullstack_core::FullstackContext::commit_http_status(
                axum::http::StatusCode::NOT_FOUND,
                None,
            );
            return Ok(base);
        }
        Err(error) => return Err(ServerFnError::new(error.to_string())),
    };

    let social_accounts =
        store::social_accounts::for_actor(&surreal, store_role(role), actor_person_id)
            .await
            .map_err(|error| ServerFnError::new(error.to_string()))?
            .into_iter()
            .filter(|account| account.brand_key == key)
            .map(|account| SocialAccountView {
                provider: account.provider.as_str().to_string(),
                provider_account_id: account.provider_account_id,
                display_name: account.display_name,
                status: if account.status == "connected"
                    && (!account.provider.missing_scopes(&account.scopes).is_empty()
                        || account
                            .provider
                            .token_renewal(account.expires_at, chrono::Utc::now())
                            == store::social_accounts::TokenRenewal::Reconnect)
                {
                    "reconnect needed".to_string()
                } else {
                    account.status
                },
                scopes: account.scopes,
                expires_at: account.expires_at.map(|expiry| expiry.to_rfc3339()),
            })
            .collect();

    let logo_url = brand
        .logo_object_key
        .as_deref()
        .map(|object_key| format!("/assets/{object_key}"));
    let font = brand
        .font_object_key
        .as_deref()
        .zip(brand.font_licence.as_deref())
        .map(|(object_key, licence)| (format!("/assets/{object_key}"), licence.to_string()));
    let compiled = views::brand::BrandKey::parse(&key);
    let default_logo_url = compiled.map(|key| {
        key.resolve_branding(&views::brand::DEFAULT_BRANDING)
            .firm
            .logo_href
            .to_string()
    });
    let default_typeface_label = brand
        .typeface
        .as_deref()
        .and_then(views::brand_presentation::typeface_by_id)
        .or_else(|| {
            brand
                .typeface
                .is_none()
                .then(|| compiled.map(views::brand::BrandKey::default_typeface))
                .flatten()
        })
        .map(|face| face.label.to_string());

    Ok(BrandsEditView {
        fields: Some(BrandPresentationFields {
            name: brand.name,
            typeface: brand.typeface.unwrap_or_default(),
            primary_color: brand.primary_color.unwrap_or_default(),
            font_family: brand.font_family.unwrap_or_default(),
            logo_url,
            font,
            default_logo_url,
            default_typeface_label,
        }),
        social_accounts,
        social_selections: store::social_account_selections::for_brand(&surreal, &key)
            .await
            .map_err(|error| ServerFnError::new(error.to_string()))?
            .into_iter()
            .map(|selection| (selection.provider, selection.provider_account_id))
            .collect(),
        firm_href: brand.firm_id.map(|id| format!("/app/admin/firms/{id}")),
        ..base
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

/// Route entry for `/app/admin/brands/{key}/edit`.
#[component]
pub fn BrandsEdit() -> Element {
    let resource = use_server_future(get_brands_edit)?;
    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        Some(Err(_)) => {
            return rsx! {
                main { id: "brands-edit", p { "Failed to load the brand." } }
            };
        }
        None => {
            return rsx! {
                main { id: "brands-edit", p { "Loading…" } }
            };
        }
    };
    brands_edit_body(&view)
}

fn presentation_form(view: &BrandsEditView, fields: &BrandPresentationFields) -> Element {
    let form_fields = vec![Field::color(
        "Primary colour",
        "primary_color",
        fields.primary_color.clone(),
    )
    .required()
    .help(
        "A #rrggbb hex. Its on-primary text (white or black, whichever contrasts more) must \
             clear WCAG AA 4.5:1, and it must clear 3:1 against the light page surface, or the \
             save is refused naming the ratio.",
    )];
    rsx! {
        FormCard {
            title: format!("Edit {}", fields.name),
            action: format!("/app/admin/brands/{}/edit", view.key),
            submit_label: "Save presentation".to_string(),
            heading: Heading::H1,
            csrf_token: Some(view.csrf_token.clone()),
            fields: form_fields,
        }
    }
}

fn logo_status(fields: &BrandPresentationFields) -> Element {
    rsx! {
        section { id: "brand-logo",
            Card {
                header: rsx! { h2 { "Logo" } },
                if let Some(url) = &fields.logo_url {
                    p { img { class: "brand-logo-preview", src: "{url}", alt: "Current logo" } }
                    p { a { href: "{url}", "Open logo asset" } }
                } else if let Some(url) = &fields.default_logo_url {
                    p { img { class: "brand-logo-preview", src: "{url}", alt: "Current built-in logo" } }
                    p { a { href: "{url}", "Open built-in logo" } }
                } else {
                    p { class: "muted", "No logo configured." }
                }
            }
        }
    }
}

fn font_status(view: &BrandsEditView, fields: &BrandPresentationFields) -> Element {
    rsx! {
        section { id: "brand-font",
            Card {
                header: rsx! { h2 { "Font" } },
                if let Some((url, licence)) = &fields.font {
                    p {
                        "Uploaded font: "
                        a { href: "{url}", "download" }
                        " (" {licence.clone()} ")"
                    }
                } else if let Some(label) = &fields.default_typeface_label {
                    p { "Current typeface: " {label.clone()} }
                    p { a { href: format!("/public/css/brand-{}-tokens.css", view.key), "Open Brand font stylesheet" } }
                } else {
                    p { class: "muted", "No font configured." }
                }
            }
        }
    }
}

fn social_accounts(view: &BrandsEditView) -> Element {
    let providers = [
        "facebook",
        "instagram",
        "threads",
        "youtube",
        "linkedin",
        "x",
        "tiktok",
    ];
    rsx! {
        section { id: "brand-social-accounts", aria_label: "Social accounts",
            h2 { "Social accounts" }
            p { "Connect accounts to let Navigator publish this Brand’s approved scheduled Posts and manage social conversations. Disconnect an account to stop." }
            div { class: "brand-social-accounts-grid",
                section { aria_label: "Meta", h3 { "Meta" }
                    p { "Facebook and Instagram share Facebook Login. Save the Page and its linked Instagram professional account, then connect Meta once. Threads requires separate authorization." }
                    for provider in ["facebook", "instagram", "threads"] { {social_provider(view, provider)} }
                }
                for provider in providers.into_iter().filter(|provider| !matches!(*provider, "facebook" | "instagram" | "threads")) {
                    {social_provider(view, provider)}
                }
            }
        }
    }
}

fn social_provider(view: &BrandsEditView, provider: &str) -> Element {
    let label = match provider {
        "facebook" => "Facebook",
        "instagram" => "Instagram",
        "threads" => "Threads",
        "youtube" => "YouTube",
        "linkedin" => "LinkedIn",
        "x" => "X",
        "tiktok" => "TikTok",
        _ => provider,
    };
    let saved = view
        .social_selections
        .get(provider)
        .filter(|id| !id.is_empty());
    let input_id = format!("provider_account_id-{provider}");
    rsx! {
                    section { class: "brand-social-provider", aria_label: "{label}",
                        h3 { class: "brand-social-provider__heading",
                            match provider {
                                "youtube" => rsx! { Icon { name: IconName::YouTube } },
                                "linkedin" => rsx! { Icon { name: IconName::LinkedIn } },
                                "x" => rsx! { Icon { name: IconName::X } },
                                _ => rsx! { SocialMarkGlyph { label: label.to_string() } },
                            }
                            "{label}"
                        }
                        for account in view.social_accounts.iter().filter(|account| account.provider == provider) {
                            p { "{account.display_name.as_deref().unwrap_or(&account.provider_account_id)} ({account.status})" }
                            details {
                                summary { "Authorization details" }
                                p { "Permissions: " {account.scopes.join(", ")} }
                                p { "Authorization expiry: " {account.expires_at.as_deref().unwrap_or("No scheduled expiry")} }
                            }
                            if account.status != "revoked" {
                                form {
                                    class: "nav-form",
                                    method: "post",
                                    action: format!("/app/admin/brands/{}/social/{}/{}/disconnect", view.key, provider, account.provider_account_id),
                                    aria_label: format!("Disconnect {provider} account {}", account.provider_account_id),
                                    input { class: "nav-control", r#type: "hidden", name: "_csrf", value: "{view.csrf_token}" }
                                    button { class: "nav-btn nav-btn--secondary", r#type: "submit", "Disconnect" }
                                }
                            }
                        }
                        if view.social_selections.contains_key(provider) {
                            if !view.social_accounts.iter().any(|account| account.provider == provider && account.status == "connected" && view.social_selections.get(provider).is_some_and(|selection| account.matches_selection(selection))) {
                                p { "Authorization needed. Connect or reconnect this saved destination." }
                            }
                        }
                        form {
                            class: "nav-form",
                            method: "post",
                            action: format!("/app/admin/brands/{}/social/{provider}/connect", view.key),
                            aria_label: format!("Connect {provider} account"),
                            input { class: "nav-control", r#type: "hidden", name: "_csrf", value: "{view.csrf_token}" }
                            div { class: "nav-field",
                                label { r#for: "{input_id}",
                                    if matches!(provider, "x" | "tiktok") { "{label} username" }
                                    else { "{label} account ID" }
                                }
                                input { class: "nav-control", r#type: "text", name: "provider_account_id",
                                    id: "{input_id}", required: true, readonly: saved.is_some(),
                                    value: saved.map_or("", String::as_str),
                                }
                                if saved.is_some() {
                                    button { class: "nav-btn nav-btn--secondary", r#type: "button",
                                        "data-account-update": "{input_id}", aria_controls: "{input_id}",
                                        aria_pressed: "false", "Update {label} account"
                                    }
                                }
                            }
                            if provider == "x" {
                                p { "Enter this Brand's X username without @. X publishes text only within a conservative 280-weight budget." }
                            }
                            if provider == "tiktok" {
                                p { "Enter this Brand's TikTok username without @. TikTok Business access approval and verified video URLs are required." }
                            }
                            if provider == "threads" {
                                p { "Enter this Brand's exact Threads username without @, or its provider account ID." }
                            }
                            if provider == "facebook" {
                                p { "Enter this Brand's exact Facebook Page ID. Meta connects only this Page and the explicitly selected linked Instagram account." }
                            }
                            if provider == "instagram" {
                                p { "Enter the professional Instagram account ID linked to the saved Facebook Page. Connect/Reconnect Meta verifies this link." }
                            }
                            if provider == "youtube" {
                                p { "Enter this Brand's exact YouTube Channel ID. Only this Channel will be connected." }
                            }
                            if provider == "linkedin" {
                                p { "Enter this Brand's numeric Company Page ID. LinkedIn publishes text only." }
                            }
                            button { class: "nav-btn nav-btn--secondary", r#type: "submit",
                                "data-account-save": "", hidden: saved.is_some(),
                                formnovalidate: true,
                                formaction: format!("/app/admin/brands/{}/social/{provider}/selection", view.key),
                                "Save {provider} account"
                            }
                            if saved.is_none() { p { "Save the destination first, then connect. Saving does not authorize or publish." } }
                            if provider != "instagram" {
                                button { class: "nav-btn nav-btn--secondary", r#type: "submit",
                                    "data-account-connect": "",
                                    if provider == "facebook" { "Connect/Reconnect Meta" } else { "Connect {provider}" }
                                }
                            } else { p { "Use Connect/Reconnect Meta after saving the linked Facebook Page." } }
                        }
                    }
    }
}

/// Split from the component so tests render a fixed view.
pub fn brands_edit_body(view: &BrandsEditView) -> Element {
    let role = view.role;
    let firm_name = view.firm_name.clone();
    rsx! {
        document::Title { "{firm_name} | Brands | Edit" }
        document::Script { src: "/public/js/brand-social-accounts.js", defer: true }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{view.tokens_href}" }
        crate::components::AppNavbar {
            destinations: crate::app_chrome::app_destinations(role),
            logo: view.logo.clone(),
        }
        main { id: "brands-edit", class: "nav-theme",
            p { class: "page-subtitle", id: "brand-website",
                "{crate::brand_website::BrandWebsite::from_key(&view.key).host_line()}"
            }
        if let Some(error) = &view.error {
            p { class: "nav-form-error", role: "alert", "{error}" }
        }
            if let Some(notice) = &view.notice { p { role: "status", "{notice}" } }
            match &view.fields {
                Some(fields) => rsx! {
                    if let Some(href) = &view.firm_href {
                        p { a { href: "{href}", "Firm settings and people" } }
                    }
                    {social_accounts(view)}
                    {presentation_form(view, fields)}
                    {logo_status(fields)}
                    {font_status(view, fields)}
                    p { a { href: "/app/admin/brands", "← Brands" } }
                },
                None => rsx! {
                    h1 { "Brand not found" }
                    p { "No brand exists with key " code { "{view.key}" } "." }
                    p { a { href: "/app/admin/brands", "← Brands" } }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::form::assert_forms_accessible;

    fn view(fields: Option<BrandPresentationFields>) -> BrandsEditView {
        BrandsEditView {
            tokens_href: String::new(),
            firm_name: "Neon Law".to_string(),
            role: ViewerRole::Owner,
            logo: None,
            key: "neon".to_string(),
            fields,
            firm_href: Some("/app/admin/firms/synthetic-firm".into()),
            csrf_token: "TOK".to_string(),
            social_accounts: Vec::new(),
            social_selections: std::collections::BTreeMap::default(),
            error: None,
            notice: None,
        }
    }

    fn fields() -> BrandPresentationFields {
        BrandPresentationFields {
            name: "Neon Law".to_string(),
            typeface: "uploaded".to_string(),
            primary_color: "#007c91".to_string(),
            font_family: "Custom Sans".to_string(),
            logo_url: None,
            font: None,
            default_logo_url: None,
            default_typeface_label: None,
        }
    }

    #[test]
    fn social_account_cards_offer_connect_and_disconnect() {
        let mut view = view(Some(fields()));
        view.social_selections
            .insert("facebook".into(), "page-1".into());
        view.social_accounts = vec![SocialAccountView {
            provider: "facebook".into(),
            provider_account_id: "page-1".into(),
            display_name: Some("Neon Page".into()),
            status: "connected".into(),
            scopes: vec!["pages_manage_posts".into()],
            expires_at: None,
        }];
        let html = dioxus_ssr::render_element(brands_edit_body(&view));
        assert!(html.contains("Save facebook account"));
        assert!(html.contains("value=\"page-1\""));
        assert!(html.contains("/social/facebook/selection"));
        assert_eq!(html.matches("aria-label=\"Meta\"").count(), 1);
        assert!(html.contains("Social accounts"), "{html}");
        assert!(html.contains("Neon Page"), "{html}");
        assert!(html.contains("Connect/Reconnect Meta"), "{html}");
        assert!(!html.contains(">Connect instagram<"), "{html}");
        assert!(html.contains("Threads requires separate authorization"));
        assert!(html.contains("Connect linkedin"), "{html}");
        assert!(html.contains("Connect x"), "{html}");
        assert!(!html.contains("name=\"auto_publish\""), "{html}");
        assert!(html.contains("Disconnect an account to stop"), "{html}");
        assert!(html.contains("pages_manage_posts"), "{html}");
        assert!(html.contains("provider_account_id"), "{html}");
        assert!(
            html.contains("/app/admin/brands/neon/social/facebook/connect"),
            "{html}"
        );
        assert!(
            html.contains("/app/admin/brands/neon/social/facebook/page-1/disconnect"),
            "{html}"
        );
        assert!(html.contains("Disconnect"), "{html}");
    }

    #[test]
    fn saved_destinations_are_read_only_with_an_explicit_update_toggle() {
        let mut view = view(Some(fields()));
        view.social_selections
            .insert("youtube".into(), "synthetic-channel".into());
        let html = dioxus_ssr::render_element(brands_edit_body(&view));
        assert_forms_accessible(&html, "brand social accounts");
        for provider in [
            "facebook",
            "instagram",
            "threads",
            "youtube",
            "linkedin",
            "x",
            "tiktok",
        ] {
            let id = format!("provider_account_id-{provider}");
            assert_eq!(html.matches(&format!("id=\"{id}\"")).count(), 1, "{html}");
            assert!(html.contains(&format!("for=\"{id}\"")), "{html}");
        }
        assert!(html.contains("readonly"), "{html}");
        assert!(
            html.contains("data-account-update=\"provider_account_id-youtube\""),
            "{html}"
        );
        assert!(html.contains("aria-pressed=\"false\""), "{html}");
        assert!(!html.contains("Saved selection:"), "{html}");
        assert!(!html.contains("auto_publish"), "{html}");
        for label in ["Facebook", "Instagram", "Threads", "TikTok"] {
            assert!(
                html.contains(&format!("data-social-mark=\"{label}\"")),
                "{html}"
            );
        }
    }

    #[test]
    fn the_presentation_form_is_a_hex_input_and_posts_to_the_key() {
        let html = dioxus_ssr::render_element(brands_edit_body(&view(Some(fields()))));
        assert_forms_accessible(&html, "brand presentation");
        assert!(
            html.contains(r#"action="/app/admin/brands/neon/edit""#),
            "{html}"
        );
        assert!(!html.contains(r#"name="typeface""#), "{html}");
        assert!(html.contains(r#"name="primary_color""#), "{html}");
        assert!(html.contains(r##"value="#007c91""##), "{html}");
        assert!(!html.contains(r#"name="palette""#), "{html}");
        assert!(!html.contains(r#"name="font_family""#), "{html}");
        assert!(html.contains("www.neonlaw.com"), "{html}");
    }

    #[test]
    fn presentation_uses_existing_assets_without_upload_boxes() {
        let html = dioxus_ssr::render_element(brands_edit_body(&view(Some(fields()))));
        assert!(!html.contains("multipart/form-data"));
        assert!(!html.contains("Upload logo"));
        assert!(!html.contains("Upload font"));
        assert!(!html.contains(r#"name="typeface""#));
        assert!(html.contains(r#"type="color""#));
        assert!(html.contains("Firm settings and people"));
    }

    #[test]
    fn an_uploaded_logo_and_font_render_their_status() {
        let mut fields = fields();
        fields.logo_url = Some("/app/admin/brands/neon/logo".to_string());
        fields.font = Some((
            "/assets/fonts/brands/neon/abc.woff2".to_string(),
            "OFL-1.1".to_string(),
        ));
        let html = dioxus_ssr::render_element(brands_edit_body(&view(Some(fields))));
        assert!(html.contains("Current logo"), "{html}");
        assert!(html.contains("Uploaded font:"), "{html}");
        assert!(html.contains("OFL-1.1"), "{html}");
    }

    #[test]
    fn built_in_brand_assets_are_visible_without_replacement_uploads() {
        let mut fields = fields();
        fields.default_logo_url = Some("/public/brand/vesta.svg".into());
        fields.default_typeface_label = Some("EB Garamond".into());
        let mut view = view(Some(fields));
        view.key = "vesta".into();
        let html = dioxus_ssr::render_element(brands_edit_body(&view));
        assert!(html.contains("/public/brand/vesta.svg"), "{html}");
        assert!(html.contains("Current typeface: "), "{html}");
        assert!(html.contains("EB Garamond"), "{html}");
        assert!(
            html.contains("/public/css/brand-vesta-tokens.css"),
            "{html}"
        );
        assert!(!html.contains("No font configured"), "{html}");
        assert!(!html.contains("No logo configured"), "{html}");
    }

    #[test]
    fn a_refusal_names_the_rule() {
        let mut view = view(Some(fields()));
        view.error = Some(
            "#f5f5a0 is 1.1:1 against the light page surface; it must be at least 3.0:1."
                .to_string(),
        );
        let html = dioxus_ssr::render_element(brands_edit_body(&view));
        assert!(html.contains("must be at least 3.0:1"), "{html}");
        assert!(
            html.find("role=\"alert\"").unwrap()
                < html.find("id=\"brand-social-accounts\"").unwrap()
        );
    }

    #[test]
    fn a_missing_brand_renders_not_found() {
        let html = dioxus_ssr::render_element(brands_edit_body(&view(None)));
        assert!(html.contains("Brand not found"), "{html}");
    }
}
