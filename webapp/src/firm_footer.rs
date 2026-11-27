//! The `/app` footer: a single centered copyright line naming the Firm that
//! wears the current brand, resolved per request rather than a process-wide
//! constant (ENG-589).
//!
//! [`FirmFooterModel`] is also what `crate::public_chrome::apply_resolved_firm_footer`
//! reads to overlay the live Firm's legal entity and home onto the public
//! chrome once a Firm exists.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// The data the `/app` footer needs, resolved once per request from the Firm
/// that wears the current brand — never a process-wide constant.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct FirmFooterModel {
    pub legal_entity: String,
    /// Server-proven canonical Entity may retain the compiled operator facts.
    #[serde(default)]
    pub compiled_identity_allowed: bool,
    pub copyright_year: i32,
    /// The resolved Firm's own home — the production home of the firm's own
    /// brand when the Firm wears it. The public footer links the copyright's
    /// legal-entity name here. Empty when the Firm wears no compiled firm
    /// brand (a firm with no site of its own yet): its name then renders
    /// unlinked rather than linking another firm's site. The compiled
    /// fallback resolves to the compiled firm brand's production home, which
    /// is also what a fresh deployment (no `firm` rows) renders.
    #[serde(default)]
    pub firm_home_href: String,
}

/// The `/app` footer: one centered copyright line naming the resolved Firm's
/// legal entity.
#[component]
pub fn FirmFooter(model: FirmFooterModel) -> Element {
    rsx! {
        footer { class: "app-footer",
            p { class: "app-footer__copyright", "© {model.copyright_year} {model.legal_entity}" }
        }
    }
}

/// The footer for the client's view of a Project. Unlike the
/// application-wide [`FirmFooter`], this names the firm behind the Project's
/// own brand: one signed-in person can have matters with different firms.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct ClientPortalFooterModel {
    pub legal_entity: String,
    pub copyright_year: i32,
}

/// The matter page's one-line copyright notice: the firm's name alone.
#[component]
pub fn ClientPortalFooter(model: ClientPortalFooterModel) -> Element {
    rsx! {
        footer { class: "app-footer app-footer--client-portal",
            p { class: "app-footer__copyright", "© {model.copyright_year} {model.legal_entity}" }
        }
    }
}

/// Render the footer to a standalone HTML string for injection into every
/// `/app` HTML response. Server-only: `dioxus-ssr` is not in the wasm bundle.
#[cfg(feature = "server")]
#[must_use]
pub fn render_firm_footer(model: FirmFooterModel) -> String {
    let mut dom = VirtualDom::new_with_props(FirmFooter, FirmFooterProps { model });
    dom.rebuild_in_place();
    dioxus_ssr::render(&dom)
}

#[cfg(feature = "server")]
fn compiled_site_brand(current: views::brand::BrandKey) -> &'static views::brand::SiteBrand {
    if current == views::brand::brand_key() {
        &views::brand::FIRM_BRAND
    } else {
        &current
            .resolve_branding(&views::brand::DEFAULT_BRANDING)
            .firm
    }
}

/// The no-store fallback: the compiled `Branding` for `current`. What a fresh
/// deployment (no `firm` rows yet) or a brand key no Firm wears resolves to.
#[cfg(feature = "server")]
#[must_use]
pub fn compiled_firm_footer_model(
    current: views::brand::BrandKey,
    copyright_year: i32,
) -> FirmFooterModel {
    let brand = compiled_site_brand(current);
    FirmFooterModel {
        legal_entity: brand.legal_entity.to_string(),
        compiled_identity_allowed: true,
        copyright_year,
        // The compiled firm brand's production home, for Guieb & Shook PC's
        // own brands only: another firm's brand never links Guieb & Shook.
        firm_home_href: if current.is_guieb_shook() {
            views::brand::BrandKey::GuiebShook.public_home_href()
        } else {
            String::new()
        },
    }
}

