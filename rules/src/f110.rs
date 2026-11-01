#![allow(clippy::doc_markdown)]
//! `N110` — notation templates must live on the shelf their tree owns
//! and declare their jurisdiction.
//!
//! Navigator's own catalog lives under `templates/notations/`, which has
//! two shelves:
//!
//! - `notations/neon_law/` for firm-authored templates.
//! - `notations/forms/` for government form-backed templates whose repo
//!   path (below `notations/`) mirrors their public bucket key.
//!
//! A Project repository is a different tree: its root `navigator.yaml`
//! carries `project:`, and each blueprint is a direct
//! `templates/<code>.md` or `drafts/<code>.md` file. N110 accepts either flat layout there and
//! refuses a subdirectory. The layout gate in
//! `cli/src/projects/repository.rs` makes the same demand.
//!
//! Jurisdiction is explicit metadata, not a deep practice-area path. A
//! form template at `templates/notations/forms/united_states/nevada/
//! state/nv__llc_formation.md` therefore declares `jurisdiction: NV` and
//! `code: nv__llc_formation`; the blank PDF lives in the public assets
//! bucket at the matching key `forms/united_states/nevada/state/
//! nv__llc_formation.pdf`, pinned by the sibling `.sha256` file.

use crate::{frontmatter, line_byte_range, Rule, SourceFile, Violation};
use std::path::Path;
use std::sync::LazyLock;

/// The single required top-level directory for every notation.
const CATALOG_ROOT: &str = "notations";
/// The shelves `CATALOG_ROOT` holds.
const SHELVES: &[&str] = &["neon_law", "forms"];

const JURISDICTION_SEED_YAML: &str = include_str!("../../store/seeds/Jurisdiction.yaml");

#[derive(serde::Deserialize)]
struct JurisdictionSeed {
    records: Vec<JurisdictionRecord>,
}

#[derive(serde::Deserialize)]
struct JurisdictionRecord {
    code: String,
}

/// Every jurisdiction code Navigator seeds, paired with its lowercase form
/// prefix (e.g. `NV` -> `nv`). Derived from `store/seeds/Jurisdiction.yaml`
/// at compile time — not a database lookup — so this list can never fall
/// behind the seeded reference data; `cli/tests/jurisdiction_reconciliation.rs`
/// guards the reverse direction (every template's jurisdiction is seeded).
pub static JURISDICTIONS: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
    let seed: JurisdictionSeed =
        serde_yaml::from_str(JURISDICTION_SEED_YAML).expect("Jurisdiction.yaml parses");
    seed.records
        .into_iter()
        .map(|record| {
            let prefix = record.code.to_lowercase();
            (record.code, prefix)
        })
        .collect()
});

/// True when an ancestor of `path` carries a YAML `navigator.yaml` with a
/// `project` key — the signal that this tree is a Project repository rather
/// than Navigator's catalog. A malformed project value still claims the
/// repository shape, so its manifest finding does not cascade into catalog
/// path findings.
pub(crate) fn is_project_repository_tree(path: &Path) -> bool {
    let mut current = path.parent();
    while let Some(dir) = current {
        let manifest = dir.join("navigator.yaml");
        if let Ok(raw) = std::fs::read_to_string(&manifest) {
            if serde_yaml::from_str::<serde_yaml::Value>(&raw)
                .ok()
                .and_then(|value| value.get("project").cloned())
                .is_some()
            {
                return true;
            }
        }
        current = dir.parent();
    }
    false
}

pub struct F110JurisdictionPath;

impl F110JurisdictionPath {
    pub const CODE: &'static str = "N110";

