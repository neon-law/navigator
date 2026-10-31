//! Typed record citations: parsing what a draft cites into the record it points at.
//!
//! The cite-check view ([`crate::harvard_outline`] narrates a draft; this module
//! supplies its other half) needs to know, for every citation in a draft, which
//! record the sentence points at: a page and line of a pleading, a paragraph of
//! a declaration, a page of an exhibit, or a shorthand (`Id.`, `Ibid.`,
//! `supra`) that leans on an earlier citation. A [`CitationParser`] walks the
//! draft once and returns the citations in reading order, each carrying:
//!
//! - its [`Span`] (byte offsets) so the view can mark the draft text;
//! - the [`RecordRef`] — the typed record the citation names;
//! - the document the citation resolved to, when a caller-supplied alias table
//!   matched the document's name;
//! - the sentence of the draft that contains the citation, for the marked list
//!   under the split;
//! - an `unresolved_shorthand` flag when an `Id.`/`Ibid.`/`supra` has no
//!   earlier citation to lean on — a manual-review mark, not an error, because
//!   the draft may legitimately continue a thread from an earlier section.
//!
//! Document names are matched by their drafted shape — capitalized words,
//! optionally joined by `of` (`Complaint`, `Amended Complaint`,
//! `Declaration of Jane Roe`) — and resolved through the caller's alias table
//! (lowercased name → document), so the same draft parses against any matter's
//! documents without hardcoding spellings.

use std::collections::HashMap;

use regex::Regex;

/// A byte-offset span in the draft text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Byte offset of the first byte of the span.
    pub start: usize,
    /// Byte offset one past the last byte of the span.
    pub end: usize,
}

/// A range of lines on one page: `12:14` or `12:14–19`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineSpan {
    /// The first line cited.
    pub first: u32,
    /// The last line cited, when the citation closes a range.
    pub last: Option<u32>,
}

/// The short forms a draft may lean on instead of repeating a citation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shorthand {
    /// `Id.` — the same document as the previous citation.
    Id,
    /// `Ibid.` — the same document *and* page as the previous citation.
    Ibid,
    /// `supra` — an earlier-cited document, page restated.
    Supra,
}

impl Shorthand {
    /// The citation's spelled form, for display.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Id => "Id.",
            Self::Ibid => "Ibid.",
            Self::Supra => "supra",
        }
    }
}

/// The record a citation names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordRef {
    /// A page (and optional line range) of a document: `Complaint at 4:12–19`.
    Page {
        /// The document's drafted name, when the citation names one.
        document: Option<String>,
        /// The page cited.
        page: u32,
        /// The line span, when the citation pins lines.
        lines: Option<LineSpan>,
    },
    /// A paragraph of a document: `Complaint ¶ 14`, `¶¶ 6–8`.
    Paragraph {
        /// The document's drafted name, when the citation names one.
        document: Option<String>,
        /// The first paragraph cited.
        paragraph: u32,
        /// The last paragraph of a range, when the citation closes one.
        through: Option<u32>,
    },
    /// A page of an exhibit: `Exhibit A at 3`.
    ExhibitPage {
        /// The exhibit's letter or number, as drafted.
        exhibit: String,
        /// The page of the exhibit, when the citation pins one.
        page: Option<u32>,
    },
    /// A short form leaning on an earlier citation: `Id. at 5`, `Ibid.`,
    /// `supra at 7`.
    Shorthand {
        /// Which short form the draft used.
        form: Shorthand,
        /// The restated page, when the short form carries one.
        page: Option<u32>,
        /// The restated line span, when the short form carries one.
        lines: Option<LineSpan>,
    },
}

