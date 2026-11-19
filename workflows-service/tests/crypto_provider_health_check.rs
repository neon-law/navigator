//! Exercise JWT provider selection in a fresh process, independent of other tests.
//!
//! The normal build has one backend and can auto-select it. The explicit
//! `jwt-provider-regression` feature adds the second backend: the probe must
//! catch that genuine ambiguity. The existing library test runs separately to
//! verify startup pinning BEFORE its first probe: provider selection lasts for
//! the process lifetime and cannot be repaired after probing ambiguity.

use workflows_service::request_identity::crypto_provider_is_healthy;

#[test]
fn isolated_unpinned_provider_health_tracks_selected_backend_ambiguity() {
    #[cfg(not(feature = "jwt-provider-regression"))]
    assert!(
        crypto_provider_is_healthy(),
        "the single JWT backend must auto-select successfully"
    );
    #[cfg(feature = "jwt-provider-regression")]
    assert!(
        !crypto_provider_is_healthy(),
        "two unpinned JWT backends must report unhealthy without escaping the probe"
    );
}
