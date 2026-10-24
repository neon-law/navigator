//! Convert a notation template's Markdown body into Typst markup.
//!
//! Notation templates are authored in **Markdown** (`##` headings,
//! `**bold**`, `-` lists) — that is what the `rules` validator checks.
//! The [`crate::render`] pipeline, however, compiles **Typst**, whose
//! markup is close but not identical: emphasis is `*x*` not `**x**`,
//! headings are `=` not `#`, and a stray `#` or `$` in prose is a
//! function call or math delimiter. Feeding raw Markdown to Typst
//! therefore renders wrong or fails to compile.
//!
//! [`to_typst`] walks the [`pulldown_cmark`] event stream and emits the
//! equivalent Typst markup, escaping every character Typst would
//! otherwise treat as syntax. It covers the constructs that appear in
//! notation bodies (headings, paragraphs, strong/emphasis, ordered and
//! unordered lists, block quotes, inline code, links, tables, horizontal
//! rules); raw HTML is dropped rather than leaked as literal tags, with a
//! single exception — an HTML comment `<!-- pagebreak -->` maps to a Typst
//! `#pagebreak()`, so a body can force the next content (e.g. a signature
//! block) onto a fresh page.
//!
//! ## Answered and unanswered
//!
//! The caller substitutes `{{placeholder}}` tokens before conversion, and
//! this module marks both outcomes so a reader can tell them apart at a
//! glance. An **answered** value the caller wrapped with [`bold_answer`]
//! renders bold, so every fact particular to a matter stands out from the
//! boilerplate. An **unanswered** token — one still present at render time
//! — survives verbatim, wrapped in a yellow highlight, so an unfinished
//! document is unmistakably unfinished rather than looking like prose.

use pulldown_cmark::{Alignment, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// Convert a Markdown `body` to Typst markup suitable for
/// [`crate::render`].
///
/// The output is fragment markup (no page setup or font rule) — the
/// caller wraps it in an [`crate::OutputFormat`] chrome preamble.
#[must_use]
pub fn to_typst(body: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);
    let parser = Parser::new_ext(body, opts);

    let mut out = String::with_capacity(body.len() + body.len() / 8);
    // Ordered-list counters, one per nesting level. `None` marks an
    // unordered list at that depth.
    let mut list_stack: Vec<Option<u64>> = Vec::new();
    // How many bracketed Typst content containers (block quote, table
    // cell, link, strikethrough) are currently open. A `#pagebreak()` is
    // illegal inside one, so the sentinel is honored only at depth 0.
    let mut container_depth: usize = 0;
    let mut quote_depth: usize = 0;
    let mut hang = Hang::default();
    // A `<!-- keep -->` group is open, and one has been used at all (which
    // is what makes the output carry the helper that lays it out).
    let mut keep_open = false;
    let mut keep_used = false;

    for event in parser {
        match event {
            Event::Start(tag) => {
                if opens_container(&tag) {
                    container_depth += 1;
                }
                match tag {
                    Tag::BlockQuote(_) => quote_depth += 1,
                    Tag::Paragraph if quote_depth > 0 => hang.begin(out.len()),
                    _ => {}
                }
                start_tag(&mut out, &tag, &mut list_stack);
            }
            Event::End(tag) => {
                if closes_container(tag) {
                    container_depth = container_depth.saturating_sub(1);
                }
                match tag {
                    TagEnd::BlockQuote(_) => quote_depth = quote_depth.saturating_sub(1),
                    TagEnd::Paragraph => hang.end(&mut out),
                    _ => {}
                }
                end_tag(&mut out, tag, &mut list_stack);
            }
            Event::Text(text) => {
                let rest = hang.text(&mut out, &text);
                out.push_str(&escape_text_marking_placeholders(rest));
            }
            Event::Code(code) => {
                out.push_str("#raw(");
                out.push_str(&typst_string(&code));
                out.push(')');
            }
            Event::SoftBreak => out.push(' '),
            Event::HardBreak => out.push_str(" \\\n"),
            Event::Rule => out.push_str("\n#line(length: 100%)\n\n"),
            Event::Html(html) | Event::InlineHtml(html)
                if word::paper::is_html_line_break(&html) =>
            {
                out.push_str(" \\\n");
            }
            // A Signer Box the fill wrote in place of an unanswered
            // placeholder: an empty rule the signer completes by hand,
            // drawn wherever it sits — in a paragraph, a list item, or a
            // table cell — and never highlighted, because it is not a
            // blank the questionnaire forgot.
            Event::Html(html) | Event::InlineHtml(html) if word::paper::is_signer_box(&html) => {
                out.push_str(SIGNER_BOX_TYPST);
            }
            // The one HTML token honored: a top-level `<!-- pagebreak -->`
            // comment forces a Typst page break (used to start a signature
            // block on its own page). Inside a container (table cell, block
            // quote, …) a `#pagebreak()` is illegal and would fail the whole
            // render, so there the sentinel is dropped like any other HTML.
            // Every other raw/inline HTML token — plus footnotes, math, and
            // task markers — is dropped rather than leaked as literal tags.
            Event::Html(html) | Event::InlineHtml(html)
                if container_depth == 0 && is_pagebreak_comment(&html) =>
            {
                out.push_str("\n#pagebreak()\n\n");
            }
            // A `<!-- keep -->` … `<!-- /keep -->` pair keeps the blocks
            // between them on one page (a request with its response, a
            // signature block with its date). Like the pagebreak, it is
            // honored only at the top level; a group is a container, so a
            // pagebreak inside one is dropped rather than fatal.
            Event::Html(html) | Event::InlineHtml(html) => match word::paper::keep_marker(&html) {
                Some(word::paper::Keep::Open) if container_depth == 0 => {
                    keep_open = true;
                    keep_used = true;
                    container_depth += 1;
                    out.push_str("\n#keep[\n");
                }
                Some(word::paper::Keep::Close) if keep_open => {
                    keep_open = false;
                    container_depth = container_depth.saturating_sub(1);
                    out.push_str("\n]\n\n");
                }
                _ => {}
            },
            _ => {}
        }
    }
    if keep_open {
        out.push_str("\n]\n");
    }
    if keep_used {
        out.insert_str(0, KEEP_FN);
    }
    // Collapse any run of 3+ newlines the structure handlers produced
    // into the canonical paragraph break.
    normalize_blank_lines(&out)
}