    fn segments_under_tree(path: &Path, project_repository: bool) -> Option<(String, Vec<&str>)> {
        let mut comps = path.components();
        let mut directory = None;
        for c in comps.by_ref() {
            if let std::path::Component::Normal(seg) = c {
                if seg == "templates" || (project_repository && seg == "drafts") {
                    directory = seg.to_str().map(str::to_string);
                    break;
                }
            }
        }
        Some((
            directory?,
            comps
                .filter_map(|c| match c {
                    std::path::Component::Normal(seg) => seg.to_str(),
                    _ => None,
                })
                .collect(),
        ))
    }

    fn violation(file: &SourceFile, message: String) -> Violation {
        Violation {
            code: Self::CODE,
            path: file.path.clone(),
            line: 1,
            range: line_byte_range(&file.contents, 1),
            message,
        }
    }
}

impl Rule for F110JurisdictionPath {
    fn code(&self) -> &'static str {
        Self::CODE
    }

    fn description(&self) -> &'static str {
        "Notation templates live on the catalog shelves or as a direct Project blueprint file"
    }

    fn lint(&self, file: &SourceFile) -> Vec<Violation> {
        let project_repository = is_project_repository_tree(&file.path);
        let Some((directory, rel)) = Self::segments_under_tree(&file.path, project_repository)
        else {
            return Vec::new();
        };
        let Some(first) = rel.first() else {
            return Vec::new();
        };
        if rel.len() == 1 && first.eq_ignore_ascii_case("README.md") {
            return Vec::new();
        }

        let mut violations = Vec::new();
        let matched_shelf: Option<&str> = if project_repository {
            if rel.len() != 1 {
                violations.push(Self::violation(
                    file,
                    format!(
                        "Project templates must be direct `{directory}/<code>.md` files; \
                         found `{}`",
                        rel.join("/")
                    ),
                ));
            }
            None
        } else if *first == CATALOG_ROOT {
            rel.get(1)
                .copied()
                .filter(|second| SHELVES.contains(second))
        } else {
            None
        };
        if !project_repository && matched_shelf.is_none() {
            violations.push(Self::violation(
                file,
                format!(
                    "Template must live under `notations/neon_law/` or `notations/forms/`; \
                     found `{}`",
                    rel.join("/")
                ),
            ));
        }

        let Some(fm) = frontmatter::extract(&file.contents) else {
            violations.push(Self::violation(
                file,
                "Missing frontmatter (file must declare `jurisdiction:`)".to_string(),
            ));
            return violations;
        };
        let jurisdiction = frontmatter::field(fm, "jurisdiction");
        let Some(jurisdiction) = jurisdiction.filter(|j| !j.is_empty()) else {
            // A resume presents a person under no body of law, so it has no
            // jurisdiction to declare. One it does declare is still checked.
            if frontmatter::field(fm, "kind").as_deref() == Some(crate::Kind::Resume.as_str()) {
                return violations;
            }
            violations.push(Self::violation(
                file,
                "Frontmatter is missing required `jurisdiction:` field".to_string(),
            ));
            return violations;
        };
        let Some((_, prefix)) = JURISDICTIONS.iter().find(|(code, _)| *code == jurisdiction) else {
            violations.push(Self::violation(
                file,
                format!(
                    "Unknown jurisdiction `{jurisdiction}`; expected a code seeded in \
                     store/seeds/Jurisdiction.yaml"
                ),
            ));
            return violations;
        };

        if !project_repository && matched_shelf == Some("forms") {
            if !frontmatter::field(fm, "origin_url").is_some_and(|url| origin_url_is_official(&url))
            {
                violations.push(Self::violation(
                    file,
                    "Form templates must declare an HTTPS government `origin_url:`".to_string(),
                ));
            }
            let stem = file.path.file_stem().and_then(|s| s.to_str());
            if let Some(stem) = stem {
                if !stem.starts_with(&format!("{prefix}__")) {
                    violations.push(Self::violation(
                        file,
                        format!(
                            "Form filename stem `{stem}` must start with jurisdiction prefix `{prefix}__`"
                        ),
                    ));
                }
            }
        }

        violations
    }
}

