//! The action list: an auto-numbered list of next steps, each a bold title
//! with an optional detail line.

use dioxus::prelude::*;

/// One step in an [`ActionList`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionItem {
    pub title: String,
    pub detail: Option<String>,
}

impl ActionItem {
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            detail: None,
        }
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// An ordered list of next steps. The numbering is the list's own, so a
/// screen reader announces each step's position.
#[component]
pub fn ActionList(items: Vec<ActionItem>) -> Element {
    rsx! {
        ol { class: "nav-action-list",
            for item in items.iter() {
                li { class: "nav-action-list__item",
                    strong { class: "nav-action-list__title", "{item.title}" }
                    if let Some(detail) = &item.detail {
                        span { class: "nav-action-list__detail", "{detail}" }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_an_ordered_list_with_optional_details() {
        fn app() -> Element {
            rsx! {
                ActionList {
                    items: vec![
                        ActionItem::new("Sign the engagement letter").detail("Sent to your email."),
                        ActionItem::new("Upload the lease"),
                    ],
                }
            }
        }
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        assert!(
            html.starts_with(r#"<ol class="nav-action-list">"#),
            "{html}"
        );
        assert_eq!(html.matches("<li ").count(), 2, "{html}");
        assert_eq!(html.matches("nav-action-list__detail").count(), 1, "{html}");
        assert!(html.contains("Sign the engagement letter"), "{html}");
    }
}
