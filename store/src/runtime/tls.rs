//! Shared process-level TLS configuration for client libraries.

use std::sync::Once;

/// Install a TLS provider for clients constructed by the application.
pub fn install_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}