/// Typst for a `<!-- keep -->` group: the blocks inside stay on one page
/// when they fit on one, and flow across pages, whole and unclipped, when
/// they are taller than a page's text column. `layout` reports that
/// column's full height — not the room left on the current page — which is
/// the comparison that decides it.
const KEEP_FN: &str = "\
#let keep(body) = layout(size => {\n\
\x20 let h = measure(block(width: size.width, body)).height\n\
\x20 if h <= size.height { block(breakable: false, width: 100%, body) } else { body }\n\
})\n";

/// The width a hanging Harvard marker (`A.`) occupies, and so the column the
/// label and every wrapped line of the paragraph start at.
const MARKER_COLUMN: &str = "2em";

/// Hanging-marker state for the paragraphs of a block quote — a lettered
/// subsection such as `> **A. Payment.** …`.
///
/// The marker is set in a fixed-width box and the paragraph hangs by the
/// same width, so the label and each wrapped line start under the text
/// after the marker rather than under the marker itself. A quoted
/// paragraph that does not open with a marker is left as written.
#[derive(Default)]
struct Hang {
    /// Byte offset in the output where the open quoted paragraph began,
    /// until its first text has been seen.
    start: Option<usize>,
    /// The paragraph was wrapped in `#par(hanging-indent: …)[` and needs
    /// its closing bracket.
    open: bool,
    /// The space after a marker that ended its own text run is covered by
    /// the marker column, so the next run's leading space is dropped.
    trim_next: bool,
}

impl Hang {
    fn begin(&mut self, at: usize) {
        self.start = Some(at);
    }

    /// Handle a text run; returns the text left to emit after any marker.
    fn text<'a>(&mut self, out: &mut String, text: &'a str) -> &'a str {
        if let Some(start) = self.start.take() {
            if let Some((marker, rest)) = split_marker(text) {
                out.insert_str(start, &format!("#par(hanging-indent: {MARKER_COLUMN})["));
                self.open = true;
                out.push_str("#box(width: ");
                out.push_str(MARKER_COLUMN);
                out.push_str(")[");
                out.push_str(marker);
                out.push(']');
                self.trim_next = rest.is_empty();
                return rest;
            }
        }
        if std::mem::take(&mut self.trim_next) {
            return text.trim_start();
        }
        text
    }

    fn end(&mut self, out: &mut String) {
        self.start = None;
        self.trim_next = false;
        if std::mem::take(&mut self.open) {
            out.push(']');
        }
    }
}

/// Split a leading Harvard marker — one letter or up to three digits and a
/// period, then a space or the end of the run — from `text`.
fn split_marker(text: &str) -> Option<(&str, &str)> {
    let dot = text.find('.')?;
    let head = &text[..dot];
    let is_marker = match head.len() {
        1 => head.chars().all(|c| c.is_ascii_alphanumeric()),
        2..=3 => head.chars().all(|c| c.is_ascii_digit()),
        _ => false,
    };
    let rest = &text[dot + 1..];
    if !is_marker || !(rest.is_empty() || rest.starts_with(' ')) {
        return None;
    }
    Some((&text[..=dot], rest.trim_start()))
}

