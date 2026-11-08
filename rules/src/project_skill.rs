//! The Project Skill catalog — `skills/{jurisdiction_code_lowercase}/{practice_area}.md`
//! — and the parser/validator every entry is held to.
//!
//! A **Project Skill** is a practice playbook: a jurisdiction, a practice
//! area, a name, a version, the [Notation](crate::kind) codes it bundles,
//! and the Firm Assumption keys it renders. `navigator project skill use`
//! (in the `cli` crate) pins one onto a Project's `navigator.yaml`. This
//! module owns only the offline shape of
//! the catalog entry — parsing one file's frontmatter into [`ProjectSkill`],
//! and [`catalog_violations`], the cross-file pass that walks `skills/` the
//! way [`crate::code_uniqueness_violations`] walks `templates/` for `N111`.
//!
//! Modeled on [`crate::kind`] and [`crate::s103`] for the parsing/validation
//! style — a closed, explicit `match` over frontmatter fields, a clear
//! `Display` error per failure mode — but this catalog is its own noun with
//! its own frontmatter schema, not a variant of either.
//!
//! # Jurisdiction is validated offline, not through `store::jurisdictions`
//!
//! The acceptance criteria describe resolving `jurisdiction` against the
//! seeded `jurisdiction` table (`store::jurisdictions::find_by_code`). This
//! crate cannot do that: `rules` has no `store` or `surrealdb` dependency
//! (by design — the same engine backs the LSP, `cli validate`, and CI, none
//! of which open a database connection), and `store::jurisdictions::find_by_code`
//! is async besides. `F110JurisdictionPath` (`N110`, notation template
//! jurisdictions) already answers the identical need offline, from
//! [`crate::f110::JURISDICTIONS`] — codes compiled in at build time from
//! `store/seeds/Jurisdiction.yaml`, the same source table `find_by_code`
//! reads at runtime. This module reuses that static rather than opening a
//! second embed of the same seed file.

use crate::assumption_keys::catalog_keys;
use crate::f110::JURISDICTIONS;
use crate::{frontmatter, line_byte_range, FileFilter, Violation};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// `N126` — a `skills/` catalog entry's frontmatter is malformed.
pub const MALFORMED_CODE: &str = "N126";
/// `N127` — two catalog entries declare the same `(jurisdiction, practice_area)` pair.
pub const DUPLICATE_CODE: &str = "N127";
/// `N128` — a catalog entry declares an assumption key the Firm Assumptions
/// catalog (`assumptions/schema.yaml`) does not define.
pub const UNDEFINED_ASSUMPTION_CODE: &str = "N128";

/// One parsed `skills/<jurisdiction>/<practice_area>.md` catalog entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSkill {
    /// A seeded jurisdiction code (`NV`, `TX`, `US`) — see
    /// [`crate::f110::JURISDICTIONS`].
    pub jurisdiction: String,
    /// The practice area slug (`estates`, `probate`), lowercase by
    /// convention (it names the file: `skills/nv/estates.md`).
    pub practice_area: String,
    /// The human-readable playbook name.
    pub name: String,
    /// The version a Project pins when it runs `navigator project skill use`.
    pub version: String,
    /// Notation `code`s this skill bundles, scaffolded onto a Project that
    /// pins it.
    pub notations: Vec<String>,
    /// Firm Assumption keys this kit renders, declared in the optional
    /// `assumptions:` frontmatter list. A kit carries no Firm numbers: the
    /// body references a key by dotted path and the Firm's own value is
    /// resolved at run time (`docs/glossary/firm-assumption.md`).
    pub assumptions: Vec<String>,
    /// The markdown body below the frontmatter — the playbook itself.
    pub body: String,
}

