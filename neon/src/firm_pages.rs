//! The firm's public Dioxus SSR pages, and the content each one renders.
//!
//! Every firm page renders through the Dioxus port, so this module — not an
//! Axum route table — is where the firm's public surface actually lives. Copy
//! for pages a brand publishes is loaded per `BrandKey` and injected on the
//! request task (the same seam public chrome uses), so a house-brand host
//! never renders another brand's heading.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{from_fn, from_fn_with_state, Next};
use axum::response::{IntoResponse, Response};
use portal::hosting::PublicRouter as Router;
use portal::{dioxus_app, secure_cookies, AppState, WorkshopIndex};
use views::brand::BrandKey;

use crate::firm_copy;
use crate::locales;

const PRESENTATION_INDEX_TITLE: &str = "Presentations";
const PRESENTATION_INDEX_LEDE: &str = "Presentations and workshops published by the firm.";

/// Build the `presentations` index: every material the manifest files under
/// that category, in manifest order.
///
/// The contact address is the firm's inbox — a reader on `neonlaw.com` who
/// wants us at their meetup writes to the firm.
fn presentation_index_content(
    workshops: &WorkshopIndex,
) -> webapp::catalog_index::CatalogIndexContent {
    catalog_index_content(
        workshops,
        &["presentations", "workshops"],
        PRESENTATION_INDEX_TITLE,
        PRESENTATION_INDEX_LEDE,
        true,
    )
}

/// One category index's Dioxus content: every material the manifest files
/// under `category`, in manifest order.
///
/// The contact address is the firm's on both catalogs — the firm gives the
/// talks and runs the classes, so a reader who wants either writes to it.
fn catalog_index_content(
    workshops: &WorkshopIndex,
    categories: &[&str],
    title: &str,
    lede: &str,
    include_testimonials: bool,
) -> webapp::catalog_index::CatalogIndexContent {
    webapp::catalog_index::CatalogIndexContent {
        title: title.to_string(),
        introduction: None,
        lede: lede.to_string(),
        materials: workshops
            .materials()
            .iter()
            .filter(|m| categories.contains(&m.category.as_str()))
            .map(|m| webapp::catalog_index::CatalogMaterial {
                href: format!("/{}/{}", m.category, m.slug),
                eyebrow: m.audience.clone(),
                title: m.title.clone(),
                summary: m.benefit.clone(),
                ..webapp::catalog_index::CatalogMaterial::default()
            })
            .collect(),
        contact_email: views::brand::firm_email().to_string(),
        footnote: String::new(),
        include_testimonials,
        brand_key: String::new(),
        kinds: Vec::new(),
    }
}

const NOTATIONS_INDEX_TITLE: &str = "Notations";
const NOTATIONS_BLOB_BASE: &str = "https://github.com/neon-law/navigator/blob/main/templates/";

// Every bundled notation's raw Markdown, hoisted to module scope so both
// `notation_preview_docs` (the show pages) and `notations_index_content`
// (the catalog cards) read the same bytes — the single source LAW-53 grounds
// each card's `kind:` facet in, rather than a hand-typed guess that can
// drift from what the template actually declares.
// Public business templates: slug, title, source path, review scope, and source.
const BUSINESS_NOTATIONS: &[(&str, &str, &str, &str, &str)] = &[
    (
        "mutual-nda",
        "Mutual Non-Disclosure Agreement",
        "notations/neon_law/mutual_nda.md",
        "Two-way confidentiality for a shared business discussion.",
        include_str!("../../templates/notations/neon_law/mutual_nda.md"),
    ),
    (
        "one-way-nda",
        "One-Way Non-Disclosure Agreement",
        "notations/neon_law/one_way_nda.md",
        "Confidentiality when your company shares information with another party.",
        include_str!("../../templates/notations/neon_law/one_way_nda.md"),
    ),
    (
        "advisor-agreement",
        "Advisor Agreement",
        "notations/neon_law/advisor_agreement.md",
        "Advisory services, compensation, and ownership of the work.",
        include_str!("../../templates/notations/neon_law/advisor_agreement.md"),
    ),
    (
        "master-services-agreement",
        "Master Services Agreement",
        "notations/neon_law/master_services_agreement.md",
        "Services, payment, intellectual property, and a clear way to exit.",
        include_str!("../../templates/notations/neon_law/master_services_agreement.md"),
    ),
    (
        "employee-offer-letter",
        "Employee Offer Letter",
        "notations/neon_law/employee_offer_letter.md",
        "An offer for any U.S. state with fillable role, pay, and benefit terms.",
        include_str!("../../templates/notations/neon_law/employee_offer_letter.md"),
    ),
    (
        "business-associate-agreement",
        "Business Associate Agreement",
        "notations/neon_law/business_associate_agreement.md",
        "HIPAA duties for services that handle protected health information.",
        include_str!("../../templates/notations/neon_law/business_associate_agreement.md"),
    ),
    (
        "dpa-us",
        "U.S. Data Processing Addendum",
        "notations/neon_law/dpa_us.md",
        "U.S. processing instructions, safeguards, and subprocessor terms.",
        include_str!("../../templates/notations/neon_law/dpa_us.md"),
    ),
    (
        "dpa-global",
        "Global Data Processing Addendum",
        "notations/neon_law/dpa_global.md",
        "U.S. and European processing, with required transfer-document review.",
        include_str!("../../templates/notations/neon_law/dpa_global.md"),
    ),
    (
        "privacy-policy",
        "Privacy Policy",
        "notations/neon_law/privacy_policy.md",
        "Data practices, cookies, and privacy requests, with U.S. state and GDPR clauses.",
        include_str!("../../templates/notations/neon_law/privacy_policy.md"),
    ),
];

const ONBOARDING: &str = include_str!("../../templates/notations/neon_law/onboarding__letter.md");
const OFFBOARDING: &str = include_str!("../../templates/notations/neon_law/offboarding__letter.md");
const RESCISSION_NOTICE: &str =
    include_str!("../../templates/notations/neon_law/rescission_notice__nevada.md");
const WITNESS_AFFIDAVIT: &str =
    include_str!("../../templates/notations/neon_law/witness_affidavit__nevada.md");
const ANSWER_TO_COUNTERCLAIM: &str =
    include_str!("../../templates/notations/neon_law/answer_to_counterclaim__nevada.md");
const ENGAGEMENT_LETTER: &str =
    include_str!("../../templates/notations/neon_law/engagement_letter__nevada.md");
const SUMMONS: &str = include_str!("../../templates/notations/neon_law/summons__nevada.md");
const ESTATE_SIGNING_PREPARATION: &str =
    include_str!("../../templates/notations/neon_law/estate_signing_preparation__letter.md");
const FORM_990: &str =
    include_str!("../../templates/notations/forms/united_states/federal/irs/us__form_990.md");
const NATURALIZATION: &str = include_str!(
    "../../templates/notations/forms/united_states/federal/uscis/us__naturalization.md"
);
const NV_LLC: &str =
    include_str!("../../templates/notations/forms/united_states/nevada/state/nv__llc_formation.md");
const NV_PROFIT_CORP: &str = include_str!(
    "../../templates/notations/forms/united_states/nevada/state/nv__profit_corp_formation.md"
);
const NV_BUSINESS_TRUST: &str = include_str!(
    "../../templates/notations/forms/united_states/nevada/state/nv__business_trust_formation.md"
);
const NV_NONPROFIT: &str = include_str!(
    "../../templates/notations/forms/united_states/nevada/state/nv__nonprofit_501c3_formation.md"
);
const NV_ANNUAL_REPORT: &str =
    include_str!("../../templates/notations/forms/united_states/nevada/state/nv__annual_report.md");
const NV_DISSOLUTION: &str =
    include_str!("../../templates/notations/forms/united_states/nevada/state/nv__dissolution.md");
const NV_MODIFIED_BUSINESS_TAX: &str = include_str!(
    "../../templates/notations/forms/united_states/nevada/state/nv__modified_business_tax.md"
);
const NV_CHARITABLE: &str = include_str!(
    "../../templates/notations/forms/united_states/nevada/state/nv__charitable_solicitation_registration.md"
);

