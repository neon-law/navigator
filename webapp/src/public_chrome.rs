//! The public-page chrome resolved from the request's scoped brand — the reusable
//! header + footer data every ported public marketing page needs (issue #641 /
//! #730 PR6).
//!
//! Extracted from the first page port (`team_nick`) once a second page needed
//! the same header nav + footer legal strip. The DTOs are wasm-safe (plain
//! serde); the resolver reads `views::brand` and so is server-only, called from
//! each page's `#[server]` view function.
//!
//! The footer half carries the firm's legal identity only: the legal person
//! the copyright names, the resolved firm home it links, the mark notice, and
//! the disclaimer. The compiled marketing surface the footer once rendered —
//! offices, bar licences, memberships, the family row, social profiles, the
//! contact band — stays in `views::brand` for the consumers that still read it;
//! the footer itself renders three lines. Where the store can answer, the
//! portal pre-layer (`inject_public_utility`, reading
//! `webapp::firm_footer`'s resolved model) overlays the live Firm→Entity
//! values onto this chrome; the compiled values are fallback only.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::SiteFooterLegal;

/// One nav destination, resolved from the brand for the header.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct ChromeNavLink {
    pub label: String,
    pub href: String,
}

/// The header's auth-aware utility links (Portal / role-gated Lawyer·Admin /
/// Sign out for a signed-in viewer, or Sign in for an anonymous visitor on a
/// law-firm brand), resolved from the request session. The wasm-safe request
/// extension the portal router injects (`portal::dioxus_app`), read by each
/// page's `#[server]` view function — the session lives on `portal`'s
/// `SessionData`, which `webapp` cannot see. Empty when no session and the
/// brand is not a law firm.
#[derive(Clone, Default)]
pub struct PublicUtility(pub Vec<ChromeNavLink>);

/// The public-page chrome: everything the [`crate::components::SiteHeader`] and
/// [`crate::components::SiteFooterLegal`] need, resolved from the process brand
/// per request so the wasm client never links the view layer.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct PublicChrome {
    pub brand_name: String,
    pub home_href: String,
    pub logo_href: String,
    /// The absolute URL of the brand's raster mark, for `og:image` — scrapers
    /// drop relative image URLs, so it is resolved against the site origin on
    /// the server.
    pub social_image: String,
    pub destinations: Vec<ChromeNavLink>,
    pub utility: Vec<ChromeNavLink>,
    pub firm_name: String,
    /// The firm's own mark. Distinct from `logo_href`, the header's mark: a
    /// white-label bundle can publish a different mark in each place. Kept on
    /// the chrome for the consumers that still read the firm identity; the
    /// footer itself renders no mark.
    pub firm_logo_href: String,
    /// The resolved firm's home — the door the footer's copyright name links.
    /// Compiled from the firm brand as the fallback, overlaid from the live
    /// Firm resolution (`webapp::firm_footer`) wherever the store can answer.
    /// Empty renders the copyright unlinked, which is what a firm publishing
    /// no home gets.
    pub firm_home_href: String,
    /// The legal person the footer's copyright names. Compiled from the firm
    /// brand as the fallback, overlaid from the live Firm→Entity resolution
    /// wherever the store can answer.
    pub legal_entity: String,
    pub disclaimer: String,
    /// Registration metadata resolved from the firm brand. The notice names
    /// the trademark holder independently of the operating legal entity — the
    /// registration belongs to the mark, not to whichever firm wears the
    /// brand.
    pub trademark: String,
    pub trademark_registration: String,
    pub trademark_record_url: String,
    pub copyright_year: i32,
    /// The firm's published voice line. The footer no longer renders it; the
    /// litigation page dials it from here.
    pub firm_phone: String,
    /// The resolved brand's tokens stylesheet href — `/public/css/brand-
    /// <key>-tokens.css`. Resolved from [`views::brand::brand_key`], the same
    /// registry key that produced the rest of this chrome, so the palette
    /// always matches the identity the header and footer already carry.
    pub tokens_href: String,
    /// Whether the request's header is one of the firm's launched practice
    /// brands. The server resolves this from the closed brand registry so the
    /// wasm client does not need to link the `views` crate. A practice page's
    /// copyright links the resolved firm's home; the firm's own page renders
    /// the name unlinked.
    #[serde(default)]
    pub is_practice: bool,
}

