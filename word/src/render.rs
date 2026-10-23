//! Deterministic Office Open XML rendering for repository-authored notations.

use std::fmt::Write as _;
use std::io::{Cursor, Write as _};

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use zip::write::SimpleFileOptions;

/// Firm identity carried by a letter-frame Word document.
pub struct RenderLetterhead<'a> {
    pub name: &'a str,
    pub phone: &'a str,
    pub email: &'a str,
    pub web: &'a str,
    pub logo_png: &'a [u8],
}

/// Stable metadata recorded in a generated Word package.
#[derive(Default)]
pub struct RenderOptions<'a> {
    pub source_revision: Option<&'a str>,
    /// Court-paper geometry for a pleading; `None` for every other frame.
    pub pleading: Option<PleadingFrame>,
}

/// Court-paper geometry for a pleading's Word render, in Word's own units:
/// twips (twentieths of a point) for lengths and half-points for type size.
///
/// `pdf::pleading::Variant::word_frame` builds it from the same calibration
/// the PDF renders, so the `.docx` and the PDF frame a pleading alike. With
/// a rail, every body line sits on an exact `line_pitch_twips` baseline and
/// the header carries a numbered rail of `rail_lines` lines on that same
/// pitch, starting at the top margin, between a double rule on the left of
/// the text column and a single rule on its right. Without one, body text
/// is double-spaced and nothing is drawn in the margin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PleadingFrame {
    pub face: &'static str,
    pub type_size_half_points: u32,
    pub top_margin_twips: u32,
    pub bottom_margin_twips: u32,
    pub left_margin_twips: u32,
    pub right_margin_twips: u32,
    pub line_pitch_twips: u32,
    /// Lines in the numbered rail, or `None` for a calibration without one.
    pub rail_lines: Option<u32>,
}

/// Where the rail's numbers sit: 0.75in from the page's left edge, in a
/// 0.35in column, the same placement the PDF rail uses.
const RAIL_X_TWIPS: u32 = 1080;
const RAIL_WIDTH_TWIPS: u32 = 504;
/// Rail numbers are set at 12pt, as in the PDF.
const RAIL_TYPE_HALF_POINTS: u32 = 24;

