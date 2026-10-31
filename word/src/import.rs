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
//!    revisions or review comments is refused; the lawyer resolves them in
//!    Word first.
//! 3. **Placeholder protection** — a `{{…}}` span (a fill placeholder or a
//!    `{{> name}}` include) may never be dropped, and new spans may never be
//!    invented, by an edit.
//!
//! Tables are protected: baseline table blocks are never rewritten, and
//! paragraphs inside the package's tables are ignored. Drafts rendered with
//! a pleading caption are also out of scope — the caption has no baseline.

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
    #[error("the edited draft carries review comments; resolve them in Word, then export again")]
    Comments,
    #[error("the edit removed the placeholder {0}, which the template needs")]
    PlaceholderRemoved(String),
    #[error("the edit added a new placeholder {0}, which the template never had")]
    PlaceholderAdded(String),
    #[error("an import hunk no longer fits the template")]
    HunkOutOfBounds,
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
    if edited_doc.tracked_changes || edited_doc.commented {
        return Err(if edited_doc.tracked_changes {
            ImportError::TrackedChanges
        } else {
            ImportError::Comments
        });
    }

    let baseline: Vec<SourceBlock> = source_blocks(template)
        .into_iter()
        .filter(|block| block.kind != SourceBlockKind::Table)
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
/// template — only possible for a plan built by hand, never by
/// [`plan_import`].
pub fn apply_import(template: &str, plan: &ImportPlan) -> Result<String, ImportError> {
    let mut output = template.to_string();
    for hunk in plan.hunks.iter().rev() {
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

fn push_run_hunks(
    template: &str,
    deleted: &[SourceBlock],
    inserted: &[String],
    anchor: usize,
    hunks: &mut Vec<Hunk>,
) {
    let paired = deleted.len().min(inserted.len());
    for (base_block, new) in deleted.iter().zip(inserted.iter()).take(paired) {
        hunks.push(Hunk {
            source: base_block.source.clone(),
            kind: HunkKind::Replaced {
                old: template[base_block.source.clone()].to_string(),
                new: rewrite(template, base_block, new),
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
        let text = format!("{new}\n\n");
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
        SourceBlockKind::ListItem => edited
            .trim_start()
            .trim_start_matches("• ")
            .trim_start()
            .to_string(),
        SourceBlockKind::Quote => edited.trim_start().to_string(),
        _ => edited.to_string(),
    };
    match block.kind {
        SourceBlockKind::Heading => format!("{} {}", heading_prefix(template, block), text),
        SourceBlockKind::ListItem => format!("- {text}"),
        SourceBlockKind::Quote => format!("> {text}"),
        SourceBlockKind::Paragraph | SourceBlockKind::Rule | SourceBlockKind::Table => text,
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

// --- edited package reading -------------------------------------------------

/// One body paragraph of the edited package: its concatenated visible text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EditedParagraph {
    text: String,
}

struct EditedDocument {
    paragraphs: Vec<EditedParagraph>,
    tracked_changes: bool,
    commented: bool,
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

#[cfg(test)]
mod tests {
    use super::{apply_import, dry_run_import, plan_import, HunkKind, ImportError};
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
                "<w:r><w:t xml:space=\"preserve\">Fees",
                "<w:r><w:rPr/><w:commentReference w:id=\"7\"/><w:t xml:space=\"preserve\">Fees",
            );
        });
        let error = plan_import(TEMPLATE, TEMPLATE.as_bytes(), &docx).unwrap_err();
        assert!(matches!(error, ImportError::Comments));
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
}