/// A notation's card: the default link opens the show page at
/// `/notations/{slug}` (built from [`notation_preview_docs`]) — a letter's
/// paragraph-highlighted stage or a form's cover sheet — rather than the raw
/// GitHub source, which now lives as a link on that page instead. `slug`
/// names the document (`onboarding-letter`, `nevada-llc-formation`), not
/// just an eyebrow word, because it also becomes the URL's `{slug}` — and,
/// through the sitewide `stamp_document_title` path-derived tab title, the
/// words that title-case into the browser tab's title.
///
/// `eyebrow` names only the jurisdiction or authority (`"Nevada"`,
/// `"Federal"`, `"Firm"`) — LAW-53 moved the kind itself out of this
/// hand-typed line and into `kind`/`kind_label`/`category`, derived from
/// `src`'s own `kind:` frontmatter via [`views::kind_catalog::declared_kind`]
/// (the same classifier `S103` runs), so the card's kind can never drift
/// from what the template actually declares the way a hand-typed eyebrow
/// could (and had: a `kind: pleading` fixture once carried a `"Filing"`
/// eyebrow).
fn notation_card(
    eyebrow: &str,
    title: &str,
    slug: &str,
    summary: &str,
    src: &str,
) -> webapp::catalog_index::CatalogMaterial {
    let kind = views::kind_catalog::declared_kind(src).unwrap_or_default();
    let entry = views::kind_catalog::entries()
        .into_iter()
        .find(|entry| entry.kind == kind);
    webapp::catalog_index::CatalogMaterial {
        href: format!("/notations/{slug}"),
        eyebrow: eyebrow.to_string(),
        title: title.to_string(),
        summary: summary.to_string(),
        kind: kind.clone(),
        kind_label: entry.as_ref().map_or_else(String::new, |e| e.label.clone()),
        category: entry
            .as_ref()
            .map_or_else(String::new, |e| e.category.clone()),
        category_slug: entry.map_or_else(String::new, |e| e.category_slug.clone()),
    }
}

/// Every `kind:` value `S103` accepts (LAW-53), for `/notations`' kind
/// catalog section — a direct projection of `views::kind_catalog::entries()`,
/// itself built from `rules::kind::Kind`, so this list can never drift from
/// the gate.
fn kind_catalog_entries() -> Vec<webapp::catalog_index::KindCatalogEntry> {
    views::kind_catalog::entries()
        .into_iter()
        .map(|entry| webapp::catalog_index::KindCatalogEntry {
            kind: entry.kind,
            label: entry.label,
            category: entry.category,
            category_slug: entry.category_slug,
            definition: entry.definition,
            rules: entry
                .rules
                .into_iter()
                .map(|rule| webapp::catalog_index::KindRule {
                    code: rule.code,
                    note: rule.note,
                })
                .collect(),
        })
        .collect()
}

/// One bundled notation's show-page content, projected from its own
/// Markdown by [`portal::notation_preview_doc`] — the same projection
/// `navigator notation preview` applies to a file on disk, so the local
/// preview and this published page cannot disagree.
fn preview_doc(slug: &str, source_path: &str, src: &str) -> webapp::notation_preview::PreviewDoc {
    portal::notation_preview_doc::from_markdown(
        slug,
        &format!("{NOTATIONS_BLOB_BASE}{source_path}"),
        src,
    )
}

/// Every bundled notation's show-page content — the sample letters, fixture
/// filings, and every government form — the content
/// [`portal::dioxus_app::notation_preview_router`] serves at
/// `/notations/{slug}`.
#[allow(clippy::too_many_lines)] // The literal source-to-preview inventory is reviewed as one catalog.
fn notation_preview_docs() -> Vec<webapp::notation_preview::PreviewDoc> {
    let mut docs: Vec<_> = BUSINESS_NOTATIONS
        .iter()
        .map(|(slug, _, path, _, source)| preview_doc(slug, path, source))
        .collect();
    docs.extend([
        preview_doc(
            "onboarding-letter",
            "notations/neon_law/onboarding__letter.md",
            ONBOARDING,
        ),
        preview_doc(
            "offboarding-letter",
            "notations/neon_law/offboarding__letter.md",
            OFFBOARDING,
        ),
        preview_doc(
            "nevada-rescission-notice",
            "notations/neon_law/rescission_notice__nevada.md",
            RESCISSION_NOTICE,
        ),
        preview_doc(
            "nevada-witness-affidavit",
            "notations/neon_law/witness_affidavit__nevada.md",
            WITNESS_AFFIDAVIT,
        ),
        preview_doc(
            "nevada-answer-to-counterclaim",
            "notations/neon_law/answer_to_counterclaim__nevada.md",
            ANSWER_TO_COUNTERCLAIM,
        ),
        preview_doc(
            "nevada-engagement-letter",
            "notations/neon_law/engagement_letter__nevada.md",
            ENGAGEMENT_LETTER,
        ),
        preview_doc(
            "nevada-summons",
            "notations/neon_law/summons__nevada.md",
            SUMMONS,
        ),
        preview_doc(
            "nevada-estate-signing-preparation",
            "notations/neon_law/estate_signing_preparation__letter.md",
            ESTATE_SIGNING_PREPARATION,
        ),
        preview_doc(
            "irs-form-990",
            "notations/forms/united_states/federal/irs/us__form_990.md",
            FORM_990,
        ),
        preview_doc(
            "application-for-naturalization",
            "notations/forms/united_states/federal/uscis/us__naturalization.md",
            NATURALIZATION,
        ),
        preview_doc(
            "nevada-llc-formation",
            "notations/forms/united_states/nevada/state/nv__llc_formation.md",
            NV_LLC,
        ),
        preview_doc(
            "nevada-profit-corporation-formation",
            "notations/forms/united_states/nevada/state/nv__profit_corp_formation.md",
            NV_PROFIT_CORP,
        ),
        preview_doc(
            "nevada-business-trust-formation",
            "notations/forms/united_states/nevada/state/nv__business_trust_formation.md",
            NV_BUSINESS_TRUST,
        ),
        preview_doc(
            "nevada-nonprofit-formation",
            "notations/forms/united_states/nevada/state/nv__nonprofit_501c3_formation.md",
            NV_NONPROFIT,
        ),
        preview_doc(
            "nevada-annual-list",
            "notations/forms/united_states/nevada/state/nv__annual_report.md",
            NV_ANNUAL_REPORT,
        ),
        preview_doc(
            "nevada-llc-dissolution",
            "notations/forms/united_states/nevada/state/nv__dissolution.md",
            NV_DISSOLUTION,
        ),
        preview_doc(
            "nevada-modified-business-tax",
            "notations/forms/united_states/nevada/state/nv__modified_business_tax.md",
            NV_MODIFIED_BUSINESS_TAX,
        ),
        preview_doc(
            "nevada-charitable-solicitation-registration",
            "notations/forms/united_states/nevada/state/nv__charitable_solicitation_registration.md",
            NV_CHARITABLE,
        ),
    ]);
    docs
}

