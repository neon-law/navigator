//! A Notation's checklist: one step per [Hold](../../../docs/glossary/hold.md)
//! and per fill-in blank, in reading order.
//!
//! Ported from the retired React package's `deriveNotationChecklist` and kept
//! to its rules, step for step:
//!
//! - A hold whose text reads `LABEL — detail` becomes a step titled by the
//!   label in sentence case, with the whole detail beneath it. Any other hold
//!   is titled by its first sentence, and the rest is the detail.
//! - A hold whose text starts `Resolved.` is a checked step.
//! - Each bracketed blank inside a paragraph (`Name: [CLIENT NAME]`) is an
//!   open step titled by the words before the first bracket, else by the
//!   blank's own words, else *Complete this blank*.
//! - Blocks before the first heading carry no steps, as in the original.
//!
//! One deliberate difference: a Markdown link (`[text](url)`) is prose here,
//! never a blank, matching how [`views::harvard_outline::hold_body`] already
//! reads a bracketed link.

use serde::{Deserialize, Serialize};

/// One block of a parsed Notation, as the checklist reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChecklistBlock {
    /// The block's DOM id.
    pub id: String,
    /// A hold rather than prose.
    pub hold: bool,
    /// A hold's inner text, or the prose's Markdown, on one line.
    pub text: String,
}

/// One step of the checklist.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecklistStep {
    /// `<source id>-check-<n>`, stable across re-parses.
    pub id: String,
    /// The DOM id of the block the step came from.
    pub source_id: String,
    pub title: String,
    pub detail: Option<String>,
    pub checked: bool,
}

/// The steps `blocks` leave open, in reading order.
#[must_use]
pub fn derive(blocks: &[ChecklistBlock]) -> Vec<ChecklistStep> {
    let mut steps = Vec::new();
    for block in blocks {
        if block.hold {
            let label = labelled(&block.text);
            let content = label.map_or(block.text.as_str(), |(_, detail)| detail);
            let resolved = content
                .trim()
                .get(..9)
                .is_some_and(|lead| lead.eq_ignore_ascii_case("resolved."));
            let (title, detail) = match label {
                Some((label, detail)) => (sentence_case(label), Some(detail.to_string())),
                None => first_sentence(content),
            };
            steps.push(ChecklistStep {
                id: format!("{}-check-1", block.id),
                source_id: block.id.clone(),
                title,
                detail,
                checked: resolved,
            });
            continue;
        }
        let lead = block.text.find('[').map_or("", |i| &block.text[..i]);
        let lead = lead.trim().trim_end_matches([':', '—', '–', '-']).trim();
        for (index, blank) in blanks(&block.text).into_iter().enumerate() {
            let words = blank.trim_matches(|c: char| c == '.' || c == '…' || c.is_whitespace());
            let title = if !lead.is_empty() {
                sentence_case(lead)
            } else if !words.is_empty() && !words.eq_ignore_ascii_case("blank") {
                sentence_case(words)
            } else {
                "Complete this blank".to_string()
            };
            steps.push(ChecklistStep {
                id: format!("{}-check-{}", block.id, index + 1),
                source_id: block.id.clone(),
                title,
                detail: None,
                checked: false,
            });
        }
    }
    steps
}

/// The `[...]` blanks in a paragraph, in order: every bracketed run that is
/// neither part of an open term (`{{...}}`) nor a Markdown link.
fn blanks(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    loop {
        let open_term = rest.find("{{");
        let bracket = rest.find('[');
        match (open_term, bracket) {
            (Some(term), Some(open)) if term < open => match rest[term + 2..].find("}}") {
                Some(close) => rest = &rest[term + 2 + close + 2..],
                None => break,
            },
            (_, Some(open)) => {
                let Some(close) = rest[open + 1..].find(']').map(|i| open + 1 + i) else {
                    break;
                };
                let inner = &rest[open + 1..close];
                let after = &rest[close + 1..];
                if !after.starts_with('(') && !inner.contains('[') {
                    out.push(inner.trim());
                }
                rest = after;
            }
            _ => break,
        }
    }
    out
}