fn start_tag(out: &mut String, tag: &Tag, list_stack: &mut Vec<Option<u64>>) {
    match tag {
        Tag::Heading { level, .. } => {
            out.push('\n');
            for _ in 0..heading_depth(*level) {
                out.push('=');
            }
            out.push(' ');
        }
        Tag::Strong => out.push('*'),
        Tag::Emphasis => out.push('_'),
        Tag::Strikethrough => out.push_str("#strike["),
        Tag::List(first) => {
            // A tight item carries no paragraph, so its text is still on the
            // line when a nested list opens; without a break the child's
            // `1.` is read as more text of the parent.
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            list_stack.push(*first);
        }
        Tag::Item => {
            // Indent nested items two spaces per level below the top.
            let depth = list_stack.len().saturating_sub(1);
            for _ in 0..depth {
                out.push_str("  ");
            }
            match list_stack.last_mut() {
                Some(Some(n)) => {
                    out.push_str(&n.to_string());
                    out.push_str(". ");
                    *n += 1;
                }
                _ => out.push_str("- "),
            }
        }
        Tag::BlockQuote(_) => out.push_str("#quote(block: true)[\n"),
        Tag::Link { dest_url, .. } => {
            out.push_str("#link(");
            out.push_str(&typst_string(dest_url));
            out.push_str(")[");
        }
        // A Markdown table becomes a centered Typst `#table(..)`: one
        // column per alignment slot, centered by default unless the
        // author set per-column alignment, then a flat cell stream Typst
        // lays out row by row. Header cells are wrapped in
        // `table.header(..)` so Typst treats them as the header row
        // (repeated when the table breaks across pages); each cell's
        // inline markup (strong, links, escaped currency) is emitted by
        // the same handlers as prose.
        Tag::Table(alignments) => {
            out.push_str("\n#align(center)[#table(\n  columns: ");
            out.push_str(&alignments.len().to_string());
            out.push_str(",\n");
            if alignments.iter().all(|a| *a == Alignment::None) {
                out.push_str("  align: center,\n");
            } else {
                out.push_str("  align: (");
                for (i, alignment) in alignments.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(typst_align(*alignment));
                }
                out.push_str("),\n");
            }
        }
        Tag::TableHead => out.push_str("  table.header("),
        Tag::TableRow => out.push_str("  "),
        Tag::TableCell => out.push('['),
        // Headings/paragraphs inside other blocks and unhandled tags
        // contribute their text via Text events; no wrapper needed.
        _ => {}
    }
}

/// Map a Markdown column alignment to the Typst `align` keyword. A
/// column with no explicit alignment (`None`) uses Navigator's default
/// centered table-cell alignment.
fn typst_align(alignment: Alignment) -> &'static str {
    match alignment {
        Alignment::None | Alignment::Center => "center",
        Alignment::Left => "left",
        Alignment::Right => "right",
    }
}

fn end_tag(out: &mut String, tag: TagEnd, list_stack: &mut Vec<Option<u64>>) {
    match tag {
        TagEnd::Heading(_) | TagEnd::Paragraph => out.push_str("\n\n"),
        TagEnd::Strong => out.push('*'),
        TagEnd::Emphasis => out.push('_'),
        TagEnd::Strikethrough | TagEnd::Link => out.push(']'),
        TagEnd::List(_) => {
            list_stack.pop();
            if list_stack.is_empty() {
                out.push('\n');
            }
        }
        TagEnd::Item | TagEnd::TableRow => out.push('\n'),
        TagEnd::BlockQuote(_) => out.push_str("]\n\n"),
        TagEnd::Table => out.push_str(")]\n\n"),
        TagEnd::TableHead => out.push_str("),\n"),
        TagEnd::TableCell => out.push_str("], "),
        _ => {}
    }
}

