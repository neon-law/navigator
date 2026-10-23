//! One notation as one collapsible document: a native `<details>` whose
//! summary names the document and whose body is its Harvard outline.
//!
//! The component renders data, not Markdown. A page parses a template body
//! with `views::harvard_outline` and maps it through
//! `crate::notation_outline::notation_document`; a document the firm
//! transcribed rather than authored builds the same [`NotationDocumentData`]
//! by hand and renders the same way.
//!
//! What the outline shows:
//!
//! - every outline unit nested under its parent, depth one as a heading and a
//!   quoted subsection (`> **A. Label.**`, `> **(a) Label.**`) as a block quote;
//! - paragraphs, **holds** (a parked drafting decision, flagged in words as
//!   well as colour), and quotations beneath the unit they sit in;
//! - **open terms**, the `{{question_code}}` runs a questionnaire has not
//!   answered, highlighted and linked to the answering question when the
//!   page supplies its href;
//! - a stable `id` on every unit and block, from the Harvard path, with a
//!   link to it beside each one so a reader can take the address. The ids
//!   are in the markup whether the document is open or closed.
//!
//! The content needs no JavaScript: `<details>` opens and closes natively,
//! and every link is a plain fragment anchor. One small progressive
//! enhancement, [`NOTATION_ANCHOR_SCRIPT_HREF`], opens the disclosure a deep
//! link lands inside in browsers that do not do so natively.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use super::{Badge, BadgeTone, Empty};

/// The stylesheet the document is styled by, shared with the narration stage.
pub const NOTATION_DOCUMENT_STYLESHEET_HREF: &str = "/public/css/harvard-outline.css";

/// Opens every ancestor `<details>` of `location.hash` on load and on
/// `hashchange`.
pub const NOTATION_ANCHOR_SCRIPT_HREF: &str = "/public/js/notation-anchor.js";

/// A notation ready to render.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default, Debug)]
pub struct NotationDocumentData {
    pub title: String,
    /// How depth-one units are numbered, as a reader reads it
    /// (`Roman numerals`), or `None` for a document with no outline.
    pub numbering: Option<String>,
    /// Blocks before the first heading.
    pub preamble: Vec<NotationBlock>,
    pub sections: Vec<NotationSection>,
}

impl NotationDocumentData {
    /// Whether the document has nothing to show.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.preamble.is_empty() && self.sections.is_empty()
    }

    /// Every block in reading order, preamble first.
    fn blocks(&self) -> Vec<&NotationBlock> {
        fn walk<'a>(sections: &'a [NotationSection], out: &mut Vec<&'a NotationBlock>) {
            for section in sections {
                out.extend(section.blocks.iter());
                walk(&section.sections, out);
            }
        }
        let mut out: Vec<&NotationBlock> = self.preamble.iter().collect();
        walk(&self.sections, &mut out);
        out
    }

    /// How many holds the document carries.
    #[must_use]
    pub fn hold_count(&self) -> usize {
        self.blocks()
            .into_iter()
            .filter(|block| block.kind == NotationBlockKind::Hold)
            .count()
    }

    /// The distinct questions whose open terms the document carries.
    #[must_use]
    pub fn open_term_count(&self) -> usize {
        fn runs_in(sections: &[NotationSection], out: &mut Vec<String>) {
            for section in sections {
                collect(&section.heading, out);
                for block in &section.blocks {
                    collect(&block.runs, out);
                }
                runs_in(&section.sections, out);
            }
        }
        fn collect(runs: &[NotationRun], out: &mut Vec<String>) {
            for run in runs {
                if let NotationRun::OpenTerm { question, .. } = run {
                    if !out.contains(question) {
                        out.push(question.clone());
                    }
                }
            }
        }
        let mut questions = Vec::new();
        for block in &self.preamble {
            collect(&block.runs, &mut questions);
        }
        runs_in(&self.sections, &mut questions);
        questions.len()
    }
}

/// One outline unit with what sits beneath it.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default, Debug)]
pub struct NotationSection {
    /// The unit's DOM id, from its Harvard path (`section-ii-a`).
    pub fragment: String,
    /// Displayed marker (`I`, `A`, `(a)`, `Section 1.01`), empty for an
    /// unnumbered heading.
    pub marker: String,
    /// Harvard path (`II.A`), the unit's name in a link label.
    pub path: String,
    /// A quoted subsection rather than a heading.
    pub quoted: bool,
    /// The caption, or the opening text of a quoted subsection.
    pub heading: Vec<NotationRun>,
    pub blocks: Vec<NotationBlock>,
    pub sections: Vec<NotationSection>,
}

/// What kind of block sits beneath a unit.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum NotationBlockKind {
    #[default]
    Paragraph,
    /// A parked drafting decision; the document is not ready to leave.
    Hold,
    /// Quoted material.
    Quotation,
}

