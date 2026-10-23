//! The status strip: a row of labelled values summarising where a matter
//! stands, as a description list so each label is read with its value.

use dioxus::prelude::*;

use super::BadgeTone;

/// One labelled value in a [`StatusStrip`]. `tone` reuses the [`BadgeTone`]
/// vocabulary; a non-default tone edges the cell in that status band.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusCell {
    pub label: String,
    pub value: String,
    pub tone: BadgeTone,
}

impl StatusCell {
    #[must_use]
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            tone: BadgeTone::Default,
        }
    }

    #[must_use]
    pub fn tone(mut self, tone: BadgeTone) -> Self {
        self.tone = tone;
        self
    }

    fn class(&self) -> String {
        match self.tone.suffix() {
            Some(suffix) => {
                format!("nav-status-strip__cell nav-status-strip__cell--{suffix}")
            }
            None => "nav-status-strip__cell".to_string(),
        }
    }
}

/// A row of [`StatusCell`]s.
#[component]
pub fn StatusStrip(cells: Vec<StatusCell>) -> Element {
    rsx! {
        dl { class: "nav-status-strip",
            for cell in cells.iter() {
                div { class: "{cell.class()}",
                    dt { class: "nav-status-strip__label", "{cell.label}" }
                    dd { class: "nav-status-strip__value", "{cell.value}" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_cell_pairs_its_label_with_its_value() {
        fn app() -> Element {
            rsx! {
                StatusStrip {
                    cells: vec![
                        StatusCell::new("Stage", "Discovery"),
                        StatusCell::new("Next deadline", "May 2").tone(BadgeTone::Review),
                    ],
                }
            }
        }
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        assert!(
            html.starts_with(r#"<dl class="nav-status-strip">"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<dt class="nav-status-strip__label">Stage</dt><dd class="nav-status-strip__value">Discovery</dd>"#
            ),
            "{html}"
        );
        assert!(
            html.contains("nav-status-strip__cell nav-status-strip__cell--review"),
            "{html}"
        );
    }
}
