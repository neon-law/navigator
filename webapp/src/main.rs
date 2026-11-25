//! Client entry point for the wasm bundle.
//!
//! `dx` compiles this binary to `wasm32-unknown-unknown` with the `web`
//! feature. Dedicated pages select their server-rendered roots by route and DOM marker;
//! other routes retain [`webapp::App`]. The `portal` crate links `webapp` as a library, so with no
//! platform feature selected (the workspace default) `main` is intentionally
//! empty and `cargo build --workspace` stays green.

fn main() {
    #[cfg(all(feature = "web", target_arch = "wasm32"))]
    {
        // This public site uses ordinary links and forms, with a standalone
        // script for its interactive presentation.
        if web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id("guieb-shook-website"))
            .is_some()
        {
            return;
        }
        // Hydration must launch the same root used by the server's home router.
        let is_design = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id("design"))
            .is_some();
        let is_cyber_home = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id("cyber-main"))
            .is_some();
        let is_firm_move = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id("firm-move"))
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
        let is_intake_review = web_sys::window().is_some_and(|window| {
            let path = window.location().pathname().unwrap_or_default();
            let has_root = window
                .document()
                .is_some_and(|document| document.get_element_by_id("intake-review").is_some());
            intake_review_root(&path, has_root)
        });
        let inbox = web_sys::window()
            .and_then(|window| inbox_root(&window.location().pathname().unwrap_or_default()));
        // Every SSR route already supplies its head. Preserve it during
        // hydration: dynamic document scripts are rejected by the nonce CSP.
        let root = if is_design {
            webapp::design::DesignGallery
        } else if is_cyber_home {
            webapp::home::HomePageEntry
        } else if is_firm_move {
            webapp::firm_move::MoveBrand
        } else if is_intake_review {
            webapp::intake_review::IntakeReview
        } else if is_matter_page {
            webapp::sections::page::MatterPage
        } else if inbox == Some(InboxRoot::Firm) {
            webapp::inbox::OwnerInbox
        } else if inbox == Some(InboxRoot::FirmThread) {
            webapp::inbox::OwnerInboxThread
        } else if inbox == Some(InboxRoot::Project) {
            webapp::inbox::ProjectInbox
        } else if inbox == Some(InboxRoot::ProjectThread) {
            webapp::inbox::ProjectInboxThread
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

/// Hydrate a notation review only where its server-rendered root is present.
#[cfg(any(test, all(feature = "web", target_arch = "wasm32")))]
fn intake_review_root(path: &str, has_root: bool) -> bool {
    has_root
        && path
            .strip_prefix("/app/lawyer/notations/")
            .and_then(|tail| tail.strip_suffix("/review"))
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
}

/// Inbox lists and full conversations each hydrate the root rendered by SSR.
#[cfg(any(test, all(feature = "web", target_arch = "wasm32")))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InboxRoot {
    Firm,
    FirmThread,
    Project,
    ProjectThread,
}

#[cfg(any(test, all(feature = "web", target_arch = "wasm32")))]
fn inbox_root(path: &str) -> Option<InboxRoot> {
    for base in [webapp::inbox::INBOX_PATH, webapp::inbox::OWNER_INBOX_PATH] {
        if path == base {
            return Some(InboxRoot::Firm);
        }
        if path
            .strip_prefix(base)
            .and_then(|tail| tail.strip_prefix('/'))
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
        {
            return Some(InboxRoot::FirmThread);
        }
    }
    let (code, route) = path.strip_prefix("/app/projects/")?.split_once('/')?;
    if code.is_empty() {
        return None;
    }
    if route == "inbox" {
        return Some(InboxRoot::Project);
    }
    route
        .strip_prefix("inbox/")
        .filter(|id| uuid::Uuid::parse_str(id).is_ok())
        .map(|_| InboxRoot::ProjectThread)
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
    use super::{inbox_root, intake_review_root, private_matter_page_root, InboxRoot};

    #[test]
    fn inbox_lists_and_conversations_hydrate_as_their_server_roots() {
        assert_eq!(inbox_root("/app/inbox"), Some(InboxRoot::Firm));
        assert_eq!(inbox_root("/app/owner/inbox"), Some(InboxRoot::Firm));
        assert_eq!(
            inbox_root("/app/projects/synthetic/inbox"),
            Some(InboxRoot::Project)
        );
        let id = "01a1b198-c04d-732c-926f-972f8588d5db";
        for base in ["/app/inbox", "/app/owner/inbox"] {
            assert_eq!(
                inbox_root(&format!("{base}/{id}")),
                Some(InboxRoot::FirmThread)
            );
        }
        assert_eq!(
            inbox_root(&format!("/app/projects/synthetic/inbox/{id}")),
            Some(InboxRoot::ProjectThread)
        );
        for path in [
            "/app/inbox/triage",
            "/app/inbox/calls",
            "/app/projects//inbox",
            "/app/projects/synthetic/inbox/not-a-conversation",
            "/app/projects/synthetic/inbox/01a1b198-c04d-732c-926f-972f8588d5db/notes",
            "/app/projects/a/b/inbox",
            "/app/team",
        ] {
            assert_eq!(inbox_root(path), None, "{path}");
        }
    }

    #[test]
    fn review_hydration_requires_its_private_notation_route_and_ssr_root() {
        let notation = uuid::Uuid::from_u128(1);
        let review = format!("/app/lawyer/notations/{notation}/review");
        assert!(intake_review_root(&review, true));
        assert!(!intake_review_root(&review, false));
        for path in [
            format!("/app/lawyer/notations/{notation}/clauses"),
            format!("/app/lawyer/notations/{notation}/review/extra"),
            format!("/app/projects/{notation}/review"),
            format!("/app/notations/{notation}/review"),
            "/app/lawyer/notations/not-a-uuid/review".to_string(),
            "/glossary/intake-review".to_string(),
        ] {
            assert!(!intake_review_root(&path, true), "{path}");
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
