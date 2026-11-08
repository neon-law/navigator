//! The public-page chrome resolved from the request's scoped brand — the reusable
//! header + footer data every ported public marketing page needs (issue #641 /
//! #730 PR6).
//!
//! Extracted from the first page port (`team_nick`) once a second page needed
//! the same header nav + footer legal strip. The DTOs are wasm-safe (plain
//! serde); the resolver reads `views::brand` and so is server-only, called from
//! each page's `#[server]` view function.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{
    FooterAttorney, FooterBarLicense, FooterMembership, FooterNavLink, FooterOffice,
    FooterSocialLink, SiteFooterLegal,
};

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

/// One published office, resolved from the brand for the footer.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct ChromeOffice {
    pub state: String,
    pub address: String,
    /// A qualification published under the address, e.g. an admission that has
    /// not issued yet. Mirrors `views::brand::FirmOffice::note`.
    pub note: Option<String>,
}

/// One bar license, resolved from the brand for the footer.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct ChromeBarLicense {
    pub jurisdiction: String,
    pub number: String,
    pub license_url: String,
}

/// One licensed attorney and their bar licenses, resolved from the brand.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct ChromeAttorney {
    pub name: String,
    pub licenses: Vec<ChromeBarLicense>,
}

/// One social profile the public footer links under the wordmark.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct ChromeSocial {
    pub label: String,
    pub href: String,
    /// `x`, `linkedin`, or `youtube` — which mark the footer draws.
    #[serde(default)]
    pub network: String,
}

/// One association affiliation for the public footer's standing line. Resolved
/// from the firm brand, whichever house brand's host serves the page.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct ChromeMembership {
    pub label: String,
    /// The full standing sentence, minus its trailing period: the firm's
    /// association-specific standing and the association's name.
    pub standing: String,
    pub href: String,
    /// The association's mark, already resolved through the asset seam
    /// (`views::assets::asset_url`) to this deployment's own origin — its
    /// assets bucket in a cloud deployment, the `/public` mount in dev and
    /// tests. Empty for an association publishing no mark.
    #[serde(default)]
    pub logo_href: String,
}

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
    /// The brand's own canonical origin (`https://www.cyberinjurylaw.com`):
    /// scheme plus [`views::brand::BrandKey::canonical_host`], no trailing
    /// slash. The base every page's `og:url` and canonical link build on —
    /// the address the brand answers to in production, whatever host the
    /// render actually ran on. Empty when no brand origin is resolved (the
    /// wasm client, a bare `PublicChrome::default()`), where a page publishes
    /// no canonical URL rather than a wrong one.
    #[serde(default)]
    pub canonical_origin: String,
    pub destinations: Vec<ChromeNavLink>,
    pub utility: Vec<ChromeNavLink>,
    /// The public pages the footer links rather than the header — the two
    /// organizations' homes, Navigator, the Blog, Contact, and the rest. The
    /// same row on both faces, because there is one footer.
    pub footer_links: Vec<ChromeNavLink>,
    pub firm_name: String,
    /// The firm's own mark, which is the one the footer opens on. Distinct
    /// from `logo_href`, the header's mark: a white-label bundle can publish a
    /// different mark in each place.
    pub firm_logo_href: String,
    /// Where the footer's mark links — the firm's own home. Distinct from
    /// `home_href`, which is the header's.
    pub firm_home_href: String,
    /// The legal person the footer's copyright names, resolved from the firm
    /// brand on both faces.
    pub legal_entity: String,
    #[serde(default)]
    pub disclosure: crate::components::site_footer::FooterDisclosure,
    /// "A practice of Guieb & Shook PC", when the name on the masthead is not
    /// the name of the legal person behind it. Empty otherwise.
    ///
    /// Derived rather than listed. A trade name is permitted in Nevada
    /// because it is not misleading, and *disclosed affiliation* is what does
    /// that work — so the line is owed exactly when the visible name and the
    /// legal entity differ, which is a property of the brand rather than a
    /// set of sites someone remembers to update. Summons Defense demonstrates
    /// why this must stay derived: its public wordmark differs from the legal
    /// entity, so it needs the same attribution line as every other trade
    /// name.
    #[serde(default)]
    pub attribution: String,
    pub disclaimer: String,
    pub copyright_year: i32,
    /// The firm's inbound support address — the footer's contact CTA.
    pub firm_email: String,
    /// The firm's published voice line, dialled from the footer's `tel:` link.
    pub firm_phone: String,
    /// Every office the firm publishes, in the order the brand gives them.
    pub offices: Vec<ChromeOffice>,
    /// The firm's attorneys and the bar licenses each holds.
    pub attorneys: Vec<ChromeAttorney>,
    /// The published release this deployment runs (`YY.M.D`), resolved by
    /// `crate::cli_release::release_version` — the same source `/navigator`'s
    /// download links resolve from, so the footer and the downloads page
    /// cannot disagree about what is running. Empty only if that function
    /// ever changed to allow it; the footer renders no version in that case
    /// rather than a bare `<p>` with nothing after the product name.
    pub navigator_version: String,
    /// Last known public GitHub star count; absent until GitHub responds.
    #[serde(default)]
    pub github_stars: Option<u64>,
    /// The resolved brand's tokens stylesheet href — `/public/css/brand-
    /// <key>-tokens.css`. Resolved from [`views::brand::brand_key`], the same
    /// registry key that produced the rest of this chrome, so the palette
    /// always matches the identity the header and footer already carry.
    pub tokens_href: String,
    /// Optional decorative character for this brand's public pages.
    #[serde(default)]
    pub companion_image: Option<String>,
    /// Whether the request's header is one of the firm's launched practice
    /// brands. The server resolves this from the closed brand registry so the
    /// wasm client does not need to link the `views` crate.
    #[serde(default)]
    pub is_practice: bool,
    /// The associations the firm has a public affiliation with — the footer's
    /// standing lines.
    /// Empty renders none.
    #[serde(default)]
    pub memberships: Vec<ChromeMembership>,
    /// Resolved membership badge for the verified operating Firm.
    #[serde(default)]
    pub membership_badge_href: String,
    /// Whether the legal strip closes with the firm's tagline
    /// ([`crate::components::FOOTER_TAGLINE`]). Guieb & Shook PC only.
    #[serde(default)]
    pub tagline: bool,
    /// The request Brand's public profiles and the shared `YouTube` channel,
    /// under the wordmark. Empty when a mounted bundle renamed the firm. See
    /// `views::brand::firm_social`.
    #[serde(default)]
    pub social: Vec<ChromeSocial>,
}

