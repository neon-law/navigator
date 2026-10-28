// Brand-name prose (NeonLaw) trips clippy::doc_markdown; the brand
// names are not code identifiers. Same precedent as
// views/src/components.rs.
#![allow(clippy::doc_markdown)]

//! Site brand: the strings and links that identify the product to
//! the visitor (name, copyright owner, nav targets).
//!
//! [`FIRM_BRAND`] reads whichever [`Branding`] is scoped to the current
//! request. Each page takes it through `PageLayout::with_brand`; the layout
//! never branches on the URL. What varies the scoped value is the request's
//! resolved [`BrandKey`]: one running deployment can serve more than one
//! house brand, each with its own registered hosts and its own `Branding`,
//! chosen by [`BrandKey::resolve_branding`] and scoped by the caller for the
//! life of the request.
//!
//! Branding defaults to Neon Law's complete in-repository identity. A web
//! deployment can load a validated [`crate::brand_bundle::BrandManifest`]
//! once and scope the resulting immutable [`Branding`] to each request. No
//! brand field is read from or written to process environment.

use std::env;
use std::future::Future;
use std::ops::Deref;
use std::sync::{LazyLock, OnceLock};

use crate::brand_bundle::BrandManifest;

pub use crate::brand_presentation::{
    derive_scheme, font_face_for, font_stack_for, palette_by_id, resolve_presentation,
    tokens_stylesheet, tokens_stylesheet_from_hex, typeface_by_id, DerivedScheme, Palette,
    PaletteScheme, Typeface, PALETTE, TYPEFACES,
};

/// Bundle of strings + nav links that identify the running site.
///
/// `Copy` is preserved so the layout's `with_brand(SiteBrand)` API
/// continues to take the brand by value without a clone.
#[derive(Debug, Clone, Copy)]
pub struct SiteBrand {
    pub site_name: &'static str,
    pub tagline: &'static str,
    /// Landing path the navbar brand (logo + wordmark) links back to.
    pub home_href: &'static str,
    /// One-line postal address rendered in the footer. A mounted bundle lets a
    /// downstream deployment supply its own registered address.
    pub postal_address: &'static str,
    /// Path to the header brand mark served under `/public/`.
    ///
    /// `SiteHeader` renders it through `<img src=…>` at 32 CSS px, which
    /// constrains the asset in two ways. An SVG here runs in the browser's
    /// **secure static mode**, so it must be wholly self-contained: it cannot
    /// fetch an external `<image>`, stylesheet, or font, and it cannot inherit
    /// `currentColor` from the page. A raster here must be sized for the
    /// header rather than for print, since every visitor downloads it. The
    /// built-in brand supplies a self-contained vector whose colours are
    /// written into the file.
    pub logo_href: &'static str,
    /// Path to a **raster** (PNG) brand mark served under `/public/`,
    /// used as the Open Graph / Twitter Card `og:image`. Social-share
    /// scrapers (iMessage, Slack, Facebook, X, LinkedIn) generally
    /// won't rasterize SVG, so the share card needs its own PNG. It is also
    /// the full-resolution mark, where [`logo_href`] is header-sized.
    pub social_image: &'static str,
    pub nav: &'static [NavLink],
    /// When true, the layout renders firm-only portal links. A brand that does
    /// not practise law leaves this false. The legal-advice disclaimer is not
    /// gated here — the footer always shows the firm's via
    /// [`firm_disclaimer`].
    pub is_law_firm: bool,
    /// The legal person the footer's copyright names — the entity that owns
    /// the site and renders the firm's legal services. A configurable identity
    /// so a rename is a manifest edit, not a code change. Empty for a non-firm
    /// brand, which falls back to noticing its own wordmark. Sourced from
    /// `brand.firm_legal_entity` in the manifest;
    /// defaults to today's value.
    ///
    /// Kept in step with `store::seed::FIRM_ENTITY_NAME`, the firm Entity row
    /// the application refuses to delete.
    pub legal_entity: &'static str,
}

/// One header nav entry. A `NavLink` is either a leaf (no children)
/// or a dropdown (children populate a Pico `<details class="dropdown">`).
#[derive(Debug, Clone, Copy)]
pub struct NavLink {
    pub label: &'static str,
    pub href: &'static str,
    /// Optional sub-items. Empty slice means a plain leaf link.
    pub children: &'static [NavLink],
    /// Optional Bootstrap Icon name (the part after `bi-`, e.g.
    /// `"star-fill"`) shown before the label. `None` renders no icon.
    /// Used to denote each product in the Services dropdown.
    pub icon: Option<&'static str>,
}

impl NavLink {
    #[must_use]
    pub const fn leaf(label: &'static str, href: &'static str) -> Self {
        Self {
            label,
            href,
            children: &[],
            icon: None,
        }
    }

    /// A leaf link prefixed with a Bootstrap Icon. `icon` is the glyph
    /// name without the `bi-` prefix (e.g. `"shield-fill-check"`).
    #[must_use]
    pub const fn leaf_with_icon(
        label: &'static str,
        href: &'static str,
        icon: &'static str,
    ) -> Self {
        Self {
            label,
            href,
            children: &[],
            icon: Some(icon),
        }
    }

    #[must_use]
    pub const fn dropdown(label: &'static str, children: &'static [NavLink]) -> Self {
        Self {
            label,
            href: "#",
            children,
            icon: None,
        }
    }

    #[must_use]
    pub const fn is_dropdown(&self) -> bool {
        !self.children.is_empty()
    }
}

/// The fee schedule, then the next step is a call.
pub const SHOOK_CONSULTATION_URL: &str = "https://calendar.notion.so/meet/nick-shook/or15n4yy7";

const FIRM_NAV: &[NavLink] = &[
    NavLink::leaf("Services", "/services"),
    NavLink::leaf("Book Consultation", SHOOK_CONSULTATION_URL),
];

/// Neon's own header adds Nautilus, the subscription only Neon publishes at
/// `/nautilus`; every other brand host answers that path with a 404.
const NEON_NAV: &[NavLink] = &[
    NavLink::leaf("Services", "/services"),
    NavLink::leaf("Nautilus", "/nautilus"),
    NavLink::leaf("Book Consultation", SHOOK_CONSULTATION_URL),
];

/// Public footer pages. `firm_footer_nav` adds Contact, Testimonials, Privacy,
/// and Terms, and resolves shared pages to their host for each Brand.
const FIRM_FOOTER_NAV: &[NavLink] = &[
    NavLink::leaf("API", "/api"),
    NavLink::leaf("Blog", "/blog"),
    NavLink::leaf("Design", "/design"),
    NavLink::leaf("Glossary", "/glossary"),
    NavLink::leaf("Navigator", "/navigator"),
    NavLink::leaf("Notations", "/notations"),
    NavLink::leaf("Presentations", "/presentations"),
    NavLink::leaf("Team", "/team"),
];

/// One bar license a named attorney holds: the jurisdiction, the number that
/// jurisdiction publishes them under, and the public record it can be checked
/// against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarLicense {
    pub jurisdiction: &'static str,
    pub number: &'static str,
    pub license_url: &'static str,
}

/// One licensed attorney and the set of bar licenses they hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirmAttorney {
    pub name: &'static str,
    pub licenses: &'static [BarLicense],
}

/// The attorneys whose bar licences the public footer discloses, one line each
/// with every number linked to that bar's own record.
///
/// Empty on this deployment: the firm publishes no per-attorney bar disclosure
/// in its footer, and no bar number anywhere on the site. `/team` still names
/// the jurisdictions each attorney is licensed in, as a credential chip on
/// their profile — see [`crate::team`] — which is where a reader choosing a
/// lawyer looks for it.
///
/// The seam stays live for the deployments that do publish one — a white-label
/// bundle sets `brand.firm_attorneys` and its footer renders the list (see
/// [`crate::brand_bundle`]). Any entry must give the number its own bar
/// publishes and that bar's own record as the URL, so the two can never drift
/// apart unnoticed, and the full legal name rather than a display name: a bar
/// record is searched under the name the licensing jurisdiction registered.
const FIRM_ATTORNEYS: &[FirmAttorney] = &[];

/// One published office: the city a visitor recognizes it by and the street
/// address they would mail or walk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirmOffice {
    pub state: &'static str,
    pub address: &'static str,
    /// A qualification the office cannot be published without — today, whether
    /// an attorney is actually admitted in that jurisdiction yet. Regulated
    /// copy, not decoration: listing an address in a state reads as a holding
    /// out to practise there, so a pending admission must travel with the
    /// address rather than sit in a legend somewhere else on the page.
    /// `None` publishes the address unqualified.
    pub note: Option<&'static str>,
}

/// The firm's published offices, rendered in the public footer.
///
/// This is Guieb & Shook PC's address — the entity of record behind the Neon Law
/// mark, the legal person a client actually engages — kept in step with its row
/// in `store/seeds/neon/Address.yaml`.
///
/// California, Nevada, New York, and Washington offices are shared by every
/// registered site.
///
/// Still a separate field from [`SiteBrand::postal_address`], which is the one
/// registered address the letterhead carries, even though both name the same
/// Reno box today. Within the Ridgeview Mail Center the box number is the whole
/// address, so a wrong suffix misdelivers to another entity of ours rather than
/// bouncing — `405-9002` is the firm's.
///
/// Each office is published under its state rather than its city, so the footer
/// reads as the map of where the firm practises, and the list is ordered
/// alphabetically by that state so a new office slots in by where it sits
/// rather than by whoever edited the list last. The street address underneath
/// still names the city.
///
/// Every comma is a line break. The footer sets an address the way an envelope
/// carries it — street, then unit, then city — so the suite gets its own line
/// and the city starts one, rather than the whole address running together and
/// wrapping wherever the column ends. The city keeps its state and ZIP on the
/// same line. See `webapp::components::site_footer`, which does the splitting.
const FIRM_OFFICES: &[FirmOffice] = &[
    FirmOffice {
        state: "California",
        address: "1515 20th Street, Bakersfield, CA 93301",
        note: None,
    },
    FirmOffice {
        state: "Nevada",
        address: "5150 Mae Anne Ave, Ste 405-9002, Reno, NV 89523",
        note: None,
    },
    FirmOffice {
        state: "New York",
        address: "12 E 49th St, 18th Floor, New York, NY 10017",
        note: None,
    },
    FirmOffice {
        state: "Washington",
        address: "1111 Main St, STE 201E, Vancouver, WA 98660",
        note: None,
    },
];

/// One professional association the firm has a public affiliation with,
/// published in the footer of every page: the association's name as it writes
/// it, the firm's standing, its own site, and its mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirmMembership {
    pub name: &'static str,
    /// The full standing sentence, minus its trailing period: the firm's
    /// association-specific standing and the association's name.
    pub standing: &'static str,
    pub href: &'static str,
    /// The association's own mark, as a repo-relative key in the deployment's
    /// public assets bucket (`img/<slug>/<file>`) — never a root-relative
    /// path. It resolves through [`crate::assets::asset_url`], so a cloud
    /// deployment serves it from its own `<project>-assets` bucket while
    /// dev, tests, and a fork with no bucket configured fall back to the
    /// crate-bundled `/public` mount. That is the seam every other photo
    /// rides, and it is why these bytes are not in git.
    ///
    /// Rendered decoratively (`alt=""`) beside the line that names the
    /// association, because the text is the label. Empty renders no badge.
    pub logo_key: &'static str,
}

/// One verified public social profile linked under the request Brand's footer
/// wordmark. House Brands share the firm's `YouTube` channel; other destinations
/// belong only to the Brand that carries them. Renamed firms inherit none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirmSocial {
    /// Provider name; networks without a built-in mark render their visible label.
    pub network: &'static str,
    pub label: &'static str,
    pub href: &'static str,
}

const YOUTUBE_SOCIAL: FirmSocial = FirmSocial {
    network: "youtube",
    label: "YouTube",
    href: "https://www.youtube.com/@neon-law-firm",
};

/// The shared channel is the only destination inherited across house Brands.
const SHARED_SOCIAL: &[FirmSocial] = &[YOUTUBE_SOCIAL];

/// Vesta's public profiles, verified during account onboarding. These links
/// advertise the profiles; they neither require nor authorize API publishing.
const VESTA_SOCIAL: &[FirmSocial] = &[
    FirmSocial {
        network: "facebook",
        label: "Facebook",
        href: "https://www.facebook.com/people/Vesta-Estate-Planning/61594889293557/",
    },
    FirmSocial {
        network: "instagram",
        label: "Instagram",
        href: "https://www.instagram.com/vesta.estate.planning/",
    },
    FirmSocial {
        network: "threads",
        label: "Threads",
        href: "https://www.threads.com/@vesta.estate.planning",
    },
    FirmSocial {
        network: "tiktok",
        label: "TikTok",
        href: "https://www.tiktok.com/@vesta.estate.planning",
    },
    YOUTUBE_SOCIAL,
];

/// Neon Law's own profiles, in the order its footer shows them.
const FIRM_SOCIAL: &[FirmSocial] = &[
    FirmSocial {
        network: "x",
        label: "X",
        href: "https://x.com/NeonLawUSA",
    },
    FirmSocial {
        network: "linkedin",
        label: "LinkedIn",
        href: "https://www.linkedin.com/company/neon-law-usa",
    },
    YOUTUBE_SOCIAL,
];

/// The associations the firm has a public affiliation with. The footer
/// renders each association's standing line, linking the association's own
/// site so a reader can check the relationship against the association's
/// published guidance rather than trusting this page's word for it.
///
/// An affiliation belongs to the firm, not to a brand: Guieb & Shook PC is the
/// organization represented here, so every house brand it trades under carries
/// the same line. A bundle that renames the firm publishes none — see
/// [`Branding::firm_memberships`].
///
/// The firm's standing with the Justice Technology Association — the
/// footer's link text beside the association's mark, which names the
/// association itself. The one source every render and every test derives
/// from, so a copy change touches this line alone.
pub const JTA_STANDING: &str = "Mission-Aligned Partner";

const FIRM_MEMBERSHIPS: &[FirmMembership] = &[FirmMembership {
    name: "Justice Technology Association",
    standing: JTA_STANDING,
    href: "https://justicetechassociation.org/get-involved",
    // The association's own mark, as it publishes it. A bucket key,
    // resolved through the asset seam per deployment.
    logo_key: "img/justice-technology-association/logo.png",
}];

/// All identity consumed by rendering. The web router scopes one immutable
/// instance to a request; direct view tests receive [`DEFAULT_BRANDING`].
#[derive(Debug, Clone, Copy)]
pub struct Branding {
    pub firm: SiteBrand,
    pub firm_email: &'static str,
    /// The host the firm's support address is built on — the `HOST` in
    /// `support@{HOST}`. Set this and the firm's inbound address follows; set
    /// `firm_email` (manifest `brand.support_email`) to name an address that
    /// is not `support@` at all. Deliberately separate from
    /// [`Branding::primary_domain`], which is the infrastructure apex the
    /// deployment derives service hostnames from (`workflows.<apex>`) — moving
    /// the mailbox must not move the cluster.
    pub support_domain: &'static str,
    /// The firm's published voice line, rendered on `/contact` and in the
    /// public footer, dialled from the `tel:` link at either. Brand identity
    /// like the postal address: a deployment that mounts its own manifest
    /// publishes its own number.
    pub firm_phone: &'static str,
    /// Every office the firm publishes, rendered in the public footer. See
    /// [`FIRM_OFFICES`] for why this is not [`SiteBrand::postal_address`].
    pub firm_offices: &'static [FirmOffice],
    /// The firm's attorneys and their bar licenses, rendered in the public
    /// footer. See [`FIRM_ATTORNEYS`]: this is the footer's only bar
    /// disclosure, and it names who holds each licence.
    pub firm_attorneys: &'static [FirmAttorney],
    /// The associations the firm has a public affiliation with, rendered as
    /// the footer's standing line on every page. See [`FIRM_MEMBERSHIPS`]. Empty on a
    /// bundle that renames the firm, for the same reason the trademark
    /// notice is: the affiliation is Guieb & Shook PC's, and a renamed firm is a
    /// different firm whose affiliations this repository cannot vouch for.
    pub firm_memberships: &'static [FirmMembership],
    /// This Brand's verified public profiles and the shared `YouTube` channel.
    /// Empty on a bundle that renames the firm.
    pub firm_social: &'static [FirmSocial],
    /// The house brands the firm trades under — the footer's "Our Family"
    /// row, in registry order. [`BrandKey::ALL`] on every house brand, so a
    /// reader on any one of them sees the other two; empty on a bundle that
    /// renames the firm, whose footer must not list another firm's brands.
    /// The live `firm_brand` rows override this when a Firm wears the
    /// request's key (`webapp::firm_footer`); this is what renders before
    /// any Firm is seeded, so the row is on every page from first boot.
    pub firm_family: &'static [BrandKey],
    /// The firm's registered word mark, spelled the way the register spells it
    /// — `NEON LAW`, not the title-case wordmark the header wears. The footer's
    /// trademark notice opens on it, followed by `®`.
    pub firm_trademark: &'static str,
    /// The U.S. registration number for [`Branding::firm_trademark`], written
    /// the way the certificate writes it (`6,325,650`).
    ///
    /// The registrant the notice names is [`SiteBrand::legal_entity`], which is
    /// the same legal person on the firm's deploy. Those two must move together:
    /// a notice that cites this number and names anyone else states the wrong
    /// owner of a live registration, and `cli/tests/license_of_record.rs`
    /// fails the build over exactly that on every other surface that cites it.
    pub firm_trademark_registration: &'static str,
    /// The register's own record for that registration, so a reader verifies the
    /// claim against the USPTO rather than trusting the site's line about
    /// itself — the same rule the footer's bar-licence rows already follow.
    pub firm_trademark_record_url: &'static str,
    pub consultation_url: &'static str,
    pub terms_url: &'static str,
    pub privacy_url: &'static str,
    pub base_url: &'static str,
    pub primary_domain: &'static str,
    pub firm_disclaimer: &'static str,
    pub mission_description: &'static str,
    pub service_description: &'static str,
    pub portal_only: bool,
    /// The registry key this `Branding` renders under. A mounted white-label
    /// manifest always carries [`BrandKey::Neon`] — it replaces the default
    /// brand's identity, never its registry slot — so this field lets a
    /// caller holding only the scoped `Branding` (not the key that resolved
    /// it) recover which per-brand assets to link, such as the tokens
    /// stylesheet [`crate::brand`]'s scoped accessor resolves by key.
    pub brand_key: BrandKey,
}

