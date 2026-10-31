//! The public footer's legal strip, as a Dioxus component (issue #641, Phase 2).
//!
//! Three lines, and the footer resolves all of them live: the copyright that
//! names the legal person the page's Firm resolves to, the registered-mark
//! notice when the brand's compiled `Branding` carries a trademark, and the
//! attorney-advertising disclaimer. Everything else the footer once carried —
//! the contact band, offices, bar licences, association memberships, the
//! "Our Family" row, social profiles, the platform stamp — came off the
//! rendered footer when firm identity became live-resolved; the compiled
//! marketing surface stays in `views::brand` for the consumers that still read
//! it (letterhead, `/team`), it just no longer renders here.
//!
//! It is prop-driven like [`crate::components::PricingSection`]: the process
//! brand (`views::brand`) and the resolved Firm→Entity are mapped onto
//! [`SiteFooterLegal`]'s props per request on the server, so the wasm client
//! never links the view layer and a white-label deploy emits its own identity.
//! `crate::public_chrome::PublicFooter` owns that mapping so no page restates
//! it.

use dioxus::prelude::*;

use crate::components::ExternalLink;

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

/// The footer legal strip. Every field is resolved per request, so the
/// component itself is pure presentation.
///
/// - `copyright_holder`: the legal person that owns the site and the words on
///   it — the live Firm→Entity name where the store can answer, the compiled
///   `SiteBrand::legal_entity` as fallback. It is the only line naming the
///   entity behind the site. Deliberately a separate dial from the wordmark
///   the page trades under, even where a deploy sets both to the same words: a
///   copyright notice has to name an entity that can hold one, and a
///   white-label bundle renames the wordmark without renaming the copyright
///   holder.
/// - `copyright_href`: the resolved firm's home — where the copyright name
///   links. Empty renders the name unlinked, which is what a firm with no
///   published home gets rather than an `<a href="">` that reloads whatever
///   page it is sitting on.
/// - `disclaimer`: the attorney-advertising disclaimer copy.
/// - `copyright_year`: the year on the copyright line, resolved by the
///   server per request so the reusable component never carries a stale year.
/// - `trademark` / `trademark_registration` / `trademark_record_url`: the
///   registered word mark the site trades under, spelled the way the register
///   spells it, and the registration that proves it. Renders one notice under
///   the copyright line — the site's two ownership facts read together —
///   naming the mark, the registrant, and the number, with the number linked
///   to the register's own record so a reader verifies the claim there rather
///   than trusting the site's line about itself. The mark belongs to the
///   registration, not to whichever firm wears the brand, so these stay fed
///   from the brand's compiled `Branding` and render only when the brand
///   carries a trademark at all: a brand with an empty compiled trademark
///   (every non-Neon brand today) renders no notice.
///
/// The strip is width-constrained to the same 72rem column the
/// [`crate::components::SiteHeader`] nav uses, so its lines line up with the
/// navbar above them instead of running to the viewport edge. The rule and
/// background stay full-bleed, matching the header's `border-bottom`.
#[component]
pub fn SiteFooterLegal(
    copyright_holder: String,
    #[props(default)] copyright_href: String,
    disclaimer: String,
    copyright_year: i32,
    #[props(default)] trademark: String,
    #[props(default)] trademark_registration: String,
    #[props(default)] trademark_record_url: String,
) -> Element {
    rsx! {
        footer { class: "site-footer", role: "contentinfo",
            div { class: "site-footer__inner",
                div { class: "site-footer__legal",
                    p { class: "site-footer__copyright",
                        // The site and the words on it belong to the firm's
                        // legal person, which is what a copyright notice names
                        // — the wordmark cannot hold one. This heads the legal
                        // strip, because it is the only line naming the entity
                        // behind the site. The name links to the resolved
                        // firm's own home when the live firm data publishes
                        // one, and stands as plain text when it does not.
                        "© {copyright_year} "
                        if copyright_href.is_empty() {
                            "{copyright_holder}"
                        } else {
                            a { href: "{copyright_href}", "{copyright_holder}" }
                        }
                    }
                    // The other ownership fact, directly under the first: the
                    // mark this page trades under is a registered mark, and
                    // this is who holds it and under what number. It links out
                    // for the same reason the old bar rows did — a reader
                    // checks the register, not the page's word for it. Fed
                    // from the brand's compiled `Branding`, so it follows the
                    // mark's own registration wherever the brand's pages are
                    // served from, and it does not render at all when the
                    // brand carries no trademark.
                    if !trademark.is_empty() {
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
                    p { class: "site-footer__disclaimer", "{disclaimer}" }
                }
            }
        }
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

    /// The firm's legal strip: the copyright line that names the entity, the
    /// mark notice, and the attorney-advertising disclaimer — the three lines
    /// every public page renders, and nothing else.
    fn legal_html() -> String {
        fn app() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Guieb & Shook PC".to_string(),
                    copyright_href: "https://www.guiebandshook.com".to_string(),
                    disclaimer: "Attorney advertisement. Nothing here is legal advice without a signed retainer for an active project. Past results do not guarantee future outcomes.".to_string(),
                    copyright_year: 2026,
                    trademark: "NEON LAW".to_string(),
                    trademark_registration: "6,325,650".to_string(),
                    trademark_record_url:
                        "https://tmsearch.uspto.gov/search/search-results/90039224".to_string(),
                }
            }
        }
        ssr(app)
    }

    /// The legal strip renders exactly three lines: the linked copyright, the
    /// mark notice, and the disclaimer — in that order, and nothing else.
    #[test]
    fn renders_exactly_the_three_legal_lines() {
        let out = legal_html();
        assert!(
            out.contains(
                r#"© 2026 <a href="https://www.guiebandshook.com">Guieb &#38; Shook PC</a>"#
            ),
            "the copyright names the live entity, linked to the firm's home: {out}"
        );
        assert!(
            out.contains("NEON LAW")
                && out.contains("is a registered trademark of Guieb &#38; Shook PC"),
            "the mark notice names the registrant the copyright names: {out}"
        );
        assert!(
            out.contains("U.S. Reg. No. 6,325,650"),
            "the registration is the claim: {out}"
        );
        assert!(
            out.contains(r#"href="https://tmsearch.uspto.gov/search/search-results/90039224""#),
            "linked to the register's own record: {out}"
        );
        assert!(
            out.contains("site-footer__disclaimer"),
            "the attorney-advertising disclaimer renders: {out}"
        );
        let copyright = out.find("site-footer__copyright").expect("the copyright");
        let mark = out.find("site-footer__trademark").expect("the mark notice");
        let disclaimer = out.find("site-footer__disclaimer").expect("the disclaimer");
        assert!(
            copyright < mark && mark < disclaimer,
            "the three lines read copyright, mark, disclaimer: {out}"
        );
        // The rows that came off the footer: none of their classes survive.
        for gone in [
            "site-footer__masthead",
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
    }

    /// An empty compiled trademark renders no mark notice at all — the path
    /// every non-Neon brand (and the IEP brand once it launches) takes.
    #[test]
    fn a_brand_without_a_trademark_renders_no_notice() {
        fn app() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Lex Tecnica LLP".to_string(),
                    disclaimer: "Attorney advertisement.".to_string(),
                    copyright_year: 2026,
                }
            }
        }
        let out = ssr(app);
        assert!(
            out.contains("© 2026 Lex Tecnica LLP"),
            "the copyright renders unlinked with no home to link: {out}"
        );
        assert!(
            !out.contains("site-footer__trademark"),
            "no notice where no mark exists: {out}"
        );
        assert!(
            out.contains("site-footer__disclaimer"),
            "the disclaimer: {out}"
        );
    }

    /// A mark with a registration but no record URL renders the number as
    /// plain text, not an empty anchor.
    #[test]
    fn a_registration_without_a_record_link_renders_the_number_as_text() {
        fn app() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Example PC".to_string(),
                    disclaimer: String::new(),
                    copyright_year: 2026,
                    trademark: "EXAMPLE".to_string(),
                    trademark_registration: "6,000,000".to_string(),
                }
            }
        }
        let out = ssr(app);
        assert!(out.contains("U.S. Reg. No. 6,000,000"), "{out}");
        assert!(!out.contains(r#"href="""#), "no empty anchors: {out}");
    }

    /// A common-law mark (no registration) renders the ™ notice naming the
    /// copyright holder, without a registration number.
    #[test]
    fn a_common_law_mark_names_its_holder_without_a_number() {
        fn app() -> Element {
            rsx! {
                SiteFooterLegal {
                    copyright_holder: "Example PC".to_string(),
                    disclaimer: String::new(),
                    copyright_year: 2026,
                    trademark: "EXAMPLE".to_string(),
                }
            }
        }
        let out = ssr(app);
        assert!(
            out.contains("EXAMPLE") && out.contains("is a common-law mark of Example PC"),
            "{out}"
        );
    }

    /// `tel_href` dials digits only — the litigation page's `tel:` link and
    /// any future consumer share the format.
    #[test]
    fn tel_href_keeps_digits_and_the_plus() {
        assert_eq!(tel_href("+1 (555) 555-0134"), "tel:+15555550134");
        assert_eq!(tel_href("555.555.0134"), "tel:5555550134");
    }
}
