//! The court-paper parts of a pleading body, read once for both renders.
//!
//! A pleading's Markdown carries three things plain Markdown cannot say:
//! who the parties are (the caption), what the paper is called (its
//! footer title), and which blocks must stay on one page (a request with
//! its response, a verification with its signature). The PDF and the Word
//! output both need the same answers, so this module is the one place
//! that reads them; `pdf` depends on `word`, so the model lives here.
//!
//! # Authoring
//!
//! The caption is one fenced block whose info string is `caption`. Each
//! line is `field: value`, and a field repeats once per printed line:
//!
//! ~~~text
//! ```caption
//! counsel: Pat Counsel (SBN 000000)
//! court: SUPERIOR COURT OF CALIFORNIA
//! parties: SAMPLE PLAINTIFF, Plaintiff,
//! case: Case No. 00CV000000
//! ```
//! ~~~
//!
//! - `counsel` — the attorney block, top left, from line 1.
//! - `court` — the court's title, centred from line 8.
//! - `parties` — the left cell of the two-column caption.
//! - `case` — the right cell: case number, department, judge.
//! - `footer` — the short title the footer prints.
//!
//! A filing's caption is not authored: [`crate::matter::compose`] builds
//! it from the Project's matter and the notation's frontmatter, and
//! [`Caption::to_fence`] hands it to this reader.
//!
//! The paper's title is the document's one `#` heading; the footer carries
//! it, abbreviated by [`footer_title`] when it would not fit one line.
//!
//! A block that must stay whole is wrapped in `<!-- keep -->` and
//! `<!-- /keep -->` comments, recognised by [`keep_marker`].
//!
//! # Answers
//!
//! The CLI wraps each questionnaire answer in `**…**` (with Markdown
//! escapes) before substituting it, so a reader can find every fact
//! particular to a matter. Inside a caption line that markup would print
//! literally, so [`plain_answer`] strips exactly the markup
//! `pdf::markdown::bold_answer` adds and nothing else.

use std::fmt::Write as _;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// The longest title, in characters, the footer carries on its one line:
/// at 10pt, about what fits the 6.5in text column.
pub const FOOTER_TITLE_MAX_CHARS: usize = 90;

/// The info string that marks the caption block.
pub const CAPTION_FENCE: &str = "caption";

/// Why a pleading body's court-paper parts could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PaperError {
    /// A caption line is not `field: value`.
    #[error("caption line {line} is not `field: value`")]
    MalformedLine { line: usize },
    /// A caption line names a field the caption does not have.
    #[error("caption line {line} names no caption field (counsel, court, parties, case, footer)")]
    UnknownField { line: usize },
    /// A body has one caption; a second block is ambiguous.
    #[error("a pleading carries one caption block")]
    SecondCaption,
}

/// The first-page caption, one entry per printed line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Caption {
    pub counsel: Vec<String>,
    pub court: Vec<String>,
    pub parties: Vec<String>,
    pub case: Vec<String>,
    /// The short title the footer prints (CRC 2.110); the paper's `#`
    /// title stands in when it is absent.
    pub footer: Option<String>,
}

impl Caption {
    /// This caption as the fenced block [`parse`] reads, so a caption
    /// composed from a matter reaches both renders through the one reader.
    #[must_use]
    pub fn to_fence(&self) -> String {
        let mut out = format!("```{CAPTION_FENCE}\n");
        let fields = [
            ("counsel", &self.counsel),
            ("court", &self.court),
            ("parties", &self.parties),
            ("case", &self.case),
        ];
        for (field, lines) in fields {
            for line in lines {
                let _ = writeln!(out, "{field}: {line}");
            }
        }
        if let Some(footer) = &self.footer {
            let _ = writeln!(out, "footer: {footer}");
        }
        out.push_str("```\n\n");
        out
    }
}

