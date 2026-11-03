//! Grounding test: the letterhead a rendered letter goes out under is the
//! identity its Brand publishes.
//!
//! `notation pdf` / `notation word` resolve the Project's Brand from
//! `navigator.yaml` (`cli::letterhead`) and frame the letter with it. The
//! letterhead is built from the compiled Brand registry, source rather than a
//! mounted bundle, because a rendered letter is a binding artifact whose
//! re-render must print what was sent.
//!
//! Building from source is not a licence to drift from it. When a Brand's
//! voice line or name changes, a website that takes the new value while the
//! letterhead keeps the old one sends letters inviting clients to dial a dead
//! line. These tests turn that drift into a red build, for every registered
//! host, not just the default Brand.
//!
//! `pdf::Letterhead::default()` is what a template outside any Project renders
//! on, so it is pinned to the default Brand's resolved letterhead.

use views::brand::{BrandKey, DEFAULT_BRANDING};

/// The letterhead outside a Project is the default Brand's own, less the
/// Firm's legal name, which only the Brand registry carries.
#[test]
fn the_default_letterhead_is_the_default_brands() {
    let resolved = cli::letterhead::for_brand(BrandKey::Neon).expect("the default Brand resolves");
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
/// production host, and `contact@` that host's registrable domain.
#[test]
fn every_registered_host_wears_its_brands_letterhead() {
    for key in BrandKey::ALL.iter().copied() {
        let branding = key.resolve_branding(&DEFAULT_BRANDING);
        for host in key.hosts() {
            let letterhead = cli::letterhead::for_project(None, Some(host))
                .unwrap_or_else(|error| panic!("{host}: {error}"));
            assert_eq!(letterhead.name, branding.firm.site_name, "{host}");
            assert_eq!(letterhead.entity, branding.firm.legal_entity, "{host}");
            assert_eq!(letterhead.phone, branding.firm_phone.trim(), "{host}");
            assert_eq!(letterhead.web, key.canonical_host(), "{host}");
            let labels: Vec<&str> = key.canonical_host().split('.').collect();
            let domain = labels[labels.len() - 2..].join(".");
            assert_eq!(letterhead.email, format!("contact@{domain}"), "{host}");
            assert_eq!(letterhead.logo, key == BrandKey::Neon, "{host}");
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
