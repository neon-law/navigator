//! Final documents only: what a document leaving Navigator may not carry.
//!
//! Every document a Project produces for a client, a court, a counterparty, or
//! a signer is final. Each fact is filled in and each choice is made before it
//! leaves Navigator, so its text carries none of these [`Marker`]s:
//!
//! - an unanswered `{{placeholder}}` — the fact belongs to a questionnaire
//!   state, which the refusal names;
//! - a bracketed drafting marker such as `[INSERT DATE]` or `[Client name]`;
//! - a fill-in blank (`____`) outside a signature block;
//! - a drafting note: `TODO`, `TK`, or `XX`;
//! - an instruction to the drafter or reviewer: a line led by a label such
//!   as `Note to drafter:` or `Reviewer note:`;
//! - an incomplete certificate or proof of service: a blank or unanswered
//!   placeholder under that heading, because a certificate states service
//!   that has happened and is rendered only once it is recorded.
//!
//! Signature, initial, date-of-signing, and notary blocks are the only spaces
//! a person completes. A line belongs to one when it sits under a signature
//! heading (`## Signatures`, `## Acceptance`, `## Notary acknowledgment`, …)
//! or after an `Agreed and accepted:` / `IN WITNESS WHEREOF` lead-in, or when
//! it carries its own signing label (`By:`, `Signature`, `Initials`) or opens
//! a jurat (`Subscribed and sworn before me:`). Signing anchors
//! (`{{client.signature}}`, `{{firm.date}}`) are the signature provider's,
//! not unanswered facts.
//!
//! A bracket that is a Markdown link, a footnote, an empty or ticked
//! checkbox, `[sic]`, or a citation carrying a number (`[2024] UKSC 1`) is
//! prose, not a drafting marker. An HTML comment never renders, so drafting
//! guidance kept in `<!-- … -->` is outside the document and not scanned.
//!
//! A Signer Box — a questionnaire state declared `completed_by: signer` —
//! is the one blank a final document keeps: the fill prints [`SIGNER_BOX`]
//! in place of its unanswered placeholder, and the renders draw an empty
//! rule there for the signer's hand. The marker is a comment, so [`scan`]
//! passes it; a template that types the marker itself is refused by
//! [`drafting_markers`], because the blank belongs in the render, never the
//! source.
//!
//! [`scan`] checks a filled document before a final render or a send;
//! [`drafting_markers`] checks a template, where placeholders stay legal.

use std::fmt;

/// The placeholder fields a signer completes at signing.
const SIGNING_FIELDS: [&str; 3] = ["signature", "initials", "date"];

/// The text the fill prints for a Signer Box with no answer — the same
/// bytes as `word::paper::SIGNER_BOX` and `forms::notation::SIGNER_BOX`,
/// repeated so this crate stays free of the render dependencies; `views`
/// pins the three together.
pub const SIGNER_BOX: &str = "<!-- signer-box -->";

/// Heading or lead-in words that open a signature block.
const SIGNATURE_SECTIONS: [&str; 7] = [
    "signature",
    "execution",
    "notar",
    "acknowledg",
    "witness",
    "issuance",
    "accept",
];

/// Labels that make a single line a signing line wherever it sits.
const SIGNING_LABELS: [&str; 5] = ["by:", "signature", "signed", "initials", "sworn"];

/// Drafting notes, matched as whole uppercase words.
const NOTES: [&str; 4] = ["TODO", "TK", "XX", "XXX"];

/// Labels that lead an instruction to the drafter or reviewer, matched at the
/// start of a line and followed by a colon.
const INSTRUCTION_LABELS: [&str; 10] = [
    "note to drafter",
    "drafting note",
    "note to file",
    "internal note",
    "reviewer note",
    "review note",
    "editor's note",
    "instructions",
    "instruction",
    "complete before filing",
];

/// Headings that open a certificate or proof of service.
const SERVICE_SECTIONS: [&str; 2] = ["certificate of service", "proof of service"];

/// One thing a final document may not carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Marker {
    /// An unanswered `{{placeholder}}`; the string is its questionnaire state.
    Placeholder(String),
    /// A bracketed drafting marker, brackets included.
    Bracket(String),
    /// A fill-in blank outside a signature block.
    Blank,
    /// A drafting note.
    Note(String),
    /// An instruction to the drafter or reviewer; the string is its label.
    Instruction(String),
    /// A certificate or proof of service whose service is not yet recorded.
    Service,
    /// The Signer Box marker typed into a template; the render writes it,
    /// the source never does.
    SignerBox,
}