/// Why a catalog entry failed to parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectSkillError {
    /// No leading `---`-fenced frontmatter block at all.
    MissingFrontmatter,
    /// The frontmatter block is not valid YAML.
    InvalidYaml(String),
    /// The frontmatter block is not a top-level mapping.
    NotAMapping,
    /// A required key is absent.
    Missing(&'static str),
    /// A required key is present but empty.
    Empty(&'static str),
    /// A required key is present but is not a scalar (string/number/bool).
    NotAScalar(&'static str),
    /// `notations:` is present but is not a sequence of strings.
    NotASequence(&'static str),
    /// `jurisdiction:` does not name a code seeded in
    /// `store/seeds/Jurisdiction.yaml` — a free-text value like `Nevada`
    /// lands here, not in [`Self::Missing`] or [`Self::Empty`].
    UnknownJurisdiction(String),
    /// The body references an assumption key the frontmatter does not
    /// declare in `assumptions:` — a typo lands here rather than rendering
    /// blank quietly at run time.
    UndeclaredReference(String),
}

impl std::fmt::Display for ProjectSkillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingFrontmatter => write!(f, "missing YAML frontmatter"),
            Self::InvalidYaml(error) => write!(f, "frontmatter is not valid YAML: {error}"),
            Self::NotAMapping => write!(f, "frontmatter must be a mapping"),
            Self::Missing(key) => write!(f, "frontmatter is missing required `{key}:` field"),
            Self::Empty(key) => write!(f, "frontmatter `{key}:` is empty"),
            Self::NotAScalar(key) => write!(f, "frontmatter `{key}:` must be a scalar value"),
            Self::NotASequence(key) => write!(f, "frontmatter `{key}:` must be a list of strings"),
            Self::UnknownJurisdiction(value) => write!(
                f,
                "unknown jurisdiction `{value}`; expected a code seeded in \
                 store/seeds/Jurisdiction.yaml"
            ),
            Self::UndeclaredReference(key) => write!(
                f,
                "body references assumption key `{key}`, which the \
                 `assumptions:` frontmatter does not declare"
            ),
        }
    }
}

impl std::error::Error for ProjectSkillError {}

/// Parse one catalog entry's frontmatter and body.
///
/// # Errors
///
/// See [`ProjectSkillError`] for every rejected shape: missing/empty/
/// non-scalar `jurisdiction`, `practice_area`, `name`, or `version`; a
/// `jurisdiction` that is not a seeded code; or a `notations:` that is not a
/// list of strings.
pub fn parse(contents: &str) -> Result<ProjectSkill, ProjectSkillError> {
    let Some((fm, body)) = frontmatter::split(contents) else {
        return Err(ProjectSkillError::MissingFrontmatter);
    };
    let document: serde_yaml::Value = serde_yaml::from_str(fm)
        .map_err(|error| ProjectSkillError::InvalidYaml(error.to_string()))?;
    let mapping = document
        .as_mapping()
        .ok_or(ProjectSkillError::NotAMapping)?;

    let scalar = |key: &'static str| -> Result<String, ProjectSkillError> {
        match mapping.get(key) {
            None => Err(ProjectSkillError::Missing(key)),
            Some(serde_yaml::Value::Null) => Err(ProjectSkillError::Empty(key)),
            Some(serde_yaml::Value::String(value)) => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    Err(ProjectSkillError::Empty(key))
                } else {
                    Ok(trimmed.to_string())
                }
            }
            Some(_) => Err(ProjectSkillError::NotAScalar(key)),
        }
    };

    let jurisdiction = scalar("jurisdiction")?;
    let practice_area = scalar("practice_area")?;
    let name = scalar("name")?;
    let version = scalar("version")?;

    if !JURISDICTIONS.iter().any(|(code, _)| *code == jurisdiction) {
        return Err(ProjectSkillError::UnknownJurisdiction(jurisdiction));
    }

    let notations = match mapping.get("notations") {
        None => Vec::new(),
        Some(serde_yaml::Value::Sequence(items)) => {
            let mut codes = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(code) if !code.trim().is_empty() => codes.push(code.trim().to_string()),
                    _ => return Err(ProjectSkillError::NotASequence("notations")),
                }
            }
            codes
        }
        Some(_) => return Err(ProjectSkillError::NotASequence("notations")),
    };

    // `assumptions:` mirrors `notations:` — an optional list of scalar keys.
    // Declaring keys here is what lets the gate (`N128`) catch a typo'd
    // dotted path before a Firm renders the kit, and what lets the body's
    // `{{ key }}` references be checked against a declaration rather than
    // trusted.
    let assumptions = match mapping.get("assumptions") {
        None => Vec::new(),
        Some(serde_yaml::Value::Sequence(items)) => {
            let mut keys = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(key) if !key.trim().is_empty() => keys.push(key.trim().to_string()),
                    _ => return Err(ProjectSkillError::NotASequence("assumptions")),
                }
            }
            keys
        }
        Some(_) => return Err(ProjectSkillError::NotASequence("assumptions")),
    };

    for reference in assumption_references(body) {
        if !assumptions.iter().any(|key| key == &reference) {
            return Err(ProjectSkillError::UndeclaredReference(reference));
        }
    }

    Ok(ProjectSkill {
        jurisdiction,
        practice_area,
        name,
        version,
        notations,
        assumptions,
        body: body.to_string(),
    })
}

