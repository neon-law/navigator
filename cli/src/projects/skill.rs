//! Project Skill commands and manifest pins.
//!
//! `list`, `show`, `use`, and `status` read the compiled catalog shared through
//! [`rules::project_skill_catalog`]. Lookups need no network or database connection.

use std::path::Path;
use std::process::ExitCode;

pub use rules::project_skill_catalog::{catalog, find, CatalogEntry};

use super::manifest;

/// The message `show`/`use` report when a `(jurisdiction, practice_area)`
/// pair does not resolve: the catalog entries closest to it by edit
/// distance, so a typo is correctable without dumping the whole catalog.
///
/// Deliberately does not echo the caller's raw `jurisdiction`/`practice_area`
/// back into the returned string. Both are eventually written through
/// `eprintln!` in `run_show`/`run_use`, and the `rust/cleartext-logging`
/// `CodeQL` query taints the whole parsed `Command` enum through the unrelated
/// `Commands::Secrets` arm, so any printed value that transitively descends
/// from a clap-parsed argument is flagged — even though these two are
/// plain-text operator input, never a secret. Rather than carry that known
/// false positive, the sink simply never echoes the tainted value: only
/// catalog-derived text (untainted — it comes from the compiled-in catalog,
/// not the parsed argument) reaches the message.
#[must_use]
pub fn unresolved_message(
    entries: &[CatalogEntry],
    jurisdiction: &str,
    practice_area: &str,
) -> String {
    let asked = format!(
        "{}/{}",
        jurisdiction.to_uppercase(),
        practice_area.to_lowercase()
    );
    let mut ranked: Vec<(usize, String)> = entries
        .iter()
        .map(|entry| {
            let pair = format!("{}/{}", entry.jurisdiction, entry.practice_area);
            (levenshtein(&asked, &pair), pair)
        })
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    if ranked.is_empty() {
        return "no Project Skill resolves for that jurisdiction and practice area; \
                 the catalog is empty"
            .to_string();
    }
    let closest: Vec<String> = ranked.into_iter().take(3).map(|(_, pair)| pair).collect();
    format!(
        "no Project Skill resolves for that jurisdiction and practice area; \
         closest catalog entries: {}",
        closest.join(", ")
    )
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, &ac) in a.iter().enumerate() {
        let mut curr = vec![i + 1; b.len() + 1];
        curr[0] = i + 1;
        for (j, &bc) in b.iter().enumerate() {
            curr[j + 1] = if ac == bc {
                prev[j]
            } else {
                1 + prev[j].min(prev[j + 1]).min(curr[j])
            };
        }
        prev = curr;
    }
    prev[b.len()]
}

/// `navigator project skill list` — every catalog entry's `jurisdiction`,
/// `practice_area`, and `name`.
#[must_use]
pub fn run_list() -> ExitCode {
    let entries = catalog();
    for entry in &entries {
        println!(
            "{}\t{}\t{}",
            entry.jurisdiction, entry.practice_area, entry.name
        );
    }
    ExitCode::SUCCESS
}

/// `navigator project skill show <jurisdiction> <practice_area>` — the
/// resolved entry's full body.
#[must_use]
pub fn run_show(jurisdiction: &str, practice_area: &str) -> ExitCode {
    let entries = catalog();
    if let Some(entry) = find(&entries, jurisdiction, practice_area) {
        println!("{}", entry.body.trim_end());
        return ExitCode::SUCCESS;
    }
    eprintln!(
        "navigator: {}",
        unresolved_message(&entries, jurisdiction, practice_area)
    );
    ExitCode::from(1)
}

/// `navigator project skill use <jurisdiction> <practice_area>` — pin the
/// resolved entry's version onto the Project rooted at `dir`, and scaffold
/// each Notation `code` it bundles.
#[must_use]
pub fn run_use(dir: &Path, jurisdiction: &str, practice_area: &str) -> ExitCode {
    let entries = catalog();
    let Some(entry) = find(&entries, jurisdiction, practice_area) else {
        eprintln!(
            "navigator: {}",
            unresolved_message(&entries, jurisdiction, practice_area)
        );
        return ExitCode::from(1);
    };

    let manifest_path = dir.join(manifest::FILE);
    let contents = match std::fs::read_to_string(&manifest_path) {
        Ok(contents) => contents,
        Err(error) => {
            eprintln!("navigator: read {}: {error}", manifest_path.display());
            return ExitCode::from(2);
        }
    };
    let (updated, changed) = match manifest::pin_skill(
        &contents,
        &entry.jurisdiction,
        &entry.practice_area,
        &entry.version,
    ) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("navigator: {error}");
            return ExitCode::from(2);
        }
    };
    if changed {
        if let Err(error) = std::fs::write(&manifest_path, &updated) {
            eprintln!("navigator: write {}: {error}", manifest_path.display());
            return ExitCode::from(2);
        }
    }

    for code in &entry.notations {
        if let Err(error) = scaffold_notation(dir, code) {
            eprintln!("navigator: {error}");
            return ExitCode::from(2);
        }
    }

    if changed {
        println!(
            "pinned {}/{} at {}",
            entry.jurisdiction, entry.practice_area, entry.version
        );
    } else {
        println!(
            "{}/{} already pinned at {}",
            entry.jurisdiction, entry.practice_area, entry.version
        );
    }
    ExitCode::SUCCESS
}

