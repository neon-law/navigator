//! The panel: a bordered section with an optional head — the workhorse
//! container of a matter page.
//!
//! A titled panel with an `id` is a `section` named by its own `h2`
//! (`aria-labelledby`), so a screen reader lists it as a region. An untitled
//! panel is a plain `section` with only a body.

use dioxus::prelude::*;

/// A bordered content section.
///
/// - `title`: the `h2`; omit for a body-only panel.
/// - `note`: a secondary line under the title.
/// - `actions`: a right-aligned slot in the head (badges, a button row).
/// - `id`: the section's id. A titled panel needs one so its heading can name
///   the region; without it the heading still renders but names nothing.
#[component]
pub fn Panel(
    children: Element,
    #[props(default)] title: Option<String>,
    #[props(default)] note: Option<String>,
    #[props(default)] actions: Option<Element>,
    #[props(default)] id: Option<String>,
) -> Element {
    let heading_id = match (&id, &title) {
        (Some(id), Some(_)) => Some(format!("{id}-title")),
        _ => None,
    };
    let labelledby = heading_id.clone();
    let has_head = title.is_some() || actions.is_some();
    rsx! {
        section {
            class: "nav-panel",
            id: id,
            "aria-labelledby": labelledby,
            if has_head {
                div { class: "nav-panel__head",
                    div { class: "nav-panel__heading",
                        if let Some(title) = title {
                            h2 { id: heading_id, "{title}" }
                        }
                        if let Some(note) = note {
                            p { class: "nav-panel__note", "{note}" }
                        }
                    }
                    if let Some(actions) = actions {
                        div { class: "nav-panel__actions", {actions} }
                    }
                }
            }
            div { class: "nav-panel__body", {children} }
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
    fn titled_panel_names_its_region_by_its_heading() {
        fn app() -> Element {
            rsx! {
                Panel {
                    id: "deadlines".to_string(),
                    title: "Deadlines".to_string(),
                    note: "Computed from the service date.".to_string(),
                    actions: rsx! { a { href: "/design", "Export" } },
                    p { "Answer due." }
                }
            }
        }
        let html = ssr(app);
        assert!(
            html.contains(
                r#"<section class="nav-panel" id="deadlines" aria-labelledby="deadlines-title""#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<h2 id="deadlines-title">Deadlines</h2>"#),
            "{html}"
        );
        assert!(html.contains("nav-panel__note"), "{html}");
        assert!(html.contains("nav-panel__actions"), "{html}");
        assert!(html.contains("<p>Answer due.</p>"), "{html}");
    }

    #[test]
    fn untitled_panel_renders_only_a_body() {
        fn app() -> Element {
            rsx! { Panel { p { "Body only." } } }
        }
        let html = ssr(app);
        assert!(!html.contains("nav-panel__head"), "{html}");
        assert!(!html.contains("aria-labelledby"), "{html}");
        assert!(
            html.contains(r#"<div class="nav-panel__body"><p>Body only.</p></div>"#),
            "{html}"
        );
    }
}