pub static DEFAULT_BRANDING: Branding = Branding {
    firm: SiteBrand {
        // The firm signs its front door with the mark, not with a lawyer's
        // name: the header, the hero, the page titles, and the `og:site_name`
        // card all read "Neon Law". That is the whole point of the mark — the
        // practice is bigger than any single name on the door, and a client
        // hires the firm rather than whoever happens to be listed first.
        //
        // Deliberately NOT [`SiteBrand::legal_entity`], which is Guieb & Shook
        // PC. One is the signature the site is presented under; the other is
        // the legal person the footer's copyright and the retainer must name.
        // A law firm may trade under a mark, but it may not obscure who the
        // client is actually engaging, which is why both are published.
        site_name: "Neon Law",
        home_href: "/",
        tagline: "Flat-fee legal services for emerging technology companies, with every price on the page.",
        // Guieb & Shook PC's own box, the entity of record.
        postal_address: "5150 Mae Anne Ave Ste 405-9002, Reno, NV 89523",
        // The NL mark, in teal.
        //
        // Drawn geometry, so it is a true vector with every colour written
        // into the file. That matters because `SiteHeader` renders it through
        // `<img src=…>`, which puts an SVG in the browser's secure static
        // mode: every external reference inside the document is blocked, so an
        // SVG that merely wraps an external PNG paints nothing at all. This
        // one references nothing.
        logo_href: "/public/logo.svg",
        social_image: "/public/logo.png",
        nav: NEON_NAV,
        is_law_firm: true,
        // The professional corporation a client engages and the entity that renders the
        // legal services, so it is what the footer's copyright names — and,
        // matching it, what `store::seed::FIRM_ENTITY_NAME` protects as the
        // firm's Entity row. Never equal to `site_name` here: the site trades
        // as Neon Law, and a copyright notice has to name a legal person.
        legal_entity: "Guieb & Shook PC",
    },
    // The address the public site publishes, on the host `support_domain` names
    // below. The local part is `contact@`, not `support@`, which is the second
    // dial `firm_support_email` documents — the firm invites new matters to a
    // mailbox named for what a visitor is doing, not for a help desk.
    //
    // This is the *published* address only. Navigator's outbound `From` and the
    // inbound threading address are `workflows::email::DEFAULT_FROM_EMAIL`, a
    // separate constant that stays `support@`: moving what the site advertises
    // must not re-point the mail pipeline.
    firm_email: "contact@neonlaw.com",
    support_domain: "neonlaw.com",
    firm_phone: "+1 510 800 2090",
    firm_offices: FIRM_OFFICES,
    firm_attorneys: FIRM_ATTORNEYS,
    firm_memberships: FIRM_MEMBERSHIPS,
    firm_social: FIRM_SOCIAL,
    firm_family: BrandKey::ALL,
    // The mark as registered: the register carries the word mark in capitals,
    // and a notice that cites a registration should spell the mark the way the
    // registration does. `site_name` above is the same mark set the way the
    // firm signs its door.
    firm_trademark: "NEON LAW",
    firm_trademark_registration: "6,325,650",
    // Serial 90039224 is the file this registration issued from, and the search
    // record is the public page that resolves it. The registration number is
    // what the notice claims; this is where a reader checks it.
    firm_trademark_record_url: "https://tmsearch.uspto.gov/search/search-results/90039224",
    consultation_url: SHOOK_CONSULTATION_URL,
    terms_url: "/terms",
    privacy_url: "/privacy",
    base_url: "",
    primary_domain: "neonlaw.com",
    firm_disclaimer: "Attorney advertisement. Nothing here is legal advice without a signed retainer for an active project. Past results do not guarantee future outcomes.",
    mission_description: "How Neon Law makes routine legal services affordable without sacrificing correctness, and what a licensed attorney in the loop actually buys you.",
    service_description: "Flat-fee legal services from Neon Law, with every price published.",
    portal_only: false,
    brand_key: BrandKey::Neon,
};

/// The `delete-your-data` house brand. Page copy lives in
/// `neon/locales/en/delete-your-data/`; the three strings here are the chrome
/// and crawler descriptions for the same practice. Trademark fields stay
/// empty until that brand's registration status is decided.
pub static DELETE_YOUR_DATA_BRANDING: Branding = Branding {
    firm: SiteBrand {
        site_name: "DeleteYourData.com",
        home_href: "/",
        tagline: "A licensed attorney helps you ask companies to delete personal data.",
        postal_address: "5150 Mae Anne Ave Ste 405-9002, Reno, NV 89523",
        logo_href: "/public/brand/delete-your-data/logo.svg",
        social_image: "/public/brand/delete-your-data/logo.png",
        nav: &[],
        is_law_firm: true,
        legal_entity: "Guieb & Shook PC",
    },
    firm_email: "contact@deleteyourdata.com",
    support_domain: "deleteyourdata.com",
    firm_phone: "+1 510 800 2090",
    // The firm's one office, on this practice's pages as on the firm's own:
    // the address is Guieb & Shook PC's, and a reader on any house brand is
    // reading the same firm.
    firm_offices: FIRM_OFFICES,
    firm_attorneys: &[],
    // The affiliated organization is Guieb & Shook PC and the family is its
    // brands, on this practice's pages as on the firm's own.
    firm_memberships: FIRM_MEMBERSHIPS,
    firm_social: SHARED_SOCIAL,
    firm_family: BrandKey::ALL,
    firm_trademark: "",
    firm_trademark_registration: "",
    firm_trademark_record_url: "",
    consultation_url: SHOOK_CONSULTATION_URL,
    terms_url: "/terms",
    privacy_url: "/privacy",
    base_url: "",
    primary_domain: "deleteyourdata.com",
    firm_disclaimer: "DeleteYourData.com is a non-legal service. Enrollment does not create an attorney-client relationship. Legal advice and representation require a separate engagement. Data removal does not guarantee compliance or prevent a future leak.",
    mission_description: "DeleteYourData.com is a non-legal service that helps you ask companies to delete personal data they hold about you. $29/mo or $290/year prepaid for data removal, privacy protection, and basic form filings such as an LLC. Government fees are separate. Enrollment does not create an attorney-client relationship. Gift cards are available. Privacy and credit monitoring are opt-in. Data removal does not guarantee compliance or prevent a future leak.",
    service_description: "Non-legal data removal and privacy protection from DeleteYourData.com, plus basic form filings such as forming an LLC. $29/mo or $290/year prepaid. Government fees are separate. Legal representation beyond those filings requires a separate engagement. Gift cards are available. Privacy and credit monitoring are opt-in.",
    portal_only: false,
    brand_key: BrandKey::DeleteYourData,
};

/// The Lawyer Shook house brand: a bare holding page for Guieb & Shook PC, not
/// an active marketing site. `/` renders only the firm's name, one statement
/// of what it is, and a sign-in line for an existing client — see
/// `neon::firm_pages::lawyer_shook_holding_content`. Its web face is still
/// selected by the per-brand asset head and token stylesheet (Tinos), and
/// `neon/locales/en/lawyer-shook/services.yaml` remains its `/services`
/// catalog even though that route is gated off, since the page carries no
/// per-brand marketing prose to hand-write in Rust.
pub static LAWYER_SHOOK_BRANDING: Branding = Branding {
    firm: SiteBrand {
        site_name: "Lawyer Shook",
        home_href: "/",
        tagline: "The Guieb & Shook PC holding page.",
        postal_address: "5150 Mae Anne Ave Ste 405-9002, Reno, NV 89523",
        logo_href: "/public/brand/lawyer-shook/logo.svg",
        social_image: "/public/brand/lawyer-shook/logo.png",
        nav: &[],
        is_law_firm: true,
        legal_entity: "Guieb & Shook PC",
    },
    firm_email: "contact@lawyershook.com",
    support_domain: "lawyershook.com",
    firm_phone: "+1 510 800 2090",
    firm_offices: FIRM_OFFICES,
    firm_attorneys: &[],
    firm_memberships: FIRM_MEMBERSHIPS,
    firm_social: SHARED_SOCIAL,
    firm_family: BrandKey::ALL,
    firm_trademark: "",
    firm_trademark_registration: "",
    firm_trademark_record_url: "",
    consultation_url: SHOOK_CONSULTATION_URL,
    terms_url: "/terms",
    privacy_url: "/privacy",
    base_url: "",
    primary_domain: "lawyershook.com",
    firm_disclaimer: "Attorney advertisement. Nothing here is legal advice without a signed retainer for an active project. Past results do not guarantee future outcomes.",
    // Unused by the rendered page (`neon::firm_pages::lawyer_shook_holding_content`
    // is the single source of that statement) and by `llms_txt`, which reuses
    // that same function rather than reading these two fields. Kept accurate
    // anyway, since a `Branding` value should not carry a stale claim.
    mission_description: "Lawyer Shook is the holding page for Guieb & Shook PC's LAWYER SHOOK mark, not an active practice. Guieb & Shook PC is an American law firm. We build brands that put the customer not the lawyer first.",
    service_description: "Lawyer Shook publishes no services or fee schedule here; it is a holding page for Guieb & Shook PC only.",
    portal_only: false,
    brand_key: BrandKey::LawyerShook,
};

/// The Guieb & Shook house brand: a bare holding page for Guieb & Shook PC,
/// the firm's name and nothing more. `/` renders the firm's name, "Attorneys
/// at Law", and a sign-in line for an existing client — see
/// `neon::firm_pages::guieb_shook_holding_content`. Its web face is selected
/// by the per-brand asset head and token stylesheet (Tinos), like Lawyer
/// Shook's.
pub static GUIEB_SHOOK_BRANDING: Branding = Branding {
    firm: SiteBrand {
        site_name: "Guieb & Shook",
        home_href: "/",
        tagline: "Attorneys at Law.",
        postal_address: "5150 Mae Anne Ave Ste 405-9002, Reno, NV 89523",
        logo_href: "/assets/brand/guieb-shook/logo.svg",
        social_image: "/assets/brand/guieb-shook/logo.png",
        nav: &[],
        is_law_firm: true,
        legal_entity: "Guieb & Shook PC",
    },
    firm_email: "contact@guiebandshook.com",
    support_domain: "guiebandshook.com",
    firm_phone: "+1 510 800 2090",
    firm_offices: FIRM_OFFICES,
    firm_attorneys: &[],
    firm_memberships: FIRM_MEMBERSHIPS,
    firm_social: SHARED_SOCIAL,
    firm_family: BrandKey::ALL,
    firm_trademark: "",
    firm_trademark_registration: "",
    firm_trademark_record_url: "",
    consultation_url: SHOOK_CONSULTATION_URL,
    terms_url: "/terms",
    privacy_url: "/privacy",
    base_url: "",
    primary_domain: "guiebandshook.com",
    firm_disclaimer: "Attorney advertisement. Nothing here is legal advice without a signed retainer for an active project. Past results do not guarantee future outcomes.",
    // Unused by the rendered page, which reads
    // `neon::firm_pages::guieb_shook_holding_content`; kept accurate anyway.
    mission_description: "Guieb & Shook PC, Attorneys at Law.",
    service_description: "Guieb & Shook publishes no services or fee schedule here; it is the holding page for Guieb & Shook PC.",
    portal_only: false,
    brand_key: BrandKey::GuiebShook,
};

/// The `vesta` house brand. Page copy lives in `neon/locales/en/vesta/`;
/// the strings here are the chrome and crawler descriptions for the same
/// practice. Trademark fields stay empty until that brand's registration
/// status is decided.
pub static VESTA_BRANDING: Branding = Branding {
    firm: SiteBrand {
        site_name: "Vesta Estate Planning",
        home_href: "/",
        tagline: "Estate plans tailored to you, with Nevada asset protection planning when it fits.",
        postal_address: "5150 Mae Anne Ave Ste 405-9002, Reno, NV 89523",
        logo_href: "/public/brand/vesta.svg",
        social_image: "/public/brand/vesta.png",
        nav: &[
            NavLink::leaf("Estates", "/services"),
            NavLink::leaf("Trust Administration", "/services#trust-administration"),
            NavLink::leaf("Probate", "/probate"),
        ],
        is_law_firm: true,
        legal_entity: "Guieb & Shook PC",
    },
    firm_email: "contact@vestaestateplanning.com",
    support_domain: "vestaestateplanning.com",
    firm_phone: "+1 510 800 2090",
    firm_offices: FIRM_OFFICES,
    firm_attorneys: &[],
    firm_memberships: FIRM_MEMBERSHIPS,
    firm_social: VESTA_SOCIAL,
    firm_family: BrandKey::ALL,
    firm_trademark: "",
    firm_trademark_registration: "",
    firm_trademark_record_url: "",
    consultation_url: SHOOK_CONSULTATION_URL,
    terms_url: "/terms",
    privacy_url: "/privacy",
    base_url: "",
    primary_domain: "vestaestateplanning.com",
    firm_disclaimer: "Attorney advertisement. Nothing here is legal advice without a signed retainer for an active project. Past results do not guarantee future outcomes.",
    mission_description: "Vesta Estate Planning is a practice of Guieb & Shook PC. We create estate plans tailored to your life and assess whether Nevada asset protection planning fits. A licensed attorney prepares and reviews the work. Trust administration has separate written terms. Attorney advertisement.",
    service_description: "Estate planning tailored to you from Vesta Estate Planning, a practice of Guieb & Shook PC. We assess whether a Nevada asset protection trust fits your circumstances and applicable law. Contact us for trust administration; scope and fees are agreed separately in writing.",
    portal_only: false,
    brand_key: BrandKey::Vesta,
};

/// The `misericordia` house brand. Page copy lives in `neon/locales/en/misericordia/`;
/// the strings here are the chrome and crawler descriptions for the same
/// practice. Trademark fields stay empty until that brand's registration
/// status is decided.
pub static MISERICORDIA_BRANDING: Branding = Branding {
    firm: SiteBrand {
        site_name: "Misericordia Injury Law",
        home_href: "/",
        tagline: "Injury claims, on a contingency fee, with a free first conversation.",
        postal_address: "5150 Mae Anne Ave Ste 405-9002, Reno, NV 89523",
        logo_href: "/public/brand/misericordia/logo.svg",
        social_image: "",
        nav: &[],
        is_law_firm: true,
        legal_entity: "Guieb & Shook PC",
    },
    firm_email: "contact@misericordialaw.com",
    support_domain: "misericordialaw.com",
    firm_phone: "+1 510 800 2090",
    firm_offices: FIRM_OFFICES,
    firm_attorneys: &[],
    firm_memberships: FIRM_MEMBERSHIPS,
    firm_social: SHARED_SOCIAL,
    firm_family: BrandKey::ALL,
    firm_trademark: "",
    firm_trademark_registration: "",
    firm_trademark_record_url: "",
    consultation_url: SHOOK_CONSULTATION_URL,
    terms_url: "/terms",
    privacy_url: "/privacy",
    base_url: "",
    primary_domain: "misericordialaw.com",
    firm_disclaimer: "Attorney advertisement. Nothing here is legal advice without a signed retainer for an active project. Past results do not guarantee future outcomes.",
    mission_description: "Misericordia Injury Law is a practice of Guieb & Shook PC. A licensed attorney handles car, truck, motorcycle, pedestrian, premises, and wrongful death claims. The first conversation is free, and the fee is a percentage of what is recovered, agreed in writing beforehand. This is an attorney advertisement, not a promise about a result.",
    service_description: "Injury claims from Misericordia Injury Law, a practice of Guieb & Shook PC. Free consultation, contingency fee agreed in writing before work begins.",
    portal_only: false,
    brand_key: BrandKey::Misericordia,
};

/// The `abhaya` house brand. Page copy lives in `neon/locales/en/abhaya/`;
/// the strings here are the chrome and crawler descriptions for the same
/// practice. Trademark fields stay empty until that brand's registration
/// status is decided.
pub static ABHAYA_BRANDING: Branding = Branding {
    firm: SiteBrand {
        site_name: "Abhaya Immigration",
        home_href: "/",
        tagline: "Immigration counsel for individuals, families, founders, and employers.",
        postal_address: "5150 Mae Anne Ave Ste 405-9002, Reno, NV 89523",
        logo_href: "/public/brand/abhaya/logo.svg",
        social_image: "",
        nav: FIRM_NAV,
        is_law_firm: true,
        legal_entity: "Guieb & Shook PC",
    },
    firm_email: "contact@abhayaimmigration.com",
    support_domain: "abhayaimmigration.com",
    firm_phone: "+1 510 800 2090",
    firm_offices: FIRM_OFFICES,
    firm_attorneys: &[],
    firm_memberships: FIRM_MEMBERSHIPS,
    firm_social: SHARED_SOCIAL,
    firm_family: BrandKey::ALL,
    firm_trademark: "",
    firm_trademark_registration: "",
    firm_trademark_record_url: "",
    consultation_url: SHOOK_CONSULTATION_URL,
    terms_url: "/terms",
    privacy_url: "/privacy",
    base_url: "",
    primary_domain: "abhayaimmigration.com",
    firm_disclaimer: "Attorney advertisement. Nothing here is legal advice without a signed retainer for an active project. Past results do not guarantee future outcomes.",
    mission_description: "Abhaya Immigration is a practice of Guieb & Shook PC. A licensed attorney handles work visas, family petitions, green cards, naturalization, consular processing, employer sponsorship, and USCIS notice responses for a flat fee quoted before work begins. Government filing fees are separate. We cannot promise approval or a government timeline; the relevant agency decides. This is an attorney advertisement.",
    service_description: "Immigration work from Abhaya Immigration, a practice of Guieb & Shook PC. Flat fees quoted before work begins, with USCIS filing fees separate and government-set.",
    portal_only: false,
    brand_key: BrandKey::Abhaya,
};

