//! The empty state: what a list shows before it has anything in it.
//!
//! The title should say which emptiness this is. "No documents yet" and "No
//! documents match this filter" send a reader to different next actions, so
//! the copy belongs to the caller, along with the action that resolves it.

use dioxus::prelude::*;

/// A titled empty state with an optional explanation and resolving action.
#[component]
pub fn Empty(
    title: String,
    #[props(default)] description: Option<String>,
    #[props(default)] action: Option<Element>,
) -> Element {
    rsx! {
        div { class: "nav-empty",
            p { class: "nav-empty__title", "{title}" }
            if let Some(description) = description {
                p { class: "nav-empty__description", "{description}" }
            }
            if let Some(action) = action {
                div { class: "nav-empty__action", {action} }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssr(app: fn() -> Element) -> String {
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    #[test]
    fn renders_title_description_and_action() {
        fn app() -> Element {
            rsx! {
                Empty {
                    title: "No documents yet".to_string(),
                    description: "Uploaded files appear here.".to_string(),
                    action: rsx! { a { href: "/design", "Upload a document" } },
                }
            }
        }
        let html = ssr(app);
        assert!(
            html.contains(r#"<p class="nav-empty__title">No documents yet</p>"#),
            "{html}"
        );
        assert!(html.contains("nav-empty__description"), "{html}");
        assert!(
            html.contains(r#"<div class="nav-empty__action"><a href="/design">"#),
            "{html}"
        );
    }

    #[test]
    fn title_alone_is_enough() {
        fn app() -> Element {
            rsx! { Empty { title: "No documents match this filter".to_string() } }
        }
        let html = ssr(app);
        assert!(!html.contains("nav-empty__description"), "{html}");
        assert!(!html.contains("nav-empty__action"), "{html}");
    }
}