/// True when the `origin_url` points at an official source: any `https://`
/// `.gov` host, or an exact domain from [`OFFICIAL_COURT_DOMAINS`]. Nevada's
/// county courts run their official sites on `.com` (and provide a court
/// forms service through a county-contracted nonprofit), so a bare TLD
/// test would refuse the very pages those courts publish the forms on.
fn origin_url_is_official(url: &str) -> bool {
    if !url.starts_with("https://") {
        return false;
    }
    if url.contains(".gov") {
        return true;
    }
    let host = url
        .strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("")
        .to_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    OFFICIAL_COURT_DOMAINS.contains(&host)
}

/// Official form sources that live outside `.gov`, exact-host pinned. Each
/// entry carries its provenance: a reader should be able to trust the
/// domain the way they trust a `.gov` host.
const OFFICIAL_COURT_DOMAINS: &[&str] = &[
    // The Second Judicial District Court (Washoe County, Reno) publishes its
    // own forms on this host — the court's official website.
    "washoecourts.com",
    // The Civil Law Self-Help Center sits on the Regional Justice Center's
    // first floor and is operated under contract with Clark County by the
    // Legal Aid Center of Southern Nevada; the Clark County Justice and
    // District Courts approve the forms it publishes.
    "civillawselfhelpcenter.org",
];

#[cfg(test)]
mod tests {
    use super::F110JurisdictionPath;
    use crate::{Rule, SourceFile};
    use std::path::PathBuf;

