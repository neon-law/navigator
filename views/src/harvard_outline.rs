//! Parse a legal Markdown body into Harvard-outline narration units.
//!
//! Motions and contracts are walked paragraph by paragraph on a stage: each
//! heading, body paragraph, and lettered subsection is one unit a narrator can
//! highlight. Closing letters and wills number depth-1 headings with **Roman
//! numerals** (`I.`, `II.`). Pleadings use **Arabic numerals** (`1.`, `2.`).
//! An agreement may use that Roman scheme, decimal numbers (`1.`, `1.1`), or
//! `Section 1.01`; `N123` keeps the choice consistent. Lettered subsections
//! (`A.`, `B.`) live in Markdown block quotes so the PDF conversion indents
//! them. Decimal and Section agreements use `(a)` block quotes the same way.
//!
//! Unlabeled paragraphs under a heading inherit that heading's depth, so they
//! share one highlight colour as the narrator steps through them.
//!
//! Two block shapes carry drafting state rather than prose. A **hold** is a
//! paragraph written wholly in square brackets (`[Confirm the venue.]`): the
//! drafter has parked a decision there and the document is not ready to
//! leave. An **open term** is a `{{question_code}}` run, the answer a
//! questionnaire has not supplied yet. An unlabeled paragraph inside a block
//! quote that carries no subsection marker is a **quotation**.
//!
//! # Fragments
//!
//! Every unit carries a [`Unit::fragment`], the DOM id a link lands on. It is
//! derived from the Harvard path, never from position, so the same source
//! always yields the same ids and inserting a paragraph moves no other
//! unit's link:
//!
//! - an outline unit is `section-` and its path, lowercased, with each run of
//!   punctuation folded to `-`: `I` is `section-i`, `II.A` is
//!   `section-ii-a`, `1.01.(a)` is `section-1-01-a`;
//! - an unnumbered heading is `heading-` and its caption the same way;
//! - a paragraph, hold, or quotation is its enclosing unit's fragment and
//!   `-p` with its ordinal under that unit (`section-ii-a-p2`), or
//!   `preamble-p1` before the first heading. No marker spells `p` followed
//!   by digits, so a paragraph id cannot collide with a child unit's.
//!
//! A repeated fragment (two sections both marked `II`) takes `-2`, `-3`.

use std::collections::{HashMap, HashSet};

use crate::markdown;

/// How depth-1 headings are numbered in this document. This is the shared
/// Word/PDF vocabulary, re-exported here for the narration API.
pub use word::OutlineScheme as DepthOneScheme;

/// What kind of block a unit is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    Heading,
    Paragraph,
    Subsection,
    /// A paragraph written wholly in square brackets: a parked decision.
    Hold,
    /// An unlabeled paragraph inside a block quote with no subsection marker.
    Quotation,
}

impl UnitKind {
    /// Whether this unit opens an outline node rather than sitting in one.
    #[must_use]
    pub const fn is_outline(self) -> bool {
        matches!(self, Self::Heading | Self::Subsection)
    }

    const fn class(self) -> &'static str {
        match self {
            Self::Heading => "heading",
            Self::Paragraph => "paragraph",
            Self::Subsection => "subsection",
            Self::Hold => "hold",
            Self::Quotation => "quotation",
        }
    }
}

/// How depth-one headings are written: the four spellings `N123` accepts.
/// [`DepthOneScheme`] is the coarser Word/PDF root (Roman or decimal); this
/// keeps the agreement conventions apart for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthOneNumbering {
    /// `I.`, `II.`
    Roman,
    /// `1.`, `2.`
    Arabic,
    /// `1.1`, `1.2`, `2.1`
    Decimal,
    /// `Section 1.01`
    Section,
}

impl DepthOneNumbering {
    /// The Word/PDF root this numbering renders under.
    #[must_use]
    pub const fn scheme(self) -> DepthOneScheme {
        match self {
            Self::Roman => DepthOneScheme::Roman,
            Self::Arabic | Self::Decimal | Self::Section => DepthOneScheme::Arabic,
        }
    }

    /// A reader-facing name for the numbering.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Roman => "Roman numerals",
            Self::Arabic => "Arabic numerals",
            Self::Decimal => "Decimal numbers",
            Self::Section => "Section numbers",
        }
    }
}

/// One highlightable block on the narration stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub index: usize,
    /// Stable within this Markdown source; imported Word anchors are carried
    /// through the canonical adapter before this narration projection.
    pub anchor: String,
    /// Outline depth, 0 for preamble before the first heading, then 1..=7.
    pub depth: u8,
    /// Displayed marker (`I`, `A`, `1`) or empty for unlabeled prose.
    pub marker: String,
    /// Full path from the root (`I`, `I.A`, `1.B`).
    pub path: String,
    /// The DOM id a link to this unit lands on, derived from `path` (see the
    /// module's Fragments section). Stable across re-parses of one source.
    pub fragment: String,
    pub kind: UnitKind,
    /// Markdown of this unit (heading text without the `#` marks, or the
    /// paragraph/quote body).
    pub markdown: String,
}

/// A parsed document ready to render as a narration stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineDocument {
    pub title: String,
    pub scheme: Option<DepthOneScheme>,
    /// The depth-one spelling the first numbered heading used.
    pub numbering: Option<DepthOneNumbering>,
    pub units: Vec<Unit>,
    /// The raw YAML between the `---` fences, with neither fence line. `None`
    /// for a document with no frontmatter.
    pub frontmatter: Option<String>,
}

impl OutlineDocument {
    /// Convert this parsed Markdown projection to the shared canonical model.
    #[must_use]
    pub fn canonical_model(&self) -> word::CanonicalDocument {
        canonical_model(self)
    }
}

/// Parse Markdown (optional YAML frontmatter) into narration units.
#[must_use]
pub fn parse(src: &str) -> OutlineDocument {
    let (title, frontmatter, body) = title_and_body(src);
    let mut units = Vec::new();
    let mut numbering: Option<DepthOneNumbering> = None;
    let mut current_depth: u8 = 0;
    let mut current_path = String::new();
    // The open outline units, shallowest first, so a marker's path joins the
    // nearest shallower unit's rather than its preceding sibling's: `(b)`
    // after `1.01.(a)` is `1.01.(b)`.
    let mut open: Vec<(u8, String)> = Vec::new();
    // The last capital letter at depth 2 in this section, so a lone `C`
    // after `B` reads as the third letter rather than the numeral 100.
    let mut last_letter: Option<char> = None;

    for block in blocks(body) {
        match block {
            Block::Heading { text, anchor } => {
                let (depth, marker, path_marker, heading_text) =
                    heading_parts(text, last_letter, &mut numbering, &open, current_depth);
                let path = if marker.is_empty() {
                    current_path.clone()
                } else {
                    open_unit(&mut open, depth, &path_marker)
                };
                current_depth = depth;
                current_path.clone_from(&path);
                if depth == 1 {
                    last_letter = None;
                } else if depth == 2 {
                    last_letter = marker.chars().next();
                }
                push_unit(
                    &mut units,
                    depth,
                    marker,
                    path,
                    UnitKind::Heading,
                    &heading_text,
                );
                set_last_anchor(&mut units, anchor);
            }
            Block::Quote { paragraphs, anchor } => {
                let mut anchor = anchor;
                // An unlabeled paragraph after a marked one in the same quote
                // continues that subsection; before any, it is a quotation.
                let mut marked = false;
                for para in paragraphs {
                    if let Some((letter, rest)) = parse_bold_letter_lead(&para) {
                        let path = open_unit(&mut open, 2, &letter);
                        last_letter = letter.chars().next();
                        current_depth = 2;
                        current_path.clone_from(&path);
                        marked = true;
                        push_unit(&mut units, 2, letter, path, UnitKind::Subsection, &rest);
                    } else if let Some((marker, rest, depth)) = parse_deeper_lead(&para) {
                        let depth = disambiguate_roman(&open, &marker, depth);
                        let path = open_unit(&mut open, depth, &marker);
                        current_depth = depth;
                        current_path.clone_from(&path);
                        marked = true;
                        push_unit(&mut units, depth, marker, path, UnitKind::Subsection, &rest);
                    } else {
                        let kind = if hold_body(&para).is_some() {
                            UnitKind::Hold
                        } else if marked {
                            UnitKind::Paragraph
                        } else {
                            UnitKind::Quotation
                        };
                        push_block(
                            &mut units,
                            current_depth.max(1),
                            current_path.clone(),
                            kind,
                            &para,
                        );
                    }
                    set_last_anchor(&mut units, anchor.take());
                }
            }
            Block::Prose { text, anchor } => {
                let kind = if hold_body(&text).is_some() {
                    UnitKind::Hold
                } else {
                    UnitKind::Paragraph
                };
                // 0 in the preamble, else the enclosing heading's depth.
                push_block(&mut units, current_depth, current_path.clone(), kind, &text);
                set_last_anchor(&mut units, anchor);
            }
        }
    }

    assign_fragments(&mut units);
    OutlineDocument {
        title,
        scheme: numbering.map(DepthOneNumbering::scheme),
        numbering,
        units,
        frontmatter: frontmatter.map(str::to_string),
    }
}

