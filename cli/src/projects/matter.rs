//! A pleading's matter, read from the Project's `navigator.yaml`.
//!
//! `matter:` is the one source of the court, case number, judicial officer,
//! parties, and counsel block every pleading in a Project shares. A
//! notation adds only its own title and hearing, from its frontmatter.
//! [`caption_for`] joins them; `notation pdf`, `notation word`, and
//! `project gate` all call it, so a pleading that renders is a pleading
//! that gates, and the reverse.
//!
//! A pleading in a multi-case Project says *which* case it serves with a
//! `docket:` key naming one of the Project's declared `dockets:` (LAW-204).
//! The value is matched case-insensitively against the declarations and the
//! caption carries the declared spelling; a value the Project has not
//! declared refuses, and a pleading with no `docket:` falls back to the
//! `matter:` block's own case number, which a single-case Project needs.

use std::path::{Path, PathBuf};

use word::matter::{Filing, Matter};

use super::manifest;

/// The manifest key that carries the shared matter.
pub const KEY: &str = "matter";

/// The manifest key listing every case number the Project may file under.
const DOCKETS_KEY: &str = "dockets";

/// The frontmatter key that binds a pleading to a declared docket.
const DOCKET_KEY: &str = "docket";

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

/// The pleading's declared `docket:` value, exactly as written (blank or
/// not — callers decide what a blank means).
fn docket_of(contents: &str) -> Option<String> {
    rules::frontmatter::extract(contents).and_then(|fm| rules::frontmatter::field(fm, DOCKET_KEY))
}

/// Every case number a pleading in this Project may caption itself with:
/// the manifest's `dockets:` list, plus the matter's own case number.
fn declarable_dockets(path: &Path, matter: &Matter) -> Vec<String> {
    let mut dockets = std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| {
            serde_yaml::from_str::<serde_yaml::Value>(&contents)
                .ok()
                .and_then(|document| document.get(DOCKETS_KEY).cloned())
                .and_then(|value| serde_yaml::from_value::<Vec<String>>(value).ok())
        })
        .unwrap_or_default();
    if !dockets.iter().any(|d| d == &matter.case_number) {
        dockets.push(matter.case_number.clone());
    }
    dockets
}

/// The case number the caption carries: the pleading's `docket:` value
/// matched (case-insensitively) against the declared spellings, else the
/// matter's own case number.
///
/// # Errors
///
/// A `docket:` value the Project has not declared, naming the value and the
/// declarations it missed.
fn resolve_case_number(
    docket: Option<&str>,
    matter: &Matter,
    declarable: &[String],
) -> Result<String, String> {
    let Some(docket) = docket.map(str::trim).filter(|d| !d.is_empty()) else {
        return Ok(matter.case_number.clone());
    };
    declarable
        .iter()
        .find(|d| d.eq_ignore_ascii_case(docket))
        .cloned()
        .ok_or_else(|| {
            format!(
                "case number {docket} is not declared in the Project's dockets: {}",
                declarable.join(", ")
            )
        })
}

/// [`read`] with the pleading's `docket:` resolved into the case number the
/// caption carries.
///
/// # Errors
///
/// [`read`]'s message, or an undeclared docket value.
fn read_resolved(path: &Path, contents: &str) -> Result<Matter, String> {
    let mut matter = read(path)?;
    let declarable = declarable_dockets(path, &matter);
    matter.case_number = resolve_case_number(docket_of(contents).as_deref(), &matter, &declarable)?;
    Ok(matter)
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
        Ok(path) => read_resolved(&path, contents),
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
        .and_then(|path| {
            read_assigning(&path).and_then(|mut matter| {
                let declarable = declarable_dockets(&path, &matter);
                matter.case_number =
                    resolve_case_number(docket_of(contents).as_deref(), &matter, &declarable)?;
                Ok(matter)
            })
        });
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

#[cfg(test)]
mod tests {
    use super::{docket_of, resolve_case_number};

    fn matter() -> word::matter::Matter {
        word::matter::Matter {
            case_number: "00CV000000".into(),
            ..word::matter::Matter::default()
        }
    }

    fn declarable(case_numbers: &[&str]) -> Vec<String> {
        case_numbers.iter().map(|d| (*d).to_string()).collect()
    }

    #[test]
    fn a_docket_key_parses_from_frontmatter() {
        let contents = "---\nkind: pleading\ndocket: 00CV000000\n---\n\n# Body\n";
        assert_eq!(docket_of(contents).as_deref(), Some("00CV000000"));
        assert_eq!(docket_of("---\nkind: pleading\n---\n\n# Body\n"), None);
    }

    #[test]
    fn a_blank_or_absent_docket_falls_back_to_the_matter_case_number() {
        let matter = matter();
        let declared = declarable(&["00CV000000"]);
        assert_eq!(
            resolve_case_number(None, &matter, &declared).unwrap(),
            "00CV000000"
        );
        assert_eq!(
            resolve_case_number(Some("   "), &matter, &declared).unwrap(),
            "00CV000000"
        );
    }

    #[test]
    fn docket_matching_is_case_insensitive_and_prints_the_declared_spelling() {
        let matter = matter();
        let declared = declarable(&["00CV000000", "B327654"]);
        assert_eq!(
            resolve_case_number(Some("b327654"), &matter, &declared).unwrap(),
            "B327654"
        );
        assert_eq!(
            resolve_case_number(Some("00cv000000"), &matter, &declared).unwrap(),
            "00CV000000"
        );
    }

    #[test]
    fn an_undeclared_docket_refuses_by_name() {
        let matter = matter();
        let declared = declarable(&["00CV000000", "B327654"]);
        let error = resolve_case_number(Some("99CV999999"), &matter, &declared).unwrap_err();
        assert!(error.contains("99CV999999"), "{error}");
        assert!(error.contains("dockets: 00CV000000, B327654"), "{error}");
    }
}