impl Paper {
    /// The title the footer prints: the caption's `footer`, else the
    /// paper's `#` title.
    #[must_use]
    pub fn footer(&self) -> Option<&str> {
        self.caption
            .as_ref()
            .and_then(|caption| caption.footer.as_deref())
            .or(self.title.as_deref())
    }
}

/// A pleading body split into the parts both renders treat specially.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paper {
    pub caption: Option<Caption>,
    /// The first `#` heading's text, before any footer abbreviation.
    pub title: Option<String>,
    /// The Markdown with the caption block removed.
    pub body: String,
}

/// An open or close marker for a block that must stay on one page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    Open,
    Close,
}

/// Read a raw HTML token as a [`Keep`] marker: `<!-- keep -->` opens a
/// group and `<!-- /keep -->` closes it. Case and inner spacing are free.
#[must_use]
pub fn keep_marker(html: &str) -> Option<Keep> {
    let inner = html
        .trim()
        .strip_prefix("<!--")?
        .strip_suffix("-->")?
        .trim()
        .to_ascii_lowercase();
    match inner.as_str() {
        "keep" => Some(Keep::Open),
        "/keep" => Some(Keep::Close),
        _ => None,
    }
}

/// The text a declared Signer Box prints when no answer fills it: an HTML
/// comment the fill writes in place of the placeholder, which each render
/// draws as an empty rule for the signer to complete by hand. A comment, so
/// a final-document scan — which never reads a comment — passes it, while
/// the renders that know it draw the rule. `forms::notation::SIGNER_BOX` and
/// `rules::final_document::SIGNER_BOX` carry the same bytes; `views` pins
/// the three together.
pub const SIGNER_BOX: &str = "<!-- signer-box -->";

/// Whether `html` is the Signer Box marker, in any case or inner spacing.
#[must_use]
pub fn is_signer_box(html: &str) -> bool {
    html.trim()
        .strip_prefix("<!--")
        .and_then(|value| value.strip_suffix("-->"))
        .is_some_and(|inner| inner.trim().eq_ignore_ascii_case("signer-box"))
}

/// Where counsel signs a court filing: the author writes this marker in its
/// own paragraph. Both renders draw it as italic `/s/ DRAFT` on a signature
/// rule (the paragraph's bottom border), with `DRAFT` highlighted, until a
/// final render names counsel through [`signed_counsel_signature`].
pub const COUNSEL_SIGNATURE: &str = "<!-- counsel-signature -->";

/// What a counsel signature line prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CounselSignature {
    /// A draft: `/s/ DRAFT`, with `DRAFT` highlighted.
    Draft,
    /// A final render: `/s/` and counsel's name.
    Signed(String),
}

impl CounselSignature {
    /// The text the line prints after `/s/ `.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Draft => "DRAFT",
            Self::Signed(name) => name,
        }
    }
}

/// The marker a final render writes in place of [`COUNSEL_SIGNATURE`]: the
/// same line, signed `/s/ <name>`. A `--` cannot sit inside an HTML comment,
/// so the name is written with single hyphens.
#[must_use]
pub fn signed_counsel_signature(name: &str) -> String {
    let mut name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    while name.contains("--") {
        name = name.replace("--", "-");
    }
    format!("<!-- counsel-signature: {name} -->")
}

/// `body` with every draft counsel signature signed `/s/ <name>`.
#[must_use]
pub fn sign_counsel_signatures(body: &str, name: &str) -> String {
    let signed = signed_counsel_signature(name);
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find("<!--") {
        let Some(length) = rest[start..].find("-->").map(|end| end + "-->".len()) else {
            break;
        };
        let comment = &rest[start..start + length];
        out.push_str(&rest[..start]);
        if counsel_signature(comment) == Some(CounselSignature::Draft) {
            out.push_str(&signed);
        } else {
            out.push_str(comment);
        }
        rest = &rest[start + length..];
    }
    out.push_str(rest);
    out
}