/// Where a Signer Box sits in a paragraph's text. The Markdown walker
/// replaces the `<!-- signer-box -->` marker with this private-use
/// character so the text stays one string, and `runs_xml` turns each one
/// into an underlined run the signer completes by hand.
const SIGNER_BOX_RUN: char = '\u{E000}';
/// The rule a Signer Box draws: underlined no-break spaces, about an inch
/// and a half at body size. No-break spaces rather than spaces because Word
/// does not underline a space that ends a line.
const SIGNER_BOX_RULE: &str = "\u{A0}\u{A0}\u{A0}\u{A0}\u{A0}\u{A0}\u{A0}\u{A0}\u{A0}\u{A0}\u{A0}\u{A0}\u{A0}\u{A0}\u{A0}\u{A0}\u{A0}\u{A0}\u{A0}\u{A0}";

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("write Word package: {0}")]
    Io(#[from] std::io::Error),
    #[error("write Word package: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("pleading paper: {0}")]
    Paper(#[from] crate::paper::PaperError),
}

#[derive(Default)]
struct Paragraph {
    style: Option<&'static str>,
    text: String,
    /// The `<!-- keep -->` group this paragraph belongs to. Paragraphs
    /// sharing a group are kept on one page: each is told to stay with the
    /// next, so Word moves the whole group together when it fits and
    /// breaks it where it must when it does not.
    group: Option<usize>,
}

/// A GFM pipe table: the header row first, every row padded to the header's
/// width. Cell text is plain, as in a paragraph.
struct Table {
    rows: Vec<Vec<String>>,
}

enum Block {
    Paragraph(Paragraph),
    Table(Table),
}

/// Render notation Markdown into a deterministic `.docx` package.
#[allow(clippy::too_many_lines)]
pub fn render_notation(
    markdown: &str,
    letterhead: Option<&RenderLetterhead<'_>>,
    options: &RenderOptions<'_>,
) -> Result<Vec<u8>, RenderError> {
    let pleading = options.pleading.as_ref();
    // A pleading's caption and title come out of the body once, so the
    // first page, the footer, and the paragraphs agree with the PDF.
    let paper = pleading
        .map(|_| crate::paper::parse(markdown))
        .transpose()?;
    let body = paper.as_ref().map_or(markdown, |paper| paper.body.as_str());
    let blocks = blocks(body);
    let caption = pleading
        .zip(paper.as_ref().and_then(|paper| paper.caption.as_ref()))
        .map(|(frame, caption)| caption_xml(frame, caption))
        .unwrap_or_default();
    let document = document_xml(&blocks, letterhead.is_some(), pleading, &caption);
    let core = core_xml(options.source_revision);
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let file_options = SimpleFileOptions::default();

    write_entry(
        &mut archive,
        "[Content_Types].xml",
        content_types(letterhead.is_some()).as_bytes(),
        file_options,
    )?;
    write_entry(
        &mut archive,
        "_rels/.rels",
        ROOT_RELS.as_bytes(),
        file_options,
    )?;
    write_entry(
        &mut archive,
        "docProps/core.xml",
        core.as_bytes(),
        file_options,
    )?;
    write_entry(
        &mut archive,
        "docProps/app.xml",
        APP_XML.as_bytes(),
        file_options,
    )?;
    write_entry(
        &mut archive,
        "word/document.xml",
        document.as_bytes(),
        file_options,
    )?;
    write_entry(
        &mut archive,
        "word/styles.xml",
        pleading
            .map_or_else(|| STYLES_XML.to_string(), pleading_styles_xml)
            .as_bytes(),
        file_options,
    )?;
    write_entry(
        &mut archive,
        "word/_rels/document.xml.rels",
        document_rels(letterhead.is_some()).as_bytes(),
        file_options,
    )?;
    write_entry(
        &mut archive,
        "word/header1.xml",
        running_header_xml(pleading).as_bytes(),
        file_options,
    )?;
    let footer = match (pleading, &paper) {
        (Some(_), Some(paper)) => pleading_footer_xml(paper.footer()),
        _ => footer_xml(),
    };
    write_entry(
        &mut archive,
        "word/footer1.xml",
        footer.as_bytes(),
        file_options,
    )?;
    write_entry(
        &mut archive,
        "word/footer2.xml",
        footer.as_bytes(),
        file_options,
    )?;
    if let Some(letterhead) = letterhead {
        write_entry(
            &mut archive,
            "word/header2.xml",
            first_header_xml(letterhead).as_bytes(),
            file_options,
        )?;
        write_entry(
            &mut archive,
            "word/_rels/header2.xml.rels",
            HEADER_RELS.as_bytes(),
            file_options,
        )?;
        write_entry(
            &mut archive,
            "word/media/logo-neon-law.png",
            letterhead.logo_png,
            file_options,
        )?;
    }
    Ok(archive.finish()?.into_inner())
}

fn write_entry(
    archive: &mut zip::ZipWriter<Cursor<Vec<u8>>>,
    name: &str,
    bytes: &[u8],
    options: SimpleFileOptions,
) -> Result<(), RenderError> {
    archive.start_file(name, options)?;
    archive.write_all(bytes)?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn blocks(markdown: &str) -> Vec<Block> {
    let mut result = Vec::new();
    let mut current: Option<Paragraph> = None;
    let mut table: Option<Table> = None;
    let mut row: Vec<String> = Vec::new();
    let mut cell: Option<String> = None;
    let mut list_depth = 0usize;
    let mut quote_depth = 0usize;
    let mut group: Option<usize> = None;
    let mut groups = 0usize;
    for event in Parser::new_ext(
        markdown,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
    ) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                current = Some(Paragraph {
                    style: Some(heading_style(level)),
                    ..Paragraph::default()
                });
            }
            Event::Start(Tag::Paragraph) => current = Some(Paragraph::default()),
            Event::Start(Tag::Item) => {
                let mut paragraph = Paragraph::default();
                paragraph
                    .text
                    .push_str(if list_depth > 1 { "    • " } else { "• " });
                current = Some(paragraph);
            }
            Event::Start(Tag::List(_)) => list_depth += 1,
            Event::Start(Tag::BlockQuote(_)) => quote_depth += 1,
            Event::Start(Tag::Table(_)) => table = Some(Table { rows: Vec::new() }),
            Event::Start(Tag::TableHead | Tag::TableRow) => row = Vec::new(),
            Event::Start(Tag::TableCell) => cell = Some(String::new()),
            Event::End(TagEnd::TableCell) => row.extend(cell.take()),
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                if let Some(table) = table.as_mut() {
                    table.rows.push(std::mem::take(&mut row));
                }
            }
            Event::End(TagEnd::Table) => {
                if let Some(mut finished) = table.take() {
                    let width = finished.rows.first().map_or(0, Vec::len);
                    for row in &mut finished.rows {
                        row.resize(width, String::new());
                    }
                    result.push(Block::Table(finished));
                }
            }
            Event::Text(text) | Event::Code(text) if cell.is_some() => {
                cell.get_or_insert_with(String::new).push_str(&text);
            }
            Event::SoftBreak if cell.is_some() => {
                cell.get_or_insert_with(String::new).push(' ');
            }
            Event::HardBreak if cell.is_some() => {
                cell.get_or_insert_with(String::new).push('\n');
            }
            Event::Text(text) | Event::Code(text) => {
                current
                    .get_or_insert_with(Paragraph::default)
                    .text
                    .push_str(&text);
            }
            // A soft break is a plain source-line wrap (S102 packs prose to
            // 120 columns, so most paragraphs carry several); CommonMark
            // renders it as a space, not a line break. Only a real hard
            // break — a trailing `\` or two trailing spaces, and not at the
            // end of the block — becomes a `<w:br/>` in `document_xml`.
            Event::SoftBreak => {
                current
                    .get_or_insert_with(Paragraph::default)
                    .text
                    .push(' ');
            }
            Event::HardBreak => {
                current
                    .get_or_insert_with(Paragraph::default)
                    .text
                    .push('\n');
            }
            Event::Rule => result.push(Block::Paragraph(Paragraph {
                text: "────────────────────────".into(),
                group,
                ..Paragraph::default()
            })),
            Event::Html(html) | Event::InlineHtml(html)
                if crate::paper::is_html_line_break(&html) =>
            {
                if let Some(cell) = cell.as_mut() {
                    cell.push('\n');
                } else {
                    current
                        .get_or_insert_with(Paragraph::default)
                        .text
                        .push('\n');
                }
            }
            // A Signer Box the fill wrote in place of an unanswered
            // placeholder: an empty rule the signer completes by hand.
            Event::Html(html) | Event::InlineHtml(html) if crate::paper::is_signer_box(&html) => {
                if let Some(cell) = cell.as_mut() {
                    cell.push(SIGNER_BOX_RUN);
                } else {
                    current
                        .get_or_insert_with(Paragraph::default)
                        .text
                        .push(SIGNER_BOX_RUN);
                }
            }
            Event::Html(html) | Event::InlineHtml(html) => match crate::paper::keep_marker(&html) {
                Some(crate::paper::Keep::Open) if quote_depth == 0 && list_depth == 0 => {
                    groups += 1;
                    group = Some(groups);
                }
                Some(crate::paper::Keep::Close) => group = None,
                _ => {}
            },
            Event::End(TagEnd::Heading(_) | TagEnd::Paragraph | TagEnd::Item) => {
                if let Some(mut paragraph) = current.take() {
                    if quote_depth > 0 {
                        paragraph.text.insert_str(0, "    ");
                    }
                    strip_trailing_escaped_break(&mut paragraph.text);
                    if !paragraph.text.is_empty() {
                        paragraph.group = group;
                        result.push(Block::Paragraph(paragraph));
                    }
                }
            }
            Event::End(TagEnd::List(_)) => list_depth = list_depth.saturating_sub(1),
            Event::End(TagEnd::BlockQuote(_)) => quote_depth = quote_depth.saturating_sub(1),
            _ => {}
        }
    }
    if let Some(mut paragraph) = current.filter(|paragraph| !paragraph.text.is_empty()) {
        paragraph.group = group;
        result.push(Block::Paragraph(paragraph));
    }
    result
}

/// A trailing `\` written as `CommonMark` hard-break syntax loses that meaning
/// when it lands on a block's last line — pulldown-cmark then passes it
/// through as an ordinary character instead of emitting `Event::HardBreak`.
/// Templates wrap every line (S102 packs prose to 120 columns), so the habit
/// of ending a source line with `\` survives onto a paragraph's final line
/// too; drop it rather than print a stray backslash nobody meant to keep.
fn strip_trailing_escaped_break(text: &mut String) {
    if text.ends_with('\\') && !text.ends_with("\\\\") {
        text.pop();
    }
}

fn heading_style(level: HeadingLevel) -> &'static str {
    match level {
        HeadingLevel::H1 => "Title",
        HeadingLevel::H2 => "Heading1",
        HeadingLevel::H3 => "Heading2",
        HeadingLevel::H4 => "Heading3",
        HeadingLevel::H5 => "Heading4",
        HeadingLevel::H6 => "Heading5",
    }
}

