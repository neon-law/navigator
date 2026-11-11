//! `navigator-deploy nautilus preview` — the Nautilus mobile shell, served locally
//! and hydrated.
//!
//! The shell itself ([`webapp::mobile::MobileApp`]) is content-local and
//! static by design; what this command adds is the browser experience of it
//! without a Mac, Xcode, or `dx`: one Axum listener that server-renders the
//! shell and layers the staged `nautilus-preview` web bundle on top — the
//! exact composition the portal's hydration mechanics exist for. Shaped like
//! the `navigator-deploy notation preview` offline lint: a provenance banner that
//! never lets the render masquerade as the Project's portal, an honest
//! degraded mode when no bundle is staged, and Ctrl-C or SIGTERM to stop.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;

use anyhow::{Context, Result};
use axum::Router;
use clap::Subcommand;

use crate::devx::nautilus::{staged_bundle_dir, STAGED_BUNDLE_ENV};

/// `navigator-deploy nautilus …` — the Nautilus mobile shell, served locally for a
/// browser.
#[derive(Subcommand)]
pub enum NautilusCmd {
    /// Serve the mobile shell on this machine, hydrated when a staged
    /// `nautilus-preview` bundle exists.
    Preview {
        /// Port to serve on. `0` picks a free one.
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
}

/// `navigator-deploy nautilus preview`: serve the mobile shell on one localhost
/// listener until the process is interrupted. `port` `0` asks the OS for a
/// free one, so several previews can run at once.
pub async fn run(port: u16) -> Result<()> {
    // Set before the router is built: the portal router resolves the staged
    // bundle root from this variable for the assets mount and points
    // dioxus-server's template knob at the same directory.
    let bundle = staged_bundle_dir();
    if let Some(dir) = &bundle {
        std::env::set_var(STAGED_BUNDLE_ENV, dir);
    }

    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    let bound = listener.local_addr().context("reading the bound port")?;

    for line in provenance_banner(bundle.as_deref()) {
        eprintln!("{line}");
    }
    eprintln!("==> http://{bound}/");
    eprintln!("    Ctrl-C or SIGTERM to stop.");

    axum::serve(listener, router())
        .with_graceful_shutdown(portal::hosting::shutdown_signal())
        .await
        .context("serving the Nautilus preview")?;
    Ok(())
}

/// The preview surface: the portal's own hydration mechanics — the SSR shell
/// at the root, plus the staged bundle's wasm and glue when one is staged.
/// The degraded, SSR-only case is the portal router's designed mode, not a
/// gap to fill here.
fn router() -> Router {
    portal::dioxus_app::nautilus_preview_router()
}

/// The provenance banner, one line per `eprintln!`: what this server is,
/// what it is not, and — with no staged bundle — what would make navigation
/// work. A pure function of the staged bundle, so the honesty contract is
/// testable.
fn provenance_banner(bundle: Option<&Path>) -> Vec<String> {
    let mut lines = vec![
        "==> Nautilus mobile shell — a local render, not the Project's portal.".to_string(),
        "    nothing here is pushed, stored, or reachable beyond this machine.".to_string(),
    ];
    match bundle {
        Some(dir) => lines.push(format!("    preview bundle: {}", dir.display())),
        None => lines.push(
            "    preview bundle: none — the home renders but navigation is inert. \
             Build one with `navigator-deploy dev build-nautilus`."
                .to_string(),
        ),
    }
    lines
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{provenance_banner, router};
    use axum::body::to_bytes;
    use tower::ServiceExt;

    /// Without a staged bundle the banner still leads with provenance and
    /// tells the reader exactly why navigation is inert and what would make
    /// it work — the degraded mode's honesty contract.
    #[test]
    fn without_a_bundle_the_banner_reports_inert_navigation() {
        let banner = provenance_banner(None).join("\n");

        assert!(
            banner.contains("a local render, not the Project's portal"),
            "provenance first: {banner}"
        );
        assert!(banner.contains("navigation is inert"), "{banner}");
        assert!(
            banner.contains("navigator-deploy dev build-nautilus"),
            "names the build step: {banner}"
        );
    }

    /// With a bundle staged, the banner names it and never says navigation
    /// is inert.
    #[test]
    fn with_a_bundle_the_banner_names_it_and_drops_the_inert_note() {
        let banner =
            provenance_banner(Some(Path::new("/checkout/server/public/nautilus"))).join("\n");

        assert!(
            banner.contains("/checkout/server/public/nautilus"),
            "names the staged bundle: {banner}"
        );
        assert!(!banner.contains("inert"), "{banner}");
    }

    /// The served surface renders the mobile shell server-side: the Nautilus
    /// home markup and its navigation hook are readable before any wasm
    /// hydrates, with or without a staged bundle — this test process makes no
    /// claim about which, and the router serves the shell either way.
    #[tokio::test]
    async fn the_served_root_renders_the_mobile_shell() {
        let response = router()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/")
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let html = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body")
                .to_vec(),
        )
        .expect("UTF-8 HTML");
        assert!(html.contains("Nautilus"), "home markup: {html}");
        assert!(
            html.contains(r#"data-mobile-nav="dyd""#),
            "the navigation hook the native shell taps: {html}"
        );
    }
}