/// One record citation found in a draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordCitation {
    /// Where the citation sits in the draft text (byte offsets).
    pub span: Span,
    /// The citation as drafted, trimmed of surrounding whitespace.
    pub text: String,
    /// The typed record the citation names.
    pub reference: RecordRef,
    /// The document the citation resolved to — the alias table's value — when
    /// the drafted name matched. `None` leaves the reference unlinked, which
    /// the view renders as an unresolved document, not an error.
    pub resolved: Option<String>,
    /// Whether a short form (`Id.`, `Ibid.`, `supra`) has no earlier citation
    /// in this draft to lean on. The view flags these for manual review.
    pub unresolved_shorthand: bool,
    /// The sentence of the draft that contains the citation.
    pub sentence: String,
}

/// The compiled patterns behind [`CitationParser::parse`].
#[derive(Debug, Clone)]
struct Patterns {
    exhibit: Regex,
    id_at: Regex,
    ibid: Regex,
    supra: Regex,
    paragraph: Regex,
    page: Regex,
}

/// A compiled record-citation parser. Construct once per process with
/// [`CitationParser::compile`]; the patterns are static, so compilation only
/// fails on a bug in this module, and the error is surfaced rather than
/// swallowed.
#[derive(Debug, Clone)]
pub struct CitationParser {
    patterns: Patterns,
}

impl CitationParser {
    /// Compile the citation patterns.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`regex::Error`] if any static pattern fails to
    /// compile, which would mean this module's patterns are malformed.
    pub fn compile() -> Result<Self, regex::Error> {
        // A document name as drafted: capitalized words, optionally joined by
        // `of` (`Complaint`, `Amended Complaint`, `Declaration of Jane Roe`).
        // Case-sensitive on purpose — a citation names a document, and the
        // capitalized shape keeps prose ("the court denied the motion at 3")
        // out of the match.
        let document = r"[A-Z][\w'’.-]*(?:\s+(?:of\s+)?[A-Z][\w'’.-]*)*";
        // `Exhibit A at 3` / `Ex. 2`.
        let exhibit = Regex::new(r"\bEx(hibit)?\.?\s+([A-Z0-9]{1,3})")?;
        // `Id. at 5:2` / `Id.`
        let id_at =
            Regex::new(r"(?i)\bId\.(?:\s+at\s+(\d{1,4})(?::(\d{1,4})(?:[–-](\d{1,4}))?)?)?")?;
        // `Ibid.`
        let ibid = Regex::new(r"(?i)\bIbid\.")?;
        // `supra at 7` / `supra, at 7`.
        let supra = Regex::new(r"(?i)\bsupra,?\s+at\s+(\d{1,4})(?::(\d{1,4})(?:[–-](\d{1,4}))?)?")?;
        // `Complaint ¶ 14` / `Roe Declaration ¶¶ 6–8` / `Complaint para. 6`.
        let paragraph = Regex::new(&format!(
            r"\b({document})\s+(¶¶?|paras?\.)\s*(\d{{1,4}})(?:\s*[–-]\s*(\d{{1,4}}))?"
        ))?;
        // `Complaint at 4:12–19` / `Amended Complaint at 12`.
        let page = Regex::new(&format!(
            r"\b({document})\s+at\s+(\d{{1,4}})(?::(\d{{1,4}})(?:[–-](\d{{1,4}}))?)?"
        ))?;
        Ok(Self {
            patterns: Patterns {
                exhibit,
                id_at,
                ibid,
                supra,
                paragraph,
                page,
            },
        })
    }

    /// Parse every record citation in `text`, in reading order.
    ///
    /// `aliases` maps a lowercased document name to the document record it is:
    /// a citation whose drafted name is in the table comes back `resolved`.
    #[must_use]
    pub fn parse(&self, text: &str, aliases: &HashMap<String, String>) -> Vec<RecordCitation> {
        let mut candidates: Vec<Candidate> = Vec::new();
        self.exhibit_candidates(text, &mut candidates);
        self.shorthand_candidates(text, &mut candidates);
        self.paragraph_candidates(text, &mut candidates);
        self.page_candidates(text, &mut candidates);
        deduplicate(text, candidates, aliases)
    }

