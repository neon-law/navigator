//! Importing a lawyer-edited `.docx` back into its notation template.
//!
//! The flow is the inverse of [`crate::render_notation`]: the lawyer receives
//! a Word export, edits it in Word, and hands the `.docx` back. Import
//! re-derives the template's block baseline with the renderer's own walker
//! ([`crate::source_blocks`]), reads the edited package's body paragraphs,
//! aligns the two sequences, and returns the edits as hunks that splice back
//! into the template's Markdown at the original byte ranges.
//!
//! Guards, in order:
//!
//! 1. **Provenance** — the package must carry the export record
//!    ([`crate::read_provenance`]) and its template hash must still match
//!    ([`crate::verify_template_hash`]).
//! 2. **Tracked changes and comments** — a package with unaccepted
//!    revisions is refused; a package with review comments is refused with
//!    every comment listed by author, text, and the anchor paragraph it
//!    sits on, so the lawyer knows exactly what to resolve in Word.
//! 3. **Placeholder protection** — a `{{…}}` span (a fill placeholder or a
//!    `{{> name}}` include) may never be dropped, and new spans may never be
//!    invented, by an edit.
//!
//! The baseline covers only the template's body — everything past the
//! frontmatter block, which is exactly what the export rendered
//! ([`body_offset`]). The frontmatter is never offered to the aligner, and
//! `apply_import` refuses a hunk that reaches into it: a lawyer edits the
//! document, never the template's metadata. Changed prose is reflowed to
//! the project gate's line width on the way back in, and the CLI re-runs
//! the blocking gate over the updated template before anything is written.
//!
//! Tables are protected: baseline table blocks are never rewritten, and
//! paragraphs inside the package's tables are ignored. Drafts rendered with
//! a pleading caption are also out of scope — the caption has no baseline.

use std::collections::HashMap;
use std::io::Read;
use std::ops::Range;

use crate::provenance::{read_provenance, verify_template_hash, ProvenanceError};
use crate::render::{source_blocks, SourceBlock, SourceBlockKind};

/// One importable edit: the template byte range it rewrites and the exact
/// replacement text (including the block's Markdown prefix and separators).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// The range of the template this hunk replaces. An empty range marks a
    /// pure insertion at that offset.
    pub source: Range<usize>,
    pub kind: HunkKind,
}

/// What a hunk does, kept for the dry-run report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunkKind {
    Replaced { old: String, new: String },
    Inserted { new: String },
    Deleted { old: String },
}

impl Hunk {
    #[must_use]
    pub fn replacement(&self) -> &str {
        match &self.kind {
            HunkKind::Replaced { new, .. } | HunkKind::Inserted { new } => new,
            HunkKind::Deleted { .. } => "",
        }
    }

    #[must_use]
    pub fn summary(&self) -> String {
        match &self.kind {
            HunkKind::Replaced { old, new } => {
                format!("replaced {:?} with {:?}", abbreviate(old), abbreviate(new))
            }
            HunkKind::Inserted { new } => format!("inserted {:?}", abbreviate(new)),
            HunkKind::Deleted { old } => format!("deleted {:?}", abbreviate(old)),
        }
    }
}

fn abbreviate(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= 60 {
        compact
    } else {
        let cut = compact
            .char_indices()
            .nth(60)
            .map_or(compact.len(), |(i, _)| i);
        format!("{}…", &compact[..cut])
    }
}

/// The planned, placeholder-checked edits for one template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportPlan {
    /// The template path recorded by the export that produced the package.
    pub template_path: String,
    pub hunks: Vec<Hunk>,
}

/// Why an edited package cannot be imported.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ImportError {
    #[error(transparent)]
    Provenance(#[from] ProvenanceError),
    #[error("the edited draft still carries tracked changes; accept or reject them in Word, then export again")]
    TrackedChanges,
    #[error("the edit removed the placeholder {0}, which the template needs")]
    PlaceholderRemoved(String),
    #[error("the edit added a new placeholder {0}, which the template never had")]
    PlaceholderAdded(String),
    #[error("an import hunk would rewrite the template's frontmatter, which no edit may touch")]
    FrontmatterMutation,
    #[error(
        "the edited draft carries review comments; resolve them in Word, then export again{list}"
    )]
    Comments {
        /// The rendered comment list, one comment per line — the format
        /// argument the refusal message prints.
        list: String,
        /// The comments themselves, for callers that inspect the refusal.
        comments: Vec<ReviewComment>,
    },
    #[error("an import hunk no longer fits the template")]
    HunkOutOfBounds,
}

/// One review comment an edited package still carries, reported in the
/// refusal: who wrote it, what it says, and the anchor paragraph it sits on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewComment {
    pub author: String,
    pub text: String,
    pub anchor: String,
}

impl std::fmt::Display for ReviewComment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "\"{}\"", self.text)?;
        if !self.author.is_empty() {
            write!(f, " by {}", self.author)?;
        }
        if !self.anchor.is_empty() {
            write!(f, ", anchored at: \"{}\"", self.anchor)?;
        }
        Ok(())
    }
}

/// The comment list's rendering in the [`ImportError::Comments`] message:
/// one comment per line, so the lawyer sees every unresolved thread.
fn list_comments(comments: &[ReviewComment]) -> String {
    use std::fmt::Write as _;
    comments.iter().fold(String::new(), |mut list, comment| {
        let _ = write!(list, "\n  - {comment}");
        list
    })
}