/// The public footer, mapped from an already-resolved [`PublicChrome`].
///
/// ONE footer, byte-identical on every public page: this mapping reads no
/// field a white-label bundle varies per header, so
/// [`crate::components::SiteFooterLegal`] renders the same mark, wordmark,
/// copyright, link row, bar licenses, offices, and contact channels wherever a
/// reader reaches it. `every_page_renders_the_one_shared_footer` compares two
/// renders byte for byte.
///
/// The header half of the chrome is where a bundle differs — `brand_name`,
/// `logo_href`, `home_href`, `social_image`, and `destinations`. None of them
/// belongs here: `firm_name` and `firm_logo_href` are the footer's identity.
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
        if let Some(src) = &chrome.companion_image {
            div { class: "public-brand-companion", "aria-hidden": "true",
                crate::components::aster::AsterArtwork { src: src.clone(), pose: crate::components::aster::AsterPose::Guide }
            }
        }
        SiteFooterLegal {
            // The copyright names the legal person that renders the legal
            // services, resolved from the firm brand, so it is the same name at
            // the bottom of every page.
            copyright_holder: chrome.legal_entity.clone(),
            disclosure: chrome.disclosure.clone(),
            membership_badge_href: chrome.membership_badge_href.clone(),
            tagline: chrome.tagline,
            attribution: if practice_footer { String::new() } else { chrome.attribution.clone() },
            copyright_href: if practice_footer { chrome.firm_home_href.clone() } else { String::new() },
            disclaimer: chrome.disclaimer.clone(),
            copyright_year: chrome.copyright_year,
            logo_href: chrome.firm_logo_href.clone(),
            // The wordmark beside the footer mark is the firm's, on every
            // page, with the line naming the legal person below it.
            brand_name: chrome.firm_name.clone(),
            // And that mark is the door home. The header's is off screen by the
            // time a reader reaches the footer, so this is the one they click.
            home_href: chrome.firm_home_href.clone(),
            contact_email: chrome.firm_email.clone(),
            phone: chrome.firm_phone.clone(),
            offices: chrome
                .offices
                .iter()
                .map(|office| FooterOffice {
                    state: office.state.clone(),
                    address: office.address.clone(),
                    note: office.note.clone(),
                })
                .collect(),
            nav: chrome
                .footer_links
                .iter()
                .map(|link| FooterNavLink {
                    label: link.label.clone(),
                    href: link.href.clone(),
                })
                .collect(),
            attorneys: chrome
                .attorneys
                .iter()
                .map(|attorney| FooterAttorney {
                    name: attorney.name.clone(),
                    licenses: attorney
                        .licenses
                        .iter()
                        .map(|license| FooterBarLicense {
                            jurisdiction: license.jurisdiction.clone(),
                            number: license.number.clone(),
                            license_url: license.license_url.clone(),
                        })
                        .collect(),
                })
                .collect(),
            navigator_version: chrome.navigator_version.clone(),
            github_stars: chrome.github_stars,
            memberships: chrome
                .memberships
                .iter()
                .map(|membership| FooterMembership {
                    label: membership.label.clone(),
                    standing: membership.standing.clone(),
                    href: membership.href.clone(),
                    logo_href: membership.logo_href.clone(),
                })
                .collect(),
            social: chrome
                .social
                .iter()
                .map(|profile| FooterSocialLink {
                    label: profile.label.clone(),
                    href: profile.href.clone(),
                    network: profile.network.clone(),
                })
                .collect(),
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

/// The brand's absolute `og:image` URL. The deployment's `NAV_BASE_URL`
/// resolution wins, as everywhere `views::assets::absolute_url` runs; when
/// the deployment advertises no origin (dev, tests, an OSS fork), the
/// brand's own canonical origin resolves the path instead, so a share card
/// never falls back to the relative path a scraper drops.
#[cfg(feature = "server")]
fn social_image_url(brand: &views::brand::SiteBrand) -> String {
    let resolved = views::assets::absolute_url(brand.social_image);
    if resolved.starts_with('/') {
        format!(
            "https://{}{resolved}",
            views::brand::brand_key().canonical_host()
        )
    } else {
        resolved
    }
}

/// Build the public chrome for `brand`'s header, with the firm's footer data.
///
/// One brand reaches this — the firm's — because the site publishes one
/// header. The parameter stays because a white-label bundle still mounts its
/// own brand through it.
#[cfg(feature = "server")]
fn chrome_for(brand: &views::brand::SiteBrand, utility: Vec<ChromeNavLink>) -> PublicChrome {
    use views::brand::FIRM_BRAND;

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
        social_image: social_image_url(brand),
        canonical_origin: format!("https://{}", views::brand::brand_key().canonical_host()),
        destinations,
        utility,
        footer_links: views::brand::firm_footer_nav()
            .iter()
            .map(|link| ChromeNavLink {
                label: link.label.to_string(),
                href: link.href.to_string(),
            })
            .collect(),
        firm_name: FIRM_BRAND.site_name.to_string(),
        firm_logo_href: FIRM_BRAND.logo_href.to_string(),
        firm_home_href: FIRM_BRAND.home_href.to_string(),
        legal_entity: FIRM_BRAND.legal_entity.to_string(),
        disclosure: crate::components::site_footer::FooterDisclosure::default(),
        membership_badge_href: if FIRM_BRAND.legal_entity == "Guieb & Shook PC" {
            views::assets::asset_url(views::assets::GUIEB_SHOOK_GVC_BADGE_KEY)
        } else {
            String::new()
        },
        tagline: FIRM_BRAND.legal_entity == "Guieb & Shook PC",
        attribution: firm_attribution(brand),
        disclaimer: views::brand::firm_disclaimer().to_string(),
        // The footer fixes the joint-copyright year too; a deploy-time
        // value replaces the constant when the footer year is wired through.
        copyright_year: 2026,
        // The contact band is firm-anchored for the same reason the legal strip
        // is: one footer serves the firm, and the firm is the entity a
        // visitor calls, writes to, or walks in on.
        firm_email: views::brand::firm_email().to_string(),
        firm_phone: views::brand::firm_phone().to_string(),
        offices: views::brand::firm_offices()
            .iter()
            .map(|office| ChromeOffice {
                state: office.state.to_string(),
                address: office.address.to_string(),
                note: office.note.map(str::to_string),
            })
            .collect(),
        attorneys: views::brand::firm_attorneys()
            .iter()
            .map(|attorney| ChromeAttorney {
                name: attorney.name.to_string(),
                licenses: attorney
                    .licenses
                    .iter()
                    .map(|license| ChromeBarLicense {
                        jurisdiction: license.jurisdiction.to_string(),
                        number: license.number.to_string(),
                        license_url: license.license_url.to_string(),
                    })
                    .collect(),
            })
            .collect(),
        // The release stamp this deployment runs, from the same function
        // `/navigator`'s download links resolve from — see
        // `crate::cli_release::release_version`'s own docs for why it prefers
        // the environment and falls back to the compiled manifest version
        // rather than ever rendering a bare, unstamped line.
        navigator_version: crate::cli_release::release_version(),
        github_stars: crate::source_repository::known_stars(),
        tokens_href: crate::brand_style::brand_tokens_href(views::brand::brand_key().as_str()),
        companion_image: (views::brand::brand_key() == views::brand::BrandKey::Vesta)
            .then(|| views::assets::asset_url(views::assets::ASTER_BOARD_KEY)),
        is_practice: views::brand::BrandKey::LIVE.contains(&views::brand::brand_key()),
        memberships: views::brand::firm_memberships()
            .iter()
            .map(|membership| ChromeMembership {
                label: membership.name.to_string(),
                standing: membership.standing.to_string(),
                href: membership.href.to_string(),
                // Resolved here, beside `social_image` above, because this is
                // where the request already knows the deployment's asset
                // origin. An association publishing no mark stays empty
                // rather than resolving to a bare base URL.
                logo_href: if membership.logo_key.is_empty() {
                    String::new()
                } else {
                    views::assets::asset_url(membership.logo_key)
                },
            })
            .collect(),
        social: chrome_social(),
    }
}