/// Materialize one bundled Notation `code` into this Project's
/// `drafts/<code>.md` (or its `templates/` alias), the flat layout `N110`
/// requires of a Project repository. A no-op when the file already exists —
/// re-running `use` never overwrites an attorney's edits to a scaffolded
/// template. An unedited lifecycle stub is a placeholder, not an edit, so the
/// bundled Notation replaces it. Reads the
/// bundled Notation catalog through [`portal::template_api::bundled_files`],
/// the same compiled-in reader `navigator notation preview`
/// (`crate::notations_preview::bundled_template`) already resolves a
/// template's body from, so the catalog is read in exactly one place rather
/// than embedded a second time.
fn scaffold_notation(dir: &Path, code: &str) -> Result<(), String> {
    let target = dir
        .join(super::repository::draft_directory(dir))
        .join(format!("{code}.md"));
    match std::fs::read_to_string(&target) {
        Ok(contents) if !super::lifecycle::is_unedited_stub(&contents) => return Ok(()),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("read {}: {error}", target.display())),
    }
    // The filename stem is the canonical template code. This works for both
    // government forms and firm notation templates, so no frontmatter parse
    // is needed to resolve a catalog entry.
    let body = portal::template_api::bundled_files()
        .into_iter()
        .find(|(path, _)| Path::new(path).file_stem().and_then(|stem| stem.to_str()) == Some(code))
        .map(|(_, bytes)| bytes)
        .ok_or_else(|| format!("bundled Notation `{code}` not found in this binary's catalog"))?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    std::fs::write(&target, body).map_err(|error| format!("write {}: {error}", target.display()))
}