/// Plan the import of `docx` back into `template`. `template_bytes` is the
/// template file's current content, hashed against the export provenance.
///
/// # Errors
/// The package's provenance fails verification, the package still carries
/// tracked changes or comments, or an edit would drop or invent a
/// `{{…}}` placeholder. A plan is only produced when every hunk is safe
/// to splice.
pub fn plan_import(
    template: &str,
    template_bytes: &[u8],
    docx: &[u8],
) -> Result<ImportPlan, ImportError> {
    let record = read_provenance(docx)?;
    verify_template_hash(&record, template_bytes)?;

    let body = extract_body_xml(docx).ok_or(ProvenanceError::Unreadable)?;
    let edited_doc = parse_body(&body).ok_or(ProvenanceError::Unreadable)?;
    if edited_doc.tracked_changes {
        return Err(ImportError::TrackedChanges);
    }
    if edited_doc.commented {
        let comments = read_comments(docx, &edited_doc.comment_anchors());
        return Err(ImportError::Comments {
            list: list_comments(&comments),
            comments,
        });
    }

    // The export rendered the template's body alone, so the baseline is the
    // body alone: the frontmatter never reaches the aligner, and no hunk
    // can claim a range inside it.
    let frontmatter_len = body_offset(template);
    let baseline: Vec<SourceBlock> = source_blocks(&template[frontmatter_len..])
        .into_iter()
        .filter(|block| block.kind != SourceBlockKind::Table)
        .map(|mut block| {
            block.source = block.source.start + frontmatter_len..block.source.end + frontmatter_len;
            block
        })
        .collect();
    let edited: Vec<String> = edited_doc
        .paragraphs
        .iter()
        .map(|paragraph| normalize(&paragraph.text))
        .filter(|text| !text.is_empty())
        .collect();

    let hunks = align(template, &baseline, &edited);
    let plan = ImportPlan {
        template_path: record.template_path,
        hunks,
    };
    protect_placeholders(template, &plan)?;
    Ok(plan)
}

/// Apply a plan produced by [`plan_import`], replacing hunks right-to-left so
/// earlier ranges stay valid while later ones splice.
///
/// # Errors
/// [`ImportError::HunkOutOfBounds`] when a plan's ranges no longer fit the
/// template, or [`ImportError::FrontmatterMutation`] when a hunk reaches
/// into the frontmatter — both only possible for a plan built by hand,
/// never by [`plan_import`].
pub fn apply_import(template: &str, plan: &ImportPlan) -> Result<String, ImportError> {
    let frontmatter_len = body_offset(template);
    let mut output = template.to_string();
    for hunk in plan.hunks.iter().rev() {
        if hunk.source.start < frontmatter_len
            && hunk.source.start <= hunk.source.end
            && hunk.source.end <= output.len()
        {
            return Err(ImportError::FrontmatterMutation);
        }
        if hunk.source.end > output.len() || hunk.source.start > hunk.source.end {
            return Err(ImportError::HunkOutOfBounds);
        }
        output.replace_range(hunk.source.clone(), hunk.replacement());
    }
    Ok(output)
}

/// The dry-run preview: the plan plus the import applied to a copy of the
/// template, which the CLI then diffs for the lawyer. The template itself is
/// untouched.
///
/// # Errors
/// Whatever [`plan_import`] or [`apply_import`] can raise.
pub fn dry_run_import(
    template: &str,
    template_bytes: &[u8],
    docx: &[u8],
) -> Result<(ImportPlan, String), ImportError> {
    let plan = plan_import(template, template_bytes, docx)?;
    let applied = apply_import(template, &plan)?;
    Ok((plan, applied))
}

/// The byte offset where a notation template's body starts: past a leading
/// YAML frontmatter block, or 0 when the template has none. The Word export
/// renders exactly this body, so the import baseline covers exactly this
/// body — and the render path's own frontmatter strip (`cli`'s
/// `strip_frontmatter`) delegates here, so the renderer and the importer
/// cannot disagree about where the frontmatter ends.
#[must_use]
pub fn body_offset(contents: &str) -> usize {
    let Some(after_open) = contents.strip_prefix("---\n") else {
        return 0;
    };
    match after_open.find("\n---\n") {
        Some(end) => {
            let body = &after_open[end + "\n---\n".len()..];
            contents.len() - body.trim_start_matches('\n').len()
        }
        // Closer at EOF with no body, or no closer at all.
        None if after_open.ends_with("\n---") => contents.len(),
        None => 0,
    }
}

/// Align the edited paragraphs against the baseline blocks and return hunks.
///
/// Matching is longest-common-subsequence over normalized text, computed by
/// dynamic programming — templates are small (hundreds of blocks), so the
/// quadratic table is fine and stays deterministic. Between matches, paired
/// runs become replacements and leftovers become inserts or deletes.
fn align(template: &str, baseline: &[SourceBlock], edited: &[String]) -> Vec<Hunk> {
    let mut table = vec![vec![0usize; edited.len() + 1]; baseline.len() + 1];
    for (b, base_block) in baseline.iter().enumerate().rev() {
        for (e, text) in edited.iter().enumerate().rev() {
            table[b][e] = if equal(base_block, text) {
                table[b + 1][e + 1] + 1
            } else {
                table[b + 1][e].max(table[b][e + 1])
            };
        }
    }

    let mut hunks = Vec::new();
    let (mut b, mut e) = (0usize, 0usize);
    while b < baseline.len() && e < edited.len() {
        if equal(&baseline[b], &edited[e]) {
            b += 1;
            e += 1;
            continue;
        }
        let run_b = b;
        let run_e = e;
        while b < baseline.len() && e < edited.len() && !equal(&baseline[b], &edited[e]) {
            if table[b + 1][e] >= table[b][e + 1] {
                b += 1;
            } else {
                e += 1;
            }
        }
        // A run that exhausted either list can never reach a later anchor,
        // so it extends to the end of both: its deletions and insertions
        // then pair as replacements inside one hunk, where the placeholder
        // guard can check them, instead of splitting into delete + insert.
        if b == baseline.len() || e == edited.len() {
            b = baseline.len();
            e = edited.len();
        }
        // Where an unmatched insert belongs: after the run's last rewritten
        // block, or before the next surviving block for a pure insert.
        let anchor = match baseline.get(b) {
            Some(next) if run_b == b => next.source.start,
            _ => match baseline.get(run_b..b).and_then(<[SourceBlock]>::last) {
                Some(last) => insert_position(template, last.source.end),
                None => template.len(),
            },
        };
        push_run_hunks(
            template,
            &baseline[run_b..b],
            &edited[run_e..e],
            anchor,
            &mut hunks,
        );
    }
    if b < baseline.len() || e < edited.len() {
        let anchor = baseline
            .get(b)
            .map_or(template.len(), |next| next.source.start);
        push_run_hunks(template, &baseline[b..], &edited[e..], anchor, &mut hunks);
    }
    hunks
}

