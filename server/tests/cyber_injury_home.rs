//! `CyberInjuryLaw`'s public Dioxus home, on its own launched hosts.

use views::brand::BrandKey;

/// The campaign reports to its own Plausible site.
#[test]
fn campaign_is_live_with_its_own_analytics() {
    assert!(BrandKey::CyberInjuryLaw.is_live());
    assert!(
        portal::plausible::PlausibleSite::for_brand(BrandKey::CyberInjuryLaw)
            .script_tags()
            .contains("pa-Ns-yAKpkSNXzfvnEJYZJX.js")
    );
}
