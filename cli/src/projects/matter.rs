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

/// The `matter:` block exactly as the manifest at `path` declares it, with
/// the manifest's text.
fn declared(path: &Path) -> Result<(String, Matter), String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|error| format!("read {}: {error}", manifest::FILE))?;
    let document: serde_yaml::Value = serde_yaml::from_str(&contents)
        .map_err(|error| format!("{} is not valid YAML: {error}", manifest::FILE))?;
    let block = document.get(KEY).cloned().ok_or_else(|| {
        format!(
            "a pleading needs a `{KEY}:` block in {} (court, parties, counsel; case_number, \
             judicial_officer, department, and filed_on once the court assigns them)",
            manifest::FILE
        )
    })?;
    let matter = serde_yaml::from_value(block)
        .map_err(|error| format!("`{KEY}:` in {} is malformed: {error}", manifest::FILE))?;
    Ok((contents, matter))
}

/// Read the `matter:` block of the manifest at `path`, with counsel's office
/// details defaulted from the Firm's office in `jurisdiction`
/// ([`with_office_defaults`]).
///
/// # Errors
///
/// A message naming the field to add or fix.
pub fn read(path: &Path, jurisdiction: &str) -> Result<Matter, String> {
    let (contents, mut matter) = declared(path)?;
    with_office_defaults(&mut matter, &contents, jurisdiction);
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

/// A template's `jurisdiction:` code, or blank when it declares none.
#[must_use]
pub fn jurisdiction_of(contents: &str) -> String {
    rules::frontmatter::extract(contents)
        .and_then(|fm| rules::frontmatter::field(fm, "jurisdiction"))
        .unwrap_or_default()
}

/// Fill counsel's blank `firm`, `address`, `telephone`, and `email` from the
/// office the Project's Brand publishes for `jurisdiction` (LAW-329), so a
/// Washington pleading carries the Washington office without anyone typing
/// it. A jurisdiction with no office takes the Firm's default office. A
/// party appearing in pro per has no Firm office, and a declared value is
/// never replaced.
pub fn with_office_defaults(matter: &mut Matter, manifest_contents: &str, jurisdiction: &str) {
    if matter.counsel.pro_per {
        return;
    }
    let Ok(parsed) = manifest::parse(manifest_contents) else {
        return;
    };
    let Ok(Some(office)) = crate::letterhead::office_for_project(
        parsed.brand.as_deref(),
        parsed.host.as_deref(),
        jurisdiction,
    ) else {
        return;
    };
    let counsel = &mut matter.counsel;
    if counsel.address.iter().all(|line| line.trim().is_empty()) {
        counsel.address = office.address;
    }
    for (field, value) in [
        (&mut counsel.firm, office.legal_entity),
        (&mut counsel.telephone, office.phone),
        (&mut counsel.email, office.email),
    ] {
        if field.trim().is_empty() {
            *field = value;
        }
    }
}

/// `Y019`'s warning for a pleading whose declared counsel address is in a
/// different state from its `jurisdiction:` — a Washington paper about to
/// go out with the Nevada office. `None` when they agree, or when either
/// does not name a two-letter state.
#[must_use]
pub fn counsel_state_mismatch(file: &Path, contents: &str) -> Option<String> {
    let jurisdiction = jurisdiction_of(contents);
    let state_code = |text: &str| -> Option<String> {
        let code = text
            .rsplit(',')
            .next()?
            .split_whitespace()
            .next()?
            .to_string();
        (code.len() == 2 && code.chars().all(|c| c.is_ascii_uppercase())).then_some(code)
    };
    let jurisdiction = state_code(&jurisdiction)?;
    let (_, matter) = declared(&find_manifest(file)?).ok()?;
    let declared = state_code(matter.counsel.address.last()?)?;
    (declared != jurisdiction).then(|| {
        format!(
            "matter.counsel.address is in {declared}, but this pleading's jurisdiction is \
             {jurisdiction}; remove the address to print the Firm's {jurisdiction} office, or \
             correct it"
        )
    })
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
    if !matter.case_number.trim().is_empty() && !dockets.iter().any(|d| d == &matter.case_number) {
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
    let mut matter = read(path, &jurisdiction_of(contents))?;
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
            let style = word::matter::CaptionStyle::for_jurisdiction(&jurisdiction_of(contents));
            Ok(word::matter::compose(&matter, &filing, style).to_fence())
        }
        _ => Err(format!(
            "this pleading cannot be captioned: {}",
            problems.join("; ")
        )),
    }
}

/// The name a final render signs counsel's `/s/` line with:
/// `matter.counsel.name` from the Project's manifest.
///
/// # Errors
///
/// The message [`read`] gives for a missing or malformed `matter:`.
pub fn counsel_name(file: &Path) -> Result<String, String> {
    let path = find_manifest(file).ok_or_else(|| {
        format!(
            "no {} found above this pleading; add one with a `{KEY}:` block",
            manifest::FILE
        )
    })?;
    Ok(read(&path, "")?.counsel.name.trim().to_string())
}

