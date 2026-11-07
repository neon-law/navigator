//! Client entry point for the wasm bundle.
//!
//! `dx` compiles this binary to `wasm32-unknown-unknown` with the `web`
//! feature; the design gallery and campaign launch their matching server-rendered
//! roots. Private Matter Pages and the inbox workspaces launch their dedicated
//! roots; other routes retain [`webapp::App`]. Navigator's server is the `web`
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
        let inbox = web_sys::window()
            .and_then(|window| inbox_root(&window.location().pathname().unwrap_or_default()));
        // Every SSR route already supplies its head. Preserve it during
        // hydration: dynamic document scripts are rejected by the nonce CSP.
        let root = if is_design {
            webapp::design::DesignGallery
        } else if is_cyber_home {
            webapp::home::HomePageEntry
        } else if is_matter_page {
            webapp::sections::page::MatterPage
        } else if inbox == Some(InboxRoot::Firm) {
            webapp::inbox::OwnerInbox
        } else if inbox == Some(InboxRoot::Project) {
            webapp::inbox::ProjectInbox
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

/// The inbox workspace a list route renders. Its reading pane is a client
/// signal, so the list must hydrate as the component the server rendered
/// there, not as [`webapp::App`].
#[cfg(any(test, all(feature = "web", target_arch = "wasm32")))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InboxRoot {
    Firm,
    Project,
}

#[cfg(any(test, all(feature = "web", target_arch = "wasm32")))]
fn inbox_root(path: &str) -> Option<InboxRoot> {
    if path == webapp::inbox::INBOX_PATH || path == webapp::inbox::OWNER_INBOX_PATH {
        return Some(InboxRoot::Firm);
    }
    path.strip_prefix("/app/projects/")
        .and_then(|tail| tail.strip_suffix("/inbox"))
        .filter(|code| !code.is_empty() && !code.contains('/'))
        .map(|_| InboxRoot::Project)
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
    use super::{inbox_root, private_matter_page_root, InboxRoot};

    #[test]
    fn inbox_lists_hydrate_as_their_workspace_roots() {
        assert_eq!(inbox_root("/app/inbox"), Some(InboxRoot::Firm));
        assert_eq!(inbox_root("/app/owner/inbox"), Some(InboxRoot::Firm));
        assert_eq!(
            inbox_root("/app/projects/synthetic/inbox"),
            Some(InboxRoot::Project)
        );
        for path in [
            "/app/inbox/01a1b198-c04d-732c-926f-972f8588d5db",
            "/app/inbox/triage",
            "/app/inbox/calls",
            "/app/projects//inbox",
            "/app/projects/synthetic/inbox/01a1b198-c04d-732c-926f-972f8588d5db",
            "/app/projects/a/b/inbox",
            "/app/team",
        ] {
            assert_eq!(inbox_root(path), None, "{path}");
        }
    }

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
