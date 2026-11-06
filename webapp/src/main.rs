//! Client entry point for the wasm bundle.
//!
//! `dx` compiles this binary to `wasm32-unknown-unknown` with the `web`
//! feature; the design gallery and campaign launch their matching server-rendered
//! roots. Private Matter Pages launch their dedicated root; other routes retain
//! [`webapp::App`]. Navigator's server is the `web`
//! crate, not this binary — `web` links `webapp` as a library — so with no
//! platform feature selected (the workspace default) `main` is intentionally
//! empty and `cargo build --workspace` stays green.

fn main() {
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
        let is_matter_page = web_sys::window().is_some_and(|window| {
            let path = window.location().pathname().unwrap_or_default();
            let has_root = window.document().is_some_and(|document| {
                document
                    .query_selector("main[data-matter-page-root=\"true\"]")
                    .ok()
                    .flatten()
                    .is_some()
            });
            private_matter_page_root(&path, has_root)
        });
        // Every SSR route already supplies its head. Preserve it during
        // hydration: dynamic document scripts are rejected by the nonce CSP.
        let root = if is_design {
            webapp::design::DesignGallery
        } else if is_cyber_home {
            webapp::home::HomePageEntry
        } else if is_matter_page {
            webapp::sections::page::MatterPage
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

// The SSR marker belongs to the dedicated root, not to a glossary term or nested section.
#[cfg(any(test, all(feature = "web", target_arch = "wasm32")))]
fn private_matter_page_root(path: &str, has_root: bool) -> bool {
    has_root
        && path.strip_prefix("/app/projects/").is_some_and(|tail| {
            tail.split_once('/').is_some_and(|(code, slug)| {
                !code.is_empty() && !slug.is_empty() && !slug.contains('/')
            })
        })
}

#[cfg(test)]
mod tests {
    use super::private_matter_page_root;

    #[test]
    fn matter_hydration_requires_its_dedicated_root_and_private_page_path() {
        for path in [
            "/app/projects/synthetic/reflection",
            "/app/projects/synthetic/instruments",
            "/app/projects/synthetic/unknown",
        ] {
            assert!(private_matter_page_root(path, true));
            assert!(!private_matter_page_root(path, false));
        }
        for path in [
            "/glossary",
            "/glossary#matter-page",
            "/world",
            "/app/projects",
            "/app/projects/synthetic",
            "/app/projects//reflection",
            "/app/projects/synthetic/",
            "/app/projects/synthetic/review/id",
            "/app/projects/synthetic/intake/id",
            "/app/lawyer/projects/synthetic/reflection",
        ] {
            assert!(!private_matter_page_root(path, true), "{path}");
        }
        // Existing specific one-segment roots do not carry the MatterPage marker.
        assert!(!private_matter_page_root(
            "/app/projects/synthetic/conversation",
            false
        ));
    }
}