    /// `Exhibit A at 3`: the exhibit itself, plus a page when one follows it.
    fn exhibit_candidates(&self, text: &str, candidates: &mut Vec<Candidate>) {
        for captures in self.patterns.exhibit.captures_iter(text) {
            let whole = captures.get(0).unwrap_or_else(|| captures.get(1).unwrap());
            let exhibit = captures
                .get(2)
                .map_or(String::new(), |m| m.as_str().to_string());
            // `Exhibit A at 3` may also carry the page: reuse the exhibit
            // match only when the page follows the exhibit itself.
            let after = &text[whole.end()..];
            let page = trailing_page(after).map(|(consumed, page)| (whole.end() + consumed, page));
            candidates.push(Candidate {
                span: Span {
                    start: whole.start(),
                    end: page.map_or(whole.end(), |(end, _)| end),
                },
                reference: RecordRef::ExhibitPage {
                    exhibit,
                    page: page.map(|(_, p)| p),
                },
            });
        }
    }

    /// `Id. at 5:2`, `Ibid.`, and `supra at 7` — the short forms that point
    /// back at an earlier citation in the same draft.
    fn shorthand_candidates(&self, text: &str, candidates: &mut Vec<Candidate>) {
        for captures in self.patterns.id_at.captures_iter(text) {
            let whole = captures.get(0).unwrap_or_else(|| captures.get(1).unwrap());
            candidates.push(Candidate {
                span: Span {
                    start: whole.start(),
                    end: whole.end(),
                },
                reference: shorthand(Shorthand::Id, &captures, 1),
            });
        }
        for captures in self.patterns.ibid.captures_iter(text) {
            let whole = captures.get(0).unwrap_or_else(|| captures.get(1).unwrap());
            candidates.push(Candidate {
                span: Span {
                    start: whole.start(),
                    end: whole.end(),
                },
                reference: RecordRef::Shorthand {
                    form: Shorthand::Ibid,
                    page: None,
                    lines: None,
                },
            });
        }
        for captures in self.patterns.supra.captures_iter(text) {
            let whole = captures.get(0).unwrap_or_else(|| captures.get(1).unwrap());
            candidates.push(Candidate {
                span: Span {
                    start: whole.start(),
                    end: whole.end(),
                },
                reference: shorthand(Shorthand::Supra, &captures, 1),
            });
        }
    }

    /// `Complaint ¶ 14` / `Roe Declaration paragraphs 6–8`.
    fn paragraph_candidates(&self, text: &str, candidates: &mut Vec<Candidate>) {
        for captures in self.patterns.paragraph.captures_iter(text) {
            let whole = captures.get(0).unwrap_or_else(|| captures.get(1).unwrap());
            let document = captures.get(1).map(|m| m.as_str());
            let paragraph = captures.get(3).and_then(|m| m.as_str().parse().ok());
            let through = captures.get(4).and_then(|m| m.as_str().parse().ok());
            let (Some(paragraph), span) = (
                paragraph,
                document.map_or(whole.start(), |name| {
                    whole.start() + (name.len() - without_article(name).len())
                }),
            ) else {
                continue;
            };
            let span = Span {
                start: span,
                end: whole.end(),
            };
            let document = document.map(|name| without_article(name).to_string());
            candidates.push(Candidate {
                span,
                reference: RecordRef::Paragraph {
                    document,
                    paragraph,
                    through,
                },
            });
        }
    }