fn equal(block: &SourceBlock, edited: &str) -> bool {
    normalize(&block.text) == edited
}

/// Normalize for comparison only: collapse whitespace runs and unify the
/// Signer Box sentinel (the renderer's private-use marker and the rule of
/// non-breaking spaces Word shows in its place).
#[must_use]
pub fn normalize(text: &str) -> String {
    let signer = char::from_u32(0xE000).expect("a Unicode scalar");
    let unified = text.replace('\u{00A0}', &signer.to_string());
    let mut out = String::with_capacity(unified.len());
    let mut pending_space = false;
    for ch in unified.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
        } else if ch == signer {
            if !out.ends_with(signer) {
                out.push(signer);
            }
            pending_space = false;
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(ch);
        }
    }
    out
}

/// The line width the import reflows changed prose to — the project gate's
/// own line limit (`rules::S101LineLength::DEFAULT_MAX`). Duplicated as a
/// constant because the `word` crate does not depend on `rules`.
const REFLOW_WIDTH: usize = 120;

/// Hard-wrap one prose line at word boundaries to the reflow width. A soft
/// wrap inside a paragraph is invisible in the rendered document — Markdown
/// joins the lines — so the wrap is value-preserving. A word longer than
/// the width stays on its own line: a URL must not be broken.
fn wrap_line(line: &str) -> String {
    if line.chars().count() <= REFLOW_WIDTH {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len() + 8);
    let mut width = 0usize;
    for word in line.split_whitespace() {
        let word_width = word.chars().count();
        if width > 0 && width + 1 + word_width > REFLOW_WIDTH {
            out.push('\n');
            width = 0;
        } else if width > 0 {
            out.push(' ');
            width += 1;
        }
        out.push_str(word);
        width += word_width;
    }
    out
}

/// Reflow a replacement text that renders as plain prose: every line longer
/// than the gate's width limit is hard-wrapped, so an import never hands the
/// project gate an `S101` failure on prose the lawyer wrote. A trailing
/// newline is structural — list items carry theirs — so it survives. Lines
/// that are not prose (headings, quotes) pass through unwrapped — the gate
/// re-check refuses an over-long line that remains.
fn reflow_prose(text: &str) -> String {
    let mut out = text.lines().map(wrap_line).collect::<Vec<_>>().join("\n");
    if text.ends_with('\n') && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn push_run_hunks(
    template: &str,
    deleted: &[SourceBlock],
    inserted: &[String],
    anchor: usize,
    hunks: &mut Vec<Hunk>,
) {
    let paired = deleted.len().min(inserted.len());
    for (base_block, new) in deleted.iter().zip(inserted.iter()).take(paired) {
        let mut replacement = rewrite(template, base_block, new);
        // A block's source range carries exactly one trailing newline as
        // its line terminator; the rewritten block must keep it or the
        // applied template glues this block to the next — and drops the
        // file's final newline, which the gate refuses (M047).
        if template[base_block.source.clone()].ends_with('\n') && !replacement.ends_with('\n') {
            replacement.push('\n');
        }
        hunks.push(Hunk {
            source: base_block.source.clone(),
            kind: HunkKind::Replaced {
                old: template[base_block.source.clone()].to_string(),
                new: replacement,
            },
        });
    }
    for base_block in &deleted[paired..] {
        hunks.push(Hunk {
            source: base_block.source.clone(),
            kind: HunkKind::Deleted {
                old: template[base_block.source.clone()].to_string(),
            },
        });
    }
    // Remaining inserts land one after another at the run's anchor, each
    // closed with a blank line so the next block still starts fresh.
    let mut at = anchor;
    for new in &inserted[paired..] {
        let text = format!("{}\n\n", reflow_prose(new));
        hunks.push(Hunk {
            source: at..at,
            kind: HunkKind::Inserted { new: text.clone() },
        });
        at += text.len();
    }
}

/// The offset just past `from` where template content resumes — the position
/// an insertion lands at without cutting into the next line's indent.
fn insert_position(template: &str, from: usize) -> usize {
    let rest = template.get(from..).unwrap_or("");
    match rest.find(|ch: char| !ch.is_whitespace()) {
        Some(offset) if offset > 0 => from + offset,
        _ => from,
    }
}

/// The replacement text for one rewritten block: the edited text under the
/// baseline block's own Markdown shape, at the level the template wrote.
fn rewrite(template: &str, block: &SourceBlock, edited: &str) -> String {
    let text = match block.kind {
        SourceBlockKind::ListItem => {
            let edited = edited.trim_start();
            let marker = block
                .text
                .trim_start()
                .split_once(' ')
                .map(|(marker, _)| marker);
            marker
                .and_then(|marker| edited.strip_prefix(&format!("{marker} ")))
                .unwrap_or(edited)
                .trim_start()
                .to_string()
        }
        SourceBlockKind::Quote => edited.trim_start().to_string(),
        _ => edited.to_string(),
    };
    match block.kind {
        SourceBlockKind::Heading => format!("{} {}", heading_prefix(template, block), text),
        SourceBlockKind::ListItem => {
            let source = template
                .get(block.source.clone())
                .unwrap_or("")
                .trim_start();
            let prefix = source.split_whitespace().next().unwrap_or("-");
            let trailing = &source[source.trim_end().len()..];
            // A wrapped continuation of a list item stays inside the item:
            // CommonMark joins lazy continuation lines without an indent.
            reflow_prose(&format!("{prefix} {text}{trailing}"))
        }
        SourceBlockKind::Quote => format!("> {text}"),
        SourceBlockKind::Paragraph => reflow_prose(&text),
        SourceBlockKind::Rule | SourceBlockKind::Table => text,
    }
}

fn heading_prefix(template: &str, block: &SourceBlock) -> String {
    // The heading level comes from the template's own source, not the Word
    // style, so an edit preserves exactly the level the template wrote.
    let source = template
        .get(block.source.start..block.source.end)
        .unwrap_or("");
    let level = source.chars().take_while(|&ch| ch == '#').count();
    let level = if level == 0 { 2 } else { level };
    "#".repeat(level)
}

/// Placeholder guard: every `{{…}}` span in the template slice a hunk
/// touches must survive the edit, and the edit may not invent spans the
/// template never had.
fn protect_placeholders(template: &str, plan: &ImportPlan) -> Result<(), ImportError> {
    for hunk in &plan.hunks {
        let old = template
            .get(hunk.source.start..hunk.source.end)
            .unwrap_or("");
        let new = hunk.replacement();
        for code in spans(old) {
            if !spans(new).contains(&code) {
                return Err(ImportError::PlaceholderRemoved(code));
            }
        }
        for code in spans(new) {
            if !spans(old).contains(&code) {
                return Err(ImportError::PlaceholderAdded(code));
            }
        }
    }
    Ok(())
}

/// The `{{…}}` spans (fill placeholders and `{{> include}}` partials) of a
/// text, in order. Unterminated spans are ignored — a lone `{{` is prose.
#[must_use]
pub fn spans(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        match after.find("}}") {
            Some(close) => {
                found.push(after[..close].trim().to_string());
                rest = &after[close + 2..];
            }
            None => break,
        }
    }
    found
}