/// A [`Marker`] and the 1-based line of the scanned text it sits on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub line: usize,
    pub marker: Marker,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let line = self.line;
        match &self.marker {
            Marker::Placeholder(state) => write!(
                f,
                "line {line}: `{{{{{state}}}}}` is unanswered; answer the `{state}` questionnaire state"
            ),
            Marker::Bracket(text) => write!(
                f,
                "line {line}: `{text}` is a drafting marker; replace it with the final text"
            ),
            Marker::Blank => write!(
                f,
                "line {line}: a fill-in blank outside a signature block; collect the fact through \
                 the questionnaire"
            ),
            Marker::Note(note) => {
                write!(f, "line {line}: `{note}` is a drafting note; resolve and remove it")
            }
            Marker::Instruction(label) => write!(
                f,
                "line {line}: `{label}:` is an instruction to the drafter; keep it out of the document"
            ),
            Marker::Service => write!(
                f,
                "line {line}: the certificate of service is incomplete; it states service that has \
                 happened, so render it only once the date, method, and recipients are recorded"
            ),
            Marker::SignerBox => write!(
                f,
                "line {line}: `{SIGNER_BOX}` is written by the render, not the template; declare a \
                 `completed_by: signer` questionnaire state and use its placeholder"
            ),
        }
    }
}

/// Every marker in a filled document: unanswered placeholders and drafting
/// markers alike.
#[must_use]
pub fn scan(text: &str) -> Vec<Finding> {
    collect(text, true)
}

/// The drafting markers in a template body: brackets, blanks, and notes.
/// Placeholders are a template's business and are not reported.
#[must_use]
pub fn drafting_markers(text: &str) -> Vec<Finding> {
    collect(text, false)
}

fn collect(text: &str, placeholders: bool) -> Vec<Finding> {
    let mut found = Vec::new();
    let mut in_fence = false;
    let mut in_signature_block = false;
    let mut in_service = false;
    let mut in_comment = false;
    for (index, source) in text.lines().enumerate() {
        let line = index + 1;
        if !placeholders && !in_fence && types_signer_box(source) {
            found.push(Finding {
                line,
                marker: Marker::SignerBox,
            });
        }
        let visible = strip_comments(source, &mut in_comment);
        let raw = visible.as_str();
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let lower = raw.to_ascii_lowercase();
        if trimmed.starts_with('#') {
            in_signature_block = opens_signature_block(&lower);
            in_service = SERVICE_SECTIONS
                .iter()
                .any(|heading| lower.contains(heading));
        } else if opens_signature_block(&lower) && !lower.contains("____") {
            in_signature_block = true;
        }

        let (prose, tokens) = split_placeholders(raw);
        let signing_line = in_signature_block || SIGNING_LABELS.iter().any(|l| lower.contains(l));
        let unanswered: Vec<String> = if placeholders {
            tokens
                .iter()
                .filter_map(|token| unanswered_state(token))
                .collect()
        } else {
            Vec::new()
        };
        let blank = !signing_line && has_blank(&prose);
        if in_service && (blank || !unanswered.is_empty()) {
            found.push(Finding {
                line,
                marker: Marker::Service,
            });
        } else {
            for state in unanswered {
                found.push(Finding {
                    line,
                    marker: Marker::Placeholder(state),
                });
            }
            if blank {
                found.push(Finding {
                    line,
                    marker: Marker::Blank,
                });
            }
        }
        if let Some(label) = instruction_label(&prose) {
            found.push(Finding {
                line,
                marker: Marker::Instruction(label),
            });
        }
        for bracket in drafting_brackets(&prose) {
            found.push(Finding {
                line,
                marker: Marker::Bracket(bracket),
            });
        }
        for note in notes(&prose) {
            found.push(Finding {
                line,
                marker: Marker::Note(note),
            });
        }
    }
    found
}

/// Whether `line` carries the Signer Box marker as an HTML comment, in any
/// case or inner spacing — what the renders would draw as a rule.
fn types_signer_box(line: &str) -> bool {
    let mut rest = line;
    while let Some(start) = rest.find("<!--") {
        let after = &rest[start + 4..];
        let Some(end) = after.find("-->") else {
            return false;
        };
        if after[..end].trim().eq_ignore_ascii_case("signer-box") {
            return true;
        }
        rest = &after[end + 3..];
    }
    false
}