/// A heading's depth, displayed marker, path segment, and caption. The
/// first numbered depth-one heading fixes the document's `numbering`.
fn heading_parts(
    text: String,
    last_letter: Option<char>,
    numbering: &mut Option<DepthOneNumbering>,
    open: &[(u8, String)],
    current_depth: u8,
) -> (u8, String, String, String) {
    let (labeled, continued) = classify_heading(&text, last_letter);
    let roman = *numbering == Some(DepthOneNumbering::Roman);
    match labeled {
        // A `1.` heading is depth 1 for motion practice. Under a Roman
        // contract it is a depth-3 heading (`### 1.`).
        Some((DepthOneNumbering::Arabic, marker, rest)) if roman => {
            (3, marker.clone(), marker, rest)
        }
        Some((found, marker, rest)) => {
            numbering.get_or_insert(found);
            let path_marker = depth_one_path_marker(&marker);
            (1, marker, path_marker, rest)
        }
        None => {
            if let Some((letter, rest)) = continued.or_else(|| parse_capital_letter(&text)) {
                (2, letter.clone(), letter, rest)
            } else if let Some((marker, rest, depth)) = parse_parenthetical(&text) {
                let depth = disambiguate_roman(open, &marker, depth);
                (depth, marker.clone(), marker, rest)
            } else {
                let depth = if current_depth == 0 { 1 } else { current_depth };
                (depth, String::new(), String::new(), text)
            }
        }
    }
}

/// The path segment a depth-one marker contributes: `Section 1.01` is
/// addressed as `1.01`, every other marker as itself.
fn depth_one_path_marker(marker: &str) -> String {
    marker
        .strip_prefix("Section ")
        .unwrap_or(marker)
        .to_string()
}

/// Open an outline unit at `depth`, closing every open unit as deep or
/// deeper, and return its path.
fn open_unit(open: &mut Vec<(u8, String)>, depth: u8, marker: &str) -> String {
    while open
        .last()
        .is_some_and(|(open_depth, _)| *open_depth >= depth)
    {
        open.pop();
    }
    let parent = open.last().map_or("", |(_, path)| path.as_str());
    let path = join_path(parent, marker);
    open.push((depth, path.clone()));
    path
}

/// `(i)`, `(v)`, and `(x)` are lower letters and lower roman numerals at
/// once. One continues the lettering at depth six only when it follows its
/// predecessor letter there (`(i)` after `(h)`); under any other depth-six
/// unit, or within a roman run, it is the depth-seven numeral.
fn disambiguate_roman(open: &[(u8, String)], marker: &str, depth: u8) -> u8 {
    let Some(letter) = marker
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
        .filter(|inner| matches!(*inner, "i" | "v" | "x"))
        .and_then(|inner| inner.chars().next())
    else {
        return depth;
    };
    if depth != 6 {
        return depth;
    }
    let Some((open_depth, path)) = open.last() else {
        return depth;
    };
    let previous_letter = path
        .rsplit('.')
        .next()
        .and_then(|segment| segment.strip_prefix('('))
        .and_then(|rest| rest.strip_suffix(')'))
        .and_then(|inner| {
            let mut chars = inner.chars();
            chars.next().filter(|_| chars.next().is_none())
        });
    match (*open_depth, previous_letter) {
        (6, Some(previous)) if u32::from(previous) + 1 == u32::from(letter) => 6,
        (6 | 7, _) => 7,
        _ => depth,
    }
}

/// A `### (a) Label` heading: the parenthetical marker, caption, and depth.
fn parse_parenthetical(text: &str) -> Option<(String, String, u8)> {
    if text.trim_start().starts_with('(') {
        parse_deeper_lead(text)
    } else {
        None
    }
}

/// The text inside a hold: a paragraph written wholly in square brackets.
/// `[blank]`, `[...]`, and `[…]` are fill-in blanks, not holds, and a
/// bracketed link (`[text](url)`) is prose.
#[must_use]
pub fn hold_body(markdown: &str) -> Option<&str> {
    let inner = markdown.trim().strip_prefix('[')?.strip_suffix(']')?.trim();
    let blank = inner.is_empty()
        || inner.eq_ignore_ascii_case("blank")
        || inner.chars().all(|c| c == '.' || c == '…');
    (!blank && !inner.contains("](") && !inner.contains('[')).then_some(inner)
}

/// Give every unit its path-derived DOM id (see the module's Fragments
/// section).
fn assign_fragments(units: &mut [Unit]) {
    let mut taken: HashSet<String> = HashSet::new();
    let mut ordinals: HashMap<String, usize> = HashMap::new();
    let mut enclosing = "preamble".to_string();
    for unit in units {
        if unit.kind.is_outline() {
            let base = if unit.marker.is_empty() || unit.path.is_empty() {
                let caption = slug(&unit.markdown);
                if caption.is_empty() {
                    "heading".to_string()
                } else {
                    format!("heading-{caption}")
                }
            } else {
                format!("section-{}", slug(&unit.path))
            };
            enclosing = unique(&mut taken, &base);
            unit.fragment.clone_from(&enclosing);
        } else {
            let ordinal = ordinals.entry(enclosing.clone()).or_default();
            *ordinal += 1;
            unit.fragment = unique(&mut taken, &format!("{enclosing}-p{ordinal}"));
        }
    }
}

fn unique(taken: &mut HashSet<String>, base: &str) -> String {
    let mut candidate = base.to_string();
    let mut n = 1;
    while taken.contains(&candidate) {
        n += 1;
        candidate = format!("{base}-{n}");
    }
    taken.insert(candidate.clone());
    candidate
}

/// Lowercase ASCII alphanumerics with every other run folded to one `-`.
fn slug(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// One inline run of a unit's Markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Run {
    /// Prose, still Markdown.
    Text(String),
    /// A `{{question_code}}` the questionnaire has not answered. `label` is
    /// the whole expression (`person__client.name`); `question` is the code
    /// of the question that answers it (`person__client`).
    OpenTerm { label: String, question: String },
}