/// The `delete-your-debt` house brand. Page copy lives in `neon/locales/en/delete-your-debt/`;
/// the strings here are the chrome and crawler descriptions for the same
/// practice. Trademark fields stay empty until that brand's registration
/// status is decided.
pub static DELETE_YOUR_DEBT_BRANDING: Branding = Branding {
    firm: SiteBrand {
        site_name: "DeleteYourDebt.com",
        home_href: "/",
        tagline: "Work with PandA Law on debt relief in Nevada, Washington, New York, and California.",
        postal_address: "5150 Mae Anne Ave Ste 405-9002, Reno, NV 89523",
        logo_href: "/public/brand/delete-your-debt/logo.svg",
        social_image: "",
        nav: &[NavLink::leaf("How it works", "/services"), NavLink::leaf("Talk to PandA Law", "https://www.pandalawfirm.com/contact/")],
        is_law_firm: true,
        legal_entity: "Guieb & Shook PC",
    },
    firm_email: "contact@deleteyourdebt.com",
    support_domain: "deleteyourdebt.com",
    firm_phone: "+1 510 800 2090",
    firm_offices: FIRM_OFFICES,
    firm_attorneys: &[],
    firm_memberships: FIRM_MEMBERSHIPS,
    firm_social: SHARED_SOCIAL,
    firm_family: BrandKey::ALL,
    firm_trademark: "",
    firm_trademark_registration: "",
    firm_trademark_record_url: "",
    consultation_url: "https://www.pandalawfirm.com/contact/",
    terms_url: "/terms",
    privacy_url: "/privacy",
    base_url: "",
    primary_domain: "deleteyourdebt.com",
    firm_disclaimer: "Attorney advertisement. Legal services are provided by PandA Law Firm. Nothing here is legal advice without a signed engagement with PandA Law Firm. Past results do not guarantee future outcomes.",
    mission_description: "DeleteYourDebt.com is operated by Guieb & Shook PC; legal services are provided by PandA Law Firm. It connects people with PandA Law for debt relief in Nevada, Washington, New York, and California. Our team works across the country, with in-person meetings in Brooklyn. Attorney advertisement. No result is promised.",
    service_description: "Work with PandA Law on debt relief in Nevada, Washington, New York, and California. Remote conversations and in-person meetings in Brooklyn. Scope and fees are agreed in writing before representation begins.",
    portal_only: false,
    brand_key: BrandKey::DeleteYourDebt,
};

/// The `summons` house brand. Page copy lives in `neon/locales/en/summons/`;
/// the strings here are the chrome and crawler descriptions for the same
/// practice. Trademark fields stay empty until that brand's registration
/// status is decided.
///
/// The public masthead and domain use the Summons Defense brand. The legal
/// entity remains Guieb & Shook PC in the legal footer and required notices.
pub static SUMMONS_BRANDING: Branding = Branding {
    firm: SiteBrand {
        site_name: "Summons Defense",
        home_href: "/",
        tagline: "A private law firm, not affiliated with the City of New York or OATH. We defend City summonses at the OATH Hearings Division.",
        postal_address: "5150 Mae Anne Ave Ste 405-9002, Reno, NV 89523",
        logo_href: "/public/brand/summons/logo.svg",
        social_image: "",
        nav: &[],
        is_law_firm: true,
        legal_entity: "Guieb & Shook PC",
    },
    firm_email: "contact@summonsdefense.nyc",
    support_domain: "summonsdefense.nyc",
    firm_phone: "+1 510 800 2090",
    firm_offices: FIRM_OFFICES,
    firm_attorneys: &[],
    firm_memberships: FIRM_MEMBERSHIPS,
    firm_social: SHARED_SOCIAL,
    firm_family: BrandKey::ALL,
    firm_trademark: "",
    firm_trademark_registration: "",
    firm_trademark_record_url: "",
    consultation_url: SHOOK_CONSULTATION_URL,
    terms_url: "/terms",
    privacy_url: "/privacy",
    base_url: "",
    primary_domain: "summonsdefense.nyc",
    firm_disclaimer: "Attorney advertisement. Nothing here is legal advice without a signed retainer for an active project. Past results do not guarantee future outcomes.",
    mission_description: "Summons Defense represents respondents at New York City's OATH Hearings Division against summonses written by City enforcement agencies. It is a private law firm and is not affiliated with the City of New York or with OATH, which runs a free Help Center at every hearing location. Local counsel appears at your hearing by phone for $100 per hearing, or in person for a one-time $500 appearance fee. This is an attorney advertisement, not a promise about a result.",
    service_description: "NYC summons defense from Summons Defense at the OATH Hearings Division. Local counsel appears by phone for $100 per hearing or in person for a one-time $500 appearance fee.",
    portal_only: false,
    brand_key: BrandKey::Summons,
};

/// The `daybridge` house brand. Page copy lives in
/// `neon/locales/en/daybridge/`.
pub static DAYBRIDGE_BRANDING: Branding = Branding {
    firm: SiteBrand {
        site_name: "Daybridge Divorce Law",
        home_href: "/",
        tagline: "A brighter way through divorce for couples who agree.",
        postal_address: "5150 Mae Anne Ave Ste 405-9002, Reno, NV 89523",
        logo_href: "/public/brand/daybridge/logo.svg",
        social_image: "",
        nav: &[],
        is_law_firm: true,
        legal_entity: "Guieb & Shook PC",
    },
    firm_email: "contact@daybridgedivorce.com",
    support_domain: "daybridgedivorce.com",
    firm_phone: "+1 510 800 2090",
    firm_offices: FIRM_OFFICES,
    firm_attorneys: &[],
    firm_memberships: FIRM_MEMBERSHIPS,
    firm_social: SHARED_SOCIAL,
    firm_family: &[
        BrandKey::Neon,
        BrandKey::DeleteYourData,
        BrandKey::DeleteYourDebt,
        BrandKey::Vesta,
        BrandKey::Misericordia,
        BrandKey::Abhaya,
        BrandKey::Summons,
        BrandKey::Daybridge,
    ],
    firm_trademark: "",
    firm_trademark_registration: "",
    firm_trademark_record_url: "",
    consultation_url: SHOOK_CONSULTATION_URL,
    terms_url: "/terms",
    privacy_url: "/privacy",
    base_url: "",
    primary_domain: "daybridgedivorce.com",
    firm_disclaimer: "Attorney advertisement. Nothing here is legal advice without a signed retainer for an active project. Past results do not guarantee future outcomes.",
    mission_description: "Daybridge Divorce Law is operated by Guieb & Shook PC for couples who agree. Joint representation is subject to applicable rules, conflict review, and informed written consent. A one-time $5,000 fee covers a mutual split within the written retainer. Contested work is outside the joint engagement. Attorney advertisement; no result is promised.",
    service_description: "Agreed divorce for couples through Daybridge Divorce Law, operated by Guieb & Shook PC. Joint representation where permitted after conflict review and informed written consent. A one-time $5,000 fee for a mutual split. The written retainer defines the work.",
    portal_only: false,
    brand_key: BrandKey::Daybridge,
};

/// The `death-and-divorce` house brand. Page copy lives in
/// `neon/locales/en/death-and-divorce/`.
pub static DEATH_AND_DIVORCE_BRANDING: Branding = Branding {
    firm: SiteBrand {
        site_name: "Death & Divorce",
        home_href: "/",
        tagline: "Divorce and probate. Care for what comes next.",
        postal_address: "5150 Mae Anne Ave Ste 405-9002, Reno, NV 89523",
        logo_href: "/public/brand/death-and-divorce/mark.svg",
        social_image: "/public/brand/death-and-divorce/mark.svg",
        nav: &[],
        is_law_firm: true,
        legal_entity: "Guieb & Shook PC",
    },
    firm_email: "contact@deathanddivorcelaw.com",
    support_domain: "deathanddivorcelaw.com",
    firm_phone: "+1 510 800 2090",
    firm_offices: FIRM_OFFICES,
    firm_attorneys: &[],
    firm_memberships: FIRM_MEMBERSHIPS,
    firm_social: SHARED_SOCIAL,
    firm_family: BrandKey::ALL,
    firm_trademark: "",
    firm_trademark_registration: "",
    firm_trademark_record_url: "",
    consultation_url: SHOOK_CONSULTATION_URL,
    terms_url: "/terms",
    privacy_url: "/privacy",
    base_url: "",
    primary_domain: "deathanddivorcelaw.com",
    firm_disclaimer: "Attorney advertisement. Nothing here is legal advice without a signed retainer for an active project. Past results do not guarantee future outcomes.",
    mission_description: "Death & Divorce is a practice of Guieb & Shook PC for divorce and probate. Estate planning is offered through Vesta Estate Planning. We help people move through endings, transitions, and what comes after. This is an attorney advertisement, not a promise about a result.",
    service_description: "Divorce and probate from Death & Divorce, a practice of Guieb & Shook PC. Scope and fees are set out in writing before work begins.",
    portal_only: false,
    brand_key: BrandKey::DeathAndDivorce,
};

/// CyberInjuryLaw uses the firm's verified contact and engagement identity.
pub const CYBER_INJURY_BRANDING: Branding = Branding {
    firm: SiteBrand {
        site_name: "CyberInjuryLaw",
        tagline: "We use AI to put more money in your pocket.",
        logo_href: "/public/brand/cyber-injury-law.svg",
        social_image: "/assets/img/cyber-injury-law/canal-street-ad-f9faa356.png",
        ..DEFAULT_BRANDING.firm
    },
    primary_domain: "cyberinjurylaw.com",
    mission_description: "Cyber Injury Law reviews car accidents, negligence, medical malpractice, data theft, cybercrime, and online abuse. This is an attorney advertisement; a lawyer must review the facts before an engagement.",
    service_description: "Personal injury and cybercrime claims from Cyber Injury Law: car accidents, negligence, medical malpractice, data theft, and online abuse.",
    firm_email: "support@cyberinjurylaw.com",
    brand_key: BrandKey::CyberInjuryLaw,
    firm_social: SHARED_SOCIAL,
    ..DEFAULT_BRANDING
};

/// The Aegis Criminal Defense house identity.
pub const AEGIS_BRANDING: Branding = Branding {
    firm: SiteBrand {
        site_name: "Aegis Criminal Defense",
        tagline: "Your rights. Your defense. Your next step.",
        logo_href: "/public/brand/aegis/mark.svg",
        social_image: "",
        nav: &[
            NavLink::leaf("Services", "/services"),
            NavLink::leaf("Explainers", "/explainers/first-steps"),
            NavLink::leaf("Contact", SHOOK_CONSULTATION_URL),
        ],
        ..DEFAULT_BRANDING.firm
    },
    primary_domain: "aegiscriminaldefense.com",
    support_domain: "aegiscriminaldefense.com",
    mission_description: "Aegis Criminal Defense is a practice of Guieb & Shook PC. Criminal defense. Scope, fees, and jurisdiction are confirmed before an engagement.",
    service_description: "Criminal defense. Scope, fees, and jurisdiction are confirmed before an engagement.",
    firm_social: SHARED_SOCIAL,
    firm_family: BrandKey::ALL,
    firm_trademark: "",
    firm_trademark_registration: "",
    firm_trademark_record_url: "",
    brand_key: BrandKey::Aegis,
    ..DEFAULT_BRANDING
};

/// The Groundwork Real Estate Law house identity.
pub const GROUNDWORK_BRANDING: Branding = Branding {
    firm: SiteBrand {
        site_name: "Groundwork Real Estate Law",
        tagline: "Clear counsel for your next property move.",
        logo_href: "/public/brand/groundwork/mark.svg",
        social_image: "",
        nav: &[
            NavLink::leaf("Services", "/services"),
            NavLink::leaf("Explainers", "/explainers/first-steps"),
            NavLink::leaf("Contact", SHOOK_CONSULTATION_URL),
        ],
        ..DEFAULT_BRANDING.firm
    },
    primary_domain: "groundworkrealestatelaw.com",
    support_domain: "groundworkrealestatelaw.com",
    mission_description: "Groundwork Real Estate Law is a practice of Guieb & Shook PC. Real estate legal services. Scope, fees, and jurisdiction are confirmed before an engagement.",
    service_description: "Real estate legal services. Scope, fees, and jurisdiction are confirmed before an engagement.",
    firm_social: SHARED_SOCIAL,
    firm_family: BrandKey::ALL,
    firm_trademark: "",
    firm_trademark_registration: "",
    firm_trademark_record_url: "",
    brand_key: BrandKey::Groundwork,
    ..DEFAULT_BRANDING
};

/// A closed key naming which house brand a request resolves to. Distinct
/// from `portal::hosting::Site`, which names the *binary*: a `BrandKey`
/// names one request's resolved identity, and one running binary can resolve
/// more than one of them. Adding a brand is a code change to this enum plus
/// [`BrandKey::hosts`] and a covering test — configuration, not a table,
/// which is the right cost for a legal identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BrandKey {
    #[default]
    Neon,
    DeleteYourData,
    LawyerShook,
    Vesta,
    Misericordia,
    Abhaya,
    DeleteYourDebt,
    /// The NYC summons / OATH practice, presented publicly as Summons Defense.
    Summons,
    /// The mutual-split divorce practice.
    Daybridge,
    /// Divorce and probate for endings and transitions.
    DeathAndDivorce,
    /// Injury claims and cybercrime, including vehicle collisions and online harm.
    CyberInjuryLaw,
    /// Criminal defense.
    Aegis,
    /// Real estate transactions and property disputes.
    Groundwork,
    /// The Guieb & Shook PC holding page.
    GuiebShook,
}