/// Every Firm Assumption key the body renders through `{{ key }}`, deduplicated
/// and sorted. A span is an assumption reference when it sits inside `{{`/`}}`
/// and carries no whitespace after trimming — a dotted key, not prose or an
/// unrelated template syntax.
#[must_use]
pub fn assumption_references(body: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        match after.find("}}") {
            Some(close) => {
                let candidate = after[..close].trim();
                let is_key = !candidate.is_empty()
                    && !candidate.chars().any(char::is_whitespace)
                    && !keys.iter().any(|key| key == candidate);
                if is_key {
                    keys.push(candidate.to_string());
                }
                rest = &after[close + 2..];
            }
            None => break,
        }
    }
    keys.sort();
    keys
}

/// Render one catalog entry's body against a Firm's resolved assumption
/// values. Each `{{ key }}` span becomes the resolver's value for `key`, and
/// an unset or unknown key renders as the empty string — a fresh deployment
/// shows blanks, never a crash and never another Firm's economics.
///
/// The substitution is pure: the caller resolves keys through
/// `store::firm_assumptions::lookup`, which is firm-scoped by construction.
#[must_use]
pub fn render_assumptions(body: &str, resolve: impl Fn(&str) -> Option<String>) -> String {
    let mut rendered = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(open) = rest.find("{{") {
        rendered.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            rendered.push_str("{{");
            rest = after;
            continue;
        };
        let candidate = after[..close].trim();
        if candidate.is_empty() || candidate.chars().any(char::is_whitespace) {
            rendered.push_str(&after[..close + 2]);
        } else {
            rendered.push_str(resolve(candidate).as_deref().unwrap_or_default());
        }
        rest = &after[close + 2..];
    }
    rendered.push_str(rest);
    rendered
}

/// Cross-file catalog checks (`N126`/`N127`/`N128`): walk `<dir>/skills/`,
/// parse every `.md` file, report each parse failure, every duplicate
/// `(jurisdiction, practice_area)` pair, and every `assumptions:` declaration
/// the Firm Assumptions catalog does not define. A repository with no
/// `skills/` directory simply finds nothing — mirrors
/// [`crate::code_uniqueness_violations`] and [`crate::service_template_violations`].
///
/// # Errors
///
/// An [`io::Error`] if the directory cannot be walked or a file cannot be
/// read.
pub fn catalog_violations(dir: &Path, filter: &dyn FileFilter) -> io::Result<Vec<Violation>> {
    let root = dir.join("skills");
    if !root.is_dir() {
        return Ok(Vec::new());
    }

    let mut entries: Vec<(PathBuf, String)> = Vec::new();
    for entry in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() && e.depth() > 0 {
                filter.include_dir(e.path())
            } else {
                true
            }
        })
    {
        let entry = entry.map_err(io::Error::other)?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        if !filter.include_file(path) {
            continue;
        }
        entries.push((path.to_path_buf(), fs::read_to_string(path)?));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut violations = Vec::new();
    let mut first_seen: HashMap<(String, String), PathBuf> = HashMap::new();
    let defined_keys = catalog_keys();
    for (path, contents) in entries {
        match parse(&contents) {
            Ok(skill) => {
                for key in &skill.assumptions {
                    if !defined_keys.contains(key) {
                        violations.push(Violation {
                            code: UNDEFINED_ASSUMPTION_CODE,
                            path: path.clone(),
                            line: 1,
                            range: line_byte_range(&contents, 1),
                            message: format!(
                                "Project Skill declares assumption key `{key}`, which \
                                 assumptions/schema.yaml does not define"
                            ),
                        });
                    }
                }
                let pair = (skill.jurisdiction, skill.practice_area);
                if let Some(prev) = first_seen.get(&pair) {
                    violations.push(Violation {
                        code: DUPLICATE_CODE,
                        path: path.clone(),
                        line: 1,
                        range: line_byte_range(&contents, 1),
                        message: format!(
                            "Duplicate Project Skill `{}`/`{}`; already declared in `{}`",
                            pair.0,
                            pair.1,
                            prev.display()
                        ),
                    });
                } else {
                    first_seen.insert(pair, path);
                }
            }
            Err(error) => violations.push(Violation {
                code: MALFORMED_CODE,
                path: path.clone(),
                line: 1,
                range: line_byte_range(&contents, 1),
                message: format!("Project Skill catalog entry is invalid: {error}"),
            }),
        }
    }
    Ok(violations)
}