/// `line` without its HTML comment text, carrying an open comment across
/// lines in `in_comment`.
fn strip_comments(line: &str, in_comment: &mut bool) -> String {
    let mut visible = String::with_capacity(line.len());
    let mut rest = line;
    loop {
        if *in_comment {
            let Some(end) = rest.find("-->") else {
                return visible;
            };
            rest = &rest[end + 3..];
            *in_comment = false;
        }
        let Some(start) = rest.find("<!--") else {
            visible.push_str(rest);
            return visible;
        };
        visible.push_str(&rest[..start]);
        rest = &rest[start + 4..];
        *in_comment = true;
    }
}

/// The label of an instruction line (`**Note to drafter:** …`), as written.
fn instruction_label(line: &str) -> Option<String> {
    let lead = line.trim_start_matches(|character: char| {
        character.is_whitespace() || matches!(character, '>' | '-' | '*' | '_' | '#')
    });
    let lower = lead.to_ascii_lowercase();
    INSTRUCTION_LABELS.iter().find_map(|label| {
        let rest = lower.strip_prefix(label)?;
        rest.trim_start_matches(['*', '_'])
            .starts_with(':')
            .then(|| lead[..label.len()].to_string())
    })
}

fn opens_signature_block(lower: &str) -> bool {
    SIGNATURE_SECTIONS.iter().any(|word| lower.contains(word))
}

/// The line with every `{{…}}` span blanked out, and the inner text of each.
fn split_placeholders(line: &str) -> (String, Vec<String>) {
    let mut prose = String::with_capacity(line.len());
    let mut tokens = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find("{{") {
        prose.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            prose.push_str(&rest[start..]);
            rest = "";
            break;
        };
        tokens.push(after[..end].trim().to_string());
        prose.push(' ');
        rest = &after[end + 2..];
    }
    prose.push_str(rest);
    (prose, tokens)
}

/// The questionnaire state an unanswered token names, or `None` for a
/// signing anchor or a control token.
fn unanswered_state(token: &str) -> Option<String> {
    if token.starts_with(['#', '/', '!']) || token.is_empty() {
        return None;
    }
    let (state, field) = token.split_once('.').unwrap_or((token, ""));
    if SIGNING_FIELDS.contains(&field) {
        return None;
    }
    Some(state.to_string())
}

fn drafting_brackets(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(open) = line[from..].find('[').map(|offset| from + offset) {
        let Some(close) = line[open + 1..].find(']').map(|offset| open + 1 + offset) else {
            break;
        };
        from = close + 1;
        let inner = line[open + 1..close].trim();
        let image = open > 0 && bytes[open - 1] == b'!';
        let linked = matches!(bytes.get(close + 1), Some(b'(' | b'[' | b':'));
        let prose = inner.is_empty()
            || inner.eq_ignore_ascii_case("x")
            || inner.eq_ignore_ascii_case("sic")
            || inner.starts_with('^')
            || inner.chars().any(|character| character.is_ascii_digit());
        if !(image || linked || prose) {
            // An escaped answer reads `[INSERT FEE\]`; report what prints.
            found.push(line[open..=close].replace('\\', ""));
        }
    }
    found
}

/// A run of three or more underscores on a line that is not a Markdown rule.
fn has_blank(line: &str) -> bool {
    let rule = line
        .trim()
        .chars()
        .all(|character| matches!(character, '_' | ' '));
    !rule && line.contains("___")
}