/// The public `/notations` catalog: the bundled letters and filings, plus
/// every government form in `templates/notations/forms/`.
#[allow(clippy::too_many_lines)] // The literal public inventory stays aligned with the preview catalog above.
fn notations_index_content() -> webapp::catalog_index::CatalogIndexContent {
    let introduction = crate::locales::notations_content();
    let mut content = webapp::catalog_index::CatalogIndexContent {
        title: NOTATIONS_INDEX_TITLE.to_string(),
        lede: introduction.meta_description.clone(),
        introduction: Some(introduction),
        materials: vec![
            notation_card(
                "Firm",
                "Onboarding Letter",
                "onboarding-letter",
                "The sample letter that opens a matter (`onboarding__letter`).",
                ONBOARDING,
            ),
            notation_card(
                "Firm",
                "Closing Letter",
                "offboarding-letter",
                "The sample letter that closes a matter (`offboarding__letter`).",
                OFFBOARDING,
            ),
            notation_card(
                "Nevada",
                "Notice of Rescission",
                "nevada-rescission-notice",
                "Fixture notice of rescission for a Nevada matter.",
                RESCISSION_NOTICE,
            ),
            notation_card(
                "Nevada",
                "Affidavit of Percipient Witness",
                "nevada-witness-affidavit",
                "Fixture affidavit of a percipient witness for a Nevada matter.",
                WITNESS_AFFIDAVIT,
            ),
            notation_card(
                "Nevada",
                "Answer to Counterclaim",
                "nevada-answer-to-counterclaim",
                "Fixture answer to a counterclaim in Nevada.",
                ANSWER_TO_COUNTERCLAIM,
            ),
            notation_card(
                "Nevada",
                "Engagement Letter — Arbitration",
                "nevada-engagement-letter",
                "Fixture arbitration engagement letter for a Nevada matter.",
                ENGAGEMENT_LETTER,
            ),
            notation_card(
                "Nevada",
                "Summons",
                "nevada-summons",
                "Fixture civil summons for a Nevada matter.",
                SUMMONS,
            ),
            notation_card(
                "Nevada",
                "Prepare for Your Signing",
                "nevada-estate-signing-preparation",
                "The client email sent ahead of an estate-plan signing ceremony (`estate_signing_preparation__letter`).",
                ESTATE_SIGNING_PREPARATION,
            ),
            notation_card(
                "Federal",
                "IRS Form 990",
                "irs-form-990",
                "Return of Organization Exempt From Income Tax.",
                FORM_990,
            ),
            notation_card(
                "Federal",
                "Application for Naturalization (N-400)",
                "application-for-naturalization",
                "Intake summary for Form N-400.",
                NATURALIZATION,
            ),
            notation_card(
                "Nevada",
                "Nevada LLC Formation",
                "nevada-llc-formation",
                "Articles of organization for a Nevada limited-liability company.",
                NV_LLC,
            ),
            notation_card(
                "Nevada",
                "Nevada Profit Corporation Formation",
                "nevada-profit-corporation-formation",
                "Articles of incorporation for a Nevada profit corporation.",
                NV_PROFIT_CORP,
            ),
            notation_card(
                "Nevada",
                "Nevada Business Trust Formation",
                "nevada-business-trust-formation",
                "Certificate of business trust for Nevada.",
                NV_BUSINESS_TRUST,
            ),
            notation_card(
                "Nevada",
                "Nevada Nonprofit Articles of Incorporation (501(c)(3))",
                "nevada-nonprofit-formation",
                "Articles that form a Nevada nonprofit seeking 501(c)(3) status.",
                NV_NONPROFIT,
            ),
            notation_card(
                "Nevada",
                "Nevada Annual List",
                "nevada-annual-list",
                "Annual list of managers, members, and registered agent.",
                NV_ANNUAL_REPORT,
            ),
            notation_card(
                "Nevada",
                "Nevada LLC Articles of Dissolution",
                "nevada-llc-dissolution",
                "The filing that dissolves a Nevada LLC.",
                NV_DISSOLUTION,
            ),
            notation_card(
                "Nevada",
                "Nevada Modified Business Tax Return",
                "nevada-modified-business-tax",
                "Nevada Modified Business Tax return.",
                NV_MODIFIED_BUSINESS_TAX,
            ),
            notation_card(
                "Nevada",
                "Nevada Charitable Solicitation Registration",
                "nevada-charitable-solicitation-registration",
                "Registration before soliciting donations in Nevada.",
                NV_CHARITABLE,
            ),
        ],
        contact_email: views::brand::firm_email().to_string(),
        footnote: String::new(),
        include_testimonials: false,
        brand_key: String::new(),
        kinds: kind_catalog_entries(),
    };
    let business = BUSINESS_NOTATIONS
        .iter()
        .map(|(slug, title, _, summary, source)| {
            let jurisdiction = if *slug == "employee-offer-letter" {
                "United States"
            } else {
                "Business · lawyer review"
            };
            notation_card(jurisdiction, title, slug, summary, source)
        });
    content.materials.splice(0..0, business);
    content
}