    /// `Complaint at 4:12–19` / `Amended Complaint at 12`.
    fn page_candidates(&self, text: &str, candidates: &mut Vec<Candidate>) {
        for captures in self.patterns.page.captures_iter(text) {
            let whole = captures.get(0).unwrap_or_else(|| captures.get(1).unwrap());
            let name = captures.get(1).map(|m| m.as_str());
            let page = captures.get(2).and_then(|m| m.as_str().parse().ok());
            let (Some(page), span) = (
                page,
                name.map_or(whole.start(), |n| {
                    whole.start() + (n.len() - without_article(n).len())
                }),
            ) else {
                continue;
            };
            let span = Span {
                start: span,
                end: whole.end(),
            };
            let name = name.map(|n| without_article(n).to_string());
            let lines = match (captures.get(3), captures.get(4)) {
                (Some(first), Some(last)) => Some(LineSpan {
                    first: first.as_str().parse().unwrap_or(page),
                    last: last.as_str().parse().ok(),
                }),
                (Some(first), None) => Some(LineSpan {
                    first: first.as_str().parse().unwrap_or(page),
                    last: None,
                }),
                (None, _) => None,
            };
            // `Exhibit A at 3` is an exhibit-page citation, not a page of a
            // document named "Exhibit A"; same for a shorthand wearing a
            // trailing period (`Id. at 5` — the id pattern already matched).
            if is_exhibit_name(name.as_deref().unwrap_or_default()) {
                continue;
            }
            candidates.push(Candidate {
                span,
                reference: RecordRef::Page {
                    document: name,
                    page,
                    lines,
                },
            });
        }
    }
}

/// Reading order; on a tie the longer citation wins, so `Id. at 5`
/// (already captured as a shorthand) is not re-captured by the page
/// pattern under the same span. Overlapping candidates drop in favor of
/// the earlier accepted citation, and each keeps the sentence it sits in.
fn deduplicate(
    text: &str,
    mut candidates: Vec<Candidate>,
    aliases: &HashMap<String, String>,
) -> Vec<RecordCitation> {
    candidates.sort_by(|a, b| {
        a.span
            .start
            .cmp(&b.span.start)
            .then(b.span.end.cmp(&a.span.end))
    });

    let sentences = sentences(text);
    let mut citations: Vec<RecordCitation> = Vec::new();
    let mut cursor: Option<Span> = None;
    for candidate in candidates {
        // Drop overlaps with an already-accepted citation.
        if cursor.is_some_and(|kept| candidate.span.start < kept.end) {
            continue;
        }
        let reference = candidate.reference;
        let resolved = resolve(&reference, aliases);
        let shorthand_unresolved =
            matches!(reference, RecordRef::Shorthand { .. }) && citations.is_empty();
        let sentence = sentences
            .iter()
            .find(|sentence| sentence.contains(&candidate.span))
            .map_or_else(|| text.trim().to_string(), |sentence| sentence.slice(text));
        cursor = Some(candidate.span);
        citations.push(RecordCitation {
            span: candidate.span,
            text: text[candidate.span.start..candidate.span.end]
                .trim()
                .to_string(),
            reference,
            resolved,
            unresolved_shorthand: shorthand_unresolved,
            sentence,
        });
    }
    citations
}

struct Candidate {
    span: Span,
    reference: RecordRef,
}

/// Build a [`RecordRef::Shorthand`] from a regex whose page group is at
/// `base`, line range groups at `base + 1` and `base + 2`.
fn shorthand(form: Shorthand, captures: &regex::Captures<'_>, base: usize) -> RecordRef {
    let page = captures.get(base).and_then(|m| m.as_str().parse().ok());
    let lines = match (captures.get(base + 1), captures.get(base + 2)) {
        (Some(first), Some(last)) => {
            let first = first.as_str().parse().ok();
            let last = last.as_str().parse().ok();
            match (first, last) {
                (Some(first), last) => Some(LineSpan { first, last }),
                (None, _) => None,
            }
        }
        (Some(first), None) => {
            let first = first.as_str().parse().ok();
            first.map(|first| LineSpan { first, last: None })
        }
        (None, _) => None,
    };
    RecordRef::Shorthand { form, page, lines }
}