#[cfg(test)]
mod tests {
    use super::{
        catalog_violations, parse, ProjectSkillError, DUPLICATE_CODE, MALFORMED_CODE,
        UNDEFINED_ASSUMPTION_CODE,
    };
    use crate::DefaultFileFilter;

    const ESTATES: &str = "---\njurisdiction: NV\npractice_area: estates\nname: Nevada Estates Playbook\nversion: \"1\"\nnotations:\n  - onboarding__letter\n---\n\nSynthetic playbook body.\n";
    const PROBATE: &str = "---\njurisdiction: TX\npractice_area: probate\nname: Texas Probate Playbook\nversion: \"1\"\nnotations:\n  - onboarding__letter\n  - offboarding__letter\n---\n\nSynthetic playbook body.\n";

    #[test]
    fn parses_the_nevada_estates_seed() {
        let skill = parse(ESTATES).expect("parses");
        assert_eq!(skill.jurisdiction, "NV");
        assert_eq!(skill.practice_area, "estates");
        assert_eq!(skill.name, "Nevada Estates Playbook");
        assert_eq!(skill.version, "1");
        assert_eq!(skill.notations, vec!["onboarding__letter".to_string()]);
        assert!(skill.body.contains("Synthetic playbook body."));
    }

    #[test]
    fn parses_the_texas_probate_seed() {
        let skill = parse(PROBATE).expect("parses");
        assert_eq!(skill.jurisdiction, "TX");
        assert_eq!(skill.practice_area, "probate");
        assert_eq!(
            skill.notations,
            vec![
                "onboarding__letter".to_string(),
                "offboarding__letter".to_string()
            ]
        );
    }

    #[test]
    fn missing_jurisdiction_fails_validation() {
        let body = "---\npractice_area: estates\nname: N\nversion: \"1\"\n---\nbody\n";
        assert_eq!(parse(body), Err(ProjectSkillError::Missing("jurisdiction")));
    }

    #[test]
    fn missing_practice_area_fails_validation() {
        let body = "---\njurisdiction: NV\nname: N\nversion: \"1\"\n---\nbody\n";
        assert_eq!(
            parse(body),
            Err(ProjectSkillError::Missing("practice_area"))
        );
    }

    #[test]
    fn a_free_text_jurisdiction_fails_validation() {
        let body =
            "---\njurisdiction: Nevada\npractice_area: estates\nname: N\nversion: \"1\"\n---\nbody\n";
        assert_eq!(
            parse(body),
            Err(ProjectSkillError::UnknownJurisdiction("Nevada".to_string()))
        );
    }

    #[test]
    fn an_unrecognized_jurisdiction_code_fails_validation() {
        let body =
            "---\njurisdiction: ZZ\npractice_area: estates\nname: N\nversion: \"1\"\n---\nbody\n";
        assert_eq!(
            parse(body),
            Err(ProjectSkillError::UnknownJurisdiction("ZZ".to_string()))
        );
    }

    #[test]
    fn empty_name_fails_validation() {
        let body =
            "---\njurisdiction: NV\npractice_area: estates\nname:\nversion: \"1\"\n---\nbody\n";
        assert_eq!(parse(body), Err(ProjectSkillError::Empty("name")));
    }

    #[test]
    fn missing_frontmatter_fails_validation() {
        assert_eq!(
            parse("no frontmatter here\n"),
            Err(ProjectSkillError::MissingFrontmatter)
        );
    }

    #[test]
    fn notations_may_be_absent() {
        let body =
            "---\njurisdiction: NV\npractice_area: estates\nname: N\nversion: \"1\"\n---\nbody\n";
        assert_eq!(parse(body).unwrap().notations, Vec::<String>::new());
    }

