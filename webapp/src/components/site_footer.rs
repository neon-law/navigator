//! The site footer, as a Dioxus component (issue #641, Phase 2).
//!
//! Three bands. The contact band reaches the firm: the published email address, the
//! voice line, and every office it keeps. The affiliations row under it names
//! the firm's family — every house brand it trades under, the current one
//! unlinked — and the firm's association affiliations, each with its published
//! standing line linking the association's own site. Below both the legal strip carries
//! the load-bearing, brand-driven lines every public page owes — the copyright
//! that names the legal person behind the site, which attorney holds which bar
//! licence, and the attorney-advertising disclaimer.
//!
//! It is prop-driven like [`crate::components::PricingSection`]: the process
//! brand (`views::brand`) is mapped onto [`SiteFooterLegal`]'s props per request
//! on the server, so the wasm client never links the view layer and a
//! white-label deploy emits its own identity. `crate::public_chrome::PublicFooter`
//! owns that mapping so no page restates it.
//!
//! The copy is legal-council-reviewed and must not drift. Bar disclosure is per
//! attorney and never firm-level: every published number links to the bar's own
//! record, so a visitor verifies a licence against the licensing jurisdiction
//! rather than trusting a summary line the page wrote about itself.

use dioxus::prelude::*;

/// The platform line every Navigator footer carries, so a white-label
/// deployment still names the software it runs. Shared with
/// [`crate::firm_footer::FirmFooter`] so the wording cannot drift.
pub const POWERED_BY_NEON_LAW_NAVIGATOR: &str = "Powered by Neon Law Navigator";

/// The firm's final footer line, shared by public and authenticated pages.
pub const FOOTER_TAGLINE: &str = "Everyone deserves access to justice. Made with ❤️ in 🗽.";

use crate::components::{ExternalLink, Icon, IconName};

/// Provider and property notices, independent of the operating firm.
#[derive(Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FooterDisclosure {
    pub prefix: String,
    pub provider: String,
    pub href: String,
    pub ownership: String,
}

/// One published office — the state it sits in and its street address.
/// Mirrors `views::brand::FirmOffice`.
#[derive(Clone, PartialEq, Eq)]
pub struct FooterOffice {
    pub state: String,
    pub address: String,
    /// A qualification rendered under the address — e.g. an admission that has
    /// not issued yet. `None` publishes the address unqualified. See
    /// `views::brand::FirmOffice::note` for why this rides the address.
    pub note: Option<String>,
}

/// Outlines of the states the firm publishes an office in, each derived from
/// the state's real border geometry rather than drawn by hand.
///
/// The source is Wikimedia Commons' `Blank US Map (states only).svg`, released
/// under CC0, whose per-state paths carry the actual border coordinates in one
/// shared projection. Each state's path was lifted from that file, simplified
/// with Douglas–Peucker to a few dozen vertices, and fitted — **at its true
/// aspect ratio**, centred — into this 100×100 box. Aspect ratio is the reason
/// the earlier hand-traced set read wrong: a state stretched to fill a square
/// stops being that state's silhouette, whatever its corners do.
///
/// Fitting to the larger dimension means a wide state (Washington) leaves space
/// above and below and a tall one (California, Nevada) leaves it to the sides,
/// so all four render at a consistent scale beside one another.
///
/// **Deliberately not the state flags.** New York's flag is the state coat of
/// arms and Washington's is the state seal; both states restrict use of those
/// devices in advertising, and a law-firm footer is advertising copy. An
/// outline carries the same "this is where we are" meaning and claims nothing
/// about who endorses the firm. It is also legible at one colour and one low
/// opacity, which four multi-colour flags behind body text would not be.
///
/// A state with no entry renders no watermark, so a white-label deploy that
/// publishes an office somewhere else simply gets the plain treatment.
const STATE_OUTLINES: &[(&str, &str)] = &[
    (
        "California",
        "M53.6 95 52.4 87.7 48.9 83.2 46.7 82.4 46.9 80.3 43.4 79.1 40 74.9 34.4 73.1 33.4 72.1 \
         34.5 67.1 31.3 61.3 29.8 56.3 28.7 55.3 28.9 52.5 30.6 50.9 30.4 49.7 29 49.3 27.5 46.7 \
         28 41.3 29 41.3 28.6 43.2 30.1 44.5 29.3 40.2 30.4 39.6 29.7 38.4 29.1 38.6 28.1 41 26 \
         38.6 25.8 34.9 23.3 29.8 24.6 21.7 22.4 17.4 22.5 15.3 25.9 11.4 27.4 7.3 27.8 3 53.7 \
         10.3 47.3 35.1 75.4 77.6 75 78.4 77.6 84.5 75.2 85.7 74.3 87.1 73.6 89.8 72.1 91.2 71.6 \
         93.9 73.2 95.2 72.9 96.2 70.8 97Z",
    ),
    (
        "Nevada",
        "M68.9 74.4 67.1 84 65.7 85.6 64.7 85.6 64 84.1 62 83.3 60.1 83.6 59.8 93.8 58.7 97 20.2 \
         39.5 19.6 37.6 28.6 3 80.4 14.7Z",
    ),
    // Two subpaths: the mainland, and Long Island east of the city.
    (
        "New York",
        "M73.5 84.3 75.7 84.8 83 81.7 80 82 86.4 79.6 97 71.1 92.5 73.4 91.3 72.1 90.5 75.5 89.1 \
         75.5 92.5 70.3 87.7 75 77 78.7 73.6 82.8ZM65.8 16.5 66.4 21.8 68 23.4 67.6 29.8 69.8 \
         34.6 69.2 35.8 69.9 37.4 70.5 36.3 71.9 37.3 74.4 49.5 74.1 60.2 76.6 72.5 77.8 73.5 \
         75.3 75.9 76.6 77.5 73.5 82.7 73.6 78 60.8 74 53.6 66.4 3.6 76.3 3 72 11.9 62.4 8.2 \
         57.6 8 55.3 13.4 52.5 19.3 51.4 25.3 52.4 33 49.9 36.4 45.8 38.6 45.2 38.9 42.9 37 40.5 \
         38.4 38.3 35.7 37.3 35.6 35.1 47.3 20.1 65.2 15.2Z",
    ),
    (
        "Washington",
        "M86.8 77.6 86.5 84.2 62.2 78.4 34.4 78.4 30.4 74.9 20.7 75.8 15.4 72.5 16 64.7 3 57 5 51 \
         4.8 56.2 8.4 51.4 5.5 49.8 5.6 46.4 9.6 46.7 5.3 45.5 5.5 22.9 7.4 19.5 13.7 25.6 24.1 \
         29.1 24.3 31.5 28.7 31.2 27.7 35 19.4 42 23 42.6 20.2 41.8 29.4 34.5 29.5 37.7 25.7 39.8 \
         25.6 46.2 24.7 43.6 22.9 46.6 23.3 43.3 19.2 47 22.9 48.6 28.8 45.3 29.1 39.4 33 35.2 \
         31.1 26 31.6 28.5 28.8 29.2 31.5 35.3 27.9 28.9 30.6 23.2 32.4 25.8 33.4 23.9 30.9 20.2 \
         32.3 15.8 97 32.6Z",
    ),
];

/// The outline for a published state, or `None` for one this component does not
/// carry a tracing of.
fn state_outline(state: &str) -> Option<&'static str> {
    STATE_OUTLINES
        .iter()
        .find(|(name, _)| *name == state)
        .map(|(_, outline)| *outline)
}

/// One page the footer links. Mirrors `views::brand::NavLink`, narrowed to what
/// a flat footer row renders — the footer has no dropdowns.
#[derive(Clone, PartialEq, Eq)]
pub struct FooterNavLink {
    pub label: String,
    pub href: String,
}

/// One social profile the footer links under the wordmark.
///
/// `network` selects the mark: `x`, `linkedin`, or `youtube`. An unknown
/// network still links, with the label and no mark, rather than dropping the
/// profile.
#[derive(Clone, PartialEq, Eq)]
pub struct FooterSocialLink {
    pub label: String,
    pub href: String,
    pub network: String,
}

/// The mark for a known network, or `None` when the footer has no drawing of it.
fn social_icon(network: &str) -> Option<IconName> {
    match network {
        "x" => Some(IconName::X),
        "linkedin" => Some(IconName::LinkedIn),
        "youtube" => Some(IconName::YouTube),
        _ => None,
    }
}

/// Any multi-brand family uses two columns on desktop and one on mobile.
pub const FAMILY_TWO_COLUMN_THRESHOLD: usize = 2;

/// The family list's class, widened for every multi-brand family.
#[must_use]
pub fn family_list_class(entries: usize) -> &'static str {
    if entries >= FAMILY_TWO_COLUMN_THRESHOLD {
        "site-footer__family-list site-footer__family-list--two-column"
    } else {
        "site-footer__family-list"
    }
}

/// One brand the current firm wears — an entry in the footer's "Our Family"
/// row. The current brand is shown but not linked; every other entry links
/// that brand's home host. An entry with no `href` (a runtime-created brand
/// with no host yet) is shown unlinked too, never as an empty anchor.
#[derive(Clone, PartialEq, Eq)]
pub struct FooterBrandLink {
    pub label: String,
    pub href: String,
    pub current: bool,
    /// The brand's own mark; decorative because the adjacent text names it.
    pub logo_href: String,
    /// What this brand does, in a few words. A cold reader learns nothing
    /// from "Vesta" or "Abhaya", so the family list is close to useless as
    /// bare wordmarks. Empty renders the wordmark alone.
    pub byline: String,
}

/// One association affiliation — the footer's standing line, linking the
/// association's own site. Mirrors `views::brand::FirmMembership`.
#[derive(Clone, PartialEq, Eq)]
pub struct FooterMembership {
    pub label: String,
    /// The firm's standing with the association (`Mission-Aligned
    /// Partner`), shown as the link text beside the association's mark.
    pub standing: String,
    pub href: String,
    /// The association's mark, already resolved to this deployment's asset
    /// origin by `crate::public_chrome`. Rendered decoratively beside the
    /// line; empty renders the award glyph instead.
    pub logo_href: String,
}

/// One bar license an attorney holds. Mirrors `views::brand::BarLicense`.
#[derive(Clone, PartialEq, Eq)]
pub struct FooterBarLicense {
    pub jurisdiction: String,
    pub number: String,
    pub license_url: String,
}