fn document_xml(
    blocks: &[Block],
    has_letterhead: bool,
    pleading: Option<&PleadingFrame>,
    caption: &str,
) -> String {
    let text_width = pleading.map_or(DEFAULT_TEXT_WIDTH_TWIPS, |frame| {
        PAGE_WIDTH_TWIPS - frame.left_margin_twips - frame.right_margin_twips
    });
    let mut body = String::from(caption);
    for (index, block) in blocks.iter().enumerate() {
        let paragraph = match block {
            Block::Paragraph(paragraph) => paragraph,
            Block::Table(table) => {
                body.push_str(&table_xml(table, text_width));
                continue;
            }
        };
        body.push_str("<w:p>");
        let keep_next = paragraph.group.is_some()
            && matches!(
                blocks.get(index + 1),
                Some(Block::Paragraph(next)) if next.group == paragraph.group
            );
        let keep_lines = paragraph.group.is_some();
        if paragraph.style.is_some() || keep_lines {
            body.push_str("<w:pPr>");
            if let Some(style) = paragraph.style {
                let _ = write!(body, "<w:pStyle w:val=\"{style}\"/>");
            }
            if keep_next {
                body.push_str("<w:keepNext/>");
            }
            if keep_lines {
                body.push_str("<w:keepLines/>");
            }
            body.push_str("</w:pPr>");
        }
        body.push_str(&runs_xml(&paragraph.text, false));
        body.push_str("</w:p>");
    }
    let first_header = if has_letterhead {
        "<w:headerReference w:type=\"first\" r:id=\"rId3\"/>"
    } else {
        ""
    };
    let title_page = if has_letterhead { "<w:titlePg/>" } else { "" };
    let page_margins =
        pleading.map_or_else(|| DEFAULT_PAGE_MARGINS.to_string(), pleading_page_margins);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>{body}<w:sectPr><w:headerReference w:type="default" r:id="rId2"/>{first_header}<w:footerReference w:type="default" r:id="rId4"/><w:footerReference w:type="first" r:id="rId5"/><w:pgSz w:w="12240" w:h="15840"/>{page_margins}{title_page}</w:sectPr></w:body></w:document>"#
    )
}

/// A paragraph's text as runs, a `<w:br/>` for each hard break and an
/// underlined rule for each Signer Box.
fn runs_xml(text: &str, bold: bool) -> String {
    let weight = if bold { "<w:rPr><w:b/></w:rPr>" } else { "" };
    let mut runs = String::new();
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            runs.push_str("<w:r><w:br/></w:r>");
        }
        for (piece, text) in line.split(SIGNER_BOX_RUN).enumerate() {
            if piece > 0 {
                let _ = write!(
                    runs,
                    "<w:r><w:rPr><w:u w:val=\"single\"/></w:rPr><w:t xml:space=\"preserve\">{SIGNER_BOX_RULE}</w:t></w:r>"
                );
            }
            if text.is_empty() && piece > 0 {
                continue;
            }
            let _ = write!(
                runs,
                "<w:r>{weight}<w:t xml:space=\"preserve\">{}</w:t></w:r>",
                xml(text)
            );
        }
    }
    runs
}

/// A bordered table of equal columns spanning the text column. The header
/// row is bold and repeats at the top of each page the table spans; no row
/// splits across pages. An empty paragraph follows, as Word requires after a
/// table, and it keeps adjacent tables apart.
fn table_xml(table: &Table, text_width: u32) -> String {
    let columns = table.rows.first().map_or(0, Vec::len);
    let Ok(count) = u32::try_from(columns) else {
        return String::new();
    };
    if count == 0 {
        return String::new();
    }
    let width = text_width / count;
    let mut borders = String::new();
    for side in ["top", "left", "bottom", "right", "insideH", "insideV"] {
        let _ = write!(
            borders,
            r#"<w:{side} w:val="single" w:sz="4" w:space="0" w:color="auto"/>"#
        );
    }
    let mut out = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="{}" w:type="dxa"/><w:tblBorders>{borders}</w:tblBorders><w:tblLayout w:type="fixed"/></w:tblPr><w:tblGrid>"#,
        width * count
    );
    for _ in 0..count {
        let _ = write!(out, r#"<w:gridCol w:w="{width}"/>"#);
    }
    out.push_str("</w:tblGrid>");
    for (index, row) in table.rows.iter().enumerate() {
        let header = index == 0;
        out.push_str("<w:tr><w:trPr><w:cantSplit/>");
        if header {
            out.push_str("<w:tblHeader/>");
        }
        out.push_str("</w:trPr>");
        for text in row {
            let _ = write!(
                out,
                r#"<w:tc><w:tcPr><w:tcW w:w="{width}" w:type="dxa"/></w:tcPr><w:p>{}</w:p></w:tc>"#,
                runs_xml(text, header)
            );
        }
        out.push_str("</w:tr>");
    }
    out.push_str("</w:tbl><w:p/>");
    out
}

fn running_header_xml(pleading: Option<&PleadingFrame>) -> String {
    let rail = pleading.map(rail_xml).unwrap_or_default();
    // A pleading's page number sits at the bottom (CRC 2.109), in its
    // footer; its header carries the rail alone.
    let page = if pleading.is_some() {
        "<w:p/>".to_string()
    } else {
        format!(
            r#"<w:p><w:pPr><w:jc w:val="right"/></w:pPr><w:r><w:t>Page </w:t></w:r>{}</w:p>"#,
            field("PAGE")
        )
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{rail}{page}</w:hdr>"#
    )
}

const PAGE_WIDTH_TWIPS: u32 = 12_240;
/// The text column of the default page: 8.5in less two 1in margins.
const DEFAULT_TEXT_WIDTH_TWIPS: u32 = PAGE_WIDTH_TWIPS - 2 * 1440;
const DEFAULT_PAGE_MARGINS: &str = r#"<w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720"/>"#;

/// The pleading page: its margins, with the header and footer kept clear
/// of the text column, and — when the frame has a rail — a double rule on
/// the left of the text column and a single rule on its right, running the
/// column's full height.
fn pleading_page_margins(frame: &PleadingFrame) -> String {
    let rules = if frame.rail_lines.is_some() {
        r#"<w:pgBorders w:offsetFrom="text"><w:left w:val="double" w:sz="4" w:space="20" w:color="auto"/><w:right w:val="single" w:sz="4" w:space="4" w:color="auto"/></w:pgBorders>"#
    } else {
        ""
    };
    format!(
        r#"<w:pgMar w:top="{top}" w:right="{right}" w:bottom="{bottom}" w:left="{left}" w:header="720" w:footer="360"/>{rules}"#,
        top = frame.top_margin_twips,
        right = frame.right_margin_twips,
        bottom = frame.bottom_margin_twips,
        left = frame.left_margin_twips,
    )
}

/// The numbered rail: one exact-pitch paragraph per line, all sharing one
/// page-anchored frame whose top is the top margin, so number `n` sits on
/// the baseline of body line `n`. Header content repeats on every page.
fn rail_xml(frame: &PleadingFrame) -> String {
    let Some(lines) = frame.rail_lines else {
        return String::new();
    };
    let pitch = frame.line_pitch_twips;
    let height = pitch * lines;
    let top = frame.top_margin_twips;
    let face = xml(frame.face);
    let mut rail = String::new();
    for number in 1..=lines {
        let _ = write!(
            rail,
            r#"<w:p><w:pPr><w:framePr w:w="{RAIL_WIDTH_TWIPS}" w:h="{height}" w:hRule="exact" w:wrap="around" w:hAnchor="page" w:vAnchor="page" w:x="{RAIL_X_TWIPS}" w:y="{top}"/><w:spacing w:before="0" w:after="0" w:line="{pitch}" w:lineRule="exact"/><w:jc w:val="right"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii="{face}" w:hAnsi="{face}"/><w:sz w:val="{RAIL_TYPE_HALF_POINTS}"/></w:rPr><w:t>{number}</w:t></w:r></w:p>"#,
        );
    }
    rail
}

