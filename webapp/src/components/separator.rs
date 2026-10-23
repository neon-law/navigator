//! The separator: a rule between two groups.
//!
//! A `div` rather than an `hr`, because `hr` is a thematic break in content and
//! a vertical rule between two toolbar groups is not one. A decorative rule is
//! hidden from assistive technology (`role="none"`); a structural one is a
//! `separator`, and states its orientation only when vertical, since
//! horizontal is the role's default.

use dioxus::prelude::*;

/// Which way a [`Separator`] runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SeparatorOrientation {
    #[default]
    Horizontal,
    Vertical,
}

/// A horizontal or vertical rule.
#[component]
pub fn Separator(
    #[props(default)] orientation: SeparatorOrientation,
    #[props(default)] decorative: bool,
) -> Element {
    let (class, vertical) = match orientation {
        SeparatorOrientation::Horizontal => ("nav-separator nav-separator--horizontal", false),
        SeparatorOrientation::Vertical => ("nav-separator nav-separator--vertical", true),
    };
    let role = if decorative { "none" } else { "separator" };
    let aria_orientation = (vertical && !decorative).then_some("vertical");
    rsx! {
        div {
            class: "{class}",
            role: "{role}",
            "aria-orientation": aria_orientation,
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
    fn horizontal_is_a_structural_separator_by_default() {
        fn app() -> Element {
            rsx! { Separator {} }
        }
        assert_eq!(
            ssr(app),
            r#"<div class="nav-separator nav-separator--horizontal" role="separator"></div>"#
        );
    }

    #[test]
    fn vertical_states_its_orientation_unless_decorative() {
        fn structural() -> Element {
            rsx! { Separator { orientation: SeparatorOrientation::Vertical } }
        }
        fn decorative() -> Element {
            rsx! { Separator { orientation: SeparatorOrientation::Vertical, decorative: true } }
        }
        assert!(ssr(structural).contains(r#"aria-orientation="vertical""#));
        let html = ssr(decorative);
        assert!(html.contains(r#"role="none""#), "{html}");
        assert!(!html.contains("aria-orientation"), "{html}");
    }
}