/// The public footer, mapped from an already-resolved [`PublicChrome`].
///
/// ONE footer, byte-identical on every public page: this mapping reads no
/// field a white-label bundle varies per header, so
/// [`crate::components::SiteFooterLegal`] renders the same three lines —
/// copyright, mark notice, disclaimer — wherever a reader reaches it.
/// `a_different_header_renders_the_same_footer` compares two renders byte for
/// byte.
///
/// The header half of the chrome is where a bundle differs — `brand_name`,
/// `logo_href`, `home_href`, `social_image`, and `destinations`. None of them
/// belongs here: `legal_entity` is the footer's identity.
///
/// Pages call `rsx! { PublicFooter { chrome } }` and pass the result to
/// [`crate::components::PublicShell`].
#[component]
pub fn PublicFooter(chrome: PublicChrome) -> Element {
    let practice_footer = chrome.is_practice;
    rsx! {
        // The resolved brand's colour, hoisted here because this is the one
        // place that already knows which brand the page wears. Navigator's
        // shared tokens are teal; each registered brand overrides the alias
        // block. Loading it here means every public page gets the palette —
        // including `/contact`, `/blog`, and the legal pages, which hoist no
        // marketing layer.
        document::Stylesheet { href: "{chrome.tokens_href}" }
        SiteFooterLegal {
            // The copyright names the legal person that renders the legal
            // services — the live Firm→Entity resolution where the store can
            // answer, the compiled fallback otherwise — so it is the same name
            // at the bottom of every page of the brand wearing that firm.
            copyright_holder: chrome.legal_entity.clone(),
            // On a practice page the name links the resolved firm's home (no
            // more hardcoded firm URL); on the firm's own site there is no
            // other door to send a reader to, so the name stands unlinked.
            copyright_href: if practice_footer {
                chrome.firm_home_href.clone()
            } else {
                String::new()
            },
            disclaimer: chrome.disclaimer.clone(),
            // Registration metadata accompanies the ownership notice: the
            // mark belongs to its registration, not to whichever firm wears
            // the brand.
            trademark: chrome.trademark.clone(),
            trademark_registration: chrome.trademark_registration.clone(),
            trademark_record_url: chrome.trademark_record_url.clone(),
            copyright_year: chrome.copyright_year,
        }
    }
}

/// Resolve the firm host's public chrome from the request's scoped brand. Server-only
/// (`views::brand` does not compile to wasm); each page's `#[server]` view
/// function calls this, and the macro stubs those bodies for the wasm client.
///
/// `utility` is the auth-aware header utility group the portal router resolves
/// from the request session (see [`PublicUtility`]) and the page's server
/// function passes through; it is empty for an anonymous visitor on a
/// non-firm brand.
///
#[cfg(feature = "server")]
#[must_use]
pub fn firm_public_chrome(utility: Vec<ChromeNavLink>) -> PublicChrome {
    chrome_for(&views::brand::FIRM_BRAND, utility)
}

/// The firm's own page speaks for itself: no Neon mark notice.
#[cfg(feature = "server")]
fn footer_trademark() -> (&'static str, &'static str, &'static str) {
    if views::brand::brand_key() == views::brand::BrandKey::GuiebShook {
        ("", "", "")
    } else {
        views::brand::firm_trademark()
    }
}

/// Resolve validated public English copy without linking the brand application.
#[cfg(feature = "server")]
pub(crate) fn marketing_copy(key: &str) -> String {
    static COPY: std::sync::OnceLock<views::locales::shared::SharedCatalog> =
        std::sync::OnceLock::new();
    COPY.get_or_init(|| {
        views::locales::shared::SharedCatalog::parse(include_str!(
            "../../neon/locales/en/shared.yaml"
        ))
        .expect("invariant: the compiled shared catalog is validated")
    })
    .lookup(views::brand::brand_key().as_str(), key)
    .unwrap_or_default()
    .to_string()
}