impl BrandKey {
    /// Every key the registry serves, in registry order.
    pub const ALL: &'static [Self] = &[
        Self::Neon,
        Self::DeleteYourData,
        Self::DeleteYourDebt,
        Self::Vesta,
        Self::Misericordia,
        Self::Abhaya,
        Self::LawyerShook,
        Self::Summons,
        Self::Daybridge,
        Self::DeathAndDivorce,
        Self::CyberInjuryLaw,
        Self::Aegis,
        Self::Groundwork,
        Self::GuiebShook,
    ];

    /// The compiled keys whose hosts currently serve a public site. The
    /// remaining registered keys are built and staged, but intentionally stay
    /// out of the footer's link row until their launches are approved.
    pub const LIVE: &'static [Self] = &[
        Self::Neon,
        Self::DeleteYourData,
        Self::DeleteYourDebt,
        Self::Vesta,
        Self::Misericordia,
        Self::Abhaya,
        Self::LawyerShook,
        Self::Summons,
        Self::Daybridge,
        Self::DeathAndDivorce,
        Self::CyberInjuryLaw,
        Self::Aegis,
        Self::Groundwork,
        Self::GuiebShook,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Neon => "neon",
            Self::DeleteYourData => "delete-your-data",
            Self::LawyerShook => "lawyer-shook",
            Self::Vesta => "vesta",
            Self::Misericordia => "misericordia",
            Self::Abhaya => "abhaya",
            Self::DeleteYourDebt => "delete-your-debt",
            Self::Summons => "summons",
            Self::Daybridge => "daybridge",
            Self::DeathAndDivorce => "death-and-divorce",
            Self::Aegis => "aegis",
            Self::Groundwork => "groundwork",
            Self::GuiebShook => "guieb-shook",
            Self::CyberInjuryLaw => "cyber-injury-law",
        }
    }

    /// The one-line description shown beside this brand in the footer's
    /// "Our Family" block.
    ///
    /// The short names carry no information to a cold reader — "Vesta" and
    /// "Abhaya" say nothing about wills or visas — so the family list is
    /// unusable as bare links. These lines are what make it navigable.
    ///
    /// `DeleteYourDebt` connects visitors with PandA Law's debt-relief work.
    #[must_use]
    pub const fn family_byline(self) -> &'static str {
        match self {
            Self::Neon => "flat-fee legal services for emerging tech",
            Self::DeleteYourData => "protect your personal information",
            Self::LawyerShook => "Nicholas Shook",
            Self::Vesta => "estate planning, wills, and trusts",
            Self::Misericordia => "accidents and injury claims",
            Self::Abhaya => "visas, green cards, and citizenship",
            Self::DeleteYourDebt => "debt relief with PandA Law",
            Self::Summons => "NYC summons defense at OATH hearings",
            Self::Daybridge => "a brighter way through divorce for couples who agree",
            Self::DeathAndDivorce => "divorce and probate",
            Self::Aegis => "criminal defense",
            Self::Groundwork => "real estate transactions and property disputes",
            Self::GuiebShook => "attorneys at law",
            Self::CyberInjuryLaw => "injury claims and cybercrime",
        }
    }

    /// Parse a registry key. Runtime-created keys have no compiled hosts.
    #[must_use]
    pub fn parse(key: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|item| item.as_str() == key)
    }

    /// The production home this key answers on, as an absolute URL.
    #[must_use]
    pub fn public_home_href(self) -> String {
        format!("https://{}", self.hosts()[0])
    }

    /// This key's home in the deployment named by `deployment_host`.
    ///
    /// Persistent staging runs the production runtime profile, so the
    /// deployment's public hostname — not `NAVIGATOR_ENVIRONMENT` — selects
    /// the matching sibling-brand host. Local and unconfigured callers keep
    /// the production URL.
    #[must_use]
    pub fn public_home_href_for(self, deployment_host: Option<&str>) -> String {
        let staging = deployment_host.is_some_and(|host| host.starts_with("staging."));
        let prefix = if staging { "staging." } else { "www." };
        let host = self
            .hosts()
            .iter()
            .copied()
            .find(|host| host.starts_with(prefix))
            .unwrap_or_else(|| self.canonical_host());
        format!("https://{host}")
    }

    /// English catalog stems this key ships under `locales/en/<key>/`.
    ///
    /// This is the list of *files a key ships*, not the list of paths it
    /// answers: [`Self::publishes_firm_path`] is the route gate. Neon still
    /// ships every stem in [`crate::locales::KNOWN_PAGES`] — `fractional-gc`
    /// and `litigation` remain authored, and remain the only readers of their
    /// `{shared:…}` keys, even though the firm answers neither `/business` nor
    /// `/disputes`. `services` is also authored here, and today answers
    /// `/services` too. Dropping a stem here while the file still ships would
    /// strand its shared copy, which
    /// `shared_copy_is_consumed_by_pages_or_the_exported_home_contract` fails
    /// on.
    ///
    /// A house brand ships only the stems it actually serves; a missing file is
    /// a loader-test failure, not a first-request panic. Lawyer Shook's `/` is a
    /// hardcoded bare statement rather than a YAML page (there is no marketing
    /// copy to edit), so `home` is absent from its list even though `/services`
    /// still builds from a catalog file — the route is gated off by
    /// [`Self::publishes_firm_path`], not the file removed.
    #[must_use]
    pub fn catalog_pages(self) -> &'static [&'static str] {
        match self {
            Self::Neon => crate::locales::KNOWN_PAGES,
            Self::Vesta => &["home", "services", "explainer-probate"],
            Self::Aegis | Self::Groundwork => &[
                "home",
                "services",
                "explainer-first-steps",
                "explainer-process",
                "explainer-documents",
            ],
            // Every practice brand publishes the two stems it actually
            // serves; a missing file is a loader-test failure rather than a
            // first-request panic.
            Self::CyberInjuryLaw
            | Self::LawyerShook
            | Self::GuiebShook
            | Self::DeleteYourData
            | Self::Misericordia
            | Self::Abhaya
            | Self::DeleteYourDebt
            | Self::Summons
            | Self::Daybridge
            | Self::DeathAndDivorce => &["home", "services"],
        }
    }

    /// Firm marketing paths this key answers on its own hosts.
    ///
    /// Neon answers its fee schedule at `/services`, alongside `/`;
    /// `/business` and `/disputes` remain retired and 404. DeleteYourData
    /// answers only the pages it has a catalog for (plus `/contact`, which is
    /// addresses rather than a YAML stem). Lawyer Shook keeps its holding
    /// notice and practice cards on `/`. Every brand answers `/contact/sent`
    /// for its lead form and serves its own Posts at `/blog`, the target of
    /// its footer link; other firm paths 404 on the holding host.
    #[must_use]
    pub fn publishes_firm_path(self, path: &str) -> bool {
        if path == "/probate" {
            return self == Self::Vesta;
        }
        if path == "/blog" || path.starts_with("/blog/") {
            return true;
        }
        if path.starts_with("/explainers/") {
            return self
                .explainer_pages()
                .iter()
                .any(|(route, _)| *route == path);
        }
        // Every house-brand home accepts contact requests through `/leads`.
        if path == "/contact/sent" {
            return true;
        }
        // A lead confirmed from any brand lands on its Purchases there.
        if path == "/purchases" || path.starts_with("/purchases/") {
            return true;
        }
        match self {
            Self::Neon => !matches!(path.trim_end_matches('/'), "/business" | "/disputes"),
            Self::DeleteYourData => matches!(path, "/" | "/contact" | "/testimonials"),
            Self::LawyerShook | Self::GuiebShook => matches!(path, "/" | "/testimonials"),
            Self::CyberInjuryLaw => {
                matches!(path, "/" | "/contact" | "/consultation" | "/testimonials")
            }
            Self::Summons => matches!(path, "/" | "/services" | "/contact" | "/testimonials"),
            Self::Aegis
            | Self::Groundwork
            | Self::Vesta
            | Self::Misericordia
            | Self::Abhaya
            | Self::DeleteYourDebt
            | Self::Daybridge
            | Self::DeathAndDivorce => {
                matches!(path, "/" | "/services" | "/contact" | "/testimonials")
            }
        }
    }

    /// The routes and catalog stems for this brand's plain-language explainers.
    #[must_use]
    pub fn explainer_pages(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Vesta => &[("/probate", "explainer-probate")],
            Self::Aegis | Self::Groundwork => &[
                ("/explainers/first-steps", "explainer-first-steps"),
                ("/explainers/process", "explainer-process"),
                ("/explainers/documents", "explainer-documents"),
            ],
            _ => &[],
        }
    }

    /// The naked domain this key owns, which the DNS provider 301s to its own
    /// `www` host.
    ///
    /// The apex deliberately stays off GKE Ingress and its
    /// `ManagedCertificate`: its DNS `URL` record does not resolve to the load
    /// balancer, so listing it there would leave that certificate in
    /// `Provisioning`. [`Self::canonical_host`] supplies the redirect target
    /// that DNS setup derives for this key.
    ///
    /// It is deliberately *not* in [`Self::hosts`]. A host listed there is
    /// one this key **serves**, and an apex that served the page would
    /// publish the same content at two addresses. The apex only ever
    /// redirects.
    #[must_use]
    pub const fn apex(self) -> &'static str {
        match self {
            Self::Neon => "neonlaw.com",
            Self::DeleteYourData => "deleteyourdata.com",
            Self::LawyerShook => "lawyershook.com",
            Self::Vesta => "vestaestateplanning.com",
            Self::Misericordia => "misericordialaw.com",
            Self::Abhaya => "abhayaimmigration.com",
            Self::DeleteYourDebt => "deleteyourdebt.com",
            Self::Summons => "summonsdefense.nyc",
            Self::Daybridge => "daybridgedivorce.com",
            Self::DeathAndDivorce => "deathanddivorcelaw.com",
            Self::Aegis => "aegiscriminaldefense.com",
            Self::Groundwork => "groundworkrealestatelaw.com",
            Self::GuiebShook => "guiebandshook.com",
            Self::CyberInjuryLaw => "cyberinjurylaw.com",
        }
    }

    /// Where this key's apex sends a visitor: its own production host.
    ///
    /// Its *own*, which is the whole point. A single deployment-wide
    /// canonical host would send every brand's naked domain to the firm's
    /// site, so a reader typing `vestaestateplanning.com` would land on
    /// Neon Law.
    #[must_use]
    pub fn canonical_host(self) -> &'static str {
        self.hosts()[0]
    }

    /// Whether a visitor can actually reach this brand's site.
    ///
    /// A brand is built, registered, and serving its copy from this
    /// repository long before anyone can visit it. This is the flag that
    /// separates the two.
    ///
    /// **It no longer gates the certificate.** Before ENG-808, `cli::devx::ship`
    /// derived a `ManagedCertificate` and an Ingress host per *live* family
    /// from this registry, so a held-out brand had no TLS at all until its
    /// launch. That coupled two decisions that do not belong together: a
    /// certificate is release infrastructure that should be ready — issued,
    /// trusted, serving the right SAN — well before a launch is approved, not
    /// requested by the same change that approves it. `cli::devx::ship` now
    /// derives certificates and Ingress rules from [`release_brand_hosts`],
    /// the full registry, so every key carries a valid, isolated certificate
    /// family (ENG-768) in both environments regardless of `is_live()`.
    ///
    /// It gates the footer and host admission; route publication is separate.
    #[must_use]
    pub fn is_live(self) -> bool {
        Self::LIVE.contains(&self)
    }

    /// Every host this key answers to, across every environment this
    /// repository deploys. A host absent from every key's list is
    /// unregistered.
    #[must_use]
    pub const fn hosts(self) -> &'static [&'static str] {
        match self {
            Self::Neon => &["www.neonlaw.com", "staging.neonlaw.com"],
            Self::DeleteYourData => &["www.deleteyourdata.com", "staging.deleteyourdata.com"],
            Self::LawyerShook => &["www.lawyershook.com", "staging.lawyershook.com"],
            Self::Vesta => &[
                "www.vestaestateplanning.com",
                "staging.vestaestateplanning.com",
            ],
            Self::Misericordia => &["www.misericordialaw.com", "staging.misericordialaw.com"],
            Self::Abhaya => &["www.abhayaimmigration.com", "staging.abhayaimmigration.com"],
            Self::DeleteYourDebt => &["www.deleteyourdebt.com", "staging.deleteyourdebt.com"],
            Self::Summons => &["www.summonsdefense.nyc", "staging.summonsdefense.nyc"],
            Self::Daybridge => &["www.daybridgedivorce.com", "staging.daybridgedivorce.com"],
            Self::DeathAndDivorce => &[
                "www.deathanddivorcelaw.com",
                "staging.deathanddivorcelaw.com",
            ],
            Self::Aegis => &[
                "www.aegiscriminaldefense.com",
                "staging.aegiscriminaldefense.com",
            ],
            Self::Groundwork => &[
                "www.groundworkrealestatelaw.com",
                "staging.groundworkrealestatelaw.com",
            ],
            Self::CyberInjuryLaw => &["www.cyberinjurylaw.com", "staging.cyberinjurylaw.com"],
            Self::GuiebShook => &["www.guiebandshook.com", "staging.guiebandshook.com"],
        }
    }

    /// Resolve the [`Branding`] this key renders. `Neon` renders whatever
    /// this deployment resolved as its own default branding — the built-in
    /// value, or a mounted white-label manifest — so a white-label deploy's
    /// rename still applies to the default brand; every other key renders
    /// its own compiled placeholder, untouched by that manifest.
    #[must_use]
    pub fn resolve_branding(self, default_branding: &'static Branding) -> &'static Branding {
        match self {
            Self::Neon => default_branding,
            Self::DeleteYourData => &DELETE_YOUR_DATA_BRANDING,
            Self::LawyerShook => &LAWYER_SHOOK_BRANDING,
            Self::Vesta => &VESTA_BRANDING,
            Self::Misericordia => &MISERICORDIA_BRANDING,
            Self::Abhaya => &ABHAYA_BRANDING,
            Self::DeleteYourDebt => &DELETE_YOUR_DEBT_BRANDING,
            Self::Summons => &SUMMONS_BRANDING,
            Self::Daybridge => &DAYBRIDGE_BRANDING,
            Self::DeathAndDivorce => &DEATH_AND_DIVORCE_BRANDING,
            Self::Aegis => &AEGIS_BRANDING,
            Self::Groundwork => &GROUNDWORK_BRANDING,
            Self::GuiebShook => &GUIEB_SHOOK_BRANDING,
            Self::CyberInjuryLaw => &CYBER_INJURY_BRANDING,
        }
    }

    /// The env var naming this key's local-only bind port, if it needs one.
    ///
    /// Every registered host in [`Self::hosts`] is a real production/staging
    /// domain, so `Host:` header matching alone cannot reach a second brand
    /// locally — there is no DNS, TLS, or `/etc/hosts` entry standing in for
    /// `www.deleteyourdata.com` on a developer's machine. `Neon` needs no
    /// entry here: it is the default key, so it already answers on whatever
    /// port `PORT`/`AppConfig::port` binds, the same as a deployment with a
    /// single brand always has. Every other key gets its own dedicated port,
    /// named by this var, so `portal::hosting::run` can bind it and
    /// `portal::canonical_host` can resolve `localhost:<that port>` straight
    /// to this key without touching [`Self::hosts`], which stays a pure,
    /// compile-time registry of real hostnames only.
    #[must_use]
    pub const fn local_port_env_var(self) -> Option<&'static str> {
        match self {
            Self::Neon => None,
            Self::DeleteYourData => Some("NAVIGATOR_LOCAL_DELETE_YOUR_DATA_PORT"),
            Self::LawyerShook => Some("NAVIGATOR_LOCAL_LAWYER_SHOOK_PORT"),
            Self::Vesta => Some("NAVIGATOR_LOCAL_VESTA_PORT"),
            Self::Misericordia => Some("NAVIGATOR_LOCAL_MISERICORDIA_PORT"),
            Self::Abhaya => Some("NAVIGATOR_LOCAL_ABHAYA_PORT"),
            Self::DeleteYourDebt => Some("NAVIGATOR_LOCAL_DELETE_YOUR_DEBT_PORT"),
            Self::Summons => Some("NAVIGATOR_LOCAL_SUMMONS_PORT"),
            Self::Daybridge => Some("NAVIGATOR_LOCAL_DAYBRIDGE_PORT"),
            Self::DeathAndDivorce => Some("NAVIGATOR_LOCAL_DEATH_AND_DIVORCE_PORT"),
            Self::Aegis => Some("NAVIGATOR_LOCAL_AEGIS_PORT"),
            Self::Groundwork => Some("NAVIGATOR_LOCAL_GROUNDWORK_PORT"),
            Self::GuiebShook => Some("NAVIGATOR_LOCAL_GUIEB_SHOOK_PORT"),
            Self::CyberInjuryLaw => Some("NAVIGATOR_LOCAL_CYBER_INJURY_LAW_PORT"),
        }
    }
}

/// The key whose *apex* this host is, if any — **registry order, not launch
/// order**. `None` for a host that is either served (see
/// [`registered_brand_key`]) or unknown.
///
/// Registered is not admitted. A public request path wants
/// [`admitted_brand_key_for_apex`]; this answers only what the registry
/// compiled.
#[must_use]
pub fn brand_key_for_apex(host: &str) -> Option<BrandKey> {
    BrandKey::ALL
        .iter()
        .copied()
        .find(|key| key.apex().eq_ignore_ascii_case(host))
}

/// Look up which key, if any, is registered to serve `host` (already
/// port-stripped). `None` means the host answers to no brand in the
/// registry.
///
/// **Registered is not admitted.** A key is registered here — with its copy,
/// its mailbox, and its hosts — for as long as it takes to build the brand,
/// which is long before anyone may reach it. Answering a public request from
/// this function is what let the router's admission set drift away from the
/// deployment's: see [`admitted_brand_key`], which is what a request path
/// must call.
///
/// Matching folds case, because a `Host:` header is not case-sensitive and a
/// crawler that sends `WWW.NEONLAW.COM` is addressing the same site.
#[must_use]
pub fn registered_brand_key(host: &str) -> Option<BrandKey> {
    BrandKey::ALL.iter().copied().find(|key| {
        key.hosts()
            .iter()
            .any(|registered| registered.eq_ignore_ascii_case(host))
    })
}

// --- The launch gate --------------------------------------------------------
//
// [`BrandKey::LIVE`] is the approved launch set, and everything below is the
// single seam every public *content* surface reads it through: the request
// router's host admission, the apex redirect, the crawler's `robots.txt` /
// sitemap base, and the footer's family row. Four surfaces that each filtered
// `BrandKey::ALL` on `is_live()` in their own way is exactly how the router
// and the deploy render once came apart — they agreed by coincidence rather
// than by construction.
//
// Adding a surface that decides whether a host is public means calling one of
// these, never re-deriving the filter. `the_launch_gate_admits_exactly_the_live_set`
// and the cross-crate invariant tests hold each content surface to this list.
//
// The `ManagedCertificate` render and the Ingress render (`cli::devx::ship`)
// are deliberately *not* on this list as of ENG-808. TLS is release
// infrastructure, not a launch decision: [`release_brand_hosts`] below is the
// seam they read instead, and it covers the full registry, live or not. A
// held-out host therefore serves a valid, trusted certificate and then
// [`held_out_host`]'s `404` — never a TLS handshake failure — which is the
// whole point of separating "the certificate exists" from "the practice may
// be advertised."

/// Every (key, host) pair the launch gate admits, in registry order: the
/// approved set crossed with the hosts each approved key serves.
///
/// This is the list. The deploy render turns it into certificates and Ingress
/// rules; the router admits exactly these hostnames; the invariant tests
/// compare both against it.
#[must_use]
pub fn live_brand_hosts() -> Vec<(BrandKey, &'static str)> {
    BrandKey::LIVE
        .iter()
        .copied()
        .flat_map(|key| key.hosts().iter().copied().map(move |host| (key, host)))
        .collect()
}

/// Every (key, apex) pair the launch gate admits — the naked domains whose
/// 301 to a brand's own `www` host may be served.
#[must_use]
pub fn live_brand_apexes() -> Vec<(BrandKey, &'static str)> {
    BrandKey::LIVE
        .iter()
        .copied()
        .map(|key| (key, key.apex()))
        .collect()
}

/// Every (key, host) pair the compiled registry names, across every key —
/// live or held out. This is the **release inventory**: what a next release
/// must provision a valid, isolated `ManagedCertificate` and Ingress rule
/// for, per `cli::devx::ship`. It is [`BrandKey::ALL`] crossed with the hosts
/// each key serves, the same shape as [`live_brand_hosts`] with the `is_live`
/// filter removed — TLS coverage is deliberately wider than content
/// admission (see the launch-gate note above [`live_brand_hosts`]).
///
/// A key added to [`BrandKey::ALL`] therefore reaches this list — and so a
/// next release's certificate/Ingress render — with no second hostname list
/// to keep in sync, the same property [`live_brand_hosts`] already gives the
/// launch gate.
#[must_use]
pub fn release_brand_hosts() -> Vec<(BrandKey, &'static str)> {
    BrandKey::ALL
        .iter()
        .copied()
        .flat_map(|key| key.hosts().iter().copied().map(move |host| (key, host)))
        .collect()
}

/// Resolve a public `Host:` (already port-stripped) to the brand allowed to
/// wear it. `None` for an unregistered host **and** for a registered host
/// whose brand has not launched.
///
/// This is the admission decision. [`registered_brand_key`] is the registry
/// lookup underneath it and is not an admission decision.
#[must_use]
pub fn admitted_brand_key(host: &str) -> Option<BrandKey> {
    registered_brand_key(host).filter(|key| key.is_live())
}