/// One licensed attorney and the set of bar licenses they hold. Mirrors
/// `views::brand::FirmAttorney`.
#[derive(Clone, PartialEq, Eq)]
pub struct FooterAttorney {
    pub name: String,
    pub licenses: Vec<FooterBarLicense>,
}

/// The lines a published address is set over.
///
/// An address is published as one string, here and in a white-label manifest
/// (`brand.firm_offices[].address`), because that is how a firm writes its own
/// address. The footer sets it the way an envelope carries it — street, then
/// unit, then city — so the suite gets its own line and the city a reader is
/// scanning for starts one, instead of the whole address running together and
/// breaking wherever the narrow column happens to end.
///
/// Every comma starts a new line, except the one between the city and its
/// state: `Walnut Creek, CA 94596` is one line, because a city split from its
/// ZIP stops reading as a place. So the last two of three or more
/// comma-separated parts are the final line, and each part before them is a
/// line of its own. An address written with no comma at all is published as the
/// one line it was written as, rather than broken at a guess.
fn address_lines(address: &str) -> Vec<&str> {
    let mut commas = address.rmatch_indices(',').map(|(index, _)| index);
    // The last comma separates the city from `ST ZIP` and is not a break; the
    // one before it ends the last line above the city.
    commas.next();
    let (above, city) = match commas.next() {
        Some(index) => (&address[..index], Some(address[index + 1..].trim_start())),
        None => (address, None),
    };
    let mut lines: Vec<&str> = above
        .split(',')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    lines.extend(city);
    lines
}

/// `tel:` dials digits, not the human spacing a number is written with.
/// `pub(crate)` so the litigation page uses the same dialable format.
pub(crate) fn tel_href(phone: &str) -> String {
    format!(
        "tel:{}",
        phone
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '+')
            .collect::<String>()
    )
}