/// The matter's signing date, `YYYY-MM-DD` — the only source the signature
/// block's date line takes (LAW-242). `None` when the manifest declares no
/// `signed_on:`, which a preview fills with a placeholder and a final
/// export refuses.
///
/// # Errors
///
/// The message [`read`] gives for a missing or malformed `matter:`.
pub fn signed_on(file: &Path) -> Result<Option<String>, String> {
    let path = find_manifest(file).ok_or_else(|| {
        format!(
            "no {} found above this pleading; add one with a `{KEY}:` block",
            manifest::FILE
        )
    })?;
    // The jurisdiction only shapes counsel's office defaults, none of
    // which this helper reads — the same pattern project_preview uses.
    Ok(read(&path, "")?.signed_on)
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

/// [`caption_for`] for a preview. A preview captions exactly what a final
/// render would, except that it accepts a `pending` assignment.
///
/// # Errors
///
/// The same message [`caption_for`] gives for any missing field.
pub fn caption_for_preview(file: &Path, contents: &str) -> Result<String, String> {
    caption_for_export(file, contents, false)
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

    /// The synthetic Washington matter in a tempdir, its counsel block
    /// replaced by `counsel`, and a pleading declaring `jurisdiction`.
    fn washington_tree(
        counsel: &str,
        jurisdiction: &str,
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/synthetic_washington_matter/navigator.yaml");
        let manifest = std::fs::read_to_string(fixture).unwrap();
        let head = &manifest[..manifest.find("  counsel:\n").unwrap()];
        std::fs::write(
            dir.path().join("navigator.yaml"),
            format!("{head}  counsel:\n{counsel}"),
        )
        .unwrap();
        let notice = dir.path().join("notice.md");
        std::fs::write(
            &notice,
            format!(
                "---\nkind: pleading\njurisdiction: {jurisdiction}\ncaption_title: NOTICE\n\
                 footer_title: NOTICE\n---\n\n# NOTICE\n"
            ),
        )
        .unwrap();
        (dir, notice)
    }

    const NAMED_COUNSEL: &str = "    name: Pat Sample\n    bar_jurisdiction: Washington\n    \
        bar_number: \"00000\"\n    appears_for: Respondent SAM SYNTHETIC TWO\n";

    /// LAW-329: a pleading whose counsel block names only the lawyer takes
    /// the Firm's office in the pleading's jurisdiction — never the Nevada
    /// default when a Washington office exists — and a jurisdiction with no
    /// office takes the default one.
    #[test]
    fn counsel_s_office_defaults_to_the_firm_s_office_in_the_jurisdiction() {
        let (dir, notice) = washington_tree(NAMED_COUNSEL, "WA");
        let manifest = dir.path().join("navigator.yaml");
        let matter = super::read(&manifest, "WA").expect("the office fills counsel's block");
        assert_eq!(
            matter.counsel.address,
            ["1111 Main St", "STE 201E", "Vancouver, WA 98660"]
        );
        assert!(!matter.counsel.firm.is_empty());
        assert!(!matter.counsel.telephone.is_empty());
        assert!(matter.counsel.email.contains('@'));
        let caption =
            super::caption_for(&notice, &std::fs::read_to_string(&notice).unwrap()).unwrap();
        assert!(caption.contains("office: Vancouver, WA 98660"), "{caption}");

        let texas = super::read(&manifest, "TX").unwrap();
        assert!(texas.counsel.address.last().unwrap().contains("NV"));

        let declared = format!(
            "{NAMED_COUNSEL}    address:\n      - 1 Example Way\n      - Sampleton, WA 99000\n"
        );
        let (dir, _) = washington_tree(&declared, "WA");
        let matter = super::read(&dir.path().join("navigator.yaml"), "WA").unwrap();
        assert_eq!(
            matter.counsel.address,
            ["1 Example Way", "Sampleton, WA 99000"]
        );

        let pro_per = "    name: Sam Two\n    appears_for: Respondent\n    pro_per: true\n";
        let (dir, _) = washington_tree(pro_per, "WA");
        let refused = super::read(&dir.path().join("navigator.yaml"), "WA").unwrap_err();
        assert!(refused.contains("matter.counsel.address"), "{refused}");
    }

    /// LAW-329: a declared counsel address in another state than the
    /// pleading's jurisdiction is a warning; an agreeing one is not.
    #[test]
    fn a_counsel_address_in_another_state_is_named() {
        let nevada = format!(
            "{NAMED_COUNSEL}    address:\n      - 5150 Example Ave\n      - Reno, NV 89523\n"
        );
        let (_dir, notice) = washington_tree(&nevada, "WA");
        let contents = std::fs::read_to_string(&notice).unwrap();
        let message = super::counsel_state_mismatch(&notice, &contents).expect("a mismatch");
        assert!(
            message.contains("in NV") && message.contains("is WA"),
            "{message}"
        );

        let (_dir, notice) = washington_tree(&nevada, "NV");
        let contents = std::fs::read_to_string(&notice).unwrap();
        assert_eq!(super::counsel_state_mismatch(&notice, &contents), None);
        let (_dir, notice) = washington_tree(NAMED_COUNSEL, "WA");
        let contents = std::fs::read_to_string(&notice).unwrap();
        assert_eq!(super::counsel_state_mismatch(&notice, &contents), None);
    }
}