/// Build the public chrome for `brand`'s header, with the firm's footer data.
///
/// One brand reaches this — the firm's — because the site publishes one
/// header. The parameter stays because a white-label bundle still mounts its
/// own brand through it.
#[cfg(feature = "server")]
fn chrome_for(brand: &views::brand::SiteBrand, utility: Vec<ChromeNavLink>) -> PublicChrome {
    use views::brand::FIRM_BRAND;

    // The mark, its registration, and the register's record, resolved together
    // — the notice is one claim and a footer holding two thirds of it makes an
    // ownership statement a reader cannot check.
    let (trademark, registration, record) = footer_trademark();

    let destinations = brand
        .nav
        .iter()
        .map(|link| ChromeNavLink {
            label: link.label.to_string(),
            href: link.href.to_string(),
        })
        .collect();

    PublicChrome {
        brand_name: brand.site_name.to_string(),
        home_href: brand.home_href.to_string(),
        logo_href: brand.logo_href.to_string(),
        social_image: views::assets::absolute_url(brand.social_image),
        destinations,
        utility,
        firm_name: FIRM_BRAND.site_name.to_string(),
        firm_logo_href: FIRM_BRAND.logo_href.to_string(),
        // The compiled fallback for the copyright's link: the firm brand's
        // production home. The request overlay (`inject_public_utility`,
        // reading `webapp::firm_footer`'s resolved model) replaces it with the
        // live Firm's home once a Firm wears the key.
        firm_home_href: views::brand::BrandKey::GuiebShook.public_home_href(),
        legal_entity: FIRM_BRAND.legal_entity.to_string(),
        disclaimer: views::brand::firm_disclaimer().to_string(),
        trademark: trademark.to_string(),
        trademark_registration: registration.to_string(),
        trademark_record_url: record.to_string(),
        // The footer fixes the joint-copyright year too; a deploy-time
        // value replaces the constant when the footer year is wired through.
        copyright_year: 2026,
        firm_phone: views::brand::firm_phone().to_string(),
        tokens_href: crate::brand_style::brand_tokens_href(views::brand::brand_key().as_str()),
        is_practice: views::brand::BrandKey::LIVE.contains(&views::brand::brand_key()),
    }
}

/// Resolve the firm host's public chrome, reading the auth-aware utility group
/// from the request's injected [`PublicUtility`] extension (empty when the
/// portal router injected none). The single entry point each public page's
/// `#[server]` view function calls.
#[cfg(feature = "server")]
pub async fn firm_public_chrome_from_context() -> PublicChrome {
    // Prefer the chrome the portal pre-layer (`inject_public_utility`) resolved
    // on the request task, where the brand `task_local` is live — this server-fn
    // runs on a task that does not inherit it, so building the chrome here would
    // read the default brand under a mounted white-label bundle. Fall back to
    // building it from context if the extension is absent.
    if let Ok(axum::Extension(chrome)) =
        dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<PublicChrome>, _>().await
    {
        return chrome;
    }
    let utility =
        dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<PublicUtility>, _>()
            .await
            .map_or_else(|_| Vec::new(), |axum::Extension(utility)| utility.0);
    firm_public_chrome(utility)
}

