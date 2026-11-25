//! Grounding test: the letterhead a rendered letter goes out under is the
//! identity its Brand publishes.
//!
//! `notation pdf` / `notation word` resolve the Project's Brand from
//! `navigator.yaml` (`cli::letterhead::resolve_brand`) and frame the letter with it. Which
//! frame that Brand wears has two sources (LAW-266): the Brand's runtime
//! letterhead row — the same rows `/app/admin/brands` edits — when the
//! deployment's stores answer, and the compiled Brand registry when they
//! cannot, a fallback the render announces. The runtime rows' rendering is
//! exercised against real stores in `render.rs`; these tests pin the other
//! half of the contract: the compiled fallback agrees with the registry it
//! is compiled from, for every registered host, and a Brand with no
//! runtime row still wears a correct frame.
//!
//! Agreement is not a licence to drift. When a Brand's voice line or name
//! changes, a website that takes the new value while the letterhead keeps
//! the old one sends letters inviting clients to dial a dead line. These
//! tests turn that drift into a red build.
//!
//! `pdf::Letterhead::default()` is what a template outside any Project renders
//! on, so it is pinned to the default Brand's resolved letterhead.

use cli::rendering_letterhead::{for_brand, for_project};
use views::brand::{BrandKey, DEFAULT_BRANDING};

/// The letterhead outside a Project is the default Brand's own, less the
/// Firm's legal name, which only the Brand registry carries.
#[test]
fn the_default_letterhead_is_the_default_brands() {
    let resolved = for_brand(BrandKey::Neon).expect("the default Brand resolves");
    assert_eq!(
        pdf::Letterhead::default(),
        pdf::Letterhead {
            entity: String::new(),
            ..resolved
        },
        "pdf::Letterhead::default() drifted from views::brand::DEFAULT_BRANDING"
    );
}

/// Every registered host frames its letter with the Brand it serves: the
/// Brand's display name, its Firm's legal name, its voice line, its
/// production host, and the published correspondence mailbox.
#[test]
fn every_registered_host_wears_its_brands_letterhead() {
    for key in BrandKey::ALL.iter().copied() {
        let branding = key.resolve_branding(&DEFAULT_BRANDING);
        for host in key.hosts() {
            let letterhead =
                for_project(None, Some(host)).unwrap_or_else(|error| panic!("{host}: {error}"));
            assert_eq!(letterhead.name, branding.firm.site_name, "{host}");
            assert_eq!(letterhead.entity, branding.firm.legal_entity, "{host}");
            // The wordmark and the entity line are deliberately distinct
            // (LAW-466): a signature block quoting the display name is not
            // the Firm's legal identity.
            assert_ne!(
                letterhead.name, letterhead.entity,
                "the wordmark must not swallow the legal entity: {host}"
            );
            assert_eq!(letterhead.phone, branding.firm_phone.trim(), "{host}");
            assert_eq!(letterhead.web, key.canonical_host(), "{host}");
            let labels: Vec<&str> = key.canonical_host().split('.').collect();
            let domain = labels[labels.len() - 2..].join(".");
            if key == BrandKey::GuiebShook {
                assert_eq!(letterhead.email, branding.firm_email, "{host}");
                assert_eq!(letterhead.mark, pdf::LetterheadMark::GuiebShook, "{host}");
                assert_eq!(letterhead.font_family, "Instrument Sans", "{host}");
                assert!(letterhead.postal_address.contains("Reno"), "{host}");
            } else {
                assert_eq!(letterhead.email, format!("contact@{domain}"), "{host}");
                let expected_mark = if key == BrandKey::Neon {
                    pdf::LetterheadMark::Compiled
                } else {
                    pdf::LetterheadMark::Wordmark
                };
                assert_eq!(letterhead.mark, expected_mark, "{host}");
                assert!(letterhead.postal_address.is_empty(), "{host}");
                assert!(letterhead.font_family.is_empty(), "{host}");
            }
        }
    }
}

/// The default Brand's letterhead reads the mailbox and website the site
/// publishes.
#[test]
fn the_default_letterhead_matches_the_published_brand() {
    let letterhead = pdf::Letterhead::default();
    assert_eq!(letterhead.email, DEFAULT_BRANDING.firm_email);
    assert_eq!(
        letterhead.web,
        format!("www.{}", DEFAULT_BRANDING.primary_domain)
    );
    assert_eq!(letterhead.name, DEFAULT_BRANDING.firm.site_name);
    assert_eq!(letterhead.phone, DEFAULT_BRANDING.firm_phone);
}