    #[test]
    fn a_non_string_notations_entry_fails_validation() {
        let body = "---\njurisdiction: NV\npractice_area: estates\nname: N\nversion: \"1\"\nnotations:\n  - 1\n---\nbody\n";
        assert_eq!(
            parse(body),
            Err(ProjectSkillError::NotASequence("notations"))
        );
    }

    #[test]
    fn every_seeded_jurisdiction_code_is_accepted() {
        for (code, _prefix) in crate::f110::JURISDICTIONS.iter() {
            let body =
                format!("---\njurisdiction: {code}\npractice_area: estates\nname: N\nversion: \"1\"\n---\nbody\n");
            assert!(parse(&body).is_ok(), "jurisdiction `{code}` should parse");
        }
    }

    #[test]
    fn every_error_reads_as_a_sentence_naming_its_key() {
        let cases = [
            (
                ProjectSkillError::MissingFrontmatter,
                "missing YAML frontmatter",
            ),
            (
                ProjectSkillError::InvalidYaml("bad indent".into()),
                "frontmatter is not valid YAML: bad indent",
            ),
            (
                ProjectSkillError::NotAMapping,
                "frontmatter must be a mapping",
            ),
            (
                ProjectSkillError::Missing("name"),
                "frontmatter is missing required `name:` field",
            ),
            (
                ProjectSkillError::Empty("version"),
                "frontmatter `version:` is empty",
            ),
            (
                ProjectSkillError::NotAScalar("practice_area"),
                "frontmatter `practice_area:` must be a scalar value",
            ),
            (
                ProjectSkillError::NotASequence("notations"),
                "frontmatter `notations:` must be a list of strings",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
        }
        let unknown = ProjectSkillError::UnknownJurisdiction("Nevada".into()).to_string();
        assert!(
            unknown.starts_with("unknown jurisdiction `Nevada`"),
            "{unknown}"
        );
        assert!(
            unknown.contains("store/seeds/Jurisdiction.yaml"),
            "{unknown}"
        );
    }

    #[test]
    fn a_repository_without_a_skills_directory_has_no_catalog_violations() {
        let dir = tempfile::tempdir().unwrap();
        let violations = catalog_violations(dir.path(), &DefaultFileFilter::default()).unwrap();
        assert!(violations.is_empty());
    }

    #[test]
    fn the_catalog_reports_malformed_entries_and_duplicate_pairs_but_skips_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let skills = dir.path().join("skills");
        std::fs::create_dir_all(skills.join("nv")).unwrap();
        std::fs::create_dir_all(skills.join("tx")).unwrap();
        std::fs::write(skills.join("nv/estates.md"), ESTATES).unwrap();
        // The same (NV, estates) pair again, in a later file.
        std::fs::write(skills.join("tx/estates-copy.md"), ESTATES).unwrap();
        std::fs::write(skills.join("tx/probate.md"), PROBATE).unwrap();
        std::fs::write(skills.join("tx/broken.md"), "no frontmatter here\n").unwrap();
        std::fs::write(skills.join("tx/notes.txt"), "not a catalog entry").unwrap();

        let violations = catalog_violations(dir.path(), &DefaultFileFilter::default()).unwrap();
        assert_eq!(violations.len(), 2, "{violations:?}");

        let malformed = violations
            .iter()
            .find(|violation| violation.code == MALFORMED_CODE)
            .expect("the entry without frontmatter is malformed");
        assert!(malformed.path.ends_with("tx/broken.md"));
        assert_eq!(malformed.line, 1);
        assert!(malformed.message.contains("missing YAML frontmatter"));

        let duplicate = violations
            .iter()
            .find(|violation| violation.code == DUPLICATE_CODE)
            .expect("the repeated pair is a duplicate");
        assert!(
            duplicate.path.ends_with("tx/estates-copy.md"),
            "files are walked in path order"
        );
        assert!(
            duplicate.message.contains("`NV`/`estates`"),
            "{}",
            duplicate.message
        );
        assert!(
            duplicate.message.contains("nv/estates.md"),
            "{}",
            duplicate.message
        );
    }