/// A paragraph, hold, or quotation.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default, Debug)]
pub struct NotationBlock {
    /// The block's DOM id (`section-ii-a-p2`).
    pub fragment: String,
    pub kind: NotationBlockKind,
    pub runs: Vec<NotationRun>,
}

/// One inline run.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub enum NotationRun {
    /// Firm-authored inline HTML rendered from the template's Markdown.
    Text { html: String },
    /// A term the questionnaire has not answered yet.
    OpenTerm { label: String, question: String },
}

/// Where the question that answers an open term lives on the page.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct OpenTermLink {
    pub question: String,
    pub href: String,
}

/// One notation as one `<details>` disclosure around its Harvard outline.
///
/// `id` names the disclosure itself, so a link can land on the whole
/// document. `open` seeds the pre-hydration state. `after` renders inside the
/// disclosure once the outline ends (a signature block, a reply). The summary
/// is the document's only heading-like label: the component renders no
/// `<h1>`, so it sits under whatever heading the page already has.
#[component]
pub fn NotationDocument(
    document: NotationDocumentData,
    id: String,
    #[props(default)] open: bool,
    #[props(default)] open_terms: Vec<OpenTermLink>,
    #[props(default)] after: Option<Element>,
) -> Element {
    let holds = document.hold_count();
    let terms = document.open_term_count();
    let hold_label = if holds == 1 {
        "1 hold".to_string()
    } else {
        format!("{holds} holds")
    };
    let term_label = if terms == 1 {
        "1 open term".to_string()
    } else {
        format!("{terms} open terms")
    };
    rsx! {
        document::Stylesheet { href: NOTATION_DOCUMENT_STYLESHEET_HREF }
        document::Script { src: NOTATION_ANCHOR_SCRIPT_HREF, defer: true }
        details { class: "notation-document", id: "{id}", open,
            summary { class: "notation-document__summary",
                span { class: "notation-document__title", "{document.title}" }
                span { class: "notation-document__badges",
                    if let Some(numbering) = document.numbering.as_ref() {
                        Badge { tone: BadgeTone::Source, "{numbering}" }
                    }
                    if holds > 0 {
                        Badge { tone: BadgeTone::Blocked, "{hold_label}" }
                    }
                    if terms > 0 {
                        Badge { tone: BadgeTone::Review, "{term_label}" }
                    }
                }
            }
            div { class: "notation-document__body",
                if document.is_empty() {
                    Empty {
                        title: "No outline yet".to_string(),
                        description: "This notation has no provisions to show.".to_string(),
                    }
                }
                for block in document.preamble.iter() {
                    {block_view(block, &open_terms)}
                }
                for section in document.sections.iter() {
                    {section_view(section, 1, &open_terms)}
                }
                if let Some(after) = after {
                    div { class: "notation-document__after", {after} }
                }
            }
        }
    }
}

/// The marker as it is printed: `I.`, `A.`, `1.`, and `a.` take their period;
/// `(a)`, `1.1`, and `Section 1.01` are already complete.
fn printed_marker(marker: &str) -> String {
    if !marker.is_empty() && marker.chars().all(|c| c.is_ascii_alphanumeric()) {
        format!("{marker}.")
    } else {
        marker.to_string()
    }
}

fn anchor_link(fragment: &str, label: &str) -> Element {
    rsx! {
        a {
            class: "notation-anchor",
            href: "#{fragment}",
            "aria-label": "Link to {label}",
            title: "Link to {label}",
            span { "aria-hidden": "true", "#" }
        }
    }
}