/// Styles for a pleading: one face and one type size throughout, with
/// headings set apart by weight alone, and every paragraph on the frame's
/// line pitch — exact when a rail registers against it, double-spaced
/// otherwise.
fn pleading_styles_xml(frame: &PleadingFrame) -> String {
    let face = xml(frame.face);
    let size = frame.type_size_half_points;
    let spacing = if frame.rail_lines.is_some() {
        format!(
            r#"<w:spacing w:before="0" w:after="0" w:line="{}" w:lineRule="exact"/>"#,
            frame.line_pitch_twips
        )
    } else {
        r#"<w:spacing w:before="0" w:after="0" w:line="480" w:lineRule="auto"/>"#.to_string()
    };
    let mut styles = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:pPr>{spacing}</w:pPr><w:rPr><w:rFonts w:ascii="{face}" w:hAnsi="{face}"/><w:sz w:val="{size}"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:basedOn w:val="Normal"/><w:pPr><w:jc w:val="center"/></w:pPr><w:rPr><w:b/></w:rPr></w:style>"#
    );
    for (id, name) in [
        ("Heading1", "heading 1"),
        ("Heading2", "heading 2"),
        ("Heading3", "heading 3"),
        ("Heading4", "heading 4"),
        ("Heading5", "heading 5"),
    ] {
        let _ = write!(
            styles,
            r#"<w:style w:type="paragraph" w:styleId="{id}"><w:name w:val="{name}"/><w:basedOn w:val="Normal"/><w:qFormat/><w:rPr><w:b/></w:rPr></w:style>"#
        );
    }
    styles.push_str("</w:styles>");
    styles
}

fn footer_xml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:ftr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:t>Page </w:t></w:r>{}<w:r><w:t> of </w:t></w:r>{}</w:p></w:ftr>"#,
        field("PAGE"),
        field("NUMPAGES")
    )
}

/// A pleading's footer: the Arabic page number (CRC 2.109), then a printed
/// rule, then the paper's title at 10pt (CRC 2.110), on one line.
///
/// Both lines sit on exact pitches (14pt and 12pt) so the footer stays
/// inside the 48pt bottom margin with the 0.25in footer distance and
/// never pushes the text column up off the rail.
fn pleading_footer_xml(title: Option<&str>) -> String {
    let rule = r#"<w:pBdr><w:top w:val="single" w:sz="4" w:space="1" w:color="auto"/></w:pBdr><w:spacing w:before="0" w:after="0" w:line="240" w:lineRule="exact"/>"#;
    let title = match title
        .map(crate::paper::footer_title)
        .filter(|title| !title.is_empty())
    {
        Some(title) => format!(
            r#"<w:p><w:pPr>{rule}<w:jc w:val="center"/></w:pPr><w:r><w:rPr><w:sz w:val="{FOOTER_TITLE_HALF_POINTS}"/></w:rPr><w:t xml:space="preserve">{}</w:t></w:r></w:p>"#,
            xml(&title)
        ),
        None => format!("<w:p><w:pPr>{rule}</w:pPr></w:p>"),
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:ftr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:pPr><w:spacing w:before="0" w:after="0" w:line="280" w:lineRule="exact"/><w:jc w:val="center"/></w:pPr>{}</w:p>{title}</w:ftr>"#,
        sized_field("PAGE", FOOTER_NUMBER_HALF_POINTS)
    )
}

/// Paper-title size in the footer: CRC 2.110's 10-point floor.
const FOOTER_TITLE_HALF_POINTS: u32 = 20;
/// Page-number size in the footer.
const FOOTER_NUMBER_HALF_POINTS: u32 = 24;
/// Counsel lines reserved above the court's title (CRC 2.111(1)): the
/// title then lands on line 8, 3⅓in down a page whose top margin is 1in.
const COUNSEL_LINES: u32 = 7;
/// Space between compactly set caption lines, in twips (4pt), as in the PDF.
const CAPTION_LEADING_TWIPS: u32 = 80;
/// Average glyph width as a fraction of the em in Tinos prose, used only to
/// estimate how many lines a caption cell wraps to.
const AVERAGE_GLYPH_EM: f64 = 0.45;

fn sized_field(instruction: &str, half_points: u32) -> String {
    let run = format!(r#"<w:r><w:rPr><w:sz w:val="{half_points}"/></w:rPr>"#);
    format!("{run}<w:fldChar w:fldCharType=\"begin\"/></w:r>{run}<w:instrText xml:space=\"preserve\"> {instruction} </w:instrText></w:r>{run}<w:fldChar w:fldCharType=\"end\"/></w:r>")
}

/// How many 24pt grid lines a caption cell needs: its wrapped lines, set
/// compactly, rounded up to the grid. An estimate, used as the row's
/// minimum height so the body lands on a numbered line when the estimate
/// holds and the row simply grows (never clips) when it does not.
fn grid_lines_for(frame: &PleadingFrame, lines: &[String], width_twips: u32, min: u32) -> u32 {
    let size_pt = f64::from(frame.type_size_half_points) / 2.0;
    let width_pt = f64::from(width_twips) / 20.0;
    let per_line = (width_pt / (size_pt * AVERAGE_GLYPH_EM)).floor().max(1.0);
    // A caption line is a few dozen characters, far below f64's exact range.
    #[allow(clippy::cast_precision_loss)]
    let wrapped: f64 = lines
        .iter()
        .map(|line| (line.chars().count() as f64 / per_line).ceil().max(1.0))
        .sum();
    let pitch_pt = size_pt + f64::from(CAPTION_LEADING_TWIPS) / 20.0;
    let grid_pt = f64::from(frame.line_pitch_twips) / 20.0;
    // `wrapped` is a small positive count of lines.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let needed = ((wrapped * pitch_pt) / grid_pt - 0.001).ceil().max(0.0) as u32;
    needed.max(min)
}

fn caption_paragraph(frame: &PleadingFrame, text: &str, bold: bool) -> String {
    let pitch = frame.type_size_half_points * 10 + CAPTION_LEADING_TWIPS;
    let weight = if bold { "<w:rPr><w:b/></w:rPr>" } else { "" };
    format!(
        r#"<w:p><w:pPr><w:spacing w:before="0" w:after="0" w:line="{pitch}" w:lineRule="exact"/></w:pPr><w:r>{weight}<w:t xml:space="preserve">{}</w:t></w:r></w:p>"#,
        xml(text)
    )
}

fn caption_cell(frame: &PleadingFrame, width: u32, borders: &str, lines: &[String]) -> String {
    let paragraphs = if lines.is_empty() {
        "<w:p/>".to_string()
    } else {
        lines
            .iter()
            .map(|line| caption_paragraph(frame, line, false))
            .collect()
    };
    format!(
        r#"<w:tc><w:tcPr><w:tcW w:w="{width}" w:type="dxa"/>{borders}</w:tcPr>{paragraphs}</w:tc>"#
    )
}

fn caption_table(widths: [u32; 2], height: u32, cells: &[String; 2]) -> String {
    let total = widths[0] + widths[1];
    format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="{total}" w:type="dxa"/><w:tblLayout w:type="fixed"/><w:tblCellMar><w:left w:w="0" w:type="dxa"/><w:right w:w="120" w:type="dxa"/></w:tblCellMar></w:tblPr><w:tblGrid><w:gridCol w:w="{}"/><w:gridCol w:w="{}"/></w:tblGrid><w:tr><w:trPr><w:cantSplit/><w:trHeight w:val="{height}" w:hRule="atLeast"/></w:trPr>{}{}</w:tr></w:tbl>"#,
        widths[0], widths[1], cells[0], cells[1]
    )
}