/// The apex form of [`admitted_brand_key`]: the brand whose naked domain
/// this is, when that brand has launched.
#[must_use]
pub fn admitted_brand_key_for_apex(host: &str) -> Option<BrandKey> {
    brand_key_for_apex(host).filter(|key| key.is_live())
}

/// Whether `host` is a name this registry compiled but the launch gate
/// refuses — a served host or an apex belonging to a brand that is built and
/// staged but not approved.
///
/// Distinct from "unknown", and the distinction is the whole point: an
/// unknown host is somebody else's and gets the deployment's ordinary
/// canonical-host treatment, while a held-out host is *ours and not yet
/// public*, so it is refused outright rather than redirected. A 301 from
/// `www.future-brand.example` to the firm's site would confirm the
/// association, seed a crawler's cache with a permanent redirect that has to
/// be undone at launch, and — for a practice awaiting admission — is itself a
/// form of holding out.
#[must_use]
pub fn held_out_host(host: &str) -> bool {
    registered_brand_key(host).is_some_and(|key| !key.is_live())
        || brand_key_for_apex(host).is_some_and(|key| !key.is_live())
}

tokio::task_local! {
    static ACTIVE_BRANDING: &'static Branding;
}

static PROCESS_BRANDING: OnceLock<&'static Branding> = OnceLock::new();

/// Install the immutable branding used by spawned work in a production
/// binary. Request-scoped branding takes precedence, which keeps library tests
/// and independently constructed routers isolated from one another.
pub fn install_process_branding(branding: &'static Branding) -> Result<(), &'static str> {
    PROCESS_BRANDING
        .set(branding)
        .map_err(|_| "process branding is already installed")
}

/// Scope branding to one request future. Task-local storage follows async task
/// migration and keeps concurrently constructed routers isolated.
pub async fn scope<R>(branding: &'static Branding, future: impl Future<Output = R>) -> R {
    ACTIVE_BRANDING.scope(branding, future).await
}

fn current() -> &'static Branding {
    ACTIVE_BRANDING
        .try_with(|branding| *branding)
        .unwrap_or_else(|_| PROCESS_BRANDING.get().copied().unwrap_or(&DEFAULT_BRANDING))
}

fn value(value: Option<&str>, default: &'static str) -> &'static str {
    value
        .filter(|value| !value.is_empty())
        .map_or(default, |value| {
            Box::leak(value.to_owned().into_boxed_str())
        })
}

/// Resolve the firm's inbound support address from a manifest.
///
/// Two dials, most specific first:
///
/// 1. `brand.support_email` — a whole address, for a mailbox that is not
///    `support@` (`hello@`, `intake@`, a shared alias on another domain).
/// 2. `brand.support_domain` — just the host, giving `support@{HOST}`. This is
///    the ordinary dial: a firm that moves domains changes one field.
///
/// With neither set the firm reads mail at [`DEFAULT_BRANDING`]'s address.
fn firm_support_email(support_email: Option<&str>, support_domain: Option<&str>) -> &'static str {
    if let Some(address) = support_email.filter(|address| !address.is_empty()) {
        return Box::leak(address.to_owned().into_boxed_str());
    }
    match support_domain.filter(|domain| !domain.is_empty()) {
        Some(domain) => Box::leak(format!("support@{domain}").into_boxed_str()),
        None => DEFAULT_BRANDING.firm_email,
    }
}

impl Branding {
    /// Resolve one validated manifest into immutable rendering state.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn from_manifest(manifest: &BrandManifest) -> &'static Self {
        let brand = &manifest.brand;
        let firm_name = value(brand.firm.as_deref(), DEFAULT_BRANDING.firm.site_name);
        let firm_logo = manifest
            .assets
            .firm_logo
            .as_ref()
            .map_or(DEFAULT_BRANDING.firm.logo_href, |_| {
                "/public/brand/firm-logo.svg"
            });
        let firm_raster = manifest
            .assets
            .firm_logo_raster
            .as_ref()
            .map_or(DEFAULT_BRANDING.firm.social_image, |_| {
                "/public/brand/firm-logo.png"
            });
        let disclaimer = Box::leak(
            format!(
                "Nothing on this site is legal advice. An attorney-client relationship begins only with a signed retainer between you and {firm_name}. Every legal matter is different, and past results do not guarantee a similar result."
            )
            .into_boxed_str(),
        );
        let mission_description = Box::leak(
            format!(
                "How {firm_name} makes routine legal services affordable without sacrificing correctness, and what a licensed attorney in the loop actually buys you."
            )
            .into_boxed_str(),
        );
        let service_description =
            Box::leak(format!("Flat-fee legal services from {firm_name}.").into_boxed_str());
        // A registration belongs to its registrant, and a bundle that renames
        // the firm is a different firm: it publishes no mark notice at all,
        // because U.S. Reg. No. 6,325,650 is not theirs to notice. There is
        // deliberately no manifest key for it either — an operator with their
        // own registration is making an ownership claim this repository cannot
        // verify, and a notice naming the wrong owner is worse than none, since
        // it is the line a reader relies on for permission.
        let (firm_trademark, firm_trademark_registration, firm_trademark_record_url) =
            if brand.firm.is_some() {
                ("", "", "")
            } else {
                (
                    DEFAULT_BRANDING.firm_trademark,
                    DEFAULT_BRANDING.firm_trademark_registration,
                    DEFAULT_BRANDING.firm_trademark_record_url,
                )
            };
        // The same rule for the firm's affiliations: the association
        // affiliation and the house-brand family are Guieb & Shook PC's, so a
        // bundle that renames the firm publishes neither — a footer claiming
        // another firm's affiliation, or listing its brands as "Our Family",
        // is the wrong claim under the wrong name. The social profiles follow
        // that rename: the shared YouTube channel and Neon Law's own profiles
        // are not another firm's destinations.
        let (firm_memberships, firm_family, firm_social): (
            &'static [FirmMembership],
            &'static [BrandKey],
            &'static [FirmSocial],
        ) = if brand.firm.is_some() {
            (&[], &[], &[])
        } else {
            (
                DEFAULT_BRANDING.firm_memberships,
                DEFAULT_BRANDING.firm_family,
                DEFAULT_BRANDING.firm_social,
            )
        };
        Box::leak(Box::new(Self {
            firm: SiteBrand {
                site_name: firm_name,
                postal_address: value(
                    brand.firm_address.as_deref(),
                    DEFAULT_BRANDING.firm.postal_address,
                ),
                logo_href: firm_logo,
                social_image: firm_raster,
                // A mounted bundle that renames the firm but declares no
                // corporate name falls back to *its own* wordmark, never to this
                // firm's legal entity. The built-in default is only right when
                // the bundle left the firm name alone: otherwise the footer of a
                // white-label host would publish `Neon Law` as the
                // corporation behind someone else's site.
                legal_entity: value(
                    brand.firm_legal_entity.as_deref(),
                    if brand.firm.is_some() {
                        firm_name
                    } else {
                        DEFAULT_BRANDING.firm.legal_entity
                    },
                ),
                ..DEFAULT_BRANDING.firm
            },
            firm_email: firm_support_email(
                brand.support_email.as_deref(),
                brand.support_domain.as_deref(),
            ),
            support_domain: value(
                brand.support_domain.as_deref(),
                DEFAULT_BRANDING.support_domain,
            ),
            firm_phone: value(brand.firm_phone.as_deref(), DEFAULT_BRANDING.firm_phone),
            firm_offices: if brand.firm_offices.is_empty() {
                DEFAULT_BRANDING.firm_offices
            } else {
                let leaked: Vec<FirmOffice> = brand
                    .firm_offices
                    .iter()
                    .map(|entry| FirmOffice {
                        state: Box::leak(entry.state.clone().into_boxed_str()),
                        address: Box::leak(entry.address.clone().into_boxed_str()),
                        note: entry
                            .note
                            .clone()
                            .map(|note| &*Box::leak(note.into_boxed_str())),
                    })
                    .collect();
                Box::leak(leaked.into_boxed_slice())
            },
            firm_attorneys: if brand.firm_attorneys.is_empty() {
                DEFAULT_BRANDING.firm_attorneys
            } else {
                let leaked: Vec<FirmAttorney> = brand
                    .firm_attorneys
                    .iter()
                    .map(|entry| FirmAttorney {
                        name: Box::leak(entry.name.clone().into_boxed_str()),
                        licenses: {
                            let licenses: Vec<BarLicense> = entry
                                .licenses
                                .iter()
                                .map(|license| BarLicense {
                                    jurisdiction: Box::leak(
                                        license.jurisdiction.clone().into_boxed_str(),
                                    ),
                                    number: Box::leak(license.number.clone().into_boxed_str()),
                                    license_url: Box::leak(
                                        license.license_url.clone().into_boxed_str(),
                                    ),
                                })
                                .collect();
                            Box::leak(licenses.into_boxed_slice())
                        },
                    })
                    .collect();
                Box::leak(leaked.into_boxed_slice())
            },
            firm_memberships,
            firm_family,
            firm_social,
            firm_trademark,
            firm_trademark_registration,
            firm_trademark_record_url,
            consultation_url: value(
                brand.consultation_url.as_deref(),
                DEFAULT_BRANDING.consultation_url,
            ),
            terms_url: value(brand.terms_url.as_deref(), DEFAULT_BRANDING.terms_url),
            privacy_url: value(brand.privacy_url.as_deref(), DEFAULT_BRANDING.privacy_url),
            base_url: value(brand.base_url.as_deref(), DEFAULT_BRANDING.base_url),
            primary_domain: value(
                brand.primary_domain.as_deref(),
                DEFAULT_BRANDING.primary_domain,
            ),
            firm_disclaimer: disclaimer,
            mission_description,
            service_description,
            portal_only: manifest.portal_only,
            // A mounted manifest always replaces the default brand's
            // identity, never its registry slot.
            brand_key: BrandKey::Neon,
        }))
    }
}

#[derive(Debug, Clone, Copy)]
enum BrandKind {
    Firm,
}

/// Copy-compatible accessor preserving existing view APIs while resolving the
/// request-scoped structured branding.
pub struct BrandAccessor(BrandKind);

impl Deref for BrandAccessor {
    type Target = SiteBrand;

    fn deref(&self) -> &'static Self::Target {
        match self.0 {
            BrandKind::Firm => &current().firm,
        }
    }
}

pub static FIRM_BRAND: BrandAccessor = BrandAccessor(BrandKind::Firm);

/// Firm inbound email from request-scoped branding.
#[must_use]
pub fn firm_email() -> &'static str {
    current().firm_email
}

/// Firm consultation booking URL for public calls to action and the
/// `/consultation` redirect. A mounted brand bundle may supply its own
/// scheduler without changing the page code.
#[must_use]
pub fn consultation_url() -> &'static str {
    current().consultation_url
}

/// Where the footer's "Terms" link points. Defaults to the in-app
/// `/terms` page (NeonLaw's bundled terms of use). A white-label deploy
/// — a firm whose own marketing site already hosts its terms — sets
/// bundle's `terms_url` to that off-site URL so Neon Law Navigator links out
/// instead of serving someone else's binding legal text. Same `Copy`-
/// friendly `&'static str` shape as the other brand links; resolved when the
/// bundle loads.
#[must_use]
pub fn terms_url() -> &'static str {
    current().terms_url
}

/// Where the footer's "Privacy" link points. Defaults to the in-app
/// `/privacy` page; a bundle can point at a
/// deployer's own hosted privacy policy. See [`terms_url`].
#[must_use]
pub fn privacy_url() -> &'static str {
    current().privacy_url
}

#[must_use]
pub fn base_url() -> &'static str {
    current().base_url
}

#[must_use]
pub fn primary_domain() -> &'static str {
    current().primary_domain
}

/// The firm's own host — the `HOST` in `support@{HOST}`, and the host its
/// public site is served from.
///
/// Distinct from [`primary_domain`], which is the infrastructure apex a
/// deployment derives service hostnames from. The two are different things
/// that both look like domains; see [`Branding::support_domain`].
#[must_use]
pub fn support_domain() -> &'static str {
    current().support_domain
}

#[must_use]
pub fn portal_only() -> bool {
    current().portal_only
}

/// The registry key of the request-scoped `Branding` — which per-brand
/// assets (the tokens stylesheet) this request's chrome should link.
#[must_use]
pub fn brand_key() -> BrandKey {
    current().brand_key
}

/// The firm's published voice line from request-scoped branding.
#[must_use]
pub fn firm_phone() -> &'static str {
    current().firm_phone
}

/// The firm's published offices from request-scoped branding.
#[must_use]
pub fn firm_offices() -> &'static [FirmOffice] {
    current().firm_offices
}

/// The firm's attorneys and their bar licenses from request-scoped branding.
#[must_use]
pub fn firm_attorneys() -> &'static [FirmAttorney] {
    current().firm_attorneys
}

/// The associations the firm has a public affiliation with, from
/// request-scoped branding — the footer's standing line. Empty on a bundle
/// that renamed the firm.
#[must_use]
pub fn firm_memberships() -> &'static [FirmMembership] {
    current().firm_memberships
}

/// The request Brand's public social profiles, including the shared `YouTube`
/// channel, rendered under its footer wordmark. Renamed firms inherit none.
#[must_use]
pub fn firm_social() -> &'static [FirmSocial] {
    current().firm_social
}

/// The house brands the firm trades under, from request-scoped branding —
/// the footer's "Our Family" row before any Firm row overrides it. Empty on
/// a bundle that renamed the firm.
#[must_use]
pub fn firm_family() -> &'static [BrandKey] {
    current().firm_family
}

/// The firm's registered word mark, its U.S. registration number, and the
/// register's own record for it — the three parts of the footer's trademark
/// notice, resolved together because a notice missing any of them makes a claim
/// a reader cannot check.
///
/// All three are empty on a bundle that renamed the firm: see
/// [`Branding::firm_trademark_registration`] for why a host publishes no notice
/// rather than this one.
#[must_use]
pub fn firm_trademark() -> (&'static str, &'static str, &'static str) {
    let branding = current();
    (
        branding.firm_trademark,
        branding.firm_trademark_registration,
        branding.firm_trademark_record_url,
    )
}

/// The public pages the footer links rather than the header — [`FIRM_FOOTER_NAV`]
/// (Navigator, Blog, Notations, and the rest of Neon's own public surface),
/// present only on the compiled default brand itself — [`BrandKey::Neon`],
/// and not merely a bundle that happens to carry that same registry
/// placeholder. A mounted manifest always reports `brand_key: BrandKey::Neon`
/// (see [`Branding::brand_key`]), including one that renames the firm
/// entirely, so the gate also requires [`firm_family`] to be non-empty —
/// empty exactly when a manifest renamed the firm, per
/// [`Branding::firm_family`]. Every other brand's row, house or mounted,
/// carries Testimonials plus the three legal/contact entries:
///
/// Privacy and Terms sit in this row on the same footing as the Blog and
/// Notations, not in a smaller strip beneath it. They are the two documents a
/// reader is entitled to find without hunting, and the legal strip below
/// already carries the copyright, the bar disclosure, and the advertising
/// disclaimer — a second, quieter row of legal links there would read as fine
/// print about fine print. Their hrefs are [`terms_url`] and [`privacy_url`],
/// resolved per brand: the house-brand defaults are local paths, while a
/// white-label deploy can point to its own hosted terms and privacy pages.
///
/// Contact is a page of its own, alongside the consultation CTAs the practice
/// pages quote through: a reader who scrolled to the bottom of the site
/// looking for "how do I reach them" gets a page naming the firm's inbox and
/// voice line rather than having to find a CTA on some other page first.
/// Every brand publishes its own channels there — see `firm_email`.
/// Most brands link `/contact`. Aegis and Groundwork use their configured
/// consultation calendar for Contact in both navigation rows.
#[must_use]
pub fn firm_footer_nav() -> Vec<NavLink> {
    let branding = current();
    let mut nav: Vec<NavLink> = Vec::new();
    if !branding.firm_family.is_empty() {
        let shared = shared_footer_hrefs();
        nav.extend(FIRM_FOOTER_NAV.iter().map(|link| match link.href {
            // Every registered Brand serves its own `/blog` route.
            "/blog" => NavLink::leaf(link.label, link.href),
            // Neon's own pages stay on its own host: only another brand,
            // borrowing the shared row, leaves the site for them.
            path if path.starts_with('/') && branding.brand_key == BrandKey::Neon => {
                NavLink::leaf(link.label, path)
            }
            path if path.starts_with('/') => NavLink::leaf(link.label, shared.neon_path(path)),
            absolute => NavLink::leaf(link.label, absolute),
        }));
    }
    nav.push(NavLink::leaf("Testimonials", "/testimonials"));
    let contact_href = if matches!(branding.brand_key, BrandKey::Aegis | BrandKey::Groundwork) {
        branding.consultation_url
    } else {
        "/contact"
    };
    nav.push(NavLink::leaf("Contact", contact_href));
    nav.push(NavLink::leaf("Privacy", branding.privacy_url));
    nav.push(NavLink::leaf("Terms", branding.terms_url));
    nav.sort_by(|a, b| a.label.cmp(b.label));
    nav
}

/// Absolute hrefs the one shared footer links out to, resolved once per
/// process for the deployment's own environment (staging links staging).
struct SharedFooterHrefs {
    neon: String,
    neon_paths: std::sync::Mutex<Vec<(&'static str, &'static str)>>,
}

impl SharedFooterHrefs {
    /// `https://<neon host><path>`, interned so the `&'static str` a
    /// [`NavLink`] holds is built once per path rather than per request.
    fn neon_path(&self, path: &'static str) -> &'static str {
        let mut cache = self.neon_paths.lock().expect("footer href cache");
        if let Some((_, href)) = cache.iter().find(|(key, _)| *key == path) {
            return href;
        }
        let href: &'static str = Box::leak(format!("{}{path}", self.neon).into_boxed_str());
        cache.push((path, href));
        href
    }
}

fn shared_footer_hrefs() -> &'static SharedFooterHrefs {
    static HREFS: LazyLock<SharedFooterHrefs> = LazyLock::new(|| {
        let host = env::var("NAVIGATOR_PUBLIC_HOST").ok();
        SharedFooterHrefs {
            neon: BrandKey::Neon.public_home_href_for(host.as_deref()),
            neon_paths: std::sync::Mutex::new(Vec::new()),
        }
    });
    &HREFS
}

