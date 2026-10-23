//! A Project manifest's `pages:` block — the [Matter
//! Pages](../../docs/glossary/matter-page.md) a Project declares.
//!
//! Each entry names one page of the matter: a URL `slug`, a dashboard `kind`
//! (see [`crate::dashboard`]), a `title`, an optional `draft` path, and the
//! per-lens section lists under `lenses:`. The entry is the only place a
//! matter page is composed; a Markdown file that declares a dashboard kind is
//! refused by [`crate::S103KindEnum`] with a pointer here.
//!
//! This module is the one reader of the block. `Y021` (in the CLI's manifest
//! pass) checks its shape through [`shape_problems`]; the D-family rules
//! check each entry's composition through [`declared_pages`]; `project sync`
//! stores the entries it reads through [`entries`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::dashboard::{Lens, Section};
use crate::kind::Kind;
use crate::{line_byte_range, SourceFile, Violation};

/// The top-level manifest key.
pub const KEY: &str = "pages";

/// The keys a `pages:` entry may carry.
pub const ENTRY_KEYS: &[&str] = &["slug", "kind", "title", "draft", "lenses"];

/// The longest slug a page may take, so a URL segment stays readable.
pub const MAX_SLUG_LEN: usize = 48;

/// The tab label of the matter's own show page, `/app/projects/{code}`,
/// which leads every page strip. A declared page titled the same would put
/// two identical tabs side by side, so `Y021` refuses it.
pub const SHOW_PAGE_TITLE: &str = "Overview";

/// Path segments under `/app/projects/{code}/` that a route already owns.
/// A page slug that matched one would be shadowed by that route, so `Y021`
/// refuses it by name.
pub const RESERVED_SLUGS: &[&str] = &[
    "close",
    "contract-review",
    "conversation",
    "documents",
    "edit",
    "inbox",
    "intake",
    "notations",
    "people",
    "portal",
    "review",
    "testimonial",
    "view-as-client",
];

/// One `pages:` entry as authored: names are kept as written so a rule can
/// report the offending text. Lens keys are sorted; section order within a
/// lens is the order the page renders in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageEntry {
    pub slug: String,
    pub kind: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<String>,
    #[serde(default)]
    pub lenses: BTreeMap<String, Vec<String>>,
}

impl PageEntry {
    /// The declared dashboard kind, or `None` when `kind` names something
    /// else.
    #[must_use]
    pub fn dashboard_kind(&self) -> Option<Kind> {
        Kind::parse(&self.kind).filter(|kind| kind.is_dashboard())
    }

    /// The sections declared for `lens`, in declared order, skipping any
    /// name outside the vocabulary. `None` when the page declares no such
    /// lens, which is how a reader without that lens finds no page.
    #[must_use]
    pub fn sections_for(&self, lens: Lens) -> Option<Vec<Section>> {
        self.lenses
            .get(lens.as_str())
            .map(|names| names.iter().filter_map(|n| Section::parse(n)).collect())
    }
}

/// Whether `slug` is a lowercase, hyphen-separated URL segment.
#[must_use]
pub fn is_valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= MAX_SLUG_LEN
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--")
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A `pages:` entry whose `kind:` is a dashboard kind, located in the file
/// for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredPage {
    pub slug: String,
    pub kind: Kind,
    /// `None` when the entry has no `lenses:` mapping (D004 reports it);
    /// otherwise each lens key and its section names as written.
    pub lenses: Option<Vec<(String, Vec<String>)>>,
    /// The 1-based line the entry starts on.
    pub line: usize,
    /// The 1-based line the next entry starts on, or past the block's end.
    pub end_line: usize,
}

impl DeclaredPage {
    /// The line inside this entry that mentions `name`, falling back to the
    /// entry's first line.
    #[must_use]
    pub fn line_of(&self, contents: &str, name: &str) -> usize {
        contents
            .lines()
            .enumerate()
            .skip(self.line.saturating_sub(1))
            .take(self.end_line.saturating_sub(self.line))
            .find(|(_, line)| line.contains(name))
            .map_or(self.line, |(idx, _)| idx + 1)
    }
}

fn pages_sequence(contents: &str) -> Option<Vec<serde_yaml::Value>> {
    let document: serde_yaml::Value = serde_yaml::from_str(contents).ok()?;
    document.as_mapping()?.get(KEY)?.as_sequence().cloned()
}

