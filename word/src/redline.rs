//! Real Word tracked changes between two renders of one notation template.
//!
//! `navigator notation word <template> --redline-against <previous>.docx`
//! renders the template exactly as `notation word` always does, then
//! rewrites the rendered package's `word/document.xml` so every difference
//! from the previous version is a real revision (`w:ins` / `w:del`) authored
//! as the signing lawyer. Word shows the struck and underlined text and the
//! lawyer accepts or rejects each change before the document is delivered.
//!
//! The redline is for review, never for splicing: [`crate::import`] refuses
//! any package carrying revisions, and this emitter writes exactly the
//! `<w:ins ` / `<w:del ` markers that refusal detects, so a redline fed
//! back to `notation import` is refused like any other tracked-changes
//! draft. The diff walks the same view the import planner aligns — body
//! paragraphs' visible text, tables passed through untouched — so the two
//! directions can never disagree about what a document says.
//!
//! Deleted paragraphs are carried into the redline as fully struck
//! paragraphs (runs converted to `w:delText`, the paragraph mark marked
//! deleted), so accepting all changes in Word makes them disappear.
//! Rewrapped paragraphs carry both the struck previous text and the
//! inserted current text in one paragraph, the way Word itself records a
//! rewrite. No revision carries a `w:date`: the package must stay
//! byte-identical across runs (LAW-19).

use std::io::{Read as _, Write as _};

use crate::import::{extract_body_xml, paragraph_text};
use crate::provenance::read_provenance;
use crate::render::xml;

/// What one redline pass changed, for the CLI's summary line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedlineCounts {
    /// Whole paragraphs the previous version did not have.
    pub insertions: usize,
    /// Whole paragraphs carried over struck through, for Word to remove.
    pub deletions: usize,
    /// Paragraphs rewritten in place: struck previous text, then the
    /// current text as an insertion, in one paragraph.
    pub replacements: usize,
}

impl RedlineCounts {
    #[must_use]
    pub fn total(&self) -> usize {
        self.insertions + self.deletions + self.replacements
    }
}

/// A redlined package: the rendered bytes with every difference from the
/// previous version written as a real Word revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redline {
    pub bytes: Vec<u8>,
    pub counts: RedlineCounts,
}

