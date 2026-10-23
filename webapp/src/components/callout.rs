//! The callout: a bordered aside for a caveat, a warning, or good news.
//!
//! A callout is a note beside the content, not a live alert, so it carries
//! `role="note"` and is never announced on load (that is [`super::Toast`]'s
//! job). The tone picks one of the `--nav-*` status bands.

use dioxus::prelude::*;

/// Which status band a [`Callout`] sits on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CalloutTone {
    /// A neutral aside on the raised surface, edged in the link colour.
    #[default]
    Info,
    /// A caveat the reader should weigh before acting.
    Warning,
    /// Something that will go wrong if the reader ignores it.
    Danger,
    /// Good news.
    Success,
}

impl CalloutTone {
    fn class(self) -> &'static str {
        match self {
            CalloutTone::Info => "nav-callout nav-callout--info",
            CalloutTone::Warning => "nav-callout nav-callout--warning",
            CalloutTone::Danger => "nav-callout nav-callout--danger",
            CalloutTone::Success => "nav-callout nav-callout--success",
        }
    }
}

/// A bordered note. `title`, when set, renders as a bold lead line.
#[component]
pub fn Callout(
    children: Element,
    #[props(default)] tone: CalloutTone,
    #[props(default)] title: Option<String>,
) -> Element {
    let class = tone.class();
    rsx! {
        div { class: "{class}", role: "note",
            if let Some(title) = title {
                p { class: "nav-callout__title", strong { "{title}" } }
            }
            div { class: "nav-callout__body", {children} }
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
    fn info_is_the_default_tone_and_a_note() {
        fn app() -> Element {
            rsx! { Callout { "Filing fees are set by the state." } }
        }
        let html = ssr(app);
        assert!(
            html.contains(r#"class="nav-callout nav-callout--info""#),
            "{html}"
        );
        assert!(html.contains(r#"role="note""#), "{html}");
        assert!(!html.contains("nav-callout__title"), "{html}");
    }

    #[test]
    fn every_tone_renders_with_its_title() {
        fn app() -> Element {
            rsx! {
                Callout { tone: CalloutTone::Warning, title: "Deadline".to_string(), "Answer due in 21 days." }
                Callout { tone: CalloutTone::Danger, title: "Missing".to_string(), "No signed engagement letter." }
                Callout { tone: CalloutTone::Success, title: "Filed".to_string(), "Accepted by the clerk." }
            }
        }
        let html = ssr(app);
        for tone in ["warning", "danger", "success"] {
            assert!(
                html.contains(&format!("nav-callout--{tone}")),
                "{tone}: {html}"
            );
        }
        assert!(html.contains("<strong>Deadline</strong>"), "{html}");
        assert_eq!(html.matches(r#"role="note""#).count(), 3, "{html}");
    }
}