/// The firm's legal-advice disclaimer, shown in the footer of every page. The
/// footer is firm-anchored, so it lives here as a single resolved string rather
/// than a per-brand `SiteBrand` field. Names the firm via [`FIRM_BRAND`];
/// resolved when the bundle loads.
#[must_use]
pub fn firm_disclaimer() -> &'static str {
    current().firm_disclaimer
}

#[must_use]
pub fn mission_description() -> &'static str {
    current().mission_description
}

#[must_use]
pub fn service_description() -> &'static str {
    current().service_description
}

/// The deployed release — the `YY.M.D` Artifact Registry tag this image
/// was published under, baked into the web image by `deploy.yml` as
/// `NAVIGATOR_RELEASE_TAG` (the same value `GET /version` reports as
/// `release`). Rendered in the footer so a push is visible end-to-end:
/// the moment a new image is live on the site, the footer's version
/// changes. `None` on a local `cargo run` (the env var is unset, or the
/// build honestly reports `unknown`), so dev never shows a bogus
/// version. Resolved once per process.
#[must_use]
pub fn deployed_release() -> Option<&'static str> {
    static RELEASE: LazyLock<Option<&'static str>> =
        LazyLock::new(|| match env::var("NAVIGATOR_RELEASE_TAG") {
            Ok(v) if v.is_empty() || v == "unknown" => None,
            Ok(v) => Some(&*Box::leak(v.into_boxed_str())),
            Err(_) => None,
        });
    *RELEASE
}

#[cfg(test)]
mod tests {
    use super::{scope, Branding, NavLink, DEFAULT_BRANDING, FIRM_BRAND};
    use crate::brand_bundle::BrandManifest;

    #[tokio::test]
    async fn mounted_manifest_is_request_scoped_and_complete() {
        let manifest: BrandManifest = serde_yaml::from_str(
            "version: 1\nbrand:\n  firm: Acme Law\n  support_email: firm@acme.example\n  firm_address: 1 Main St\n  base_url: https://app.acme.example\n  primary_domain: acme.example\n  consultation_url: https://acme.example/book\n  terms_url: https://acme.example/terms\n  privacy_url: https://acme.example/privacy\nassets:\n  firm_logo: firm.svg\n  firm_logo_raster: firm.png\n",
        )
        .unwrap();
        let branding = Branding::from_manifest(&manifest);
        scope(branding, async {
            assert_eq!(FIRM_BRAND.site_name, "Acme Law");
            assert_eq!(FIRM_BRAND.postal_address, "1 Main St");
            assert_eq!(FIRM_BRAND.logo_href, "/public/brand/firm-logo.svg");
            assert_eq!(FIRM_BRAND.social_image, "/public/brand/firm-logo.png");
            assert_eq!(super::firm_email(), "firm@acme.example");
            assert_eq!(super::consultation_url(), "https://acme.example/book");
            assert_eq!(super::terms_url(), "https://acme.example/terms");
            assert_eq!(super::privacy_url(), "https://acme.example/privacy");
            assert_eq!(branding.base_url, "https://app.acme.example");
            assert_eq!(branding.primary_domain, "acme.example");
            assert!(super::firm_disclaimer().contains("Acme Law"));
            assert!(super::mission_description().contains("Acme Law"));
            assert_eq!(
                super::service_description(),
                "Flat-fee legal services from Acme Law."
            );
            // Unset in the manifest, but the manifest renamed the firm → the
            // bundle's own wordmark, never this firm's corporate name.
            assert_eq!(FIRM_BRAND.legal_entity, "Acme Law");
        })
        .await;
        assert_eq!(FIRM_BRAND.site_name, DEFAULT_BRANDING.firm.site_name);
    }

    /// A manifest that hosts its own terms and privacy pages off-site sees
    /// that carried into the footer row itself, not only into `terms_url`
    /// and `privacy_url` in isolation.
    #[tokio::test]
    async fn a_mounted_manifests_off_site_terms_and_privacy_reach_the_footer_row() {
        let manifest: BrandManifest = serde_yaml::from_str(
            "version: 1\nbrand:\n  firm: Acme Law\n  support_email: firm@acme.example\n  firm_address: 1 Main St\n  base_url: https://app.acme.example\n  primary_domain: acme.example\n  consultation_url: https://acme.example/book\n  terms_url: https://acme.example/terms\n  privacy_url: https://acme.example/privacy\nassets:\n  firm_logo: firm.svg\n  firm_logo_raster: firm.png\n",
        )
        .unwrap();
        let branding = Branding::from_manifest(&manifest);
        scope(branding, async {
            let footer = super::firm_footer_nav();
            assert_eq!(
                footer.iter().map(|n| n.label).collect::<Vec<_>>(),
                ["Contact", "Privacy", "Terms", "Testimonials"],
                "off Neon, only the firm's own legal surface remains"
            );
            assert_eq!(
                footer.iter().find(|n| n.label == "Terms").unwrap().href,
                "https://acme.example/terms"
            );
            assert_eq!(
                footer.iter().find(|n| n.label == "Privacy").unwrap().href,
                "https://acme.example/privacy"
            );
        })
        .await;
    }

    /// Launched practice domains stay isolated and prepared for release.
    #[test]
    fn new_practice_domains_are_isolated_and_prepared_for_release() {
        for (name, domain) in [
            ("aegis", "aegiscriminaldefense.com"),
            ("groundwork", "groundworkrealestatelaw.com"),
        ] {
            let key = BrandKey::parse(name).expect("practice is registered");
            assert_eq!(key.apex(), domain);
            assert_eq!(key.canonical_host(), format!("www.{domain}"));
            assert_eq!(
                super::registered_brand_key(&format!("staging.{domain}")),
                Some(key)
            );
            assert!(key.is_live(), "launched after DNS and TLS readiness");
            assert!(super::release_brand_hosts()
                .iter()
                .any(|(brand, _)| *brand == key));
            assert!(key.publishes_firm_path("/services"));
            assert!(!key.publishes_firm_path("/business"));
        }
    }

    /// The footer publishes the firm's offices; the letterhead names the entity's box.
    #[test]
    fn publishes_every_firm_office_without_touching_the_letterhead() {
        let published: Vec<(&str, &str)> = DEFAULT_BRANDING
            .firm_offices
            .iter()
            .map(|office| (office.state, office.address))
            .collect();
        assert_eq!(
            published,
            [
                ("California", "1515 20th Street, Bakersfield, CA 93301"),
                ("Nevada", "5150 Mae Anne Ave, Ste 405-9002, Reno, NV 89523"),
                ("New York", "12 E 49th St, 18th Floor, New York, NY 10017"),
                ("Washington", "1111 Main St, STE 201E, Vancouver, WA 98660"),
            ],
            "the footer publishes the offices the firm actually keeps, ordered by state",
        );
        assert_eq!(
            DEFAULT_BRANDING.firm.postal_address, "5150 Mae Anne Ave Ste 405-9002, Reno, NV 89523",
            "the letterhead names the entity of record's own box",
        );
    }

    /// Every office is published bare. The `note` seam stays — a deployment
    /// that must qualify an address sets it and the footer renders it under
    /// that address alone — but this firm qualifies none, and a note that
    /// reappeared would disclaim a practice the firm actually has.
    #[test]
    fn publishes_every_office_address_unqualified() {
        let qualified: Vec<(&str, Option<&str>)> = DEFAULT_BRANDING
            .firm_offices
            .iter()
            .map(|office| (office.state, office.note))
            .collect();
        assert_eq!(
            qualified,
            [
                ("California", None),
                ("Nevada", None),
                ("New York", None),
                ("Washington", None),
            ],
            "no office carries a note",
        );
    }

    /// The firm's footer discloses no per-attorney bar licence. The credentials
    /// belong on a public lawyer profile, where a reader choosing a lawyer
    /// looks for them.
    #[test]
    fn the_firm_footer_discloses_no_bar_licences() {
        assert!(
            DEFAULT_BRANDING.firm_attorneys.is_empty(),
            "the firm publishes no bar numbers in its footer",
        );
    }

    /// Every published bar number must point at the record that issued it: a
    /// number and a URL that disagree is a false statement about a licence.
    /// This deployment publishes none, so the rule guards whatever a manifest
    /// sets — the check runs against the resolved set, not a literal list.
    #[test]
    fn every_published_bar_number_appears_in_the_record_it_links() {
        for attorney in DEFAULT_BRANDING.firm_attorneys {
            assert!(
                !attorney.licenses.is_empty(),
                "{} is listed with no licence",
                attorney.name
            );
            for license in attorney.licenses {
                assert!(
                    license.license_url.contains(license.number),
                    "{} {} No. {} does not appear in {}",
                    attorney.name,
                    license.jurisdiction,
                    license.number,
                    license.license_url,
                );
            }
        }
    }

    #[tokio::test]
    async fn firm_offices_and_attorneys_are_configurable_via_the_manifest() {
        // A white-label deploy publishes its own doors and its own licensed
        // attorneys; neither falls back to the firm's once it sets its own.
        let manifest: BrandManifest = serde_yaml::from_str(
            "version: 1\nbrand:\n  firm_offices:\n    - state: Idaho\n      address: 1 Main St, Boise, ID 83702\n  firm_attorneys:\n    - name: Aquarius Lumen\n      licenses:\n        - jurisdiction: Idaho\n          number: \"4242\"\n          license_url: https://isb.idaho.gov/4242\n",
        )
        .unwrap();
        let branding = Branding::from_manifest(&manifest);
        scope(branding, async {
            assert_eq!(super::firm_offices().len(), 1);
            assert_eq!(super::firm_offices()[0].state, "Idaho");
            assert_eq!(super::firm_attorneys().len(), 1);
            assert_eq!(super::firm_attorneys()[0].name, "Aquarius Lumen");
            assert_eq!(super::firm_attorneys()[0].licenses[0].number, "4242");
        })
        .await;
        // The compiled default is unchanged.
        assert_eq!(DEFAULT_BRANDING.firm_offices.len(), 4);
        assert!(DEFAULT_BRANDING.firm_attorneys.is_empty());
    }

    #[tokio::test]
    async fn firm_legal_entity_is_configurable_via_the_manifest() {
        // The footer's regulated legal-services attribution is a manifest edit,
        // not a code change: setting `firm_legal_entity` renames the entity.
        // The override is deliberately a name the compiled default does not
        // carry, so a scope that silently fell back would fail here.
        let manifest: BrandManifest =
            serde_yaml::from_str("version: 1\nbrand:\n  firm_legal_entity: Cascade Law LLP\n")
                .unwrap();
        let branding = Branding::from_manifest(&manifest);
        scope(branding, async {
            assert_eq!(FIRM_BRAND.legal_entity, "Cascade Law LLP");
        })
        .await;
        // The compiled default is unchanged.
        assert_eq!(DEFAULT_BRANDING.firm.legal_entity, "Guieb & Shook PC");
    }

    /// The compiled default notices the live registration, spelled the way the
    /// register spells it and pointing at the register's own record.
    ///
    /// The registrant is [`SiteBrand::legal_entity`], asserted here beside the
    /// number so the pair cannot drift: U.S. Reg. No. 6,325,650 is the Firm's,
    /// and the footer composes its notice from these two fields.
    #[test]
    fn the_default_notices_the_registration_and_where_to_verify_it() {
        let (mark, registration, record) = (
            DEFAULT_BRANDING.firm_trademark,
            DEFAULT_BRANDING.firm_trademark_registration,
            DEFAULT_BRANDING.firm_trademark_record_url,
        );
        assert_eq!(mark, "NEON LAW");
        assert_eq!(registration, "6,325,650");
        assert_eq!(
            record,
            "https://tmsearch.uspto.gov/search/search-results/90039224"
        );
        assert_eq!(
            DEFAULT_BRANDING.firm.legal_entity, "Guieb & Shook PC",
            "the notice names this entity as the registrant"
        );
    }

    /// A bundle that renames the firm publishes no mark notice.
    ///
    /// The registration is not the mounting host's, so there is nothing for it
    /// to notice — and a host inheriting this one would tell its own readers
    /// that someone else's registration covers the name on its door.
    #[tokio::test]
    async fn a_renamed_firm_notices_no_registration() {
        let manifest: BrandManifest =
            serde_yaml::from_str("version: 1\nbrand:\n  firm: Cascade Law\n").unwrap();
        let branding = Branding::from_manifest(&manifest);
        scope(branding, async {
            assert_eq!(super::firm_trademark(), ("", "", ""));
        })
        .await;
        // A bundle that changed something else keeps it: the firm did not move.
        let untouched: BrandManifest =
            serde_yaml::from_str("version: 1\nbrand:\n  firm_phone: '+1 555 000 0000'\n").unwrap();
        scope(Branding::from_manifest(&untouched), async {
            assert_eq!(
                super::firm_trademark(),
                (
                    "NEON LAW",
                    "6,325,650",
                    DEFAULT_BRANDING.firm_trademark_record_url
                )
            );
        })
        .await;
    }

    /// The firm's affiliations follow the trademark rule: a bundle that
    /// renames the firm publishes no association affiliation and lists no
    /// house-brand family, because both are Guieb & Shook PC's; a bundle that
    /// changes anything else keeps them. Every house brand carries the same
    /// two, since the affiliated organization and the family are the firm, not
    /// the brand.
    #[tokio::test]
    async fn a_renamed_firm_claims_no_membership_and_lists_no_family() {
        let renamed: BrandManifest =
            serde_yaml::from_str("version: 1\nbrand:\n  firm: Cascade Law\n").unwrap();
        scope(Branding::from_manifest(&renamed), async {
            assert!(super::firm_memberships().is_empty());
            assert!(super::firm_family().is_empty());
            assert!(super::firm_social().is_empty());
        })
        .await;
        let untouched: BrandManifest =
            serde_yaml::from_str("version: 1\nbrand:\n  firm_phone: '+1 555 000 0000'\n").unwrap();
        scope(Branding::from_manifest(&untouched), async {
            assert_eq!(super::firm_memberships(), DEFAULT_BRANDING.firm_memberships);
            assert_eq!(super::firm_family(), BrandKey::ALL);
            assert_eq!(super::firm_social(), DEFAULT_BRANDING.firm_social);
        })
        .await;
        for branding in [&DELETE_YOUR_DATA_BRANDING, &LAWYER_SHOOK_BRANDING] {
            assert_eq!(branding.firm_memberships, DEFAULT_BRANDING.firm_memberships);
            assert_eq!(branding.firm_family, BrandKey::ALL);
            assert_eq!(branding.firm_social, super::SHARED_SOCIAL);
            // Every house brand publishes the same firm offices.
            assert_eq!(branding.firm_offices, DEFAULT_BRANDING.firm_offices);
        }
    }

    /// The three profiles the Neon Law footer links, in display order. Each
    /// href is the live profile: X `@NeonLawUSA`, the LinkedIn company
    /// `neon-law-usa`, and the shared `YouTube` channel `@neon-law-firm`.
    /// Other house Brands inherit only the `YouTube` destination.
    #[test]
    fn the_firm_links_its_own_social_profiles() {
        let published: Vec<(&str, &str, &str)> = DEFAULT_BRANDING
            .firm_social
            .iter()
            .map(|profile| (profile.network, profile.label, profile.href))
            .collect();
        assert_eq!(
            published,
            [
                ("x", "X", "https://x.com/NeonLawUSA"),
                (
                    "linkedin",
                    "LinkedIn",
                    "https://www.linkedin.com/company/neon-law-usa"
                ),
                (
                    "youtube",
                    "YouTube",
                    "https://www.youtube.com/@neon-law-firm"
                ),
            ]
        );
        for key in BrandKey::ALL {
            if *key == BrandKey::Neon {
                continue;
            }
            let branding = key.resolve_branding(&DEFAULT_BRANDING);
            assert_eq!(
                branding
                    .firm_social
                    .iter()
                    .filter(|link| link.network == "youtube")
                    .count(),
                1,
                "{} shares the firm's YouTube channel",
                branding.firm.site_name
            );
            assert!(branding
                .firm_social
                .iter()
                .all(|link| !matches!(link.network, "x" | "linkedin")));
            assert_eq!(
                branding.firm_social.len(),
                if *key == BrandKey::Vesta { 5 } else { 1 }
            );
        }
    }

    #[tokio::test]
    async fn social_profiles_follow_the_request_brand() {
        scope(&super::VESTA_BRANDING, async {
            let profiles = super::firm_social();
            assert_eq!(
                profiles.iter().map(|link| link.network).collect::<Vec<_>>(),
                ["facebook", "instagram", "threads", "tiktok", "youtube"]
            );
            assert_eq!(
                profiles[0].href,
                "https://www.facebook.com/people/Vesta-Estate-Planning/61594889293557/"
            );
            assert_eq!(
                profiles[1].href,
                "https://www.instagram.com/vesta.estate.planning/"
            );
            assert_eq!(
                profiles[2].href,
                "https://www.threads.com/@vesta.estate.planning"
            );
            assert_eq!(
                profiles[3].href,
                "https://www.tiktok.com/@vesta.estate.planning"
            );
        })
        .await;
        scope(&super::CYBER_INJURY_BRANDING, async {
            assert_eq!(
                super::firm_social()
                    .iter()
                    .map(|link| link.network)
                    .collect::<Vec<_>>(),
                ["youtube"]
            );
        })
        .await;
    }