fn runs_view(runs: &[NotationRun], links: &[OpenTermLink]) -> Element {
    rsx! {
        for run in runs.iter() {
            match run {
                NotationRun::Text { html } => rsx! {
                    span { dangerous_inner_html: "{html}" }
                },
                NotationRun::OpenTerm { label, question } => {
                    let href = links
                        .iter()
                        .find(|link| &link.question == question)
                        .map(|link| link.href.clone());
                    rsx! {
                        if let Some(href) = href {
                            a { class: "notation-open-term", href: "{href}",
                                mark { "{label}" }
                                span { class: "nav-visually-hidden", " (open term, answer this question)" }
                            }
                        } else {
                            mark { class: "notation-open-term",
                                "{label}"
                                span { class: "nav-visually-hidden", " (open term)" }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn block_view(block: &NotationBlock, links: &[OpenTermLink]) -> Element {
    let anchor = anchor_link(&block.fragment, "this paragraph");
    match block.kind {
        NotationBlockKind::Paragraph => rsx! {
            p { class: "notation-block", id: "{block.fragment}",
                {runs_view(&block.runs, links)}
                " "
                {anchor}
            }
        },
        NotationBlockKind::Hold => rsx! {
            p { class: "notation-block notation-block--hold", id: "{block.fragment}",
                strong { class: "notation-block__flag", "Hold" }
                " "
                {runs_view(&block.runs, links)}
                " "
                {anchor}
            }
        },
        NotationBlockKind::Quotation => rsx! {
            blockquote { class: "notation-block notation-block--quotation", id: "{block.fragment}",
                p {
                    {runs_view(&block.runs, links)}
                    " "
                    {anchor}
                }
            }
        },
    }
}

fn section_view(section: &NotationSection, depth: usize, links: &[OpenTermLink]) -> Element {
    let marker = printed_marker(&section.marker);
    let label = if section.path.is_empty() {
        "this heading".to_string()
    } else {
        format!("provision {}", section.path)
    };
    let class = format!(
        "notation-unit notation-unit--depth-{}{}",
        depth.min(4),
        if section.quoted {
            " notation-unit--quoted"
        } else {
            ""
        }
    );
    let lead = rsx! {
        if !marker.is_empty() {
            span { class: "notation-unit__marker", "{marker}" }
            " "
        }
        {runs_view(&section.heading, links)}
        " "
        {anchor_link(&section.fragment, &label)}
    };
    let body = rsx! {
        for block in section.blocks.iter() {
            {block_view(block, links)}
        }
        for child in section.sections.iter() {
            {section_view(child, depth + 1, links)}
        }
    };
    if section.quoted {
        rsx! {
            section { class: "{class}", id: "{section.fragment}",
                blockquote { class: "notation-unit__quote",
                    p { class: "notation-unit__lead", {lead} }
                    {body}
                }
            }
        }
    } else if depth == 1 {
        rsx! {
            section { class: "{class}", id: "{section.fragment}",
                h3 { class: "notation-unit__heading", {lead} }
                {body}
            }
        }
    } else {
        rsx! {
            section { class: "{class}", id: "{section.fragment}",
                h4 { class: "notation-unit__heading", {lead} }
                {body}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(html: &str) -> NotationRun {
        NotationRun::Text {
            html: html.to_string(),
        }
    }

    fn block(fragment: &str, kind: NotationBlockKind, runs: Vec<NotationRun>) -> NotationBlock {
        NotationBlock {
            fragment: fragment.to_string(),
            kind,
            runs,
        }
    }

    fn sample() -> NotationDocumentData {
        NotationDocumentData {
            title: "Sample Motion".to_string(),
            numbering: Some("Arabic numerals".to_string()),
            preamble: vec![block(
                "preamble-p1",
                NotationBlockKind::Paragraph,
                vec![text("Comes now the plaintiff.")],
            )],
            sections: vec![NotationSection {
                fragment: "section-2".to_string(),
                marker: "2".to_string(),
                path: "2".to_string(),
                quoted: false,
                heading: vec![text("Argument")],
                blocks: vec![
                    block(
                        "section-2-p1",
                        NotationBlockKind::Quotation,
                        vec![text("<em>The standard is de novo.</em>")],
                    ),
                    block(
                        "section-2-p2",
                        NotationBlockKind::Hold,
                        vec![text("Confirm the venue.")],
                    ),
                ],
                sections: vec![NotationSection {
                    fragment: "section-2-a".to_string(),
                    marker: "A".to_string(),
                    path: "2.A".to_string(),
                    quoted: true,
                    heading: vec![
                        text("Damages of "),
                        NotationRun::OpenTerm {
                            label: "custom_money__damages".to_string(),
                            question: "custom_money__damages".to_string(),
                        },
                        text(" by "),
                        NotationRun::OpenTerm {
                            label: "person__defendant.name".to_string(),
                            question: "person__defendant".to_string(),
                        },
                    ],
                    blocks: Vec::new(),
                    sections: Vec::new(),
                }],
            }],
        }
    }

    fn render(document: NotationDocumentData, open: bool, links: Vec<OpenTermLink>) -> String {
        dioxus_ssr::render_element(rsx! {
            NotationDocument {
                document,
                id: "notation-sample".to_string(),
                open,
                open_terms: links,
            }
        })
    }

    #[test]
    fn renders_one_details_disclosure_named_by_its_title_with_no_h1() {
        let html = render(sample(), false, Vec::new());
        assert_eq!(html.matches("<details").count(), 1, "{html}");
        assert!(html.contains("id=\"notation-sample\""), "{html}");
        assert!(
            html.contains("<details class=\"notation-document\" id=\"notation-sample\">"),
            "closed by default: {html}"
        );
        assert!(html.contains("<summary"), "{html}");
        assert!(html.contains("Sample Motion"), "{html}");
        assert!(html.contains("Arabic numerals"), "{html}");
        assert!(!html.contains("<h1"), "{html}");
        let open = render(sample(), true, Vec::new());
        assert!(open.contains("id=\"notation-sample\" open"), "{open}");
    }

    #[test]
    fn every_unit_and_block_carries_its_id_and_a_link_to_it_even_closed() {
        let html = render(sample(), false, Vec::new());
        for fragment in [
            "preamble-p1",
            "section-2",
            "section-2-p1",
            "section-2-p2",
            "section-2-a",
        ] {
            assert!(
                html.contains(&format!("id=\"{fragment}\"")),
                "{fragment}: {html}"
            );
            assert!(
                html.contains(&format!("href=\"#{fragment}\"")),
                "{fragment}: {html}"
            );
        }
        assert!(
            html.contains("aria-label=\"Link to provision 2.A\""),
            "{html}"
        );
    }

    #[test]
    fn a_depth_one_unit_is_a_heading_and_a_lettered_subsection_a_quote() {
        let html = render(sample(), true, Vec::new());
        assert!(
            html.contains("<h3 class=\"notation-unit__heading\""),
            "{html}"
        );
        assert!(html.contains(">2.<"), "{html}");
        assert!(html.contains("notation-unit--quoted"), "{html}");
        assert!(
            html.contains("<blockquote class=\"notation-unit__quote\""),
            "{html}"
        );
        assert!(html.contains(">A.<"), "{html}");
    }

    #[test]
    fn each_block_kind_renders_its_own_shape() {
        let html = render(sample(), true, Vec::new());
        assert!(
            html.contains("<p class=\"notation-block\" id=\"preamble-p1\""),
            "{html}"
        );
        assert!(
            html.contains("<blockquote class=\"notation-block notation-block--quotation\""),
            "{html}"
        );
        assert!(html.contains("<em>The standard is de novo.</em>"), "{html}");
        assert!(html.contains("notation-block--hold"), "{html}");
        assert!(
            html.contains(">Hold<"),
            "the hold is flagged in words: {html}"
        );
        assert!(html.contains("1 hold"), "{html}");
    }

    #[test]
    fn open_terms_are_marked_and_linked_to_the_answering_question() {
        let html = render(
            sample(),
            true,
            vec![OpenTermLink {
                question: "person__defendant".to_string(),
                href: "#question-person__defendant".to_string(),
            }],
        );
        assert!(html.contains("2 open terms"), "{html}");
        assert!(
            html.contains("<mark class=\"notation-open-term\">custom_money__damages"),
            "an unlinked term is a plain mark: {html}"
        );
        assert!(
            html.contains("href=\"#question-person__defendant\""),
            "a linked term lands on its question: {html}"
        );
        assert!(html.contains("person__defendant.name"), "{html}");
        assert!(html.contains("(open term"), "{html}");
    }

    /// The deep-link script is hoisted by the component, so every page that
    /// renders a Notation gets it, and it opens ancestors on load and on
    /// `hashchange`. No JS test runner exists in the workspace; the browser
    /// behaviour is checked by hand (see the PR) against this source.
    #[test]
    fn the_component_hoists_the_deep_link_script() {
        let src = include_str!("notation_document.rs");
        assert!(
            src.contains("document::Script { src: NOTATION_ANCHOR_SCRIPT_HREF, defer: true }"),
            "the component must hoist the deep-link script"
        );
        let js = include_str!("../../../server/public/js/notation-anchor.js");
        assert!(js.contains("addEventListener(\"hashchange\""), "{js}");
        assert!(js.contains("node.tagName === \"DETAILS\""), "{js}");
        assert!(js.contains("node.open = true"), "{js}");
        assert!(js.contains("parentElement"), "{js}");
    }

    #[test]
    fn printed_markers_take_a_period_only_when_bare() {
        assert_eq!(printed_marker("IV"), "IV.");
        assert_eq!(printed_marker("a"), "a.");
        assert_eq!(printed_marker("(a)"), "(a)");
        assert_eq!(printed_marker("1.1"), "1.1");
        assert_eq!(printed_marker("Section 1.01"), "Section 1.01");
        assert_eq!(printed_marker(""), "");
    }

    #[test]
    fn an_empty_document_shows_the_empty_state() {
        let html = render(
            NotationDocumentData {
                title: "Blank".to_string(),
                ..NotationDocumentData::default()
            },
            true,
            Vec::new(),
        );
        assert!(html.contains("nav-empty"), "{html}");
        assert!(html.contains("No outline yet"), "{html}");
        assert!(!html.contains("notation-unit"), "{html}");
    }

    #[test]
    fn the_after_slot_renders_inside_the_disclosure() {
        let html = dioxus_ssr::render_element(rsx! {
            NotationDocument {
                document: sample(),
                id: "notation-sample".to_string(),
                after: rsx! { p { "Signature block" } },
            }
        });
        let close = html.find("</details>").expect("details closes");
        let after = html.find("Signature block").expect("after renders");
        assert!(after < close, "{html}");
    }
}