fn notes(line: &str) -> Vec<String> {
    line.split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| NOTES.contains(word))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{drafting_markers, scan, Finding, Marker, SIGNER_BOX};

    fn markers(text: &str) -> Vec<Marker> {
        scan(text)
            .into_iter()
            .map(|finding| finding.marker)
            .collect()
    }

    #[test]
    fn a_final_document_is_clean() {
        let text = "# Engagement\n\nThe Client is Example Co. See [the glossary](https://example.com).\n\n\
                    This holding was affirmed in [2024] UKSC 1 and quoted [sic].\n\n---\n\n___\n";
        assert_eq!(scan(text), Vec::new());
    }

    #[test]
    fn an_unanswered_placeholder_names_its_questionnaire_state() {
        let found = scan(
            "Dear {{person__client.name}},\n\nSigned {{client.signature}} on {{client.date}}.\n",
        );
        assert_eq!(
            found,
            vec![Finding {
                line: 1,
                marker: Marker::Placeholder("person__client".to_string()),
            }]
        );
        assert!(found[0]
            .to_string()
            .contains("answer the `person__client` questionnaire state"));
    }

    #[test]
    fn a_signature_block_blank_passes_and_a_body_blank_fails() {
        let text = "Payment is due on Date: ________.\n\n## Signatures\n\n\
                    | Party | Signature | Date |\n| --- | --- | --- |\n| Example Co. | ________ | ________ |\n";
        assert_eq!(markers(text), vec![Marker::Blank]);
        assert_eq!(scan(text)[0].line, 1);

        for block in [
            "Agreed and accepted:\n\nExample Co.\n\nBy: ____________  Date: ________\n",
            "## Acceptance\n\nEmployee: ____________  Date: ________\n",
            "Subscribed and sworn before me: ______________\n",
        ] {
            assert!(scan(block).is_empty(), "{block}");
        }
    }

    #[test]
    fn brackets_and_notes_are_drafting_markers() {
        let text =
            "The fee is [INSERT FEE\\] payable to [Client name].\n\nTODO confirm the venue. TK\n";
        assert_eq!(
            markers(text),
            vec![
                Marker::Bracket("[INSERT FEE]".to_string()),
                Marker::Bracket("[Client name]".to_string()),
                Marker::Note("TODO".to_string()),
                Marker::Note("TK".to_string()),
            ]
        );
    }

    #[test]
    fn an_instruction_to_the_drafter_is_a_marker_but_prose_about_instructions_is_not() {
        let text = "**Note to drafter:** confirm the hearing date.\n\n\
                    > Reviewer note: cite the local rule.\n\n\
                    The court's instructions to the jury were clear.\n\n\
                    Plaintiff requests verification of the responses under the governing rule.\n";
        assert_eq!(
            markers(text),
            vec![
                Marker::Instruction("Note to drafter".to_string()),
                Marker::Instruction("Reviewer note".to_string()),
            ]
        );
    }

    #[test]
    fn an_incomplete_certificate_of_service_names_the_certificate() {
        let text = "# Motion\n\nArgument.\n\n## Certificate of Service\n\n\
                    On {{custom_text__service_date}}, I served this motion by ________.\n\n\
                    By: ____________\n";
        let found = scan(text);
        assert_eq!(
            found
                .iter()
                .map(|f| (f.line, &f.marker))
                .collect::<Vec<_>>(),
            vec![(7, &Marker::Service)]
        );
        assert!(
            found[0].to_string().contains("certificate of service"),
            "{}",
            found[0]
        );

        let served = text.replace(
            "On {{custom_text__service_date}}, I served this motion by ________.",
            "On 1 October 2026, I served this motion by electronic service.",
        );
        assert!(scan(&served).is_empty(), "{:?}", scan(&served));
    }

    #[test]
    fn guidance_in_an_html_comment_is_outside_the_document() {
        let text = "Terms.\n\n<!--\nDrafting note: confirm [the parties]. TODO\n-->\n\n\
                    Body <!-- Note to drafter: x --> text.\n";
        assert!(scan(text).is_empty(), "{:?}", scan(text));
    }

    #[test]
    fn checkboxes_footnotes_and_fenced_code_are_not_markers() {
        let text = "- [ ] Not yet\n- [x] Done\n\nA claim.[^1]\n\n```\n[INSERT] TODO ____\n```\n";
        assert!(scan(text).is_empty(), "{:?}", scan(text));
    }

    /// LAW-141: the fill prints a Signer Box as a comment, so a filled
    /// document carrying one is final; a template that types the marker
    /// itself is refused, because the blank belongs in the render.
    #[test]
    fn a_signer_box_passes_a_filled_document_and_is_refused_in_a_template() {
        let filled = format!(
            "- ({SIGNER_BOX}) (L) retirement benefit transactions;\n\nShares: {SIGNER_BOX}\n"
        );
        assert!(scan(&filled).is_empty(), "{:?}", scan(&filled));

        let typed = format!("Shares: {SIGNER_BOX}\n\nUnits: <!-- Signer-Box -->\n");
        assert_eq!(
            drafting_markers(&typed),
            vec![
                Finding {
                    line: 1,
                    marker: Marker::SignerBox
                },
                Finding {
                    line: 3,
                    marker: Marker::SignerBox
                },
            ]
        );
        assert!(drafting_markers("Shares: <!-- note to self -->\n").is_empty());
    }

    #[test]
    fn a_template_keeps_its_placeholders_but_not_its_drafting_markers() {
        let text = "Dear {{person__client.name}},\n\nThe fee is [INSERT FEE].\n";
        assert_eq!(
            drafting_markers(text)
                .into_iter()
                .map(|finding| finding.marker)
                .collect::<Vec<_>>(),
            vec![Marker::Bracket("[INSERT FEE]".to_string())]
        );
    }
}