/// The firm host's public Dioxus SSR pages, as raw routers for
/// [`portal::bootstrap`]'s `host_dioxus` argument. `bootstrap` wraps each in
/// the anonymous-access session boundary and the shared layer stack, exactly as
/// it does the built-in Dioxus routes.
///
/// Takes `state` because the content-backed pages (e.g. `/blog`) read request
/// state (the database handle) the router injects into the render context; the
/// brand-only pages ignore it.
#[must_use]
#[allow(clippy::too_many_lines)] // A flat list of the firm's public page routers.
pub fn firm_public_dioxus_routers(state: &AppState) -> Vec<Router> {
    let mut routers = vec![
        dioxus_app::blog_index_router(state.surreal.clone()),
        dioxus_app::blog_post_router(state.surreal.clone()),
    ];
    // Resolve the branding from `state.brand_bundle` (mirroring `bootstrap`)
    // rather than the ambient `current()`: this content is baked at
    // router-build time, before any request scopes branding.
    let branding = state
        .brand_bundle
        .as_ref()
        .map_or(&views::brand::DEFAULT_BRANDING, |bundle| {
            views::brand::Branding::from_manifest(&bundle.manifest)
        });
    routers.push(dioxus_app::catalog_index_router(
        dioxus_app::NOTATIONS_INDEX_PATH,
        notations_index_content(),
        state.surreal.clone(),
    ));
    // Also carries `navigator notation preview`'s draft door (LAW-29): a
    // pushed template, stored and addressable but explicitly not run. Both
    // mounts share one `FullstackState` deliberately — see the function's
    // own doc comment.
    routers.push(dioxus_app::notation_preview_router(
        notation_preview_docs(),
        webapp::notation_preview::NotationPreviewMode::Published,
        Some(state.surreal.clone()),
    ));
    let contact_copy = branded_map(branding, "/contact", |resolved| {
        webapp::contact_page::InjectedContact(resolve_firm_contact_content(resolved))
    });
    routers.push(with_branded(
        dioxus_app::contact_router(
            "/contact",
            resolve_firm_contact_content(branding),
            state.sessions.clone(),
            portal::secure_cookies(state),
        ),
        contact_copy,
    ));
    routers.push(with_branded(
        dioxus_app::contact_sent_router("/contact/sent", resolve_firm_contact_content(branding)),
        branded_map(branding, "/contact/sent", |resolved| {
            webapp::contact_page::InjectedContact(resolve_firm_contact_content(resolved))
        }),
    ));
    // The team's public copy is a small static catalog; the public roster
    // never reads person records or client data.
    let team = locales::team(branding);
    routers.push(dioxus_app::team_index_router(
        "/team",
        webapp::team_page::TeamIndexContent {
            head_title: team.head_title.clone(),
            meta_description: team.meta_description.clone(),
            og_description: team.og_description.clone(),
            page_title: team.page_title,
            members: team
                .members
                .iter()
                .map(|member| webapp::team_page::TeamMemberSummary {
                    name: member.name.clone(),
                    href: member.href.clone(),
                })
                .collect(),
        },
    ));
    routers.push(dioxus_app::team_profile_router(
        "/team/nick",
        webapp::team_page::TeamProfileContent {
            head_title: format!("{} | {}", branding.firm.site_name, team.profile.name),
            meta_description: team.profile.biography.clone(),
            og_description: None,
            name: team.profile.name,
            biography: team.profile.biography,
            linkedin_url: team.profile.linkedin_url,
        },
    ));
    let deployment_host = state.canonical_host.host();
    let home = resolve_firm_home_content(branding, deployment_host);
    // The home page (`/`): static copy plus the store's approved testimonials.
    // The practice boxes on `/` are the YAML catalog workshop slides
    // reuse — one list, not a second Rust copy. Slides always expand the
    // Neon catalog, even when another host is serving `/`.
    let practice_catalog = locales::home(&views::brand::DEFAULT_BRANDING)
        .practices
        .clone();
    let home_copy = branded_map(branding, "/", |resolved| webapp::home::InjectedHome {
        content: resolve_firm_home_content(resolved, deployment_host),
        lead_capture: Some(home_lead_capture(resolved)),
        brand_key: resolved.brand_key.as_str().to_string(),
    });
    routers.push(with_branded(
        dioxus_app::home_router(
            "/",
            home,
            state.surreal.clone(),
            Some(home_lead_capture(branding)),
            state.sessions.clone(),
            portal::secure_cookies(state),
        ),
        home_copy,
    ));
    let testimonials_copy = branded_map(branding, "/testimonials", |resolved| {
        webapp::testimonials_page::InjectedTestimonials {
            brand_key: resolved.brand_key.as_str().to_string(),
        }
    });
    routers.push(with_branded(
        dioxus_app::testimonials_router(dioxus_app::TESTIMONIALS_PATH, state.surreal.clone()),
        testimonials_copy,
    ));
    // The practice pages the home page's cards lead into. Static copy like the
    // home page's, resolved here so the `<title>` names the mounted brand.
    routers.push(dioxus_app::litigation_router(
        "/disputes",
        resolve_litigation_content(branding),
    ));
    routers.push(dioxus_app::transactional_router(
        "/business",
        resolve_transactional_content(branding),
    ));
    // The platform page. It carries a commercial offer, so it sits with the
    // firm's own pages.
    // The retired consumer plan. The firm speaks to emerging technology
    // companies now, so the page is gone — but its URL was published, and a
    // published URL outlives the page behind it. A permanent redirect keeps
    // every inbound link and search result resolving instead of stranding it
    // on a 404.
    //
    // 301 rather than axum's `Redirect::permanent`, which is a 308. Both are
    // permanent, but 308 additionally promises the method is preserved —
    // a guarantee about request semantics that a retired marketing page has
    // no need to make, and that the older crawlers and link checkers reading
    // this URL understand least well. 301 is what a moved marketing page has
    // always answered.
    routers.push(Router::new().route(
        dioxus_app::FIRM_RETIRED_PERSONAL_PATH,
        axum::routing::get(|| async {
            (
                StatusCode::MOVED_PERMANENTLY,
                [(
                    axum::http::header::LOCATION,
                    dioxus_app::FIRM_RETIRED_PERSONAL_TARGET,
                )],
            )
        }),
    ));
    routers.push(dioxus_app::marketing_page_router(
        dioxus_app::FIRM_NAVIGATOR_PATH,
        firm_copy::navigator(branding),
        state.sessions.clone(),
        portal::secure_cookies(state),
    ));
    routers.push(dioxus_app::marketing_page_router(
        "/legal-aid",
        firm_copy::legal_aid(branding),
        state.sessions.clone(),
        portal::secure_cookies(state),
    ));
    let services_copy = branded_map(branding, "/services", |resolved| {
        webapp::marketing_page::InjectedMarketingPage(firm_copy::legal_services(resolved))
    });
    for source_key in [BrandKey::Aegis, BrandKey::Vesta] {
        for (path, stem) in source_key.explainer_pages() {
            let copies = BrandKey::ALL
                .iter()
                .copied()
                .filter(|key| key.explainer_pages().contains(&(*path, *stem)))
                .map(|key| {
                    (
                        key,
                        webapp::marketing_page::InjectedMarketingPage(firm_copy::explainer(
                            key.resolve_branding(branding),
                            stem,
                        )),
                    )
                })
                .collect();
            routers.push(with_branded(
                dioxus_app::marketing_page_router(
                    path,
                    firm_copy::explainer(source_key.resolve_branding(branding), stem),
                    state.sessions.clone(),
                    portal::secure_cookies(state),
                ),
                copies,
            ));
        }
    }
    routers.push(with_branded(
        dioxus_app::marketing_page_router(
            dioxus_app::FIRM_SERVICES_PATH,
            firm_copy::legal_services(branding),
            state.sessions.clone(),
            portal::secure_cookies(state),
        ),
        services_copy,
    ));
    // `/delete-your-debt` — Neon's own gateway to the DeleteYourDebt.com
    // practice. Neon-only, like `/navigator`: no `with_branded` injection,
    // because `views::brand::BrandKey::publishes_firm_path`'s Neon arm
    // already admits this path (it is not one of the three retired-page
    // exclusions) while every other brand's arm is its own finite allow-list
    // that does not name it, so `reject_unpublished_firm_path` 404s it on
    // every other host without any further gating here.
    routers.push(dioxus_app::marketing_page_router(
        "/delete-your-debt",
        firm_copy::delete_your_debt_gateway(branding, deployment_host),
        state.sessions.clone(),
        portal::secure_cookies(state),
    ));
    // `/delete-your-data` — Neon's own gateway to the DeleteYourData.com
    // practice. Same admission shape as `/delete-your-debt` immediately
    // above: Neon's `publishes_firm_path` arm admits it by not excluding it,
    // and every other brand's own finite allow-list does not name it, so
    // `reject_unpublished_firm_path` 404s it everywhere else with no further
    // gating here.
    routers.push(dioxus_app::marketing_page_router(
        "/delete-your-data",
        firm_copy::delete_your_data_gateway(branding, deployment_host),
        state.sessions.clone(),
        portal::secure_cookies(state),
    ));
    // `/nautilus` — Neon Law Nautilus, the DeleteYourData.com subscription
    // and its optional death check. Same admission shape as
    // `/delete-your-debt` above.
    routers.push(dioxus_app::marketing_page_router(
        "/nautilus",
        firm_copy::nautilus(branding, deployment_host),
        state.sessions.clone(),
        portal::secure_cookies(state),
    ));
    // `/immigration` — Neon's own gateway to the Abhaya Immigration
    // practice. Same admission shape as `/delete-your-debt` above.
    routers.push(dioxus_app::marketing_page_router(
        "/immigration",
        firm_copy::immigration_gateway(branding, deployment_host),
        state.sessions.clone(),
        portal::secure_cookies(state),
    ));
    // `/estate-planning` — Neon's own gateway to the Vesta Estate Planning
    // practice. Same admission shape as `/delete-your-debt` above.
    routers.push(dioxus_app::marketing_page_router(
        "/estate-planning",
        firm_copy::estate_planning_gateway(branding, deployment_host),
        state.sessions.clone(),
        portal::secure_cookies(state),
    ));
    // `/accidents` — Neon's own gateway to the Misericordia Injury Law
    // practice. Same admission shape as `/delete-your-debt` above.
    routers.push(dioxus_app::marketing_page_router(
        "/accidents",
        firm_copy::accidents_gateway(branding, deployment_host),
        state.sessions.clone(),
        portal::secure_cookies(state),
    ));
    // `/divorce` — Neon's own gateway to the Daybridge Divorce Law practice.
    // Same admission shape as `/delete-your-debt` above.
    routers.push(dioxus_app::marketing_page_router(
        "/divorce",
        firm_copy::divorce_gateway(branding, deployment_host),
        state.sessions.clone(),
        portal::secure_cookies(state),
    ));
    // The talks catalog, and the five read routes each talk publishes: the
    // hub, its light table, the classroom step face, the projector face a
    // presenter opens on a second screen, and the certificate confirmation.
    // The hub's pre-layer also owns the `…/{slug}.md` raw-Markdown twin, which
    // matchit routes there rather than to a second path.
    //
    routers.push(dioxus_app::catalog_index_router(
        dioxus_app::PRESENTATION_INDEX_PATH,
        presentation_index_content(&state.workshops),
        state.surreal.clone(),
    ));
    routers.extend(dioxus_app::catalog_material_routers(
        &dioxus_app::PRESENTATION_PATHS,
        state.workshops.clone(),
        &state.sessions,
        secure_cookies(state),
        practice_catalog.clone(),
    ));
    // The Navigator classes, anonymous like the talks.
    // The certificate `POST` keeps its own gate: who may claim a completion
    // certificate is an authorization question, and it stays one even when the
    // material is free to read.
    routers.push(dioxus_app::retired_workshops_router());
    routers.extend(dioxus_app::catalog_material_routers(
        &dioxus_app::WORKSHOP_PATHS,
        state.workshops.clone(),
        &state.sessions,
        secure_cookies(state),
        practice_catalog,
    ));
    routers.push(portal::catalog_workshop_command_routes(state));
    let services_catalog = locales::services_catalog(branding);
    routers.push(portal::purchases::routes(
        state,
        services_catalog
            .as_ref()
            .map(|catalog| catalog.purchase.clone()),
    ));
    routers.push(portal::start_door::routes(state, services_catalog));
    routers
        .into_iter()
        .map(|router| router.layer(from_fn(reject_unpublished_firm_path)))
        .collect()
}