    /// The one affiliation the firm publishes names the association in full,
    /// carries its actual standing, and links the association's Get Involved
    /// page over HTTPS — a reader verifies the relationship there, not on this
    /// page.
    #[test]
    fn the_affiliation_names_the_association_and_links_its_get_involved_page() {
        let [membership] = DEFAULT_BRANDING.firm_memberships else {
            panic!("the firm publishes exactly one membership");
        };
        assert_eq!(membership.name, "Justice Technology Association");
        assert_eq!(
            membership.href,
            "https://justicetechassociation.org/get-involved"
        );
        // A bucket key, not a root-relative path: `rewrite_image_src` passes
        // anything starting with `/` straight through, so a leading slash
        // would pin every deployment to the container's own copy and never
        // reach that deployment's assets bucket.
        assert_eq!(
            membership.logo_key,
            "img/justice-technology-association/logo.png"
        );
        assert!(
            !membership.logo_key.starts_with('/'),
            "a bucket key routes through the asset seam: {}",
            membership.logo_key
        );
        // With no `NAVIGATOR_ASSET_BASE_URL` — dev, tests, a fork — it falls
        // back to the crate-bundled mount rather than breaking.
        assert_eq!(
            crate::assets::asset_url(membership.logo_key),
            "/public/img/justice-technology-association/logo.png"
        );
    }

    /// The mark and the legal person behind it are two different strings, and
    /// the site publishes both.
    ///
    /// This is the load-bearing assertion of the whole rename. A firm may trade
    /// under a mark, but a client has to be able to see which legal person they
    /// are engaging — the copyright, the retainer, and the delete-guarded
    /// Entity row all name `Guieb & Shook PC` while the header reads `Neon Law`.
    /// Collapsing the two would either put a trade name on a binding instrument
    /// or put a lawyer's name back on the door the mark exists to replace.
    #[test]
    fn the_mark_and_the_entity_of_record_are_distinct() {
        assert_eq!(FIRM_BRAND.site_name, "Neon Law");
        // The firm ALONE. The rendered copyright line is "© {year}
        // {copyright_holder}", composed straight from this field, so this
        // one must stay the single legal person the mark's registrant is.
        assert_eq!(FIRM_BRAND.legal_entity, "Guieb & Shook PC");
        assert_ne!(
            FIRM_BRAND.site_name, FIRM_BRAND.legal_entity,
            "the mark is not the legal person; the footer must publish both"
        );
        assert!(FIRM_BRAND.is_law_firm);
    }

    /// The disclaimer states the two things that matter regardless of which
    /// face renders it: nothing here is legal advice, and no attorney-client
    /// relationship exists without a signed retainer for an active project.
    #[test]
    fn the_disclaimer_requires_a_signed_retainer_for_an_active_project() {
        let disclaimer = super::firm_disclaimer();
        assert!(disclaimer.contains("Nothing here is legal advice"));
        assert!(disclaimer.contains("signed retainer"));
        assert!(disclaimer.contains("active project"));
    }

    /// The two disclaimers carry the same substance and differ by exactly one
    /// thing: the firm's opens by naming itself an attorney advertisement.
    ///
    /// Dropping the label loses a notice a law firm's public pages are expected
    /// to carry, and it is only honest on a brand that is a law firm — so the
    /// label rides `firm_disclaimer` and the flag is asserted beside it.
    #[test]
    fn the_firm_disclaimer_names_itself_an_advertisement() {
        const LABEL: &str = "Attorney advertisement.";

        let firm = super::firm_disclaimer();

        assert!(
            firm.starts_with(LABEL),
            "the firm's disclaimer opens with the label: {firm}"
        );
        // The label is only honest on a face that is a law firm.
        assert!(FIRM_BRAND.is_law_firm);
    }

    /// The disclaimer states the past-results line, wherever a reader could
    /// infer one. It moved off `/disputes` into this shared string, so the
    /// guard that it is still said belongs here rather than on that page.
    #[test]
    fn the_disclaimer_carries_the_past_results_line() {
        let disclaimer = super::firm_disclaimer();
        assert!(
            disclaimer.contains("Past results do not guarantee future outcomes"),
            "the past-results line: {disclaimer}"
        );
    }

    /// Every request-scoped accessor reads the field it is named for.
    ///
    /// These are a wall of three-line functions over one struct, which is
    /// exactly the shape a copy-paste gets wrong silently: `support_domain`
    /// returning `primary_domain` still compiles, still returns a plausible
    /// domain, and ships `support@` at the infrastructure apex. The two are
    /// deliberately different values here so that swap cannot pass, and the
    /// pair is asserted distinct so a future edit collapsing them fails loudly
    /// rather than making this test vacuous.
    #[test]
    fn each_brand_accessor_reads_its_own_field() {
        assert_eq!(super::base_url(), DEFAULT_BRANDING.base_url);
        assert_eq!(super::primary_domain(), DEFAULT_BRANDING.primary_domain);
        assert_eq!(super::support_domain(), DEFAULT_BRANDING.support_domain);
        assert_eq!(super::portal_only(), DEFAULT_BRANDING.portal_only);
        assert_eq!(super::firm_phone(), DEFAULT_BRANDING.firm_phone);
        assert_eq!(super::firm_memberships(), DEFAULT_BRANDING.firm_memberships);
        assert_eq!(super::firm_family(), DEFAULT_BRANDING.firm_family);
        assert_eq!(super::terms_url(), DEFAULT_BRANDING.terms_url);
        assert_eq!(super::privacy_url(), DEFAULT_BRANDING.privacy_url);
    }

    /// A build that was never stamped reports no release rather than a
    /// placeholder. The footer prints the platform line only when this is
    /// `Some`, so a bogus value here would publish "Navigator #unknown" on
    /// every page of a local run.
    #[test]
    fn an_unstamped_build_reports_no_release() {
        // The test process is not a deployed image, so the tag is absent or
        // honestly `unknown`; either way the accessor must decline it.
        assert_eq!(super::deployed_release(), None);
    }

    /// A white-label bundle that renames the firm but declares no corporate name
    /// takes its own wordmark as the legal entity. Falling back to the compiled
    /// default would print this firm's entity in the footer of someone else's
    /// site — a leak of our identity, not a harmless default.
    #[tokio::test]
    async fn a_renamed_firm_without_a_legal_entity_does_not_inherit_this_firms() {
        let manifest: BrandManifest =
            serde_yaml::from_str("version: 1\nbrand:\n  firm: Acme Law\n").unwrap();
        let branding = Branding::from_manifest(&manifest);
        scope(branding, async {
            assert_eq!(FIRM_BRAND.legal_entity, "Acme Law");
            assert!(!FIRM_BRAND.legal_entity.contains("Guieb & Shook PC"));
        })
        .await;
    }

    /// A bundle that leaves the firm name alone is this firm's own deploy, so the
    /// compiled legal entity is still the right answer.
    #[tokio::test]
    async fn a_bundle_that_keeps_the_firm_name_keeps_the_compiled_legal_entity() {
        let manifest: BrandManifest =
            serde_yaml::from_str("version: 1\nbrand:\n  firm_phone: +1 555 0100\n").unwrap();
        let branding = Branding::from_manifest(&manifest);
        scope(branding, async {
            assert_eq!(FIRM_BRAND.legal_entity, "Guieb & Shook PC");
        })
        .await;
    }

    #[tokio::test]
    async fn concurrent_brand_scopes_do_not_cross_contaminate() {
        let first: BrandManifest =
            serde_yaml::from_str("version: 1\nbrand:\n  firm: First Law\n").unwrap();
        let second: BrandManifest =
            serde_yaml::from_str("version: 1\nbrand:\n  firm: Second Law\n").unwrap();
        let first = Branding::from_manifest(&first);
        let second = Branding::from_manifest(&second);

        let (first_name, second_name) = tokio::join!(
            scope(first, async {
                tokio::task::yield_now().await;
                (
                    FIRM_BRAND.site_name,
                    super::mission_description(),
                    super::service_description(),
                )
            }),
            scope(second, async {
                tokio::task::yield_now().await;
                (
                    FIRM_BRAND.site_name,
                    super::mission_description(),
                    super::service_description(),
                )
            })
        );
        assert_eq!(first_name.0, "First Law");
        assert!(first_name.1.contains("First Law"));
        assert!(first_name.2.contains("First Law"));
        assert!(!first_name.1.contains("Second Law"));
        assert_eq!(second_name.0, "Second Law");
        assert!(second_name.1.contains("Second Law"));
        assert!(second_name.2.contains("Second Law"));
        assert!(!second_name.1.contains("First Law"));
    }

    /// One brand, and it is the law firm's.
    ///
    /// The nonprofit's wordmark used to sit beside this one, on a second brand
    /// a reader could reach from the header. It is retired, and this asserts
    /// that no built-in brand carries it back in.
    #[test]
    fn the_only_built_in_brand_is_the_firms() {
        assert_eq!(FIRM_BRAND.site_name, "Neon Law");
        assert!(FIRM_BRAND.is_law_firm);
        assert_eq!(FIRM_BRAND.legal_entity, "Guieb & Shook PC");
        assert!(
            !FIRM_BRAND.site_name.contains("Foundation"),
            "the retired nonprofit wordmark is not the firm's: {}",
            FIRM_BRAND.site_name
        );
    }

    #[test]
    fn the_brand_publishes_a_raster_social_image() {
        // The Open Graph card needs a PNG — scrapers won't render the SVG
        // favicon.
        let is_png = |path: &str| {
            std::path::Path::new(path)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
        };
        assert!(is_png(FIRM_BRAND.social_image));
    }

    /// The firm publishes its own box. Within the Ridgeview Mail Center the box
    /// number is the whole address, so serving another entity's suite under the
    /// firm's name misdelivers rather than bounces — `405-9002` is the firm's.
    ///
    /// Three wrong answers, and they fail differently. `405-9005` is a *live*
    /// box belonging to another entity of ours, so publishing it would send a
    /// client's mail somewhere it is actually collected. `405-9999` was the
    /// retired nonprofit's and `405-9777` the retired partnership's: nothing
    /// holds either now, so they would bounce — but a suffix nobody collects is
    /// still the wrong thing to print on a law firm's contact page.
    #[test]
    fn the_firm_publishes_only_its_own_suite_address() {
        assert!(FIRM_BRAND.postal_address.contains("405-9002"));
        for wrong in ["405-9005", "405-9999", "405-9777"] {
            assert!(
                !FIRM_BRAND.postal_address.contains(wrong),
                "another entity's box is not the firm's published address"
            );
        }
    }

    #[test]
    fn the_firm_nav_leads_with_services_then_nautilus_then_consultation_booking() {
        assert_eq!(FIRM_BRAND.nav.len(), 3);
        assert_eq!(FIRM_BRAND.nav[0].label, "Services");
        assert_eq!(FIRM_BRAND.nav[0].href, "/services");
        assert_eq!(FIRM_BRAND.nav[1].label, "Nautilus");
        assert_eq!(FIRM_BRAND.nav[1].href, "/nautilus");
        assert_eq!(FIRM_BRAND.nav[2].label, "Book Consultation");
        assert_eq!(
            FIRM_BRAND.nav[2].href,
            "https://calendar.notion.so/meet/nick-shook/or15n4yy7"
        );
    }

    /// How many public destinations every brand's footer row links.
    const FOOTER_LINK_COUNT: usize = 12;

    #[test]
    fn the_footer_nav_carries_what_the_header_does_not() {
        let footer: Vec<&str> = super::firm_footer_nav().iter().map(|n| n.label).collect();
        assert_eq!(
            footer,
            [
                "API",
                "Blog",
                "Contact",
                "Design",
                "Glossary",
                "Navigator",
                "Notations",
                "Presentations",
                "Privacy",
                "Team",
                "Terms",
                "Testimonials",
            ]
        );
        assert_eq!(
            footer.len(),
            FOOTER_LINK_COUNT,
            "the footer has twelve public destinations: {footer:?}"
        );
        let mut sorted = footer.clone();
        sorted.sort_unstable();
        assert_eq!(footer, sorted, "the footer nav is alphabetized by label");
        // Every label is linked once, from exactly one of the two rows. There
        // is no longer any exception: the one duplicate was the nonprofit's
        // cross-link, which had to sit in both rows while the site served two
        // faces, and both faces are one now.
        let header: Vec<&str> = FIRM_BRAND.nav.iter().map(|n| n.label).collect();
        for label in &footer {
            assert!(
                !header.contains(label),
                "{label} is linked once, from the footer"
            );
        }
        for retired in ["Foundation", "Firm"] {
            assert!(
                !header.contains(&retired) && !footer.contains(&retired),
                "{retired} names a retired or redundant entry that neither row may link",
            );
        }
        assert!(
            super::firm_footer_nav()
                .iter()
                .all(|link| !link.is_dropdown()),
            "every footer link is a flat leaf"
        );
    }

    /// Every registered Brand publishes the same twelve-link footer row.
    #[tokio::test]
    async fn every_brand_footer_has_twelve_links_including_design() {
        for key in super::BrandKey::ALL {
            let branding = key.resolve_branding(&super::DEFAULT_BRANDING);
            scope(branding, async {
                let nav = super::firm_footer_nav();
                assert_eq!(
                    nav.len(),
                    FOOTER_LINK_COUNT,
                    "{} footer: {:?}",
                    key.as_str(),
                    nav.iter().map(|n| n.label).collect::<Vec<_>>()
                );
                let design = nav
                    .iter()
                    .find(|n| n.label == "Design")
                    .unwrap_or_else(|| panic!("{} footer links Design", key.as_str()));
                assert!(design.href.ends_with("/design"), "{}", design.href);
            })
            .await;
        }
    }

    /// The footer is one row for every brand. Each brand's Blog, Contact,
    /// Privacy, and Terms stay on that brand.
    #[tokio::test]
    async fn every_brand_footer_keeps_its_blog_link_on_that_brand() {
        let labels = || {
            super::firm_footer_nav()
                .iter()
                .map(|n| n.label)
                .collect::<Vec<_>>()
        };
        let neon = labels();
        for branding in [&super::ABHAYA_BRANDING] {
            scope(branding, async {
                assert_eq!(labels(), neon, "every brand shares Neon's footer row");
                let nav = super::firm_footer_nav();
                let href = |label: &str| nav.iter().find(|n| n.label == label).unwrap().href;
                assert_eq!(href("Glossary"), "https://www.neonlaw.com/glossary");
                assert_eq!(href("Blog"), "/blog");
                assert_eq!(href("Privacy"), "/privacy");
                assert_eq!(href("Terms"), "/terms");
                assert_eq!(
                    href("Contact"),
                    "/contact",
                    "a brand's contact page is its own"
                );
            })
            .await;
        }
    }

    /// Neon's own footer links its pages relatively. On any other brand only
    /// Contact, Testimonials, Blog, Privacy, and Terms are local routes;
    /// Neon's unique pages link to their published destinations.
    #[tokio::test]
    async fn contact_testimonials_and_blog_are_internal_footer_links() {
        for link in super::firm_footer_nav() {
            match link.label {
                "Contact" | "Testimonials" | "Blog" => assert!(link.href.starts_with('/')),
                "Privacy" => assert_eq!(link.href, "/privacy"),
                "Terms" => assert_eq!(link.href, "/terms"),
                _ => assert!(
                    link.href.starts_with('/'),
                    "{} links {} — Neon's own footer has no external links",
                    link.label,
                    link.href
                ),
            }
        }
        scope(&super::ABHAYA_BRANDING, async {
            for link in super::firm_footer_nav() {
                if !matches!(
                    link.label,
                    "Contact" | "Testimonials" | "Blog" | "Privacy" | "Terms"
                ) {
                    assert!(
                        link.href.starts_with("https://www.neonlaw.com/"),
                        "{} links {}",
                        link.label,
                        link.href
                    );
                }
            }
        })
        .await;
        for key in super::BrandKey::ALL {
            let href = key.public_home_href();
            assert!(
                href.starts_with("https://") && key.hosts().iter().any(|host| href.ends_with(host)),
                "{} home is a registry host: {href}",
                key.as_str(),
            );
        }
    }

    /// No row links a retired URL.
    ///
    /// The legal aid audience page and the whole `/foundation` tree were
    /// retired outright, and `/foundation/*` answers `410 Gone`. A link in
    /// either row would send a reader at one of those answers, which is the
    /// failure this catches for every retired shape at once. `/team` is no
    /// longer in this list — the compact directory links to its own profile.
    #[test]
    fn neither_row_links_a_retired_url() {
        for link in FIRM_BRAND.nav.iter().chain(super::firm_footer_nav().iter()) {
            for retired in ["/foundation", "legal-aid", "/mission", "/attorneys"] {
                assert!(
                    !link.href.starts_with(retired),
                    "{} links the retired {retired}: {}",
                    link.label,
                    link.href
                );
            }
            assert_ne!(link.label, "Legal aid centers");
            assert_ne!(link.label, "Foundation");
        }
    }

    /// The firm's contact page is linked from the footer, at its own name.
    #[test]
    fn contact_is_a_firm_footer_leaf_at_its_own_name() {
        let contact = super::firm_footer_nav()
            .into_iter()
            .find(|n| n.label == "Contact")
            .expect("Contact leaf present");
        assert!(!contact.is_dropdown());
        assert_eq!(contact.href, "/contact");
    }

    /// The talks catalog is linked from the firm's footer, at its own name.
    #[test]
    fn presentations_is_a_firm_footer_leaf_at_the_catalogs_own_name() {
        let presentations = super::firm_footer_nav()
            .into_iter()
            .find(|n| n.label == "Presentations")
            .expect("Presentations leaf present");
        assert!(!presentations.is_dropdown());
        assert_eq!(
            presentations.href, "/presentations",
            "Neon's own footer keeps its pages on its own host"
        );
    }

    /// The public testimonials page stays one click from every page.
    #[test]
    fn the_public_testimonials_page_is_linked_from_the_footer() {
        let testimonials = super::firm_footer_nav()
            .into_iter()
            .find(|n| n.label == "Testimonials")
            .expect("the public testimonials page is linked");
        assert_eq!(testimonials.href, "/testimonials");
    }