/// Resolve [`FirmFooterModel`] for `current`: the Firm wearing that brand
/// key, its Entity's legal name, and its own home. No Firm wearing `current`
/// (or no live rows at all) falls back to [`compiled_firm_footer_model`].
///
/// # Errors
///
/// Never returns an error: any store failure degrades to the compiled
/// fallback rather than breaking every page's footer.
#[cfg(feature = "server")]
pub async fn resolve_firm_footer_model(
    application_store: &store::application_store::ApplicationStore,
    current: views::brand::BrandKey,
    copyright_year: i32,
) -> FirmFooterModel {
    let fallback = || compiled_firm_footer_model(current, copyright_year);

    let Ok(Some(firm_id)) =
        store::firms::firm_id_for_brand_key(application_store, current.as_str()).await
    else {
        return fallback();
    };
    let Ok(Some(firm)) = store::firms::find_by_id(application_store, firm_id).await else {
        return fallback();
    };
    let Some(entity_id) = firm.entity_id else {
        return fallback();
    };
    let Ok(Some(entity)) = store::entities::find_by_id(application_store, entity_id).await else {
        return fallback();
    };
    let Ok(keys) = store::firms::brand_keys_for_firm(application_store, firm_id).await else {
        return fallback();
    };
    if keys.is_empty() {
        return fallback();
    }

    let compiled_identity_allowed = entity.firm_anchor_key.as_deref()
        == Some(
            compiled_site_brand(current)
                .legal_entity
                .trim()
                .to_lowercase()
                .as_str(),
        );
    // The canonical Firm's home is its compiled firm Brand's, even before
    // that Brand row is attached. Other operators link it only when they
    // wear that compiled firm Brand; a matching display name is not proof
    // of identity.
    let canonical_firm = compiled_identity_allowed && current.is_guieb_shook();
    let firm_home_href = if canonical_firm
        || keys
            .iter()
            .any(|key| key == views::brand::BrandKey::GuiebShook.as_str())
    {
        views::brand::BrandKey::GuiebShook.public_home_href()
    } else {
        String::new()
    };

    FirmFooterModel {
        compiled_identity_allowed,
        legal_entity: entity.name,
        copyright_year,
        firm_home_href,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssr(app: fn() -> Element) -> String {
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    fn model() -> FirmFooterModel {
        FirmFooterModel {
            legal_entity: "Guieb & Shook PC".to_string(),
            compiled_identity_allowed: true,
            copyright_year: 2026,
            firm_home_href: "https://www.guiebshook.com".to_string(),
        }
    }

    #[test]
    fn the_footer_renders_a_centered_copyright_line() {
        fn app() -> Element {
            rsx! {
                FirmFooter { model: model() }
            }
        }
        let html = ssr(app);
        assert!(html.contains("© 2026 Guieb &#38; Shook PC"), "{html}");
        assert!(html.contains("app-footer__copyright"), "{html}");
        assert!(html.starts_with("<footer"), "{html}");
    }

    /// Every class the footer emits is styled by the theme it ships with.
    /// Mirrors `sample_matters_banner`'s own guard: a renamed class with no
    /// matching rule renders as unstyled text and nothing else catches it.
    #[test]
    fn every_class_the_footer_emits_is_styled_by_the_theme() {
        fn app() -> Element {
            rsx! {
                FirmFooter { model: model() }
            }
        }

        let theme =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../server/public/css/theme.css");
        let css = std::fs::read_to_string(&theme)
            .unwrap_or_else(|e| panic!("the theme stylesheet must be readable: {e}"));
        let out = ssr(app);

        let mut classes = Vec::new();
        let mut rest = out.as_str();
        while let Some(at) = rest.find("class=\"") {
            rest = &rest[at + 7..];
            let Some(end) = rest.find('"') else { break };
            classes.extend(rest[..end].split_whitespace().map(str::to_string));
            rest = &rest[end..];
        }
        assert!(!classes.is_empty(), "the footer emits classes: {out}");

        for class in classes
            .iter()
            .filter(|class| class.starts_with("app-footer"))
        {
            assert!(
                css.contains(&format!(".{class}")),
                "`{class}` is emitted by the footer but has no rule in \
                 server/public/css/theme.css"
            );
        }
    }

    /// Public chrome reads the legal entity and firm home from the compiled fallback.
    #[cfg(feature = "server")]
    #[test]
    fn the_compiled_fallback_resolves_the_legal_entity_and_the_firm_home() {
        let model = compiled_firm_footer_model(views::brand::BrandKey::Neon, 2026);
        assert_eq!(model.legal_entity, "Guieb & Shook PC");
        assert_eq!(
            model.firm_home_href,
            views::brand::BrandKey::GuiebShook.public_home_href(),
            "the compiled fallback resolves the compiled firm brand's production home"
        );
        let other = compiled_firm_footer_model(views::brand::BrandKey::LexTecnica, 2026);
        assert_eq!(other.legal_entity, "Lex Tecnica, Ltd.");
        assert_eq!(
            other.firm_home_href, "",
            "a Lex Tecnica brand links no Guieb home"
        );
    }

    /// The seeded `brand` row wears the same wordmark the compiled
    /// `Branding` publishes.
    ///
    /// `store` cannot depend on `views`, so the seeded row's name is a
    /// hand-copied constant — and this is a place in the workspace that can
    /// see both halves and hold them together. Without it, a deployment with
    /// a seeded Firm names a brand differently than its own pages do.
    #[cfg(feature = "server")]
    #[test]
    fn every_compiled_brand_seeds_its_published_wordmark() {
        for key in views::brand::BrandKey::ALL {
            let published = key
                .resolve_branding(&views::brand::DEFAULT_BRANDING)
                .firm
                .site_name;
            assert_eq!(
                store::seed::compiled_brand_name(key.as_str()),
                Some(published),
                "{} seeds a brand row named for the wordmark it publishes",
                key.as_str(),
            );
        }
    }

    /// The seeded `brand` row's website URL is the compiled registry's own
    /// production host. `store` cannot see `views`, so the URL is a copied
    /// constant; its host is the brand's approved email domain, so this is
    /// the check that the copy names the domain the registry serves.
    #[cfg(feature = "server")]
    #[test]
    fn every_compiled_brand_seeds_its_registry_website_url() {
        // Lex Tecnica's brands seed the firm's own sites instead, which is
        // what approves its people's `lextecnica.com` addresses.
        assert_eq!(
            store::seed::compiled_brand_website_url("lex-tecnica"),
            Some("https://www.lextecnica.com")
        );
        for key in views::brand::BrandKey::ALL.iter().filter(|key| {
            !matches!(
                key,
                views::brand::BrandKey::LexTecnica | views::brand::BrandKey::IepDefenders
            )
        }) {
            assert_eq!(
                store::seed::compiled_brand_website_url(key.as_str()),
                Some(format!("https://{}", key.canonical_host()).as_str()),
                "{} seeds the website URL its registry entry serves",
                key.as_str(),
            );
        }
    }

    #[cfg(feature = "server")]
    #[tokio::test]
    async fn scoped_fallback_preserves_custom_identity_and_explicit_other_brand() {
        let mut custom = views::brand::DEFAULT_BRANDING;
        custom.firm.legal_entity = "Synthetic Custom Entity LLC";
        custom.firm.site_name = "Synthetic Custom Practice";
        custom.firm.logo_href = "/synthetic-custom-logo.svg";
        custom.firm_family = &[];
        let custom = Box::leak(Box::new(custom));
        views::brand::scope(custom, async {
            let model = compiled_firm_footer_model(views::brand::BrandKey::Neon, 2026);
            assert_eq!(model.legal_entity, "Synthetic Custom Entity LLC");
            let other = compiled_firm_footer_model(views::brand::BrandKey::IepDefenders, 2026);
            assert_eq!(other.legal_entity, "Lex Tecnica, Ltd.");
        })
        .await;
        let other = compiled_firm_footer_model(views::brand::BrandKey::IepDefenders, 2026);
        assert_eq!(other.legal_entity, "Lex Tecnica, Ltd.");
    }
}
