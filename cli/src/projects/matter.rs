//! A pleading's matter, read from the Project's `navigator.yaml`.
//!
//! `matter:` is the one source of the court, case number, judicial officer,
//! parties, and counsel block every pleading in a Project shares. A
//! notation adds only its own title and hearing, from its frontmatter.
//! [`caption_for`] joins them; `notation pdf`, `notation word`, and
//! `project gate` all call it, so a pleading that renders is a pleading
//! that gates, and the reverse.

use std::path::{Path, PathBuf};

use word::matter::{Filing, Matter};

use super::manifest;

/// The manifest key that carries the shared matter.
pub const KEY: &str = "matter";

/// `Y019` — a pleading's matter is missing or malformed.
pub const CODE: &str = "Y019";

/// The nearest `navigator.yaml` at or above `start`'s directory.
#[must_use]
pub fn find_manifest(start: &Path) -> Option<PathBuf> {
    let start = std::fs::canonicalize(start).ok()?;
    let mut dir = start.parent()?;
    loop {
        let candidate = dir.join(manifest::FILE);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
}

/// Read the `matter:` block of the manifest at `path`.
///
/// # Errors
///
/// A message naming the field to add or fix.
pub fn read(path: &Path) -> Result<Matter, String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|error| format!("read {}: {error}", manifest::FILE))?;
    let document: serde_yaml::Value = serde_yaml::from_str(&contents)
        .map_err(|error| format!("{} is not valid YAML: {error}", manifest::FILE))?;
    let block = document.get(KEY).cloned().ok_or_else(|| {
        format!(
            "a pleading needs a `{KEY}:` block in {} (court, case_number, judicial_officer, \
             department, filed_on, parties, counsel)",
            manifest::FILE
        )
    })?;
    let matter: Matter = serde_yaml::from_value(block)
        .map_err(|error| format!("`{KEY}:` in {} is malformed: {error}", manifest::FILE))?;
    let missing = matter.missing();
    if missing.is_empty() {
        Ok(matter)
    } else {
        Err(format!(
            "{} is missing or has a malformed {}",
            manifest::FILE,
            missing.join(", ")
        ))
    }
}

/// A notation's own caption fields, from its frontmatter.
#[must_use]
pub fn filing_from(contents: &str) -> Filing {
    let field = |key: &str| {
        rules::frontmatter::extract(contents)
            .and_then(|fm| rules::frontmatter::field(fm, key))
            .filter(|value| !value.is_empty())
    };
    Filing {
        caption_title: field("caption_title").unwrap_or_default(),
        footer_title: field("footer_title").unwrap_or_default(),
        hearing_date: field("hearing_date"),
        hearing_time: field("hearing_time"),
        reservation_id: field("reservation_id"),
    }
}

/// The caption fence for the pleading at `file` whose source is `contents`.
///
/// # Errors
///
/// A message naming every missing manifest or frontmatter field.
pub fn caption_for(file: &Path, contents: &str) -> Result<String, String> {
    caption_for_export(file, contents, false)
}

fn caption_for_export(file: &Path, contents: &str, final_export: bool) -> Result<String, String> {
    let filing = filing_from(contents);
    let mut problems = filing.missing();
    let matter = match find_manifest(file).ok_or_else(|| {
        format!(
            "no {} found above this pleading; add one with a `{KEY}:` block",
            manifest::FILE
        )
    }) {
        Ok(path) => read(&path),
        Err(message) => Err(message),
    };
    let matter = match matter {
        Ok(matter) => Some(matter),
        Err(message) => {
            problems.insert(0, message);
            None
        }
    };
    match (matter, problems.is_empty()) {
        (Some(matter), true) => {
            if final_export {
                require_assigned(&matter)?;
            }
            Ok(word::matter::compose(&matter, &filing).to_fence())
        }
        _ => Err(format!(
            "this pleading cannot be captioned: {}",
            problems.join("; ")
        )),
    }
}

/// A final export requires the court's assignments, even when a draft
/// explicitly declares them pending.
///
/// # Errors
///
/// Names each pending assignment, or a missing caption field.
pub fn caption_for_final(file: &Path, contents: &str) -> Result<String, String> {
    caption_for_export(file, contents, true)
}

fn require_assigned(matter: &Matter) -> Result<(), String> {
    let pending: Vec<_> = [
        ("matter.department", matter.department.as_str()),
        ("matter.judicial_officer", matter.judicial_officer.as_str()),
    ]
    .into_iter()
    .filter_map(|(field, value)| {
        value
            .trim()
            .eq_ignore_ascii_case(word::matter::PENDING)
            .then_some(field)
    })
    .collect();
    if pending.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "this pleading cannot be exported: {} pending; record the court's assignment or use --preview",
            pending.join(", ")
        ))
    }
}

/// What a preview prints for a judicial officer or department the court has
/// not yet assigned.
pub const TO_BE_ASSIGNED: &str = "To be assigned";

/// [`caption_for`] for a preview (LAW-183): a blank department or judicial
/// officer prints "To be assigned" instead of refusing, since a draft exists
/// before the court assigns either. Every other missing field still refuses,
/// and a final render never calls this.
///
/// # Errors
///
/// The same message [`caption_for`] gives for any other missing field.
pub fn caption_for_preview(file: &Path, contents: &str) -> Result<String, String> {
    let filing = filing_from(contents);
    let mut problems = filing.missing();
    let matter = find_manifest(file)
        .ok_or_else(|| {
            format!(
                "no {} found above this pleading; add one with a `{KEY}:` block",
                manifest::FILE
            )
        })
        .and_then(|path| read_assigning(&path));
    let matter = match matter {
        Ok(matter) => Some(matter),
        Err(message) => {
            problems.insert(0, message);
            None
        }
    };
    match (matter, problems.is_empty()) {
        (Some(matter), true) => Ok(word::matter::compose(&matter, &filing).to_fence()),
        _ => Err(format!(
            "this pleading cannot be captioned: {}",
            problems.join("; ")
        )),
    }
}

/// [`read`] with [`TO_BE_ASSIGNED`] standing in for a blank department or
/// judicial officer.
fn read_assigning(path: &Path) -> Result<Matter, String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|error| format!("read {}: {error}", manifest::FILE))?;
    let document: serde_yaml::Value = serde_yaml::from_str(&contents)
        .map_err(|error| format!("{} is not valid YAML: {error}", manifest::FILE))?;
    let block = document
        .get(KEY)
        .cloned()
        .ok_or_else(|| format!("a pleading needs a `{KEY}:` block in {}", manifest::FILE))?;
    let mut matter: Matter = serde_yaml::from_value(block)
        .map_err(|error| format!("`{KEY}:` in {} is malformed: {error}", manifest::FILE))?;
    for field in [&mut matter.department, &mut matter.judicial_officer] {
        if field.trim().is_empty() {
            *field = TO_BE_ASSIGNED.to_string();
        }
    }
    let missing = matter.missing();
    if missing.is_empty() {
        Ok(matter)
    } else {
        Err(format!(
            "{} is missing or has a malformed {}",
            manifest::FILE,
            missing.join(", ")
        ))
    }
}