/// The 1-based line each top-level `pages:` item starts on, and the line
/// just past the block.
fn item_lines(contents: &str) -> (Vec<usize>, usize) {
    let lines: Vec<&str> = contents.lines().collect();
    let Some(start) = lines
        .iter()
        .position(|line| line.trim_end() == "pages:" || line.starts_with("pages: "))
    else {
        return (Vec::new(), lines.len() + 1);
    };
    let mut starts = Vec::new();
    let mut indent: Option<usize> = None;
    let mut end = lines.len() + 1;
    for (idx, line) in lines.iter().enumerate().skip(start + 1) {
        if line.trim().is_empty() {
            continue;
        }
        let leading = line.len() - line.trim_start().len();
        if leading == 0 && !line.starts_with('-') {
            end = idx + 1;
            break;
        }
        if line.trim_start().starts_with("- ") || line.trim() == "-" {
            match indent {
                None => {
                    indent = Some(leading);
                    starts.push(idx + 1);
                }
                Some(i) if i == leading => starts.push(idx + 1),
                Some(_) => {}
            }
        }
    }
    (starts, end)
}

fn text(entry: &serde_yaml::Mapping, key: &str) -> Option<String> {
    entry
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn lenses_of(entry: &serde_yaml::Mapping) -> Option<Vec<(String, Vec<String>)>> {
    let mapping = entry.get("lenses")?.as_mapping()?;
    let mut out = Vec::new();
    for (key, list) in mapping {
        let Some(name) = key.as_str() else {
            continue;
        };
        let sections = list
            .as_sequence()
            .map(|seq| {
                seq.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        out.push((name.to_string(), sections));
    }
    Some(out)
}

/// Every `pages:` entry that declares a dashboard kind. An entry whose
/// shape is wrong is `Y021`'s report and is skipped here, so each mistake
/// carries exactly one code.
#[must_use]
pub fn declared_pages(contents: &str) -> Vec<DeclaredPage> {
    let Some(items) = pages_sequence(contents) else {
        return Vec::new();
    };
    let (starts, block_end) = item_lines(contents);
    let mut out = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let Some(entry) = item.as_mapping() else {
            continue;
        };
        let Some(kind) = text(entry, "kind")
            .and_then(|k| Kind::parse(&k))
            .filter(|k| k.is_dashboard())
        else {
            continue;
        };
        let line = starts.get(index).copied().unwrap_or(1);
        let end_line = starts.get(index + 1).copied().unwrap_or(block_end);
        out.push(DeclaredPage {
            slug: text(entry, "slug").unwrap_or_default(),
            kind,
            lenses: lenses_of(entry),
            line,
            end_line,
        });
    }
    out
}

/// A D-family violation at `line` of a manifest.
pub(crate) fn violation(
    code: &'static str,
    file: &SourceFile,
    line: usize,
    message: String,
) -> Violation {
    Violation {
        code,
        path: file.path.clone(),
        line,
        range: line_byte_range(&file.contents, line),
        message,
    }
}

/// A one-page `navigator.yaml` for a rule's unit tests: `lenses` pairs a
/// lens key with its flow-style section list.
#[cfg(test)]
pub(crate) fn one_page(kind: &str, lenses: &[(&str, &str)]) -> SourceFile {
    use std::fmt::Write as _;
    let mut contents =
        format!("pages:\n  - slug: hub\n    kind: {kind}\n    title: Hub\n    lenses:\n");
    for (lens, list) in lenses {
        let _ = writeln!(contents, "      {lens}: [{list}]");
    }
    SourceFile {
        path: std::path::PathBuf::from("navigator.yaml"),
        contents,
    }
}

/// One `Y021` problem: the 1-based line and the message.
pub type ShapeProblem = (usize, String);

fn dashboard_kind_names() -> String {
    Kind::ALL
        .iter()
        .filter(|k| k.is_dashboard())
        .map(|k| k.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The shape problems in a manifest's `pages:` block: not a list, an entry
/// that is not a map or carries an unknown key, a missing or malformed
/// `slug`, `kind`, or `title`, a reserved or repeated slug or title, or a `draft`
/// that is not a relative path. The composition under `lenses:` is the
/// D-family's to check.
#[must_use]
pub fn shape_problems(contents: &str) -> Vec<ShapeProblem> {
    let Ok(document) = serde_yaml::from_str::<serde_yaml::Value>(contents) else {
        return Vec::new();
    };
    let Some(value) = document.as_mapping().and_then(|m| m.get(KEY)) else {
        return Vec::new();
    };
    let (starts, _) = item_lines(contents);
    let block_line = contents
        .lines()
        .position(|line| line.starts_with("pages:"))
        .map_or(1, |idx| idx + 1);
    let Some(items) = value.as_sequence() else {
        return vec![(
            block_line,
            "`pages:` must be a list of {slug, kind, title, lenses} entries".to_string(),
        )];
    };
    let mut problems = Vec::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut seen_titles: BTreeMap<String, usize> = BTreeMap::new();
    for (index, item) in items.iter().enumerate() {
        let line = starts.get(index).copied().unwrap_or(block_line);
        let mut push = |message: String| problems.push((line, message));
        let Some(entry) = item.as_mapping() else {
            push(format!(
                "`pages:` entry {} must be a map of {}",
                index + 1,
                ENTRY_KEYS.join(", ")
            ));
            continue;
        };
        for key in entry.keys() {
            match key.as_str() {
                Some(name) if ENTRY_KEYS.contains(&name) => {}
                Some(name) => push(format!(
                    "unknown `pages:` key `{name}`; expected one of {}",
                    ENTRY_KEYS.join(", ")
                )),
                None => push("`pages:` entry keys must be strings".to_string()),
            }
        }
        match text(entry, "slug") {
            None => push("each `pages:` entry must carry a non-empty `slug`".to_string()),
            Some(slug) if !is_valid_slug(&slug) => push(format!(
                "`pages:` slug `{slug}` must be lowercase letters, digits, and single hyphens, \
                 at most {MAX_SLUG_LEN} characters"
            )),
            Some(slug) if RESERVED_SLUGS.contains(&slug.as_str()) => push(format!(
                "`pages:` slug `{slug}` is a route the matter page already owns; reserved: {}",
                RESERVED_SLUGS.join(", ")
            )),
            Some(slug) => {
                if let Some(first) = seen.get(&slug) {
                    push(format!(
                        "`pages:` slug `{slug}` repeats the entry on line {first}; each slug is \
                         one page"
                    ));
                } else {
                    seen.insert(slug, line);
                }
            }
        }
        match text(entry, "kind") {
            None => push(format!(
                "each `pages:` entry must carry a `kind`: one of {}",
                dashboard_kind_names()
            )),
            Some(kind) if Kind::parse(&kind).is_some_and(Kind::is_dashboard) => {}
            Some(kind) => push(format!(
                "`pages:` kind `{kind}` is not a matter page kind; expected one of {}",
                dashboard_kind_names()
            )),
        }
        if let Some(message) = title_problem(text(entry, "title"), line, &mut seen_titles) {
            push(message);
        }
        if let Some(draft) = entry.get("draft") {
            let ok = draft.as_str().map(str::trim).is_some_and(|path| {
                !path.is_empty()
                    && !path.starts_with('/')
                    && !path.contains('\\')
                    && !path.split('/').any(|segment| segment == "..")
            });
            if !ok {
                push(
                    "`pages:` draft must be a repository-relative path with no `..` segment"
                        .to_string(),
                );
            }
        }
    }
    problems
}

/// A page title's problem, if any: missing, the show page's own tab label,
/// or one an earlier entry already took (each case-insensitively). The first
/// line each title appeared on is recorded in `seen`.
fn title_problem(
    title: Option<String>,
    line: usize,
    seen: &mut BTreeMap<String, usize>,
) -> Option<String> {
    let Some(title) = title else {
        return Some("each `pages:` entry must carry a non-empty `title`".to_string());
    };
    if title.eq_ignore_ascii_case(SHOW_PAGE_TITLE) {
        return Some(format!(
            "`pages:` title `{title}` is the show page's tab, `{SHOW_PAGE_TITLE}`; name the page \
             for what it shows"
        ));
    }
    let key = title.to_lowercase();
    if let Some(first) = seen.get(&key) {
        return Some(format!(
            "`pages:` title `{title}` repeats the entry on line {first}; each tab needs its own \
             name"
        ));
    }
    seen.insert(key, line);
    None
}

/// The `pages:` entries of a well-shaped manifest, in declared order.
///
/// # Errors
///
/// When the block is present but not a list of maps, or an entry lacks
/// `slug`, `kind`, or `title`.
pub fn entries(mapping: &serde_yaml::Mapping) -> Result<Vec<PageEntry>, String> {
    let Some(value) = mapping.get(KEY) else {
        return Ok(Vec::new());
    };
    let items = value
        .as_sequence()
        .ok_or_else(|| "navigator.yaml pages must be a list".to_string())?;
    items
        .iter()
        .map(|item| {
            let entry = item
                .as_mapping()
                .ok_or_else(|| "navigator.yaml pages entry must be a map".to_string())?;
            let required = |key: &str| {
                text(entry, key)
                    .ok_or_else(|| format!("navigator.yaml pages entry must carry `{key}`"))
            };
            Ok(PageEntry {
                slug: required("slug")?,
                kind: required("kind")?,
                title: required("title")?,
                draft: text(entry, "draft"),
                lenses: lenses_of(entry).unwrap_or_default().into_iter().collect(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{declared_pages, entries, is_valid_slug, shape_problems, PageEntry};
    use crate::dashboard::{Lens, Section};
    use crate::kind::Kind;

    const VALID: &str = "version: 26.10.6\npages:\n  - slug: timeline\n    kind: docket_deadline_board\n    \
title: Case timeline\n    lenses:\n      lawyer: [deadline_board, boundary_note, provenance_statement]\n      \
client: [key_dates_strip, boundary_note, provenance_statement]\n  - slug: engagement\n    \
kind: engagement_billing_records\n    title: Engagement\n    draft: drafts/engagement.md\n    lenses:\n      \
lawyer: [engagement_letter, invoice, boundary_note, provenance_statement]\n";

    #[test]
    fn a_valid_block_has_no_shape_problems() {
        assert!(
            shape_problems(VALID).is_empty(),
            "{:?}",
            shape_problems(VALID)
        );
    }

    #[test]
    fn declared_pages_locate_each_entry() {
        let pages = declared_pages(VALID);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].slug, "timeline");
        assert_eq!(pages[0].kind, Kind::DocketDeadlineBoard);
        assert_eq!(pages[0].line, 3);
        assert_eq!(pages[1].line, 9);
        assert_eq!(pages[0].line_of(VALID, "key_dates_strip"), 8);
        // A name outside the entry falls back to the entry's first line.
        assert_eq!(pages[0].line_of(VALID, "engagement_letter"), 3);
    }

    #[test]
    fn a_duplicate_slug_is_one_problem_on_the_second_entry() {
        let yaml = VALID.replace("slug: engagement", "slug: timeline");
        let problems = shape_problems(&yaml);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].0, 9);
        assert!(problems[0].1.contains("repeats"), "{}", problems[0].1);
    }

    #[test]
    fn the_show_page_title_and_a_repeated_title_are_refused() {
        let yaml = VALID.replace("title: Case timeline", "title: overview");
        let problems = shape_problems(&yaml);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].1.contains("`Overview`"), "{}", problems[0].1);

        let yaml = VALID.replace("title: Engagement", "title: Case Timeline");
        let problems = shape_problems(&yaml);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].0, 9);
        assert!(problems[0].1.contains("repeats"), "{}", problems[0].1);
    }

    #[test]
    fn slug_charset_and_reserved_routes_are_refused() {
        for bad in ["Timeline", "time_line", "-x", "x-", "a--b", ""] {
            assert!(!is_valid_slug(bad), "{bad}");
        }
        assert!(is_valid_slug("case-timeline-2"));
        let yaml = VALID.replace("slug: timeline", "slug: documents");
        assert!(shape_problems(&yaml)[0].1.contains("route"));
    }

    #[test]
    fn a_non_page_kind_and_missing_title_are_shape_problems() {
        let yaml = "pages:\n  - slug: a\n    kind: letter\n    lenses: {}\n";
        let messages: Vec<String> = shape_problems(yaml).into_iter().map(|p| p.1).collect();
        assert!(messages
            .iter()
            .any(|m| m.contains("not a matter page kind")));
        assert!(messages.iter().any(|m| m.contains("`title`")));
        assert!(
            declared_pages(yaml).is_empty(),
            "D-rules skip a non-page kind"
        );
    }

    #[test]
    fn a_non_list_and_an_escaping_draft_are_refused() {
        assert_eq!(shape_problems("pages: nope\n").len(), 1);
        let yaml = VALID.replace("drafts/engagement.md", "../secret.md");
        assert!(shape_problems(&yaml)[0].1.contains("draft"));
    }

    #[test]
    fn entries_read_the_typed_block_in_order() {
        let document: serde_yaml::Value = serde_yaml::from_str(VALID).unwrap();
        let pages: Vec<PageEntry> = entries(document.as_mapping().unwrap()).unwrap();
        assert_eq!(pages[1].draft.as_deref(), Some("drafts/engagement.md"));
        assert_eq!(
            pages[0].sections_for(Lens::Client).unwrap(),
            vec![
                Section::KeyDatesStrip,
                Section::BoundaryNote,
                Section::ProvenanceStatement
            ]
        );
        assert!(pages[1].sections_for(Lens::Client).is_none());
        assert_eq!(pages[0].dashboard_kind(), Some(Kind::DocketDeadlineBoard));
    }
}