    #[test]
    fn firm_email_defaults_to_a_mailbox_on_the_firms_own_host() {
        assert_eq!(super::firm_email(), "contact@neonlaw.com");
        // The local part is the firm's to choose — `firm_support_email`'s first
        // dial exists precisely to name a mailbox that is not `support@`. What
        // must not drift is the *host*: the built-in address reads mail at the
        // built-in domain, so moving `support_domain` without moving the address
        // is caught here.
        assert_eq!(
            super::firm_email().rsplit_once('@').map(|(_, host)| host),
            Some(DEFAULT_BRANDING.support_domain),
            "the built-in address and the built-in host cannot drift apart"
        );
    }

    /// The mailbox host and the infrastructure apex are still two dials, and on
    /// this deployment they hold the same value.
    ///
    /// They were required to differ while the firm read mail at one domain and
    /// derived `workflows.<apex>` from another. One site on one domain collapses
    /// that, so the assertion is now that both resolve to `neonlaw.com` — while
    /// the fields stay separate, because a deployment that moves its mailbox
    /// must still be able to do so without moving its service hostnames.
    #[test]
    fn the_mailbox_host_and_the_infrastructure_apex_are_both_neonlaw_com() {
        assert_eq!(DEFAULT_BRANDING.support_domain, "neonlaw.com");
        assert_eq!(DEFAULT_BRANDING.primary_domain, "neonlaw.com");
    }

    #[test]
    fn a_support_domain_alone_builds_the_support_address() {
        assert_eq!(
            super::firm_support_email(None, Some("example.test")),
            "support@example.test"
        );
    }

    #[test]
    fn an_explicit_support_email_wins_over_the_domain() {
        assert_eq!(
            super::firm_support_email(Some("hello@other.test"), Some("example.test")),
            "hello@other.test"
        );
    }

    #[test]
    fn neither_dial_set_keeps_the_built_in_address() {
        assert_eq!(super::firm_support_email(None, None), "contact@neonlaw.com");
        // An empty string is "unset", not "an address with no characters".
        assert_eq!(
            super::firm_support_email(Some(""), Some("")),
            "contact@neonlaw.com"
        );
    }

    #[test]
    fn terms_and_privacy_links_default_to_the_in_app_pages() {
        // Unset → the bundled `/terms` and `/privacy` routes. A white-label
        // deploy overrides these to its own off-site legal pages so
        // Neon Law Navigator never serves a deployer's binding legal text.
        assert_eq!(super::terms_url(), "/terms");
        assert_eq!(super::privacy_url(), "/privacy");
    }

    #[test]
    fn leaf_constructor_yields_no_children() {
        let n = NavLink::leaf("Home", "/");
        assert!(!n.is_dropdown());
        assert_eq!(n.href, "/");
    }

    #[test]
    fn dropdown_constructor_carries_children() {
        const CHILDREN: &[NavLink] = &[NavLink::leaf("A", "/a")];
        let n = NavLink::dropdown("Group", CHILDREN);
        assert!(n.is_dropdown());
        assert_eq!(n.children.len(), 1);
    }

    use super::{registered_brand_key, BrandKey, DELETE_YOUR_DATA_BRANDING, LAWYER_SHOOK_BRANDING};

    /// Every host a key claims resolves back to that same key.
    #[test]
    fn every_registered_host_maps_to_its_key() {
        for key in BrandKey::ALL {
            for host in key.hosts() {
                assert_eq!(
                    registered_brand_key(host),
                    Some(*key),
                    "{host} should resolve to {key:?}"
                );
            }
        }
    }

    #[test]
    fn public_home_href_matches_the_deployments_host_family() {
        for key in BrandKey::ALL {
            assert_eq!(
                key.public_home_href_for(Some("www.neonlaw.com")),
                format!("https://www.{}", key.apex()),
            );
            assert_eq!(
                key.public_home_href_for(Some("staging.neonlaw.com")),
                format!("https://staging.{}", key.apex()),
            );
            assert_eq!(key.public_home_href_for(None), key.public_home_href());
        }
    }

    /// The separately approved launch set is drawn from the same registry and
    /// every live key has a host that resolves back to it.
    #[test]
    fn every_live_key_is_registered_and_reachable() {
        for key in BrandKey::LIVE {
            assert!(BrandKey::ALL.contains(key), "{key:?} is not in ALL");
            assert!(
                key.hosts()
                    .iter()
                    .any(|host| registered_brand_key(host) == Some(*key)),
                "{key:?} has no resolving host"
            );
        }
    }

    // --- The launch gate ---------------------------------------------------

    /// The gate admits the approved set and nothing else, in both
    /// directions: every live key's hosts are admitted, and every host of
    /// every key that is not live is refused.
    ///
    /// This is the invariant the router, the crawler base, the footer, the
    /// certificate render, and the Ingress render all read. Flipping a key
    /// into `LIVE` moves all five at once; nothing else can.
    #[test]
    fn the_launch_gate_admits_exactly_the_live_set() {
        for key in BrandKey::ALL {
            for host in key.hosts() {
                if key.is_live() {
                    assert_eq!(
                        super::admitted_brand_key(host),
                        Some(*key),
                        "{host} belongs to the live brand {}",
                        key.as_str()
                    );
                    assert!(!super::held_out_host(host), "{host} is live, not held out");
                } else {
                    assert_eq!(
                        super::admitted_brand_key(host),
                        None,
                        "{host} belongs to {}, which has not launched",
                        key.as_str()
                    );
                    assert!(
                        super::held_out_host(host),
                        "{host} is ours and not yet public"
                    );
                }
            }
        }
    }

    /// The same gate over naked domains. A held-out apex is refused rather
    /// than 301'd, so no crawler learns the association before launch.
    #[test]
    fn the_launch_gate_admits_exactly_the_live_apexes() {
        for key in BrandKey::ALL {
            let apex = key.apex();
            if key.is_live() {
                assert_eq!(super::admitted_brand_key_for_apex(apex), Some(*key));
                assert!(!super::held_out_host(apex));
            } else {
                assert_eq!(super::admitted_brand_key_for_apex(apex), None);
                assert!(
                    super::held_out_host(apex),
                    "{apex} is ours and not yet public"
                );
            }
        }
    }

    /// [`super::live_brand_hosts`] is the list every surface derives from, so
    /// it must be exactly `LIVE` crossed with the hosts those keys serve —
    /// each key's production and staging name, and no other key's.
    #[test]
    fn live_brand_hosts_is_the_live_set_crossed_with_its_hosts() {
        let listed = super::live_brand_hosts();
        let expected: Vec<(BrandKey, &str)> = BrandKey::ALL
            .iter()
            .copied()
            .filter(|key| key.is_live())
            .flat_map(|key| key.hosts().iter().copied().map(move |host| (key, host)))
            .collect();
        assert_eq!(listed, expected);
        for (_, host) in &listed {
            assert!(
                super::admitted_brand_key(host).is_some(),
                "{host} is rendered but not admitted"
            );
        }
        assert_eq!(
            super::live_brand_apexes().len(),
            BrandKey::LIVE.len(),
            "one apex per live key"
        );
    }

    /// [`super::release_brand_hosts`] is the TLS/Ingress release inventory:
    /// every registered key crossed with its hosts, unfiltered by
    /// `is_live()`. It must include [`super::live_brand_hosts`]
    /// — every live host still gets a certificate — while also covering every
    /// held-out brand's hosts, which `live_brand_hosts` deliberately omits.
    #[test]
    fn release_brand_hosts_is_the_full_registry_crossed_with_its_hosts() {
        let released = super::release_brand_hosts();
        let expected: Vec<(BrandKey, &str)> = BrandKey::ALL
            .iter()
            .copied()
            .flat_map(|key| key.hosts().iter().copied().map(move |host| (key, host)))
            .collect();
        assert_eq!(released, expected);
        assert_eq!(
            released.len(),
            BrandKey::ALL.len() * 2,
            "every registered key serves exactly a production and a staging host"
        );
        for pair in super::live_brand_hosts() {
            assert!(
                released.contains(&pair),
                "{pair:?} is live and must still be in the release inventory"
            );
        }
        let held_out: Vec<(BrandKey, &str)> = released
            .iter()
            .copied()
            .filter(|(key, _)| !key.is_live())
            .collect();
        for (key, host) in held_out {
            assert_eq!(
                super::admitted_brand_key(host),
                None,
                "{host} is in the release inventory (gets a certificate) but must stay refused \
                 at the router until {} launches",
                key.as_str(),
            );
        }
    }

    /// A `Host:` header is not case-sensitive. A crawler sending
    /// `WWW.NEONLAW.COM` addresses the live site, and one sending
    /// `WWW.SUMMONSDEFENSE.NYC` addresses its own hearing practice.
    #[test]
    fn the_gate_folds_host_case() {
        assert_eq!(
            super::admitted_brand_key("WWW.NEONLAW.COM"),
            Some(BrandKey::Neon)
        );
        assert_eq!(
            super::admitted_brand_key("WWW.SUMMONSDEFENSE.NYC"),
            Some(BrandKey::Summons)
        );
        assert!(!super::held_out_host("WWW.SUMMONSDEFENSE.NYC"));
        assert!(!super::held_out_host("SummonsDefense.NYC"));
    }

    /// An unknown host is somebody else's, not ours-and-not-yet-public: it
    /// is not held out, so the deployment keeps redirecting it to its
    /// canonical host rather than answering `404`.
    #[test]
    fn an_unknown_host_is_not_held_out() {
        assert!(!super::held_out_host("unregistered.example"));
        assert!(!super::held_out_host("localhost"));
    }

    /// A host no key claims resolves to no brand at all — the caller decides
    /// what an unregistered host means (redirect, or fall back to default).
    #[test]
    fn an_unregistered_host_resolves_to_no_key() {
        assert_eq!(registered_brand_key("unregistered.example"), None);
        assert_eq!(registered_brand_key("localhost"), None);
    }

    /// No host is claimed by more than one key — a spoofable ambiguity would
    /// let a request pick which brand's chrome it renders.
    #[test]
    fn no_host_is_claimed_by_two_keys() {
        let mut seen = std::collections::HashSet::new();
        for key in BrandKey::ALL {
            for host in key.hosts() {
                assert!(seen.insert(*host), "{host} is registered to two keys");
            }
        }
    }

    /// `Neon` is the default key, and resolving it hands back whatever
    /// default branding the caller supplies — including a white-label
    /// manifest's — untouched.
    #[test]
    fn neon_resolves_the_supplied_default_branding() {
        assert_eq!(BrandKey::default(), BrandKey::Neon);
        let resolved = BrandKey::Neon.resolve_branding(&DEFAULT_BRANDING);
        assert_eq!(resolved.firm.site_name, "Neon Law");
    }

    /// `DeleteYourData` resolves its own compiled placeholder, never the supplied
    /// default — a white-label rename of the firm's brand must not leak into
    /// the house brand next to it.
    #[test]
    fn delete_your_data_resolves_its_own_branding_regardless_of_the_supplied_default() {
        let renamed = Branding::from_manifest(
            &serde_yaml::from_str("version: 1\nbrand:\n  firm: Acme Law\n").unwrap(),
        );
        let resolved = BrandKey::DeleteYourData.resolve_branding(renamed);
        assert_eq!(resolved.firm.site_name, "DeleteYourData.com");
        assert_eq!(
            resolved.firm.site_name,
            DELETE_YOUR_DATA_BRANDING.firm.site_name
        );
    }

    /// `Neon` needs no local port override — it is the default key, so it
    /// always answers on the ambient `PORT`. Every non-default key must
    /// name a distinct env var, so two house brands never fight over one
    /// local bind port.
    #[test]
    fn only_the_default_key_has_no_local_port_env_var() {
        assert_eq!(BrandKey::Neon.local_port_env_var(), None);
        let mut seen = std::collections::HashSet::new();
        for key in BrandKey::ALL {
            if let Some(var) = key.local_port_env_var() {
                assert_ne!(*key, BrandKey::Neon, "the default key must have none");
                assert!(seen.insert(var), "{var} is registered to two keys");
            }
        }
        assert_eq!(
            BrandKey::DeleteYourData.local_port_env_var(),
            Some("NAVIGATOR_LOCAL_DELETE_YOUR_DATA_PORT")
        );
        assert_eq!(
            BrandKey::LawyerShook.local_port_env_var(),
            Some("NAVIGATOR_LOCAL_LAWYER_SHOOK_PORT")
        );
    }

    /// The registry brands render distinct chrome, which is the whole point
    /// of separate house-brand entries.
    #[test]
    fn registry_brands_carry_distinct_chrome() {
        assert_ne!(
            DEFAULT_BRANDING.firm.site_name,
            DELETE_YOUR_DATA_BRANDING.firm.site_name
        );
        assert_ne!(
            DEFAULT_BRANDING.firm.logo_href,
            DELETE_YOUR_DATA_BRANDING.firm.logo_href
        );
        assert_ne!(
            DEFAULT_BRANDING.firm.site_name,
            LAWYER_SHOOK_BRANDING.firm.site_name
        );
        assert_eq!(
            LAWYER_SHOOK_BRANDING.firm.logo_href,
            "/public/brand/lawyer-shook/logo.svg"
        );
        assert_eq!(
            LAWYER_SHOOK_BRANDING.firm.social_image,
            "/public/brand/lawyer-shook/logo.png"
        );
        assert_eq!(LAWYER_SHOOK_BRANDING.firm.legal_entity, "Guieb & Shook PC");
        assert_eq!(LAWYER_SHOOK_BRANDING.support_domain, "lawyershook.com");
        assert!(LAWYER_SHOOK_BRANDING.firm_trademark.is_empty());
        assert!(LAWYER_SHOOK_BRANDING.firm_trademark_registration.is_empty());
    }

    /// Every compiled `Branding` carries the registry key that produced it, so
    /// a caller resolving per-brand assets (the tokens stylesheet) can recover
    /// the key from the scoped value alone — it does not have to be threaded
    /// alongside `Branding` everywhere `Branding` already travels.
    #[test]
    fn every_branding_carries_its_own_registry_key() {
        assert_eq!(DEFAULT_BRANDING.brand_key, BrandKey::Neon);
        assert_eq!(
            DELETE_YOUR_DATA_BRANDING.brand_key,
            BrandKey::DeleteYourData
        );
    }

    /// Chrome copy for a house brand is real prose, not a `Placeholder …`
    /// string, and it does not invent a trademark notice.
    #[test]
    fn delete_your_data_chrome_copy_is_not_a_placeholder_and_carries_no_mark() {
        let branding = &DELETE_YOUR_DATA_BRANDING;
        assert!(!branding.firm.tagline.contains("Placeholder"));
        assert!(!branding.mission_description.contains("Placeholder"));
        assert!(!branding.service_description.contains("Placeholder"));
        assert!(!branding.firm.tagline.contains("Guieb & Shook PC practice"));
        assert!(branding.mission_description.contains("non-legal service"));
        assert!(branding
            .service_description
            .contains("$29/mo or $290/year prepaid"));
        assert!(!branding.service_description.contains("additional member"));
        assert!(!branding.service_description.contains("$50"));
        assert!(branding.service_description.contains("opt-in"));
        assert!(branding.firm_trademark.is_empty());
        assert!(branding.firm_trademark_registration.is_empty());
        assert!(branding.firm_trademark_record_url.is_empty());
    }

    #[test]
    fn every_registry_key_declares_the_catalog_pages_it_ships() {
        for key in BrandKey::ALL {
            let pages = key.catalog_pages();
            assert!(!pages.is_empty(), "{key:?} ships at least one catalog page");
            for page in pages {
                assert!(
                    crate::locales::locale_page_kind(page).is_some(),
                    "{key:?} catalog page `{page}` is not a known stem"
                );
            }
        }
        assert_eq!(
            BrandKey::DeleteYourData.catalog_pages(),
            &["home", "services"]
        );
        assert_eq!(BrandKey::LawyerShook.catalog_pages(), &["home", "services"]);
        for key in BrandKey::ALL {
            assert!(
                key.publishes_firm_path("/blog"),
                "{key:?} serves its footer's /blog"
            );
            assert!(key.publishes_firm_path("/blog/a-post"));
        }
        assert!(BrandKey::DeleteYourData.publishes_firm_path("/"));
        assert!(!BrandKey::DeleteYourData.publishes_firm_path("/services"));
        assert!(BrandKey::DeleteYourData.publishes_firm_path("/contact"));
        assert!(!BrandKey::DeleteYourData.publishes_firm_path("/disputes"));
        assert!(BrandKey::LawyerShook.publishes_firm_path("/"));
        assert!(!BrandKey::LawyerShook.publishes_firm_path("/services"));
        assert!(!BrandKey::LawyerShook.publishes_firm_path("/contact"));
        assert!(!BrandKey::LawyerShook.publishes_firm_path("/disputes"));
        assert!(BrandKey::Neon.publishes_firm_path("/services"));
        assert!(!BrandKey::Neon.publishes_firm_path("/business"));
        assert!(!BrandKey::Neon.publishes_firm_path("/disputes"));
    }

    /// A mounted white-label manifest replaces the *default* brand's identity,
    /// never the key: it always renders in the `Neon` registry slot, the same
    /// slot `BrandKey::Neon` resolves through [`BrandKey::resolve_branding`].
    #[tokio::test]
    async fn a_mounted_manifest_still_carries_the_neon_key() {
        let manifest: BrandManifest =
            serde_yaml::from_str("version: 1\nbrand:\n  firm: Acme Law\n").unwrap();
        let branding = Branding::from_manifest(&manifest);
        assert_eq!(branding.brand_key, BrandKey::Neon);
        scope(branding, async {
            assert_eq!(super::brand_key(), BrandKey::Neon);
        })
        .await;
    }
}