/// Split Markdown into prose and open-term runs. A backtick pair wrapping an
/// open term (`` `{{entity_name}}` ``) belongs to the term.
#[must_use]
pub fn runs(markdown: &str) -> Vec<Run> {
    let mut out = Vec::new();
    let mut rest = markdown;
    while let Some(open) = rest.find("{{") {
        let Some(close) = rest[open + 2..].find("}}").map(|i| open + 2 + i) else {
            break;
        };
        let label = rest[open + 2..close].trim();
        if label.is_empty() {
            push_text(&mut out, &rest[..close + 2]);
            rest = &rest[close + 2..];
            continue;
        }
        let ticked = rest[..open].ends_with('`') && rest[close + 2..].starts_with('`');
        let (before, after) = if ticked {
            (&rest[..open - 1], close + 3)
        } else {
            (&rest[..open], close + 2)
        };
        push_text(&mut out, before);
        let question = label
            .split(|c: char| c == '.' || c == '|' || c.is_whitespace())
            .next()
            .unwrap_or(label)
            .to_string();
        out.push(Run::OpenTerm {
            label: label.to_string(),
            question,
        });
        rest = &rest[after..];
    }
    push_text(&mut out, rest);
    out
}

fn push_text(out: &mut Vec<Run>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(Run::Text(previous)) = out.last_mut() {
        previous.push_str(text);
    } else {
        out.push(Run::Text(text.to_string()));
    }
}

/// Inline Markdown as HTML, with no wrapping paragraph: `**Fee.** Due` is
/// `<strong>Fee.</strong> Due`.
#[must_use]
pub fn inline_html(markdown: &str) -> String {
    let html = markdown::render(markdown);
    let trimmed = html.trim();
    trimmed
        .strip_prefix("<p>")
        .and_then(|inner| inner.strip_suffix("</p>"))
        .filter(|inner| !inner.contains("<p>"))
        .map_or_else(|| trimmed.to_string(), str::to_string)
}

/// A parsed document as nested outline nodes, each holding the paragraphs,
/// holds, and quotations that sit beneath it. Indices point into
/// [`OutlineDocument::units`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutlineTree {
    /// Blocks before the first heading.
    pub preamble: Vec<usize>,
    pub sections: Vec<OutlineNode>,
}

/// One outline unit with its blocks and its children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineNode {
    pub unit: usize,
    pub blocks: Vec<usize>,
    pub children: Vec<OutlineNode>,
}

/// Nest a document's units by depth: an outline unit sits under the nearest
/// earlier outline unit of a shallower depth, and a block under the outline
/// unit before it.
#[must_use]
pub fn outline_tree(doc: &OutlineDocument) -> OutlineTree {
    fn close(stack: &mut Vec<(u8, OutlineNode)>, roots: &mut Vec<OutlineNode>) {
        if let Some((_, node)) = stack.pop() {
            match stack.last_mut() {
                Some((_, parent)) => parent.children.push(node),
                None => roots.push(node),
            }
        }
    }
    let mut tree = OutlineTree::default();
    let mut stack: Vec<(u8, OutlineNode)> = Vec::new();
    for (index, unit) in doc.units.iter().enumerate() {
        if unit.kind.is_outline() {
            while stack.last().is_some_and(|(depth, _)| *depth >= unit.depth) {
                close(&mut stack, &mut tree.sections);
            }
            stack.push((
                unit.depth,
                OutlineNode {
                    unit: index,
                    blocks: Vec::new(),
                    children: Vec::new(),
                },
            ));
        } else if let Some((_, node)) = stack.last_mut() {
            node.blocks.push(index);
        } else {
            tree.preamble.push(index);
        }
    }
    while !stack.is_empty() {
        close(&mut stack, &mut tree.sections);
    }
    tree
}

/// Project narration units into the canonical model shared with Word import.
/// The narration parser remains intentionally presentation-focused; this
/// projection gives callers a typed, ordered representation without creating
/// a second numbering vocabulary.
#[must_use]
pub fn canonical_model(doc: &OutlineDocument) -> word::CanonicalDocument {
    let blocks = doc
        .units
        .iter()
        .map(|unit| {
            let is_outline = !unit.marker.is_empty();
            let outline = is_outline.then(|| word::OutlineUnit {
                anchor: unit.anchor.clone(),
                depth: unit.depth,
                marker: unit.marker.clone(),
                path: unit.path.clone(),
                text: unit.markdown.clone(),
                // Narration Markdown has no OOXML list behind it. The
                // depth is real and the numbering identity is genuinely
                // absent, so these fields stay empty rather than carrying
                // an invented `w:numFmt` a caller might act on.
                list: word::ListIdentity {
                    numbering_id: String::new(),
                    abstract_numbering_id: None,
                    level: unit.depth.saturating_sub(1),
                    number_format: String::new(),
                    level_text: String::new(),
                    start: 1,
                    restart_level: None,
                    override_start: None,
                    style_id: None,
                },
                manual_label: None,
            });
            word::CanonicalBlock {
                anchor: unit.anchor.clone(),
                kind: if is_outline {
                    word::CanonicalBlockKind::Outline
                } else {
                    word::CanonicalBlockKind::Paragraph
                },
                text: unit.markdown.clone(),
                outline,
                manual_label: None,
                inlines: vec![word::CanonicalInline::Text {
                    text: unit.markdown.clone(),
                }],
                children: Vec::new(),
            }
        })
        .collect();
    word::CanonicalDocument {
        scheme: doc.scheme,
        stories: vec![word::CanonicalStory {
            kind: word::StoryKind::MainDocument,
            part_uri: "markdown".into(),
            blocks,
        }],
        diagnostics: Vec::new(),
    }
}

/// Inner stage markup: the article a page or standalone file wraps.
#[must_use]
pub fn stage_html(doc: &OutlineDocument) -> String {
    let mut out = String::from(
        "<article class=\"harvard-stage\" data-harvard-outline>\n\
         <header class=\"harvard-stage__chrome\">\n\
         <p class=\"harvard-stage__title\">",
    );
    out.push_str(&escape_text(&doc.title));
    out.push_str("</p>\n<p class=\"harvard-stage__counter\" data-harvard-counter></p>\n");
    out.push_str(
        "<p class=\"harvard-stage__hint\">Arrow keys or Space step through the document. \
         Click a paragraph to highlight it.</p>\n</header>\n<div class=\"harvard-doc\">\n",
    );
    for unit in &doc.units {
        out.push_str(&unit_html(unit));
    }
    out.push_str("</div>\n</article>\n");
    out
}

/// One unit as a focusable section.
#[must_use]
pub fn unit_html(unit: &Unit) -> String {
    let kind = unit.kind.class();
    let mut out = format!(
        "<section class=\"harvard-unit harvard-unit--depth-{depth} harvard-unit--{kind}\" \
         id=\"harvard-u-{index}\" tabindex=\"-1\" data-harvard-index=\"{index}\" \
         data-harvard-depth=\"{depth}\" data-harvard-path=\"{path}\" data-harvard-kind=\"{kind}\">\n",
        depth = unit.depth,
        kind = kind,
        index = unit.index,
        path = escape_attr(&unit.path),
    );
    if !unit.marker.is_empty() {
        out.push_str("<span class=\"harvard-unit__marker\" aria-hidden=\"true\">");
        out.push_str(&escape_text(&unit.marker));
        out.push_str("</span>\n");
    }
    if unit.kind == UnitKind::Heading {
        out.push_str("<h2>");
        out.push_str(&escape_text(&unit.markdown));
        out.push_str("</h2>\n");
    } else {
        out.push_str(&markdown::render(&unit.markdown));
    }
    out.push_str("</section>\n");
    out
}

fn push_unit(
    units: &mut Vec<Unit>,
    depth: u8,
    marker: String,
    path: String,
    kind: UnitKind,
    markdown: &str,
) {
    let markdown = markdown.trim().to_string();
    // An uncaptioned heading (`## I`) is still a unit a reader cites; only a
    // block with neither marker nor text is nothing.
    if markdown.is_empty() && marker.is_empty() {
        return;
    }
    units.push(Unit {
        index: units.len(),
        anchor: format!("markdown:unit:{}", units.len()),
        depth,
        marker,
        path,
        // Assigned once the whole document is read; see `assign_fragments`.
        fragment: String::new(),
        kind,
        markdown,
    });
}