/// The dry-run report: a block-level diff of the plan — each hunk's old
/// block text and new block text in full, `- old` / `+ new` — so the lawyer
/// approves exactly the lines that move, not an abbreviation of them.
#[must_use]
pub fn diff_report(plan: &ImportPlan) -> String {
    let mut report = String::new();
    for hunk in &plan.hunks {
        match &hunk.kind {
            HunkKind::Replaced { old, new } => {
                push_block(&mut report, '-', old);
                push_block(&mut report, '+', new);
            }
            HunkKind::Inserted { new } => push_block(&mut report, '+', new),
            HunkKind::Deleted { old } => push_block(&mut report, '-', old),
        }
    }
    report
}

fn push_block(report: &mut String, marker: char, block: &str) {
    for line in block.lines() {
        report.push(marker);
        report.push(' ');
        report.push_str(line);
        report.push('\n');
    }
}

// --- edited package reading -------------------------------------------------

/// One body paragraph of the edited package: its concatenated visible text
/// and the ids of any review comments anchored on it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EditedParagraph {
    text: String,
    comment_refs: Vec<String>,
}

struct EditedDocument {
    paragraphs: Vec<EditedParagraph>,
    tracked_changes: bool,
    commented: bool,
}

impl EditedDocument {
    /// The anchor paragraph for each comment reference, keyed by comment id.
    /// First reference wins: a comment is anchored where it was written.
    fn comment_anchors(&self) -> HashMap<String, String> {
        let mut anchors = HashMap::new();
        for paragraph in &self.paragraphs {
            for id in &paragraph.comment_refs {
                anchors
                    .entry(id.clone())
                    .or_insert_with(|| paragraph.text.clone());
            }
        }
        anchors
    }
}

/// The `word/document.xml` part of a package.
fn extract_body_xml(docx: &[u8]) -> Option<String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(docx)).ok()?;
    let mut xml = String::new();
    archive
        .by_name("word/document.xml")
        .ok()?
        .read_to_string(&mut xml)
        .ok()?;
    let start = xml.find("<w:body>")? + "<w:body>".len();
    let end = xml[start..]
        .find("<w:sectPr")
        .map_or(xml.len(), |end| start + end);
    Some(xml[start..end].to_string())
}

/// Walk the body's top-level elements: paragraphs in reading order (with
/// their text) and a whole-table skip. No XML dependency beyond the package
/// shape the renderer itself writes — and Word preserves that shape.
fn parse_body(body: &str) -> Option<EditedDocument> {
    let tracked_changes = body.contains("<w:ins ") || body.contains("<w:del ");
    let commented = body.contains("<w:commentReference ");
    let mut paragraphs = Vec::new();
    let mut rest = body;
    while let Some(position) = rest.find('<') {
        let element = &rest[position..];
        if element.starts_with("<w:p>") || element.starts_with("<w:p ") {
            let end = element.find("</w:p>")? + "</w:p>".len();
            paragraphs.push(EditedParagraph {
                text: paragraph_text(&element[..end]),
                comment_refs: comment_reference_ids(&element[..end]),
            });
            rest = &element[end..];
        } else if element.starts_with("<w:tbl>") {
            let end = element.find("</w:tbl>")? + "</w:tbl>".len();
            rest = &element[end..];
        } else {
            rest = &rest[position + 1..];
        }
    }
    Some(EditedDocument {
        paragraphs,
        tracked_changes,
        commented,
    })
}

/// A paragraph's visible text: every run's `w:t` contents joined, `<w:br/>`
/// as a newline, XML-escaped back to plain text.
fn paragraph_text(paragraph: &str) -> String {
    let mut text = String::new();
    let mut rest = paragraph;
    while let Some(position) = rest.find('<') {
        if rest[position..].starts_with("<w:br/>") {
            text.push('\n');
            rest = &rest[position + "<w:br/>".len()..];
            continue;
        }
        // `<w:t>` or `<w:t ` — never `<w:tab/>`, which shares the prefix.
        let opens_text =
            rest[position..].starts_with("<w:t>") || rest[position..].starts_with("<w:t ");
        if opens_text {
            let Some(tag_end) = rest[position..].find('>') else {
                break;
            };
            let open_end = position + tag_end + 1;
            let Some(close_offset) = rest[open_end..].find("</w:t>") else {
                break;
            };
            let close = open_end + close_offset;
            text.push_str(&unescape(&rest[open_end..close]));
            rest = &rest[close + "</w:t>".len()..];
            continue;
        }
        rest = &rest[position + 1..];
    }
    text
}

