//! The cite-check split view: a draft beside the record page it cites.
//!
//! A drafted assertion sits on the left, the source page it cites on the
//! right, and the draft's record citations are listed under the split — each
//! marked with what its check says (`draft`, `verified`, `review`), the way
//! the authorities section marks a drafted assertion beside its source. The
//! view is data-driven: a server loader parses the draft (the `views` crate's
//! `record_citation` parser is the producer) and hands the sides and citations
//! in, so the component never parses and renders at once.

use dioxus::prelude::*;

/// Which pane of the split a side occupies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CiteCheckPane {
    /// The draft under review.
    #[default]
    Draft,
    /// The record page the draft cites.
    Source,
}

impl CiteCheckPane {
    /// The pane's name as a class modifier.
    #[must_use]
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Source => "source",
        }
    }
}

/// What a citation's check says for the reader. The tone names the state,
/// never a colour, so every brand re-tones it through the `--nav-*` bands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CiteCheckStatus {
    /// Cited but not yet checked against its record page.
    #[default]
    Draft,
    /// Checked: the record page says what the draft says it says.
    Verified,
    /// A short form or unresolved reference a lawyer should look at.
    Review,
}

impl CiteCheckStatus {
    /// The status's name as a class modifier, or `None` for the default.
    #[must_use]
    pub fn suffix(self) -> Option<&'static str> {
        match self {
            Self::Draft => Some("draft"),
            Self::Verified => Some("verified"),
            Self::Review => Some("review"),
        }
    }

    /// The status's name as the reader sees it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Draft => "Draft",
            Self::Verified => "Verified",
            Self::Review => "Review",
        }
    }
}

/// One side of the split: a titled pane of text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CiteCheckSide {
    /// Which pane this is.
    pub pane: CiteCheckPane,
    /// The pane's heading.
    pub title: String,
    /// The pane's text, pre-wrapped — a draft body or a record page.
    pub body: String,
}

/// One marked citation under the split.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CiteCheckCitation {
    /// The citation as drafted (`Complaint at 4:12–19`).
    pub reference: String,
    /// The sentence of the draft that contains the citation.
    pub sentence: String,
    /// What the citation's check says.
    pub status: CiteCheckStatus,
}

/// The cite-check split view: draft beside source, citations marked under it.
#[component]
pub fn CiteCheckView(
    id: String,
    draft: CiteCheckSide,
    source: CiteCheckSide,
    #[props(default)] citations: Vec<CiteCheckCitation>,
) -> Element {
    rsx! {
        div { class: "nav-cite-check", id,
            div { class: "nav-cite-check__side nav-cite-check__side--{draft.pane.suffix()}",
                h3 { class: "nav-cite-check__title", "{draft.title}" }
                pre { class: "nav-cite-check__body", "{draft.body}" }
            }
            div { class: "nav-cite-check__side nav-cite-check__side--{source.pane.suffix()}",
                h3 { class: "nav-cite-check__title", "{source.title}" }
                pre { class: "nav-cite-check__body", "{source.body}" }
            }
            if citations.is_empty() {
                div { class: "nav-cite-check__empty",
                    p { "No record citations in this draft yet — cite a document's page, paragraph, or exhibit and its check appears here." }
                }
            } else {
                ul { class: "nav-cite-check__citations",
                    for citation in &citations {
                        li { class: "nav-cite-check__citation",
                            span {
                                class: "nav-cite-check__status nav-cite-check__status--{citation.status.suffix().unwrap_or_default()}",
                                "{citation.status.label()}"
                            }
                            span { class: "nav-cite-check__reference", "{citation.reference}" }
                            span { class: "nav-cite-check__sentence", "{citation.sentence}" }
                        }
                    }
                }
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

    fn draft_side() -> CiteCheckSide {
        CiteCheckSide {
            pane: CiteCheckPane::Draft,
            title: "Draft: Opposition".into(),
            body: "The Complaint at 4:12–19 alleges the breach.".into(),
        }
    }

    fn source_side() -> CiteCheckSide {
        CiteCheckSide {
            pane: CiteCheckPane::Source,
            title: "Complaint (Doc. 3)".into(),
            body: "¶ 1. The parties signed a written supply contract.".into(),
        }
    }

    #[test]
    fn the_draft_renders_beside_its_source() {
        fn app() -> Element {
            rsx! {
                CiteCheckView { id: "cite-check".to_string(), draft: draft_side(), source: source_side() }
            }
        }
        let html = ssr(app);
        assert!(html.contains("nav-cite-check__side--draft"), "{html}");
        assert!(html.contains("nav-cite-check__side--source"), "{html}");
        assert!(html.contains("Draft: Opposition"), "{html}");
        assert!(html.contains("Complaint (Doc. 3)"), "{html}");
        assert!(html.contains("supply contract"), "{html}");
    }

    #[test]
    fn draft_citations_are_marked() {
        fn app() -> Element {
            rsx! {
                CiteCheckView {
                    id: "cite-check".to_string(),
                    draft: draft_side(),
                    source: source_side(),
                    citations: vec![CiteCheckCitation {
                        reference: "Complaint at 4:12–19".into(),
                        sentence: "The Complaint at 4:12–19 alleges the breach.".into(),
                        status: CiteCheckStatus::Draft,
                    }],
                }
            }
        }
        let html = ssr(app);
        assert!(
            html.contains("nav-cite-check__status--draft"),
            "a draft citation carries its tone: {html}"
        );
        assert!(html.contains("Complaint at 4:12–19"), "{html}");
    }

    #[test]
    fn verified_citations_render_their_tone() {
        fn app() -> Element {
            rsx! {
                CiteCheckView {
                    id: "cite-check".to_string(),
                    draft: draft_side(),
                    source: source_side(),
                    citations: vec![CiteCheckCitation {
                        reference: "Complaint ¶ 14".into(),
                        sentence: "The Complaint ¶ 14 alleges the breach.".into(),
                        status: CiteCheckStatus::Verified,
                    }],
                }
            }
        }
        let html = ssr(app);
        assert!(html.contains("nav-cite-check__status--verified"), "{html}");
        assert!(html.contains("Verified"), "{html}");
    }

    #[test]
    fn review_citations_render_their_tone() {
        fn app() -> Element {
            rsx! {
                CiteCheckView {
                    id: "cite-check".to_string(),
                    draft: draft_side(),
                    source: source_side(),
                    citations: vec![CiteCheckCitation {
                        reference: "Id. at 5".into(),
                        sentence: "Id. at 5 adds the rest.".into(),
                        status: CiteCheckStatus::Review,
                    }],
                }
            }
        }
        let html = ssr(app);
        assert!(html.contains("nav-cite-check__status--review"), "{html}");
        assert!(html.contains("Review"), "{html}");
    }

    #[test]
    fn an_empty_citation_list_renders_the_empty_state() {
        fn app() -> Element {
            rsx! {
                CiteCheckView { id: "cite-check".to_string(), draft: draft_side(), source: source_side() }
            }
        }
        let html = ssr(app);
        assert!(html.contains("nav-cite-check__empty"), "{html}");
        assert!(!html.contains("nav-cite-check__citations"), "{html}");
    }
}
