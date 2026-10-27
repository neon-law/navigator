//! The Project Skill catalog shared by the CLI and read-only tools.

use include_dir::{include_dir, Dir};

static SKILLS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../skills");

/// One compiled Project Skill entry.
pub use crate::project_skill::ProjectSkill as CatalogEntry;

/// Every catalog entry compiled into this binary, sorted by
/// `(jurisdiction, practice_area)`.
///
/// A file that fails to parse is skipped rather than panicking the CLI:
/// `navigator project gate` (`crate::project_skill_catalog_violations`,
/// `N126`/`N127`) is what refuses a catalog entry that does not parse, so a
/// released binary always carries a clean catalog. Skipping here, rather
/// than propagating the error, keeps `list`/`show` usable even if that
/// invariant were ever violated — the offending entry is simply invisible
/// instead of crashing every Project Skill command.
#[must_use]
pub fn catalog() -> Vec<CatalogEntry> {
    let mut files = Vec::new();
    collect(&SKILLS, &mut files);
    let mut entries: Vec<CatalogEntry> = files
        .into_iter()
        .filter(|file| file.path().extension().and_then(|ext| ext.to_str()) == Some("md"))
        .filter_map(|file| {
            let contents = file.contents_utf8()?;
            crate::project_skill::parse(contents).ok()
        })
        .collect();
    entries.sort_by(|a, b| {
        (a.jurisdiction.as_str(), a.practice_area.as_str())
            .cmp(&(b.jurisdiction.as_str(), b.practice_area.as_str()))
    });
    entries
}

fn collect(dir: &'static Dir<'static>, out: &mut Vec<&'static include_dir::File<'static>>) {
    for file in dir.files() {
        out.push(file);
    }
    for child in dir.dirs() {
        collect(child, out);
    }
}

/// Resolve one catalog entry, matching `jurisdiction` and `practice_area`
/// case-insensitively (`show nv estates` and `show NV estates` are the same
/// lookup).
#[must_use]
pub fn find<'a>(
    entries: &'a [CatalogEntry],
    jurisdiction: &str,
    practice_area: &str,
) -> Option<&'a CatalogEntry> {
    entries.iter().find(|entry| {
        entry.jurisdiction.eq_ignore_ascii_case(jurisdiction)
            && entry.practice_area.eq_ignore_ascii_case(practice_area)
    })
}