fn unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The ids of the review comments anchored on one paragraph, in order.
fn comment_reference_ids(paragraph: &str) -> Vec<String> {
    let marker = "<w:commentReference w:id=\"";
    let mut ids = Vec::new();
    let mut rest = paragraph;
    while let Some(position) = rest.find(marker) {
        let after = &rest[position + marker.len()..];
        match after.find('"') {
            Some(end) => {
                ids.push(after[..end].to_string());
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    ids
}

/// The review comments of an edited package: every comment in the package's
/// `word/comments.xml`, joined to the anchor paragraph its reference sits
/// on. A package whose comment part is missing or unreadable reports no
/// comments — the reference check has already refused the import; the list
/// only tells the lawyer what to resolve.
fn read_comments(docx: &[u8], anchors: &HashMap<String, String>) -> Vec<ReviewComment> {
    let Ok(mut archive) = zip::ZipArchive::new(std::io::Cursor::new(docx)) else {
        return Vec::new();
    };
    let xml = match archive.by_name("word/comments.xml") {
        Ok(mut part) => {
            let mut xml = String::new();
            if part.read_to_string(&mut xml).is_err() {
                return Vec::new();
            }
            xml
        }
        Err(_) => return Vec::new(),
    };

    let mut comments = Vec::new();
    let mut rest = xml.as_str();
    while let Some(open) = rest.find("<w:comment ") {
        let Some(tag_end) = rest[open..].find('>') else {
            break;
        };
        let open_tag_end = open + tag_end;
        let Some(element_end) = rest[open_tag_end..].find("</w:comment>") else {
            break;
        };
        let attributes = &rest[open..open_tag_end];
        let body = &rest[open_tag_end + 1..open_tag_end + element_end];
        let anchor_id = xml_attribute(attributes, "w:id").unwrap_or_default();
        comments.push(ReviewComment {
            author: xml_attribute(attributes, "w:author").unwrap_or_default(),
            text: comment_text(body),
            anchor: anchors.get(&anchor_id).cloned().unwrap_or_default(),
        });
        rest = &rest[open_tag_end + element_end + "</w:comment>".len()..];
    }
    comments
}

/// One attribute of an element's open tag. The renderer writes these parts
/// itself, so a boundary scan with XML-entity unescaping is the whole
/// grammar.
fn xml_attribute(source: &str, name: &str) -> Option<String> {
    let marker = format!("{name}=\"");
    let start = source.find(&marker)? + marker.len();
    let rest = &source[start..];
    let end = rest.find('"')?;
    Some(unescape(&rest[..end]))
}

/// The visible text of one comment: each of its paragraphs' run text,
/// joined with a space.
fn comment_text(body: &str) -> String {
    let mut parts = Vec::new();
    let mut rest = body;
    while let Some(position) = rest.find("<w:p>") {
        let element = &rest[position..];
        let Some(end) = element.find("</w:p>") else {
            break;
        };
        parts.push(paragraph_text(&element[..end + "</w:p>".len()]));
        rest = &element[end + "</w:p>".len()..];
    }
    if parts.is_empty() {
        paragraph_text(body)
    } else {
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_import, body_offset, diff_report, dry_run_import, plan_import, Hunk, HunkKind,
        ImportError, ImportPlan,
    };
    use crate::provenance::{template_hash, Provenance};
    use crate::render::{render_notation, RenderOptions};
    use std::io::Read;

    const TEMPLATE: &str = concat!(
        "# Service Agreement\n",
        "\n",
        "This agreement is between {{client_name}} and the Firm.\n",
        "\n",
        "The Firm will provide services described in {{> scope}}.\n",
        "\n",
        "## Fees\n",
        "\n",
        "Fees are due on the first of each month.\n",
        "\n",
        "- Retainer applies to the first invoice\n",
        "- Hourly work is billed monthly\n",
        "\n",
        "> Payment plans are available on request.\n",
    );

    /// Export TEMPLATE with provenance, apply an edit to the package the way
    /// Word would (rewrite `w:t` contents in `word/document.xml`), and hand
    /// the result back to the importer.
    fn export_then_edit(template: &str, edit: impl Fn(&mut String)) -> Vec<u8> {
        let bytes = render_notation(
            template,
            None,
            &RenderOptions {
                provenance: Some(Provenance {
                    template_path: "notation_templates/engagement/service.md",
                    template_hash: &template_hash(template.as_bytes()),
                }),
                ..RenderOptions::default()
            },
        )
        .unwrap();
        let mut document = document_xml(&bytes);
        edit(&mut document);
        rebuild(&bytes, &document)
    }

    /// Inject a `word/comments.xml` part into a package, as Word does when a
    /// reviewer saves comments into a draft.
    fn with_comment_part(bytes: &[u8], xml: &str) -> Vec<u8> {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut output = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).unwrap();
            let name = entry.name().unwrap_or_default().to_string();
            output
                .start_file(name.as_str(), zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::copy(&mut entry, &mut output).unwrap();
        }
        output
            .start_file(
                "word/comments.xml",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        std::io::Write::write_all(&mut output, xml.as_bytes()).unwrap();
        output.finish().unwrap().into_inner()
    }

    fn document_xml(bytes: &[u8]) -> String {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut xml = String::new();
        archive
            .by_name("word/document.xml")
            .unwrap()
            .read_to_string(&mut xml)
            .unwrap();
        xml
    }

    fn rebuild(bytes: &[u8], document: &str) -> Vec<u8> {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut output = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).unwrap();
            let name = entry.name().unwrap_or_default().to_string();
            output
                .start_file(name.as_str(), zip::write::SimpleFileOptions::default())
                .unwrap();
            if name == "word/document.xml" {
                std::io::Write::write_all(&mut output, document.as_bytes()).unwrap();
            } else {
                std::io::copy(&mut entry, &mut output).unwrap();
            }
        }
        output.finish().unwrap().into_inner()
    }

    fn replace_paragraph_text(document: &str, needle: &str, replacement: &str) -> String {
        document.replacen(needle, replacement, 1)
    }

    #[test]
    fn an_unchanged_round_trip_plans_no_edits() {
        let docx = export_then_edit(TEMPLATE, |_| {});
        let plan = plan_import(TEMPLATE, TEMPLATE.as_bytes(), &docx).unwrap();
        assert!(plan.hunks.is_empty(), "{:?}", plan.hunks);
        assert_eq!(
            plan.template_path,
            "notation_templates/engagement/service.md"
        );
    }

    #[test]
    fn an_edited_paragraph_replaces_only_that_paragraph_in_the_template() {
        let docx = export_then_edit(TEMPLATE, |document| {
            *document = replace_paragraph_text(
                document,
                "Fees are due on the first of each month.",
                "Fees are due on the tenth of each month.",
            );
        });
        let (plan, applied) = dry_run_import(TEMPLATE, TEMPLATE.as_bytes(), &docx).unwrap();
        assert_eq!(plan.hunks.len(), 1, "{:?}", plan.hunks);
        assert!(matches!(plan.hunks[0].kind, HunkKind::Replaced { .. }));
        assert!(applied.contains("Fees are due on the tenth of each month."));
        assert!(!applied.contains("Fees are due on the first"));
        // Everything the lawyer did not touch is byte-identical.
        assert!(applied.contains("This agreement is between {{client_name}} and the Firm."));
        assert!(applied.contains("{{> scope}}"));
        assert!(applied.contains("## Fees"));
        assert!(applied.contains("- Retainer applies to the first invoice"));
        assert!(applied.contains("> Payment plans are available on request."));
        // And the rewritten template still parses to the same block count.
        assert_eq!(
            crate::render::source_blocks(TEMPLATE).len(),
            crate::render::source_blocks(&applied).len()
        );
    }

    #[test]
    fn a_list_item_edit_keeps_its_dash_prefix() {
        let docx = export_then_edit(TEMPLATE, |document| {
            *document = replace_paragraph_text(
                document,
                "• Retainer applies to the first invoice",
                "• Retainer applies to every invoice",
            );
        });
        let (plan, applied) = dry_run_import(TEMPLATE, TEMPLATE.as_bytes(), &docx).unwrap();
        assert_eq!(plan.hunks.len(), 1);
        assert!(applied.contains("- Retainer applies to every invoice"));
        assert!(applied.contains("- Hourly work is billed monthly"));
    }

    #[test]
    fn a_new_paragraph_after_an_unchanged_one_is_an_insertion_not_a_replacement() {
        let docx = export_then_edit(TEMPLATE, |document| {
            let needle = "</w:t></w:r></w:p><w:p><w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr>";
            let insertion = "</w:t></w:r></w:p><w:p><w:r><w:t xml:space=\"preserve\">\
                A notice period of thirty days applies.</w:t></w:r></w:p><w:p>\
                <w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr>";
            *document = replace_paragraph_text(document, needle, insertion);
        });
        let (plan, applied) = dry_run_import(TEMPLATE, TEMPLATE.as_bytes(), &docx).unwrap();
        assert_eq!(plan.hunks.len(), 1, "{:?}", plan.hunks);
        assert!(matches!(plan.hunks[0].kind, HunkKind::Inserted { .. }));
        assert!(applied.contains("A notice period of thirty days applies."));
        assert!(
            applied.contains("## Fees"),
            "the heading after the insert survives"
        );
    }

    #[test]
    fn a_deleted_paragraph_leaves_the_rest_of_the_template_intact() {
        let docx = export_then_edit(TEMPLATE, |document| {
            let start = document.find("Hourly work is billed monthly").unwrap();
            let open = document[..start].rfind("<w:p>").unwrap();
            let end = document[start..].find("</w:p>").unwrap() + start + "</w:p>".len();
            document.replace_range(open..end, "");
        });
        let (plan, applied) = dry_run_import(TEMPLATE, TEMPLATE.as_bytes(), &docx).unwrap();
        assert_eq!(plan.hunks.len(), 1, "{:?}", plan.hunks);
        assert!(matches!(plan.hunks[0].kind, HunkKind::Deleted { .. }));
        assert!(!applied.contains("Hourly work is billed monthly"));
        assert!(applied.contains("- Retainer applies to the first invoice"));
    }

    #[test]
    fn dropping_a_placeholder_is_refused() {
        let docx = export_then_edit(TEMPLATE, |document| {
            *document = replace_paragraph_text(document, "{{client_name}}", "the client");
        });
        let error = plan_import(TEMPLATE, TEMPLATE.as_bytes(), &docx).unwrap_err();
        assert!(
            matches!(error, ImportError::PlaceholderRemoved(ref code) if code == "client_name")
        );
    }

    #[test]
    fn dropping_a_clause_include_is_refused_naming_it() {
        // The lawyer deleted the sentence that carried the `{{> scope}}`
        // partial. The refusal names the include, it does not silently bake
        // the paragraph's rendered value into the template.
        let docx = export_then_edit(TEMPLATE, |document| {
            *document = replace_paragraph_text(
                document,
                "The Firm will provide services described in {{&gt; scope}}.",
                "The Firm will provide the services described above.",
            );
        });
        let error = plan_import(TEMPLATE, TEMPLATE.as_bytes(), &docx).unwrap_err();
        assert!(matches!(error, ImportError::PlaceholderRemoved(ref code) if code == "> scope"));
    }

    #[test]
    fn adding_a_clause_include_is_refused() {
        let docx = export_then_edit(TEMPLATE, |document| {
            *document = replace_paragraph_text(
                document,
                "Fees are due on the first of each month.",
                "Fees are due on the first of each month. See {{&gt; termination}} for terms.",
            );
        });
        let error = plan_import(TEMPLATE, TEMPLATE.as_bytes(), &docx).unwrap_err();
        assert!(
            matches!(error, ImportError::PlaceholderAdded(ref code) if code == "> termination")
        );
    }

    #[test]
    fn splitting_a_placeholder_across_paragraphs_is_refused() {
        // The lawyer broke the placeholder itself in two: each half is plain
        // prose on its own paragraph, so the guard sees the span disappear.
        let docx = export_then_edit(TEMPLATE, |document| {
            *document = replace_paragraph_text(
                document,
                "{{client_name}}",
                "{{clie</w:t></w:r></w:p><w:p><w:r><w:t xml:space=\"preserve\">nt_name}}",
            );
        });
        let error = plan_import(TEMPLATE, TEMPLATE.as_bytes(), &docx).unwrap_err();
        assert!(
            matches!(error, ImportError::PlaceholderRemoved(ref code) if code == "client_name")
        );
    }

    #[test]
    fn replacing_a_placeholder_with_its_filled_value_is_refused() {
        // Baking the rendered answer into the template is the same defect as
        // dropping the placeholder: the next render would print the baked
        // value for every client.
        let docx = export_then_edit(TEMPLATE, |document| {
            *document = replace_paragraph_text(document, "{{client_name}}", "Widget Works, Inc.");
        });
        let error = plan_import(TEMPLATE, TEMPLATE.as_bytes(), &docx).unwrap_err();
        assert!(
            matches!(error, ImportError::PlaceholderRemoved(ref code) if code == "client_name")
        );
    }

    #[test]
    fn inventing_a_placeholder_is_refused() {
        let docx = export_then_edit(TEMPLATE, |document| {
            *document = replace_paragraph_text(
                document,
                "Fees are due on the first of each month.",
                "Fees are due on the first of each month at {{hourly_rate}}.",
            );
        });
        let error = plan_import(TEMPLATE, TEMPLATE.as_bytes(), &docx).unwrap_err();
        assert!(matches!(error, ImportError::PlaceholderAdded(ref code) if code == "hourly_rate"));
    }

    #[test]
    fn editing_around_a_placeholder_is_allowed() {
        let docx = export_then_edit(TEMPLATE, |document| {
            *document = replace_paragraph_text(
                document,
                "This agreement is between",
                "This engagement agreement is between",
            );
        });
        let plan = plan_import(TEMPLATE, TEMPLATE.as_bytes(), &docx).unwrap();
        assert_eq!(plan.hunks.len(), 1);
    }

    #[test]
    fn a_hash_mismatch_refuses_the_import() {
        let docx = export_then_edit(TEMPLATE, |_| {});
        let changed = format!("{TEMPLATE}\n<!-- edited later -->\n");
        let error = plan_import(&changed, changed.as_bytes(), &docx).unwrap_err();
        assert!(error.to_string().contains("re-export a fresh draft"));
        assert!(matches!(
            error,
            ImportError::Provenance(crate::provenance::ProvenanceError::HashMismatch { .. })
        ));
    }

    #[test]
    fn a_package_without_provenance_is_refused() {
        let bytes = render_notation(TEMPLATE, None, &RenderOptions::default()).unwrap();
        let error = plan_import(TEMPLATE, TEMPLATE.as_bytes(), &bytes).unwrap_err();
        assert!(error.to_string().contains("re-export the draft"));
    }

    #[test]
    fn tracked_changes_are_refused() {
        let docx = export_then_edit(TEMPLATE, |document| {
            *document = document.replace(
                "<w:r><w:t xml:space=\"preserve\">Fees",
                "<w:ins w:id=\"1\" w:author=\"N\"><w:r><w:t xml:space=\"preserve\">Fees",
            );
        });
        let error = plan_import(TEMPLATE, TEMPLATE.as_bytes(), &docx).unwrap_err();
        assert!(matches!(error, ImportError::TrackedChanges));
        assert!(error.to_string().contains("tracked changes"));
    }

    #[test]
    fn comments_are_refused() {
        let docx = export_then_edit(TEMPLATE, |document| {
            *document = document.replace(
                "<w:r><w:t xml:space=\"preserve\">Fees are due",
                "<w:r><w:rPr/><w:commentReference w:id=\"7\"/><w:t xml:space=\"preserve\">Fees are due",
            );
        });
        let error = plan_import(TEMPLATE, TEMPLATE.as_bytes(), &docx).unwrap_err();
        assert!(matches!(error, ImportError::Comments { .. }));
    }

    #[test]
    fn the_comment_refusal_prints_every_comment_with_its_anchor() {
        let docx = export_then_edit(TEMPLATE, |document| {
            *document = document.replace(
                "<w:r><w:t xml:space=\"preserve\">Fees are due",
                "<w:r><w:rPr/><w:commentReference w:id=\"7\"/><w:t xml:space=\"preserve\">Fees are due",
            );
        });
        let docx = with_comment_part(
            &docx,
            concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
                "<w:comments xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">",
                "<w:comment w:id=\"7\" w:author=\"Nicholas Shook\">",
                "<w:p><w:r><w:t>Fee is wrong</w:t></w:r></w:p></w:comment>",
                "<w:comment w:id=\"8\" w:author=\"Nick\"><w:p><w:r><w:t>Confirm the retainer</w:t></w:r></w:p></w:comment>",
                "</w:comments>",
            ),
        );
        let error = plan_import(TEMPLATE, TEMPLATE.as_bytes(), &docx).unwrap_err();
        let ImportError::Comments { comments, .. } = &error else {
            panic!("expected a comment refusal, got: {error}")
        };
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].author, "Nicholas Shook");
        assert_eq!(comments[0].text, "Fee is wrong");
        assert_eq!(
            comments[0].anchor,
            "Fees are due on the first of each month."
        );
        assert_eq!(comments[1].text, "Confirm the retainer");
        // Comment 8 is unreferenced in the body, so it anchors nowhere.
        assert_eq!(comments[1].anchor, "");
        let message = error.to_string();
        assert!(message.contains("Fee is wrong"), "{message}");
        assert!(
            message.contains("anchored at: \"Fees are due on the first"),
            "{message}"
        );
        assert!(message.contains("Confirm the retainer"), "{message}");
    }

    #[test]
    fn tables_are_protected_from_edits() {
        let template = concat!(
            "# Rates\n",
            "\n",
            "| Service | Fee |\n",
            "|---------|-----|\n",
            "| Filing | $200 |\n",
            "\n",
            "Contact {{attorney_email}} for questions.\n",
        );
        let docx = export_then_edit(template, |document| {
            *document = replace_paragraph_text(document, "Contact", "Write");
        });
        // The edit lands in the paragraph after the table; the table's cells
        // were never offered to the aligner.
        let (plan, applied) = dry_run_import(template, template.as_bytes(), &docx).unwrap();
        assert!(applied.contains("| Filing | $200 |"));
        assert!(applied.contains("Write {{attorney_email}} for questions."));
        assert_eq!(plan.hunks.len(), 1);
    }

    #[test]
    fn applying_a_plan_right_to_left_never_shifts_earlier_ranges() {
        use super::{Hunk, ImportPlan};
        let template = "one\ntwo\nthree\n";
        let plan = ImportPlan {
            template_path: "t.md".into(),
            hunks: vec![
                Hunk {
                    source: 0..3,
                    kind: HunkKind::Replaced {
                        old: "one".into(),
                        new: "ONE".into(),
                    },
                },
                Hunk {
                    source: 8..13,
                    kind: HunkKind::Replaced {
                        old: "three".into(),
                        new: "THREE".into(),
                    },
                },
            ],
        };
        assert_eq!(apply_import(template, &plan).unwrap(), "ONE\ntwo\nTHREE\n");
    }
    #[test]
    fn an_ordered_item_edit_keeps_its_marker_without_duplicating_the_number() {
        let template = "3) Original item\n4) Untouched item\n";
        let bytes = export_then_edit(template, |document| {
            *document = replace_paragraph_text(document, "3. Original item", "3. Revised item");
        });
        let (plan, applied) = dry_run_import(template, template.as_bytes(), &bytes).unwrap();
        assert_eq!(plan.hunks.len(), 1);
        assert_eq!(applied, "3) Revised item\n4) Untouched item\n");
    }

    /// A template with frontmatter — the shape every real notation has. The
    /// export renders only the body, so the import must baseline only the
    /// body and never offer the frontmatter to the aligner.
    const FRONTMATTER_TEMPLATE: &str = concat!(
        "---\n",
        "kind: agreement\n",
        "---\n",
        "\n",
        "# Service Agreement\n",
        "\n",
        "This agreement is between {{client_name}} and the Firm.\n",
        "\n",
        "Fees are due on the first of each month.\n",
    );

    fn frontmatter_body(template: &str) -> &str {
        &template[body_offset(template)..]
    }

    fn export_body_with_provenance(body: &str, provenance_template: &str) -> Vec<u8> {
        render_notation(
            body,
            None,
            &RenderOptions {
                provenance: Some(Provenance {
                    template_path: "notation_templates/engagement/service.md",
                    template_hash: &template_hash(provenance_template.as_bytes()),
                }),
                ..RenderOptions::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn a_frontmatter_template_plans_no_edits_when_the_body_is_unchanged() {
        let docx = export_body_with_provenance(
            frontmatter_body(FRONTMATTER_TEMPLATE),
            FRONTMATTER_TEMPLATE,
        );
        let plan =
            plan_import(FRONTMATTER_TEMPLATE, FRONTMATTER_TEMPLATE.as_bytes(), &docx).unwrap();
        assert!(plan.hunks.is_empty(), "{:?}", plan.hunks);
    }

    #[test]
    fn a_frontmatter_round_trip_leaves_the_frontmatter_untouched() {
        let docx = export_body_with_provenance(
            frontmatter_body(FRONTMATTER_TEMPLATE),
            FRONTMATTER_TEMPLATE,
        );
        let docx = {
            let document = document_xml(&docx);
            let document = replace_paragraph_text(
                &document,
                "Fees are due on the first of each month.",
                "Fees are due on the tenth of each month.",
            );
            rebuild(&docx, &document)
        };
        let (plan, applied) =
            dry_run_import(FRONTMATTER_TEMPLATE, FRONTMATTER_TEMPLATE.as_bytes(), &docx).unwrap();
        assert_eq!(plan.hunks.len(), 1, "{:?}", plan.hunks);
        let frontmatter_len = body_offset(FRONTMATTER_TEMPLATE);
        assert_eq!(
            &applied[..frontmatter_len],
            &FRONTMATTER_TEMPLATE[..frontmatter_len]
        );
        assert!(applied.contains("Fees are due on the tenth"));
    }

    #[test]
    fn a_hand_built_plan_cannot_rewrite_the_frontmatter() {
        let plan = ImportPlan {
            template_path: "t.md".into(),
            hunks: vec![Hunk {
                source: 0..2,
                kind: HunkKind::Replaced {
                    old: "--".into(),
                    new: "xx".into(),
                },
            }],
        };
        let error = apply_import(FRONTMATTER_TEMPLATE, &plan).unwrap_err();
        assert!(matches!(error, ImportError::FrontmatterMutation), "{error}");
    }

    #[test]
    fn a_long_prose_edit_reflows_to_the_gate_width() {
        let long = "The Firm agrees to provide the professional legal services described in the statement of work, and the client agrees to pay for them monthly, subject to the limitation of liability set forth in the following section of this agreement.";
        assert!(long.chars().count() > 120);
        let docx = export_then_edit(TEMPLATE, |document| {
            *document =
                replace_paragraph_text(document, "Fees are due on the first of each month.", long);
        });
        let (_plan, applied) = dry_run_import(TEMPLATE, TEMPLATE.as_bytes(), &docx).unwrap();
        assert!(
            applied.lines().all(|line| line.chars().count() <= 120),
            "{applied}"
        );
        // The wrap is a soft wrap: the paragraph's words all survive, and
        // Markdown joins them back into one paragraph.
        let rejoined = applied.lines().collect::<Vec<_>>().join(" ");
        assert!(rejoined.contains(long), "{applied}");
    }

    #[test]
    fn the_dry_run_report_is_a_block_level_diff() {
        let docx = export_then_edit(TEMPLATE, |document| {
            *document = replace_paragraph_text(
                document,
                "Fees are due on the first of each month.",
                "Fees are due on the tenth of each month.",
            );
        });
        let (plan, _applied) = dry_run_import(TEMPLATE, TEMPLATE.as_bytes(), &docx).unwrap();
        let report = diff_report(&plan);
        assert!(
            report.contains("- Fees are due on the first of each month."),
            "{report}"
        );
        assert!(
            report.contains("+ Fees are due on the tenth of each month."),
            "{report}"
        );
    }
}