    fn at(path: &str, fm: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from(path),
            contents: format!("---\n{fm}\n---\nbody\n"),
        }
    }

    #[test]
    fn accepts_a_firm_template_with_jurisdiction() {
        let v = F110JurisdictionPath.lint(&at(
            "templates/notations/neon_law/onboarding__letter.md",
            "title: T\ncode: onboarding__letter\njurisdiction: NV",
        ));
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn a_resume_needs_no_jurisdiction_but_a_declared_one_is_checked() {
        let v = F110JurisdictionPath.lint(&at(
            "templates/notations/neon_law/resume.md",
            "title: T\nkind: resume",
        ));
        assert!(v.is_empty(), "{v:?}");
        let v = F110JurisdictionPath.lint(&at(
            "templates/notations/neon_law/resume.md",
            "title: T\nkind: resume\njurisdiction: ZZ",
        ));
        assert!(
            v.iter().any(|v| v.message.contains("Unknown jurisdiction")),
            "{v:?}"
        );
        let v = F110JurisdictionPath.lint(&at(
            "templates/notations/neon_law/letter.md",
            "title: T\nkind: letter",
        ));
        assert!(
            v.iter().any(|v| v.message.contains("jurisdiction")),
            "{v:?}"
        );
    }

    #[test]
    fn accepts_a_form_template_matching_code_and_origin() {
        let v = F110JurisdictionPath.lint(&at(
            "templates/notations/forms/united_states/nevada/state/nv__llc_formation.md",
            "title: T\ncode: nv__llc_formation\njurisdiction: NV\norigin_url: https://www.nvsos.gov/forms",
        ));
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn flags_old_top_level_roots() {
        let v = F110JurisdictionPath.lint(&at(
            "templates/united_states/nevada/state/x.md",
            "title: T\ncode: x\njurisdiction: NV",
        ));
        assert_eq!(v[0].code, "N110");
        assert!(v[0].message.contains("notations"));
    }

    #[test]
    fn rejects_the_legacy_bare_neon_law_shelf() {
        let v = F110JurisdictionPath.lint(&at(
            "templates/neon_law/shared/onboarding_letter.md",
            "title: T\ncode: onboarding__letter\njurisdiction: NV",
        ));
        assert_eq!(v[0].code, "N110");
        assert!(v[0].message.contains("notations/neon_law"), "{v:?}");
    }

    #[test]
    fn rejects_the_legacy_bare_forms_shelf() {
        let v = F110JurisdictionPath.lint(&at(
            "templates/forms/united_states/nevada/state/nv__llc_formation.md",
            "title: T\ncode: nv__llc_formation\njurisdiction: NV\norigin_url: https://www.nvsos.gov/forms",
        ));
        assert_eq!(v[0].code, "N110");
        assert!(v[0].message.contains("notations/forms"), "{v:?}");
        // Off its real shelf, the file is not held to the forms-specific
        // checks — only the location violation fires.
        assert_eq!(v.len(), 1, "{v:?}");
    }

    #[test]
    fn rejects_an_unknown_shelf_under_notations() {
        let v = F110JurisdictionPath.lint(&at(
            "templates/notations/mutual_nda.md",
            "title: T\ncode: mutual_nda\njurisdiction: US",
        ));
        assert_eq!(v[0].code, "N110");
        assert!(v[0].message.contains("notations/neon_law"), "{v:?}");
    }

    #[test]
    fn rejects_the_retired_public_template_shelf() {
        let v = F110JurisdictionPath.lint(&at(
            "templates/open_source/mutual_nda.md",
            "title: T\ncode: mutual_nda\njurisdiction: US",
        ));
        assert_eq!(v[0].code, "N110");
        assert!(v[0].message.contains("open_source"), "{v:?}");
    }

    #[test]
    fn flags_missing_jurisdiction() {
        let v = F110JurisdictionPath.lint(&at(
            "templates/notations/neon_law/onboarding__letter.md",
            "title: T\ncode: onboarding__letter",
        ));
        assert_eq!(v[0].code, "N110");
        assert!(v[0].message.contains("jurisdiction"));
    }

    #[test]
    fn flags_form_filename_without_jurisdiction_prefix() {
        let v = F110JurisdictionPath.lint(&at(
            "templates/notations/forms/united_states/nevada/state/us__llc_formation.md",
            "title: T\ncode: nv__profit_corp_formation\njurisdiction: NV\norigin_url: https://www.nvsos.gov/forms",
        ));
        assert!(v
            .iter()
            .any(|v| v.message.contains("filename stem `us__llc_formation`")));
        assert!(v
            .iter()
            .any(|v| v.message.contains("jurisdiction prefix `nv__`")));
    }

    #[test]
    fn accepts_an_official_court_domain_outside_gov() {
        // Washoe publishes its own forms on the court's `.com` site.
        let v = F110JurisdictionPath.lint(&at(
            "templates/notations/forms/united_states/nevada/second_judicial_district_court/nv__washoe_fee_waiver_application.md",
            "title: T\ncode: nv__washoe_fee_waiver_application\njurisdiction: NV\norigin_url: https://www.washoecourts.com/Forms/F-6.pdf",
        ));
        assert!(!v.iter().any(|v| v.message.contains("origin_url")));
    }

    #[test]
    fn still_flags_an_unrelated_origin_host() {
        let v = F110JurisdictionPath.lint(&at(
            "templates/notations/forms/united_states/nevada/second_judicial_district_court/nv__washoe_fee_waiver_application.md",
            "title: T\ncode: nv__washoe_fee_waiver_application\njurisdiction: NV\norigin_url: https://forms.example.com/F-6.pdf",
        ));
        assert!(v.iter().any(|v| v.message.contains("origin_url")));
    }

    #[test]
    fn flags_unknown_jurisdiction() {
        let v = F110JurisdictionPath.lint(&at(
            "templates/notations/neon_law/onboarding__letter.md",
            "title: T\ncode: onboarding__letter\njurisdiction: ZZ",
        ));
        assert_eq!(v[0].code, "N110");
        assert!(v[0].message.contains("Unknown jurisdiction `ZZ`"));
    }

    #[test]
    fn accepts_every_seeded_jurisdiction_code() {
        for (code, _prefix) in super::JURISDICTIONS.iter() {
            let v = F110JurisdictionPath.lint(&at(
                "templates/notations/neon_law/onboarding__letter.md",
                &format!("title: T\ncode: onboarding__letter\njurisdiction: {code}"),
            ));
            assert!(v.is_empty(), "jurisdiction `{code}` should validate: {v:?}");
        }
    }

    #[test]
    fn flags_form_template_without_government_origin_url() {
        let v = F110JurisdictionPath.lint(&at(
            "templates/notations/forms/united_states/nevada/state/nv__llc_formation.md",
            "title: T\ncode: nv__llc_formation\njurisdiction: NV\norigin_url: http://example.com",
        ));
        assert!(v
            .iter()
            .any(|v| v.message.contains("government `origin_url:`")));
    }

    #[test]
    fn flags_form_code_missing_jurisdiction_prefix() {
        let v = F110JurisdictionPath.lint(&at(
            "templates/notations/forms/united_states/nevada/state/llc_formation.md",
            "title: T\ncode: llc_formation\njurisdiction: NV\norigin_url: https://www.nvsos.gov/forms",
        ));
        assert!(v.iter().any(|v| v
            .message
            .contains("must start with jurisdiction prefix `nv__`")));
    }

    #[test]
    fn flags_missing_frontmatter() {
        let v = F110JurisdictionPath.lint(&SourceFile {
            path: PathBuf::from("templates/notations/neon_law/onboarding__letter.md"),
            contents: "no frontmatter here\n".to_string(),
        });
        assert_eq!(v[0].code, "N110");
        assert!(v[0].message.contains("Missing frontmatter"));
    }

    #[test]
    fn skips_the_catalog_readme() {
        let v = F110JurisdictionPath.lint(&SourceFile {
            path: PathBuf::from("templates/README.md"),
            contents: "# Notation templates\n\nNo frontmatter, and that is fine.\n".to_string(),
        });
        assert!(v.is_empty(), "{v:?}");
    }

    fn project_tree(relative: &str, fm: &str) -> (tempfile::TempDir, SourceFile) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("navigator.yaml"),
            "host: staging.neonlaw.com\nproject: acme\n",
        )
        .unwrap();
        let path = dir.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let contents = format!("---\n{fm}\n---\nbody\n");
        std::fs::write(&path, &contents).unwrap();
        (dir, SourceFile { path, contents })
    }

    #[test]
    fn accepts_a_flat_project_repository_template() {
        let (_dir, file) = project_tree(
            "templates/acme__engagement.md",
            "title: T\ncode: acme__engagement\njurisdiction: NV",
        );
        let v = F110JurisdictionPath.lint(&file);
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn accepts_a_flat_project_repository_draft() {
        let (_dir, file) = project_tree(
            "drafts/acme__engagement.md",
            "title: T\ncode: acme__engagement\njurisdiction: NV",
        );
        let v = F110JurisdictionPath.lint(&file);
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn rejects_a_subdirectory_in_project_drafts() {
        let (_dir, file) = project_tree(
            "drafts/nested/acme__engagement.md",
            "title: T\ncode: acme__engagement\njurisdiction: NV",
        );
        let v = F110JurisdictionPath.lint(&file);
        assert_eq!(v[0].code, "N110");
        assert!(v[0].message.contains("direct `drafts/<code>.md`"), "{v:?}");
    }

    #[test]
    fn rejects_a_subdirectory_in_a_project_repository() {
        let (_dir, file) = project_tree(
            "templates/neon_law/acme__engagement.md",
            "title: T\ncode: acme__engagement\njurisdiction: NV",
        );
        let v = F110JurisdictionPath.lint(&file);
        assert_eq!(v[0].code, "N110");
        assert!(
            v[0].message
                .contains("Project templates must be direct `templates/<code>.md` files"),
            "{v:?}"
        );
        assert!(!v[0].message.contains("notations/neon_law"), "{v:?}");
    }
}
