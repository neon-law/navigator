//! Client entry point for the wasm bundle.
//!
//! `dx` compiles this binary to `wasm32-unknown-unknown` with the `web`
//! feature; the design gallery and campaign launch their matching server-rendered
//! roots, while other routes retain [`webapp::App`]. Navigator's server is the `web`
//! crate, not this binary — `web` links `webapp` as a library — so with no
//! platform feature selected (the workspace default) `main` is intentionally
//! empty and `cargo build --workspace` stays green.

fn main() {
        // Native mobile shell (`dx serve --platform ios|android` from webapp/):
    // the Nautilus subscription experience with local content only — no
    // SSR-injected context or server futures, which do not exist natively.
    #[cfg(feature = "mobile")]
    dioxus::launch(webapp::mobile::MobileApp);

#[cfg(all(feature = "web", target_arch = "wasm32"))]
    {
        // Hydration must launch the same root used by the server's home router.
        let is_design = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id("design"))
            .is_some();
        let is_cyber_home = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id("cyber-main"))
            .is_some();
        // Every SSR route already supplies its head. Preserve it during
        // hydration: dynamic document scripts are rejected by the nonce CSP.
        let root = if is_design {
            webapp::design::DesignGallery
        } else if is_cyber_home {
            webapp::home::HomePageEntry
        } else {
            webapp::App
        };
        dioxus::LaunchBuilder::web()
            .with_context_provider(|| {
                Box::new(std::rc::Rc::new(
                    dioxus_fullstack_core::document::FullstackWebDocument::from(
                        dioxus::document::NoOpDocument,
                    ),
                )
                    as std::rc::Rc<dyn dioxus::document::Document>)
            })
            .launch(root);
    }
}