/// An unlabeled block (paragraph, hold, or quotation) — no marker, no
/// distinct subsection.
fn push_block(units: &mut Vec<Unit>, depth: u8, path: String, kind: UnitKind, markdown: &str) {
    push_unit(units, depth, String::new(), path, kind, markdown);
}

fn join_path(prefix: &str, marker: &str) -> String {
    if prefix.is_empty() {
        marker.to_string()
    } else if marker.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}.{marker}")
    }
}

fn title_and_body(src: &str) -> (String, Option<&str>, &str) {
    let Some(after_open) = src.strip_prefix("---\n") else {
        return ("Untitled".to_string(), None, src);
    };
    let Some(end) = after_open.find("\n---\n") else {
        return ("Untitled".to_string(), None, src);
    };
    let yaml = &after_open[..end];
    let body = &after_open[end + "\n---\n".len()..];
    let title = yaml
        .lines()
        .find_map(|line| {
            line.strip_prefix("title:")
                .map(|rest| unquote(rest.trim()))
                .filter(|t| !t.is_empty())
        })
        .unwrap_or_else(|| "Untitled".to_string());
    (title, Some(yaml), body)
}

/// Read one flat top-level scalar key (`origin_url:`, `jurisdiction:`, …) out
/// of a template's raw frontmatter YAML, the same way [`title_and_body`]
/// reads `title:`. `None` when the key is absent or its value is empty.
#[must_use]
pub fn frontmatter_field(yaml: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    yaml.lines().find_map(|line| {
        line.strip_prefix(prefix.as_str())
            .map(|rest| unquote(rest.trim()))
            .filter(|value| !value.is_empty())
    })
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

enum Block {
    Heading {
        text: String,
        anchor: Option<String>,
    },
    Quote {
        paragraphs: Vec<String>,
        anchor: Option<String>,
    },
    Prose {
        text: String,
        anchor: Option<String>,
    },
}

fn blocks(body: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut lines = body.lines().peekable();
    let mut pending_anchor = None;
    while let Some(line) = lines.next() {
        if line.trim().is_empty() {
            continue;
        }
        // `word::notation` carries a canonical import's structural identity
        // in `navigator-*` comments. Narration needs only the source anchor
        // out of them; the rest is invisible to this presentation parser.
        if let Some(directive) = navigator_directive(line) {
            if let Some(anchor) = comment_attribute(directive, "anchor") {
                pending_anchor = Some(anchor);
            }
            continue;
        }
        if line.trim().eq_ignore_ascii_case("<!-- pagebreak -->") {
            pending_anchor = None;
            continue;
        }
        if let Some(heading) = heading_text(line) {
            out.push(Block::Heading {
                text: heading,
                anchor: pending_anchor.take(),
            });
            continue;
        }
        if line.starts_with('>') {
            let mut quote_lines = vec![strip_quote(line)];
            while matches!(lines.peek(), Some(next) if next.starts_with('>') || next.trim().is_empty())
            {
                let next = lines.next().expect("peeked line");
                if next.trim().is_empty() {
                    // A blank inside a quote run ends this quote paragraph group
                    // only when the following line is not a continuation quote.
                    if matches!(lines.peek(), Some(peek) if peek.starts_with('>')) {
                        quote_lines.push(String::new());
                    } else {
                        break;
                    }
                } else {
                    quote_lines.push(strip_quote(next));
                }
            }
            let paragraphs = quote_lines
                .split(String::is_empty)
                .map(|para| para.join("\n"))
                .filter(|p| !p.trim().is_empty())
                .collect();
            out.push(Block::Quote {
                paragraphs,
                anchor: pending_anchor.take(),
            });
            continue;
        }
        let mut prose = vec![line.to_string()];
        while matches!(lines.peek(), Some(next) if !next.trim().is_empty() && !next.starts_with('>') && heading_text(next).is_none())
        {
            prose.push(lines.next().expect("peeked line").to_string());
        }
        out.push(Block::Prose {
            text: prose.join("\n"),
            anchor: pending_anchor.take(),
        });
    }
    out
}

/// The body of a `<!-- navigator-… -->` comment occupying a whole line.
fn navigator_directive(line: &str) -> Option<&str> {
    line.trim()
        .strip_prefix("<!-- navigator-")?
        .strip_suffix("-->")
        .map(str::trim_end)
}

fn comment_attribute(directive: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let start = directive.find(&needle)? + needle.len();
    let rest = &directive[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn set_last_anchor(units: &mut [Unit], anchor: Option<String>) {
    if let (Some(unit), Some(anchor)) = (units.last_mut(), anchor) {
        unit.anchor = anchor;
    }
}

fn heading_text(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = trimmed.get(hashes..)?;
    if !rest.starts_with(' ') {
        return None;
    }
    Some(rest.trim().to_string())
}

fn strip_quote(line: &str) -> String {
    line.trim_start()
        .strip_prefix('>')
        .map_or(line, str::trim_start)
        .to_string()
}

fn parse_depth_one_heading(text: &str) -> Option<(DepthOneNumbering, String, String)> {
    if let Some(found) = decimal_heading(text) {
        return Some(found);
    }
    if let Some(marker) = bare_marker(text) {
        return if marker.chars().all(|c| c.is_ascii_digit()) {
            Some((DepthOneNumbering::Arabic, marker, String::new()))
        } else if is_canonical_roman(&marker) {
            Some((DepthOneNumbering::Roman, marker, String::new()))
        } else {
            None
        };
    }
    let (marker, rest) = split_marker(text)?;
    if is_roman(&marker) {
        Some((DepthOneNumbering::Roman, marker, rest))
    } else if marker.chars().all(|c| c.is_ascii_digit()) {
        Some((DepthOneNumbering::Arabic, marker, rest))
    } else {
        None
    }
}

/// A `1.1 Caption` or `Section 1.01 Caption` heading, with or without a
/// period after the number. The marker keeps the word `Section`.
fn decimal_heading(text: &str) -> Option<(DepthOneNumbering, String, String)> {
    let text = text.trim();
    let (numbering, rest) = match text.strip_prefix("Section ") {
        Some(rest) => (DepthOneNumbering::Section, rest.trim_start()),
        None => (DepthOneNumbering::Decimal, text),
    };
    let token = rest.split_whitespace().next()?;
    let caption = rest[token.len()..].trim().to_string();
    let number = token.strip_suffix('.').unwrap_or(token);
    let (major, minor) = number.split_once('.')?;
    let digits = |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit());
    if !digits(major) || !digits(minor) {
        return None;
    }
    let marker = match numbering {
        DepthOneNumbering::Section => format!("Section {number}"),
        _ => number.to_string(),
    };
    Some((numbering, marker, caption))
}

fn parse_capital_letter(text: &str) -> Option<(String, String)> {
    let (marker, rest) = split_heading_marker(text)?;
    let mut chars = marker.chars();
    let c = chars.next()?;
    if chars.next().is_none() && c.is_ascii_uppercase() {
        Some((marker, rest))
    } else {
        None
    }
}

/// `**A. Label.** body` — the engagement-letter subsection form.
fn parse_bold_letter_lead(para: &str) -> Option<(String, String)> {
    let trimmed = para.trim();
    let rest = trimmed.strip_prefix("**")?;
    let closing = rest.find("**")?;
    let bold = &rest[..closing];
    let (marker, label) = split_marker(bold)?;
    let mut chars = marker.chars();
    let c = chars.next()?;
    if chars.next().is_some() || !c.is_ascii_uppercase() {
        return None;
    }
    let body = rest[closing + 2..].trim();
    let text = if body.is_empty() {
        label
    } else {
        format!("{label} {body}")
    };
    Some((marker, text))
}

/// The marker leading a quoted subsection, and the depth its group sits at.
///
/// The root scheme is not an input. Only depth one changes glyph between a
/// contract and a motion — `I.` against `1.` — and depth one is a heading,
/// never a quote, so every marker reaching this function belongs to one of
/// the six groups that read the same under both roots. `1.` here is the
/// depth-three decimal and `(1)` the depth-five one whichever root the
/// document declared.
fn parse_deeper_lead(para: &str) -> Option<(String, String, u8)> {
    let trimmed = para.trim().trim_start_matches('*').trim_start();
    if let Some(rest) = trimmed.strip_prefix('(') {
        let close = rest.find(')')?;
        let inner = &rest[..close];
        let after = rest[close + 1..].trim().to_string();
        if inner.chars().all(|c| c.is_ascii_digit()) {
            return Some((format!("({inner})"), after, 5));
        }
        // A single lower letter is the depth-six group. `(i)`, `(v)`, and
        // `(x)` are also lower roman numerals; the caller settles those from
        // the unit before them (`disambiguate_roman`). A longer roman
        // (`(ii)`, `(iv)`) is depth seven.
        let mut chars = inner.chars();
        if chars.next().is_some_and(|c| c.is_ascii_lowercase()) && chars.next().is_none() {
            return Some((format!("({inner})"), after, 6));
        }
        if inner.chars().all(|c| c.is_ascii_lowercase()) && is_roman(&inner.to_ascii_uppercase()) {
            return Some((format!("({inner})"), after, 7));
        }
        if inner.chars().all(|c| c.is_ascii_lowercase()) {
            return Some((format!("({inner})"), after, 6));
        }
        return None;
    }
    let (marker, rest) = split_marker(trimmed)?;
    if marker.chars().all(|c| c.is_ascii_lowercase()) && marker.len() == 1 {
        return Some((marker, rest, 4));
    }
    if marker.chars().all(|c| c.is_ascii_digit()) {
        return Some((marker, rest, 3));
    }
    None
}

/// A heading read as a depth-1 marker, or, when it continues the lettering
/// at depth 2, as that letter instead (see [`continued_letter`]).
type DepthOneHeading = Option<(DepthOneNumbering, String, String)>;
fn classify_heading(
    text: &str,
    last_letter: Option<char>,
) -> (DepthOneHeading, Option<(String, String)>) {
    match continued_letter(text, last_letter) {
        Some(letter) => (None, Some(letter)),
        None => (parse_depth_one_heading(text), None),
    }
}

/// A heading's capital-letter marker when it continues the lettering at
/// depth 2: a lone `C`, `D`, `I`, `L`, `M`, `V`, or `X` is the next letter
/// after its predecessor (`III.C` after `III.B`), and a numeral otherwise.
fn continued_letter(text: &str, last_letter: Option<char>) -> Option<(String, String)> {
    let (marker, rest) = split_heading_marker(text)?;
    let mut chars = marker.chars();
    let letter = chars.next()?;
    let previous = last_letter?;
    (chars.next().is_none()
        && letter.is_ascii_uppercase()
        && u32::from(letter) == u32::from(previous) + 1)
        .then_some((marker, rest))
}

/// A heading's marker and caption, including an uncaptioned heading that
/// carries its marker alone (`## I`, `### A.`).
fn split_heading_marker(text: &str) -> Option<(String, String)> {
    bare_marker(text).map_or_else(
        || split_marker(text),
        |marker| Some((marker, String::new())),
    )
}

/// The marker of a heading that carries nothing else: one alphanumeric
/// token, with or without its period.
fn bare_marker(text: &str) -> Option<String> {
    let text = text.trim();
    let marker = text.strip_suffix('.').unwrap_or(text);
    (!marker.is_empty() && marker.chars().all(|c| c.is_ascii_alphanumeric()))
        .then(|| marker.to_string())
}

/// A roman numeral in the one spelling its value has, so a word made only of
/// roman digits (`CIVIL`, `DID`) is not read as a marker.
fn is_canonical_roman(s: &str) -> bool {
    const PAIRS: [(u32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    if !is_roman(s) {
        return false;
    }
    let digit = |c: char| match c {
        'I' => 1,
        'V' => 5,
        'X' => 10,
        'L' => 50,
        'C' => 100,
        'D' => 500,
        _ => 1000,
    };
    let mut value = 0_u32;
    let mut previous = 0_u32;
    for c in s.chars().rev() {
        let d = digit(c);
        if d < previous {
            value = value.saturating_sub(d);
        } else {
            value += d;
            previous = d;
        }
    }
    let mut canonical = String::new();
    let mut rest = value;
    for (step, glyph) in PAIRS {
        while rest >= step {
            canonical.push_str(glyph);
            rest -= step;
        }
    }
    canonical == s
}

fn split_marker(text: &str) -> Option<(String, String)> {
    let trimmed = text.trim();
    let dot = trimmed.find('.')?;
    if dot == 0 {
        return None;
    }
    let marker = trimmed[..dot].to_string();
    if marker.chars().any(|c| !(c.is_ascii_alphanumeric())) {
        return None;
    }
    let rest = trimmed[dot + 1..].trim().to_string();
    Some((marker, rest))
}

fn is_roman(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| matches!(c, 'I' | 'V' | 'X' | 'L' | 'C' | 'D' | 'M'))
}

fn escape_text(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(raw: &str) -> String {
    escape_text(raw)
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Synthetic motion-practice body shared by the lawyer stage and CLI tests.
pub const SAMPLE_MOTION: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/sample_motion.md"
));

#[cfg(test)]
mod tests {
    use super::{
        canonical_model, frontmatter_field, hold_body, inline_html, outline_tree, parse, runs,
        stage_html, DepthOneNumbering, DepthOneScheme, Run, UnitKind,
    };

    const ONBOARDING: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../templates/notations/neon_law/onboarding__letter.md"
    ));
    const OFFBOARDING: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../templates/notations/neon_law/offboarding__letter.md"
    ));

    #[test]
    fn roman_headings_and_lettered_quotes() {
        let doc = parse(
            "---\ntitle: Sample\n---\n\nDear client:\n\n## I. Scope\n\nThe firm acts.\n\n\
             ## II. Fees\n\n> **A. Invoices.** Read them.\n>\n> **B. Costs.** Passed through.\n",
        );
        assert_eq!(doc.title, "Sample");
        assert_eq!(doc.scheme, Some(DepthOneScheme::Roman));
        let headings: Vec<_> = doc
            .units
            .iter()
            .filter(|u| u.kind == UnitKind::Heading)
            .map(|u| (u.marker.as_str(), u.depth, u.markdown.as_str()))
            .collect();
        assert_eq!(headings, vec![("I", 1, "Scope"), ("II", 1, "Fees")]);
        let subs: Vec<_> = doc
            .units
            .iter()
            .filter(|u| u.kind == UnitKind::Subsection)
            .map(|u| (u.path.as_str(), u.marker.as_str(), u.depth))
            .collect();
        assert_eq!(subs, vec![("II.A", "A", 2), ("II.B", "B", 2)]);
        let preamble = doc
            .units
            .iter()
            .find(|u| u.markdown.contains("Dear client"))
            .expect("preamble");
        assert_eq!(preamble.depth, 0);
        let under_scope = doc
            .units
            .iter()
            .find(|u| u.markdown.contains("The firm acts"))
            .expect("body under I");
        assert_eq!(under_scope.depth, 1);
        assert_eq!(under_scope.path, "I");
    }

    #[test]
    fn marker_only_headings_are_uncaptioned_units() {
        let doc = parse(
            "## I\n\nThe Parties agree.\n\n### A\n\nSubject to.\n\n## II.\n\nThe Parties will.\n\n\
             ## IV. Governing Law\n\nThis Agreement.\n",
        );
        assert_eq!(doc.scheme, Some(DepthOneScheme::Roman));
        let headings: Vec<_> = doc
            .units
            .iter()
            .filter(|u| u.kind == UnitKind::Heading)
            .map(|u| (u.path.as_str(), u.depth, u.markdown.as_str()))
            .collect();
        assert_eq!(
            headings,
            vec![
                ("I", 1, ""),
                ("I.A", 2, ""),
                ("II", 1, ""),
                ("IV", 1, "Governing Law"),
            ]
        );
    }

    #[test]
    fn a_lone_roman_letter_after_its_predecessor_is_a_letter() {
        // III.C follows III.B, so `C` is the third letter, not the numeral 100;
        // a `C` with no `B` before it at that depth stays a numeral.
        let doc = parse(
            "## III. Terms\n\n### A\n\nText.\n\n### B. Price\n\nText.\n\n### C\n\nText.\n\n\
             ### D. Delivery\n\nText.\n\n## IV\n\nText.\n",
        );
        let paths: Vec<_> = doc
            .units
            .iter()
            .filter(|u| u.kind == UnitKind::Heading)
            .map(|u| (u.path.as_str(), u.depth))
            .collect();
        assert_eq!(
            paths,
            vec![
                ("III", 1),
                ("III.A", 2),
                ("III.B", 2),
                ("III.C", 2),
                ("III.D", 2),
                ("IV", 1),
            ]
        );
        let numeral = parse("## I. Scope\n\nText.\n\n## V\n\nText.\n");
        let v = numeral.units.iter().find(|u| u.marker == "V").expect("V");
        assert_eq!((v.depth, v.path.as_str()), (1, "V"));
    }

    #[test]
    fn a_one_word_heading_is_not_mistaken_for_a_marker() {
        let doc = parse("## CIVIL\n\nText.\n\n## XCX\n\nText.\n\n## Recitals\n\nText.\n");
        assert!(
            doc.units.iter().all(|u| u.marker.is_empty()),
            "{:?}",
            doc.units
        );
        assert_eq!(doc.scheme, None);
    }

    #[test]
    fn parse_extracts_the_frontmatter_yaml_verbatim() {
        let doc = parse("---\ntitle: Sample\ncode: sample__doc\n---\n\nBody.\n");
        assert_eq!(
            doc.frontmatter.as_deref(),
            Some("title: Sample\ncode: sample__doc")
        );
    }

    #[test]
    fn a_document_with_no_frontmatter_has_none() {
        let doc = parse("Just a body, no frontmatter fences.\n");
        assert_eq!(doc.frontmatter, None);
    }

    #[test]
    fn frontmatter_field_reads_a_flat_scalar_key() {
        let yaml = "kind: filing\ntitle: Nevada LLC Formation\norigin_url: https://example.gov/forms\njurisdiction: NV";
        assert_eq!(
            frontmatter_field(yaml, "origin_url").as_deref(),
            Some("https://example.gov/forms")
        );
        assert_eq!(
            frontmatter_field(yaml, "jurisdiction").as_deref(),
            Some("NV")
        );
        assert_eq!(frontmatter_field(yaml, "no_such_key"), None);
    }

    #[test]
    fn motion_practice_uses_arabic_depth_one() {
        let doc = parse(
            "## 1. Introduction\n\nFacts follow.\n\n## 2. Argument\n\n\
             > **A. Standard.** De novo.\n",
        );
        assert_eq!(doc.scheme, Some(DepthOneScheme::Arabic));
        assert_eq!(doc.units[0].marker, "1");
        assert_eq!(doc.units[0].depth, 1);
        assert_eq!(doc.units[1].depth, 1);
        assert_eq!(doc.units[1].path, "1");
        let sub = doc
            .units
            .iter()
            .find(|u| u.kind == UnitKind::Subsection)
            .expect("A");
        assert_eq!(sub.path, "2.A");
        assert_eq!(sub.depth, 2);
        assert_eq!(doc.canonical_model().scheme, Some(DepthOneScheme::Arabic));
    }

    #[test]
    fn the_bundled_onboarding_letter_is_a_roman_outline() {
        let doc = parse(ONBOARDING);
        assert_eq!(doc.title, "Onboarding Letter");
        assert_eq!(doc.scheme, Some(DepthOneScheme::Roman));
        let markers: Vec<_> = doc
            .units
            .iter()
            .filter(|u| u.kind == UnitKind::Heading)
            .map(|u| u.marker.as_str())
            .collect();
        assert_eq!(
            markers,
            vec![
                "I", "II", "III", "IV", "V", "VI", "VII", "VIII", "IX", "X", "XI", "XII", "XIII",
                "XIV",
            ]
        );
        // The flat-fee letter labels its parts with bold run-ins rather than
        // the lettered blockquotes the hourly letter carried, so no unit sits
        // deeper than 1. Preamble before § I is depth 0.
        // `stage_html_nests_a_lettered_subsection` holds the depth-2 rendering.
        assert!(doc.units.iter().any(|u| u.depth == 0));
        assert!(doc.units.iter().all(|u| u.depth <= 1));
        let html = stage_html(&doc);
        assert!(html.contains("data-harvard-outline"));
        assert!(html.contains("data-harvard-path=\"I\""));
        assert!(html.contains("harvard-unit--depth-1"));
        assert!(html.contains("Scope"));
    }

    /// Depth-2 staging, which the bundled letters do not exercise: a
    /// lettered blockquote under a roman section nests one level and carries
    /// the `<section>.<letter>` path into the rendered markup.
    #[test]
    fn stage_html_nests_a_lettered_subsection() {
        let doc = parse(
            "---\ntitle: Sample\n---\n\nDear client:\n\n## I. Scope\n\nThe firm acts.\n\n\
             ## II. Fees\n\n> **A. Invoices.** Read them.\n>\n> **B. Costs.** Passed through.\n",
        );
        assert!(doc.units.iter().any(|u| u.path == "II.A" && u.depth == 2));
        assert!(doc.units.iter().any(|u| u.path == "II.B" && u.depth == 2));
        let html = stage_html(&doc);
        assert!(html.contains("data-harvard-path=\"II.A\""));
        assert!(html.contains("harvard-unit--depth-1"));
        assert!(html.contains("harvard-unit--depth-2"));
    }

    #[test]
    fn narration_carries_the_source_anchors_of_a_canonical_word_import() {
        // The canonical projection is `word::notation`; narration is the
        // presentation stage downstream of it. What narration owes the
        // import is the source anchor, so an edit made here still names the
        // same clause in the imported document.
        let document = word::CanonicalDocument {
            scheme: Some(DepthOneScheme::Roman),
            stories: vec![word::CanonicalStory {
                kind: word::StoryKind::MainDocument,
                part_uri: "/word/document.xml".into(),
                blocks: vec![outline_block(
                    "/word/document.xml:paragraph:7A",
                    1,
                    "I",
                    "Term",
                )],
            }],
            diagnostics: Vec::new(),
        };

        let narrated = parse(&document.to_markdown());

        assert_eq!(narrated.scheme, Some(DepthOneScheme::Roman));
        assert_eq!(
            narrated
                .units
                .iter()
                .map(|unit| unit.anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["/word/document.xml:paragraph:7A"]
        );
        assert_eq!(narrated.units[0].path, "I");
    }

    #[test]
    fn narration_units_that_never_saw_word_still_carry_a_stable_anchor() {
        let doc = parse("# I. First\n\n# II. Second\n");
        assert_eq!(doc.units[0].anchor, "markdown:unit:0");
        assert_eq!(doc.units[1].anchor, "markdown:unit:1");
        assert_eq!(
            canonical_model(&doc).stories[0].blocks[1].anchor,
            "markdown:unit:1"
        );
    }

    fn outline_block(anchor: &str, depth: u8, marker: &str, text: &str) -> word::CanonicalBlock {
        word::CanonicalBlock {
            anchor: anchor.into(),
            kind: word::CanonicalBlockKind::Outline,
            text: text.into(),
            outline: Some(word::OutlineUnit {
                anchor: anchor.into(),
                depth,
                marker: marker.into(),
                path: marker.into(),
                text: text.into(),
                list: word::ListIdentity {
                    numbering_id: "1".into(),
                    abstract_numbering_id: None,
                    level: depth - 1,
                    number_format: "upperRoman".into(),
                    level_text: format!("%{depth}."),
                    start: 1,
                    restart_level: None,
                    override_start: None,
                    style_id: None,
                },
                manual_label: None,
            }),
            manual_label: None,
            inlines: vec![word::CanonicalInline::Text { text: text.into() }],
            children: Vec::new(),
        }
    }

    /// A canonical import carrying every Harvard depth, under each root,
    /// projected through `word::notation` exactly as the import writes it.
    fn seven_depth_import(scheme: DepthOneScheme) -> word::CanonicalDocument {
        let root = if scheme == DepthOneScheme::Roman {
            "I"
        } else {
            "1"
        };
        let mut path = root.to_string();
        let mut blocks = vec![outline_path_block(
            "/word/document.xml:paragraph:1",
            1,
            root,
            root,
            "Root clause",
        )];
        for (index, marker) in ["A", "1", "a", "(1)", "(a)", "(i)"].iter().enumerate() {
            let depth = u8::try_from(index).unwrap_or_default() + 2;
            path = format!("{path}.{marker}");
            blocks.push(outline_path_block(
                &format!("/word/document.xml:paragraph:{depth}"),
                depth,
                marker,
                &path,
                &format!("Clause at depth {depth}"),
            ));
        }
        word::CanonicalDocument {
            scheme: Some(scheme),
            stories: vec![word::CanonicalStory {
                kind: word::StoryKind::MainDocument,
                part_uri: "/word/document.xml".into(),
                blocks,
            }],
            diagnostics: Vec::new(),
        }
    }

    fn outline_path_block(
        anchor: &str,
        depth: u8,
        marker: &str,
        path: &str,
        text: &str,
    ) -> word::CanonicalBlock {
        let mut block = outline_block(anchor, depth, marker, text);
        if let Some(unit) = block.outline.as_mut() {
            unit.path = path.into();
            unit.list.level_text = format!("%{depth}.");
        }
        block
    }

    /// The compatibility ENG-577 asks the narration side to prove: every
    /// Harvard depth, under both roots, read back by the presentation stage
    /// from what the canonical emitter actually writes. The one-depth anchor
    /// test above stays as the smoke case; this is the breadth behind it.
    #[test]
    fn narration_reads_every_depth_of_both_roots_from_a_canonical_import() {
        for (scheme, root) in [(DepthOneScheme::Roman, "I"), (DepthOneScheme::Arabic, "1")] {
            let imported = seven_depth_import(scheme);
            let narrated = parse(&imported.to_markdown());

            assert_eq!(narrated.scheme, Some(scheme));
            assert_eq!(narrated.units.len(), 7, "{scheme:?}");

            // Every unit still names the clause it came from.
            assert_eq!(
                narrated
                    .units
                    .iter()
                    .map(|unit| unit.anchor.as_str())
                    .collect::<Vec<_>>(),
                (1..=7)
                    .map(|index| format!("/word/document.xml:paragraph:{index}"))
                    .collect::<Vec<_>>(),
                "{scheme:?} anchors",
            );

            // Depths two through seven are one vocabulary under both roots;
            // only the root glyph changes.
            assert_eq!(
                narrated
                    .units
                    .iter()
                    .map(|unit| unit.marker.as_str())
                    .collect::<Vec<_>>(),
                vec![root, "A", "1", "a", "(1)", "(a)", "(i)"],
                "{scheme:?} markers",
            );

            let mut expected = vec![root.to_string()];
            for marker in ["A", "1", "a", "(1)", "(a)", "(i)"] {
                let parent = expected.last().cloned().unwrap_or_default();
                expected.push(format!("{parent}.{marker}"));
            }
            assert_eq!(
                narrated
                    .units
                    .iter()
                    .map(|unit| unit.path.clone())
                    .collect::<Vec<_>>(),
                expected,
                "{scheme:?} paths",
            );

            // The canonical paths are the same strings, so the narration
            // stage and the import agree on where every clause sits.
            assert_eq!(
                imported.stories[0]
                    .blocks
                    .iter()
                    .filter_map(|block| block.outline.as_ref())
                    .map(|unit| unit.path.clone())
                    .collect::<Vec<_>>(),
                expected,
                "{scheme:?} canonical paths",
            );

            // `(i)` is the ninth lower letter at depth six and the first
            // lower roman at depth seven. After `(a)` it cannot continue the
            // lettering (that would need `(h)` before it), so the stage reads
            // it as the depth-seven numeral.
            assert_eq!(
                narrated
                    .units
                    .iter()
                    .map(|unit| unit.depth)
                    .collect::<Vec<_>>(),
                vec![1, 2, 3, 4, 5, 6, 7],
                "{scheme:?} depths",
            );
        }
    }

    #[test]
    fn the_bundled_offboarding_letter_is_a_roman_outline() {
        let doc = parse(OFFBOARDING);
        assert_eq!(doc.title, "Closing Letter");
        assert_eq!(doc.scheme, Some(DepthOneScheme::Roman));
        let markers: Vec<_> = doc
            .units
            .iter()
            .filter(|u| u.kind == UnitKind::Heading)
            .map(|u| u.marker.as_str())
            .collect();
        assert_eq!(markers, vec!["I", "II", "III", "IV", "V"]);
        let html = stage_html(&doc);
        assert!(html.contains("data-harvard-outline"));
        assert!(html.contains("data-harvard-path=\"I\""));
        assert!(html.contains("harvard-unit--depth-1"));
        assert!(html.contains("Representation concluded"));
    }

    #[test]
    fn unlabeled_paragraphs_share_the_section_highlight_depth() {
        let doc = parse("## I. One\n\nFirst.\n\nSecond.\n");
        assert_eq!(doc.units.len(), 3);
        assert!(doc.units[1..].iter().all(|u| u.depth == 1 && u.path == "I"));
    }

    #[test]
    fn the_sample_motion_is_an_arabic_outline() {
        let doc = parse(super::SAMPLE_MOTION);
        assert_eq!(doc.title, "Sample Motion");
        assert_eq!(doc.scheme, Some(DepthOneScheme::Arabic));
        let headings: Vec<_> = doc
            .units
            .iter()
            .filter(|u| u.kind == UnitKind::Heading)
            .map(|u| u.path.as_str())
            .collect();
        assert_eq!(headings, vec!["1", "2", "3"]);
        assert!(doc.units.iter().any(|u| u.path == "2.A" && u.depth == 2));
        assert!(doc.units.iter().any(|u| u.path == "2.B" && u.depth == 2));
    }

    fn fragments(src: &str) -> Vec<(String, String)> {
        parse(src)
            .units
            .into_iter()
            .map(|unit| (unit.path, unit.fragment))
            .collect()
    }

    #[test]
    fn each_depth_one_scheme_is_named_and_addressed() {
        let cases = [
            (
                "## I. Scope\n\nText.\n\n## II. Fees\n\nText.\n",
                DepthOneNumbering::Roman,
                DepthOneScheme::Roman,
                vec!["I", "II"],
                vec!["section-i", "section-ii"],
            ),
            (
                "## 1. Introduction\n\nText.\n\n## 2. Argument\n\nText.\n",
                DepthOneNumbering::Arabic,
                DepthOneScheme::Arabic,
                vec!["1", "2"],
                vec!["section-1", "section-2"],
            ),
            (
                "## 1.1 Definitions\n\nText.\n\n## 1.2. Term\n\nText.\n",
                DepthOneNumbering::Decimal,
                DepthOneScheme::Arabic,
                vec!["1.1", "1.2"],
                vec!["section-1-1", "section-1-2"],
            ),
            (
                "## Section 1.01 Definitions\n\nText.\n\n## Section 1.02 Term\n\nText.\n",
                DepthOneNumbering::Section,
                DepthOneScheme::Arabic,
                vec!["Section 1.01", "Section 1.02"],
                vec!["section-1-01", "section-1-02"],
            ),
        ];
        for (src, numbering, scheme, markers, ids) in cases {
            let doc = parse(src);
            assert_eq!(doc.numbering, Some(numbering), "{src}");
            assert_eq!(doc.scheme, Some(scheme), "{src}");
            let headings: Vec<_> = doc
                .units
                .iter()
                .filter(|u| u.kind == UnitKind::Heading)
                .collect();
            assert_eq!(
                headings
                    .iter()
                    .map(|u| u.marker.as_str())
                    .collect::<Vec<_>>(),
                markers,
                "{src}"
            );
            assert!(headings.iter().all(|u| u.depth == 1), "{src}");
            assert_eq!(
                headings
                    .iter()
                    .map(|u| u.fragment.as_str())
                    .collect::<Vec<_>>(),
                ids,
                "{src}"
            );
            assert_eq!(numbering.scheme(), scheme);
            assert!(!numbering.label().is_empty());
        }
    }

    #[test]
    fn parenthetical_siblings_share_their_parent_path() {
        let doc = parse(
            "## Section 1.01 Definitions\n\n> **(a) Affiliate.** Controls.\n>\n\
             > **(b) Business Day.** Weekday.\n\n### (c) Closing\n\nThe day.\n",
        );
        let paths: Vec<_> = doc
            .units
            .iter()
            .filter(|u| u.kind.is_outline())
            .map(|u| (u.path.as_str(), u.fragment.as_str()))
            .collect();
        assert_eq!(
            paths,
            vec![
                ("1.01", "section-1-01"),
                ("1.01.(a)", "section-1-01-a"),
                ("1.01.(b)", "section-1-01-b"),
                ("1.01.(c)", "section-1-01-c"),
            ]
        );
    }

    #[test]
    fn a_bracketed_paragraph_is_a_hold_and_a_blank_is_not() {
        let doc = parse("## I. Venue\n\n[Confirm the venue with the client.]\n\n[blank]\n");
        let kinds: Vec<_> = doc.units.iter().map(|u| u.kind).collect();
        assert_eq!(
            kinds,
            vec![UnitKind::Heading, UnitKind::Hold, UnitKind::Paragraph]
        );
        assert_eq!(
            hold_body(&doc.units[1].markdown),
            Some("Confirm the venue with the client.")
        );
        for not_a_hold in ["[...]", "[…]", "[ ]", "[the link](/design)", "Plain."] {
            assert_eq!(hold_body(not_a_hold), None, "{not_a_hold}");
        }
        let html = stage_html(&doc);
        assert!(html.contains("harvard-unit--hold"), "{html}");
    }

    #[test]
    fn an_unmarked_quote_is_a_quotation_and_a_continued_subsection_is_not() {
        let doc = parse(
            "## 1. Argument\n\n> The standard is de novo.\n\n\
             > **A. Standard.** Review.\n>\n> More on review.\n",
        );
        let kinds: Vec<_> = doc
            .units
            .iter()
            .map(|u| (u.kind, u.path.as_str()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (UnitKind::Heading, "1"),
                (UnitKind::Quotation, "1"),
                (UnitKind::Subsection, "1.A"),
                (UnitKind::Paragraph, "1.A"),
            ]
        );
    }

    #[test]
    fn open_terms_split_out_of_prose_with_their_question() {
        assert_eq!(
            runs("Signed by `{{person__client.name}}` on {{ custom_datetime__signed }}."),
            vec![
                Run::Text("Signed by ".into()),
                Run::OpenTerm {
                    label: "person__client.name".into(),
                    question: "person__client".into(),
                },
                Run::Text(" on ".into()),
                Run::OpenTerm {
                    label: "custom_datetime__signed".into(),
                    question: "custom_datetime__signed".into(),
                },
                Run::Text(".".into()),
            ]
        );
        assert_eq!(runs("No terms."), vec![Run::Text("No terms.".into())]);
        assert_eq!(
            runs("Empty {{}} stays."),
            vec![Run::Text("Empty {{}} stays.".into())]
        );
        assert_eq!(runs("Unclosed {{x"), vec![Run::Text("Unclosed {{x".into())]);
        assert!(runs("").is_empty());
    }

    #[test]
    fn inline_html_drops_the_wrapping_paragraph() {
        assert_eq!(inline_html("**Fee.** Due"), "<strong>Fee.</strong> Due");
    }

    #[test]
    fn fragments_follow_the_harvard_path_and_survive_a_reparse() {
        let src = "Dear client:\n\n## I. Scope\n\nFirst.\n\nSecond.\n\n\
                   ## II. Fees\n\n> **A. Invoices.** Read them.\n>\n> Later.\n\n\
                   ## Signatures\n\nSigned.\n";
        let first = fragments(src);
        assert_eq!(
            first
                .iter()
                .map(|(_, fragment)| fragment.as_str())
                .collect::<Vec<_>>(),
            vec![
                "preamble-p1",
                "section-i",
                "section-i-p1",
                "section-i-p2",
                "section-ii",
                "section-ii-a",
                "section-ii-a-p1",
                "heading-signatures",
                "heading-signatures-p1",
            ]
        );
        assert_eq!(first, fragments(src), "the same source yields the same ids");

        // A paragraph inserted under I moves no other unit's id.
        let inserted = fragments(&src.replace("First.\n\n", "New.\n\nFirst.\n\n"));
        for (path, fragment) in first.iter().filter(|(_, f)| !f.starts_with("section-i-p")) {
            assert!(
                inserted.iter().any(|(p, f)| p == path && f == fragment),
                "{fragment} moved"
            );
        }
    }

    #[test]
    fn a_repeated_path_takes_a_numeric_suffix() {
        let ids: Vec<_> = fragments("## I. One\n\n## I. Again\n")
            .into_iter()
            .map(|(_, f)| f)
            .collect();
        assert_eq!(ids, vec!["section-i", "section-i-2"]);
    }

    #[test]
    fn the_tree_nests_units_by_depth_with_their_blocks() {
        let doc = parse(
            "Preamble.\n\n## I. Scope\n\nBody.\n\n> **A. Part.** Text.\n>\n> **B. Part.** Text.\n\n\
             ## II. Fees\n\n[Hold.]\n",
        );
        let tree = outline_tree(&doc);
        assert_eq!(tree.preamble, vec![0]);
        assert_eq!(tree.sections.len(), 2);
        let scope = &tree.sections[0];
        assert_eq!(doc.units[scope.unit].path, "I");
        assert_eq!(scope.blocks, vec![2]);
        assert_eq!(
            scope
                .children
                .iter()
                .map(|c| doc.units[c.unit].path.as_str())
                .collect::<Vec<_>>(),
            vec!["I.A", "I.B"]
        );
        let fees = &tree.sections[1];
        assert_eq!(doc.units[fees.blocks[0]].kind, UnitKind::Hold);
    }
}