/// The footer legal strip. Every field is resolved from the deploy's brand and
/// handed in per request, so the component itself is pure presentation.
///
/// - `copyright_holder`: the legal person that owns the site and the words on
///   it — `Shook Law PC` on the firm's deploy. It heads the legal strip, and
///   it is the only line naming the entity behind the site. Deliberately a
///   separate dial from the wordmark the page trades under, even where a
///   deploy sets both to the same words: a copyright notice has to name an
///   entity that can hold one, and a white-label bundle renames the wordmark
///   without renaming the copyright holder.
/// - `disclaimer`: the attorney-advertising disclaimer copy.
/// - `copyright_year`: the year on the copyright line, resolved by the
///   server per request so the reusable component never carries a stale year.
/// - `contact_email` / `phone` / `offices`: the firm's published contact
///   channels. Each is independently optional — an empty value renders
///   nothing, so a deploy that publishes no voice line or no walk-in office
///   simply omits it rather than rendering an empty label. With all three
///   empty the contact band disappears and only the legal strip remains.
/// - `attorneys`: the licensed attorneys and the bar licences each holds, one
///   line per attorney, every number linked to that bar's own record. Empty
///   renders no licence list.
/// - `logo_href` / `brand_name`: the mark the footer opens on and the wordmark
///   beside it. `crate::public_chrome::PublicFooter` feeds both from the firm
///   brand on every page of the site, so the bottom of a page names one
///   organization whichever header a white-label bundle mounts above it.
/// - `home_href`: where that mark links. A reader who has scrolled to the
///   bottom of a long page clicks the logo to get back to the top of the site,
///   and the header's mark is off screen by then — so the footer's has to be
///   the same door rather than an inert picture. It is the *firm's* home under
///   any header, matching the mark and wordmark beside it.
///   Empty renders the mark unlinked, which is what a gallery driving the
///   component with no destination should get rather than an `<a href="">`
///   that reloads whatever page it is sitting on.
/// - `nav`: the public routes the header does not carry, rendered as one list
///   the stylesheet lays out in columns on a wide viewport and one column on
///   a narrow one. Empty renders no row.
///
/// The whole footer is width-constrained to the same 72rem column the
/// [`crate::components::SiteHeader`] nav uses, so its content lines up with
/// the navbar above it instead of running to the viewport edge. The rule and
/// background stay full-bleed, matching the header's `border-bottom`.
#[component]
pub fn SiteFooterLegal(
    copyright_holder: String,
    #[props(default)] disclosure: FooterDisclosure,
    #[props(default)] copyright_href: String,
    disclaimer: String,
    copyright_year: i32,
    #[props(default)] logo_href: String,
    #[props(default)] contact_email: String,
    #[props(default)] phone: String,
    #[props(default)] offices: Vec<FooterOffice>,
    #[props(default)] attorneys: Vec<FooterAttorney>,
    #[props(default)] brand_name: String,
    /// "A practice of Shook Law PC", rendered above the legal strip on a
    /// brand site.
    ///
    /// This is what keeps a trade name from being misleading. Nevada permits
    /// a firm to trade under a name that is not its own, and *disclosed
    /// affiliation* is the thing doing that work — a visitor on
    /// `vestaestateplanning.com` has to be able to see which licensed entity
    /// they would actually be retaining. So it is a regulatory line, not
    /// footer decoration, and it renders on every page rather than only the
    /// home page. Empty on the firm's own site, where the masthead already
    /// names the firm.
    #[props(default)]
    attribution: String,
    #[props(default)] home_href: String,
    #[props(default)] nav: Vec<FooterNavLink>,
    /// The house brands this firm trades under — the "Our Family" row, in
    /// registry order, the current one unlinked. One entry is no row: a
    /// single-brand firm has no family to name, and stays byte-identical to
    /// a footer that never learned the list.
    #[props(default)]
    brands: Vec<FooterBrandLink>,
    /// The associations the firm has a public affiliation with, one standing
    /// line per association. Empty renders no line — a white-label deploy
    /// cannot claim this repository's affiliations.
    #[props(default)]
    memberships: Vec<FooterMembership>,
    /// The registered word mark the site trades under, spelled the way the
    /// register spells it, and the registration that proves it. Renders one
    /// notice under the copyright line — the site's two ownership facts read
    /// together — naming the mark, the registrant, and the number, with the
    /// number linked to the register's own record so a reader verifies the
    /// claim there rather than trusting the site's line about itself. That is
    /// the same rule the bar-licence rows below follow.
    ///
    /// A supplied ownership disclosure replaces this generic notice. Without
    /// one, the generic footer names its copyright holder as the mark owner.
    #[props(default)]
    trademark: String,
    #[props(default)] trademark_registration: String,
    #[props(default)] trademark_record_url: String,
    /// The version this deployment is running, plain text right after the
    /// linked product name — "Powered by Neon Law Navigator 26.9.27" closes
    /// the strip, one line and no attribution prose. Empty renders no version
    /// at all, which is the ordinary local case: `NAVIGATOR_RELEASE_TAG` is
    /// unset under `cargo run`, and a footer inventing one would be worse
    /// than a footer that names the software and stops there.
    #[props(default)]
    navigator_version: String,
    /// Where "Neon Law Navigator" links — the page that describes the
    /// platform. Empty renders the name unlinked rather than an `<a
    /// href="">` that reloads whatever page it sits on.
    #[props(default)]
    navigator_href: String,
    /// Last known star count for Navigator's public source repository.
    #[props(default)]
    github_stars: Option<u64>,
    /// The firm's own profiles, drawn under the wordmark. Empty renders no
    /// row — a deploy that renamed the firm publishes none, because the
    /// accounts belong to this firm.
    #[props(default)]
    social: Vec<FooterSocialLink>,
) -> Element {
    let has_contact = !contact_email.is_empty() || !phone.is_empty() || !offices.is_empty();
    let has_masthead =
        !logo_href.is_empty() || !brand_name.is_empty() || !nav.is_empty() || !social.is_empty();
    // The mark's contents, built once: the element around them is an anchor or
    // a plain box depending on whether the deploy published a home to link to,
    // and writing the image and wordmark out under each branch is how the two
    // drift apart.
    let mark = rsx! {
        if !logo_href.is_empty() {
            img { class: "site-footer__logo", src: "{logo_href}", alt: "" }
        }
        if !brand_name.is_empty() {
            strong { class: "site-footer__wordmark", "{brand_name}" }
        }
    };
    rsx! {
        footer { class: "site-footer", role: "contentinfo",
            div { class: "site-footer__inner",
                // The mark, the name it belongs to, and the link row, in one
                // element so they share one grid — and so that grid can be the
                // contact band's own, which is what lines the first link column
                // up with the first address tile below it. Two separate
                // children of `__inner` could not: the band insets its tiles by
                // its own padding, so a track measured outside the band never
                // lands where a track measured inside it does.
                if has_masthead {
                    div { class: "site-footer__masthead",
                        // The logo alone left the footer opening on an
                        // unlabelled glyph; the wordmark is the same one the
                        // header carries, so the bottom of the page says whose
                        // page it is. The image stays decorative (`alt=""`)
                        // because the text beside it is the label.
                        if !logo_href.is_empty() || !brand_name.is_empty() || !social.is_empty() {
                            // The profiles sit in the same cell as the mark, so
                            // the page list keeps the columns it lines up with
                            // the address tiles. A second grid item here would
                            // land on top of that list.
                            div { class: "site-footer__identity",
                                if !logo_href.is_empty() || !brand_name.is_empty() {
                                    // Linked when there is a home to link to, and the
                                    // anchor carries the accessible name for the same
                                    // reason the header's does: the image is decorative
                                    // and the wordmark beside it is the label, so a
                                    // screen reader announcing both would say the brand
                                    // twice.
                                    if home_href.is_empty() {
                                        div { class: "site-footer__brand", {mark} }
                                    } else {
                                        a {
                                            class: "site-footer__brand",
                                            href: "{home_href}",
                                            "aria-label": "{brand_name} home",
                                            {mark}
                                        }
                                    }
                                }
                                if !social.is_empty() {
                                    nav { class: "site-footer__social", "aria-label": "{brand_name} social profiles",
                                        ul { class: "site-footer__social-list",
                                            for link in social.iter() {
                                                li {
                                                    class: "site-footer__social-item",
                                                    key: "{link.href}",
                                                    ExternalLink {
                                                        class: "site-footer__social-link".to_string(),
                                                        href: link.href.clone(),
                                                        if let Some(icon) = social_icon(&link.network) {
                                                            Icon { name: icon }
                                                        }
                                                        span { "{link.label}" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // The pages the header does not carry, as one list of
                        // the firm's own routes, so a reader who scrolled to the
                        // bottom looking for the Blog, Contact, or the platform
                        // page finds them before the contact band's detail.
                        //
                        // A list, because that is what it is: a set of sibling
                        // destinations a screen reader should announce with a
                        // count. One `<ul>` in the markup at every width — the
                        // two columns a wide viewport shows are the stylesheet
                        // flowing this single list down five rows and across, so
                        // the reading order and the announced count are the same
                        // everywhere.
                        if !nav.is_empty() {
                            nav { class: "site-footer__nav", "aria-label": "More pages",
                                ul { class: "site-footer__nav-list",
                                    for link in nav.iter() {
                                        li { class: "site-footer__nav-item", key: "{link.href}",
                                            // An entry that leaves this site
                                            // wears the same off-site arrow
                                            // every other outbound link on the
                                            // page does, so a reader sees it
                                            // is leaving before they click —
                                            // `href.starts_with("http")` is
                                            // the row's own test for that.
                                            if link.href.starts_with("http") {
                                                ExternalLink {
                                                    class: "site-footer__nav-link".to_string(),
                                                    href: link.href.clone(),
                                                    "{link.label}"
                                                }
                                            } else {
                                                a {
                                                    class: "site-footer__nav-link",
                                                    href: "{link.href}",
                                                    "{link.label}"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if has_contact {
                    div { class: "site-footer__contact",
                        // The published email address and dialable phone sit
                        // beside the offices, both as tiles that respond to
                        // pointer and keyboard interaction.
                        ul { class: "site-footer__offices",
                            if !contact_email.is_empty() {
                                li { class: "site-footer__office site-footer__office--channel",
                                    a {
                                        class: "site-footer__channel-link",
                                        href: "mailto:{contact_email}",
                                        Icon { name: IconName::EnvelopeFill }
                                        span { "{contact_email}" }
                                    }
                                }
                            }
                            if !phone.is_empty() {
                                li { class: "site-footer__office site-footer__office--channel",
                                    a {
                                        class: "site-footer__channel-link",
                                        href: tel_href(&phone),
                                        Icon { name: IconName::TelephoneFill }
                                        span { "{phone}" }
                                    }
                                }
                            }
                            for office in offices.iter() {
                                li { class: "site-footer__office", key: "{office.state}",
                                        // The state behind its own address, as
                                        // a watermark. Decorative and
                                        // `aria-hidden`: the label above it
                                        // already names the state, so a screen
                                        // reader that announced this too would
                                        // hear the office twice.
                                        if let Some(outline) = state_outline(&office.state) {
                                            svg {
                                                class: "site-footer__office-map",
                                                "viewBox": "0 0 100 100",
                                                "aria-hidden": "true",
                                                "focusable": "false",
                                                fill: "currentColor",
                                                path { d: outline }
                                            }
                                        }
                                        span { class: "site-footer__office-label", "{office.state}" }
                                        // `<address>` is the semantic element
                                        // for the contact details of its
                                        // nearest ancestor section — here, the
                                        // firm that owns the page. Set line by
                                        // line — street, unit, city — the way
                                        // the envelope would carry it.
                                        address { class: "site-footer__office-address",
                                            for line in address_lines(&office.address) {
                                                span {
                                                    class: "site-footer__office-line",
                                                    key: "{line}",
                                                    "{line}"
                                                }
                                            }
                                        }
                                        // The qualification sits inside the
                                        // same `<li>` as the address it
                                        // qualifies, so no reader can pair it
                                        // with the wrong office.
                                        if let Some(note) = office.note.as_ref() {
                                            span { class: "site-footer__office-note", "{note}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                // The firm's affiliations, in one row between the contact
                // band and the legal strip: the house brands it trades under
                // on the left, the association affiliations on the right.
                // Both are facts about the firm rather than about a brand —
                // the same family and the same affiliation on every host — so
                // they sit with the firm's contact detail above and its legal
                // strip below, not beside a brand's header.
                if brands.len() > 1 || !memberships.is_empty() {
                    div { class: "site-footer__affiliations",
                        // "Our Family": every house brand the firm wears, the
                        // current one shown but not linked, the rest linking
                        // their own homes. A `<nav>`, because it is a set of
                        // destinations, named by the same words the visible
                        // heading carries so a landmark list reads what a
                        // sighted reader sees. `aria-label` rather than
                        // `aria-labelledby`: a page that renders this footer
                        // twice (the `/design` gallery does) would otherwise
                        // duplicate the heading's id.
                        if brands.len() > 1 {
                            nav { class: "site-footer__family", "aria-label": "Our family",
                                h2 { class: "site-footer__family-heading", "Our Family" }
                                ul { class: family_list_class(brands.len()),
                                    for brand in brands.iter() {
                                        li { class: "site-footer__family-item", key: "{brand.label}",
                                            // The brand a reader is already on is
                                            // text, not a link to the page they are
                                            // reading, and `aria-current` says so
                                            // the way the header marks the current
                                            // page. A brand with no host yet is text
                                            // too, rather than an `<a href="">` that
                                            // reloads whatever page it sits on.
                                            if brand.current {
                                                span {
                                                    class: "site-footer__family-current",
                                                    "aria-current": "true",
                                                    if !brand.logo_href.is_empty() {
                                                        img { class: "site-footer__family-logo", src: "{brand.logo_href}", alt: "" }
                                                    }
                                                    span { "{brand.label}" }
                                                }
                                            } else if brand.href.is_empty() {
                                                span { class: "site-footer__family-current",
                                                    if !brand.logo_href.is_empty() {
                                                        img { class: "site-footer__family-logo", src: "{brand.logo_href}", alt: "" }
                                                    }
                                                    span { "{brand.label}" }
                                                }
                                            } else {
                                                a {
                                                    class: "site-footer__family-link",
                                                    href: "{brand.href}",
                                                    target: "_blank",
                                                    rel: "noopener noreferrer",
                                                    if !brand.logo_href.is_empty() {
                                                        img { class: "site-footer__family-logo", src: "{brand.logo_href}", alt: "" }
                                                    }
                                                    span { "{brand.label}" }
                                                }
                                            }
                                            if !brand.byline.is_empty() {
                                                span {
                                                    class: "site-footer__family-byline",
                                                    " · {brand.byline}"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // One standing line per association, linking the
                        // association's own site so a reader checks the
                        // relationship against the association's published
                        // guidance. Off-site, so it wears the
                        // same new-tab treatment every outbound link on the
                        // page does. The award glyph is decorative: the
                        // sentence beside it is the whole meaning.
                        if !memberships.is_empty() {
                            ul { class: "site-footer__memberships",
                                for membership in memberships.iter() {
                                    li { class: "site-footer__membership", key: "{membership.href}",
                                        // The association's own mark, on a
                                        // dark tile. The standing line is
                                        // short ("Mission-Aligned Partner"),
                                        // so the mark is what names the
                                        // association, and its alt text says
                                        // so; without a mark, the line spells
                                        // the name out itself.
                                        if membership.logo_href.is_empty() {
                                            Icon { name: IconName::AwardFill }
                                            ExternalLink {
                                                class: "site-footer__membership-link".to_string(),
                                                href: membership.href.clone(),
                                                "{membership.standing}, {membership.label}"
                                            }
                                        } else {
                                            span { class: "site-footer__membership-badge",
                                                img {
                                                    class: "site-footer__membership-logo",
                                                    src: "{membership.logo_href}",
                                                    alt: "{membership.label}",
                                                    loading: "lazy",
                                                }
                                            }
                                            ExternalLink {
                                                class: "site-footer__membership-link".to_string(),
                                                href: membership.href.clone(),
                                                "{membership.standing}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "site-footer__legal",
                    p { class: "site-footer__disclaimer", "{disclaimer}" }
                    // Above the ownership lines, not inside them: a reader
                    // looking for who they are dealing with should not have to
                    // parse the copyright line to find it.
                    if !attribution.is_empty() {
                        p { class: "site-footer__attribution", "{attribution}" }
                    }
                    div { class: "site-footer__legal-practice",
                        p { class: "site-footer__copyright",
                        // The site and the words on it belong to the firm's
                        // legal person, which is what a copyright notice names
                        // — the wordmark cannot hold one. This heads the legal
                        // strip rather than trailing it, because it is the only
                        // line naming the entity behind the site. One name: the
                        // wordmark it trades under is named on the line below,
                        // as the mark this same person registered.
                        "© {copyright_year} "
                        if copyright_href.is_empty() {
                            "{copyright_holder}"
                        } else {
                            a { href: "{copyright_href}", "{copyright_holder}" }
                        }
                    }
                    // The other ownership fact, directly under the first: the
                    // wordmark this footer opens on is a registered mark, and
                    // this is who holds it and under what number. It sits above
                    // the bar licences because it is a property notice about
                    // the site rather than regulated attorney copy, and it
                    // links out for the same reason those rows do — a reader
                    // checks the register, not the page's word for it.
                    if !trademark.is_empty() && disclosure.ownership.is_empty() {
                        p { class: "site-footer__trademark",
                            "{trademark}"
                            if trademark_registration.is_empty() {
                                sup { class: "site-footer__trademark-mark", "™" }
                                " is a common-law mark of {copyright_holder}"
                            } else {
                                // `line-height: 0` in the stylesheet keeps the
                                // superscript from stretching the fine print's
                                // line box; the glyph is part of the mark, not
                                // decoration, so it stays in the text.
                                sup { class: "site-footer__trademark-mark", "®" }
                                " is a registered trademark of {copyright_holder}, "
                                if trademark_record_url.is_empty() {
                                    "U.S. Reg. No. {trademark_registration}"
                                } else {
                                    ExternalLink {
                                        href: trademark_record_url.clone(),
                                        class: "link-secondary".to_string(),
                                        "U.S. Reg. No. {trademark_registration}"
                                    }
                                }
                            }
                        }
                    }
                    // Who is licensed, where, and under what number — each
                    // linked to the bar's own record so a visitor can verify
                    // the licence rather than take the site's word for it. This
                    // is the footer's only bar disclosure: a firm-level
                    // "Admitted in …" line said nothing these rows do not, in
                    // the jurisdictions they already name.
                        if !attorneys.is_empty() {
                            ul { class: "site-footer__licenses",
                                for attorney in attorneys.iter() {
                                    li { class: "site-footer__licensee", key: "{attorney.name}",
                                        span { class: "site-footer__licensee-name", "{attorney.name}" }
                                        " — "
                                        for (index, license) in attorney.licenses.iter().enumerate() {
                                            if index > 0 {
                                                " · "
                                            }
                                            ExternalLink {
                                                href: license.license_url.clone(),
                                                class: "link-secondary".to_string(),
                                                "{license.jurisdiction} Bar No. {license.number}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // One line names the software this page runs and, right
                    // after it, the release serving here: "Powered by", the
                    // linked product name, and the version as plain text —
                    // one line, with the public source repository below.
                    //
                    // `navigator_href` empty renders the name unlinked rather
                    // than an `<a href="">` that reloads the page; the version
                    // is independent of it: `NAVIGATOR_RELEASE_TAG` is unset
                    // under `cargo run`, so a local build names the software
                    // and stops there.
                    div { class: "site-footer__legal-platform",
                        p { class: "site-footer__powered",
                            if navigator_href.is_empty() {
                                "{POWERED_BY_NEON_LAW_NAVIGATOR}"
                            } else {
                                "Powered by "
                                a { class: "site-footer__release", href: "{navigator_href}",
                                    "Neon Law Navigator"
                                }
                            }
                            if !navigator_version.is_empty() {
                                " {navigator_version}"
                            }
                        }
                    }
                    p { class: "site-footer__source",
                        crate::components::GitHubStars {
                            href: crate::source_repository::SOURCE_HREF.to_string(),
                            repo: crate::source_repository::SOURCE_REPO.to_string(),
                            stars: github_stars,
                        }
                    }
                    if !disclosure.provider.is_empty() {
                        p { class: "site-footer__provider",
                            "{disclosure.prefix} "
                            ExternalLink { href: disclosure.href.clone(), "{disclosure.provider}" }
                        }
                    }
                    if !disclosure.ownership.is_empty() {
                        p { class: "site-footer__ownership", "{disclosure.ownership}"
                            if !trademark_registration.is_empty() && !trademark_record_url.is_empty() {
                                " "
                                ExternalLink { class: "link-secondary".to_string(), href: trademark_record_url.clone(), "U.S. Reg. No. {trademark_registration}" }
                            }
                        }
                    }
                    p { class: "site-footer__tagline", "{FOOTER_TAGLINE}" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn provider_and_owner_are_independent_of_the_operating_firm() {
        fn app() -> Element {
            rsx! { SiteFooterLegal {
                copyright_holder: "Example Practice PC".to_string(),
                copyright_year: 2026,
                disclaimer: String::new(),
                trademark: "NEON LAW".to_string(),
                disclosure: FooterDisclosure {
                    prefix: "Legal services prepared by".into(),
                    provider: "Lex Tecnica".into(),
                    href: "https://www.lextecnica.com/".into(),
                    ownership: "Neon Law is a trademark of Neon Law IP LLC.".into(),
                },
            } }
        }
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        assert!(html.contains("© 2026 Example Practice PC"), "{html}");
        assert!(html.contains("https://www.lextecnica.com/"), "{html}");
        let provider = html.find("Legal services prepared by").expect("provider");
        let owner = html
            .find("Neon Law is a trademark of Neon Law IP LLC.")
            .expect("owner");
        let mission = html.find(FOOTER_TAGLINE).expect("mission");
        assert!(provider < owner && owner < mission, "{html}");
        assert!(!html.contains("trademark of Example Practice PC"), "{html}");
    }
    use super::*;
    use views::brand::JTA_STANDING;

    fn ssr(app: fn() -> Element) -> String {
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    /// The link row a deploy hands this component, mirroring what
    /// `views::brand::firm_footer_nav` publishes: the twelve public routes the
    /// header does not carry, alphabetized by label.
    const FOOTER_ROW: [(&str, &str); 12] = [
        ("API", "/api"),
        ("Blog", "/blog"),
        ("Contact", "/contact"),
        ("Design", "/design"),
        ("Glossary", "/glossary"),
        ("Navigator", "/navigator"),
        ("Notations", "/notations"),
        ("Presentations", "/presentations"),
        ("Privacy", "/privacy"),
        ("Team", "/team"),
        ("Terms", "/terms"),
        ("Testimonials", "/testimonials"),
    ];

    /// The firm's legal strip: the copyright line that names the entity, the
    /// attorney-advertising disclaimer, and nothing else the deploy did not
    /// hand in.
    fn legal_html() -> String {
        fn app() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Neon Law".to_string(),
                    disclaimer: "Attorney advertisement. Nothing here is legal advice without a signed retainer for an active project. Past results do not guarantee future outcomes.".to_string(),
                    copyright_year: 2026,
                    trademark: "NEON LAW".to_string(),
                    trademark_registration: "6,325,650".to_string(),
                    trademark_record_url:
                        "https://tmsearch.uspto.gov/search/search-results/90039224".to_string(),
                    github_stars: 1234u64,
                    navigator_version: "26.8.20".to_string(),
                    navigator_href: "https://www.neonlaw.com/navigator".to_string(),
                }
            }
        }
        ssr(app)
    }

    /// The mark notice names the mark, the registrant, and the number, and
    /// links the number to the register's own record.
    ///
    /// Order is the substance: the two ownership facts read together, so the
    /// notice follows the copyright and precedes the bar licences.
    #[test]
    fn notices_the_registered_mark_under_the_copyright() {
        let out = legal_html();
        assert!(
            out.contains("NEON LAW") && out.contains("®"),
            "the mark renders as the register spells it, with its symbol: {out}"
        );
        assert!(
            out.contains("is a registered trademark of Neon Law"),
            "and names the registrant the copyright line names: {out}"
        );
        assert!(
            out.contains("U.S. Reg. No. 6,325,650"),
            "the registration is the claim: {out}"
        );
        assert!(
            out.contains(r#"href="https://tmsearch.uspto.gov/search/search-results/90039224""#),
            "linked to the register's own record: {out}"
        );
        let copyright = out.find("site-footer__copyright").expect("the copyright");
        let mark = out.find("site-footer__trademark").expect("the mark notice");
        let licences = out.find("site-footer__licenses").unwrap_or(usize::MAX);
        assert!(
            copyright < mark && mark < licences,
            "the ownership facts read together, above the bar rows: {out}"
        );
    }

    #[test]
    fn ends_the_legal_strip_with_the_tagline_after_the_disclaimer_and_brands() {
        let out = legal_html();
        let legal = out
            .split(r#"<div class="site-footer__legal">"#)
            .nth(1)
            .expect("the legal strip renders");
        let disclaimer = legal
            .find("Attorney advertisement")
            .expect("the disclaimer renders");
        let copyright = legal
            .find("site-footer__copyright")
            .expect("the copyright renders");
        let trademark = legal
            .find("site-footer__trademark")
            .expect("the trademark renders");
        let platform = legal
            .find("site-footer__legal-platform")
            .expect("the platform renders");
        let tagline = legal
            .rfind(FOOTER_TAGLINE)
            .expect("the final tagline renders");
        assert!(disclaimer < copyright && copyright < trademark && trademark < platform);
        assert!(platform < tagline);
        assert_eq!(legal.matches(FOOTER_TAGLINE).count(), 1);
    }

    /// A deploy holding no registration notices none, and one that cites a
    /// number with no public record renders the claim unlinked rather than
    /// pointing at nothing.
    #[test]
    fn omits_the_mark_notice_when_unregistered_and_the_link_when_unrecorded() {
        fn unregistered() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Cascade Law LLP".to_string(),
                    disclaimer: "This is an attorney advertisement.".to_string(),
                    copyright_year: 2026,
                }
            }
        }
        fn unrecorded() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Neon Law".to_string(),
                    disclaimer: "This is an attorney advertisement.".to_string(),
                    copyright_year: 2026,
                    trademark: "NEON LAW".to_string(),
                    trademark_registration: "6,325,650".to_string(),
                }
            }
        }
        fn common_law() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Shook Law PC".to_string(),
                    disclaimer: "This is an attorney advertisement.".to_string(),
                    copyright_year: 2026,
                    trademark: "LAWYER SHOOK".to_string(),
                }
            }
        }

        let out = ssr(unregistered);
        assert!(
            !out.contains("site-footer__trademark") && !out.contains("registered trademark"),
            "no registration, no notice: {out}"
        );

        let out = ssr(unrecorded);
        assert!(
            out.contains("U.S. Reg. No. 6,325,650"),
            "the claim still renders: {out}"
        );
        assert!(
            !out.contains("tmsearch"),
            "with no link to a record it does not have: {out}"
        );

        let out = ssr(common_law);
        assert!(out.contains("LAWYER SHOOK") && out.contains("™"), "{out}");
        assert!(out.contains("common-law mark of Shook Law PC"), "{out}");
        assert!(!out.contains("®") && !out.contains("U.S. Reg."), "{out}");
    }

    /// The platform line names the software, links it to the page describing
    /// it, and states the running release — and it closes the strip, below
    /// the disclaimer.
    ///
    /// Order is the substance here, as it is for every other line in this
    /// strip: a reader meets the bar records and the advertising disclaimer
    /// before the page mentions what serves it.
    #[test]
    fn closes_the_strip_with_the_navigator_attribution() {
        let out = legal_html();
        assert!(
            out.contains(
                r#"<a class="site-footer__release" href="https://www.neonlaw.com/navigator">Neon Law Navigator</a>"#
            ),
            "the product name links to the page describing the platform: {out}"
        );
        assert!(
            out.contains("Neon Law Navigator</a> 26.8.20"),
            "the running release reads right after the product name: {out}"
        );
        assert!(
            out.contains("https://github.com/neon-law/navigator")
                && out.contains("1,234")
                && out.contains("<title>GitHub stars</title>"),
            "the shared footer links the public source and names its star count: {out}"
        );
        let disclaimer = out.find("Attorney advertisement").expect("the disclaimer");
        let platform = out.find("site-footer__powered").expect("the platform line");
        assert!(
            disclaimer < platform,
            "the platform line closes the strip, under the disclaimer: {out}"
        );
    }

    /// "Powered by", the linked product name, and the running release are one
    /// line — one `<p>`, with exactly one link and no `#` prefix on the
    /// version.
    ///
    /// The single line is the assertion. The two facts name one thing between
    /// them — this software, at this build — so they belong in one glance;
    /// a second link on the version is the shape this component used to take
    /// and must not regress to.
    #[test]
    fn collapses_the_platform_attribution_onto_one_line() {
        let out = contactable_html();
        let line = out
            .split(r#"<p class="site-footer__powered">"#)
            .nth(1)
            .and_then(|rest| rest.split("</p>").next())
            .expect("the platform line renders");
        assert_eq!(
            out.matches(r#"class="site-footer__powered""#).count(),
            1,
            "one line carries the whole attribution: {out}"
        );
        assert_eq!(
            line.matches("<a ").count(),
            1,
            "exactly one link — the product name, never a second one on the version: {line}"
        );
        assert!(!line.contains('#'), "no # prefix on the version: {line}");

        let powered = line.find("Powered by").expect("the attribution opens it");
        let product_link = line
            .find(r#"class="site-footer__release""#)
            .expect("the linked product name");
        let version = line.find("26.8.20").expect("the version");
        assert!(
            powered < product_link && product_link < version,
            "it reads Powered by, the linked product name, then the release: {line}"
        );
    }

    /// With no release stamp, the footer still names the software and links
    /// it — it just omits the version, rather than rendering a bare "#".
    /// With nowhere to link either, it falls back to the shared unlinked
    /// wording.
    #[test]
    fn omits_the_version_when_unstamped_and_falls_back_when_unlinked() {
        fn linked_but_unstamped() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Neon Law".to_string(),
                    disclaimer: "This is an attorney advertisement.".to_string(),
                    copyright_year: 2026,
                    navigator_href: "https://www.neonlaw.com/navigator".to_string(),
                }
            }
        }
        fn neither() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Neon Law".to_string(),
                    disclaimer: "This is an attorney advertisement.".to_string(),
                    copyright_year: 2026,
                }
            }
        }

        let out = ssr(linked_but_unstamped);
        let line = out
            .split(r#"<p class="site-footer__powered">"#)
            .nth(1)
            .and_then(|rest| rest.split("</p>").next())
            .expect("the platform line renders");
        assert!(
            line.ends_with("</a>"),
            "the product name still links, with no version after it: {line}"
        );

        let bare = ssr(neither);
        assert!(
            bare.contains("site-footer__legal-platform"),
            "the platform region carries the powered-by line: {bare}"
        );
        assert!(
            bare.contains(POWERED_BY_NEON_LAW_NAVIGATOR),
            "with nowhere to link, the shared unlinked wording carries the line: {bare}"
        );
        assert!(!bare.contains(r#"href="""#), "no empty anchor: {bare}");
    }

    /// The copyright names one entity: the firm's legal person, and nobody
    /// else.
    ///
    /// It used to name a second organization beside it, when the site served a
    /// nonprofit's pages as well as the firm's. That surface is retired, and a
    /// copyright line crediting an organization whose pages the site no longer
    /// publishes would be a claim about ownership that is no longer true.
    #[test]
    fn the_copyright_names_the_firms_legal_person_alone() {
        let out = legal_html();
        let copyright = out
            .split(r#"<p class="site-footer__copyright">"#)
            .nth(1)
            .and_then(|rest| rest.split("</p>").next())
            .expect("the copyright line renders");
        assert!(
            copyright.contains("\u{a9} 2026 Neon Law"),
            "the copyright names the holding entity: {copyright}"
        );
        assert!(
            !copyright.contains("Foundation"),
            "the copyright names one organization: {copyright}"
        );
        assert!(
            !copyright.contains(" and "),
            "and names it without a second: {copyright}"
        );
    }

    /// The copyright heads the legal strip and there is exactly one of it.
    ///
    /// Asserted on the copyright element rather than on the entity's name. The
    /// name now appears twice on purpose: the supporter line at the foot of the
    /// page says "Neon Law is a proud supporter of …", which is the
    /// firm's own wording. What must not recur is the *copyright*, which once
    /// trailed a firm-level attribution line saying the same thing.
    #[test]
    fn renders_the_copyright_once_at_the_head_of_the_strip() {
        let out = legal_html();
        assert_eq!(
            out.matches(r#"class="site-footer__copyright""#).count(),
            1,
            "one copyright line: {out}"
        );
        let legal = out
            .find(r#"<div class="site-footer__legal""#)
            .expect("the legal strip renders");
        let copyright = out
            .find(r#"<p class="site-footer__copyright""#)
            .expect("the copyright renders");
        let disclaimer = out
            .find(r#"<p class="site-footer__disclaimer""#)
            .expect("the disclaimer renders");
        assert!(
            legal < disclaimer && disclaimer < copyright,
            "the disclaimer precedes the copyright: {out}"
        );
    }

    /// The firm-level "Admitted in California \u{b7} Washington \u{b7} Nevada" line is
    /// gone. A jurisdiction is published per attorney, with the bar number, by
    /// the licence list below — never as a firm-wide claim.
    #[test]
    fn publishes_no_firm_level_admissions_line() {
        let out = legal_html();
        assert!(
            !out.contains("Admitted in"),
            "the firm-level admissions line is retired: {out}"
        );
        let licensed = contactable_html();
        assert!(
            licensed.contains("Nevada Bar No."),
            "the jurisdiction is still published as an attorney licence: {licensed}"
        );
    }

    /// A footer carrying every contact channel: the CTA, the voice line, the
    /// offices, and the per-attorney bar licenses.
    fn contactable_html() -> String {
        fn app() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Neon Law".to_string(),
                    disclaimer: "This is an attorney advertisement.".to_string(),
                    copyright_year: 2026,
                    logo_href: "/public/logo.svg".to_string(),
                    brand_name: "Neon Law".to_string(),
                    contact_email: "support@neonlaw.com".to_string(),
                    phone: "+1 510 800 2090".to_string(),
                    // The real addresses, as `views::brand` publishes them: a
                    // suite is its own comma-separated part, which is what the
                    // footer breaks a line on.
                    offices: [
                        ("California", "1990 N California Blvd, Ste 800, Walnut Creek, CA 94596", None),
                        ("Nevada", "5150 Mae Anne Ave, Ste 405-9777, Reno, NV 89523", None),
                        (
                            "New York",
                            "12 E 49th St, 18th Floor, New York, NY 10017",
                            Some("Bar admission pending"),
                        ),
                        ("Washington", "720 Seneca St, Ste 107-715, Seattle, WA 98101", None),
                    ]
                    .into_iter()
                    .map(|(state, address, note)| FooterOffice {
                        state: state.to_string(),
                        address: address.to_string(),
                        note: note.map(str::to_string),
                    })
                    .collect(),
                    attorneys: vec![
                        FooterAttorney {
                            name: "Nicholas Richard Shook".to_string(),
                            licenses: vec![FooterBarLicense {
                                jurisdiction: "Nevada".to_string(),
                                number: "13400".to_string(),
                                license_url: "https://nvbar.org/find-a-lawyer/?usearch=13400"
                                    .to_string(),
                            }],
                        },
                    ],
                    nav: FOOTER_ROW
                        .into_iter()
                        .map(|(label, href)| FooterNavLink {
                            label: label.to_string(),
                            href: href.to_string(),
                        })
                        .collect(),
                    navigator_version: "26.8.20".to_string(),
                    navigator_href: "https://www.neonlaw.com/navigator".to_string(),
                }
            }
        }
        ssr(app)
    }

    /// The footer's content sits in the same width-capped column the header nav
    /// uses, so it lines up with the navbar rather than running to the edge.
    #[test]
    fn wraps_its_content_in_the_navbar_aligned_column() {
        let out = contactable_html();
        assert!(
            out.contains(r#"<footer class="site-footer""#),
            "the rule/background element is the outer footer: {out}"
        );
        let inner = out
            .find(r#"<div class="site-footer__inner""#)
            .expect("the width-capped column is present");
        let legal = out.find("site-footer__legal").expect("legal strip present");
        assert!(
            inner < legal,
            "the legal strip sits inside the column: {out}"
        );
        for region in ["site-footer__legal-practice", "site-footer__legal-platform"] {
            assert!(
                out.contains(region),
                "the legal copy has a {region} region: {out}"
            );
        }
    }

    #[test]
    fn renders_the_contact_cta_and_voice_line() {
        let out = contactable_html();
        assert!(
            out.contains(r#"class="site-footer__logo" src="/public/logo.svg" alt="""#),
            "the supplied brand mark renders as decorative footer identity: {out}"
        );
        assert!(
            out.contains("support@neonlaw.com")
                && out.contains(r#"href="mailto:support@neonlaw.com""#),
            "the address is a link, like the voice line: {out}"
        );
        // `tel:` dials digits only — the human spacing would not dial.
        assert!(
            out.contains(r#"href="tel:+15108002090""#),
            "the voice line is dialable: {out}"
        );
        assert!(
            out.contains("+1 510 800 2090"),
            "the number is shown as written: {out}"
        );
    }

    /// Each office is labelled by its state and they render in the given
    /// order. The assertion anchors on the street addresses rather than on the
    /// labels: "Walnut Creek" and "Seattle" still occur inside their own
    /// addresses, so a label-only check would pass even if the labels had never
    /// changed.
    #[test]
    fn lists_every_office_in_order() {
        let out = contactable_html();
        let positions: Vec<usize> = [
            "1990 N California Blvd",
            "5150 Mae Anne Ave",
            "12 E 49th St",
            "720 Seneca St",
        ]
        .iter()
        .map(|address| {
            out.find(address)
                .unwrap_or_else(|| panic!("{address} present: {out}"))
        })
        .collect();
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "offices render in the given order: {out}"
        );
        for label in ["California", "Nevada", "New York", "Washington"] {
            assert!(
                out.contains(&format!(r#"class="site-footer__office-label">{label}<"#)),
                "{label} labels its office: {out}"
            );
        }
    }

    /// Each address is set line by line — street, unit, then city with its state
    /// and ZIP — so the suite has its own line and the city starts one instead
    /// of landing wherever the narrow footer column happened to wrap.
    #[test]
    fn sets_each_address_over_a_street_a_unit_and_a_city_line() {
        let out = contactable_html();
        for (street, unit, city) in [
            (
                "1990 N California Blvd",
                "Ste 800",
                "Walnut Creek, CA 94596",
            ),
            ("5150 Mae Anne Ave", "Ste 405-9777", "Reno, NV 89523"),
            ("12 E 49th St", "18th Floor", "New York, NY 10017"),
            ("720 Seneca St", "Ste 107-715", "Seattle, WA 98101"),
        ] {
            let line =
                |text: &str| format!(r#"<span class="site-footer__office-line">{text}</span>"#);
            assert!(
                out.contains(&format!("{}{}{}", line(street), line(unit), line(city))),
                "{street} / {unit} / {city} are three lines: {out}"
            );
        }
        assert_eq!(
            out.matches("site-footer__office-line").count(),
            12,
            "three lines for each of the four offices: {out}"
        );
    }

    /// The city keeps its state and ZIP, every other comma is a break, and an
    /// address with no city to lift out publishes as the one line it was written
    /// as rather than broken at a guess.
    #[test]
    fn breaks_on_every_comma_but_the_one_holding_a_city_to_its_state() {
        assert_eq!(
            super::address_lines("1990 N California Blvd, Ste 800, Walnut Creek, CA 94596"),
            [
                "1990 N California Blvd",
                "Ste 800",
                "Walnut Creek, CA 94596"
            ],
        );
        // A firm that writes its address on one line still gets a city line.
        assert_eq!(
            super::address_lines("1 Main St, Boise, ID 83702"),
            ["1 Main St", "Boise, ID 83702"],
        );
        // Four lines above the city, if that is how the address is written.
        assert_eq!(
            super::address_lines("Attn: Mail, Bldg 4, Ste 400, 1 Main St, Boise, ID 83702"),
            [
                "Attn: Mail",
                "Bldg 4",
                "Ste 400",
                "1 Main St",
                "Boise, ID 83702"
            ],
        );
        // A city with no state beside it is still a line of its own.
        assert_eq!(
            super::address_lines("1 Main St, Boise"),
            ["1 Main St", "Boise"],
        );
        // Nothing to break: published as written.
        assert_eq!(
            super::address_lines("General Delivery"),
            ["General Delivery"]
        );
        assert!(super::address_lines("").is_empty(), "no address, no line");
    }

    /// Every published state carries its outline, and the outline is
    /// decoration rather than content: it is `aria-hidden`, so a screen reader
    /// hears the state once from the label rather than twice.
    #[test]
    fn draws_each_state_behind_its_own_office() {
        let out = contactable_html();
        assert_eq!(
            out.matches("site-footer__office-map").count(),
            4,
            "one watermark per office: {out}"
        );
        for state in ["California", "Nevada", "New York", "Washington"] {
            assert!(
                super::state_outline(state).is_some(),
                "{state} has a traced outline"
            );
        }
        // The SVG is hidden from assistive technology and unfocusable.
        assert!(
            out.contains(r#"focusable="false""#),
            "not a tab stop: {out}"
        );
        // Colour comes from the stylesheet, never from the component — the
        // outline inherits it so one token themes all four.
        assert!(
            out.contains(r#"fill="currentColor""#),
            "the outline inherits its colour: {out}"
        );
    }

    /// Every point of an outline, read out of its `d` attribute.
    fn outline_points(state: &str) -> Vec<(f64, f64)> {
        let d = super::state_outline(state).expect("the state has an outline");
        let numbers: Vec<f64> = d
            .replace(['M', 'Z'], " ")
            .split_whitespace()
            .map(|token| token.parse::<f64>().expect("a coordinate"))
            .collect();
        assert_eq!(numbers.len() % 2, 0, "{state} has whole coordinate pairs");
        numbers.chunks(2).map(|pair| (pair[0], pair[1])).collect()
    }

    /// The outlines are the real state geometry, fitted at true aspect ratio —
    /// not four silhouettes stretched to fill a square.
    ///
    /// This is the whole substance of the tracing: a border scaled unequally on
    /// its two axes stops being that state's shape no matter how faithful its
    /// corners are, and it is exactly the failure a hand-drawn replacement
    /// reintroduces. So each outline has to fill the padded box on its long
    /// axis, fall short of it on the short one, and stand the way the state
    /// actually stands.
    #[test]
    fn fits_each_outline_at_the_state_real_aspect_ratio() {
        // Portrait or landscape, as the state itself is: California and Nevada
        // run north–south, New York and Washington east–west.
        for (state, landscape) in [
            ("California", false),
            ("Nevada", false),
            ("New York", true),
            ("Washington", true),
        ] {
            let points = outline_points(state);
            let width = points.iter().map(|p| p.0).fold(f64::MIN, f64::max)
                - points.iter().map(|p| p.0).fold(f64::MAX, f64::min);
            let height = points.iter().map(|p| p.1).fold(f64::MIN, f64::max)
                - points.iter().map(|p| p.1).fold(f64::MAX, f64::min);
            assert!(
                points
                    .iter()
                    .all(|(x, y)| (0.0..=100.0).contains(x) && (0.0..=100.0).contains(y)),
                "{state} stays inside the 100×100 box"
            );
            assert!(
                (width.max(height) - 94.0).abs() < 1.0,
                "{state} fills the padded box on its long axis: {width}×{height}"
            );
            assert!(
                width.min(height) < 90.0,
                "{state} is fitted, not stretched square: {width}×{height}"
            );
            assert_eq!(
                width > height,
                landscape,
                "{state} stands the way the state does: {width}×{height}"
            );
        }
    }

    /// The routes the header does not carry live in the footer, so the footer
    /// has to actually link them. A route dropped from the nav and not picked
    /// up here is a page reachable only by typing its URL.
    #[test]
    fn links_the_pages_the_header_no_longer_carries() {
        let out = contactable_html();
        for (label, href) in FOOTER_ROW {
            assert!(
                out.contains(&format!(r#"href="{href}""#)),
                "the footer links {label}: {out}"
            );
        }
        assert!(
            out.contains(r#"aria-label="More pages""#),
            "the row is a labelled landmark: {out}"
        );
    }

    /// The row is one list, at every width.
    ///
    /// Four columns on a wide viewport and one column on a
    /// narrow one is a stylesheet job — `.site-footer__nav-list` flows down
    /// three rows and across, and the narrow layout stops columnizing it.
    /// Rendering four `<ul>`s to get the wide layout would give a screen
    /// reader four lists of three on every viewport and split the reading
    /// order down the middle, so
    /// this asserts the single list the CSS is written against.
    #[test]
    fn renders_the_row_as_one_list_of_sibling_destinations() {
        let out = contactable_html();
        let row = out
            .split_once(r#"aria-label="More pages""#)
            .and_then(|(_, rest)| rest.split_once("</nav>"))
            .map(|(row, _)| row)
            .expect("the row renders as a labelled landmark");
        assert_eq!(
            row.matches(r#"<ul class="site-footer__nav-list""#).count(),
            1,
            "one list, whatever the stylesheet does with it: {row}"
        );
        assert_eq!(
            row.matches(r#"<li class="site-footer__nav-item""#).count(),
            FOOTER_ROW.len(),
            "every destination is an item of it: {row}"
        );
    }

    /// The links are plain text, not pills.
    ///
    /// The row carries the site's remaining pages, so it reads as a directory
    /// in the footer's own muted type. The pill treatment it used to wear —
    /// a border, a radius, a hover fill — made ten quiet links look like ten
    /// buttons competing with the contact band's actual calls to action. The
    /// styling lives in `.site-footer__nav-link`, so this asserts the markup
    /// hands the stylesheet nothing else to hook.
    #[test]
    fn the_row_carries_no_styling_of_its_own() {
        let out = contactable_html();
        let row = out
            .split_once(r#"aria-label="More pages""#)
            .and_then(|(_, rest)| rest.split_once("</nav>"))
            .map(|(row, _)| row)
            .expect("the row renders as a labelled landmark");
        assert!(
            !row.contains("style="),
            "no inline styling on the row: {row}"
        );
        assert_eq!(
            row.matches(r#"class="site-footer__nav-link""#).count(),
            FOOTER_ROW.len(),
            "every link wears the one class and no variant: {row}"
        );
    }

    /// The footer directory contains only same-origin pages.
    #[test]
    fn footer_directory_has_no_external_links() {
        let out = contactable_html();
        let row = out
            .split_once(r#"aria-label="More pages""#)
            .and_then(|(_, rest)| rest.split_once("</nav>"))
            .map(|(row, _)| row)
            .expect("the row renders as a labelled landmark");
        assert_eq!(
            row.matches(r#"target="_blank""#).count(),
            0,
            "footer pages stay in this tab: {row}"
        );
        assert!(!row.contains("nav-icon"), "no off-site arrow: {row}");
    }

    /// A deploy that publishes no footer routes renders no empty row.
    #[test]
    fn omits_the_nav_row_when_unset() {
        fn app() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Neon Law".to_string(),
                    disclaimer: "This is an attorney advertisement.".to_string(),
                    copyright_year: 2026,
                }
            }
        }
        let out = ssr(app);
        assert!(!out.contains("site-footer__nav"), "no empty row: {out}");
        assert!(
            !out.contains("site-footer__social"),
            "no empty social row: {out}"
        );
    }

    fn social_profiles() -> Vec<FooterSocialLink> {
        [
            ("x", "X", "https://x.com/NeonLawUSA"),
            (
                "linkedin",
                "LinkedIn",
                "https://www.linkedin.com/company/neon-law-usa",
            ),
            (
                "youtube",
                "YouTube",
                "https://www.youtube.com/@neon-law-firm",
            ),
        ]
        .into_iter()
        .map(|(network, label, href)| FooterSocialLink {
            network: network.to_string(),
            label: label.to_string(),
            href: href.to_string(),
        })
        .collect()
    }

    /// The profiles sit under the wordmark, each named and opening off-site.
    ///
    /// The label is the accessible name. The X mark alone is not one a reader
    /// can rely on, and the off-site arrow says the link leaves the site.
    #[test]
    fn renders_the_firms_social_profiles_under_the_wordmark() {
        fn app() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Shook Law PC".to_string(),
                    disclaimer: "This is an attorney advertisement.".to_string(),
                    copyright_year: 2026,
                    brand_name: "Neon Law".to_string(),
                    home_href: "/".to_string(),
                    social: social_profiles(),
                }
            }
        }
        let out = ssr(app);
        let wordmark = out
            .find(r#"class="site-footer__wordmark""#)
            .expect("wordmark");
        let social_at = out
            .find(r#"aria-label="Neon Law social profiles""#)
            .expect("social landmark");
        assert!(
            wordmark < social_at,
            "profiles sit under the wordmark: {out}"
        );
        let social = out
            .split_once(r#"aria-label="Neon Law social profiles""#)
            .and_then(|(_, rest)| rest.split_once("</nav>"))
            .map(|(row, _)| row)
            .expect("the social row is one landmark");
        let mut previous = 0;
        for (label, href) in [
            ("X", "https://x.com/NeonLawUSA"),
            ("LinkedIn", "https://www.linkedin.com/company/neon-law-usa"),
            ("YouTube", "https://www.youtube.com/@neon-law-firm"),
        ] {
            let at = social
                .find(href)
                .unwrap_or_else(|| panic!("{href} missing: {social}"));
            assert!(at >= previous, "{label} is out of order: {social}");
            previous = at;
            assert!(
                social.contains(&format!(">{label}</span>")),
                "{label} is not named: {social}"
            );
        }
        assert_eq!(social.matches(r#"target="_blank""#).count(), 3, "{social}");
        assert_eq!(
            social.matches(r#"rel="noopener noreferrer""#).count(),
            3,
            "{social}"
        );
    }

    #[test]
    fn social_profiles_without_icons_keep_visible_accessible_labels() {
        fn app() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Example Firm".to_string(),
                    disclaimer: "Attorney advertisement.".to_string(),
                    copyright_year: 2026,
                    brand_name: "Example Brand".to_string(),
                    home_href: "/".to_string(),
                    social: vec![FooterSocialLink {
                        network: "facebook".into(),
                        label: "Facebook".into(),
                        href: "https://social.example/example-brand".into(),
                    }],
                }
            }
        }
        let out = ssr(app);
        assert!(out.contains(r#"aria-label="Example Brand social profiles""#));
        assert!(out.contains("https://social.example/example-brand"));
        assert!(out.contains(">Facebook</span>"));
        assert!(out.contains(r#"rel="noopener noreferrer""#));
    }

    /// The footer names the release serving the page, right after the linked
    /// product name, on the one line that closes the strip.
    ///
    /// This is what makes a push visible end to end: the moment a new image is
    /// live, the number at the bottom of every public page changes. The
    /// product name links the public Navigator page, so a reader who wants to
    /// know what the number refers to has somewhere to go.
    #[test]
    fn publishes_the_release_it_is_running() {
        let out = contactable_html();
        assert!(
            out.contains("Neon Law Navigator</a> 26.8.20"),
            "the footer names the software and the running release: {out}"
        );
        assert!(
            out.contains(
                r#"<a class="site-footer__release" href="https://www.neonlaw.com/navigator">Neon Law Navigator</a>"#,
            ),
            "the product name links to the page describing the platform: {out}"
        );
        assert!(
            !out.contains("#26.8.20"),
            "no # prefix on the version: {out}"
        );
        let platform = out
            .find("site-footer__legal-platform")
            .expect("the platform region renders");
        let powered = out.find("site-footer__powered").expect("the platform line");
        let release = out.find("26.8.20").expect("the release renders");
        assert!(
            platform < powered && powered < release,
            "the release sits right after the product name that opens the line: {out}"
        );
    }

    /// With no repository or product page to name, the line falls back to the
    /// wording `/app`'s footer carries, so the two cannot drift where they
    /// still say the same thing.
    #[test]
    fn the_platform_line_is_the_shared_powered_by_wording() {
        fn app() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Neon Law".to_string(),
                    disclaimer: "This is an attorney advertisement.".to_string(),
                    copyright_year: 2026,
                }
            }
        }
        let out = ssr(app);
        assert!(
            out.contains(POWERED_BY_NEON_LAW_NAVIGATOR),
            "the public footer uses the shared wording: {out}"
        );
        assert_eq!(
            POWERED_BY_NEON_LAW_NAVIGATOR,
            "Powered by Neon Law Navigator"
        );
    }

    /// A deploy that publishes an office in a state this component carries no
    /// tracing of renders the plain treatment rather than a stray box.
    #[test]
    fn omits_the_watermark_for_an_untraced_state() {
        assert!(super::state_outline("Idaho").is_none());
        assert!(super::state_outline("").is_none());
    }

    /// An office note is a qualification on one specific address, so it must
    /// render inside that office's own `<li>` — between the address it
    /// qualifies and the next city. A note that escapes into the neighbouring
    /// entry attaches a pending admission to the wrong jurisdiction.
    #[test]
    fn renders_an_office_note_beneath_the_address_it_qualifies() {
        let out = contactable_html();
        // The last line of the New York address, so the note has to follow the
        // whole of it rather than slotting between two of its lines.
        let address = out
            .find("New York, NY 10017")
            .expect("the New York address renders");
        let note = out
            .find("Bar admission pending")
            .expect("the New York note renders");
        let next_office = out.find("720 Seneca St").expect("the next office renders");
        assert!(
            address < note && note < next_office,
            "the note sits under its own address, before the next office: {out}"
        );
        assert!(
            out.contains(r#"class="site-footer__office-note""#),
            "the note is styled as a qualification rather than a line of the address: {out}"
        );
        // The unqualified offices publish no note element at all, rather than
        // an empty one that reserves space under every address.
        assert_eq!(
            out.matches("site-footer__office-note").count(),
            1,
            "only the qualified office renders a note: {out}"
        );
    }

    /// The bar number is the point of the licence line: it must render beside
    /// its jurisdiction and link to that bar's own record, so a visitor can
    /// verify the licence rather than trust the page.
    #[test]
    fn names_each_attorney_with_their_bar_number_and_record() {
        let out = contactable_html();
        assert!(
            out.contains("Nicholas Richard Shook"),
            "the licensed attorney is named: {out}"
        );
        assert!(
            out.contains("Nevada Bar No. 13400"),
            "the bar number renders beside its jurisdiction: {out}"
        );
        assert!(
            out.contains("nvbar.org/find-a-lawyer/?usearch=13400"),
            "the number links to the bar's own record: {out}"
        );
    }

    /// A deploy that publishes no contact channels renders no empty band — and
    /// its legal strip is still the footer's first child, so the CSS drops the
    /// second rule.
    #[test]
    fn omits_the_contact_band_when_nothing_is_published() {
        let out = legal_html();
        assert!(
            !out.contains("site-footer__contact"),
            "no contact band without contact details: {out}"
        );
        assert!(
            !out.contains("site-footer__licenses"),
            "no licence list without attorneys: {out}"
        );
        assert!(
            out.contains(r#"<div class="site-footer__legal""#),
            "the legal strip still renders: {out}"
        );
    }

    fn three_firm_brands() -> Vec<FooterBrandLink> {
        vec![
            FooterBrandLink {
                label: "Neon Law".to_string(),
                href: "https://www.neonlaw.com".to_string(),
                current: true,
                logo_href: "/public/logo.svg".to_string(),
                byline: String::new(),
            },
            FooterBrandLink {
                label: "DeleteYourData.com".to_string(),
                href: "https://www.deleteyourdata.com".to_string(),
                current: false,
                logo_href: "/public/brand/delete-your-data/logo.svg".to_string(),
                byline: String::new(),
            },
            FooterBrandLink {
                label: "Lawyer Shook".to_string(),
                href: "https://www.lawyershook.com".to_string(),
                current: false,
                logo_href: "/public/brand/lawyer-shook/logo.svg".to_string(),
                byline: String::new(),
            },
        ]
    }

    fn one_membership() -> Vec<FooterMembership> {
        vec![FooterMembership {
            label: "Justice Technology Association".to_string(),
            standing: JTA_STANDING.to_string(),
            href: "https://justicetechassociation.org/get-involved".to_string(),
            logo_href: "/public/img/justice-technology-association/logo.png".to_string(),
        }]
    }

    /// The legal strip plus the firm's live-brand affiliations
    /// and the one association affiliation.
    fn affiliated_html() -> String {
        fn app() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Neon Law".to_string(),
                    disclaimer: "This is an attorney advertisement.".to_string(),
                    copyright_year: 2026,
                    brands: three_firm_brands(),
                    memberships: one_membership(),
                }
            }
        }
        ssr(app)
    }

    /// "Our Family" lists every brand in registry order under a visible
    /// heading, as a navigation landmark named by the same words, with the
    /// current brand as text marked `aria-current` and every other brand a
    /// link to its own home.
    #[test]
    fn a_firm_with_three_brands_renders_its_family_in_order_with_the_current_one_unlinked() {
        let out = affiliated_html();
        let family = out
            .split_once(r#"<nav class="site-footer__family" aria-label="Our family">"#)
            .and_then(|(_, rest)| rest.split_once("</nav>"))
            .map(|(row, _)| row)
            .expect("the family renders as a labelled landmark");
        assert!(
            family.contains(r#"<h2 class="site-footer__family-heading">Our Family</h2>"#),
            "the row is headed by the words its landmark is named with: {family}"
        );
        let neon = family.find("Neon Law").expect("neon");
        let dyd = family.find("DeleteYourData.com").expect("dyd");
        let shook = family.find("Lawyer Shook").expect("lawyer shook");
        assert!(neon < dyd && dyd < shook, "registry order: {family}");
        assert!(
            family.contains(
                r#"<span class="site-footer__family-current" aria-current="true"><img class="site-footer__family-logo" src="/public/logo.svg" alt=""/><span>Neon Law</span></span>"#
            ),
            "the current brand is text, marked current: {family}"
        );
        assert!(
            !family.contains(r#"href="https://www.neonlaw.com""#),
            "the current brand is unlinked: {family}"
        );
        assert!(
            family.contains(
                r#"<a class="site-footer__family-link" href="https://www.deleteyourdata.com" target="_blank" rel="noopener noreferrer"><img class="site-footer__family-logo" src="/public/brand/delete-your-data/logo.svg" alt=""/><span>DeleteYourData.com</span></a>"#
            ),
            "other brands link their home host, off-site, in a new tab: {family}"
        );
        assert!(
            family.contains(r#"href="https://www.lawyershook.com""#),
            "{family}"
        );
        assert_eq!(
            family
                .matches(r#"<li class="site-footer__family-item""#)
                .count(),
            3,
            "one item per brand: {family}"
        );
    }

    /// A brand with no host yet (a runtime-created one) is shown as text,
    /// never as an anchor with an empty `href` that reloads the page.
    #[test]
    fn a_brand_with_no_host_renders_unlinked_not_as_an_empty_anchor() {
        fn app() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Neon Law".to_string(),
                    disclaimer: "This is an attorney advertisement.".to_string(),
                    copyright_year: 2026,
                    brands: vec![
                        FooterBrandLink {
                            label: "Neon Law".to_string(),
                            href: "https://www.neonlaw.com".to_string(),
                            current: true,
                            logo_href: String::new(),
                            byline: String::new(),
                        },
                        FooterBrandLink {
                            label: "Acme Runtime Brand".to_string(),
                            href: String::new(),
                            current: false,
                            logo_href: String::new(),
                            byline: String::new(),
                        },
                    ],
                }
            }
        }
        let out = ssr(app);
        assert!(out.contains("Acme Runtime Brand"), "{out}");
        assert!(!out.contains(r#"href="""#), "no empty anchor: {out}");
        assert_eq!(
            out.matches(r#"aria-current="true""#).count(),
            1,
            "only the current brand is marked current: {out}"
        );
    }

    #[test]
    fn a_firm_with_one_brand_renders_no_family_row() {
        fn app() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Neon Law".to_string(),
                    disclaimer: "This is an attorney advertisement.".to_string(),
                    copyright_year: 2026,
                    brands: vec![FooterBrandLink {
                        label: "Neon Law".to_string(),
                        href: "https://www.neonlaw.com".to_string(),
                        current: true,
                        logo_href: String::new(),
                        byline: String::new(),
                    }],
                }
            }
        }
        let out = ssr(app);
        assert!(
            !out.contains("site-footer__family"),
            "one brand is no row: {out}"
        );
        assert!(
            !out.contains("site-footer__affiliations"),
            "and with no membership either, no affiliations row at all: {out}"
        );
        let bare = legal_html();
        assert!(
            !bare.contains("site-footer__family"),
            "an unset list is no row: {bare}"
        );
        assert!(!bare.contains("Proud member"), "no membership line: {bare}");
    }

    /// The affiliation line names the association and links its Get Involved
    /// page,
    /// wearing the off-site treatment every outbound link on the page does —
    /// a new tab, the OWASP `rel` pair, and the arrow that says so — after
    /// the family row and before the legal strip.
    #[test]
    fn publishes_each_affiliation_with_its_standing_line_linking_off_site() {
        let out = affiliated_html();
        let line = out
            .split_once(r#"<ul class="site-footer__memberships">"#)
            .and_then(|(_, rest)| rest.split_once("</ul>"))
            .map(|(line, _)| line)
            .expect("the memberships render as a list");
        assert!(
            line.contains(&JTA_STANDING.to_string()),
            "the line states the firm's standing: {line}"
        );
        assert!(
            line.contains(
                r#"href="https://justicetechassociation.org/get-involved" class="site-footer__membership-link""#
            ),
            "and links its own site: {line}"
        );
        assert!(
            line.contains(r#"target="_blank""#) && line.contains(r#"rel="noopener noreferrer""#),
            "off-site, with the OWASP rel pair: {line}"
        );
        assert!(
            line.contains("<title>opens in a new tab</title>"),
            "the arrow names what the link text cannot: {line}"
        );
        assert!(
            line.contains(
                r#"<span class="site-footer__membership-badge"><img class="site-footer__membership-logo" src="/public/img/justice-technology-association/logo.png" alt="Justice Technology Association" loading="lazy"/>"#
            ),
            "the standing line is short, so the mark names the association: {line}"
        );
        assert!(
            !line.contains("award-fill") && !line.contains(r#"aria-hidden="true""#),
            "with a mark there is no award glyph: {line}"
        );
        let family = out.find("Our Family").expect("family");
        let standing = out.find(JTA_STANDING).expect("membership");
        let legal = out
            .find(r#"class="site-footer__legal""#)
            .expect("legal strip");
        assert!(
            family < standing && standing < legal,
            "family, then affiliation, then the legal strip: {out}"
        );
    }

    /// A membership alone — a firm with one brand — still renders the
    /// affiliations row, with the line and no family beside it. One with no
    /// mark of its own wears the decorative award glyph in the badge's place.
    #[test]
    fn a_membership_without_a_family_renders_the_line_alone() {
        fn app() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Neon Law".to_string(),
                    disclaimer: "This is an attorney advertisement.".to_string(),
                    copyright_year: 2026,
                    memberships: vec![FooterMembership {
                        logo_href: String::new(),
                        ..one_membership().remove(0)
                    }],
                }
            }
        }
        let out = ssr(app);
        assert!(out.contains("site-footer__affiliations"), "{out}");
        assert!(out.contains(JTA_STANDING), "{out}");
        assert!(!out.contains("site-footer__family"), "{out}");
        assert!(
            !out.contains("site-footer__membership-badge") && out.contains(r#"aria-hidden="true""#),
            "no mark, so the decorative glyph stands in: {out}"
        );
    }

    /// Every class the affiliations row emits is styled by the theme it
    /// ships with. Mirrors `firm_footer`'s guard: a renamed class with no
    /// matching rule renders as unstyled text and nothing else catches it.
    ///
    /// Scoped to the row's own classes: the legal strip's `__copyright`,
    /// `__disclaimer`, and the like are semantic hooks the strip's grid
    /// styles through their parent, and have never had rules of their own.
    #[test]
    fn every_class_the_affiliations_row_emits_is_styled_by_the_theme() {
        let theme =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../server/public/css/theme.css");
        let css = std::fs::read_to_string(&theme)
            .unwrap_or_else(|e| panic!("the theme stylesheet must be readable: {e}"));

        let mut classes = std::collections::BTreeSet::new();
        for out in [contactable_html(), affiliated_html()] {
            let mut rest = out.as_str();
            while let Some(at) = rest.find("class=\"") {
                rest = &rest[at + 7..];
                let Some(end) = rest.find('"') else { break };
                classes.extend(rest[..end].split_whitespace().map(str::to_string));
                rest = &rest[end..];
            }
        }
        let row_classes: Vec<&String> = classes
            .iter()
            .filter(|class| {
                [
                    "site-footer__affiliations",
                    "site-footer__family",
                    "site-footer__membership",
                ]
                .iter()
                .any(|prefix| class.starts_with(prefix))
            })
            .collect();
        assert!(
            row_classes.len() >= 8,
            "the row emits its own classes: {row_classes:?}"
        );
        for class in row_classes {
            assert!(
                css.contains(&format!(".{class}")),
                "`{class}` is emitted by the footer but has no rule in \
                 server/public/css/theme.css"
            );
        }
    }

    // --- ENG-741: bylines, two columns, attribution, the JTA badge --------

    /// A cold reader learns nothing from "Vesta" or "Abhaya", so every entry
    /// carries what it does — including the brand the reader is already on.
    #[test]
    fn family_entries_carry_their_bylines() {
        fn app() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Shook Law PC".to_string(),
                    disclaimer: "This is an attorney advertisement.".to_string(),
                    copyright_year: 2026,
                    brands: vec![
                        FooterBrandLink {
                            label: "Neon Law".to_string(),
                            href: String::new(),
                            current: true,
                            logo_href: String::new(),
                            byline: "flat-fee legal services for emerging tech".to_string(),
                        },
                        FooterBrandLink {
                            label: "Abhaya Immigration".to_string(),
                            href: "https://www.abhayaimmigration.com".to_string(),
                            current: false,
                            logo_href: String::new(),
                            byline: "visas, green cards, and citizenship".to_string(),
                        },
                    ],
                }
            }
        }
        let out = ssr(app);
        assert!(out.contains("visas, green cards, and citizenship"), "{out}");
        assert!(
            out.contains("flat-fee legal services for emerging tech"),
            "the current brand keeps its byline: {out}"
        );
    }

    /// Every multi-brand family gets the desktop two-column class; the
    /// responsive stylesheet collapses it to one column on mobile.
    #[test]
    fn the_family_list_splits_into_two_columns_only_once_it_is_long() {
        assert_eq!(family_list_class(1), "site-footer__family-list");
        assert_eq!(
            family_list_class(FAMILY_TWO_COLUMN_THRESHOLD),
            "site-footer__family-list site-footer__family-list--two-column"
        );
        assert_eq!(
            family_list_class(8),
            "site-footer__family-list site-footer__family-list--two-column"
        );
    }

    /// "A practice of Shook Law PC" is what keeps a trade name from being
    /// misleading — disclosed affiliation is the thing Nevada's rule turns
    /// on — so it renders on a brand host, above the legal strip.
    #[test]
    fn a_brand_site_renders_its_attribution_above_the_legal_strip() {
        fn app() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Shook Law PC".to_string(),
                    disclaimer: "This is an attorney advertisement.".to_string(),
                    copyright_year: 2026,
                    attribution: "A practice of Shook Law PC".to_string(),
                }
            }
        }
        let out = ssr(app);
        assert!(
            out.contains(r#"<p class="site-footer__attribution">A practice of Shook Law PC</p>"#),
            "{out}"
        );
        let attribution = out.find("site-footer__attribution").expect("renders");
        let copyright = out.find("site-footer__copyright").expect("renders");
        assert!(
            attribution < copyright,
            "the attribution precedes the copyright line: {out}"
        );
    }

    /// The firm's own site names the firm in its masthead, so it carries no
    /// attribution line and renders no empty element in its place.
    #[test]
    fn the_firms_own_site_renders_no_attribution_line() {
        fn app() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Shook Law PC".to_string(),
                    disclaimer: "This is an attorney advertisement.".to_string(),
                    copyright_year: 2026,
                }
            }
        }
        assert!(!ssr(app).contains("site-footer__attribution"));
    }

    /// The Justice Technology Association membership stays a *visual badge*
    /// carrying the association's mark. Demoting it to a bare text link is
    /// the regression this guards: the badge is the point, and a footer
    /// refactor that treats every membership as a link would silently drop
    /// the image while leaving the sentence intact and the test green.
    #[test]
    fn the_jta_membership_renders_as_a_badge_not_a_bare_link() {
        let out = affiliated_html();
        assert!(
            out.contains(r#"class="site-footer__membership-badge""#),
            "the membership keeps its badge wrapper: {out}"
        );
        assert!(
            out.contains(r#"class="site-footer__membership-logo""#)
                && out.contains("justice-technology-association/logo.png"),
            "the association's mark renders as an image: {out}"
        );
        assert!(
            out.contains(&JTA_STANDING.to_string()),
            "and the sentence still names it: {out}"
        );
    }
}
