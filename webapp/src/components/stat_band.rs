//! The stat grid: a row of headline figures, each with its qualifier, as a
//! Dioxus component.
//!
//! Marketing stat bands ("99% of cases won / 7 weeks average / No upfront
//! fees") state a claim and the asterisked qualifier that keeps it honest, so
//! the qualifier is part of the entry rather than an accessory a page can
//! forget: [`StatEntry::note`] is required, and [`StatGrid`] always renders it
//! beside the figure.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// One stat in a [`StatGrid`].
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct StatEntry {
    /// The headline figure, typeset large: `99%`, `7 weeks`, `No upfront`.
    pub value: String,
    /// What the figure measures.
    pub label: String,
    /// The qualifier the figure travels with (`for many qualifying due
    /// process matters`, `*`). A stat without its qualifier is a bare
    /// advertising claim, so the field is required rather than defaulted and
    /// the grid never renders one without the other.
    pub note: String,
}

/// A responsive grid of stat entries, in the [`FactGrid`](super::FactGrid)
/// style. Renders nothing when `items` is empty.
#[component]
pub fn StatGrid(items: Vec<StatEntry>) -> Element {
    if items.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "nav-stat-grid",
            for entry in items.iter() {
                article { class: "nav-stat",
                    p { class: "nav-stat__value", "{entry.value}" }
                    h3 { class: "nav-stat__label", "{entry.label}" }
                    p { class: "nav-stat__note", "{entry.note}" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssr(items: Vec<StatEntry>) -> String {
        fn app(items: Vec<StatEntry>) -> Element {
            rsx! { StatGrid { items } }
        }
        let mut dom = VirtualDom::new_with_props(app, items);
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    #[test]
    fn each_stat_renders_its_value_label_and_qualifier() {
        let html = ssr(vec![StatEntry {
            value: "99%".to_string(),
            label: "of cases won".to_string(),
            note: "for many qualifying due process matters".to_string(),
        }]);
        assert!(
            html.contains(r#"<p class="nav-stat__value">99%</p>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<h3 class="nav-stat__label">of cases won</h3>"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<p class="nav-stat__note">for many qualifying due process matters</p>"#
            ),
            "{html}"
        );
    }

    #[test]
    fn empty_items_render_no_grid() {
        assert!(ssr(Vec::new()).is_empty());
    }
}