/// The page digits immediately after `at`, when the following text is
/// ` at <page>` — the exhibit pattern stops at the exhibit itself.
fn trailing_page(after: &str) -> Option<(usize, u32)> {
    let trimmed = after.trim_start();
    if !trimmed.starts_with("at ") {
        return None;
    }
    let digits: String = trimmed[3..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let page: u32 = digits.parse().ok()?;
    let consumed = after.len() - trimmed.len() + 3 + digits.len();
    Some((consumed, page))
}

/// Whether a document name is really an exhibit citation in disguise
/// (`Exhibit A`, `Ex. 2`).
fn is_exhibit_name(name: &str) -> bool {
    let name = name.trim();
    name.starts_with("Exhibit ") || name.starts_with("Ex. ")
}

/// Resolve a reference's document through the alias table.
fn resolve(reference: &RecordRef, aliases: &HashMap<String, String>) -> Option<String> {
    let document = match reference {
        RecordRef::Page { document, .. } | RecordRef::Paragraph { document, .. } => {
            document.as_deref()
        }
        RecordRef::ExhibitPage { .. } | RecordRef::Shorthand { .. } => None,
    }?;
    aliases.get(&document.to_lowercase()).cloned()
}

/// A drafted document name may open with an article (`The Complaint`); the
/// citation names the document itself, so the article is dropped and the
/// citation's span starts at the name. A following capital letter is
/// required — `A` standing alone is a whole name, never an article.
fn without_article(name: &str) -> &str {
    for article in ["The ", "An ", "A "] {
        if let Some(rest) = name.strip_prefix(article) {
            if rest.chars().next().is_some_and(char::is_uppercase) {
                return rest;
            }
        }
    }
    name
}

/// A sentence: a span of the draft ending at `.`, `!`, or `?` followed by a
/// capital letter or the end of the text.
struct Sentence {
    span: Span,
}

impl Sentence {
    fn slice(&self, text: &str) -> String {
        text[self.span.start..self.span.end].trim().to_string()
    }

    fn contains(&self, span: &Span) -> bool {
        span.start >= self.span.start && span.start < self.span.end
    }
}

/// Split the draft into sentences. `Id.` and other abbreviations end in a
/// period, so a boundary only counts when the next non-space character is a
/// capital letter — the drafted shape of the sentence that follows.
fn sentences(text: &str) -> Vec<Sentence> {
    let mut spans = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if !matches!(byte, b'.' | b'!' | b'?') {
            continue;
        }
        let rest = text[index + 1..].trim_start();
        let next = rest.chars().next();
        let boundary = next.is_none_or(char::is_uppercase);
        if !boundary {
            continue;
        }
        let end = text.len() - rest.len();
        spans.push(Sentence {
            span: Span { start, end },
        });
        start = end;
    }
    if start < text.len() {
        spans.push(Sentence {
            span: Span {
                start,
                end: text.len(),
            },
        });
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aliases(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(name, document)| ((*name).to_string(), (*document).to_string()))
            .collect()
    }

    fn parse(text: &str) -> Vec<RecordCitation> {
        let parser = CitationParser::compile().expect("static patterns compile");
        parser.parse(text, &HashMap::new())
    }

    fn parse_with(text: &str, aliases: &HashMap<String, String>) -> Vec<RecordCitation> {
        let parser = CitationParser::compile().expect("static patterns compile");
        parser.parse(text, aliases)
    }

    #[test]
    fn parses_pleading_page_and_line_range() {
        let citations = parse("See the Complaint at 4:12–19 for the claim.");
        assert_eq!(citations.len(), 1, "{citations:?}");
        assert_eq!(citations[0].text, "Complaint at 4:12–19");
        assert_eq!(
            citations[0].reference,
            RecordRef::Page {
                document: Some("Complaint".into()),
                page: 4,
                lines: Some(LineSpan {
                    first: 12,
                    last: Some(19)
                }),
            }
        );
    }

    #[test]
    fn parses_page_without_lines() {
        let citations = parse("The Amended Complaint at 12 says otherwise.");
        assert_eq!(citations.len(), 1, "{citations:?}");
        assert_eq!(
            citations[0].reference,
            RecordRef::Page {
                document: Some("Amended Complaint".into()),
                page: 12,
                lines: None,
            }
        );
    }

    #[test]
    fn parses_hyphen_line_ranges() {
        let citations = parse("Complaint at 4:12-19.");
        assert_eq!(
            citations[0].reference,
            RecordRef::Page {
                document: Some("Complaint".into()),
                page: 4,
                lines: Some(LineSpan {
                    first: 12,
                    last: Some(19)
                }),
            }
        );
    }

    #[test]
    fn parses_declaration_of_name() {
        let citations = parse("Per the Declaration of Jane Roe at 5:2, the rent was paid.");
        assert_eq!(
            citations[0].reference,
            RecordRef::Page {
                document: Some("Declaration of Jane Roe".into()),
                page: 5,
                lines: Some(LineSpan {
                    first: 2,
                    last: None
                }),
            }
        );
    }

    #[test]
    fn parses_paragraph_citation() {
        let citations = parse("The Complaint ¶ 14 alleges the breach.");
        assert_eq!(
            citations[0].reference,
            RecordRef::Paragraph {
                document: Some("Complaint".into()),
                paragraph: 14,
                through: None,
            }
        );
    }

    #[test]
    fn parses_paragraph_range() {
        let citations = parse("Roe Declaration ¶¶ 6–8 attaches the ledger.");
        assert_eq!(
            citations[0].reference,
            RecordRef::Paragraph {
                document: Some("Roe Declaration".into()),
                paragraph: 6,
                through: Some(8),
            }
        );
    }

    #[test]
    fn parses_spelled_out_paragraph() {
        let citations = parse("The Complaint para. 6 answers it; see also Complaint paras. 6-8.");
        assert_eq!(citations.len(), 2, "{citations:?}");
        assert_eq!(
            citations[1].reference,
            RecordRef::Paragraph {
                document: Some("Complaint".into()),
                paragraph: 6,
                through: Some(8),
            }
        );
    }

    #[test]
    fn parses_exhibit_page() {
        let citations = parse("The lease is Exhibit A at 3.");
        assert_eq!(
            citations[0].reference,
            RecordRef::ExhibitPage {
                exhibit: "A".into(),
                page: Some(3),
            }
        );
    }

    #[test]
    fn parses_exhibit_abbreviation_without_page() {
        let citations = parse("Compare Ex. 2 with the affidavit.");
        assert_eq!(
            citations[0].reference,
            RecordRef::ExhibitPage {
                exhibit: "2".into(),
                page: None,
            }
        );
    }

    #[test]
    fn resolves_document_aliases() {
        let table = aliases(&[
            ("complaint", "doc_complaint"),
            ("declaration of jane roe", "doc_roe"),
        ]);
        let citations = parse_with(
            "The Complaint at 4 and the Declaration of Jane Roe at 5:2.",
            &table,
        );
        assert_eq!(citations[0].resolved.as_deref(), Some("doc_complaint"));
        assert_eq!(citations[1].resolved.as_deref(), Some("doc_roe"));
    }

    #[test]
    fn an_unmatched_document_is_unresolved_not_flagged() {
        let citations = parse("The Motion to Dismiss at 3 is pending.");
        assert_eq!(citations[0].resolved, None);
        assert!(!citations[0].unresolved_shorthand);
    }

    #[test]
    fn alias_keys_are_case_insensitive() {
        let table = aliases(&[("amended complaint", "doc_ac")]);
        let citations = parse_with("Amended Complaint at 1.", &table);
        assert_eq!(citations[0].resolved.as_deref(), Some("doc_ac"));
    }

    #[test]
    fn id_at_leans_on_the_previous_citation() {
        let citations = parse("The Complaint at 4:12. Id. at 5:2 adds the rest.");
        assert_eq!(citations.len(), 2, "{citations:?}");
        assert_eq!(
            citations[1].reference,
            RecordRef::Shorthand {
                form: Shorthand::Id,
                page: Some(5),
                lines: Some(LineSpan {
                    first: 2,
                    last: None
                }),
            }
        );
        assert!(!citations[1].unresolved_shorthand);
    }

    #[test]
    fn a_bare_id_carries_no_page() {
        let citations = parse("The Complaint at 4. Id. repeats it.");
        assert_eq!(
            citations[1].reference,
            RecordRef::Shorthand {
                form: Shorthand::Id,
                page: None,
                lines: None,
            }
        );
    }

    #[test]
    fn ibid_repeats_the_previous_citation() {
        let citations = parse("The Complaint ¶ 4. Ibid.");
        assert_eq!(
            citations[1].reference,
            RecordRef::Shorthand {
                form: Shorthand::Ibid,
                page: None,
                lines: None,
            }
        );
    }

    #[test]
    fn supra_restates_a_page() {
        let citations = parse("The Complaint at 4. See supra at 7.");
        assert_eq!(
            citations[1].reference,
            RecordRef::Shorthand {
                form: Shorthand::Supra,
                page: Some(7),
                lines: None,
            }
        );
        assert!(!citations[1].unresolved_shorthand);
    }

    #[test]
    fn a_leading_shorthand_is_flagged_for_review() {
        let citations = parse("Id. at 5 begins the memo.");
        assert_eq!(citations.len(), 1, "{citations:?}");
        assert!(citations[0].unresolved_shorthand);
    }

    #[test]
    fn spans_slice_the_draft_exactly() {
        let text = "The Complaint at 4:12–19 controls.";
        let citations = parse(text);
        assert_eq!(
            &text[citations[0].span.start..citations[0].span.end],
            "Complaint at 4:12–19"
        );
    }

    #[test]
    fn the_sentence_contains_the_citation() {
        let citations = parse("The court agrees. The Complaint at 4:12 says otherwise. Done.");
        assert_eq!(
            citations[0].sentence,
            "The Complaint at 4:12 says otherwise."
        );
    }

    #[test]
    fn citations_come_in_reading_order() {
        let citations = parse("Complaint at 4. Exhibit A at 3. Roe Declaration ¶ 6.");
        let texts: Vec<&str> = citations.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            texts,
            ["Complaint at 4", "Exhibit A at 3", "Roe Declaration ¶ 6"]
        );
    }

    #[test]
    fn shorthand_never_captures_as_a_document_page() {
        // `Id. at 5` must not also parse as a page of a document named `Id.`.
        let citations = parse("Id. at 5 and then the Complaint at 6.");
        assert_eq!(citations.len(), 2, "{citations:?}");
        assert_eq!(citations[0].text, "Id. at 5");
    }

    #[test]
    fn lowercase_shorthands_still_parse() {
        let citations = parse("The Complaint at 4. id. at 5. ibid.");
        assert_eq!(citations.len(), 3, "{citations:?}");
        assert_eq!(
            citations[1].reference,
            RecordRef::Shorthand {
                form: Shorthand::Id,
                page: Some(5),
                lines: None,
            }
        );
    }

    #[test]
    fn prose_pages_do_not_parse() {
        let citations = parse("The court denied the motion at 3 p.m.");
        assert!(citations.is_empty(), "{citations:?}");
    }

    #[test]
    fn an_empty_draft_yields_no_citations() {
        assert!(parse("").is_empty());
    }

    #[test]
    fn exhibit_page_does_not_double_parse_as_document_page() {
        let citations = parse("Exhibit A at 3");
        assert_eq!(citations.len(), 1, "{citations:?}");
        assert!(matches!(
            citations[0].reference,
            RecordRef::ExhibitPage { .. }
        ));
    }
}