/// The first-page caption: the counsel block over seven grid lines with the
/// right of them left clear for the clerk (CRC 2.111(1)–(2)), the court's
/// title on line 8, and the two-column party/case caption, set compactly
/// and padded to a whole number of grid lines so the body that follows
/// starts on a numbered line.
fn caption_xml(frame: &PleadingFrame, caption: &crate::paper::Caption) -> String {
    let text_width = PAGE_WIDTH_TWIPS - frame.left_margin_twips - frame.right_margin_twips;
    let grid = frame.line_pitch_twips;
    let mut out = String::new();
    if !caption.counsel.is_empty() {
        let counsel_width = text_width * 58 / 100;
        let min = if frame.rail_lines.is_some() {
            COUNSEL_LINES
        } else {
            0
        };
        let height = grid_lines_for(frame, &caption.counsel, counsel_width, min) * grid;
        out.push_str(&caption_table(
            [counsel_width, text_width - counsel_width],
            height,
            &[
                caption_cell(frame, counsel_width, "", &caption.counsel),
                caption_cell(frame, text_width - counsel_width, "", &[]),
            ],
        ));
    }
    for line in &caption.court {
        let _ = write!(
            out,
            r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">{}</w:t></w:r></w:p>"#,
            xml(line)
        );
    }
    if !caption.parties.is_empty() || !caption.case.is_empty() {
        let half = text_width / 2;
        let height = grid_lines_for(frame, &caption.parties, half, 1).max(grid_lines_for(
            frame,
            &caption.case,
            half,
            1,
        )) * grid;
        out.push_str(&caption_table(
            [half, text_width - half],
            height,
            &[
                caption_cell(
                    frame,
                    half,
                    r#"<w:tcBorders><w:bottom w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:right w:val="single" w:sz="4" w:space="0" w:color="auto"/></w:tcBorders>"#,
                    &caption.parties,
                ),
                caption_cell(
                    frame,
                    text_width - half,
                    r#"<w:tcMar><w:left w:w="160" w:type="dxa"/></w:tcMar>"#,
                    &caption.case,
                ),
            ],
        ));
    }
    out
}

fn field(instruction: &str) -> String {
    format!("<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText xml:space=\"preserve\"> {instruction} </w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r>")
}

fn first_header_xml(letterhead: &RenderLetterhead<'_>) -> String {
    let contact = [letterhead.phone, letterhead.email, letterhead.web]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:drawing><wp:inline><wp:extent cx="329184" cy="329184"/><wp:docPr id="1" name="Neon Law mark"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="1" name="logo-neon-law.png"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="rId1"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="329184" cy="329184"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p><w:p><w:pPr><w:jc w:val="center"/><w:pBdr><w:bottom w:val="single" w:sz="8" w:color="06B6D4"/></w:pBdr></w:pPr><w:r><w:rPr><w:b/><w:color w:val="06B6D4"/><w:spacing w:val="80"/></w:rPr><w:t>{}</w:t></w:r></w:p><w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:t>{}</w:t></w:r></w:p></w:hdr>"#,
        xml(&letterhead.name.to_uppercase()),
        xml(&contact)
    )
}

fn document_rels(has_letterhead: bool) -> String {
    let first = if has_letterhead {
        r#"<Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header2.xml"/>"#
    } else {
        ""
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>{first}<Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer2.xml"/></Relationships>"#
    )
}

fn content_types(has_letterhead: bool) -> String {
    let header = if has_letterhead {
        r#"<Override PartName="/word/header2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>"#
    } else {
        ""
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>{header}<Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/><Override PartName="/word/footer2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/><Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/></Types>"#
    )
}

fn core_xml(revision: Option<&str>) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:creator>Neon Law Navigator</dc:creator><dc:identifier>{}</dc:identifier></cp:coreProperties>"#,
        xml(revision.unwrap_or("uncommitted"))
    )
}

fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/></Relationships>"#;
const HEADER_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/logo-neon-law.png"/></Relationships>"#;
const APP_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Application>Neon Law Navigator</Application></Properties>"#;
const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:rPr><w:rFonts w:ascii="Noto Serif" w:hAnsi="Noto Serif"/><w:sz w:val="22"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:basedOn w:val="Normal"/><w:rPr><w:b/><w:sz w:val="32"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:uiPriority w:val="9"/><w:qFormat/><w:rPr><w:b/><w:sz w:val="28"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Normal"/><w:qFormat/><w:rPr><w:b/><w:sz w:val="24"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading3"><w:name w:val="heading 3"/><w:basedOn w:val="Normal"/><w:qFormat/></w:style><w:style w:type="paragraph" w:styleId="Heading4"><w:name w:val="heading 4"/><w:basedOn w:val="Normal"/></w:style><w:style w:type="paragraph" w:styleId="Heading5"><w:name w:val="heading 5"/><w:basedOn w:val="Normal"/></w:style></w:styles>"#;

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::{blocks, render_notation, PleadingFrame, RenderLetterhead, RenderOptions};

    /// The California numbered-rail frame `pdf::pleading` hands the CLI.
    const RAILED: PleadingFrame = PleadingFrame {
        face: "Tinos",
        type_size_half_points: 29,
        top_margin_twips: 1440,
        bottom_margin_twips: 960,
        left_margin_twips: 2160,
        right_margin_twips: 720,
        line_pitch_twips: 480,
        rail_lines: Some(28),
    };

    /// The federal frame: deeper top margin, no rail.
    const UNRAILED: PleadingFrame = PleadingFrame {
        top_margin_twips: 2160,
        bottom_margin_twips: 1440,
        left_margin_twips: 1440,
        right_margin_twips: 1440,
        rail_lines: None,
        ..RAILED
    };

    fn render_pleading(frame: PleadingFrame) -> Vec<u8> {
        render_notation(
            "# Complaint\n\n## 1. Section\n\nPlaintiff states as follows.",
            None,
            &RenderOptions {
                source_revision: None,
                pleading: Some(frame),
            },
        )
        .unwrap()
    }

    /// The paragraphs of `markdown`, tables aside.
    fn paragraphs(markdown: &str) -> Vec<super::Paragraph> {
        blocks(markdown)
            .into_iter()
            .filter_map(|block| match block {
                super::Block::Paragraph(paragraph) => Some(paragraph),
                super::Block::Table(_) => None,
            })
            .collect()
    }

    fn part(bytes: &[u8], name: &str) -> String {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut xml = String::new();
        archive
            .by_name(name)
            .unwrap()
            .read_to_string(&mut xml)
            .unwrap();
        xml
    }

    /// Rail numbers in a header, in order: the text of each framed paragraph.
    fn rail_numbers(header: &str) -> Vec<String> {
        header
            .split("<w:p>")
            .filter(|paragraph| paragraph.contains("<w:framePr "))
            .filter_map(|paragraph| {
                let start = paragraph.find("<w:t>")? + "<w:t>".len();
                let end = paragraph[start..].find("</w:t>")? + start;
                Some(paragraph[start..end].to_string())
            })
            .collect()
    }

    #[test]
    fn a_railed_pleading_numbers_its_frame_one_through_twenty_eight() {
        let bytes = render_pleading(RAILED);
        let header = part(&bytes, "word/header1.xml");
        let expected: Vec<String> = (1..=28).map(|n| n.to_string()).collect();
        assert_eq!(rail_numbers(&header), expected);
        // Every rail line sits on the same exact 24pt pitch, in one frame
        // whose top is the 1in top margin and whose height is the frame.
        assert_eq!(
            header.matches(r#"w:line="480" w:lineRule="exact""#).count(),
            28
        );
        assert_eq!(header.matches(r#"w:h="13440" w:hRule="exact""#).count(), 28);
        assert_eq!(header.matches(r#"w:y="1440""#).count(), 28);

        let styles = part(&bytes, "word/styles.xml");
        assert!(
            styles.contains(r#"w:line="480" w:lineRule="exact""#),
            "body text must sit on the same exact 24pt pitch: {styles}"
        );
        assert!(styles.contains(r#"w:ascii="Tinos""#) && styles.contains(r#"<w:sz w:val="29"/>"#));

        let document = part(&bytes, "word/document.xml");
        assert!(document
            .contains(r#"<w:pgMar w:top="1440" w:right="720" w:bottom="960" w:left="2160""#));
        assert!(document.contains(r#"<w:left w:val="double""#));
        assert!(document.contains(r#"<w:right w:val="single""#));
    }

    #[test]
    fn an_unrailed_pleading_draws_no_rail() {
        let bytes = render_pleading(UNRAILED);
        let header = part(&bytes, "word/header1.xml");
        assert!(rail_numbers(&header).is_empty());
        assert!(!header.contains("w:framePr"));
        let document = part(&bytes, "word/document.xml");
        assert!(!document.contains("w:pgBorders"));
        assert!(document.contains(r#"<w:pgMar w:top="2160""#));
        let styles = part(&bytes, "word/styles.xml");
        assert!(!styles.contains(r#"w:lineRule="exact""#));
    }

    #[test]
    fn repeated_pleading_renders_are_byte_identical() {
        assert_eq!(render_pleading(RAILED), render_pleading(RAILED));
    }

    const CAPTIONED: &str = "```caption\ncounsel: Pat Sample (SBN 000000)\ncounsel: SAMPLE & SONS <LLP>\n\
        counsel: **100 Example \\* Way**\ncourt: SUPERIOR COURT OF CALIFORNIA\ncourt: COUNTY OF SAMPLE\n\
        parties: SAMPLE PLAINTIFF,\nparties: Plaintiff,\nparties: v.\nparties: EXAMPLE DEFENDANT,\n\
        parties: Defendant.\ncase: Case No. 00CV000000\ncase: Dept. 12\n```\n\n\
        # RESPONSES TO REQUESTS FOR ADMISSION\n\n## 1. Responses\n\n<!-- keep -->\n\nREQUEST NO. 1:\n\n\
        Admit it.\n\nRESPONSE NO. 1:\n\nDenied.\n\n<!-- /keep -->\n\nAfter the group.\n";

    fn render_captioned(frame: PleadingFrame) -> Vec<u8> {
        render_notation(
            CAPTIONED,
            None,
            &RenderOptions {
                source_revision: None,
                pleading: Some(frame),
            },
        )
        .unwrap()
    }

    /// CRC 2.111: the counsel block holds lines 1 through 7 with the right
    /// of it clear, the court's title follows on line 8, and the two-column
    /// caption after it is padded to whole grid lines.
    #[test]
    fn a_captioned_pleading_lays_out_counsel_court_title_and_caption() {
        let document = part(&render_captioned(RAILED), "word/document.xml");
        assert_eq!(document.matches("<w:tbl>").count(), 2);
        // Seven grid lines (7 x 480 twips) at least, then the court title.
        let counsel_end = document.find("</w:tbl>").unwrap();
        let counsel = &document[..counsel_end];
        assert!(counsel.contains(r#"<w:trHeight w:val="3360" w:hRule="atLeast"/>"#));
        // The right cell is the clerk's: present, wide, and empty.
        assert!(counsel.contains("<w:p/>"));
        let court = &document[counsel_end..];
        let title = court.find("SUPERIOR COURT OF CALIFORNIA").unwrap();
        let caption = court.find("<w:tbl>").unwrap();
        assert!(title < caption, "the court's title precedes the caption");
        assert!(court[..caption].contains(r#"<w:jc w:val="center"/>"#));
        // The caption is one row of two cells, its height a whole number
        // of 24pt lines, with the parties' cell ruled right and below.
        let table = &court[caption..court[caption..].find("</w:tbl>").unwrap() + caption];
        assert_eq!(table.matches("<w:tr>").count(), 1);
        assert_eq!(table.matches("<w:tc>").count(), 2);
        assert!(table.contains(r#"<w:right w:val="single""#));
        assert!(table.contains(r#"<w:bottom w:val="single""#));
        let height = table
            .split(r#"<w:trHeight w:val=""#)
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap();
        assert_eq!(height % 480, 0, "caption height {height} is off the grid");
        assert!(table.contains("Case No. 00CV000000"));
        // Caption lines are single-spaced (18.5pt exact), independent of
        // the body's 24pt pitch, and keep the body's 14.5pt size.
        assert!(table.contains(r#"w:line="370" w:lineRule="exact""#));
    }

    /// An answer's emphasis markup never prints, and markup characters in a
    /// field are escaped for XML.
    #[test]
    fn caption_fields_print_plain_and_escape_xml() {
        let document = part(&render_captioned(RAILED), "word/document.xml");
        assert!(document.contains("SAMPLE &amp; SONS &lt;LLP&gt;"));
        assert!(document.contains("100 Example * Way"));
        assert!(!document.contains("**"));
        assert!(!document.contains("```"));
    }

    /// CRC 2.109/2.110: Arabic page number at the bottom, a printed rule
    /// under it, and the paper's title at 10pt or more below the rule. The
    /// header carries the rail and no page number.
    #[test]
    fn a_pleading_footer_numbers_the_page_and_prints_the_paper_title() {
        let bytes = render_captioned(RAILED);
        let footer = part(&bytes, "word/footer1.xml");
        assert!(footer.contains(" PAGE "));
        assert!(!footer.contains("NUMPAGES"));
        assert!(footer.contains(r#"<w:top w:val="single""#));
        assert!(footer.contains("RESPONSES TO REQUESTS FOR ADMISSION"));
        // Title at 10pt (20 half-points), number at 12pt: neither below 10pt.
        assert!(footer.contains(r#"<w:sz w:val="20"/>"#));
        for size in footer.split(r#"<w:sz w:val=""#).skip(1) {
            let half: u32 = size.split('"').next().unwrap().parse().unwrap();
            assert!(half >= 20, "footer text below 10pt: {half}");
        }
        let header = part(&bytes, "word/header1.xml");
        assert!(
            !header.contains("Page "),
            "the page number is in the footer"
        );
        assert!(!header.contains(" PAGE "));
    }

    /// A pleading with no `#` title has no footer title, but still has the
    /// number and the rule.
    #[test]
    fn a_pleading_without_a_title_keeps_the_page_number_and_rule() {
        let bytes = render_notation(
            "Plain body.\n",
            None,
            &RenderOptions {
                source_revision: None,
                pleading: Some(RAILED),
            },
        )
        .unwrap();
        let footer = part(&bytes, "word/footer1.xml");
        assert!(footer.contains(" PAGE ") && footer.contains(r#"<w:top w:val="single""#));
        assert!(!footer.contains("<w:t "));
    }

    /// A long paper title is abbreviated to one footer line.
    #[test]
    fn a_long_paper_title_is_abbreviated_in_the_footer() {
        let long = "WORD ".repeat(40);
        let bytes = render_notation(
            &format!("# {long}\n"),
            None,
            &RenderOptions {
                source_revision: None,
                pleading: Some(RAILED),
            },
        )
        .unwrap();
        let footer = part(&bytes, "word/footer1.xml");
        assert!(footer.contains('…'));
        let text = footer.split("<w:t xml:space=\"preserve\">").nth(1).unwrap();
        let text = text.split("</w:t>").next().unwrap();
        assert!(text.chars().count() <= crate::paper::FOOTER_TITLE_MAX_CHARS);
    }

    /// A kept group's paragraphs stay together on one page: each stays with
    /// the next, the last only keeps its own lines, and a paragraph outside
    /// the group has neither.
    #[test]
    fn a_keep_group_keeps_its_paragraphs_together() {
        let document = part(&render_captioned(RAILED), "word/document.xml");
        let request = document.find("REQUEST NO. 1:").unwrap();
        let before = &document[..request];
        let paragraph_start = before.rfind("<w:p>").unwrap();
        assert!(document[paragraph_start..request].contains("<w:keepNext/><w:keepLines/>"));
        let denied = document.find("Denied.").unwrap();
        let last_start = document[..denied].rfind("<w:p>").unwrap();
        let last = &document[last_start..denied];
        assert!(last.contains("<w:keepLines/>") && !last.contains("<w:keepNext/>"));
        let after = document.find("After the group.").unwrap();
        let after_start = document[..after].rfind("<w:p>").unwrap();
        assert!(!document[after_start..after].contains("keep"));
    }

    #[test]
    fn a_bad_caption_is_refused_not_dropped() {
        let error = render_notation(
            "```caption\nnot a field line\n```\n",
            None,
            &RenderOptions {
                source_revision: None,
                pleading: Some(RAILED),
            },
        )
        .unwrap_err();
        assert!(matches!(error, super::RenderError::Paper(_)));
    }

    /// Without a pleading frame a caption fence is just a code block: the
    /// generic Word output is unchanged.
    #[test]
    fn a_non_pleading_render_ignores_the_caption_convention() {
        let bytes = render_notation(CAPTIONED, None, &RenderOptions::default()).unwrap();
        let document = part(&bytes, "word/document.xml");
        assert!(!document.contains("<w:tbl>"));
        let footer = part(&bytes, "word/footer1.xml");
        assert!(footer.contains("NUMPAGES"));
    }

    /// The same wide-margin, font-floor and rail facts hold with a caption
    /// as without one: 12pt-or-larger Tinos, at least 1in left and 0.5in
    /// right margins, 28 numbers.
    #[test]
    fn a_captioned_pleading_keeps_the_court_paper_frame() {
        let bytes = render_captioned(RAILED);
        let styles = part(&bytes, "word/styles.xml");
        assert!(styles.contains(r#"w:ascii="Tinos""#) && styles.contains(r#"<w:sz w:val="29"/>"#));
        let document = part(&bytes, "word/document.xml");
        assert!(document.contains(r#"w:right="720""#) && document.contains(r#"w:left="2160""#));
        assert_eq!(rail_numbers(&part(&bytes, "word/header1.xml")).len(), 28);
        // No run sets a size below the body's 14.5pt outside the footer.
        for size in document.split(r#"<w:sz w:val=""#).skip(1) {
            let half: u32 = size.split('"').next().unwrap().parse().unwrap();
            assert!(half >= 24, "body text below 12pt: {half}");
        }
    }

    fn render() -> Vec<u8> {
        render_notation(
            "# Demand\n\n## I. Payment\n\nPay now.",
            Some(&RenderLetterhead {
                name: "Neon Law",
                phone: "+1 510 800 2090",
                email: "contact@neonlaw.com",
                web: "www.neonlaw.com",
                logo_png: b"png",
            }),
            &RenderOptions {
                source_revision: Some("abc123"),
                pleading: None,
            },
        )
        .unwrap()
    }

    #[test]
    fn repeated_word_renders_are_byte_identical() {
        assert_eq!(render(), render());
    }

    #[test]
    fn letter_package_has_editable_outline_and_page_chrome() {
        let bytes = render();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut document = String::new();
        archive
            .by_name("word/document.xml")
            .unwrap()
            .read_to_string(&mut document)
            .unwrap();
        assert!(document.contains("w:pStyle w:val=\"Heading1\""));
        assert!(document.contains("<w:titlePg/>"));
        assert!(archive.by_name("word/media/logo-neon-law.png").is_ok());
        let mut footer = String::new();
        archive
            .by_name("word/footer1.xml")
            .unwrap()
            .read_to_string(&mut footer)
            .unwrap();
        assert!(footer.contains(" PAGE ") && footer.contains(" NUMPAGES "));
    }

    const FEE_TABLE: &str = "Before the table.\n\n\
        | Work | Fee |\n| --- | --- |\n| Base engagement | $50 per day |\n\
        | Cost & \\<scope\\> | |\n| \"Quoted\" | it's `code` |\n\nAfter the table.\n";

    /// Text of each `<w:tc>` in one `<w:tr>`, in order.
    fn cells(row: &str) -> Vec<String> {
        row.split("<w:tc>")
            .skip(1)
            .map(|cell| {
                cell.split("<w:t xml:space=\"preserve\">")
                    .skip(1)
                    .map(|run| run.split("</w:t>").next().unwrap())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_markdown_table_is_a_real_word_table() {
        let bytes = render_notation(FEE_TABLE, None, &RenderOptions::default()).unwrap();
        let document = part(&bytes, "word/document.xml");
        assert_eq!(document.matches("<w:tbl>").count(), 1);
        let start = document.find("<w:tbl>").unwrap();
        let end = document.find("</w:tbl>").unwrap();
        let table = &document[start..end];
        let rows: Vec<&str> = table.split("<w:tr>").skip(1).collect();
        assert_eq!(rows.len(), 4, "a header row and three body rows");
        assert_eq!(table.matches("<w:gridCol ").count(), 2);
        // Only the header row repeats across pages, and it is bold.
        assert_eq!(table.matches("<w:tblHeader/>").count(), 1);
        assert!(rows[0].contains("<w:tblHeader/>") && rows[0].contains("<w:b/>"));
        assert!(!rows[1].contains("<w:b/>"));
        assert_eq!(cells(rows[0]), ["Work", "Fee"]);
        assert_eq!(cells(rows[1]), ["Base engagement", "$50 per day"]);
        // An empty cell is still a cell, holding one empty paragraph.
        assert_eq!(cells(rows[2]), ["Cost &amp; &lt;scope&gt;", ""]);
        assert_eq!(rows[2].matches("<w:tc>").count(), 2);
        assert_eq!(cells(rows[3]), ["&quot;Quoted&quot;", "it&apos;s code"]);
        // The surrounding paragraphs stay paragraphs, on either side.
        assert!(document[..start].contains("Before the table."));
        assert!(document[end..].contains("After the table."));
        assert!(!document[..start].contains("Work"));
        assert!(!document[end..].contains("Fee"));
        assert!(!document.contains('\t'));
    }

    /// A `<br>` inside a cell is a Word line break, the same break an authored
    /// hard line produces. The pipes stay structure, not text.
    #[test]
    fn a_break_tag_in_a_table_cell_is_a_word_line_break() {
        let markdown = "Before.\n\n\
            | Party | Role |\n\
            | --- | --- |\n\
            | Alex Example | Claimant<br>care of agent |\n\
            | Blake Example | Respondent |\n\n\
            After.\n";
        let bytes = render_notation(markdown, None, &RenderOptions::default()).unwrap();
        let document = part(&bytes, "word/document.xml");
        assert_eq!(document.matches("<w:tbl>").count(), 1);
        let start = document.find("<w:tbl>").unwrap();
        let end = document.find("</w:tbl>").unwrap();
        let table = &document[start..end];
        assert!(table.contains("<w:br/>"), "{table}");
        assert!(table.contains("Alex Example"));
        assert!(table.contains("Claimant"));
        assert!(table.contains("care of agent"));
        assert!(table.contains("Blake Example"));
        assert!(!document.contains('|'));
        assert!(!document.contains("<br"));
    }

    #[test]
    fn table_rendering_is_deterministic_and_fits_the_pleading_column() {
        assert_eq!(
            render_notation(FEE_TABLE, None, &RenderOptions::default()).unwrap(),
            render_notation(FEE_TABLE, None, &RenderOptions::default()).unwrap()
        );
        let bytes = render_notation(
            FEE_TABLE,
            None,
            &RenderOptions {
                source_revision: None,
                pleading: Some(RAILED),
            },
        )
        .unwrap();
        let document = part(&bytes, "word/document.xml");
        // 8.5in less the 1.5in and 0.5in margins leaves 9360 twips.
        assert!(document.contains(r#"<w:tblW w:w="9360" w:type="dxa"/>"#));
        assert_eq!(document.matches(r#"<w:gridCol w:w="4680"/>"#).count(), 2);
    }

    #[test]
    fn soft_wraps_join_as_spaces_not_hard_breaks() {
        let result = paragraphs(
            "If the account was opened in someone\nelse's name, tell us before\nwe pursue anything.",
        );
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].text,
            "If the account was opened in someone else's name, tell us before we pursue anything."
        );
    }

    #[test]
    fn a_real_hard_break_still_becomes_a_break() {
        let result = paragraphs("First line.  \nSecond line.");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "First line.\nSecond line.");
    }

    #[test]
    fn a_trailing_backslash_on_the_last_line_is_dropped() {
        let result = paragraphs("Date: {{client.date}} \\");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "Date: {{client.date}} ");
    }

    #[test]
    fn a_mid_paragraph_trailing_backslash_still_breaks() {
        let result = paragraphs("Date: {{client.date}} \\\nSigned,");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "Date: {{client.date}} \nSigned,");
    }

    #[test]
    fn rendering_a_wrapped_paragraph_has_no_br_and_one_text_run() {
        let bytes = render_notation(
            "Please confirm the account was opened in someone\nelse's name before\nwe file anything.",
            None,
            &RenderOptions::default(),
        )
        .unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut document = String::new();
        archive
            .by_name("word/document.xml")
            .unwrap()
            .read_to_string(&mut document)
            .unwrap();
        assert!(!document.contains("w:br"));
        assert_eq!(document.matches("<w:t ").count(), 1);
        assert!(document.contains("someone else&apos;s name before we file anything."));
    }

    /// LAW-141: a Signer Box the fill wrote in place of an unanswered
    /// placeholder is an empty rule in the Word document — an underlined
    /// run, with no marker text and no placeholder token left behind.
    #[test]
    fn a_signer_box_is_an_underlined_rule_in_a_paragraph_and_a_cell() {
        let markdown = format!(
            "Shares: {} of common stock.\n\n| Party | Initials |\n| --- | --- |\n| Example Co. | {} |\n",
            crate::paper::SIGNER_BOX,
            crate::paper::SIGNER_BOX
        );
        let bytes = render_notation(&markdown, None, &RenderOptions::default()).unwrap();
        let document = part(&bytes, "word/document.xml");
        assert_eq!(
            document.matches("<w:u w:val=\"single\"/>").count(),
            2,
            "{document}"
        );
        assert!(document.contains("Shares: </w:t>"), "{document}");
        assert!(document.contains("> of common stock.</w:t>"), "{document}");
        assert!(!document.contains("signer-box"), "{document}");
        assert!(!document.contains("{{"), "{document}");
        assert!(!document.contains('\u{E000}'), "{document}");
    }
}