/// Prefer a value the portal pre-layer injected on the request, then the
/// Dioxus `ServeConfig` context. Server-fns do not inherit the brand
/// `task_local`, so per-brand page copy has to travel the same extension
/// seam public chrome already uses.
#[cfg(feature = "server")]
pub async fn copy_from_request_or_context<T>(fallback: impl FnOnce() -> T) -> T
where
    T: Clone + Send + Sync + 'static,
{
    dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<T>, _>()
        .await
        .map_or_else(|_| fallback(), |axum::Extension(value)| value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssr(app: fn() -> Element) -> String {
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    /// A firm chrome fixture carrying the firm's regulated footer copy.
    fn firm_chrome() -> PublicChrome {
        PublicChrome {
            brand_name: "Neon Law".to_string(),
            logo_href: "/public/logo.svg".to_string(),
            firm_name: "Neon Law".to_string(),
            firm_logo_href: "/public/logo.svg".to_string(),
            firm_home_href: "https://www.guiebandshook.com".to_string(),
            legal_entity: "Guieb & Shook PC".to_string(),
            disclaimer: "This is an attorney advertisement.".to_string(),
            // The real registration, as `chrome_for` resolves it from the firm
            // brand: the notice's registrant is `legal_entity` above, so a
            // fixture inventing a number here would document the one pairing
            // this footer must never publish.
            trademark: "NEON LAW".to_string(),
            trademark_registration: "6,325,650".to_string(),
            trademark_record_url: "https://tmsearch.uspto.gov/search/search-results/90039224"
                .to_string(),
            copyright_year: 2026,
            is_practice: true,
            ..PublicChrome::default()
        }
    }

    /// A chrome whose header half names a white-label tenant, sharing the
    /// firm's footer half.
    ///
    /// It is what makes `a_different_header_renders_the_same_footer` an
    /// assertion rather than a tautology: the footer must read no header field,
    /// and only a fixture whose header fields differ can prove it.
    fn tenant_chrome() -> PublicChrome {
        PublicChrome {
            brand_name: "Acme Law".to_string(),
            logo_href: "/public/brand/firm-logo.svg".to_string(),
            ..firm_chrome()
        }
    }

    /// Firm chrome renders the three legal lines and nothing else: the
    /// linked copyright, the mark notice, the disclaimer — no mark, no
    /// contact band, no removed rows.
    #[test]
    fn firm_chrome_renders_the_firm_footer() {
        fn app() -> Element {
            rsx! { PublicFooter { chrome: firm_chrome() } }
        }
        let out = ssr(app);
        assert!(
            out.contains(
                r#"© 2026 <a href="https://www.guiebandshook.com">Guieb &#38; Shook PC</a>"#
            ),
            "the copyright links the resolved firm's home: {out}"
        );
        assert!(out.contains("site-footer__trademark"), "{out}");
        assert!(out.contains("site-footer__disclaimer"), "{out}");
        // The rows that came off the footer.
        for gone in [
            "site-footer__logo",
            "site-footer__wordmark",
            "site-footer__contact",
            "site-footer__offices",
            "site-footer__licenses",
            "site-footer__family",
            "site-footer__memberships",
            "site-footer__social",
            "site-footer__nav",
            "site-footer__attribution",
            "site-footer__provider",
            "site-footer__ownership",
            "site-footer__powered",
            "site-footer__tagline",
        ] {
            assert!(!out.contains(gone), "the removed row {gone} is gone: {out}");
        }
        assert!(!out.contains("site-footer--foundation"), "{out}");
    }

    /// The practice footer's copyright links the resolved firm's home — read
    /// from `firm_home_href`, which the live Firm resolution overlays; and on
    /// the firm's own page (`is_practice == false`) the name stands unlinked,
    /// because the page's site is the firm's home.
    #[test]
    fn the_copyright_links_the_resolved_firms_home_on_a_practice_page() {
        fn practice() -> Element {
            rsx! { PublicFooter { chrome: firm_chrome() } }
        }
        // A firm with no published home renders the name unlinked rather than
        // an `<a href="">` that reloads the page.
        fn homeless() -> Element {
            rsx! {
                PublicFooter {
                    chrome: PublicChrome {
                        firm_home_href: String::new(),
                        ..firm_chrome()
                    },
                }
            }
        }
        // The firm's own page is its own home: the name never links.
        fn own_site() -> Element {
            rsx! {
                PublicFooter {
                    chrome: PublicChrome {
                        is_practice: false,
                        ..firm_chrome()
                    },
                }
            }
        }
        let out = ssr(practice);
        assert!(
            out.contains(r#"href="https://www.guiebandshook.com""#),
            "a practice page links the firm's home: {out}"
        );

        let out = ssr(homeless);
        assert!(
            out.contains(r"© 2026 Guieb &#38; Shook PC</p>")
                && !out.contains(r#"<a href="https://www.guiebandshook.com""#),
            "no home, no link: {out}"
        );

        let out = ssr(own_site);
        assert!(
            out.contains(r"© 2026 Guieb &#38; Shook PC</p>")
                && !out.contains(r#"<a href="https://www.guiebandshook.com""#),
            "the firm's own page does not link its own name: {out}"
        );
    }

    /// `firm_public_chrome` resolves `tokens_href` from whichever brand the
    /// `views::brand` task-local currently scopes — the mechanism a
    /// `delete-your-data` request depends on for `PublicFooter` to hoist its
    /// own palette rather than the firm's. `document::Stylesheet` hoists into
    /// the document head via the full `dioxus-server` render pipeline, which
    /// a bare `dioxus_ssr::render` over `PublicFooter` alone does not
    /// exercise — `server::tests::routes` proves the head actually carries it
    /// end to end; this proves the chrome the footer reads is correct.
    #[cfg(feature = "server")]
    #[tokio::test]
    async fn firm_public_chrome_resolves_the_scoped_brands_tokens_href() {
        use views::brand::{scope, DEFAULT_BRANDING, DELETE_YOUR_DATA_BRANDING};

        scope(&DEFAULT_BRANDING, async {
            let chrome = firm_public_chrome(Vec::new());
            assert_eq!(chrome.tokens_href, "/public/css/brand-neon-tokens.css");
        })
        .await;

        scope(&DELETE_YOUR_DATA_BRANDING, async {
            let chrome = firm_public_chrome(Vec::new());
            assert_eq!(
                chrome.tokens_href,
                "/public/css/brand-delete-your-data-tokens.css"
            );
        })
        .await;
    }

    /// The footer's copyright names the legal person, not the wordmark:
    /// "Neon Law" is a brand and cannot hold a copyright.
    #[test]
    fn the_firm_copyright_names_the_legal_entity() {
        fn app() -> Element {
            rsx! { PublicFooter { chrome: firm_chrome() } }
        }
        let out = ssr(app);
        assert!(
            out.contains(
                r#"© 2026 <a href="https://www.guiebandshook.com">Guieb &#38; Shook PC</a>"#
            ),
            "{out}"
        );
    }

    /// The mark notice names the registrant the copyright line names, and
    /// points at the register rather than at the site's own word for it.
    ///
    /// The registrant is the assertion that matters: U.S. Reg. No. 6,325,650 is
    /// the Firm's, and a footer citing that number beside any other name would
    /// hand a reader permission nobody gave them.
    #[test]
    fn the_mark_notice_names_the_registrant_and_links_the_record() {
        fn app() -> Element {
            rsx! { PublicFooter { chrome: firm_chrome() } }
        }
        let out = ssr(app);
        assert!(
            out.contains("NEON LAW") && out.contains("®"),
            "the mark renders as registered: {out}"
        );
        assert!(
            out.contains("is a registered trademark of Guieb &#38; Shook PC"),
            "the registrant is the legal person, not the wordmark: {out}"
        );
        assert!(
            out.contains("U.S. Reg. No. 6,325,650")
                && out.contains(
                    r#"href="https://tmsearch.uspto.gov/search/search-results/90039224""#
                ),
            "the registration is cited and linked to the register: {out}"
        );
    }

    /// A different header renders the one shared footer, byte-identical to the
    /// firm's: the same three legal lines carry over unchanged.
    #[test]
    fn a_different_header_renders_the_same_footer() {
        fn firm_app() -> Element {
            rsx! { PublicFooter { chrome: firm_chrome() } }
        }
        fn tenant_app() -> Element {
            rsx! { PublicFooter { chrome: tenant_chrome() } }
        }
        let firm_out = ssr(firm_app);
        let tenant_out = ssr(tenant_app);
        assert_eq!(
            firm_out, tenant_out,
            "the two headers render the same footer"
        );
        assert!(
            tenant_out.contains("site-footer__disclaimer")
                && tenant_out.contains("site-footer__trademark"),
            "the three lines carry over: {tenant_out}"
        );
    }

    #[cfg(feature = "server")]
    #[tokio::test]
    async fn a_non_default_public_brand_does_not_inherit_the_neon_trademark_notice() {
        let branding = views::brand::BrandKey::DeleteYourData
            .resolve_branding(&views::brand::DEFAULT_BRANDING);
        let chrome = views::brand::scope(branding, async { firm_public_chrome(Vec::new()) }).await;
        assert!(chrome.trademark.is_empty());
        assert!(chrome.trademark_registration.is_empty());
        assert!(chrome.trademark_record_url.is_empty());
    }

    /// Guieb & Shook's own page names no Neon Law mark.
    #[cfg(feature = "server")]
    #[tokio::test]
    async fn guieb_shook_footer_omits_the_neon_mark() {
        let branding =
            views::brand::BrandKey::GuiebShook.resolve_branding(&views::brand::DEFAULT_BRANDING);
        let chrome =
            views::brand::scope(branding, async { chrome_for(&branding.firm, Vec::new()) }).await;
        let mut dom = VirtualDom::new_with_props(PublicFooter, PublicFooterProps { chrome });
        dom.rebuild_in_place();
        let out = dioxus_ssr::render(&dom);
        assert!(!out.contains("site-footer__trademark"), "{out}");
    }

    /// The public footer carries the firm's own disclaimer.
    #[test]
    fn the_footer_carries_the_firms_disclaimer() {
        fn app() -> Element {
            rsx! { PublicFooter { chrome: firm_chrome() } }
        }
        let out = ssr(app);
        assert_eq!(
            out.matches(r#"role="contentinfo""#).count(),
            1,
            "exactly one footer landmark: {out}"
        );
        assert!(
            out.contains("attorney advertisement") || out.contains("attorney advertising"),
            "the firm's own disclaimer is there: {out}"
        );
    }

    /// Every brand footer renders its service disclosure and legal entity.
    /// Legal practices carry an attorney-advertising notice; Delete Your Data
    /// identifies its non-legal service and the enrollment boundary. Every
    /// brand with an empty compiled trademark — every brand but Neon —
    /// renders no mark notice.
    ///
    /// Cover every registered brand, including prepared pages, and inspect
    /// the rendered disclaimer and copyright elements so unrendered fields
    /// cannot satisfy the contract.
    #[cfg(feature = "server")]
    #[tokio::test]
    async fn every_brand_footer_carries_its_service_disclosure_and_legal_entity() {
        for key in views::brand::BrandKey::ALL {
            let branding = key.resolve_branding(&views::brand::DEFAULT_BRANDING);
            let chrome =
                views::brand::scope(branding, async { chrome_for(&branding.firm, Vec::new()) })
                    .await;
            let mut dom = VirtualDom::new_with_props(PublicFooter, PublicFooterProps { chrome });
            dom.rebuild_in_place();
            let out = dioxus_ssr::render(&dom);
            let name = key.as_str();

            let notice = if *key == views::brand::BrandKey::DeleteYourData {
                "non-legal service"
            } else {
                "attorney advertis"
            };
            assert!(
                out.to_lowercase().contains(notice),
                "{name}'s page must carry its service disclosure: {out}"
            );
            assert!(
                out.contains(&branding.firm.legal_entity.replace('&', "&#38;")),
                "{name}'s page must name {}: {out}",
                branding.firm.legal_entity
            );
            // Both in their own element, not merely somewhere in the
            // markup: the notice is the disclaimer paragraph, and the entity
            // is the copyright holder. A word that happened to appear in a
            // nav label or an office address would satisfy a bare
            // `contains` while telling a reader nothing.
            let disclaimer = element_text(&out, "site-footer__disclaimer");
            assert_eq!(
                disclaimer, branding.firm_disclaimer,
                "{name}'s page renders its own disclaimer"
            );
            assert!(
                disclaimer.to_lowercase().contains(notice),
                "{name}'s disclaimer identifies the service: {disclaimer}"
            );
            if *key == views::brand::BrandKey::DeleteYourData {
                assert!(disclaimer
                    .contains("Enrollment does not create an attorney-client relationship."));
                assert!(disclaimer
                    .contains("Legal advice and representation require a separate engagement."));
            }
            let copyright = element_text(&out, "site-footer__copyright");
            assert!(
                copyright.contains(branding.firm.legal_entity),
                "{name}'s page names {} as the publisher: {copyright}",
                branding.firm.legal_entity
            );
            // The trademark notice follows the mark's own registration: only
            // the brand carrying a compiled trademark renders one.
            if branding.firm_trademark.is_empty() {
                assert!(
                    !out.contains("site-footer__trademark"),
                    "{name} carries no mark and renders no notice: {out}"
                );
            }
        }
    }

    /// The visible text of the first element carrying `class`, tags
    /// stripped. Enough to assert *where* a string renders rather than only
    /// that it is somewhere in the markup; the footer's legal strip nests a
    /// link inside the copyright line, so a whole-document `contains` cannot
    /// tell the two apart.
    #[cfg(feature = "server")]
    fn element_text(html: &str, class: &str) -> String {
        let after = html
            .split_once(&format!("class=\"{class}\""))
            .unwrap_or_else(|| panic!("no element with class {class}: {html}"))
            .1;
        let inner = after
            .split_once('>')
            .expect("element has no closing angle bracket")
            .1;
        let inner = inner.split_once("</p>").map_or(inner, |(head, _)| head);
        let mut text = String::new();
        let mut depth = 0usize;
        for ch in inner.chars() {
            match ch {
                '<' => depth += 1,
                '>' => depth = depth.saturating_sub(1),
                _ if depth == 0 => text.push(ch),
                _ => {}
            }
        }
        // Dioxus writes a firm name's ampersand ("Guieb & Shook PC") as `&#38;`.
        text.trim().replace("&#38;", "&")
    }
}