/// Whether `body` still carries a draft counsel signature.
#[must_use]
pub fn has_draft_counsel_signature(body: &str) -> bool {
    body.split("<!--").skip(1).any(|after| {
        after.find("-->").is_some_and(|end| {
            counsel_signature(&format!("<!--{}-->", &after[..end])) == Some(CounselSignature::Draft)
        })
    })
}

/// Read a raw HTML token as a counsel signature marker, in any case or inner
/// spacing: bare is a draft, `counsel-signature: <name>` is signed.
#[must_use]
pub fn counsel_signature(html: &str) -> Option<CounselSignature> {
    let inner = html
        .trim()
        .strip_prefix("<!--")?
        .strip_suffix("-->")?
        .trim();
    let lower = inner.to_ascii_lowercase();
    let rest = lower.strip_prefix("counsel-signature")?;
    if rest.trim().is_empty() {
        return Some(CounselSignature::Draft);
    }
    let name = inner["counsel-signature".len()..]
        .trim_start()
        .strip_prefix(':')?
        .trim();
    if name.is_empty() {
        Some(CounselSignature::Draft)
    } else {
        Some(CounselSignature::Signed(name.to_string()))
    }
}

/// Whether `html` is a `<br>` tag. Spacing and a trailing slash are free
/// (`<br>`, `<br/>`, `<br />`), and the name is case-insensitive. A cell
/// uses this where a Markdown hard break cannot sit inside a table.
#[must_use]
pub fn is_html_line_break(html: &str) -> bool {
    let inner = html
        .trim()
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
        .map(str::trim);
    let Some(inner) = inner else {
        return false;
    };
    inner
        .split_whitespace()
        .next()
        .unwrap_or(inner)
        .trim_end_matches('/')
        .eq_ignore_ascii_case("br")
}

/// Split a pleading body into its caption, title, and remaining Markdown.
///
/// # Errors
///
/// Returns [`PaperError`] when the caption block is malformed or repeated.
pub fn parse(markdown: &str) -> Result<Paper, PaperError> {
    let mut caption: Option<Caption> = None;
    let mut removed: Option<std::ops::Range<usize>> = None;
    let mut title: Option<String> = None;
    let mut in_title = false;
    let mut title_done = false;
    let mut fence: Option<String> = None;
    let mut fence_start = 0;

    for (event, range) in Parser::new_ext(markdown, Options::empty()).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
                if info.split_whitespace().next() == Some(CAPTION_FENCE) =>
            {
                if caption.is_some() {
                    return Err(PaperError::SecondCaption);
                }
                fence = Some(String::new());
                fence_start = range.start;
            }
            Event::Text(text) if fence.is_some() => {
                if let Some(buffer) = fence.as_mut() {
                    buffer.push_str(&text);
                }
            }
            Event::End(TagEnd::CodeBlock) if fence.is_some() => {
                let text = fence.take().unwrap_or_default();
                caption = Some(parse_caption(&text)?);
                removed = Some(fence_start..range.end);
            }
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            }) if !title_done => {
                in_title = true;
                title = Some(String::new());
            }
            Event::Text(text) | Event::Code(text) if in_title => {
                if let Some(buffer) = title.as_mut() {
                    buffer.push_str(&text);
                }
            }
            Event::End(TagEnd::Heading(HeadingLevel::H1)) if in_title => {
                in_title = false;
                title_done = true;
            }
            _ => {}
        }
    }

    let body = removed.map_or_else(
        || markdown.to_string(),
        |range| format!("{}{}", &markdown[..range.start], &markdown[range.end..]),
    );
    Ok(Paper {
        caption,
        title: title
            .map(|t| collapse_whitespace(&t))
            .filter(|t| !t.is_empty()),
        body,
    })
}