/// Why a redline cannot be produced.
#[derive(Debug, thiserror::Error)]
pub enum RedlineError {
    #[error("the previous document is not a readable Word package")]
    PreviousUnreadable,
    #[error(
        "the previous document was not exported by `navigator notation word` (no provenance \
         record); redline against a fresh render of the earlier version"
    )]
    PreviousWithoutProvenance,
    #[error(
        "the previous document still carries tracked changes; accept or reject them in Word \
         before redlining against it"
    )]
    PreviousTrackedChanges,
    #[error(
        "the previous document carries review comments; resolve them in Word before redlining \
         against it"
    )]
    PreviousCommented,
    #[error("the rendered document could not be re-read as a Word package")]
    RenderedUnreadable,
    #[error("rewriting the Word package failed: {0}")]
    Package(#[from] zip::result::ZipError),
    #[error("rewriting the Word package failed: {0}")]
    PackageWrite(#[from] std::io::Error),
}

/// Redline `new_docx` against `previous_docx`, authoring every revision as
/// `author`.
///
/// # Errors
///
/// [`RedlineError`] when the previous document is not a clean Navigator
/// export, or the package cannot be rewritten.
pub fn against(
    new_docx: &[u8],
    previous_docx: &[u8],
    author: &str,
) -> Result<Redline, RedlineError> {
    // The previous document must be one of ours: a package without the
    // export's provenance part has no baseline this emitter can vouch for.
    read_provenance(previous_docx).map_err(|_| RedlineError::PreviousWithoutProvenance)?;
    let previous_body = extract_body_xml(previous_docx).ok_or(RedlineError::PreviousUnreadable)?;
    if previous_body.contains("<w:ins ") || previous_body.contains("<w:del ") {
        return Err(RedlineError::PreviousTrackedChanges);
    }
    if previous_body.contains("<w:commentReference ") {
        return Err(RedlineError::PreviousCommented);
    }
    let previous_chunks = body_chunks(&previous_body).ok_or(RedlineError::PreviousUnreadable)?;
    let previous_paragraphs: Vec<String> = previous_chunks
        .iter()
        .filter_map(|chunk| chunk.paragraph_raw())
        .map(str::to_string)
        .collect();
    // Diff plain text on both sides; the raw XML is kept only for
    // reconstructing deleted and rewritten paragraphs.
    let previous_texts: Vec<String> = previous_paragraphs
        .iter()
        .map(|raw| paragraph_text(raw))
        .collect();

    let document = read_document_xml(new_docx).ok_or(RedlineError::RenderedUnreadable)?;
    let range = body_range(&document).ok_or(RedlineError::RenderedUnreadable)?;
    let new_chunks =
        body_chunks(&document[range.clone()]).ok_or(RedlineError::RenderedUnreadable)?;
    let new_paragraphs: Vec<String> = new_chunks
        .iter()
        .filter_map(Chunk::paragraph_text)
        .collect();

    let ops = diff(&previous_texts, &new_paragraphs);

    let (redlined_body, counts) = redline_body(&new_chunks, &previous_paragraphs, &ops, author);

    if counts.total() == 0 {
        return Ok(Redline {
            bytes: new_docx.to_vec(),
            counts,
        });
    }

    let redlined_document = format!(
        "{}{redlined_body}{}",
        &document[..range.start],
        &document[range.end..]
    );
    Ok(Redline {
        bytes: rewrite_package(new_docx, &redlined_document)?,
        counts,
    })
}

/// Emit the redlined body: the new render's paragraphs with their
/// revisions applied, deletions struck in place ahead of the next
/// surviving paragraph. Returns the spliced body XML and the revision
/// counts.
fn redline_body(
    new_chunks: &[Chunk],
    previous_paragraphs: &[String],
    ops: &[Op],
    author: &str,
) -> (String, RedlineCounts) {
    let mut body = String::with_capacity(4096);
    let mut counts = RedlineCounts {
        insertions: 0,
        deletions: 0,
        replacements: 0,
    };
    let mut next_id = 1u32;
    let mut op_index = 0usize;
    for chunk in new_chunks {
        let raw = match chunk {
            Chunk::Verbatim(raw) => {
                body.push_str(raw);
                continue;
            }
            Chunk::Paragraph { raw, .. } => raw,
        };
        // Deletions emit before the next surviving paragraph, so Word
        // strikes them in place.
        while matches!(ops.get(op_index), Some(Op::Delete(_))) {
            let Some(Op::Delete(old_index)) = ops.get(op_index) else {
                unreachable!("invariant: the loop condition matched a Delete")
            };
            body.push_str(&deleted_paragraph(
                &previous_paragraphs[*old_index],
                author,
                &mut next_id,
            ));
            counts.deletions += 1;
            op_index += 1;
        }
        match ops.get(op_index) {
            Some(Op::Keep) => {
                body.push_str(raw);
            }
            Some(Op::Insert(_)) => {
                body.push_str(&inserted_paragraph(raw, author, &mut next_id));
                counts.insertions += 1;
            }
            Some(Op::Replace(old_index, _)) => {
                body.push_str(&replaced_paragraph(
                    raw,
                    &previous_paragraphs[*old_index],
                    author,
                    &mut next_id,
                ));
                counts.replacements += 1;
            }
            Some(Op::Delete(_)) | None => {
                unreachable!("invariant: paragraph ops align with paragraph chunks in order")
            }
        }
        op_index += 1;
    }
    for op in &ops[op_index..] {
        let Op::Delete(old_index) = op else {
            unreachable!("invariant: only deletions remain after the last paragraph")
        };
        body.push_str(&deleted_paragraph(
            &previous_paragraphs[*old_index],
            author,
            &mut next_id,
        ));
        counts.deletions += 1;
    }
    (body, counts)
}
/// One element of a package body, in document order: a paragraph (the only
/// thing the diff reads), or everything else — tables chiefly — passed
/// through verbatim, exactly as the import baseline treats them.
enum Chunk {
    Paragraph { raw: String },
    Verbatim(String),
}

impl Chunk {
    fn paragraph_raw(&self) -> Option<&str> {
        match self {
            Chunk::Paragraph { raw } => Some(raw),
            Chunk::Verbatim(_) => None,
        }
    }

    fn paragraph_text(&self) -> Option<String> {
        self.paragraph_raw().map(paragraph_text)
    }
}

/// Split a package body into its top-level elements. `None` when an element
/// never closes — a body the renderer cannot have written.
fn body_chunks(body: &str) -> Option<Vec<Chunk>> {
    let mut chunks = Vec::new();
    let mut rest = body;
    let mut pending = String::new();
    while let Some(position) = rest.find('<') {
        let element = &rest[position..];
        let end_tag = if element.starts_with("<w:p>") || element.starts_with("<w:p ") {
            "</w:p>"
        } else if element.starts_with("<w:tbl>") {
            "</w:tbl>"
        } else {
            pending.push_str(&rest[..=position]);
            rest = &rest[position + 1..];
            continue;
        };
        let end = element.find(end_tag)? + end_tag.len();
        let raw = element[..end].to_string();
        if !pending.is_empty() {
            chunks.push(Chunk::Verbatim(std::mem::take(&mut pending)));
        }
        if end_tag == "</w:p>" {
            chunks.push(Chunk::Paragraph { raw });
        } else {
            chunks.push(Chunk::Verbatim(raw));
        }
        rest = &element[end..];
    }
    pending.push_str(rest);
    if !pending.is_empty() {
        chunks.push(Chunk::Verbatim(pending));
    }
    Some(chunks)
}

/// The byte range of the body content inside a full `word/document.xml` —
/// the same slice [`extract_body_xml`] reads, kept as offsets so the
/// redlined body can be spliced back between the untouched prologue and
/// section properties.
fn body_range(document: &str) -> Option<std::ops::Range<usize>> {
    let start = document.find("<w:body>")? + "<w:body>".len();
    let end = document[start..]
        .find("<w:sectPr")
        .map_or(document.len(), |end| start + end);
    Some(start..end)
}

fn read_document_xml(docx: &[u8]) -> Option<String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(docx)).ok()?;
    let mut document = String::new();
    archive
        .by_name("word/document.xml")
        .ok()?
        .read_to_string(&mut document)
        .ok()?;
    Some(document)
}

