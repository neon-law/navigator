//! Fact cards: compact tiles for the supporting facts of a matter, laid out by
//! a responsive [`FactGrid`].

use dioxus::prelude::*;

/// One supporting-fact tile: a short `h3` title over its body.
#[component]
pub fn FactCard(title: String, children: Element) -> Element {
    rsx! {
        article { class: "nav-fact-card",
            h3 { class: "nav-fact-card__title", "{title}" }
            div { class: "nav-fact-card__body", {children} }
        }
    }
}

/// A responsive grid of [`FactCard`]s.
#[component]
pub fn FactGrid(children: Element) -> Element {
    rsx! {
        div { class: "nav-fact-grid", {children} }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_wraps_each_fact_as_an_article() {
        fn app() -> Element {
            rsx! {
                FactGrid {
                    FactCard { title: "Venue".to_string(), "Clark County District Court" }
                    FactCard { title: "Filed".to_string(), "March 3" }
                }
            }
        }
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        assert!(html.starts_with(r#"<div class="nav-fact-grid">"#), "{html}");
        assert_eq!(
            html.matches(r#"<article class="nav-fact-card">"#).count(),
            2,
            "{html}"
        );
        assert!(
            html.contains(r#"<h3 class="nav-fact-card__title">Venue</h3>"#),
            "{html}"
        );
        assert!(html.contains("Clark County District Court"), "{html}");
    }
}