fn parse_caption(block: &str) -> Result<Caption, PaperError> {
    let mut caption = Caption::default();
    for (index, line) in block.lines().enumerate() {
        let line_number = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        let (field, value) = line
            .split_once(':')
            .ok_or(PaperError::MalformedLine { line: line_number })?;
        let target = match field.trim() {
            "counsel" => &mut caption.counsel,
            "court" => &mut caption.court,
            "parties" => &mut caption.parties,
            "case" => &mut caption.case,
            "footer" => {
                caption.footer = Some(plain_answer(value.trim()));
                continue;
            }
            _ => return Err(PaperError::UnknownField { line: line_number }),
        };
        target.push(plain_answer(value.trim()));
    }
    Ok(caption)
}

/// Undo the emphasis markup `pdf::markdown::bold_answer` wraps around a
/// substituted answer: unescaped `**` pairs vanish and a backslash before
/// one of the characters it escapes (`* _ [ ] \` and the backtick) yields
/// that character. Every other character, including a lone backslash and
/// a single `*`, is preserved.
#[must_use]
pub fn plain_answer(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek().is_some_and(|n| "*_[]`\\".contains(*n)) => {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
            }
            other => out.push(other),
        }
    }
    out
}

/// The title as the footer prints it: whitespace collapsed, and cut at a
/// word boundary with an ellipsis when longer than
/// [`FOOTER_TITLE_MAX_CHARS`], so the one-line footer never wraps into the
/// page's text column. CRC 2.110 allows "some clear and concise
/// abbreviation".
#[must_use]
pub fn footer_title(title: &str) -> String {
    let title = collapse_whitespace(title);
    if title.chars().count() <= FOOTER_TITLE_MAX_CHARS {
        return title;
    }
    let budget: String = title.chars().take(FOOTER_TITLE_MAX_CHARS - 1).collect();
    let cut = budget.rfind(' ').unwrap_or(budget.len());
    format!("{}…", budget[..cut].trim_end())
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = "```caption\ncounsel: Pat Counsel\ncounsel: Sample Firm\ncourt: SUPERIOR COURT\nparties: A, Plaintiff,\nparties: v.\ncase: Case No. 1\n```\n\n# DEMAND, SET ONE\n\n## 1. Body\n";

    #[test]
    fn the_caption_title_and_remaining_body_are_split_out() {
        let paper = parse(BODY).unwrap();
        let caption = paper.caption.unwrap();
        assert_eq!(caption.counsel, ["Pat Counsel", "Sample Firm"]);
        assert_eq!(caption.court, ["SUPERIOR COURT"]);
        assert_eq!(caption.parties, ["A, Plaintiff,", "v."]);
        assert_eq!(caption.case, ["Case No. 1"]);
        assert_eq!(paper.title.as_deref(), Some("DEMAND, SET ONE"));
        assert!(!paper.body.contains("caption"));
        assert!(paper.body.contains("# DEMAND, SET ONE"));
    }

    #[test]
    fn a_body_without_a_caption_is_returned_untouched() {
        let paper = parse("# Title\n\ntext\n").unwrap();
        assert_eq!(paper.caption, None);
        assert_eq!(paper.body, "# Title\n\ntext\n");
    }

    #[test]
    fn only_the_first_h1_is_the_title() {
        let paper = parse("# First\n\n# Second\n").unwrap();
        assert_eq!(paper.title.as_deref(), Some("First"));
    }

    #[test]
    fn malformed_unknown_and_repeated_captions_are_refused() {
        assert_eq!(
            parse("```caption\nno colon here\n```\n"),
            Err(PaperError::MalformedLine { line: 1 })
        );
        assert_eq!(
            parse("```caption\ncounsel: A\njudge: B\n```\n"),
            Err(PaperError::UnknownField { line: 2 })
        );
        assert_eq!(
            parse("```caption\ncounsel: A\n```\n\n```caption\ncounsel: B\n```\n"),
            Err(PaperError::SecondCaption)
        );
    }

    /// The CLI wraps every answer in `**…**` with Markdown escapes; a
    /// caption line must print the answer, not the wrapper.
    #[test]
    fn plain_answer_strips_exactly_the_markup_an_answer_is_given() {
        assert_eq!(plain_answer("**Pat Counsel**"), "Pat Counsel");
        assert_eq!(plain_answer("**A \\* B \\_ C \\[x\\]**"), "A * B _ C [x]");
        assert_eq!(plain_answer("Case **No.** 7, a * b"), "Case No. 7, a * b");
        assert_eq!(plain_answer("back\\\\slash and \\q"), "back\\slash and \\q");
    }

    #[test]
    fn a_substituted_answer_prints_plain_in_the_caption() {
        let paper = parse("```caption\nparties: **A \\* B**, Plaintiff,\n```\n").unwrap();
        assert_eq!(paper.caption.unwrap().parties, ["A * B, Plaintiff,"]);
    }

    #[test]
    fn a_long_title_is_cut_at_a_word_with_an_ellipsis() {
        let long = "PLAINTIFF'S RESPONSES TO DEFENDANT'S REQUESTS FOR ADMISSION AND SPECIAL INTERROGATORIES, SET ONE, WITH VERIFICATION AND EXHIBITS";
        let footer = footer_title(long);
        assert!(footer.chars().count() <= FOOTER_TITLE_MAX_CHARS);
        assert!(footer.ends_with('…'));
        assert!(long.starts_with(footer.trim_end_matches('…')));
        assert_eq!(footer_title("  Short   title "), "Short title");
    }

    #[test]
    fn keep_markers_open_and_close_in_any_case() {
        assert_eq!(keep_marker("<!-- keep -->\n"), Some(Keep::Open));
        assert_eq!(keep_marker("<!--/KEEP-->"), Some(Keep::Close));
        assert_eq!(keep_marker("<!-- pagebreak -->"), None);
        assert_eq!(keep_marker("<b>keep</b>"), None);
    }

    #[test]
    fn the_signer_box_marker_is_read_in_any_case_and_nothing_else_is() {
        assert!(is_signer_box(SIGNER_BOX));
        assert!(is_signer_box("<!--SIGNER-BOX-->\n"));
        assert!(!is_signer_box("<!-- keep -->"));
        assert!(!is_signer_box("<!-- signer box -->"));
        assert!(!is_signer_box("signer-box"));
    }

    #[test]
    fn a_counsel_signature_is_a_draft_until_a_name_signs_it() {
        assert_eq!(
            counsel_signature(COUNSEL_SIGNATURE),
            Some(CounselSignature::Draft)
        );
        assert_eq!(
            counsel_signature("<!--COUNSEL-SIGNATURE-->\n"),
            Some(CounselSignature::Draft)
        );
        let signed = signed_counsel_signature(" Pat  Sample--Jones "); // pii-scan: allow — synthetic signer name
        assert_eq!(
            counsel_signature(&signed),
            Some(CounselSignature::Signed("Pat Sample-Jones".into())) // pii-scan: allow — synthetic signer name
        );
        assert_eq!(counsel_signature("<!-- counsel-signatures -->"), None);
        assert_eq!(counsel_signature(SIGNER_BOX), None);

        let body = format!("<!-- keep -->\nSubmitted,\n\n{COUNSEL_SIGNATURE}\n\n{SIGNER_BOX}\n");
        assert!(has_draft_counsel_signature(&body));
        let final_body = sign_counsel_signatures(&body, "Pat Sample");
        assert!(!has_draft_counsel_signature(&final_body));
        assert_eq!(
            final_body,
            format!(
                "<!-- keep -->\nSubmitted,\n\n{}\n\n{SIGNER_BOX}\n",
                signed_counsel_signature("Pat Sample")
            )
        );
    }

    #[test]
    fn a_break_tag_is_a_line_break_in_any_spacing() {
        assert!(is_html_line_break("<br>"));
        assert!(is_html_line_break("<br/>"));
        assert!(is_html_line_break("<br />"));
        assert!(is_html_line_break("<BR>"));
        assert!(!is_html_line_break("<b>"));
        assert!(!is_html_line_break("<!-- pagebreak -->"));
    }
}