/// Copy the package, replacing `word/document.xml` and carrying every other
/// entry byte for byte — provenance, styles, headers, the letterhead mark.
fn rewrite_package(new_docx: &[u8], document: &str) -> Result<Vec<u8>, RedlineError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(new_docx))?;
    let mut output = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let name = entry.name().unwrap_or_default().to_string();
        output.start_file(name.as_str(), zip::write::SimpleFileOptions::default())?;
        if name == "word/document.xml" {
            output.write_all(document.as_bytes())?;
        } else {
            std::io::copy(&mut entry, &mut output)?;
        }
    }
    Ok(output.finish()?.into_inner())
}

/// The alignment of the two paragraph sequences. A delete paired with an
/// insert merges into a replacement: one rewritten paragraph, not a
/// separate struck and added one.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    Keep,
    Delete(usize),
    Insert(usize),
    Replace(usize, usize),
}

/// Alignment costs. Keeping is free; rewriting a paragraph in place with a
/// similar one (drafts rewrap prose) costs less than striking and adding;
/// pairing unrelated paragraphs costs more, so unrelated edits stay struck
/// and added — what a lawyer expects to review.
const KEEP_COST: u64 = 0;
const EDIT_COST: u64 = 100;
const REPLACE_SIMILAR_COST: u64 = 100;
const REPLACE_UNRELATED_COST: u64 = 2 * EDIT_COST + 1;