/// `LABEL — detail` split at the first spaced dash, or `None`.
fn labelled(text: &str) -> Option<(&str, &str)> {
    for (i, c) in text.char_indices() {
        if !matches!(c, '—' | '–' | '-') {
            continue;
        }
        let before = &text[..i];
        let after = &text[i + c.len_utf8()..];
        let label = before.trim_end();
        let detail = after.trim_start();
        let spaced = label.len() < before.len() && detail.len() < after.len();
        if spaced && !label.is_empty() && !detail.is_empty() {
            return Some((label, detail));
        }
    }
    None
}

/// The first sentence as a title, and what follows it as detail.
fn first_sentence(text: &str) -> (String, Option<String>) {
    let text = text.trim();
    for (i, c) in text.char_indices() {
        if i == 0 || !matches!(c, '.' | '!' | '?') {
            continue;
        }
        let end = i + c.len_utf8();
        let after = &text[end..];
        if after.is_empty() || after.starts_with(char::is_whitespace) {
            let rest = after.trim_start();
            return (
                sentence_case(&text[..end]),
                (!rest.is_empty()).then(|| rest.to_string()),
            );
        }
    }
    (sentence_case(text), None)
}

/// Capitalised, with a shouted (all-capitals) value lowered after its first
/// letter.
fn sentence_case(value: &str) -> String {
    let trimmed = value.trim();
    let mut chars = trimmed.chars();
    let Some(first) = chars.next() else {
        return "Complete this blank".to_string();
    };
    let rest = chars.as_str();
    let rest = if trimmed == trimmed.to_uppercase() {
        rest.to_lowercase()
    } else {
        rest.to_string()
    };
    first.to_uppercase().collect::<String>() + &rest
}

