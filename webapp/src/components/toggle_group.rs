//! The toggle group: a single-select segmented control that filters what is
//! on screen.
//!
//! Link-driven, like [`super::Tabs`]: each option is a plain anchor whose
//! `href` the caller supplies (usually the same page with a different query
//! value), so the selected view is in the server-rendered HTML and the control
//! works with no JavaScript. The selected option carries `aria-current="true"`;
//! `aria-pressed` belongs to buttons, and a link that claims to be pressed
//! tells a reader the wrong thing about what activating it does. A disabled
//! option renders as a non-link span with `aria-disabled`.

use dioxus::prelude::*;

/// One option in a [`ToggleGroup`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToggleOption {
    pub label: String,
    pub href: String,
    pub selected: bool,
    pub disabled: bool,
}

impl ToggleOption {
    #[must_use]
    pub fn new(label: impl Into<String>, href: impl Into<String>, selected: bool) -> Self {
        Self {
            label: label.into(),
            href: href.into(),
            selected,
            disabled: false,
        }
    }

    #[must_use]
    pub fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }
}

/// A labelled group of link toggles. `label` names the group for assistive
/// technology.
#[component]
pub fn ToggleGroup(label: String, options: Vec<ToggleOption>) -> Element {
    rsx! {
        div { class: "nav-toggle-group", role: "group", "aria-label": "{label}",
            for option in options.iter() {
                if option.disabled {
                    span {
                        class: "nav-toggle nav-toggle--disabled",
                        "aria-disabled": "true",
                        "{option.label}"
                    }
                } else if option.selected {
                    a {
                        class: "nav-toggle nav-toggle--on",
                        href: "{option.href}",
                        "aria-current": "true",
                        "{option.label}"
                    }
                } else {
                    a { class: "nav-toggle", href: "{option.href}", "{option.label}" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_are_links_and_the_selected_one_is_current() {
        fn app() -> Element {
            rsx! {
                ToggleGroup {
                    label: "Show".to_string(),
                    options: vec![
                        ToggleOption::new("All", "?view=all", true),
                        ToggleOption::new("Open", "?view=open", false),
                        ToggleOption::new("Archived", "?view=archived", false).disabled(),
                    ],
                }
            }
        }
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        assert!(
            html.contains(r#"<div class="nav-toggle-group" role="group" aria-label="Show">"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<a class="nav-toggle nav-toggle--on" href="?view=all" aria-current="true">All</a>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<a class="nav-toggle" href="?view=open">Open</a>"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<span class="nav-toggle nav-toggle--disabled" aria-disabled="true">Archived</span>"#
            ),
            "{html}"
        );
        assert!(
            !html.contains("<button"),
            "no script-driven control: {html}"
        );
        assert!(!html.contains("aria-pressed"), "{html}");
    }
}