/// Dice coefficient over character bigrams, ×100. Two characters or fewer
/// has no bigrams — equality is the whole signal.
fn similarity(a: &str, b: &str) -> u64 {
    if a == b {
        return 100;
    }
    let bigrams = |text: &str| -> Vec<(char, char)> {
        let chars: Vec<char> = text.chars().collect();
        chars.windows(2).map(|pair| (pair[0], pair[1])).collect()
    };
    let (mut left, mut right) = (bigrams(a), bigrams(b));
    if left.is_empty() || right.is_empty() {
        return 0;
    }
    left.sort_unstable();
    right.sort_unstable();
    let mut shared = 0usize;
    let (mut i, mut j) = (0, 0);
    while i < left.len() && j < right.len() {
        match left[i].cmp(&right[j]) {
            std::cmp::Ordering::Equal => {
                shared += 1;
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    (2 * shared as u64 * 100) / (left.len() + right.len()) as u64
}

fn diff(old: &[String], new: &[String]) -> Vec<Op> {
    // Weighted alignment DP over whole paragraphs: each cell holds the
    // cheapest cost of aligning the first `i` previous with the first `j`
    // current paragraphs. Document-sized inputs, so the O(n·m) table is
    // not a cost worth optimizing away.
    let infinite = (old.len() + new.len()) as u64 * REPLACE_UNRELATED_COST;
    let mut costs = vec![vec![infinite; new.len() + 1]; old.len() + 1];
    costs[0][0] = KEEP_COST;
    for (index, cost) in costs.iter_mut().enumerate().skip(1) {
        cost[0] = index as u64 * EDIT_COST;
    }
    for (index, cost) in costs[0].iter_mut().enumerate().skip(1) {
        *cost = index as u64 * EDIT_COST;
    }
    for i in 1..=old.len() {
        for j in 1..=new.len() {
            let diagonal = if old[i - 1] == new[j - 1] {
                KEEP_COST
            } else if similarity(&old[i - 1], &new[j - 1]) >= 40 {
                REPLACE_SIMILAR_COST
            } else {
                REPLACE_UNRELATED_COST
            };
            costs[i][j] = (costs[i - 1][j - 1])
                .saturating_add(diagonal)
                .min(costs[i - 1][j].saturating_add(EDIT_COST))
                .min(costs[i][j - 1].saturating_add(EDIT_COST));
        }
    }

    // Backtrack, emitting the ops in document order. On equal costs the
    // diagonal wins (a rewrite reads better than a strike-and-add), then
    // deletion, then insertion.
    let mut ops = Vec::new();
    let (mut i, mut j) = (old.len(), new.len());
    while i > 0 || j > 0 {
        let (cost, diagonal) = if i > 0 && j > 0 {
            let diagonal = if old[i - 1] == new[j - 1] {
                KEEP_COST
            } else if similarity(&old[i - 1], &new[j - 1]) >= 40 {
                REPLACE_SIMILAR_COST
            } else {
                REPLACE_UNRELATED_COST
            };
            (costs[i - 1][j - 1].saturating_add(diagonal), true)
        } else {
            (infinite, false)
        };
        if diagonal && cost == costs[i][j] {
            if old[i - 1] == new[j - 1] {
                ops.push(Op::Keep);
            } else {
                ops.push(Op::Replace(i - 1, j - 1));
            }
            i -= 1;
            j -= 1;
        } else if i > 0 && costs[i - 1][j].saturating_add(EDIT_COST) == costs[i][j] {
            ops.push(Op::Delete(i - 1));
            i -= 1;
        } else {
            ops.push(Op::Insert(j - 1));
            j -= 1;
        }
    }
    ops.reverse();
    ops
}

/// Where a paragraph's runs start: past its properties block, or right
/// after the opening tag when it has none.
fn run_insertion_point(paragraph: &str) -> usize {
    if let Some(end) = paragraph.find("</w:pPr>") {
        return end + "</w:pPr>".len();
    }
    let open_end = paragraph
        .find('>')
        .expect("invariant: a parsed paragraph opens with <w:p");
    open_end + 1
}

/// The runs region of a paragraph: everything between its properties and
/// its closing tag.
fn runs_region(paragraph: &str) -> &str {
    &paragraph[run_insertion_point(paragraph)..paragraph.len() - "</w:p>".len()]
}

/// A whole paragraph carried into the redline struck through: every run
/// becomes `w:delText` inside one `w:del`, and the paragraph mark is marked
/// deleted, so Word's accept removes the paragraph entirely.
fn deleted_paragraph(old: &str, author: &str, next_id: &mut u32) -> String {
    let open_end = old
        .find('>')
        .expect("invariant: a parsed paragraph opens with <w:p");
    let deleted_runs = deleted_runs(old, author, next_id);
    let marked = with_paragraph_mark(old, &revision_tag("w:del", author, next_id), open_end);
    let start = run_insertion_point(&marked);
    let end = marked.len() - "</w:p>".len();
    format!("{}{}{}", &marked[..start], deleted_runs, &marked[end..])
}

/// The previous text of a rewritten paragraph, as struck-through runs.
fn deleted_runs(old: &str, author: &str, next_id: &mut u32) -> String {
    let runs = runs_region(old);
    let del_id = *next_id;
    *next_id += 1;
    let struck = runs
        .replace("<w:t>", "<w:delText>")
        .replace("<w:t ", "<w:delText ")
        .replace("</w:t>", "</w:delText>");
    format!(
        r#"<w:del w:id="{del_id}" w:author="{}">{struck}</w:del>"#,
        xml(author)
    )
}

/// A paragraph the previous version did not have: its runs wrapped in one
/// `w:ins`, its paragraph mark marked inserted.
fn inserted_paragraph(paragraph: &str, author: &str, next_id: &mut u32) -> String {
    let runs = runs_region(paragraph);
    let open_end = paragraph
        .find('>')
        .expect("invariant: a parsed paragraph opens with <w:p");
    let marked = with_paragraph_mark(paragraph, &revision_tag("w:ins", author, next_id), open_end);
    let start = run_insertion_point(&marked);
    let end = marked.len() - "</w:p>".len();
    if runs.trim().is_empty() {
        return marked;
    }
    let ins_id = *next_id;
    *next_id += 1;
    format!(
        "{}<w:ins w:id=\"{ins_id}\" w:author=\"{}\">{}</w:ins>{}",
        &marked[..start],
        xml(author),
        runs,
        &marked[end..]
    )
}

/// A paragraph rewritten in place: the previous text struck through, then
/// the current text inserted, in one paragraph.
fn replaced_paragraph(current: &str, old: &str, author: &str, next_id: &mut u32) -> String {
    let deleted = deleted_runs(old, author, next_id);
    let inserted = inserted_paragraph(current, author, next_id);
    let start = run_insertion_point(&inserted);
    let end = inserted.len() - "</w:p>".len();
    format!(
        "{}{deleted}{}{}",
        &inserted[..start],
        &inserted[start..end],
        &inserted[end..]
    )
}

/// An empty revision tag marking a paragraph mark, with a fresh `w:id`.
fn revision_tag(tag: &str, author: &str, next_id: &mut u32) -> String {
    let id = *next_id;
    *next_id += 1;
    format!(r#"<{tag} w:id="{id}" w:author="{}"/>"#, xml(author))
}

/// Mark the paragraph mark itself: a `<w:rPr><w:ins|del …/></w:rPr>` inside
/// the paragraph's properties block, added or extended in place.
fn with_paragraph_mark(paragraph: &str, revision: &str, open_end: usize) -> String {
    let Some(end) = paragraph.find("</w:pPr>") else {
        return format!(
            "{}<w:pPr><w:rPr>{revision}</w:rPr></w:pPr>{}",
            &paragraph[..=open_end],
            &paragraph[open_end + 1..]
        );
    };
    let head = &paragraph[..end];
    let tail = &paragraph[end..];
    match head.rfind("<w:rPr>") {
        Some(rpr) => {
            let at = rpr + "<w:rPr>".len();
            format!("{}{revision}{}{tail}", &head[..at], &head[at..])
        }
        None => format!("{head}<w:rPr>{revision}</w:rPr>{tail}"),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::{against, RedlineError};
    use crate::import::{plan_import, ImportError};
    use crate::provenance::{template_hash, Provenance};
    use crate::render::{render_notation, RenderOptions};

    const V1: &str = concat!(
        "# Service Agreement\n",
        "\n",
        "This agreement is between the client and the Firm.\n",
        "\n",
        "Fees are due on the first of each month.\n",
        "\n",
        "Keep receipts for every expense.\n",
    );

    /// Versus V1: one paragraph rewrapped, one deleted, one inserted — the
    /// seeded set every assertion below counts.
    const V2: &str = concat!(
        "# Service Agreement\n",
        "\n",
        "This agreement is between the client and the Firm.\n",
        "\n",
        "Fees are due on the fifteenth of each month.\n",
        "\n",
        "Interest accrues at the statutory rate.\n",
    );

    const AUTHOR: &str = "Nicholas Shook";

    fn render(markdown: &str) -> Vec<u8> {
        render_notation(
            markdown,
            None,
            &RenderOptions {
                provenance: Some(Provenance {
                    template_path: "notation_templates/engagement/service.md",
                    template_hash: &template_hash(markdown.as_bytes()),
                }),
                ..RenderOptions::default()
            },
        )
        .unwrap()
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

    /// Copy a package, replacing `word/document.xml` the way Word saves one.
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

    /// The paragraph containing `needle`, for assertions that read one
    /// paragraph's revision structure.
    fn paragraph_containing(document: &str, needle: &str) -> String {
        let at = document
            .find(needle)
            .unwrap_or_else(|| panic!("document contains {needle:?}"));
        let start = document[..at]
            .rfind("<w:p>")
            .unwrap_or_else(|| panic!("{needle:?} sits outside a paragraph"));
        let end = start + document[start..].find("</w:p>").unwrap();
        document[start..end].to_string()
    }

    #[test]
    fn seeded_edits_become_exactly_the_seeded_revisions() {
        let redline = against(&render(V2), &render(V1), AUTHOR).unwrap();
        assert_eq!(
            redline.counts,
            super::RedlineCounts {
                insertions: 1,
                deletions: 1,
                replacements: 1,
            }
        );
        let document = document_xml(&redline.bytes);
        // Every revision carries run-level tags plus paragraph-mark tags:
        // the rewritten and inserted paragraphs each wrap their runs in one
        // `w:ins` and mark their paragraph marks inserted; the rewritten and
        // deleted paragraphs each strike their runs in one `w:del`, and only
        // the deleted paragraph's mark is marked deleted — the rewritten
        // paragraph survives, so its mark is marked inserted.
        assert_eq!(
            document.matches("<w:ins ").count(),
            4,
            "insertions: {document}"
        );
        assert_eq!(
            document.matches("<w:del ").count(),
            3,
            "deletions: {document}"
        );

        let rewritten = paragraph_containing(&document, "fifteenth");
        assert!(
            rewritten.contains("<w:delText"),
            "struck old text: {rewritten}"
        );
        assert!(
            rewritten.contains("Fees are due on the first"),
            "{rewritten}"
        );
        assert!(rewritten.contains("<w:ins "), "{rewritten}");

        let inserted = paragraph_containing(&document, "statutory rate");
        assert!(inserted.contains("<w:ins "), "{inserted}");
        assert!(!inserted.contains("<w:del "), "{inserted}");

        let deleted = paragraph_containing(&document, "Keep receipts");
        assert!(deleted.contains("<w:delText"), "{deleted}");
        assert!(!deleted.contains("<w:ins "), "{deleted}");

        let untouched = paragraph_containing(&document, "between the client");
        assert!(!untouched.contains("<w:ins "), "{untouched}");
        assert!(!untouched.contains("<w:del "), "{untouched}");
    }

    #[test]
    fn every_revision_is_authored_and_the_author_is_escaped() {
        let redline = against(&render(V2), &render(V1), "Shook & Associates <PLLC>").unwrap();
        let document = document_xml(&redline.bytes);
        assert!(document.contains(r#"w:author="Shook &amp; Associates &lt;PLLC&gt;""#));
        assert_eq!(
            document
                .matches(r#"w:author="Shook &amp; Associates &lt;PLLC&gt;""#)
                .count(),
            document.matches("<w:ins ").count() + document.matches("<w:del ").count(),
        );
    }

    #[test]
    fn paragraph_marks_are_marked_with_their_revision() {
        let redline = against(&render(V2), &render(V1), AUTHOR).unwrap();
        let document = document_xml(&redline.bytes);
        let inserted = paragraph_containing(&document, "statutory rate");
        assert!(
            inserted.contains(r"<w:rPr><w:ins "),
            "inserted paragraph mark: {inserted}"
        );
        let deleted = paragraph_containing(&document, "Keep receipts");
        assert!(
            deleted.contains(r"<w:rPr><w:del "),
            "deleted paragraph mark: {deleted}"
        );
    }

    #[test]
    fn deleted_text_keeps_the_run_formatting_of_the_previous_version() {
        let with_emphasis = concat!("# Service Agreement\n", "\n", "Keep *every* receipt.\n",);
        let without_emphasis = concat!("# Service Agreement\n", "\n", "Keep the receipts.\n",);
        let redline = against(&render(without_emphasis), &render(with_emphasis), AUTHOR).unwrap();
        let document = document_xml(&redline.bytes);
        let deleted = paragraph_containing(&document, "every");
        // The struck runs carry the italic run properties they had in the
        // previous version, not plain deleted text.
        assert!(deleted.contains(r"<w:delText"), "{deleted}");
        assert!(deleted.contains(r"<w:rPr><w:i/></w:rPr>"), "{deleted}");
    }

    #[test]
    fn identical_documents_redline_to_the_unchanged_render() {
        let rendered = render(V1);
        let redline = against(&rendered, &render(V1), AUTHOR).unwrap();
        assert_eq!(redline.counts.total(), 0);
        assert_eq!(redline.bytes, rendered);
    }

    #[test]
    fn the_redline_is_deterministic() {
        let first = against(&render(V2), &render(V1), AUTHOR).unwrap();
        let second = against(&render(V2), &render(V1), AUTHOR).unwrap();
        assert_eq!(first.bytes, second.bytes);
    }

    #[test]
    fn a_redlined_package_still_reads_back_as_a_valid_package() {
        let redline = against(&render(V2), &render(V1), AUTHOR).unwrap();
        let document = document_xml(&redline.bytes);
        assert!(document.starts_with("<?xml version=\"1.0\""));
        assert!(document.contains("<w:sectPr"));
    }

    #[test]
    fn the_import_verb_refuses_a_redlined_document() {
        let redline = against(&render(V2), &render(V1), AUTHOR).unwrap();
        let error = plan_import(V2, V2.as_bytes(), &redline.bytes).unwrap_err();
        assert!(matches!(error, ImportError::TrackedChanges));
        assert!(error.to_string().contains("tracked changes"));
    }

    #[test]
    fn a_previous_package_navigator_did_not_render_is_refused() {
        let bare = render_notation(V1, None, &RenderOptions::default()).unwrap();
        let error = against(&render(V2), &bare, AUTHOR).unwrap_err();
        assert!(matches!(error, RedlineError::PreviousWithoutProvenance));
    }

    #[test]
    fn a_previous_package_still_carrying_tracked_changes_is_refused() {
        let previous = render(V1);
        let mut document = document_xml(&previous);
        document = document.replace(
            "<w:r><w:t xml:space=\"preserve\">Keep receipts",
            "<w:ins w:id=\"99\" w:author=\"N\"><w:r><w:t xml:space=\"preserve\">Keep receipts",
        );
        let edited = rebuild(&previous, &document);
        let error = against(&render(V2), &edited, AUTHOR).unwrap_err();
        assert!(matches!(error, RedlineError::PreviousTrackedChanges));
    }

    #[test]
    fn an_unreadable_previous_package_is_refused() {
        let error = against(&render(V2), b"not a package", AUTHOR).unwrap_err();
        assert!(matches!(error, RedlineError::PreviousWithoutProvenance));
    }
}