/// Pre-bake one piece of content per brand, for `inject_branded` to hand the
/// SSR router at request time.
///
/// `path` is the route's own firm path: a brand that does not publish it
/// (`BrandKey::publishes_firm_path` — the same gate the request middleware
/// enforces) gets no entry, so its content is never baked. Loading a brand's
/// catalog that it does not publish would panic — the held-out IEP ships no
/// catalogs at all — and no request could ever serve the entry: the same gate
/// refuses the path first.
fn branded_map<T>(
    default: &'static views::brand::Branding,
    path: &str,
    build: impl Fn(&views::brand::Branding) -> T,
) -> HashMap<BrandKey, T> {
    BrandKey::ALL
        .iter()
        .copied()
        .filter(|key| key.publishes_firm_path(path))
        .map(|key| (key, build(key.resolve_branding(default))))
        .collect()
}

fn with_branded<T: Clone + Send + Sync + 'static>(
    router: Router,
    copies: HashMap<BrandKey, T>,
) -> Router {
    router.layer(from_fn_with_state(Arc::new(copies), inject_branded::<T>))
}

async fn inject_branded<T: Clone + Send + Sync + 'static>(
    State(copies): State<Arc<HashMap<BrandKey, T>>>,
    mut req: Request,
    next: Next,
) -> Response {
    let key = req
        .extensions()
        .get::<BrandKey>()
        .copied()
        .unwrap_or_default();
    if let Some(value) = copies.get(&key).cloned() {
        req.extensions_mut().insert(value);
    }
    next.run(req).await
}

async fn reject_unpublished_firm_path(req: Request, next: Next) -> Response {
    let key = req
        .extensions()
        .get::<BrandKey>()
        .copied()
        .unwrap_or_default();
    if key.publishes_firm_path(req.uri().path()) {
        next.run(req).await
    } else {
        (StatusCode::NOT_FOUND, webapp::error_pages::not_found()).into_response()
    }
}

/// Resolve the firm `/contact` content from the mounted `branding`'s addresses
/// — the wasm-safe [`webapp::contact_page::ContactContent`] the Dioxus contact
/// router injects. Takes the resolved `branding` explicitly because the content
/// is baked at router-build time, before per-request branding scope.
fn resolve_firm_contact_content(
    branding: &views::brand::Branding,
) -> webapp::contact_page::ContactContent {
    let firm_name = branding.firm.site_name;

    let page_title = "Contact";
    let meta_description = match branding.brand_key {
        BrandKey::DeleteYourData => format!(
            "Reach {firm_name}, a practice of Guieb & Shook PC, about a data-deletion request. \
             Attorney advertisement. Nothing here is legal advice without a signed retainer for \
             an active project."
        ),
        BrandKey::LawyerShook
        | BrandKey::Vesta
        | BrandKey::Misericordia
        | BrandKey::Abhaya
        | BrandKey::DeleteYourDebt
        | BrandKey::Summons
        | BrandKey::Daybridge
        | BrandKey::CyberInjuryLaw
        | BrandKey::Aegis
        | BrandKey::Groundwork
        | BrandKey::DeathAndDivorce => format!(
            "Reach {firm_name}, a practice of Guieb & Shook PC, about legal services. \
             Attorney advertisement. Nothing here is legal advice without a signed retainer for \
             an active project."
        ),
        // IEP belongs to the second firm, so its contact page names Lex
        // Tecnica LLP — never Guieb & Shook PC.
        BrandKey::Iep => format!(
            "Reach {firm_name}, a practice of Lex Tecnica LLP, about legal services. \
             Attorney advertisement. Nothing here is legal advice without a signed retainer for \
             an active project."
        ),
        BrandKey::GuiebShook => format!(
            "Reach {firm_name}, attorneys at law. Attorney advertisement. Nothing here is legal \
             advice without a signed retainer for an active project."
        ),
        BrandKey::Neon => format!(
            "Reach {firm_name} for estate planning, corporate formation, litigation, and ongoing \
             legal services."
        ),
    };
    webapp::contact_page::ContactContent {
        head_title: format!("{firm_name} | {page_title}"),
        meta_description,
        og_description: None,
        page_title: page_title.to_string(),
        email_label: "Email".to_string(),
        phone_label: "Phone".to_string(),
        firm_email: branding.firm_email.to_string(),
        booking_href: branding.consultation_url.to_string(),
        booking_label: if branding.brand_key == BrandKey::DeleteYourDebt {
            "Contact PandA Law"
        } else {
            "Book Consultation"
        }
        .to_string(),
        firm_phone: branding.firm_phone.to_string(),
        lead_capture: locales::lead_capture(branding),
    }
}