#[cfg(feature = "server")]
fn firm_attribution(brand: &views::brand::SiteBrand) -> String {
    use views::brand::{DAYBRIDGE_BRANDING, FIRM_BRAND};

    if brand.site_name == FIRM_BRAND.legal_entity
        || brand.site_name == DAYBRIDGE_BRANDING.firm.site_name
    {
        String::new()
    } else {
        format!("A practice of {}", FIRM_BRAND.legal_entity)
    }
}

/// Overlay a resolved operator without inheriting another Entity's public credentials.
#[cfg(feature = "server")]
pub fn apply_resolved_firm_footer(
    chrome: &mut PublicChrome,
    model: &crate::firm_footer::FirmFooterModel,
) {
    if !model.compiled_identity_allowed {
        // Provider attribution belongs to the operating Firm, not the Brand.
        chrome.disclosure.prefix.clear();
        chrome.disclosure.provider.clear();
        chrome.disclosure.href.clear();
        let old_entity = chrome.legal_entity.clone();
        if !old_entity.is_empty() {
            chrome.disclaimer = chrome.disclaimer.replace(&old_entity, &model.legal_entity);
        }
        chrome.firm_name.clone_from(&model.legal_entity);
        chrome.firm_logo_href.clear();
        chrome.firm_email.clear();
        chrome.firm_phone.clear();
        chrome.offices.clear();
        chrome.attorneys.clear();
        chrome.memberships.clear();
        chrome.membership_badge_href.clear();
        chrome.tagline = false;
        chrome.social.clear();
        chrome.attribution = format!("A practice of {}", model.legal_entity);
    }
    chrome.legal_entity.clone_from(&model.legal_entity);
    chrome.firm_home_href.clone_from(&model.firm_home_href);
    chrome.copyright_year = model.copyright_year;
}