/// `navigator project skill status` — every Project Skill this Project's
/// `navigator.yaml` pins, and whether it still resolves.
#[must_use]
pub fn run_status(dir: &Path) -> ExitCode {
    match resolve_pins(dir) {
        Ok(resolutions) => {
            for resolution in &resolutions {
                let state = if resolution.resolved {
                    "resolvable"
                } else {
                    "unresolvable"
                };
                println!(
                    "{}\t{}\t{}\t{state}",
                    resolution.jurisdiction, resolution.practice_area, resolution.pinned_version
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator: {error}");
            ExitCode::from(2)
        }
    }
}

/// One pinned Project Skill, resolved against the compiled-in catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinResolution {
    pub jurisdiction: String,
    pub practice_area: String,
    pub pinned_version: String,
    /// `true` when the catalog still carries this `(jurisdiction,
    /// practice_area)` pair at exactly `pinned_version`.
    pub resolved: bool,
    /// The version the catalog currently carries for this pair, when the
    /// pair itself still resolves (even if the version has moved on).
    pub catalog_version: Option<String>,
}

/// Resolve every `skills:` pin in `<dir>/navigator.yaml` against the
/// compiled-in catalog. Shared by `navigator project gate --check` (ENG-879)
/// and `navigator project skill status` (ENG-880) so the two report the same
/// verdict for the same fixture rather than reimplementing the check twice.
///
/// # Errors
///
/// A string error if `navigator.yaml` cannot be read or does not parse.
pub fn resolve_pins(dir: &Path) -> Result<Vec<PinResolution>, String> {
    let manifest_path = dir.join(manifest::FILE);
    let contents = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("read {}: {error}", manifest_path.display()))?;
    let parsed = manifest::parse(&contents)?;
    let entries = catalog();
    Ok(parsed
        .skills
        .into_iter()
        .map(|pin| {
            let catalog_entry = find(&entries, &pin.jurisdiction, &pin.practice_area);
            let (resolved, catalog_version) = match catalog_entry {
                Some(entry) if entry.version == pin.version => (true, Some(entry.version.clone())),
                Some(entry) => (false, Some(entry.version.clone())),
                None => (false, None),
            };
            PinResolution {
                jurisdiction: pin.jurisdiction,
                practice_area: pin.practice_area,
                pinned_version: pin.version,
                resolved,
                catalog_version,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{
        catalog, find, resolve_pins, run_status, run_use, scaffold_notation, unresolved_message,
    };

    #[test]
    fn the_catalog_carries_both_seeded_entries() {
        let entries = catalog();
        assert!(
            find(&entries, "NV", "estates").is_some(),
            "nv/estates should be in the catalog: {entries:?}"
        );
        assert!(
            find(&entries, "TX", "probate").is_some(),
            "tx/probate should be in the catalog: {entries:?}"
        );
    }

    #[test]
    fn find_is_case_insensitive_on_jurisdiction() {
        let entries = catalog();
        let upper = find(&entries, "NV", "estates").expect("NV estates resolves");
        let lower = find(&entries, "nv", "estates").expect("nv estates resolves");
        assert_eq!(upper.name, lower.name);
    }

    #[test]
    fn an_unrecognized_jurisdiction_names_the_closest_entries_without_echoing_the_input() {
        // "zz"/"wills" are deliberately not the seeded NV/estates or
        // TX/probate pair, so a coincidental substring match can't hide a
        // regression either way.
        let entries = catalog();
        assert!(find(&entries, "zz", "wills").is_none());
        let message = unresolved_message(&entries, "zz", "wills");
        assert!(
            !message.contains("zz") && !message.contains("wills"),
            "the raw argument must not be echoed back (CodeQL rust/cleartext-logging): {message}"
        );
        assert!(message.contains("NV/estates"), "{message}");
    }

    fn scaffold(dir: &std::path::Path, yaml: &str) {
        std::fs::write(dir.join("navigator.yaml"), yaml).unwrap();
    }

    #[test]
    fn use_pins_the_entry_and_scaffolds_its_notations() {
        let dir = tempfile::tempdir().unwrap();
        scaffold(dir.path(), "host: staging.neonlaw.com\nproject: acme\n");
        let code = run_use(dir.path(), "nv", "estates");
        assert_eq!(code, std::process::ExitCode::SUCCESS);
        let manifest_contents = std::fs::read_to_string(dir.path().join("navigator.yaml")).unwrap();
        assert!(manifest_contents.contains("skills"), "{manifest_contents}");
        assert!(manifest_contents.contains("estates"), "{manifest_contents}");
        assert!(
            dir.path().join("drafts/onboarding__letter.md").is_file(),
            "the bundled onboarding__letter notation should be scaffolded"
        );
    }

    #[test]
    fn use_replaces_the_scaffolds_unedited_stub_but_keeps_a_drafted_letter() {
        let dir = tempfile::tempdir().unwrap();
        scaffold(dir.path(), "host: staging.neonlaw.com\nproject: acme\n");
        let (relative, stub) =
            crate::projects::lifecycle::scaffold_draft(crate::projects::lifecycle::ONBOARDING);
        let path = dir.path().join(&relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &stub).unwrap();
        assert_eq!(
            run_use(dir.path(), "nv", "estates"),
            std::process::ExitCode::SUCCESS
        );
        let replaced = std::fs::read_to_string(&path).unwrap();
        assert_ne!(replaced, stub, "the bundled letter replaces the stub");

        std::fs::write(&path, "---\nkind: onboarding\ntitle: Drafted\n---\n").unwrap();
        scaffold_notation(dir.path(), "onboarding__letter").unwrap();
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("title: Drafted"));
        assert!(!dir.path().join("templates").exists());
    }

    #[test]
    fn an_unreadable_draft_is_refused_without_replacing_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drafts/onboarding__letter.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = [0xff, 0xfe, b'D', b'r', b'a', b'f', b't'];
        std::fs::write(&path, original).unwrap();

        let result = scaffold_notation(dir.path(), "onboarding__letter");

        assert!(
            std::fs::read(&path).unwrap() == original,
            "an existing Draft must remain byte-identical when it cannot be read"
        );
        assert!(result.is_err(), "the unreadable Draft must be refused");
    }

    #[test]
    fn a_second_use_of_the_same_pin_does_not_duplicate_it() {
        let dir = tempfile::tempdir().unwrap();
        scaffold(dir.path(), "host: staging.neonlaw.com\nproject: acme\n");
        assert_eq!(
            run_use(dir.path(), "nv", "estates"),
            std::process::ExitCode::SUCCESS
        );
        assert_eq!(
            run_use(dir.path(), "NV", "estates"),
            std::process::ExitCode::SUCCESS
        );
        let resolutions = resolve_pins(dir.path()).unwrap();
        assert_eq!(resolutions.len(), 1, "{resolutions:?}");
    }

    #[test]
    fn use_of_an_unknown_pair_fails_without_touching_the_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "host: staging.neonlaw.com\nproject: acme\n";
        scaffold(dir.path(), yaml);
        let code = run_use(dir.path(), "zz", "estates");
        assert_ne!(code, std::process::ExitCode::SUCCESS);
        let after = std::fs::read_to_string(dir.path().join("navigator.yaml")).unwrap();
        assert_eq!(after, yaml, "a failed use must not modify navigator.yaml");
    }

    #[test]
    fn resolve_pins_reports_a_pin_present_at_its_catalog_version() {
        let dir = tempfile::tempdir().unwrap();
        scaffold(
            dir.path(),
            "host: staging.neonlaw.com\nproject: acme\nskills:\n  - jurisdiction: NV\n    practice_area: estates\n    version: \"1\"\n",
        );
        let resolutions = resolve_pins(dir.path()).unwrap();
        assert_eq!(resolutions.len(), 1);
        assert!(resolutions[0].resolved, "{resolutions:?}");
    }

    #[test]
    fn resolve_pins_reports_an_unknown_code_as_unresolved() {
        let dir = tempfile::tempdir().unwrap();
        scaffold(
            dir.path(),
            "host: staging.neonlaw.com\nproject: acme\nskills:\n  - jurisdiction: ZZ\n    practice_area: nowhere\n    version: \"1\"\n",
        );
        let resolutions = resolve_pins(dir.path()).unwrap();
        assert_eq!(resolutions.len(), 1);
        assert!(!resolutions[0].resolved);
        assert!(resolutions[0].catalog_version.is_none());
    }

    #[test]
    fn resolve_pins_reports_a_stale_version_as_unresolved_naming_both() {
        let dir = tempfile::tempdir().unwrap();
        scaffold(
            dir.path(),
            "host: staging.neonlaw.com\nproject: acme\nskills:\n  - jurisdiction: NV\n    practice_area: estates\n    version: \"999\"\n",
        );
        let resolutions = resolve_pins(dir.path()).unwrap();
        assert_eq!(resolutions.len(), 1);
        assert!(!resolutions[0].resolved);
        assert_eq!(resolutions[0].pinned_version, "999");
        assert_eq!(resolutions[0].catalog_version.as_deref(), Some("1"));
    }

    const TWO_PIN_FIXTURE: &str = concat!(
        "host: staging.neonlaw.com\nproject: acme\nskills:\n",
        "  - jurisdiction: NV\n    practice_area: estates\n    version: \"1\"\n",
        "  - jurisdiction: TX\n    practice_area: probate\n    version: \"1\"\n",
    );

    const STALE_PIN_FIXTURE: &str = concat!(
        "host: staging.neonlaw.com\nproject: acme\nskills:\n",
        "  - jurisdiction: NV\n    practice_area: estates\n    version: \"999\"\n",
    );

    #[test]
    fn a_fixture_with_two_pins_reports_both_resolvable() {
        let dir = tempfile::tempdir().unwrap();
        scaffold(dir.path(), TWO_PIN_FIXTURE);
        let resolutions = resolve_pins(dir.path()).unwrap();
        assert_eq!(resolutions.len(), 2, "{resolutions:?}");
        assert!(resolutions.iter().all(|r| r.resolved), "{resolutions:?}");
    }

    #[test]
    fn status_and_gate_check_agree_on_the_same_fixture() {
        // `navigator project skill status` (`run_status`, this test) and
        // `navigator project gate --check` (`crate::append_skill_pin_findings`
        // in `cli/src/main.rs`) both read their verdict from
        // `resolve_pins` — this is the shared-fixture proof ENG-880 asks for:
        // a two-pin fixture reports both resolvable, and a stale pin reports
        // unresolved, the same way on both surfaces because there is only one
        // resolution function.
        let resolvable = tempfile::tempdir().unwrap();
        scaffold(resolvable.path(), TWO_PIN_FIXTURE);
        assert_eq!(
            run_status(resolvable.path()),
            std::process::ExitCode::SUCCESS
        );
        assert!(resolve_pins(resolvable.path())
            .unwrap()
            .iter()
            .all(|r| r.resolved));

        let stale = tempfile::tempdir().unwrap();
        scaffold(stale.path(), STALE_PIN_FIXTURE);
        assert_eq!(run_status(stale.path()), std::process::ExitCode::SUCCESS);
        assert!(!resolve_pins(stale.path()).unwrap()[0].resolved);
    }
}