/// Typst supports six heading levels; clamp deeper Markdown headings to
/// the deepest Typst level rather than emitting an over-long `=` run.
fn heading_depth(level: HeadingLevel) -> usize {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Escape the characters Typst treats as markup syntax so prose text
/// renders verbatim. The set is the markup sigils that can fire
/// mid-line: a function/label call (`#`, `<`), math (`$`), emphasis
/// (`*`, `_`), raw (`` ` ``), reference (`@`), content brackets
/// (`[`, `]`), and the escape char itself (`\`).
/// Wrap a questionnaire answer so it renders **bold** where it is
/// substituted into a template body.
///
/// The two halves of the same idea: an *unanswered* `{{placeholder}}` gets
/// the yellow wash from [`escape_text_marking_placeholders`], and an
/// *answered* one gets bold. A reader skimming a retainer can then find
/// every fact particular to their matter — names, dates, the fee, the
/// scope — without reading the boilerplate around them.
///
/// The caller substitutes this into the Markdown body, so the value is
/// Markdown-escaped first: a name or a scope sentence carrying `*` or `_`
/// would otherwise unbalance the strong run and swallow the rest of the
/// paragraph. A blank value is returned untouched, because `****` is not
/// emphasis in Markdown — it is four asterisks on the page.
#[must_use]
pub fn bold_answer(value: &str) -> String {
    if value.trim().is_empty() {
        return value.to_string();
    }
    let mut escaped = String::with_capacity(value.len() + 4);
    escaped.push_str("**");
    for c in value.chars() {
        if matches!(c, '*' | '_' | '[' | ']' | '`' | '\\') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped.push_str("**");
    escaped
}

/// The rule a Signer Box draws: a fixed-width box with a bottom stroke,
/// about an inch and a half, sized to the line it sits on.
const SIGNER_BOX_TYPST: &str =
    "#box(width: 1.5in, height: 0.75em, stroke: (bottom: 0.5pt + black))";

/// The wash behind an unanswered `{{placeholder}}`. A soft highlighter
/// yellow: loud enough that nobody signs or sends a document with a blank
/// still in it, light enough that the token underneath stays legible in
/// print and in grayscale.
const PLACEHOLDER_HIGHLIGHT: &str = "#fff3a3";

/// Escape `s` for Typst, wrapping any surviving `{{placeholder}}` in a
/// yellow highlight.
///
/// A token still present at render time is one the questionnaire never
/// answered. Rendering it as ordinary prose makes a blank look like
/// intentional text — the exact failure that puts `{{person__client.name}}`
/// in front of a client. Marking it makes an unfinished document
/// unmistakably unfinished at a glance, while leaving it readable so an
/// attorney can see *which* answer is missing.
///
/// Only a whole `{{…}}` run on one text event is marked; a token broken
/// across events falls through to plain escaping rather than emitting
/// unbalanced markup.
fn escape_text_marking_placeholders(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find("{{") {
        let after_open = &rest[open + 2..];
        let Some(close) = after_open.find("}}") else {
            break;
        };
        out.push_str(&escape_marking_open_flags(&rest[..open]));
        out.push_str("#highlight(fill: rgb(\"");
        out.push_str(PLACEHOLDER_HIGHLIGHT);
        out.push_str("\"))[");
        out.push_str(&escape_text(&rest[open..open + 2 + close + 2]));
        out.push(']');
        rest = &after_open[close + 2..];
    }
    out.push_str(&escape_marking_open_flags(rest));
    out
}

/// The opening of a preview's inline flag for an open drafting marker.
pub const OPEN_FLAG_START: char = '\u{ab}';
/// The close of a preview's inline flag.
pub const OPEN_FLAG_END: char = '\u{bb}';

/// Escape `s`, washing every `«OPEN: …»` flag yellow. A `notation pdf
/// --preview` render rewrites each drafting marker into such a flag, so the
/// lawyer sees exactly what is still open; a final render never carries one.
fn escape_marking_open_flags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    let prefix = format!("{OPEN_FLAG_START}OPEN:");
    while let Some(open) = rest.find(&prefix) {
        let Some(close) = rest[open..].find(OPEN_FLAG_END) else {
            break;
        };
        let end = open + close + OPEN_FLAG_END.len_utf8();
        out.push_str(&escape_text(&rest[..open]));
        out.push_str("#highlight(fill: rgb(\"");
        out.push_str(PLACEHOLDER_HIGHLIGHT);
        out.push_str("\"))[");
        out.push_str(&escape_text(&rest[open..end]));
        out.push(']');
        rest = &rest[end..];
    }
    out.push_str(&escape_text(rest));
    out
}

fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '#' | '$' | '*' | '_' | '`' | '<' | '@' | '[' | ']'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Whether `tag` opens a bracketed Typst content container — one whose
/// markup wraps its children in `[...]` (block quote, table cell, link,
/// strikethrough). A `#pagebreak()` emitted inside one is a Typst error
/// ("pagebreaks are not allowed inside of containers"), so [`to_typst`]
/// suppresses the pagebreak sentinel while any such container is open.
fn opens_container(tag: &Tag) -> bool {
    matches!(
        tag,
        Tag::BlockQuote(_) | Tag::TableCell | Tag::Link { .. } | Tag::Strikethrough
    )
}

/// The [`opens_container`] counterpart for a closing tag.
fn closes_container(tag: TagEnd) -> bool {
    matches!(
        tag,
        TagEnd::BlockQuote(_) | TagEnd::TableCell | TagEnd::Link | TagEnd::Strikethrough
    )
}

/// Recognize the one HTML token [`to_typst`] honors: an HTML comment whose
/// body is `pagebreak` (case-insensitive, surrounding whitespace ignored),
/// e.g. `<!-- pagebreak -->`. It is invisible in the on-screen HTML preview
/// and maps to a Typst `#pagebreak()` in the PDF, letting a template force a
/// hard page break — for example, to start a signature block on a fresh page.
fn is_pagebreak_comment(html: &str) -> bool {
    let Some(inner) = html
        .trim()
        .strip_prefix("<!--")
        .and_then(|s| s.strip_suffix("-->"))
    else {
        return false;
    };
    inner.trim().eq_ignore_ascii_case("pagebreak")
}

/// Render `s` as a Typst double-quoted string literal, escaping the two
/// characters significant inside one. Used for `#raw(..)` / `#link(..)`
/// arguments, where the content is a string expression, not markup.
fn typst_string(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Squeeze runs of 3+ newlines down to exactly two (one blank line),
/// and trim leading/trailing whitespace, so the emitted Typst has
/// stable paragraph spacing regardless of how the handlers stacked
/// their `\n`s.
fn normalize_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut newline_run = 0usize;
    for c in s.chars() {
        if c == '\n' {
            newline_run += 1;
            if newline_run <= 2 {
                out.push(c);
            }
        } else {
            newline_run = 0;
            out.push(c);
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::to_typst;

    #[test]
    fn headings_become_equals_runs() {
        assert_eq!(to_typst("# Title"), "= Title");
        assert_eq!(to_typst("## Section"), "== Section");
        assert_eq!(to_typst("### Sub"), "=== Sub");
    }

    #[test]
    fn strong_collapses_to_single_asterisks() {
        // The crux: Markdown `**x**` must NOT survive as `**x**`, which
        // renders un-bolded in Typst.
        assert_eq!(to_typst("**bold**"), "*bold*");
    }

    #[test]
    fn emphasis_becomes_underscores() {
        assert_eq!(to_typst("*italic*"), "_italic_");
        assert_eq!(to_typst("_italic_"), "_italic_");
    }

    #[test]
    fn unordered_list_uses_typst_dash_markers() {
        let out = to_typst("- one\n- two\n");
        assert_eq!(out, "- one\n- two");
    }

    #[test]
    fn ordered_list_numbers_explicitly() {
        let out = to_typst("1. first\n2. second\n");
        assert_eq!(out, "1. first\n2. second");
    }

    #[test]
    fn inline_code_becomes_raw_call_not_backticks() {
        // A Typst backtick run would need matched delimiters; `#raw(..)`
        // is unambiguous and escapes nothing in the prose stream.
        assert_eq!(to_typst("`code`"), "#raw(\"code\")");
    }

    #[test]
    fn placeholder_tokens_pass_through_verbatim() {
        // `{{name}}` carries no Typst meaning; it must survive so the
        // caller can substitute it (before or after conversion).
        assert_eq!(to_typst("Hello `{{name}}`"), "Hello #raw(\"{{name}}\")");
        // The at-risk path is a *bare* token in prose, not backtick-wrapped:
        // `{` / `}` are deliberately absent from `escape_text` because Typst
        // markup treats them as literal characters (code is `#{..}`). So an
        // unfilled token must reach the page verbatim — escaping it to
        // `\{\{name\}\}` would corrupt the passthrough guarantee. It is
        // wrapped in the unanswered-placeholder highlight, but the token
        // inside is byte-for-byte what the caller substitutes on.
        assert_eq!(
            to_typst("Hello {{name}}"),
            "Hello #highlight(fill: rgb(\"#fff3a3\"))[{{name}}]"
        );
        // Even an own-line token and a dotted token (both present in real
        // templates) survive unescaped and compile.
        assert_eq!(
            to_typst("{{custom_clauses}}"),
            "#highlight(fill: rgb(\"#fff3a3\"))[{{custom\\_clauses}}]"
        );
        assert_eq!(
            to_typst("Signed {{client.signature}}"),
            "Signed #highlight(fill: rgb(\"#fff3a3\"))[{{client.signature}}]"
        );
    }

    /// LAW-183: a preview's `«OPEN: …»` flag is washed yellow like an
    /// unanswered placeholder; ordinary guillemets are not.
    #[test]
    fn an_open_flag_is_highlighted_and_other_guillemets_are_not() {
        assert_eq!(
            to_typst("Fee \u{ab}OPEN: amount\u{bb} due"),
            "Fee #highlight(fill: rgb(\"#fff3a3\"))[\u{ab}OPEN: amount\u{bb}] due"
        );
        assert!(!to_typst("\u{ab}quoted\u{bb}").contains("#highlight"));
    }

    #[test]
    fn typst_sigils_in_prose_are_escaped() {
        // A bare `#`/`$` in prose would otherwise start a Typst function
        // call or math block and break compilation.
        assert_eq!(to_typst(r"Pay $9,999 to #1"), r"Pay \$9,999 to \#1");
    }

    #[test]
    fn link_becomes_typst_link_call() {
        assert_eq!(
            to_typst("[neon](https://www.neonlaw.com)"),
            "#link(\"https://www.neonlaw.com\")[neon]"
        );
    }

    #[test]
    fn paragraphs_are_separated_by_one_blank_line() {
        assert_eq!(to_typst("one\n\ntwo"), "one\n\ntwo");
    }

    #[test]
    fn blockquote_wraps_in_typst_quote() {
        let out = to_typst("> noted");
        assert!(out.starts_with("#quote(block: true)["), "got: {out}");
        assert!(out.contains("noted"));
    }

    #[test]
    fn table_becomes_typst_table_call() {
        // A GFM table must reach Typst as `#table(..)`, not leak its
        // `| .. |` pipes into the page as literal prose. The header row
        // is wrapped in `table.header(..)`; body cells follow as a flat
        // stream that Typst lays out `columns`-wide.
        let out = to_typst("| A | B |\n| - | - |\n| 1 | 2 |");
        assert!(out.contains("#align(center)[#table("), "got: {out}");
        assert!(out.contains("columns: 2"), "got: {out}");
        assert!(out.contains("align: center"), "got: {out}");
        assert!(out.contains("table.header([A], [B], )"), "got: {out}");
        assert!(out.contains("[1], [2],"), "got: {out}");
    }

    /// `<br>` in a cell is a Typst line break, and the pipes are not prose.
    #[test]
    fn a_break_tag_in_a_table_cell_is_a_typst_line_break() {
        let out = to_typst(
            "| Party | Role |\n| --- | --- |\n| Alex Example | Claimant<br>care of agent |\n",
        );
        assert!(out.contains("#table("), "got: {out}");
        assert!(out.contains("Alex Example"), "got: {out}");
        assert!(out.contains("Claimant \\\ncare of agent"), "got: {out}");
        assert!(!out.contains('|'), "got: {out}");
        assert!(!out.contains("<br"), "got: {out}");
    }

    #[test]
    fn table_cells_carry_inline_markup_and_escapes() {
        // Cell content flows through the same inline handlers as prose:
        // `**bold**` collapses to Typst `*bold*`, and a `$` (Typst math)
        // is escaped so a currency figure renders verbatim.
        let out = to_typst("| Filing | Fee |\n| - | - |\n| List | **$150.00** |");
        assert!(out.contains(r"[*\$150.00*]"), "got: {out}");
    }

    #[test]
    fn table_column_alignment_maps_to_typst_align() {
        // Explicit `:--` / `:--:` / `--:` alignment markers force the
        // tuple, mapping left/center/right per column.
        let out = to_typst("| A | B | C |\n| :-- | :--: | --: |\n| 1 | 2 | 3 |");
        assert!(out.contains("align: (left, center, right)"), "got: {out}");
        crate::render(&out).expect("converted aligned table must compile through Typst");
    }

    #[test]
    fn table_output_is_typst_compilable() {
        // The safety net: a real fee table (header, currency, a bold
        // total row) must compile all the way through Typst.
        let md = "\
| Nevada Secretary of State filing | Fee |\n\
| - | - |\n\
| Articles of Organization | $75.00 |\n\
| **Total state filing fees** | **$425.00** |";
        crate::render(&to_typst(md)).expect("a converted table must compile through Typst");
    }

    #[test]
    fn pagebreak_comment_becomes_typst_pagebreak() {
        // The one HTML token honored: `<!-- pagebreak -->` forces a page
        // break. Whitespace and case inside the comment don't matter.
        assert!(to_typst("<!-- pagebreak -->").contains("#pagebreak()"));
        assert!(to_typst("<!--pagebreak-->").contains("#pagebreak()"));
        assert!(to_typst("<!--  PageBreak  -->").contains("#pagebreak()"));
        // A page break between two paragraphs must compile through Typst.
        let typ = to_typst("First page.\n\n<!-- pagebreak -->\n\nSecond page.");
        assert!(typ.contains("#pagebreak()"), "got: {typ}");
        crate::render(&typ).expect("a page break must compile through Typst");
    }

    #[test]
    fn pagebreak_sentinel_inside_a_container_is_dropped_not_emitted() {
        // A `#pagebreak()` is illegal inside a Typst container and would
        // fail the whole render, so the sentinel is honored only at the top
        // level. Inside a table cell or a block quote it is dropped (like
        // any other HTML), and — crucially — the document still compiles.
        let table = to_typst("| a | b |\n|---|---|\n| <!-- pagebreak --> | y |\n");
        assert!(
            !table.contains("#pagebreak()"),
            "sentinel in a table cell must not emit a page break; got: {table}"
        );
        crate::render(&table).expect("a table with the sentinel in a cell must still compile");

        let quote = to_typst("> before\n>\n> <!-- pagebreak -->\n>\n> after");
        assert!(
            !quote.contains("#pagebreak()"),
            "sentinel in a block quote must not emit a page break; got: {quote}"
        );
        crate::render(&quote).expect("a block quote with the sentinel must still compile");

        // A top-level sentinel after a container still works — the depth
        // counter returns to 0 when the container closes.
        let after = to_typst("> quoted\n\n<!-- pagebreak -->\n\nAfter.");
        assert!(
            after.contains("#pagebreak()"),
            "a top-level sentinel after a container must still break; got: {after}"
        );
        crate::render(&after).expect("must compile");
    }

    #[test]
    fn non_pagebreak_html_comments_are_dropped() {
        // Only the pagebreak sentinel is honored; any other comment is
        // dropped, neither leaked as text nor treated as a page break.
        let out = to_typst("Before.\n\n<!-- TODO: revise -->\n\nAfter.");
        assert!(!out.contains("TODO"), "got: {out}");
        assert!(!out.contains("#pagebreak()"), "got: {out}");
    }

    #[test]
    fn output_is_typst_compilable() {
        // The real safety net: whatever we emit must compile.
        let md = "# Demand\n\nPay **now** to `{{party}}`:\n\n- item one\n- item two\n\n> heed this";
        let typ = to_typst(md);
        crate::render(&typ).expect("converted markdown must compile through Typst");
    }

    #[test]
    fn an_unanswered_placeholder_is_highlighted_and_prose_is_not() {
        // A token still present at render time is a blank. It must be
        // impossible to mistake for intentional prose, and still readable
        // so an attorney sees which answer is missing.
        let out = to_typst("Dear {{person__client.name}}, welcome.");
        assert!(
            out.contains("#highlight(fill: rgb(\"#fff3a3\"))["),
            "placeholder not highlighted: {out}"
        );
        assert!(out.contains("person\\_\\_client.name"), "token lost: {out}");
        // The surrounding prose is untouched.
        assert!(out.contains("Dear "), "{out}");
        assert!(out.contains(", welcome."), "{out}");
        // Text with no placeholder gains no markup at all.
        assert!(!to_typst("Plain prose.").contains("#highlight"));
    }

    /// LAW-141: a Signer Box the fill wrote in place of an unanswered
    /// placeholder is an empty rule, not a highlighted blank.
    #[test]
    fn a_signer_box_draws_an_empty_rule_and_is_not_highlighted() {
        let body = format!(
            "- ({}) (L) retirement benefit transactions;\n\nShares: {}\n\n\
             | Party | Initials |\n| --- | --- |\n| Example Co. | {} |\n",
            word::paper::SIGNER_BOX,
            word::paper::SIGNER_BOX,
            word::paper::SIGNER_BOX
        );
        let out = to_typst(&body);
        assert_eq!(out.matches("stroke: (bottom:").count(), 3, "{out}");
        assert!(!out.contains("#highlight"), "{out}");
        assert!(!out.contains("signer-box"), "{out}");
        assert!(
            out.contains("(#box("),
            "the box sits inside the parentheses: {out}"
        );
        crate::render(&out).expect("must compile");
    }

    #[test]
    fn several_placeholders_on_one_line_are_each_highlighted() {
        let out = to_typst("{{a}} and {{b}} and {{c}}");
        assert_eq!(out.matches("#highlight(fill:").count(), 3, "{out}");
        crate::render(&out).expect("must compile");
    }

    #[test]
    fn a_filled_answer_renders_bold() {
        // A substituted answer must be visually distinct from the boilerplate
        // it sits in: a reader skimming a retainer should be able to find
        // every fact particular to their matter without reading the prose.
        assert_eq!(
            super::bold_answer("Acme Robotics, Inc."),
            "**Acme Robotics, Inc.**"
        );
        let typ = to_typst(&format!("Client: {}.", super::bold_answer("Jordan Rivera")));
        assert!(typ.contains("*Jordan Rivera*"), "not strong: {typ}");
        crate::render(&typ).expect("must compile");
    }

    #[test]
    fn a_filled_answer_containing_markup_stays_bold_and_compiles() {
        // A client name or a scope sentence may contain `*` or `_`. Wrapping
        // it naively would unbalance the strong run and either swallow the
        // rest of the paragraph or fail to compile.
        for value in ["A*B", "under_score", "**already bold**", "50% * 2"] {
            let typ = to_typst(&format!("Value: {}.", super::bold_answer(value)));
            crate::render(&typ).unwrap_or_else(|e| panic!("`{value}` broke rendering: {e}"));
            // Typst strong is `*…*`; the run must open right where the
            // answer starts and close before the trailing period, with the
            // value's own sigils escaped inside rather than ending it early.
            let inner = typ
                .strip_prefix("Value: *")
                .and_then(|t| t.strip_suffix("*."))
                .unwrap_or_else(|| panic!("`{value}` did not open one strong run: {typ}"));
            assert!(
                !inner.is_empty() && !inner.contains("*."),
                "`{value}` closed its strong run early: {typ}"
            );
        }
    }

    #[test]
    fn an_empty_answer_is_not_wrapped_into_stray_asterisks() {
        // `****` is not strong emphasis in Markdown; it renders as four
        // literal asterisks on the page.
        assert_eq!(super::bold_answer(""), "");
        assert_eq!(super::bold_answer("   "), "   ");
    }

    #[test]
    fn an_unterminated_placeholder_falls_through_to_plain_escaping() {
        // `{{` with no closer must not emit an unbalanced `#highlight[`
        // that fails to compile.
        let out = to_typst("An open {{brace and nothing else.");
        assert!(!out.contains("#highlight"), "{out}");
        crate::render(&out).expect("must compile");
    }

    #[test]
    fn a_lettered_quote_paragraph_hangs_its_marker() {
        let out = to_typst("> **A. Payment.** Due on receipt.");
        assert!(
            out.contains("#par(hanging-indent: 2em)[*#box(width: 2em)[A.]Payment.* Due"),
            "{out}"
        );
        crate::render(&out).expect("must compile");
    }

    #[test]
    fn a_marker_that_ends_its_own_run_drops_the_following_space() {
        let out = to_typst("> **A.** Payment.");
        assert!(out.contains("[A.]*Payment.]"), "{out}");
    }

    #[test]
    fn a_quote_that_opens_with_prose_or_a_top_level_paragraph_is_not_hung() {
        for md in [
            "> No. 5 is due.",
            "> Plain prose.",
            "> 1234. Too many digits.",
            "A. Not in a quote.",
        ] {
            let out = to_typst(md);
            assert!(!out.contains("hanging-indent"), "{md}: {out}");
        }
    }

    #[test]
    fn a_nested_list_opens_on_its_own_line() {
        let out = to_typst("1. One\n   1. Nested\n2. Two");
        assert!(out.contains("1. One\n  1. Nested\n"), "{out}");
    }

    /// A soft break is a source-line wrap and prints as a space; only a
    /// `CommonMark` hard break (two trailing spaces, or a backslash before
    /// the newline) starts a new line.
    #[test]
    fn soft_breaks_join_and_hard_breaks_break() {
        assert_eq!(to_typst("one\ntwo").trim(), "one two");
        assert_eq!(to_typst("one  \ntwo").trim(), "one \\\ntwo");
        assert_eq!(to_typst("one\\\ntwo").trim(), "one \\\ntwo");
        // A trailing backslash on a block's last line is a literal one.
        assert_eq!(to_typst("one\\").trim(), "one\\\\");
    }

    /// An answer substituted into the body is escaped by `bold_answer`, so
    /// strong markup in it stays strong and a literal sigil stays literal;
    /// valid `**strong**` in authored prose is real strong, not text.
    #[test]
    fn a_bold_answer_keeps_its_sigils_literal_and_strong_markup_strong() {
        let answered = format!("Plaintiff {} appears.", super::bold_answer("A * B_C [x]"));
        let out = to_typst(&answered);
        assert!(out.contains("*A \\* B\\_C \\[x\\]*"), "{out}");
        assert_eq!(to_typst("a **b** c").trim(), "a *b* c");
        crate::render(&out).expect("must compile");
    }

    #[test]
    fn a_keep_group_wraps_its_blocks_and_defines_its_helper() {
        let out = to_typst("<!-- keep -->\n\nRequest.\n\nResponse.\n\n<!-- /keep -->\n\nAfter.");
        assert!(out.starts_with("#let keep(body)"), "{out}");
        assert!(out.contains("#keep[\nRequest."), "{out}");
        assert!(out.contains("Response.\n\n]\n\nAfter."), "{out}");
        crate::render(&out).expect("must compile");
    }

    #[test]
    fn an_unclosed_keep_group_is_closed_and_a_stray_close_is_dropped() {
        let out = to_typst("<!-- /keep -->\n\nA.\n\n<!-- keep -->\n\nB.");
        assert!(!out.contains("#keep[\nA"), "{out}");
        crate::render(&out).expect("an unbalanced marker must still compile");
        assert!(!to_typst("Plain.").contains("#let keep"));
    }

    #[test]
    fn a_pagebreak_inside_a_keep_group_is_dropped_not_fatal() {
        let out = to_typst("<!-- keep -->\n\nA.\n\n<!-- pagebreak -->\n\nB.\n\n<!-- /keep -->");
        assert!(!out.contains("#pagebreak"), "{out}");
        crate::render(&out).expect("must compile");
    }
}