/// Public social destinations scoped to the request Brand.
#[cfg(feature = "server")]
fn chrome_social() -> Vec<ChromeSocial> {
    views::brand::firm_social()
        .iter()
        .map(|profile| ChromeSocial {
            label: profile.label.to_string(),
            href: profile.href.to_string(),
            network: profile.network.to_string(),
        })
        .collect()
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
    if let Ok(axum::Extension(mut chrome)) =
        dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<PublicChrome>, _>().await
    {
        chrome.github_stars = crate::source_repository::stars().await;
        return chrome;
    }
    let utility =
        dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<PublicUtility>, _>()
            .await
            .map_or_else(|_| Vec::new(), |axum::Extension(utility)| utility.0);
    let mut chrome = firm_public_chrome(utility);
    chrome.github_stars = crate::source_repository::stars().await;
    chrome
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
    use views::brand::JTA_STANDING;

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
            firm_home_href: "/".to_string(),
            legal_entity: "Guieb & Shook PC".to_string(),
            membership_badge_href: views::assets::asset_url(
                views::assets::GUIEB_SHOOK_GVC_BADGE_KEY,
            ),
            tagline: true,
            attribution: "A practice of Guieb & Shook PC".to_string(),
            disclaimer: "This is an attorney advertisement.".to_string(),
            copyright_year: 2026,
            firm_email: "support@neonlaw.com".to_string(),
            firm_phone: "+1 555 010 0100".to_string(),
            attorneys: vec![ChromeAttorney {
                name: "Aquarius Lumen".to_string(),
                licenses: vec![ChromeBarLicense {
                    jurisdiction: "California".to_string(),
                    number: "100001".to_string(),
                    license_url: "https://example.com/bar/100001".to_string(),
                }],
            }],
            memberships: vec![ChromeMembership {
                label: "Justice Technology Association".to_string(),
                standing: JTA_STANDING.to_string(),
                href: "https://justicetechassociation.org/get-involved".to_string(),
                logo_href: "/public/img/justice-technology-association/logo.png".to_string(),
            }],
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

    /// Firm chrome renders the firm's footer: the copyright that names the
    /// regulated entity, and the firm's own contact channels.
    #[test]
    fn firm_chrome_renders_the_firm_footer() {
        fn app() -> Element {
            rsx! { PublicFooter { chrome: firm_chrome() } }
        }
        let out = ssr(app);
        assert!(out.contains("\u{a9} 2026 Guieb &#38; Shook PC"), "{out}");
        assert!(out.contains("support@neonlaw.com"), "{out}");
        assert!(
            out.contains(r#"href="mailto:support@neonlaw.com""#),
            "{out}"
        );
        assert!(
            out.contains(r#"class="site-footer__logo" src="/public/logo.svg" alt="""#),
            "the firm footer carries the firm's mark: {out}"
        );
        assert!(!out.contains("site-footer--foundation"), "{out}");
    }

    /// `firm_public_chrome` resolves `tokens_href` from whichever brand the
    /// `views::brand` task-local currently scopes — the mechanism a
    /// `delete-your-data` request depends on for `PublicFooter` to hoist its
    /// own palette rather than the firm's. `document::Stylesheet` hoists into
    /// the document head via the full `dioxus-server` render pipeline, which
    /// a bare `dioxus_ssr::render` over `PublicFooter` alone does not
    /// exercise — `server::tests::routes` proves the head actually carries it
    /// end to end; this proves the chrome the footer reads is correct.
    #[tokio::test]
    async fn firm_public_chrome_resolves_the_scoped_brands_tokens_href() {
        use views::brand::{scope, DEFAULT_BRANDING, DELETE_YOUR_DATA_BRANDING};

        scope(&DEFAULT_BRANDING, async {
            let chrome = firm_public_chrome(Vec::new());
            assert_eq!(chrome.tokens_href, "/public/css/brand-neon-tokens.css");
            let profiles: Vec<(&str, &str)> = chrome
                .social
                .iter()
                .map(|profile| (profile.label.as_str(), profile.href.as_str()))
                .collect();
            assert_eq!(
                profiles,
                [
                    ("X", "https://x.com/NeonLawUSA"),
                    ("LinkedIn", "https://www.linkedin.com/company/neon-law-usa"),
                    ("YouTube", "https://www.youtube.com/@neon-law-firm"),
                ]
            );
        })
        .await;

        scope(&DELETE_YOUR_DATA_BRANDING, async {
            let chrome = firm_public_chrome(Vec::new());
            assert_eq!(
                chrome.tokens_href,
                "/public/css/brand-delete-your-data-tokens.css"
            );
            assert_eq!(chrome.social.len(), 1);
            assert_eq!(chrome.social[0].network, "youtube");
        })
        .await;
    }

    #[tokio::test]
    async fn public_chrome_keeps_vesta_profiles_off_other_brands() {
        use views::brand::{scope, CYBER_INJURY_BRANDING, VESTA_BRANDING};
        scope(&VESTA_BRANDING, async {
            let chrome = firm_public_chrome(Vec::new());
            assert_eq!(
                chrome
                    .social
                    .iter()
                    .map(|profile| profile.network.as_str())
                    .collect::<Vec<_>>(),
                ["facebook", "instagram", "threads", "tiktok", "youtube"]
            );
        })
        .await;
        scope(&CYBER_INJURY_BRANDING, async {
            let chrome = firm_public_chrome(Vec::new());
            assert_eq!(
                chrome
                    .social
                    .iter()
                    .map(|profile| profile.network.as_str())
                    .collect::<Vec<_>>(),
                ["youtube"]
            );
        })
        .await;
    }

    /// A bundle that renames the firm does not inherit Neon Law's profiles.
    #[tokio::test]
    async fn a_renamed_firm_publishes_no_social_profiles() {
        use views::brand::{scope, Branding};
        use views::brand_bundle::BrandManifest;

        let manifest: BrandManifest =
            serde_yaml::from_str("version: 1\nbrand:\n  firm: Cascade Law\n").unwrap();
        scope(Branding::from_manifest(&manifest), async {
            assert!(firm_public_chrome(Vec::new()).social.is_empty());
        })
        .await;
    }

    /// The footer's mark is a link to the firm's home, under any header.
    ///
    /// On a long page the header's mark is off screen by the time a reader
    /// reaches the bottom, so the footer's is the one they click. The
    /// destination is the firm's home even under a tenant's header, matching
    /// the firm wordmark and mark beside it.
    #[test]
    fn the_footer_mark_links_to_the_firms_home_under_any_header() {
        fn firm() -> Element {
            rsx! { PublicFooter { chrome: firm_chrome() } }
        }
        fn tenant() -> Element {
            rsx! { PublicFooter { chrome: tenant_chrome() } }
        }
        for face in [firm as fn() -> Element, tenant] {
            let out = ssr(face);
            let brand = out
                .split(r#"class="site-footer__brand""#)
                .nth(1)
                .and_then(|rest| rest.split('>').next())
                .expect("the footer renders a brand mark");
            assert!(
                brand.contains(r#"href="/""#),
                "the footer mark opens the firm's home: {out}"
            );
            assert!(
                brand.contains(r#"aria-label="Neon Law home""#),
                "and is announced as that door: {out}"
            );
            assert!(
                !out.contains(r#"<div class="site-footer__brand""#),
                "the mark is a link, not an inert box: {out}"
            );
        }
    }

    /// The footer's identity is the firm's under any header: the firm's mark,
    /// the firm's wordmark beside it, and the legal person named below.
    ///
    /// A header wordmark and mark that reached the footer would put one
    /// organization's name at the bottom of a page with another's copyright
    /// directly under it. One footer means one identity, and this keeps the
    /// header's out of it.
    #[test]
    fn the_footer_identity_is_the_firms_under_any_header() {
        fn app() -> Element {
            rsx! { PublicFooter { chrome: tenant_chrome() } }
        }
        let out = ssr(app);
        assert!(
            out.contains(r#"<strong class="site-footer__wordmark">Neon Law</strong>"#),
            "the footer wordmark is the firm's: {out}"
        );
        assert!(
            out.contains(r#"src="/public/logo.svg""#),
            "and so is the mark: {out}"
        );
        assert!(
            !out.contains("Acme Law"),
            "the tenant's header wordmark does not reach the footer: {out}"
        );
    }

    /// The footer's copyright names the legal person, not the wordmark:
    /// "Neon Law" is a brand and cannot hold a copyright.
    #[test]
    fn the_firm_copyright_names_the_legal_entity() {
        fn app() -> Element {
            rsx! { PublicFooter { chrome: firm_chrome() } }
        }
        let out = ssr(app);
        assert!(out.contains("© 2026 Guieb &#38; Shook PC"), "{out}");
    }

    /// A different header renders the one shared footer, byte-identical to the
    /// firm's: the bar licenses, the offices, and the contact band all carry
    /// over unchanged.
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
            tenant_out.contains("Bar No."),
            "the bar license carries over: {tenant_out}"
        );
        assert!(
            tenant_out.contains("support@neonlaw.com"),
            "the contact band carries over: {tenant_out}"
        );
        assert!(
            tenant_out.contains(&JTA_STANDING.to_string())
                && tenant_out.contains(r#"href="https://justicetechassociation.org/get-involved""#),
            "the membership line carries over: {tenant_out}"
        );
    }

    /// The chrome the server resolves carries the firm's affiliation, so the
    /// line renders from first boot, before any Firm row exists to override
    /// it.
    #[cfg(feature = "server")]
    #[test]
    fn the_resolved_chrome_carries_the_membership() {
        let chrome = firm_public_chrome(Vec::new());
        assert_eq!(chrome.memberships.len(), 1);
        assert_eq!(
            chrome.memberships[0].label,
            "Justice Technology Association"
        );
        assert_eq!(
            chrome.memberships[0].href,
            "https://justicetechassociation.org/get-involved"
        );
        assert_eq!(
            chrome.memberships[0].logo_href, "/public/img/justice-technology-association/logo.png",
            "with no asset base configured it falls back to the bundled mount"
        );
    }

    #[cfg(feature = "server")]
    #[tokio::test]
    async fn every_brand_renders_only_its_firms_jta_standing() {
        let expected = JTA_STANDING.to_string();

        // Guieb & Shook's membership; another firm's brand never claims it.
        for key in views::brand::BrandKey::GUIEB_SHOOK_FAMILY {
            let branding = key.resolve_branding(&views::brand::DEFAULT_BRANDING);
            let output = views::brand::scope(branding, async {
                let chrome = chrome_for(&branding.firm, Vec::new());
                let mut dom =
                    VirtualDom::new_with_props(PublicFooter, PublicFooterProps { chrome });
                dom.rebuild_in_place();
                dioxus_ssr::render(&dom)
            })
            .await;

            if branding.firm_memberships.is_empty() {
                assert!(
                    !output.contains(&expected) && !output.contains("site-footer__membership-link"),
                    "{key:?} must not inherit another Firm's association standing"
                );
                continue;
            }
            assert!(
                output.contains(&expected),
                "{key:?} must render its Firm's association standing: {output}"
            );
            assert!(
                output
                    .split_once(
                        r#"<a href="https://justicetechassociation.org/get-involved" class="site-footer__membership-link""#,
                    )
                    .and_then(|(_, rest)| rest.split_once("</a>"))
                    .is_some_and(|(link_body, _)| !link_body.contains("member")),
                "{key:?} must not call the firm a member in the standing line: {output}"
            );
        }
    }

    /// Guieb & Shook's own page names no provider.
    #[cfg(feature = "server")]
    #[tokio::test]
    async fn guieb_shook_footer_omits_the_provider() {
        let branding =
            views::brand::BrandKey::GuiebShook.resolve_branding(&views::brand::DEFAULT_BRANDING);
        let chrome =
            views::brand::scope(branding, async { chrome_for(&branding.firm, Vec::new()) }).await;
        let mut dom = VirtualDom::new_with_props(PublicFooter, PublicFooterProps { chrome });
        dom.rebuild_in_place();
        let out = dioxus_ssr::render(&dom);
        assert!(!out.contains("site-footer__provider"), "{out}");
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
    /// identifies its non-legal service and the enrollment boundary.
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
        }
    }

    #[cfg(feature = "server")]
    #[tokio::test]
    async fn guieb_shook_brands_never_credit_another_firm() {
        for key in views::brand::BrandKey::ALL {
            let branding = key.resolve_branding(&views::brand::DEFAULT_BRANDING);
            if branding.firm.legal_entity != "Guieb & Shook PC" {
                continue;
            }
            let chrome =
                views::brand::scope(branding, async { chrome_for(&branding.firm, Vec::new()) })
                    .await;
            let html = render_chrome(chrome);
            assert!(
                !html.contains("site-footer__provider"),
                "{}: {html}",
                key.as_str()
            );
            assert!(!html.contains("lextecnica.com"), "{}: {html}", key.as_str());
            assert!(
                html.contains("Everyone deserves access to justice.<br/>Made with ❤️ in 🗽."),
                "{}: {html}",
                key.as_str()
            );
            assert!(
                html.contains(views::assets::GUIEB_SHOOK_GVC_BADGE_KEY),
                "{}: {html}",
                key.as_str()
            );
            assert!(
                html.find("site-footer__gvc-badge") > html.find("site-footer__membership-link")
                    && html.find("site-footer__gvc-badge") < html.find("site-footer__legal"),
                "{html}"
            );
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

    /// The attribution line is derived, not listed.
    ///
    /// A trade name is permitted in Nevada because it is not misleading, and
    /// disclosed affiliation is what does that work — so the line is owed
    /// exactly when the masthead name and the legal entity differ. Deriving
    /// it means a brand added later cannot be forgotten, which is the failure
    /// a hardcoded list of "brand sites" eventually produces.
    #[cfg(feature = "server")]
    #[test]
    fn the_attribution_line_follows_the_name_on_the_masthead() {
        for key in views::brand::BrandKey::ALL {
            let branding = key.resolve_branding(&views::brand::DEFAULT_BRANDING);
            let chrome = chrome_for(&branding.firm, Vec::new());

            if branding.firm.site_name == branding.firm.legal_entity {
                assert!(
                    chrome.attribution.is_empty(),
                    "{} already wears the firm's name and needs no line",
                    key.as_str()
                );
            } else {
                assert_eq!(
                    chrome.attribution,
                    if *key == views::brand::BrandKey::Daybridge {
                        ""
                    } else {
                        "A practice of Guieb & Shook PC"
                    },
                    "{} has the expected attribution treatment",
                    key.as_str()
                );
            }
        }
    }
    #[cfg(feature = "server")]
    fn resolved_operator(entity: &str, allowed: bool) -> crate::firm_footer::FirmFooterModel {
        crate::firm_footer::FirmFooterModel {
            legal_entity: entity.into(),
            compiled_identity_allowed: allowed,
            firm_home_href: String::new(),
            copyright_year: 2026,
        }
    }
    #[cfg(feature = "server")]
    fn render_chrome(chrome: PublicChrome) -> String {
        let mut dom = VirtualDom::new_with_props(PublicFooter, PublicFooterProps { chrome });
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }
    #[test]
    #[cfg(feature = "server")]
    fn changed_operator_keeps_the_advertising_warning() {
        let mut chrome = firm_chrome();
        chrome.is_practice = true;
        chrome.disclaimer = "Attorney advertisement. Guieb & Shook PC. Non-legal services do not create an attorney-client relationship.".into();
        chrome.disclosure.provider = "Independent Provider".into();
        apply_resolved_firm_footer(
            &mut chrome,
            &resolved_operator("Synthetic New Entity LLC", false),
        );
        assert!(chrome.offices.is_empty() && chrome.attorneys.is_empty());
        assert!(chrome.memberships.is_empty() && chrome.social.is_empty());
        assert!(
            chrome.firm_logo_href.is_empty()
                && chrome.firm_email.is_empty()
                && chrome.firm_phone.is_empty()
        );
        assert_eq!(chrome.disclaimer, "Attorney advertisement. Synthetic New Entity LLC. Non-legal services do not create an attorney-client relationship.");
        let html = render_chrome(chrome);
        assert!(
            html.contains("Synthetic New Entity LLC") && html.contains("Attorney advertisement."),
            "{html}"
        );
        assert!(
            !html.contains(views::assets::GUIEB_SHOOK_GVC_BADGE_KEY),
            "{html}"
        );
        assert!(!html.contains("Independent Provider"), "{html}");
        assert!(!html.contains("site-footer__provider"), "{html}");
        assert!(
            !html.contains("Aquarius Lumen")
                && !html.contains("Justice Technology Association")
                && !html.contains("support@neonlaw.com"),
            "{html}"
        );
    }
    #[test]
    #[cfg(feature = "server")]
    fn namesake_without_canonical_identity_cannot_inherit_credentials() {
        let mut chrome = firm_chrome();
        let entity = chrome.legal_entity.clone();
        apply_resolved_firm_footer(&mut chrome, &resolved_operator(&entity, false));
        assert!(
            chrome.attorneys.is_empty()
                && chrome.memberships.is_empty()
                && chrome.firm_email.is_empty()
        );
        let html = render_chrome(chrome);
        assert!(
            !html.contains(views::assets::GUIEB_SHOOK_GVC_BADGE_KEY),
            "{html}"
        );
    }
    #[test]
    #[cfg(feature = "server")]
    fn canonical_identity_preserves_all_compiled_facts() {
        let mut chrome = firm_chrome();
        let model = crate::firm_footer::FirmFooterModel {
            legal_entity: chrome.legal_entity.clone(),
            compiled_identity_allowed: true,
            firm_home_href: chrome.firm_home_href.clone(),
            copyright_year: chrome.copyright_year,
        };
        let before = serde_json::to_value(&chrome).unwrap();
        apply_resolved_firm_footer(&mut chrome, &model);
        assert_eq!(serde_json::to_value(&chrome).unwrap(), before);
    }
}