    #[test]
    fn an_optional_assumptions_list_parses() {
        let body = "---\njurisdiction: NV\npractice_area: estates\nname: N\nversion: \"1\"\nassumptions:\n  - plan.target_minutes.document_review\n---\nbody\n";
        let skill = parse(body).expect("parses");
        assert_eq!(
            skill.assumptions,
            vec!["plan.target_minutes.document_review".to_string()]
        );
    }

    #[test]
    fn an_assumptions_list_that_is_not_a_list_of_scalars_is_malformed() {
        let body = "---\njurisdiction: NV\npractice_area: estates\nname: N\nversion: \"1\"\nassumptions: 45\n---\nbody\n";
        assert_eq!(
            parse(body),
            Err(ProjectSkillError::NotASequence("assumptions"))
        );
        let blank = "---\njurisdiction: NV\npractice_area: estates\nname: N\nversion: \"1\"\nassumptions:\n  - \"\"\n---\nbody\n";
        assert_eq!(
            parse(blank),
            Err(ProjectSkillError::NotASequence("assumptions"))
        );
    }

    #[test]
    fn a_body_reference_to_an_undeclared_key_is_malformed() {
        let body = "---\njurisdiction: NV\npractice_area: estates\nname: N\nversion: \"1\"\nassumptions:\n  - plan.target_minutes.letter\n---\nThe plan targets {{ plan.target_minutes.document_review }} minutes.\n";
        let error = parse(body).expect_err("the undeclared reference is refused");
        assert_eq!(
            error,
            ProjectSkillError::UndeclaredReference(
                "plan.target_minutes.document_review".to_string()
            )
        );
        assert!(error.to_string().contains("does not declare"), "{error}");
    }

    #[test]
    fn a_declared_key_outside_the_firm_assumptions_catalog_is_n128() {
        let dir = tempfile::tempdir().unwrap();
        let skills = dir.path().join("skills");
        std::fs::create_dir_all(skills.join("nv")).unwrap();
        std::fs::write(
            skills.join("nv/estates.md"),
            "---\njurisdiction: NV\npractice_area: estates\nname: N\nversion: \"1\"\nassumptions:\n  - plan.target_minutes.nope\n---\nbody\n",
        )
        .unwrap();

        let violations = catalog_violations(dir.path(), &DefaultFileFilter::default()).unwrap();
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].code, UNDEFINED_ASSUMPTION_CODE);
        assert!(violations[0].message.contains("plan.target_minutes.nope"));
    }

    #[test]
    fn a_declared_catalog_key_passes_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        let skills = dir.path().join("skills");
        std::fs::create_dir_all(skills.join("nv")).unwrap();
        std::fs::write(
            skills.join("nv/estates.md"),
            "---\njurisdiction: NV\npractice_area: estates\nname: N\nversion: \"1\"\nassumptions:\n  - plan.target_minutes.document_review\n---\nThe plan targets {{ plan.target_minutes.document_review }} minutes.\n",
        )
        .unwrap();

        let violations = catalog_violations(dir.path(), &DefaultFileFilter::default()).unwrap();
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn reference_extraction_finds_deduplicates_and_sorts() {
        let body = "B references {{plan.target_minutes.letter}} then {{ plan.target_minutes.document_review }} and repeats {{plan.target_minutes.letter}}; {{not an assumption}} and {{ untethered stay literal.";
        assert_eq!(
            super::assumption_references(body),
            vec![
                "plan.target_minutes.document_review".to_string(),
                "plan.target_minutes.letter".to_string()
            ]
        );
    }

    #[test]
    fn rendering_substitutes_resolved_values_and_blanks_the_rest() {
        let body = "Review: {{ plan.target_minutes.document_review }}; letter: {{plan.target_minutes.letter}}; unset: {{ plan.target_minutes.uncontested_divorce }}.";
        let rendered = super::render_assumptions(body, |key| {
            if key == "plan.target_minutes.document_review" {
                Some("45".to_string())
            } else if key == "plan.target_minutes.letter" {
                Some("30".to_string())
            } else {
                None
            }
        });
        assert_eq!(
            rendered, "Review: 45; letter: 30; unset: .",
            "an unset key renders blank, never a crash and never another Firm's number"
        );
    }

    #[test]
    fn an_unknown_key_renders_blank_rather_than_crashing() {
        let rendered =
            super::render_assumptions("Value: {{ plan.target_minutes.everything }}.", |_| None);
        assert_eq!(rendered, "Value: .");
    }
}