/// The checklist blocks of a parsed outline: every hold and paragraph after
/// the first heading, in reading order.
#[cfg(feature = "server")]
#[must_use]
pub fn blocks(doc: &views::harvard_outline::OutlineDocument) -> Vec<ChecklistBlock> {
    use views::harvard_outline::{hold_body, UnitKind};

    doc.units
        .iter()
        .filter(|unit| unit.depth > 0 && !unit.kind.is_outline())
        .map(|unit| {
            let one_line = unit
                .markdown
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let hold = unit.kind == UnitKind::Hold;
            let text = if hold {
                hold_body(&one_line).unwrap_or(&one_line).to_string()
            } else {
                one_line
            };
            ChecklistBlock {
                id: unit.fragment.clone(),
                hold,
                text,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{derive, first_sentence, labelled, sentence_case, ChecklistBlock};

    fn hold(id: &str, text: &str) -> ChecklistBlock {
        ChecklistBlock {
            id: id.into(),
            hold: true,
            text: text.into(),
        }
    }

    fn prose(id: &str, text: &str) -> ChecklistBlock {
        ChecklistBlock {
            id: id.into(),
            hold: false,
            text: text.into(),
        }
    }

    #[test]
    fn sentence_case_lowers_a_shouted_value_and_keeps_a_mixed_one() {
        assert_eq!(sentence_case("CLIENT NAME"), "Client name");
        assert_eq!(sentence_case("confirm the Venue"), "Confirm the Venue");
        assert_eq!(sentence_case("   "), "Complete this blank");
    }

    #[test]
    fn a_spaced_dash_labels_a_hold_and_a_hyphen_does_not() {
        assert_eq!(labelled("FEE — Confirm it."), Some(("FEE", "Confirm it.")));
        assert_eq!(labelled("A - B - C"), Some(("A", "B - C")));
        assert_eq!(labelled("Cross-check the date."), None);
    }

    #[test]
    fn the_first_sentence_ends_at_punctuation_before_a_space() {
        assert_eq!(
            first_sentence("Check v. Smith first. Then file."),
            ("Check v.".into(), Some("Smith first. Then file.".into()))
        );
        assert_eq!(
            first_sentence("No punctuation"),
            ("No punctuation".into(), None)
        );
        assert_eq!(first_sentence("Ends here."), ("Ends here.".into(), None));
    }

    #[test]
    fn open_terms_and_links_are_not_blanks() {
        let steps = derive(&[prose(
            "p1",
            "Pay {{custom_money__fee}} per [the schedule](https://example.com) by [DATE].",
        )]);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].title, "Pay {{custom_money__fee}} per");
    }

    #[test]
    fn a_labelled_hold_keeps_its_detail_and_a_resolved_one_is_checked() {
        let steps = derive(&[
            hold("p1", "SAMPLE FEE — Confirm the synthetic fee."),
            hold("p2", "Resolved. The synthetic venue is settled."),
            prose("p3", "No blanks in this synthetic sentence."),
        ]);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].title, "Sample fee");
        assert_eq!(
            steps[0].detail.as_deref(),
            Some("Confirm the synthetic fee.")
        );
        assert!(!steps[0].checked);
        assert_eq!(steps[1].title, "Resolved.");
        assert!(steps[1].checked);
    }

    // Parity with the retired `deriveNotationChecklist` tests, on the same
    // shapes with synthetic words. Each runs through the real parser, so the
    // source ids are this crate's path-derived fragments.

    #[cfg(feature = "server")]
    fn checklist(markdown: &str) -> Vec<super::ChecklistStep> {
        derive(&super::blocks(&views::harvard_outline::parse(markdown)))
    }

    #[cfg(feature = "server")]
    #[test]
    fn parity_labelled_holds_resolved_holds_and_blanks_become_ordered_steps() {
        let steps = checklist(
            "## I. Sample Scope\n\nThe firm reviews one synthetic agreement.\n\n\
             1. A synthetic list item.\n\n\
             [SAMPLE HOLD LABEL — Confirm the synthetic fee schedule.]\n\n\
             Sample lead: [SAMPLE BLANK]\n\n\
             ### A. Sample Nested\n\n\
             [resolved. The synthetic venue is settled.]\n",
        );
        assert_eq!(steps.len(), 3, "{steps:?}");
        assert_eq!(steps[0].source_id, "section-i-p3");
        assert_eq!(steps[0].title, "Sample hold label");
        assert_eq!(
            steps[0].detail.as_deref(),
            Some("Confirm the synthetic fee schedule.")
        );
        assert!(!steps[0].checked);
        assert_eq!(steps[1].source_id, "section-i-p4");
        assert_eq!(steps[1].title, "Sample lead");
        assert!(!steps[1].checked);
        assert_eq!(steps[2].source_id, "section-i-a-p1");
        assert!(steps[2].title.starts_with("Resolved."), "{steps:?}");
        assert!(steps[2].checked);
        assert_eq!(steps[0].id, "section-i-p3-check-1");
    }

    #[cfg(feature = "server")]
    #[test]
    fn parity_an_unlabelled_hold_is_its_sentence_and_a_lone_bracket_its_words() {
        let steps = checklist(
            "## I. Sample Scope\n\n[Confirm the synthetic hearing date.]\n\n[STANDALONE BLANK]\n",
        );
        assert_eq!(steps[0].title, "Confirm the synthetic hearing date.");
        assert_eq!(steps[1].title, "Standalone blank");
    }

    #[cfg(feature = "server")]
    #[test]
    fn parity_empty_blanks_and_unpunctuated_holds_invent_no_detail() {
        let steps = checklist("## I. Sample Scope\n\n[Synthetic Short Hold]\n\n[ blank ]\n");
        assert_eq!(steps.len(), 2, "{steps:?}");
        assert_eq!(steps[0].detail, None);
        assert_eq!(steps[1].title, "Complete this blank");
        assert!(checklist("").is_empty());
        assert!(checklist("## I. Sample Heading Only\n").is_empty());
    }

    #[cfg(feature = "server")]
    #[test]
    fn a_preamble_carries_no_steps() {
        assert!(checklist("[Synthetic preamble hold.]\n\n## I. Scope\n").is_empty());
    }
}
