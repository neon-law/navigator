//! The toned status pill.
//!
//! A small inline label for a status or a provenance. The tone names what the
//! label means for the reader (ready, under review, blocked), never a colour, so
//! every brand re-tones it through the `--nav-*` bands without a markup change.

use dioxus::prelude::*;

/// What a [`Badge`] says about the thing it labels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BadgeTone {
    /// A neutral label: the brand tint.
    #[default]
    Default,
    /// The current item: a filled brand pill.
    Active,
    /// Done or good to go: the success band.
    Ready,
    /// Coming up: the notice band.
    Next,
    /// Waiting on a lawyer or a client: the warning band.
    Review,
    /// Cannot move until something else happens: the danger band.
    Blocked,
    /// Where a fact came from: a bordered neutral pill.
    Source,
}

impl BadgeTone {
    /// The tone's name as a class suffix, or `None` for the default tone.
    #[must_use]
    pub fn suffix(self) -> Option<&'static str> {
        match self {
            BadgeTone::Default => None,
            BadgeTone::Active => Some("active"),
            BadgeTone::Ready => Some("ready"),
            BadgeTone::Next => Some("next"),
            BadgeTone::Review => Some("review"),
            BadgeTone::Blocked => Some("blocked"),
            BadgeTone::Source => Some("source"),
        }
    }

    /// The full class list for a pill in this tone.
    #[must_use]
    pub fn class(self) -> String {
        match self.suffix() {
            Some(suffix) => format!("nav-badge nav-badge--{suffix}"),
            None => "nav-badge".to_string(),
        }
    }
}

/// A small pill carrying a status or provenance label.
#[component]
pub fn Badge(children: Element, #[props(default)] tone: BadgeTone) -> Element {
    let class = tone.class();
    rsx! {
        span { class: "{class}", {children} }
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
    fn default_badge_is_the_plain_pill() {
        fn app() -> Element {
            rsx! { Badge { "Draft" } }
        }
        assert_eq!(ssr(app), r#"<span class="nav-badge">Draft</span>"#);
    }

    #[test]
    fn each_tone_adds_its_modifier() {
        fn app() -> Element {
            rsx! {
                Badge { tone: BadgeTone::Active, "Active" }
                Badge { tone: BadgeTone::Ready, "Ready" }
                Badge { tone: BadgeTone::Next, "Next" }
                Badge { tone: BadgeTone::Review, "Review" }
                Badge { tone: BadgeTone::Blocked, "Blocked" }
                Badge { tone: BadgeTone::Source, "Source" }
            }
        }
        let html = ssr(app);
        for tone in ["active", "ready", "next", "review", "blocked", "source"] {
            assert!(
                html.contains(&format!(r#"class="nav-badge nav-badge--{tone}""#)),
                "{tone}: {html}"
            );
        }
    }
}