/// Resolve the firm home page's static copy from the mounted `branding` — the
/// wasm-safe [`webapp::home::HomeContent`] the Dioxus home router injects.
/// Brand-safe like [`resolve_firm_contact_content`]: the `<title>` names the
/// mounted brand, resolved at router-build time.
///
/// Neon presents membership, the one-time setup fee, and booking on one page.
/// Other house brands resolve their own home catalogs.
pub(crate) fn resolve_firm_home_content(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> webapp::home::HomeContent {
    match branding.brand_key {
        BrandKey::LawyerShook => lawyer_shook_holding_content(branding),
        BrandKey::GuiebShook => guieb_shook_holding_content(branding),
        BrandKey::Neon
        | BrandKey::DeleteYourData
        | BrandKey::Vesta
        | BrandKey::Misericordia
        | BrandKey::Abhaya
        | BrandKey::DeleteYourDebt
        | BrandKey::Daybridge
        | BrandKey::CyberInjuryLaw
        | BrandKey::Summons
        | BrandKey::Aegis
        | BrandKey::Groundwork
        | BrandKey::DeathAndDivorce
        | BrandKey::Iep => locales::home_for_host(branding, deployment_host),
    }
}

/// Brands that collect a contact request directly on their home page.
fn home_lead_capture(branding: &views::brand::Branding) -> webapp::lead_capture::LeadCaptureCopy {
    locales::lead_capture(branding)
}

/// The firm's notice and sign-in line, followed by the practice cards from
/// Lawyer Shook's home catalog, over the shared footer.
fn lawyer_shook_holding_content(branding: &views::brand::Branding) -> webapp::home::HomeContent {
    let legal_entity = branding.firm.legal_entity;
    let paragraph = format!(
        "{legal_entity} is an American law firm. We build brands that put the customer not \
         the lawyer first."
    );
    let mut content = webapp::home::HomeContent {
        head_title: format!("{legal_entity} | Home"),
        meta_description: paragraph.clone(),
        bare: Some(webapp::home::BareStatement {
            heading: legal_entity.to_string(),
            paragraph,
            // The footer's wordmark otherwise carries this brand's own name
            // ("Lawyer Shook"); the firm's holding page reads as the firm,
            // so its footer names the legal entity instead.
            footer_wordmark: legal_entity.to_string(),
            sign_in: vec![
                webapp::home::CopyRun {
                    text: "Sign in ".to_string(),
                    emphasis: false,
                    href: None,
                },
                webapp::home::CopyRun {
                    text: "here".to_string(),
                    emphasis: false,
                    // Absolute against the deployment's own origin, not this
                    // host's: `/auth/login` sets the one-shot pre-auth cookie
                    // on whichever host serves it, and the OIDC callback lands
                    // only on `OAUTH_REDIRECT_URI`'s host, so a login started
                    // on the holding page's own host would arrive at the
                    // callback without its cookie. Where no `NAV_BASE_URL` is
                    // configured (dev, tests) this stays the relative path.
                    href: Some(views::assets::absolute_url("/auth/login")),
                },
                webapp::home::CopyRun {
                    text: " if you are an active client.".to_string(),
                    emphasis: false,
                    href: None,
                },
            ],
        }),
        ..locales::home(branding)
    };
    content.practices_heading = "Our Family".to_string();
    content.practices = portfolio_practices(locales::home(branding).practices);
    content
}

/// Guieb & Shook's whole page: the firm's legal name, "Attorneys at Law.",
/// and the sign-in line — no practice cards, no catalog copy.
fn guieb_shook_holding_content(branding: &views::brand::Branding) -> webapp::home::HomeContent {
    let mut content = lawyer_shook_holding_content(branding);
    let legal_entity = branding.firm.legal_entity;
    let paragraph = format!("Attorneys at Law. {legal_entity} is an American law firm.");
    if let Some(bare) = content.bare.as_mut() {
        bare.paragraph.clone_from(&paragraph);
    }
    content.meta_description = paragraph;
    content.practices_heading = String::new();
    content.practices = Vec::new();
    content.signup = None;
    content
}

/// The parent firm's directory is derived from the compiled registry so a
/// newly registered brand cannot silently disappear from the holding page.
/// Every registered house brand appears here, including practices whose
/// public hosts still follow the launch gate. Sorted alphabetically by name
/// rather than registry order, since this is a directory for a reader to
/// scan, not a ranking.
fn portfolio_practices(copy: Vec<webapp::home::PracticeLink>) -> Vec<webapp::home::PracticeLink> {
    let descriptions: std::collections::HashMap<String, String> = copy
        .into_iter()
        .map(|practice| (practice.href, practice.body))
        .collect();
    let mut practices: Vec<webapp::home::PracticeLink> = views::brand::BrandKey::ALL
        .iter()
        .copied()
        .filter(|key| !matches!(key, BrandKey::LawyerShook | BrandKey::GuiebShook))
        .map(|key| {
            let branding = key.resolve_branding(&views::brand::DEFAULT_BRANDING);
            webapp::home::PracticeLink {
                mark: webapp::components::PracticeMark::Scales,
                heading: branding.firm.site_name.to_string(),
                body: descriptions
                    .get(&key.public_home_href())
                    .cloned()
                    .unwrap_or_else(|| key.family_byline().to_string()),
                href: key.public_home_href(),
                logo_href: branding.firm.logo_href.to_string(),
                font_family: key.default_typeface().stack.to_string(),
                primary_color: key.default_palette().light.primary.to_string(),
            }
        })
        .collect();
    practices.sort_by(|a, b| a.heading.cmp(&b.heading));
    practices
}

#[cfg(test)]
mod home_page_tests {
    use super::resolve_firm_home_content;
    use views::brand::BrandKey;

    /// Every registered brand has a deliberate home surface: authored design
    /// copy, a holding statement, or an explicit Coming Soon notice.
    #[test]
    fn every_brand_home_has_design_or_a_coming_soon_notice() {
        for key in BrandKey::ALL {
            let branding = key.resolve_branding(&views::brand::DEFAULT_BRANDING);
            let content = resolve_firm_home_content(branding, None);
            let authored_design = !content.heading.is_empty()
                && (content.service.is_some()
                    || content.estate.is_some()
                    || content.privacy.is_some()
                    || content.company.is_some()
                    || content.daybridge.is_some()
                    || content.death_and_divorce.is_some()
                    || content.cyber_injury.is_some()
                    || content.summons.is_some()
                    || !content.practices.is_empty());
            let holding_design = content
                .bare
                .as_ref()
                .is_some_and(|bare| bare.heading != "Coming Soon" && !bare.paragraph.is_empty());
            let coming_soon = content
                .bare
                .as_ref()
                .is_some_and(|bare| bare.heading == "Coming Soon");

            assert!(
                authored_design || holding_design || coming_soon,
                "{} has no designed or Coming Soon home surface",
                key.as_str()
            );
        }
    }

    #[test]
    fn summons_publishes_the_catalog_and_intake() {
        let content = resolve_firm_home_content(&views::brand::SUMMONS_BRANDING, None);
        let copy = content.summons.expect("Summons has a designed home");
        assert!(content.bare.is_none());
        assert_eq!(copy.fees[0][1], "$100");
        assert_eq!(copy.fees[1][1], "$500");
        assert!(!super::home_lead_capture(&views::brand::SUMMONS_BRANDING)
            .consent_sentence
            .is_empty());
        for href in copy
            .resources
            .iter()
            .map(|link| &link[1])
            .chain(copy.guides.iter().map(|guide| &guide[4]))
            .chain(copy.cases.iter().map(|case| &case[3]))
        {
            assert!(href.starts_with("https://www.nyc.gov/"), "{href}");
        }
    }

    #[test]
    fn lawyer_shook_home_carries_its_practice_descriptions_from_its_catalog() {
        let content = resolve_firm_home_content(&views::brand::LAWYER_SHOOK_BRANDING, None);
        let misericordia = content
            .practices
            .iter()
            .find(|practice| practice.heading == "Misericordia Injury Law")
            .expect("Misericordia is in the brand directory");
        assert!(misericordia.body.contains("wrongful death"));
        let cyber = content
            .practices
            .iter()
            .find(|practice| practice.heading == "CyberInjuryLaw")
            .expect("Cyber Injury Law is in the brand directory");
        for term in ["cybercrime", "AI"] {
            assert!(cyber.body.contains(term), "missing {term}");
        }
        for heading in ["Aegis Criminal Defense", "Groundwork Real Estate Law"] {
            assert!(
                content
                    .practices
                    .iter()
                    .any(|practice| practice.heading == heading),
                "missing {heading}"
            );
        }
    }
}

#[cfg(test)]
mod lawyer_shook_holding_page_tests {
    use super::lawyer_shook_holding_content;

    #[test]
    fn the_home_page_is_a_bare_statement_naming_the_firm() {
        let content = lawyer_shook_holding_content(&views::brand::LAWYER_SHOOK_BRANDING);
        let bare = content
            .bare
            .expect("Lawyer Shook's home page is the bare-statement variant");
        assert_eq!(bare.heading, "Guieb & Shook PC");
        assert_eq!(content.head_title, "Guieb & Shook PC | Home");
        assert!(bare
            .paragraph
            .contains("Guieb & Shook PC is an American law firm"));
        assert!(bare
            .paragraph
            .contains("We build brands that put the customer not the lawyer first."));
        assert!(
            !bare.paragraph.contains("Lawyer Shook"),
            "{}",
            bare.paragraph
        );
        // The footer names the firm, not this brand's own "Lawyer Shook" mark.
        assert_eq!(bare.footer_wordmark, "Guieb & Shook PC");
        // The firm's notice leads into every admitted brand door.
        assert!(content.service.is_none());
        let headings: Vec<&str> = content
            .practices
            .iter()
            .map(|practice| practice.heading.as_str())
            .collect();
        assert_eq!(
            headings,
            vec![
                "Abhaya Immigration",
                "Aegis Criminal Defense",
                "CyberInjuryLaw",
                "Daybridge Divorce Law",
                "Death & Divorce",
                "DeleteYourData.com",
                "DeleteYourDebt.com",
                "Groundwork Real Estate Law",
                "Misericordia Injury Law",
                "Neon Law",
                "Summons Defense",
                "Vesta Estate Planning",
            ],
            "the directory is alphabetical, not registry order"
        );
        assert!(content
            .practices
            .iter()
            .all(|practice| !practice.logo_href.is_empty()
                && !practice.font_family.is_empty()
                && !practice.primary_color.is_empty()));
        let summons = content
            .practices
            .iter()
            .find(|practice| practice.href == "https://www.summonsdefense.nyc")
            .expect("Lawyer Shook links its Summons Defense NYC practice");
        assert_eq!(summons.heading, "Summons Defense");
        assert_eq!(summons.body, "NYC summons defense at OATH hearings");
        assert!(content.provenance.is_none());
        // The one link on the page: an existing client's way to `/app`.
        let sign_in_text: String = bare.sign_in.iter().map(|run| run.text.as_str()).collect();
        assert!(sign_in_text.contains("Sign in"));
        assert!(sign_in_text.contains("active client"));
        assert!(bare.sign_in.iter().any(|run| run
            .href
            .as_deref()
            .is_some_and(|href| href.ends_with("/auth/login"))));
    }

    #[test]
    fn guieb_shook_home_does_not_open_the_account_invitation() {
        let content = super::guieb_shook_holding_content(&views::brand::GUIEB_SHOOK_BRANDING);
        assert!(content.signup.is_none());
    }
}

/// Resolve the firm `/disputes` page — the statement, the practice, and how
/// the firm runs a matter.
///
/// **The page's claim is speed, and speed is stated as method rather than as
/// outcome.** "Litigation built for speed" is a differentiator a bar
/// examiner reads as an implied result unless the body binds it to *how the
/// firm works*, so the closing paragraph says so outright. The same line is why
/// `publishes_no_quantified_efficiency_claim` matters more here than it did
/// under the previous framing: a page that leads with speed is one number away
/// from advertising a result.
///
/// The copy carries no em dash. That is the firm's own style call for this
/// page, and `publishes_no_em_dash` holds it.
///
/// Brand-safe like [`resolve_firm_home_content`]: the `<title>` names the
/// mounted brand, resolved at router-build time.
///
/// **The page names matter *types* the firm has litigated and never a matter.**
/// Trademark and copyright, prison rights, divorce, restraining orders, and
/// domestic violence are categories, so none of them identifies a client, a
/// Project code, or an outcome. That distinction is what keeps the copy inside
/// the no-client-data rule while still telling a reader whether this is their
/// practice. The docket is open: the page says the firm takes cases of every
/// kind, then names types it has litigated so a reader can still recognise
/// their matter. The focus is impact litigation, stated as aim rather than as
/// a promised result. Naming experience is also precisely the situation the
/// footer's "Past results do not guarantee future outcomes." exists to cover,
/// and `carries_the_regulated_copy_and_no_results_promise` asserts it reaches
/// the reader.
///
/// **The body is the firm's own filed copy and `locales/en/litigation.yaml` holds it verbatim.**
/// The page arrived at these paragraphs by subtraction: it was a Rule 23
/// explainer with six certification-element cards, an authority strip, a phase
/// rail, a chip list, and a fee section. Each was a reasonable answer to a
/// question a prospective client does not walk in with.
///
/// The last four paragraphs — how a matter actually runs here — are additions
/// since, and they are deliberately *prose in the same card* rather than feature
/// sections, because a heading and a grid is the shape of everything this page
/// shed. `renders_two_sections_and_no_more` is what keeps that distinction, so a
/// paragraph may be added here and a section may not.
///
/// The first of them is the only paragraph on the page that links, which is why
/// the body is runs rather than plain strings: it names Navigator and points at
/// `/navigator` instead of restating that page here, the same way the home
/// page's prose does.
///
/// **Every mechanism named is one the workspace can be opened to prove.** The
/// durable event-driven engine lives in `workflows-service`; the graph is the
/// `relationship` relation plus the append-only `relationship_log`; and the
/// filing kinds are `store::cases::EntryKind`.
///
/// **Four claims were drafted for this page and cut for want of an
/// implementation**: semantic case-law search and the vendors behind it, regex
/// over the record, fact extraction, and a per-pleading template library (the
/// tree carries one litigation template, a TRO). A vendor name or a capability
/// on this page is a claim that the workspace carries it, and
/// `litigation_claims_only_capabilities_the_workspace_carries` is the guard
/// that keeps the claim checkable. Describe the step; name the tool only once a
/// module in this tree calls it.
///
/// **This page states no disclaimer of its own.** The notice lives once, in
/// `views::brand::DEFAULT_BRANDING`'s `firm_disclaimer`, which opens with
/// "Attorney advertisement." and reaches this page through `PublicFooter`.
///
/// **The page states no fee arrangement, deliberately.** Whether to state that a
/// contingency case costs the reader nothing to bring is an open question: the
/// copy addresses people whose first question is whether they can afford to
/// walk in at all. Fee *amounts* stay off the page either way, and the currency
/// guard holds that.
pub(crate) fn resolve_litigation_content(
    branding: &views::brand::Branding,
) -> webapp::litigation_page::LitigationContent {
    locales::litigation(branding)
}

/// Resolve the firm `/business` page — the company counsel practice with
/// its own published pricing, the published turnaround, and the work that
/// sits outside the retainer.
///
/// Brand-safe like [`resolve_firm_home_content`]. The base fee is published on
/// the page as flat-fee pricing cards (annual or daily cadence, plus the MSA
/// flat fee) rather than quoted through the contact inbox.
pub(crate) fn resolve_transactional_content(
    branding: &views::brand::Branding,
) -> webapp::transactional_page::TransactionalContent {
    locales::fractional_gc(branding)
}

#[cfg(test)]
mod formation_engagement_copy_tests {
    /// The three Nevada formation bodies, read as the bytes
    /// `notation_preview_docs` publishes at `/notations/{slug}`. The constants
    /// there are function-local, so this reads the same files rather than
    /// reaching into that scope.
    const BODIES: [(&str, &str); 3] = [
        (
            "nv__business_trust_formation",
            include_str!(
                "../../templates/notations/forms/united_states/nevada/state/nv__business_trust_formation.md"
            ),
        ),
        (
            "nv__llc_formation",
            include_str!("../../templates/notations/forms/united_states/nevada/state/nv__llc_formation.md"),
        ),
        (
            "nv__profit_corp_formation",
            include_str!(
                "../../templates/notations/forms/united_states/nevada/state/nv__profit_corp_formation.md"
            ),
        ),
    ];

    /// Each body defines `the "Engagement"` and must then use that defined term
    /// to carry its scope. A bare `It covers the` leaves the pronoun to resolve
    /// across two intervening nouns — the entity type and the client's name —
    /// so a reader can attach the scope list to the entity rather than to the
    /// Engagement. The defined term is already in the sentence; using it costs
    /// nothing and removes the ambiguity.
    #[test]
    fn scope_sentence_uses_the_defined_term() {
        for (code, body) in BODIES {
            assert!(
                body.contains("(the \"Engagement\")"),
                "{code}: body no longer defines the \"Engagement\" term"
            );
            assert!(
                body.contains("The Engagement covers the"),
                "{code}: scope sentence does not carry the defined term"
            );
            assert!(
                !body.contains("It covers the"),
                "{code}: scope sentence still leads with the ambiguous pronoun"
            );
        }
    }

    /// Each body opens an engagement, states a scope, and carries both
    /// signature blocks, so it is a paper the client signs. NV RPC 1.5(b)
    /// requires the basis or rate of the fee to be communicated in writing.
    /// These bodies are published templates rather than the fee agreement, so
    /// they satisfy that by naming where the fee is set — not by quoting one.
    #[test]
    fn each_body_says_where_the_fee_is_set() {
        for (code, body) in BODIES {
            assert!(
                body.contains("{{client.signature}}") && body.contains("{{firm.signature}}"),
                "{code}: expected a body both parties sign"
            );
            assert!(
                body.contains("set in the separate signed fee agreement"),
                "{code}: body states a scope but never says where the fee is set"
            );
            assert!(
                body.contains("passed through at cost"),
                "{code}: body does not name filing fees as pass-through"
            );
        }
    }

    /// A published template is a fixed artifact, so any amount baked into one
    /// goes stale the moment the firm re-prices — which is what a former
    /// catalog list price did here. The bodies name where the fee lives and
    /// publish no amount, matching the convention `resolve_transactional_content`
    /// documents for the firm pages.
    #[test]
    fn no_body_publishes_an_amount() {
        for (code, body) in BODIES {
            assert!(
                !body
                    .as_bytes()
                    .windows(2)
                    .any(|w| w[0] == b'$' && w[1].is_ascii_digit()),
                "{code}: body publishes a currency amount"
            );
            assert!(
                !body.contains("per year"),
                "{code}: body publishes a recurring price cadence"
            );
        }
    }
}

#[cfg(test)]
mod notation_catalog_tests {
    use std::{collections::BTreeSet, fs, path::Path};

    use super::{notation_preview_docs, notations_index_content, NOTATIONS_BLOB_BASE};

    fn collect_markdown_paths(root: &Path, base: &Path, paths: &mut BTreeSet<String>) {
        for entry in fs::read_dir(root).expect("read legal template shelf") {
            let path = entry.expect("read legal template directory entry").path();
            if path.is_dir() {
                collect_markdown_paths(&path, base, paths);
            } else if path.extension().is_some_and(|extension| extension == "md") {
                paths.insert(
                    path.strip_prefix(base)
                        .expect("legal template is beneath templates/")
                        .to_string_lossy()
                        .replace(std::path::MAIN_SEPARATOR, "/"),
                );
            }
        }
    }

    #[test]
    fn business_notations_have_fillable_previews_and_review_workflows() {
        assert_eq!(super::BUSINESS_NOTATIONS.len(), 9);
        let seeded = store::seed::seeded_template_codes().expect("seed catalog");
        for (slug, _, path, _, source) in super::BUSINESS_NOTATIONS {
            let code = slug.replace('-', "_");
            assert!(
                seeded.contains(&code),
                "{code} must be available on a matter"
            );
            let doc = super::preview_doc(slug, path, source);
            assert!(
                !doc.demo_questions.is_empty(),
                "{slug} has no questionnaire"
            );
            assert!(
                doc.demo_questions.iter().any(|q| q.interactive),
                "{slug} has no fillable entries"
            );
            let metadata: serde_yaml::Value =
                serde_yaml::from_str(&doc.frontmatter).expect("frontmatter");
            assert_eq!(
                metadata["workflow"]["BEGIN"]["_"].as_str(),
                Some("lawyer_review")
            );
            assert_eq!(
                metadata["workflow"]["lawyer_review"]["approved"].as_str(),
                Some("END")
            );
            let composed = rules::clauses::compile(source).expect("catalog template composes");
            let (_, body) = composed
                .strip_prefix("---\n")
                .expect("frontmatter")
                .split_once("\n---\n")
                .expect("body");
            let mut answers = std::collections::BTreeMap::new();
            for question in &doc.demo_questions {
                let value = match question.answer_type.as_str() {
                    "entity" | "person" => {
                        r#"{"name":"Sample party","title":"Contact","email":"sample@example.com","street":"1 Example Street","city":"Example","state":"CA","zip":"00000","country":"US"}"#
                    }
                    "people" => {
                        r#"[{"name":"Sample party","title":"Contact","email":"sample@example.com","street":"1 Example Street","city":"Example","state":"CA","zip":"00000","country":"US"}]"#
                    }
                    "custom_datetime" => "2026-09-24",
                    _ => "Sample answer",
                };
                answers.insert(question.code.clone(), value.to_string());
                answers.insert(
                    format!("{}.name", question.code),
                    "Sample party".to_string(),
                );
            }
            let filled = views::notation::fill(body, &answers);
            assert!(
                !filled.contains("{{"),
                "{slug} leaves an unbound entry: {filled}"
            );
        }
    }

    #[test]
    fn public_notations_catalog_matches_legal_templates() {
        let repository_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let templates_root = repository_root.join("templates");
        let notations_root = templates_root.join("notations");
        let mut intended_paths = BTreeSet::new();
        // These are the legal-template shelves. The GitHub shelf and prose such
        // as templates/README.md are deliberately outside this catalog.
        for shelf in ["forms", "neon_law"] {
            collect_markdown_paths(
                &notations_root.join(shelf),
                &templates_root,
                &mut intended_paths,
            );
        }

        let catalog = notations_index_content();
        let cards: Vec<String> = catalog
            .materials
            .iter()
            .map(|material| {
                material
                    .href
                    .strip_prefix("/notations/")
                    .expect("catalog card links to a notation preview")
                    .to_string()
            })
            .collect();
        let card_slugs: BTreeSet<&str> = cards.iter().map(String::as_str).collect();
        assert_eq!(
            card_slugs.len(),
            cards.len(),
            "catalog contains a duplicate card"
        );

        let previews = notation_preview_docs();
        let preview_slugs: Vec<&str> = previews
            .iter()
            .map(|preview| preview.slug.as_str())
            .collect();
        let unique_preview_slugs: BTreeSet<&str> = preview_slugs.iter().copied().collect();
        assert_eq!(
            unique_preview_slugs.len(),
            preview_slugs.len(),
            "preview catalog contains a duplicate slug"
        );
        assert_eq!(
            card_slugs, unique_preview_slugs,
            "every catalog card must have exactly one matching preview"
        );

        let preview_paths: Vec<String> = previews
            .iter()
            .map(|preview| {
                preview
                    .source_href
                    .strip_prefix(NOTATIONS_BLOB_BASE)
                    .expect("preview source points at the Navigator template repository")
                    .to_string()
            })
            .collect();
        let unique_preview_paths: BTreeSet<&str> =
            preview_paths.iter().map(String::as_str).collect();
        assert_eq!(
            unique_preview_paths.len(),
            preview_paths.len(),
            "preview catalog contains a duplicate source path"
        );
        assert_eq!(
            intended_paths,
            preview_paths.iter().cloned().collect(),
            "the public catalog must contain every legal template exactly once"
        );

        for preview in previews {
            let source_path = preview
                .source_href
                .strip_prefix(NOTATIONS_BLOB_BASE)
                .expect("preview source points at the Navigator template repository");
            let source = fs::read_to_string(templates_root.join(source_path))
                .expect("preview source path exists");
            let composed = rules::clauses::compile(&source).expect("catalog template composes");
            let document = views::harvard_outline::parse(&composed);
            assert_eq!(
                preview.title, document.title,
                "preview title must come from {source_path}"
            );
            assert_eq!(
                preview.frontmatter,
                document.frontmatter.clone().unwrap_or_default(),
                "preview frontmatter must come from {source_path}"
            );
            assert_eq!(
                preview.stage_html,
                views::harvard_outline::stage_html(&document),
                "preview body must come from {source_path}"
            );
        }
    }

    /// LAW-53: every catalog card's `kind` facet must come from the
    /// template's own frontmatter, not a hand-typed eyebrow. Before this
    /// grounding, `nevada-witness-affidavit` and two other `kind: pleading`
    /// fixtures carried a `"Filing"` eyebrow that had drifted from the
    /// template's real declared kind — this pins the fix and guards the
    /// regression.
    #[test]
    fn every_catalog_card_derives_its_kind_from_the_templates_own_frontmatter() {
        let catalog = notations_index_content();
        for material in &catalog.materials {
            assert!(
                !material.kind.is_empty(),
                "{} has no derived kind",
                material.href
            );
            assert!(
                !material.category_slug.is_empty(),
                "{} has no derived category",
                material.href
            );
        }
        let by_href = |href: &str| {
            catalog
                .materials
                .iter()
                .find(|m| m.href == href)
                .unwrap_or_else(|| panic!("no card for {href}"))
        };
        assert_eq!(by_href("/notations/onboarding-letter").kind, "onboarding");
        assert_eq!(by_href("/notations/offboarding-letter").kind, "offboarding");
        assert_eq!(
            by_href("/notations/nevada-rescission-notice").kind,
            "letter"
        );
        // A witness affidavit, an answer to a counterclaim, and a summons are
        // `kind: pleading`, never `kind: filing`.
        for href in [
            "/notations/nevada-witness-affidavit",
            "/notations/nevada-answer-to-counterclaim",
            "/notations/nevada-summons",
        ] {
            assert_eq!(by_href(href).kind, "pleading", "{href}");
            assert_eq!(by_href(href).category_slug, "court-paper", "{href}");
        }
        assert_eq!(by_href("/notations/nevada-llc-formation").kind, "filing");
        assert_eq!(
            by_href("/notations/nevada-llc-formation").category_slug,
            "filing"
        );
    }

    /// The kind catalog (LAW-53) mirrors `views::kind_catalog::entries()`
    /// exactly — every kind `S103` accepts, with none dropped or invented.
    #[test]
    fn the_kind_catalog_matches_the_shared_source() {
        let catalog = notations_index_content();
        let expected = views::kind_catalog::entries();
        assert_eq!(catalog.kinds.len(), expected.len());
        let agreement = catalog
            .kinds
            .iter()
            .find(|entry| entry.kind == "agreement")
            .expect("agreement is in the kind catalog");
        assert_eq!(agreement.category_slug, "instrument");
        assert!(agreement.rules.iter().any(|rule| rule.code == "N123"));
    }

    /// The naturalization form declares a branching `workflow:` block
    /// (`lawyer_review` alone has `approved`/`rejected`) — grounds that
    /// `demo_workflow` actually reaches the page rather than defaulting to
    /// empty, the same way `demo_questions` already does for the
    /// questionnaire section.
    #[test]
    fn the_naturalization_form_carries_its_declared_workflow_states() {
        let previews = notation_preview_docs();
        let naturalization = previews
            .iter()
            .find(|preview| preview.slug == "application-for-naturalization")
            .expect("the naturalization form is in the public catalog");
        let names: BTreeSet<&str> = naturalization
            .demo_workflow
            .iter()
            .map(|state| state.name.as_str())
            .collect();
        assert!(names.contains("lawyer_review"), "{names:?}");
        assert!(names.contains("BEGIN"), "{names:?}");
    }
}
